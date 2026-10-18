//! Client SFTP au-dessus d'un canal SSH (sous-système « sftp »).
//!
//! Utilise la session SFTP « brute » de russh-sftp pour pouvoir envoyer de
//! nombreuses requêtes READ/WRITE en parallèle (pipeline) : indispensable pour
//! un bon débit sur les liaisons à latence.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::stream::{FuturesUnordered, StreamExt};
use russh::Channel;
use russh::client::Msg;
use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};
use rustscp_shared::Entry;
use rustscp_shared::util::{S_IFDIR, S_IFLNK, S_IFMT, rjoin};

use crate::error::{Error, Result};
use rustscp_shared::tr;

/// Taille des blocs lus / écrits et nombre de requêtes simultanées.
pub const CHUNK: u32 = 64 * 1024;
pub const IN_FLIGHT: usize = 48;

pub struct Sftp {
    raw: RawSftpSession,
    posix_rename: bool,
    closed: AtomicBool,
}

fn is_status(e: &SftpError, code: StatusCode) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == code)
}

fn ssh_string(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
}

pub fn attrs_to_entry(name: String, a: &FileAttributes, longname: &str) -> Entry {
    let mode = a.permissions.unwrap_or(0);
    let (mut owner, mut group) = (
        a.user.clone().unwrap_or_default(),
        a.group.clone().unwrap_or_default(),
    );
    if owner.is_empty() {
        let parts: Vec<&str> = longname.split_whitespace().collect();
        if parts.len() >= 4 {
            owner = parts[2].to_string();
            group = parts[3].to_string();
        } else if let (Some(u), Some(g)) = (a.uid, a.gid) {
            owner = u.to_string();
            group = g.to_string();
        }
    }
    Entry {
        name,
        size: a.size.unwrap_or(0),
        mtime: a.mtime.unwrap_or(0) as i64,
        mode,
        is_dir: mode & S_IFMT == S_IFDIR,
        is_link: mode & S_IFMT == S_IFLNK,
        owner,
        group,
    }
}

