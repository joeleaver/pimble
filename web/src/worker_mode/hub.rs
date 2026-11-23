//! The page's side of `web/hub.js`: the leader tab, its worker, and the
//! connections every tab makes to the one server (docs/ONE_CODEBASE_CONTRACT.md
//! section 4).

use js_sys::{Promise, Reflect};
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::MessagePort;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = PimbleHub, js_name = start)]
    fn hub_start(options: &JsValue);
    #[wasm_bindgen(js_namespace = PimbleHub, js_name = connect)]
    fn hub_connect() -> Promise;
    #[wasm_bindgen(js_namespace = PimbleHub, js_name = control)]
    fn hub_control(cmd: &JsValue) -> Promise;
    #[wasm_bindgen(js_namespace = PimbleHub, js_name = status)]
    fn hub_status() -> JsValue;
    #[wasm_bindgen(js_namespace = PimbleHub, js_name = take)]
    fn hub_take(name: &str) -> Promise;
    #[wasm_bindgen(js_namespace = PimbleHub, js_name = leaveOffline)]
    fn hub_leave_offline() -> Promise;
}

/// The switch off: the service worker that kept this app's files for a page
/// opened with no network is unregistered and its caches deleted.
pub fn leave_offline() {
    let _ = hub_leave_offline();
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let o = js_sys::Object::new();
    for (k, v) in fields {
        let _ = Reflect::set(&o, &JsValue::from_str(k), v);
    }
    o.into()
}

fn message(e: JsValue) -> String {
    Reflect::get(&e, &JsValue::from_str("message")).ok().and_then(|m| m.as_string()).or_else(|| e.as_string()).unwrap_or_else(|| format!("{e:?}"))
}

/// Begin: every tab asks for the leader's lock; the one that gets it starts
/// the server in a worker, kept as `mode` says (`auto`: remembered when this
/// browser remembers itself). `inherited` is the device handed over from this
/// browser's `localStorage` (JSON), taken by stores the server first meets.
pub fn start(mode: &str, inherited: Option<String>) {
    hub_start(&object(&[("mode", mode.into()), ("inherited", inherited.map(JsValue::from).unwrap_or(JsValue::NULL))]));
}

/// A port whose other end reaches the server, wherever it runs.
pub async fn connect() -> Result<MessagePort, String> {
    let port = JsFuture::from(hub_connect()).await.map_err(message)?;
    port.dyn_into::<MessagePort>().map_err(|_| "the hub answered something that is not a port".to_string())
}

/// Keep the stores in this browser (`remember`) or in memory (`memory`): the
/// leader restarts its worker if it keeps them otherwise. Answers the mode
/// the server now runs in.
pub async fn keep(mode: &str) -> Result<String, String> {
    let value = JsFuture::from(hub_control(&object(&[("t", "restart".into()), ("mode", mode.into())]))).await.map_err(message)?;
    Ok(value.as_string().unwrap_or_default())
}

/// Forget this browser: the device key, then its files; the server starts
/// again in memory. Answers whether OPFS was empty after.
pub async fn forget() -> Result<bool, String> {
    let value = JsFuture::from(hub_control(&object(&[("t", "forget".into())]))).await.map_err(message)?;
    Ok(Reflect::get(&value, &JsValue::from_str("empty")).ok().and_then(|e| e.as_bool()).unwrap_or(false))
}

/// What this tab knows of the hub.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Status {
    pub leading: bool,
    /// `remembered` or `memory`, once the leader has said.
    pub mode: Option<String>,
}

pub fn status() -> Status {
    let value = hub_status();
    let get = |k: &str| Reflect::get(&value, &JsValue::from_str(k)).ok();
    Status { leading: get("leading").and_then(|v| v.as_bool()).unwrap_or(false), mode: get("mode").and_then(|v| v.as_string()) }
}

/// A turn at something only one tab does at a time (a Web Lock named
/// `name`): held until the answer is dropped.
pub async fn exclusive(name: &str) -> Turn {
    match JsFuture::from(hub_take(name)).await {
        Ok(release) => Turn(release.dyn_into::<js_sys::Function>().ok()),
        Err(_) => Turn(None),
    }
}

pub struct Turn(Option<js_sys::Function>);

impl Drop for Turn {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}
