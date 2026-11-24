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

use pimble_core::diagnostics::{random_hex, report_code, valid_install, Envelope, LogBudget, Scrubber, Severity};
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

    /// Everything recorded since the last take, as record lines of `install`.
    pub fn take(&mut self, install: &str) -> Vec<String> {
        let envelope = self.envelope(install);
        self.pending_bytes = 0;
        self.pending.drain(..).map(|p| envelope.record(&self.scrubber, p.seq, &p.ts, &p.kind, p.data, p.account.as_deref())).collect()
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
/// when it was written, so the oldest go first. One server holds a browser's
/// disk at a time (section 4), so no file is ever claimed by two senders.
pub struct Queue {
    disk: Disk,
    dir: PathBuf,
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

    /// Write `lines` as one batch, taken at `millis` by `session`.
    pub async fn append(&self, lines: &[String], millis: u128, session: &str) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        self.disk.create_dir_all(&self.dir).await?;
        let mut text = lines.join("\n");
        text.push('\n');
        self.disk.write(self.dir.join(format!("records-{millis:013}-{session}.ndjson")), text).await
    }

    /// The waiting batches, oldest first.
    pub async fn waiting(&self) -> Vec<PathBuf> {
        let Ok(entries) = self.disk.list(&self.dir).await else { return Vec::new() };
        let mut files: Vec<PathBuf> =
            entries.into_iter().filter(|e| !e.is_dir && e.name().is_some_and(|n| n.starts_with("records-") && n.ends_with(".ndjson"))).map(|e| e.path).collect();
        files.sort();
        files
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

    /// Send every waiting batch to `url`, oldest first, with `bearer` (the
    /// account's session). A batch taken, or refused as unreadable or too
    /// big (400, 413), is deleted; anything else stops the send.
    pub async fn send(&self, http: &dyn Http, url: &str, bearer: Option<&str>) -> Sent {
        for path in self.waiting().await {
            let Ok(bytes) = self.disk.read(&path).await else { continue };
            let request = HttpRequest { method: Method::Post, url: url.to_string(), bearer: bearer.map(str::to_string), body: Some(Body::ndjson(bytes)) };
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

    /// Turned off: everything waiting goes (the install number stays).
    pub async fn clear(&self) {
        for path in self.waiting().await {
            let _ = self.disk.remove_file(&path).await;
        }
    }
}

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
