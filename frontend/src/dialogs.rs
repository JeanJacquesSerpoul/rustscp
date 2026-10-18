//! Boîtes de dialogue : confirmation, saisie, clé d'hôte, 2FA/TOTP, secrets,
//! écrasement, permissions, réglages, à propos.

use std::sync::{Arc, Mutex};

use futures::channel::oneshot;
use leptos::html;
use leptos::prelude::*;
use rustscp_shared::util::{fmt_int, human_size};
use rustscp_shared::{PromptField, PromptReply, PromptRequest, overwrite_choices};
use serde_json::json;
use wasm_bindgen::JsCast;

use crate::api::fire;
use crate::fmt::format_mtime;
use crate::icons::{icon, logo};
use crate::state::{Dialog, DialogEntry, Reply, ctx, reply};
use rustscp_shared::{tr, trf};

// ------------------------------------------------------------- appels async
async fn open<T: Send + 'static>(make: impl FnOnce(Reply<T>) -> Dialog) -> Option<T> {
    let (tx, rx) = oneshot::channel();
    let r: Reply<T> = Arc::new(Mutex::new(Some(tx)));
    ctx().open_dialog(make(r));
    rx.await.ok()
}

pub async fn confirm(title: &str, text: &str) -> bool {
    let (title, text) = (title.to_string(), text.to_string());
    open(|tx| Dialog::Confirm {
        title,
        text,
        yes: tr!("Oui", "Yes").into(),
        no: tr!("Non", "No").into(),
        tx,
    })
    .await
    .unwrap_or(false)
}

pub async fn ask_text(title: &str, label: &str, value: &str) -> Option<String> {
    let (title, label, value) = (title.to_string(), label.to_string(), value.to_string());
    open(|tx| Dialog::Text {
        title,
        label,
        value,
        tx,
    })
    .await
    .flatten()
}

pub async fn alert(title: &str, text: &str, error: bool) {
    let (title, text) = (title.to_string(), text.to_string());
    open(|tx| Dialog::Alert {
        title,
        text,
        error,
        tx,
    })
    .await;
}

pub async fn chmod_dialog(title: &str, mode: u32, has_dirs: bool) -> Option<(u32, bool)> {
    let title = title.to_string();
    open(|tx| Dialog::Chmod {
        title,
        mode,
        has_dirs,
        tx,
    })
    .await
    .flatten()
}

// ---------------------------------------------------------------- cadre
/// Fenêtre modale : Échap ferme (`on_close`), le formulaire valide avec Entrée.
#[component]
pub fn Modal(
    title: String,
    #[prop(optional)] class: &'static str,
    #[prop(into)] on_close: Callback<()>,
    children: Children,
) -> impl IntoView {
    let keydown = move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Escape" {
            ev.prevent_default();
            ev.stop_propagation();
            on_close.run(());
        }
    };
    view! {
        <div class="modal-backdrop" on:keydown=keydown on:mousedown=|ev| ev.stop_propagation()>
            <div class=format!("modal {class}") role="dialog" aria-modal="true">
                <div class="modal-title">
                    <span>{title}</span>
                    <button type="button" class="icon-btn" title=tr!("Fermer", "Close") on:click=move |_| on_close.run(())>
                        {icon("close")}
                    </button>
                </div>
                {children()}
            </div>
        </div>
    }
}

/// Donne le focus au premier champ (ou au bouton par défaut) d'une boîte.
fn autofocus<E>(node: NodeRef<E>)
where
    E: leptos::html::ElementType,
    E::Output: JsCast + Clone + 'static,
{
    Effect::new(move |_| {
        if let Some(el) = node.get() {
            let el: web_sys::HtmlElement = el.unchecked_into();
            request_animation_frame(move || {
                let _ = el.focus();
            });
        }
    });
}

#[component]
pub fn DialogHost() -> impl IntoView {
    let ctx = ctx();
    view! {
        <For each=move || ctx.dialogs.get() key=|d| d.id children=render/>
    }
}

