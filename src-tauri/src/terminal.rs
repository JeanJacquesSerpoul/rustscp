//! Terminal SSH interactif : un canal « shell » avec pseudo-terminal (PTY)
//! ouvert sur la connexion déjà authentifiée de la session.
//!
//! Comme les transferts, le terminal est **multiplexé** : il ne rouvre pas de
//! connexion et ne redemande donc pas de code TOTP.
//!
//! Flux de sortie vers l'interface : trames binaires (premier octet = type)
//! - `0` + octets bruts produits par le serveur ;
//! - `1` + JSON `{code, message, lost}` à la fin de la session.
//!
//! Les sorties sont regroupées (quelques millisecondes) pour limiter le nombre
//! de messages, et l'interface acquitte ce qu'elle a affiché : au-delà de
//! `HIGH_WATER` octets non acquittés, l'envoi est suspendu (un `cat` d'un gros
//! fichier ne sature pas l'affichage). Le canal SSH est lu en continu pour ne
//! jamais bloquer les autres canaux de la connexion (navigation, transferts).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelReadHalf, Pty};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::ssh::{Session, shell_quote};
use rustscp_shared::{tr, trf};

/// Type de terminal annoncé au serveur (xterm.js est compatible xterm).
pub const TERM: &str = "xterm-256color";
/// Taille maximale d'une trame de sortie.
const FRAME_MAX: usize = 128 * 1024;
/// Octets envoyés à l'interface mais pas encore affichés.
const HIGH_WATER: usize = 2 * 1024 * 1024;
/// Au-delà, le canal n'est plus lu (contre-pression SSH classique).
const BUFFER_MAX: usize = 32 * 1024 * 1024;
/// Regroupement des sorties : imperceptible à la frappe, efficace en rafale.
const COALESCE: Duration = Duration::from_millis(3);

pub const FRAME_DATA: u8 = 0;
pub const FRAME_EXIT: u8 = 1;

/// Destination des trames (fenêtre Tauri, passerelle de développement, tests).
pub trait TermSink: Send + Sync + 'static {
    /// `false` si le destinataire a disparu (fenêtre fermée).
    fn send(&self, frame: Vec<u8>) -> bool;
}

enum Cmd {
    Input(Vec<u8>),
    Resize(u32, u32),
    Ack(usize),
    /// Fermeture demandée (motif affiché, sinon « Terminal fermé »).
    Close(Option<String>),
}

struct Handle {
    session: u64,
    /// Propriétaire (étiquette de la fenêtre).
    owner: String,
    tx: mpsc::UnboundedSender<Cmd>,
}

#[derive(Serialize)]
struct Exit {
    code: Option<i32>,
    message: String,
    /// Connexion perdue (et non fin normale du shell).
    lost: bool,
}

/// Terminaux ouverts.
#[derive(Default)]
pub struct Terminals {
    map: Mutex<HashMap<u64, Handle>>,
    next: AtomicU64,
}

impl Terminals {
    pub fn new() -> Arc<Self> {
        Arc::new(Terminals::default())
    }

    /// Ouvre un shell interactif ; `cwd` : dossier de départ.
    pub async fn open(
        self: &Arc<Self>,
        session: Arc<Session>,
        owner: String,
        (cols, rows): (u32, u32),
        cwd: Option<String>,
        sink: Arc<dyn TermSink>,
    ) -> Result<u64> {
        if session.ftp().is_some() {
            return Err(Error::Remote(
                tr!(
                    "Le terminal nécessite une connexion SSH (indisponible en FTP)",
                    "The terminal requires an SSH connection (not available over FTP)"
                )
                .into(),
            ));
        }
        let (cols, rows) = (cols.clamp(2, 1000), rows.clamp(1, 1000));
        let chan = session.open_channel().await?;
        let (read, write) = match start_shell(chan, cols, rows, cwd.as_deref()).await {
            Ok(halves) => halves,
            Err(e) => {
                session.log("error", trf!("Terminal SSH : {e}", "SSH terminal: {e}"));
                return Err(e);
            }
        };
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = mpsc::unbounded_channel();
        self.map.lock().unwrap().insert(
            id,
            Handle {
                session: session.id,
                owner,
                tx,
            },
        );
        session.log(
            "status",
            trf!(
                "Terminal SSH ouvert sur {} ({TERM}, {cols}×{rows})",
                "SSH terminal opened on {} ({TERM}, {cols}×{rows})",
                session.label()
            ),
        );
        let this = self.clone();
        tokio::spawn(async move {
            run(read, write, rx, sink, &session).await;
            this.map.lock().unwrap().remove(&id);
        });
        Ok(id)
    }

