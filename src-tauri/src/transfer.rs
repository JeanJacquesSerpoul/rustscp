//! File d'attente de transferts avec workers parallèles.
//!
//! Tous les workers d'une même session partagent son transport SSH authentifié
//! (un canal par transfert) : parallélisme sans nouvelle saisie de code TOTP.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustscp_shared::util::{fmt_int, rjoin, rparent};
use rustscp_shared::{
    Direction, EditChanged, PROTO_SCP, PromptReply, PromptRequest, RemoteItem, Settings,
    TransferFinished, TransferPoll, TransferProgress, TransferStats, TransferStatus, TransferView,
};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::error::{Error, Result};
use crate::ftp::Ftp;
use crate::platform::{self, native_path, ui_path};
use crate::scp;
use crate::sftp::Sftp;
use crate::ssh::Session;
use crate::ui::Ui;
use rustscp_shared::trf;

/// Lance une tâche asynchrone, y compris depuis une commande Tauri synchrone
/// (thread principal, sans runtime tokio : `tokio::spawn` y paniquerait).
fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => drop(h.spawn(fut)),
        Err(_) => drop(tauri::async_runtime::spawn(fut)),
    }
}

/// Compteurs mis à jour sans verrou pendant le transfert.
struct Progress {
    transferred: AtomicU64,
    size: AtomicU64,
    speed_bits: AtomicU64,
    cancel: AtomicBool,
    start: Instant,
    sample_ms: AtomicU64,
    sample_bytes: AtomicU64,
}

impl Progress {
    fn new(size: u64) -> Arc<Self> {
        Arc::new(Progress {
            transferred: AtomicU64::new(0),
            size: AtomicU64::new(size),
            speed_bits: AtomicU64::new(0),
            cancel: AtomicBool::new(false),
            start: Instant::now(),
            sample_ms: AtomicU64::new(0),
            sample_bytes: AtomicU64::new(0),
        })
    }

    fn speed(&self) -> f64 {
        f64::from_bits(self.speed_bits.load(Ordering::Relaxed))
    }

    fn reset(&self) {
        self.transferred.store(0, Ordering::Relaxed);
        self.speed_bits.store(0f64.to_bits(), Ordering::Relaxed);
        self.sample_ms
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
        self.sample_bytes.store(0, Ordering::Relaxed);
    }

    fn add(&self, n: u64) {
        let total = self.transferred.fetch_add(n, Ordering::Relaxed) + n;
        let now = self.start.elapsed().as_millis() as u64;
        let last = self.sample_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= 500 {
            let dt = (now - last) as f64 / 1000.0;
            let inst = total.saturating_sub(self.sample_bytes.load(Ordering::Relaxed)) as f64 / dt;
            let old = self.speed();
            let speed = if old == 0.0 {
                inst
            } else {
                old * 0.6 + inst * 0.4
            };
            self.speed_bits.store(speed.to_bits(), Ordering::Relaxed);
            self.sample_ms.store(now, Ordering::Relaxed);
            self.sample_bytes.store(total, Ordering::Relaxed);
        }
    }
}

pub struct NewItem {
    pub direction: Direction,
    pub local: PathBuf,
    pub remote: String,
    pub size: u64,
    pub session: Arc<Session>,
    pub force: bool,
    pub tag: String,
}

impl NewItem {
    pub fn new(
        direction: Direction,
        local: PathBuf,
        remote: String,
        size: u64,
        session: Arc<Session>,
    ) -> Self {
        NewItem {
            direction,
            local,
            remote,
            size,
            session,
            force: false,
            tag: String::new(),
        }
    }
}

struct Item {
    id: u64,
    direction: Direction,
    local: PathBuf,
    remote: String,
    session: Arc<Session>,
    server: String,
    status: TransferStatus,
    error: String,
    protocol: String,
    force: bool,
    tag: String,
    rev: u64,
    progress: Arc<Progress>,
}

impl Item {
    fn view(&self) -> TransferView {
        let p = &self.progress;
        TransferView {
            id: self.id,
            direction: self.direction,
            local: ui_path(&self.local),
            remote: self.remote.clone(),
            size: p.size.load(Ordering::Relaxed),
            transferred: p.transferred.load(Ordering::Relaxed),
            speed: if self.status == TransferStatus::Active {
                p.speed()
            } else {
                0.0
            },
            status: self.status,
            error: self.error.clone(),
            protocol: self.protocol.clone(),
            server: self.server.clone(),
            session_id: self.session.id,
            tag: self.tag.clone(),
        }
    }

