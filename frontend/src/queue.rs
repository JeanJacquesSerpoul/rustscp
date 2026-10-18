//! Panneau de la file de transferts (en attente / échecs / réussis).

use std::collections::HashSet;
use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use rustscp_shared::util::{human_duration, human_size, human_speed};
use rustscp_shared::{Direction, TransferPoll, TransferStatus, TransferView};
use serde_json::json;
use wasm_bindgen::JsCast;

use crate::api::{call, fire};
use crate::icons::{file_icon, icon};
use crate::layout::{use_height, visible_range};
use crate::state::{Ctx, MenuItem, MenuState, ctx};

const ROW_H: f64 = 22.0;
const TITLES: [&str; 3] = [
    "Fichiers en attente",
    "Transferts échoués",
    "Transferts réussis",
];
const COLS: [(&str, f64); 9] = [
    ("Fichier local", 300.0),
    ("", 26.0),
    ("Fichier distant", 300.0),
    ("Taille", 80.0),
    ("Progression", 130.0),
    ("Vitesse", 90.0),
    ("Restant", 70.0),
    ("Statut", 220.0),
    ("Serveur", 160.0),
];

/// Interroge le cœur périodiquement (mise à jour incrémentale).
pub fn start_polling(c: Ctx) {
    let busy = StoredValue::new(false);
    let _ = set_interval_with_handle(
        move || {
            if busy.get_value() {
                return;
            }
            busy.set_value(true);
            let since = c.queue.version.get_value();
            spawn_local(async move {
                if let Ok(poll) =
                    call::<TransferPoll>("transfer_poll", json!({ "since": since })).await
                {
                    c.queue.apply(poll);
                }
                busy.set_value(false);
            });
        },
        Duration::from_millis(250),
    );
}

fn ids_of(c: Ctx, group: usize) -> Vec<u64> {
    let q = c.queue;
    q.items.with_untracked(|items| {
        items
            .iter()
            .filter(|t| t.status.group() == group)
            .map(|t| t.id)
            .collect()
    })
}

fn menu(c: Ctx, group: usize, x: f64, y: f64) {
    let sel: Vec<u64> = {
        let s = c.queue.selected.get_untracked();
        c.queue.items.with_untracked(|items| {
            items
                .iter()
                .filter(|t| t.status.group() == group && s.contains(&t.id))
                .map(|t| t.id)
                .collect()
        })
    };
    let any = !sel.is_empty();
    let s1 = sel.clone();
    let s2 = sel.clone();
    let mut items = Vec::new();
    if group == 0 {
        items.push(
            MenuItem::action("Annuler la sélection", "stop", move || {
                fire("transfer_cancel", json!({ "ids": s1 }))
            })
            .enabled(any),
        );
        items.push(MenuItem::action("Tout annuler", "stop", move || {
            fire("transfer_cancel_all", json!({}))
        }));
    } else {
        if group == 1 {
            items.push(
                MenuItem::action("Réessayer la sélection", "refresh", move || {
                    fire("transfer_retry", json!({ "ids": s1 }))
                })
                .enabled(any),
            );
            items.push(MenuItem::action("Tout réessayer", "refresh", move || {
                fire("transfer_retry", json!({ "ids": ids_of(c, 1) }))
            }));
        }
        items.push(
            MenuItem::action("Retirer la sélection", "trash", move || {
                fire("transfer_remove", json!({ "ids": s2 }))
            })
            .enabled(any),
        );
        items.push(MenuItem::action("Tout effacer", "trash", move || {
            fire("transfer_clear", json!({ "group": group }))
        }));
    }
    c.menu.set(Some(MenuState { x, y, items }));
}

