//! Actions de l'interface : onglets, connexion, navigation, opérations, transferts.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use rustscp_shared::util::{rbasename, rjoin, rnormalize, rparent};
use rustscp_shared::{EnqueueResult, Entry, RemoteItem, SessionInfo, Site};
use serde_json::{Value, json};

use crate::api::{call, fire};
use crate::dialogs::{alert, ask_text, chmod_dialog, confirm};
use crate::state::{ConnState, Ctx, PARENT, Side, Tab};
use rustscp_shared::{tr, trf};

// ------------------------------------------------------------------ onglets
pub fn new_tab(ctx: Ctx) -> Tab {
    let id = ctx.next_tab.get_value();
    ctx.next_tab.set_value(id + 1);
    let start = ctx
        .current()
        .map(|t| t.local.path.get_untracked())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| ctx.info.get_untracked().home);
    let tab = crate::state::with_root(|| Tab::new(id, ctx.show_hidden));
    ctx.tabs.update(|t| t.push(tab));
    ctx.active.set(id);
    navigate_local(ctx, tab, start);
    tab
}

pub fn close_tab(ctx: Ctx, id: u64) {
    let Some(tab) = ctx.tab(id) else { return };
    spawn_local(async move {
        if let Some(sid) = tab.session.get_untracked() {
            let busy = call::<bool>("session_busy", json!({ "id": sid }))
                .await
                .unwrap_or(false);
            if busy
                && !confirm(
                    tr!("Transferts en cours", "Transfers in progress"),
                    tr!(
                        "Des transferts sont en cours sur cet onglet. Les annuler et fermer ?",
                        "Transfers are in progress on this tab. Cancel them and close?"
                    ),
                )
                .await
            {
                return;
            }
        }
        disconnect(ctx, tab);
        let tabs = ctx.tabs.get_untracked();
        let pos = tabs.iter().position(|t| t.id == id).unwrap_or(0);
        ctx.tabs.update(|t| t.retain(|t| t.id != id));
        // libère la mémoire des listings de l'onglet fermé
        tab.local.entries.set(Arc::new(Vec::new()));
        tab.remote.entries.set(Arc::new(Vec::new()));
        tab.cache.set_value(Default::default());
        let remaining = ctx.tabs.get_untracked();
        if remaining.is_empty() {
            new_tab(ctx);
        } else if ctx.active.get_untracked() == id {
            ctx.active.set(remaining[pos.min(remaining.len() - 1)].id);
        }
    });
}

// ---------------------------------------------------------- panneau local
pub fn navigate_local(ctx: Ctx, tab: Tab, path: String) {
    spawn_local(async move {
        match call::<String>("local_resolve", json!({ "path": path })).await {
            Ok(real) => load_local(ctx, tab, real, true).await,
            Err(e) => {
                tab.local.busy.set(Some(e.message));
                tab.local.path_text.set(tab.local.path.get_untracked());
            }
        }
    });
}

async fn load_local(ctx: Ctx, tab: Tab, path: String, reset: bool) {
    let _ = ctx;
    match call::<Vec<Entry>>("local_list", json!({ "path": path })).await {
        Ok(entries) => {
            let pane = tab.local;
            let same = pane.path.get_untracked() == path;
            if !same {
                pane.filter.set(String::new());
                pane.clear_selection();
                pane.scroll.set(0.0);
            }
            if !same || reset || pane.entries.with_untracked(|e| **e != entries) {
                if same {
                    let names: HashSet<String> = entries.iter().map(|e| e.name.clone()).collect();
                    if pane
                        .selected
                        .with_untracked(|s| s.iter().any(|n| !names.contains(n)))
                    {
                        pane.selected.update(|s| s.retain(|n| names.contains(n)));
                    }
                }
                if same {
                    // garde l'arborescence en phase avec le contenu du dossier
                    let mut dirs: Vec<String> = entries
                        .iter()
                        .filter(|e| e.is_dir)
                        .map(|e| e.name.clone())
                        .collect();
                    dirs.sort_by_key(|a| a.to_lowercase());
                    if tab
                        .local_tree
                        .children
                        .with_untracked(|c| c.get(&path) != Some(&dirs))
                    {
                        tab.local_tree.children.update(|c| {
                            c.insert(path.clone(), dirs);
                        });
                    }
                }
                pane.entries.set(Arc::new(entries));
            }
            if !same {
                pane.path.set(path.clone());
                pane.path_text.set(path.clone());
            }
            if pane.busy.get_untracked().is_some() {
                pane.busy.set(None);
            }
            if !same {
                sync_local_tree(tab, path).await;
            }
        }
        Err(e) => {
            tab.local.busy.set(Some(trf!("Erreur : {e}", "Error: {e}")));
            tab.local.path_text.set(tab.local.path.get_untracked());
        }
    }
}

