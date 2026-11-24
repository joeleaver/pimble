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
//!
//! **Only what is wanted.** The hosted server tells only writes to the
//! documents its listener named (`setWriteInterest`: opaque ids per store,
//! and the roots of the shares its watchers hold the store through), so the
//! accounts service never hears of a write to a note nobody watches. The
//! server learns which documents are watched, never by whom. A relay face
//! is the owner's own server for the owner's own store, and tells every
//! write ([`Writes::tell_everything`]).
//!
//! **The roots that reach it.** Each write told names the shared roots whose
//! scope reaches its document (among the roots named, or every root a relay
//! face's store publishes), judged once per gathering by the same scope sets
//! every scoped read is judged by (`crate::handler::scope_set_of`): a
//! watcher who holds the store through a share hears only of documents that
//! share reaches.
//!
//! With nobody listening nothing is gathered, so a server nobody asks keeps
//! nothing. The operator's own writes (`Principal::Service`: the desktop's
//! embedded server, the CLI) are never recorded: on a person's own computer
//! there is no account to tell apart.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use pimble_core::watch::{Batcher, Gathered, Window};
use pimble_core::{NodeId, StoreId};
use pimble_rpc::{SetWriteInterestRequest, WriteSeen};
use pimble_store::StoreManager;
use tokio::sync::broadcast;

/// How often the gathered writes are looked over.
const TICK: Duration = Duration::from_millis(500);

type Key = (StoreId, NodeId, String);

/// Which writes are told.
#[derive(Debug)]
enum Interest {
    /// Writes to these documents, per store (the hosted server's: what its
    /// listener named; nothing until it names something).
    Named(HashMap<StoreId, Wanted>),
    /// Every write (a relay face's).
    Everything,
}

#[derive(Debug, Default)]
struct Wanted {
    docs: HashSet<NodeId>,
    roots: Vec<NodeId>,
}

/// The roots a write's document is judged against when its gathering
/// starts ([`roots_reaching`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidates {
    /// The roots the listener named for the store.
    Named(Vec<NodeId>),
    /// Every root the store publishes (a relay face's twin).
    Published,
}

/// What [`Writes::record`] did with a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// Nobody listens, or nobody wants this document's writes.
    Unwanted,
    /// Added to a gathering already under way.
    Added,
    /// The first of a gathering: judge which of these roots reach the
    /// document and [`Writes::start`] it.
    Starts(Candidates),
}

#[derive(Debug)]
struct Inner {
    gathered: Batcher<Key>,
    /// The roots each gathering's document was judged to be reached by.
    roots: HashMap<Key, Vec<NodeId>>,
    interest: Interest,
}

impl Inner {
    fn candidates(&self, store: StoreId, doc: NodeId) -> Option<Candidates> {
        match &self.interest {
            Interest::Everything => Some(Candidates::Published),
            Interest::Named(stores) => stores.get(&store).filter(|w| w.docs.contains(&doc)).map(|w| Candidates::Named(w.roots.clone())),
        }
    }

    /// What is due (or everything) as the writes told, less any document no
    /// longer wanted since its gathering started.
    fn seen(&mut self, taken: Vec<(Key, Gathered)>) -> Vec<WriteSeen> {
        taken
            .into_iter()
            .filter_map(|(key, g)| {
                let roots = self.roots.remove(&key).unwrap_or_default();
                let (store_id, doc_id, writer) = key;
                self.candidates(store_id, doc_id)?;
                Some(WriteSeen { store_id, doc_id, writer, count: g.count, roots, last_ms: g.last_ms })
            })
            .collect()
    }
}

/// This server's writes: one per `RpcHandler`, `Arc`-shared.
pub struct Writes {
    inner: Mutex<Inner>,
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
    /// A server that tells only the writes its listener names.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner { gathered: Batcher::new(Window::REPORT), roots: HashMap::new(), interest: Interest::Named(HashMap::new()) }),
            seen: broadcast::channel(1024).0,
            ticking: Mutex::new(None),
            running: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tell every write from now on, with every root the store publishes
    /// (a relay face: the owner's server for the owner's store).
    pub fn tell_everything(&self) {
        self.lock().interest = Interest::Everything;
    }

