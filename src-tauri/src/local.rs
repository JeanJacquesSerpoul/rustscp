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

/// Ouvre un fichier avec l'éditeur de texte par défaut (celui des « .txt »),
/// quelle que soit son extension.
pub fn open_as_text(path: &Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        if !open_as_text_native(&path) {
            log::warn!("éditeur de texte introuvable pour {}", path.display());
            open_with_default(&path);
        }
    });
}

#[cfg(windows)]
fn open_as_text_native(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_CLASSNAME, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
        ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    let file = wide(path.as_os_str());
    let class = wide(".txt".as_ref());
    let verb = wide("open".as_ref());
    // SAFETY: structure initialisée à zéro puis remplie ; les chaînes vivent jusqu'à la fin de l'appel
    unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as _,
        );
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_CLASSNAME | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpClass = class.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        if ShellExecuteExW(&mut info) != 0 {
            return true;
        }
    }
    // association absente : le Bloc-notes est toujours là
    std::process::Command::new("notepad.exe")
        .arg(path)
        .spawn()
        .map(|mut c| {
            let _ = c.wait();
        })
        .is_ok()
}

#[cfg(not(windows))]
fn open_as_text_native(path: &Path) -> bool {
    use std::process::Command;
    let quiet = |mut c: Command| {
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c.status().is_ok_and(|s| s.success())
    };
    // application par défaut des text/plain, lancée par son fichier .desktop
    let Some(id) = Command::new("xdg-mime")
        .args(["query", "default", "text/plain"])
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|id| id.ends_with(".desktop"))
    else {
        return false;
    };
    if let Some(desktop) = desktop_file(&id) {
        let mut c = Command::new("gio");
        c.arg("launch").arg(desktop).arg(path);
        if quiet(c) {
            return true;
        }
    }
    let mut c = Command::new("gtk-launch");
    c.arg(&id).arg(path);
    quiet(c)
}

/// Chemin d'un fichier .desktop d'après son identifiant (répertoires XDG).
#[cfg(not(windows))]
fn desktop_file(id: &str) -> Option<PathBuf> {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"));
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    std::iter::once(data_home)
        .chain(dirs.split(':').map(PathBuf::from))
        .map(|d| d.join("applications").join(id))
        .find(|p| p.is_file())
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
