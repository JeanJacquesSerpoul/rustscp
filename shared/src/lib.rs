//! Types et fonctions partagés entre le cœur (Tauri) et l'interface (Leptos/WASM).

use serde::{Deserialize, Serialize};

pub mod util;

pub const APP_NAME: &str = "RustSCP";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ------------------------------------------------------------------ fichiers
/// Entrée d'un listing (local ou distant).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    /// Date de modification (secondes depuis l'époque Unix).
    pub mtime: i64,
    /// Mode complet (type + droits), comme `st_mode`.
    pub mode: u32,
    pub is_dir: bool,
    #[serde(default)]
    pub is_link: bool,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub group: String,
}

/// Élément distant sélectionné (pour téléchargement, suppression, chmod…).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RemoteItem {
    pub path: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mode: u32,
}

// ------------------------------------------------------------------- sites
// Types d'authentification proposés dans l'interface. Le moteur suit de toute
// façon les méthodes exigées par le serveur (AuthenticationMethods), ce choix
// détermine les informations d'identification qu'il a à sa disposition.
pub const AUTH_KEY_OTP: &str = "key_otp";
pub const AUTH_KEY: &str = "key";
pub const AUTH_PASSWORD: &str = "password";
pub const AUTH_PASSWORD_OTP: &str = "password_otp";
pub const AUTH_KEY_PASSWORD_OTP: &str = "key_password_otp";
pub const AUTH_INTERACTIVE: &str = "interactive";

pub const AUTH_CHOICES: &[(&str, &str)] = &[
    (AUTH_KEY_OTP, "Clé SSH + code TOTP (2FA)"),
    (AUTH_KEY, "Clé SSH"),
    (AUTH_PASSWORD, "Mot de passe"),
    (AUTH_PASSWORD_OTP, "Mot de passe + code TOTP (2FA)"),
    (AUTH_KEY_PASSWORD_OTP, "Clé SSH + mot de passe + code TOTP"),
    (AUTH_INTERACTIVE, "Interactive (tout demander)"),
];

pub fn auth_label(auth: &str) -> &str {
    AUTH_CHOICES
        .iter()
        .find(|(k, _)| *k == auth)
        .map(|(_, l)| *l)
        .unwrap_or(auth)
}

pub const OTP_ASK: &str = "ask";
pub const OTP_SECRET: &str = "secret";

pub const PROTO_SCP: &str = "scp";
pub const PROTO_SFTP: &str = "sftp";

pub const SECRET_PASSWORD: &str = "password";
pub const SECRET_PASSPHRASE: &str = "passphrase";
pub const SECRET_OTP: &str = "otp";
pub const SECRET_KINDS: &[&str] = &[SECRET_PASSWORD, SECRET_PASSPHRASE, SECRET_OTP];

pub fn auth_uses_key(auth: &str) -> bool {
    matches!(
        auth,
        AUTH_KEY | AUTH_KEY_OTP | AUTH_KEY_PASSWORD_OTP | AUTH_INTERACTIVE
    )
}

pub fn auth_uses_password(auth: &str) -> bool {
    matches!(
        auth,
        AUTH_PASSWORD | AUTH_PASSWORD_OTP | AUTH_KEY_PASSWORD_OTP | AUTH_INTERACTIVE
    )
}

pub fn auth_uses_otp(auth: &str) -> bool {
    matches!(
        auth,
        AUTH_KEY_OTP | AUTH_PASSWORD_OTP | AUTH_KEY_PASSWORD_OTP | AUTH_INTERACTIVE
    )
}

/// Profil de connexion (format JSON compatible avec WSCP).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Site {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: String,
    pub key_file: String,
    pub use_agent: bool,
    pub otp_mode: String,
    pub protocol: String,
    pub remote_dir: String,
    pub local_dir: String,
    pub folder: String,
    pub keepalive: u32,
    pub compression: bool,
    pub comments: String,
    pub id: String,
}

impl Default for Site {
    fn default() -> Self {
        Site {
            name: "Nouveau site".into(),
            host: String::new(),
            port: 22,
            user: String::new(),
            auth: AUTH_KEY_OTP.into(),
            key_file: String::new(),
            use_agent: true,
            otp_mode: OTP_ASK.into(),
            protocol: PROTO_SCP.into(),
            remote_dir: String::new(),
            local_dir: String::new(),
            folder: String::new(),
            keepalive: 30,
            compression: false,
            comments: String::new(),
            id: String::new(),
        }
    }
}

impl Site {
    pub fn label(&self) -> String {
        if !self.name.is_empty() && self.name != self.host {
            self.name.clone()
        } else if !self.user.is_empty() {
            format!("{}@{}", self.user, self.host)
        } else {
            self.host.clone()
        }
    }

    pub fn address(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct SitesFile {
    pub folders: Vec<String>,
    pub sites: Vec<Site>,
}

// ---------------------------------------------------------------- réglages
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct QuickEntry {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: String,
    pub key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub max_transfers: u32,
    /// ask | overwrite | newer | size | newer_or_size | rename | skip
    pub overwrite_default: String,
    pub show_hidden: bool,
    pub confirm_delete: bool,
    pub preserve_times: bool,
    pub connect_timeout: u32,
    pub double_click_transfers: bool,
    pub quickconnect_history: Vec<QuickEntry>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_transfers: 3,
            overwrite_default: "ask".into(),
            show_hidden: false,
            confirm_delete: true,
            preserve_times: true,
            connect_timeout: 15,
            double_click_transfers: true,
            quickconnect_history: Vec::new(),
        }
    }
}

