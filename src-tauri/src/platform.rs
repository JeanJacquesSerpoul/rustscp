//! Différences Unix / Windows : droits, dates, lectures positionnelles et chemins locaux.
//!
//! L'interface manipule les chemins locaux comme des chemins POSIX (séparateur « / »).
//! Sur Windows, « C:\Users\x » y devient « /C:/Users/x » et « / » liste les lecteurs ;
//! [`native_path`] et [`ui_path`] font la conversion (sans effet sur Unix).

use std::fs::{File, Metadata, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Droits de type Unix (sur Windows : déduits de l'attribut lecture seule).
pub fn mode(meta: &Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode()
    }
    #[cfg(not(unix))]
    {
        use rustscp_shared::util::{S_IFDIR, S_IFREG};
        let write = if meta.permissions().readonly() {
            0
        } else {
            0o200
        };
        if meta.is_dir() {
            S_IFDIR | 0o555 | write
        } else {
            S_IFREG | 0o444 | write
        }
    }
}

fn secs(t: std::io::Result<SystemTime>) -> i64 {
    match t.map(|t| t.duration_since(UNIX_EPOCH)) {
        Ok(Ok(d)) => d.as_secs() as i64,
        Ok(Err(e)) => -(e.duration().as_secs() as i64),
        Err(_) => 0,
    }
}

/// Date de modification (secondes depuis l'époque Unix).
pub fn mtime(meta: &Metadata) -> i64 {
    secs(meta.modified())
}

/// Date de dernier accès (secondes depuis l'époque Unix).
pub fn atime(meta: &Metadata) -> i64 {
    secs(meta.accessed())
}

/// Applique des droits Unix (sans effet sur Windows).
pub fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

/// Applique des droits Unix à un fichier ouvert (sans effet sur Windows).
pub fn set_file_mode(file: &File, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (file, mode);
}

/// Options d'ouverture d'un fichier lisible par son seul propriétaire (0600 sur Unix ;
/// sur Windows, le profil utilisateur est déjà privé).
pub fn private_options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut opts = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts
}

/// Lecture à une position donnée (sans déplacer de curseur partagé sur Unix).
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::FileExt::seek_read(file, buf, offset)
    }
}

/// Écriture complète à une position donnée.
pub fn write_all_at(file: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        let (mut buf, mut offset) = (buf, offset);
        while !buf.is_empty() {
            match std::os::windows::fs::FileExt::seek_write(file, buf, offset)? {
                0 => return Err(std::io::ErrorKind::WriteZero.into()),
                n => {
                    buf = &buf[n..];
                    offset += n as u64;
                }
            }
        }
        Ok(())
    }
}

/// Identifiant de l'utilisateur courant (noms de dossiers temporaires).
pub fn user_tag() -> String {
    #[cfg(unix)]
    {
        unsafe { libc::getuid() }.to_string()
    }
    #[cfg(not(unix))]
    {
        std::env::var("USERNAME").unwrap_or_default()
    }
}

/// Nom de l'utilisateur courant.
pub fn user_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
}

/// Ouvre un fichier avec l'application par défaut du bureau.
pub fn open_command(path: &Path) -> std::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("explorer");
        cmd.arg(path);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(path);
        cmd
    }
}

// ------------------------------------------------------------ chemins locaux
/// Vrai pour la racine virtuelle de l'interface (liste des lecteurs sur Windows).
pub fn is_virtual_root(ui: &str) -> bool {
    cfg!(windows) && ui.trim_end_matches(['/', '\\']).is_empty() && !ui.is_empty()
}

/// Lecteurs disponibles (« C: »…) ; vide hors Windows.
pub fn drives() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    (b'A'..=b'Z')
        .map(|c| format!("{}:", c as char))
        .filter(|d| Path::new(&format!("{d}\\")).exists())
        .collect()
}

/// Chemin de l'interface (ou chemin natif, `~` compris) → chemin natif.
pub fn native_path(ui: &str) -> PathBuf {
    if ui == "~" || ui.starts_with("~/") || (cfg!(windows) && ui.starts_with("~\\")) {
        let home = dirs::home_dir().unwrap_or_default();
        return match ui.get(2..) {
            Some(rest) if !rest.is_empty() => home.join(native_path(rest)),
            _ => home,
        };
    }
    #[cfg(windows)]
    {
        let b = ui.as_bytes();
        // « /C:/x » ou « /C: » → « C:\x », « C:\ »
        if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
            let mut s = ui[1..].replace('/', "\\");
            if s.len() == 2 {
                s.push('\\');
            }
            return PathBuf::from(s);
        }
        // « //serveur/partage » → « \\serveur\partage »
        if ui.starts_with("//") {
            return PathBuf::from(ui.replace('/', "\\"));
        }
        PathBuf::from(ui.replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(ui)
    }
}

/// Chemin natif → chemin de l'interface.
pub fn ui_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    #[cfg(windows)]
    {
        let s = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else {
            s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
        };
        let s = s.replace('\\', "/");
        let s = s.trim_end_matches('/');
        let b = s.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            format!("/{}{}", s[..1].to_ascii_uppercase(), &s[1..])
        } else if s.is_empty() {
            "/".into()
        } else {
            s.to_string()
        }
    }
    #[cfg(not(windows))]
    {
        s.into_owned()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(native_path("/C:/Users/x"), PathBuf::from(r"C:\Users\x"));
        assert_eq!(native_path("/C:"), PathBuf::from(r"C:\"));
        assert_eq!(native_path(r"D:\a"), PathBuf::from(r"D:\a"));
        assert_eq!(ui_path(Path::new(r"\\?\C:\Users\x")), "/C:/Users/x");
        assert_eq!(ui_path(Path::new(r"c:\")), "/C:");
        assert_eq!(ui_path(Path::new(r"\\srv\share\a")), "//srv/share/a");
        assert!(is_virtual_root("/"));
        assert!(!is_virtual_root("/C:"));
    }
}