pub fn refresh_local(ctx: Ctx, tab: Tab) {
    let path = tab.local.path.get_untracked();
    if path.is_empty() {
        return;
    }
    spawn_local(async move { load_local(ctx, tab, path, false).await });
}

/// Rafraîchit le panneau local après un court délai (regroupe les événements).
pub fn schedule_local_refresh(ctx: Ctx, tab: Tab) {
    if let Some(h) = tab.local_timer.get_value() {
        h.clear();
    }
    let h =
        set_timeout_with_handle(move || refresh_local(ctx, tab), Duration::from_millis(300)).ok();
    tab.local_timer.set_value(h);
}

async fn sync_local_tree(tab: Tab, path: String) {
    let Ok(levels) = call::<Vec<(String, Vec<String>)>>(
        "local_tree",
        json!({ "path": path, "show_hidden": true }),
    )
    .await
    else {
        return;
    };
    let tree = tab.local_tree;
    tree.children.update(|c| {
        for (dir, names) in &levels {
            c.insert(dir.clone(), names.clone());
        }
    });
    tree.loaded
        .update(|l| l.extend(levels.iter().map(|(d, _)| d.clone())));
    tree.expanded
        .update(|e| e.extend(levels.iter().map(|(d, _)| d.clone())));
    tree.selected.set(path);
}

pub fn local_up(ctx: Ctx, tab: Tab) {
    let path = tab.local.path.get_untracked();
    if path != "/" && !path.is_empty() {
        navigate_local(ctx, tab, rparent(&path));
    }
}

pub fn local_activate(ctx: Ctx, tab: Tab, e: &Entry) {
    if e.name == PARENT {
        return local_up(ctx, tab);
    }
    let path = rjoin(&tab.local.path.get_untracked(), &e.name);
    if e.is_dir {
        navigate_local(ctx, tab, path);
    } else if tab.connected() && ctx.settings.get_untracked().double_click_transfers {
        upload(ctx, tab, vec![path], None);
    } else {
        fire("local_open", json!({ "path": path }));
    }
}

pub fn local_selected_paths(tab: Tab) -> Vec<String> {
    let base = tab.local.path.get_untracked();
    tab.local
        .selected_entries()
        .iter()
        .map(|e| rjoin(&base, &e.name))
        .collect()
}

pub fn local_mkdir(ctx: Ctx, tab: Tab) {
    spawn_local(async move {
        let Some(name) = ask_text(
            tr!("Créer un dossier", "Create folder"),
            tr!("Nom du nouveau dossier :", "New folder name:"),
            tr!("nouveau dossier", "new folder"),
        )
        .await
        else {
            return;
        };
        let path = rjoin(&tab.local.path.get_untracked(), &name);
        if let Err(e) = call::<Value>("local_mkdir", json!({ "path": path })).await {
            tab.local.busy.set(Some(trf!("Erreur : {e}", "Error: {e}")));
        }
        refresh_local(ctx, tab);
    });
}

pub fn local_rename(ctx: Ctx, tab: Tab) {
    let paths = local_selected_paths(tab);
    if paths.len() != 1 {
        return;
    }
    let old = paths[0].clone();
    spawn_local(async move {
        let base = rbasename(&old).to_string();
        let Some(new) = ask_text(
            tr!("Renommer", "Rename"),
            tr!("Nouveau nom :", "New name:"),
            &base,
        )
        .await
        else {
            return;
        };
        if new == base {
            return;
        }
        let dst = rjoin(&rparent(&old), &new);
        match call::<Value>("local_rename", json!({ "src": old, "dst": dst })).await {
            Ok(_) => tab.local.selected.set(HashSet::from([new])),
            Err(e) => tab.local.busy.set(Some(trf!("Erreur : {e}", "Error: {e}"))),
        }
        refresh_local(ctx, tab);
    });
}

