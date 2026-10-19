//! Passerelle de développement : expose le cœur de RustSCP en HTTP pour tester
//! l'interface dans un navigateur (`trunk serve`, port 1420) sans fenêtre Tauri.
//!
//!     cargo run -p rustscp-devbridge
//!
//! POST /invoke/<commande> (corps JSON) → résultat JSON ; GET /events?since=N.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rustscp_lib::commands::AppState;
use rustscp_lib::error::CmdError;
use rustscp_lib::platform::{is_virtual_root, native_path, ui_path};
use rustscp_lib::transfer::{Edits, NewItem, expand_download, expand_upload};
use rustscp_lib::ui::{BoxFut, PromptHub, Ui, now_hms};
use rustscp_lib::{config, local, sites, ssh, totp};
use rustscp_shared::util::{rbasename, rjoin};
use rustscp_shared::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

struct BridgeUi {
    hub: Arc<PromptHub>,
    events: Mutex<VecDeque<(u64, String, Value)>>,
    seq: AtomicU64,
}

impl BridgeUi {
    fn push(&self, name: &str, payload: impl Serialize) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut ev = self.events.lock().unwrap();
        ev.push_back((
            seq,
            name.to_string(),
            serde_json::to_value(payload).unwrap_or(Value::Null),
        ));
        while ev.len() > 5000 {
            ev.pop_front();
        }
    }
}

impl Ui for BridgeUi {
    fn log(&self, level: &str, message: String) {
        eprintln!("[{level}] {message}");
        self.push(
            "log",
            LogLine {
                time: now_hms(),
                level: level.into(),
                message,
            },
        );
    }
    fn ask(&self, request: PromptRequest) -> BoxFut<'_, PromptReply> {
        Box::pin(self.hub.ask(request, |ev| {
            self.push("prompt", ev);
            true
        }))
    }
    fn transfer_finished(&self, event: TransferFinished) {
        self.push("transfer-finished", event);
    }
    fn edit_changed(&self, event: EditChanged) {
        self.push("edit-changed", event);
    }
}

fn de<T: DeserializeOwned>(v: &Value, key: &str) -> Result<T, CmdError> {
    serde_json::from_value(v.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|e| CmdError::from(format!("argument « {key} » : {e}")))
}

