//! Panneaux de fichiers local et distant (arborescence + liste), façon FileZilla.

use std::cell::Cell;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use rustscp_shared::util::{file_type_for, filemode, human_size, rjoin, rparent};
use rustscp_shared::{Entry, RemoteItem};
use serde_json::json;
use wasm_bindgen::JsCast;

use crate::actions::{self as act};
use crate::api::call;
use crate::fmt::format_mtime;
use crate::icons::{file_icon, icon};
use crate::layout::{
    Axis, Splitter, ensure_visible, layout, start_resize, use_height, visible_range,
};
use crate::state::{
    Col, ConnState, Ctx, DragPayload, DragState, MenuItem, MenuState, PARENT, Pane, Side, Tab, ctx,
};

pub const ROW_H: f64 = 22.0;
pub const HEADER_H: f64 = 24.0;

thread_local! {
    /// Vrai juste après un glisser-déposer (le clic qui suit est ignoré).
    pub static DRAG_ENDED: Cell<bool> = const { Cell::new(false) };
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Local => "local",
        Side::Remote => "remote",
    }
}

// ---------------------------------------------------------------- panneau
#[component]
pub fn PaneView(tab: Tab, side: Side) -> impl IntoView {
    let c = ctx();
    let lay = layout();
    let pane = tab.pane(side);
    let tree_h = match side {
        Side::Local => lay.local_tree_h,
        Side::Remote => lay.remote_tree_h,
    };
    let connected = move || {
        side == Side::Local
            || (tab.session.get().is_some() && tab.conn.get() != ConnState::Connecting)
    };
    let title = if side == Side::Local {
        "Site local :"
    } else {
        "Site distant :"
    };
    let datalist_id = format!("complete-{}-{}", side_name(side), tab.id);
    let completions = RwSignal::new(Vec::<String>::new());

    let go = move || {
        let p = pane.path_text.get_untracked();
        match side {
            Side::Local => act::navigate_local(c, tab, p),
            Side::Remote => act::navigate_remote(c, tab, p, false, false),
        }
    };
    let complete = move |text: String| {
        if side != Side::Local || !text.starts_with('/') {
            return;
        }
        let (dir, prefix) = match text.rfind('/') {
            Some(i) => (
                if i == 0 {
                    "/".to_string()
                } else {
                    text[..i].to_string()
                },
                text[i + 1..].to_string(),
            ),
            None => return,
        };
        spawn_local(async move {
            if let Ok(entries) = call::<Vec<Entry>>("local_list", json!({ "path": dir })).await {
                let mut v: Vec<String> = entries
                    .into_iter()
                    .filter(|e| {
                        e.is_dir
                            && e.name.starts_with(&prefix)
                            && (prefix.starts_with('.') || !e.name.starts_with('.'))
                    })
                    .map(|e| rjoin(&dir, &e.name))
                    .collect();
                v.sort();
                v.truncate(50);
                completions.set(v);
            }
        });
    };

    let status = move || {
        if let Some(b) = pane.busy.get() {
            return b;
        }
        if side == Side::Remote && !connected() {
            return "Non connecté".into();
        }
        let sel = pane.selected.get();
        pane.rows.with(|rows| {
            let chosen: Vec<&Entry> = rows
                .iter()
                .filter(|e| e.name != PARENT && sel.contains(&e.name))
                .collect();
            if !chosen.is_empty() {
                let size: u64 = chosen.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
                format!(
                    "{} élément(s) sélectionné(s), {}",
                    chosen.len(),
                    human_size(size)
                )
            } else {
                let files: Vec<&Entry> = rows.iter().filter(|e| !e.is_dir).collect();
                let dirs = rows.iter().filter(|e| e.is_dir && e.name != PARENT).count();
                let total: u64 = files.iter().map(|e| e.size).sum();
                format!(
                    "{} fichier(s) et {dirs} dossier(s). Taille totale : {}",
                    files.len(),
                    human_size(total)
                )
            }
        })
    };

    view! {
        <div class="pane">
            <div class="pathbar">
                <label>{title}</label>
                <input type="text" class="path-input" list=datalist_id.clone() spellcheck="false"
                    disabled=move || !connected()
                    prop:value=pane.path_text
                    on:input=move |ev| { let v = event_target_value(&ev); pane.path_text.set(v.clone()); complete(v); }
                    on:change=move |ev| {
                        // choix dans la liste de complétion
                        let v = event_target_value(&ev);
                        if completions.with_untracked(|c| c.contains(&v)) { pane.path_text.set(v); go(); }
                    }
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" { ev.prevent_default(); go(); }
                        if ev.key() == "Escape" { pane.path_text.set(pane.path.get_untracked()); }
                    }/>
                <datalist id=datalist_id>
                    {move || completions.get().into_iter().map(|p| view! { <option value=p></option> }).collect_view()}
                </datalist>
                <button class="icon-btn" title="Dossier parent (Retour arrière)" disabled=move || !connected()
                    on:click=move |_| match side { Side::Local => act::local_up(c, tab), Side::Remote => act::remote_up(c, tab) }>
                    {icon("up")}
                </button>
                <button class="icon-btn" title="Dossier personnel" disabled=move || !connected()
                    on:click=move |_| match side {
                        Side::Local => act::navigate_local(c, tab, c.info.get_untracked().home),
                        Side::Remote => act::navigate_remote(c, tab, tab.home.get_untracked(), false, false),
                    }>
                    {icon("home")}
                </button>
            </div>
            <div class="tree-wrap" style:height=move || format!("{}px", tree_h.get())>
                <TreeView tab=tab side=side/>
            </div>
            <Splitter axis=Axis::Vertical size=tree_h min=40.0/>
            <div class="list-wrap">
                <Show when=connected fallback=move || view! {
                    <div class="placeholder">
                        {move || if tab.conn.get() == ConnState::Connecting {
                            view! { <div class="spinner"></div><p>{move || pane.busy.get().unwrap_or_default()}</p> }.into_any()
                        } else {
                            view! {
                                <p>"Non connecté à un serveur."</p>
                                <p class="muted">"Utilisez la connexion rapide ou le gestionnaire de sites (Ctrl+S)."</p>
                            }.into_any()
                        }}
                    </div>
                }>
                    <div class="filter-bar">
                        {icon("search")}
                        <input type="search" placeholder="Filtrer (texte ou motif *.log)…" spellcheck="false"
                            prop:value=pane.filter on:input=move |ev| pane.filter.set(event_target_value(&ev))/>
                    </div>
                    <FileList tab=tab side=side/>
                </Show>
            </div>
            <div class="pane-status" title=status>{status}</div>
        </div>
    }
}