    fn send(&self, id: u64, cmd: Cmd) {
        if let Some(h) = self.map.lock().unwrap().get(&id) {
            let _ = h.tx.send(cmd);
        }
    }

    /// Frappe clavier ou collage.
    pub fn write(&self, id: u64, data: Vec<u8>) {
        if !data.is_empty() {
            self.send(id, Cmd::Input(data));
        }
    }

    pub fn resize(&self, id: u64, cols: u32, rows: u32) {
        self.send(id, Cmd::Resize(cols.clamp(2, 1000), rows.clamp(1, 1000)));
    }

    /// L'interface a affiché `bytes` octets.
    pub fn ack(&self, id: u64, bytes: usize) {
        self.send(id, Cmd::Ack(bytes));
    }

    pub fn close(&self, id: u64) {
        self.send(id, Cmd::Close(None));
    }

    fn close_where(&self, keep: impl Fn(&Handle) -> bool, reason: Option<String>) {
        for h in self.map.lock().unwrap().values().filter(|h| !keep(h)) {
            let _ = h.tx.send(Cmd::Close(reason.clone()));
        }
    }

    /// Ferme les terminaux d'une fenêtre.
    pub fn close_owner(&self, owner: &str) {
        self.close_where(|h| h.owner != owner, None);
    }

    /// Ferme les terminaux d'une session (déconnexion, fermeture de l'onglet).
    pub fn close_session(&self, session: u64) {
        let reason = tr!(
            "Session SSH fermée dans la fenêtre principale",
            "SSH session closed in the main window"
        );
        self.close_where(|h| h.session != session, Some(reason.into()));
    }

    /// Nombre de terminaux ouverts (d'une session, d'une fenêtre ou en tout).
    pub fn count(&self, session: Option<u64>, owner: Option<&str>) -> usize {
        self.map
            .lock()
            .unwrap()
            .values()
            .filter(|h| session.is_none_or(|s| h.session == s))
            .filter(|h| owner.is_none_or(|o| h.owner == o))
            .count()
    }
}

type Halves = (ChannelReadHalf, russh::ChannelWriteHalf<Msg>);

/// Demande le PTY puis le shell (ou `cd dossier; exec $SHELL -l`).
async fn start_shell(
    chan: Channel<Msg>,
    cols: u32,
    rows: u32,
    cwd: Option<&str>,
) -> Result<Halves> {
    let (mut read, write) = chan.split();
    // effacement = DEL (ce qu'envoie xterm.js), saisie UTF-8 correcte en mode canonique
    let modes = [
        (Pty::VERASE, 0x7f),
        (Pty::IUTF8, 1),
        (Pty::TTY_OP_ISPEED, 38400),
        (Pty::TTY_OP_OSPEED, 38400),
    ];
    write
        .request_pty(true, TERM, cols, rows, 0, 0, &modes)
        .await?;
    if !reply(&mut read).await? {
        return Err(Error::Remote(
            tr!(
                "le serveur refuse l'allocation d'un pseudo-terminal (PTY)",
                "the server refuses to allocate a pseudo-terminal (PTY)"
            )
            .into(),
        ));
    }
    // variables souvent refusées (AcceptEnv) : sans réponse attendue
    let _ = write.set_env(false, "COLORTERM", "truecolor").await;
    let _ = write.set_env(false, "TERM_PROGRAM", "RustSCP").await;
    // locale (comme « SendEnv LANG » d'OpenSSH) : xterm.js est toujours en UTF-8
    let lang = std::env::var("LANG")
        .ok()
        .filter(|l| l.to_ascii_uppercase().replace('-', "").contains("UTF8"))
        .unwrap_or_else(|| "C.UTF-8".into());
    let _ = write.set_env(false, "LANG", lang).await;
    match cwd.filter(|d| !d.is_empty()) {
        Some(dir) => {
            let cmd = format!("cd {} 2>/dev/null; exec \"$SHELL\" -l", shell_quote(dir));
            write.exec(true, cmd).await?;
        }
        None => write.request_shell(true).await?,
    }
    if !reply(&mut read).await? {
        return Err(Error::Remote(
            tr!(
                "le serveur n'autorise pas l'ouverture d'un shell (accès SFTP uniquement ?)",
                "the server does not allow opening a shell (SFTP-only access?)"
            )
            .into(),
        ));
    }
    Ok((read, write))
}