impl Sftp {
    pub async fn new(chan: Channel<Msg>) -> Result<Self> {
        let cfg = russh_sftp::client::Config {
            request_timeout_secs: 120,
            max_concurrent_reads: IN_FLIGHT,
            max_concurrent_writes: IN_FLIGHT,
            ..Default::default()
        };
        let raw = RawSftpSession::new_with_config(chan.into_stream(), cfg);
        let version = tokio::time::timeout(std::time::Duration::from_secs(20), raw.init())
            .await
            .map_err(|_| {
                Error::Remote(
                    tr!(
                        "SFTP : pas de réponse du serveur",
                        "SFTP: no response from the server"
                    )
                    .into(),
                )
            })??;
        let posix_rename = version.extensions.contains_key("posix-rename@openssh.com");
        Ok(Sftp {
            raw,
            posix_rename,
            closed: AtomicBool::new(false),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        let _ = self.raw.close_session();
        self.closed.store(true, Ordering::Relaxed);
    }

    /// Convertit l'erreur ; une erreur hors statut SFTP signifie que le canal est mort.
    fn check<T>(&self, r: std::result::Result<T, SftpError>) -> Result<T> {
        r.map_err(|e| {
            if !matches!(e, SftpError::Status(_)) {
                self.closed.store(true, Ordering::Relaxed);
            }
            Error::from(e)
        })
    }

    pub async fn realpath(&self, path: &str) -> Result<String> {
        let name = self.check(self.raw.realpath(path).await)?;
        name.files
            .into_iter()
            .next()
            .map(|f| f.filename)
            .ok_or_else(|| {
                Error::Remote(tr!("realpath : réponse vide", "realpath: empty response").into())
            })
    }

    /// None si le chemin n'existe pas.
    pub async fn stat(&self, path: &str) -> Result<Option<FileAttributes>> {
        match self.raw.stat(path).await {
            Ok(a) => Ok(Some(a.attrs)),
            Err(e) if is_status(&e, StatusCode::NoSuchFile) => Ok(None),
            Err(e) => self.check(Err(e)),
        }
    }

    pub async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let handle = self.check(self.raw.opendir(path).await)?.handle;
        let mut entries = Vec::new();
        let mut links = Vec::new();
        loop {
            match self.raw.readdir(handle.as_str()).await {
                Ok(name) => {
                    for f in name.files {
                        if f.filename == "." || f.filename == ".." {
                            continue;
                        }
                        let e = attrs_to_entry(f.filename, &f.attrs, &f.longname);
                        if e.is_link {
                            links.push(entries.len());
                        }
                        entries.push(e);
                    }
                }
                Err(e) if is_status(&e, StatusCode::Eof) => break,
                Err(e) => {
                    let _ = self.raw.close(handle.as_str()).await;
                    return self.check(Err(e));
                }
            }
        }
        let _ = self.raw.close(handle).await;
        // Résout la cible des liens (en parallèle) pour savoir si ce sont des dossiers.
        let mut pending: FuturesUnordered<_> = links
            .into_iter()
            .map(|i| {
                let target = rjoin(path, &entries[i].name);
                async move { (i, self.raw.stat(target).await) }
            })
            .collect();
        let mut resolved = Vec::new();
        while let Some((i, res)) = pending.next().await {
            if let Ok(a) = res {
                resolved.push((i, a.attrs));
            }
        }
        drop(pending);
        for (i, a) in resolved {
            let target_mode = a.permissions.unwrap_or(0);
            entries[i].is_dir = target_mode & S_IFMT == S_IFDIR;
            if !entries[i].is_dir {
                entries[i].size = a.size.unwrap_or(0);
            }
        }
        Ok(entries)
    }

    pub async fn mkdir(&self, path: &str) -> Result<()> {
        self.check(self.raw.mkdir(path, FileAttributes::empty()).await)
            .map(|_| ())
    }

    pub async fn rmdir(&self, path: &str) -> Result<()> {
        self.check(self.raw.rmdir(path).await).map(|_| ())
    }

    pub async fn remove(&self, path: &str) -> Result<()> {
        self.check(self.raw.remove(path).await).map(|_| ())
    }

    pub async fn rename(&self, src: &str, dst: &str) -> Result<()> {
        if self.posix_rename {
            let mut data = Vec::new();
            ssh_string(&mut data, src);
            ssh_string(&mut data, dst);
            match self.check(self.raw.extended("posix-rename@openssh.com", data).await)? {
                Packet::Status(s) if s.status_code == StatusCode::Ok => return Ok(()),
                Packet::Status(s) => {
                    return Err(Error::Remote(if s.error_message.is_empty() {
                        s.status_code.to_string()
                    } else {
                        s.error_message
                    }));
                }
                _ => {}
            }
        }
        self.check(self.raw.rename(src, dst).await).map(|_| ())
    }

    pub async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let attrs = FileAttributes {
            permissions: Some(mode & 0o7777),
            ..FileAttributes::empty()
        };
        self.check(self.raw.setstat(path, attrs).await).map(|_| ())
    }

    pub async fn set_times(&self, path: &str, atime: i64, mtime: i64) -> Result<()> {
        let attrs = FileAttributes {
            atime: Some(atime as u32),
            mtime: Some(mtime as u32),
            ..FileAttributes::empty()
        };
        self.check(self.raw.setstat(path, attrs).await).map(|_| ())
    }

    pub async fn touch(&self, path: &str) -> Result<()> {
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
        let h = self
            .check(self.raw.open(path, flags, FileAttributes::empty()).await)?
            .handle;
        self.check(self.raw.close(h).await).map(|_| ())
    }

