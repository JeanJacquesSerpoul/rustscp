//! Formatage (dates locales) et petits utilitaires DOM.

use wasm_bindgen::JsCast;

pub fn format_mtime(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts as f64 * 1000.0));
    let (day, month, year) = (d.get_date(), d.get_month() + 1, d.get_full_year());
    let time = format!(
        "{:02}:{:02}:{:02}",
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds()
    );
    if rustscp_shared::i18n::is_fr() {
        format!("{day:02}/{month:02}/{year:04} {time}")
    } else {
        format!("{year:04}-{month:02}-{day:02} {time}")
    }
}

pub fn now_hms() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "{:02}:{:02}:{:02}",
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds()
    )
}

pub fn window() -> web_sys::Window {
    web_sys::window().expect("window")
}

pub fn storage_get(key: &str) -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item(key).ok().flatten())
}

pub fn storage_set(key: &str, value: &str) {
    if let Some(s) = window().local_storage().ok().flatten() {
        let _ = s.set_item(key, value);
    }
}

/// Premier ancêtre (ou soi-même) portant l'attribut `attr`.
pub fn closest_with(el: Option<web_sys::Element>, selector: &str) -> Option<web_sys::Element> {
    el.and_then(|e| e.closest(selector).ok().flatten())
}

pub fn element_at(x: f64, y: f64) -> Option<web_sys::Element> {
    window().document()?.element_from_point(x as f32, y as f32)
}

pub fn target_element(ev: &web_sys::Event) -> Option<web_sys::Element> {
    ev.target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
}

/// Vrai si la cible de l'événement est un champ de saisie.
pub fn in_text_field(ev: &web_sys::Event) -> bool {
    target_element(ev).is_some_and(|e| {
        let tag = e.tag_name().to_lowercase();
        tag == "input" || tag == "textarea" || tag == "select"
    })
}

pub fn random_id() -> String {
    (0..32)
        .map(|_| format!("{:x}", (js_sys::Math::random() * 16.0) as u8))
        .collect()
}
