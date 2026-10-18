//! Session SSH : connexion, vérification de la clé d'hôte et authentification multi-facteurs.
//!
//! Point clé : le moteur d'authentification enchaîne les méthodes exigées par le
//! serveur (`AuthenticationMethods publickey,keyboard-interactive` par exemple).
//! Une clé acceptée « partiellement » est suivie d'un échange keyboard-interactive
//! où le code TOTP est soit calculé automatiquement depuis le secret enregistré,
//! soit demandé à l'utilisateur.
//!
//! Une fois authentifiée, la connexion est **multiplexée** : la navigation et
//! chaque transfert parallèle ouvrent leur propre canal sur le même transport.
//! Le code TOTP n'est donc demandé qu'une seule fois, quel que soit le nombre de
//! transferts simultanés (FileZilla rouvre une connexion par transfert).

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, Handle, KeyboardInteractiveAuthResponse, Msg};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{
    Certificate, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate,
};
use russh::{Channel, ChannelMsg, MethodKind, MethodSet};
use rustscp_shared::{
    OTP_SECRET, PromptField, PromptReply, PromptRequest, SECRET_OTP, SECRET_PASSPHRASE,
    SECRET_PASSWORD, Site, auth_uses_key, auth_uses_otp, auth_uses_password,
};
use tokio::sync::Mutex as AsyncMutex;

use crate::config::config_dir;
use crate::error::{Error, Result};
use crate::sftp::Sftp;
use crate::sites::SecretStore;
use crate::totp;
use crate::ui::Ui;

/// Fenêtre SSH par canal : de grandes fenêtres = débit élevé sur les liaisons à latence.
pub const WINDOW_SIZE: u32 = 16 * 1024 * 1024;
pub const MAX_PACKET: u32 = 32 * 1024;

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

pub fn classify_prompt(text: &str) -> &'static str {
    let t = text.to_lowercase();
    const OTP: [&str; 12] = [
        "verification",
        "vérification",
        "one-time",
        "one time",
        "onetime",
        "otp",
        "token",
        "jeton",
        "authenticator",
        "2fa",
        "totp",
        "passcode",
    ];
    let has_code_word = t.split(|c: char| !c.is_alphanumeric()).any(|w| w == "code");
    if OTP.iter().any(|w| t.contains(w)) || has_code_word {
        return "otp";
    }
    if ["password", "mot de passe", "passwort", "contraseña"]
        .iter()
        .any(|w| t.contains(w))
    {
        return "password";
    }
    "other"
}

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

fn method_names(set: &MethodSet) -> Vec<&'static str> {
    set.iter().map(<&'static str>::from).collect()
}

// ----------------------------------------------------------- clé d'hôte
struct HostCheck {
    ui: Arc<dyn Ui>,
    host: String,
    port: u16,
    /// Une question est posée à l'utilisateur (le délai de connexion est suspendu).
    prompting: AtomicBool,
    verified: AtomicBool,
    rejected: AtomicBool,
}

impl HostCheck {
    fn known_hosts_files() -> [PathBuf; 2] {
        let home = dirs::home_dir().unwrap_or_default();
        [
            home.join(".ssh").join("known_hosts"),
            config_dir().join("known_hosts"),
        ]
    }

