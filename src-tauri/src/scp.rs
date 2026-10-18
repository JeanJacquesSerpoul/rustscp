//! Implémentation native du protocole SCP (source/sink) sur un canal SSH.
//!
//! Chaque transfert ouvre un canal `exec` sur le transport déjà authentifié :
//! aucune ré-authentification (ni code TOTP) n'est nécessaire.

use std::fs::{File, FileTimes};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use bytes::Bytes;
use russh::client::Msg;
use russh::{Channel, ChannelMsg};

use crate::error::{Error, Result};
use crate::ssh::shell_quote;
use rustscp_shared::{tr, trf};

pub const CHUNK: usize = 256 * 1024;

/// Lecture tamponnée sur un canal + capture de stderr et du code de sortie.
struct ChanIo {
    chan: Channel<Msg>,
    buf: Bytes,
    stderr: Vec<u8>,
    exit: Option<u32>,
    eof: bool,
}

impl ChanIo {
    fn new(chan: Channel<Msg>) -> Self {
        ChanIo {
            chan,
            buf: Bytes::new(),
            stderr: Vec::new(),
            exit: None,
            eof: false,
        }
    }

    /// Remplit le tampon ; faux si plus aucune donnée n'arrivera.
    async fn fill(&mut self) -> bool {
        while self.buf.is_empty() {
            if self.eof {
                return false;
            }
            match self.chan.wait().await {
                Some(ChannelMsg::Data { data }) => self.buf = data,
                Some(ChannelMsg::ExtendedData { data, .. }) => self.stderr.extend_from_slice(&data),
                Some(ChannelMsg::ExitStatus { exit_status }) => self.exit = Some(exit_status),
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => self.eof = true,
                Some(ChannelMsg::Failure) => {
                    self.eof = true;
                    self.stderr.extend_from_slice(
                        tr!(
                            "le serveur refuse l'exécution de commandes",
                            "the server refuses to run commands"
                        )
                        .as_bytes(),
                    );
                }
                Some(_) => {}
            }
        }
        true
    }

    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }

    async fn read_byte(&mut self) -> Option<u8> {
        if !self.fill().await {
            return None;
        }
        let b = self.buf[0];
        self.buf = self.buf.slice(1..);
        Some(b)
    }

    async fn read_line(&mut self) -> String {
        let mut line = Vec::new();
        while let Some(b) = self.read_byte().await {
            if b == b'\n' {
                break;
            }
            line.push(b);
        }
        String::from_utf8_lossy(&line).into_owned()
    }

    async fn read_ack(&mut self) -> Result<()> {
        match self.read_byte().await {
            None => {
                let err = self.stderr_text();
                Err(Error::Remote(if err.is_empty() {
                    tr!("Connexion SCP fermée par le serveur (scp est-il installé sur le serveur ?)", "SCP connection closed by the server (is scp installed on the server?)")
                        .into()
                } else {
                    err
                }))
            }
            Some(0) => Ok(()),
            Some(_) => {
                let msg = self.read_line().await;
                Err(Error::Remote(if msg.is_empty() {
                    tr!("Erreur SCP distante", "Remote SCP error").into()
                } else {
                    msg
                }))
            }
        }
    }

    async fn send(&self, data: &[u8]) -> Result<()> {
        self.chan
            .data(data)
            .await
            .map_err(|e| Error::Remote(format!("SCP : {e}")))
    }

    /// Attend la fin du programme distant et vérifie son code de sortie.
    async fn finish(mut self) -> Result<()> {
        let _ = self.chan.eof().await;
        let wait = async {
            while self.exit.is_none() {
                match self.chan.wait().await {
                    Some(ChannelMsg::ExitStatus { exit_status }) => self.exit = Some(exit_status),
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        self.stderr.extend_from_slice(&data)
                    }
                    Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(30), wait).await;
        let _ = self.chan.close().await;
        match self.exit {
            Some(rc) if rc != 0 => {
                let err = self.stderr_text();
                Err(Error::Remote(if err.is_empty() {
                    trf!("scp a retourné le code {rc}", "scp returned code {rc}")
                } else {
                    err
                }))
            }
            _ => Ok(()),
        }
    }
}

