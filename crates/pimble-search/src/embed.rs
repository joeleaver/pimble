//! One embedding model for the whole process, and the one worker that feeds
//! every store's index from it.
//!
//! Each store's [`crate::SearchIndex`] holds its own rhypedb database and
//! vector index, and stays in its own store folder (Joe, 2026-11-11). The
//! model does not: before 2026-11-12 every index let rhypedb's `Vectorizer`
//! load its own `all-MiniLM-L6-v2`, so each open store carried its own ONNX
//! Runtime session, thread pool and arena (about 600 MB a store). Now the
//! model lives here, once, and an index hands rhypedb plain vectors
//! (`Vectorizer::ingest_vectors`, `Vectorizer::search_vector`).
//!
//! A search embeds its query here and waits behind at most one small batch
//! of background work: the worker takes [`BATCH`] chunks at a time and steps
//! aside while a query is waiting.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

#[cfg(feature = "semantic")]
use crate::error::SearchError;
use crate::error::Result;

/// The model every index embeds with (the schema's `Vector<384>`).
pub const MODEL: &str = "all-MiniLM-L6-v2";

/// Chunks the worker embeds at a time. Small, so a query waits little.
const BATCH: usize = 8;

/// Queries waiting for the model: the worker steps aside while there are any.
static QUERIES_WAITING: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "semantic")]
fn model() -> &'static Mutex<Option<rhypedb_embed::FastEmbedder>> {
    static MODEL_SLOT: OnceLock<Mutex<Option<rhypedb_embed::FastEmbedder>>> = OnceLock::new();
    MODEL_SLOT.get_or_init(|| Mutex::new(None))
}

/// Load the model if it is not loaded yet (downloading it on first use into
/// [`crate::set_model_cache_dir`]'s directory).
#[cfg(feature = "semantic")]
pub(crate) fn load() -> Result<()> {
    let mut slot = model().lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() {
        let options = rhypedb_embed::EmbedOptions {
            cache_dir: crate::warmup::model_cache_dir(),
            ..rhypedb_embed::EmbedOptions::default()
        };
        let embedder = rhypedb_embed::FastEmbedder::with_options(MODEL, &options)
            .map_err(|e| SearchError::ModelUnavailable(e.to_string()))?;
        *slot = Some(embedder);
    }
    Ok(())
}

#[cfg(feature = "semantic")]
fn embed_now(texts: &[&str]) -> Result<Vec<Vec<f32>>> {
    use rhypedb_embed::Embedder;
    load()?;
    let mut slot = model().lock().unwrap_or_else(|e| e.into_inner());
    let embedder = slot.as_mut().ok_or_else(|| SearchError::ModelUnavailable("not loaded".into()))?;
    embedder.embed(texts).map_err(|e| SearchError::Embedding(e.to_string()))
}

#[cfg(not(feature = "semantic"))]
fn embed_now(_texts: &[&str]) -> Result<Vec<Vec<f32>>> {
    Err(crate::error::SearchError::ModelUnavailable("built without semantic search".into()))
}

/// The query's vector. A search asks every open store's index in turn with
/// the same text, so the last query's vector is kept and the model runs once.
pub(crate) fn embed_query(text: &str) -> Result<Vec<f32>> {
    static LAST: OnceLock<Mutex<Option<(String, Vec<f32>)>>> = OnceLock::new();
    let last = LAST.get_or_init(|| Mutex::new(None));
    if let Some((t, v)) = last.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if t == text {
            return Ok(v.clone());
        }
    }
    QUERIES_WAITING.fetch_add(1, Ordering::SeqCst);
    let result = embed_now(&[text]);
    QUERIES_WAITING.fetch_sub(1, Ordering::SeqCst);
    let vector = result?.into_iter().next().unwrap_or_default();
    *last.lock().unwrap_or_else(|e| e.into_inner()) = Some((text.to_string(), vector.clone()));
    Ok(vector)
}

/// What one index waits to have embedded: chunk object ids in arrival order,
/// each with the text it had when it was last queued (a chunk queued again
/// before it was embedded keeps its place and takes the newer text).
#[derive(Default)]
pub(crate) struct Queue {
    order: VecDeque<u64>,
    text: HashMap<u64, String>,
}

impl Queue {
    fn take(&mut self, n: usize) -> Vec<(u64, String)> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let Some(id) = self.order.pop_front() else { break };
            if let Some(text) = self.text.remove(&id) {
                out.push((id, text));
            }
        }
        out
    }
}

