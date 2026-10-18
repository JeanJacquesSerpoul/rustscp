//! Codes TOTP (RFC 6238, compatibles Google Authenticator) et protection
//! contre la réutilisation d'un code déjà consommé (`DISALLOW_REUSE`).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha1::Sha1;

pub const INTERVAL: u64 = 30;
const DIGITS: u32 = 6;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn decode_secret(secret: &str) -> Result<Vec<u8>, String> {
    let clean: String = secret
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect::<String>()
        .trim_end_matches('=')
        .to_string();
    if clean.is_empty() {
        return Err("secret vide".into());
    }
    data_encoding::BASE32_NOPAD
        .decode(clean.as_bytes())
        .map_err(|_| "secret invalide (base32 attendu)".to_string())
}

pub fn code_at(key: &[u8], counter: u64) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC accepte toute longueur de clé");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let bin = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);
    format!(
        "{:0width$}",
        bin % 10u32.pow(DIGITS),
        width = DIGITS as usize
    )
}

/// Code courant et secondes restantes avant expiration.
pub fn current(secret: &str) -> Result<(String, u64), String> {
    let key = decode_secret(secret)?;
    let t = now();
    Ok((code_at(&key, t / INTERVAL), INTERVAL - t % INTERVAL))
}

static LAST: LazyLock<Mutex<HashMap<String, (u64, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Mémorise un code saisi manuellement pour ne pas le régénérer à la reconnexion.
pub fn mark_used(key: &str, code: &str) {
    LAST.lock()
        .unwrap()
        .insert(key.to_string(), (now() / INTERVAL, code.trim().to_string()));
}

/// Prochain code jamais utilisé pour cette clé : attend la fenêtre suivante si nécessaire.
pub async fn next_code(key: &str, secret: &str, log: impl Fn(String)) -> Result<String, String> {
    let k = decode_secret(secret)?;
    loop {
        let wait = {
            let mut last = LAST.lock().unwrap();
            let t = now();
            let counter = t / INTERVAL;
            if last.get(key).is_none_or(|(c, _)| *c != counter) {
                let code = code_at(&k, counter);
                last.insert(key.to_string(), (counter, code.clone()));
                return Ok(code);
            }
            INTERVAL - t % INTERVAL
        };
        log(format!(
            "Code TOTP courant déjà utilisé, attente du suivant ({wait} s)…"
        ));
        tokio::time::sleep(std::time::Duration::from_millis(wait * 1000 + 200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_vectors() {
        // RFC 6238, annexe B (SHA-1, 8 chiffres → on vérifie les 6 derniers)
        let key = b"12345678901234567890";
        assert_eq!(code_at(key, 59 / 30), "287082");
        assert_eq!(code_at(key, 1111111109 / 30), "081804");
        assert_eq!(code_at(key, 1234567890 / 30), "005924");
    }

    #[test]
    fn base32() {
        assert_eq!(
            decode_secret("JBSWY3DPEHPK3PXP").unwrap(),
            b"Hello!\xde\xad\xbe\xef"
        );
        assert_eq!(
            decode_secret("jbsw y3dp ehpk 3pxp").unwrap(),
            b"Hello!\xde\xad\xbe\xef"
        );
        assert!(decode_secret("!!!").is_err());
    }
}
