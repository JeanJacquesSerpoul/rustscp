//! Fenêtre principale : menus, barre d'outils, connexion rapide, journal,
//! onglets de session, file de transferts, barre d'état.

use std::cell::RefCell;
use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use rustscp_shared::util::{human_speed, rbasename, rparent};
use rustscp_shared::{
    AUTH_CHOICES, AUTH_INTERACTIVE, Direction, EditChanged, LogLine, PROTO_SCP, PromptEvent,
    QuickEntry, Settings, Site, SitesFile, TransferFinished, auth_label,
};
use serde::Deserialize;
use serde_json::json;
use wasm_bindgen::JsCast;

use crate::actions as act;
use crate::api::{call, fire, listen};
use crate::dialogs::{DialogHost, confirm};
use crate::fmt::{closest_with, element_at, in_text_field, window};
use crate::icons::{icon, logo};
use crate::layout::{Axis, Layout, Splitter, layout};
use crate::panes::{DRAG_ENDED, PaneView, start_local_watch};
use crate::queue::{QueuePanel, start_polling};
use crate::state::{AppInfo, ConnState, Ctx, Dialog, DragPayload, MenuItem, MenuState, ctx};

thread_local! {
    static HOVER: RefCell<Option<web_sys::Element>> = const { RefCell::new(None) };
}

// ------------------------------------------------------------- commandes
fn open_site_manager(c: Ctx) {
    let select = c
        .current()
        .and_then(|t| t.site.get_untracked())
        .map(|s| s.id);
    c.open_dialog(Dialog::SiteManager { select });
}

fn refresh_all(c: Ctx) {
    if let Some(t) = c.current() {
        act::refresh_local(c, t);
        act::refresh_remote(c, t);
    }
}

fn reconnect(c: Ctx) {
    if let Some(t) = c.current() {
        act::reconnect(c, t);
    }
}

fn disconnect(c: Ctx) {
    if let Some(t) = c.current() {
        act::disconnect(c, t);
    }
}

fn toggle_queue(c: Ctx) {
    let running = !c.queue.running.get_untracked();
    c.queue.running.set(running);
    fire("transfer_running", json!({ "running": running }));
}

fn cancel_all(c: Ctx) {
    fire("transfer_cancel_all", json!({}));
    let _ = c;
}

fn close_current_tab(c: Ctx) {
    act::close_tab(c, c.active.get_untracked());
}

fn sites_menu_items(c: Ctx) -> Vec<MenuItem> {
    let groups = act::sites_by_folder(c);
    if groups.iter().all(|(_, v)| v.is_empty()) {
        return vec![MenuItem::Label("(aucun site enregistré)".into())];
    }
    let entry = move |s: Site| {
        MenuItem::action(s.name.clone(), "server", move || {
            act::connect_site(c, s.clone())
        })
    };
    let mut items = Vec::new();
    for (folder, sites) in groups {
        if folder.is_empty() {
            items.extend(sites.into_iter().map(entry));
        } else {
            items.insert(
                0,
                MenuItem::Sub {
                    label: folder,
                    icon: "folder",
                    items: sites.into_iter().map(entry).collect(),
                },
            );
        }
    }
    items
}

