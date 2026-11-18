//! [`SearchIndex`]: one rhypedb database per open store, holding `Node`,
//! `Tag`, and (with semantic search on) `Chunk` objects.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pimble_core::{IndexUnit, NodeId};
use rhypedb_engine::database::Database;
use rhypedb_engine::object::{FieldMap, Object, Value};
use rhypedb_engine::vectorizer::{CrossEncoder, SimilarHit, Vectorizer, VectorizerConfig};
use rhypedb_engine::EngineError;
use rhypedb_schema::parser::parse_schema;

use crate::chunk::chunk_units;
use crate::error::{Result, SearchError};
use crate::query::{SearchHit, SearchQuery};
use crate::schema::{compose_schema, schema_hash, SCHEMA_HASH_FILE};

/// Cosine-distance ceiling for a semantic hit to count as a real match at
/// all, rather than "closest of a bad lot." Measured against
/// `all-MiniLM-L6-v2`'s embeddings (the only model this crate's schema
/// names): topically related passages land around 0.3-0.5 cosine distance,
/// unrelated ones around 0.7 and up, with a fuzzy band in between. 0.65 sits
/// in that band, erring toward still admitting a loosely-related hit rather
/// than a strict "must be closely related" cutoff. Without this, a query
/// with no real semantic match in the index still gets `k` hits — whichever
/// chunks happened to be least-far, however unrelated — because ANN search
/// always returns its `k` best candidates, never "none."
const SEMANTIC_MAX_DISTANCE: f32 = 0.65;

/// How much a title match counts against a match in the text.
const TITLE_WEIGHT: f32 = 2.0;
/// How much a heading match counts against a match in the text (the same
/// words are in the text too, so this is on top of it).
const HEADING_WEIGHT: f32 = 1.5;

/// The block a unit or chunk locator starts in: `"b:12"` and `"b:12/r:3"` are 12.
fn block_of(path: &str) -> Option<u32> {
    path.strip_prefix("b:")?.split('/').next()?.parse().ok()
}

/// One heading of a node, as the index keeps it (`Node.outline`).
#[derive(Debug, Clone, PartialEq)]
struct OutlineEntry {
    block: Option<u32>,
    /// Its byte offset in the node's indexed `text`, when it is found there.
    offset: Option<usize>,
    level: u8,
    text: String,
}

/// A node's headings in order: what says which heading a hit is under.
#[derive(Debug, Clone, Default, PartialEq)]
struct Outline {
    entries: Vec<OutlineEntry>,
}

impl Outline {
    /// The headings among `units`, each placed in `text` (the units' text
    /// joined by newlines, as the server builds it; searched for otherwise).
    fn of(text: &str, units: &[IndexUnit]) -> Self {
        let mut entries = Vec::new();
        let mut offset = 0usize;
        let mut searched_from = 0usize;
        for unit in units {
            if let pimble_core::UnitKind::Heading(level) = unit.kind {
                let heading = unit.text.replace(['\t', '\n'], " ");
                let at = if text.get(offset..).is_some_and(|rest| rest.starts_with(unit.text.as_str())) {
                    Some(offset)
                } else {
                    text.get(searched_from..).and_then(|rest| rest.find(unit.text.as_str())).map(|p| p + searched_from)
                };
                if let Some(at) = at {
                    searched_from = at + unit.text.len();
                }
                if !heading.trim().is_empty() {
                    entries.push(OutlineEntry { block: block_of(&unit.path), offset: at, level, text: heading.trim().to_string() });
                }
            }
            offset += unit.text.len() + 1;
        }
        Self { entries }
    }

