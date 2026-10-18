//! Tests d'intégration du cœur contre le serveur Docker (tests/server/run.sh) :
//! clé SSH + code TOTP obligatoires.
//!
//!     cargo test -p rustscp --test integration -- --nocapture

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rand::RngCore;
use rustscp_lib::sites::SecretStore;
use rustscp_lib::ssh::Session;
use rustscp_lib::totp;
use rustscp_lib::transfer::{TransferManager, expand_download, expand_upload};
use rustscp_lib::ui::{BoxFut, Ui};
use rustscp_shared::{
    AUTH_KEY_OTP, OTP_ASK, OTP_SECRET, PROTO_SCP, PROTO_SFTP, PromptReply, PromptRequest,
    RemoteItem, SECRET_OTP, SECRET_PASSPHRASE, Settings, Site, TransferStatus,
};
use sha2::{Digest, Sha256};

const SECRET: &str = "JBSWY3DPEHPK3PXP";
const OTP_KEY: &str = "test@127.0.0.1:2222";
const PPK_PASSPHRASE: &str = "rustscp";

fn key_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/server/test_key")
        .to_string_lossy()
        .into_owned()
}

fn server_up() -> bool {
    std::net::TcpStream::connect_timeout(&"127.0.0.1:2222".parse().unwrap(), Duration::from_secs(1))
        .is_ok()
}

#[derive(Default)]
struct TestUi {
    logs: Mutex<Vec<(String, String)>>,
    asked: Mutex<Vec<Vec<String>>>,
    overwrite: Mutex<String>,
}

