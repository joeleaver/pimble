//! The tree over node documents (docs/NODE_DOCUMENT_CONTRACT.md section 2):
//! a store's node documents, the root's id, and the operations the store
//! document used to offer. There is no tree document: a node's place is in its
//! own document (`parent_id`) and in its parent's (`children`), and
//! [`Tree::repair`] (decision 9 of docs/history/HARDENING_CONTRACT.md, over
//! documents) settles what concurrent edits leave inconsistent.
//!
//! Every operation edits the documents it touches, each in its own
//! transaction, and returns a [`TreeEdit`] naming them with the update each
//! transaction produced, so a caller can persist and broadcast per document.

use std::collections::HashMap;

use pimble_core::NodeId;

use crate::error::{CrdtError, Result};
use crate::node_doc::{NodeDoc, NodeUpdateEffect};
pub use crate::store_document::{NodeInfo, TreeIssue};

/// What a tree operation changed: per touched document, the v1 update its
/// transaction produced.
#[derive(Debug, Clone, Default)]
pub struct TreeEdit {
    pub touched: Vec<(NodeId, Vec<u8>)>,
}

impl TreeEdit {
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }

    pub fn node_ids(&self) -> Vec<NodeId> {
        self.touched.iter().map(|(id, _)| *id).collect()
    }
}

/// A store's node documents.
pub struct Tree {
    root: NodeId,
    docs: HashMap<NodeId, NodeDoc>,
}

fn not_built(what: &str) -> CrdtError {
    CrdtError::Yrs(format!("Tree::{what} is not built yet (docs/NODE_DOCUMENT_CONTRACT.md wave 1)"))
}

impl Tree {
    /// A new store: a root folder named `name`.
    pub fn new(root: NodeId, name: &str, now: &str) -> Result<Self> {
        let mut tree = Self { root, docs: HashMap::new() };
        let mut doc = NodeDoc::new();
        doc.init(pimble_core::node_types::FOLDER, name, None, now)?;
        tree.docs.insert(root, doc);
        Ok(tree)
    }

