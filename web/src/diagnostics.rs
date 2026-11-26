//! The browser's alpha diagnostics (docs/DIAGNOSTICS_CONTRACT.md section 5):
//! the same records the desktop sends (`log` at warn and error from every
//! target and at info from the sync targets, `system`, `panic`), scrubbed in
//! the page, queued in `localStorage` (at most [`MAX_QUEUED_BYTES`]) and sent
//! every minute to the accounts service's `/api/v1/diagnostics/records`,
//! which adds the send key; no page carries it. Settings > Diagnostics turns
//! it off (`localStorage` `pimble.diagnostics`), and the page keeps the last
//! [`RECENT_LINES`] log lines for Help > "Send Diagnostics...".

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Write as _;

use pimble_core::diagnostics::{is_info_target, random_hex, timestamp, valid_install, Envelope, LogBudget, SavedChoice, Scrubber, Severity};
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
const EVERY_MS: i32 = 60_000;
/// The page's targets whose `info` lines are recorded: how its connections
/// go, and what the app makes of them (the worker's server records its own,
/// `pimble_core::diagnostics::INFO_TARGETS`).
const INFO_TARGETS: &[&str] = &["pimble_web::worker_mode", "pimble_app::events", "pimble_client"];

struct State {
    envelope: Envelope,
    seq: u64,
    queue: Vec<String>,
    queued_bytes: usize,
    recent: VecDeque<String>,
    budget: LogBudget,
    scrubber: Scrubber,
    account: Option<String>,
    enabled: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// Told when the choice is saved (worker mode tells its server).
    static ON_SAVE: RefCell<Option<fn()>> = const { RefCell::new(None) };
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The saved choice, with the clear it owes (`SavedChoice`).
fn saved_choice() -> SavedChoice {
    SavedChoice::parse(storage().and_then(|s| s.get_item(SETTINGS_KEY).ok().flatten()).as_deref())
}

/// The saved choice: on, never told, when nothing is saved.
pub fn settings() -> (bool, bool) {
    let choice = saved_choice();
    (choice.enabled, choice.noticed)
}

/// The tag of an off whose clear no server has confirmed yet, which every
/// worker this page connects to hears (`setDiagnostics` `clear_owed`).
pub fn clear_owed() -> Option<String> {
    saved_choice().clear_owed
}

/// The server confirmed the clear owed under `tag`; a later off's stays.
pub fn clear_confirmed(tag: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(SETTINGS_KEY, &saved_choice().confirmed(tag).to_json());
    }
}

/// Save the choice; off deletes what waits and stops recording, and notes
/// the clear it owes the server's queue beside the choice.
pub fn save_settings(enabled: bool, noticed: bool) -> Result<(), String> {
    let s = storage().ok_or_else(|| "this browser keeps no settings for the page".to_string())?;
    let choice = saved_choice().saved(enabled, noticed, || random_hex(8));
    s.set_item(SETTINGS_KEY, &choice.to_json()).map_err(|_| "the setting could not be saved".to_string())?;
    if !enabled {
        let _ = s.remove_item(QUEUE_KEY);
        STATE.with(|st| {
            if let Some(st) = st.borrow_mut().as_mut() {
                st.enabled = false;
                st.queue.clear();
                st.queued_bytes = 0;
            }
        });
    } else {
        STATE.with(|st| {
            if let Some(st) = st.borrow_mut().as_mut() {
                st.enabled = true;
            }
        });
    }
    if let Some(told) = ON_SAVE.with(|f| *f.borrow()) {
        told();
    }
    Ok(())
}

/// Call `told` whenever the choice is saved.
pub fn on_save(told: fn()) {
    ON_SAVE.with(|f| *f.borrow_mut() = Some(told));
}

fn install_id() -> String {
    let Some(s) = storage() else { return random_hex(16) };
    if let Some(id) = s.get_item(INSTALL_KEY).ok().flatten().filter(|id| valid_install(id)) {
        return id;
    }
    let id = random_hex(16);
    let _ = s.set_item(INSTALL_KEY, &id);
    id
}

/// This page's install number, while diagnostics record.
pub fn install() -> Option<String> {
    STATE.with(|st| st.borrow().as_ref().filter(|s| s.enabled).map(|s| s.envelope.install.clone()))
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
            envelope: Envelope { install: install_id(), session: random_hex(16), app: env!("CARGO_PKG_VERSION").to_string(), os: "web".to_string() },
            seq: 0,
            queue,
            queued_bytes,
            recent: VecDeque::new(),
            budget: LogBudget::default(),
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
    timestamp(chrono::Utc::now())
}

/// Queue one record of `kind`, its strings scrubbed.
pub fn record(kind: &str, data: Value) {
    STATE.with(|st| {
        let mut st = st.borrow_mut();
        let Some(st) = st.as_mut().filter(|s| s.enabled) else { return };
        let line = st.envelope.record(&st.scrubber, st.seq, &now(), kind, data, st.account.as_deref());
        st.seq += 1;
        st.queued_bytes += line.len();
        st.queue.push(line);
        while st.queued_bytes > MAX_QUEUED_BYTES && !st.queue.is_empty() {
            let dropped = st.queue.remove(0);
            st.queued_bytes -= dropped.len();
        }
    });
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

/// This page's recent log lines, oldest first: what a report carries as
/// `page.log` (handed to this browser's server, which adds its own).
pub fn recent_log() -> String {
    STATE.with(|st| st.borrow().as_ref().map(|s| s.recent.iter().cloned().collect::<Vec<_>>().join("\n")).unwrap_or_default())
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
        let mut message = Message::default();
        event.record(&mut message);
        let severity = if *meta.level() <= Level::WARN {
            Some(Severity::Warning)
        } else {
            is_info_target(target, INFO_TARGETS).then_some(Severity::Info)
        };
        let line = STATE.with(|st| {
            let mut st = st.borrow_mut();
            let st = st.as_mut()?;
            let line = st.scrubber.scrub(&format!("{} {} {target}: {}", now(), meta.level(), message.0));
            st.recent.push_back(line);
            if st.recent.len() > RECENT_LINES {
                st.recent.pop_front();
            }
            let severity = severity.filter(|_| st.enabled)?;
            let text = st.budget.admit(severity, target, &message.0)?;
            Some(json!({ "level": meta.level().to_string(), "target": target, "message": text }))
        });
        if let Some(data) = line {
            record("log", data);
        }
    }
}

/// A `tracing` event's message and fields as one line of text.
#[derive(Default)]
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
        save: |enabled, noticed| save_settings(enabled, noticed).map_err(pimble_app::diagnostics_ui::SaveFailed::NotSaved),
        install,
        available: || true,
    }
}
