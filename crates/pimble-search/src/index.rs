//! [`SearchIndex`]: one rhypedb database per open store, holding `Node`,
//! `Tag`, and (with semantic search on) `Chunk` objects.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use pimble_core::{IndexUnit, NodeId};
use rhypedb_engine::database::Database;
use rhypedb_engine::object::{FieldMap, Object, Value};
use rhypedb_engine::vectorizer::{VectorizeJob, Vectorizer};
use rhypedb_engine::EngineError;
use rhypedb_schema::parser::parse_schema;

use crate::chunk::chunk_units;
use crate::error::{Result, SearchError};
use crate::query::{SearchHit, SearchQuery};
use crate::schema::{compose_schema, schema_hash, SCHEMA_HASH_FILE};

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
    /// `ContentDoc::units()` or a plugin's `index_units`) — the chunker's
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
            let v = Vectorizer::new(
                Arc::clone(db.storage()),
                schema.clone(),
                db.type_ids().clone(),
                db.field_ids().clone(),
            )
            .map_err(map_engine_err)?;
            let v = Arc::new(v);
            v.start_worker(1);
            Some(v)
        } else {
            None
        };

        Ok(Self { db, vectorizer })
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
        match &self.vectorizer {
            Some(v) => v.process_pending().map_err(map_engine_err),
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
    pub fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>> {
        let keyword_hits = self.keyword_search(&q.text, q.limit.max(1))?;
        if !q.semantic || self.vectorizer.is_none() {
            let mut hits = keyword_hits;
            hits.truncate(q.limit);
            return Ok(hits);
        }
        let semantic_hits = self.semantic_search(&q.text, (q.limit * 3).max(1))?;
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
                    let obj = self.db.update("Chunk", *chunk_id, fields).map_err(map_engine_err)?;
                    self.enqueue_vectorize(&obj);
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
                    self.enqueue_vectorize(&obj);
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

    /// Mirrors `rhypedb-server`'s own `enqueue_vectorize`: queue a background
    /// embedding job for every `@vectorize` field on `obj`'s type. A no-op
    /// without a live vectorizer.
    fn enqueue_vectorize(&self, obj: &Object) {
        let Some(vectorizer) = &self.vectorizer else {
            return;
        };
        let schema = self.db.schema();
        if let Some(type_def) = schema.get_type(&obj.type_name) {
            for field in &type_def.fields {
                if let Some(vec_def) = field.vectorize() {
                    let _ = vectorizer.enqueue(VectorizeJob {
                        type_name: obj.type_name.clone(),
                        object_id: obj.id,
                        source_field: vec_def.source_field.clone(),
                        vector_field: field.name.clone(),
                        model: vec_def.model.clone(),
                    });
                }
            }
        }
    }

    // -- search -------------------------------------------------------------

    fn keyword_search(&self, query_text: &str, limit: usize) -> Result<Vec<SearchHit>> {
        if query_text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let k = (limit * 4).max(20);

        let title_hits = self
            .db
            .fulltext_search("Node", "title", query_text, k, None, None)
            .map_err(map_fulltext_err)?
            .hits;
        let text_hits = self
            .db
            .fulltext_search("Node", "text", query_text, k, None, None)
            .map_err(map_fulltext_err)?
            .hits;

        let mut combined: HashMap<u64, f32> = HashMap::new();
        for h in &title_hits {
            *combined.entry(h.object_id).or_insert(0.0) += h.score * 2.0;
        }
        for h in &text_hits {
            *combined.entry(h.object_id).or_insert(0.0) += h.score;
        }

        let mut ranked: Vec<(u64, f32)> = combined.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(limit);

        let ids: Vec<u64> = ranked.iter().map(|(id, _)| *id).collect();
        let objects_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Node", &ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        Ok(ranked
            .into_iter()
            .filter_map(|(id, score)| {
                let obj = objects_by_id.get(&id)?;
                node_hit(obj, score, query_text, "node", None)
            })
            .collect())
    }

    fn semantic_search(&self, query_text: &str, k: usize) -> Result<Vec<SearchHit>> {
        let Some(vectorizer) = &self.vectorizer else {
            return Ok(Vec::new());
        };
        if query_text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let ef = k.max(64);
        let candidates = vectorizer
            .search_text("Chunk", "embedding", query_text, k, ef, false, None)
            .map_err(map_engine_err)?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let chunk_ids: Vec<u64> = candidates.iter().map(|(id, _)| *id).collect();
        let chunk_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Chunk", &chunk_ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        // Group by node, keeping each node's best (lowest-distance) chunk.
        let mut best_per_node: HashMap<u64, (f32, u64)> = HashMap::new();
        for (chunk_id, distance) in &candidates {
            let node_id = self
                .db
                .get_links("Chunk", *chunk_id, "node")
                .map_err(map_engine_err)?
                .into_iter()
                .next()
                .map(|(id, _)| id);
            let Some(node_id) = node_id else { continue };
            best_per_node
                .entry(node_id)
                .and_modify(|(best_dist, best_chunk)| {
                    if *distance < *best_dist {
                        *best_dist = *distance;
                        *best_chunk = *chunk_id;
                    }
                })
                .or_insert((*distance, *chunk_id));
        }

        let node_ids: Vec<u64> = best_per_node.keys().copied().collect();
        let node_by_id: HashMap<u64, Object> = self
            .db
            .get_many("Node", &node_ids)
            .map_err(map_engine_err)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();

        let mut ranked: Vec<(u64, f32, u64)> = best_per_node
            .into_iter()
            .map(|(node_id, (dist, chunk_id))| (node_id, dist, chunk_id))
            .collect();
        ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(ranked
            .into_iter()
            .filter_map(|(node_id, distance, chunk_id)| {
                let node_obj = node_by_id.get(&node_id)?;
                let chunk_obj = chunk_by_id.get(&chunk_id)?;
                chunk_hit(node_obj, chunk_obj, distance)
            })
            .collect())
    }
}

fn node_hit(obj: &Object, score: f32, query_text: &str, kind: &str, path: Option<String>) -> Option<SearchHit> {
    let node_id = match obj.fields.get("node_id") {
        Some(Value::String(s)) => NodeId::parse(s).ok()?,
        _ => return None,
    };
    let title = match obj.fields.get("title") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let text = match obj.fields.get("text") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    Some(SearchHit {
        node_id,
        score,
        title,
        snippet: snippet_around(&text, query_text),
        kind: kind.to_string(),
        path,
    })
}

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
    // Distance (lower = closer) -> a "higher is better" score, uniform with
    // keyword hits' BM25 convention.
    let score = 1.0 / (1.0 + distance.max(0.0));
    Some(SearchHit {
        node_id,
        score,
        title,
        snippet,
        kind,
        path,
    })
}

/// First query term's window (about 160 chars) in `text`, or its first 160
/// chars if the term isn't found (or the query is unparseable-for-snippet
/// purposes, e.g. only phrase/`+`/quote syntax) — a best-effort preview,
/// never an error. The window is snapped outward to whitespace on each side
/// it doesn't already touch (so it never opens or closes mid-word), each such
/// trimmed side gets an "…", and every cut stays on a UTF-8 char boundary.
fn snippet_around(text: &str, query_text: &str) -> String {
    const WINDOW: usize = 160;
    if text.is_empty() {
        return String::new();
    }
    let lower_text = text.to_lowercase();
    let first_term = query_text
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| c == '+' || c == '"').to_lowercase())
        .find(|t| !t.is_empty());

    let pos = first_term.and_then(|t| lower_text.find(&t));
    let (start, end) = match pos {
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
        other => SearchError::Engine(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