fn render(entry: DialogEntry) -> AnyView {
    let id = entry.id;
    let close = move || ctx().close_dialog(id);
    match entry.dialog {
        Dialog::Confirm {
            title,
            text,
            yes,
            no,
            tx,
        } => {
            let (t1, t2) = (tx.clone(), tx.clone());
            let ok = NodeRef::<html::Button>::new();
            autofocus(ok);
            view! {
                <Modal title=title on_close=Callback::new(move |_| { reply(&t1, false); close(); })>
                    <div class="modal-body"><p class="pre">{text}</p></div>
                    <div class="modal-buttons">
                        <button type="button" on:click=move |_| { reply(&t2, false); close(); }>{no}</button>
                        <button type="button" class="primary" node_ref=ok on:click=move |_| { reply(&tx, true); close(); }>{yes}</button>
                    </div>
                </Modal>
            }
            .into_any()
        }
        Dialog::Alert {
            title,
            text,
            error,
            tx,
        } => {
            let t1 = tx.clone();
            let ok = NodeRef::<html::Button>::new();
            autofocus(ok);
            view! {
                <Modal title=title on_close=Callback::new(move |_| { reply(&t1, ()); close(); })>
                    <div class="modal-body alert-body">
                        <span class=if error { "alert-ic error" } else { "alert-ic" }>{icon(if error { "warning" } else { "info" })}</span>
                        <p class="pre">{text}</p>
                    </div>
                    <div class="modal-buttons">
                        <button type="button" class="primary" node_ref=ok on:click=move |_| { reply(&tx, ()); close(); }>"OK"</button>
                    </div>
                </Modal>
            }
            .into_any()
        }
        Dialog::Text {
            title,
            label,
            value,
            tx,
        } => text_dialog(id, title, label, value, tx),
        Dialog::Chmod {
            title,
            mode,
            has_dirs,
            tx,
        } => chmod_view(id, title, mode, has_dirs, tx),
        Dialog::Prompt { id: pid, request } => prompt_view(id, pid, request),
        Dialog::SiteManager { select } => {
            view! { <crate::sitemanager::SiteManager dialog_id=id select=select/> }.into_any()
        }
        Dialog::Settings => settings_view(id),
        Dialog::About => about_view(id),
    }
}

fn text_dialog(
    id: u64,
    title: String,
    label: String,
    value: String,
    tx: Reply<Option<String>>,
) -> AnyView {
    let close = move || ctx().close_dialog(id);
    let text = RwSignal::new(value.clone());
    let input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            let value = value.clone();
            request_animation_frame(move || {
                let _ = el.focus();
                let n = value.chars().count() as u32;
                // sélectionne le nom sans l'extension, comme un gestionnaire de fichiers
                let end = match value.rfind('.') {
                    Some(i) if i > 0 => value[..i].chars().count() as u32,
                    _ => n,
                };
                let _ = el.set_selection_range(0, end);
            });
        }
    });
    let (t1, t2) = (tx.clone(), tx.clone());
    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let v = text.get_untracked().trim().to_string();
        reply(&tx, if v.is_empty() { None } else { Some(v) });
        close();
    };
    view! {
        <Modal title=title on_close=Callback::new(move |_| { reply(&t1, None); close(); })>
            <form on:submit=submit>
                <div class="modal-body">
                    <label class="field-label">{label}</label>
                    <input class="wide" type="text" node_ref=input prop:value=text on:input=move |ev| text.set(event_target_value(&ev))/>
                </div>
                <div class="modal-buttons">
                    <button type="button" on:click=move |_| { reply(&t2, None); close(); }>{tr!("Annuler", "Cancel")}</button>
                    <button type="submit" class="primary">"OK"</button>
                </div>
            </form>
        </Modal>
    }
    .into_any()
}