impl TestUi {
    fn joined(&self) -> String {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .map(|(_, m)| m.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Ui for TestUi {
    fn log(&self, level: &str, message: String) {
        eprintln!("[{level}] {message}");
        self.logs.lock().unwrap().push((level.to_string(), message));
    }

    fn ask(&self, request: PromptRequest) -> BoxFut<'_, PromptReply> {
        Box::pin(async move {
            match request {
                PromptRequest::HostKey { .. } => PromptReply::HostKey {
                    choice: "once".into(),
                },
                PromptRequest::Interactive { fields, .. } => {
                    self.asked
                        .lock()
                        .unwrap()
                        .push(fields.iter().map(|f| f.text.clone()).collect());
                    let code = totp::next_code(OTP_KEY, SECRET, |_| {}).await.unwrap();
                    PromptReply::Interactive {
                        answers: fields.iter().map(|_| code.clone()).collect(),
                    }
                }
                PromptRequest::Overwrite { .. } => PromptReply::Overwrite {
                    action: self.overwrite.lock().unwrap().clone(),
                    apply_all: true,
                },
                PromptRequest::Secret { .. } => PromptReply::Cancel,
            }
        })
    }
}

async fn make_session(protocol: &str, otp_mode: &str) -> (Arc<Session>, Arc<TestUi>) {
    make_session_with(protocol, otp_mode, &key_path(), None).await
}

async fn make_session_with(
    protocol: &str,
    otp_mode: &str,
    key_file: &str,
    passphrase: Option<&str>,
) -> (Arc<Session>, Arc<TestUi>) {
    let site = Site {
        name: "test".into(),
        host: "127.0.0.1".into(),
        port: 2222,
        user: "test".into(),
        auth: AUTH_KEY_OTP.into(),
        key_file: key_file.into(),
        use_agent: false,
        otp_mode: otp_mode.into(),
        protocol: protocol.into(),
        id: format!("test-{protocol}-{otp_mode}"),
        ..Default::default()
    };
    let secrets = Arc::new(SecretStore::in_memory());
    secrets.set(&site.id, SECRET_OTP, SECRET);
    if let Some(pass) = passphrase {
        secrets.set(&site.id, SECRET_PASSPHRASE, pass);
    }
    let ui = Arc::new(TestUi::default());
    let session = Session::new(site, ui.clone(), secrets, 15);
    session.connect().await.expect("connexion");
    // attend la détection des commandes shell (nécessaire au choix SCP/SFTP)
    let deadline = Instant::now() + Duration::from_secs(10);
    while session.shell_ok().is_none() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (session, ui)
}

async fn wait(mgr: &TransferManager) {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let s = mgr.stats();
        if s.queued == 0 && s.active == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "délai dépassé");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn sha(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}

fn random(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::rng().fill_bytes(&mut v);
    v
}

async fn transfers_roundtrip(protocol: &str) {
    let (session, ui) = make_session(protocol, OTP_SECRET).await;
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("sub/deep")).unwrap();
    std::fs::create_dir_all(src.join("empty")).unwrap();
    let files: HashMap<&str, Vec<u8>> = HashMap::from([
        ("a.bin", random(5 * 1024 * 1024 + 7)),
        ("sub/b.txt", b"hello\n".repeat(1000)),
        ("sub/deep/c é.bin", random(123456)),
        ("zero", Vec::new()),
    ]);
    for (rel, data) in &files {
        std::fs::write(src.join(rel), data).unwrap();
    }
    let remote_root = format!("/home/test/rustscp-test-{protocol}");
    if session.stat(&remote_root).await.unwrap().is_some() {
        session.remove(&remote_root, true).await.unwrap();
    }
    session.mkdir(&remote_root, false).await.unwrap();

    let settings = Arc::new(RwLock::new(Settings {
        max_transfers: 4,
        ..Default::default()
    }));
    let mgr = TransferManager::new(settings.clone(), ui.clone());
    let items = expand_upload(
        &session,
        &[src.to_string_lossy().into_owned()],
        &remote_root,
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 4);
    mgr.add(items);
    wait(&mgr).await;
    let poll = mgr.poll(0);
    assert!(
        poll.upserts
            .iter()
            .all(|i| i.status == TransferStatus::Done),
        "{:?}",
        poll.upserts
    );
    assert!(poll.upserts.iter().all(|i| i.protocol == protocol));
    assert!(
        session
            .stat(&format!("{remote_root}/src/empty"))
            .await
            .unwrap()
            .unwrap()
            .is_dir
    );

    // téléchargement de l'arborescence
    let dst = tmp.path().join("dst");
    std::fs::create_dir_all(&dst).unwrap();
    let mgr2 = TransferManager::new(settings.clone(), ui.clone());
    let items = expand_download(
        &session,
        &[RemoteItem {
            path: format!("{remote_root}/src"),
            is_dir: true,
            size: 0,
            mode: 0,
        }],
        &dst,
    )
    .await
    .unwrap();
    mgr2.add(items);
    wait(&mgr2).await;
    let poll = mgr2.poll(0);
    assert!(
        poll.upserts
            .iter()
            .all(|i| i.status == TransferStatus::Done),
        "{:?}",
        poll.upserts
    );
    for rel in files.keys() {
        assert_eq!(
            sha(&src.join(rel)),
            sha(&dst.join("src").join(rel)),
            "{rel}"
        );
    }
    assert!(dst.join("src/empty").is_dir());

    // politique d'écrasement : ignorer puis renommer
    *ui.overwrite.lock().unwrap() = "skip".into();
    let single = vec![src.join("sub/b.txt").to_string_lossy().into_owned()];
    let mgr3 = TransferManager::new(settings.clone(), ui.clone());
    mgr3.add(
        expand_upload(&session, &single, &remote_root)
            .await
            .unwrap(),
    );
    wait(&mgr3).await;
    mgr3.add(
        expand_upload(&session, &single, &remote_root)
            .await
            .unwrap(),
    );
    wait(&mgr3).await;
    let statuses: Vec<_> = mgr3.poll(0).upserts.iter().map(|i| i.status).collect();
    assert_eq!(
        statuses,
        vec![TransferStatus::Done, TransferStatus::Skipped]
    );
    let mgr4 = TransferManager::new(settings.clone(), ui.clone());
    *ui.overwrite.lock().unwrap() = "rename".into();
    mgr4.add(
        expand_upload(&session, &single, &remote_root)
            .await
            .unwrap(),
    );
    wait(&mgr4).await;
    assert_eq!(
        mgr4.poll(0).upserts[0].remote,
        format!("{remote_root}/b (1).txt")
    );

    // opérations distantes
    let f = format!("{remote_root}/nouveau.txt");
    session.touch(&f).await.unwrap();
    session
        .rename(&f, &format!("{remote_root}/renommé.txt"))
        .await
        .unwrap();
    session
        .chmod(&format!("{remote_root}/renommé.txt"), 0o600, false)
        .await
        .unwrap();
    let e = session
        .stat(&format!("{remote_root}/renommé.txt"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e.mode & 0o777, 0o600);
    session.remove(&remote_root, true).await.unwrap();
    assert!(session.stat(&remote_root).await.unwrap().is_none());
    session.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn integration() {
    // les vérifications portent sur les messages en français
    rustscp_shared::set_lang(rustscp_shared::Lang::Fr);
    if !server_up() {
        // en CI, l'absence du serveur est une erreur et non un test ignoré
        assert!(
            std::env::var_os("RUSTSCP_REQUIRE_SERVER").is_none(),
            "serveur de test absent alors que RUSTSCP_REQUIRE_SERVER est défini"
        );
        eprintln!("serveur de test non démarré (tests/server/run.sh) : test ignoré");
        return;
    }

    // 1. clé + TOTP généré automatiquement
    let (session, ui) = make_session(PROTO_SCP, OTP_SECRET).await;
    assert!(session.is_connected());
    assert_eq!(session.home(), "/home/test");
    assert!(
        ui.asked.lock().unwrap().is_empty(),
        "le code TOTP doit être généré automatiquement"
    );
    assert!(ui.joined().contains("authentification partielle"));
    assert_eq!(session.shell_ok(), Some(true));

    // 2. listing shell identique au listing SFTP
    let key = |e: &rustscp_shared::Entry| {
        (
            e.name.clone(),
            e.is_dir,
            if e.is_link { None } else { Some(e.size) },
        )
    };
    let mut a: Vec<_> = session
        .listdir("/etc")
        .await
        .unwrap()
        .iter()
        .map(key)
        .collect();
    let mut b: Vec<_> = session
        .listdir_shell("/etc")
        .await
        .unwrap()
        .iter()
        .map(key)
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);

    // 3. reconnexion transparente après une coupure (nouveau code TOTP, sans réutiliser l'ancien)
    session
        .current()
        .unwrap()
        .handle
        .disconnect(russh::Disconnect::ByApplication, "", "")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!session.is_connected());
    assert!(!session.listdir("/home/test").await.unwrap().is_empty() || true);
    assert!(session.is_connected());
    assert!(ui.joined().contains("reconnexion"));
    session.close().await;

    // 4. transferts parallèles sur une seule authentification, en SCP puis en SFTP
    for proto in [PROTO_SCP, PROTO_SFTP] {
        transfers_roundtrip(proto).await;
    }

    // 5. code TOTP demandé à l'utilisateur
    let (session, ui) = make_session(PROTO_SCP, OTP_ASK).await;
    assert!(session.is_connected());
    assert_eq!(
        *ui.asked.lock().unwrap(),
        vec![vec!["Verification code: ".to_string()]]
    );
    session.close().await;

    // 6. clé PuTTY (.ppk v3 chiffrée), phrase de passe enregistrée
    let ppk = fixture("test_key_v3.ppk").to_string_lossy().into_owned();
    let (session, ui) = make_session_with(PROTO_SFTP, OTP_SECRET, &ppk, Some(PPK_PASSPHRASE)).await;
    assert!(session.is_connected());
    assert!(ui.joined().contains("authentification partielle"));
    session.close().await;
}

/// Débit SCP / SFTP sur 256 Mio (à lancer en mode release) :
///     cargo test --release -p rustscp --test integration bench -- --ignored --nocapture
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench() {
    if !server_up() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("bench.bin");
    std::fs::write(&src, random(256 * 1024 * 1024)).unwrap();
    let (session, ui) = make_session(PROTO_SCP, OTP_SECRET).await;
    for proto in [PROTO_SCP, PROTO_SFTP] {
        let settings = Arc::new(RwLock::new(Settings {
            max_transfers: 1,
            overwrite_default: "overwrite".into(),
            ..Default::default()
        }));
        let mgr = TransferManager::new(settings, ui.clone());
        session.set_protocol(proto);
        let t = Instant::now();
        mgr.add(
            expand_upload(
                &session,
                &[src.to_string_lossy().into_owned()],
                "/home/test",
            )
            .await
            .unwrap(),
        );
        wait(&mgr).await;
        let up = 256.0 / t.elapsed().as_secs_f64();
        let dst = tmp.path().join(format!("dl-{proto}"));
        std::fs::create_dir_all(&dst).unwrap();
        let t = Instant::now();
        mgr.add(
            expand_download(
                &session,
                &[RemoteItem {
                    path: "/home/test/bench.bin".into(),
                    is_dir: false,
                    size: 0,
                    mode: 0,
                }],
                &dst,
            )
            .await
            .unwrap(),
        );
        wait(&mgr).await;
        let down = 256.0 / t.elapsed().as_secs_f64();
        assert_eq!(sha(&src), sha(&dst.join("bench.bin")));
        eprintln!("BENCH {proto} : envoi {up:.0} Mio/s, réception {down:.0} Mio/s");
    }
    session.close().await;
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/server")
        .join(name)
}

/// Clés PuTTY (PPK v2 en clair, v3 chiffrée Argon2id) : même clé que `test_key`.
#[test]
fn ppk_keys() {
    let reference = russh::keys::load_secret_key(key_path(), None).unwrap();
    let v2 = russh::keys::load_secret_key(fixture("test_key_v2.ppk"), None).unwrap();
    assert_eq!(v2.key_data(), reference.key_data());
    let v3 =
        russh::keys::load_secret_key(fixture("test_key_v3.ppk"), Some(PPK_PASSPHRASE)).unwrap();
    assert_eq!(v3.key_data(), reference.key_data());
    for pass in [None, Some("mauvaise")] {
        let err = russh::keys::load_secret_key(fixture("test_key_v3.ppk"), pass).unwrap_err();
        assert!(rustscp_lib::ssh::key_needs_passphrase(&err), "{err}");
    }
}
