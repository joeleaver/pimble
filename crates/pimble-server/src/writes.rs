//! The writes signed-in people make here (docs/NOTIFICATIONS_CONTRACT.md
//! section 3, wave 3): what watches are evaluated from.
//!
//! A server that verifies tokens (the hosted server, an owner's relay face)
//! sees every append and edit: which document, which account, when, and
//! nothing of what was written. Each one a `Principal::User` makes to a
//! node's document is recorded here, gathered per (store, document, writer)
//! under [`Window::REPORT`], and told to whoever listens: the accounts
//! service over `subscribeWrites` on the hosted server, the owner's own
//! server in-process for its relay face (`crate::relay_writes`).
//! With nobody listening nothing is gathered, so a server nobody asks keeps
//! nothing. The operator's own writes (`Principal::Service`: the desktop's
//! embedded server, the CLI) are never recorded: on a person's own computer
//! there is no account to tell apart.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use pimble_core::watch::{Batcher, Window};
use pimble_core::{NodeId, StoreId};
use pimble_rpc::WriteSeen;
use tokio::sync::broadcast;

/// How often the gathered writes are looked over.
const TICK: Duration = Duration::from_millis(500);

type Key = (StoreId, NodeId, String);

/// This server's writes: one per `RpcHandler`, `Arc`-shared.
pub struct Writes {
    gathered: Mutex<Batcher<Key>>,
    seen: broadcast::Sender<WriteSeen>,
    /// The task that tells what falls due, while anything is gathered.
    ticking: Mutex<Option<pimble_host::Task>>,
    /// Whether that task runs (or is about to): set by whoever starts it,
    /// cleared by it only once nothing is gathered.
    running: AtomicBool,
}

impl Default for Writes {
    fn default() -> Self {
        Self::new()
    }
}

impl Writes {
    pub fn new() -> Self {
        Self { gathered: Mutex::new(Batcher::new(Window::REPORT)), seen: broadcast::channel(1024).0, ticking: Mutex::new(None), running: AtomicBool::new(false) }
    }

    /// Hear each gathered write.
    pub fn subscribe(&self) -> broadcast::Receiver<WriteSeen> {
        self.seen.subscribe()
    }

    /// Whether anybody listens, so a write is worth gathering.
    pub fn heard(&self) -> bool {
        self.seen.receiver_count() > 0
    }

    /// `writer` wrote `doc` of `store` at `now_ms`. Answers whether it was
    /// gathered (nobody listening: not).
    pub fn record(&self, store: StoreId, doc: NodeId, writer: &str, now_ms: i64) -> bool {
        if !self.heard() {
            return false;
        }
        self.gathered.lock().unwrap_or_else(|e| e.into_inner()).record((store, doc, writer.to_string()), 1, now_ms);
        true
    }

    /// Tell what is due at `now_ms`; answers whether anything is still
    /// gathered.
    pub fn tell_due(&self, now_ms: i64) -> bool {
        let (due, left) = {
            let mut gathered = self.gathered.lock().unwrap_or_else(|e| e.into_inner());
            let due = gathered.due(now_ms);
            (due, !gathered.is_empty())
        };
        for ((store_id, doc_id, writer), g) in due {
            let _ = self.seen.send(WriteSeen { store_id, doc_id, writer, count: g.count });
        }
        left
    }

    /// Tell everything gathered at once (the server is stopping).
    pub fn tell_all(&self) {
        let all = self.gathered.lock().unwrap_or_else(|e| e.into_inner()).drain();
        for ((store_id, doc_id, writer), g) in all {
            let _ = self.seen.send(WriteSeen { store_id, doc_id, writer, count: g.count });
        }
    }

    /// After a write was gathered: make sure the task that tells it runs.
    /// It ends by itself once nothing is gathered.
    pub fn keep_telling(self: &std::sync::Arc<Self>, runtime: &pimble_host::Runtime) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let writes = std::sync::Arc::clone(self);
        let rt = runtime.clone();
        let task = runtime.spawn(async move {
            loop {
                rt.sleep(TICK).await;
                if writes.tell_due(now_ms()) {
                    continue;
                }
                writes.running.store(false, Ordering::Release);
                // A write gathered between the look and the store found the
                // task still running and started none: take it on.
                let empty = writes.gathered.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
                if empty || writes.running.swap(true, Ordering::AcqRel) {
                    break;
                }
            }
        });
        *self.ticking.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
    }

    /// The server stops: what is gathered is told, and the task ends.
    pub fn stop(&self) {
        if let Some(task) = self.ticking.lock().unwrap_or_else(|e| e.into_inner()).take() {
            task.abort();
        }
        self.running.store(false, Ordering::Release);
        self.tell_all();
    }
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_gathered_per_document_and_writer_and_told_once_quiet() {
        let w = Writes::new();
        let (s, d, e) = (StoreId::new(), NodeId::new(), NodeId::new());
        assert!(!w.record(s, d, "ann", 0), "nobody listens: nothing is kept");
        let mut heard = w.subscribe();
        for t in [0, 300, 600, 900] {
            assert!(w.record(s, d, "ann", t));
        }
        w.record(s, d, "bob", 900);
        w.record(s, e, "ann", 900);
        assert!(w.tell_due(2_000), "not quiet for two seconds yet");
        assert!(heard.try_recv().is_err());
        assert!(!w.tell_due(2_900));
        let mut told = Vec::new();
        while let Ok(seen) = heard.try_recv() {
            told.push((seen.doc_id, seen.writer, seen.count));
        }
        told.sort_by(|a, b| (a.1.clone(), a.2).cmp(&(b.1.clone(), b.2)));
        assert_eq!(told.len(), 3);
        assert!(told.contains(&(d, "ann".into(), 4)));
        assert!(told.contains(&(d, "bob".into(), 1)));
        assert!(told.contains(&(e, "ann".into(), 1)));
    }

    #[test]
    fn stopping_tells_what_is_gathered() {
        let w = Writes::new();
        let mut heard = w.subscribe();
        w.record(StoreId::new(), NodeId::new(), "ann", 0);
        w.stop();
        assert_eq!(heard.try_recv().expect("told at stop").count, 1);
    }
}
