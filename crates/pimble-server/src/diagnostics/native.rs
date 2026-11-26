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

/// Why [`save_settings`] did not end where it was asked to.
#[derive(Debug)]
pub enum SaveError {
    /// What was recorded could not all be deleted. An off is saved and in
    /// force regardless (recording has stopped, and each start while off
    /// tries again); an on is not saved, and diagnostics stay off.
    RecordsKept(std::io::Error),
    /// The choice could not be saved; the one before it stands.
    NotSaved(std::io::Error),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::RecordsKept(e) => write!(f, "what diagnostics recorded could not all be deleted ({e})"),
            SaveError::NotSaved(e) => write!(f, "the diagnostics setting could not be saved ({e})"),
        }
    }
}

impl std::error::Error for SaveError {}

/// Save the choice; turning it off deletes what waits and stops recording at
/// once, turning it on starts the next time a program starts. An off that
/// could not delete everything waiting is saved and answers
/// [`SaveError::RecordsKept`] (each start while off tries again); an on after
/// an off first deletes whatever that off left, and stays off with
/// [`SaveError::RecordsKept`] when it cannot, so nothing recorded before an
/// off is ever sent.
pub fn save_settings(settings: &Settings) -> Result<(), SaveError> {
    let path = settings_path().ok_or_else(|| SaveError::NotSaved(std::io::Error::other("no config folder")))?;
    if settings.enabled && !self::settings().enabled {
        if let Some(dir) = queue_dir() {
            clear_queue(&dir).map_err(SaveError::RecordsKept)?;
        }
    }
    let written = match path.parent() {
        Some(dir) => fs::create_dir_all(dir),
        None => Ok(()),
    }
    .and_then(|()| Ok(serde_json::to_vec_pretty(settings)?))
    .and_then(|bytes| fs::write(&path, bytes));
    written.map_err(SaveError::NotSaved)?;
    if !settings.enabled {
        ENABLED.store(false, Ordering::SeqCst);
        if let Some(dir) = queue_dir() {
            clear_queue(&dir).map_err(SaveError::RecordsKept)?;
        }
    }
    Ok(())
}

/// Delete the queue in `dir`, everything in it: a folder that is not there
/// holds nothing.
fn clear_queue(dir: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
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
            // What an off could not delete; nothing is sent while off, and
            // an on deletes it first.
            if let Err(e) = clear_queue(&dir) {
                tracing::debug!("diagnostics are off and what they kept is not deleted yet: {e}");
            }
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

/// Permissions are how a test makes a removal fail, so these run on Unix.
#[cfg(all(test, unix))]
mod tests {
    use super::clear_queue;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_queue_that_cannot_be_deleted_says_so() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("diagnostics");
        clear_queue(&dir).expect("a queue that is not there is cleared");
        let inner = dir.join("held");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("records-1.ndjson"), "{}").unwrap();
        // A folder whose entries cannot be removed.
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o500)).unwrap();
        let refused = clear_queue(&dir);
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(refused.is_err(), "the failure is answered, not dropped");
        assert!(inner.join("records-1.ndjson").exists());
        clear_queue(&dir).expect("once it can, the queue goes");
        assert!(!dir.exists());
    }
}