fn main_menu(c: Ctx, which: &str) -> Vec<MenuItem> {
    let has_session = c
        .current()
        .is_some_and(|t| t.session.get_untracked().is_some());
    let has_site = c
        .current()
        .is_some_and(|t| t.site.get_untracked().is_some());
    match which {
        "Fichier" => vec![
            MenuItem::action("Gestionnaire de sites", "server", move || {
                open_site_manager(c)
            })
            .shortcut("Ctrl+S"),
            MenuItem::Sub {
                label: "Sites".into(),
                icon: "folder",
                items: sites_menu_items(c),
            },
            MenuItem::action("Nouvel onglet", "tab-new", move || {
                act::new_tab(c);
            })
            .shortcut("Ctrl+T"),
            MenuItem::action("Fermer l'onglet", "close", move || close_current_tab(c))
                .shortcut("Ctrl+W"),
            MenuItem::Separator,
            MenuItem::action("Quitter", "disconnect", move || act::quit(c)).shortcut("Ctrl+Q"),
        ],
        "Serveur" => vec![
            MenuItem::action("Reconnecter", "connect", move || reconnect(c))
                .enabled(has_site)
                .shortcut("Ctrl+R"),
            MenuItem::action("Déconnecter", "disconnect", move || disconnect(c))
                .enabled(has_session)
                .shortcut("Ctrl+Maj+D"),
            MenuItem::action("Actualiser", "refresh", move || refresh_all(c)).shortcut("F5"),
        ],
        "Transferts" => vec![
            MenuItem::action("Traiter la file", "play", move || toggle_queue(c))
                .checked(c.queue.running.get_untracked()),
            MenuItem::action("Annuler tous les transferts", "stop", move || cancel_all(c)),
        ],
        "Affichage" => vec![
            MenuItem::action("Fichiers cachés", "eye", move || act::toggle_hidden(c))
                .checked(c.settings.get_untracked().show_hidden)
                .shortcut("Ctrl+H"),
        ],
        "Édition" => vec![
            MenuItem::action("Paramètres", "settings", move || {
                c.open_dialog(Dialog::Settings);
            })
            .shortcut("Ctrl+,"),
        ],
        _ => vec![MenuItem::action("À propos", "info", move || {
            c.open_dialog(Dialog::About);
        })],
    }
}

fn open_menu_below(c: Ctx, ev: &web_sys::MouseEvent, items: Vec<MenuItem>) {
    ev.stop_propagation();
    if let Some(el) =
        crate::fmt::target_element(ev).and_then(|e| e.closest("button").ok().flatten())
    {
        let r = el.get_bounding_client_rect();
        c.menu.set(Some(MenuState {
            x: r.left(),
            y: r.bottom(),
            items,
        }));
    }
}

// ----------------------------------------------------------- composants
#[component]
fn MenuBar() -> impl IntoView {
    let c = ctx();
    view! {
        <div class="menubar">
            {["Fichier", "Serveur", "Transferts", "Affichage", "Édition", "Aide"].into_iter().map(|m| view! {
                <button class="menubar-item" on:mousedown=move |ev| open_menu_below(c, &ev, main_menu(c, m))>{m}</button>
            }).collect_view()}
        </div>
    }
}

#[component]
fn Toolbar() -> impl IntoView {
    let c = ctx();
    let has_session = move || {
        c.current_tracked()
            .is_some_and(|t| t.session.get().is_some())
    };
    let has_site = move || c.current_tracked().is_some_and(|t| t.site.get().is_some());
    view! {
        <div class="toolbar">
            <span class="split-btn">
                <button class="tool" title="Gestionnaire de sites (Ctrl+S)" on:click=move |_| open_site_manager(c)>{icon("server")}</button>
                <button class="tool drop" title="Sites enregistrés" on:mousedown=move |ev| open_menu_below(c, &ev, sites_menu_items(c))>{icon("chevron-down")}</button>
            </span>
            <button class="tool" title="Nouvel onglet (Ctrl+T)" on:click=move |_| { act::new_tab(c); }>{icon("tab-new")}</button>
            <span class="sep"></span>
            <button class="tool" title="Actualiser les listes de fichiers (F5)" on:click=move |_| refresh_all(c)>{icon("refresh")}</button>
            <button class="tool" title="Reconnecter (Ctrl+R)" disabled=move || !has_site() on:click=move |_| reconnect(c)>{icon("connect")}</button>
            <button class="tool" title="Déconnecter (Ctrl+Maj+D)" disabled=move || !has_session() on:click=move |_| disconnect(c)>{icon("disconnect")}</button>
            <span class="sep"></span>
            <button class="tool" class:toggled=move || !c.queue.running.get()
                title=move || if c.queue.running.get() { "Mettre en pause la file de transferts" } else { "Reprendre la file de transferts" }
                on:click=move |_| toggle_queue(c)>
                {move || if c.queue.running.get() { icon("pause").into_any() } else { icon("play").into_any() }}
            </button>
            <button class="tool" title="Annuler tous les transferts" on:click=move |_| cancel_all(c)>{icon("stop")}</button>
            <span class="sep"></span>
            <button class="tool" class:toggled=move || c.settings.with(|s| s.show_hidden) title="Afficher les fichiers cachés (Ctrl+H)"
                on:click=move |_| act::toggle_hidden(c)>{icon("eye")}</button>
            <button class="tool" title="Paramètres (Ctrl+,)" on:click=move |_| { c.open_dialog(Dialog::Settings); }>{icon("settings")}</button>
            <span class="grow"></span>
            <span class="brand">{logo(20)}<span>"RustSCP"</span></span>
        </div>
    }
}

