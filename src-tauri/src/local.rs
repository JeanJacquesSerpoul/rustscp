//! Système de fichiers local (listing, opérations, ouverture).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rustscp_shared::Entry;
use rustscp_shared::util::S_IFDIR;

use crate::platform::{self, drives, is_virtual_root, native_path, ui_path};

/// Liste un dossier (chemin de l'interface ; sur Windows, « / » liste les lecteurs).
pub fn list(ui: &str) -> std::io::Result<Vec<Entry>> {
    if is_virtual_root(ui) {
        return Ok(drives()
            .into_iter()
            .map(|name| Entry {
                name,
                size: 0,
                mtime: 0,
                mode: S_IFDIR | 0o755,
                is_dir: true,
                is_link: false,
                owner: String::new(),
                group: String::new(),
            })
            .collect());
    }
    let path = native_path(ui);
    let mut out = Vec::new();
    for item in fs::read_dir(&path)? {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        let Ok(lmeta) = item
            .metadata()
            .or_else(|_| fs::symlink_metadata(item.path()))
        else {
            continue;
        };
        let is_link = lmeta.file_type().is_symlink();
        // suit les liens pour connaître la nature de la cible
        let meta = if is_link {
            fs::metadata(item.path()).unwrap_or(lmeta)
        } else {
            lmeta
        };
        out.push(Entry {
            name,
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime: platform::mtime(&meta),
            mode: platform::mode(&meta),
            is_dir: meta.is_dir(),
            is_link,
            owner: String::new(),
            group: String::new(),
        });
    }
    Ok(out)
}

/// Sous-dossiers de chaque ancêtre de `path` (pour déplier l'arborescence d'un coup).
pub fn tree_path(ui: &str, show_hidden: bool) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    let mut cur = (!is_virtual_root(ui)).then(|| native_path(ui));
    while let Some(dir) = cur {
        if let Ok(rd) = fs::read_dir(&dir) {
            let mut names: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter(|e| fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| show_hidden || !n.starts_with('.'))
                .collect();
            names.sort_by_key(|a| a.to_lowercase());
            result.push((ui_path(&dir), names));
        }
        cur = dir.parent().map(Path::to_path_buf);
    }
    if cfg!(windows) {
        result.push(("/".into(), drives()));
    }
    result
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Ouvre un fichier avec l'application par défaut du bureau.
pub fn open_with_default(path: &Path) {
    match platform::open_command(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::warn!("ouverture de {} : {e}", path.display()),
    }
}

/// Clés privées usuelles (pour pré-remplir la connexion rapide).
pub fn default_key() -> String {
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        if home().join(".ssh").join(name).exists() {
            return format!("~/.ssh/{name}");
        }
    }
    String::new()
}
