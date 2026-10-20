//! Interface de RustSCP (Leptos, compilée en WebAssembly et affichée par Tauri).

mod actions;
mod api;
mod app;
mod dialogs;
mod fmt;
mod icons;
mod layout;
mod panes;
mod queue;
mod sitemanager;
mod state;
mod terminal;

fn main() {
    console_error_panic_hook::set_once();
    // la langue (choisie par le cœur d'après les réglages ou la locale) doit être
    // connue avant le premier rendu (exécuteur de Leptos pas encore initialisé ici)
    wasm_bindgen_futures::spawn_local(async {
        let info = api::call::<state::AppInfo>("app_info", serde_json::json!({})).await;
        let lang = match info.map(|i| i.lang) {
            Ok(code) if code == "fr" => rustscp_shared::Lang::Fr,
            _ => rustscp_shared::Lang::En,
        };
        rustscp_shared::set_lang(lang);
        if let Some(root) = fmt::window().document().and_then(|d| d.document_element()) {
            let _ = root.set_attribute("lang", lang.code());
        }
        match terminal::launch_params() {
            // fenêtre de terminal SSH
            Some(launch) => {
                terminal::load_assets().await;
                leptos::mount::mount_to_body(move || {
                    leptos::view! { <terminal::TerminalApp launch=launch.clone()/> }
                });
            }
            None => leptos::mount::mount_to_body(app::App),
        }
    });
}
