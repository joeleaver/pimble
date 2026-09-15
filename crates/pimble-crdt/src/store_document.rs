//! CRDT-backed store document for tree structure and node metadata
//!
//! A single yrs document per store containing:
//! - Tree structure (parent-child relationships, children ordering)
//! - Node metadata (title, type, tags, custom fields, timestamps)
//! - Node existence (create/delete)

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use pimble_core::NodeId;
use yrs::types::{Event, PathSegment};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{
    Array, ArrayPrelim, ArrayRef, DeepObservable, Doc, Map, MapPrelim, MapRef, OffsetKind,
    Options, Out, ReadTxn, StateVector, Transact, TransactionMut, Update,
};

use crate::error::{CrdtError, Result};

/// Information about a node read from the store document
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub node_type: String,
    pub title: String,
    pub tags: Vec<String>,
    pub custom: HashMap<String, serde_json::Value>,
    pub created_at: String,
    pub modified_at: String,
}

/// Issues found during tree validation
#[derive(Debug, Clone)]
pub enum TreeIssue {
    /// Node references a parent that doesn't exist
    OrphanNode { node_id: NodeId, missing_parent: NodeId },
    /// A child ID in a parent's children list doesn't exist
    MissingChild { parent_id: NodeId, child_id: NodeId },
    /// A child appears in multiple parents' children lists
    DuplicateChild { child_id: NodeId, parents: Vec<NodeId> },
    /// Cycle detected in the tree
    Cycle { node_ids: Vec<NodeId> },
}

/// What [`StoreDocument::repair`] changed: the yrs update its transaction
/// produced (to broadcast and forward like any other store update) and the
/// node entries it touched (docs/HARDENING_CONTRACT.md decision 9).
#[derive(Debug, Clone)]
pub struct TreeRepair {
    pub update: Vec<u8>,
    pub touched: Vec<NodeId>,
}

/// A CRDT-backed store document managing tree structure and node metadata.
///
/// Schema (a yrs `Doc` built with `OffsetKind::Utf16`, matching `ContentDoc`):
/// ```text
/// root map "meta":  { name: String, root_node_id: String }
/// root map "nodes": { <node-id>: Map {
///     parent_id: String | absent,
///     node_type: String, title: String,
///     created_at: String (rfc3339), modified_at: String (rfc3339),
///     tags: Array<String>,
///     custom: Map<String, String (json)>,
///     children: Array<String (node-id)>
/// } }
/// ```
pub struct StoreDocument {
    doc: Doc,
    /// Root map "meta", resolved once at construction. `Doc::get_or_insert_map`
    /// opens its own write transaction, so it must never run while a caller holds
    /// one; caching the handles here makes every method single-transaction.
    meta: MapRef,
    /// Root map "nodes", resolved once at construction (see `meta`).
    nodes: MapRef,
}

impl StoreDocument {
    /// Create a new store document with a root folder node.
    pub fn new(name: &str, root_node_id: NodeId) -> Result<Self> {
        let this = Self::empty();

        let mut txn = this.doc.transact_mut();
        this.meta().insert(&mut txn, "name", name.to_string());
        this.meta().insert(&mut txn, "root_node_id", root_node_id.to_string());

        let now = chrono::Utc::now().to_rfc3339();
        let root_key = root_node_id.to_string();
        let node_map = this.nodes().insert(&mut txn, root_key, MapPrelim::default());
        // parent_id is absent for root
        node_map.insert(&mut txn, "node_type", "folder".to_string());
        node_map.insert(&mut txn, "title", name.to_string());
        node_map.insert(&mut txn, "created_at", now.clone());
        node_map.insert(&mut txn, "modified_at", now);
        node_map.insert(&mut txn, "tags", ArrayPrelim::default());
        node_map.insert(&mut txn, "custom", MapPrelim::default());
        node_map.insert(&mut txn, "children", ArrayPrelim::default());
        drop(txn);

        Ok(this)
    }

