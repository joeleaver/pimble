//! A server's diagnostics over the host's own seams (docs/ONE_CODEBASE_CONTRACT.md
//! section 6, item 10): what the server in a browser's worker records, held
//! in memory by [`Recorder`] as lines are logged, queued on the server's
//! [`Disk`] by [`Queue`], and sent through its [`Http`] to the accounts
//! service, which adds the send key (docs/DIAGNOSTICS_CONTRACT.md section 5).
//! Built for every target so that it is tested natively; the worker's
//! `diagnostics` module is what drives it.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;

use pimble_core::diagnostics::{may_send, random_hex, report_code, valid_install, Envelope, LogBudget, Scrubber, Severity};
use pimble_host::http::{Body, Http, HttpRequest, Method};
use pimble_host::Disk;
use serde_json::{json, Value};

/// The log lines a report carries: the last this many.
pub const RECENT_LINES: usize = 3000;
/// What waits in memory for the queue, at most; the oldest go first.
pub const MAX_PENDING_BYTES: usize = 1 << 20;
/// What waits on the disk, at most: a browser's storage is smaller than a
/// computer's, and a remembered browser's is sealed and counted against the
/// site.
pub const MAX_QUEUED_BYTES: u64 = 4 << 20;

/// The accounts service's endpoint for records.
pub fn records_url(service: &str) -> String {
    format!("{}/api/v1/diagnostics/records", service.trim_end_matches('/'))
}

/// The accounts service's endpoint for reports.
pub fn reports_url(service: &str) -> String {
    format!("{}/api/v1/diagnostics/reports", service.trim_end_matches('/'))
}


/// A record taken before this install's number is known: stamped when it
/// leaves for the queue.
struct Pending {
    seq: u64,
    ts: String,
    kind: String,
    data: Value,
    account: Option<String>,
    bytes: usize,
}

/// One session's records and recent log, in memory.
pub struct Recorder {
    session: String,
    app: String,
    os: String,
    seq: u64,
    scrubber: Scrubber,
    budget: LogBudget,
    account: Option<String>,
    enabled: bool,
    pending: VecDeque<Pending>,
    pending_bytes: usize,
    recent: VecDeque<String>,
}

impl Recorder {
    /// A new session of `app` on `os`, recording until told otherwise.
    pub fn new(app: &str, os: &str) -> Self {
        Recorder {
            session: random_hex(16),
            app: app.to_string(),
            os: os.to_string(),
            seq: 0,
            scrubber: Scrubber::new(None),
            budget: LogBudget::default(),
            account: None,
            enabled: true,
            pending: VecDeque::new(),
            pending_bytes: 0,
            recent: VecDeque::new(),
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Off drops what waits in memory and records nothing more; the recent
    /// log is kept for a report, which is sent whether or not records are.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.pending.clear();
            self.pending_bytes = 0;
        }
    }

    /// The signed-in account's id, carried by every record from now on.
    pub fn set_account(&mut self, account: Option<String>) {
        self.account = account;
    }

    /// Queue one record of `kind` (taken at `ts`), its strings scrubbed now.
    pub fn record(&mut self, kind: &str, data: Value, ts: String) {
        if !self.enabled {
            return;
        }
        let data = pimble_core::diagnostics::scrub_value(&self.scrubber, data);
        let bytes = data.to_string().len() + ts.len() + kind.len() + 200;
        self.pending.push_back(Pending { seq: self.seq, ts, kind: kind.to_string(), data, account: self.account.clone(), bytes });
        self.seq += 1;
        self.pending_bytes += bytes;
        while self.pending_bytes > MAX_PENDING_BYTES {
            let Some(dropped) = self.pending.pop_front() else { break };
            self.pending_bytes -= dropped.bytes;
        }
    }

    /// One log line at `level` (`"INFO"`, `"WARN"`...): kept for a report,
    /// and recorded when `severity` says it is one that is and the budget
    /// lets it.
    pub fn log(&mut self, level: &str, severity: Option<Severity>, target: &str, message: &str, ts: String) {
        let line = self.scrubber.scrub(&format!("{ts} {level} {target}: {message}"));
        self.recent.push_back(line);
        if self.recent.len() > RECENT_LINES {
            self.recent.pop_front();
        }
        let Some(severity) = severity.filter(|_| self.enabled) else { return };
        if let Some(text) = self.budget.admit(severity, target, message) {
            self.record("log", json!({ "level": level, "target": target, "message": text }), ts);
        }
    }

