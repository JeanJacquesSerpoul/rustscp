//! Fenêtre du terminal SSH : onglets, recherche, réglages, menu contextuel.
//!
//! Chaque onglet est une vue xterm.js (terminal.js) ouverte sur la session SSH
//! de la fenêtre principale : même connexion authentifiée, aucun nouveau code
//! TOTP. Le flux de données passe directement du cœur à xterm.js ; cette
//! partie pilote les vues et réagit à leurs événements.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use leptos::html;
use leptos::prelude::*;
use rustscp_shared::{TERM_FONTS, TermPrefs, term_themes, tr, trf};
use serde::Deserialize;
use serde_json::json;
use wasm_bindgen::prelude::*;

use crate::api::{call, fire, listen};
use crate::fmt::window;
use crate::icons::icon;

// ------------------------------------------------------------ terminal.js
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = RustScpTerm, js_name = create)]
    fn js_create(
        el: &web_sys::HtmlElement,
        prefs: &JsValue,
        on_event: &js_sys::Function,
    ) -> TermView;
    #[wasm_bindgen(js_namespace = RustScpTerm, js_name = applyChrome)]
    fn js_apply_chrome(theme: &str);
    #[wasm_bindgen(js_namespace = RustScpTerm, js_name = onSchemeChange)]
    fn js_on_scheme_change(cb: &js_sys::Function);

    type TermView;
    #[wasm_bindgen(method)]
    fn start(this: &TermView, session: f64, cwd: Option<String>);
    #[wasm_bindgen(method)]
    fn dispose(this: &TermView);
    #[wasm_bindgen(method)]
    fn show(this: &TermView);
    #[wasm_bindgen(method)]
    fn hide(this: &TermView);
    #[wasm_bindgen(method)]
    fn focus(this: &TermView);
    #[wasm_bindgen(method)]
    fn copy(this: &TermView) -> bool;
    #[wasm_bindgen(method)]
    fn paste(this: &TermView);
    #[wasm_bindgen(method, js_name = selectAll)]
    fn select_all(this: &TermView);
    #[wasm_bindgen(method)]
    fn clear(this: &TermView);
    #[wasm_bindgen(method)]
    fn notice(this: &TermView, text: &str, kind: &str);
    #[wasm_bindgen(method, js_name = currentDir)]
    fn current_dir(this: &TermView) -> Option<String>;
    #[wasm_bindgen(method)]
    fn find(this: &TermView, query: &str, opts: &JsValue, backward: bool) -> bool;
    #[wasm_bindgen(method, js_name = clearSearch)]
    fn clear_search(this: &TermView);
    #[wasm_bindgen(method, js_name = setPrefs)]
    fn set_prefs(this: &TermView, prefs: &JsValue);
    #[wasm_bindgen(method)]
    fn renderer(this: &TermView) -> String;
}

/// Vues xterm.js par onglet (avec leur fonction de rappel, gardée en vie).
struct ViewSlot {
    view: TermView,
    _callback: Closure<dyn FnMut(String, JsValue)>,
}

thread_local! {
    static VIEWS: RefCell<HashMap<u64, ViewSlot>> = RefCell::new(HashMap::new());
}

fn with_view<T>(id: u64, f: impl FnOnce(&TermView) -> T) -> Option<T> {
    VIEWS.with(|v| v.borrow().get(&id).map(|s| f(&s.view)))
}

fn dev_mode() -> bool {
    js_sys::Reflect::get(&window(), &"__RUSTSCP_DEV__".into()).is_ok_and(|v| v.is_truthy())
}

// --------------------------------------------------------------- lancement
/// Paramètres de la fenêtre (injectés par le cœur, ou adresse en développement).
#[derive(Clone, Debug, Deserialize)]
pub struct Launch {
    pub label: String,
    pub session: u64,
    pub title: String,
    pub cwd: Option<String>,
}

/// Fenêtre de terminal ? (sinon : fenêtre principale)
pub fn launch_params() -> Option<Launch> {
    let w = window();
    if let Ok(v) = js_sys::Reflect::get(&w, &"__RUSTSCP_TERM__".into())
        && !v.is_undefined()
        && let Ok(l) = serde_wasm_bindgen::from_value::<Launch>(v)
    {
        return Some(l);
    }
    // développement (navigateur) : ?term=<session>&title=…&cwd=…
    let search = w.location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    let session = params.get("term")?.parse().ok()?;
    Some(Launch {
        label: "dev".into(),
        session,
        title: params.get("title").unwrap_or_default(),
        cwd: params.get("cwd").filter(|c| !c.is_empty()),
    })
}

