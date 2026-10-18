//! Petites fonctions utilitaires partagées.

use crate::i18n::is_fr;
use crate::{tr, trf};

const UNITS_FR: [&str; 6] = ["o", "Kio", "Mio", "Gio", "Tio", "Pio"];
const UNITS_EN: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

pub fn human_size(n: u64) -> String {
    let units = if is_fr() { UNITS_FR } else { UNITS_EN };
    let mut v = n as f64;
    for (i, unit) in units.iter().enumerate() {
        if v < 1024.0 || i == units.len() - 1 {
            return if i == 0 {
                format!("{n} {unit}")
            } else {
                format!("{v:.1} {unit}")
            };
        }
        v /= 1024.0;
    }
    String::new()
}

pub fn human_speed(bps: f64) -> String {
    if bps > 0.0 {
        format!("{}/s", human_size(bps as u64))
    } else {
        String::new()
    }
}

pub fn human_duration(seconds: Option<f64>) -> String {
    match seconds {
        Some(s) if (0.0..=86400.0 * 99.0).contains(&s) => {
            let s = s as u64;
            let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
            if h > 0 {
                format!("{h}:{m:02}:{s:02}")
            } else {
                format!("{m:02}:{s:02}")
            }
        }
        _ => String::new(),
    }
}

/// Entier avec séparateur de milliers (espace en français, virgule en anglais).
pub fn fmt_int(n: u64) -> String {
    let sep = if is_fr() { ' ' } else { ',' };
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFSOCK: u32 = 0o140000;

/// Équivalent de `stat.filemode` : « drwxr-xr-x ».
pub fn filemode(mode: u32) -> String {
    if mode == 0 {
        return String::new();
    }
    let t = match mode & S_IFMT {
        S_IFDIR => 'd',
        S_IFLNK => 'l',
        S_IFBLK => 'b',
        S_IFCHR => 'c',
        S_IFIFO => 'p',
        S_IFSOCK => 's',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(t);
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    for (i, (bit, ch)) in bits.iter().enumerate() {
        let set = mode & bit != 0;
        let special = match i {
            2 => mode & 0o4000 != 0,
            5 => mode & 0o2000 != 0,
            8 => mode & 0o1000 != 0,
            _ => false,
        };
        s.push(match (special, set, i) {
            (true, true, 8) => 't',
            (true, false, 8) => 'T',
            (true, true, _) => 's',
            (true, false, _) => 'S',
            (false, true, _) => *ch,
            (false, false, _) => '-',
        });
    }
    s
}

// --------------------------------------------------------- chemins distants
/// Jointure de chemins distants (toujours POSIX).
pub fn rjoin(base: &str, name: &str) -> String {
    if name.starts_with('/') || base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

pub fn rparent(path: &str) -> String {
    if path.is_empty() || path == "/" {
        return "/".into();
    }
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => trimmed[..i].to_string(),
    }
}

pub fn rbasename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// Normalisation à la `posixpath.normpath` (chemin absolu attendu).
pub fn rnormalize(path: &str) -> String {
    if path.is_empty() {
        return "/".into();
    }
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        "/".into()
    } else {
        joined
    }
}

/// Extension en minuscules (sans le point), ou "" s'il n'y en a pas.
pub fn extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

// -------------------------------------------------------------------- TOTP
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Accepte un secret base32 brut ou une URI otpauth:// (QR code).
pub fn parse_otp_secret(text: &str) -> String {
    let mut text = text.trim().to_string();
    if text.to_lowercase().starts_with("otpauth://") {
        let query = text.split_once('?').map(|(_, q)| q).unwrap_or("");
        let query = query.split('#').next().unwrap_or("");
        text = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| k.eq_ignore_ascii_case("secret"))
            .map(|(_, v)| percent_decode(v))
            .unwrap_or_default();
    }
    text.replace(' ', "").to_uppercase()
}

