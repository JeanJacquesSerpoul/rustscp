//! FTP et FTPS (TLS explicite, AUTH TLS) via `suppaftp`.
//!
//! Une connexion de contrôle sert à la navigation ; chaque transfert parallèle utilise
//! sa propre connexion (FTP ne multiplexe pas), ouverte avec les identifiants validés
//! à la connexion et conservée ensuite pour les transferts suivants.

use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rustscp_shared::util::{S_IFDIR, S_IFLNK, S_IFREG, rbasename, rjoin, rparent};
use rustscp_shared::{Entry, PROTO_FTPS, PromptReply, PromptRequest, SECRET_PASSWORD, Site};
use rustscp_shared::{tr, trf};
use suppaftp::list::{File as FtpFile, ListParser, PosixPexQuery};
use suppaftp::tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream};
use suppaftp::types::FileType;
use suppaftp::{FtpError, Mode, Status};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};
use tokio_rustls::rustls;

use crate::config::config_dir;
use crate::error::{Error, Result};
use crate::ssh::Session;

type Stream = AsyncRustlsFtpStream;

/// Connexions de transfert gardées ouvertes au repos.
const IDLE_MAX: usize = 8;
const CHUNK: usize = 256 * 1024;

pub struct Ftp {
    secure: bool,
    host: String,
    port: u16,
    browse: AsyncMutex<Option<Stream>>,
    idle: Mutex<Vec<Stream>>,
    /// Identifiants validés (pour ouvrir les connexions de transfert sans redemander).
    login: Mutex<Option<(String, String)>>,
    /// Empreinte du certificat accepté pendant cette session.
    accepted_cert: Mutex<Option<String>>,
    mlsd: AtomicBool,
    mfmt: AtomicBool,
}

fn ftp_err(e: FtpError) -> Error {
    match e {
        FtpError::UnexpectedResponse(r) => {
            let msg = String::from_utf8_lossy(&r.body).trim().to_string();
            Error::Remote(if msg.is_empty() {
                format!("FTP {}", r.status.code())
            } else {
                msg
            })
        }
        FtpError::ConnectionError(e) => Error::Connect(format!("FTP : {e}")),
        other => Error::Remote(format!("FTP : {other}")),
    }
}

/// Connexion de contrôle perdue (coupure réseau, ou 421 après inactivité).
fn is_lost(e: &FtpError) -> bool {
    match e {
        FtpError::ConnectionError(_) => true,
        FtpError::UnexpectedResponse(r) => r.status == Status::NotAvailable,
        _ => false,
    }
}

fn is_missing(e: &FtpError) -> bool {
    matches!(e, FtpError::UnexpectedResponse(r) if r.status == Status::FileUnavailable)
}

fn secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn to_entry(f: &FtpFile) -> Entry {
    let mut perm = 0;
    for (shift, who) in [
        (6, PosixPexQuery::Owner),
        (3, PosixPexQuery::Group),
        (0, PosixPexQuery::Others),
    ] {
        let bits = (f.can_read(who) as u32) << 2
            | (f.can_write(who) as u32) << 1
            | f.can_execute(who) as u32;
        perm |= bits << shift;
    }
    let kind = if f.is_symlink() {
        S_IFLNK
    } else if f.is_directory() {
        S_IFDIR
    } else {
        S_IFREG
    };
    Entry {
        name: f.name().to_string(),
        size: if f.is_directory() { 0 } else { f.size() as u64 },
        mtime: secs(f.modified()),
        mode: kind | perm,
        is_dir: f.is_directory(),
        is_link: f.is_symlink(),
        owner: f.uid().map(|u| u.to_string()).unwrap_or_default(),
        group: f.gid().map(|g| g.to_string()).unwrap_or_default(),
    }
}

fn parse_line(line: &str, mlsd: bool) -> Option<FtpFile> {
    if mlsd {
        return ListParser::parse_mlsd(line).ok();
    }
    ListParser::parse_posix(line)
        .or_else(|_| ListParser::parse_dos(line))
        .ok()
}