/// Charge xterm.js et ses modules (uniquement dans les fenêtres de terminal).
pub async fn load_assets() {
    let doc = window().document().expect("document");
    let head = doc.head().expect("head");
    if let Ok(link) = doc.create_element("link") {
        let _ = link.set_attribute("rel", "stylesheet");
        let _ = link.set_attribute("href", "xterm/xterm.css");
        let _ = head.append_child(&link);
    }
    let scripts = [
        "xterm/xterm.js",
        "xterm/addon-fit.js",
        "xterm/addon-webgl.js",
        "xterm/addon-search.js",
        "xterm/addon-web-links.js",
        "xterm/addon-unicode11.js",
        "xterm/addon-image.js",
        "terminal.js",
    ];
    // exécution dans l'ordre (async = false) ; on attend le dernier
    let mut last = None;
    for src in scripts {
        let Ok(el) = doc.create_element("script") else {
            continue;
        };
        let _ = el.set_attribute("src", src);
        let _ = js_sys::Reflect::set(&el, &"async".into(), &false.into());
        let _ = head.append_child(&el);
        last = Some(el);
    }
    if let Some(el) = last {
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let _ = el.add_event_listener_with_callback("load", &resolve);
            let _ = el.add_event_listener_with_callback("error", &resolve);
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
}

// ------------------------------------------------------------------- état
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TState {
    Connecting,
    Running,
    Ended,
}

#[derive(Clone, Copy)]
struct TTab {
    id: u64,
    /// Titre annoncé par le shell (séquence OSC), sinon celui du site.
    title: RwSignal<String>,
    state: RwSignal<TState>,
    /// Sortie reçue pendant que l'onglet est en arrière-plan.
    activity: RwSignal<bool>,
    bell: RwSignal<bool>,
    host: NodeRef<html::Div>,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct SearchOpts {
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
}

#[derive(Clone)]
struct Confirm {
    text: String,
    yes: String,
    run: Arc<dyn Fn() + Send + Sync>,
}

#[derive(Clone)]
enum MenuEntry {
    Item {
        label: &'static str,
        icon: &'static str,
        shortcut: &'static str,
        enabled: bool,
        run: Arc<dyn Fn() + Send + Sync>,
    },
    Sep,
}

impl MenuEntry {
    fn item(
        label: &'static str,
        icon: &'static str,
        shortcut: &'static str,
        run: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        MenuEntry::Item {
            label,
            icon,
            shortcut,
            enabled: true,
            run: Arc::new(run),
        }
    }

    fn enabled(mut self, on: bool) -> Self {
        if let MenuEntry::Item { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }
}

#[derive(Clone, Copy)]
struct T {
    launch: StoredValue<Launch>,
    tabs: RwSignal<Vec<TTab>>,
    active: RwSignal<u64>,
    next: StoredValue<u64>,
    prefs: RwSignal<TermPrefs>,
    search_open: RwSignal<bool>,
    query: RwSignal<String>,
    opts: RwSignal<SearchOpts>,
    /// (index, nombre) ; index −1 : aucun résultat sélectionné.
    results: RwSignal<(i32, u32)>,
    settings_open: RwSignal<bool>,
    menu: RwSignal<Option<(f64, f64, Vec<MenuEntry>)>>,
    confirm: RwSignal<Option<Confirm>>,
    search_input: NodeRef<html::Input>,
}

const ZOOM_MIN: f64 = 6.0;
const ZOOM_MAX: f64 = 40.0;

/// Préférences transmises à xterm.js (police par défaut, libellés).
fn js_prefs(p: &TermPrefs) -> JsValue {
    let mut p = p.clone();
    if p.font_family.trim().is_empty() {
        p.font_family = TERM_FONTS.into();
    }
    let v = serde_wasm_bindgen::to_value(&p).unwrap_or(JsValue::NULL);
    let _ = js_sys::Reflect::set(
        &v,
        &"link_hint".into(),
        &tr!(
            "Ctrl+clic pour ouvrir le lien",
            "Ctrl+click to open the link"
        )
        .into(),
    );
    v
}

#[derive(Deserialize, Default)]
struct StateEv {
    state: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: bool,
    #[serde(default)]
    lost: bool,
}

#[derive(Deserialize)]
struct SearchEv {
    index: i32,
    count: u32,
}

#[derive(Deserialize)]
struct MenuEv {
    x: f64,
    y: f64,
    selection: bool,
}

impl T {
    fn tab(&self, id: u64) -> Option<TTab> {
        self.tabs
            .with_untracked(|t| t.iter().find(|t| t.id == id).copied())
    }

    fn current(&self) -> Option<TTab> {
        self.tab(self.active.get_untracked())
    }

    fn view_do<R>(&self, f: impl FnOnce(&TermView) -> R) -> Option<R> {
        with_view(self.active.get_untracked(), f)
    }

    fn focus(&self) {
        let id = self.active.get_untracked();
        set_timeout(
            move || {
                with_view(id, |v| v.focus());
            },
            std::time::Duration::ZERO,
        );
    }

    fn new_tab(&self, cwd: Option<String>) {
        let id = self.next.get_value();
        self.next.set_value(id + 1);
        let title = self.launch.with_value(|l| l.title.clone());
        let tab = TTab {
            id,
            title: RwSignal::new(title),
            state: RwSignal::new(TState::Connecting),
            activity: RwSignal::new(false),
            bell: RwSignal::new(false),
            host: NodeRef::new(),
        };
        self.tabs.update(|t| t.push(tab));
        self.activate(id);
        let t = *self;
        // la vue est créée une fois le conteneur dans le document (pas de
        // requestAnimationFrame : suspendu tant que la fenêtre n'est pas affichée)
        set_timeout(move || t.create_view(tab, cwd), std::time::Duration::ZERO);
    }

    fn create_view(&self, tab: TTab, cwd: Option<String>) {
        let Some(el) = tab.host.get_untracked() else {
            return;
        };
        let t = *self;
        let id = tab.id;
        let callback =
            Closure::<dyn FnMut(String, JsValue)>::new(move |kind: String, data: JsValue| {
                t.on_event(id, &kind, data);
            });
        let view = js_create(
            &el,
            &js_prefs(&self.prefs.get_untracked()),
            callback.as_ref().unchecked_ref(),
        );
        let session = self.launch.with_value(|l| l.session) as f64;
        view.start(session, cwd);
        if self.active.get_untracked() == id {
            view.show();
        } else {
            view.hide();
        }
        VIEWS.with(|v| {
            v.borrow_mut().insert(
                id,
                ViewSlot {
                    view,
                    _callback: callback,
                },
            )
        });
    }

    fn activate(&self, id: u64) {
        let old = self.active.get_untracked();
        if old != id {
            with_view(old, |v| v.hide());
            if self.search_open.get_untracked() {
                with_view(old, |v| v.clear_search());
            }
        }
        self.active.set(id);
        if let Some(tab) = self.tab(id) {
            tab.activity.set(false);
            tab.bell.set(false);
        }
        set_timeout(
            move || {
                with_view(id, |v| v.show());
            },
            std::time::Duration::ZERO,
        );
    }

    fn cycle(&self, step: i64) {
        let tabs = self.tabs.get_untracked();
        if tabs.len() < 2 {
            return;
        }
        let pos = tabs
            .iter()
            .position(|t| t.id == self.active.get_untracked())
            .unwrap_or(0) as i64;
        let n = tabs.len() as i64;
        self.activate(tabs[((pos + step) % n + n) as usize % tabs.len()].id);
    }

    fn move_tab(&self, step: i64) {
        let id = self.active.get_untracked();
        self.tabs.update(|tabs| {
            if let Some(pos) = tabs.iter().position(|t| t.id == id) {
                let to = pos as i64 + step;
                if to >= 0 && (to as usize) < tabs.len() {
                    tabs.swap(pos, to as usize);
                }
            }
        });
    }

    fn close_tab(&self, id: u64) {
        if let Some(slot) = VIEWS.with(|v| v.borrow_mut().remove(&id)) {
            slot.view.dispose();
        }
        let pos = self
            .tabs
            .with_untracked(|v| v.iter().position(|x| x.id == id))
            .unwrap_or(0);
        self.tabs.update(|v| v.retain(|x| x.id != id));
        let rest = self.tabs.get_untracked();
        if rest.is_empty() {
            close_window();
        } else if self.active.get_untracked() == id {
            self.activate(rest[pos.min(rest.len() - 1)].id);
        }
    }

    fn restart(&self) {
        let session = self.launch.with_value(|l| l.session) as f64;
        self.view_do(|v| {
            let cwd = v.current_dir();
            v.start(session, cwd);
        });
        self.focus();
    }

    fn zoom(&self, delta: f64) {
        self.prefs.update(|p| {
            p.font_size = if delta == 0.0 {
                TermPrefs::default().font_size
            } else {
                (p.font_size + delta).clamp(ZOOM_MIN, ZOOM_MAX)
            }
        });
        self.save_prefs();
    }

    fn save_prefs(&self) {
        let prefs = self.prefs.get_untracked();
        apply_prefs(&prefs);
        fire("term_prefs_set", json!({ "prefs": prefs }));
    }

    fn open_search(&self) {
        self.search_open.set(true);
        self.menu.set(None);
        let input = self.search_input;
        set_timeout(
            move || {
                if let Some(el) = input.get_untracked() {
                    let _ = el.focus();
                    el.select();
                }
            },
            std::time::Duration::ZERO,
        );
    }

    fn close_search(&self) {
        self.search_open.set(false);
        self.view_do(|v| v.clear_search());
        self.results.set((-1, 0));
        self.focus();
    }

    fn find(&self, backward: bool, incremental: bool) {
        let q = self.query.get_untracked();
        let o = self.opts.get_untracked();
        let opts = serde_wasm_bindgen::to_value(&json!({
            "case_sensitive": o.case_sensitive,
            "whole_word": o.whole_word,
            "regex": o.regex,
            "incremental": incremental,
        }))
        .unwrap_or(JsValue::NULL);
        self.view_do(|v| v.find(&q, &opts, backward));
    }

    fn on_event(&self, id: u64, kind: &str, data: JsValue) {
        let Some(tab) = self.tab(id) else { return };
        match kind {
            "title" => {
                if let Some(s) = data.as_string().filter(|s| !s.trim().is_empty()) {
                    tab.title.set(s);
                }
            }
            "state" => {
                let ev: StateEv = serde_wasm_bindgen::from_value(data).unwrap_or_default();
                match ev.state.as_str() {
                    "connecting" => tab.state.set(TState::Connecting),
                    "running" => tab.state.set(TState::Running),
                    _ => {
                        tab.state.set(TState::Ended);
                        let msg = ev.message.unwrap_or_default();
                        let hint = tr!(
                            "Entrée : relancer la session  ·  Ctrl+Maj+W : fermer l'onglet",
                            "Enter: restart the session  ·  Ctrl+Shift+W: close the tab"
                        );
                        let kind = if ev.error || ev.lost { "error" } else { "dim" };
                        with_view(id, |v| {
                            if ev.error {
                                v.notice(
                                    &trf!(
                                        "Échec de l'ouverture du terminal : {msg}",
                                        "Unable to open the terminal: {msg}"
                                    ),
                                    kind,
                                );
                            } else {
                                v.notice(&format!("[{msg}]"), kind);
                            }
                            v.notice(hint, "dim");
                        });
                    }
                }
            }
            "activity" => {
                if self.active.get_untracked() != id {
                    tab.activity.set(true);
                }
            }
            "bell" => {
                if self.prefs.with_untracked(|p| p.visual_bell) {
                    tab.bell.set(false);
                    request_animation_frame(move || tab.bell.set(true));
                    set_timeout(
                        move || tab.bell.set(false),
                        std::time::Duration::from_millis(700),
                    );
                }
            }
            "search" => {
                if let Ok(ev) = serde_wasm_bindgen::from_value::<SearchEv>(data) {
                    self.results.set((ev.index, ev.count));
                }
            }
            "menu" => {
                if let Ok(ev) = serde_wasm_bindgen::from_value::<MenuEv>(data) {
                    self.menu
                        .set(Some((ev.x, ev.y, self.menu_items(ev.selection))));
                }
            }
            "key" => self.shortcut(&data.as_string().unwrap_or_default()),
            _ => {}
        }
    }

    fn shortcut(&self, action: &str) {
        match action {
            "search" => self.open_search(),
            "find-next" => self.find(false, false),
            "find-prev" => self.find(true, false),
            "new-tab" => self.new_tab(self.view_do(|v| v.current_dir()).flatten()),
            "close-tab" => self.close_tab(self.active.get_untracked()),
            "next-tab" => self.cycle(1),
            "prev-tab" => self.cycle(-1),
            "move-tab-left" => self.move_tab(-1),
            "move-tab-right" => self.move_tab(1),
            "zoom-in" => self.zoom(1.0),
            "zoom-out" => self.zoom(-1.0),
            "zoom-reset" => self.zoom(0.0),
            _ => {}
        }
    }

    fn menu_items(&self, selection: bool) -> Vec<MenuEntry> {
        let t = *self;
        let ended = self
            .current()
            .is_some_and(|tab| tab.state.get_untracked() == TState::Ended);
        vec![
            MenuEntry::item(
                tr!("Copier", "Copy"),
                "copy",
                tr!("Ctrl+Maj+C", "Ctrl+Shift+C"),
                move || {
                    t.view_do(|v| v.copy());
                },
            )
            .enabled(selection),
            MenuEntry::item(
                tr!("Coller", "Paste"),
                "edit",
                tr!("Ctrl+Maj+V", "Ctrl+Shift+V"),
                move || {
                    t.view_do(|v| v.paste());
                },
            ),
            MenuEntry::item(
                tr!("Tout sélectionner", "Select all"),
                "check",
                tr!("Ctrl+Maj+A", "Ctrl+Shift+A"),
                move || {
                    t.view_do(|v| v.select_all());
                },
            ),
            MenuEntry::item(
                tr!("Effacer l'écran", "Clear screen"),
                "trash",
                tr!("Ctrl+Maj+K", "Ctrl+Shift+K"),
                move || {
                    t.view_do(|v| v.clear());
                },
            ),
            MenuEntry::Sep,
            MenuEntry::item(
                tr!("Rechercher…", "Find…"),
                "search",
                tr!("Ctrl+Maj+F", "Ctrl+Shift+F"),
                move || t.open_search(),
            ),
            MenuEntry::Sep,
            MenuEntry::item(
                tr!("Nouvel onglet", "New tab"),
                "tab-new",
                tr!("Ctrl+Maj+T", "Ctrl+Shift+T"),
                move || t.shortcut("new-tab"),
            ),
            MenuEntry::item(
                tr!("Relancer la session", "Restart session"),
                "refresh",
                "",
                move || t.restart(),
            )
            .enabled(ended),
            MenuEntry::item(
                tr!("Fermer l'onglet", "Close tab"),
                "close",
                tr!("Ctrl+Maj+W", "Ctrl+Shift+W"),
                move || t.close_tab(t.active.get_untracked()),
            ),
            MenuEntry::Sep,
            MenuEntry::item(
                tr!("Fenêtre des fichiers", "Files window"),
                "folder",
                "",
                focus_main,
            ),
        ]
    }
}

fn apply_prefs(prefs: &TermPrefs) {
    js_apply_chrome(&prefs.theme);
    let js = js_prefs(prefs);
    VIEWS.with(|v| {
        for slot in v.borrow().values() {
            slot.view.set_prefs(&js);
        }
    });
}

fn close_window() {
    if dev_mode() {
        let _ = window().close();
    } else {
        fire("term_window_close", json!({}));
    }
}

fn focus_main() {
    if dev_mode() {
        if let Ok(Some(opener)) = window()
            .opener()
            .map(|o| o.dyn_into::<web_sys::Window>().ok())
        {
            let _ = opener.focus();
        }
    } else {
        fire("focus_main", json!({}));
    }
}

// ------------------------------------------------------------ composants
#[component]
fn TabStrip(t: T) -> impl IntoView {
    view! {
        <div class="term-tabs" on:dblclick=move |ev| {
            // double-clic sur l'espace libre : nouvel onglet
            if crate::fmt::target_element(&ev).is_some_and(|e| e.class_list().contains("term-tabs")) {
                t.shortcut("new-tab");
            }
        }>
            <For each=move || t.tabs.get() key=|tab| tab.id children=move |tab| {
                let id = tab.id;
                view! {
                    <div class="term-tab" class:active=move || t.active.get() == id
                        class:activity=move || tab.activity.get() class:bell=move || tab.bell.get()
                        title=move || tab.title.get()
                        on:mousedown=move |ev| {
                            if ev.button() == 1 { ev.prevent_default(); t.close_tab(id); }
                            else if ev.button() == 0 { t.activate(id); }
                        }>
                        <span class=move || match tab.state.get() {
                            TState::Connecting => "term-dot wait",
                            TState::Running => "term-dot ok",
                            TState::Ended => "term-dot off",
                        }></span>
                        <span class="term-tab-title">{move || tab.title.get()}</span>
                        <button class="term-tab-close" title=tr!("Fermer l'onglet (Ctrl+Maj+W)", "Close tab (Ctrl+Shift+W)")
                            on:mousedown=|ev| ev.stop_propagation()
                            on:click=move |_| t.close_tab(id)>{icon("close")}</button>
                    </div>
                }
            }/>
            <button class="term-btn term-new" title=tr!("Nouvel onglet (Ctrl+Maj+T)", "New tab (Ctrl+Shift+T)")
                on:click=move |_| t.shortcut("new-tab")>{icon("plus")}</button>
            <span class="grow"></span>
            <button class="term-btn" class:on=move || t.search_open.get() title=tr!("Rechercher (Ctrl+Maj+F)", "Find (Ctrl+Shift+F)")
                on:click=move |_| if t.search_open.get_untracked() { t.close_search() } else { t.open_search() }>{icon("search")}</button>
            <button class="term-btn" class:on=move || t.settings_open.get() title=tr!("Réglages du terminal", "Terminal settings")
                on:mousedown=|ev| ev.stop_propagation()
                on:click=move |_| t.settings_open.update(|o| *o = !*o)>{icon("settings")}</button>
            <button class="term-btn" title=tr!("Fenêtre des fichiers", "Files window") on:click=|_| focus_main()>{icon("folder")}</button>
        </div>
    }
}

#[component]
fn SearchBar(t: T) -> impl IntoView {
    let toggle = move |f: fn(&mut SearchOpts)| {
        t.opts.update(f);
        t.find(false, true);
    };
    let count = move || {
        let (i, n) = t.results.get();
        if t.query.with(|q| q.is_empty()) {
            String::new()
        } else if n == 0 {
            tr!("Aucun résultat", "No results").to_string()
        } else if i < 0 {
            trf!("{n} résultat(s)", "{n} result(s)")
        } else {
            format!("{}/{n}", i + 1)
        }
    };
    view! {
        <div class="term-search" class:hidden=move || !t.search_open.get() on:mousedown=|ev| ev.stop_propagation()>
            <input type="text" spellcheck="false" placeholder=tr!("Rechercher", "Find") node_ref=t.search_input
                prop:value=t.query
                on:input=move |ev| { t.query.set(event_target_value(&ev)); t.find(false, true); }
                on:keydown=move |ev| match ev.key().as_str() {
                    "Enter" | "F3" => { ev.prevent_default(); t.find(ev.shift_key(), false); }
                    "Escape" => { ev.prevent_default(); t.close_search(); }
                    _ => {}
                }/>
            <span class="term-count">{count}</span>
            <button class="term-opt" class:on=move || t.opts.get().case_sensitive title=tr!("Respecter la casse", "Match case")
                on:click=move |_| toggle(|o| o.case_sensitive = !o.case_sensitive)>"Aa"</button>
            <button class="term-opt" class:on=move || t.opts.get().whole_word title=tr!("Mot entier", "Whole word")
                on:click=move |_| toggle(|o| o.whole_word = !o.whole_word)><u>"ab"</u></button>
            <button class="term-opt" class:on=move || t.opts.get().regex title=tr!("Expression régulière", "Regular expression")
                on:click=move |_| toggle(|o| o.regex = !o.regex)>".*"</button>
            <button class="term-opt" title=tr!("Précédent (Maj+Entrée)", "Previous (Shift+Enter)") on:click=move |_| t.find(true, false)>
                <span class="flip">{icon("chevron-down")}</span></button>
            <button class="term-opt" title=tr!("Suivant (Entrée)", "Next (Enter)") on:click=move |_| t.find(false, false)>{icon("chevron-down")}</button>
            <button class="term-opt" title=tr!("Fermer (Échap)", "Close (Esc)") on:click=move |_| t.close_search()>{icon("close")}</button>
        </div>
    }
}

#[component]
fn SettingsPanel(t: T) -> impl IntoView {
    let set = move |f: Box<dyn Fn(&mut TermPrefs)>| {
        t.prefs.update(|p| f(p));
        t.save_prefs();
    };
    let num =
        move |ev: &web_sys::Event| event_target_value(ev).replace(',', ".").parse::<f64>().ok();
    let check = move |label: &'static str,
                      get: fn(&TermPrefs) -> bool,
                      put: fn(&mut TermPrefs, bool)| {
        view! {
            <label class="term-check">
                <input type="checkbox" prop:checked=move || t.prefs.with(get)
                    on:change=move |ev| { let on = event_target_checked(&ev); set(Box::new(move |p| put(p, on))) }/>
                {label}
            </label>
        }
    };
    let renderer = move || {
        t.prefs.track();
        t.active.track();
        t.settings_open.track();
        let r = t.view_do(|v| v.renderer()).unwrap_or_default();
        if r == "webgl" {
            tr!(
                "Rendu actuel : GPU (WebGL)",
                "Current renderer: GPU (WebGL)"
            )
        } else {
            tr!("Rendu actuel : DOM", "Current renderer: DOM")
        }
    };
    view! {
        <div class="term-settings" class:hidden=move || !t.settings_open.get() on:mousedown=|ev| ev.stop_propagation()>
            <div class="term-settings-title">{tr!("Réglages du terminal", "Terminal settings")}</div>
            <div class="term-grid">
                <label>{tr!("Thème", "Theme")}</label>
                <select on:change=move |ev| { let v = event_target_value(&ev); set(Box::new(move |p| p.theme = v.clone())) }>
                    {term_themes().into_iter().map(|(k, l)| view! { <option value=k prop:selected=move || t.prefs.with(|p| p.theme == k)>{l}</option> }).collect_view()}
                </select>
                <label>{tr!("Police", "Font")}</label>
                <input type="text" spellcheck="false" placeholder=tr!("(par défaut)", "(default)")
                    prop:value=move || t.prefs.with(|p| p.font_family.clone())
                    on:change=move |ev| { let v = event_target_value(&ev); set(Box::new(move |p| p.font_family = v.trim().to_string())) }/>
                <label>{tr!("Taille", "Size")}</label>
                <div class="term-row">
                    <input type="number" min="6" max="40" step="1" prop:value=move || t.prefs.with(|p| p.font_size.to_string())
                        on:change=move |ev| if let Some(v) = num(&ev) { set(Box::new(move |p| p.font_size = v.clamp(ZOOM_MIN, ZOOM_MAX))) }/>
                    <span class="muted small">{tr!("Ctrl+molette, Ctrl+= / Ctrl+-", "Ctrl+wheel, Ctrl+= / Ctrl+-")}</span>
                </div>
                <label>{tr!("Interligne", "Line height")}</label>
                <input type="number" min="1" max="2" step="0.05" prop:value=move || t.prefs.with(|p| format!("{:.2}", p.line_height))
                    on:change=move |ev| if let Some(v) = num(&ev) { set(Box::new(move |p| p.line_height = v.clamp(1.0, 2.0))) }/>
                <label>{tr!("Curseur", "Cursor")}</label>
                <select on:change=move |ev| { let v = event_target_value(&ev); set(Box::new(move |p| p.cursor_style = v.clone())) }>
                    {[("block", tr!("Bloc", "Block")), ("bar", tr!("Barre", "Bar")), ("underline", tr!("Souligné", "Underline"))]
                        .into_iter().map(|(k, l)| view! { <option value=k prop:selected=move || t.prefs.with(|p| p.cursor_style == k)>{l}</option> }).collect_view()}
                </select>
                <label>{tr!("Historique", "Scrollback")}</label>
                <div class="term-row">
                    <input type="number" min="0" max="1000000" step="1000" prop:value=move || t.prefs.with(|p| p.scrollback.to_string())
                        on:change=move |ev| if let Some(v) = num(&ev) { set(Box::new(move |p| p.scrollback = v.clamp(0.0, 1_000_000.0) as u32)) }/>
                    <span class="muted small">{tr!("lignes", "lines")}</span>
                </div>
            </div>
            {check(tr!("Curseur clignotant", "Blinking cursor"), |p| p.cursor_blink, |p, v| p.cursor_blink = v)}
            {check(tr!("Copier automatiquement la sélection", "Copy selection automatically"), |p| p.copy_on_select, |p, v| p.copy_on_select = v)}
            {check(tr!("Clic droit : coller (sinon menu)", "Right click: paste (otherwise menu)"), |p| p.right_click_paste, |p, v| p.right_click_paste = v)}
            {check(tr!("Signal sonore visuel", "Visual bell"), |p| p.visual_bell, |p, v| p.visual_bell = v)}
            {check(tr!("Accélération graphique (GPU)", "GPU acceleration"), |p| p.gpu, |p, v| p.gpu = v)}
            <div class="muted small term-renderer">{renderer}</div>
            <div class="term-settings-buttons">
                <button type="button" on:click=move |_| { t.prefs.set(TermPrefs::default()); t.save_prefs(); }>
                    {tr!("Valeurs par défaut", "Defaults")}</button>
                <button type="button" class="primary" on:click=move |_| { t.settings_open.set(false); t.focus(); }>"OK"</button>
            </div>
        </div>
    }
}

#[component]
fn TermMenu(t: T) -> impl IntoView {
    let node = NodeRef::<html::Div>::new();
    let pos = RwSignal::new((0.0, 0.0));
    Effect::new(move |_| {
        if let Some((x, y, _)) = t.menu.get() {
            pos.set((x, y));
            request_animation_frame(move || {
                if let Some(el) = node.get_untracked() {
                    // le menu prend le clavier (Échap) à la place du terminal
                    let _ = el.focus();
                    let r = el.get_bounding_client_rect();
                    let w = window();
                    let vw = w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1e4);
                    let vh = w
                        .inner_height()
                        .ok()
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1e4);
                    pos.set((
                        if x + r.width() > vw {
                            (vw - r.width() - 4.0).max(0.0)
                        } else {
                            x
                        },
                        if y + r.height() > vh {
                            (vh - r.height() - 4.0).max(0.0)
                        } else {
                            y
                        },
                    ));
                }
            });
        }
    });
    view! {
        {move || t.menu.get().map(|(_, _, items)| view! {
            <div class="menu term-menu" tabindex="-1" node_ref=node style:left=move || format!("{}px", pos.get().0) style:top=move || format!("{}px", pos.get().1)
                on:mousedown=|ev| ev.stop_propagation() on:contextmenu=|ev| ev.prevent_default()
                on:keydown=move |ev| if ev.key() == "Escape" { ev.prevent_default(); t.menu.set(None); t.focus(); }
                on:focusout=move |_| t.menu.set(None)>
                {items.into_iter().map(|e| match e {
                    MenuEntry::Sep => view! { <div class="menu-sep"></div> }.into_any(),
                    MenuEntry::Item { label, icon: ic, shortcut, enabled, run } => view! {
                        <div class="menu-item" class:disabled=!enabled
                            on:click=move |_| if enabled { t.menu.set(None); run(); t.focus(); }>
                            <span class="menu-ic">{icon(ic)}</span>
                            <span class="menu-label">{label}</span>
                            <span class="menu-shortcut">{shortcut}</span>
                        </div>
                    }.into_any(),
                }).collect_view()}
            </div>
        })}
    }
}

#[component]
fn ConfirmBox(t: T) -> impl IntoView {
    view! {
        {move || t.confirm.get().map(|c| {
            let run = c.run.clone();
            view! {
                <div class="modal-backdrop">
                    <div class="modal term-confirm">
                        <div class="modal-body"><p class="pre">{c.text.clone()}</p></div>
                        <div class="modal-buttons">
                            <button type="button" on:click=move |_| { t.confirm.set(None); t.focus(); }>{tr!("Annuler", "Cancel")}</button>
                            <button type="button" class="primary" on:click=move |_| { t.confirm.set(None); run(); }>{c.yes.clone()}</button>
                        </div>
                    </div>
                </div>
            }
        })}
    }
}

/// Application de la fenêtre de terminal.
#[component]
pub fn TerminalApp(launch: Launch) -> impl IntoView {
    let t = T {
        launch: StoredValue::new(launch.clone()),
        tabs: RwSignal::new(Vec::new()),
        active: RwSignal::new(0),
        next: StoredValue::new(1),
        prefs: RwSignal::new(TermPrefs::default()),
        search_open: RwSignal::new(false),
        query: RwSignal::new(String::new()),
        opts: RwSignal::new(SearchOpts::default()),
        results: RwSignal::new((-1, 0)),
        settings_open: RwSignal::new(false),
        menu: RwSignal::new(None),
        confirm: RwSignal::new(None),
        search_input: NodeRef::new(),
    };
    if let Some(body) = window().document().and_then(|d| d.body()) {
        let _ = body.class_list().add_1("term-window");
    }

    leptos::task::spawn_local(async move {
        if let Ok(p) = call::<TermPrefs>("term_prefs_get", json!({})).await {
            t.prefs.set(p);
        }
        js_apply_chrome(&t.prefs.get_untracked().theme);
        t.new_tab(launch.cwd.clone());
    });
    listen::<TermPrefs>("term-prefs", move |p| {
        if t.prefs.get_untracked() != p {
            t.prefs.set(p.clone());
            apply_prefs(&p);
        }
    });
    let label = launch.label.clone();
    listen::<String>("term-close-requested", move |target| {
        if target != label {
            return;
        }
        let n = t.tabs.with_untracked(|v| v.len());
        t.confirm.set(Some(Confirm {
            text: trf!(
                "{n} onglets sont ouverts dans cette fenêtre.\nLes fermer tous ?",
                "{n} tabs are open in this window.\nClose them all?"
            ),
            yes: tr!("Fermer tout", "Close all").into(),
            run: Arc::new(close_window),
        }));
    });
    // thème « auto » : suit le système
    let scheme = Closure::<dyn FnMut()>::new(move || apply_prefs(&t.prefs.get_untracked()));
    js_on_scheme_change(scheme.as_ref().unchecked_ref());
    scheme.forget();

    // titre de la fenêtre : celui de l'onglet actif
    Effect::new(move |_| {
        let id = t.active.get();
        let title = t
            .tabs
            .with(|v| v.iter().find(|x| x.id == id).map(|x| x.title.get()))
            .unwrap_or_default();
        let full = format!(
            "{title} — {}",
            tr!("Terminal SSH RustSCP", "RustSCP SSH terminal")
        );
        if let Some(doc) = window().document() {
            doc.set_title(&full);
        }
        if !dev_mode() {
            fire("term_set_title", json!({ "title": full }));
        }
    });

    let _ = window_event_listener(leptos::ev::mousedown, move |_| {
        t.menu.set(None);
        t.settings_open.set(false);
    });
    let _ = window_event_listener(leptos::ev::blur, move |_| t.menu.set(None));
    // fermeture de la page : libère les terminaux distants
    let _ = window_event_listener(leptos::ev::beforeunload, move |_| {
        VIEWS.with(|v| {
            for (_, slot) in v.borrow_mut().drain() {
                slot.view.dispose();
            }
        });
    });
    let _ = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape"
            && (t.menu.get_untracked().is_some() || t.settings_open.get_untracked())
        {
            t.menu.set(None);
            t.settings_open.set(false);
            t.focus();
        }
    });
    view! {
        <div class="term-app">
            <TabStrip t=t/>
            <div class="term-body">
                <For each=move || t.tabs.get() key=|tab| tab.id children=move |tab| {
                    let id = tab.id;
                    view! { <div class="term-host" class:active=move || t.active.get() == id node_ref=tab.host></div> }
                }/>
                <SearchBar t=t/>
                <SettingsPanel t=t/>
            </div>
            <TermMenu t=t/>
            <ConfirmBox t=t/>
        </div>
    }
}