/// An index's share of the background work: its queue and where its vectors go.
pub(crate) struct Pending {
    queue: Mutex<Queue>,
    sink: Box<dyn Fn(&[(u64, Vec<f32>)]) + Send + Sync>,
}

impl Pending {
    /// Register an index's queue with the worker; `sink` stores a batch of
    /// vectors in that index. The worker holds it weakly: dropping the
    /// returned `Arc` (the index closing) ends its work.
    pub(crate) fn register(sink: impl Fn(&[(u64, Vec<f32>)]) + Send + Sync + 'static) -> Arc<Pending> {
        let pending = Arc::new(Pending { queue: Mutex::new(Queue::default()), sink: Box::new(sink) });
        let worker = worker();
        worker.indexes.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::downgrade(&pending));
        pending
    }

    /// Queue `chunk` to be embedded from `text`.
    pub(crate) fn push(&self, chunk: u64, text: String) {
        {
            let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            if q.text.insert(chunk, text).is_none() {
                q.order.push_back(chunk);
            }
        }
        worker().wake.notify_one();
    }

    /// Embed one batch of this index's queue now, returning how many ran.
    fn run_batch(&self) -> Result<usize> {
        let batch = self.queue.lock().unwrap_or_else(|e| e.into_inner()).take(BATCH);
        if batch.is_empty() {
            return Ok(0);
        }
        let texts: Vec<&str> = batch.iter().map(|(_, t)| t.as_str()).collect();
        let vectors = embed_now(&texts)?;
        let rows: Vec<(u64, Vec<f32>)> = batch.iter().map(|(id, _)| *id).zip(vectors).collect();
        (self.sink)(&rows);
        Ok(rows.len())
    }

    /// Embed everything queued for this index now (tests, and a caller that
    /// wants an upsert's embeddings ready before it returns).
    pub(crate) fn drain(&self) -> Result<usize> {
        let mut total = 0;
        loop {
            let n = self.run_batch()?;
            if n == 0 {
                return Ok(total);
            }
            total += n;
        }
    }
}

struct Worker {
    indexes: Mutex<Vec<Weak<Pending>>>,
    wake: Condvar,
    idle: Mutex<()>,
}

/// The one background worker, started the first time an index registers.
fn worker() -> &'static Worker {
    static WORKER: OnceLock<Worker> = OnceLock::new();
    static STARTED: OnceLock<()> = OnceLock::new();
    let w = WORKER.get_or_init(|| Worker { indexes: Mutex::new(Vec::new()), wake: Condvar::new(), idle: Mutex::new(()) });
    STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("pimble-embed".into())
            .spawn(run_worker)
            .expect("spawn the embedding worker");
    });
    w
}

fn run_worker() {
    let w = worker();
    loop {
        // A query first: it holds or is about to take the model.
        while QUERIES_WAITING.load(Ordering::SeqCst) > 0 {
            std::thread::yield_now();
        }
        let live: Vec<Arc<Pending>> = {
            let mut indexes = w.indexes.lock().unwrap_or_else(|e| e.into_inner());
            indexes.retain(|p| p.strong_count() > 0);
            indexes.iter().filter_map(Weak::upgrade).collect()
        };
        // One batch from each index in turn, so one store's backfill does
        // not hold back another's edit.
        let mut ran = 0;
        for pending in &live {
            match pending.run_batch() {
                Ok(n) => ran += n,
                Err(e) => {
                    tracing::warn!("embedding a batch failed ({e}); it is dropped and its chunks stay keyword-only");
                }
            }
        }
        drop(live);
        if ran == 0 {
            let guard = w.idle.lock().unwrap_or_else(|e| e.into_inner());
            let _ = w.wake.wait_timeout(guard, std::time::Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_queued_twice_keeps_its_place_and_takes_the_newer_text() {
        let mut q = Queue::default();
        for (id, text) in [(1, "a"), (2, "b"), (1, "a2")] {
            if q.text.insert(id, text.to_string()).is_none() {
                q.order.push_back(id);
            }
        }
        assert_eq!(q.take(8), vec![(1, "a2".to_string()), (2, "b".to_string())]);
        assert!(q.take(8).is_empty());
    }

    #[test]
    fn a_batch_is_at_most_batch_long() {
        let mut q = Queue::default();
        for id in 0..20u64 {
            q.text.insert(id, id.to_string());
            q.order.push_back(id);
        }
        assert_eq!(q.take(BATCH).len(), BATCH);
        assert_eq!(q.take(100).len(), 20 - BATCH);
    }
}
