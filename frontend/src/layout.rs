//! Disposition : séparateurs redimensionnables, hauteur observée des vues
//! virtualisées, persistance des tailles.

use std::cell::RefCell;
use std::sync::Arc;

use leptos::html;
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::fmt::{storage_get, storage_set};

#[derive(Clone, Copy)]
pub struct Layout {
    pub log_h: RwSignal<f64>,
    pub queue_h: RwSignal<f64>,
    pub left_w: RwSignal<f64>,
    pub local_tree_h: RwSignal<f64>,
    pub remote_tree_h: RwSignal<f64>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    log_h: f64,
    queue_h: f64,
    left_w: f64,
    local_tree_h: f64,
    remote_tree_h: f64,
}

const KEY: &str = "rustscp.layout";

impl Layout {
    pub fn load() -> Self {
        let saved: Option<Saved> = storage_get(KEY).and_then(|s| serde_json::from_str(&s).ok());
        let s = saved.unwrap_or(Saved {
            log_h: 110.0,
            queue_h: 190.0,
            left_w: 0.0,
            local_tree_h: 150.0,
            remote_tree_h: 150.0,
        });
        Layout {
            log_h: RwSignal::new(s.log_h),
            queue_h: RwSignal::new(s.queue_h),
            left_w: RwSignal::new(s.left_w),
            local_tree_h: RwSignal::new(s.local_tree_h),
            remote_tree_h: RwSignal::new(s.remote_tree_h),
        }
    }

    pub fn save(&self) {
        let s = Saved {
            log_h: self.log_h.get_untracked(),
            queue_h: self.queue_h.get_untracked(),
            left_w: self.left_w.get_untracked(),
            local_tree_h: self.local_tree_h.get_untracked(),
            remote_tree_h: self.remote_tree_h.get_untracked(),
        };
        if let Ok(text) = serde_json::to_string(&s) {
            storage_set(KEY, &text);
        }
    }
}

thread_local! {
    static LAYOUT: std::cell::Cell<Option<Layout>> = const { std::cell::Cell::new(None) };
}

pub fn set_layout(l: Layout) {
    LAYOUT.with(|c| c.set(Some(l)));
}

pub fn layout() -> Layout {
    LAYOUT
        .with(|c| c.get())
        .expect("disposition non initialisée")
}

// ------------------------------------------------------------- séparateurs
#[derive(Clone, Copy, PartialEq)]
pub enum Axis {
    /// Séparateur horizontal : on fait varier une hauteur.
    Vertical,
    /// Séparateur vertical : on fait varier une largeur.
    Horizontal,
}

struct Resize {
    axis: Axis,
    start: f64,
    start_size: f64,
    sign: f64,
    min: f64,
    set: Arc<dyn Fn(f64)>,
}

thread_local! {
    static RESIZE: RefCell<Option<Resize>> = const { RefCell::new(None) };
}

pub fn start_resize(
    ev: &web_sys::MouseEvent,
    axis: Axis,
    start_size: f64,
    sign: f64,
    min: f64,
    set: impl Fn(f64) + 'static,
) {
    ev.prevent_default();
    ev.stop_propagation();
    let start = if axis == Axis::Vertical {
        ev.client_y() as f64
    } else {
        ev.client_x() as f64
    };
    RESIZE.with(|r| {
        *r.borrow_mut() = Some(Resize {
            axis,
            start,
            start_size,
            sign,
            min,
            set: Arc::new(set),
        })
    });
    if let Some(body) = crate::fmt::window().document().and_then(|d| d.body()) {
        let _ = body.class_list().add_1(if axis == Axis::Vertical {
            "resizing-v"
        } else {
            "resizing-h"
        });
    }
}

/// À appeler depuis le gestionnaire global `mousemove` ; vrai si un redimensionnement est en cours.
pub fn on_mouse_move(ev: &web_sys::MouseEvent) -> bool {
    RESIZE.with(|r| {
        let r = r.borrow();
        let Some(rs) = r.as_ref() else { return false };
        let pos = if rs.axis == Axis::Vertical {
            ev.client_y() as f64
        } else {
            ev.client_x() as f64
        };
        let size = (rs.start_size + (pos - rs.start) * rs.sign).max(rs.min);
        (rs.set)(size);
        true
    })
}

pub fn on_mouse_up() -> bool {
    let was = RESIZE.with(|r| r.borrow_mut().take().is_some());
    if was {
        if let Some(body) = crate::fmt::window().document().and_then(|d| d.body()) {
            let _ = body.class_list().remove_2("resizing-v", "resizing-h");
        }
        layout().save();
    }
    was
}

/// Séparateur faisant varier `size` (sign = -1 si la zone est après le séparateur).
#[component]
pub fn Splitter(
    axis: Axis,
    size: RwSignal<f64>,
    #[prop(default = 1.0)] sign: f64,
    #[prop(default = 40.0)] min: f64,
) -> impl IntoView {
    let class = if axis == Axis::Vertical {
        "splitter splitter-v"
    } else {
        "splitter splitter-h"
    };
    view! {
        <div class=class on:mousedown=move |ev| {
            if ev.button() == 0 {
                start_resize(&ev, axis, size.get_untracked(), sign, min, move |v| size.set(v));
            }
        }></div>
    }
}

// ------------------------------------------------------------ hauteur observée
/// Hauteur (px) d'un élément, mise à jour à chaque redimensionnement.
pub fn use_height(node: NodeRef<html::Div>) -> RwSignal<f64> {
    let h = RwSignal::new(400.0);
    Effect::new(move |_| {
        let Some(el) = node.get() else { return };
        let el: web_sys::Element = el.into();
        h.set(el.client_height() as f64);
        let el2 = el.clone();
        let cb = Closure::<dyn FnMut(js_sys::Array)>::new(move |_entries: js_sys::Array| {
            let v = el2.client_height() as f64;
            if (h.get_untracked() - v).abs() > 0.5 {
                h.set(v);
            }
        });
        let Ok(ro) = web_sys::ResizeObserver::new(cb.as_ref().unchecked_ref()) else {
            return;
        };
        ro.observe(&el);
        let keep = SendWrapper::new((ro, cb));
        on_cleanup(move || {
            keep.0.disconnect();
            drop(keep);
        });
    });
    h
}

/// Fenêtre de lignes à afficher : (début, fin) en indices.
pub fn visible_range(scroll: f64, height: f64, row_h: f64, count: usize) -> (usize, usize) {
    let start = ((scroll / row_h).floor() as isize - 6).max(0) as usize;
    let end = (((scroll + height) / row_h).ceil() as usize + 6).min(count);
    (start.min(end), end)
}

/// Fait défiler `vp` pour que la ligne `index` soit visible.
pub fn ensure_visible(vp: &web_sys::Element, index: usize, row_h: f64, header: f64) {
    let top = index as f64 * row_h;
    let view_top = vp.scroll_top() as f64;
    let view_h = vp.client_height() as f64 - header;
    if top < view_top {
        vp.set_scroll_top(top as i32);
    } else if top + row_h > view_top + view_h {
        vp.set_scroll_top((top + row_h - view_h) as i32);
    }
}
