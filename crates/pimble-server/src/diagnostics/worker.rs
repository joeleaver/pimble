//! The browser's worker's recorder (docs/ONE_CODEBASE_CONTRACT.md section 6,
//! item 10): the same functions the desktop's server calls (`ids`,
//! `set_account`, `record`, the `tracing` [`Layer`]), feeding one
//! [`Recorder`] in this worker's memory; [`run`] moves what it records onto
//! the server's disk every [`EVERY`] and sends it to the accounts service as
//! the signed-in account. The page says whether it is on and which install
//! it is (`setDiagnostics`); until it has, records wait in memory.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use pimble_core::diagnostics::timestamp;
use serde_json::{json, Value};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;

use super::on_disk::{records_url, Queue, Recorder, MAX_QUEUED_BYTES};
use super::{severity, Message};
use crate::RpcHandler;

/// How often what was recorded is written and sent.
pub const EVERY: Duration = Duration::from_secs(60);
/// The first send soon after start, for what an earlier run left.
const FIRST: Duration = Duration::from_secs(10);
/// The program a worker's records say they come from (the page's say `web`).
pub const PROGRAM: &str = "worker";

struct State {
    recorder: Recorder,
    /// The page's install number, once it has said it.
    install: Option<String>,
    /// Where the queue is, once the server has started.
    dir: Option<PathBuf>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { recorder: Recorder::new(env!("CARGO_PKG_VERSION"), "web"), install: None, dir: None });
}

fn now() -> String {
    timestamp(chrono::Utc::now())
}

/// This install's id and the worker's session, once the page has told the
/// server diagnostics are on.
pub fn ids() -> Option<(String, String)> {
    STATE.with(|s| {
        let s = s.borrow();
        let install = s.install.clone().filter(|_| s.recorder.enabled())?;
        Some((install, s.recorder.session().to_string()))
    })
}

/// The signed-in account's id, carried by every record from now on.
pub fn set_account(account: Option<String>) {
    STATE.with(|s| s.borrow_mut().recorder.set_account(account));
}

/// Queue one record of `kind`; `data`'s strings are scrubbed.
pub fn record(kind: &str, data: Value) {
    let ts = now();
    STATE.with(|s| s.borrow_mut().recorder.record(kind, data, ts));
}

/// The worker's recent log, for a report.
pub fn recent_log() -> String {
    STATE.with(|s| s.borrow().recorder.recent())
}

/// The `tracing` layer that keeps the worker's recent log and records what
/// diagnostics record (install it beside the console's).
pub struct Layer;

impl<S: Subscriber> tracing_subscriber::Layer<S> for Layer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if *meta.level() > tracing::Level::INFO || meta.target().starts_with("pimble_server::diagnostics") {
            return;
        }
        let mut message = Message::default();
        event.record(&mut message);
        let ts = now();
        // Never inside a borrow: a record made while one is held (a log line
        // from inside this module) would be a second borrow.
        let _ = STATE.try_with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                s.recorder.log(&meta.level().to_string(), severity(meta.level(), meta.target()), meta.target(), &message.0, ts);
            }
        });
    }
}

/// What the page says (`setDiagnostics`): on or off, and its install number.
/// Off drops what waits, in memory and on the disk.
pub async fn configure(handler: &RpcHandler, enabled: bool, install: Option<String>) {
    let dir = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.recorder.set_enabled(enabled);
        s.dir.clone()
    });
    let Some(dir) = dir else { return };
    let queue = Queue::new(handler.disk().clone(), dir);
    let install = queue.install(install.as_deref()).await;
    let first = STATE.with(|s| s.borrow_mut().install.replace(install).is_none());
    if !enabled {
        queue.clear().await;
    } else if first {
        record_system();
    }
}

/// Which program this session is, once, when diagnostics are first on (the
/// page's own `system` record says what the browser is).
fn record_system() {
    record("system", json!({ "program": PROGRAM, "host": crate::local::host_kind() }));
}

/// Move what was recorded onto the disk under `dir` and send what waits,
/// every [`EVERY`], for as long as the server runs.
pub async fn run(handler: RpcHandler, dir: PathBuf) {
    STATE.with(|s| s.borrow_mut().dir = Some(dir.clone()));
    let queue = Queue::new(handler.disk().clone(), dir);
    handler.runtime().sleep(FIRST).await;
    loop {
        pump(&handler, &queue).await;
        handler.runtime().sleep(EVERY).await;
    }
}

async fn pump(handler: &RpcHandler, queue: &Queue) {
    let taken = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let install = s.install.clone().filter(|_| s.recorder.enabled())?;
        let session = s.recorder.session().to_string();
        Some((s.recorder.take(&install), session))
    });
    let Some((lines, session)) = taken else { return };
    let millis = handler.runtime().wall().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    if let Err(e) = queue.append(&lines, millis, &session).await {
        tracing::debug!("diagnostic records not written: {e}");
    }
    queue.cap(MAX_QUEUED_BYTES).await;
    // Signed out (or the session ran out): they wait for the next sign-in.
    let Some(account) = handler.keystore().account().await.filter(|_| !handler.keystore().session_expired()) else { return };
    queue.send(handler.http(), &records_url(&account.url), Some(&account.session)).await;
}

/// Help > "Send Diagnostics...": the page's `logs`, this worker's recent log,
/// the note and the stores, sent as the signed-in account. Sent whether or
/// not records are: pressing Send is the asking.
pub async fn send_report(handler: &RpcHandler, note: &str, stores: Value, logs: Vec<(String, String)>) -> Result<String, String> {
    let account = handler.keystore().account().await.ok_or_else(|| "Sign in to send diagnostics from this browser.".to_string())?;
    let dir = STATE.with(|s| s.borrow().dir.clone());
    let queue_install = match dir {
        Some(dir) => Queue::new(handler.disk().clone(), dir).install(None).await,
        None => pimble_core::diagnostics::random_hex(16),
    };
    let body = STATE.with(|s| {
        let s = s.borrow();
        let install = s.install.clone().unwrap_or(queue_install);
        s.recorder.report(&install, &now(), note, logs, "worker.log", stores)
    });
    super::on_disk::send_report(handler.http(), &account.url, &account.session, &body).await
}
