//! Emplacements de configuration et réglages persistants.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rustscp_shared::i18n::{Lang, resolve, set_lang};
use rustscp_shared::{Settings, TermPrefs};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// `~/.config/rustscp` (ou `$XDG_CONFIG_HOME/rustscp`) ; `%APPDATA%\rustscp` sur Windows.
pub fn config_dir() -> PathBuf {
    let base = if cfg!(windows) {
        dirs::config_dir()
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
    }
    .unwrap_or_else(std::env::temp_dir);
    let path = base.join("rustscp");
    let _ = fs::create_dir_all(&path);
    crate::platform::set_mode(&path, 0o700);
    path
}

pub fn write_json_atomic<T: Serialize>(path: &Path, data: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut fh = crate::platform::private_options()
            .write(true)
            .create(true)
            .truncate(true)
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

/// Préférences du terminal (fichier séparé : modifiées depuis ses fenêtres).
pub fn load_term_prefs() -> TermPrefs {
    read_json(&config_dir().join("terminal.json")).unwrap_or_default()
}

pub fn save_term_prefs(prefs: &TermPrefs) -> std::io::Result<()> {
    write_json_atomic(&config_dir().join("terminal.json"), prefs)
}

/// Locale du système (première variable renseignée parmi LC_ALL, LC_MESSAGES, LANG, LANGUAGE ;
/// à défaut, sur Windows, langue de l'utilisateur).
pub fn system_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .or_else(windows_locale)
        .unwrap_or_default()
}

#[cfg(windows)]
fn windows_locale() -> Option<String> {
    sys_locale::get_locale()
}

#[cfg(not(windows))]
fn windows_locale() -> Option<String> {
    None
}

/// Applique le réglage de langue (auto : locale du système) ; retourne la langue choisie.
pub fn apply_language(settings: &Settings) -> Lang {
    let lang = resolve(&settings.language, &system_locale());
    set_lang(lang);
    lang
}
