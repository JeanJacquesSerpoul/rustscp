//! État global de l'interface (signaux Leptos).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use futures::channel::oneshot;
use leptos::prelude::*;
use rustscp_shared::tr;
use rustscp_shared::util::name_matches;
use rustscp_shared::{
    Entry, LogLine, PromptRequest, SecretBackend, Settings, Site, SitesFile, TransferPoll,
    TransferStats, TransferView,
};
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub secrets: Option<SecretBackend>,
    pub default_key: String,
    pub home: String,
    pub user: String,
    /// Langue effective choisie par le cœur (fr | en).
    pub lang: String,
}

// ------------------------------------------------------------------ journal
#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry {
    pub id: u64,
    pub line: LogLine,
}

pub const LOG_MAX: usize = 3000;

// ----------------------------------------------------------------- panneaux
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Local,
    Remote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Col {
    Name,
    Size,
    Type,
    Modified,
    Perms,
    Owner,
}

impl Col {
    pub fn title(self) -> &'static str {
        match self {
            Col::Name => tr!("Nom", "Name"),
            Col::Size => tr!("Taille", "Size"),
            Col::Type => "Type",
            Col::Modified => tr!("Modifié", "Modified"),
            Col::Perms => tr!("Droits", "Permissions"),
            Col::Owner => tr!("Propriétaire/Groupe", "Owner/Group"),
        }
    }
}

pub const PARENT: &str = "..";

/// Une liste de fichiers (locale ou distante) : données, tri, filtre, sélection.
#[derive(Clone, Copy)]
pub struct Pane {
    pub side: Side,
    pub path: RwSignal<String>,
    pub path_text: RwSignal<String>,
    pub entries: RwSignal<Arc<Vec<Entry>>>,
    pub filter: RwSignal<String>,
    pub sort: RwSignal<(Col, bool)>,
    pub selected: RwSignal<HashSet<String>>,
    pub anchor: RwSignal<Option<String>>,
    pub cursor: RwSignal<Option<String>>,
    /// Message d'état prioritaire (lecture en cours, erreur…).
    pub busy: RwSignal<Option<String>>,
    pub scroll: RwSignal<f64>,
    pub widths: RwSignal<Vec<f64>>,
    pub rows: Memo<Arc<Vec<Entry>>>,
}

impl Pane {
    pub fn new(side: Side, show_hidden: Signal<bool>) -> Self {
        let path = RwSignal::new(String::new());
        let entries = RwSignal::new(Arc::new(Vec::<Entry>::new()));
        let filter = RwSignal::new(String::new());
        let sort = RwSignal::new((Col::Name, true));
        let rows = Memo::new(move |_| {
            let hidden = show_hidden.get();
            let flt = filter.with(|f| f.trim().to_lowercase());
            let (col, asc) = sort.get();
            let at_root = path.with(|p| p.is_empty() || p == "/");
            entries.with(|all| {
                let mut rows: Vec<Entry> = all
                    .iter()
                    .filter(|e| hidden || !e.name.starts_with('.'))
                    .filter(|e| name_matches(&flt, &e.name))
                    .cloned()
                    .collect();
                sort_entries(&mut rows, col, asc);
                if !at_root {
                    rows.insert(
                        0,
                        Entry {
                            name: PARENT.into(),
                            is_dir: true,
                            mode: 0o040755,
                            ..Default::default()
                        },
                    );
                }
                Arc::new(rows)
            })
        });
        let widths = match side {
            Side::Local => vec![260.0, 90.0, 140.0, 150.0],
            Side::Remote => vec![240.0, 90.0, 120.0, 150.0, 95.0, 140.0],
        };
        Pane {
            side,
            path,
            path_text: RwSignal::new(String::new()),
            entries,
            filter,
            sort,
            selected: RwSignal::new(HashSet::new()),
            anchor: RwSignal::new(None),
            cursor: RwSignal::new(None),
            busy: RwSignal::new(None),
            scroll: RwSignal::new(0.0),
            widths: RwSignal::new(widths),
            rows,
        }
    }

