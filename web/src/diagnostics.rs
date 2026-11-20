//! The browser's alpha diagnostics (docs/DIAGNOSTICS_CONTRACT.md section 5):
//! the same records the desktop sends (`log` at warn and error from every
//! target and at info from the sync targets, `system`, `panic`), scrubbed in
//! the page, queued in `localStorage` (at most [`MAX_QUEUED_BYTES`]) and sent
//! every minute to the accounts service's `/api/v1/diagnostics/records`,
//! which adds the send key; no page carries it. Settings > Diagnostics turns
//! it off (`localStorage` `pimble.diagnostics`), and the page keeps the last
//! [`RECENT_LINES`] log lines for Help > "Send Diagnostics...".

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use pimble_core::diagnostics::Scrubber;
use serde_json::{json, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestCredentials, RequestInit, Response};

const SETTINGS_KEY: &str = "pimble.diagnostics";
const INSTALL_KEY: &str = "pimble.diagnostics.install";
const QUEUE_KEY: &str = "pimble.diagnostics.queue";
const MAX_QUEUED_BYTES: usize = 1 << 20;
const RECENT_LINES: usize = 3000;
const MAX_WARNINGS: u32 = 500;
const MAX_INFO: u32 = 2000;
const SAME_MESSAGE: u32 = 3;
const EVERY_MS: i32 = 60_000;
const INFO_TARGETS: &[&str] = &["pimble_web::vault", "pimble_web::backend", "pimble_web::endpoints", "pimble_app::events", "pimble_client"];

struct State {
    install: String,
    session: String,
    seq: u64,
    queue: Vec<String>,
    queued_bytes: usize,
    recent: VecDeque<String>,
    seen: HashMap<String, u32>,
    warnings: u32,
    infos: u32,
    scrubber: Scrubber,
    account: Option<String>,
    enabled: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The saved choice: on, never told, when nothing is saved.
pub fn settings() -> (bool, bool) {
    let saved: Option<Value> = storage().and_then(|s| s.get_item(SETTINGS_KEY).ok().flatten()).and_then(|t| serde_json::from_str(&t).ok());
    match saved {
        Some(v) => (v["enabled"].as_bool().unwrap_or(true), v["noticed"].as_bool().unwrap_or(false)),
        None => (true, false),
    }
}

/// Save the choice; off deletes what waits and stops recording.
pub fn save_settings(enabled: bool, noticed: bool) -> Result<(), String> {
    let s = storage().ok_or_else(|| "this browser keeps no settings for the page".to_string())?;
    s.set_item(SETTINGS_KEY, &json!({ "enabled": enabled, "noticed": noticed }).to_string()).map_err(|_| "the setting could not be saved".to_string())?;
    if !enabled {
        let _ = s.remove_item(QUEUE_KEY);
        STATE.with(|st| {
            if let Some(st) = st.borrow_mut().as_mut() {
                st.enabled = false;
                st.queue.clear();
                st.queued_bytes = 0;
            }
        });
    }
    Ok(())
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    let _ = getrandom::getrandom(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn install_id() -> String {
    let Some(s) = storage() else { return random_hex(16) };
    if let Some(id) = s.get_item(INSTALL_KEY).ok().flatten().filter(|id| id.len() == 32) {
        return id;
    }
    let id = random_hex(16);
    let _ = s.set_item(INSTALL_KEY, &id);
    id
}

/// This page's install number, while diagnostics record.
pub fn install() -> Option<String> {
    STATE.with(|st| st.borrow().as_ref().filter(|s| s.enabled).map(|s| s.install.clone()))
}

/// Start, once, before the subscriber with [`Layer`] is installed: what an
/// earlier page queued is picked up, and a `system` record says what this
/// browser is.
pub fn start() {
    let (enabled, _) = settings();
    let queue: Vec<String> = if enabled {
        storage().and_then(|s| s.get_item(QUEUE_KEY).ok().flatten()).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default()
    } else {
        Vec::new()
    };
    let queued_bytes = queue.iter().map(String::len).sum();
    STATE.with(|st| {
        *st.borrow_mut() = Some(State {
            install: install_id(),
            session: random_hex(16),
            seq: 0,
            queue,
            queued_bytes,
            recent: VecDeque::new(),
            seen: HashMap::new(),
            warnings: 0,
            infos: 0,
            scrubber: Scrubber::new(None),
            account: None,
            enabled,
        })
    });
    let agent = web_sys::window().and_then(|w| w.navigator().user_agent().ok()).unwrap_or_default();
    record("system", json!({ "program": "web", "user_agent": agent }));
    let earlier = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
        let message = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_else(|| "(not a string)".into());
        record("panic", json!({ "message": message, "location": location, "thread": "page" }));
        save_queue();
        earlier(info);
    }));
    wasm_bindgen_futures::spawn_local(async {
        // The first send soon, for what an earlier page left.
        crate::util::sleep_ms(10_000).await;
        loop {
            send_waiting().await;
            crate::util::sleep_ms(EVERY_MS).await;
        }
    });
}