// ------------------------------------------------------------- liste
fn template(widths: &[f64]) -> String {
    let mut s: String = widths.iter().map(|w| format!("{w}px ")).collect();
    s.push_str("minmax(0,1fr)");
    s
}

fn cell_text(col: Col, e: &Entry) -> String {
    if e.name == PARENT && col != Col::Name {
        return String::new();
    }
    match col {
        Col::Name => e.name.clone(),
        Col::Size => {
            if e.is_dir {
                String::new()
            } else {
                human_size(e.size)
            }
        }
        Col::Type => {
            let base = if e.is_dir {
                "Dossier".to_string()
            } else {
                file_type_for(&e.name)
            };
            if e.is_link {
                format!("{base} (lien)")
            } else {
                base
            }
        }
        Col::Modified => format_mtime(e.mtime),
        Col::Perms => filemode(e.mode),
        Col::Owner => format!("{} {}", e.owner, e.group).trim().to_string(),
    }
}

fn select_only(pane: Pane, name: &str) {
    pane.selected.set(HashSet::from([name.to_string()]));
    pane.anchor.set(Some(name.to_string()));
    pane.cursor.set(Some(name.to_string()));
}

fn select_range(pane: Pane, rows: &[Entry], from: &str, to: &str) {
    let a = rows.iter().position(|e| e.name == from).unwrap_or(0);
    let b = rows.iter().position(|e| e.name == to).unwrap_or(0);
    let (lo, hi) = (a.min(b), a.max(b));
    let set: HashSet<String> = rows[lo..=hi]
        .iter()
        .filter(|e| e.name != PARENT)
        .map(|e| e.name.clone())
        .collect();
    pane.selected.set(set);
}

fn activate(c: Ctx, tab: Tab, side: Side, e: &Entry) {
    match side {
        Side::Local => act::local_activate(c, tab, e),
        Side::Remote => act::remote_activate(c, tab, e),
    }
}