pub fn local_trash(ctx: Ctx, tab: Tab) {
    let paths = local_selected_paths(tab);
    if paths.is_empty() {
        return;
    }
    spawn_local(async move {
        if ctx.settings.get_untracked().confirm_delete
            && !confirm(
                tr!("Corbeille", "Trash"),
                &trf!(
                    "Mettre {} élément(s) à la corbeille ?",
                    "Move {} item(s) to the trash?",
                    paths.len()
                ),
            )
            .await
        {
            return;
        }
        if let Err(e) = call::<Value>("local_trash", json!({ "paths": paths })).await {
            tab.local.busy.set(Some(e.message));
        }
        tab.local.clear_selection();
        refresh_local(ctx, tab);
    });
}

// ------------------------------------------------------------- connexion
pub fn connect_site(ctx: Ctx, site: Site) {
    let tab = match ctx.current() {
        Some(t)
            if t.session.get_untracked().is_none()
                && t.conn.get_untracked() != ConnState::Connecting =>
        {
            t
        }
        _ => new_tab(ctx),
    };
    tab_connect(ctx, tab, site);
}

pub fn tab_connect(ctx: Ctx, tab: Tab, site: Site) {
    if tab.session.get_untracked().is_some() {
        disconnect(ctx, tab);
    }
    tab.site.set(Some(site.clone()));
    tab.conn.set(ConnState::Connecting);
    tab.shell_ok.set(None);
    tab.remote.busy.set(Some(trf!(
        "Connexion à {}…",
        "Connecting to {}…",
        site.host
    )));
    ctx.log(
        "status",
        trf!(
            "Connexion à {} ({}@{}:{})…",
            "Connecting to {} ({}@{}:{})…",
            site.label(),
            site.user,
            site.host,
            site.port
        ),
    );
    if !site.local_dir.is_empty() {
        navigate_local(ctx, tab, site.local_dir.clone());
    }
    spawn_local(async move {
        let id = match call::<u64>("session_create", json!({ "site": site })).await {
            Ok(id) => id,
            Err(e) => {
                tab.conn.set(ConnState::Lost);
                ctx.log(
                    "error",
                    trf!("Échec de la connexion : {e}", "Connection failed: {e}"),
                );
                return;
            }
        };
        tab.session.set(Some(id));
        match call::<SessionInfo>("session_connect", json!({ "id": id })).await {
            Ok(info) => {
                if tab.session.get_untracked() != Some(id) {
                    return;
                }
                tab.conn.set(ConnState::Connected);
                tab.home.set(info.home.clone());
                tab.shell_ok.set(info.shell_ok);
                tab.remote_tree.clear();
                tab.remote.busy.set(None);
                let start = if site.remote_dir.is_empty() {
                    info.home
                } else {
                    site.remote_dir.clone()
                };
                navigate_remote(ctx, tab, start, false, false);
            }
            Err(e) => {
                if tab.session.get_untracked() != Some(id) {
                    return;
                }
                tab.session.set(None);
                tab.conn.set(ConnState::Lost);
                tab.remote
                    .busy
                    .set(Some(tr!("Non connecté", "Not connected").into()));
                fire("session_close", json!({ "id": id }));
                if e.kind == "cancelled" {
                    ctx.log(
                        "error",
                        trf!("Connexion annulée : {e}", "Connection cancelled: {e}"),
                    );
                    return;
                }
                ctx.log(
                    "error",
                    trf!("Échec de la connexion : {e}", "Connection failed: {e}"),
                );
                let kind = if e.kind == "auth" {
                    tr!("d'authentification", "Authentication")
                } else {
                    tr!("de connexion", "Connection")
                };
                alert(
                    &trf!("Erreur {kind}", "{kind} error"),
                    &format!("{}\n\n{e}", site.label()),
                    true,
                )
                .await;
            }
        }
    });
}

pub fn disconnect(ctx: Ctx, tab: Tab) {
    let _ = ctx;
    let Some(id) = tab.session.get_untracked() else {
        if tab.conn.get_untracked() == ConnState::Connecting {
            tab.conn.set(ConnState::Lost);
        }
        return;
    };
    fire("session_close", json!({ "id": id }));
    tab.session.set(None);
    tab.conn.set(if tab.site.get_untracked().is_some() {
        ConnState::Lost
    } else {
        ConnState::Idle
    });
    tab.cache.set_value(Default::default());
    tab.remote.entries.set(Arc::new(Vec::new()));
    tab.remote.path.set(String::new());
    tab.remote.path_text.set(String::new());
    tab.remote.clear_selection();
    tab.remote
        .busy
        .set(Some(tr!("Non connecté", "Not connected").into()));
    tab.remote_tree.clear();
}

