//! Alpha diagnostics (docs/DIAGNOSTICS_CONTRACT.md): what this computer's
//! Pimble sends so that a tester's problem can be read from both ends.
//!
//! On unless the person turned it off (`<config dir>/pimble/diagnostics.json`,
//! `{"enabled": false}`), and only in a build that carries the send key
//! (`PIMBLE_DIAGNOSTICS_KEY` when it was built, or at run time). Every record
//! goes through [`pimble_core::diagnostics::Scrubber`] before it is queued:
//! the home folder is `~`, email addresses, secrets and known titles are
//! taken out. Queued as NDJSON files under `<data dir>/pimble/diagnostics/`,
//! sent every minute (gzip, `POST /v1/records`), each file deleted once the
//! server has it; at most [`MAX_QUEUED_BYTES`] wait, the oldest dropped first.
//! Turned off, the queue is deleted and nothing is recorded.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pimble_core::diagnostics::Scrubber;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;

/// Where records go unless `PIMBLE_DIAGNOSTICS_URL` says otherwise.
pub const DEFAULT_URL: &str = "https://pimble-diagnostics.jkbase.app";
/// The version of the record envelope.
const V: u32 = 1;
const FILE_BYTES: u64 = 256 * 1024;
const FILE_AGE: Duration = Duration::from_secs(60);
pub const MAX_QUEUED_BYTES: u64 = 20 << 20;
const EVERY: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// Warnings and errors recorded per session; the `info` lines of the sync
/// and sharing targets have a budget of their own.
const MAX_WARNINGS: u32 = 500;
const MAX_INFO: u32 = 2000;
/// The same message (numbers aside) is recorded this often, then at its
/// 10th, 100th, 1000th... time with the count.
const SAME_MESSAGE: u32 = 3;

/// The targets whose `info` lines are recorded: how syncing and sharing go.
const INFO_TARGETS: &[&str] = &[
    "pimble_server::vault_link",
    "pimble_server::sync_link",
    "pimble_server::share",
    "pimble_server::relay_face",
    "pimble_server::relay_tunnel",
    "pimble_server::handler",
    "pimble_server::local",
    "pimble_server::server",
    "pimble_app::events",
];

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

struct Recorder {
    tx: Mutex<Sender<Msg>>,
    install: String,
    session: String,
    seq: AtomicU64,
    app: String,
    scrubber: RwLock<Scrubber>,
}

enum Msg {
    Line(String),
    Flush(Sender<()>),
}

static RECORDER: OnceLock<Recorder> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static ACCOUNT: RwLock<Option<String>> = RwLock::new(None);

fn recorder() -> Option<&'static Recorder> {
    ENABLED.load(Ordering::Relaxed).then(|| RECORDER.get()).flatten()
}

/// The send key this build carries, or one given at run time.
fn send_key() -> Option<String> {
    std::env::var("PIMBLE_DIAGNOSTICS_KEY").ok().or_else(|| option_env!("PIMBLE_DIAGNOSTICS_KEY").map(str::to_string)).filter(|k| !k.trim().is_empty())
}

/// This install's id, the session's, and whether diagnostics are on in this
/// process.
pub fn ids() -> Option<(String, String)> {
    recorder().map(|r| (r.install.clone(), r.session.clone()))
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
    let install = install_id(&dir.join("install-id"));
    let session = random_hex(16);
    let (tx, rx) = mpsc::channel();
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let recorder = Recorder {
        tx: Mutex::new(tx),
        install: install.clone(),
        session: session.clone(),
        seq: AtomicU64::new(0),
        app: env!("CARGO_PKG_VERSION").to_string(),
        scrubber: RwLock::new(Scrubber::new(home.as_deref())),
    };
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
        let _ = fs::remove_file(dir.join(format!("running-{}", r.session)));
    }
}

