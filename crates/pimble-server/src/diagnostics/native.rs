//! The desktop's recorder: one per process (the app, each `pimble-mcp`), all
//! of them sharing one queue folder and one install number.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use pimble_core::diagnostics::{random_hex, timestamp, valid_install, Envelope, LogBudget, Scrubber};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;

use super::queue::{Uploader, Writer};
use super::system::{crash_watch, record_system};
use super::{severity, Message};

/// Where records go unless `PIMBLE_DIAGNOSTICS_URL` says otherwise.
pub const DEFAULT_URL: &str = "https://pimble-diagnostics.jkbase.app";
pub const MAX_QUEUED_BYTES: u64 = 20 << 20;

/// The person's choice, and whether they have been told.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    /// The notice was shown (the app's first start since diagnostics came).
    #[serde(default)]
    pub noticed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, noticed: false }
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("pimble").join("diagnostics.json"))
}

/// The saved choice (on, never told, when nothing is saved).
pub fn settings() -> Settings {
    settings_path().and_then(|p| fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Save the choice; turning it off deletes what waits and stops recording at
/// once, turning it on starts the next time a program starts.
pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    let path = settings_path().ok_or_else(|| std::io::Error::other("no config folder"))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(settings)?)?;
    if !settings.enabled {
        ENABLED.store(false, Ordering::SeqCst);
        if let Some(dir) = queue_dir() {
            let _ = fs::remove_dir_all(dir);
        }
    }
    Ok(())
}

/// Where records wait.
pub fn queue_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("pimble").join("diagnostics"))
}

pub(super) struct Recorder {
    tx: Mutex<Sender<Msg>>,
    envelope: Envelope,
    seq: AtomicU64,
    pub(super) scrubber: RwLock<Scrubber>,
}

pub(super) enum Msg {
    Line(String),
    Flush(Sender<()>),
}

pub(super) static RECORDER: OnceLock<Recorder> = OnceLock::new();
pub(super) static ENABLED: AtomicBool = AtomicBool::new(false);
pub(super) static ACCOUNT: RwLock<Option<String>> = RwLock::new(None);
static BUDGET: Mutex<Option<LogBudget>> = Mutex::new(None);

fn recorder() -> Option<&'static Recorder> {
    ENABLED.load(Ordering::Relaxed).then(|| RECORDER.get()).flatten()
}

/// The send key this build carries, or one given at run time.
pub(super) fn send_key() -> Option<String> {
    std::env::var("PIMBLE_DIAGNOSTICS_KEY").ok().or_else(|| option_env!("PIMBLE_DIAGNOSTICS_KEY").map(str::to_string)).filter(|k| !k.trim().is_empty())
}

/// This install's id, the session's, and whether diagnostics are on in this
/// process.
pub fn ids() -> Option<(String, String)> {
    recorder().map(|r| (r.envelope.install.clone(), r.envelope.session.clone()))
}

/// Start recording in this process (`program`: `"app"` or `"mcp"`), when the
/// build carries a key and the person has not turned it off. Call once, early,
/// before the tracing subscriber with [`Layer`] is installed.
pub fn start(program: &'static str) {
    let Some(key) = send_key() else { return };
    if !settings().enabled {
        if let Some(dir) = queue_dir() {
            let _ = fs::remove_dir_all(dir);
        }
        return;
    }
    let Some(dir) = queue_dir() else { return };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let envelope = Envelope {
        install: install_id(&dir.join("install-id")),
        session: random_hex(16),
        app: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
    };
    let session = envelope.session.clone();
    let (tx, rx) = mpsc::channel();
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let recorder = Recorder { tx: Mutex::new(tx), envelope, seq: AtomicU64::new(0), scrubber: RwLock::new(Scrubber::new(home.as_deref())) };
    if RECORDER.set(recorder).is_err() {
        return;
    }
    ENABLED.store(true, Ordering::SeqCst);
    let base = std::env::var("PIMBLE_DIAGNOSTICS_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    let writer = Writer { dir: dir.clone(), session: session.clone(), open: None, opened: Instant::now(), uploader: Uploader::new(base, key, session.clone(), dir.clone()) };
    let _ = std::thread::Builder::new().name("pimble-diagnostics".into()).spawn(move || writer.run(rx));
    crash_watch(&dir, program, &session);
    record_system(program);
}

/// Stop for a clean quit: what is queued is sent (a few seconds at most), and
/// the run is marked as one that ended well.
pub fn quit() {
    flush(Duration::from_secs(4));
    if let (Some(r), Some(dir)) = (RECORDER.get(), queue_dir()) {
        let _ = fs::remove_file(dir.join(format!("running-{}", r.envelope.session)));
    }
}

pub(super) fn flush(wait: Duration) {
    let Some(r) = recorder() else { return };
    let (done, waited) = mpsc::channel();
    if r.tx.lock().map(|tx| tx.send(Msg::Flush(done)).is_ok()).unwrap_or(false) {
        let _ = waited.recv_timeout(wait);
    }
}

/// The signed-in account's id (a UUID, never the email), carried by every
/// record from now on; `None` when signed out.
pub fn set_account(account: Option<String>) {
    if let Ok(mut a) = ACCOUNT.write() {
        *a = account;
    }
}

/// A note title that must never be sent, should a log line name it.
pub fn forget_title(title: &str) {
    if let Some(r) = RECORDER.get() {
        if let Ok(mut s) = r.scrubber.write() {
            s.forget_title(title);
        }
    }
}

/// Queue one record of `kind`; `data`'s strings are scrubbed.
pub fn record(kind: &str, data: Value) {
    let Some(r) = recorder() else { return };
    let Ok(scrubber) = r.scrubber.read() else { return };
    let account = ACCOUNT.read().ok().and_then(|a| a.clone());
    let line = r.envelope.record(&scrubber, r.seq.fetch_add(1, Ordering::Relaxed), &timestamp(SystemTime::now().into()), kind, data, account.as_deref());
    if let Ok(tx) = r.tx.lock() {
        let _ = tx.send(Msg::Line(line));
    }
}

/// The `tracing` layer that records log lines (install it beside the
/// formatter).
pub struct Layer;

impl<S: Subscriber> tracing_subscriber::Layer<S> for Layer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let meta = event.metadata();
        let target = meta.target();
        let Some(severity) = severity(meta.level(), target) else { return };
        let mut message = Message::default();
        event.record(&mut message);
        let admitted = BUDGET.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert_with(LogBudget::default).admit(severity, target, &message.0);
        if let Some(text) = admitted {
            record("log", json!({"level": meta.level().to_string(), "target": target, "message": text}));
        }
    }
}

/// This computer's install number, made the first time.
pub(super) fn install_id(path: &Path) -> String {
    if let Ok(id) = fs::read_to_string(path).map(|s| s.trim().to_string()) {
        if valid_install(&id) {
            return id;
        }
    }
    let id = random_hex(16);
    let _ = fs::write(path, &id);
    id
}
