//! The browser's worker's recorder (docs/ONE_CODEBASE_CONTRACT.md section 6,
//! item 10): the same functions the desktop's server calls (`ids`,
//! `set_account`, `record`, the `tracing` [`Layer`]), feeding one
//! [`Recorder`] in this worker's memory; [`run`] moves what it records onto
//! the server's disk every [`EVERY`] and sends it to the accounts service as
//! the signed-in account, each batch only as the account that recorded it
//! ([`WorkerDiagnostics`], which also keeps a pump and the page's say from
//! overtaking each other). The page says whether it is on and which install
//! it is (`setDiagnostics`); until it has, records wait in memory.

use std::path::PathBuf;
use std::time::Duration;

use pimble_core::diagnostics::timestamp;
use serde_json::{json, Value};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;

use super::on_disk::Queue;
use super::worker_core::{Applied, SendAs, WorkerDiagnostics};
use super::{severity, Message};
use crate::RpcHandler;

/// How often what was recorded is written and sent.
pub const EVERY: Duration = Duration::from_secs(60);
/// The first send soon after start, for what an earlier run left.
const FIRST: Duration = Duration::from_secs(10);
/// The program a worker's records say they come from (the page's say `web`).
pub const PROGRAM: &str = "worker";

thread_local! {
    /// The worker's one recorder, for the worker's life (its one thread).
    static DIAGNOSTICS: &'static WorkerDiagnostics = Box::leak(Box::new(WorkerDiagnostics::new(env!("CARGO_PKG_VERSION"), "web")));
}

fn diagnostics() -> &'static WorkerDiagnostics {
    DIAGNOSTICS.with(|d| *d)
}

fn now() -> String {
    timestamp(chrono::Utc::now())
}

/// This install's id and the worker's session, once the page has told the
/// server diagnostics are on.
pub fn ids() -> Option<(String, String)> {
    diagnostics().ids()
}

/// The signed-in account's id, carried by every record from now on.
pub fn set_account(account: Option<String>) {
    diagnostics().set_account(account);
}

/// Queue one record of `kind`; `data`'s strings are scrubbed.
pub fn record(kind: &str, data: Value) {
    diagnostics().record(kind, data, now());
}

/// The worker's recent log, for a report.
pub fn recent_log() -> String {
    diagnostics().recent_log()
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
        let _ = DIAGNOSTICS.try_with(|d| d.log(&meta.level().to_string(), severity(meta.level(), meta.target()), meta.target(), &message.0, ts));
    }
}

/// What the page says (`setDiagnostics`): on or off, and its install number.
/// Off drops what waits, in memory at once and on the disk once no pump is
/// running; a say that comes before the server has started is applied when
/// it starts.
pub async fn configure(handler: &RpcHandler, enabled: bool, install: Option<String>) {
    let applied = diagnostics().configure(handler.disk(), enabled, install, wall_millis(handler)).await;
    said(applied);
}

fn said(applied: Applied) {
    if applied == Applied::FirstOn {
        // Which program this session is, once, when diagnostics are first
        // on (the page's own `system` record says what the browser is).
        record("system", json!({ "program": PROGRAM, "host": crate::local::host_kind() }));
    }
}

/// Move what was recorded onto the disk under `dir` and send what waits,
/// every [`EVERY`], for as long as the server runs.
pub async fn run(handler: RpcHandler, dir: PathBuf) {
    said(diagnostics().attach(handler.disk(), dir).await);
    handler.runtime().sleep(FIRST).await;
    loop {
        pump(&handler).await;
        handler.runtime().sleep(EVERY).await;
    }
}

/// The wall time now, in the milliseconds batches and offs are named by.
fn wall_millis(handler: &RpcHandler) -> u128 {
    handler.runtime().wall().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// One pump as the signed-in account, if one is and its session holds.
async fn pump(handler: &RpcHandler) {
    let millis = wall_millis(handler);
    let account = handler.keystore().account().await.filter(|_| !handler.keystore().session_expired());
    let send_as = account.as_ref().map(|a| SendAs { url: &a.url, session: &a.session, user_id: &a.user_id });
    diagnostics().pump(handler.disk(), handler.http(), handler.runtime(), send_as, millis).await;
}

/// Help > "Send Diagnostics...": the page's `logs`, this worker's recent log,
/// the note and the stores, sent as the signed-in account. Sent whether or
/// not records are: pressing Send is the asking.
pub async fn send_report(handler: &RpcHandler, note: &str, stores: Value, logs: Vec<(String, String)>) -> Result<String, String> {
    let account = handler.keystore().account().await.ok_or_else(|| "Sign in to send diagnostics from this browser.".to_string())?;
    let queue_install = match diagnostics().dir() {
        Some(dir) => Queue::new(handler.disk().clone(), dir).install(None).await,
        None => pimble_core::diagnostics::random_hex(16),
    };
    let body = diagnostics().report(queue_install, &now(), note, logs, stores);
    super::on_disk::send_report(handler.http(), &account.url, &account.session, &body).await
}
