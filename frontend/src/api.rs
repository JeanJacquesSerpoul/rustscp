//! Pont vers le cœur Tauri : commandes (invoke) et événements.

use rustscp_shared::tr;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen)]
    async fn tauri_listen(event: &str, handler: &Closure<dyn FnMut(JsValue)>) -> JsValue;
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ApiError {
    #[serde(default)]
    pub kind: String,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn to_js<T: Serialize>(value: &T) -> JsValue {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .unwrap_or(JsValue::NULL)
}

fn to_error(err: JsValue) -> ApiError {
    if let Some(s) = err.as_string() {
        return ApiError {
            kind: "other".into(),
            message: s,
        };
    }
    serde_wasm_bindgen::from_value::<ApiError>(err.clone()).unwrap_or_else(|_| ApiError {
        kind: "other".into(),
        message: js_sys::JSON::stringify(&err)
            .map(String::from)
            .unwrap_or_else(|_| tr!("erreur inconnue", "unknown error").into()),
    })
}

/// Appelle une commande du cœur ; `args` est un objet JSON.
pub async fn call<R: DeserializeOwned>(cmd: &str, args: Value) -> Result<R, ApiError> {
    let res = tauri_invoke(cmd, to_js(&args)).await.map_err(to_error)?;
    serde_wasm_bindgen::from_value(res).map_err(|e| ApiError {
        kind: "decode".into(),
        message: e.to_string(),
    })
}

/// Variante sans résultat : les erreurs sont journalisées dans la console.
pub fn fire(cmd: &'static str, args: Value) {
    leptos::task::spawn_local(async move {
        if let Err(e) = call::<Value>(cmd, args).await {
            web_sys_log(&format!("{cmd} : {e}"));
        }
    });
}

pub fn web_sys_log(msg: &str) {
    let console = js_sys::Reflect::get(&js_sys::global(), &"console".into()).ok();
    if let Some(c) = console
        && let Ok(f) = js_sys::Reflect::get(&c, &"warn".into())
        && let Some(f) = f.dyn_ref::<js_sys::Function>()
    {
        let _ = f.call1(&c, &msg.into());
    }
}

#[derive(Deserialize)]
struct EventEnvelope<T> {
    payload: T,
}

/// Écoute un événement émis par le cœur (pour toute la durée de l'application).
pub fn listen<T: DeserializeOwned + 'static>(event: &'static str, handler: impl Fn(T) + 'static) {
    let closure =
        Closure::<dyn FnMut(JsValue)>::new(
            move |raw: JsValue| match serde_wasm_bindgen::from_value::<EventEnvelope<T>>(raw) {
                Ok(env) => handler(env.payload),
                Err(e) => web_sys_log(&format!("événement {event} illisible : {e}")),
            },
        );
    leptos::task::spawn_local(async move {
        tauri_listen(event, &closure).await;
        closure.forget();
    });
}