    fn name(&self) -> String {
        match self.direction {
            Direction::Up => self
                .local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Direction::Down => rustscp_shared::util::rbasename(&self.remote).to_string(),
        }
    }
}

/// Copie de travail d'un élément pendant son traitement.
struct Job {
    id: u64,
    direction: Direction,
    local: PathBuf,
    remote: String,
    session: Arc<Session>,
    force: bool,
    progress: Arc<Progress>,
    protocol: String,
    /// Début effectif du transfert (après une éventuelle question d'écrasement).
    begun: Option<Instant>,
}

impl Job {
    fn cancelled(&self) -> bool {
        self.progress.cancel.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct Inner {
    items: BTreeMap<u64, Item>,
    pending: VecDeque<u64>,
    version: u64,
    removed_log: VecDeque<(u64, u64)>,
    log_floor: u64,
    running: bool,
    overwrite_all: Option<String>,
    workers: usize,
    next_id: u64,
}

impl Inner {
    fn bump(&mut self) -> u64 {
        self.version += 1;
        self.version
    }

    fn touch(&mut self, id: u64) {
        let v = self.bump();
        if let Some(it) = self.items.get_mut(&id) {
            it.rev = v;
        }
    }

    fn log_removed(&mut self, id: u64) {
        let v = self.bump();
        self.removed_log.push_back((v, id));
        while self.removed_log.len() > 50_000 {
            if let Some((v, _)) = self.removed_log.pop_front() {
                self.log_floor = v;
            }
        }
    }
}

pub struct TransferManager {
    inner: Mutex<Inner>,
    notify: Notify,
    settings: Arc<RwLock<Settings>>,
    ui: Arc<dyn Ui>,
    overwrite_lock: AsyncMutex<()>,
    /// Canaux SFTP partagés par les workers, par session : (génération, clients).
    pool: Mutex<HashMap<u64, SftpPool>>,
    pub edits: Arc<Edits>,
}

/// (génération de la connexion, clients SFTP, indice du prochain client à utiliser)
type SftpPool = (u64, Vec<Arc<Sftp>>, usize);

/// Canaux SFTP ouverts au plus par session pour les transferts (OpenSSH limite à 10
/// le nombre de canaux par connexion : MaxSessions).
const SFTP_POOL: usize = 4;

impl TransferManager {
    pub fn new(settings: Arc<RwLock<Settings>>, ui: Arc<dyn Ui>) -> Arc<Self> {
        let edits = Edits::new(ui.clone());
        Arc::new(TransferManager {
            inner: Mutex::new(Inner {
                running: true,
                next_id: 1,
                ..Default::default()
            }),
            notify: Notify::new(),
            settings,
            ui,
            overwrite_lock: AsyncMutex::new(()),
            pool: Mutex::new(HashMap::new()),
            edits,
        })
    }

    fn max_workers(&self) -> usize {
        self.settings.read().unwrap().max_transfers.clamp(1, 32) as usize
    }

    // ------------------------------------------------------------ file
    pub fn add(self: &Arc<Self>, items: Vec<NewItem>) -> Vec<u64> {
        let mut ids = Vec::with_capacity(items.len());
        {
            let mut inner = self.inner.lock().unwrap();
            let v = inner.bump();
            for n in items {
                let id = inner.next_id;
                inner.next_id += 1;
                let server = n.session.label();
                inner.items.insert(
                    id,
                    Item {
                        id,
                        direction: n.direction,
                        local: n.local,
                        remote: n.remote,
                        session: n.session,
                        server,
                        status: TransferStatus::Queued,
                        error: String::new(),
                        protocol: String::new(),
                        force: n.force,
                        tag: n.tag,
                        rev: v,
                        progress: Progress::new(n.size),
                    },
                );
                inner.pending.push_back(id);
                ids.push(id);
            }
        }
        self.ensure_workers();
        self.notify.notify_waiters();
        ids
    }

    pub fn ensure_workers(self: &Arc<Self>) {
        let max = self.max_workers();
        let mut inner = self.inner.lock().unwrap();
        while inner.workers < max {
            inner.workers += 1;
            let me = self.clone();
            spawn(async move { me.worker().await });
        }
        drop(inner);
        self.notify.notify_waiters();
    }

    pub fn set_running(self: &Arc<Self>, running: bool) {
        self.inner.lock().unwrap().running = running;
        if running {
            self.ensure_workers();
        }
        self.notify.notify_waiters();
    }

    pub fn cancel(&self, ids: &[u64]) {
        let mut inner = self.inner.lock().unwrap();
        for id in ids {
            let Some(it) = inner.items.get_mut(id) else {
                continue;
            };
            it.progress.cancel.store(true, Ordering::Relaxed);
            if it.status == TransferStatus::Queued {
                it.status = TransferStatus::Cancelled;
                inner.pending.retain(|p| p != id);
                inner.touch(*id);
            }
        }
        inner.bump();
    }

    pub fn cancel_all(&self) {
        let ids: Vec<u64> = {
            let inner = self.inner.lock().unwrap();
            inner
                .items
                .values()
                .filter(|i| i.status.group() == 0)
                .map(|i| i.id)
                .collect()
        };
        self.cancel(&ids);
    }

    pub fn cancel_session(&self, session_id: u64) {
        let ids: Vec<u64> = {
            let inner = self.inner.lock().unwrap();
            inner
                .items
                .values()
                .filter(|i| i.session.id == session_id && i.status.group() == 0)
                .map(|i| i.id)
                .collect()
        };
        self.cancel(&ids);
    }

    pub fn session_busy(&self, session_id: u64) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .items
            .values()
            .any(|i| i.session.id == session_id && i.status.group() == 0)
    }

    pub fn remove(&self, ids: &[u64]) {
        let mut inner = self.inner.lock().unwrap();
        for id in ids {
            let active = match inner.items.get(id) {
                Some(it) => {
                    it.progress.cancel.store(true, Ordering::Relaxed);
                    it.status == TransferStatus::Active
                }
                None => continue,
            };
            if !active {
                inner.items.remove(id);
                inner.pending.retain(|p| p != id);
                inner.log_removed(*id);
            }
        }
    }

    pub fn retry(self: &Arc<Self>, ids: &[u64]) {
        {
            let mut inner = self.inner.lock().unwrap();
            for id in ids {
                let Some(it) = inner.items.get_mut(id) else {
                    continue;
                };
                if matches!(
                    it.status,
                    TransferStatus::Failed | TransferStatus::Cancelled | TransferStatus::Skipped
                ) {
                    it.status = TransferStatus::Queued;
                    it.error.clear();
                    let size = it.progress.size.load(Ordering::Relaxed);
                    it.progress = Progress::new(size);
                    inner.pending.push_back(*id);
                    inner.touch(*id);
                }
            }
        }
        self.ensure_workers();
    }

    /// Retire de la file les éléments d'un onglet (1 = échecs, 2 = réussis).
    pub fn clear(&self, group: usize) {
        if group == 0 {
            return self.cancel_all();
        }
        let mut inner = self.inner.lock().unwrap();
        let ids: Vec<u64> = inner
            .items
            .values()
            .filter(|i| i.status.group() == group)
            .map(|i| i.id)
            .collect();
        for id in ids {
            inner.items.remove(&id);
            inner.log_removed(id);
        }
    }

    pub fn stats(&self) -> TransferStats {
        let inner = self.inner.lock().unwrap();
        let mut s = TransferStats::default();
        for it in inner.items.values() {
            match it.status {
                TransferStatus::Queued => s.queued += 1,
                TransferStatus::Active => {
                    s.active += 1;
                    s.speed += it.progress.speed();
                }
                _ => {}
            }
        }
        s
    }

    pub fn poll(&self, since: u64) -> TransferPoll {
        let inner = self.inner.lock().unwrap();
        let reset = since == 0 || since < inner.log_floor || since > inner.version;
        let mut poll = TransferPoll {
            version: inner.version,
            reset,
            running: inner.running,
            ..Default::default()
        };
        for it in inner.items.values() {
            if reset || it.rev > since {
                poll.upserts.push(it.view());
            }
            match it.status {
                TransferStatus::Queued => poll.stats.queued += 1,
                TransferStatus::Active => {
                    let p = &it.progress;
                    let speed = p.speed();
                    poll.stats.active += 1;
                    poll.stats.speed += speed;
                    poll.progress.push(TransferProgress {
                        id: it.id,
                        transferred: p.transferred.load(Ordering::Relaxed),
                        size: p.size.load(Ordering::Relaxed),
                        speed,
                    });
                }
                _ => {}
            }
        }
        if !reset {
            poll.removed = inner
                .removed_log
                .iter()
                .filter(|(v, _)| *v > since)
                .map(|(_, id)| *id)
                .collect();
        }
        poll
    }

    // ------------------------------------------------------------- workers
    async fn next_job(&self) -> Option<Job> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut inner = self.inner.lock().unwrap();
                if inner.workers > self.max_workers() {
                    inner.workers -= 1;
                    return None;
                }
                if inner.running {
                    while let Some(id) = inner.pending.pop_front() {
                        let v = inner.bump();
                        let Some(it) = inner.items.get_mut(&id) else {
                            continue;
                        };
                        if it.status != TransferStatus::Queued {
                            continue;
                        }
                        it.status = TransferStatus::Active;
                        it.rev = v;
                        it.progress.reset();
                        return Some(Job {
                            id,
                            direction: it.direction,
                            local: it.local.clone(),
                            remote: it.remote.clone(),
                            session: it.session.clone(),
                            force: it.force,
                            progress: it.progress.clone(),
                            protocol: String::new(),
                            begun: None,
                        });
                    }
                }
                if inner.pending.is_empty()
                    && inner
                        .items
                        .values()
                        .all(|i| i.status != TransferStatus::Active)
                {
                    inner.overwrite_all = None; // la file est vide : on oublie « appliquer à tous »
                    self.pool.lock().unwrap().clear(); // et on libère les canaux SFTP
                }
            }
            let _ = tokio::time::timeout(Duration::from_secs(2), notified).await;
        }
    }

    async fn worker(self: Arc<Self>) {
        while let Some(mut job) = self.next_job().await {
            let started = Instant::now();
            let res = self.process(&mut job).await;
            let (status, error) = match res {
                Ok(true) => (TransferStatus::Done, String::new()),
                Ok(false) => (TransferStatus::Skipped, String::new()),
                Err(_) if job.cancelled() => (TransferStatus::Cancelled, String::new()),
                Err(Error::TransferCancelled) => (TransferStatus::Cancelled, String::new()),
                Err(e) => {
                    self.pool.lock().unwrap().remove(&job.session.id);
                    (TransferStatus::Failed, e.to_string())
                }
            };
            let (event, name, size) = {
                let mut inner = self.inner.lock().unwrap();
                let Some(it) = inner.items.get_mut(&job.id) else {
                    inner.bump();
                    continue;
                };
                it.status = status;
                it.error = error.clone();
                it.local = job.local.clone();
                it.remote = job.remote.clone();
                it.protocol = job.protocol.clone();
                if status == TransferStatus::Done {
                    let size = it.progress.size.load(Ordering::Relaxed);
                    it.progress.transferred.fetch_max(size, Ordering::Relaxed);
                }
                let event = TransferFinished {
                    id: it.id,
                    session_id: it.session.id,
                    direction: it.direction,
                    local: ui_path(&it.local),
                    remote: it.remote.clone(),
                    status,
                    tag: it.tag.clone(),
                };
                let (name, size) = (it.name(), it.progress.size.load(Ordering::Relaxed));
                inner.touch(job.id);
                (event, name, size)
            };
            self.notify.notify_waiters();
            match status {
                TransferStatus::Done => {
                    let elapsed = job
                        .begun
                        .unwrap_or(started)
                        .elapsed()
                        .as_secs_f64()
                        .max(1e-6);
                    let arrow = if job.direction == Direction::Up {
                        "↑"
                    } else {
                        "↓"
                    };
                    self.ui.log(
                        "status",
                        trf!(
                            "{arrow} {name} : {} octets en {elapsed:.1} s ({:.1} Mio/s, {})",
                            "{arrow} {name}: {} bytes in {elapsed:.1} s ({:.1} MiB/s, {})",
                            fmt_int(size),
                            size as f64 / elapsed / 1048576.0,
                            job.protocol.to_uppercase()
                        ),
                    );
                    if event.tag == "edit" {
                        self.edits.opened(job.session.id, &job.local, &job.remote);
                    }
                }
                TransferStatus::Failed => self.ui.log(
                    "error",
                    trf!(
                        "Échec du transfert de {name} : {error}",
                        "Transfer of {name} failed: {error}"
                    ),
                ),
                _ => {}
            }
            self.ui.transfer_finished(event);
        }
    }

    fn use_scp(session: &Session) -> bool {
        session.site().protocol == PROTO_SCP && session.shell_ok() != Some(false)
    }

    async fn sftp_for(&self, session: &Session) -> Result<Arc<Sftp>> {
        let conn = session.ensure_connected().await?;
        let reuse = {
            let mut pool = self.pool.lock().unwrap();
            let entry = pool
                .entry(session.id)
                .or_insert((conn.generation, Vec::new(), 0));
            if entry.0 != conn.generation {
                *entry = (conn.generation, Vec::new(), 0);
            }
            entry.1.retain(|s| !s.is_closed());
            if entry.1.len() >= SFTP_POOL {
                entry.2 = (entry.2 + 1) % entry.1.len();
                Some(entry.1[entry.2].clone())
            } else {
                None
            }
        };
        if let Some(s) = reuse {
            return Ok(s);
        }
        match conn.open_sftp().await {
            Ok(s) => {
                let s = Arc::new(s);
                let mut pool = self.pool.lock().unwrap();
                let entry = pool
                    .entry(session.id)
                    .or_insert((conn.generation, Vec::new(), 0));
                if entry.0 == conn.generation {
                    entry.1.push(s.clone());
                }
                Ok(s)
            }
            Err(e) => {
                // canal refusé (trop de canaux ouverts ?) : on partage un canal existant
                let pool = self.pool.lock().unwrap();
                match pool
                    .get(&session.id)
                    .and_then(|(_, v, _)| v.first().cloned())
                {
                    Some(s) => Ok(s),
                    None => Err(e),
                }
            }
        }
    }

    /// Ok(true) : transféré ; Ok(false) : ignoré (politique d'écrasement).
    async fn process(&self, job: &mut Job) -> Result<bool> {
        let session = job.session.clone();
        session.ensure_ready().await?;
        let preserve = self.settings.read().unwrap().preserve_times;
        let progress = job.progress.clone();
        let on_progress = |n: u64| progress.add(n);
        let cancel_flag = job.progress.clone();
        let cancelled = move || cancel_flag.cancel.load(Ordering::Relaxed);
        match job.direction {
            Direction::Down => {
                if !job.force
                    && let Ok(st) = fs::metadata(&job.local)
                {
                    let Some(remote) = session.stat(&job.remote).await? else {
                        return Err(Error::Remote(trf!(
                            "{} : fichier introuvable",
                            "{}: file not found",
                            job.remote
                        )));
                    };
                    if remote.is_dir {
                        return Err(Error::Remote(trf!(
                            "{} est un dossier",
                            "{} is a directory",
                            job.remote
                        )));
                    }
                    job.progress.size.store(remote.size, Ordering::Relaxed);
                    if !self
                        .overwrite_ok(
                            job,
                            remote.size,
                            remote.mtime,
                            st.len(),
                            platform::mtime(&st),
                        )
                        .await?
                    {
                        return Ok(false);
                    }
                }
                if let Some(parent) = job.local.parent() {
                    fs::create_dir_all(parent)?;
                }
                job.progress.reset();
                job.begun = Some(Instant::now());
                if let Some(ftp) = session.ftp().cloned() {
                    job.protocol = session.site().protocol;
                    ftp_download(
                        &ftp,
                        &session,
                        &job.remote,
                        &job.local,
                        &job.progress,
                        &on_progress,
                        &cancelled,
                        preserve,
                    )
                    .await?;
                } else if Self::use_scp(&session) {
                    job.protocol = "scp".into();
                    let chan = session.open_channel().await?;
                    let size = scp::download(
                        chan,
                        &job.remote,
                        &job.local,
                        &on_progress,
                        &cancelled,
                        preserve,
                    )
                    .await?;
                    job.progress.size.store(size, Ordering::Relaxed);
                } else {
                    job.protocol = "sftp".into();
                    let s = self.sftp_for(&session).await?;
                    sftp_download(
                        &s,
                        &job.remote,
                        &job.local,
                        &job.progress,
                        &on_progress,
                        &cancelled,
                        preserve,
                    )
                    .await?;
                }
            }
            Direction::Up => {
                let st = fs::metadata(&job.local)?;
                if st.is_dir() {
                    return Err(Error::Io(trf!(
                        "{} est un dossier",
                        "{} is a directory",
                        job.local.display()
                    )));
                }
                job.progress.size.store(st.len(), Ordering::Relaxed);
                if let Some(remote) = session.stat(&job.remote).await? {
                    if remote.is_dir {
                        return Err(Error::Remote(trf!(
                            "{} est un dossier",
                            "{} is a directory",
                            job.remote
                        )));
                    }
                    if !job.force
                        && !self
                            .overwrite_ok(
                                job,
                                st.len(),
                                platform::mtime(&st),
                                remote.size,
                                remote.mtime,
                            )
                            .await?
                    {
                        return Ok(false);
                    }
                }
                job.progress.reset();
                job.begun = Some(Instant::now());
                if let Some(ftp) = session.ftp().cloned() {
                    job.protocol = session.site().protocol;
                    let mut fh = File::open(&job.local)?;
                    let mtime = preserve.then(|| platform::mtime(&st));
                    ftp.upload(
                        &session,
                        &mut fh,
                        &job.remote,
                        mtime,
                        &on_progress,
                        &cancelled,
                    )
                    .await?;
                } else if Self::use_scp(&session) {
                    job.protocol = "scp".into();
                    let chan = session.open_channel().await?;
                    scp::upload(
                        chan,
                        &job.local,
                        &job.remote,
                        &on_progress,
                        &cancelled,
                        preserve,
                    )
                    .await?;
                } else {
                    job.protocol = "sftp".into();
                    let s = self.sftp_for(&session).await?;
                    let fh = File::open(&job.local)?;
                    s.upload(&fh, st.len(), &job.remote, &on_progress, &cancelled)
                        .await?;
                    if cfg!(unix) {
                        let _ = s.chmod(&job.remote, platform::mode(&st)).await;
                    }
                    if preserve {
                        let _ = s
                            .set_times(&job.remote, platform::atime(&st), platform::mtime(&st))
                            .await;
                    }
                }
            }
        }
        if job.cancelled() {
            return Err(Error::TransferCancelled);
        }
        Ok(true)
    }

    /// Applique la politique d'écrasement ; peut modifier la cible (renommage).
    async fn overwrite_ok(
        &self,
        job: &mut Job,
        src_size: u64,
        src_mtime: i64,
        dst_size: u64,
        dst_mtime: i64,
    ) -> Result<bool> {
        let action = {
            let _guard = self.overwrite_lock.lock().await; // une seule boîte de dialogue à la fois
            let preset = {
                let inner = self.inner.lock().unwrap();
                inner.overwrite_all.clone()
            };
            let action =
                preset.unwrap_or_else(|| self.settings.read().unwrap().overwrite_default.clone());
            if action == "ask" {
                let (source, target) = match job.direction {
                    Direction::Up => (job.local.to_string_lossy().into_owned(), job.remote.clone()),
                    Direction::Down => {
                        (job.remote.clone(), job.local.to_string_lossy().into_owned())
                    }
                };
                let reply = self
                    .ui
                    .ask(PromptRequest::Overwrite {
                        direction: job.direction,
                        source,
                        target,
                        src_size,
                        src_mtime,
                        dst_size,
                        dst_mtime,
                    })
                    .await;
                match reply {
                    PromptReply::Overwrite { action, apply_all } => {
                        if apply_all {
                            self.inner.lock().unwrap().overwrite_all = Some(action.clone());
                        }
                        action
                    }
                    _ => return Err(Error::TransferCancelled),
                }
            } else {
                action
            }
        };
        Ok(match action.as_str() {
            "overwrite" => true,
            "newer" => src_mtime > dst_mtime + 1,
            "size" => src_size != dst_size,
            "newer_or_size" => src_mtime > dst_mtime + 1 || src_size != dst_size,
            "rename" => {
                self.rename_target(job).await?;
                true
            }
            _ => false,
        })
    }

    async fn rename_target(&self, job: &mut Job) -> Result<()> {
        fn split_ext(name: &str) -> (&str, &str) {
            match name.rfind('.') {
                Some(i) if i > 0 => (&name[..i], &name[i..]),
                _ => (name, ""),
            }
        }
        match job.direction {
            Direction::Down => {
                let name = job
                    .local
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let (base, ext) = split_ext(&name);
                let dir = job
                    .local
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                let mut n = 1;
                while dir.join(format!("{base} ({n}){ext}")).exists() {
                    n += 1;
                }
                job.local = dir.join(format!("{base} ({n}){ext}"));
            }
            Direction::Up => {
                let name = rustscp_shared::util::rbasename(&job.remote).to_string();
                let (base, ext) = split_ext(&name);
                let dir = rparent(&job.remote);
                let mut n = 1;
                while job
                    .session
                    .stat(&rjoin(&dir, &format!("{base} ({n}){ext}")))
                    .await?
                    .is_some()
                {
                    n += 1;
                }
                job.remote = rjoin(&dir, &format!("{base} ({n}){ext}"));
            }
        }
        Ok(())
    }
}

