//! Gestionnaire de sites (profils de connexion, dossiers, secrets).

use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use rustscp_shared::util::parse_otp_secret;
use rustscp_shared::{
    AUTH_PASSWORD, OTP_ASK, OTP_SECRET, PROTO_FTP, PROTO_FTPS, PROTO_SCP, PROTO_SFTP, SECRET_OTP,
    SECRET_PASSPHRASE, SECRET_PASSWORD, Site, SitesFile, TotpPreview, auth_choices, auth_uses_key,
    auth_uses_otp, auth_uses_password, proto_is_ftp,
};
use serde_json::{Value, json};

use crate::api::call;
use crate::dialogs::{Modal, alert, ask_text, confirm};
use crate::fmt::random_id;
use crate::icons::icon;
use crate::state::ctx;
use rustscp_shared::{tr, trf};

#[derive(Clone, PartialEq)]
enum Selection {
    None,
    Folder(String),
    Site(String),
}

#[derive(Clone, Copy)]
struct Secrets {
    passphrase: RwSignal<String>,
    password: RwSignal<String>,
    otp: RwSignal<String>,
    /// Valeurs chargées depuis le trousseau (pour n'écrire que les changements).
    original: StoredValue<[String; 3]>,
}

fn secret_input(
    value: RwSignal<String>,
    placeholder: &'static str,
    enabled: Signal<bool>,
) -> impl IntoView {
    let shown = RwSignal::new(false);
    view! {
        <span class="secret-field">
            <input type=move || if shown.get() { "text" } else { "password" } placeholder=placeholder
                autocomplete="off" disabled=move || !enabled.get()
                prop:value=value on:input=move |ev| value.set(event_target_value(&ev))/>
            <button type="button" class="icon-btn" title=tr!("Afficher / masquer", "Show / hide") disabled=move || !enabled.get()
                on:click=move |_| shown.update(|s| *s = !*s)>
                {move || if shown.get() { icon("eye-off").into_any() } else { icon("eye").into_any() }}
            </button>
        </span>
    }
}