    /// The `headings` field: one heading per line.
    fn headings_text(&self) -> String {
        self.entries.iter().map(|e| e.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// The `outline` field.
    fn encode(&self) -> String {
        let num = |n: Option<String>| n.unwrap_or_else(|| "-".to_string());
        self.entries
            .iter()
            .map(|e| format!("{}\t{}\t{}\t{}", num(e.block.map(|b| b.to_string())), num(e.offset.map(|o| o.to_string())), e.level, e.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The `outline` field read back; a line it cannot read is left out.
    fn decode(field: &str) -> Self {
        let entries = field
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(4, '\t');
                let block = parts.next()?.parse().ok();
                let offset = parts.next()?.parse().ok();
                let level = parts.next()?.parse().ok()?;
                let text = parts.next()?.to_string();
                Some(OutlineEntry { block, offset, level, text })
            })
            .collect();
        Self { entries }
    }

    /// The path of headings ending with entry `i`: it and the ones above it.
    fn path_to(&self, i: usize) -> Vec<String> {
        let mut stack: Vec<&OutlineEntry> = Vec::new();
        for e in &self.entries[..=i] {
            while stack.last().is_some_and(|top| top.level >= e.level) {
                stack.pop();
            }
            stack.push(e);
        }
        stack.into_iter().map(|e| e.text.clone()).collect()
    }

    /// The path of headings above the last entry `before` accepts.
    fn last_path(&self, before: impl Fn(&OutlineEntry) -> bool) -> Vec<String> {
        match self.entries.iter().rposition(before) {
            Some(i) => self.path_to(i),
            None => Vec::new(),
        }
    }

    /// The headings above byte `at` of the text.
    fn section_at_offset(&self, at: usize) -> Vec<String> {
        self.last_path(|e| e.offset.is_some_and(|o| o <= at))
    }

    /// The headings above block `block` (or that block itself, a heading).
    fn section_at_block(&self, block: u32) -> Vec<String> {
        self.last_path(|e| e.block.is_some_and(|b| b <= block))
    }
}

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

    /// Create or update the `Node` object for `node.node_id`, reconciling its
    /// `parent`/`links`/`tags` relationships and (with semantic search on)
    /// its `Chunk` children against `node.units`.
    pub fn upsert(&self, node: &IndexNode) -> Result<()> {
        let node_id_str = node.node_id.to_string();
        let existing = self
            .db
            .find_by_unique("Node", "node_id", &Value::String(node_id_str.clone()))
            .map_err(map_engine_err)?;

        let mut fields = FieldMap::new();
        fields.insert("kind".to_string(), Value::String(node.kind.clone()));
        fields.insert("title".to_string(), Value::String(node.title.clone()));
        fields.insert("text".to_string(), Value::String(node.text.clone()));
        let outline = Outline::of(&node.text, &node.units);
        fields.insert("headings".to_string(), Value::String(outline.headings_text()));
        fields.insert("outline".to_string(), Value::String(outline.encode()));
        fields.insert("modified_at".to_string(), Value::I64(node.modified_at));

        let object_id = match existing {
            Some(obj) => {
                self.db.update("Node", obj.id, fields).map_err(map_engine_err)?;
                obj.id
            }
            None => {
                fields.insert("node_id".to_string(), Value::String(node_id_str.clone()));
                self.db.create("Node", fields).map_err(map_engine_err)?.id
            }
        };

        self.reconcile_parent(object_id, node.parent)?;
        self.reconcile_links(object_id, &node.links)?;
        self.reconcile_tags(object_id, &node.tags)?;
        if self.vectorizer.is_some() {
            self.reconcile_chunks(object_id, &node_id_str, &node.title, &node.units)?;
        }
        Ok(())
    }

    /// Drop the `Node` for `node_id` (and, via `@on_delete(cascade)`, its
    /// chunks; its tag/link edges are removed, not their targets). A no-op if
    /// the node isn't indexed.
    pub fn remove(&self, node_id: NodeId) -> Result<()> {
        if let Some(obj) = self
            .db
            .find_by_unique("Node", "node_id", &Value::String(node_id.to_string()))
            .map_err(map_engine_err)?
        {
            self.db.delete("Node", obj.id).map_err(map_engine_err)?;
        }
        Ok(())
    }

    /// The nodes that link to `node_id` (the `backlinks` inverse of `links`).
    /// `[]` if `node_id` isn't indexed.
    pub fn backlinks(&self, node_id: NodeId) -> Result<Vec<NodeId>> {
        let Some(obj) = self
            .db
            .find_by_unique("Node", "node_id", &Value::String(node_id.to_string()))
            .map_err(map_engine_err)?
        else {
            return Ok(Vec::new());
        };
        let links = self
            .db
            .get_links("Node", obj.id, "backlinks")
            .map_err(map_engine_err)?;
        let ids: Vec<u64> = links.into_iter().map(|(id, _)| id).collect();
        let objects = self.db.get_many("Node", &ids).map_err(map_engine_err)?;
        Ok(objects
            .iter()
            .filter_map(|o| match o.fields.get("node_id") {
                Some(Value::String(s)) => NodeId::parse(s).ok(),
                _ => None,
            })
            .collect())
    }

    /// Drop every `Node` and `Tag` (and, transitively, every `Chunk`). Used
    /// before a full re-index.
    pub fn clear(&self) -> Result<()> {
        for obj in self.db.scan_type("Node").map_err(map_engine_err)? {
            self.db.delete("Node", obj.id).map_err(map_engine_err)?;
        }
        for obj in self.db.scan_type("Tag").map_err(map_engine_err)? {
            // Every tag's links were already removed above; a delete here
            // can't be denied. Ignore a benign race against a concurrent
            // clear rather than fail the whole sweep.
            let _ = self.db.delete("Tag", obj.id);
        }
        Ok(())
    }

    /// Run `q` and return up to `q.limit` hits: keyword-only when
    /// `q.semantic` is false or this index has no vectorizer, otherwise a
    /// reciprocal-rank-fusion (k = 60) blend of keyword and semantic results.
    ///
    /// Degrades to keyword-only (rather than failing the query) if the
    /// embedding model isn't available right now — still downloading, in
    /// load-retry backoff, or never warmed up — logging a warning the first
    /// time that happens for this index, not on every subsequent query.
    pub fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>> {
        let keyword_hits = self.keyword_search(&q.text, q.limit.max(1))?;
        if !q.semantic || self.vectorizer.is_none() {
            let mut hits = keyword_hits;
            hits.truncate(q.limit);
            return Ok(hits);
        }
        let semantic_hits = match self.semantic_search(&q.text, (q.limit * 3).max(1)) {
            Ok(hits) => hits,
            Err(SearchError::ModelUnavailable(msg)) => {
                if !self.model_unavailable_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "Semantic search unavailable ({msg}); degrading hybrid queries to \
                         keyword-only until the embedding model loads"
                    );
                }
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        Ok(hybrid_merge(keyword_hits, semantic_hits, q.limit))
    }

    /// The ordinals and content hashes of `node_id`'s current chunks, in
    /// ordinal order — a debugging/verification hook (not part of the
    /// contract's literal API): lets a caller (or a test) confirm which
    /// chunks changed across a re-index without re-deriving the whole
    /// object. `[]` without semantic search, or if the node isn't indexed.
    pub fn chunk_hashes(&self, node_id: NodeId) -> Result<Vec<(u32, String)>> {
        let Some(obj) = self
            .db
            .find_by_unique("Node", "node_id", &Value::String(node_id.to_string()))
            .map_err(map_engine_err)?
        else {
            return Ok(Vec::new());
        };
        let links = match self.db.get_links("Node", obj.id, "chunks") {
            Ok(links) => links,
            Err(_) => return Ok(Vec::new()), // no `chunks` field: not semantic.
        };
        let mut out = Vec::with_capacity(links.len());
        for (chunk_id, _) in links {
            let chunk_obj = self.db.get("Chunk", chunk_id).map_err(map_engine_err)?;
            let Some(Value::U32(ordinal)) = chunk_obj.fields.get("ordinal") else {
                continue;
            };
            let hash = match chunk_obj.fields.get("hash") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            out.push((*ordinal, hash));
        }
        out.sort_by_key(|(ordinal, _)| *ordinal);
        Ok(out)
    }

    // -- relationship reconciliation -------------------------------------

    fn reconcile_parent(&self, object_id: u64, parent: Option<NodeId>) -> Result<()> {
        let desired: HashSet<u64> = match parent {
            Some(p) => self.resolve_node_ids(std::slice::from_ref(&p))?.into_iter().collect(),
            None => HashSet::new(),
        };
        self.reconcile_edges(object_id, "parent", &desired)
    }

    fn reconcile_links(&self, object_id: u64, links: &[NodeId]) -> Result<()> {
        let desired: HashSet<u64> = self.resolve_node_ids(links)?.into_iter().collect();
        self.reconcile_edges(object_id, "links", &desired)
    }

    fn reconcile_tags(&self, object_id: u64, tags: &[String]) -> Result<()> {
        let mut desired = HashSet::with_capacity(tags.len());
        for name in tags {
            desired.insert(self.find_or_create_tag(name)?);
        }
        self.reconcile_edges(object_id, "tags", &desired)
    }

    /// Resolve pimble [`NodeId`]s to rhypedb object ids, silently dropping
    /// any that aren't indexed yet (a forward reference to a node this
    /// indexer hasn't reached; the link appears once that node is upserted).
    fn resolve_node_ids(&self, ids: &[NodeId]) -> Result<Vec<u64>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self
                .db
                .find_by_unique("Node", "node_id", &Value::String(id.to_string()))
                .map_err(map_engine_err)?
            {
                Some(obj) => out.push(obj.id),
                None => tracing::debug!("SearchIndex: link target {id} isn't indexed yet, skipping"),
            }
        }
        Ok(out)
    }

