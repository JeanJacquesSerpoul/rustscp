//! Système de fichiers local (listing, opérations, ouverture).

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rustscp_shared::Entry;

fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.mtime()
}

pub fn list(path: &Path) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for item in fs::read_dir(path)? {
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
            mtime: mtime_of(&meta),
            mode: meta.permissions().mode(),
            is_dir: meta.is_dir(),
            is_link,
            owner: String::new(),
            group: String::new(),
        });
    }
    Ok(out)
}

/// Sous-dossiers de chaque ancêtre de `path` (pour déplier l'arborescence d'un coup).
pub fn tree_path(path: &Path, show_hidden: bool) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    let mut cur = Some(path.to_path_buf());
    while let Some(dir) = cur {
        if let Ok(rd) = fs::read_dir(&dir) {
            let mut names: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter(|e| fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| show_hidden || !n.starts_with('.'))
                .collect();
            names.sort_by_key(|a| a.to_lowercase());
            result.push((dir.to_string_lossy().into_owned(), names));
        }
        cur = dir.parent().map(Path::to_path_buf);
    }
    result
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Ouvre un fichier avec l'application par défaut du bureau.
pub fn open_with_default(path: &Path) {
    match Command::new("xdg-open")
        .arg(path)
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
        Err(e) => log::warn!("xdg-open : {e}"),
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