    /// Télécharge `remote` dans le fichier local déjà ouvert ;
    /// retourne (taille reçue, date de modification distante).
    pub async fn download(
        &self,
        remote: &str,
        out: &File,
        on_size: &(dyn Fn(u64) + Sync),
        progress: &(dyn Fn(u64) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(u64, i64)> {
        let handle = self
            .check(
                self.raw
                    .open(remote, OpenFlags::READ, FileAttributes::empty())
                    .await,
            )?
            .handle;
        let res = self
            .download_handle(&handle, out, on_size, progress, cancelled)
            .await;
        let _ = self.raw.close(handle).await;
        res
    }

    async fn download_handle(
        &self,
        handle: &str,
        out: &File,
        on_size: &(dyn Fn(u64) + Sync),
        progress: &(dyn Fn(u64) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<(u64, i64)> {
        let attrs = self.check(self.raw.fstat(handle).await)?.attrs;
        let size = attrs.size.unwrap_or(0);
        on_size(size);
        let read =
            |off: u64, len: u32| async move { (off, len, self.raw.read(handle, off, len).await) };
        let mut inflight = FuturesUnordered::new();
        let mut next = 0u64;
        let mut eof_at: Option<u64> = None;
        let mut received = 0u64;
        // Au-delà de la taille annoncée, on continue tant que le serveur renvoie des données
        // (fichiers qui grossissent, /proc…).
        loop {
            while inflight.len() < IN_FLIGHT
                && eof_at.is_none_or(|e| next < e)
                && (next < size || inflight.is_empty())
            {
                inflight.push(read(next, CHUNK));
                next += CHUNK as u64;
            }
            let Some((off, len, res)) = inflight.next().await else {
                break;
            };
            if cancelled() {
                return Err(Error::TransferCancelled);
            }
            match res {
                Ok(data) => {
                    let n = data.data.len() as u64;
                    if n > 0 {
                        crate::platform::write_all_at(out, &data.data, off)?;
                        received += n;
                        progress(n);
                    }
                    if n < len as u64 && n > 0 {
                        // lecture partielle : on redemande le reste
                        inflight.push(read(off + n, len - n as u32));
                    } else if n == 0 {
                        eof_at = Some(eof_at.map_or(off, |e| e.min(off)));
                    }
                }
                Err(e) if is_status(&e, StatusCode::Eof) => {
                    eof_at = Some(eof_at.map_or(off, |e| e.min(off)));
                }
                Err(e) => return self.check(Err(e)),
            }
        }
        Ok((received, attrs.mtime.unwrap_or(0) as i64))
    }

    /// Envoie un fichier local ; `mode` = droits à appliquer au fichier distant.
    pub async fn upload(
        &self,
        input: &File,
        size: u64,
        remote: &str,
        progress: &(dyn Fn(u64) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<()> {
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
        let handle = self
            .check(self.raw.open(remote, flags, FileAttributes::empty()).await)?
            .handle;
        let res = self
            .upload_handle(&handle, input, size, progress, cancelled)
            .await;
        let closed = self.raw.close(handle.as_str()).await;
        res?;
        self.check(closed).map(|_| ())
    }

    async fn upload_handle(
        &self,
        handle: &str,
        input: &File,
        size: u64,
        progress: &(dyn Fn(u64) + Sync),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<()> {
        let write = |off: u64, data: Vec<u8>| {
            let n = data.len() as u64;
            async move { (n, self.raw.write(handle, off, data).await) }
        };
        let mut inflight = FuturesUnordered::new();
        let mut off = 0u64;
        let mut done = false;
        loop {
            while !done && inflight.len() < IN_FLIGHT {
                let want = (size.saturating_sub(off)).min(CHUNK as u64) as usize;
                let mut buf = vec![0u8; want.max(1)];
                let n = if want == 0 {
                    0
                } else {
                    crate::platform::read_at(input, &mut buf, off)?
                };
                if n == 0 {
                    done = true;
                    if off < size {
                        return Err(Error::Io(
                            tr!(
                                "Le fichier local a été tronqué pendant l'envoi",
                                "The local file was truncated during upload"
                            )
                            .into(),
                        ));
                    }
                    break;
                }
                buf.truncate(n);
                inflight.push(write(off, buf));
                off += n as u64;
            }
            let Some((n, res)) = inflight.next().await else {
                break;
            };
            self.check(res)?;
            progress(n);
            if cancelled() {
                return Err(Error::TransferCancelled);
            }
        }
        Ok(())
    }
}