pub fn reconnect(ctx: Ctx, tab: Tab) {
    if let Some(site) = tab.site.get_untracked() {
        tab_connect(ctx, tab, site);
    }
}

/// Met à jour l'état de connexion des onglets (appelé périodiquement).
pub fn poll_sessions(ctx: Ctx) {
    for tab in ctx.tabs.get_untracked() {
        let Some(id) = tab.session.get_untracked() else {
            continue;
        };
        if tab.conn.get_untracked() == ConnState::Connecting {
            continue;
        }
        spawn_local(async move {
            if let Ok(info) = call::<SessionInfo>("session_info", json!({ "id": id })).await {
                if tab.session.get_untracked() != Some(id) {
                    return;
                }
                if tab.shell_ok.get_untracked() != info.shell_ok {
                    tab.shell_ok.set(info.shell_ok);
                }
                let state = if info.connected {
                    ConnState::Connected
                } else {
                    ConnState::Lost
                };
                if tab.conn.get_untracked() != state {
                    if state == ConnState::Lost {
                        ctx.log("error", tr!("Connexion perdue. Utilisez « Reconnecter » (Ctrl+R) ou poursuivez : la reconnexion est automatique.", "Connection lost. Use \"Reconnect\" (Ctrl+R) or carry on: reconnection is automatic."));
                    }
                    tab.conn.set(state);
                }
            }
        });
    }
}

// ----------------------------------------------------------- navigation
pub fn navigate_remote(ctx: Ctx, tab: Tab, path: String, refresh: bool, quiet: bool) {
    let Some(sid) = tab.session.get_untracked() else {
        return;
    };
    let home = tab.home.get_untracked();
    let mut path = path.trim().to_string();
    if path.is_empty() {
        path = home.clone();
    }
    if let Some(rest) = path.strip_prefix('~') {
        path = format!("{home}{rest}");
    }
    if !path.starts_with('/') {
        let cur = tab.remote.path.get_untracked();
        path = rjoin(if cur.is_empty() { &home } else { &cur }, &path);
    }
    let path = rnormalize(&path);
    let token = tab.nav_token.get_value() + 1;
    tab.nav_token.set_value(token);
    if !refresh && let Some(cached) = tab.cache.with_value(|c| c.get(&path).cloned()) {
        show_listing(tab, &path, cached);
    }
    if !quiet {
        tab.remote.busy.set(Some(trf!(
            "Lecture du dossier {path}…",
            "Reading folder {path}…"
        )));
        ctx.log("command", format!("LIST {path}"));
    }
    spawn_local(async move {
        match call::<Vec<Entry>>("remote_list", json!({ "id": sid, "path": path })).await {
            Ok(entries) => {
                let entries = Arc::new(entries);
                tab.cache.update_value(|c| {
                    c.insert(path.clone(), entries.clone());
                });
                if token == tab.nav_token.get_value() && tab.session.get_untracked() == Some(sid) {
                    let n = entries.len();
                    show_listing(tab, &path, entries);
                    tab.remote.busy.set(None);
                    if tab.conn.get_untracked() == ConnState::Lost {
                        tab.conn.set(ConnState::Connected);
                    }
                    if !quiet {
                        ctx.log(
                            "status",
                            trf!(
                                "Contenu de « {path} » affiché ({n} éléments)",
                                "Listing of \"{path}\" displayed ({n} items)"
                            ),
                        );
                    }
                }
            }
            Err(e) => {
                if tab.session.get_untracked() != Some(sid) {
                    return;
                }
                ctx.log(
                    "error",
                    trf!(
                        "Impossible de lister {path} : {e}",
                        "Unable to list {path}: {e}"
                    ),
                );
                if token == tab.nav_token.get_value() {
                    tab.remote
                        .busy
                        .set(Some(trf!("Erreur : {e}", "Error: {e}")));
                    tab.remote.path_text.set(tab.remote.path.get_untracked());
                }
                if e.kind == "closed" {
                    tab.session.set(None);
                    tab.conn.set(ConnState::Lost);
                }
            }
        }
    });
}

