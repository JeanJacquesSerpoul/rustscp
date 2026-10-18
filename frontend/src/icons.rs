//! Icônes vectorielles (traits 24×24, couleur héritée du texte).

use leptos::prelude::*;

fn paths(name: &str) -> &'static str {
    match name {
        "folder" => {
            r#"<path class="fill" d="M3 7a2 2 0 0 1 2-2h4.2l2 2H19a2 2 0 0 1 2 2v8.5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
        }
        "folder-open" => {
            r#"<path class="fill" d="M3 7a2 2 0 0 1 2-2h4.2l2 2H18a2 2 0 0 1 2 2v1H7.5L4.6 18.6 3 17z"/><path class="fill" d="M7.5 10H22l-3 9.5H4.4z"/>"#
        }
        "file" => r#"<path class="fill" d="M6 2.8h8.2L19 7.6V21.2H6z"/><path d="M14 3v5h5"/>"#,
        "up" => r#"<path d="M12 19V6M6 11.5 12 5.5l6 6"/>"#,
        "parent" => r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>"#,
        "home" => {
            r#"<path d="M3.5 11 12 3.5l8.5 7.5"/><path d="M5.5 9.5V20h13V9.5"/><path d="M10 20v-6h4v6"/>"#
        }
        "refresh" => r#"<path d="M20 12a8 8 0 1 1-2.6-5.9"/><path d="M20 4v5h-5"/>"#,
        "server" => {
            r#"<rect x="3.5" y="4" width="17" height="7" rx="1.5"/><rect x="3.5" y="13" width="17" height="7" rx="1.5"/><path d="M7.5 7.5h.01M7.5 16.5h.01"/>"#
        }
        "connect" => {
            r#"<path d="M9 2.5v4.5M15 2.5v4.5"/><path d="M6 7h12v3.5a6 6 0 0 1-12 0z"/><path d="M12 16.5v5"/>"#
        }
        "disconnect" => {
            r#"<path d="M9 2.5v4.5M15 2.5v4.5"/><path d="M6 7h12v3.5a6 6 0 0 1-12 0z"/><path d="M12 16.5v5"/><path d="M3 3l18 18"/>"#
        }
        "tab-new" => {
            r#"<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M12 9v6M9 12h6"/>"#
        }
        "pause" => r#"<path d="M8.5 5v14M15.5 5v14"/>"#,
        "play" => r#"<path class="fill" d="M7 4.5 19 12 7 19.5z"/>"#,
        "stop" => r#"<circle cx="12" cy="12" r="9"/><path d="M9 9l6 6M15 9l-6 6"/>"#,
        "eye" => {
            r#"<path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>"#
        }
        "eye-off" => {
            r#"<path d="M3 3l18 18"/><path d="M10.6 5.1A10 10 0 0 1 12 5c6.4 0 10 7 10 7a17 17 0 0 1-3.2 4.2M6.6 6.6C3.7 8.5 2 12 2 12s3.6 7 10 7a9.6 9.6 0 0 0 5.4-1.6"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/>"#
        }
        "settings" => {
            r#"<path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1"/><circle cx="15" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="17" cy="18" r="2"/>"#
        }
        "info" => r#"<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.5h.01"/>"#,
        "trash" => r#"<path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/>"#,
        "rename" => r#"<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>"#,
        "copy" => {
            r#"<rect x="8" y="8" width="12.5" height="12.5" rx="2"/><path d="M16 8V5.5A1.5 1.5 0 0 0 14.5 4h-9A1.5 1.5 0 0 0 4 5.5v9A1.5 1.5 0 0 0 5.5 16H8"/>"#
        }
        "folder-new" => {
            r#"<path d="M3 7a2 2 0 0 1 2-2h4.2l2 2H19a2 2 0 0 1 2 2v8.5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M12 10.5v6M9 13.5h6"/>"#
        }
        "file-new" => r#"<path d="M6 2.8h8.2L19 7.6V21.2H6z"/><path d="M12 10.5v7M8.5 14h7"/>"#,
        "download" => r#"<path d="M12 3.5v11M7 10l5 5 5-5M4.5 20h15"/>"#,
        "upload" => r#"<path d="M12 20V9M7 13.5l5-5 5 5M4.5 4h15"/>"#,
        "edit" => {
            r#"<path d="M14 3H6v18h12V7z"/><path d="M14 3v4h4"/><path d="M9 17h2l5-5-2-2-5 5z"/>"#
        }
        "lock" => {
            r#"<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3"/>"#
        }
        "key" => {
            r#"<circle cx="7.5" cy="15.5" r="4"/><path d="M10.5 12.5 20 3M16 7l3 3M13.5 9.5l2 2"/>"#
        }
        "shield" => {
            r#"<path d="M12 3 4.5 6v6c0 4.6 3.2 7.8 7.5 9 4.3-1.2 7.5-4.4 7.5-9V6z"/><path d="M8.8 12.2l2.2 2.2 4.4-4.6"/>"#
        }
        "warning" => r#"<path d="M12 3.5 2.5 20h19z"/><path d="M12 10v4.5M12 17.5h.01"/>"#,
        "plus" => r#"<path d="M12 5v14M5 12h14"/>"#,
        "close" => r#"<path d="M6 6l12 12M18 6 6 18"/>"#,
        "chevron-right" => r#"<path d="M9.5 6l6 6-6 6"/>"#,
        "chevron-down" => r#"<path d="M6 9.5l6 6 6-6"/>"#,
        "arrow-right" => r#"<path d="M4 12h15M13 6l6 6-6 6"/>"#,
        "arrow-left" => r#"<path d="M20 12H5M11 6l-6 6 6 6"/>"#,
        "check" => r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#,
        "search" => r#"<circle cx="11" cy="11" r="6.5"/><path d="M20 20l-4.4-4.4"/>"#,
        "link" => {
            r#"<path d="M10 14a4.5 4.5 0 0 0 6.4 0l3.2-3.2a4.5 4.5 0 0 0-6.4-6.4L11.6 6"/><path d="M14 10a4.5 4.5 0 0 0-6.4 0l-3.2 3.2a4.5 4.5 0 0 0 6.4 6.4l1.6-1.6"/>"#
        }
        "menu" => r#"<path d="M4 7h16M4 12h16M4 17h16"/>"#,
        "image" => {
            r#"<path class="fill" d="M6 2.8h8.2L19 7.6V21.2H6z"/><path d="M14 3v5h5"/><path d="M8 18l3-3.5 2 2 1.5-1.5L17 18z" class="glyph"/>"#
        }
        "audio" => {
            r#"<path class="fill" d="M6 2.8h8.2L19 7.6V21.2H6z"/><path d="M14 3v5h5"/><path class="glyph" d="M11 17.5v-6l4-1v5.5"/><circle class="glyph" cx="10" cy="17.5" r="1.2"/><circle class="glyph" cx="14" cy="16" r="1.2"/>"#
        }
        "video" => {
            r#"<path class="fill" d="M6 2.8h8.2L19 7.6V21.2H6z"/><path d="M14 3v5h5"/><path class="glyph" d="M10 12.5v5.5l4.5-2.75z"/>"#
        }
        "archive" => {
            r#"<path class="fill" d="M6 2.8h8.2L19 7.6V21.2H6z"/><path d="M14 3v5h5"/><path class="glyph" d="M11 4v2h1.5v2H11v2h1.5v2H11v4h2.5v-4"/>"#
        }
        "code" => {
            r#"<path class="fill" d="M6 2.8h8.2L19 7.6V21.2H6z"/><path d="M14 3v5h5"/><path class="glyph" d="M10.5 12.5 8.5 15l2 2.5M14.5 12.5l2 2.5-2 2.5"/>"#
        }
        "document" => {
            r#"<path class="fill" d="M6 2.8h8.2L19 7.6V21.2H6z"/><path d="M14 3v5h5"/><path class="glyph" d="M9 12h7M9 15h7M9 18h5"/>"#
        }
        _ => r#"<circle cx="12" cy="12" r="8"/>"#,
    }
}

