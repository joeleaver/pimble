//! The relationships between nodes: `parent`, `links` and `tags` edges
//! reconciled against what a node says, and the backlinks they give.

use std::collections::HashSet;

use pimble_core::NodeId;
use rhypedb_engine::object::{FieldMap, Value};

use super::errors::map_engine_err;
use super::SearchIndex;
use crate::error::{Result, SearchError};

impl SearchIndex {
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

    pub(super) fn reconcile_parent(&self, object_id: u64, parent: Option<NodeId>) -> Result<()> {
        let desired: HashSet<u64> = match parent {
            Some(p) => self.resolve_node_ids(std::slice::from_ref(&p))?.into_iter().collect(),
            None => HashSet::new(),
        };
        self.reconcile_edges(object_id, "parent", &desired)
    }

    pub(super) fn reconcile_links(&self, object_id: u64, links: &[NodeId]) -> Result<()> {
        let desired: HashSet<u64> = self.resolve_node_ids(links)?.into_iter().collect();
        self.reconcile_edges(object_id, "links", &desired)
    }

    pub(super) fn reconcile_tags(&self, object_id: u64, tags: &[String]) -> Result<()> {
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
    pub(super) fn reconcile_edges(&self, source_id: u64, field_name: &str, desired: &HashSet<u64>) -> Result<()> {
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
}
