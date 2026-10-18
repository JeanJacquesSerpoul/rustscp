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

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(app::App);
}