#[component]
fn QuickConnect() -> impl IntoView {
    let c = ctx();
    let host = RwSignal::new(String::new());
    let user = RwSignal::new(String::new());
    let port = RwSignal::new(22u16);
    let auth = RwSignal::new(AUTH_CHOICES[0].0.to_string());
    let key = RwSignal::new(String::new());
    let host_input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        let k = c.info.with(|i| i.default_key.clone());
        if key.get_untracked().is_empty() {
            key.set(k);
        }
    });
    let key_enabled = move || {
        let a = auth.get();
        a.contains("key") || a == AUTH_INTERACTIVE
    };

    let connect = move || {
        let text = host.get_untracked().trim().to_string();
        if text.is_empty() {
            if let Some(el) = host_input.get_untracked() {
                let _ = el.focus();
            }
            return;
        }
        let mut text = text;
        for scheme in ["sftp://", "scp://", "ssh://"] {
            if let Some(rest) = text.strip_prefix(scheme) {
                text = rest.to_string();
            }
        }
        let mut u = user.get_untracked().trim().to_string();
        let mut p = port.get_untracked();
        if let Some((a, b)) = text.rsplit_once('@') {
            u = a.to_string();
            text = b.to_string();
        }
        if let Some(rest) = text.strip_prefix('[') {
            if let Some((h, pp)) = rest.split_once("]:")
                && let Ok(n) = pp.parse()
            {
                p = n;
                text = h.to_string();
            }
        } else if let Some((h, pp)) = text.rsplit_once(':')
            && !h.contains(':')
            && let Ok(n) = pp.parse()
        {
            p = n;
            text = h.to_string();
        }
        let u = if u.is_empty() {
            c.info.get_untracked().user
        } else {
            u
        };
        let site = Site {
            name: format!("{u}@{text}"),
            host: text.clone(),
            port: p,
            user: u.clone(),
            auth: auth.get_untracked(),
            key_file: key.get_untracked().trim().to_string(),
            protocol: PROTO_SCP.into(),
            id: format!("quick:{u}@{text}:{p}"),
            ..Default::default()
        };
        let entry = QuickEntry {
            host: site.host.clone(),
            port: p,
            user: u,
            auth: site.auth.clone(),
            key: site.key_file.clone(),
        };
        c.settings.update(|s| {
            s.quickconnect_history.retain(|h| *h != entry);
            s.quickconnect_history.insert(0, entry);
            s.quickconnect_history.truncate(10);
        });
        act::save_settings(c);
        act::connect_site(c, site);
    };
    let apply = move |h: QuickEntry| {
        host.set(h.host);
        user.set(h.user);
        port.set(h.port);
        auth.set(if h.auth.is_empty() {
            AUTH_CHOICES[0].0.to_string()
        } else {
            h.auth
        });
        key.set(h.key);
        connect();
    };
    let history = move |ev: web_sys::MouseEvent| {
        let hist = c.settings.get_untracked().quickconnect_history;
        let mut items: Vec<MenuItem> = hist
            .iter()
            .map(|h| {
                let h2 = h.clone();
                MenuItem::action(
                    format!(
                        "{}@{}:{}  —  {}",
                        h.user,
                        h.host,
                        h.port,
                        auth_label(&h.auth)
                    ),
                    "connect",
                    move || apply(h2.clone()),
                )
            })
            .collect();
        if items.is_empty() {
            items.push(MenuItem::Label("(aucune connexion récente)".into()));
        } else {
            items.push(MenuItem::Separator);
            items.push(MenuItem::action(
                "Effacer l'historique",
                "trash",
                move || {
                    c.settings.update(|s| s.quickconnect_history.clear());
                    act::save_settings(c);
                },
            ));
        }
        open_menu_below(c, &ev, items);
    };
    let browse = move |_| {
        spawn_local(async move {
            if let Ok(Some(p)) =
                call::<Option<String>>("pick_file", json!({ "start": "~/.ssh" })).await
            {
                key.set(p);
            }
        });
    };
    let enter = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Enter" {
            ev.prevent_default();
            connect();
        }
    };
    view! {
        <div class="quickconnect">
            <label>"Hôte :"</label>
            <input type="text" class="qc-host" placeholder="utilisateur@hôte:port" spellcheck="false" node_ref=host_input
                prop:value=host on:input=move |ev| host.set(event_target_value(&ev)) on:keydown=enter/>
            <label>"Identifiant :"</label>
            <input type="text" class="qc-user" placeholder=move || c.info.with(|i| i.user.clone()) spellcheck="false"
                prop:value=user on:input=move |ev| user.set(event_target_value(&ev)) on:keydown=enter/>
            <label>"Port :"</label>
            <input type="number" class="qc-port" min="1" max="65535" prop:value=move || port.get().to_string()
                on:input=move |ev| { if let Ok(p) = event_target_value(&ev).parse::<u16>() { port.set(p.max(1)) } }/>
            <label>"Auth. :"</label>
            <select on:change=move |ev| auth.set(event_target_value(&ev))>
                {AUTH_CHOICES.iter().map(|(k, l)| view! { <option value=*k prop:selected=move || auth.get() == *k>{*l}</option> }).collect_view()}
            </select>
            <label>"Clé :"</label>
            <input type="text" class="qc-key" placeholder="~/.ssh/id_ed25519" spellcheck="false" disabled=move || !key_enabled()
                prop:value=key on:input=move |ev| key.set(event_target_value(&ev)) on:keydown=enter/>
            <button class="icon-btn" title="Choisir une clé privée" disabled=move || !key_enabled() on:click=browse>"…"</button>
            <button class="primary qc-go" on:click=move |_| connect()>{icon("connect")}"Connexion rapide"</button>
            <button class="icon-btn" title="Connexions récentes" on:mousedown=history>{icon("chevron-down")}</button>
        </div>
    }
}

