//! Commandes appelées par l'interface (invoke).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use rustscp_shared::util::{rbasename, rjoin};
use rustscp_shared::{
    Direction, EnqueueResult, Entry, PromptReply, RemoteItem, SecretBackend, SessionInfo, Settings,
    Site, SitesFile, TotpPreview, TransferPoll,
};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::error::CmdError;
use crate::platform::{native_path, ui_path};
use crate::sites::SecretStore;
use crate::ssh::Session;
use crate::transfer::{Edits, NewItem, TransferManager, expand_download, expand_upload};
use crate::ui::{PromptHub, Ui};
use crate::{config, local, sites, totp};
use rustscp_shared::{tr, trf};

type CmdResult<T> = Result<T, CmdError>;

pub struct AppState {
    pub ui: Arc<dyn Ui>,
    pub hub: Arc<PromptHub>,
    pub settings: Arc<RwLock<Settings>>,
    pub secrets: Arc<SecretStore>,
    pub sessions: Mutex<HashMap<u64, Arc<Session>>>,
    pub transfers: Arc<TransferManager>,
}

impl AppState {
    pub fn new(ui: Arc<dyn Ui>, hub: Arc<PromptHub>, secrets: Arc<SecretStore>) -> Self {
        let loaded = config::load_settings();
        config::apply_language(&loaded);
        let settings = Arc::new(RwLock::new(loaded));
        let transfers = TransferManager::new(settings.clone(), ui.clone());
        AppState {
            ui,
            hub,
            settings,
            secrets,
            sessions: Mutex::new(HashMap::new()),
            transfers,
        }
    }

    pub fn session(&self, id: u64) -> CmdResult<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| CmdError {
                kind: "closed",
                message: tr!("Session fermée", "Session closed").into(),
            })
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CmdError::from(e.to_string()))
}

// ------------------------------------------------------------------ général
#[derive(Serialize)]
pub struct AppInfo {
    name: &'static str,
    version: &'static str,
    secrets: SecretBackend,
    default_key: String,
    home: String,
    user: String,
    /// Langue effective (fr | en).
    lang: &'static str,
}

