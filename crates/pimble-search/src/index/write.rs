//! Writing to the index: creating, updating, removing and clearing nodes.

use pimble_core::NodeId;
use rhypedb_engine::object::{FieldMap, Value};

use super::errors::map_engine_err;
use super::outline::Outline;
use super::{IndexNode, SearchIndex};
use crate::error::Result;

impl SearchIndex {
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
}