#[component]
fn LogPane() -> impl IntoView {
    let c = ctx();
    let node = NodeRef::<html::Div>::new();
    let stick = StoredValue::new(true);
    Effect::new(move |_| {
        c.logs.track();
        if stick.get_value()
            && let Some(el) = node.get_untracked()
        {
            request_animation_frame(move || el.set_scroll_top(el.scroll_height()));
        }
    });
    let label = |level: &str| match level {
        "command" => "Commande",
        "response" => "Réponse",
        "error" => "Erreur",
        "trace" => "Trace",
        _ => "Statut",
    };
    view! {
        <div class="log selectable" node_ref=node
            on:scroll=move |ev| {
                let el: web_sys::Element = ev.target().unwrap().unchecked_into();
                stick.set_value(el.scroll_top() + el.client_height() >= el.scroll_height() - 4);
            }
            on:contextmenu=move |ev| {
                ev.prevent_default();
                let items = vec![
                    MenuItem::action("Copier le journal", "copy", move || {
                        let text = c.logs.with_untracked(|l| l.iter().map(|e| format!("{}  {:<9} {}", e.line.time, label(&e.line.level), e.line.message)).collect::<Vec<_>>().join("\n"));
                        act::copy_text(text);
                    }),
                    MenuItem::action("Effacer le journal", "trash", move || c.logs.set(Vec::new())),
                ];
                c.menu.set(Some(MenuState { x: ev.client_x() as f64, y: ev.client_y() as f64, items }));
            }>
            <For each=move || c.logs.get() key=|e| e.id children=move |e| {
                let LogLine { time, level, message } = e.line;
                view! {
                    <div class=format!("log-line log-{level}")>
                        <span class="log-time">{time}</span>
                        <span class="log-level">{label(&level)}</span>
                        <span class="log-msg">{message}</span>
                    </div>
                }
            }/>
        </div>
    }
}