fn show_listing(tab: Tab, path: &str, entries: Arc<Vec<Entry>>) {
    let pane = tab.remote;
    let same = pane.path.get_untracked() == path;
    if !same {
        pane.filter.set(String::new());
        pane.clear_selection();
        pane.scroll.set(0.0);
    } else {
        // conserve la sélection des éléments toujours présents
        let names: HashSet<String> = entries.iter().map(|e| e.name.clone()).collect();
        if pane
            .selected
            .with_untracked(|s| s.iter().any(|n| !names.contains(n)))
        {
            pane.selected.update(|s| s.retain(|n| names.contains(n)));
        }
    }
    update_remote_tree(tab, path, &entries, true);
    pane.entries.set(entries);
    pane.path.set(path.to_string());
    pane.path_text.set(path.to_string());
}

fn update_remote_tree(tab: Tab, path: &str, entries: &[Entry], select: bool) {
    let tree = tab.remote_tree;
    let mut dirs: Vec<String> = entries
        .iter()
        .filter(|e| e.is_dir)
        .map(|e| e.name.clone())
        .collect();
    dirs.sort_by_key(|a| a.to_lowercase());
    let path = rnormalize(path);
    tree.children.update(|c| {
        c.insert(path.clone(), dirs);
        // ancêtres : on s'assure que chaque niveau connaît le suivant
        let mut cur = path.clone();
        while cur != "/" {
            let parent = rparent(&cur);
            let name = rbasename(&cur).to_string();
            let list = c.entry(parent.clone()).or_default();
            if !list.contains(&name) {
                list.push(name);
                list.sort_by_key(|a| a.to_lowercase());
            }
            cur = parent;
        }
    });
    tree.loaded.update(|l| {
        l.insert(path.clone());
    });
    if select {
        tree.expanded.update(|e| {
            let mut cur = path.clone();
            e.insert(cur.clone());
            while cur != "/" {
                cur = rparent(&cur);
                e.insert(cur.clone());
            }
        });
        tree.selected.set(path);
    }
}

/// Charge les sous-dossiers d'un nœud d'arborescence déplié.
pub fn load_tree_children(ctx: Ctx, tab: Tab, side: Side, path: String) {
    let _ = ctx;
    spawn_local(async move {
        match side {
            Side::Local => {
                if let Ok(entries) = call::<Vec<Entry>>("local_list", json!({ "path": path })).await
                {
                    let mut dirs: Vec<String> = entries
                        .into_iter()
                        .filter(|e| e.is_dir)
                        .map(|e| e.name)
                        .collect();
                    dirs.sort_by_key(|a| a.to_lowercase());
                    tab.local_tree.children.update(|c| {
                        c.insert(path.clone(), dirs);
                    });
                    tab.local_tree.loaded.update(|l| {
                        l.insert(path);
                    });
                }
            }
            Side::Remote => {
                let Some(sid) = tab.session.get_untracked() else {
                    return;
                };
                if let Ok(entries) =
                    call::<Vec<Entry>>("remote_list", json!({ "id": sid, "path": path })).await
                {
                    update_remote_tree(tab, &path, &entries, false);
                    tab.cache.update_value(|c| {
                        c.insert(path, Arc::new(entries));
                    });
                }
            }
        }
    });
}

pub fn remote_up(ctx: Ctx, tab: Tab) {
    let path = tab.remote.path.get_untracked();
    if tab.session.get_untracked().is_some() && path != "/" && !path.is_empty() {
        navigate_remote(ctx, tab, rparent(&path), false, false);
    }
}

pub fn refresh_remote(ctx: Ctx, tab: Tab) {
    if tab.session.get_untracked().is_some() {
        navigate_remote(ctx, tab, tab.remote.path.get_untracked(), true, false);
    }
}

pub fn schedule_refresh(ctx: Ctx, tab: Tab, remote_dir: &str) {
    let dir = rnormalize(remote_dir);
    if tab.session.get_untracked().is_some() && dir == tab.remote.path.get_untracked() {
        if let Some(h) = tab.refresh_timer.get_value() {
            h.clear();
        }
        let h = set_timeout_with_handle(
            move || navigate_remote(ctx, tab, tab.remote.path.get_untracked(), true, true),
            Duration::from_millis(400),
        )
        .ok();
        tab.refresh_timer.set_value(h);
    } else {
        tab.cache.update_value(|c| {
            c.remove(&dir);
        });
    }
}