    /// Everything recorded since the last take, as record lines of
    /// `install`, one batch per account that was signed in when its records
    /// were made (`None`: no one was), in the order each first appears.
    pub fn take(&mut self, install: &str) -> Vec<(Option<String>, Vec<String>)> {
        let envelope = self.envelope(install);
        self.pending_bytes = 0;
        let mut batches: Vec<(Option<String>, Vec<String>)> = Vec::new();
        for p in self.pending.drain(..) {
            let line = envelope.record(&self.scrubber, p.seq, &p.ts, &p.kind, p.data, p.account.as_deref());
            match batches.iter_mut().find(|(account, _)| *account == p.account) {
                Some((_, lines)) => lines.push(line),
                None => batches.push((p.account, vec![line])),
            }
        }
        batches
    }

    /// The recent log, oldest line first.
    pub fn recent(&self) -> String {
        self.recent.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// A report's body (docs/DIAGNOSTICS_CONTRACT.md section 4): `logs`
    /// handed over by the client, then this server's recent log as
    /// `log_name`.
    pub fn report(&self, install: &str, ts: &str, note: &str, mut logs: Vec<(String, String)>, log_name: &str, stores: Value) -> Value {
        logs.push((log_name.to_string(), self.recent()));
        self.envelope(install).report(&self.scrubber, ts, note, logs, stores, self.account.as_deref())
    }

    fn envelope(&self, install: &str) -> Envelope {
        Envelope { install: install.to_string(), session: self.session.clone(), app: self.app.clone(), os: self.os.clone() }
    }
}

/// How a send went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// Every waiting file was taken (or refused for good) and deleted.
    All,
    /// One was not taken (no network, signed out, the service down): it and
    /// the ones after it wait for the next time.
    Stopped,
}

/// Records waiting on the server's disk: one NDJSON file per batch, named by
/// its place in the queue's own sequence (a number kept beside the batches
/// that only ever grows, never the wall clock, which can step back), so the
/// oldest go first and an off can name the batches from before it; and by
/// the account whose records it holds, so that it is only ever sent as that
/// account. One server holds a browser's disk at a time (section 4), so no
/// file is ever claimed by two senders.
pub struct Queue {
    disk: Disk,
    dir: PathBuf,
}

/// Whose records a batch holds, from its name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    /// Recorded while no one was signed in: sent as whoever signs in next.
    NoOne,
    /// Recorded while this account was signed in: sent only as it.
    Account(String),
    /// A name that says neither (no batch this version writes).
    Unknown,
}

/// A batch's name: `records-s<seq>-<session>-<owner>.ndjson`, `seq` its
/// number in the queue's sequence (20 digits, so names sort as numbers), the
/// owner `none` or `u` and the account id in hex (any id, and no `-` in it).
/// Versions before 2026-11-25 wrote `records-<wall millis>-...` (13 digits,
/// no `s`): those sort first, and every off clears them (see [`Written`]).
fn batch_name(seq: u64, session: &str, account: Option<&str>) -> String {
    let owner = match account {
        None => "none".to_string(),
        Some(id) => format!("u{}", id.bytes().map(|b| format!("{b:02x}")).collect::<String>()),
    };
    format!("records-s{seq:020}-{session}-{owner}.ndjson")
}

/// Where a batch stands in the queue's sequence, from its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Written {
    /// At this number.
    At(u64),
    /// Named by the wall time of a version before sequence numbers, or by
    /// nothing that can be read: before any off written down now, so every
    /// off clears it.
    BeforeAnyMark,
}

fn written(path: &std::path::Path) -> Written {
    let seq = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("records-s")?.split('-').next()?.parse::<u64>().ok());
    seq.map_or(Written::BeforeAnyMark, Written::At)
}

/// The file beside the batches holding the last number the sequence gave.
const SEQ_FILE: &str = "batch-seq";

fn owner_of(path: &std::path::Path) -> Owner {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return Owner::Unknown };
    let Some(stem) = name.strip_prefix("records-").and_then(|n| n.strip_suffix(".ndjson")) else { return Owner::Unknown };
    let parts: Vec<&str> = stem.split('-').collect();
    let [_seq, _session, owner] = parts.as_slice() else { return Owner::Unknown };
    if *owner == "none" {
        return Owner::NoOne;
    }
    let Some(hex) = owner.strip_prefix('u').filter(|h| !h.is_empty() && h.len() % 2 == 0) else { return Owner::Unknown };
    let bytes: Option<Vec<u8>> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect();
    match bytes.and_then(|b| String::from_utf8(b).ok()) {
        Some(id) => Owner::Account(id),
        None => Owner::Unknown,
    }
}

impl Queue {
    pub fn new(disk: Disk, dir: PathBuf) -> Self {
        Queue { disk, dir }
    }