pub const OVERWRITE_CHOICES: &[(&str, &str)] = &[
    ("overwrite", "Écraser"),
    ("newer", "Écraser si la source est plus récente"),
    ("size", "Écraser si la taille est différente"),
    (
        "newer_or_size",
        "Écraser si plus récent ou taille différente",
    ),
    ("rename", "Renommer (ajouter un numéro)"),
    ("skip", "Ignorer"),
];

// -------------------------------------------------------------- transferts
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Up,
    Down,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TransferStatus {
    Queued,
    Active,
    Done,
    Failed,
    Cancelled,
    Skipped,
}

impl TransferStatus {
    pub fn label(self) -> &'static str {
        match self {
            TransferStatus::Queued => "En attente",
            TransferStatus::Active => "En cours",
            TransferStatus::Done => "Terminé",
            TransferStatus::Failed => "Échec",
            TransferStatus::Cancelled => "Annulé",
            TransferStatus::Skipped => "Ignoré (existe déjà)",
        }
    }

    /// Onglet de la file : 0 = en attente, 1 = échecs, 2 = réussis.
    pub fn group(self) -> usize {
        match self {
            TransferStatus::Queued | TransferStatus::Active => 0,
            TransferStatus::Failed | TransferStatus::Cancelled => 1,
            TransferStatus::Done | TransferStatus::Skipped => 2,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TransferView {
    pub id: u64,
    pub direction: Direction,
    pub local: String,
    pub remote: String,
    pub size: u64,
    pub transferred: u64,
    pub speed: f64,
    pub status: TransferStatus,
    pub error: String,
    pub protocol: String,
    pub server: String,
    pub session_id: u64,
    pub tag: String,
}

impl TransferView {
    pub fn name(&self) -> &str {
        let p = match self.direction {
            Direction::Up => &self.local,
            Direction::Down => &self.remote,
        };
        p.rsplit('/').next().unwrap_or(p)
    }

    pub fn fraction(&self) -> f64 {
        if self.size == 0 {
            if self.status == TransferStatus::Done {
                1.0
            } else {
                0.0
            }
        } else {
            (self.transferred as f64 / self.size as f64).min(1.0)
        }
    }

    pub fn eta(&self) -> Option<f64> {
        if self.speed <= 0.0 || self.size == 0 {
            return None;
        }
        Some((self.size.saturating_sub(self.transferred)) as f64 / self.speed)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TransferProgress {
    pub id: u64,
    pub transferred: u64,
    pub size: u64,
    pub speed: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct TransferStats {
    pub queued: u64,
    pub active: u64,
    pub speed: f64,
}

/// Réponse à l'interrogation périodique de la file (incrémentale).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct TransferPoll {
    pub version: u64,
    /// Vrai si `upserts` contient toute la file (resynchronisation complète).
    pub reset: bool,
    pub upserts: Vec<TransferView>,
    pub removed: Vec<u64>,
    pub progress: Vec<TransferProgress>,
    pub stats: TransferStats,
    pub running: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TransferFinished {
    pub id: u64,
    pub session_id: u64,
    pub direction: Direction,
    pub local: String,
    pub remote: String,
    pub status: TransferStatus,
    pub tag: String,
}

/// Résultat de la préparation d'un envoi / téléchargement.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct EnqueueResult {
    pub count: u64,
    pub total: u64,
}

// ---------------------------------------------------------------- sessions
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SessionInfo {
    pub session_id: u64,
    pub home: String,
    pub connected: bool,
    /// None : pas encore déterminé.
    pub shell_ok: Option<bool>,
    pub sftp: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EditChanged {
    pub session_id: u64,
    pub local: String,
    pub remote: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LogLine {
    pub time: String,
    /// status | command | response | error | trace
    pub level: String,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SecretBackend {
    pub available: bool,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TotpPreview {
    pub code: String,
    pub left: u64,
}

// ------------------------------------------------------- dialogues (cœur → UI)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PromptField {
    pub text: String,
    pub echo: bool,
    /// otp | password | other
    pub kind: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind")]
pub enum PromptRequest {
    HostKey {
        host: String,
        key_type: String,
        fingerprint: String,
        /// unknown | changed
        status: String,
        old_fingerprint: String,
    },
    Secret {
        title: String,
        label: String,
    },
    Interactive {
        site_label: String,
        title: String,
        instructions: String,
        fields: Vec<PromptField>,
    },
    Overwrite {
        direction: Direction,
        source: String,
        target: String,
        src_size: u64,
        src_mtime: i64,
        dst_size: u64,
        dst_mtime: i64,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PromptEvent {
    pub id: u64,
    pub request: PromptRequest,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind")]
pub enum PromptReply {
    Cancel,
    /// "save" | "once"
    HostKey {
        choice: String,
    },
    Secret {
        value: String,
        remember: bool,
    },
    Interactive {
        answers: Vec<String>,
    },
    Overwrite {
        action: String,
        apply_all: bool,
    },
}
