//! [`SearchIndex`]: one rhypedb database per open store, holding `Node`,
//! `Tag`, and (with semantic search on) `Chunk` objects.
//!
//! The index is split by responsibility: opening and the types live here;
//! `write` holds the writes, `edges` the relationship reconciliation and
//! backlinks, `chunks` the semantic chunks, `search` the queries, `hits` how a
//! hit is built from a stored object, `outline` a node's headings, `errors`
//! the engine-error mapping and `prefix` search-as-you-type.

mod chunks;
mod edges;
mod errors;
mod hits;
mod outline;
mod prefix;
mod search;
#[cfg(test)]
mod tests;
mod write;

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use pimble_core::{IndexUnit, NodeId};
use rhypedb_engine::database::Database;
use rhypedb_engine::vectorizer::{CrossEncoder, Vectorizer, VectorizerConfig};
use rhypedb_schema::parser::parse_schema;

use crate::error::{Result, SearchError};
use crate::schema::{compose_schema, schema_hash, SCHEMA_HASH_FILE};

pub(crate) use errors::map_engine_err;


/// A node's projection into the index: what [`SearchIndex::upsert`] needs to
/// create or update the `Node` object (and, with semantic search on, its
/// `Chunk` children).
#[derive(Debug, Clone)]
pub struct IndexNode {
    pub node_id: NodeId,
    pub kind: String,
    pub title: String,
    pub text: String,
    pub modified_at: i64,
    pub parent: Option<NodeId>,
    pub tags: Vec<String>,
    pub links: Vec<NodeId>,
    /// This node's content already broken into [`IndexUnit`]s (from
    /// `NodeDoc::units()` or a plugin's `index_units`) — the chunker's
    /// input. Deviates from the contract's literal `IndexNode` (which has no
    /// `units` field): chunking needs the unit structure, not the flattened
    /// `text`, so one `upsert` call keeps a node's whole-text and chunk
    /// projections from drifting apart (see `docs/STEP5_CONTRACT.md` §
    /// "One way to write node content" spirit). Ignored when the index was
    /// opened without semantic search; `Vec::new()` is always fine.
    pub units: Vec<IndexUnit>,
}

/// One rhypedb database, opened at `store.pimble/index/rhypedb/`, holding the
/// search index for a single store. Derived and disposable: deleting the
/// directory and calling [`SearchIndex::open`] again rebuilds it from
/// scratch via a fresh sequence of [`SearchIndex::upsert`] calls.
pub struct SearchIndex {
    db: Arc<Database>,
    vectorizer: Option<Arc<Vectorizer>>,
    /// This index's chunks waiting for the process's one embedding model
    /// (`crate::embed`); `Some` exactly when `vectorizer` is.
    pending: Option<Arc<crate::embed::Pending>>,
    /// Set once `search`'s hybrid path has logged a `ModelUnavailable`
    /// degrade-to-keyword-only warning, so a run of hybrid queries while the
    /// model is down (never warmed up, still downloading, or in load-retry
    /// backoff) logs once, not once per query.
    model_unavailable_warned: AtomicBool,
}

impl Drop for SearchIndex {
    /// Let go of the vector index the embedding worker's queue stores into,
    /// so the storage (and the directory's lock) goes with this index even
    /// while the worker holds the queue (`crate::embed::Pending::close`).
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.close();
        }
    }
}

impl SearchIndex {
    /// Open (creating if needed) the index at `dir`. `semantic` asks for the
    /// `Chunk` type and a live vectorizer; it has no effect unless this crate
    /// was built with the `semantic` feature — without it, the schema is
    /// always keyword-only, matching `docs/STEP5_CONTRACT.md`'s "without the
    /// semantic feature the Chunk type is omitted and no chunks are written".
    pub fn open(dir: &Path, semantic: bool) -> Result<Self> {
        let semantic = semantic && cfg!(feature = "semantic");

        std::fs::create_dir_all(dir)?;
        let schema_text = compose_schema(semantic, &[]);
        let schema = parse_schema(&schema_text).map_err(|e| SearchError::Schema(e.to_string()))?;
        let db = Database::open(schema.clone(), dir).map_err(map_engine_err)?;

        if let Err(e) = std::fs::write(dir.join(SCHEMA_HASH_FILE), schema_hash(&schema_text)) {
            tracing::debug!("SearchIndex::open: could not write schema hash marker: {e}");
        }

        Self::over(db, schema, semantic)
    }