pub async fn upload(
    chan: Channel<Msg>,
    local: &Path,
    remote: &str,
    progress: &(dyn Fn(u64) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
    preserve_times: bool,
) -> Result<()> {
    let mut fh = File::open(local)?;
    let meta = fh.metadata()?;
    let mut io = ChanIo::new(chan);
    let flags = if preserve_times { "-t -p" } else { "-t" };
    io.chan
        .exec(true, format!("scp {flags} -- {}", shell_quote(remote)))
        .await?;
    io.read_ack().await?;
    if preserve_times {
        let (mtime, atime) = (crate::platform::mtime(&meta), crate::platform::atime(&meta));
        io.send(format!("T{mtime} 0 {atime} 0\n").as_bytes())
            .await?;
        io.read_ack().await?;
    }
    let name = rustscp_shared::util::rbasename(remote).replace('\n', "_");
    io.send(
        format!(
            "C{:04o} {} {}\n",
            crate::platform::mode(&meta) & 0o7777,
            meta.len(),
            name
        )
        .as_bytes(),
    )
    .await?;
    io.read_ack().await?;
    let mut remaining = meta.len();
    let mut buf = vec![0u8; CHUNK];
    while remaining > 0 {
        if cancelled() {
            let _ = io.chan.close().await;
            return Err(Error::TransferCancelled);
        }
        let want = remaining.min(CHUNK as u64) as usize;
        let n = fh.read(&mut buf[..want])?;
        if n == 0 {
            return Err(Error::Io(
                tr!(
                    "Le fichier local a été tronqué pendant l'envoi",
                    "The local file was truncated during upload"
                )
                .into(),
            ));
        }
        io.send(&buf[..n]).await?;
        remaining -= n as u64;
        progress(n as u64);
    }
    io.send(b"\0").await?;
    io.read_ack().await?;
    io.finish().await
}

/// Télécharge vers `local` (via un fichier .part). Retourne la taille reçue.
pub async fn download(
    chan: Channel<Msg>,
    remote: &str,
    local: &Path,
    progress: &(dyn Fn(u64) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
    preserve_times: bool,
) -> Result<u64> {
    let mut part = local.as_os_str().to_owned();
    part.push(".rustscp-part");
    let part = std::path::PathBuf::from(part);
    let res = download_inner(
        chan,
        remote,
        local,
        &part,
        progress,
        cancelled,
        preserve_times,
    )
    .await;
    if res.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    res
}

async fn download_inner(
    chan: Channel<Msg>,
    remote: &str,
    local: &Path,
    part: &Path,
    progress: &(dyn Fn(u64) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
    preserve_times: bool,
) -> Result<u64> {
    let mut io = ChanIo::new(chan);
    let flag = if preserve_times { "-p " } else { "" };
    io.chan
        .exec(true, format!("scp -f {flag}-- {}", shell_quote(remote)))
        .await?;
    io.send(b"\0").await?;
    let mut times: Option<(i64, i64)> = None;
    let (mode, size) = loop {
        let Some(head) = io.read_byte().await else {
            let err = io.stderr_text();
            return Err(Error::Remote(if err.is_empty() {
                tr!(
                    "Connexion SCP fermée par le serveur",
                    "SCP connection closed by the server"
                )
                .into()
            } else {
                err
            }));
        };
        if head == 1 || head == 2 {
            let msg = io.read_line().await;
            return Err(Error::Remote(if msg.is_empty() {
                tr!("Erreur SCP distante", "Remote SCP error").into()
            } else {
                msg
            }));
        }
        let line = io.read_line().await;
        match head {
            b'T' => {
                let p: Vec<&str> = line.split_whitespace().collect();
                if p.len() >= 3 {
                    times = Some((p[0].parse().unwrap_or(0), p[2].parse().unwrap_or(0)));
                }
                io.send(b"\0").await?;
            }
            b'C' => {
                let mut it = line.splitn(3, ' ');
                let mode = u32::from_str_radix(it.next().unwrap_or("644"), 8).unwrap_or(0o644);
                let size: u64 = it.next().and_then(|s| s.parse().ok()).ok_or_else(|| {
                    Error::Remote(trf!(
                        "Réponse SCP invalide : C{line}",
                        "Invalid SCP response: C{line}"
                    ))
                })?;
                break (mode, size);
            }
            b'D' => {
                return Err(Error::Remote(
                    tr!("La source est un dossier", "The source is a directory").into(),
                ));
            }
            other => {
                return Err(Error::Remote(trf!(
                    "Réponse SCP inattendue : {:?}{line}",
                    "Unexpected SCP response: {:?}{line}",
                    other as char
                )));
            }
        }
    };
    io.send(b"\0").await?;
    let mut out = BufWriter::with_capacity(1 << 20, File::create(part)?);
    let mut remaining = size;
    while remaining > 0 {
        if cancelled() {
            let _ = io.chan.close().await;
            return Err(Error::TransferCancelled);
        }
        if !io.fill().await {
            return Err(Error::Remote(
                tr!(
                    "Connexion interrompue pendant le téléchargement",
                    "Connection interrupted during download"
                )
                .into(),
            ));
        }
        let take = (io.buf.len() as u64).min(remaining) as usize;
        let chunk = io.buf.split_to(take);
        out.write_all(&chunk)?;
        remaining -= take as u64;
        progress(take as u64);
    }
    let file = out.into_inner().map_err(|e| Error::Io(e.to_string()))?;
    io.read_ack().await?;
    io.send(b"\0").await?;
    crate::platform::set_file_mode(&file, mode & 0o777);
    if let Some((mtime, atime)) = times.filter(|_| preserve_times) {
        let t = |s: i64| UNIX_EPOCH + Duration::from_secs(s.max(0) as u64);
        let _ = file.set_times(
            FileTimes::new()
                .set_modified(t(mtime))
                .set_accessed(t(atime)),
        );
    }
    drop(file);
    std::fs::rename(part, local)?;
    let _ = io.finish().await;
    Ok(size)
}