fn chmod_view(
    id: u64,
    title: String,
    mode: u32,
    has_dirs: bool,
    tx: Reply<Option<(u32, bool)>>,
) -> AnyView {
    let close = move || ctx().close_dialog(id);
    let initial = if mode & 0o7777 != 0 {
        mode & 0o7777
    } else {
        0o644
    };
    let bits = RwSignal::new(initial);
    let text = RwSignal::new(format!("{:03o}", initial));
    let recursive = RwSignal::new(false);
    let rows = [
        (tr!("Propriétaire", "Owner"), 6u32),
        (tr!("Groupe", "Group"), 3),
        (tr!("Autres", "Others"), 0),
    ];
    let cols = [
        (tr!("Lecture", "Read"), 2u32),
        (tr!("Écriture", "Write"), 1),
        (tr!("Exécution", "Execute"), 0),
    ];
    let (t1, t2) = (tx.clone(), tx.clone());
    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        reply(&tx, Some((bits.get_untracked(), recursive.get_untracked())));
        close();
    };
    view! {
        <Modal title=tr!("Modifier les droits", "Change permissions").to_string() on_close=Callback::new(move |_| { reply(&t1, None); close(); })>
            <form on:submit=submit>
                <div class="modal-body">
                    <p>{tr!("Droits de ", "Permissions of ")} <b>{title}</b></p>
                    <table class="chmod-grid">
                        <tr><th></th>{cols.iter().map(|(c, _)| view! { <th>{*c}</th> }).collect_view()}</tr>
                        {rows.iter().map(|(label, shift)| {
                            let shift = *shift;
                            view! {
                                <tr>
                                    <td class="row-label">{*label}</td>
                                    {cols.iter().map(|(_, b)| {
                                        let bit = 1u32 << (shift + b);
                                        view! {
                                            <td><input type="checkbox"
                                                prop:checked=move || bits.get() & bit != 0
                                                on:change=move |ev| {
                                                    let on = event_target_checked(&ev);
                                                    bits.update(|m| if on { *m |= bit } else { *m &= !bit });
                                                    text.set(format!("{:03o}", bits.get_untracked()));
                                                }/></td>
                                        }
                                    }).collect_view()}
                                </tr>
                            }
                        }).collect_view()}
                    </table>
                    <div class="form-row">
                        <label>{tr!("Valeur numérique :", "Numeric value:")}</label>
                        <input class="mono narrow" maxlength="4" prop:value=text on:input=move |ev| {
                            let v = event_target_value(&ev);
                            text.set(v.clone());
                            if let Ok(n) = u32::from_str_radix(v.trim(), 8)
                                && n <= 0o7777 { bits.set(n); }
                        }/>
                    </div>
                    <Show when=move || has_dirs>
                        <label class="check"><input type="checkbox" prop:checked=recursive on:change=move |ev| recursive.set(event_target_checked(&ev))/>{tr!("Appliquer récursivement aux sous-dossiers", "Apply recursively to subfolders")}</label>
                    </Show>
                </div>
                <div class="modal-buttons">
                    <button type="button" on:click=move |_| { reply(&t2, None); close(); }>{tr!("Annuler", "Cancel")}</button>
                    <button type="submit" class="primary">"OK"</button>
                </div>
            </form>
        </Modal>
    }
    .into_any()
}

// -------------------------------------------------- questions du cœur
fn answer(dialog_id: u64, prompt_id: u64, reply: PromptReply) {
    fire("prompt_reply", json!({ "id": prompt_id, "reply": reply }));
    ctx().close_dialog(dialog_id);
}