#[component]
fn TabsBar() -> impl IntoView {
    let c = ctx();
    view! {
        <div class="tabs-bar session-tabs">
            <For each=move || c.tabs.get() key=|t| t.id children=move |t| {
                let id = t.id;
                view! {
                    <div class="session-tab" class:active=move || c.active.get() == id
                        title=move || t.site.get().map(|s| s.address()).unwrap_or_default()
                        on:mousedown=move |ev| {
                            if ev.button() == 1 { ev.prevent_default(); act::close_tab(c, id); }
                            else if ev.button() == 0 { c.active.set(id); }
                        }>
                        <span class=move || match t.conn.get() {
                            ConnState::Connected => "dot ok", ConnState::Connecting => "dot wait",
                            ConnState::Lost => "dot lost", ConnState::Idle => "dot",
                        }></span>
                        <span class="tab-title">{move || t.title()}</span>
                        <button class="tab-close" title="Fermer l'onglet (Ctrl+W)"
                            on:mousedown=|ev| ev.stop_propagation()
                            on:click=move |_| act::close_tab(c, id)>{icon("close")}</button>
                    </div>
                }
            }/>
            <button class="icon-btn new-tab" title="Nouvel onglet (Ctrl+T)" on:click=move |_| { act::new_tab(c); }>{icon("plus")}</button>
        </div>
    }
}

#[component]
fn TabContent() -> impl IntoView {
    let c = ctx();
    let lay = layout();
    let container = NodeRef::<html::Div>::new();
    // largeur initiale : moitié de la zone
    Effect::new(move |_| {
        if let Some(el) = container.get() {
            request_animation_frame(move || {
                let w = el.client_width() as f64;
                if lay.left_w.get_untracked() < 100.0 && w > 0.0 {
                    lay.left_w.set((w / 2.0).max(220.0));
                }
            });
        }
    });
    view! {
        <div class="tab-content" node_ref=container>
            {move || c.current_tracked().map(|t| view! {
                <div class="pane-col" style:flex-basis=move || { let w = lay.left_w.get(); if w < 100.0 { "50%".to_string() } else { format!("{w}px") } }>
                    <PaneView tab=t side=crate::state::Side::Local/>
                </div>
                <Splitter axis=Axis::Horizontal size=lay.left_w min=220.0/>
                <div class="pane-col grow">
                    <PaneView tab=t side=crate::state::Side::Remote/>
                </div>
            })}
        </div>
    }
}

#[component]
fn StatusBar() -> impl IntoView {
    let c = ctx();
    let conn = move || match c.current_tracked() {
        None => "Non connecté".to_string(),
        Some(t) => match t.site.get() {
            None => "Non connecté".into(),
            Some(site) => match t.conn.get() {
                ConnState::Connecting => format!("Connexion à {}…", site.label()),
                ConnState::Connected => {
                    let mut proto = site.protocol.to_uppercase();
                    if site.protocol == PROTO_SCP && t.shell_ok.get() == Some(false) {
                        proto = "SFTP (repli : SCP indisponible)".into();
                    }
                    format!("Connecté à {} — {proto}", site.label())
                }
                _ => format!("Déconnecté de {}", site.label()),
            },
        },
    };
    let transfers = move || {
        let s = c.queue.stats.get();
        let mut parts = Vec::new();
        if s.queued > 0 || s.active > 0 {
            parts.push(format!("{} actif(s), {} en attente", s.active, s.queued));
        }
        if s.speed > 0.0 {
            parts.push(human_speed(s.speed));
        }
        if !c.queue.running.get() {
            parts.push("file en pause".into());
        }
        parts.join("   |   ")
    };
    view! {
        <div class="statusbar">
            <span class="status-conn">{conn}</span>
            <span class="grow"></span>
            <span>{transfers}</span>
        </div>
    }
}

