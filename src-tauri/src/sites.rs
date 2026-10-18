//! Gestionnaire de sites : profils de connexion et secrets associés.
//!
//! Les profils sont stockés en JSON (~/.config/rustscp/sites.json). Les secrets (mot de passe, phrase de passe de la clé, secret
//! TOTP) ne sont jamais écrits en clair : ils vont dans le trousseau du système
//! (Secret Service / KWallet) lorsqu'il est disponible.

use std::collections::HashMap;
use std::sync::Mutex;

use rustscp_shared::{SECRET_KINDS, SitesFile, tr};

use crate::config::{config_dir, read_json, write_json_atomic};

pub fn load_sites() -> SitesFile {
    let mut file: SitesFile = read_json(&config_dir().join("sites.json")).unwrap_or_default();
    for site in &mut file.sites {
        if site.id.is_empty() {
            site.id = uuid::Uuid::new_v4().simple().to_string();
        }
    }
    file
}

pub fn save_sites(file: &SitesFile) -> std::io::Result<()> {
    write_json_atomic(&config_dir().join("sites.json"), file)
}

/// Accès au trousseau système, avec repli en mémoire si indisponible.
pub struct SecretStore {
    memory: Mutex<HashMap<String, String>>,
    pub available: bool,
}

const SERVICE: &str = "rustscp";

fn key(site_id: &str, kind: &str) -> String {
    format!("{site_id}:{kind}")
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore {
    pub fn new() -> Self {
        let available =
            match keyring::Entry::new(SERVICE, "__probe__").and_then(|e| e.get_password()) {
                Ok(_) | Err(keyring::Error::NoEntry) => true,
                Err(err) => {
                    log::warn!("trousseau indisponible : {err}");
                    false
                }
            };
        SecretStore {
            memory: Mutex::new(HashMap::new()),
            available,
        }
    }

    /// Stockage en mémoire uniquement (tests).
    pub fn in_memory() -> Self {
        SecretStore {
            memory: Mutex::new(HashMap::new()),
            available: false,
        }
    }

    /// Où les secrets sont conservés (pour l'affichage).
    pub fn backend_name(&self) -> &'static str {
        if self.available && cfg!(windows) {
            tr!(
                "gestionnaire d'identification Windows",
                "Windows Credential Manager"
            )
        } else if self.available {
            tr!(
                "trousseau système (Secret Service)",
                "system keyring (Secret Service)"
            )
        } else {
            tr!("mémoire (non persistant)", "memory (not persistent)")
        }
    }

    fn entry(service: &str, site_id: &str, kind: &str) -> Option<keyring::Entry> {
        keyring::Entry::new(service, &key(site_id, kind)).ok()
    }

    pub fn get(&self, site_id: &str, kind: &str) -> String {
        if let Some(v) = self.memory.lock().unwrap().get(&key(site_id, kind)) {
            return v.clone();
        }
        if self.available
            && let Some(entry) = Self::entry(SERVICE, site_id, kind)
        {
            return entry.get_password().unwrap_or_default();
        }
        String::new()
    }

    /// Retourne vrai si le secret est stocké de façon persistante.
    pub fn set(&self, site_id: &str, kind: &str, value: &str) -> bool {
        if value.is_empty() {
            self.delete(site_id, kind);
            return true;
        }
        if self.available
            && let Some(entry) = Self::entry(SERVICE, site_id, kind)
            && entry.set_password(value).is_ok()
        {
            self.memory.lock().unwrap().remove(&key(site_id, kind));
            return true;
        }
        self.memory
            .lock()
            .unwrap()
            .insert(key(site_id, kind), value.to_string());
        false
    }

    pub fn delete(&self, site_id: &str, kind: &str) {
        self.memory.lock().unwrap().remove(&key(site_id, kind));
        if self.available
            && let Some(entry) = Self::entry(SERVICE, site_id, kind)
        {
            let _ = entry.delete_credential();
        }
    }

    pub fn delete_site(&self, site_id: &str) {
        for kind in SECRET_KINDS {
            self.delete(site_id, kind);
        }
    }

    pub fn copy_site(&self, src: &str, dst: &str) {
        for kind in SECRET_KINDS {
            let value = self.get(src, kind);
            if !value.is_empty() {
                self.set(dst, kind, &value);
            }
        }
    }
}
