//! The browser's worker's diagnostics, one step at a time
//! (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 2"): the
//! [`Recorder`], the page's choice, the install number and the queue's
//! folder, and the two steps that touch the queue, applying a choice and a
//! pump (write what was recorded, send what waits). Built for every target so
//! that it is tested natively; `worker` holds the one the worker uses and
//! hands it the host's seams.
//!
//! The steps take turns behind one lock, so an off is never overtaken: the
//! switch is turned the moment the page says so, a pump already sending
//! stops before its next batch, and the off's clearing of the queue runs
//! once that pump has ended, so nothing written by it survives on the disk.
//!
//! Every off advances a count, and a clear is owed for each one even when an
//! "on" follows before the clear could run: a pump goes on only while no off
//! has come since it took its records, and no pump starts while a clear is
//! owed. So nothing recorded before the latest off is ever sent, and none of
//! it stays on the disk. A request is never waited for longer than
//! [`SEND_TIMEOUT`] (the host's [`Bounded`]), so neither is an off.
//!
//! The count lives in memory, so the clear an off owes is also written down
//! on the disk, before the off waits for its turn: a mark outside the
//! `records-*` names holding the off's wall time (batches are named by
//! theirs). [`WorkerDiagnostics::attach`] and every say delete each batch
//! written at or before a mark and then the mark, and no pump runs while a
//! mark is there. So a session that ends with the clear still owed (the tab
//! closed during a send, after an off and an "on", or the off's step
//! dropped) leaves it to the next one, which clears before it sends.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use pimble_host::http::{Bounded, Http};
use pimble_host::{Disk, Runtime};
use serde_json::Value;

use super::on_disk::{records_url, Queue, Recorder, MAX_QUEUED_BYTES};
use pimble_core::diagnostics::Severity;

/// How long one request of a pump is waited for: what the desktop's own
/// client allows a call to the accounts service. An off waits for a pump in
/// flight, so this is also the longest an off's clear can wait.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// The page's say (`setDiagnostics`), kept until the queue's folder is known;
/// on or off is the recorder's switch, turned at once.
#[derive(Debug, Clone)]
struct Choice {
    /// The install number the page offered.
    install: Option<String>,
}

struct State {
    recorder: Recorder,
    /// The install number in use, once a choice has been applied.
    install: Option<String>,
    /// Where the queue is, once the server has started.
    dir: Option<PathBuf>,
    /// A choice not yet applied to the queue: it came before the folder.
    unapplied: Option<Choice>,
    /// Whether this session has said which program it is.
    said_system: bool,
    /// How many offs the page has said: each one owes the queue a clear.
    offs: u64,
    /// How many of them the queue has been cleared for.
    cleared: u64,
    /// The latest off said before the queue's folder was known, when it was
    /// said: [`WorkerDiagnostics::attach`] writes it down first.
    unmarked_off: Option<u128>,
}

/// The signed-in account a pump sends as.
#[derive(Debug, Clone, Copy)]
pub struct SendAs<'a> {
    /// The accounts service.
    pub url: &'a str,
    /// The account's session, the bearer.
    pub session: &'a str,
    /// The account's id, which its records carry.
    pub user_id: &'a str,
}

/// What a choice applied asks of the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// Nothing more.
    Done,
    /// Diagnostics are on for the first time this session: say which program
    /// it is (a `system` record).
    FirstOn,
}

/// One worker's recorder, its choice and its queue.
pub struct WorkerDiagnostics {
    state: RefCell<State>,
    /// Held for the whole of a step that touches the queue.
    step: tokio::sync::Mutex<()>,
    /// How long one request is waited for ([`SEND_TIMEOUT`]).
    send_timeout: Duration,
}

impl WorkerDiagnostics {
    /// A new session recording as `app` on `os`, waiting for the page's say.
    pub fn new(app: &str, os: &str) -> Self {
        let state = State { recorder: Recorder::new(app, os), install: None, dir: None, unapplied: None, said_system: false, offs: 0, cleared: 0, unmarked_off: None };
        WorkerDiagnostics { state: RefCell::new(state), step: tokio::sync::Mutex::new(()), send_timeout: SEND_TIMEOUT }
    }