    async fn verify(&self, server_key: &PublicKeyOrCertificate) -> bool {
        let key = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(cert) => {
                PublicKey::new(cert.public_key().clone(), "")
            }
        };
        let name = if self.port == 22 {
            self.host.clone()
        } else {
            format!("[{}]:{}", self.host, self.port)
        };
        let alg = key.algorithm();
        let fp = fingerprint(&key);
        let mut same_alg: Vec<PublicKey> = Vec::new();
        for path in Self::known_hosts_files() {
            match russh::keys::known_hosts::known_host_keys_path(&self.host, self.port, &path) {
                Ok(list) => same_alg.extend(
                    list.into_iter()
                        .map(|(_, k)| k)
                        .filter(|k| k.algorithm() == alg),
                ),
                Err(err) => self.ui.log(
                    "error",
                    format!("known_hosts illisible ({}) : {err}", path.display()),
                ),
            }
        }
        if same_alg.iter().any(|k| k.key_data() == key.key_data()) {
            self.ui.log(
                "status",
                format!("Clé d'hôte reconnue ({} {fp})", alg.as_str()),
            );
            self.verified.store(true, Ordering::SeqCst);
            return true;
        }
        let (status, old) = match same_alg.first() {
            Some(old) => ("changed", fingerprint(old)),
            None => ("unknown", String::new()),
        };
        self.prompting.store(true, Ordering::SeqCst);
        let reply = self
            .ui
            .ask(PromptRequest::HostKey {
                host: name.clone(),
                key_type: alg.as_str().to_string(),
                fingerprint: fp,
                status: status.into(),
                old_fingerprint: old,
            })
            .await;
        self.prompting.store(false, Ordering::SeqCst);
        match reply {
            PromptReply::HostKey { choice } => {
                if choice == "save" {
                    let own = config_dir().join("known_hosts");
                    match russh::keys::known_hosts::learn_known_hosts_path(
                        &self.host, self.port, &key, &own,
                    ) {
                        Ok(()) => {
                            set_private(&own);
                            self.ui.log(
                                "status",
                                format!("Clé d'hôte enregistrée dans {}", own.display()),
                            );
                        }
                        Err(err) => self.ui.log(
                            "error",
                            format!("Impossible d'enregistrer la clé d'hôte : {err}"),
                        ),
                    }
                }
                self.verified.store(true, Ordering::SeqCst);
                true
            }
            _ => {
                self.rejected.store(true, Ordering::SeqCst);
                false
            }
        }
    }
}

fn set_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

pub struct ClientHandler {
    check: Arc<HostCheck>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(self.check.verify(key).await)
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        for line in banner.lines().filter(|l| !l.trim().is_empty()) {
            self.check.ui.log("response", line.to_string());
        }
        Ok(())
    }
}

// -------------------------------------------------------------- connexion
/// Connexion authentifiée (un transport, N canaux).
pub struct Conn {
    pub handle: Handle<ClientHandler>,
    pub generation: u64,
    /// SFTP de navigation : None = pas encore essayé ; Some(None) = indisponible.
    browse: AsyncMutex<Option<Option<Arc<Sftp>>>>,
}

impl Conn {
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    pub async fn open_channel(&self) -> Result<Channel<Msg>> {
        Ok(self.handle.channel_open_session().await?)
    }

    pub async fn open_sftp(&self) -> Result<Sftp> {
        let chan = self.open_channel().await?;
        chan.request_subsystem(true, "sftp").await?;
        Sftp::new(chan).await
    }