fn refresh(c: Ctx, tab: Tab, side: Side) {
    match side {
        Side::Local => act::refresh_local(c, tab),
        Side::Remote => act::refresh_remote(c, tab),
    }
}

fn drag_payload(tab: Tab, side: Side) -> Option<(DragPayload, String)> {
    let label;
    let payload = match side {
        Side::Local => {
            let paths = act::local_selected_paths(tab);
            if paths.is_empty() {
                return None;
            }
            label = if paths.len() == 1 {
                rustscp_shared::util::rbasename(&paths[0]).to_string()
            } else {
                format!("{} éléments", paths.len())
            };
            DragPayload::Local { tab: tab.id, paths }
        }
        Side::Remote => {
            let items: Vec<RemoteItem> = act::remote_selected_items(tab);
            if items.is_empty() {
                return None;
            }
            label = if items.len() == 1 {
                rustscp_shared::util::rbasename(&items[0].path).to_string()
            } else {
                format!("{} éléments", items.len())
            };
            DragPayload::Remote { tab: tab.id, items }
        }
    };
    Some((payload, label))
}

pub fn context_menu(c: Ctx, tab: Tab, side: Side, x: f64, y: f64) {
    let pane = tab.pane(side);
    let sel = pane.selected_entries();
    let n = sel.len();
    let single_file = n == 1 && !sel[0].is_dir;
    let base = pane.path.get_untracked();
    let items = match side {
        Side::Local => {
            let paths: Vec<String> = sel.iter().map(|e| rjoin(&base, &e.name)).collect();
            let first = paths.first().cloned().unwrap_or_default();
            let copy = if paths.is_empty() {
                base.clone()
            } else {
                paths.join("\n")
            };
            let mut v = vec![
                MenuItem::action("Envoyer", "upload", move || act::upload_selected(c, tab))
                    .enabled(n > 0 && tab.connected())
                    .shortcut("Ctrl+U"),
            ];
            if single_file {
                v.push(MenuItem::action("Ouvrir", "edit", move || {
                    crate::api::fire("local_open", json!({ "path": first }))
                }));
            }
            v.extend([
                MenuItem::Separator,
                MenuItem::action("Créer un dossier…", "folder-new", move || {
                    act::local_mkdir(c, tab)
                }),
                MenuItem::action("Renommer…", "rename", move || act::local_rename(c, tab))
                    .enabled(n == 1)
                    .shortcut("F2"),
                MenuItem::action("Mettre à la corbeille", "trash", move || {
                    act::local_trash(c, tab)
                })
                .enabled(n > 0)
                .shortcut("Suppr"),
                MenuItem::Separator,
                MenuItem::action("Copier le chemin", "copy", move || {
                    act::copy_text(copy.clone())
                }),
                MenuItem::action("Actualiser", "refresh", move || act::refresh_local(c, tab))
                    .shortcut("F5"),
            ]);
            v
        }
        Side::Remote => {
            if tab.session.get_untracked().is_none() {
                return;
            }
            let items = act::remote_selected_items(tab);
            let copy = if items.is_empty() {
                base.clone()
            } else {
                items
                    .iter()
                    .map(|i| i.path.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let first = items.first().cloned();
            let mut v = vec![
                MenuItem::action("Télécharger", "download", move || {
                    act::download_selected(c, tab)
                })
                .enabled(n > 0)
                .shortcut("Ctrl+D"),
            ];
            if single_file {
                let item = first.clone().unwrap();
                v.push(MenuItem::action("Afficher / Éditer", "edit", move || {
                    act::edit_remote(c, tab, item.clone())
                }));
            }
            v.extend([
                MenuItem::Separator,
                MenuItem::action("Créer un dossier…", "folder-new", move || {
                    act::remote_mkdir(c, tab)
                })
                .shortcut("F7"),
                MenuItem::action("Créer un fichier vide…", "file-new", move || {
                    act::remote_touch(c, tab)
                }),
                MenuItem::action("Renommer…", "rename", move || act::remote_rename(c, tab))
                    .enabled(n == 1)
                    .shortcut("F2"),
                MenuItem::action("Supprimer", "trash", move || act::remote_delete(c, tab))
                    .enabled(n > 0)
                    .shortcut("Suppr"),
                MenuItem::action("Droits d'accès (chmod)…", "lock", move || {
                    act::remote_chmod(c, tab)
                })
                .enabled(n > 0),
                MenuItem::Separator,
                MenuItem::action("Copier le chemin", "copy", move || {
                    act::copy_text(copy.clone())
                }),
                MenuItem::action("Actualiser", "refresh", move || act::refresh_remote(c, tab))
                    .shortcut("F5"),
            ]);
            v
        }
    };
    c.menu.set(Some(MenuState { x, y, items }));
}

#[component]
pub fn FileList(tab: Tab, side: Side) -> impl IntoView {
    let c = ctx();
    let pane = tab.pane(side);
    let vp = NodeRef::<html::Div>::new();
    let height = use_height(vp);
    let cols = pane.columns();
    let typeahead = StoredValue::new((String::new(), 0.0f64));
    let side_s = side_name(side);

    // restaure la position de défilement (changement d'onglet, nouveau listing)
    Effect::new(move |_| {
        let want = pane.scroll.get();
        if let Some(el) = vp.get()
            && (el.scroll_top() as f64 - want).abs() > 1.0
        {
            el.set_scroll_top(want as i32);
        }
    });

    let focus = move || {
        if let Some(el) = vp.get_untracked() {
            let _ = el.focus();
        }
    };

    let header = move || {
        let widths = pane.widths.get();
        let (sort_col, asc) = pane.sort.get();
        let tpl = template(&widths);
        view! {
            <div class="list-header" style:grid-template-columns=tpl.clone()>
                {cols.iter().enumerate().map(|(i, col)| {
                    let col = *col;
                    let w = widths[i];
                    let arrow = (sort_col == col).then_some(if asc { "▲" } else { "▼" });
                    view! {
                        <div class=if col == Col::Size { "hcell right" } else { "hcell" }
                            on:click=move |_| pane.sort.update(|s| if s.0 == col { s.1 = !s.1 } else { *s = (col, true) })>
                            <span class="htext">{col.title()}</span>
                            <span class="sort">{arrow}</span>
                            <span class="col-resize" on:click=|ev| ev.stop_propagation() on:mousedown=move |ev| {
                                start_resize(&ev, Axis::Horizontal, w, 1.0, 40.0, move |v| pane.widths.update(|ws| ws[i] = v));
                            }></span>
                        </div>
                    }
                }).collect_view()}
                <div class="hcell filler"></div>
            </div>
        }
    };

    let rows_view = move || {
        let (start, end) = pane
            .rows
            .with(|r| visible_range(pane.scroll.get(), height.get(), ROW_H, r.len()));
        let sel = pane.selected.get();
        let cursor = pane.cursor.get();
        let tpl = pane.widths.with(|w| template(w));
        let base = pane.path.get();
        pane.rows.with(|rows| {
            rows[start..end]
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let idx = start + i;
                    let e = e.clone();
                    let name = e.name.clone();
                    let selected = sel.contains(&name);
                    let is_cursor = cursor.as_deref() == Some(name.as_str());
                    let drop_path = if e.name == PARENT {
                        Some(rparent(&base))
                    } else if e.is_dir {
                        Some(rjoin(&base, &e.name))
                    } else {
                        None
                    };
                    let (n1, n2, n3) = (name.clone(), name.clone(), name.clone());
                    let e2 = e.clone();
                    let mousedown = move |ev: web_sys::MouseEvent| {
                        ev.prevent_default();
                        focus();
                        c.menu.set(None);
                        let ctrl = ev.ctrl_key() || ev.meta_key();
                        let shift = ev.shift_key();
                        let is_parent = n1 == PARENT;
                        if ev.button() == 2 {
                            if !pane.selected.with_untracked(|s| s.contains(&n1)) {
                                if is_parent { pane.clear_selection() } else { select_only(pane, &n1) }
                            }
                            return;
                        }
                        if ev.button() != 0 {
                            return;
                        }
                        if is_parent {
                            pane.clear_selection();
                            pane.cursor.set(Some(n1.clone()));
                            return;
                        }
                        if ctrl {
                            pane.selected.update(|s| if !s.remove(&n1) { s.insert(n1.clone()); });
                            pane.anchor.set(Some(n1.clone()));
                            pane.cursor.set(Some(n1.clone()));
                            return;
                        }
                        if shift {
                            let anchor = pane.anchor.get_untracked().unwrap_or_else(|| n1.clone());
                            pane.rows.with_untracked(|rows| select_range(pane, rows, &anchor, &n1));
                            pane.cursor.set(Some(n1.clone()));
                            return;
                        }
                        if !pane.selected.with_untracked(|s| s.contains(&n1)) {
                            select_only(pane, &n1);
                        } else {
                            pane.cursor.set(Some(n1.clone()));
                        }
                        if let Some((payload, label)) = drag_payload(tab, side) {
                            let p = (ev.client_x() as f64, ev.client_y() as f64);
                            c.drag.set(Some(DragState { payload, label, start: p, pos: p, active: false }));
                        }
                    };
                    let click = move |ev: web_sys::MouseEvent| {
                        if DRAG_ENDED.with(|d| d.replace(false)) {
                            return;
                        }
                        if ev.button() == 0 && !ev.ctrl_key() && !ev.meta_key() && !ev.shift_key() && n2 != PARENT {
                            select_only(pane, &n2);
                        }
                    };
                    let dbl = move |ev: web_sys::MouseEvent| {
                        ev.prevent_default();
                        activate(c, tab, side, &e2);
                    };
                    let ctxmenu = move |ev: web_sys::MouseEvent| {
                        ev.prevent_default();
                        ev.stop_propagation();
                        let _ = &n3;
                        context_menu(c, tab, side, ev.client_x() as f64, ev.client_y() as f64);
                    };
                    view! {
                        <div class="row" class:selected=selected class:cursor=is_cursor class:link=e.is_link
                            class:odd=idx % 2 == 1
                            data-drop=drop_path.as_ref().map(|_| side_s) data-tab=tab.id.to_string() data-path=drop_path
                            style:top=format!("{}px", idx as f64 * ROW_H) style:grid-template-columns=tpl.clone()
                            on:mousedown=mousedown on:click=click on:dblclick=dbl on:contextmenu=ctxmenu>
                            {cols.iter().map(|col| {
                                let col = *col;
                                if col == Col::Name {
                                    let ic = if e.name == PARENT { icon("parent").into_any() } else { file_icon(&e.name, e.is_dir, e.is_link).into_any() };
                                    view! { <div class="cell name" title=rjoin(&base, &e.name)>{ic}<span class="cell-text">{e.name.clone()}</span></div> }.into_any()
                                } else {
                                    view! { <div class=if col == Col::Size { "cell right" } else { "cell" }>{cell_text(col, &e)}</div> }.into_any()
                                }
                            }).collect_view()}
                            <div class="cell"></div>
                        </div>
                    }
                })
                .collect_view()
        })
    };

    let keydown = move |ev: web_sys::KeyboardEvent| {
        let key = ev.key();
        let ctrl = ev.ctrl_key() || ev.meta_key();
        let shift = ev.shift_key();
        let rows = pane.rows.get_untracked();
        let n = rows.len();
        let cur_idx = pane
            .cursor
            .get_untracked()
            .and_then(|c| rows.iter().position(|e| e.name == c));
        let mut handled = true;
        let mut move_to: Option<usize> = None;
        let page = ((height.get_untracked() - HEADER_H) / ROW_H)
            .floor()
            .max(1.0) as usize;
        match key.as_str() {
            "ArrowDown" => move_to = Some(cur_idx.map_or(0, |i| (i + 1).min(n.saturating_sub(1)))),
            "ArrowUp" => move_to = Some(cur_idx.map_or(0, |i| i.saturating_sub(1))),
            "PageDown" => {
                move_to = Some(cur_idx.map_or(0, |i| (i + page).min(n.saturating_sub(1))))
            }
            "PageUp" => move_to = Some(cur_idx.map_or(0, |i| i.saturating_sub(page))),
            "Home" => move_to = Some(0),
            "End" => move_to = Some(n.saturating_sub(1)),
            "Enter" => {
                if let Some(e) = cur_idx.and_then(|i| rows.get(i)) {
                    activate(c, tab, side, e);
                }
            }
            "Backspace" => match side {
                Side::Local => act::local_up(c, tab),
                Side::Remote => act::remote_up(c, tab),
            },
            "F2" => match side {
                Side::Local => act::local_rename(c, tab),
                Side::Remote => act::remote_rename(c, tab),
            },
            "Delete" => match side {
                Side::Local => act::local_trash(c, tab),
                Side::Remote => act::remote_delete(c, tab),
            },
            "F5" => refresh(c, tab, side),
            "F7" if side == Side::Remote => act::remote_mkdir(c, tab),
            "Escape" => pane.clear_selection(),
            "a" | "A" if ctrl => {
                pane.selected.set(
                    rows.iter()
                        .filter(|e| e.name != PARENT)
                        .map(|e| e.name.clone())
                        .collect(),
                );
            }
            "u" | "U" if ctrl && side == Side::Local => act::upload_selected(c, tab),
            "d" | "D" if ctrl && !shift && side == Side::Remote => act::download_selected(c, tab),
            "ContextMenu" => {
                if let Some(el) = vp.get_untracked() {
                    let r = el.get_bounding_client_rect();
                    context_menu(c, tab, side, r.left() + 40.0, r.top() + 40.0);
                }
            }
            k if k.chars().count() == 1 && !ctrl && !ev.alt_key() => {
                // recherche au clavier : saute au premier nom commençant par les lettres tapées
                let now = js_sys::Date::now();
                let (mut buf, last) = typeahead.get_value();
                if now - last > 800.0 {
                    buf.clear();
                }
                buf.push_str(&k.to_lowercase());
                typeahead.set_value((buf.clone(), now));
                if let Some(i) = rows
                    .iter()
                    .position(|e| e.name != PARENT && e.name.to_lowercase().starts_with(&buf))
                {
                    move_to = Some(i);
                }
            }
            _ => handled = false,
        }
        if let Some(i) = move_to.filter(|_| n > 0) {
            let name = rows[i].name.clone();
            if shift {
                let anchor = pane.anchor.get_untracked().unwrap_or_else(|| name.clone());
                select_range(pane, &rows, &anchor, &name);
                pane.cursor.set(Some(name));
            } else if name == PARENT {
                pane.clear_selection();
                pane.cursor.set(Some(name));
            } else {
                select_only(pane, &name);
            }
            if let Some(el) = vp.get_untracked() {
                ensure_visible(&el, i, ROW_H, HEADER_H);
            }
        }
        if handled {
            ev.prevent_default();
            ev.stop_propagation();
        }
    };

    let total_w = move || pane.widths.with(|w| w.iter().sum::<f64>() + 20.0);

    view! {
        <div class="list" tabindex="0" node_ref=vp
            data-drop=side_s data-tab=tab.id.to_string() data-path=move || pane.path.get()
            on:scroll=move |ev| {
                let el: web_sys::Element = ev.target().unwrap().unchecked_into();
                pane.scroll.set(el.scroll_top() as f64);
            }
            on:keydown=keydown
            on:mousedown=move |ev| {
                // clic dans le vide : désélectionne
                if crate::fmt::target_element(&ev).is_some_and(|t| t.class_list().contains("rows") || t.class_list().contains("list")) {
                    c.menu.set(None);
                    if ev.button() == 0 { pane.clear_selection(); }
                    focus();
                }
            }
            on:contextmenu=move |ev| {
                ev.prevent_default();
                context_menu(c, tab, side, ev.client_x() as f64, ev.client_y() as f64);
            }>
            <div class="list-inner" style:min-width=move || format!("{}px", total_w())>
                {header}
                <div class="rows" style:height=move || format!("{}px", pane.rows.with(|r| r.len()) as f64 * ROW_H)>
                    {rows_view}
                </div>
            </div>
        </div>
    }
}