    pub fn columns(&self) -> &'static [Col] {
        match self.side {
            Side::Local => &[Col::Name, Col::Size, Col::Type, Col::Modified],
            Side::Remote => &[
                Col::Name,
                Col::Size,
                Col::Type,
                Col::Modified,
                Col::Perms,
                Col::Owner,
            ],
        }
    }

    /// Entrées sélectionnées (sans la ligne « .. »), dans l'ordre d'affichage.
    pub fn selected_entries(&self) -> Vec<Entry> {
        let sel = self.selected.get_untracked();
        self.rows.with_untracked(|rows| {
            rows.iter()
                .filter(|e| e.name != PARENT && sel.contains(&e.name))
                .cloned()
                .collect()
        })
    }

    pub fn clear_selection(&self) {
        self.selected.set(HashSet::new());
        self.anchor.set(None);
        self.cursor.set(None);
    }
}

pub fn sort_entries(rows: &mut [Entry], col: Col, asc: bool) {
    use rustscp_shared::util::file_type_for;
    let key_name = |e: &Entry| e.name.to_lowercase();
    match col {
        Col::Name => rows.sort_by_cached_key(key_name),
        Col::Size => rows.sort_by_cached_key(|e| (e.size, key_name(e))),
        Col::Type => rows.sort_by_cached_key(|e| {
            (
                if e.is_dir {
                    String::new()
                } else {
                    file_type_for(&e.name).to_lowercase()
                },
                key_name(e),
            )
        }),
        Col::Modified => rows.sort_by_cached_key(|e| (e.mtime, key_name(e))),
        Col::Perms => rows.sort_by_cached_key(|e| (e.mode, key_name(e))),
        Col::Owner => rows.sort_by_cached_key(|e| (e.owner.clone(), e.group.clone(), key_name(e))),
    }
    if !asc {
        rows.reverse();
    }
    // les dossiers restent toujours en tête
    rows.sort_by_key(|e| !e.is_dir);
}

/// Arborescence de dossiers chargée à la demande.
#[derive(Clone, Copy)]
pub struct Tree {
    /// Sous-dossiers connus de chaque dossier.
    pub children: RwSignal<HashMap<String, Vec<String>>>,
    /// Dossiers listés complètement (les autres ne connaissent qu'une partie de leurs enfants).
    pub loaded: RwSignal<HashSet<String>>,
    pub expanded: RwSignal<HashSet<String>>,
    pub selected: RwSignal<String>,
    pub scroll: RwSignal<f64>,
}

impl Tree {
    pub fn new() -> Self {
        Tree {
            children: RwSignal::new(HashMap::new()),
            loaded: RwSignal::new(HashSet::new()),
            expanded: RwSignal::new(HashSet::new()),
            selected: RwSignal::new(String::new()),
            scroll: RwSignal::new(0.0),
        }
    }

    pub fn clear(&self) {
        self.children.set(HashMap::new());
        self.loaded.set(HashSet::new());
        self.expanded.set(HashSet::new());
        self.selected.set(String::new());
    }
}

// ------------------------------------------------------------------- onglets
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Idle,
    Connecting,
    Connected,
    Lost,
}

#[derive(Clone, Copy)]
pub struct Tab {
    pub id: u64,
    pub site: RwSignal<Option<Site>>,
    pub session: RwSignal<Option<u64>>,
    pub conn: RwSignal<ConnState>,
    pub shell_ok: RwSignal<Option<bool>>,
    pub home: RwSignal<String>,
    pub local: Pane,
    pub remote: Pane,
    pub local_tree: Tree,
    pub remote_tree: Tree,
    pub cache: StoredValue<HashMap<String, Arc<Vec<Entry>>>>,
    pub nav_token: StoredValue<u64>,
    pub refresh_timer: StoredValue<Option<TimeoutHandle>>,
    pub local_timer: StoredValue<Option<TimeoutHandle>>,
    /// Terminal à ouvrir dès la connexion établie (dossier de départ éventuel).
    pub pending_terminal: StoredValue<Option<Option<String>>>,
}