    /// Exécute une commande : (code de sortie, stdout, stderr).
    pub async fn exec(&self, command: &str, timeout: Duration) -> Result<(i32, Vec<u8>, Vec<u8>)> {
        let mut chan = self.open_channel().await?;
        chan.exec(true, command).await?;
        let run = async {
            let (mut out, mut err, mut code) = (Vec::new(), Vec::new(), None::<i32>);
            while let Some(msg) = chan.wait().await {
                match msg {
                    ChannelMsg::Data { data } => out.extend_from_slice(&data),
                    ChannelMsg::ExtendedData { data, .. } => err.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
                    ChannelMsg::Failure => {
                        return Err(Error::Remote(
                            "le serveur refuse l'exécution de commandes".into(),
                        ));
                    }
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            Ok((code.unwrap_or(-1), out, err))
        };
        let res = tokio::time::timeout(timeout, run).await;
        let _ = chan.close().await;
        res.map_err(|_| Error::Remote(format!("délai dépassé pour la commande « {command} »")))?
    }
}

enum KeySource {
    File {
        key: Arc<PrivateKey>,
        cert: Option<Certificate>,
    },
}

pub struct Session {
    pub id: u64,
    site: Mutex<Site>,
    /// Secrets saisis pendant la session (jamais écrits sur disque).
    transient: Mutex<HashMap<&'static str, String>>,
    pub ui: Arc<dyn Ui>,
    secrets: Arc<SecretStore>,
    timeout: Duration,
    conn: Mutex<Option<Arc<Conn>>>,
    connect_lock: AsyncMutex<()>,
    closing: AtomicBool,
    /// 0 : inconnu, 1 : commandes acceptées, 2 : refusées.
    shell: Arc<AtomicU8>,
    home: Mutex<String>,
    generation: AtomicU64,
}

impl Session {
    pub fn new(
        site: Site,
        ui: Arc<dyn Ui>,
        secrets: Arc<SecretStore>,
        timeout_secs: u32,
    ) -> Arc<Self> {
        Arc::new(Session {
            id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            site: Mutex::new(site),
            transient: Mutex::new(HashMap::new()),
            ui,
            secrets,
            timeout: Duration::from_secs(timeout_secs.max(3) as u64),
            conn: Mutex::new(None),
            connect_lock: AsyncMutex::new(()),
            closing: AtomicBool::new(false),
            shell: Arc::new(AtomicU8::new(0)),
            home: Mutex::new("/".into()),
            generation: AtomicU64::new(0),
        })
    }

    pub fn log(&self, level: &str, message: impl Into<String>) {
        self.ui.log(level, message.into());
    }

    pub fn site(&self) -> Site {
        self.site.lock().unwrap().clone()
    }

    pub fn label(&self) -> String {
        self.site.lock().unwrap().label()
    }

    /// Change le protocole de transfert (SCP / SFTP) sans se reconnecter.
    pub fn set_protocol(&self, protocol: &str) {
        self.site.lock().unwrap().protocol = protocol.to_string();
    }

    pub fn home(&self) -> String {
        self.home.lock().unwrap().clone()
    }

    pub fn shell_ok(&self) -> Option<bool> {
        match self.shell.load(Ordering::Relaxed) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    pub fn current(&self) -> Option<Arc<Conn>> {
        self.conn.lock().unwrap().clone()
    }

    pub fn is_connected(&self) -> bool {
        self.current().is_some_and(|c| !c.is_closed())
    }

    // ---------------------------------------------------------- connexion
    pub async fn connect(&self) -> Result<()> {
        let _guard = self.connect_lock.lock().await;
        self.closing.store(false, Ordering::SeqCst);
        self.connect_locked().await
    }

    /// Reconnexion transparente (appelée par les transferts et la navigation).
    pub async fn ensure_connected(&self) -> Result<Arc<Conn>> {
        if let Some(c) = self.current().filter(|c| !c.is_closed()) {
            return Ok(c);
        }
        let _guard = self.connect_lock.lock().await;
        if self.closing.load(Ordering::SeqCst) {
            return Err(Error::Connect("Session fermée".into()));
        }
        if let Some(c) = self.current().filter(|c| !c.is_closed()) {
            return Ok(c);
        }
        self.log(
            "status",
            format!("Connexion perdue avec {}, reconnexion…", self.label()),
        );
        self.connect_locked().await?;
        self.current()
            .ok_or_else(|| Error::Connect("Connexion impossible".into()))
    }

    async fn connect_locked(&self) -> Result<()> {
        self.drop_conn().await;
        let site = self.site();
        self.log(
            "status",
            format!("Résolution de {}:{}…", site.host, site.port),
        );
        let tcp = tokio::time::timeout(
            self.timeout,
            tokio::net::TcpStream::connect((site.host.as_str(), site.port)),
        )
        .await
        .map_err(|_| {
            Error::Connect(format!(
                "Impossible de joindre {}:{} : délai dépassé",
                site.host, site.port
            ))
        })?
        .map_err(|e| {
            Error::Connect(format!(
                "Impossible de joindre {}:{} : {e}",
                site.host, site.port
            ))
        })?;
        let _ = tcp.set_nodelay(true);
        let peer = tcp
            .peer_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_default();
        self.log("status", format!("Connecté à {peer}, négociation SSH…"));

        let preferred = russh::Preferred {
            compression: if site.compression {
                Cow::Borrowed(&[
                    russh::compression::ZLIB_LEGACY,
                    russh::compression::ZLIB,
                    russh::compression::NONE,
                ])
            } else {
                Cow::Borrowed(&[russh::compression::NONE])
            },
            ..Default::default()
        };
        let config = client::Config {
            window_size: WINDOW_SIZE,
            maximum_packet_size: MAX_PACKET,
            channel_buffer_size: 256,
            keepalive_interval: (site.keepalive > 0)
                .then(|| Duration::from_secs(site.keepalive as u64)),
            keepalive_max: 4,
            nodelay: true,
            preferred,
            ..Default::default()
        };
        let check = Arc::new(HostCheck {
            ui: self.ui.clone(),
            host: site.host.clone(),
            port: site.port,
            prompting: AtomicBool::new(false),
            verified: AtomicBool::new(false),
            rejected: AtomicBool::new(false),
        });
        let handler = ClientHandler {
            check: check.clone(),
        };
        let fut = client::connect_stream(Arc::new(config), tcp, handler);
        tokio::pin!(fut);
        // Le délai ne s'applique pas pendant que l'utilisateur examine la clé d'hôte.
        let mut handle = loop {
            tokio::select! {
                res = &mut fut => break res,
                _ = tokio::time::sleep(self.timeout) => {
                    if check.prompting.load(Ordering::SeqCst) || check.verified.load(Ordering::SeqCst) {
                        continue;
                    }
                    return Err(Error::Connect("Échec de la négociation SSH : délai dépassé".into()));
                }
            }
        }
        .map_err(|e| {
            if check.rejected.load(Ordering::SeqCst) {
                Error::Auth("Clé d'hôte refusée par l'utilisateur".into())
            } else {
                Error::Connect(format!("Échec de la négociation SSH : {e}"))
            }
        })?;
        self.log("response", "Négociation SSH terminée (chiffrement établi)");

        if let Err(err) = self.authenticate(&mut handle, &site).await {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "", "")
                .await;
            return Err(err);
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let conn = Arc::new(Conn {
            handle,
            generation,
            browse: AsyncMutex::new(None),
        });
        *self.conn.lock().unwrap() = Some(conn.clone());
        let home = self.fs_home(&conn).await;
        *self.home.lock().unwrap() = home.clone();
        self.log(
            "status",
            format!("Authentifié. Répertoire personnel : {home}"),
        );
        let (shell, ui, c) = (self.shell.clone(), self.ui.clone(), conn.clone());
        tokio::spawn(async move { probe_shell(c, shell, ui).await });
        Ok(())
    }

    async fn drop_conn(&self) {
        let old = self.conn.lock().unwrap().take();
        if let Some(c) = old {
            if let Some(Some(s)) = c.browse.lock().await.take() {
                s.close();
            }
            let _ = c
                .handle
                .disconnect(russh::Disconnect::ByApplication, "", "")
                .await;
        }
    }

    pub async fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _guard = self.connect_lock.lock().await;
        self.drop_conn().await;
        self.log("status", format!("Déconnecté de {}", self.label()));
    }

    /// SFTP partagé pour la navigation (None si le serveur ne le propose pas).
    pub async fn browse_sftp(&self) -> Result<(Arc<Conn>, Option<Arc<Sftp>>)> {
        let conn = self.ensure_connected().await?;
        let sftp = self.browse_sftp_on(&conn).await;
        Ok((conn, sftp))
    }

    pub(crate) async fn browse_sftp_on(&self, conn: &Arc<Conn>) -> Option<Arc<Sftp>> {
        let mut slot = conn.browse.lock().await;
        if let Some(Some(s)) = slot.as_ref() {
            if !s.is_closed() {
                return Some(s.clone());
            }
            *slot = None;
        }
        if slot.is_none() {
            match conn.open_sftp().await {
                Ok(s) => {
                    if conn.generation == 1 {
                        self.log("status", "Sous-système SFTP disponible pour la navigation");
                    }
                    *slot = Some(Some(Arc::new(s)));
                }
                Err(e) => {
                    self.log(
                        "status",
                        format!("SFTP indisponible ({e}), navigation via commandes shell"),
                    );
                    *slot = Some(None);
                }
            }
        }
        slot.as_ref().and_then(|s| s.clone())
    }

    /// Oublie le SFTP de navigation (après une erreur de canal).
    pub async fn reset_browse(&self, conn: &Conn) {
        let mut slot = conn.browse.lock().await;
        if matches!(slot.as_ref(), Some(Some(_))) {
            *slot = None;
        }
    }

    // ------------------------------------------------------ authentification
    fn secret(&self, kind: &'static str) -> String {
        if let Some(v) = self
            .transient
            .lock()
            .unwrap()
            .get(kind)
            .filter(|v| !v.is_empty())
        {
            return v.clone();
        }
        let id = self.site.lock().unwrap().id.clone();
        self.secrets.get(&id, kind)
    }

    fn remember(&self, kind: &'static str, value: &str, persist: bool) {
        self.transient
            .lock()
            .unwrap()
            .insert(kind, value.to_string());
        if persist {
            let id = self.site.lock().unwrap().id.clone();
            if !self.secrets.set(&id, kind, value) {
                self.log("error", "Trousseau système indisponible : secret conservé pour cette session uniquement");
            }
        }
    }

    fn forget(&self, kind: &'static str) {
        self.transient.lock().unwrap().remove(kind);
    }

    fn otp_key(site: &Site) -> String {
        format!("{}@{}:{}", site.user, site.host, site.port)
    }

    async fn ask_secret(&self, title: &str, label: String) -> Result<(String, bool)> {
        match self
            .ui
            .ask(PromptRequest::Secret {
                title: title.into(),
                label,
            })
            .await
        {
            PromptReply::Secret { value, remember } => Ok((value, remember)),
            _ => Err(Error::Cancelled(format!(
                "Saisie annulée : {}",
                title.to_lowercase()
            ))),
        }
    }

    async fn load_file_key(&self, site: &Site) -> Result<Option<KeySource>> {
        if site.key_file.is_empty() {
            return Ok(None);
        }
        let path = expand_tilde(&site.key_file);
        if !path.exists() {
            return Err(Error::Auth(format!(
                "Clé privée introuvable : {}",
                path.display()
            )));
        }
        let mut passphrase = self.secret(SECRET_PASSPHRASE);
        let mut remember = false;
        for attempt in 0..4 {
            let pass = (!passphrase.is_empty()).then_some(passphrase.as_str());
            match russh::keys::load_secret_key(&path, pass) {
                Ok(key) => {
                    if attempt > 0 {
                        self.remember(SECRET_PASSPHRASE, &passphrase, remember);
                    }
                    let mut cert_path = path.clone().into_os_string();
                    cert_path.push("-cert.pub");
                    let cert_path = PathBuf::from(cert_path);
                    let cert = if cert_path.exists() {
                        match russh::keys::load_openssh_certificate(&cert_path) {
                            Ok(c) => {
                                self.log(
                                    "status",
                                    format!("Certificat chargé : {}", cert_path.display()),
                                );
                                Some(c)
                            }
                            Err(e) => {
                                self.log("error", format!("Certificat ignoré : {e}"));
                                None
                            }
                        }
                    } else {
                        None
                    };
                    return Ok(Some(KeySource::File {
                        key: Arc::new(key),
                        cert,
                    }));
                }
                Err(err) => {
                    let msg = err.to_string().to_lowercase();
                    let encrypted = matches!(err, russh::keys::Error::KeyIsEncrypted)
                        || [
                            "encrypt",
                            "decrypt",
                            "crypto",
                            "passphrase",
                            "password",
                            "cipher",
                            "padding",
                            "mac",
                        ]
                        .iter()
                        .any(|w| msg.contains(w));
                    if !encrypted {
                        return Err(Error::Auth(format!(
                            "Clé privée illisible ({}) : {err}",
                            path.display()
                        )));
                    }
                }
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let label = if attempt > 0 || !passphrase.is_empty() {
                "Phrase de passe incorrecte, réessayez :"
            } else {
                "Phrase de passe :"
            };
            let (p, r) = self
                .ask_secret("Clé privée protégée", format!("Clé {name}\n{label}"))
                .await?;
            passphrase = p;
            remember = r;
        }
        Err(Error::Auth("Phrase de passe de la clé incorrecte".into()))
    }

    async fn rsa_hash(&self, handle: &Handle<ClientHandler>, key: &PrivateKey) -> Option<HashAlg> {
        if !key.algorithm().is_rsa() {
            return None;
        }
        match handle.best_supported_rsa_hash().await {
            Ok(Some(h)) => h,
            _ => Some(HashAlg::Sha256),
        }
    }

    /// Essaie la clé du fichier puis les clés de l'agent. Retourne Some(résultat) si
    /// une clé a été acceptée (totalement ou partiellement).
    async fn try_keys(
        &self,
        handle: &mut Handle<ClientHandler>,
        user: &str,
        site: &Site,
    ) -> Result<Option<russh::client::AuthResult>> {
        use russh::client::AuthResult;
        let accepted = |r: &AuthResult| match r {
            AuthResult::Success => true,
            AuthResult::Failure {
                partial_success, ..
            } => *partial_success,
        };
        let mut any = false;
        if let Some(KeySource::File { key, cert }) = self.load_file_key(site).await? {
            any = true;
            let desc = format!(
                "{} {}",
                key.algorithm().as_str(),
                fingerprint(key.public_key())
            );
            if let Some(cert) = cert {
                self.log("command", format!("Essai du certificat {desc}"));
                let r = handle
                    .authenticate_openssh_cert(user, key.clone(), cert)
                    .await?;
                if accepted(&r) {
                    return Ok(Some(r));
                }
                self.log("response", format!("Certificat refusé : {desc}"));
            }
            self.log("command", format!("Essai de la clé {desc}"));
            let hash = self.rsa_hash(handle, &key).await;
            let r = handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, hash))
                .await?;
            if accepted(&r) {
                return Ok(Some(r));
            }
            self.log("response", format!("Clé refusée : {desc}"));
        }
        if site.use_agent {
            match AgentClient::connect_env().await {
                Ok(mut agent) => match agent.request_identities().await {
                    Ok(ids) => {
                        if !ids.is_empty() {
                            self.log(
                                "status",
                                format!("Agent SSH : {} clé(s) disponible(s)", ids.len()),
                            );
                            any = true;
                        }
                        for id in ids {
                            let pk = id.public_key().into_owned();
                            let desc =
                                format!("{} {} (agent)", pk.algorithm().as_str(), fingerprint(&pk));
                            self.log("command", format!("Essai de la clé {desc}"));
                            let res = match &id {
                                AgentIdentity::PublicKey { key, .. } => {
                                    let hash = if key.algorithm().is_rsa() {
                                        handle
                                            .best_supported_rsa_hash()
                                            .await
                                            .ok()
                                            .flatten()
                                            .unwrap_or(Some(HashAlg::Sha256))
                                    } else {
                                        None
                                    };
                                    handle
                                        .authenticate_publickey_with(
                                            user,
                                            key.clone(),
                                            hash,
                                            &mut agent,
                                        )
                                        .await
                                }
                                AgentIdentity::Certificate { certificate, .. } => {
                                    handle
                                        .authenticate_certificate_with(
                                            user,
                                            certificate.clone(),
                                            None,
                                            &mut agent,
                                        )
                                        .await
                                }
                            };
                            match res {
                                Ok(r) if accepted(&r) => return Ok(Some(r)),
                                Ok(_) => self.log("response", format!("Clé refusée : {desc}")),
                                Err(e) => {
                                    self.log("error", format!("Erreur avec la clé {desc} : {e}"))
                                }
                            }
                        }
                    }
                    Err(e) => self.log("trace", format!("Agent SSH indisponible : {e}")),
                },
                Err(e) => self.log("trace", format!("Agent SSH indisponible : {e}")),
            }
        }
        if !any {
            self.log("error", "Aucune clé SSH disponible (fichier ou agent)");
        }
        Ok(None)
    }

    async fn authenticate(&self, handle: &mut Handle<ClientHandler>, site: &Site) -> Result<()> {
        use russh::client::AuthResult;
        let user = if site.user.is_empty() {
            std::env::var("USER").unwrap_or_default()
        } else {
            site.user.clone()
        };
        let all = || {
            MethodSet::from(
                &[
                    MethodKind::PublicKey,
                    MethodKind::KeyboardInteractive,
                    MethodKind::Password,
                ][..],
            )
        };
        let mut allowed = match handle.authenticate_none(&user).await? {
            AuthResult::Success => {
                self.log("status", "Authentifié sans mot de passe (méthode none)");
                return Ok(());
            }
            AuthResult::Failure {
                remaining_methods, ..
            } if !remaining_methods.is_empty() => remaining_methods,
            AuthResult::Failure { .. } => all(),
        };
        let (mut tried_pk, mut tried_password, mut kbd_attempts, mut step) = (false, 0, 0, 1);
        loop {
            if handle.is_closed() {
                return Err(Error::Auth(
                    "Le serveur a fermé la connexion pendant l'authentification".into(),
                ));
            }
            self.log(
                "response",
                format!(
                    "Méthodes acceptées par le serveur : {}",
                    method_names(&allowed).join(", ")
                ),
            );
            let has = |m: MethodKind| allowed.contains(&m);

            if has(MethodKind::PublicKey) && !tried_pk && auth_uses_key(&site.auth) {
                tried_pk = true;
                match self.try_keys(handle, &user, site).await? {
                    Some(AuthResult::Success) => {
                        self.log("status", format!("Étape {step} réussie : clé acceptée"));
                        return Ok(());
                    }
                    Some(AuthResult::Failure {
                        remaining_methods, ..
                    }) => {
                        self.log(
                            "status",
                            format!("Étape {step} réussie : clé acceptée (authentification partielle), second facteur requis"),
                        );
                        step += 1;
                        allowed = remaining_methods;
                    }
                    None => {}
                }
                continue;
            }

            if has(MethodKind::KeyboardInteractive) && kbd_attempts < 3 {
                kbd_attempts += 1;
                let mut asked = false;
                let mut resp = handle
                    .authenticate_keyboard_interactive_start(&user, None)
                    .await?;
                loop {
                    match resp {
                        KeyboardInteractiveAuthResponse::Success => {
                            self.log(
                                "status",
                                format!("Étape {step} réussie : keyboard-interactive"),
                            );
                            return Ok(());
                        }
                        KeyboardInteractiveAuthResponse::Failure {
                            remaining_methods,
                            partial_success,
                        } => {
                            if partial_success {
                                self.log(
                                    "status",
                                    format!("Étape {step} réussie : keyboard-interactive"),
                                );
                                step += 1;
                            } else {
                                self.log(
                                    "error",
                                    "Échec keyboard-interactive : code ou réponse refusé",
                                );
                                self.forget(SECRET_PASSWORD);
                                if handle.is_closed() {
                                    return Err(Error::Auth(
                                        "Le serveur a fermé la connexion après l'échec du code (trop de tentatives ?)".into(),
                                    ));
                                }
                            }
                            if !remaining_methods.is_empty() {
                                allowed = remaining_methods;
                            }
                            break;
                        }
                        KeyboardInteractiveAuthResponse::InfoRequest {
                            name,
                            instructions,
                            prompts,
                        } => {
                            let answers = self
                                .kbd_answers(site, &name, &instructions, prompts, &mut asked)
                                .await?;
                            resp = handle
                                .authenticate_keyboard_interactive_respond(answers)
                                .await?;
                        }
                    }
                }
                continue;
            }

            if has(MethodKind::Password)
                && tried_password < 3
                && (auth_uses_password(&site.auth) || kbd_attempts == 0)
            {
                tried_password += 1;
                let mut password = if tried_password == 1 {
                    self.secret(SECRET_PASSWORD)
                } else {
                    String::new()
                };
                let mut remember = false;
                if password.is_empty() {
                    let (p, r) = self
                        .ask_secret(
                            "Mot de passe",
                            format!("Mot de passe pour {user}@{} :", site.host),
                        )
                        .await?;
                    password = p;
                    remember = r;
                }
                match handle.authenticate_password(&user, &password).await? {
                    AuthResult::Success => {
                        self.remember(SECRET_PASSWORD, &password, remember);
                        self.log("status", format!("Étape {step} réussie : mot de passe"));
                        return Ok(());
                    }
                    AuthResult::Failure {
                        remaining_methods,
                        partial_success: true,
                    } => {
                        self.remember(SECRET_PASSWORD, &password, remember);
                        self.log("status", format!("Étape {step} réussie : mot de passe"));
                        step += 1;
                        allowed = remaining_methods;
                    }
                    AuthResult::Failure { .. } => {
                        self.log("error", "Mot de passe refusé");
                        self.forget(SECRET_PASSWORD);
                    }
                }
                continue;
            }

            return Err(Error::Auth(format!(
                "Authentification impossible : méthodes restantes [{}] non satisfaites avec le type d'authentification choisi",
                method_names(&allowed).join(", ")
            )));
        }
    }

    async fn kbd_answers(
        &self,
        site: &Site,
        title: &str,
        instructions: &str,
        prompts: Vec<russh::client::Prompt>,
        asked: &mut bool,
    ) -> Result<Vec<String>> {
        if prompts.is_empty() {
            return Ok(Vec::new());
        }
        let mut answers = vec![String::new(); prompts.len()];
        let mut missing: Vec<(usize, PromptField)> = Vec::new();
        for (i, p) in prompts.iter().enumerate() {
            let kind = classify_prompt(&p.prompt);
            self.log("response", format!("Serveur demande : {}", p.prompt.trim()));
            if kind == "otp" && site.otp_mode == OTP_SECRET && auth_uses_otp(&site.auth) {
                let secret = self.secret(SECRET_OTP);
                if !secret.is_empty() {
                    let ui = self.ui.clone();
                    match totp::next_code(&Self::otp_key(site), &secret, |m| ui.log("status", m))
                        .await
                    {
                        Ok(code) => {
                            answers[i] = code;
                            self.log(
                                "status",
                                "Code TOTP généré automatiquement depuis le secret enregistré",
                            );
                            continue;
                        }
                        Err(e) => self.log("error", format!("Secret TOTP invalide : {e}")),
                    }
                }
            }
            if kind == "password" {
                let password = self.secret(SECRET_PASSWORD);
                if !password.is_empty() && !*asked {
                    answers[i] = password;
                    continue;
                }
            }
            missing.push((
                i,
                PromptField {
                    text: p.prompt.clone(),
                    echo: p.echo,
                    kind: kind.into(),
                },
            ));
        }
        if !missing.is_empty() {
            *asked = true;
            let reply = self
                .ui
                .ask(PromptRequest::Interactive {
                    site_label: site.label(),
                    title: title.to_string(),
                    instructions: instructions.to_string(),
                    fields: missing.iter().map(|(_, f)| f.clone()).collect(),
                })
                .await;
            let PromptReply::Interactive { answers: given } = reply else {
                return Err(Error::Cancelled(
                    "Authentification annulée par l'utilisateur".into(),
                ));
            };
            for ((i, field), value) in missing.iter().zip(given) {
                if field.kind == "otp" {
                    totp::mark_used(&Self::otp_key(site), &value);
                }
                if field.kind == "password" {
                    self.transient
                        .lock()
                        .unwrap()
                        .insert(SECRET_PASSWORD, value.clone());
                }
                answers[*i] = value;
            }
        }
        Ok(answers)
    }

    // ------------------------------------------------------------- utilitaires
    /// Canal pour un transfert (reconnexion transparente si nécessaire).
    pub async fn open_channel(&self) -> Result<Channel<Msg>> {
        let conn = self.ensure_connected().await?;
        conn.open_channel().await
    }

    pub async fn exec(&self, command: &str, timeout: Duration) -> Result<(i32, Vec<u8>, Vec<u8>)> {
        let conn = self.ensure_connected().await?;
        conn.exec(command, timeout).await
    }
}

/// Vérifie que le serveur accepte les commandes (nécessaire au protocole SCP).
async fn probe_shell(conn: Arc<Conn>, shell: Arc<AtomicU8>, ui: Arc<dyn Ui>) {
    let ok = matches!(conn.exec("echo RUSTSCP_OK", Duration::from_secs(8)).await,
                      Ok((_, out, _)) if String::from_utf8_lossy(&out).contains("RUSTSCP_OK"));
    if !ok && conn.is_closed() {
        return; // connexion fermée entre-temps : rien à conclure
    }
    shell.store(if ok { 1 } else { 2 }, Ordering::Relaxed);
    if !ok {
        ui.log(
            "status",
            "Le serveur n'accepte pas l'exécution de commandes (SFTP uniquement)".into(),
        );
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir().unwrap_or_default().join(rest)
    } else if path == "~" {
        dirs::home_dir().unwrap_or_default()
    } else {
        PathBuf::from(path)
    }
}

/// Protège un argument pour le shell distant (équivalent de `shlex.quote`).
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify() {
        assert_eq!(classify_prompt("Verification code: "), "otp");
        assert_eq!(classify_prompt("Code de vérification : "), "otp");
        assert_eq!(classify_prompt("One-time password (OATH) for `x':"), "otp");
        assert_eq!(classify_prompt("Password: "), "password");
        assert_eq!(classify_prompt("Mot de passe : "), "password");
        assert_eq!(classify_prompt("Barcode reader?"), "other");
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/tmp/a.txt"), "/tmp/a.txt");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
        assert_eq!(shell_quote(""), "''");
    }
}