    /// The documents a store holds, as loaded from disk. Nothing is repaired
    /// here; the caller runs [`Tree::repair`] after loading.
    pub fn from_docs(root: NodeId, docs: HashMap<NodeId, NodeDoc>) -> Self {
        Self { root, docs }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    // ── Documents ────────────────────────────────────────────────────────

    pub fn doc(&self, id: NodeId) -> Option<&NodeDoc> {
        self.docs.get(&id)
    }

    pub fn doc_mut(&mut self, id: NodeId) -> Option<&mut NodeDoc> {
        self.docs.get_mut(&id)
    }

    /// Hold a document (a peer's, the importer's). Replaces one already held.
    pub fn insert_doc(&mut self, id: NodeId, doc: NodeDoc) {
        self.docs.insert(id, doc);
    }

    /// Stop holding a document (a purge). Not a delete: see [`Tree::remove_node`].
    pub fn take_doc(&mut self, id: NodeId) -> Option<NodeDoc> {
        self.docs.remove(&id)
    }

    /// Every held document's id, tombstoned ones included.
    pub fn ids(&self) -> Vec<NodeId> {
        self.docs.keys().copied().collect()
    }

    /// Merge a peer's update into `id`'s document, creating it when unknown.
    pub fn apply_update(&mut self, _id: NodeId, _update: &[u8]) -> Result<NodeUpdateEffect> {
        Err(not_built("apply_update"))
    }

    // ── Reading ──────────────────────────────────────────────────────────

    /// Held, initialised and not deleted.
    pub fn has_node(&self, _id: NodeId) -> bool {
        false
    }

    /// Every undeleted node.
    pub fn list_node_ids(&self) -> Vec<NodeId> {
        Vec::new()
    }

    pub fn get_node_info(&self, _id: NodeId) -> Result<NodeInfo> {
        Err(not_built("get_node_info"))
    }

    /// The undeleted, held children of `id` in stored order, each once
    /// (what repair would make the list; the stored list may not be there yet).
    pub fn get_children(&self, _id: NodeId) -> Result<Vec<NodeId>> {
        Err(not_built("get_children"))
    }

    /// `root` and everything under it, preorder, undeleted; a node counts as
    /// a child where its `parent_id` and the parent's list agree.
    pub fn subtree_ids(&self, _root: NodeId) -> Result<Vec<NodeId>> {
        Err(not_built("subtree_ids"))
    }

    // ── Editing ──────────────────────────────────────────────────────────

    /// Create `id` under `parent` (the root when `None`) at `position`
    /// (the end when `None`).
    pub fn add_node(
        &mut self,
        _id: NodeId,
        _parent: Option<NodeId>,
        _position: Option<usize>,
        _node_type: &str,
        _title: &str,
        _now: &str,
    ) -> Result<TreeEdit> {
        Err(not_built("add_node"))
    }

    /// [`Tree::add_node`] for a document that already has content (the importer).
    pub fn add_node_with_doc(
        &mut self,
        _id: NodeId,
        _doc: NodeDoc,
        _parent: Option<NodeId>,
        _position: Option<usize>,
        _node_type: &str,
        _title: &str,
        _now: &str,
    ) -> Result<TreeEdit> {
        Err(not_built("add_node_with_doc"))
    }

    /// Move `id` under `new_parent` at `position` (the end when `None`).
    /// The root cannot move; a move under a node's own descendant is refused.
    pub fn move_node(&mut self, _id: NodeId, _new_parent: NodeId, _position: Option<usize>, _now: &str) -> Result<TreeEdit> {
        Err(not_built("move_node"))
    }

    /// Delete `id` and its subtree: tombstones, and `id` leaves its parent's
    /// list. The root cannot be deleted.
    pub fn remove_node(&mut self, _id: NodeId, _now: &str) -> Result<TreeEdit> {
        Err(not_built("remove_node"))
    }

    /// Clear the tombstones of `id` and its subtree and put `id` back in its
    /// parent's list (the root when the parent is gone).
    pub fn undelete_node(&mut self, _id: NodeId, _now: &str) -> Result<TreeEdit> {
        Err(not_built("undelete_node"))
    }

    pub fn set_title(&mut self, _id: NodeId, _title: &str, _now: &str) -> Result<TreeEdit> {
        Err(not_built("set_title"))
    }

    pub fn set_tags(&mut self, _id: NodeId, _tags: &[String], _now: &str) -> Result<TreeEdit> {
        Err(not_built("set_tags"))
    }

    pub fn set_custom(&mut self, _id: NodeId, _key: &str, _value: &serde_json::Value, _now: &str) -> Result<TreeEdit> {
        Err(not_built("set_custom"))
    }

    pub fn remove_custom(&mut self, _id: NodeId, _key: &str, _now: &str) -> Result<TreeEdit> {
        Err(not_built("remove_custom"))
    }

    pub fn set_node_type(&mut self, _id: NodeId, _node_type: &str, _now: &str) -> Result<TreeEdit> {
        Err(not_built("set_node_type"))
    }

    pub fn touch_modified(&mut self, _id: NodeId, _now: &str) -> Result<TreeEdit> {
        Err(not_built("touch_modified"))
    }

    // ── Shape ────────────────────────────────────────────────────────────

    /// Exactly the conditions [`Tree::repair`] fixes; empty after a repair.
    pub fn validate_tree(&self) -> Vec<TreeIssue> {
        Vec::new()
    }

    /// Decision 9 over documents: effective parents, cycles broken at the
    /// smallest id, every list made to hold exactly the undeleted nodes whose
    /// effective parent is its owner (first occurrence kept, missing ones
    /// appended in id order). Deterministic in the held state. `None` when
    /// nothing needed fixing.
    pub fn repair(&mut self, _now: &str) -> Result<Option<TreeEdit>> {
        Err(not_built("repair"))
    }
}