    /// This install's number: `offered` when it is one (the page's, which
    /// outlives a browser that is not remembered), kept for next time; else
    /// the one kept, or a new one.
    pub async fn install(&self, offered: Option<&str>) -> String {
        let path = self.dir.join("install-id");
        let kept = self.disk.read_to_string(&path).await.ok().map(|s| s.trim().to_string()).filter(|id| valid_install(id));
        let id = match (offered.filter(|id| valid_install(id)), kept.clone()) {
            (Some(offered), _) => offered.to_string(),
            (None, Some(kept)) => kept,
            (None, None) => random_hex(16),
        };
        if kept.as_deref() != Some(id.as_str()) {
            let _ = self.disk.create_dir_all(&self.dir).await;
            let _ = self.disk.write(&path, id.as_bytes()).await;
        }
        id
    }

    /// The last number the queue's sequence gave, in this session or any
    /// before it: the one kept, or the highest a batch or an off written
    /// down names, whichever is greater (a number given and not yet kept, by
    /// a session that ended, is still never given twice while its batch is
    /// there); 0 for a queue that has never had one.
    pub async fn last_seq(&self) -> u64 {
        let kept = self.disk.read_to_string(self.dir.join(SEQ_FILE)).await.ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
        let waiting = self.waiting().await;
        let batches = waiting.iter().filter_map(|p| match written(p) {
            Written::At(seq) => Some(seq),
            Written::BeforeAnyMark => None,
        });
        let marks = self.clear_marks().await.into_iter().map(|(_, through)| through).filter(|through| *through != u64::MAX);
        batches.chain(marks).fold(kept, u64::max)
    }

    /// Keep `seq` as the last number the sequence gave, before any batch is
    /// named by it.
    pub async fn keep_seq(&self, seq: u64) -> io::Result<()> {
        self.disk.create_dir_all(&self.dir).await?;
        self.disk.write(self.dir.join(SEQ_FILE), seq.to_string().as_bytes()).await
    }