fn prompt_view(id: u64, pid: u64, request: PromptRequest) -> AnyView {
    let cancel = Callback::new(move |_| answer(id, pid, PromptReply::Cancel));
    match request {
        PromptRequest::HostKey {
            host,
            key_type,
            fingerprint,
            status,
            old_fingerprint,
        } => {
            let changed = status == "changed";
            let title = if changed {
                tr!("Clé d'hôte modifiée !", "Host key changed!")
            } else {
                tr!("Clé d'hôte inconnue", "Unknown host key")
            };
            let default_btn = NodeRef::<html::Button>::new();
            autofocus(default_btn);
            let head = if changed {
                view! {
                    <div class="hostkey-head danger">
                        {icon("warning")}
                        <div><b>{tr!("ATTENTION : la clé d'hôte du serveur a changé !", "WARNING: the server's host key has changed!")}</b><br/>
                        {tr!("Il peut s'agir d'une attaque de type « homme du milieu » ou d'une réinstallation du serveur.", "This may be a man-in-the-middle attack or a server reinstallation.")}</div>
                    </div>
                }
                .into_any()
            } else {
                view! {
                    <div class="hostkey-head">
                        {icon("shield")}
                        <div>{tr!("La clé d'hôte du serveur ", "The host key of server ")} <b>{host.clone()}</b> {tr!(" est inconnue.", " is unknown.")}<br/>
                        {tr!("Vérifiez l'empreinte auprès de l'administrateur avant de faire confiance au serveur.", "Check the fingerprint with the administrator before trusting the server.")}</div>
                    </div>
                }
                .into_any()
            };
            view! {
                <Modal title=title.to_string() on_close=cancel>
                    <div class="modal-body">
                        {head}
                        <div class="kv">
                            <span>{tr!("Serveur :", "Server:")}</span><span>{host}</span>
                            <span>{tr!("Type de clé :", "Key type:")}</span><span>{key_type}</span>
                            <span>{tr!("Empreinte :", "Fingerprint:")}</span><span class="mono selectable">{fingerprint}</span>
                            {changed.then(|| view! { <span>{tr!("Ancienne empreinte :", "Previous fingerprint:")}</span><span class="mono selectable">{old_fingerprint}</span> })}
                        </div>
                    </div>
                    <div class="modal-buttons">
                        <button type="button" on:click=move |_| answer(id, pid, PromptReply::Cancel)>{tr!("Annuler", "Cancel")}</button>
                        <button type="button" on:click=move |_| answer(id, pid, PromptReply::HostKey { choice: "once".into() })>{tr!("Faire confiance une fois", "Trust once")}</button>
                        <button type="button" class=if changed { "" } else { "primary" } node_ref=default_btn
                            on:click=move |_| answer(id, pid, PromptReply::HostKey { choice: "save".into() })>{tr!("Faire confiance et enregistrer", "Trust and save")}</button>
                    </div>
                </Modal>
            }
            .into_any()
        }
        PromptRequest::Secret { title, label } => {
            let value = RwSignal::new(String::new());
            let remember = RwSignal::new(false);
            let input = NodeRef::<html::Input>::new();
            autofocus(input);
            let submit = move |ev: web_sys::SubmitEvent| {
                ev.prevent_default();
                answer(
                    id,
                    pid,
                    PromptReply::Secret {
                        value: value.get_untracked(),
                        remember: remember.get_untracked(),
                    },
                );
            };
            view! {
                <Modal title=title on_close=cancel>
                    <form on:submit=submit>
                        <div class="modal-body">
                            <p class="pre">{label}</p>
                            <input class="wide" type="password" node_ref=input on:input=move |ev| value.set(event_target_value(&ev))/>
                            <label class="check"><input type="checkbox" on:change=move |ev| remember.set(event_target_checked(&ev))/>{tr!("Mémoriser dans le trousseau du système", "Remember in the system keyring")}</label>
                        </div>
                        <div class="modal-buttons">
                            <button type="button" on:click=move |_| answer(id, pid, PromptReply::Cancel)>{tr!("Annuler", "Cancel")}</button>
                            <button type="submit" class="primary">"OK"</button>
                        </div>
                    </form>
                </Modal>
            }
            .into_any()
        }
        PromptRequest::Interactive {
            site_label,
            title,
            instructions,
            fields,
        } => interactive_view(id, pid, site_label, title, instructions, fields),
        PromptRequest::Overwrite {
            direction: _,
            source,
            target,
            src_size,
            src_mtime,
            dst_size,
            dst_mtime,
        } => {
            let c = ctx();
            let choice = RwSignal::new(c.last_overwrite.get_value());
            let apply_all = RwSignal::new(false);
            let submit = move |ev: web_sys::SubmitEvent| {
                ev.prevent_default();
                let action = choice.get_untracked();
                c.last_overwrite.set_value(action.clone());
                answer(
                    id,
                    pid,
                    PromptReply::Overwrite {
                        action,
                        apply_all: apply_all.get_untracked(),
                    },
                );
            };
            let ok = NodeRef::<html::Button>::new();
            autofocus(ok);
            view! {
                <Modal title=tr!("Le fichier cible existe déjà", "The target file already exists").to_string() class="wide-modal" on_close=cancel>
                    <form on:submit=submit>
                        <div class="modal-body">
                            <p><b>{tr!("Le fichier cible existe déjà :", "The target file already exists:")}</b><br/><span class="selectable">{target.clone()}</span></p>
                            <table class="cmp">
                                <tr><th></th><th>"Source"</th><th>{tr!("Cible", "Target")}</th></tr>
                                <tr><td class="row-label">{tr!("Chemin", "Path")}</td><td class="path">{source}</td><td class="path">{target}</td></tr>
                                <tr><td class="row-label">{tr!("Taille", "Size")}</td>
                                    <td>{format!("{} ({} o)", human_size(src_size), fmt_int(src_size))}</td>
                                    <td>{format!("{} ({} o)", human_size(dst_size), fmt_int(dst_size))}</td></tr>
                                <tr><td class="row-label">{tr!("Modifié", "Modified")}</td><td>{format_mtime(src_mtime)}</td><td>{format_mtime(dst_mtime)}</td></tr>
                            </table>
                            <fieldset>
                                <legend>"Action"</legend>
                                {overwrite_choices().into_iter().map(|(key, label)| {
                                    let key = key.to_string();
                                    let k2 = key.clone();
                                    view! {
                                        <label class="radio"><input type="radio" name="ow"
                                            prop:checked=move || choice.get() == key
                                            on:change=move |_| choice.set(k2.clone())/>{label}</label>
                                    }
                                }).collect_view()}
                            </fieldset>
                            <label class="check"><input type="checkbox" on:change=move |ev| apply_all.set(event_target_checked(&ev))/>{tr!("Toujours utiliser cette action pour le reste de la file", "Always use this action for the rest of the queue")}</label>
                        </div>
                        <div class="modal-buttons">
                            <button type="button" on:click=move |_| answer(id, pid, PromptReply::Cancel)>{tr!("Annuler ce transfert", "Cancel this transfer")}</button>
                            <button type="submit" class="primary" node_ref=ok>"OK"</button>
                        </div>
                    </form>
                </Modal>
            }
            .into_any()
        }
    }
}