    fn find_or_create_tag(&self, name: &str) -> Result<u64> {
        if let Some(obj) = self
            .db
            .find_by_unique("Tag", "name", &Value::String(name.to_string()))
            .map_err(map_engine_err)?
        {
            return Ok(obj.id);
        }
        let mut fields = FieldMap::new();
        fields.insert("name".to_string(), Value::String(name.to_string()));
        match self.db.create("Tag", fields) {
            Ok(obj) => Ok(obj.id),
            Err(_) => self
                .db
                .find_by_unique("Tag", "name", &Value::String(name.to_string()))
                .map_err(map_engine_err)?
                .map(|o| o.id)
                .ok_or_else(|| SearchError::Engine(format!("tag '{name}' vanished after a failed create"))),
        }
    }

    /// Make `Node.<field_name>` on `source_id` point at exactly `desired`,
    /// unlinking anything extra and linking anything missing. Works
    /// uniformly for to-one (`parent`) and to-many (`links`, `tags`) fields —
    /// `link`/`unlink`/`get_links` don't distinguish arity.
    fn reconcile_edges(&self, source_id: u64, field_name: &str, desired: &HashSet<u64>) -> Result<()> {
        let current: HashSet<u64> = self
            .db
            .get_links("Node", source_id, field_name)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        for id in current.difference(desired) {
            self.db
                .unlink("Node", source_id, field_name, *id)
                .map_err(map_engine_err)?;
        }
        for id in desired.difference(&current) {
            self.db
                .link("Node", source_id, field_name, *id, None)
                .map_err(map_engine_err)?;
        }
        Ok(())
    }

    // -- chunks -----------------------------------------------------------