    /// The listener names the documents it wants told (`setWriteInterest`).
    /// A relay face tells everything and keeps doing so.
    pub fn set_interest(&self, request: SetWriteInterestRequest) {
        let mut inner = self.lock();
        let Interest::Named(stores) = &mut inner.interest else { return };
        if request.whole {
            stores.clear();
        }
        for interest in request.stores {
            if interest.docs.is_empty() {
                stores.remove(&interest.store_id);
            } else {
                let mut roots = interest.roots;
                sort_ids(&mut roots);
                stores.insert(interest.store_id, Wanted { docs: interest.docs.into_iter().collect(), roots });
            }
        }
    }

    /// Hear each gathered write.
    pub fn subscribe(&self) -> broadcast::Receiver<WriteSeen> {
        self.seen.subscribe()
    }

    /// Whether anybody listens, so a write is worth gathering.
    pub fn heard(&self) -> bool {
        self.seen.receiver_count() > 0
    }

    /// `writer` wrote `doc` of `store` at `now_ms`.
    pub fn record(&self, store: StoreId, doc: NodeId, writer: &str, now_ms: i64) -> Recorded {
        if !self.heard() {
            return Recorded::Unwanted;
        }
        let mut inner = self.lock();
        let Some(candidates) = inner.candidates(store, doc) else { return Recorded::Unwanted };
        let key = (store, doc, writer.to_string());
        if inner.roots.contains_key(&key) {
            inner.gathered.record(key, 1, now_ms);
            return Recorded::Added;
        }
        Recorded::Starts(candidates)
    }

    /// The first write of a gathering, its document reached by `roots`
    /// (another write may have started it meanwhile: then it is added).
    pub fn start(&self, store: StoreId, doc: NodeId, writer: &str, now_ms: i64, roots: Vec<NodeId>) {
        let mut inner = self.lock();
        let key = (store, doc, writer.to_string());
        inner.roots.entry(key.clone()).or_insert(roots);
        inner.gathered.record(key, 1, now_ms);
    }

    /// Tell what is due at `now_ms`; answers whether anything is still
    /// gathered.
    pub fn tell_due(&self, now_ms: i64) -> bool {
        let (seen, left) = {
            let mut inner = self.lock();
            let due = inner.gathered.due(now_ms);
            let seen = inner.seen(due);
            (seen, !inner.gathered.is_empty())
        };
        for seen in seen {
            let _ = self.seen.send(seen);
        }
        left
    }