async fn dispatch(st: &AppState, cmd: &str, a: Value) -> Result<Value, CmdError> {
    let id = || a["id"].as_u64().unwrap_or(0);
    let s = |k: &str| a[k].as_str().unwrap_or_default().to_string();
    let ok = |v: ()| Ok::<Value, CmdError>(json!(v));
    match cmd {
        "app_info" => Ok(json!({
            "name": APP_NAME, "version": VERSION,
            "secrets": { "available": st.secrets.available, "name": st.secrets.backend_name() },
            "default_key": local::default_key(), "home": ui_path(&local::home()),
            "user": std::env::var("USER").unwrap_or_default(),
            "lang": rustscp_shared::lang().code()
        })),
        "settings_get" => Ok(json!(*st.settings.read().unwrap())),
        "settings_set" => {
            let settings: Settings = de(&a, "settings")?;
            config::save_settings(&settings)?;
            config::apply_language(&settings);
            *st.settings.write().unwrap() = settings;
            st.transfers.ensure_workers();
            ok(())
        }
        "quit" | "copy_text" => ok(()),
        "pick_file" | "pick_dir" => Ok(Value::Null),
        "prompt_reply" => {
            st.hub.reply(id(), de(&a, "reply")?);
            ok(())
        }
        "sites_get" => Ok(json!(sites::load_sites())),
        "sites_set" => ok(sites::save_sites(&de(&a, "file")?)?),
        "secret_get" => Ok(json!(st.secrets.get(&s("site_id"), &s("kind")))),
        "secret_set" => Ok(json!(st.secrets.set(
            &s("site_id"),
            &s("kind"),
            &s("value")
        ))),
        "secrets_copy" => {
            st.secrets.copy_site(&s("src"), &s("dst"));
            ok(())
        }
        "secrets_delete" => {
            st.secrets.delete_site(&s("site_id"));
            ok(())
        }
        "totp_preview" => totp::current(&util::parse_otp_secret(&s("secret")))
            .map(|(code, left)| json!(TotpPreview { code, left }))
            .map_err(CmdError::from),
        "session_create" => {
            let timeout = st.settings.read().unwrap().connect_timeout;
            let session =
                ssh::Session::new(de(&a, "site")?, st.ui.clone(), st.secrets.clone(), timeout);
            let sid = session.id;
            st.sessions.lock().unwrap().insert(sid, session);
            Ok(json!(sid))
        }
        "session_connect" | "session_info" => {
            let sess = st.session(id())?;
            if cmd == "session_connect" {
                sess.connect().await?;
            }
            Ok(json!(SessionInfo {
                session_id: sess.id,
                home: sess.home(),
                connected: sess.is_connected(),
                shell_ok: sess.shell_ok(),
                sftp: true
            }))
        }
        "session_close" => {
            st.transfers.cancel_session(id());
            if let Some(sess) = st.sessions.lock().unwrap().remove(&id()) {
                tokio::spawn(async move { sess.close().await });
            }
            ok(())
        }
        "session_busy" => Ok(json!(st.transfers.session_busy(id()))),
        "remote_list" => Ok(json!(st.session(id())?.listdir(&s("path")).await?)),
        "remote_mkdir" => ok(st.session(id())?.mkdir(&s("path"), false).await?),
        "remote_touch" => ok(st.session(id())?.touch(&s("path")).await?),
        "remote_rename" => ok(st.session(id())?.rename(&s("src"), &s("dst")).await?),
        "remote_move" => {
            let sess = st.session(id())?;
            for it in de::<Vec<RemoteItem>>(&a, "items")? {
                sess.rename(&it.path, &rjoin(&s("target"), rbasename(&it.path)))
                    .await?;
            }
            ok(())
        }
        "remote_delete" => {
            let sess = st.session(id())?;
            for it in de::<Vec<RemoteItem>>(&a, "items")? {
                sess.remove(&it.path, it.is_dir).await?;
            }
            ok(())
        }
        "remote_chmod" => {
            let sess = st.session(id())?;
            let (mode, rec): (u32, bool) = (de(&a, "mode")?, de(&a, "recursive")?);
            for it in de::<Vec<RemoteItem>>(&a, "items")? {
                sess.chmod(&it.path, mode, rec && it.is_dir).await?;
            }
            ok(())
        }
        "queue_upload" | "queue_download" => {
            let sess = st.session(id())?;
            let items = if cmd == "queue_upload" {
                expand_upload(&sess, &de::<Vec<String>>(&a, "paths")?, &s("remote_dir")).await?
            } else {
                expand_download(
                    &sess,
                    &de::<Vec<RemoteItem>>(&a, "items")?,
                    &native_path(&s("local_dir")),
                )
                .await?
            };
            let res = EnqueueResult {
                count: items.len() as u64,
                total: items.iter().map(|i| i.size).sum(),
            };
            st.transfers.add(items);
            Ok(json!(res))
        }
        "edit_remote" => {
            let sess = st.session(id())?;
            let local = Edits::temp_path(id(), &s("path"));
            Edits::prepare_dir(&local)?;
            let mut item = NewItem::new(
                Direction::Down,
                local,
                s("path"),
                a["size"].as_u64().unwrap_or(0),
                sess,
            );
            item.tag = if a["text"].as_bool().unwrap_or(false) {
                "edit"
            } else {
                "open"
            }
            .into();
            st.transfers.add(vec![item]);
            ok(())
        }
        "reupload" => {
            let sess = st.session(id())?;
            let local = native_path(&s("local"));
            let size = std::fs::metadata(&local)?.len();
            let mut item = NewItem::new(Direction::Up, local, s("remote"), size, sess);
            item.force = true;
            st.transfers.add(vec![item]);
            ok(())
        }
        "transfer_poll" => Ok(json!(st.transfers.poll(a["since"].as_u64().unwrap_or(0)))),
        "transfer_cancel" => {
            st.transfers.cancel(&de::<Vec<u64>>(&a, "ids")?);
            ok(())
        }
        "transfer_cancel_all" => {
            st.transfers.cancel_all();
            ok(())
        }
        "transfer_retry" => {
            let _: () = st.transfers.retry(&de::<Vec<u64>>(&a, "ids")?);
            ok(())
        }
        "transfer_remove" => {
            st.transfers.remove(&de::<Vec<u64>>(&a, "ids")?);
            ok(())
        }
        "transfer_clear" => {
            st.transfers
                .clear(a["group"].as_u64().unwrap_or(0) as usize);
            ok(())
        }
        "transfer_running" => {
            let _: () = st.transfers.set_running(de(&a, "running")?);
            ok(())
        }
        "local_list" => Ok(json!(local::list(&s("path"))?)),
        "local_tree" => Ok(json!(local::tree_path(&s("path"), true))),
        "local_resolve" => {
            if is_virtual_root(s("path").trim()) {
                return Ok(json!("/"));
            }
            let p = native_path(s("path").trim());
            let p = if p.is_absolute() {
                p
            } else {
                local::home().join(p)
            };
            match std::fs::canonicalize(&p) {
                Ok(c) if c.is_dir() => Ok(json!(ui_path(&c))),
                _ => Err(CmdError::from(format!(
                    "Dossier introuvable : {}",
                    p.display()
                ))),
            }
        }
        "local_mkdir" => ok(std::fs::create_dir(native_path(&s("path")))?),
        "local_rename" => ok(std::fs::rename(
            native_path(&s("src")),
            native_path(&s("dst")),
        )?),
        "local_trash" => {
            let paths = de::<Vec<String>>(&a, "paths")?;
            trash::delete_all(paths.iter().map(|p| native_path(p)))
                .map_err(|e| CmdError::from(e.to_string()))?;
            ok(())
        }
        "local_open" => {
            local::open_with_default(&native_path(&s("path")));
            ok(())
        }
        other => Err(CmdError::from(format!("commande inconnue : {other}"))),
    }
}

