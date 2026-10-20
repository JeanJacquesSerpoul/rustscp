//! Interface entre le cœur réseau et l'interface utilisateur.
//!
//! Le cœur journalise et pose des questions (clé d'hôte, code TOTP, phrase de
//! passe, écrasement…) via ce trait ; l'implémentation Tauri émet des
//! événements vers la fenêtre et attend la réponse, les tests utilisent une
//! implémentation sans interface.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rustscp_shared::{
    EditChanged, LogLine, PromptEvent, PromptReply, PromptRequest, TransferFinished,
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Ui: Send + Sync + 'static {
    fn log(&self, level: &str, message: String);
    /// Pose une question à l'utilisateur ; `PromptReply::Cancel` si refus.
    fn ask(&self, request: PromptRequest) -> BoxFut<'_, PromptReply>;
    fn transfer_finished(&self, _event: TransferFinished) {}
    fn edit_changed(&self, _event: EditChanged) {}
}

pub fn now_hms() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Questions en attente de réponse de l'interface (partagé par les implémentations).
pub struct PromptHub {
    pending: Mutex<HashMap<u64, oneshot::Sender<PromptReply>>>,
    next_id: AtomicU64,
    /// Une seule boîte de dialogue à la fois.
    dialog_lock: tokio::sync::Mutex<()>,
}

impl Default for PromptHub {
    fn default() -> Self {
        PromptHub {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            dialog_lock: tokio::sync::Mutex::new(()),
        }
    }
}

impl PromptHub {
    /// Émet la question avec `emit` puis attend la réponse.
    pub async fn ask(
        &self,
        request: PromptRequest,
        emit: impl Fn(PromptEvent) -> bool,
    ) -> PromptReply {
        let _guard = self.dialog_lock.lock().await;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if !emit(PromptEvent { id, request }) {
            self.pending.lock().unwrap().remove(&id);
            return PromptReply::Cancel;
        }
        rx.await.unwrap_or(PromptReply::Cancel)
    }

    pub fn reply(&self, id: u64, reply: PromptReply) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(reply);
        }
    }
}

/// Implémentation Tauri : événements « log », « prompt », « transfer-finished »…
pub struct TauriUi {
    app: AppHandle,
    pub hub: Arc<PromptHub>,
}

impl TauriUi {
    pub fn new(app: AppHandle, hub: Arc<PromptHub>) -> Self {
        TauriUi { app, hub }
    }
}

impl Ui for TauriUi {
    fn log(&self, level: &str, message: String) {
        let line = LogLine {
            time: now_hms(),
            level: level.to_string(),
            message,
        };
        let _ = self.app.emit("log", line);
    }

    fn ask(&self, request: PromptRequest) -> BoxFut<'_, PromptReply> {
        // les questions s'affichent dans la fenêtre principale : la ramener
        // devant si l'utilisateur est dans un terminal (reconnexion, code TOTP)
        let in_terminal = self.app.webview_windows().iter().any(|(label, w)| {
            crate::commands::is_term_window(label) && w.is_focused().unwrap_or(false)
        });
        if in_terminal && let Some(main) = self.app.get_webview_window("main") {
            let _ = main.unminimize();
            let _ = main.set_focus();
        }
        Box::pin(
            self.hub
                .ask(request, |ev| self.app.emit("prompt", ev).is_ok()),
        )
    }

    fn transfer_finished(&self, event: TransferFinished) {
        let _ = self.app.emit("transfer-finished", event);
    }

    fn edit_changed(&self, event: EditChanged) {
        let _ = self.app.emit("edit-changed", event);
    }
}