impl Tab {
    pub fn new(id: u64, show_hidden: Signal<bool>) -> Self {
        Tab {
            id,
            site: RwSignal::new(None),
            session: RwSignal::new(None),
            conn: RwSignal::new(ConnState::Idle),
            shell_ok: RwSignal::new(None),
            home: RwSignal::new("/".into()),
            local: Pane::new(Side::Local, show_hidden),
            remote: Pane::new(Side::Remote, show_hidden),
            local_tree: Tree::new(),
            remote_tree: Tree::new(),
            cache: StoredValue::new(HashMap::new()),
            nav_token: StoredValue::new(0),
            refresh_timer: StoredValue::new(None),
            local_timer: StoredValue::new(None),
            pending_terminal: StoredValue::new(None),
        }
    }

    pub fn pane(&self, side: Side) -> Pane {
        match side {
            Side::Local => self.local,
            Side::Remote => self.remote,
        }
    }

    pub fn tree(&self, side: Side) -> Tree {
        match side {
            Side::Local => self.local_tree,
            Side::Remote => self.remote_tree,
        }
    }

    pub fn connected(&self) -> bool {
        self.conn.get() == ConnState::Connected
    }

    /// Terminal SSH possible : site SSH (pas FTP) acceptant les commandes.
    pub fn terminal_ok(&self) -> bool {
        self.site
            .get()
            .is_some_and(|s| !rustscp_shared::proto_is_ftp(&s.protocol))
            && self.shell_ok.get() != Some(false)
    }

    pub fn title(&self) -> String {
        match self.site.get() {
            None => tr!("Nouvel onglet", "New tab").into(),
            Some(site) => {
                let prefix = match self.conn.get() {
                    ConnState::Connecting => "⏳ ",
                    ConnState::Connected => "",
                    _ => "✖ ",
                };
                format!("{prefix}{}", site.label())
            }
        }
    }
}

// ------------------------------------------------------------ transferts
#[derive(Clone, Copy)]
pub struct Queue {
    pub items: RwSignal<Vec<TransferView>>,
    pub index: StoredValue<HashMap<u64, usize>>,
    pub version: StoredValue<u64>,
    pub stats: RwSignal<TransferStats>,
    pub running: RwSignal<bool>,
    pub tab: RwSignal<usize>,
    pub selected: RwSignal<HashSet<u64>>,
    pub anchor: RwSignal<Option<u64>>,
    pub scroll: RwSignal<f64>,
    pub groups: [Memo<Arc<Vec<u32>>>; 3],
}

impl Queue {
    pub fn new() -> Self {
        let items: RwSignal<Vec<TransferView>> = RwSignal::new(Vec::new());
        let group = move |g: usize| {
            Memo::new(move |_| {
                items.with(|v| {
                    Arc::new(
                        v.iter()
                            .enumerate()
                            .filter(|(_, t)| t.status.group() == g)
                            .map(|(i, _)| i as u32)
                            .collect::<Vec<u32>>(),
                    )
                })
            })
        };
        Queue {
            items,
            index: StoredValue::new(HashMap::new()),
            version: StoredValue::new(0),
            stats: RwSignal::new(TransferStats::default()),
            running: RwSignal::new(true),
            tab: RwSignal::new(0),
            selected: RwSignal::new(HashSet::new()),
            anchor: RwSignal::new(None),
            scroll: RwSignal::new(0.0),
            groups: [group(0), group(1), group(2)],
        }
    }

    pub fn apply(&self, poll: TransferPoll) {
        self.version.set_value(poll.version);
        if self.stats.get_untracked() != poll.stats {
            self.stats.set(poll.stats.clone());
        }
        if self.running.get_untracked() != poll.running {
            self.running.set(poll.running);
        }
        if !poll.reset
            && poll.upserts.is_empty()
            && poll.removed.is_empty()
            && poll.progress.is_empty()
        {
            return;
        }
        let index = self.index;
        let rebuild = |items: &Vec<TransferView>| {
            index.set_value(items.iter().enumerate().map(|(i, t)| (t.id, i)).collect());
        };
        self.items.update(|items| {
            if poll.reset {
                *items = poll.upserts;
                items.sort_by_key(|t| t.id);
                rebuild(items);
            } else {
                if !poll.removed.is_empty() {
                    let gone: HashSet<u64> = poll.removed.iter().copied().collect();
                    items.retain(|t| !gone.contains(&t.id));
                    rebuild(items);
                }
                for up in poll.upserts {
                    match index.with_value(|ix| ix.get(&up.id).copied()) {
                        Some(i) => items[i] = up,
                        None if items.last().is_none_or(|l| l.id < up.id) => {
                            index.update_value(|ix| {
                                ix.insert(up.id, items.len());
                            });
                            items.push(up);
                        }
                        None => {
                            let at = items.partition_point(|t| t.id < up.id);
                            items.insert(at, up);
                            rebuild(items);
                        }
                    }
                }
            }
            for p in poll.progress {
                if let Some(i) = index.with_value(|ix| ix.get(&p.id).copied())
                    && let Some(t) = items.get_mut(i)
                {
                    t.transferred = p.transferred;
                    t.size = p.size;
                    t.speed = p.speed;
                }
            }
        });
    }
}