/// Icône par nom ; `class` permet d'ajouter une couleur (ex. « folder »).
pub fn icon(name: &'static str) -> impl IntoView {
    view! { <svg class=format!("ic ic-{name}") viewBox="0 0 24 24" aria-hidden="true" inner_html=paths(name)></svg> }
}

/// Icône d'un fichier d'après son nom.
pub fn file_icon(name: &str, is_dir: bool, is_link: bool) -> impl IntoView + use<> {
    let kind: &'static str = if is_dir {
        "folder"
    } else {
        match rustscp_shared::util::file_icon_kind(name) {
            "image" => "image",
            "audio" => "audio",
            "video" => "video",
            "archive" => "archive",
            "code" => "code",
            "document" => "document",
            "key" => "key",
            _ => "file",
        }
    };
    view! {
        <span class="file-ic">
            {icon(kind)}
            {is_link.then(|| view! { <span class="link-badge">{icon("link")}</span> })}
        </span>
    }
}

pub const LOGO: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><defs><linearGradient id="lg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#3b82c4"/><stop offset="1" stop-color="#1f4f86"/></linearGradient></defs><rect x="4" y="4" width="56" height="56" rx="12" fill="url(#lg)"/><path d="M14 24h26l-6-6M50 40H24l6 6" fill="none" stroke="#fff" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/><rect x="40" y="8" width="18" height="22" rx="4" fill="#f5b041" stroke="#1f4f86" stroke-width="2"/><path d="M44.5 17v-3a4.5 4.5 0 0 1 9 0v3" fill="none" stroke="#1f4f86" stroke-width="2.4"/><circle cx="49" cy="23" r="2.4" fill="#1f4f86"/></svg>"##;

pub fn logo(size: u32) -> impl IntoView {
    view! { <span class="logo" style=format!("width:{size}px;height:{size}px") inner_html=LOGO></span> }
}
