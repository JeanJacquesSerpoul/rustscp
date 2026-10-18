//! RustSCP — client SCP/SFTP graphique (clone de WSCP en Rust : Tauri + Leptos).

pub mod commands;
pub mod config;
pub mod error;
pub mod local;
pub mod remotefs;
pub mod scp;
pub mod sftp;
pub mod sites;
pub mod ssh;
pub mod totp;
pub mod transfer;
pub mod ui;

use std::sync::Arc;

use tauri::{Emitter, Manager, WindowEvent};

use crate::commands::AppState;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            let imported = config::import_from_wscp();
            let hub = Arc::new(ui::PromptHub::default());
            let ui = Arc::new(ui::TauriUi::new(app.handle().clone(), hub.clone()));
            app.manage(AppState::new(
                ui,
                hub,
                Arc::new(sites::SecretStore::new()),
                imported,
            ));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                let stats = state.transfers.stats();
                if stats.queued + stats.active > 0 {
                    // l'interface demande confirmation puis appelle « quit »
                    api.prevent_close();
                    let _ = window.emit("close-requested", stats.queued + stats.active);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::settings_get,
            commands::settings_set,
            commands::quit,
            commands::copy_text,
            commands::pick_file,
            commands::pick_dir,
            commands::prompt_reply,
            commands::sites_get,
            commands::sites_set,
            commands::secret_get,
            commands::secret_set,
            commands::secrets_copy,
            commands::secrets_delete,
            commands::totp_preview,
            commands::session_create,
            commands::session_connect,
            commands::session_info,
            commands::session_close,
            commands::session_busy,
            commands::remote_list,
            commands::remote_mkdir,
            commands::remote_touch,
            commands::remote_rename,
            commands::remote_move,
            commands::remote_delete,
            commands::remote_chmod,
            commands::queue_upload,
            commands::queue_download,
            commands::edit_remote,
            commands::reupload,
            commands::transfer_poll,
            commands::transfer_cancel,
            commands::transfer_cancel_all,
            commands::transfer_retry,
            commands::transfer_remove,
            commands::transfer_clear,
            commands::transfer_running,
            commands::local_list,
            commands::local_tree,
            commands::local_resolve,
            commands::local_mkdir,
            commands::local_rename,
            commands::local_trash,
            commands::local_open,
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de RustSCP");
}