fn flush(wait: Duration) {
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
    let data = match r.scrubber.read() {
        Ok(s) => scrub_value(&s, data),
        Err(_) => return,
    };
    let account = ACCOUNT.read().ok().and_then(|a| a.clone());
    let mut line = json!({
        "v": V,
        "install": r.install,
        "session": r.session,
        "seq": r.seq.fetch_add(1, Ordering::Relaxed),
        "ts": rfc3339(SystemTime::now()),
        "app": r.app,
        "os": std::env::consts::OS,
        "kind": kind,
        "data": data,
    });
    if let Some(account) = account {
        line["account"] = Value::String(account);
    }
    if let Ok(tx) = r.tx.lock() {
        let _ = tx.send(Msg::Line(line.to_string()));
    }
}

fn scrub_value(s: &Scrubber, value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(s.scrub(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| scrub_value(s, v)).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, scrub_value(s, v))).collect()),
        other => other,
    }
}

/// What the computer is, at start.
fn record_system(program: &'static str) {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    record(
        "system",
        json!({
            "program": program,
            "os_version": os_version(),
            "arch": std::env::consts::ARCH,
            "cores": cores,
            "ram_mb": ram_mb(),
            "host": crate::local::host_kind(),
        }),
    );
}

fn os_version() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = fs::read_to_string("/etc/os-release") {
            if let Some(line) = text.lines().find(|l| l.starts_with("PRETTY_NAME=")) {
                return line.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string();
            }
        }
        String::new()
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd").args(["/C", "ver"]).creation_flags(0x0800_0000).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        String::new()
    }
}

fn ram_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| t.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|kb| kb.parse::<u64>().ok()))
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

// ── Crashes ────────────────────────────────────────────────────────────

/// A run that ended without quitting shows at the next start (its `running`
/// file is still there), and a panic on any thread is recorded and sent at
/// once.
fn crash_watch(dir: &Path, program: &'static str, session: &str) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(last) = name.strip_prefix("running-") else { continue };
            let info = fs::read_to_string(entry.path()).unwrap_or_default();
            let (pid, was) = info.split_once(' ').unwrap_or(("", ""));
            // Another program of this computer that is still running (the
            // app beside an MCP) is no crash.
            if pid.parse::<u32>().is_ok_and(process_alive) {
                continue;
            }
            record("previous_run", json!({"session": last, "program": was, "message": "the last run ended without quitting (a crash, a kill, or the computer going off)"}));
            let _ = fs::remove_file(entry.path());
        }
    }
    let _ = fs::write(dir.join(format!("running-{session}")), format!("{} {program}", std::process::id()));
    let earlier = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
        let message = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_else(|| "(not a string)".into());
        let backtrace: String = std::backtrace::Backtrace::force_capture().to_string().chars().take(6000).collect();
        record("panic", json!({"message": message, "thread": thread, "location": location, "backtrace": backtrace}));
        if thread != "pimble-diagnostics" {
            flush(Duration::from_secs(3));
        }
        earlier(info);
    }));
}

fn process_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(target_os = "windows")]
    {
        // `tasklist` names the process when it runs; no console flashes up.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = pid;
        false
    }
}

// ── The log layer ─────────────────────────────────────────────────────

static WARNINGS: AtomicU32 = AtomicU32::new(0);
static INFOS: AtomicU32 = AtomicU32::new(0);
static SEEN: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