    fn reconcile_chunks(
        &self,
        node_object_id: u64,
        node_id_str: &str,
        title: &str,
        units: &[IndexUnit],
    ) -> Result<()> {
        let specs = chunk_units(title, units);

        let existing_links = self
            .db
            .get_links("Node", node_object_id, "chunks")
            .map_err(map_engine_err)?;
        let mut existing_by_ordinal: HashMap<u32, (u64, String)> = HashMap::new();
        for (chunk_id, _edge_fields) in &existing_links {
            let obj = self.db.get("Chunk", *chunk_id).map_err(map_engine_err)?;
            let Some(Value::U32(ordinal)) = obj.fields.get("ordinal") else {
                continue;
            };
            let hash = match obj.fields.get("hash") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            existing_by_ordinal.insert(*ordinal, (*chunk_id, hash));
        }

        let mut seen_ordinals: HashSet<u32> = HashSet::with_capacity(specs.len());
        for spec in &specs {
            seen_ordinals.insert(spec.ordinal);
            match existing_by_ordinal.get(&spec.ordinal) {
                Some((_chunk_id, hash)) if hash == &spec.hash => {
                    // Unchanged text: leave it (and its embedding) alone.
                }
                Some((chunk_id, _)) => {
                    let mut fields = FieldMap::new();
                    fields.insert("kind".to_string(), Value::String(spec.kind.clone()));
                    fields.insert("path".to_string(), Value::String(spec.path.clone()));
                    fields.insert("text".to_string(), Value::String(spec.text.clone()));
                    fields.insert("source".to_string(), Value::String(spec.source.clone()));
                    fields.insert("hash".to_string(), Value::String(spec.hash.clone()));
                    self.db.update("Chunk", *chunk_id, fields).map_err(map_engine_err)?;
                    self.enqueue_embedding(*chunk_id, &spec.source);
                }
                None => {
                    let mut fields = FieldMap::new();
                    fields.insert(
                        "chunk_id".to_string(),
                        Value::String(format!("{node_id_str}:{}", spec.ordinal)),
                    );
                    fields.insert("ordinal".to_string(), Value::U32(spec.ordinal));
                    fields.insert("kind".to_string(), Value::String(spec.kind.clone()));
                    fields.insert("path".to_string(), Value::String(spec.path.clone()));
                    fields.insert("text".to_string(), Value::String(spec.text.clone()));
                    fields.insert("source".to_string(), Value::String(spec.source.clone()));
                    fields.insert("hash".to_string(), Value::String(spec.hash.clone()));
                    let obj = self.db.create("Chunk", fields).map_err(map_engine_err)?;
                    self.db
                        .link("Chunk", obj.id, "node", node_object_id, None)
                        .map_err(map_engine_err)?;
                    self.enqueue_embedding(obj.id, &spec.source);
                }
            }
        }

        for (ordinal, (chunk_id, _)) in &existing_by_ordinal {
            if !seen_ordinals.contains(ordinal) {
                self.db.delete("Chunk", *chunk_id).map_err(map_engine_err)?;
            }
        }

        Ok(())
    }

    /// Queue a chunk's text for the process's embedding model
    /// (`crate::embed`), whose worker stores the vector in this index. A no-op
    /// without semantic search.
    fn enqueue_embedding(&self, chunk_object_id: u64, source: &str) {
        if let Some(pending) = &self.pending {
            pending.push(chunk_object_id, source.to_string());
        }
    }

    // -- search -------------------------------------------------------------

    /// Run the same full-text query over `title`, `text` and `headings`.
    fn fulltext_all(&self, query: &str, k: usize) -> Result<[Vec<rhypedb_engine::fulltext::FulltextHit>; 3]> {
        let field = |name: &str| -> Result<Vec<rhypedb_engine::fulltext::FulltextHit>> {
            Ok(self.db.fulltext_search("Node", name, query, k, None, None).map_err(map_fulltext_err)?.hits)
        };
        Ok([field("title")?, field("text")?, field("headings")?])
    }

    fn keyword_search(&self, query_text: &str, limit: usize) -> Result<Vec<SearchHit>> {
        if query_text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let k = (limit * 4).max(20);

        // Search-as-you-type: the last word the user is still typing is a prefix
        // term. If the engine refuses the expansion (too short after analysis, or
        // more than its cap of distinct terms), fall back to the literal query.
        let typed = with_trailing_prefix(query_text);
        let [title_hits, text_hits, heading_hits] = match self.fulltext_all(&typed, k) {
            Ok(hits) => hits,
            Err(_) if typed != query_text => self.fulltext_all(query_text, k)?,
            Err(e) => return Err(e),
        };

        let mut combined: HashMap<u64, f32> = HashMap::new();
        for h in &title_hits {
            *combined.entry(h.object_id).or_insert(0.0) += h.score * TITLE_WEIGHT;
        }
        for h in &text_hits {
            *combined.entry(h.object_id).or_insert(0.0) += h.score;
        }
        for h in &heading_hits {
            *combined.entry(h.object_id).or_insert(0.0) += h.score * HEADING_WEIGHT;
        }

        let mut ranked: Vec<(u64, f32)> = combined.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // Every candidate is judged for an exact title or heading before the
        // cut, so one that BM25 put low still comes up.
        ranked.truncate(k);

        let ids: Vec<u64> = ranked.iter().map(|(id, _)| *id).collect();
        let objects_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Node", &ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        let wanted = pimble_crdt::markdown::loose(query_text);
        let mut hits: Vec<(Exact, SearchHit)> = ranked
            .into_iter()
            .filter_map(|(id, score)| {
                let obj = objects_by_id.get(&id)?;
                keyword_hit(obj, score, query_text, &wanted)
            })
            .collect();
        // A note titled exactly the query first, then one with a heading that
        // is exactly it (ignoring case and punctuation), then the rest; each
        // group in score order (the sort is stable).
        hits.sort_by_key(|(exact, _)| std::cmp::Reverse(*exact));
        hits.truncate(limit);
        Ok(hits.into_iter().map(|(_, hit)| hit).collect())
    }