fn menu_items_view(c: Ctx, items: Vec<MenuItem>) -> impl IntoView {
    items
        .into_iter()
        .map(|item| match item {
            MenuItem::Separator => view! { <div class="menu-sep"></div> }.into_any(),
            MenuItem::Label(text) => {
                view! { <div class="menu-item disabled">{text}</div> }.into_any()
            }
            MenuItem::Action {
                label,
                icon: ic,
                shortcut,
                enabled,
                checked,
                run,
            } => view! {
                <div class="menu-item" class:disabled=!enabled
                    on:mousedown=|ev| ev.stop_propagation()
                    on:click=move |_| if enabled { c.menu.set(None); run(); }>
                    <span class="menu-ic">{match checked {
                        Some(true) => icon("check").into_any(),
                        Some(false) => ().into_any(),
                        None => icon(ic).into_any(),
                    }}</span>
                    <span class="menu-label">{label}</span>
                    <span class="menu-shortcut">{shortcut}</span>
                </div>
            }
            .into_any(),
            MenuItem::Sub {
                label,
                icon: ic,
                items,
            } => view! {
                <div class="menu-item has-sub" on:mousedown=|ev| ev.stop_propagation()>
                    <span class="menu-ic">{icon(ic)}</span>
                    <span class="menu-label">{label}</span>
                    <span class="menu-shortcut">{icon("chevron-right")}</span>
                    <div class="menu submenu">{menu_items_view(c, items)}</div>
                </div>
            }
            .into_any(),
        })
        .collect_view()
}

#[component]
fn ContextMenu() -> impl IntoView {
    let c = ctx();
    let node = NodeRef::<html::Div>::new();
    let pos = RwSignal::new((0.0, 0.0));
    // garde le menu dans la fenêtre
    Effect::new(move |_| {
        if let Some(m) = c.menu.get() {
            pos.set((m.x, m.y));
            request_animation_frame(move || {
                if let Some(el) = node.get_untracked() {
                    let r = el.get_bounding_client_rect();
                    let w = window();
                    let (vw, vh) = (
                        w.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(1e4),
                        w.inner_height()
                            .ok()
                            .and_then(|v| v.as_f64())
                            .unwrap_or(1e4),
                    );
                    let x = if m.x + r.width() > vw {
                        (vw - r.width() - 4.0).max(0.0)
                    } else {
                        m.x
                    };
                    let y = if m.y + r.height() > vh {
                        (vh - r.height() - 4.0).max(0.0)
                    } else {
                        m.y
                    };
                    pos.set((x, y));
                }
            });
        }
    });
    view! {
        {move || c.menu.get().map(|m| view! {
            <div class="menu" node_ref=node style:left=move || format!("{}px", pos.get().0) style:top=move || format!("{}px", pos.get().1)
                on:contextmenu=|ev| ev.prevent_default()>
                {menu_items_view(c, m.items)}
            </div>
        })}
    }
}

#[component]
fn DragGhost() -> impl IntoView {
    let c = ctx();
    view! {
        {move || c.drag.get().filter(|d| d.active).map(|d| view! {
            <div class="drag-ghost" style:left=format!("{}px", d.pos.0 + 14.0) style:top=format!("{}px", d.pos.1 + 10.0)>
                {icon(match d.payload { DragPayload::Local { .. } => "upload", DragPayload::Remote { .. } => "download" })}
                <span>{d.label}</span>
            </div>
        })}
    }
}

// ------------------------------------------------------- glisser-déposer
fn drop_target(x: f64, y: f64) -> Option<(String, u64, String, web_sys::Element)> {
    let el = closest_with(element_at(x, y), "[data-drop]")?;
    let kind = el.get_attribute("data-drop")?;
    let tab = el.get_attribute("data-tab")?.parse().ok()?;
    let path = el.get_attribute("data-path").unwrap_or_default();
    Some((kind, tab, path, el))
}

fn set_hover(el: Option<web_sys::Element>) {
    HOVER.with(|h| {
        let mut h = h.borrow_mut();
        if let Some(old) = h.take() {
            let _ = old.class_list().remove_1("drop-hover");
        }
        if let Some(e) = el {
            let _ = e.class_list().add_1("drop-hover");
            *h = Some(e);
        }
    });
}

fn drop_compatible(payload: &DragPayload, kind: &str, tab: u64) -> bool {
    match payload {
        DragPayload::Local { tab: t, .. } => kind == "remote" && *t == tab,
        DragPayload::Remote { tab: t, .. } => *t == tab && (kind == "local" || kind == "remote"),
    }
}