fn times_seen(target: &str, message: &str) -> u32 {
    let key = format!("{target} {}", message.chars().filter(|c| !c.is_ascii_digit()).collect::<String>());
    let mut seen = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    let n = seen.get_or_insert_with(HashMap::new).entry(key).or_insert(0);
    *n += 1;
    *n
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

/// Which budget a line counts against, if it is recorded at all.
fn budget(level: &Level, target: &str) -> Option<&'static AtomicU32> {
    if target.starts_with("pimble_server::diagnostics") {
        return None;
    }
    if *level <= Level::WARN {
        return Some(&WARNINGS);
    }
    (*level == Level::INFO && INFO_TARGETS.iter().any(|t| target == *t || target.starts_with(&format!("{t}::")))).then_some(&INFOS)
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
        let Some(count) = budget(meta.level(), target) else { return };
        let max = if std::ptr::eq(count, &WARNINGS) { MAX_WARNINGS } else { MAX_INFO };
        let mut message = Message(String::new());
        event.record(&mut message);
        let n = times_seen(target, &message.0);
        if !recorded(n) {
            return;
        }
        if n > SAME_MESSAGE {
            let _ = write!(message.0, " (seen {n} times)");
        }
        if count.fetch_add(1, Ordering::Relaxed) >= max {
            return;
        }
        record("log", json!({"level": meta.level().to_string(), "target": target, "message": message.0}));
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

// ── The queue and the uploader ─────────────────────────────────────────

struct Writer {
    dir: PathBuf,
    session: String,
    open: Option<PathBuf>,
    opened: Instant,
    uploader: Uploader,
}

impl Writer {
    fn run(mut self, rx: Receiver<Msg>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Msg::Line(line)) => self.append(&line),
                Ok(Msg::Flush(done)) => {
                    self.close();
                    self.uploader.send_waiting(Some(Duration::from_secs(3)));
                    let _ = done.send(());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if !ENABLED.load(Ordering::Relaxed) {
                return;
            }
            if self.open.is_some() && self.opened.elapsed() >= FILE_AGE {
                self.close();
            }
            if self.uploader.due() {
                self.uploader.send_waiting(None);
            }
        }
    }

    fn append(&mut self, line: &str) {
        let session = self.session.clone();
        let dir = self.dir.clone();
        let path = self.open.get_or_insert_with(|| {
            self.opened = Instant::now();
            dir.join(format!("open-{session}.ndjson"))
        });
        let written = OpenOptions::new().create(true).append(true).open(&*path).and_then(|mut f| writeln!(f, "{line}"));
        if written.is_ok() && fs::metadata(&*path).is_ok_and(|m| m.len() >= FILE_BYTES) {
            self.close();
        }
    }

    fn close(&mut self) {
        let Some(open) = self.open.take() else { return };
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let _ = fs::rename(&open, self.dir.join(format!("records-{millis}-{}.ndjson", self.session)));
        cap(&self.dir, MAX_QUEUED_BYTES);
    }
}

/// The oldest closed files go first once more than `max` wait.
fn cap(dir: &Path, max: u64) {
    let mut files = waiting(dir);
    let mut total: u64 = files.iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum();
    files.sort();
    for path in files {
        if total <= max {
            break;
        }
        let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// A file a program claimed to send and never finished (it crashed or was
/// killed mid-send) goes back to the queue after [`STRANDED`].
const STRANDED: Duration = Duration::from_secs(10 * 60);

fn reclaim(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `sending-<32 hex>-records-....ndjson`
        let Some(rest) = name.strip_prefix("sending-").and_then(|r| r.get(33..)) else { continue };
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age >= STRANDED);
        if old && rest.starts_with("records-") {
            let _ = fs::rename(entry.path(), dir.join(rest));
        }
    }
}

/// The closed files, oldest first (their names start with the time).
fn waiting(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with("records-") && n.ends_with(".ndjson")
        }))
        .collect();
    files.sort();
    files
}

struct Uploader {
    base: String,
    key: String,
    session: String,
    dir: PathBuf,
    next: Instant,
    backoff: Duration,
    runtime: Option<tokio::runtime::Runtime>,
    client: reqwest::Client,
}

enum Sent {
    Done,
    Later,
}