fn interactive_view(
    id: u64,
    pid: u64,
    site_label: String,
    title: String,
    instructions: String,
    fields: Vec<PromptField>,
) -> AnyView {
    let cancel = Callback::new(move |_| answer(id, pid, PromptReply::Cancel));
    let has_otp = fields.iter().any(|f| f.kind == "otp");
    let values: Vec<RwSignal<String>> = fields
        .iter()
        .map(|_| RwSignal::new(String::new()))
        .collect();
    let first = NodeRef::<html::Input>::new();
    autofocus(first);
    let kinds: Vec<String> = fields.iter().map(|f| f.kind.clone()).collect();
    let vals = values.clone();
    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let answers = vals
            .iter()
            .zip(&kinds)
            .map(|(v, k)| {
                let s = v.get_untracked();
                if k == "otp" {
                    s.trim().replace(' ', "")
                } else {
                    s
                }
            })
            .collect();
        answer(id, pid, PromptReply::Interactive { answers });
    };
    let info = [title, instructions]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let head = if has_otp {
        view! {
            <div class="twofa-head">
                <span class="twofa-ic">{icon("shield")}</span>
                <div>
                    <div class="twofa-title">{tr!("Double authentification", "Two-factor authentication")}</div>
                    <div>{tr!("Le serveur ", "The server ")} <b>{site_label.clone()}</b> {tr!(" demande un code à usage unique.", " requests a one-time code.")}</div>
                    <div class="muted">{tr!("Ouvrez votre application d'authentification (Google Authenticator, Aegis, FreeOTP…).", "Open your authenticator app (Google Authenticator, Aegis, FreeOTP…).")}</div>
                </div>
            </div>
        }
        .into_any()
    } else {
        view! {
            <div class="twofa-head">
                <span class="twofa-ic">{icon("lock")}</span>
                <div>{tr!("Le serveur ", "The server ")} <b>{site_label.clone()}</b> {tr!(" demande des informations supplémentaires.", " requests additional information.")}</div>
            </div>
        }
        .into_any()
    };
    view! {
        <Modal title=trf!("Authentification — {site_label}", "Authentication — {site_label}") class="auth-modal" on_close=cancel>
            <form on:submit=submit autocomplete="off">
                <div class="modal-body">
                    {head}
                    {(!info.is_empty()).then(|| view! { <p class="muted pre">{info}</p> })}
                    <div class="kv">
                        {fields.into_iter().zip(values).enumerate().map(|(i, (f, v))| {
                            let label = if f.text.trim().is_empty() { tr!("Réponse :", "Answer:").to_string() } else { f.text.trim().to_string() };
                            let otp = f.kind == "otp";
                            let ty = if otp || f.echo { "text" } else { "password" };
                            let node = if i == 0 { first } else { NodeRef::new() };
                            view! {
                                <span>{label}</span>
                                <input type=ty class=if otp { "otp-input" } else { "wide" }
                                    inputmode=if otp { "numeric" } else { "text" }
                                    placeholder=if otp { "123456" } else { "" }
                                    maxlength=if otp { "64" } else { "1024" }
                                    node_ref=node on:input=move |ev| v.set(event_target_value(&ev))/>
                            }
                        }).collect_view()}
                    </div>
                </div>
                <div class="modal-buttons">
                    <button type="button" on:click=move |_| answer(id, pid, PromptReply::Cancel)>{tr!("Annuler", "Cancel")}</button>
                    <button type="submit" class="primary">{tr!("Valider", "Submit")}</button>
                </div>
            </form>
        </Modal>
    }
    .into_any()
}