    /// An empty index in memory only, with the same schema and analyzers as
    /// [`SearchIndex::open`]'s: nothing written anywhere, built again from
    /// the store's documents each time it opens. What a browser's worker
    /// searches with (docs/ONE_CODEBASE_CONTRACT.md section 3); it builds
    /// natively too. No thread is started: the work rhypedb leaves for later
    /// (cover refreshes, full-text backfills) waits for
    /// [`SearchIndex::run_deferred_work`], which the caller drives when idle.
    /// Keyword only: semantic search in the browser is wave 6.
    pub fn open_in_memory() -> Result<Self> {
        let schema_text = compose_schema(false, &[]);
        let schema = parse_schema(&schema_text).map_err(|e| SearchError::Schema(e.to_string()))?;
        let db = Database::open_in_memory(schema.clone(), rhypedb_engine::database::OpenOptions::default()).map_err(map_engine_err)?;
        Self::over(db, schema, false)
    }

    /// Whether this index lives in memory only ([`SearchIndex::open_in_memory`]).
    pub fn is_in_memory(&self) -> bool {
        self.db.storage().is_in_memory()
    }

    /// Run what an in-memory index left for later (see
    /// [`SearchIndex::open_in_memory`]); how many pieces of work ran. Never
    /// needed for a read to be right: a write indexes its own text, and a
    /// stale cover is read through. Nothing to do for an index on disk, whose
    /// own threads do it.
    pub fn run_deferred_work(&self) -> usize {
        self.db.run_deferred_work()
    }

    fn over(db: Arc<Database>, schema: rhypedb_schema::Schema, semantic: bool) -> Result<Self> {
        let vectorizer = if semantic {
            // `embed.cache_dir` comes from `crate::warmup::set_model_cache_dir`
            // (a `OnceLock`, not an env var) so every store's lazily-created
            // `FastEmbedder` agrees with `warm_embedding_model`'s own on where
            // the model lives, instead of falling back to fastembed's
            // CWD-relative default. `cross_encoder: Off` is already
            // `VectorizerConfig::default()`'s value; set explicitly so a
            // future default change there can't silently turn on a ~280MB
            // reranker download for every store this crate opens.
            let mut config = VectorizerConfig::default();
            config.embed.cache_dir = crate::warmup::model_cache_dir();
            config.cross_encoder = CrossEncoder::Off;

            let v = Vectorizer::with_config(
                Arc::clone(db.storage()),
                schema.clone(),
                db.type_ids().clone(),
                db.field_ids().clone(),
                config,
            )
            .map_err(map_engine_err)?;
            // No worker and no model here: the vectorizer only holds this
            // store's vector index, and `crate::embed` fills it.
            Some(Arc::new(v))
        } else {
            None
        };
        let pending = vectorizer.as_ref().map(|v| {
            let v = Arc::clone(v);
            crate::embed::Pending::register(move |rows| {
                if let Err(e) = v.ingest_vectors("Chunk", "embedding", rows) {
                    tracing::warn!("storing {} chunk embeddings failed: {e}", rows.len());
                }
            })
        });

        Ok(Self {
            db,
            vectorizer,
            pending,
            model_unavailable_warned: AtomicBool::new(false),
        })
    }

    /// Whether this index was opened with a live vectorizer (semantic search
    /// available). A hybrid [`SearchQuery`] degrades to keyword-only when
    /// this is `false`.
    pub fn semantic_enabled(&self) -> bool {
        self.vectorizer.is_some()
    }

    /// Process pending embedding jobs synchronously, returning how many ran.
    /// The background worker started in [`SearchIndex::open`] does this on
    /// its own; call this directly for deterministic tests (or a
    /// caller that wants an upsert's embeddings ready before it returns). A
    /// no-op (`Ok(0)`) without semantic search.
    pub fn process_pending_embeddings(&self) -> Result<usize> {
        match &self.pending {
            Some(p) => p.drain(),
            None => Ok(0),
        }
    }
}