    /// The same, waiting at most `limit` for each request: a test's, which
    /// cannot wait [`SEND_TIMEOUT`] of real time.
    #[cfg(test)]
    pub fn with_send_timeout(mut self, limit: Duration) -> Self {
        self.send_timeout = limit;
        self
    }

    /// Whether records are kept (until the page says, they wait in memory).
    pub fn enabled(&self) -> bool {
        self.state.borrow().recorder.enabled()
    }

    /// This install's id and the session, once the page has said they are on.
    pub fn ids(&self) -> Option<(String, String)> {
        let s = self.state.borrow();
        let install = s.install.clone().filter(|_| s.recorder.enabled())?;
        Some((install, s.recorder.session().to_string()))
    }

    /// The signed-in account's id, carried by every record from now on.
    pub fn set_account(&self, account: Option<String>) {
        self.state.borrow_mut().recorder.set_account(account);
    }

    /// Queue one record of `kind` taken at `ts`.
    pub fn record(&self, kind: &str, data: Value, ts: String) {
        self.state.borrow_mut().recorder.record(kind, data, ts);
    }

    /// One log line, unless one is being kept already (a line logged from
    /// inside a borrow would be a second borrow).
    pub fn log(&self, level: &str, severity: Option<Severity>, target: &str, message: &str, ts: String) {
        if let Ok(mut s) = self.state.try_borrow_mut() {
            s.recorder.log(level, severity, target, message, ts);
        }
    }

    /// The recent log, for a report.
    pub fn recent_log(&self) -> String {
        self.state.borrow().recorder.recent()
    }

    /// The queue's folder, once the server has started.
    pub fn dir(&self) -> Option<PathBuf> {
        self.state.borrow().dir.clone()
    }

    /// A report's body, as `install` (the one in use when there is one).
    pub fn report(&self, install: String, ts: &str, note: &str, logs: Vec<(String, String)>, stores: Value) -> Value {
        let s = self.state.borrow();
        let install = s.install.clone().unwrap_or(install);
        s.recorder.report(&install, ts, note, logs, "worker.log", stores)
    }

    /// The page's say, at `millis` of wall time: on or off, and its install
    /// number. The switch turns at once (off drops what waits in memory, and
    /// stops a pump in flight before its next batch), and an off is written
    /// down on the disk before it waits for its turn; the queue follows once
    /// no pump is running, emptied for every off said, whatever came after
    /// it. Before the server has given the queue's folder, the choice is kept
    /// for [`Self::attach`].
    pub async fn configure(&self, disk: &Disk, enabled: bool, install: Option<String>, millis: u128) -> Applied {
        let mark = {
            let mut s = self.state.borrow_mut();
            s.recorder.set_enabled(enabled);
            s.unapplied = Some(Choice { install });
            if enabled {
                None
            } else {
                s.offs += 1;
                match s.dir.clone() {
                    Some(dir) => Some((dir, format!("{}-{}", s.recorder.session(), s.offs))),
                    None => {
                        s.unmarked_off = Some(s.unmarked_off.map_or(millis, |at| at.max(millis)));
                        None
                    }
                }
            }
        };
        if let Some((dir, tag)) = mark {
            mark_off(disk, dir, millis, &tag).await;
        }
        let _step = self.step.lock().await;
        self.apply(disk).await
    }

    /// The server has started: the queue is in `dir`. An off said before it
    /// is written down, the clears owed by this session or an earlier one
    /// run, and a choice that came before it is applied.
    pub async fn attach(&self, disk: &Disk, dir: PathBuf) -> Applied {
        let unmarked = {
            let mut s = self.state.borrow_mut();
            s.dir = Some(dir.clone());
            s.unmarked_off.take().map(|at| (at, format!("{}-{}", s.recorder.session(), s.offs)))
        };
        if let Some((at, tag)) = unmarked {
            mark_off(disk, dir, at, &tag).await;
        }
        let _step = self.step.lock().await;
        self.apply(disk).await
    }