/// Réponse à une requête de canal (`want_reply`).
async fn reply(read: &mut ChannelReadHalf) -> Result<bool> {
    let wait = async {
        loop {
            match read.wait().await {
                Some(ChannelMsg::Success) => return Ok(true),
                Some(ChannelMsg::Failure) => return Ok(false),
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => {
                    return Err(Error::Remote(
                        tr!("canal fermé par le serveur", "channel closed by the server").into(),
                    ));
                }
                Some(_) => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait)
        .await
        .map_err(|_| {
            Error::Remote(tr!("le serveur ne répond pas", "the server does not respond").into())
        })?
}

fn frame(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(data.len() + 1);
    f.push(tag);
    f.extend_from_slice(data);
    f
}

async fn run(
    mut read: ChannelReadHalf,
    write: russh::ChannelWriteHalf<Msg>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    sink: Arc<dyn TermSink>,
    session: &Session,
) {
    let mut out = BytesMut::new();
    let mut unacked = 0usize;
    let mut deadline: Option<Instant> = None;
    let (mut code, mut signal) = (None::<i32>, None::<String>);
    let mut closed: Option<Option<String>> = None;
    let mut alive = true;
    loop {
        let can_flush = !out.is_empty() && unacked < HIGH_WATER;
        let flush_at = deadline.unwrap_or_else(Instant::now);
        tokio::select! {
            biased;
            cmd = rx.recv() => match cmd {
                Some(Cmd::Input(data)) => {
                    if write.data_bytes(data).await.is_err() {
                        break;
                    }
                }
                Some(Cmd::Resize(c, r)) => {
                    let _ = write.window_change(c, r, 0, 0).await;
                }
                Some(Cmd::Ack(n)) => unacked = unacked.saturating_sub(n),
                Some(Cmd::Close(reason)) => {
                    closed = Some(reason);
                    let _ = write.close().await;
                    break;
                }
                None => {
                    closed = Some(None);
                    let _ = write.close().await;
                    break;
                }
            },
            _ = tokio::time::sleep_until(flush_at), if can_flush => {
                let n = out.len().min(FRAME_MAX);
                let chunk = out.split_to(n);
                unacked += n;
                if !sink.send(frame(FRAME_DATA, &chunk)) {
                    alive = false;
                    let _ = write.close().await;
                    break;
                }
                // reste à envoyer : sans attendre (dans la limite des acquittements)
                deadline = (!out.is_empty()).then(Instant::now);
                continue;
            },
            msg = read.wait(), if out.len() < BUFFER_MAX => match msg {
                Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data),
                Some(ChannelMsg::ExtendedData { data, .. }) => out.extend_from_slice(&data),
                Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status as i32),
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    signal = Some(format!("{signal_name:?}"));
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
        }
        if !out.is_empty() && deadline.is_none() {
            deadline = Some(Instant::now() + COALESCE);
        }
    }
    if !alive {
        return;
    }
    // fin : tout ce qui reste, puis le motif
    while !out.is_empty() {
        let chunk = out.split_to(out.len().min(FRAME_MAX));
        if !sink.send(frame(FRAME_DATA, &chunk)) {
            return;
        }
    }
    let lost = closed.is_none() && code.is_none() && signal.is_none() && !session.is_connected();
    let message = if let Some(reason) = closed {
        reason.unwrap_or_else(|| tr!("Terminal fermé", "Terminal closed").into())
    } else if let Some(sig) = &signal {
        trf!(
            "Processus interrompu (signal {sig})",
            "Process killed (signal {sig})"
        )
    } else if let Some(c) = code {
        trf!("Session terminée (code {c})", "Session ended (code {c})")
    } else if lost {
        tr!("Connexion perdue", "Connection lost").to_string()
    } else {
        tr!("Session terminée", "Session ended").to_string()
    };
    let exit = Exit {
        code,
        message,
        lost,
    };
    let json = serde_json::to_vec(&exit).unwrap_or_default();
    sink.send(frame(FRAME_EXIT, &json));
    session.log(
        "status",
        trf!(
            "Terminal SSH fermé ({})",
            "SSH terminal closed ({})",
            exit.message
        ),
    );
}
