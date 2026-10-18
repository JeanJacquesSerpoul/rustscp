//! Emplacements de configuration et réglages persistants.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustscp_shared::Settings;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let path = base.join("rustscp");
    let _ = fs::create_dir_all(&path);
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o700));
    path
}

pub fn write_json_atomic<T: Serialize>(path: &Path, data: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut fh = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        let text = serde_json::to_string_pretty(data).map_err(std::io::Error::other)?;
        fh.write_all(text.as_bytes())?;
        fh.sync_all()?;
    }
    fs::rename(&tmp, path)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn load_settings() -> Settings {
    read_json(&config_dir().join("settings.json")).unwrap_or_default()
}

pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    write_json_atomic(&config_dir().join("settings.json"), settings)
}