    /// The choice waiting and the clears owed, applied to the queue. Called
    /// with the step held.
    async fn apply(&self, disk: &Disk) -> Applied {
        let Some(dir) = self.state.borrow().dir.clone() else { return Applied::Done };
        let queue = Queue::new(disk.clone(), dir);
        // The offs written down, by this session or one before it: every
        // batch from before the latest goes, then the marks this read (an
        // off said meanwhile wrote its own, which its own step clears for).
        let marks = queue.clear_marks().await;
        if let Some(latest) = marks.iter().map(|(_, at)| *at).max() {
            if queue.clear_through(latest).await {
                for (mark, _) in &marks {
                    if let Err(e) = disk.remove_file(mark).await {
                        tracing::debug!("a diagnostics clear mark was not removed: {e}");
                    }
                }
            }
        }
        let (choice, offs) = {
            let mut s = self.state.borrow_mut();
            if s.unapplied.is_none() && s.offs == s.cleared {
                return Applied::Done;
            }
            (s.unapplied.take(), s.offs)
        };
        if let Some(choice) = choice {
            let install = queue.install(choice.install.as_deref()).await;
            self.state.borrow_mut().install = Some(install);
        }
        // Owed for every off said so far, even one an "on" has followed: what
        // was written before it must go. An off said during this clear owes
        // one more, which its own step runs.
        if offs > self.state.borrow().cleared {
            queue.clear().await;
            self.state.borrow_mut().cleared = offs;
        }
        // The switch as it is now: a later say has turned it already, and is
        // waiting for this step to end to apply itself.
        if !self.enabled() {
            return Applied::Done;
        }
        let mut s = self.state.borrow_mut();
        if std::mem::replace(&mut s.said_system, true) {
            Applied::Done
        } else {
            Applied::FirstOn
        }
    }

    /// Write what was recorded as batches on the disk, one per account, then
    /// send what waits as `account` (its own batches and no one's), taken at
    /// `millis`, waiting for each request at most the send timeout on
    /// `runtime`. Nothing happens while off, before the page has said, while
    /// a say or an off's clear waits to be applied, or while an off written
    /// down on the disk has not been cleared for; once an off is said,
    /// nothing more is written or sent, even if an "on" follows it.
    pub async fn pump(&self, disk: &Disk, http: &dyn Http, runtime: &Runtime, account: Option<SendAs<'_>>, millis: u128) {
        let _step = self.step.lock().await;
        let Some(dir) = self.state.borrow().dir.clone() else { return };
        if !Queue::new(disk.clone(), dir).clear_marks().await.is_empty() {
            return;
        }
        let taken = {
            let mut s = self.state.borrow_mut();
            let settled = s.recorder.enabled() && s.unapplied.is_none() && s.offs == s.cleared;
            match (s.install.clone().filter(|_| settled), s.dir.clone()) {
                (Some(install), Some(dir)) => Some((s.recorder.take(&install), s.recorder.session().to_string(), dir, s.offs)),
                _ => None,
            }
        };
        let Some((batches, session, dir, offs)) = taken else { return };
        // Asked before each write and each read and send: no off since the
        // records were taken.
        let go_on = || {
            let s = self.state.borrow();
            s.recorder.enabled() && s.offs == offs
        };
        let queue = Queue::new(disk.clone(), dir);
        for (owner, lines) in batches {
            if !go_on() {
                return;
            }
            if let Err(e) = queue.append(&lines, millis, &session, owner.as_deref()).await {
                tracing::debug!("diagnostic records not written: {e}");
            }
        }
        queue.cap(MAX_QUEUED_BYTES).await;
        // Signed out (or the session ran out): they wait for the next sign-in.
        let Some(account) = account else { return };
        let http = Bounded::new(http, runtime, self.send_timeout);
        queue.send(&http, &records_url(account.url), account.session, account.user_id, &go_on).await;
    }
}

/// Write down an off said at `millis` in the queue in `dir`. A mark that
/// cannot be written leaves the clear to this session's memory alone.
async fn mark_off(disk: &Disk, dir: PathBuf, millis: u128, tag: &str) {
    if let Err(e) = Queue::new(disk.clone(), dir).mark_cleared_before(millis, tag).await {
        tracing::debug!("a diagnostics off was not written down: {e}");
    }
}

#[cfg(test)]
#[path = "worker_core_tests.rs"]
mod tests;