pub fn remote_activate(ctx: Ctx, tab: Tab, e: &Entry) {
    if e.name == PARENT {
        return remote_up(ctx, tab);
    }
    let path = rjoin(&tab.remote.path.get_untracked(), &e.name);
    if e.is_dir {
        navigate_remote(ctx, tab, path, false, false);
    } else if ctx.settings.get_untracked().double_click_transfers {
        download(
            ctx,
            tab,
            vec![RemoteItem {
                path,
                is_dir: false,
                size: e.size,
                mode: e.mode,
            }],
            None,
        );
    } else {
        edit_remote(
            ctx,
            tab,
            RemoteItem {
                path,
                is_dir: false,
                size: e.size,
                mode: e.mode,
            },
            false,
        );
    }
}

pub fn remote_selected_items(tab: Tab) -> Vec<RemoteItem> {
    let base = tab.remote.path.get_untracked();
    tab.remote
        .selected_entries()
        .into_iter()
        .map(|e| RemoteItem {
            path: rjoin(&base, &e.name),
            is_dir: e.is_dir,
            size: e.size,
            mode: e.mode,
        })
        .collect()
}

// ------------------------------------------------------------ opérations
fn remote_op(ctx: Ctx, tab: Tab, description: String, cmd: &'static str, mut args: Value) {
    let Some(sid) = tab.session.get_untracked() else {
        return;
    };
    args["id"] = json!(sid);
    ctx.log("command", description.clone());
    spawn_local(async move {
        if let Err(e) = call::<Value>(cmd, args).await {
            ctx.log(
                "error",
                trf!("{description} : échec — {e}", "{description}: failed — {e}"),
            );
            alert(
                tr!("Erreur", "Error"),
                &format!("{description}\n\n{e}"),
                true,
            )
            .await;
        }
        if tab.session.get_untracked() == Some(sid) {
            navigate_remote(ctx, tab, tab.remote.path.get_untracked(), true, true);
        }
    });
}

fn absolute(base: &str, name: &str) -> String {
    if name.starts_with('/') {
        name.to_string()
    } else {
        rjoin(base, name)
    }
}

pub fn remote_mkdir(ctx: Ctx, tab: Tab) {
    if tab.session.get_untracked().is_none() {
        return;
    }
    spawn_local(async move {
        let cur = tab.remote.path.get_untracked();
        let Some(name) = ask_text(
            tr!("Créer un dossier", "Create folder"),
            &trf!("Nouveau dossier dans {cur} :", "New folder in {cur}:"),
            tr!("nouveau dossier", "new folder"),
        )
        .await
        else {
            return;
        };
        let p = absolute(&cur, &name);
        remote_op(
            ctx,
            tab,
            trf!("Création de {p}", "Creating {p}"),
            "remote_mkdir",
            json!({ "path": p }),
        );
    });
}

pub fn remote_touch(ctx: Ctx, tab: Tab) {
    if tab.session.get_untracked().is_none() {
        return;
    }
    spawn_local(async move {
        let cur = tab.remote.path.get_untracked();
        let Some(name) = ask_text(
            tr!("Créer un fichier vide", "Create empty file"),
            &trf!("Nouveau fichier dans {cur} :", "New file in {cur}:"),
            tr!("nouveau.txt", "new.txt"),
        )
        .await
        else {
            return;
        };
        let p = absolute(&cur, &name);
        remote_op(
            ctx,
            tab,
            trf!("Création de {p}", "Creating {p}"),
            "remote_touch",
            json!({ "path": p }),
        );
    });
}

pub fn remote_rename(ctx: Ctx, tab: Tab) {
    let items = remote_selected_items(tab);
    if items.len() != 1 {
        return;
    }
    let old = items[0].path.clone();
    spawn_local(async move {
        let base = rbasename(&old).to_string();
        let Some(new) = ask_text(
            tr!("Renommer", "Rename"),
            tr!("Nouveau nom :", "New name:"),
            &base,
        )
        .await
        else {
            return;
        };
        if new == base {
            return;
        }
        let dst = absolute(&rparent(&old), &new);
        tab.remote
            .selected
            .set(HashSet::from([rbasename(&dst).to_string()]));
        remote_op(
            ctx,
            tab,
            trf!("Renommage {old} → {dst}", "Renaming {old} → {dst}"),
            "remote_rename",
            json!({ "src": old, "dst": dst }),
        );
    });
}