impl Uploader {
    fn new(base: String, key: String, session: String, dir: PathBuf) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().ok();
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default();
        // The first send soon after start, for what an earlier run left.
        Uploader { base, key, session, dir, next: Instant::now() + Duration::from_secs(10), backoff: EVERY, runtime, client }
    }

    fn due(&self) -> bool {
        Instant::now() >= self.next
    }

    /// Send every closed file. Several programs share the queue (the app and
    /// each `pimble-mcp`), so a file is first renamed to this session's
    /// `sending-` name, which only one of them can do.
    fn send_waiting(&mut self, budget: Option<Duration>) {
        reclaim(&self.dir);
        let started = Instant::now();
        let mut ok = true;
        for path in waiting(&self.dir) {
            if budget.is_some_and(|b| started.elapsed() >= b) {
                break;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let claimed = self.dir.join(format!("sending-{}-{name}", self.session));
            if fs::rename(&path, &claimed).is_err() {
                continue;
            }
            match self.send(&claimed) {
                Sent::Done => {
                    let _ = fs::remove_file(&claimed);
                }
                Sent::Later => {
                    let _ = fs::rename(&claimed, &path);
                    ok = false;
                    break;
                }
            }
        }
        self.backoff = if ok { EVERY } else { (self.backoff * 2).min(MAX_BACKOFF) };
        self.next = Instant::now() + self.backoff;
    }

    fn send(&self, path: &Path) -> Sent {
        let Ok(body) = fs::read(path) else { return Sent::Done };
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        if gz.write_all(&body).is_err() {
            return Sent::Done;
        }
        let Ok(gz) = gz.finish() else { return Sent::Done };
        let Some(runtime) = &self.runtime else { return Sent::Later };
        let request = self
            .client
            .post(format!("{}/v1/records", self.base))
            .header("Authorization", format!("Bearer {}", self.key))
            .header("Content-Type", "application/x-ndjson")
            .header("Content-Encoding", "gzip")
            .body(gz);
        // The request is made inside the runtime: its timeout is a tokio
        // timer, which panics when made outside one.
        match runtime.block_on(async move { request.send().await }) {
            Ok(r) if r.status().is_success() || matches!(r.status().as_u16(), 400 | 413) => Sent::Done,
            Ok(r) => {
                tracing::debug!("diagnostics upload refused ({}); trying later", r.status());
                Sent::Later
            }
            Err(e) => {
                tracing::debug!("diagnostics upload failed: {e}");
                Sent::Later
            }
        }
    }
}

// ── Help > Send Diagnostics... ──────────────────────────────────────────

/// The most of one log a report carries: its last 8 MB.
const REPORT_LOG_BYTES: usize = 8 << 20;

/// Send a report (docs/DIAGNOSTICS_CONTRACT.md section 4): the last two runs'
/// logs of the app and of an MCP-run server, scrubbed as records are, the
/// open stores as the window knows them (`stores`), and the person's `note`.
/// Sent whether or not diagnostics are on: pressing Send is the asking. The
/// answer is the report's code, for the person to tell us. Blocking: call it
/// off the UI thread.
pub fn send_report(note: &str, stores: Value) -> Result<String, String> {
    let key = send_key().ok_or_else(|| "This copy of Pimble cannot send diagnostics: it was built without the key (a development build).".to_string())?;
    let dir = queue_dir().ok_or_else(|| "There is no folder to keep this computer's install number in.".to_string())?;
    let _ = fs::create_dir_all(&dir);
    let install = install_id(&dir.join("install-id"));
    let session = ids().map(|(_, s)| s).unwrap_or_else(|| random_hex(16));
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let scrubber = RECORDER.get().and_then(|r| r.scrubber.read().ok().map(|s| s.clone())).unwrap_or_else(|| Scrubber::new(home.as_deref()));
    let mut logs = Vec::new();
    if let Some(logs_dir) = crate::logs::dir() {
        for name in ["pimble.log", "pimble.previous.log", "server.log", "server.previous.log"] {
            let Ok(bytes) = fs::read(logs_dir.join(name)) else { continue };
            let start = bytes.len().saturating_sub(REPORT_LOG_BYTES);
            let text = String::from_utf8_lossy(&bytes[start..]);
            let text: String = text.lines().map(|line| scrubber.scrub(line)).collect::<Vec<_>>().join("\n");
            logs.push(json!({ "name": name, "text": text }));
        }
    }
    let mut body = json!({
        "v": V,
        "install": install,
        "session": session,
        "app": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "ts": rfc3339(SystemTime::now()),
        "note": scrubber.scrub(note),
        "logs": logs,
        "stores": scrub_value(&scrubber, stores),
    });
    if let Some(account) = ACCOUNT.read().ok().and_then(|a| a.clone()) {
        body["account"] = Value::String(account);
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(body.to_string().as_bytes()).map_err(|e| e.to_string())?;
    let gz = gz.finish().map_err(|e| e.to_string())?;
    let base = std::env::var("PIMBLE_DIAGNOSTICS_URL").unwrap_or_else(|_| DEFAULT_URL.to_string());
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| e.to_string())?;
        let answer = client
            .post(format!("{base}/v1/reports"))
            .header("Authorization", format!("Bearer {key}"))
            .header("Content-Type", "application/json")
            .header("Content-Encoding", "gzip")
            .body(gz)
            .send()
            .await
            .map_err(|e| format!("Pimble's diagnostics server could not be reached ({e}). Try again when this computer is online."))?;
        let status = answer.status();
        let text = answer.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("Pimble's diagnostics server did not take the report ({status}: {}).", text.trim()));
        }
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["report"].as_str().map(str::to_string))
            .ok_or_else(|| "Pimble's diagnostics server answered without a report code.".to_string())
    })
}