    /// Tell everything gathered at once (the server is stopping).
    pub fn tell_all(&self) {
        let seen = {
            let mut inner = self.lock();
            let all = inner.gathered.drain();
            inner.seen(all)
        };
        for seen in seen {
            let _ = self.seen.send(seen);
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
                let empty = writes.lock().gathered.is_empty();
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

/// Which of `candidates` reach `doc` of `store`: the roots whose scope set
/// holds it, the same sets every scoped read is judged by. Sorted.
pub fn roots_reaching(manager: &StoreManager, store: StoreId, doc: NodeId, candidates: &Candidates) -> Vec<NodeId> {
    let mut roots: Vec<NodeId> = match candidates {
        Candidates::Named(roots) => roots.iter().copied().filter(|root| crate::handler::scope_set_of(manager, store, &[*root]).contains(&doc)).collect(),
        Candidates::Published => match manager.store_kind(store) {
            Some(pimble_core::StoreKind::Vault) => manager.vault_scopes(store).unwrap_or_default().into_iter().filter(|(_, docs)| docs.contains(&doc)).map(|(root, _)| root).collect(),
            // A plain store publishes no scope sets: its shares are judged
            // from its tree under roots someone names.
            _ => Vec::new(),
        },
    };
    sort_ids(&mut roots);
    roots
}

/// Ids in one order (their text's), each once.
fn sort_ids(ids: &mut Vec<NodeId>) {
    ids.sort_by_cached_key(|id| id.to_string());
    ids.dedup();
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_rpc::WriteInterest;

    fn wanting(w: &Writes, store: StoreId, docs: &[NodeId], roots: &[NodeId]) {
        w.set_interest(SetWriteInterestRequest { stores: vec![WriteInterest { store_id: store, docs: docs.to_vec(), roots: roots.to_vec() }], whole: false });
    }

    /// As the handler does: start a gathering with its roots, or add to it.
    fn write(w: &Writes, store: StoreId, doc: NodeId, writer: &str, t: i64, roots: &[NodeId]) -> bool {
        match w.record(store, doc, writer, t) {
            Recorded::Unwanted => false,
            Recorded::Added => true,
            Recorded::Starts(_) => {
                w.start(store, doc, writer, t, roots.to_vec());
                true
            }
        }
    }

    fn told(heard: &mut broadcast::Receiver<WriteSeen>) -> Vec<WriteSeen> {
        let mut told = Vec::new();
        while let Ok(seen) = heard.try_recv() {
            told.push(seen);
        }
        told.sort_by(|a, b| (a.writer.clone(), a.count).cmp(&(b.writer.clone(), b.count)));
        told
    }

    #[test]
    fn writes_are_gathered_per_document_and_writer_and_told_once_quiet() {
        let w = Writes::new();
        let (s, d, e) = (StoreId::new(), NodeId::new(), NodeId::new());
        let root = NodeId::new();
        wanting(&w, s, &[d, e], &[root]);
        assert!(!write(&w, s, d, "ann", 0, &[]), "nobody listens: nothing is kept");
        let mut heard = w.subscribe();
        assert_eq!(w.record(s, d, "ann", 0), Recorded::Starts(Candidates::Named(vec![root])));
        w.start(s, d, "ann", 0, vec![root]);
        for t in [300, 600, 900] {
            assert_eq!(w.record(s, d, "ann", t), Recorded::Added);
        }
        write(&w, s, d, "bob", 900, &[]);
        write(&w, s, e, "ann", 900, &[root]);
        assert!(w.tell_due(2_000), "not quiet for two seconds yet");
        assert!(heard.try_recv().is_err());
        assert!(!w.tell_due(2_900));
        let told = told(&mut heard);
        assert_eq!(told.len(), 3);
        let of = |doc: NodeId, writer: &str| told.iter().find(|t| t.doc_id == doc && t.writer == writer).cloned().expect("told");
        assert_eq!((of(d, "ann").count, of(d, "ann").roots, of(d, "ann").last_ms), (4, vec![root], 900));
        assert_eq!((of(d, "bob").count, of(d, "bob").roots), (1, vec![]));
        assert_eq!(of(e, "ann").count, 1);
    }

    #[test]
    fn only_the_documents_named_are_told() {
        let w = Writes::new();
        let mut heard = w.subscribe();
        let (s, other_store, d, unwatched) = (StoreId::new(), StoreId::new(), NodeId::new(), NodeId::new());
        assert!(!write(&w, s, d, "ann", 0, &[]), "nothing named: nothing told");
        wanting(&w, s, &[d], &[]);
        assert!(!write(&w, s, unwatched, "ann", 0, &[]), "a document nobody watches");
        assert!(!write(&w, other_store, d, "ann", 0, &[]), "another store");
        assert!(write(&w, s, d, "ann", 0, &[]));
        // No longer wanted before it is told: it is not told.
        w.set_interest(SetWriteInterestRequest { stores: vec![], whole: true });
        w.stop();
        assert!(told(&mut heard).is_empty());
        assert!(!write(&w, s, d, "ann", 10, &[]), "the whole interest replaced, with nothing");
    }

    #[test]
    fn a_relay_face_tells_everything() {
        let w = Writes::new();
        let mut heard = w.subscribe();
        w.tell_everything();
        w.set_interest(SetWriteInterestRequest { stores: vec![], whole: true });
        let (s, d) = (StoreId::new(), NodeId::new());
        assert_eq!(w.record(s, d, "ann", 0), Recorded::Starts(Candidates::Published));
        w.start(s, d, "ann", 0, vec![]);
        w.stop();
        assert_eq!(told(&mut heard).len(), 1);
    }

    #[test]
    fn stopping_tells_what_is_gathered() {
        let w = Writes::new();
        let mut heard = w.subscribe();
        w.tell_everything();
        write(&w, StoreId::new(), NodeId::new(), "ann", 0, &[]);
        w.stop();
        assert_eq!(heard.try_recv().expect("told at stop").count, 1);
    }
}