// ------------------------------------------------------------ certificats
fn cert_fingerprint(der: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn known_certs_path() -> std::path::PathBuf {
    config_dir().join("known_certs")
}

fn host_key(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

fn saved_cert(host: &str, port: u16) -> Option<String> {
    let key = host_key(host, port);
    std::fs::read_to_string(known_certs_path())
        .ok()?
        .lines()
        .filter_map(|l| l.split_once(' '))
        .find(|(h, _)| *h == key)
        .map(|(_, fp)| fp.trim().to_string())
}

fn save_cert(host: &str, port: u16, fp: &str) -> std::io::Result<()> {
    let key = host_key(host, port);
    let mut lines: Vec<String> = std::fs::read_to_string(known_certs_path())
        .unwrap_or_default()
        .lines()
        .filter(|l| l.split_once(' ').is_none_or(|(h, _)| h != key))
        .map(str::to_string)
        .collect();
    lines.push(format!("{key} {fp}"));
    let mut f = crate::platform::private_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(known_certs_path())?;
    f.write_all((lines.join("\n") + "\n").as_bytes())
}

/// Vérifie le certificat avec le magasin du système ; à défaut, accepte l'empreinte
/// approuvée par l'utilisateur, et mémorise sinon l'empreinte refusée pour la lui présenter.
#[derive(Debug)]
struct CertCheck {
    system: rustls_platform_verifier::Verifier,
    trusted: Option<String>,
    rejected: Mutex<Option<(String, String)>>,
}

impl rustls::client::danger::ServerCertVerifier for CertCheck {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fp = cert_fingerprint(end_entity.as_ref());
        if self.trusted.as_deref() == Some(fp.as_str()) {
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }
        self.system
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .inspect_err(|e| *self.rejected.lock().unwrap() = Some((fp, e.to_string())))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.system.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.system.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.system.supported_verify_schemes()
    }
}

impl Ftp {
    pub fn new(site: &Site) -> Self {
        Ftp {
            secure: site.protocol == PROTO_FTPS,
            host: site.host.clone(),
            port: site.port,
            browse: AsyncMutex::new(None),
            idle: Mutex::new(Vec::new()),
            login: Mutex::new(None),
            accepted_cert: Mutex::new(None),
            mlsd: AtomicBool::new(false),
            mfmt: AtomicBool::new(false),
        }
    }

    pub fn is_connected(&self) -> bool {
        match self.browse.try_lock() {
            Ok(g) => g.is_some(),
            // occupée : une commande est en cours
            Err(_) => true,
        }
    }

    pub async fn close(&self) {
        if let Some(mut c) = self.browse.lock().await.take() {
            let _ = c.quit().await;
        }
        let idle: Vec<Stream> = std::mem::take(&mut *self.idle.lock().unwrap());
        for mut c in idle {
            let _ = c.quit().await;
        }
    }

    /// Ouvre la connexion de navigation (avec saisie éventuelle du mot de passe) ;
    /// retourne le répertoire courant après connexion.
    pub async fn connect(&self, s: &Session) -> Result<String> {
        self.close().await;
        *self.login.lock().unwrap() = None;
        let mut c = self.open(s, true).await?;
        let home = c.pwd().await.unwrap_or_else(|_| "/".into());
        *self.browse.lock().await = Some(c);
        Ok(if home.is_empty() { "/".into() } else { home })
    }

    fn tls_connector(&self, check: Arc<CertCheck>) -> AsyncRustlsConnector {
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(check)
            .with_no_client_auth();
        tokio_rustls::TlsConnector::from(Arc::new(config)).into()
    }

    async fn open(&self, s: &Session, interactive: bool) -> Result<Stream> {
        let site = s.site();
        let timeout = s.timeout();
        loop {
            if interactive {
                s.log(
                    "status",
                    trf!(
                        "Connexion à {}:{}…",
                        "Connecting to {}:{}…",
                        self.host,
                        self.port
                    ),
                );
            }
            let tcp = tokio::time::timeout(
                timeout,
                tokio::net::TcpStream::connect((self.host.as_str(), self.port)),
            )
            .await
            .map_err(|_| {
                Error::Connect(trf!(
                    "Impossible de joindre {}:{} : délai dépassé",
                    "Unable to reach {}:{}: timed out",
                    self.host,
                    self.port
                ))
            })?
            .map_err(|e| {
                Error::Connect(trf!(
                    "Impossible de joindre {}:{} : {e}",
                    "Unable to reach {}:{}: {e}",
                    self.host,
                    self.port
                ))
            })?;
            let ipv6 = tcp.peer_addr().map(|a| a.is_ipv6()).unwrap_or(false);
            let mut c = tokio::time::timeout(timeout, Stream::connect_with_stream(tcp))
                .await
                .map_err(|_| {
                    Error::Connect(
                        tr!(
                            "Le serveur FTP ne répond pas (délai dépassé)",
                            "The FTP server does not answer (timed out)"
                        )
                        .into(),
                    )
                })?
                .map_err(ftp_err)?;
            if interactive && let Some(w) = c.get_welcome_msg() {
                s.log("response", w.trim().to_string());
            }
            if self.secure {
                let saved = saved_cert(&self.host, self.port);
                let trusted = self.accepted_cert.lock().unwrap().clone().or(saved.clone());
                let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
                let check = Arc::new(CertCheck {
                    system: rustls_platform_verifier::Verifier::new(provider)
                        .map_err(|e| Error::Connect(format!("TLS : {e}")))?,
                    trusted,
                    rejected: Mutex::new(None),
                });
                match c
                    .into_secure(self.tls_connector(check.clone()), &self.host)
                    .await
                {
                    Ok(secured) => {
                        c = secured;
                        if interactive {
                            s.log(
                                "status",
                                tr!(
                                    "Connexion sécurisée par TLS (FTPS explicite)",
                                    "Connection secured with TLS (explicit FTPS)"
                                ),
                            );
                        }
                    }
                    Err(e) => {
                        let rejected = check.rejected.lock().unwrap().take();
                        match rejected {
                            Some((fp, why)) if interactive => {
                                s.log(
                                    "error",
                                    trf!(
                                        "Certificat TLS non reconnu : {why}",
                                        "Untrusted TLS certificate: {why}"
                                    ),
                                );
                                self.ask_cert(s, &fp, saved.as_deref()).await?;
                                continue;
                            }
                            _ => {
                                return Err(Error::Connect(trf!(
                                    "Échec de la négociation TLS : {e}",
                                    "TLS negotiation failed: {e}"
                                )));
                            }
                        }
                    }
                }
            }
            self.authenticate(s, &mut c, &site, interactive).await?;
            c.set_passive_nat_workaround(true);
            if ipv6 {
                c.set_mode(Mode::ExtendedPassive);
            }
            c.transfer_type(FileType::Binary).await.map_err(ftp_err)?;
            if interactive {
                self.probe_features(s, &mut c).await;
            }
            return Ok(c);
        }
    }

    async fn ask_cert(&self, s: &Session, fp: &str, saved: Option<&str>) -> Result<()> {
        let reply =
            s.ui.ask(PromptRequest::HostKey {
                host: host_key(&self.host, self.port),
                key_type: tr!("Certificat TLS (FTPS)", "TLS certificate (FTPS)").into(),
                fingerprint: format!("SHA256 {fp}"),
                status: if saved.is_some() {
                    "changed"
                } else {
                    "unknown"
                }
                .into(),
                old_fingerprint: saved.map(|o| format!("SHA256 {o}")).unwrap_or_default(),
            })
            .await;
        match reply {
            PromptReply::HostKey { choice } if choice == "once" || choice == "save" => {
                *self.accepted_cert.lock().unwrap() = Some(fp.to_string());
                if choice == "save"
                    && let Err(e) = save_cert(&self.host, self.port, fp)
                {
                    s.log(
                        "error",
                        trf!(
                            "Certificat non enregistré : {e}",
                            "Certificate not saved: {e}"
                        ),
                    );
                }
                Ok(())
            }
            _ => Err(Error::Auth(
                tr!(
                    "Certificat TLS refusé par l'utilisateur",
                    "TLS certificate rejected by the user"
                )
                .into(),
            )),
        }
    }

    async fn authenticate(
        &self,
        s: &Session,
        c: &mut Stream,
        site: &Site,
        interactive: bool,
    ) -> Result<()> {
        let known = self.login.lock().unwrap().clone();
        if let Some((user, pass)) = known {
            return c.login(user, pass).await.map_err(ftp_err);
        }
        if !interactive {
            return Err(Error::Auth(
                tr!(
                    "Session FTP non authentifiée",
                    "FTP session not authenticated"
                )
                .into(),
            ));
        }
        let user = site.user.trim().to_string();
        if user.is_empty() {
            return Err(Error::Auth(
                tr!(
                    "Identifiant manquant : renseignez l'utilisateur (« anonymous » pour un accès anonyme)",
                    "Missing username: enter the user (\"anonymous\" for anonymous access)"
                )
                .into(),
            ));
        }
        let anonymous = matches!(user.to_lowercase().as_str(), "anonymous" | "ftp");
        let mut pass = s.secret(SECRET_PASSWORD);
        let mut asked = None;
        for _ in 0..3 {
            if pass.is_empty() {
                if anonymous {
                    pass = "anonymous@".into();
                } else {
                    let (p, r) = s
                        .ask_secret(
                            tr!("Mot de passe", "Password"),
                            trf!(
                                "Mot de passe pour {user}@{} :",
                                "Password for {user}@{}:",
                                site.host
                            ),
                        )
                        .await?;
                    pass = p;
                    asked = Some(r);
                }
            }
            s.log("command", format!("USER {user}"));
            match c.login(user.as_str(), pass.as_str()).await {
                Ok(()) => {
                    if let Some(remember) = asked {
                        s.remember(SECRET_PASSWORD, &pass, remember);
                    }
                    s.log("status", tr!("Authentifié", "Authenticated"));
                    *self.login.lock().unwrap() = Some((user, pass));
                    return Ok(());
                }
                Err(FtpError::UnexpectedResponse(r)) if r.status == Status::NotLoggedIn => {
                    s.log("error", tr!("Identifiants refusés", "Login rejected"));
                    s.forget(SECRET_PASSWORD);
                    if anonymous {
                        break;
                    }
                    pass.clear();
                }
                Err(e) => return Err(ftp_err(e)),
            }
        }
        Err(Error::Auth(
            tr!(
                "Authentification FTP refusée (identifiant ou mot de passe incorrect)",
                "FTP login rejected (wrong username or password)"
            )
            .into(),
        ))
    }

    async fn probe_features(&self, s: &Session, c: &mut Stream) {
        let Ok(feats) = c.feat().await else {
            return;
        };
        let has = |k: &str| feats.keys().any(|f| f.eq_ignore_ascii_case(k));
        self.mlsd.store(has("MLST"), Ordering::Relaxed);
        self.mfmt.store(has("MFMT"), Ordering::Relaxed);
        if has("UTF8") {
            let _ = c.opts("UTF8", Some("ON")).await;
        }
        let mut list: Vec<&String> = feats.keys().collect();
        list.sort();
        s.log(
            "trace",
            trf!(
                "Fonctions du serveur : {}",
                "Server features: {}",
                list.iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }

    /// Connexion de navigation (rouverte si nécessaire).
    async fn browse(&self, s: &Session) -> Result<MutexGuard<'_, Option<Stream>>> {
        let mut g = self.browse.lock().await;
        if g.is_none() {
            s.log(
                "status",
                trf!(
                    "Connexion perdue avec {}, reconnexion…",
                    "Connection to {} lost, reconnecting…",
                    s.label()
                ),
            );
            *g = Some(self.open(s, true).await?);
        }
        Ok(g)
    }
}

/// Exécute une commande sur la connexion de navigation ; la rejoue une fois après une
/// reconnexion si la connexion de contrôle était perdue.
macro_rules! on_browse {
    ($self:ident, $s:ident, |$c:ident| $body:expr) => {{
        let mut retried = false;
        loop {
            let mut guard = $self.browse($s).await?;
            let $c = guard.as_mut().expect("connexion de navigation");
            match $body.await {
                Err(e) if is_lost(&e) && !retried => {
                    *guard = None;
                    retried = true;
                }
                Err(e) if is_lost(&e) => {
                    *guard = None;
                    break Err(ftp_err(e));
                }
                r => break r.map_err(ftp_err),
            }
        }
    }};
}

impl Ftp {
    pub async fn list(&self, s: &Session, path: &str) -> Result<Vec<Entry>> {
        let mlsd = self.mlsd.load(Ordering::Relaxed);
        let lines = on_browse!(self, s, |c| async {
            if mlsd {
                c.mlsd(Some(path)).await
            } else {
                c.list(Some(path)).await
            }
        })
        .map_err(|e| Error::Remote(format!("{path} : {e}")))?;
        Ok(lines
            .iter()
            .filter_map(|l| parse_line(l, mlsd))
            .filter(|f| f.name() != "." && f.name() != "..")
            .map(|f| to_entry(&f))
            .collect())
    }

    /// None si le chemin n'existe pas.
    pub async fn stat(&self, s: &Session, path: &str) -> Result<Option<Entry>> {
        let name = rbasename(path).to_string();
        if path.trim_end_matches('/').is_empty() {
            return Ok(Some(Entry {
                name: "/".into(),
                mode: S_IFDIR | 0o755,
                is_dir: true,
                ..Default::default()
            }));
        }
        if self.mlsd.load(Ordering::Relaxed) {
            return match on_browse!(self, s, |c| c.mlst(Some(path))) {
                Ok(line) => Ok(ListParser::parse_mlst(line.trim()).ok().map(|f| {
                    let mut e = to_entry(&f);
                    e.name = name;
                    e
                })),
                Err(_) => Ok(None),
            };
        }
        // SIZE + MDTM pour un fichier, CWD pour un dossier
        let size = on_browse!(self, s, |c| async {
            match c.size(path).await {
                Ok(n) => Ok(Some(n)),
                Err(e) if is_missing(&e) => Ok(None),
                Err(e) => Err(e),
            }
        });
        match size {
            Ok(Some(n)) => {
                let mtime = on_browse!(self, s, |c| c.mdtm(path))
                    .map(|t| t.and_utc().timestamp())
                    .unwrap_or(0);
                Ok(Some(Entry {
                    name,
                    size: n as u64,
                    mtime,
                    mode: S_IFREG | 0o644,
                    ..Default::default()
                }))
            }
            _ => {
                let is_dir = on_browse!(self, s, |c| c.cwd(path)).is_ok();
                Ok(is_dir.then(|| Entry {
                    name,
                    mode: S_IFDIR | 0o755,
                    is_dir: true,
                    ..Default::default()
                }))
            }
        }
    }

    pub async fn realpath(&self, s: &Session, path: &str) -> Result<String> {
        on_browse!(self, s, |c| async {
            c.cwd(path).await?;
            c.pwd().await
        })
    }

    pub async fn mkdir(&self, s: &Session, path: &str, parents: bool) -> Result<()> {
        if !parents {
            return on_browse!(self, s, |c| c.mkdir(path))
                .map_err(|e| Error::Remote(format!("mkdir {path} : {e}")));
        }
        let mut missing = Vec::new();
        let mut cur = path.trim_end_matches('/').to_string();
        while !cur.is_empty() && cur != "/" {
            if on_browse!(self, s, |c| c.cwd(cur.as_str())).is_ok() {
                break;
            }
            missing.push(cur.clone());
            cur = rparent(&cur);
        }
        for d in missing.iter().rev() {
            on_browse!(self, s, |c| c.mkdir(d.as_str()))
                .map_err(|e| Error::Remote(format!("mkdir {d} : {e}")))?;
        }
        Ok(())
    }

    pub async fn rename(&self, s: &Session, src: &str, dst: &str) -> Result<()> {
        on_browse!(self, s, |c| c.rename(src, dst))
            .map_err(|e| Error::Remote(trf!("Renommage impossible : {e}", "Rename failed: {e}")))
    }

    pub async fn chmod(&self, s: &Session, path: &str, mode: u32, recursive: bool) -> Result<()> {
        let mut stack = vec![(path.to_string(), recursive)];
        while let Some((p, descend)) = stack.pop() {
            let cmd = format!("CHMOD {:o} {p}", mode & 0o7777);
            on_browse!(self, s, |c| c.site(cmd.as_str()))
                .map_err(|e| Error::Remote(format!("chmod {p} : {e}")))?;
            if descend {
                for e in self.list(s, &p).await? {
                    if !e.is_link {
                        stack.push((rjoin(&p, &e.name), e.is_dir));
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn remove(&self, s: &Session, path: &str, is_dir: bool) -> Result<()> {
        let res = if !is_dir {
            on_browse!(self, s, |c| c.rm(path))
        } else {
            // parcours en profondeur : les dossiers sont supprimés après leur contenu
            let mut stack = vec![(path.to_string(), false)];
            let mut res = Ok(());
            while let Some((dir, visited)) = stack.pop() {
                if visited {
                    res = on_browse!(self, s, |c| c.rmdir(dir.as_str()));
                } else {
                    stack.push((dir.clone(), true));
                    for e in self.list(s, &dir).await? {
                        let child = rjoin(&dir, &e.name);
                        if e.is_dir && !e.is_link {
                            stack.push((child, false));
                        } else {
                            res = on_browse!(self, s, |c| c.rm(child.as_str()));
                        }
                        if res.is_err() {
                            break;
                        }
                    }
                }
                if res.is_err() {
                    break;
                }
            }
            res
        };
        res.map_err(|e| {
            Error::Remote(trf!(
                "Suppression de {path} impossible : {e}",
                "Unable to delete {path}: {e}"
            ))
        })
    }

    pub async fn touch(&self, s: &Session, path: &str) -> Result<()> {
        on_browse!(self, s, |c| async {
            let stream = c.put_with_stream(path).await?;
            stream.finish().await
        })
    }

    // ------------------------------------------------------------ transferts
    async fn take(&self, s: &Session) -> Result<Stream> {
        let idle = self.idle.lock().unwrap().pop();
        if let Some(mut c) = idle
            && c.noop().await.is_ok()
        {
            return Ok(c);
        }
        self.open(s, false).await
    }

    fn give(&self, c: Stream) {
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < IDLE_MAX {
            idle.push(c);
        }
    }

    /// Télécharge `remote` dans `out` ; retourne (taille, date de modification distante).
    pub async fn download(
        &self,
        s: &Session,
        remote: &str,
        out: &mut File,
        set_size: &(dyn Fn(u64) + Sync),
        progress: &(dyn Fn(u64) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(u64, i64)> {
        let mut c = self.take(s).await?;
        if let Ok(n) = c.size(remote).await {
            set_size(n as u64);
        }
        let mtime = c
            .mdtm(remote)
            .await
            .map(|t| t.and_utc().timestamp())
            .unwrap_or(0);
        let mut stream = c.retr_as_stream(remote).await.map_err(ftp_err)?;
        let mut buf = vec![0u8; CHUNK];
        let mut received = 0u64;
        loop {
            if cancelled() {
                let _ = c.abort(stream).await;
                self.give(c);
                return Err(Error::TransferCancelled);
            }
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            received += n as u64;
            progress(n as u64);
        }
        stream.finish().await.map_err(ftp_err)?;
        self.give(c);
        Ok((received, mtime))
    }

    /// Envoie `input` vers `remote` ; `mtime` : date de modification à reporter (MFMT).
    pub async fn upload(
        &self,
        s: &Session,
        input: &mut File,
        remote: &str,
        mtime: Option<i64>,
        progress: &(dyn Fn(u64) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<()> {
        let mut c = self.take(s).await?;
        let mut stream = c.put_with_stream(remote).await.map_err(ftp_err)?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancelled() {
                let _ = c.abort(stream).await;
                self.give(c);
                return Err(Error::TransferCancelled);
            }
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            stream.write_all(&buf[..n]).await?;
            progress(n as u64);
        }
        stream.finish().await.map_err(ftp_err)?;
        if let Some(t) = mtime.filter(|_| self.mfmt.load(Ordering::Relaxed))
            && let Some(dt) = chrono::DateTime::from_timestamp(t, 0)
        {
            let cmd = format!("MFMT {} {remote}", dt.format("%Y%m%d%H%M%S"));
            let _ = c.custom_command(cmd, &[Status::File]).await;
        }
        self.give(c);
        Ok(())
    }
}