// ------------------------------------------------------------- arborescence
#[derive(Clone, PartialEq)]
struct TreeRow {
    path: String,
    name: String,
    depth: usize,
    expanded: bool,
    expandable: bool,
}

#[component]
pub fn TreeView(tab: Tab, side: Side) -> impl IntoView {
    let c = ctx();
    let tree = tab.tree(side);
    let vp = NodeRef::<html::Div>::new();
    let height = use_height(vp);
    let side_s = side_name(side);
    let enabled = move || {
        side == Side::Local
            || (tab.session.get().is_some() && tab.conn.get() != ConnState::Connecting)
    };

    let rows = Memo::new(move |_| {
        if !enabled() {
            return Arc::new(Vec::<TreeRow>::new());
        }
        let hidden = c.show_hidden.get();
        let children = tree.children.get();
        let expanded = tree.expanded.get();
        let loaded = tree.loaded.get();
        let mut out = Vec::new();
        let mut stack = vec![("/".to_string(), "/".to_string(), 0usize)];
        while let Some((path, name, depth)) = stack.pop() {
            let kids: Vec<&String> = children
                .get(&path)
                .map(|v| v.iter().filter(|n| hidden || !n.starts_with('.')).collect())
                .unwrap_or_default();
            let is_open = expanded.contains(&path);
            out.push(TreeRow {
                path: path.clone(),
                name,
                depth,
                expanded: is_open,
                expandable: !loaded.contains(&path) || !kids.is_empty(),
            });
            if is_open {
                for k in kids.into_iter().rev() {
                    stack.push((rjoin(&path, k), k.clone(), depth + 1));
                }
            }
        }
        Arc::new(out)
    });

    // garde le dossier courant visible
    Effect::new(move |_| {
        let sel = tree.selected.get();
        let idx = rows.with(|r| r.iter().position(|t| t.path == sel));
        if let (Some(i), Some(el)) = (idx, vp.get_untracked()) {
            request_animation_frame(move || ensure_visible(&el, i, ROW_H, 0.0));
        }
    });

    let toggle = move |row: &TreeRow| {
        let path = row.path.clone();
        if row.expanded {
            tree.expanded.update(|e| {
                e.remove(&path);
            });
        } else {
            tree.expanded.update(|e| {
                e.insert(path.clone());
            });
            if !tree.loaded.with_untracked(|l| l.contains(&path)) {
                act::load_tree_children(c, tab, side, path);
            }
        }
    };

    let body = move || {
        let (start, end) =
            rows.with(|r| visible_range(tree.scroll.get(), height.get(), ROW_H, r.len()));
        let selected = tree.selected.get();
        rows.with(|r| {
            r[start..end]
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let idx = start + i;
                    let row = row.clone();
                    let r2 = row.clone();
                    let depth = row.depth;
                    let (is_sel, expandable, expanded, name) = (row.path == selected, row.expandable, row.expanded, row.name.clone());
                    let path = row.path.clone();
                    let p2 = path.clone();
                    view! {
                        <div class="trow" class:selected=is_sel data-drop=side_s data-tab=tab.id.to_string()
                            data-path=path.clone()
                            style:top=format!("{}px", idx as f64 * ROW_H) style:padding-left=format!("{}px", 4 + depth * 16)
                            on:mousedown=move |ev| { ev.prevent_default(); c.menu.set(None); }
                            on:click=move |_| match side {
                                Side::Local => act::navigate_local(c, tab, path.clone()),
                                Side::Remote => act::navigate_remote(c, tab, path.clone(), false, false),
                            }
                            on:dblclick=move |_| toggle(&r2)>
                            <span class="twisty" on:click=move |ev| { ev.stop_propagation(); toggle(&row); }>
                                {if !expandable { ().into_any() } else if expanded { icon("chevron-down").into_any() } else { icon("chevron-right").into_any() }}
                            </span>
                            <span class="file-ic">{icon(if expanded { "folder-open" } else { "folder" })}</span>
                            <span class="cell-text" title=p2>{name}</span>
                        </div>
                    }
                })
                .collect_view()
        })
    };

    view! {
        <div class="tree" node_ref=vp tabindex="-1"
            on:scroll=move |ev| {
                let el: web_sys::Element = ev.target().unwrap().unchecked_into();
                tree.scroll.set(el.scroll_top() as f64);
            }>
            <div class="rows" style:height=move || format!("{}px", rows.with(|r| r.len()) as f64 * ROW_H)>
                {body}
            </div>
        </div>
    }
}

/// Rafraîchissement périodique du panneau local de l'onglet actif (le système
/// de fichiers local n'envoie pas de notifications à l'interface).
pub fn start_local_watch(c: Ctx) {
    let _ = set_interval_with_handle(
        move || {
            if c.modal_open() {
                return;
            }
            if let Some(tab) = c.current()
                && !tab.local.path.get_untracked().is_empty()
            {
                act::refresh_local(c, tab);
            }
        },
        Duration::from_secs(4),
    );
}