#[tauri::command(rename_all = "snake_case")]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    if std::env::var_os("RUSTSCP_DEBUG").is_some() {
        eprintln!("RustSCP : interface chargée (IPC opérationnel)");
    }
    AppInfo {
        name: rustscp_shared::APP_NAME,
        version: rustscp_shared::VERSION,
        secrets: SecretBackend {
            available: state.secrets.available,
            name: state.secrets.backend_name().into(),
        },
        default_key: local::default_key(),
        home: ui_path(&local::home()),
        user: crate::platform::user_name(),
        lang: rustscp_shared::lang().code(),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn settings_get(state: State<'_, AppState>) -> Settings {
    state.settings.read().unwrap().clone()
}

#[tauri::command(rename_all = "snake_case")]
pub fn settings_set(state: State<'_, AppState>, settings: Settings) -> CmdResult<()> {
    config::save_settings(&settings)?;
    config::apply_language(&settings);
    *state.settings.write().unwrap() = settings;
    state.transfers.ensure_workers();
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn quit(app: AppHandle, state: State<'_, AppState>) {
    state.transfers.cancel_all();
    app.exit(0);
}

#[tauri::command(rename_all = "snake_case")]
pub fn copy_text(app: AppHandle, text: String) -> CmdResult<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard()
        .write_text(text)
        .map_err(|e| CmdError::from(e.to_string()))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn pick_file(app: AppHandle, start: String) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    blocking(move || {
        let mut d = app
            .dialog()
            .file()
            .set_title(tr!("Choisir une clé privée", "Choose a private key"));
        let dir = native_path(&start);
        if dir.is_dir() {
            d = d.set_directory(dir);
        }
        d.blocking_pick_file()
            .and_then(|p| p.into_path().ok())
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn pick_dir(app: AppHandle, start: String) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    blocking(move || {
        let mut d = app
            .dialog()
            .file()
            .set_title(tr!("Choisir un dossier", "Choose a folder"));
        let dir = native_path(&start);
        if dir.is_dir() {
            d = d.set_directory(dir);
        }
        d.blocking_pick_folder()
            .and_then(|p| p.into_path().ok())
            .map(|p| p.to_string_lossy().into_owned())
    })
    .await
}

#[tauri::command(rename_all = "snake_case")]
pub fn prompt_reply(state: State<'_, AppState>, id: u64, reply: PromptReply) {
    state.hub.reply(id, reply);
}

// -------------------------------------------------------------------- sites
#[tauri::command(rename_all = "snake_case")]
pub fn sites_get() -> SitesFile {
    sites::load_sites()
}

#[tauri::command(rename_all = "snake_case")]
pub fn sites_set(file: SitesFile) -> CmdResult<()> {
    Ok(sites::save_sites(&file)?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn secret_get(
    state: State<'_, AppState>,
    site_id: String,
    kind: String,
) -> CmdResult<String> {
    let secrets = state.secrets.clone();
    blocking(move || secrets.get(&site_id, &kind)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn secret_set(
    state: State<'_, AppState>,
    site_id: String,
    kind: String,
    value: String,
) -> CmdResult<bool> {
    let secrets = state.secrets.clone();
    blocking(move || secrets.set(&site_id, &kind, &value)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn secrets_copy(state: State<'_, AppState>, src: String, dst: String) -> CmdResult<()> {
    let secrets = state.secrets.clone();
    blocking(move || secrets.copy_site(&src, &dst)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn secrets_delete(state: State<'_, AppState>, site_id: String) -> CmdResult<()> {
    let secrets = state.secrets.clone();
    blocking(move || secrets.delete_site(&site_id)).await
}

#[tauri::command(rename_all = "snake_case")]
pub fn totp_preview(secret: String) -> Result<TotpPreview, String> {
    let secret = rustscp_shared::util::parse_otp_secret(&secret);
    totp::current(&secret).map(|(code, left)| TotpPreview { code, left })
}

// ----------------------------------------------------------------- sessions
fn info(s: &Session) -> SessionInfo {
    SessionInfo {
        session_id: s.id,
        home: s.home(),
        connected: s.is_connected(),
        shell_ok: s.shell_ok(),
        sftp: s.ftp().is_none(),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn session_create(state: State<'_, AppState>, site: Site) -> u64 {
    let timeout = state.settings.read().unwrap().connect_timeout;
    let session = Session::new(site, state.ui.clone(), state.secrets.clone(), timeout);
    let id = session.id;
    state.sessions.lock().unwrap().insert(id, session);
    id
}

#[tauri::command(rename_all = "snake_case")]
pub async fn session_connect(state: State<'_, AppState>, id: u64) -> CmdResult<SessionInfo> {
    let s = state.session(id)?;
    s.connect().await?;
    Ok(info(&s))
}

#[tauri::command(rename_all = "snake_case")]
pub fn session_info(state: State<'_, AppState>, id: u64) -> CmdResult<SessionInfo> {
    let s = state.session(id)?;
    Ok(info(&s))
}

#[tauri::command(rename_all = "snake_case")]
pub fn session_close(state: State<'_, AppState>, id: u64) {
    state.transfers.cancel_session(id);
    state.transfers.edits.forget_session(id);
    if let Some(s) = state.sessions.lock().unwrap().remove(&id) {
        tauri::async_runtime::spawn(async move { s.close().await });
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn session_busy(state: State<'_, AppState>, id: u64) -> bool {
    state.transfers.session_busy(id)
}

// ---------------------------------------------------------- fichiers distants
#[tauri::command(rename_all = "snake_case")]
pub async fn remote_list(
    state: State<'_, AppState>,
    id: u64,
    path: String,
) -> CmdResult<Vec<Entry>> {
    let s = state.session(id)?;
    Ok(s.listdir(&path).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn remote_mkdir(state: State<'_, AppState>, id: u64, path: String) -> CmdResult<()> {
    let s = state.session(id)?;
    Ok(s.mkdir(&path, false).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn remote_touch(state: State<'_, AppState>, id: u64, path: String) -> CmdResult<()> {
    let s = state.session(id)?;
    Ok(s.touch(&path).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn remote_rename(
    state: State<'_, AppState>,
    id: u64,
    src: String,
    dst: String,
) -> CmdResult<()> {
    let s = state.session(id)?;
    Ok(s.rename(&src, &dst).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn remote_move(
    state: State<'_, AppState>,
    id: u64,
    items: Vec<RemoteItem>,
    target: String,
) -> CmdResult<()> {
    let s = state.session(id)?;
    for it in items {
        s.rename(&it.path, &rjoin(&target, rbasename(&it.path)))
            .await?;
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn remote_delete(
    state: State<'_, AppState>,
    id: u64,
    items: Vec<RemoteItem>,
) -> CmdResult<()> {
    let s = state.session(id)?;
    for it in items {
        s.remove(&it.path, it.is_dir).await?;
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn remote_chmod(
    state: State<'_, AppState>,
    id: u64,
    items: Vec<RemoteItem>,
    mode: u32,
    recursive: bool,
) -> CmdResult<()> {
    let s = state.session(id)?;
    for it in items {
        s.chmod(&it.path, mode, recursive && it.is_dir).await?;
    }
    Ok(())
}

// --------------------------------------------------------------- transferts
fn summary(items: &[NewItem]) -> EnqueueResult {
    EnqueueResult {
        count: items.len() as u64,
        total: items.iter().map(|i| i.size).sum(),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn queue_upload(
    state: State<'_, AppState>,
    id: u64,
    paths: Vec<String>,
    remote_dir: String,
) -> CmdResult<EnqueueResult> {
    let s = state.session(id)?;
    let items = expand_upload(&s, &paths, &remote_dir).await?;
    let res = summary(&items);
    state.transfers.add(items);
    Ok(res)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn queue_download(
    state: State<'_, AppState>,
    id: u64,
    items: Vec<RemoteItem>,
    local_dir: String,
) -> CmdResult<EnqueueResult> {
    let s = state.session(id)?;
    let items = expand_download(&s, &items, &native_path(&local_dir)).await?;
    let res = summary(&items);
    state.transfers.add(items);
    Ok(res)
}

#[tauri::command(rename_all = "snake_case")]
pub fn edit_remote(
    state: State<'_, AppState>,
    id: u64,
    path: String,
    size: u64,
    text: bool,
) -> CmdResult<()> {
    let s = state.session(id)?;
    let local = Edits::temp_path(id, &path);
    Edits::prepare_dir(&local)?;
    let _ = std::fs::remove_file(&local);
    let mut item = NewItem::new(Direction::Down, local, path, size, s);
    item.tag = if text { "edit" } else { "open" }.into();
    state.transfers.add(vec![item]);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn reupload(
    state: State<'_, AppState>,
    id: u64,
    local: String,
    remote: String,
) -> CmdResult<()> {
    let s = state.session(id)?;
    let local = native_path(&local);
    let size = std::fs::metadata(&local)?.len();
    let mut item = NewItem::new(Direction::Up, local, remote, size, s);
    item.force = true;
    state.transfers.add(vec![item]);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn transfer_poll(state: State<'_, AppState>, since: u64) -> TransferPoll {
    state.transfers.poll(since)
}

#[tauri::command(rename_all = "snake_case")]
pub fn transfer_cancel(state: State<'_, AppState>, ids: Vec<u64>) {
    state.transfers.cancel(&ids);
}

#[tauri::command(rename_all = "snake_case")]
pub fn transfer_cancel_all(state: State<'_, AppState>) {
    state.transfers.cancel_all();
}

#[tauri::command(rename_all = "snake_case")]
pub fn transfer_retry(state: State<'_, AppState>, ids: Vec<u64>) {
    state.transfers.retry(&ids);
}

#[tauri::command(rename_all = "snake_case")]
pub fn transfer_remove(state: State<'_, AppState>, ids: Vec<u64>) {
    state.transfers.remove(&ids);
}

#[tauri::command(rename_all = "snake_case")]
pub fn transfer_clear(state: State<'_, AppState>, group: usize) {
    state.transfers.clear(group);
}

#[tauri::command(rename_all = "snake_case")]
pub fn transfer_running(state: State<'_, AppState>, running: bool) {
    state.transfers.set_running(running);
    state.ui.log(
        "status",
        if running {
            tr!("File de transferts reprise", "Transfer queue resumed").to_string()
        } else {
            tr!("File de transferts mise en pause", "Transfer queue paused").to_string()
        },
    );
}

// ------------------------------------------------------------ fichiers locaux
#[tauri::command(rename_all = "snake_case")]
pub async fn local_list(path: String) -> CmdResult<Vec<Entry>> {
    blocking(move || local::list(&path))
        .await?
        .map_err(Into::into)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn local_tree(path: String, show_hidden: bool) -> CmdResult<Vec<(String, Vec<String>)>> {
    blocking(move || local::tree_path(&path, show_hidden)).await
}

/// Normalise un chemin local ; erreur s'il ne s'agit pas d'un dossier.
#[tauri::command(rename_all = "snake_case")]
pub fn local_resolve(path: String) -> CmdResult<String> {
    let path = path.trim();
    if crate::platform::is_virtual_root(path) {
        return Ok("/".into());
    }
    let p = native_path(path);
    let p = if p.is_absolute() {
        p
    } else {
        local::home().join(p)
    };
    match std::fs::canonicalize(&p) {
        Ok(c) if c.is_dir() => Ok(ui_path(&c)),
        _ => Err(CmdError {
            kind: "io",
            message: trf!(
                "Dossier introuvable : {}",
                "Folder not found: {}",
                p.display()
            ),
        }),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn local_mkdir(path: String) -> CmdResult<()> {
    Ok(std::fs::create_dir(native_path(&path))?)
}

#[tauri::command(rename_all = "snake_case")]
pub fn local_rename(src: String, dst: String) -> CmdResult<()> {
    let (src, dst_path) = (native_path(&src), native_path(&dst));
    if dst_path.exists() {
        return Err(CmdError {
            kind: "io",
            message: trf!("{dst} existe déjà", "{dst} already exists"),
        });
    }
    Ok(std::fs::rename(src, dst_path)?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn local_trash(paths: Vec<String>) -> CmdResult<()> {
    let paths: Vec<_> = paths.iter().map(|p| native_path(p)).collect();
    blocking(move || {
        trash::delete_all(&paths).map_err(|e| CmdError::from(trf!("Corbeille : {e}", "Trash: {e}")))
    })
    .await?
}

#[tauri::command(rename_all = "snake_case")]
pub fn local_open(path: String) {
    local::open_with_default(&native_path(&path));
}