// ------------------------------------------------------------- dialogues
pub type Reply<T> = Arc<Mutex<Option<oneshot::Sender<T>>>>;

pub fn reply<T>(r: &Reply<T>, value: T) {
    if let Some(tx) = r.lock().unwrap().take() {
        let _ = tx.send(value);
    }
}

#[derive(Clone)]
pub enum Dialog {
    Confirm {
        title: String,
        text: String,
        yes: String,
        no: String,
        tx: Reply<bool>,
    },
    Text {
        title: String,
        label: String,
        value: String,
        tx: Reply<Option<String>>,
    },
    Alert {
        title: String,
        text: String,
        error: bool,
        tx: Reply<()>,
    },
    Prompt {
        id: u64,
        request: PromptRequest,
    },
    Chmod {
        title: String,
        mode: u32,
        has_dirs: bool,
        tx: Reply<Option<(u32, bool)>>,
    },
    SiteManager {
        select: Option<String>,
    },
    Settings,
    About,
}

#[derive(Clone)]
pub struct DialogEntry {
    pub id: u64,
    pub dialog: Dialog,
}

// --------------------------------------------------------------- menus
pub type Action = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub enum MenuItem {
    Action {
        label: String,
        icon: &'static str,
        shortcut: &'static str,
        enabled: bool,
        checked: Option<bool>,
        run: Action,
    },
    Sub {
        label: String,
        icon: &'static str,
        items: Vec<MenuItem>,
    },
    Separator,
    Label(String),
}

impl MenuItem {
    pub fn action(
        label: impl Into<String>,
        icon: &'static str,
        run: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        MenuItem::Action {
            label: label.into(),
            icon,
            shortcut: "",
            enabled: true,
            checked: None,
            run: Arc::new(run),
        }
    }