    fn semantic_search(&self, query_text: &str, k: usize) -> Result<Vec<SearchHit>> {
        let Some(vectorizer) = &self.vectorizer else {
            return Ok(Vec::new());
        };
        if query_text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let ef = k.max(64);
        // `exact_rescore: false` — the cheap ANN (TurboQuant) distance
        // estimate, not the full-precision-over-the-LSM rescore. Returns
        // `EngineError::ModelUnavailable` (mapped to
        // `SearchError::ModelUnavailable` below) rather than blocking if the
        // model hasn't loaded yet; `search`'s caller degrades to
        // keyword-only on that specific error.
        // The query is embedded once for every store's index (`crate::embed`).
        let query_vec = crate::embed::embed_query(query_text)?;
        let candidates = vectorizer
            .search_vector("Chunk", "embedding", &query_vec, k, ef, false, None)
            .map_err(map_engine_err)?;
        // Drop anything too far to count as a real match before grouping by
        // node, so a query with no real semantic match in the index yields
        // few or no semantic hits instead of `k` random ones (ANN search
        // always returns its `k` best candidates, never "none" — this is
        // what turns "best of a bad lot" into "none of these are good
        // enough"). See SEMANTIC_MAX_DISTANCE's doc comment.
        let candidates = drop_hits_beyond_max_distance(candidates);
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let chunk_ids: Vec<u64> = candidates.iter().map(|hit| hit.object_id).collect();
        let chunk_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Chunk", &chunk_ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        // `candidates` already comes back in rank order — `rerank_score`
        // descending where the (disabled-by-default, see `SearchIndex::open`)
        // cross-encoder ran, else `distance` ascending; see `SimilarHit`'s
        // doc comment in rhypedb-engine. So keeping the FIRST chunk seen per
        // node, in this order, is that node's best chunk by the engine's own
        // ranking — no re-sort belongs here. Re-sorting this list by
        // `distance` (as this function used to) is exactly the bug fixed by
        // rhypedb #18's `SimilarHit`: doing that against a list the
        // cross-encoder had already ranked by an unrelated, higher-is-better
        // relevance score put the least-relevant chunk first.
        let mut seen_nodes: HashSet<u64> = HashSet::new();
        let mut best_per_node: Vec<(u64, f32, u64)> = Vec::new(); // (node_id, distance, chunk_id)
        for hit in &candidates {
            let node_id = self
                .db
                .get_links("Chunk", hit.object_id, "node")
                .map_err(map_engine_err)?
                .into_iter()
                .next()
                .map(|(id, _)| id);
            let Some(node_id) = node_id else { continue };
            if seen_nodes.insert(node_id) {
                best_per_node.push((node_id, hit.distance, hit.object_id));
            }
        }

        let node_ids: Vec<u64> = best_per_node.iter().map(|(node_id, ..)| *node_id).collect();
        let node_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Node", &node_ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        Ok(best_per_node
            .into_iter()
            .filter_map(|(node_id, distance, chunk_id)| {
                let node_obj = node_by_id.get(&node_id)?;
                let chunk_obj = chunk_by_id.get(&chunk_id)?;
                chunk_hit(node_obj, chunk_obj, distance)
            })
            .collect())
    }
}

/// Drop any hit farther than [`SEMANTIC_MAX_DISTANCE`]. Preserves `hits`'
/// existing order among survivors (a plain `retain`, not a re-sort).
fn drop_hits_beyond_max_distance(mut hits: Vec<SimilarHit>) -> Vec<SimilarHit> {
    hits.retain(|hit| hit.distance <= SEMANTIC_MAX_DISTANCE);
    hits
}

fn string_field(obj: &Object, name: &str) -> String {
    match obj.fields.get(name) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// How exactly a node matched a query as a whole: its title, one of its
/// headings, or neither (ordered so the better match is the greater).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Exact {
    No,
    Heading,
    Title,
}