fn finish_drag(c: Ctx, x: f64, y: f64) {
    set_hover(None);
    let Some(d) = c.drag.get_untracked() else {
        return;
    };
    c.drag.set(None);
    if !d.active {
        return;
    }
    DRAG_ENDED.with(|f| f.set(true));
    set_timeout(
        move || DRAG_ENDED.with(|f| f.set(false)),
        Duration::from_millis(50),
    );
    let Some((kind, tab_id, path, _)) = drop_target(x, y) else {
        return;
    };
    let Some(tab) = c.tab(tab_id) else { return };
    match d.payload {
        DragPayload::Local { tab: t, paths } if t == tab_id && kind == "remote" => {
            act::upload(c, tab, paths, Some(path))
        }
        DragPayload::Remote { tab: t, items } if t == tab_id && kind == "local" => {
            act::download(c, tab, items, Some(path))
        }
        DragPayload::Remote { tab: t, items } if t == tab_id && kind == "remote" => {
            act::remote_move(c, tab, items, path)
        }
        _ => {}
    }
}

#[derive(Deserialize)]
struct Position {
    x: f64,
    y: f64,
}

#[derive(Deserialize)]
struct OsDrop {
    paths: Vec<String>,
    position: Position,
}

/// Fichiers déposés depuis le gestionnaire de fichiers du bureau.
fn os_drop(c: Ctx, ev: OsDrop) {
    let ratio = window().device_pixel_ratio().max(1.0);
    let (x, y) = (ev.position.x / ratio, ev.position.y / ratio);
    let target = drop_target(x, y).or_else(|| drop_target(ev.position.x, ev.position.y));
    match target {
        Some((kind, tab_id, path, _)) if kind == "remote" => {
            if let Some(tab) = c.tab(tab_id) {
                act::upload(c, tab, ev.paths, Some(path));
            }
        }
        _ => {
            if let Some(tab) = c.current().filter(|t| t.connected()) {
                c.log(
                    "status",
                    "Déposez les fichiers sur le panneau distant pour les envoyer.",
                );
                let _ = tab;
            }
        }
    }
}

// ------------------------------------------------------------- raccourcis
fn global_keydown(c: Ctx, ev: web_sys::KeyboardEvent) {
    if c.modal_open() {
        return;
    }
    let ctrl = ev.ctrl_key() || ev.meta_key();
    let shift = ev.shift_key();
    let key = ev.key();
    let handled = match (ctrl, shift, key.as_str()) {
        (true, false, "s" | "S") => {
            open_site_manager(c);
            true
        }
        (true, false, "t" | "T") => {
            act::new_tab(c);
            true
        }
        (true, false, "w" | "W") => {
            close_current_tab(c);
            true
        }
        (true, false, "q" | "Q") => {
            act::quit(c);
            true
        }
        (true, false, "r" | "R") => {
            reconnect(c);
            true
        }
        (true, true, "d" | "D") => {
            disconnect(c);
            true
        }
        (true, false, "h" | "H") => {
            act::toggle_hidden(c);
            true
        }
        (true, false, ",") => {
            c.open_dialog(Dialog::Settings);
            true
        }
        (false, _, "F5") => {
            refresh_all(c);
            true
        }
        (false, _, "Escape") => {
            c.menu.set(None);
            false
        }
        _ => false,
    };
    if handled {
        ev.prevent_default();
        c.menu.set(None);
    } else if !in_text_field(&ev)
        && ctrl
        && matches!(key.as_str(), "p" | "P" | "f" | "F" | "g" | "G" | "u" | "U")
    {
        ev.prevent_default(); // pas d'impression / recherche du navigateur
    }
}

