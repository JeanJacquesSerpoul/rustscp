//! Opérations sur le système de fichiers distant.
//!
//! Utilise SFTP lorsqu'il est disponible (le plus rapide pour lister) et se replie
//! sur des commandes shell (find/mkdir/mv/rm…) pour les serveurs « SCP seul ».

use std::time::Duration;

use rustscp_shared::Entry;
use rustscp_shared::util::{
    S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO, S_IFLNK, S_IFREG, S_IFSOCK, rbasename, rjoin, rparent,
};

use crate::error::{Error, Result};
use crate::sftp::attrs_to_entry;
use crate::ssh::{Session, shell_quote as q};

fn type_mode(t: &str) -> u32 {
    match t {
        "d" => S_IFDIR,
        "l" => S_IFLNK,
        "b" => S_IFBLK,
        "c" => S_IFCHR,
        "p" => S_IFIFO,
        "s" => S_IFSOCK,
        _ => S_IFREG,
    }
}

impl Session {
    async fn sh(&self, command: &str, timeout: Duration) -> Result<Vec<u8>> {
        let (rc, out, err) = self.exec(command, timeout).await?;
        if rc != 0 {
            let msg = String::from_utf8_lossy(&err).trim().to_string();
            return Err(Error::Remote(if msg.is_empty() {
                format!("code de sortie {rc}")
            } else {
                msg
            }));
        }
        Ok(out)
    }

    /// Répertoire personnel (appelé juste après l'authentification).
    pub(crate) async fn fs_home(&self, conn: &std::sync::Arc<crate::ssh::Conn>) -> String {
        if let Some(sftp) = self.browse_sftp_on(conn).await
            && let Ok(p) = sftp.realpath(".").await
        {
            return p;
        }
        match conn.exec("pwd", Duration::from_secs(20)).await {
            Ok((0, out, _)) => {
                let p = String::from_utf8_lossy(&out).trim().to_string();
                if p.is_empty() { "/".into() } else { p }
            }
            _ => "/".into(),
        }
    }

    pub async fn realpath(&self, path: &str) -> Result<String> {
        let (conn, sftp) = self.browse_sftp().await?;
        if let Some(s) = sftp {
            let r = s.realpath(path).await;
            if s.is_closed() {
                self.reset_browse(&conn).await;
            }
            return r;
        }
        let out = self
            .sh(
                &format!("cd -- {} && pwd -P", q(path)),
                Duration::from_secs(20),
            )
            .await?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    pub async fn listdir(&self, path: &str) -> Result<Vec<Entry>> {
        let (conn, sftp) = self.browse_sftp().await?;
        if let Some(s) = sftp {
            let r = s.list(path).await;
            if s.is_closed() {
                self.reset_browse(&conn).await;
            }
            return r.map_err(|e| Error::Remote(format!("{path} : {e}")));
        }
        self.listdir_shell(path).await
    }

    pub async fn listdir_shell(&self, path: &str) -> Result<Vec<Entry>> {
        let cmd = format!(
            "LC_ALL=C find {} -mindepth 1 -maxdepth 1 -printf '%y\\t%Y\\t%s\\t%T@\\t%m\\t%u\\t%g\\t%f\\0'",
            q(path)
        );
        let raw = self
            .sh(&cmd, Duration::from_secs(120))
            .await
            .map_err(|e| Error::Remote(format!("{path} : {e}")))?;
        let mut entries = Vec::new();
        for rec in raw.split(|b| *b == 0) {
            if rec.is_empty() {
                continue;
            }
            let text = String::from_utf8_lossy(rec);
            let parts: Vec<&str> = text.splitn(8, '\t').collect();
            if parts.len() != 8 {
                continue;
            }
            let perm = u32::from_str_radix(parts[4], 8).unwrap_or(0);
            entries.push(Entry {
                name: parts[7].to_string(),
                size: parts[2].parse().unwrap_or(0),
                mtime: parts[3]
                    .split('.')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
                mode: type_mode(parts[0]) | perm,
                is_dir: parts[1] == "d",
                is_link: parts[0] == "l",
                owner: parts[5].to_string(),
                group: parts[6].to_string(),
            });
        }
        Ok(entries)
    }

    /// None si le chemin n'existe pas.
    pub async fn stat(&self, path: &str) -> Result<Option<Entry>> {
        let (conn, sftp) = self.browse_sftp().await?;
        if let Some(s) = sftp {
            let r = s.stat(path).await;
            if s.is_closed() {
                self.reset_browse(&conn).await;
            }
            return Ok(r?.map(|a| attrs_to_entry(rbasename(path).to_string(), &a, "")));
        }
        let cmd = format!(
            "LC_ALL=C find {} -maxdepth 0 -printf '%Y\\t%s\\t%T@\\t%m'",
            q(path)
        );
        let (rc, out, _) = self.exec(&cmd, Duration::from_secs(30)).await?;
        if rc != 0 || out.is_empty() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out);
        let p: Vec<&str> = text.trim().split('\t').collect();
        if p.len() < 4 {
            return Ok(None);
        }
        Ok(Some(Entry {
            name: rbasename(path).to_string(),
            size: p[1].parse().unwrap_or(0),
            mtime: p[2]
                .split('.')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            mode: type_mode(p[0]) | u32::from_str_radix(p[3], 8).unwrap_or(0),
            is_dir: p[0] == "d",
            ..Default::default()
        }))
    }