pub fn remote_delete(ctx: Ctx, tab: Tab) {
    let items = remote_selected_items(tab);
    if items.is_empty() {
        return;
    }
    spawn_local(async move {
        let dirs = items.iter().filter(|i| i.is_dir).count();
        let mut msg = trf!(
            "Supprimer définitivement {} élément(s) du serveur",
            "Permanently delete {} item(s) from the server",
            items.len()
        );
        if dirs > 0 {
            msg += &trf!(
                ", dont {dirs} dossier(s) et tout leur contenu ?",
                ", including {dirs} folder(s) and all their contents?"
            );
        } else {
            msg += tr!(" ?", "?");
        }
        if ctx.settings.get_untracked().confirm_delete
            && !confirm(tr!("Suppression", "Delete"), &msg).await
        {
            return;
        }
        tab.cache.update_value(|c| {
            for it in items.iter().filter(|i| i.is_dir) {
                c.remove(&it.path);
            }
        });
        let n = items.len();
        tab.remote.clear_selection();
        remote_op(
            ctx,
            tab,
            trf!("Suppression de {n} élément(s)", "Deleting {n} item(s)"),
            "remote_delete",
            json!({ "items": items }),
        );
    });
}

pub fn remote_chmod(ctx: Ctx, tab: Tab) {
    let items = remote_selected_items(tab);
    if items.is_empty() {
        return;
    }
    spawn_local(async move {
        let title = if items.len() == 1 {
            rbasename(&items[0].path).to_string()
        } else {
            trf!("{} éléments", "{} items", items.len())
        };
        let has_dirs = items.iter().any(|i| i.is_dir);
        let Some((mode, recursive)) = chmod_dialog(&title, items[0].mode, has_dirs).await else {
            return;
        };
        let n = items.len();
        remote_op(
            ctx,
            tab,
            trf!(
                "chmod {mode:o} sur {n} élément(s)",
                "chmod {mode:o} on {n} item(s)"
            ),
            "remote_chmod",
            json!({ "items": items, "mode": mode, "recursive": recursive }),
        );
    });
}

pub fn remote_move(ctx: Ctx, tab: Tab, items: Vec<RemoteItem>, target: String) {
    let target = rnormalize(&target);
    let items: Vec<RemoteItem> = items
        .into_iter()
        .filter(|i| i.path != target && rparent(&i.path) != target)
        .collect();
    if items.is_empty()
        || items
            .iter()
            .any(|i| target.starts_with(&format!("{}/", i.path)))
    {
        return;
    }
    tab.cache.update_value(|c| {
        c.remove(&target);
    });
    let n = items.len();
    remote_op(
        ctx,
        tab,
        trf!(
            "Déplacement de {n} élément(s) vers {target}",
            "Moving {n} item(s) to {target}"
        ),
        "remote_move",
        json!({ "items": items, "target": target }),
    );
}

pub fn copy_text(text: String) {
    fire("copy_text", json!({ "text": text }));
}

// ------------------------------------------------------------ transferts
pub fn upload(ctx: Ctx, tab: Tab, paths: Vec<String>, remote_dir: Option<String>) {
    let Some(sid) = tab.session.get_untracked() else {
        return;
    };
    if paths.is_empty() {
        return;
    }
    let dir = remote_dir.unwrap_or_else(|| tab.remote.path.get_untracked());
    ctx.log(
        "status",
        trf!(
            "Préparation de l'envoi de {} élément(s) vers {dir}…",
            "Preparing upload of {} item(s) to {dir}…",
            paths.len()
        ),
    );
    spawn_local(async move {
        match call::<EnqueueResult>(
            "queue_upload",
            json!({ "id": sid, "paths": paths, "remote_dir": dir }),
        )
        .await
        {
            Ok(r) => {
                enqueued(ctx, &r);
                schedule_refresh(ctx, tab, &dir); // les dossiers créés apparaissent immédiatement
            }
            Err(e) => ctx.log(
                "error",
                trf!(
                    "Préparation de l'envoi impossible : {e}",
                    "Unable to prepare upload: {e}"
                ),
            ),
        }
    });
}