async fn sftp_download(
    s: &Sftp,
    remote: &str,
    local: &Path,
    counters: &Progress,
    progress: &(dyn Fn(u64) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
    preserve: bool,
) -> Result<()> {
    let mut part = local.as_os_str().to_owned();
    part.push(".rustscp-part");
    let part = PathBuf::from(part);
    let res = async {
        let fh = File::create(&part)?;
        let (size, mtime) = s
            .download(
                remote,
                &fh,
                &|size| counters.size.store(size, Ordering::Relaxed),
                progress,
                cancelled,
            )
            .await?;
        counters.size.store(size, Ordering::Relaxed);
        if preserve && mtime > 0 {
            let t = UNIX_EPOCH + Duration::from_secs(mtime as u64);
            let _ = fh.set_times(
                FileTimes::new()
                    .set_modified(t)
                    .set_accessed(SystemTime::now()),
            );
        }
        drop(fh);
        fs::rename(&part, local)?;
        Ok(())
    }
    .await;
    if res.is_err() {
        let _ = fs::remove_file(&part);
    }
    res
}

#[allow(clippy::too_many_arguments)]
async fn ftp_download(
    ftp: &Ftp,
    session: &Session,
    remote: &str,
    local: &Path,
    counters: &Progress,
    progress: &(dyn Fn(u64) + Sync),
    cancelled: &(dyn Fn() -> bool + Sync),
    preserve: bool,
) -> Result<()> {
    let mut part = local.as_os_str().to_owned();
    part.push(".rustscp-part");
    let part = PathBuf::from(part);
    let res = async {
        let mut fh = File::create(&part)?;
        let (size, mtime) = ftp
            .download(
                session,
                remote,
                &mut fh,
                &|size| counters.size.store(size, Ordering::Relaxed),
                progress,
                cancelled,
            )
            .await?;
        counters.size.store(size, Ordering::Relaxed);
        if preserve && mtime > 0 {
            let t = UNIX_EPOCH + Duration::from_secs(mtime as u64);
            let _ = fh.set_times(
                FileTimes::new()
                    .set_modified(t)
                    .set_accessed(SystemTime::now()),
            );
        }
        drop(fh);
        fs::rename(&part, local)?;
        Ok(())
    }
    .await;
    if res.is_err() {
        let _ = fs::remove_file(&part);
    }
    res
}

// ------------------------------------------------------------- expansion
/// Transforme fichiers/dossiers locaux en éléments de file ; crée les dossiers distants.
pub async fn expand_upload(
    session: &Arc<Session>,
    local_paths: &[String],
    remote_dir: &str,
) -> Result<Vec<NewItem>> {
    let mut items = Vec::new();
    for path in local_paths {
        let path = native_path(path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let target = rjoin(remote_dir, &name);
        let lmeta = fs::symlink_metadata(&path)?;
        if lmeta.is_dir() {
            session.mkdir(&target, true).await?;
            let mut stack = vec![(path.clone(), target.clone())];
            while let Some((dir, rdir)) = stack.pop() {
                let mut children: Vec<_> = fs::read_dir(&dir)?.filter_map(|e| e.ok()).collect();
                children.sort_by_key(|e| e.file_name());
                let mut subdirs = Vec::new();
                for child in children {
                    let cname = child.file_name().to_string_lossy().into_owned();
                    let cpath = child.path();
                    let Ok(meta) = fs::metadata(&cpath) else {
                        continue;
                    };
                    let is_link_dir =
                        child.file_type().map(|t| t.is_symlink()).unwrap_or(false) && meta.is_dir();
                    if meta.is_dir() && !is_link_dir {
                        let rsub = rjoin(&rdir, &cname);
                        session.mkdir(&rsub, true).await?;
                        subdirs.push((cpath, rsub));
                    } else if meta.is_file() {
                        items.push(NewItem::new(
                            Direction::Up,
                            cpath,
                            rjoin(&rdir, &cname),
                            meta.len(),
                            session.clone(),
                        ));
                    }
                }
                subdirs.reverse();
                stack.extend(subdirs);
            }
        } else if let Ok(meta) = fs::metadata(&path)
            && meta.is_file()
        {
            items.push(NewItem::new(
                Direction::Up,
                path,
                target,
                meta.len(),
                session.clone(),
            ));
        }
    }
    Ok(items)
}

pub async fn expand_download(
    session: &Arc<Session>,
    remote: &[RemoteItem],
    local_dir: &Path,
) -> Result<Vec<NewItem>> {
    let mut items = Vec::new();
    for r in remote {
        let name = rustscp_shared::util::rbasename(&r.path).to_string();
        let target = local_dir.join(&name);
        if r.is_dir {
            fs::create_dir_all(&target)?;
            for (rel, entry) in session.walk_files(&r.path, &|| false).await? {
                let local = if rel.is_empty() {
                    target.clone()
                } else {
                    target.join(&rel)
                };
                match entry {
                    None => fs::create_dir_all(&local)?,
                    Some(e) => items.push(NewItem::new(
                        Direction::Down,
                        local,
                        rjoin(&r.path, &rel),
                        e.size,
                        session.clone(),
                    )),
                }
            }
        } else {
            items.push(NewItem::new(
                Direction::Down,
                target,
                r.path.clone(),
                r.size,
                session.clone(),
            ));
        }
    }
    Ok(items)
}

// --------------------------------------------------------- édition distante
/// Fichiers téléchargés pour édition : ouverts avec l'application par défaut,
/// leurs modifications sont signalées à l'interface (qui propose le renvoi).
pub struct Edits {
    files: Mutex<HashMap<PathBuf, (u64, String, SystemTime)>>,
    ui: Arc<dyn Ui>,
    started: AtomicBool,
}

impl Edits {
    fn new(ui: Arc<dyn Ui>) -> Arc<Self> {
        Arc::new(Edits {
            files: Mutex::new(HashMap::new()),
            ui,
            started: AtomicBool::new(false),
        })
    }

    pub fn temp_path(session_id: u64, remote: &str) -> PathBuf {
        let uid = platform::user_tag();
        let parent = rparent(remote).trim_matches('/').replace('/', "_");
        let sub = if parent.is_empty() {
            "_root".to_string()
        } else {
            parent
        };
        std::env::temp_dir()
            .join(format!("rustscp-{uid}"))
            .join(format!("s{session_id}"))
            .join(sub)
            .join(rustscp_shared::util::rbasename(remote))
    }

    pub fn prepare_dir(path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            let mut p = parent.to_path_buf();
            let root = std::env::temp_dir();
            while p != root && p.starts_with(&root) {
                platform::set_mode(&p, 0o700);
                if !p.pop() {
                    break;
                }
            }
        }
        Ok(())
    }

    fn opened(self: &Arc<Self>, session_id: u64, local: &Path, remote: &str) {
        let mtime = fs::metadata(local)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        self.files
            .lock()
            .unwrap()
            .insert(local.to_path_buf(), (session_id, remote.to_string(), mtime));
        crate::local::open_with_default(local);
        self.ui.log(
            "status",
            trf!(
                "Fichier ouvert pour édition : {} (les modifications seront proposées à l'envoi)",
                "File opened for editing: {} (changes will be offered for upload)",
                local.display()
            ),
        );
        if !self.started.swap(true, Ordering::SeqCst) {
            let me = self.clone();
            spawn(async move { me.watch().await });
        }
    }

    async fn watch(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut changed = Vec::new();
            {
                let mut files = self.files.lock().unwrap();
                for (path, (session_id, remote, mtime)) in files.iter_mut() {
                    // certains éditeurs remplacent le fichier : s'il manque, on attend qu'il réapparaisse
                    let Ok(m) = fs::metadata(path).and_then(|m| m.modified()) else {
                        continue;
                    };
                    if m != *mtime {
                        *mtime = m;
                        changed.push(EditChanged {
                            session_id: *session_id,
                            local: ui_path(path),
                            remote: remote.clone(),
                        });
                    }
                }
            }
            for ev in changed {
                self.ui.edit_changed(ev);
            }
        }
    }

    pub fn forget_session(&self, session_id: u64) {
        self.files
            .lock()
            .unwrap()
            .retain(|_, (s, _, _)| *s != session_id);
    }
}