#[component]
pub fn QueuePanel() -> impl IntoView {
    let c = ctx();
    let q = c.queue;
    let vp = NodeRef::<html::Div>::new();
    let height = use_height(vp);
    let tpl = COLS
        .iter()
        .map(|(_, w)| format!("{w}px "))
        .collect::<String>()
        + "minmax(0,1fr)";
    let total_w: f64 = COLS.iter().map(|(_, w)| w).sum::<f64>() + 20.0;

    let switch = move |g: usize| {
        q.tab.set(g);
        q.selected.set(HashSet::new());
        q.scroll.set(0.0);
        if let Some(el) = vp.get_untracked() {
            el.set_scroll_top(0);
        }
    };

    let tpl2 = tpl.clone();
    let rows = move || {
        let g = q.tab.get();
        let idx = q.groups[g].get();
        let (start, end) = visible_range(q.scroll.get(), height.get(), ROW_H, idx.len());
        let sel = q.selected.get();
        q.items.with(|items| {
            idx[start..end]
                .iter()
                .enumerate()
                .filter_map(|(i, &ix)| items.get(ix as usize).map(|t| (start + i, t.clone())))
                .map(|(row, t)| row_view(c, g, row, t, sel.clone(), tpl2.clone()))
                .collect_view()
        })
    };

    view! {
        <div class="queue">
            <div class="tabs-bar queue-tabs">
                {(0..3).map(|g| view! {
                    <button class="tab-btn" class:active=move || q.tab.get() == g on:click=move |_| switch(g)>
                        {move || {
                            let n = q.groups[g].with(|v| v.len());
                            if n > 0 { format!("{} ({n})", TITLES[g]) } else { TITLES[g].to_string() }
                        }}
                    </button>
                }).collect_view()}
            </div>
            <div class="list queue-list" tabindex="0" node_ref=vp
                on:scroll=move |ev| {
                    let el: web_sys::Element = ev.target().unwrap().unchecked_into();
                    q.scroll.set(el.scroll_top() as f64);
                }
                on:keydown=move |ev| {
                    let g = q.tab.get_untracked();
                    match ev.key().as_str() {
                        "Delete" => {
                            let ids: Vec<u64> = q.selected.get_untracked().into_iter().collect();
                            if g == 0 { fire("transfer_cancel", json!({ "ids": ids })) } else { fire("transfer_remove", json!({ "ids": ids })) }
                            ev.prevent_default();
                        }
                        "a" | "A" if ev.ctrl_key() => {
                            q.selected.set(ids_of(c, g).into_iter().collect());
                            ev.prevent_default();
                        }
                        _ => {}
                    }
                }
                on:contextmenu=move |ev| {
                    ev.prevent_default();
                    menu(c, q.tab.get_untracked(), ev.client_x() as f64, ev.client_y() as f64);
                }>
                <div class="list-inner" style:min-width=format!("{total_w}px")>
                    <div class="list-header" style:grid-template-columns=tpl.clone()>
                        {COLS.iter().map(|(t, _)| view! { <div class="hcell"><span class="htext">{*t}</span></div> }).collect_view()}
                        <div class="hcell filler"></div>
                    </div>
                    <div class="rows" style:height=move || format!("{}px", q.groups[q.tab.get()].with(|v| v.len()) as f64 * ROW_H)>
                        {rows}
                    </div>
                </div>
            </div>
        </div>
    }
}

fn row_view(
    c: Ctx,
    group: usize,
    row: usize,
    t: TransferView,
    sel: HashSet<u64>,
    tpl: String,
) -> impl IntoView {
    let q = c.queue;
    let id = t.id;
    let selected = sel.contains(&id);
    let fraction = t.fraction();
    let active = t.status == TransferStatus::Active;
    let mut status = t.status.label().to_string();
    if !t.protocol.is_empty() && matches!(t.status, TransferStatus::Active | TransferStatus::Done) {
        status += &format!(" ({})", t.protocol.to_uppercase());
    }
    if !t.error.is_empty() {
        status += &format!(" : {}", t.error);
    }
    let status_class = match t.status {
        TransferStatus::Failed => "cell status-failed",
        TransferStatus::Done => "cell status-done",
        TransferStatus::Cancelled => "cell muted",
        _ => "cell",
    };
    let mousedown = move |ev: web_sys::MouseEvent| {
        let ids_in_group = || ids_of(c, group);
        if ev.button() == 2 {
            if !q.selected.with_untracked(|s| s.contains(&id)) {
                q.selected.set(HashSet::from([id]));
                q.anchor.set(Some(id));
            }
            return;
        }
        if ev.ctrl_key() || ev.meta_key() {
            q.selected.update(|s| {
                if !s.remove(&id) {
                    s.insert(id);
                }
            });
            q.anchor.set(Some(id));
        } else if ev.shift_key() {
            let ids = ids_in_group();
            let a = q
                .anchor
                .get_untracked()
                .and_then(|a| ids.iter().position(|x| *x == a))
                .unwrap_or(0);
            let b = ids.iter().position(|x| *x == id).unwrap_or(0);
            let (lo, hi) = (a.min(b), a.max(b));
            q.selected.set(ids[lo..=hi].iter().copied().collect());
        } else {
            q.selected.set(HashSet::from([id]));
            q.anchor.set(Some(id));
        }
    };
    let name = t.name().to_string();
    view! {
        <div class="row" class:selected=selected class:odd=row % 2 == 1
            style:top=format!("{}px", row as f64 * ROW_H) style:grid-template-columns=tpl
            on:mousedown=mousedown>
            <div class="cell name" title=t.local.clone()>{file_icon(&name, false, false)}<span class="cell-text">{t.local.clone()}</span></div>
            <div class="cell center dir">{icon(if t.direction == Direction::Up { "arrow-right" } else { "arrow-left" })}</div>
            <div class="cell" title=t.remote.clone()><span class="cell-text">{t.remote.clone()}</span></div>
            <div class="cell right">{human_size(t.size)}</div>
            <div class="cell">
                <div class="progress" class:active=active>
                    <div class="bar" style:width=format!("{:.1}%", fraction * 100.0)></div>
                    <span>{format!("{:.1} %", fraction * 100.0)}</span>
                </div>
            </div>
            <div class="cell right">{if active { human_speed(t.speed) } else { String::new() }}</div>
            <div class="cell right">{if active { human_duration(t.eta()) } else { String::new() }}</div>
            <div class=status_class title=t.error.clone()><span class="cell-text">{status}</span></div>
            <div class="cell"><span class="cell-text">{t.server.clone()}</span></div>
            <div class="cell"></div>
        </div>
    }
}