/// A keyword hit on a whole node: its snippet around where the query is in
/// its text and the headings above that place. `wanted` is the query made
/// [`pimble_crdt::markdown::loose`].
fn keyword_hit(obj: &Object, score: f32, query_text: &str, wanted: &str) -> Option<(Exact, SearchHit)> {
    let node_id = match obj.fields.get("node_id") {
        Some(Value::String(s)) => NodeId::parse(s).ok()?,
        _ => return None,
    };
    let title = string_field(obj, "title");
    let text = string_field(obj, "text");
    let outline = Outline::decode(&string_field(obj, "outline"));
    let heading = (!wanted.is_empty())
        .then(|| outline.entries.iter().position(|e| pimble_crdt::markdown::loose(&e.text) == wanted))
        .flatten();
    let exact = if !wanted.is_empty() && pimble_crdt::markdown::loose(&title) == wanted {
        Exact::Title
    } else if heading.is_some() {
        Exact::Heading
    } else {
        Exact::No
    };
    // Where the hit is: the heading the query names, else where its words are.
    let at = heading.and_then(|i| outline.entries[i].offset).or_else(|| match_position(&text, query_text));
    let section = match (heading, at) {
        (Some(i), _) => outline.path_to(i),
        (None, Some(at)) => outline.section_at_offset(at),
        (None, None) => Vec::new(),
    };
    Some((
        exact,
        SearchHit { node_id, score, title, snippet: snippet_at(&text, at), kind: "node".to_string(), path: None, section },
    ))
}

/// `distance` is `SimilarHit::distance` from the hit `semantic_search` chose
/// for this node — always a true metric distance (never a `rerank_score`;
/// `semantic_search` never reads that field, and `SearchIndex::open` keeps
/// the cross-encoder off, so it's always `None` here regardless). Purely for
/// *reporting* a "higher is better" score on `SearchHit`, uniform with
/// keyword hits' BM25 convention — this function has no say in hit order,
/// which `semantic_search` already fixed by the time it gets here.
fn chunk_hit(node_obj: &Object, chunk_obj: &Object, distance: f32) -> Option<SearchHit> {
    let node_id = match node_obj.fields.get("node_id") {
        Some(Value::String(s)) => NodeId::parse(s).ok()?,
        _ => return None,
    };
    let title = match node_obj.fields.get("title") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let snippet = match chunk_obj.fields.get("text") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let kind = match chunk_obj.fields.get("kind") {
        Some(Value::String(s)) => s.clone(),
        _ => "prose".to_string(),
    };
    let path = match chunk_obj.fields.get("path") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    let score = 1.0 / (1.0 + distance.max(0.0));
    // The chunk's first block, from its locator ("b:12", "b:12/r:3/c:1").
    let block = path.as_deref().and_then(block_of);
    let section = match block {
        Some(block) => Outline::decode(&string_field(node_obj, "outline")).section_at_block(block),
        None => Vec::new(),
    };
    Some(SearchHit {
        node_id,
        score,
        title,
        snippet,
        kind,
        path,
        section,
    })
}

/// Where `query_text` is in `text` (a byte offset into it): the whole query,
/// in any case; else its first word that is not a stop word; else its first
/// word. `None` when none of them is in it (or the query is only syntax).
fn match_position(text: &str, query_text: &str) -> Option<usize> {
    // Lowercasing can change a text's length (rarely); a position found in
    // the lowercased text is then near, not at, the place, and kept on a
    // char boundary of the original.
    let lower_text = text.to_lowercase();
    let terms: Vec<String> = query_text
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| c == '+' || c == '"' || c == '*').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let whole = terms.join(" ");
    let found = (terms.len() > 1)
        .then(|| lower_text.find(&whole))
        .flatten()
        .or_else(|| terms.iter().filter(|t| !PREFIX_STOP_WORDS.contains(&t.as_str())).find_map(|t| lower_text.find(t.as_str())))
        .or_else(|| terms.first().and_then(|t| lower_text.find(t.as_str())))?;
    Some(floor_boundary(text, found))
}

/// [`snippet_at`] around where the query is in `text`.
#[cfg(test)]
fn snippet_around(text: &str, query_text: &str) -> String {
    snippet_at(text, match_position(text, query_text))
}

/// A window of about 160 chars of `text` around byte `at`, or its first 160
/// chars without one: a best-effort preview, never an error. The window is
/// snapped outward to whitespace on each side it doesn't already touch (so it
/// never opens or closes mid-word), each such trimmed side gets an "…", and
/// every cut stays on a UTF-8 char boundary.
fn snippet_at(text: &str, at: Option<usize>) -> String {
    const WINDOW: usize = 160;
    if text.is_empty() {
        return String::new();
    }
    let (start, end) = match at {
        Some(byte_pos) => (
            byte_pos.saturating_sub(WINDOW / 2),
            (byte_pos + WINDOW / 2).min(text.len()),
        ),
        None => (0, WINDOW.min(text.len())),
    };
    let start = floor_boundary(text, start);
    let end = ceil_boundary(text, end);

    let snapped_start = snap_start_to_word_boundary(text, start, end);
    let snapped_end = snap_end_to_word_boundary(text, start, end);
    // A window with no whitespace at all in it (one very long "word") can't be
    // snapped without emptying it — fall back to the unsnapped, but still
    // char-boundary-safe, window rather than show nothing.
    let (start, end) = if snapped_start < snapped_end {
        (snapped_start, snapped_end)
    } else {
        (start, end)
    };

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push('…');
    }
    out
}

/// If `start` isn't already the text's start, advance it past the next
/// whitespace char in `text[start..end]` so the snippet opens on a fresh
/// word instead of mid-word. Left unchanged if `start == 0` or the range has
/// no whitespace to snap to.
fn snap_start_to_word_boundary(text: &str, start: usize, end: usize) -> usize {
    if start == 0 {
        return start;
    }
    match text[start..end].char_indices().find(|(_, c)| c.is_whitespace()) {
        Some((offset, ch)) => start + offset + ch.len_utf8(),
        None => start,
    }
}