pub fn download(ctx: Ctx, tab: Tab, items: Vec<RemoteItem>, local_dir: Option<String>) {
    let Some(sid) = tab.session.get_untracked() else {
        return;
    };
    if items.is_empty() {
        return;
    }
    let dir = local_dir.unwrap_or_else(|| tab.local.path.get_untracked());
    if items.iter().any(|i| i.is_dir) {
        ctx.log(
            "status",
            trf!(
                "Analyse des dossiers à télécharger vers {dir}…",
                "Scanning folders to download to {dir}…"
            ),
        );
    }
    spawn_local(async move {
        match call::<EnqueueResult>(
            "queue_download",
            json!({ "id": sid, "items": items, "local_dir": dir }),
        )
        .await
        {
            Ok(r) => {
                enqueued(ctx, &r);
                if dir == tab.local.path.get_untracked() {
                    schedule_local_refresh(ctx, tab);
                }
            }
            Err(e) => ctx.log(
                "error",
                trf!(
                    "Préparation du téléchargement impossible : {e}",
                    "Unable to prepare download: {e}"
                ),
            ),
        }
    });
}

fn enqueued(ctx: Ctx, r: &EnqueueResult) {
    if r.count == 0 {
        ctx.log("status", tr!("Rien à transférer", "Nothing to transfer"));
    } else {
        ctx.log(
            "status",
            trf!(
                "{} fichier(s) ajouté(s) à la file ({:.1} Mio)",
                "{} file(s) added to the queue ({:.1} MiB)",
                r.count,
                r.total as f64 / 1048576.0
            ),
        );
    }
}

/// Télécharge un fichier distant et l'ouvre, avec l'éditeur de texte par
/// défaut (`text`) ou l'application associée à son type.
pub fn edit_remote(ctx: Ctx, tab: Tab, item: RemoteItem, text: bool) {
    let Some(sid) = tab.session.get_untracked() else {
        return;
    };
    let _ = ctx;
    fire(
        "edit_remote",
        json!({ "id": sid, "path": item.path, "size": item.size, "text": text }),
    );
}

pub fn upload_selected(ctx: Ctx, tab: Tab) {
    let paths = local_selected_paths(tab);
    if !paths.is_empty() && tab.connected() {
        upload(ctx, tab, paths, None);
    }
}

pub fn download_selected(ctx: Ctx, tab: Tab) {
    let items = remote_selected_items(tab);
    if !items.is_empty() {
        download(ctx, tab, items, None);
    }
}

// --------------------------------------------------------------- divers
pub fn toggle_hidden(ctx: Ctx) {
    ctx.settings.update(|s| s.show_hidden = !s.show_hidden);
    save_settings(ctx);
}

pub fn save_settings(ctx: Ctx) {
    let settings = ctx.settings.get_untracked();
    spawn_local(async move {
        if let Err(e) = call::<Value>("settings_set", json!({ "settings": settings })).await {
            ctx.log(
                "error",
                trf!(
                    "Enregistrement des paramètres impossible : {e}",
                    "Unable to save settings: {e}"
                ),
            );
        }
    });
}

pub fn quit(ctx: Ctx) {
    spawn_local(async move {
        let s = ctx.queue.stats.get_untracked();
        let n = s.active + s.queued;
        if n > 0
            && !confirm(
                tr!("Quitter", "Quit"),
                &trf!(
                    "{n} transfert(s) en cours ou en attente. Quitter quand même ?",
                    "{n} transfer(s) in progress or queued. Quit anyway?"
                ),
            )
            .await
        {
            return;
        }
        fire("quit", json!({}));
    });
}

/// Liste des sites regroupés par dossier (pour les menus).
pub fn sites_by_folder(ctx: Ctx) -> Vec<(String, Vec<Site>)> {
    let file = ctx.sites.get_untracked();
    let mut sites = file.sites.clone();
    sites.sort_by_key(|s| s.name.to_lowercase());
    let mut folders: Vec<String> = sites.iter().map(|s| s.folder.clone()).collect();
    folders.sort_by_key(|f| f.to_lowercase());
    folders.dedup();
    folders
        .into_iter()
        .map(|f| {
            let list = sites.iter().filter(|s| s.folder == f).cloned().collect();
            (f, list)
        })
        .collect()
}