    /// Load a store document from bytes (a yrs v1 update: a full snapshot or any
    /// update). Empty bytes produce an empty document.
    pub fn load(bytes: &[u8]) -> Result<Self> {
        let this = Self::empty();
        if bytes.is_empty() {
            return Ok(this);
        }
        let update = Update::decode_v1(bytes).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        this.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|e| CrdtError::Yrs(e.to_string()))?;
        Ok(this)
    }

    /// An empty yrs `Doc`, UTF-16 offsets, no root content populated yet.
    fn empty() -> Self {
        let options = Options {
            offset_kind: OffsetKind::Utf16,
            ..Default::default()
        };
        let doc = Doc::with_options(options);
        let meta = doc.get_or_insert_map("meta");
        let nodes = doc.get_or_insert_map("nodes");
        Self { doc, meta, nodes }
    }

    fn meta(&self) -> MapRef {
        self.meta.clone()
    }

    fn nodes(&self) -> MapRef {
        self.nodes.clone()
    }

    /// Full snapshot: the v1 update encoding of the whole document from an empty state
    /// vector. Round-trips through [`StoreDocument::load`].
    pub fn save(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    /// This document's state vector, v1-encoded.
    pub fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    /// Everything this document has that a peer at `state_vector` (v1-encoded) lacks.
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>> {
        let sv =
            StateVector::decode_v1(state_vector).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        Ok(self.doc.transact().encode_diff_v1(&sv))
    }

    /// Merge a peer's v1 update — a broadcast delta, a reconciliation diff, or a whole
    /// snapshot — into this document. Returns the ids of every node entry the update
    /// touched (created, deleted, or modified — metadata, tags, custom fields, tree
    /// position, or children order), so a caller can feed them to a search index
    /// without having to diff the tree itself (docs/SYNC_CONTRACT.md decision 9).
    ///
    /// Implemented with a scoped `observe_deep` on the `nodes` map: since paths from
    /// `observe_deep` are relative to the observed type, an event whose path is empty
    /// fired directly on `nodes` (a node entry created or removed — that node's own
    /// map key is read from the event's own key changes) and an event with a non-empty
    /// path fired on something nested inside one node's own map (its first segment is
    /// that node's id, e.g. a title/tag/custom edit or a children-array change from a
    /// move). The subscription is dropped again before returning, so it only observes
    /// this one update.
    pub fn apply_update(&mut self, update: &[u8]) -> Result<Vec<NodeId>> {
        let update = Update::decode_v1(update).map_err(|e| CrdtError::Yrs(e.to_string()))?;

        let touched: Arc<Mutex<HashSet<NodeId>>> = Arc::new(Mutex::new(HashSet::new()));
        let touched_in_closure = Arc::clone(&touched);
        let subscription = self.nodes().observe_deep(move |txn, events| {
            for event in events.iter() {
                let path = event.path();
                match path.front() {
                    Some(PathSegment::Key(key)) => {
                        if let Ok(node_id) = NodeId::parse(key) {
                            touched_in_closure.lock().unwrap().insert(node_id);
                        }
                    }
                    Some(PathSegment::Index(_)) | None => {
                        // An empty path means the event fired on the observed
                        // `nodes` map itself: a whole node entry was added or
                        // removed. Its own key changes are the touched ids.
                        if let Event::Map(map_event) = event {
                            for key in map_event.keys(txn).keys() {
                                if let Ok(node_id) = NodeId::parse(key) {
                                    touched_in_closure.lock().unwrap().insert(node_id);
                                }
                            }
                        }
                    }
                }
            }
        });

        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|e| CrdtError::Yrs(e.to_string()))?;

        drop(subscription);
        let touched_ids: Vec<NodeId> = touched.lock().unwrap().iter().copied().collect();
        Ok(touched_ids)
    }

    /// Get the root node ID.
    pub fn root_node_id(&self) -> Result<NodeId> {
        let txn = self.doc.transact();
        let s = Self::string_value(self.meta().get(&txn, "root_node_id"))
            .ok_or_else(|| CrdtError::KeyNotFound("root_node_id".into()))?;
        NodeId::parse(&s).map_err(|e| CrdtError::Serialization(e.to_string()))
    }

    /// Get the MapRef for a specific node entry.
    fn node_map<T: ReadTxn>(&self, txn: &T, node_id: NodeId) -> Result<MapRef> {
        match self.nodes().get(txn, &node_id.to_string()) {
            Some(Out::YMap(m)) => Ok(m),
            _ => Err(CrdtError::KeyNotFound(format!("node {}", node_id))),
        }
    }

    /// Get the children ArrayRef for a node.
    fn children_array<T: ReadTxn>(&self, txn: &T, node_id: NodeId) -> Result<ArrayRef> {
        let node_map = self.node_map(txn, node_id)?;
        match node_map.get(txn, "children") {
            Some(Out::YArray(a)) => Ok(a),
            _ => Err(CrdtError::KeyNotFound("children".into())),
        }
    }

    /// Add a new node to the store document.
    pub fn add_node(
        &mut self,
        id: NodeId,
        parent_id: Option<NodeId>,
        node_type: &str,
        title: &str,
    ) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let key = id.to_string();

        if self.nodes().get(&txn, &key).is_some() {
            return Err(CrdtError::Serialization(format!("Node {} already exists", id)));
        }

        // Resolve the parent's children array before creating anything, so a missing
        // parent fails loud without leaving a half-created node behind.
        let parent_children = match parent_id {
            Some(pid) => Some(self.children_array(&txn, pid)?),
            None => None,
        };

        let now = chrono::Utc::now().to_rfc3339();
        let node_map = self.nodes().insert(&mut txn, key, MapPrelim::default());
        if let Some(pid) = parent_id {
            node_map.insert(&mut txn, "parent_id", pid.to_string());
        }
        node_map.insert(&mut txn, "node_type", node_type.to_string());
        node_map.insert(&mut txn, "title", title.to_string());
        node_map.insert(&mut txn, "created_at", now.clone());
        node_map.insert(&mut txn, "modified_at", now);
        node_map.insert(&mut txn, "tags", ArrayPrelim::default());
        node_map.insert(&mut txn, "custom", MapPrelim::default());
        node_map.insert(&mut txn, "children", ArrayPrelim::default());

        if let Some(children) = parent_children {
            children.push_back(&mut txn, id.to_string());
        }

        Ok(())
    }

    /// Remove a node from the store document.
    pub fn remove_node(&mut self, id: NodeId) -> Result<()> {
        let mut txn = self.doc.transact_mut();

        let parent_id = self.parent_id_of(&txn, id)?;
        if let Some(pid) = parent_id {
            Self::remove_from_children_list(&self.children_array(&txn, pid)?, &mut txn, id);
        }

        self.nodes().remove(&mut txn, &id.to_string());

        Ok(())
    }

    /// Move a node to a new parent at a specific position.
    pub fn move_node(
        &mut self,
        id: NodeId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> Result<()> {
        let mut txn = self.doc.transact_mut();

        let old_parent_id = self
            .parent_id_of(&txn, id)?
            .ok_or_else(|| CrdtError::Serialization("Cannot move root node".into()))?;

        Self::remove_from_children_list(&self.children_array(&txn, old_parent_id)?, &mut txn, id);

        let children = self.children_array(&txn, new_parent_id)?;
        let len = children.len(&txn);
        let pos = position.map(|p| (p as u32).min(len)).unwrap_or(len);
        children.insert(&mut txn, pos, id.to_string());

        let node_map = self.node_map(&txn, id)?;
        node_map.insert(&mut txn, "parent_id", new_parent_id.to_string());
        let now = chrono::Utc::now().to_rfc3339();
        node_map.insert(&mut txn, "modified_at", now);

        Ok(())
    }

    /// Set the title of a node.
    pub fn set_title(&mut self, id: NodeId, title: &str) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let node_map = self.node_map(&txn, id)?;
        node_map.insert(&mut txn, "title", title.to_string());
        let now = chrono::Utc::now().to_rfc3339();
        node_map.insert(&mut txn, "modified_at", now);
        Ok(())
    }

    /// Set the tags of a node.
    pub fn set_tags(&mut self, id: NodeId, tags: &[String]) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let node_map = self.node_map(&txn, id)?;
        // Replace the tags array wholesale, same as before.
        let tags_array = node_map.insert(&mut txn, "tags", ArrayPrelim::default());
        for tag in tags {
            tags_array.push_back(&mut txn, tag.clone());
        }
        let now = chrono::Utc::now().to_rfc3339();
        node_map.insert(&mut txn, "modified_at", now);
        Ok(())
    }

    /// Set a custom metadata field on a node.
    pub fn set_custom(&mut self, id: NodeId, key: &str, value: &serde_json::Value) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let node_map = self.node_map(&txn, id)?;
        let custom = match node_map.get(&txn, "custom") {
            Some(Out::YMap(m)) => m,
            _ => return Err(CrdtError::KeyNotFound("custom".into())),
        };
        // Store as JSON string for simplicity with complex values
        let json_str = serde_json::to_string(value).map_err(|e| CrdtError::Serialization(e.to_string()))?;
        custom.insert(&mut txn, key.to_string(), json_str);
        let now = chrono::Utc::now().to_rfc3339();
        node_map.insert(&mut txn, "modified_at", now);
        Ok(())
    }

    /// Get node info from the store document.
    pub fn get_node_info(&self, id: NodeId) -> Result<NodeInfo> {
        let txn = self.doc.transact();
        let node_map = self.node_map(&txn, id)?;

        let parent_id = self.parent_id_of(&txn, id)?;
        let node_type = Self::string_value(node_map.get(&txn, "node_type")).unwrap_or_default();
        let title = Self::string_value(node_map.get(&txn, "title")).unwrap_or_default();
        let created_at = Self::string_value(node_map.get(&txn, "created_at")).unwrap_or_default();
        let modified_at = Self::string_value(node_map.get(&txn, "modified_at")).unwrap_or_default();

        let tags = match node_map.get(&txn, "tags") {
            Some(Out::YArray(a)) => a
                .iter(&txn)
                .filter_map(|out| Self::string_value(Some(out)))
                .collect(),
            _ => Vec::new(),
        };

        let custom = match node_map.get(&txn, "custom") {
            Some(Out::YMap(m)) => {
                let mut custom = HashMap::new();
                for (key, value) in m.iter(&txn) {
                    if let Out::Any(any) = value {
                        if let Ok(s) = String::try_from(any) {
                            if let Ok(parsed) = serde_json::from_str(&s) {
                                custom.insert(key.to_string(), parsed);
                            }
                        }
                    }
                }
                custom
            }
            _ => HashMap::new(),
        };

        Ok(NodeInfo {
            id,
            parent_id,
            node_type,
            title,
            tags,
            custom,
            created_at,
            modified_at,
        })
    }

    /// Get the ordered children of a node.
    pub fn get_children(&self, id: NodeId) -> Result<Vec<NodeId>> {
        let txn = self.doc.transact();
        let children = self.children_array(&txn, id)?;
        Ok(children
            .iter(&txn)
            .filter_map(|out| Self::string_value(Some(out)))
            .filter_map(|s| NodeId::parse(&s).ok())
            .collect())
    }

    /// List all node IDs in the store document.
    pub fn list_node_ids(&self) -> Result<Vec<NodeId>> {
        let txn = self.doc.transact();
        Ok(self
            .nodes()
            .keys(&txn)
            .filter_map(|k| NodeId::parse(k).ok())
            .collect())
    }

    /// Check if a node exists in the store document.
    pub fn has_node(&self, id: NodeId) -> bool {
        let txn = self.doc.transact();
        matches!(self.nodes().get(&txn, &id.to_string()), Some(Out::YMap(_)))
    }

    /// Validate the tree structure and return any issues found.
    pub fn validate_tree(&self) -> Result<Vec<TreeIssue>> {
        let mut issues = Vec::new();
        let node_ids = self.list_node_ids()?;
        let node_set: std::collections::HashSet<NodeId> = node_ids.iter().copied().collect();

        // Track which parent each child belongs to
        let mut child_to_parents: HashMap<NodeId, Vec<NodeId>> = HashMap::new();

        for &node_id in &node_ids {
            // Check parent exists
            if let Ok(Some(parent_id)) = self.parent_id_of(&self.doc.transact(), node_id) {
                if !node_set.contains(&parent_id) {
                    issues.push(TreeIssue::OrphanNode {
                        node_id,
                        missing_parent: parent_id,
                    });
                }
            }

            // Check children exist
            if let Ok(children) = self.get_children(node_id) {
                for child_id in children {
                    if !node_set.contains(&child_id) {
                        issues.push(TreeIssue::MissingChild {
                            parent_id: node_id,
                            child_id,
                        });
                    }
                    child_to_parents.entry(child_id).or_default().push(node_id);
                }
            }
        }

        // Check for children in multiple parents
        for (child_id, parents) in &child_to_parents {
            if parents.len() > 1 {
                issues.push(TreeIssue::DuplicateChild {
                    child_id: *child_id,
                    parents: parents.clone(),
                });
            }
        }

        Ok(issues)
    }

    /// Make the tree well formed again after concurrent edits merged into a
    /// shape no single replica produced (a child in two parents' lists, a
    /// cycle, an orphan). Deterministic in the merged state, so replicas
    /// that repair the same state make the same changes. `None` when the
    /// tree is already well formed (docs/HARDENING_CONTRACT.md decision 9).
    ///
    /// Stub landed with the interface; agent C implements it.
    pub fn repair(&mut self) -> Result<Option<TreeRepair>> {
        Ok(None)
    }

    // ── Private helpers ─────────────────────────────────────────────

    fn parent_id_of<T: ReadTxn>(&self, txn: &T, id: NodeId) -> Result<Option<NodeId>> {
        let node_map = self.node_map(txn, id)?;
        match Self::string_value(node_map.get(txn, "parent_id")) {
            Some(s) => NodeId::parse(&s)
                .map(Some)
                .map_err(|e| CrdtError::Serialization(e.to_string())),
            None => Ok(None),
        }
    }

    fn string_value(out: Option<Out>) -> Option<String> {
        match out {
            Some(Out::Any(any)) => String::try_from(any).ok(),
            _ => None,
        }
    }

    fn remove_from_children_list(children: &ArrayRef, txn: &mut TransactionMut, child_id: NodeId) {
        let child_str = child_id.to_string();
        let len = children.len(txn);
        for i in (0..len).rev() {
            if let Some(Out::Any(any)) = children.get(txn, i) {
                if let Ok(s) = String::try_from(any) {
                    if s == child_str {
                        children.remove(txn, i);
                        break;
                    }
                }
            }
        }
    }

    /// Update the modified_at timestamp of a node to now.
    pub fn touch_modified(&mut self, id: NodeId) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let node_map = self.node_map(&txn, id)?;
        let now = chrono::Utc::now().to_rfc3339();
        node_map.insert(&mut txn, "modified_at", now);
        Ok(())
    }

    /// Set the node type of a node.
    pub fn set_node_type(&mut self, id: NodeId, node_type: &str) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let node_map = self.node_map(&txn, id)?;
        node_map.insert(&mut txn, "node_type", node_type.to_string());
        Ok(())
    }

    /// Set the parent_id of a node (used during migration).
    pub fn set_parent_id(&mut self, id: NodeId, parent_id: Option<NodeId>) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let node_map = self.node_map(&txn, id)?;
        if let Some(pid) = parent_id {
            node_map.insert(&mut txn, "parent_id", pid.to_string());
        }
        Ok(())
    }

    /// Append a child to a node's children list (used during migration).
    pub fn append_child(&mut self, parent_id: NodeId, child_id: NodeId) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let children = self.children_array(&txn, parent_id)?;
        children.push_back(&mut txn, child_id.to_string());
        Ok(())
    }

    /// Set timestamps on a node (used during migration).
    pub fn set_timestamps(&mut self, id: NodeId, created_at: &str, modified_at: &str) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let node_map = self.node_map(&txn, id)?;
        node_map.insert(&mut txn, "created_at", created_at.to_string());
        node_map.insert(&mut txn, "modified_at", modified_at.to_string());
        Ok(())
    }

    /// Add a bare node entry (no parent link, no children append) for migration.
    pub fn add_node_bare(
        &mut self,
        id: NodeId,
        node_type: &str,
        title: &str,
        created_at: &str,
        modified_at: &str,
    ) -> Result<()> {
        let mut txn = self.doc.transact_mut();
        let key = id.to_string();
        let node_map = self.nodes().insert(&mut txn, key, MapPrelim::default());
        node_map.insert(&mut txn, "node_type", node_type.to_string());
        node_map.insert(&mut txn, "title", title.to_string());
        node_map.insert(&mut txn, "created_at", created_at.to_string());
        node_map.insert(&mut txn, "modified_at", modified_at.to_string());
        node_map.insert(&mut txn, "tags", ArrayPrelim::default());
        node_map.insert(&mut txn, "custom", MapPrelim::default());
        node_map.insert(&mut txn, "children", ArrayPrelim::default());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_store_document() {
        let root_id = NodeId::new();
        let doc = StoreDocument::new("Test Store", root_id).unwrap();
        assert_eq!(doc.root_node_id().unwrap(), root_id);
        assert!(doc.has_node(root_id));

        let info = doc.get_node_info(root_id).unwrap();
        assert_eq!(info.title, "Test Store");
        assert_eq!(info.node_type, "folder");
        assert!(info.parent_id.is_none());
    }

    #[test]
    fn test_add_and_get_node() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child_id = NodeId::new();
        doc.add_node(child_id, Some(root_id), "document", "My Doc").unwrap();

        assert!(doc.has_node(child_id));
        let info = doc.get_node_info(child_id).unwrap();
        assert_eq!(info.title, "My Doc");
        assert_eq!(info.node_type, "document");
        assert_eq!(info.parent_id, Some(root_id));

        let children = doc.get_children(root_id).unwrap();
        assert_eq!(children, vec![child_id]);
    }

    #[test]
    fn test_remove_node() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child_id = NodeId::new();
        doc.add_node(child_id, Some(root_id), "document", "Doc").unwrap();
        doc.remove_node(child_id).unwrap();

        assert!(!doc.has_node(child_id));
        let children = doc.get_children(root_id).unwrap();
        assert!(children.is_empty());
    }

    #[test]
    fn test_move_node() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let folder_a = NodeId::new();
        let folder_b = NodeId::new();
        let child = NodeId::new();

        doc.add_node(folder_a, Some(root_id), "folder", "A").unwrap();
        doc.add_node(folder_b, Some(root_id), "folder", "B").unwrap();
        doc.add_node(child, Some(folder_a), "document", "Doc").unwrap();

        assert_eq!(doc.get_children(folder_a).unwrap(), vec![child]);
        assert!(doc.get_children(folder_b).unwrap().is_empty());

        doc.move_node(child, folder_b, None).unwrap();

        assert!(doc.get_children(folder_a).unwrap().is_empty());
        assert_eq!(doc.get_children(folder_b).unwrap(), vec![child]);

        let info = doc.get_node_info(child).unwrap();
        assert_eq!(info.parent_id, Some(folder_b));
    }

    #[test]
    fn test_move_node_at_position() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child_a = NodeId::new();
        let child_b = NodeId::new();
        let child_c = NodeId::new();
        let folder = NodeId::new();

        doc.add_node(folder, Some(root_id), "folder", "Folder").unwrap();
        doc.add_node(child_a, Some(folder), "document", "A").unwrap();
        doc.add_node(child_b, Some(folder), "document", "B").unwrap();
        doc.add_node(child_c, Some(folder), "document", "C").unwrap();

        // Move C to position 0
        doc.move_node(child_c, folder, Some(0)).unwrap();
        let children = doc.get_children(folder).unwrap();
        assert_eq!(children, vec![child_c, child_a, child_b]);
    }

    #[test]
    fn test_set_title() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        doc.set_title(root_id, "Renamed Store").unwrap();
        let info = doc.get_node_info(root_id).unwrap();
        assert_eq!(info.title, "Renamed Store");
    }

    #[test]
    fn test_set_tags() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();

        doc.set_tags(child, &["tag1".into(), "tag2".into()]).unwrap();
        let info = doc.get_node_info(child).unwrap();
        assert_eq!(info.tags, vec!["tag1", "tag2"]);
    }

    #[test]
    fn test_set_custom() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();

        let val = serde_json::json!(true);
        doc.set_custom(child, "explicit_title", &val).unwrap();
        let info = doc.get_node_info(child).unwrap();
        assert_eq!(info.custom.get("explicit_title"), Some(&val));
    }

    #[test]
    fn test_save_load() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();

        let bytes = doc.save();
        let loaded = StoreDocument::load(&bytes).unwrap();

        assert_eq!(loaded.root_node_id().unwrap(), root_id);
        assert!(loaded.has_node(child));
        let info = loaded.get_node_info(child).unwrap();
        assert_eq!(info.title, "Doc");
    }

    #[test]
    fn test_list_node_ids() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child1 = NodeId::new();
        let child2 = NodeId::new();
        doc.add_node(child1, Some(root_id), "document", "A").unwrap();
        doc.add_node(child2, Some(root_id), "document", "B").unwrap();

        let ids = doc.list_node_ids().unwrap();
        assert_eq!(ids.len(), 3); // root + 2 children
        assert!(ids.contains(&root_id));
        assert!(ids.contains(&child1));
        assert!(ids.contains(&child2));
    }

    #[test]
    fn test_validate_tree_clean() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();

        let issues = doc.validate_tree().unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn round_trip_save_load() {
        let root_id = NodeId::new();
        let mut doc = StoreDocument::new("Store", root_id).unwrap();

        let child = NodeId::new();
        doc.add_node(child, Some(root_id), "document", "Doc").unwrap();
        doc.set_tags(child, &["a".to_string(), "b".to_string()]).unwrap();

        let bytes = doc.save();
        let loaded = StoreDocument::load(&bytes).unwrap();

        assert_eq!(loaded.root_node_id().unwrap(), root_id);
        assert_eq!(loaded.get_children(root_id).unwrap(), vec![child]);
        let info = loaded.get_node_info(child).unwrap();
        assert_eq!(info.title, "Doc");
        assert_eq!(info.tags, vec!["a", "b"]);
        assert_eq!(loaded.state_vector(), doc.state_vector());
    }

    #[test]
    fn two_replicas_add_children_converge() {
        // `b` starts as an exact copy of `a` (same underlying object identities), then
        // each replica independently adds a *different* child under the shared root
        // before exchanging state-vector diffs — the scenario `diff_since`/
        // `apply_update` exist for.
        let root_id = NodeId::new();
        let a = StoreDocument::new("Store", root_id).unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();
        let mut a = a;

        let child_a = NodeId::new();
        let child_b = NodeId::new();
        a.add_node(child_a, Some(root_id), "document", "A").unwrap();
        b.add_node(child_b, Some(root_id), "document", "B").unwrap();

        let a_sv = a.state_vector();
        let b_sv = b.state_vector();

        let diff_for_b = a.diff_since(&b_sv).unwrap();
        let diff_for_a = b.diff_since(&a_sv).unwrap();

        b.apply_update(&diff_for_b).unwrap();
        a.apply_update(&diff_for_a).unwrap();

        let a_children: std::collections::HashSet<_> = a.get_children(root_id).unwrap().into_iter().collect();
        let b_children: std::collections::HashSet<_> = b.get_children(root_id).unwrap().into_iter().collect();

        assert_eq!(a_children.len(), 2);
        assert!(a_children.contains(&child_a));
        assert!(a_children.contains(&child_b));
        assert_eq!(a_children, b_children);

        assert!(a.validate_tree().unwrap().is_empty());
        assert!(b.validate_tree().unwrap().is_empty());
    }

    #[test]
    fn apply_update_returns_the_id_of_a_created_node() {
        let root_id = NodeId::new();
        let a = StoreDocument::new("Store", root_id).unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();
        let mut a = a;

        let b_sv = b.state_vector();
        let new_id = NodeId::new();
        a.add_node(new_id, Some(root_id), "document", "New").unwrap();
        let diff = a.diff_since(&b_sv).unwrap();

        // The new node itself, plus the root whose children array gained an entry.
        let touched: HashSet<_> = b.apply_update(&diff).unwrap().into_iter().collect();
        assert_eq!(touched, HashSet::from([new_id, root_id]));
    }

    #[test]
    fn apply_update_returns_the_id_of_a_node_whose_title_changed() {
        let root_id = NodeId::new();
        let a = StoreDocument::new("Store", root_id).unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();
        let mut a = a;

        let child = NodeId::new();
        a.add_node(child, Some(root_id), "document", "Doc").unwrap();
        b.apply_update(&a.diff_since(&b.state_vector()).unwrap()).unwrap();

        let b_sv = b.state_vector();
        a.set_title(child, "Renamed").unwrap();
        let diff = a.diff_since(&b_sv).unwrap();

        let touched = b.apply_update(&diff).unwrap();
        assert_eq!(touched, vec![child]);
    }

    #[test]
    fn apply_update_returns_ids_of_a_moved_node_and_both_parents() {
        let root_id = NodeId::new();
        let a = StoreDocument::new("Store", root_id).unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();
        let mut a = a;

        let folder_a = NodeId::new();
        let folder_b = NodeId::new();
        let child = NodeId::new();
        a.add_node(folder_a, Some(root_id), "folder", "A").unwrap();
        a.add_node(folder_b, Some(root_id), "folder", "B").unwrap();
        a.add_node(child, Some(folder_a), "document", "Doc").unwrap();
        b.apply_update(&a.diff_since(&b.state_vector()).unwrap()).unwrap();

        let b_sv = b.state_vector();
        a.move_node(child, folder_b, None).unwrap();
        let diff = a.diff_since(&b_sv).unwrap();

        let touched: HashSet<_> = b.apply_update(&diff).unwrap().into_iter().collect();
        // The moved node itself (new parent_id) plus both parents' children arrays.
        assert_eq!(touched, HashSet::from([child, folder_a, folder_b]));
    }

    #[test]
    fn apply_update_returns_the_id_of_a_removed_node() {
        let root_id = NodeId::new();
        let a = StoreDocument::new("Store", root_id).unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();
        let mut a = a;

        let child = NodeId::new();
        a.add_node(child, Some(root_id), "document", "Doc").unwrap();
        b.apply_update(&a.diff_since(&b.state_vector()).unwrap()).unwrap();

        let b_sv = b.state_vector();
        a.remove_node(child).unwrap();
        let diff = a.diff_since(&b_sv).unwrap();

        let touched: HashSet<_> = b.apply_update(&diff).unwrap().into_iter().collect();
        assert!(touched.contains(&child), "expected the removed node's id in {:?}", touched);
        assert!(!b.has_node(child));
    }
}
