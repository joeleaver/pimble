//! Semantic chunks: a node's chunk objects reconciled against its units, and
//! their embeddings queued for the process's one model.

use std::collections::{HashMap, HashSet};

use pimble_core::{IndexUnit, NodeId};
use rhypedb_engine::object::{FieldMap, Value};

use super::errors::map_engine_err;
use super::SearchIndex;
use crate::chunk::chunk_units;
use crate::error::Result;

impl SearchIndex {
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

    pub(super) fn reconcile_chunks(
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
}