// -------------------------------------------------------------- filtrage
/// Motif de type shell (`*`, `?`, `[abc]`, `[!a-z]`), insensible à la casse
/// si les deux arguments sont déjà en minuscules.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some(pi);
                    mark = ti;
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, next)) = match_class(&p, pi, t[ti]) {
                        if matched {
                            pi = next;
                            ti += 1;
                            continue;
                        }
                    } else if t[ti] == '[' {
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                c if c == t[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some(s) => {
                pi = s + 1;
                mark += 1;
                ti = mark;
            }
            None => return false,
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Retourne (correspond, indice après la classe) ou None si la classe n'est pas fermée.
fn match_class(p: &[char], start: usize, c: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = i < p.len() && (p[i] == '!' || p[i] == '^');
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() && (p[i] != ']' || first) {
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if p[i] <= c && c <= p[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if p[i] == c {
                matched = true;
            }
            i += 1;
        }
    }
    if i >= p.len() {
        return None;
    }
    Some((matched != negate, i + 1))
}

/// Filtre instantané : sous-chaîne, ou motif si le texte contient `*?[`.
pub fn name_matches(filter_lower: &str, name: &str) -> bool {
    if filter_lower.is_empty() {
        return true;
    }
    let name = name.to_lowercase();
    if filter_lower.contains(['*', '?', '[']) {
        glob_match(filter_lower, &name)
    } else {
        name.contains(filter_lower)
    }
}

// ---------------------------------------------------------- types de fichier
/// Description du type de fichier d'après l'extension.
pub fn file_type_for(name: &str) -> String {
    let ext = extension(name);
    let (fr, en) = match ext.as_str() {
        "" => return tr!("Fichier", "File").into(),
        "txt" | "text" => ("Document texte", "Text document"),
        "md" | "markdown" => ("Document Markdown", "Markdown document"),
        "log" => ("Journal", "Log file"),
        "conf" | "cfg" | "ini" | "toml" | "properties" => {
            ("Fichier de configuration", "Configuration file")
        }
        "json" => ("Document JSON", "JSON document"),
        "yaml" | "yml" => ("Document YAML", "YAML document"),
        "xml" => ("Document XML", "XML document"),
        "csv" => ("Document CSV", "CSV document"),
        "html" | "htm" => ("Page web HTML", "HTML web page"),
        "css" => ("Feuille de style CSS", "CSS stylesheet"),
        "js" | "mjs" => ("Script JavaScript", "JavaScript script"),
        "ts" => ("Script TypeScript", "TypeScript script"),
        "py" => ("Script Python", "Python script"),
        "rs" => ("Code source Rust", "Rust source code"),
        "c" => ("Code source C", "C source code"),
        "h" | "hpp" => ("En-tête C/C++", "C/C++ header"),
        "cpp" | "cc" | "cxx" => ("Code source C++", "C++ source code"),
        "java" => ("Code source Java", "Java source code"),
        "go" => ("Code source Go", "Go source code"),
        "rb" => ("Script Ruby", "Ruby script"),
        "php" => ("Script PHP", "PHP script"),
        "pl" => ("Script Perl", "Perl script"),
        "sh" | "bash" | "zsh" => ("Script shell", "Shell script"),
        "sql" => ("Script SQL", "SQL script"),
        "pdf" => ("Document PDF", "PDF document"),
        "doc" | "docx" | "odt" | "rtf" => ("Document texte formaté", "Word processor document"),
        "xls" | "xlsx" | "ods" => ("Feuille de calcul", "Spreadsheet"),
        "ppt" | "pptx" | "odp" => ("Présentation", "Presentation"),
        "png" => ("Image PNG", "PNG image"),
        "jpg" | "jpeg" => ("Image JPEG", "JPEG image"),
        "gif" => ("Image GIF", "GIF image"),
        "svg" => ("Image SVG", "SVG image"),
        "webp" => ("Image WebP", "WebP image"),
        "bmp" => ("Image BMP", "BMP image"),
        "ico" => ("Icône", "Icon"),
        "tif" | "tiff" => ("Image TIFF", "TIFF image"),
        "mp3" => ("Audio MP3", "MP3 audio"),
        "ogg" | "oga" | "opus" => ("Audio Ogg", "Ogg audio"),
        "flac" => ("Audio FLAC", "FLAC audio"),
        "wav" => ("Audio WAV", "WAV audio"),
        "mp4" | "m4v" => ("Vidéo MP4", "MP4 video"),
        "mkv" => ("Vidéo Matroska", "Matroska video"),
        "avi" => ("Vidéo AVI", "AVI video"),
        "webm" => ("Vidéo WebM", "WebM video"),
        "mov" => ("Vidéo QuickTime", "QuickTime video"),
        "zip" => ("Archive ZIP", "ZIP archive"),
        "tar" => ("Archive tar", "tar archive"),
        "gz" | "tgz" => ("Archive gzip", "gzip archive"),
        "bz2" | "tbz2" => ("Archive bzip2", "bzip2 archive"),
        "xz" | "txz" => ("Archive XZ", "XZ archive"),
        "zst" => ("Archive Zstandard", "Zstandard archive"),
        "7z" => ("Archive 7-Zip", "7-Zip archive"),
        "rar" => ("Archive RAR", "RAR archive"),
        "deb" => ("Paquet Debian", "Debian package"),
        "rpm" => ("Paquet RPM", "RPM package"),
        "iso" | "img" | "qcow2" | "vmdk" => ("Image disque", "Disk image"),
        "exe" | "msi" => ("Exécutable Windows", "Windows executable"),
        "so" => ("Bibliothèque partagée", "Shared library"),
        "o" | "a" => ("Fichier objet", "Object file"),
        "pem" | "crt" | "cer" | "key" | "pub" => ("Clé ou certificat", "Key or certificate"),
        "bak" | "old" | "orig" => ("Copie de sauvegarde", "Backup copy"),
        "tmp" | "swp" => ("Fichier temporaire", "Temporary file"),
        "db" | "sqlite" | "sqlite3" => ("Base de données", "Database"),
        "ttf" | "otf" | "woff" | "woff2" => ("Police de caractères", "Font"),
        _ => return trf!("Fichier {}", "{} file", ext.to_uppercase()),
    };
    crate::i18n::pick(fr, en).into()
}

/// Catégorie d'icône d'après l'extension.
pub fn file_icon_kind(name: &str) -> &'static str {
    match extension(name).as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" | "ico" | "tif" | "tiff" => "image",
        "mp3" | "ogg" | "oga" | "opus" | "flac" | "wav" => "audio",
        "mp4" | "m4v" | "mkv" | "avi" | "webm" | "mov" => "video",
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "tbz2" | "xz" | "txz" | "zst" | "7z" | "rar"
        | "deb" | "rpm" | "iso" => "archive",
        "rs" | "c" | "h" | "hpp" | "cpp" | "cc" | "py" | "js" | "mjs" | "ts" | "java" | "go"
        | "rb" | "php" | "pl" | "sh" | "bash" | "zsh" | "sql" | "html" | "htm" | "css" | "json"
        | "xml" | "yaml" | "yml" | "toml" => "code",
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "xls" | "xlsx" | "ods" | "ppt" | "pptx"
        | "odp" => "document",
        "pem" | "crt" | "cer" | "key" | "pub" => "key",
        _ => "file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        let _g = crate::i18n::TEST_LOCK.lock().unwrap();
        crate::i18n::set_lang(crate::i18n::Lang::Fr);
        assert_eq!(human_size(0), "0 o");
        assert_eq!(human_size(1023), "1023 o");
        assert_eq!(human_size(1536), "1.5 Kio");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 Mio");
        assert_eq!(fmt_int(1234567), "1 234 567");
        assert_eq!(fmt_int(12), "12");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(fmt_int(1234567), "1,234,567");
        assert_eq!(file_type_for("a.png"), "PNG image");
        assert_eq!(file_type_for("a.xyz"), "XYZ file");
    }

    #[test]
    fn durations() {
        assert_eq!(human_duration(Some(65.0)), "01:05");
        assert_eq!(human_duration(Some(3725.0)), "1:02:05");
        assert_eq!(human_duration(None), "");
    }

    #[test]
    fn modes() {
        assert_eq!(filemode(0o040755), "drwxr-xr-x");
        assert_eq!(filemode(0o100644), "-rw-r--r--");
        assert_eq!(filemode(0o120777), "lrwxrwxrwx");
        assert_eq!(filemode(0o104755), "-rwsr-xr-x");
        assert_eq!(filemode(0o041777), "drwxrwxrwt");
    }

    #[test]
    fn paths() {
        assert_eq!(rjoin("/a", "b"), "/a/b");
        assert_eq!(rjoin("/", "b"), "/b");
        assert_eq!(rjoin("/a", "/c"), "/c");
        assert_eq!(rparent("/a/b"), "/a");
        assert_eq!(rparent("/a"), "/");
        assert_eq!(rparent("/"), "/");
        assert_eq!(rparent("/a/b/"), "/a");
        assert_eq!(rnormalize("/a/./b/../c//"), "/a/c");
        assert_eq!(rnormalize("/.."), "/");
        assert_eq!(rnormalize(""), "/");
        assert_eq!(rbasename("/a/b.txt"), "b.txt");
    }

    #[test]
    fn otp_secret() {
        assert_eq!(parse_otp_secret(" jbsw y3dp "), "JBSWY3DP");
        assert_eq!(
            parse_otp_secret("otpauth://totp/Ex:alice%40x?secret=JBSWY3DPEHPK3PXP&issuer=Ex"),
            "JBSWY3DPEHPK3PXP"
        );
    }

    #[test]
    fn globbing() {
        assert!(glob_match("*.log", "app.log"));
        assert!(!glob_match("*.log", "app.log.1"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("[ab]*", "bcd"));
        assert!(!glob_match("[!ab]*", "bcd"));
        assert!(glob_match("*", ""));
        assert!(name_matches("rep", "Report.txt"));
        assert!(name_matches("*.txt", "Report.TXT"));
    }
}