    pub fn enabled(mut self, on: bool) -> Self {
        if let MenuItem::Action { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }

    pub fn shortcut(mut self, s: &'static str) -> Self {
        if let MenuItem::Action { shortcut, .. } = &mut self {
            *shortcut = s;
        }
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        if let MenuItem::Action { checked, .. } = &mut self {
            *checked = Some(on);
        }
        self
    }
}

#[derive(Clone)]
pub struct MenuState {
    pub x: f64,
    pub y: f64,
    pub items: Vec<MenuItem>,
}

// ------------------------------------------------------- glisser-déposer
#[derive(Clone, Debug, PartialEq)]
pub enum DragPayload {
    Local {
        tab: u64,
        paths: Vec<String>,
    },
    Remote {
        tab: u64,
        items: Vec<rustscp_shared::RemoteItem>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct DragState {
    pub payload: DragPayload,
    pub label: String,
    pub start: (f64, f64),
    pub pos: (f64, f64),
    pub active: bool,
}

// ------------------------------------------------------------- contexte
#[derive(Clone, Copy)]
pub struct Ctx {
    pub info: RwSignal<AppInfo>,
    pub settings: RwSignal<Settings>,
    pub show_hidden: Signal<bool>,
    pub sites: RwSignal<SitesFile>,
    pub tabs: RwSignal<Vec<Tab>>,
    pub active: RwSignal<u64>,
    pub next_tab: StoredValue<u64>,
    pub logs: RwSignal<Vec<LogEntry>>,
    pub log_seq: StoredValue<u64>,
    pub queue: Queue,
    pub dialogs: RwSignal<Vec<DialogEntry>>,
    pub dialog_seq: StoredValue<u64>,
    pub menu: RwSignal<Option<MenuState>>,
    pub drag: RwSignal<Option<DragState>>,
    /// Dernière action choisie dans la boîte « fichier existant ».
    pub last_overwrite: StoredValue<String>,
}

impl Ctx {
    pub fn new() -> Self {
        let settings = RwSignal::new(Settings::default());
        Ctx {
            info: RwSignal::new(AppInfo::default()),
            settings,
            show_hidden: Signal::derive(move || settings.with(|s| s.show_hidden)),
            sites: RwSignal::new(SitesFile::default()),
            tabs: RwSignal::new(Vec::new()),
            active: RwSignal::new(0),
            next_tab: StoredValue::new(1),
            logs: RwSignal::new(Vec::new()),
            log_seq: StoredValue::new(0),
            queue: Queue::new(),
            dialogs: RwSignal::new(Vec::new()),
            dialog_seq: StoredValue::new(1),
            menu: RwSignal::new(None),
            drag: RwSignal::new(None),
            last_overwrite: StoredValue::new("overwrite".into()),
        }
    }

    pub fn tab(&self, id: u64) -> Option<Tab> {
        self.tabs
            .with_untracked(|t| t.iter().find(|t| t.id == id).copied())
    }

    pub fn current(&self) -> Option<Tab> {
        let id = self.active.get_untracked();
        self.tab(id)
    }

    pub fn current_tracked(&self) -> Option<Tab> {
        let id = self.active.get();
        self.tabs.with(|t| t.iter().find(|t| t.id == id).copied())
    }

    pub fn tab_for_session(&self, session: u64) -> Option<Tab> {
        self.tabs.with_untracked(|t| {
            t.iter()
                .find(|t| t.session.get_untracked() == Some(session))
                .copied()
        })
    }

    pub fn log(&self, level: &str, message: impl Into<String>) {
        let line = LogLine {
            time: crate::fmt::now_hms(),
            level: level.into(),
            message: message.into(),
        };
        self.push_log(line);
    }

    pub fn push_log(&self, line: LogLine) {
        let id = self.log_seq.get_value() + 1;
        self.log_seq.set_value(id);
        self.logs.update(|l| {
            l.push(LogEntry { id, line });
            if l.len() > LOG_MAX {
                let excess = l.len() - LOG_MAX;
                l.drain(..excess);
            }
        });
    }

    pub fn open_dialog(&self, dialog: Dialog) -> u64 {
        let id = self.dialog_seq.get_value();
        self.dialog_seq.set_value(id + 1);
        self.menu.set(None);
        self.dialogs.update(|d| d.push(DialogEntry { id, dialog }));
        id
    }

    pub fn close_dialog(&self, id: u64) {
        self.dialogs.update(|d| d.retain(|e| e.id != id));
    }

    pub fn modal_open(&self) -> bool {
        self.dialogs.with_untracked(|d| !d.is_empty())
    }
}

thread_local! {
    static CTX: std::cell::Cell<Option<Ctx>> = const { std::cell::Cell::new(None) };
    static ROOT: std::cell::RefCell<Option<Owner>> = const { std::cell::RefCell::new(None) };
}

/// Contexte global (accessible aussi depuis les tâches asynchrones et les
/// gestionnaires d'événements de la fenêtre, qui n'ont pas de propriétaire réactif).
pub fn ctx() -> Ctx {
    CTX.with(|c| c.get())
        .expect("contexte de l'application non initialisé")
}

pub fn init_ctx(c: Ctx) {
    CTX.with(|cell| cell.set(Some(c)));
    ROOT.with(|r| *r.borrow_mut() = Owner::current());
}

/// Exécute `f` sous le propriétaire racine (les signaux créés vivent autant que l'application).
pub fn with_root<T>(f: impl FnOnce() -> T) -> T {
    match ROOT.with(|r| r.borrow().clone()) {
        Some(owner) => owner.with(f),
        None => f(),
    }
}