#[component]
pub fn SiteManager(dialog_id: u64, select: Option<String>) -> impl IntoView {
    let c = ctx();
    let draft = RwSignal::new(c.sites.get_untracked());
    let selection = RwSignal::new(Selection::None);
    let form = RwSignal::new(Site::default());
    let tab = RwSignal::new(0usize);
    let secrets = Secrets {
        passphrase: RwSignal::new(String::new()),
        password: RwSignal::new(String::new()),
        otp: RwSignal::new(String::new()),
        original: StoredValue::new(Default::default()),
    };
    let preview = RwSignal::new((String::new(), false));
    let name_input = NodeRef::<html::Input>::new();
    let warned = StoredValue::new(false);
    let available = c
        .info
        .get_untracked()
        .secrets
        .map(|s| s.available)
        .unwrap_or(false);

    let current_id = move || match selection.get_untracked() {
        Selection::Site(id) => Some(id),
        _ => None,
    };

    // ------------------------------------------------------------ liaison
    let load = move |id: String| {
        let Some(site) = draft.with_untracked(|d| d.sites.iter().find(|s| s.id == id).cloned())
        else {
            return;
        };
        form.set(site);
        selection.set(Selection::Site(id.clone()));
        secrets.passphrase.set(String::new());
        secrets.password.set(String::new());
        secrets.otp.set(String::new());
        secrets.original.set_value(Default::default());
        spawn_local(async move {
            let mut vals: [String; 3] = Default::default();
            for (i, kind) in [SECRET_PASSPHRASE, SECRET_PASSWORD, SECRET_OTP]
                .iter()
                .enumerate()
            {
                vals[i] = call::<String>("secret_get", json!({ "site_id": id, "kind": kind }))
                    .await
                    .unwrap_or_default();
            }
            // la boîte a pu être fermée ou la sélection changer entre-temps
            if selection.try_get_untracked() != Some(Selection::Site(id)) {
                return;
            }
            secrets.passphrase.try_set(vals[0].clone());
            secrets.password.try_set(vals[1].clone());
            secrets.otp.try_set(vals[2].clone());
            secrets.original.try_set_value(vals);
        });
    };

    // Écrit le formulaire dans le brouillon et enregistre les secrets modifiés.
    let commit = move || {
        let Some(id) = current_id() else { return };
        let mut site = form.get_untracked();
        site.name = site.name.trim().to_string();
        site.host = site.host.trim().to_string();
        site.user = site.user.trim().to_string();
        site.key_file = site.key_file.trim().to_string();
        site.remote_dir = site.remote_dir.trim().to_string();
        site.local_dir = site.local_dir.trim().to_string();
        if site.name.is_empty() {
            site.name = if site.host.is_empty() {
                tr!("Sans nom", "Untitled").into()
            } else {
                site.host.clone()
            };
        }
        let auth = site.auth.clone();
        let values = [
            if auth_uses_key(&auth) {
                secrets.passphrase.get_untracked()
            } else {
                String::new()
            },
            if auth_uses_password(&auth) {
                secrets.password.get_untracked()
            } else {
                String::new()
            },
            if auth_uses_otp(&auth) && site.otp_mode == OTP_SECRET {
                parse_otp_secret(&secrets.otp.get_untracked())
            } else {
                String::new()
            },
        ];
        draft.update(|d| {
            if let Some(s) = d.sites.iter_mut().find(|s| s.id == id) {
                *s = site.clone();
            }
        });
        form.set(site);
        let original = secrets.original.get_value();
        secrets.original.set_value(values.clone());
        for (i, kind) in [SECRET_PASSPHRASE, SECRET_PASSWORD, SECRET_OTP]
            .iter()
            .enumerate()
        {
            if values[i] != original[i] {
                let (id, value, kind) = (id.clone(), values[i].clone(), kind.to_string());
                spawn_local(async move {
                    let stored = call::<bool>(
                        "secret_set",
                        json!({ "site_id": id, "kind": kind, "value": value }),
                    )
                    .await
                    .unwrap_or(false);
                    if !stored && !warned.try_get_value().unwrap_or(true) {
                        warned.try_set_value(true);
                        alert(
                            tr!("Trousseau indisponible", "Keyring unavailable"),
                            tr!("Aucun trousseau système n'est disponible : les secrets ne seront conservés que pendant cette session.", "No system keyring is available: secrets will only be kept for this session."),
                            false,
                        )
                        .await;
                    }
                });
            }
        }
    };

    let select_item = move |sel: Selection| {
        if selection.get_untracked() == sel {
            return;
        }
        commit();
        match sel {
            Selection::Site(id) => load(id),
            other => selection.set(other),
        }
    };

    // sélection initiale
    {
        let initial = select
            .filter(|id| draft.with_untracked(|d| d.sites.iter().any(|s| &s.id == id)))
            .or_else(|| {
                draft.with_untracked(|d| {
                    let mut v = d.sites.clone();
                    v.sort_by_key(|s| (s.folder.to_lowercase(), s.name.to_lowercase()));
                    v.first().map(|s| s.id.clone())
                })
            });
        if let Some(id) = initial {
            load(id);
        }
    }

    // aperçu du code TOTP courant
    let tick = move || {
        let secret = secrets.otp.get_untracked();
        let auto = form.with_untracked(|f| f.otp_mode == OTP_SECRET && auth_uses_otp(&f.auth));
        if secret.trim().is_empty() || !auto {
            preview.set((String::new(), false));
            return;
        }
        spawn_local(async move {
            let value = match call::<TotpPreview>("totp_preview", json!({ "secret": secret })).await
            {
                Ok(p) => {
                    let cut = 3.min(p.code.len());
                    (
                        format!("{} {}|{}", &p.code[..cut], &p.code[cut..], p.left),
                        true,
                    )
                }
                Err(_) => (
                    tr!(
                        "Secret invalide (base32 attendu)",
                        "Invalid secret (base32 expected)"
                    )
                    .into(),
                    false,
                ),
            };
            preview.try_set(value);
        });
    };
    if let Ok(h) = set_interval_with_handle(tick, Duration::from_secs(1)) {
        on_cleanup(move || h.clear());
    }
    Effect::new(move |_| {
        secrets.otp.track();
        form.track();
        tick();
    });

    // ------------------------------------------------------------ actions
    let target_folder = move || match selection.get_untracked() {
        Selection::Folder(f) => f,
        Selection::Site(_) => form.with_untracked(|f| f.folder.clone()),
        Selection::None => String::new(),
    };
    let new_site = move |_| {
        commit();
        let site = Site {
            id: random_id(),
            name: tr!("Nouveau site", "New site").into(),
            folder: target_folder(),
            ..Default::default()
        };
        let id = site.id.clone();
        draft.update(|d| d.sites.push(site));
        load(id);
        tab.set(0);
        if let Some(el) = name_input.get_untracked() {
            request_animation_frame(move || {
                let _ = el.focus();
                el.select();
            });
        }
    };
    let new_folder = move |_| {
        spawn_local(async move {
            let Some(name) = ask_text(
                tr!("Nouveau dossier", "New folder"),
                tr!("Nom du dossier :", "Folder name:"),
                tr!("Nouveau dossier", "New folder"),
            )
            .await
            else {
                return;
            };
            if !draft.with_untracked(|d| d.folders.contains(&name)) {
                commit();
                draft.update(|d| d.folders.push(name.clone()));
                selection.set(Selection::Folder(name));
            }
        });
    };
    let duplicate = move |_| {
        let Some(src_id) = current_id() else { return };
        commit();
        let mut clone = form.get_untracked();
        clone.id = random_id();
        clone.name = trf!("{} (copie)", "{} (copy)", clone.name);
        let dst = clone.id.clone();
        draft.update(|d| d.sites.push(clone));
        spawn_local(async move {
            let _ = call::<Value>("secrets_copy", json!({ "src": src_id, "dst": dst })).await;
            if selection.try_get_untracked().is_some() {
                load(dst);
            }
        });
    };
    let delete = move |_| {
        let sel = selection.get_untracked();
        spawn_local(async move {
            match sel {
                Selection::Folder(name) => {
                    if !confirm(tr!("Supprimer", "Delete"), &trf!("Supprimer le dossier « {name} » ? Ses sites seront déplacés à la racine.", "Delete folder \"{name}\"? Its sites will be moved to the root."))
                        .await
                    {
                        return;
                    }
                    commit();
                    draft.update(|d| {
                        for s in d.sites.iter_mut().filter(|s| s.folder == name) {
                            s.folder.clear();
                        }
                        d.folders.retain(|f| f != &name);
                    });
                    if form.with_untracked(|f| f.folder == name) {
                        form.update(|f| f.folder.clear());
                    }
                    selection.set(Selection::None);
                }
                Selection::Site(id) => {
                    let name = form.with_untracked(|f| f.name.clone());
                    if !confirm(
                        tr!("Supprimer", "Delete"),
                        &trf!(
                            "Supprimer le site « {name} » et ses secrets ?",
                            "Delete site \"{name}\" and its secrets?"
                        ),
                    )
                    .await
                    {
                        return;
                    }
                    let _ = call::<Value>("secrets_delete", json!({ "site_id": id })).await;
                    draft.update(|d| d.sites.retain(|s| s.id != id));
                    selection.set(Selection::None);
                }
                Selection::None => {}
            }
        });
    };
    let save = move || {
        commit();
        let mut file: SitesFile = draft.get_untracked();
        // les dossiers référencés par des sites existent toujours
        for s in &file.sites {
            if !s.folder.is_empty() && !file.folders.contains(&s.folder) {
                file.folders.push(s.folder.clone());
            }
        }
        c.sites.set(file.clone());
        spawn_local(async move {
            if let Err(e) = call::<Value>("sites_set", json!({ "file": file })).await {
                c.log(
                    "error",
                    trf!(
                        "Enregistrement des sites impossible : {e}",
                        "Unable to save sites: {e}"
                    ),
                );
            }
        });
    };
    let close = move || c.close_dialog(dialog_id);
    let ok = move |_| {
        save();
        close();
    };
    let connect = move || {
        save();
        if current_id().is_none() {
            return;
        }
        let site = form.get_untracked();
        if site.host.is_empty() {
            spawn_local(async move {
                alert(
                    tr!("Hôte manquant", "Missing host"),
                    tr!(
                        "Indiquez l'adresse du serveur.",
                        "Enter the server address."
                    ),
                    false,
                )
                .await
            });
            return;
        }
        close();
        crate::actions::connect_site(c, site);
    };
    let browse_key = move |_| {
        let cur = form.with_untracked(|f| f.key_file.clone());
        let start = cur
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .filter(|d| !d.is_empty())
            .unwrap_or("~/.ssh".into());
        spawn_local(async move {
            if let Ok(Some(path)) =
                call::<Option<String>>("pick_file", json!({ "start": start })).await
            {
                let home = c.info.get_untracked().home;
                let shown = match path.strip_prefix(&format!("{home}/")) {
                    Some(rest) => format!("~/{rest}"),
                    None => path,
                };
                form.update(|f| f.key_file = shown);
            }
        });
    };
    let browse_local = move |_| {
        let start = form.with_untracked(|f| {
            if f.local_dir.is_empty() {
                "~".into()
            } else {
                f.local_dir.clone()
            }
        });
        spawn_local(async move {
            if let Ok(Some(path)) =
                call::<Option<String>>("pick_dir", json!({ "start": start })).await
            {
                form.update(|f| f.local_dir = path);
            }
        });
    };

    // --------------------------------------------------------------- vues
    let is_site = Signal::derive(move || matches!(selection.get(), Selection::Site(_)));
    let auth = Signal::derive(move || form.with(|f| f.auth.clone()));
    let key_on = Signal::derive(move || is_site.get() && auth_uses_key(&auth.get()));
    let pw_on = Signal::derive(move || is_site.get() && auth_uses_password(&auth.get()));
    let otp_on = Signal::derive(move || is_site.get() && auth_uses_otp(&auth.get()));
    let ftp = Signal::derive(move || form.with(|f| proto_is_ftp(&f.protocol)));
    let otp_auto = Signal::derive(move || otp_on.get() && form.with(|f| f.otp_mode == OTP_SECRET));

    let tree = move || {
        let d = draft.get();
        let sel = selection.get();
        let mut folders: Vec<String> = d.folders.clone();
        for s in &d.sites {
            if !s.folder.is_empty() && !folders.contains(&s.folder) {
                folders.push(s.folder.clone());
            }
        }
        folders.sort_by_key(|f| f.to_lowercase());
        let (live_id, live_name) = form.with(|f| (f.id.clone(), f.name.clone()));
        let mut sites = d.sites.clone();
        for s in sites.iter_mut().filter(|s| s.id == live_id) {
            s.name = live_name.clone();
        }
        sites.sort_by_key(|s| s.name.to_lowercase());
        let site_row = move |s: &Site, depth: usize, sel: &Selection| {
            let id = s.id.clone();
            let id2 = id.clone();
            let active = *sel == Selection::Site(id.clone());
            view! {
                <div class="sm-item" class:selected=active style=format!("padding-left:{}px", 8 + depth * 18)
                    on:mousedown=move |_| select_item(Selection::Site(id.clone()))
                    on:dblclick=move |_| { if current_id() == Some(id2.clone()) { connect() } }>
                    {icon("server")}<span>{s.name.clone()}</span>
                </div>
            }
        };
        let mut out = Vec::new();
        for f in folders {
            let f2 = f.clone();
            let active = sel == Selection::Folder(f.clone());
            out.push(
                view! {
                    <div class="sm-item folder" class:selected=active on:mousedown=move |_| select_item(Selection::Folder(f2.clone()))>
                        <span class="file-ic">{icon("folder")}</span><span>{f.clone()}</span>
                    </div>
                }
                .into_any(),
            );
            for s in sites.iter().filter(|s| s.folder == f) {
                out.push(site_row(s, 1, &sel).into_any());
            }
        }
        for s in sites.iter().filter(|s| s.folder.is_empty()) {
            out.push(site_row(s, 0, &sel).into_any());
        }
        if out.is_empty() {
            out.push(view! { <div class="muted sm-empty">{tr!("Aucun site. Cliquez sur « Nouveau site ».", "No site. Click \"New site\".")}</div> }.into_any());
        }
        out
    };

    let folder_options = move || {
        let d = draft.get();
        let mut folders = d.folders.clone();
        folders.sort_by_key(|f| f.to_lowercase());
        let cur = form.with(|f| f.folder.clone());
        std::iter::once(String::new())
            .chain(folders)
            .map(|f| {
                let label = if f.is_empty() {
                    tr!("(racine)", "(root)").to_string()
                } else {
                    f.clone()
                };
                let selected = f == cur;
                view! { <option value=f prop:selected=selected>{label}</option> }
            })
            .collect_view()
    };

    let text_field = move |get: fn(&Site) -> String,
                           set: fn(&mut Site, String),
                           placeholder: &'static str| {
        view! {
            <input type="text" placeholder=placeholder disabled=move || !is_site.get()
                prop:value=move || form.with(get)
                on:input=move |ev| { let v = event_target_value(&ev); form.update(|f| set(f, v)) }/>
        }
    };

    let general = move || {
        view! {
            <div class="kv sm-form">
                <span>{tr!("Nom :", "Name:")}</span>
                <input type="text" node_ref=name_input disabled=move || !is_site.get()
                    prop:value=move || form.with(|f| f.name.clone())
                    on:input=move |ev| { let v = event_target_value(&ev); form.update(|f| f.name = v) }/>
                <span>{tr!("Dossier :", "Folder:")}</span>
                <select disabled=move || !is_site.get()
                    on:change=move |ev| { let v = event_target_value(&ev); form.update(|f| f.folder = v) }>
                    {folder_options}
                </select>
                <span>{tr!("Protocole :", "Protocol:")}</span>
                <select disabled=move || !is_site.get()
                    on:change=move |ev| { let v = event_target_value(&ev); form.update(|f| set_protocol(f, v)) }>
                    <option value=PROTO_SCP prop:selected=move || form.with(|f| f.protocol == PROTO_SCP)>{tr!("SCP — protocole SCP natif (navigation via SFTP/shell)", "SCP — native SCP protocol (browsing via SFTP/shell)")}</option>
                    <option value=PROTO_SFTP prop:selected=move || form.with(|f| f.protocol == PROTO_SFTP)>"SFTP — SSH File Transfer Protocol"</option>
                    <option value=PROTO_FTP prop:selected=move || form.with(|f| f.protocol == PROTO_FTP)>{tr!("FTP — sans chiffrement", "FTP — unencrypted")}</option>
                    <option value=PROTO_FTPS prop:selected=move || form.with(|f| f.protocol == PROTO_FTPS)>{tr!("FTPS — FTP sur TLS explicite", "FTPS — FTP over explicit TLS")}</option>
                </select>
                <span>{tr!("Hôte :", "Host:")}</span>
                <span class="inline grow">
                    {text_field(|f| f.host.clone(), |f, v| f.host = v, tr!("serveur.exemple.fr", "server.example.com"))}
                    <span class="lbl">{tr!("Port :", "Port:")}</span>
                    <input type="number" min="1" max="65535" class="port" disabled=move || !is_site.get()
                        prop:value=move || form.with(|f| f.port.to_string())
                        on:input=move |ev| { if let Ok(p) = event_target_value(&ev).parse::<u16>() { form.update(|f| f.port = p.max(1)) } }/>
                </span>
                <span>{tr!("Identifiant :", "Username:")}</span>
                {text_field(|f| f.user.clone(), |f, v| f.user = v, "")}
                <span>{tr!("Authentification :", "Authentication:")}</span>
                <select disabled=move || !is_site.get() || ftp.get()
                    on:change=move |ev| { let v = event_target_value(&ev); form.update(|f| f.auth = v) }>
                    {auth_choices().into_iter().map(|(k, l)| view! {
                        <option value=k prop:selected=move || form.with(|f| f.auth == k)>{l}</option>
                    }).collect_view()}
                </select>
            </div>

            <fieldset class="group" disabled=move || !key_on.get()>
                <legend>{tr!("Clé SSH", "SSH key")}</legend>
                <div class="kv">
                    <span>{tr!("Fichier de clé :", "Key file:")}</span>
                    <span class="inline grow">
                        {text_field(|f| f.key_file.clone(), |f, v| f.key_file = v, tr!("~/.ssh/id_ed25519 ou clé PuTTY .ppk", "~/.ssh/id_ed25519 or PuTTY .ppk key"))}
                        <button type="button" on:click=browse_key>{tr!("Parcourir…", "Browse…")}</button>
                    </span>
                    <span>{tr!("Phrase de passe :", "Passphrase:")}</span>
                    {secret_input(secrets.passphrase, tr!("(demandée si nécessaire)", "(asked if needed)"), key_on)}
                </div>
                <label class="check"><input type="checkbox" prop:checked=move || form.with(|f| f.use_agent)
                    on:change=move |ev| { let on = event_target_checked(&ev); form.update(|f| f.use_agent = on) }/>
                    {tr!("Utiliser aussi les clés de l'agent SSH (ssh-agent, Pageant)", "Also use SSH agent keys (ssh-agent, Pageant)")}</label>
            </fieldset>

            <fieldset class="group" disabled=move || !pw_on.get()>
                <legend>{tr!("Mot de passe", "Password")}</legend>
                <div class="kv">
                    <span>{tr!("Mot de passe :", "Password:")}</span>
                    {secret_input(secrets.password, tr!("(demandé à la connexion si vide)", "(asked at connection if empty)"), pw_on)}
                </div>
            </fieldset>

            <fieldset class="group" disabled=move || !otp_on.get()>
                <legend>{tr!("Code à usage unique (TOTP / Google Authenticator)", "One-time code (TOTP / Google Authenticator)")}</legend>
                <label class="radio"><input type="radio" name="otp" prop:checked=move || form.with(|f| f.otp_mode != OTP_SECRET)
                    on:change=move |_| form.update(|f| f.otp_mode = OTP_ASK.into())/>{tr!("Demander le code à chaque connexion", "Ask for the code at each connection")}</label>
                <label class="radio"><input type="radio" name="otp" prop:checked=move || form.with(|f| f.otp_mode == OTP_SECRET)
                    on:change=move |_| form.update(|f| f.otp_mode = OTP_SECRET.into())/>{tr!("Générer le code automatiquement à partir du secret", "Generate the code automatically from the secret")}</label>
                <div class="kv indent">
                    <span>{tr!("Secret :", "Secret:")}</span>
                    {secret_input(secrets.otp, tr!("Base32 (JBSWY3DP…) ou URI otpauth://", "Base32 (JBSWY3DP…) or otpauth:// URI"), otp_auto)}
                </div>
                <div class="indent otp-preview">
                    {move || {
                        let (text, ok) = preview.get();
                        if text.is_empty() {
                            ().into_any()
                        } else if ok {
                            let (code, left) = text.split_once('|').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
                            view! { <span>{tr!("Code actuel : ", "Current code: ")} <b class="otp-code">{code}</b> {trf!(" (expire dans {left} s) — vérifiez qu'il correspond à votre application", " (expires in {left} s) — check that it matches your app")}</span> }.into_any()
                        } else {
                            view! { <span class="error-text">{text}</span> }.into_any()
                        }
                    }}
                </div>
                <p class="small muted">{trf!("Les secrets sont stockés dans : {}.", "Secrets are stored in: {}.", if available { tr!("trousseau système", "system keyring") } else { tr!("mémoire uniquement (aucun trousseau détecté)", "memory only (no keyring detected)") })}</p>
            </fieldset>
        }
    };

    let advanced = move || {
        view! {
            <div class="kv sm-form">
                <span>{tr!("Dossier distant initial :", "Initial remote folder:")}</span>
                {text_field(|f| f.remote_dir.clone(), |f, v| f.remote_dir = v, tr!("(dossier personnel)", "(home folder)"))}
                <span>{tr!("Dossier local initial :", "Initial local folder:")}</span>
                <span class="inline grow">
                    {text_field(|f| f.local_dir.clone(), |f, v| f.local_dir = v, "")}
                    <button type="button" disabled=move || !is_site.get() on:click=browse_local>{tr!("Parcourir…", "Browse…")}</button>
                </span>
                <span>{tr!("Keepalive SSH :", "SSH keepalive:")}</span>
                <span class="inline">
                    <input type="number" min="0" max="3600" class="narrow" disabled=move || !is_site.get() || ftp.get()
                        prop:value=move || form.with(|f| f.keepalive.to_string())
                        on:input=move |ev| { if let Ok(n) = event_target_value(&ev).parse::<u32>() { form.update(|f| f.keepalive = n.min(3600)) } }/>
                    " s " <span class="muted">{tr!("(0 = désactivé)", "(0 = disabled)")}</span>
                </span>
            </div>
            <label class="check"><input type="checkbox" disabled=move || !is_site.get() || ftp.get() prop:checked=move || form.with(|f| f.compression)
                on:change=move |ev| { let on = event_target_checked(&ev); form.update(|f| f.compression = on) }/>
                {tr!("Compression SSH (utile sur liaison lente)", "SSH compression (useful on slow links)")}</label>
        }
    };

    let comments = move || {
        view! {
            <textarea class="comments" disabled=move || !is_site.get()
                prop:value=move || form.with(|f| f.comments.clone())
                on:input=move |ev| { let v = event_target_value(&ev); form.update(|f| f.comments = v) }></textarea>
        }
    };

    view! {
        <Modal title=tr!("Gestionnaire de sites", "Site Manager").to_string() class="site-manager" on_close=Callback::new(move |_| close())>
            <div class="sm-body">
                <div class="sm-left">
                    <div class="small">{tr!("Sélectionnez une entrée :", "Select an entry:")}</div>
                    <div class="sm-tree">{tree}</div>
                    <div class="sm-buttons">
                        <button type="button" on:click=new_site>{icon("plus")}{tr!("Nouveau site", "New site")}</button>
                        <button type="button" on:click=new_folder>{icon("folder-new")}{tr!("Nouveau dossier", "New folder")}</button>
                        <button type="button" disabled=move || !is_site.get() on:click=duplicate>{icon("copy")}{tr!("Dupliquer", "Duplicate")}</button>
                        <button type="button" disabled=move || selection.get() == Selection::None on:click=delete>{icon("trash")}{tr!("Supprimer", "Delete")}</button>
                    </div>
                </div>
                <div class="sm-right">
                    <div class="tabs-bar small-tabs">
                        {[tr!("Général", "General"), tr!("Avancé", "Advanced"), tr!("Commentaires", "Comments")].iter().enumerate().map(|(i, t)| view! {
                            <button type="button" class="tab-btn" class:active=move || tab.get() == i on:click=move |_| tab.set(i)>{*t}</button>
                        }).collect_view()}
                    </div>
                    <div class="sm-page">
                        {move || match tab.get() {
                            0 => general().into_any(),
                            1 => advanced().into_any(),
                            _ => comments().into_any(),
                        }}
                    </div>
                </div>
            </div>
            <div class="modal-buttons">
                <button type="button" on:click=move |_| close()>{tr!("Annuler", "Cancel")}</button>
                <button type="button" on:click=ok>"OK"</button>
                <button type="button" class="primary" disabled=move || !is_site.get() on:click=move |_| connect()>{icon("connect")}{tr!("Connexion", "Connect")}</button>
            </div>
        </Modal>
    }
}

/// Change le protocole ; passer en FTP impose le mot de passe et le port 21 (et inversement).
fn set_protocol(f: &mut Site, protocol: String) {
    let was_ftp = proto_is_ftp(&f.protocol);
    f.protocol = protocol;
    match (was_ftp, proto_is_ftp(&f.protocol)) {
        (false, true) => {
            if f.port == 22 {
                f.port = 21;
            }
            f.auth = AUTH_PASSWORD.into();
        }
        (true, false) if f.port == 21 => f.port = 22,
        _ => {}
    }
}