/// If `end` isn't already the text's end, retreat it to the last whitespace
/// char in `text[start..end]` so the snippet closes at the end of a word
/// instead of mid-word. Left unchanged if `end == text.len()` or the range
/// has no whitespace to snap to.
fn snap_end_to_word_boundary(text: &str, start: usize, end: usize) -> usize {
    if end == text.len() {
        return end;
    }
    match text[start..end].char_indices().rev().find(|(_, c)| c.is_whitespace()) {
        Some((offset, _)) => start + offset,
        None => end,
    }
}

fn floor_boundary(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_boundary(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Reciprocal rank fusion (k = 60) of two ranked hit lists, deduped by node.
/// Where a node appears in both, the semantic hit's `kind`/`path`/`snippet`
/// win (more specific — a chunk locator beats "somewhere in this node").
fn hybrid_merge(keyword_hits: Vec<SearchHit>, semantic_hits: Vec<SearchHit>, limit: usize) -> Vec<SearchHit> {
    const RRF_K: f32 = 60.0;
    let mut rrf: HashMap<NodeId, f32> = HashMap::new();
    let mut hit_by_node: HashMap<NodeId, SearchHit> = HashMap::new();

    for (rank, hit) in keyword_hits.into_iter().enumerate() {
        *rrf.entry(hit.node_id).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
        hit_by_node.entry(hit.node_id).or_insert(hit);
    }
    for (rank, hit) in semantic_hits.into_iter().enumerate() {
        *rrf.entry(hit.node_id).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
        hit_by_node.insert(hit.node_id, hit);
    }

    let mut ranked: Vec<(NodeId, f32)> = rrf.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);

    ranked
        .into_iter()
        .filter_map(|(id, score)| {
            hit_by_node.remove(&id).map(|mut h| {
                h.score = score;
                h
            })
        })
        .collect()
}

fn map_fulltext_err(e: EngineError) -> SearchError {
    match e {
        EngineError::FulltextIndexBuilding { indexed, total, .. } => {
            SearchError::IndexBuilding { done: indexed, total }
        }
        EngineError::FulltextQuery(msg) => SearchError::Query(msg),
        other => map_engine_err(other),
    }
}

pub(crate) fn map_engine_err(e: EngineError) -> SearchError {
    match e {
        EngineError::FulltextIndexBuilding { indexed, total, .. } => {
            SearchError::IndexBuilding { done: indexed, total }
        }
        EngineError::FulltextQuery(msg) => SearchError::Query(msg),
        EngineError::ModelUnavailable(msg) => SearchError::ModelUnavailable(msg),
        other => SearchError::Engine(other.to_string()),
    }
}


/// Turn the last word of a query into a prefix term (`came` -> `came*`) so
/// results update while the user is still typing. Leaves the query alone when
/// the last token already ends a phrase or a prefix, is inside an open quote,
/// or is shorter than two characters (the engine rejects those).
pub(crate) fn with_trailing_prefix(query: &str) -> String {
    let trimmed = query.trim_end();
    if trimmed.is_empty() || trimmed.len() != query.len() {
        // Trailing whitespace means the last word is finished.
        return query.to_string();
    }
    if trimmed.matches('"').count() % 2 == 1 {
        return query.to_string();
    }
    let last = trimmed.rsplit(char::is_whitespace).next().unwrap_or("");
    let word = last.trim_start_matches('+');
    if word.ends_with('*') || word.ends_with('"') {
        return query.to_string();
    }
    if word.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
        return query.to_string();
    }
    // rhypedb's english analyzer drops stop words, but a prefix term bypasses
    // that: "of*" would match "office" and "the*" "theocratic". A stop word the
    // user is still typing is far more often the word itself than a prefix.
    if PREFIX_STOP_WORDS.contains(&word.to_lowercase().as_str()) {
        return query.to_string();
    }
    format!("{trimmed}*")
}

/// Short English function words that are never expanded into prefix terms.
const PREFIX_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "do", "for", "from", "had",
    "has", "have", "he", "her", "his", "how", "i", "if", "in", "is", "it", "its", "me",
    "my", "no", "not", "of", "on", "or", "our", "she", "so", "that", "the", "their", "them",
    "they", "this", "to", "up", "us", "was", "we", "were", "what", "when", "who", "will",
    "with", "you", "your",
];

#[cfg(test)]
mod tests {
    #[test]
    fn trailing_prefix_rules() {
        assert_eq!(with_trailing_prefix("came"), "came*");
        assert_eq!(with_trailing_prefix("security came"), "security came*");
        assert_eq!(with_trailing_prefix("came "), "came ");
        assert_eq!(with_trailing_prefix("came*"), "came*");
        assert_eq!(with_trailing_prefix("\"security cam"), "\"security cam");
        assert_eq!(with_trailing_prefix("\"security cameras\""), "\"security cameras\"");
        assert_eq!(with_trailing_prefix("c"), "c");
        assert_eq!(with_trailing_prefix("+cam"), "+cam*");
        assert_eq!(with_trailing_prefix("of"), "of");
        assert_eq!(with_trailing_prefix("video of"), "video of");
        assert_eq!(with_trailing_prefix("The"), "The");
        assert_eq!(with_trailing_prefix("theo"), "theo*");
    }