// ── Small helpers ─────────────────────────────────────────────────────

fn install_id(path: &Path) -> String {
    if let Ok(id) = fs::read_to_string(path).map(|s| s.trim().to_string()) {
        if id.len() == 32 {
            return id;
        }
    }
    let id = random_hex(16);
    let _ = fs::write(path, &id);
    id
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(&mut buf[..]);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_message_is_recorded_three_times_then_at_tens() {
        let kept: Vec<u32> = (1..=1000).filter(|&n| recorded(n)).collect();
        assert_eq!(kept, [1, 2, 3, 10, 100, 1000]);
    }

    #[test]
    fn warnings_from_anywhere_and_info_from_sync_and_sharing() {
        assert!(budget(&Level::WARN, "wgpu_hal::vulkan").is_some());
        assert!(budget(&Level::ERROR, "pimble_app::editor").is_some());
        assert!(budget(&Level::INFO, "pimble_server::vault_link").is_some());
        assert!(budget(&Level::INFO, "pimble_server::share").is_some());
        assert!(budget(&Level::INFO, "pimble_plugin_host::registry").is_none());
        assert!(budget(&Level::DEBUG, "pimble_server::vault_link").is_none());
        assert!(budget(&Level::WARN, "pimble_server::diagnostics").is_none(), "never its own lines");
    }

    #[test]
    fn strings_in_a_record_are_scrubbed_at_every_depth() {
        let s = Scrubber::new(Some("/home/tester"));
        let v = scrub_value(&s, json!({"message": "at /home/tester/x for a@b.co", "list": ["/home/tester/y"], "n": 3}));
        assert_eq!(v, json!({"message": "at ~/x for <email>", "list": ["~/y"], "n": 3}));
    }

    /// Against a diagnostics server (`diagnostics/dev.sh`):
    /// `PIMBLE_DIAGNOSTICS_URL=http://127.0.0.1:8788 PIMBLE_DIAGNOSTICS_KEY=dev-key
    /// XDG_DATA_HOME=<scratch> cargo test -p pimble-server --lib report_round_trip -- --ignored`.
    /// Point `XDG_DATA_HOME` somewhere scratch: the report reads the logs there.
    #[test]
    #[ignore = "needs a diagnostics server"]
    fn report_round_trip() {
        let logs = crate::logs::dir().unwrap();
        fs::create_dir_all(&logs).unwrap();
        let home = dirs::home_dir().unwrap().to_string_lossy().into_owned();
        fs::write(logs.join("pimble.log"), format!("INFO opened '{home}/Notes.pimble' for tester@example.com\nWARN something odd\n")).unwrap();
        let code = send_report("The share stopped syncing.", json!({ "stores": [{ "name": "Notes" }] })).expect("a report code");
        assert_eq!(code.len(), 8, "{code}");
        eprintln!("REPORT {code}");
    }

    #[test]
    fn a_file_left_mid_send_goes_back_to_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let name = format!("sending-{}-records-001-s.ndjson", "a".repeat(32));
        let path = dir.path().join(&name);
        fs::write(&path, b"x").unwrap();
        reclaim(dir.path());
        assert!(path.exists(), "a send in progress is left alone");
        let old = SystemTime::now() - STRANDED - Duration::from_secs(1);
        fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();
        reclaim(dir.path());
        assert!(dir.path().join("records-001-s.ndjson").exists());
    }

    #[test]
    fn the_queue_drops_the_oldest_past_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fs::write(dir.path().join(format!("records-{i:03}-s.ndjson")), vec![b'x'; 100]).unwrap();
        }
        cap(dir.path(), 250);
        let left: Vec<String> = waiting(dir.path()).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["records-003-s.ndjson", "records-004-s.ndjson"]);
    }
}