fn respond(req: tiny_http::Request, code: u16, body: String) {
    let cors = tiny_http::Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap();
    let ct = tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap();
    let _ = req.respond(
        tiny_http::Response::from_string(body)
            .with_status_code(code)
            .with_header(cors)
            .with_header(ct),
    );
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let _enter = rt.enter();
    let hub = Arc::new(PromptHub::default());
    let ui = Arc::new(BridgeUi {
        hub: hub.clone(),
        events: Mutex::new(VecDeque::new()),
        seq: AtomicU64::new(0),
    });
    // RUSTSCP_MEMORY_SECRETS=1 : secrets en mémoire (pas d'écriture dans le trousseau)
    let secrets = if std::env::var_os("RUSTSCP_MEMORY_SECRETS").is_some() {
        sites::SecretStore::in_memory()
    } else {
        sites::SecretStore::new()
    };
    let state = Arc::new(AppState::new(ui.clone(), hub, Arc::new(secrets)));
    let server = tiny_http::Server::http("127.0.0.1:1421").expect("port 1421");
    eprintln!("Passerelle RustSCP sur http://127.0.0.1:1421");
    for mut req in server.incoming_requests() {
        let url = req.url().to_string();
        if let Some(q) = url.strip_prefix("/events") {
            let since: u64 = q
                .split("since=")
                .nth(1)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let list: Vec<Value> = ui
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|(s, _, _)| *s > since)
                .map(|(s, n, p)| json!({ "seq": s, "event": n, "payload": p }))
                .collect();
            respond(req, 200, Value::Array(list).to_string());
            continue;
        }
        let Some(cmd) = url.strip_prefix("/invoke/").map(str::to_string) else {
            respond(req, 404, "null".into());
            continue;
        };
        let mut body = String::new();
        let _ = req.as_reader().read_to_string(&mut body);
        let args: Value = serde_json::from_str(&body).unwrap_or(json!({}));
        let state = state.clone();
        rt.spawn(async move {
            match dispatch(&state, &cmd, args).await {
                Ok(v) => respond(req, 200, v.to_string()),
                Err(e) => respond(req, 400, serde_json::to_string(&e).unwrap()),
            }
        });
    }
}