    use super::*;

    #[test]
    fn a_hit_at_distance_0_9_is_dropped() {
        let hits = vec![
            SimilarHit { object_id: 1, distance: 0.4, rerank_score: None },
            SimilarHit { object_id: 2, distance: 0.9, rerank_score: None },
        ];
        let kept = drop_hits_beyond_max_distance(hits);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].object_id, 1);
    }

    #[test]
    fn a_hit_exactly_at_the_cutoff_is_kept() {
        let hits = vec![SimilarHit {
            object_id: 1,
            distance: SEMANTIC_MAX_DISTANCE,
            rerank_score: None,
        }];
        assert_eq!(drop_hits_beyond_max_distance(hits).len(), 1);
    }

    /// `snippet`'s content (ellipses stripped) must be a substring of
    /// `original` that starts and ends on a whitespace boundary (or at
    /// `original`'s own start/end) — never mid-word.
    fn assert_no_partial_word(original: &str, snippet: &str) {
        let core = snippet.strip_prefix('…').unwrap_or(snippet);
        let core = core.strip_suffix('…').unwrap_or(core);
        if core.is_empty() {
            return;
        }
        let idx = original
            .find(core)
            .expect("snippet core should be a substring of the original text");
        if idx > 0 {
            let before = original[..idx].chars().last().unwrap();
            assert!(before.is_whitespace(), "snippet started mid-word: {snippet:?}");
        }
        let end = idx + core.len();
        if end < original.len() {
            let after = original[end..].chars().next().unwrap();
            assert!(after.is_whitespace(), "snippet ended mid-word: {snippet:?}");
        }
    }

    #[test]
    fn snippet_never_starts_or_ends_mid_word() {
        // A window centered on "keyword" with plenty of filler on each side
        // reliably lands the raw byte window mid-word on both edges before
        // snapping.
        let filler = "alphabet ".repeat(30);
        let text = format!("{filler}keyword{filler}");
        let snippet = snippet_around(&text, "keyword");
        assert_no_partial_word(&text, &snippet);
        assert!(snippet.starts_with('…'), "snippet was: {snippet:?}");
        assert!(snippet.ends_with('…'), "snippet was: {snippet:?}");
    }

    #[test]
    fn snippet_handles_a_multibyte_character_near_the_window_edge() {
        // Multi-byte characters (é, ö) sit throughout the filler on both
        // sides, right where the raw byte window's edges fall.
        let filler = "héllo wörld café ".repeat(15);
        let text = format!("{filler}keyword{filler}");
        let snippet = snippet_around(&text, "keyword");
        assert!(snippet.contains("keyword"));
        assert_no_partial_word(&text, &snippet);
    }

    #[test]
    fn snippet_has_no_ellipsis_when_the_window_touches_a_text_edge() {
        let text = "keyword right at the very start of a short text";
        let snippet = snippet_around(text, "keyword");
        assert!(!snippet.starts_with('…'), "snippet was: {snippet:?}");
        assert_no_partial_word(text, &snippet);
    }

    #[test]
    fn snippet_falls_back_gracefully_with_no_whitespace_to_snap_to() {
        // One giant "word" longer than the whole window: snapping can't find
        // whitespace, so it must fall back to the char-boundary-safe window
        // instead of panicking or returning an empty string.
        let text = "x".repeat(50) + "keyword" + &"y".repeat(500);
        let snippet = snippet_around(&text, "keyword");
        assert!(!snippet.is_empty());
        assert!(snippet.contains("keyword"));
    }
}

#[cfg(test)]
mod outline_tests {
    use super::*;
    use pimble_core::UnitKind;

    #[test]
    fn an_outline_places_headings_and_reads_back() {
        let units = vec![
            IndexUnit::new(UnitKind::Prose, "b:0", "intro"),
            IndexUnit::new(UnitKind::Heading(1), "b:1", "Design"),
            IndexUnit::new(UnitKind::Prose, "b:2", "body"),
            IndexUnit::new(UnitKind::Heading(2), "b:3", "Edges"),
            IndexUnit::new(UnitKind::Heading(1), "b:4", "Budget"),
        ];
        let text = "intro\nDesign\nbody\nEdges\nBudget";
        let outline = Outline::of(text, &units);
        assert_eq!(Outline::decode(&outline.encode()), outline);
        assert_eq!(outline.entries[1].offset, Some(text.find("Edges").unwrap()));
        assert_eq!(outline.section_at_block(3), vec!["Design", "Edges"]);
        assert_eq!(outline.section_at_block(2), vec!["Design"]);
        assert_eq!(outline.section_at_block(0), Vec::<String>::new());
        assert_eq!(outline.section_at_offset(text.find("Budget").unwrap()), vec!["Budget"]);
        assert_eq!(block_of("b:12/r:3/c:1"), Some(12));
    }
}