    /// Liste récursive : (chemin relatif, entrée) pour tous les fichiers, (dossier vide, None).
    pub async fn walk_files(
        &self,
        root: &str,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Vec<(String, Option<Entry>)>> {
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(rel) = stack.pop() {
            if cancelled() {
                break;
            }
            let dir = if rel.is_empty() {
                root.to_string()
            } else {
                rjoin(root, &rel)
            };
            let mut entries = self.listdir(&dir).await?;
            if entries.is_empty() {
                out.push((rel.clone(), None));
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            for e in entries {
                let child = if rel.is_empty() {
                    e.name.clone()
                } else {
                    format!("{rel}/{}", e.name)
                };
                if e.is_dir && !e.is_link {
                    stack.push(child);
                } else if !e.is_dir {
                    out.push((child, Some(e)));
                }
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------- modifications
    pub async fn mkdir(&self, path: &str, parents: bool) -> Result<()> {
        let (conn, sftp) = self.browse_sftp().await?;
        if let Some(s) = sftp {
            let r = if parents {
                let mut missing = Vec::new();
                let mut cur = path.trim_end_matches('/').to_string();
                while !cur.is_empty() && cur != "/" {
                    match s.stat(&cur).await? {
                        Some(a) if a.permissions.unwrap_or(0) & 0o170000 == S_IFDIR => break,
                        Some(_) => {
                            return Err(Error::Remote(format!(
                                "{cur} existe et n'est pas un dossier"
                            )));
                        }
                        None => {
                            missing.push(cur.clone());
                            cur = rparent(&cur);
                        }
                    }
                }
                let mut res = Ok(());
                for d in missing.iter().rev() {
                    if let Err(e) = s.mkdir(d).await
                        && s.stat(d).await.ok().flatten().is_none()
                    {
                        res = Err(Error::Remote(format!("mkdir {d} : {e}")));
                        break;
                    }
                }
                res
            } else {
                s.mkdir(path)
                    .await
                    .map_err(|e| Error::Remote(format!("mkdir {path} : {e}")))
            };
            if s.is_closed() {
                self.reset_browse(&conn).await;
            }
            return r;
        }
        let flag = if parents { "-p " } else { "" };
        self.sh(
            &format!("mkdir {flag}-- {}", q(path)),
            Duration::from_secs(60),
        )
        .await
        .map(|_| ())
    }

    pub async fn rename(&self, src: &str, dst: &str) -> Result<()> {
        let (conn, sftp) = self.browse_sftp().await?;
        if let Some(s) = sftp {
            let r = s
                .rename(src, dst)
                .await
                .map_err(|e| Error::Remote(format!("Renommage impossible : {e}")));
            if s.is_closed() {
                self.reset_browse(&conn).await;
            }
            return r;
        }
        self.sh(
            &format!("mv -- {} {}", q(src), q(dst)),
            Duration::from_secs(120),
        )
        .await
        .map(|_| ())
    }

    pub async fn chmod(&self, path: &str, mode: u32, recursive: bool) -> Result<()> {
        if recursive && self.shell_ok() == Some(true) {
            return self
                .sh(
                    &format!("chmod -R {:o} -- {}", mode & 0o7777, q(path)),
                    Duration::from_secs(600),
                )
                .await
                .map(|_| ());
        }
        let (conn, sftp) = self.browse_sftp().await?;
        if let Some(s) = sftp {
            let r = if recursive {
                self.chmod_tree_sftp(&s, path, mode).await
            } else {
                s.chmod(path, mode)
                    .await
                    .map_err(|e| Error::Remote(format!("chmod {path} : {e}")))
            };
            if s.is_closed() {
                self.reset_browse(&conn).await;
            }
            return r;
        }
        let flag = if recursive { "-R " } else { "" };
        self.sh(
            &format!("chmod {flag}{:o} -- {}", mode & 0o7777, q(path)),
            Duration::from_secs(600),
        )
        .await
        .map(|_| ())
    }

    async fn chmod_tree_sftp(&self, s: &crate::sftp::Sftp, path: &str, mode: u32) -> Result<()> {
        let mut stack = vec![path.to_string()];
        while let Some(dir) = stack.pop() {
            s.chmod(&dir, mode).await?;
            for e in s.list(&dir).await? {
                let child = rjoin(&dir, &e.name);
                if e.is_dir && !e.is_link {
                    stack.push(child);
                } else if !e.is_link {
                    s.chmod(&child, mode).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn remove(&self, path: &str, is_dir: bool) -> Result<()> {
        if is_dir && self.shell_ok() == Some(true) {
            return self
                .sh(&format!("rm -rf -- {}", q(path)), Duration::from_secs(3600))
                .await
                .map(|_| ());
        }
        let (conn, sftp) = self.browse_sftp().await?;
        if let Some(s) = sftp {
            let r = if is_dir {
                self.rmtree_sftp(&s, path).await
            } else {
                s.remove(path).await
            };
            if s.is_closed() {
                self.reset_browse(&conn).await;
            }
            return r.map_err(|e| Error::Remote(format!("Suppression de {path} impossible : {e}")));
        }
        let flag = if is_dir { "rf" } else { "f" };
        self.sh(
            &format!("rm -{flag} -- {}", q(path)),
            Duration::from_secs(3600),
        )
        .await
        .map(|_| ())
    }

    async fn rmtree_sftp(&self, s: &crate::sftp::Sftp, path: &str) -> Result<()> {
        // parcours en profondeur itératif : on supprime les dossiers après leur contenu
        let mut stack = vec![(path.to_string(), false)];
        while let Some((dir, visited)) = stack.pop() {
            if visited {
                s.rmdir(&dir).await?;
                continue;
            }
            stack.push((dir.clone(), true));
            for e in s.list(&dir).await? {
                let child = rjoin(&dir, &e.name);
                if e.is_dir && !e.is_link {
                    stack.push((child, false));
                } else {
                    s.remove(&child).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn touch(&self, path: &str) -> Result<()> {
        let (conn, sftp) = self.browse_sftp().await?;
        if let Some(s) = sftp {
            let r = s.touch(path).await;
            if s.is_closed() {
                self.reset_browse(&conn).await;
            }
            return r;
        }
        self.sh(&format!(": > {}", q(path)), Duration::from_secs(30))
            .await
            .map(|_| ())
    }
}
