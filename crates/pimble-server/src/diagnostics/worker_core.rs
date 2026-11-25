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

use std::cell::RefCell;
use std::path::PathBuf;

use pimble_host::http::Http;
use pimble_host::Disk;
use serde_json::Value;

use super::on_disk::{records_url, Queue, Recorder, MAX_QUEUED_BYTES};
use pimble_core::diagnostics::Severity;

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
}

impl WorkerDiagnostics {
    /// A new session recording as `app` on `os`, waiting for the page's say.
    pub fn new(app: &str, os: &str) -> Self {
        let state = State { recorder: Recorder::new(app, os), install: None, dir: None, unapplied: None, said_system: false };
        WorkerDiagnostics { state: RefCell::new(state), step: tokio::sync::Mutex::new(()) }
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

    /// The page's say: on or off, and its install number. The switch turns
    /// at once (off drops what waits in memory); the queue follows once no
    /// pump is running, off emptying it. Before the server has given the
    /// queue's folder, the choice is kept for [`Self::attach`].
    pub async fn configure(&self, disk: &Disk, enabled: bool, install: Option<String>) -> Applied {
        {
            let mut s = self.state.borrow_mut();
            s.recorder.set_enabled(enabled);
            s.unapplied = Some(Choice { install });
        }
        let _step = self.step.lock().await;
        self.apply(disk).await
    }

    /// The server has started: the queue is in `dir`, and a choice that came
    /// before it is applied now.
    pub async fn attach(&self, disk: &Disk, dir: PathBuf) -> Applied {
        self.state.borrow_mut().dir = Some(dir);
        let _step = self.step.lock().await;
        self.apply(disk).await
    }

    /// The choice waiting, applied to the queue. Called with the step held.
    async fn apply(&self, disk: &Disk) -> Applied {
        let (choice, dir) = {
            let mut s = self.state.borrow_mut();
            let Some(dir) = s.dir.clone() else { return Applied::Done };
            let Some(choice) = s.unapplied.take() else { return Applied::Done };
            (choice, dir)
        };
        let queue = Queue::new(disk.clone(), dir);
        let install = queue.install(choice.install.as_deref()).await;
        self.state.borrow_mut().install = Some(install);
        // The switch as it is now: a later say has turned it already, and is
        // waiting for this step to end to apply itself.
        if !self.enabled() {
            queue.clear().await;
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
    /// `millis`. Nothing happens while off or before the page has said.
    pub async fn pump(&self, disk: &Disk, http: &dyn Http, account: Option<SendAs<'_>>, millis: u128) {
        let _step = self.step.lock().await;
        let taken = {
            let mut s = self.state.borrow_mut();
            let install = s.install.clone().filter(|_| s.recorder.enabled() && s.unapplied.is_none());
            match (install, s.dir.clone()) {
                (Some(install), Some(dir)) => Some((s.recorder.take(&install), s.recorder.session().to_string(), dir)),
                _ => None,
            }
        };
        let Some((batches, session, dir)) = taken else { return };
        let queue = Queue::new(disk.clone(), dir);
        for (owner, lines) in batches {
            if let Err(e) = queue.append(&lines, millis, &session, owner.as_deref()).await {
                tracing::debug!("diagnostic records not written: {e}");
            }
        }
        queue.cap(MAX_QUEUED_BYTES).await;
        // Signed out (or the session ran out): they wait for the next sign-in.
        let Some(account) = account else { return };
        let go_on = || self.enabled();
        queue.send(http, &records_url(account.url), account.session, account.user_id, &go_on).await;
    }
}

#[cfg(test)]
#[path = "worker_core_tests.rs"]
mod tests;