// ------------------------------------------------------------- réglages
fn settings_view(id: u64) -> AnyView {
    let c = ctx();
    let close = move || c.close_dialog(id);
    let draft = RwSignal::new(c.settings.get_untracked());
    let backend = c
        .info
        .get_untracked()
        .secrets
        .map(|s| s.name)
        .unwrap_or_default();
    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let mut s = draft.get_untracked();
        s.max_transfers = s.max_transfers.clamp(1, 16);
        s.connect_timeout = s.connect_timeout.clamp(3, 300);
        let language_changed = s.language != c.settings.get_untracked().language;
        c.settings.set(s.clone());
        close();
        if language_changed {
            // la langue s'applique au démarrage de l'interface : on recharge après l'enregistrement
            leptos::task::spawn_local(async move {
                let _ =
                    crate::api::call::<serde_json::Value>("settings_set", json!({ "settings": s }))
                        .await;
                let _ = crate::fmt::window().location().reload();
            });
        } else {
            crate::actions::save_settings(c);
        }
    };
    let num =
        |v: String, min: u32, max: u32| v.trim().parse::<u32>().ok().map(|n| n.clamp(min, max));
    view! {
        <Modal title=tr!("Paramètres", "Settings").to_string() on_close=Callback::new(move |_| close())>
            <form on:submit=submit>
                <div class="modal-body">
                    <div class="kv">
                        <span>{tr!("Transferts simultanés :", "Simultaneous transfers:")}</span>
                        <input type="number" min="1" max="16" class="narrow"
                            title=tr!("Les transferts parallèles partagent la même connexion SSH authentifiée :\naucun code TOTP supplémentaire n'est demandé.", "Parallel transfers share the same authenticated SSH connection:\nno additional TOTP code is requested.")
                            prop:value=move || draft.with(|s| s.max_transfers.to_string())
                            on:change=move |ev| { if let Some(n) = num(event_target_value(&ev), 1, 16) { draft.update(|s| s.max_transfers = n) } }/>
                        <span>{tr!("Si le fichier existe :", "If the file exists:")}</span>
                        <select on:change=move |ev| { let v = event_target_value(&ev); draft.update(|s| s.overwrite_default = v) }>
                            {std::iter::once(("ask", tr!("Demander", "Ask"))).chain(overwrite_choices()).map(|(k, l)| {
                                let selected = draft.with_untracked(|s| s.overwrite_default == k);
                                view! { <option value=k prop:selected=selected>{l}</option> }
                            }).collect_view()}
                        </select>
                        <span>{tr!("Délai de connexion :", "Connection timeout:")}</span>
                        <span class="inline">
                            <input type="number" min="3" max="300" class="narrow"
                                prop:value=move || draft.with(|s| s.connect_timeout.to_string())
                                on:change=move |ev| { if let Some(n) = num(event_target_value(&ev), 3, 300) { draft.update(|s| s.connect_timeout = n) } }/>
                            " s"
                        </span>
                        <span>{tr!("Langue :", "Language:")}</span>
                        <select on:change=move |ev| { let v = event_target_value(&ev); draft.update(|s| s.language = v) }>
                            {rustscp_shared::i18n::language_choices().into_iter().map(|(k, l)| {
                                let selected = draft.with_untracked(|s| s.language == k);
                                view! { <option value=k prop:selected=selected>{l}</option> }
                            }).collect_view()}
                        </select>
                    </div>
                    <label class="check"><input type="checkbox" prop:checked=move || draft.with(|s| s.preserve_times)
                        on:change=move |ev| { let on = event_target_checked(&ev); draft.update(|s| s.preserve_times = on) }/>{tr!("Conserver les dates de modification", "Preserve modification times")}</label>
                    <label class="check"><input type="checkbox" prop:checked=move || draft.with(|s| s.confirm_delete)
                        on:change=move |ev| { let on = event_target_checked(&ev); draft.update(|s| s.confirm_delete = on) }/>{tr!("Confirmer avant de supprimer", "Confirm before deleting")}</label>
                    <label class="check"><input type="checkbox" prop:checked=move || draft.with(|s| s.double_click_transfers)
                        on:change=move |ev| { let on = event_target_checked(&ev); draft.update(|s| s.double_click_transfers = on) }/>{tr!("Double-clic sur un fichier : le transférer", "Double-click on a file: transfer it")}</label>
                    <p class="muted">{tr!("Stockage des secrets : ", "Secret storage: ")} {backend}</p>
                </div>
                <div class="modal-buttons">
                    <button type="button" on:click=move |_| close()>{tr!("Annuler", "Cancel")}</button>
                    <button type="submit" class="primary">"OK"</button>
                </div>
            </form>
        </Modal>
    }
    .into_any()
}