// ---------------------------------------------------------- application
#[component]
pub fn App() -> impl IntoView {
    let c = Ctx::new();
    crate::state::init_ctx(c);
    let lay = Layout::load();
    crate::layout::set_layout(lay);

    // chargement initial
    spawn_local(async move {
        if let Ok(info) = call::<AppInfo>("app_info", json!({})).await {
            c.info.set(info);
        }
        if let Ok(s) = call::<Settings>("settings_get", json!({})).await {
            c.settings.set(s);
        }
        if let Ok(sites) = call::<SitesFile>("sites_get", json!({})).await {
            c.sites.set(sites);
        }
        let info = c.info.get_untracked();
        act::new_tab(c);
        let backend = info.secrets.map(|s| s.name).unwrap_or_default();
        c.log(
            "status",
            format!("{} {} prêt. Secrets : {backend}", info.name, info.version),
        );
    });

    // événements du cœur
    listen::<LogLine>("log", move |line| c.push_log(line));
    listen::<PromptEvent>("prompt", move |p| {
        c.open_dialog(Dialog::Prompt {
            id: p.id,
            request: p.request,
        });
    });
    listen::<TransferFinished>("transfer-finished", move |ev| {
        let Some(tab) = c.tab_for_session(ev.session_id) else {
            return;
        };
        match ev.direction {
            Direction::Up => act::schedule_refresh(c, tab, &rparent(&ev.remote)),
            Direction::Down => {
                if rparent(&ev.local) == tab.local.path.get_untracked() {
                    act::schedule_local_refresh(c, tab);
                }
            }
        }
    });
    listen::<EditChanged>("edit-changed", move |ev| {
        if c.tab_for_session(ev.session_id).is_none() {
            return;
        }
        spawn_local(async move {
            let text = format!(
                "Le fichier {} a été modifié.\n\nRenvoyer sur le serveur ?\n{}",
                rbasename(&ev.remote),
                ev.remote
            );
            if confirm("Fichier modifié", &text).await {
                fire(
                    "reupload",
                    json!({ "id": ev.session_id, "local": ev.local, "remote": ev.remote }),
                );
            }
        });
    });
    listen::<u64>("close-requested", move |_| act::quit(c));
    listen::<OsDrop>("tauri://drag-drop", move |ev| os_drop(c, ev));

    start_polling(c);
    start_local_watch(c);
    let _ = set_interval_with_handle(move || act::poll_sessions(c), Duration::from_secs(2));

    // gestionnaires globaux
    let _ = window_event_listener(leptos::ev::keydown, move |ev| global_keydown(c, ev));
    let _ = window_event_listener(leptos::ev::mousemove, move |ev| {
        if crate::layout::on_mouse_move(&ev) {
            return;
        }
        if let Some(mut d) = c.drag.get_untracked() {
            if ev.buttons() & 1 == 0 {
                c.drag.set(None);
                return;
            }
            let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
            if !d.active && ((x - d.start.0).abs() + (y - d.start.1).abs()) < 6.0 {
                return;
            }
            d.active = true;
            d.pos = (x, y);
            let hover = drop_target(x, y)
                .filter(|(k, t, _, _)| drop_compatible(&d.payload, k, *t))
                .map(|(_, _, _, el)| el);
            set_hover(hover);
            c.drag.set(Some(d));
        }
    });
    let _ = window_event_listener(leptos::ev::mouseup, move |ev| {
        if crate::layout::on_mouse_up() {
            return;
        }
        finish_drag(c, ev.client_x() as f64, ev.client_y() as f64);
    });
    let _ = window_event_listener(leptos::ev::mousedown, move |_| {
        if c.menu.get_untracked().is_some() {
            c.menu.set(None);
        }
    });
    let _ = window_event_listener(leptos::ev::contextmenu, move |ev| {
        if !in_text_field(&ev) {
            ev.prevent_default();
        }
    });
    let _ = window_event_listener(leptos::ev::blur, move |_| c.menu.set(None));

    view! {
        <div class="app" class:dragging=move || c.drag.with(|d| d.as_ref().is_some_and(|d| d.active))>
            <MenuBar/>
            <Toolbar/>
            <QuickConnect/>
            <div class="log-wrap" style:height=move || format!("{}px", lay.log_h.get())>
                <LogPane/>
            </div>
            <Splitter axis=Axis::Vertical size=lay.log_h min=40.0/>
            <div class="workspace">
                <TabsBar/>
                <TabContent/>
            </div>
            <Splitter axis=Axis::Vertical size=lay.queue_h sign=-1.0 min=60.0/>
            <div class="queue-wrap" style:height=move || format!("{}px", lay.queue_h.get())>
                <QueuePanel/>
            </div>
            <StatusBar/>
            <ContextMenu/>
            <DragGhost/>
            <DialogHost/>
        </div>
    }
}