/// The signed-in account's id, carried by every record from now on.
pub fn set_account(account: Option<String>) {
    STATE.with(|st| {
        if let Some(st) = st.borrow_mut().as_mut() {
            st.account = account;
        }
    });
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Queue one record of `kind`, its strings scrubbed.
pub fn record(kind: &str, data: Value) {
    STATE.with(|st| {
        let mut st = st.borrow_mut();
        let Some(st) = st.as_mut().filter(|s| s.enabled) else { return };
        let data = scrub_value(&st.scrubber, data);
        let mut line = json!({
            "v": 1, "install": st.install, "session": st.session, "seq": st.seq, "ts": now(),
            "app": env!("CARGO_PKG_VERSION"), "os": "web", "kind": kind, "data": data,
        });
        st.seq += 1;
        if let Some(account) = &st.account {
            line["account"] = Value::String(account.clone());
        }
        let line = line.to_string();
        st.queued_bytes += line.len();
        st.queue.push(line);
        while st.queued_bytes > MAX_QUEUED_BYTES && !st.queue.is_empty() {
            let dropped = st.queue.remove(0);
            st.queued_bytes -= dropped.len();
        }
    });
}

fn scrub_value(s: &Scrubber, value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(s.scrub(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| scrub_value(s, v)).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, scrub_value(s, v))).collect()),
        other => other,
    }
}

fn save_queue() {
    let text = STATE.with(|st| st.borrow().as_ref().filter(|s| s.enabled).map(|s| s.queue.join("\n")));
    if let (Some(s), Some(text)) = (storage(), text) {
        let _ = s.set_item(QUEUE_KEY, &text);
    }
}

/// POST `body` to the accounts service, as the session cookie allows.
async fn post(path: &str, content_type: &str, body: String) -> Result<String, String> {
    let window = web_sys::window().ok_or("no window")?;
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_credentials(RequestCredentials::Include);
    init.set_body(&JsValue::from_str(&body));
    let headers = Headers::new().map_err(|_| "no headers")?;
    let _ = headers.set("content-type", content_type);
    init.set_headers(&headers);
    let request = Request::new_with_str_and_init(path, &init).map_err(|_| "the request could not be made")?;
    let response: Response = JsFuture::from(window.fetch_with_request(&request)).await.map_err(|_| "Pimble Cloud could not be reached")?.dyn_into().map_err(|_| "no response")?;
    let text = JsFuture::from(response.text().map_err(|_| "no body")?).await.ok().and_then(|t| t.as_string()).unwrap_or_default();
    if response.ok() {
        Ok(text)
    } else {
        Err(format!("{} {}", response.status(), text.trim()))
    }
}