fn about_view(id: u64) -> AnyView {
    let c = ctx();
    let info = c.info.get_untracked();
    let close = move || c.close_dialog(id);
    let ok = NodeRef::<html::Button>::new();
    autofocus(ok);
    view! {
        <Modal title=trf!("À propos de {}", "About {}", info.name) on_close=Callback::new(move |_| close())>
            <div class="modal-body about">
                {logo(56)}
                <div>
                    <h3>{format!("{} {}", info.name, info.version)}</h3>
                    <p>{tr!("Client SCP/SFTP graphique pour Linux et Windows, inspiré de FileZilla.", "Graphical SCP/SFTP client for Linux and Windows, inspired by FileZilla.")}</p>
                    <ul>
                        <li>{tr!("Authentification ", "Simultaneous ")} <b>{tr!("clé SSH + code TOTP", "SSH key + TOTP code")}</b> {tr!(" simultanée (", " authentication (")}
                            <code>"AuthenticationMethods publickey,keyboard-interactive"</code>")"</li>
                        <li>{tr!("Codes TOTP saisis ou générés automatiquement depuis le secret", "TOTP codes entered or generated automatically from the secret")}</li>
                        <li>{tr!("Transferts parallèles multiplexés sur une seule connexion authentifiée", "Parallel transfers multiplexed over a single authenticated connection")}</li>
                        <li>{tr!("Protocole SCP natif ou SFTP, navigation SFTP ou shell", "Native SCP or SFTP protocol, SFTP or shell browsing")}</li>
                    </ul>
                    <p class="muted">"Rust · Tauri · Leptos · russh"</p>
                </div>
            </div>
            <div class="modal-buttons">
                <button type="button" class="primary" node_ref=ok on:click=move |_| close()>{tr!("Fermer", "Close")}</button>
            </div>
        </Modal>
    }
    .into_any()
}
