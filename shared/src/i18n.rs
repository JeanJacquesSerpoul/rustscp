//! Langue de l'interface et des messages (français / anglais).
//!
//! La langue est globale : choisie au démarrage (réglage « language » ou, en
//! automatique, la locale du système) puis lue par `tr!` / `trf!`.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Fr,
    #[default]
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Fr => "fr",
            Lang::En => "en",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(1);

pub fn set_lang(lang: Lang) {
    LANG.store(if lang == Lang::Fr { 0 } else { 1 }, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 0 {
        Lang::Fr
    } else {
        Lang::En
    }
}

pub fn is_fr() -> bool {
    lang() == Lang::Fr
}

/// Texte dans la langue courante.
pub fn pick(fr: &'static str, en: &'static str) -> &'static str {
    if is_fr() { fr } else { en }
}

/// Réglage `language` ("auto" | "fr" | "en") ; en automatique, d'après la
/// locale (`LC_ALL`, `LC_MESSAGES`, `LANG`…) : français si elle commence par « fr ».
pub fn resolve(setting: &str, locale: &str) -> Lang {
    match setting {
        "fr" => Lang::Fr,
        "en" => Lang::En,
        _ if locale.trim().to_lowercase().starts_with("fr") => Lang::Fr,
        _ => Lang::En,
    }
}

/// Choix proposés dans les paramètres.
pub fn language_choices() -> [(&'static str, &'static str); 3] {
    [
        (
            "auto",
            pick(
                "Automatique (langue du système)",
                "Automatic (system language)",
            ),
        ),
        ("fr", "Français"),
        ("en", "English"),
    ]
}

/// Texte statique : `tr!("Bonjour", "Hello")`.
#[macro_export]
macro_rules! tr {
    ($fr:expr, $en:expr $(,)?) => {
        $crate::i18n::pick($fr, $en)
    };
}

/// Texte formaté : `trf!("Connexion à {host}", "Connecting to {host}")`.
#[macro_export]
macro_rules! trf {
    ($fr:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_fr() {
            ::std::format!($fr $(, $arg)*)
        } else {
            ::std::format!($en $(, $arg)*)
        }
    };
}

#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution() {
        assert_eq!(resolve("auto", "fr_FR.UTF-8"), Lang::Fr);
        assert_eq!(resolve("auto", "en_US.UTF-8"), Lang::En);
        assert_eq!(resolve("auto", ""), Lang::En);
        assert_eq!(resolve("fr", "en_US.UTF-8"), Lang::Fr);
        assert_eq!(resolve("en", "fr_FR.UTF-8"), Lang::En);
    }

    #[test]
    fn macros() {
        let _g = TEST_LOCK.lock().unwrap();
        let n = 3;
        set_lang(Lang::Fr);
        assert_eq!(trf!("{n} éléments", "{n} items"), "3 éléments");
        assert_eq!(tr!("Oui", "Yes"), "Oui");
        set_lang(Lang::En);
        assert_eq!(trf!("{n} éléments", "{n} items"), "3 items");
        assert_eq!(tr!("Oui", "Yes"), "Yes");
    }
}