async fn send_waiting() {
    let batch: Vec<String> = STATE.with(|st| st.borrow().as_ref().filter(|s| s.enabled).map(|s| s.queue.clone()).unwrap_or_default());
    if batch.is_empty() {
        return;
    }
    let sent = post("/api/v1/diagnostics/records", "application/x-ndjson", batch.join("\n")).await;
    // Taken (or refused for good: unreadable, too big): dropped. Anything
    // else waits for the next minute.
    let done = match &sent {
        Ok(_) => true,
        Err(e) => e.starts_with("400") || e.starts_with("413"),
    };
    if done {
        STATE.with(|st| {
            if let Some(st) = st.borrow_mut().as_mut() {
                let n = batch.len().min(st.queue.len());
                let gone: usize = st.queue.drain(..n).map(|l| l.len()).sum();
                st.queued_bytes = st.queued_bytes.saturating_sub(gone);
            }
        });
    }
    save_queue();
}

/// Help > "Send Diagnostics...": this page's recent log lines, the stores
/// the window knows, and the note; the report's code.
pub async fn send_report(note: String, stores: Value) -> Result<String, String> {
    let body = STATE.with(|st| {
        let st = st.borrow();
        let st = st.as_ref()?;
        let log: Vec<String> = st.recent.iter().cloned().collect();
        let mut body = json!({
            "v": 1, "install": st.install, "session": st.session, "app": env!("CARGO_PKG_VERSION"), "os": "web", "ts": now(),
            "note": st.scrubber.scrub(&note),
            "logs": [{ "name": "page.log", "text": log.join("\n") }],
            "stores": scrub_value(&st.scrubber, stores),
        });
        if let Some(account) = &st.account {
            body["account"] = Value::String(account.clone());
        }
        Some(body.to_string())
    });
    let body = body.ok_or_else(|| "Diagnostics did not start on this page.".to_string())?;
    let text = post("/api/v1/diagnostics/reports", "application/json", body).await.map_err(|e| format!("Pimble Cloud did not take the report ({e})."))?;
    serde_json::from_str::<Value>(&text).ok().and_then(|v| v["report"].as_str().map(str::to_string)).ok_or_else(|| "Pimble Cloud answered without a report code.".to_string())
}

fn recorded(n: u32) -> bool {
    n <= SAME_MESSAGE || (n >= 10 && {
        let mut m = n;
        while m >= 10 && m % 10 == 0 {
            m /= 10;
        }
        m == 1
    })
}

/// The `tracing` layer beside the console's.
pub struct Layer;

impl<S: Subscriber> tracing_subscriber::Layer<S> for Layer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let target = meta.target();
        if target.starts_with("pimble_web::diagnostics") || *meta.level() > Level::INFO {
            return;
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        let line = STATE.with(|st| {
            let mut st = st.borrow_mut();
            let st = st.as_mut()?;
            let line = st.scrubber.scrub(&format!("{} {} {target}: {}", now(), meta.level(), message.0));
            st.recent.push_back(line);
            if st.recent.len() > RECENT_LINES {
                st.recent.pop_front();
            }
            if !st.enabled {
                return None;
            }
            let warning = *meta.level() <= Level::WARN;
            if !warning && !INFO_TARGETS.iter().any(|t| target == *t || target.starts_with(&format!("{t}::"))) {
                return None;
            }
            let key = format!("{target} {}", message.0.chars().filter(|c| !c.is_ascii_digit()).collect::<String>());
            let n = st.seen.entry(key).or_insert(0);
            *n += 1;
            let n = *n;
            if !recorded(n) {
                return None;
            }
            let count = if warning { &mut st.warnings } else { &mut st.infos };
            *count += 1;
            if *count > if warning { MAX_WARNINGS } else { MAX_INFO } {
                return None;
            }
            let mut text = message.0.clone();
            if n > SAME_MESSAGE {
                let _ = write!(text, " (seen {n} times)");
            }
            Some(json!({ "level": meta.level().to_string(), "target": target, "message": text }))
        });
        if let Some(data) = line {
            record("log", data);
        }
    }
}

struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            let _ = write!(self.0, " {}={value}", field.name());
        }
    }
}

/// What Settings > Diagnostics reads and saves here.
pub fn hooks() -> pimble_app::diagnostics_ui::Hooks {
    pimble_app::diagnostics_ui::Hooks {
        settings,
        save: save_settings,
        install,
        available: || true,
    }
}