    /// Write `lines` as batch `seq` of `account`'s records (`None`: recorded
    /// while no one was signed in), taken by `session`.
    pub async fn append(&self, lines: &[String], seq: u64, session: &str, account: Option<&str>) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.disk.create_dir_all(&self.dir).await?;
        let mut text = lines.join("\n");
        text.push('\n');
        self.disk.write(self.dir.join(batch_name(seq, session, account)), text).await
    }

    /// The waiting batches, whoever's, oldest first (none when the folder
    /// cannot be listed).
    pub async fn waiting(&self) -> Vec<PathBuf> {
        self.listed().await.unwrap_or_default()
    }

    /// The waiting batches, oldest first, or why the folder could not be
    /// listed: a folder that is not there holds none.
    async fn listed(&self) -> io::Result<Vec<PathBuf>> {
        let entries = match self.disk.list(&self.dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut files: Vec<PathBuf> =
            entries.into_iter().filter(|e| !e.is_dir && e.name().is_some_and(|n| n.starts_with("records-") && n.ends_with(".ndjson"))).map(|e| e.path).collect();
        files.sort();
        Ok(files)
    }

    /// The oldest batches go once more than `max` bytes wait.
    pub async fn cap(&self, max: u64) {
        let mut sizes = Vec::new();
        for path in self.waiting().await {
            let len = self.disk.metadata(&path).await.map(|m| m.len).unwrap_or(0);
            sizes.push((path, len));
        }
        let mut total: u64 = sizes.iter().map(|(_, len)| len).sum();
        for (path, len) in sizes {
            if total <= max {
                break;
            }
            if self.disk.remove_file(&path).await.is_ok() {
                total -= len;
            }
        }
    }

    /// Send, oldest first, every waiting batch that is `account`'s or no
    /// one's to `url` with `bearer` (that account's session), asking
    /// `go_on` before each: once it says no (diagnostics were turned off),
    /// nothing more is read or sent. Another account's batches wait for it
    /// (`pimble_core::diagnostics::may_send`, the page's rule too);
    /// a batch whose name says no owner is deleted unsent. A batch taken, or
    /// refused as unreadable or too big (400, 413), is deleted; anything else
    /// stops the send.
    pub async fn send(&self, http: &dyn Http, url: &str, bearer: &str, account: &str, go_on: &dyn Fn() -> bool) -> Sent {
        for path in self.waiting().await {
            let owner = match owner_of(&path) {
                Owner::NoOne => None,
                Owner::Account(owner) => Some(owner),
                Owner::Unknown => {
                    let _ = self.disk.remove_file(&path).await;
                    continue;
                }
            };
            if !may_send(owner.as_deref(), Some(account)) {
                continue;
            }
            if !go_on() {
                return Sent::Stopped;
            }
            let Ok(bytes) = self.disk.read(&path).await else { continue };
            if !go_on() {
                return Sent::Stopped;
            }
            let request = HttpRequest { method: Method::Post, url: url.to_string(), bearer: Some(bearer.to_string()), body: Some(Body::ndjson(bytes)) };
            match http.send(request).await {
                Ok(answer) if answer.is_success() || matches!(answer.status, 400 | 413) => {
                    let _ = self.disk.remove_file(&path).await;
                }
                Ok(answer) => {
                    tracing::debug!("diagnostic records refused ({}); trying later", answer.status);
                    return Sent::Stopped;
                }
                Err(e) => {
                    tracing::debug!("diagnostic records not sent: {e}");
                    return Sent::Stopped;
                }
            }
        }
        Sent::All
    }

    /// Turned off: everything waiting goes, whoever's (the install number
    /// stays). Every batch is tried; the first that could not be removed is
    /// the error, and then the clear has not happened: the caller keeps it
    /// owed and tries again.
    pub async fn clear(&self) -> io::Result<()> {
        self.remove_batches(|_| true).await
    }

    /// An off said once the sequence had given `through`, written down
    /// before its clear has run, so that the clear is owed by whichever
    /// session opens this queue next (`tag` makes the name this off's own).
    /// Outside the `records-*` names.
    pub async fn mark_cleared_through(&self, through: u64, tag: &str) -> io::Result<()> {
        self.disk.create_dir_all(&self.dir).await?;
        self.disk.write(self.dir.join(format!("{CLEAR_MARK}{through:020}-{tag}")), b"").await
    }

    /// The offs written down and not yet cleared for: each mark's path and
    /// the last batch number it clears. A mark whose number cannot be read,
    /// and one a version before sequence numbers wrote (`clear-before-<wall
    /// millis>`), ask for everything ([`u64::MAX`]).
    pub async fn clear_marks(&self) -> Vec<(PathBuf, u64)> {
        let Ok(entries) = self.disk.list(&self.dir).await else { return Vec::new() };
        entries
            .into_iter()
            .filter(|e| !e.is_dir)
            .filter_map(|e| {
                let name = e.name()?;
                let through = match name.strip_prefix(CLEAR_MARK) {
                    Some(rest) => rest.split('-').next().and_then(|n| n.parse::<u64>().ok()).unwrap_or(u64::MAX),
                    None if name.starts_with(LEGACY_CLEAR_MARK) => u64::MAX,
                    None => return None,
                };
                Some((e.path.clone(), through))
            })
            .collect()
    }

    /// Every batch numbered `through` or lower goes, and every batch named
    /// before sequence numbers or by nothing readable; as [`Self::clear`],
    /// the first that could not be removed is the error.
    pub async fn clear_through(&self, through: u64) -> io::Result<()> {
        self.remove_batches(|path| match written(path) {
            Written::At(seq) => seq <= through,
            Written::BeforeAnyMark => true,
        })
        .await
    }

    /// Remove every waiting batch `which` names, trying each one; the first
    /// error, if any was not removed (or the folder could not be listed).
    async fn remove_batches(&self, which: impl Fn(&std::path::Path) -> bool) -> io::Result<()> {
        let mut first_error = None;
        for path in self.listed().await?.into_iter().filter(|p| which(p)) {
            match self.disk.remove_file(&path).await {
                // Gone already: what a clear asks.
                Err(e) if e.kind() != io::ErrorKind::NotFound => {
                    first_error.get_or_insert(e);
                }
                _ => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// The name an off written down starts with: `clear-through-<seq>-<tag>`.
const CLEAR_MARK: &str = "clear-through-";
/// What versions before 2026-11-25 named one: `clear-before-<wall millis>-<tag>`.
const LEGACY_CLEAR_MARK: &str = "clear-before-";

/// Send one report's `body` to the accounts service at `service` as the
/// account `bearer` holds: the report's code, or a sentence.
pub async fn send_report(http: &dyn Http, service: &str, bearer: &str, body: &Value) -> Result<String, String> {
    let request = HttpRequest { method: Method::Post, url: reports_url(service), bearer: Some(bearer.to_string()), body: Some(Body::json(body.to_string().into_bytes())) };
    let answer = http.send(request).await.map_err(|e| format!("Pimble Cloud could not be reached ({e}). Try again when this browser is online."))?;
    if !answer.is_success() {
        return Err(format!("Pimble Cloud did not take the report ({}: {}).", answer.status, answer.text().trim()));
    }
    report_code(&answer.text()).ok_or_else(|| "Pimble Cloud answered without a report code.".to_string())
}

#[cfg(test)]
#[path = "on_disk_tests.rs"]
mod tests;
