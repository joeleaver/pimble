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

/// Issues found during tree validation. Every variant here is exactly one of the
/// conditions [`StoreDocument::repair`] fixes (docs/history/HARDENING_CONTRACT.md decision 9):
/// after a successful `repair`, `validate_tree` on the same document is always empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeIssue {
    /// A node's `parent_id` names a node that doesn't exist in the document (including
    /// naming itself): its effective parent defaults to the root.
    OrphanNode { node_id: NodeId, missing_parent: NodeId },
    /// A node (not the root) has no `parent_id` at all.
    DetachedNode { node_id: NodeId },
    /// A children list names a node id that doesn't exist in the document.
    MissingChild { parent_id: NodeId, child_id: NodeId },
    /// The same child id appears more than once within one parent's own children list.
    DuplicateInList { parent_id: NodeId, child_id: NodeId },
    /// A children list names a child whose effective parent (by `parent_id`, after cycle
    /// resolution) is a different node — this entry is in the wrong list.
    WrongList { parent_id: NodeId, child_id: NodeId, effective_parent: NodeId },
    /// A node has an effective parent, but that parent's children list is missing it.
    MissingFromList { parent_id: NodeId, child_id: NodeId },
    /// The root node id appears in some parent's children list.
    RootInList { parent_id: NodeId },
    /// A cycle in the effective-parent chain (none of these nodes' chains reach the root).
    Cycle { node_ids: Vec<NodeId> },
    /// Node documents only (`Tree`, docs/MOVE_CONTRACT.md "Repair"): `parent_id`'s list
    /// names `child_id`, whose stored `parent_id` (`stored_parent`; `None` when it has
    /// none) would take it out of a share that list is in. The list wins: the node's
    /// effective parent is `parent_id` and its `parent_id` is rewritten to it. Reported
    /// instead of the `OrphanNode` or `DetachedNode` the stored value would otherwise be.
    LeavesShare { parent_id: NodeId, child_id: NodeId, stored_parent: Option<NodeId> },
}

/// What [`StoreDocument::repair`] changed: the yrs update its transaction
/// produced (to broadcast and forward like any other store update) and the
/// node entries it touched (docs/history/HARDENING_CONTRACT.md decision 9).
#[derive(Debug, Clone)]
pub struct TreeRepair {
    pub update: Vec<u8>,
    pub touched: Vec<NodeId>,
}

/// What merging a peer's update into the store document changed
/// (docs/history/HARDENING_CONTRACT.md decision 8): `changed` is `false` exactly when every part
/// of the update was already reflected in this document — a yrs v1 update is never
/// actually empty (see [`crate::sync_util`]), so this is computed from the merging
/// transaction's own before/after state and delete set, never from the update's byte
/// length. `touched` is the node ids the update created, deleted, or modified; it is
/// non-empty only when `changed` is `true`.
#[derive(Debug, Clone)]
pub struct StoreUpdateEffect {
    pub changed: bool,
    pub touched: Vec<NodeId>,
}

/// Read-only analysis of the tree's current shape, shared by
/// [`StoreDocument::validate_tree`] and [`StoreDocument::repair`] so the two can never
/// drift apart: `validate_tree` reports exactly the issues, `repair` applies exactly the
/// fixes computed alongside them.
struct TreeAnalysis {
    issues: Vec<TreeIssue>,
    /// `(node, new_parent)` pairs whose stored `parent_id` needs to change (decision 9
    /// step 3). Empty when nothing needs a parent rewrite.
    parent_rewrites: Vec<(NodeId, NodeId)>,
    /// Per-owner children-list surgery (decision 9 steps 4-5): `remove_indices` are raw
    /// indices into the *current* stored array, in descending order (so removing one
    /// doesn't shift the position of another still to be removed); `appends` are
    /// children to add at the end. Entries not named here are left completely alone —
    /// deliberately a targeted edit rather than a wholesale clear-and-rebuild, so that
    /// two replicas repairing the same already-correct entries never manufacture a fresh
    /// (and then duplicate, once exchanged) copy of them.
    list_rewrites: Vec<(NodeId, Vec<usize>, Vec<NodeId>)>,
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
    /// snapshot — into this document. Returns whether it changed anything and the ids of
    /// every node entry it touched (created, deleted, or modified — metadata, tags,
    /// custom fields, tree position, or children order), so a caller can feed them to a
    /// search index without having to diff the tree itself
    /// (docs/SYNC_CONTRACT.md decision 9; the `changed` flag is decision 8 of
    /// docs/history/HARDENING_CONTRACT.md).
    ///
    /// Implemented with a scoped `observe_deep` on the `nodes` map: since paths from
    /// `observe_deep` are relative to the observed type, an event whose path is empty
    /// fired directly on `nodes` (a node entry created or removed — that node's own
    /// map key is read from the event's own key changes) and an event with a non-empty
    /// path fired on something nested inside one node's own map (its first segment is
    /// that node's id, e.g. a title/tag/custom edit or a children-array change from a
    /// move). The subscription is dropped again before returning, so it only observes
    /// this one update. `changed` comes from the same transaction's before/after state
    /// and delete set, not from `touched`: a no-op update by construction touches
    /// nothing, but relying on that alone would wrongly call a real edit confined to
    /// `meta` (outside `nodes`) a no-op too.
    pub fn apply_update(&mut self, update: &[u8]) -> Result<StoreUpdateEffect> {
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

        let changed = {
            let mut txn = self.doc.transact_mut();
            txn.apply_update(update).map_err(|e| CrdtError::Yrs(e.to_string()))?;
            *txn.before_state() != *txn.after_state() || !txn.delete_set().is_empty()
        };

        drop(subscription);
        let touched_ids: Vec<NodeId> = touched.lock().unwrap().iter().copied().collect();
        Ok(StoreUpdateEffect { changed, touched: touched_ids })
    }

    /// Computes [`StoreDocument::diff_since`] against `remote_sv` and returns it only if
    /// it tells the peer something it doesn't already know
    /// (docs/history/HARDENING_CONTRACT.md decision 7): a struct beyond `remote_sv`, or one of
    /// our deletions missing from `remote_diff`'s delete set. `remote_diff` is the diff
    /// the peer just sent us (from its own `diff_since` against our last-known state) —
    /// decoded here only for its delete set, never applied. `None` when the peer already
    /// has everything, which a bare `diff_since(remote_sv).is_empty()` check can never
    /// detect (see [`crate::sync_util`]).
    pub fn diff_if_peer_lacks_it(&self, remote_sv: &[u8], remote_diff: &[u8]) -> Result<Option<Vec<u8>>> {
        let local_diff = self.diff_since(remote_sv)?;
        if crate::sync_util::peer_lacks_something(&local_diff, remote_diff)? {
            Ok(Some(local_diff))
        } else {
            Ok(None)
        }
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
            // The parent may already be gone: a batch subtree delete removes several
            // nodes in one pass, and on a merge that hasn't been repaired yet
            // (docs/history/HARDENING_CONTRACT.md decision 9) a node's stored `parent_id` can
            // point somewhere its children-list membership doesn't match, so no
            // ordering of the batch is guaranteed safe otherwise. If the parent's
            // entry is gone there is nothing to clean up — not an error.
            if let Ok(children) = self.children_array(&txn, pid) {
                Self::remove_from_children_list(&children, &mut txn, id);
            }
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

    /// Validate the tree structure and return any issues found. Empty exactly when
    /// `StoreDocument::repair` would return `None` (docs/history/HARDENING_CONTRACT.md
    /// decision 9): both are computed from the same `analyze` pass.
    pub fn validate_tree(&self) -> Result<Vec<TreeIssue>> {
        Ok(self.analyze()?.issues)
    }

    /// Make the tree well formed again after concurrent edits merged into a
    /// shape no single replica produced (a child in two parents' lists, a
    /// cycle, an orphan). Deterministic in the merged state, so replicas
    /// that repair the state make the same changes. `None` when the
    /// tree is already well formed (docs/history/HARDENING_CONTRACT.md decision 9).
    ///
    /// Never touches `created_at`/`modified_at` on any node -- only `parent_id` and
    /// children arrays, exactly the fields `analyze` found wrong.
    pub fn repair(&mut self) -> Result<Option<TreeRepair>> {
        let analysis = self.analyze()?;
        if analysis.parent_rewrites.is_empty() && analysis.list_rewrites.is_empty() {
            return Ok(None);
        }

        let before_sv = self.doc.transact().state_vector();

        // Same touched-node tracking as `apply_update`, so a repair's `TreeRepair` is
        // consumed by callers (broadcast, re-index) exactly like a merged update's.
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

        {
            let mut txn = self.doc.transact_mut();
            for (id, new_parent) in &analysis.parent_rewrites {
                let node_map = self.node_map(&txn, *id)?;
                node_map.insert(&mut txn, "parent_id", new_parent.to_string());
            }
            for (owner, remove_indices, appends) in &analysis.list_rewrites {
                let children = self.children_array(&txn, *owner)?;
                // Descending order (guaranteed by `analyze`): removing the highest
                // index first never shifts the position of one still to be removed.
                for &idx in remove_indices {
                    children.remove(&mut txn, idx as u32);
                }
                for child_id in appends {
                    children.push_back(&mut txn, child_id.to_string());
                }
            }
        }

        drop(subscription);
        let touched_ids: Vec<NodeId> = touched.lock().unwrap().iter().copied().collect();
        if touched_ids.is_empty() {
            // Every rewrite matched what was already there byte-for-byte (shouldn't
            // happen given the emptiness check above, but no observed change means
            // nothing to report or broadcast).
            return Ok(None);
        }

        let update = self.diff_since(&before_sv.encode_v1())?;
        Ok(Some(TreeRepair { update, touched: touched_ids }))
    }

    /// Read-only pass computing effective parents (after cycle resolution), every
    /// `TreeIssue` they and the children lists reveal, and exactly the writes
    /// `repair` would make to fix them (docs/history/HARDENING_CONTRACT.md decision 9). An
    /// uninitialized document -- a freshly created replica before its first reconcile
    /// has no root yet -- analyzes as empty rather than erroring.
    fn analyze(&self) -> Result<TreeAnalysis> {
        let empty = || TreeAnalysis { issues: Vec::new(), parent_rewrites: Vec::new(), list_rewrites: Vec::new() };
        let Ok(root) = self.root_node_id() else {
            return Ok(empty());
        };

        let txn = self.doc.transact();
        let node_ids = self.list_node_ids()?;
        if node_ids.is_empty() {
            return Ok(empty());
        }
        let node_set: HashSet<NodeId> = node_ids.iter().copied().collect();

        let mut issues = Vec::new();
        let mut raw_parent: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut effective_parent: HashMap<NodeId, NodeId> = HashMap::new();

        // Step 1: effective parent for every node but the root.
        for &id in &node_ids {
            if id == root {
                continue;
            }
            let p = self.parent_id_of(&txn, id)?;
            raw_parent.insert(id, p);
            let e = match p {
                None => {
                    issues.push(TreeIssue::DetachedNode { node_id: id });
                    root
                }
                Some(p) if p == id || !node_set.contains(&p) => {
                    issues.push(TreeIssue::OrphanNode { node_id: id, missing_parent: p });
                    root
                }
                Some(p) => p,
            };
            effective_parent.insert(id, e);
        }

        // Step 2: break every cycle in the effective-parent functional graph by
        // redirecting the smallest-id member (string order) of each to the root.
        let bound = node_ids.len();
        let mut visited: HashSet<NodeId> = HashSet::new();
        for &start in &node_ids {
            if start == root || visited.contains(&start) {
                continue;
            }
            let mut cur = start;
            let mut reaches_root = false;
            for _ in 0..=bound {
                if cur == root {
                    reaches_root = true;
                    break;
                }
                cur = effective_parent[&cur];
            }
            if reaches_root {
                continue;
            }

            // `start`'s chain loops without reaching the root. Walk it again,
            // recording the path, to find exactly the cycle's members (a walk from a
            // node outside the cycle first crosses a non-cyclic "tail" that needs no
            // change of its own once the cycle it feeds is broken).
            let mut path = Vec::new();
            let mut index_of: HashMap<NodeId, usize> = HashMap::new();
            let mut cur = start;
            loop {
                if let Some(&i) = index_of.get(&cur) {
                    let cycle_members: Vec<NodeId> = path[i..].to_vec();
                    let smallest = cycle_members
                        .iter()
                        .min_by_key(|id| id.to_string())
                        .copied()
                        .expect("a cycle has at least one member");
                    effective_parent.insert(smallest, root);
                    issues.push(TreeIssue::Cycle { node_ids: cycle_members });
                    break;
                }
                index_of.insert(cur, path.len());
                path.push(cur);
                visited.insert(cur);
                cur = effective_parent[&cur];
            }
        }

        // Step 3: parent_id rewrites -- anywhere the stored value differs from the
        // (possibly cycle-corrected) effective parent.
        let mut parent_rewrites = Vec::new();
        for &id in &node_ids {
            if id == root {
                continue;
            }
            let e = effective_parent[&id];
            if raw_parent[&id] != Some(e) {
                parent_rewrites.push((id, e));
            }
        }

        // Steps 4-5: targeted surgery on every children list from the (now well formed)
        // effective-parent map: remove exactly the entries that don't belong, append
        // exactly the children left with no remaining entry anywhere. Every entry not
        // named by a removal is left completely alone — a wholesale clear-and-rebuild
        // would recreate even the already-correct entries as brand new CRDT items,
        // which is harmless on one replica alone but never converges once two replicas
        // both repair the same state and exchange the result: each would fabricate its
        // own fresh copy of every "kept" entry, and those copies would never be
        // recognized as the same entry.
        //
        // Kept as `Option<NodeId>` (not `filter_map`-ed away) so index `i` here always
        // means index `i` in the live array — an unparseable entry (never written by
        // this crate, but not a shape repair should tolerate either) is simply queued
        // for removal like any other invalid one.
        let mut raw_lists: HashMap<NodeId, Vec<Option<NodeId>>> = HashMap::new();
        for &id in &node_ids {
            let strs = self.raw_children_strings(&txn, id)?;
            raw_lists.insert(id, strs.iter().map(|s| NodeId::parse(s).ok()).collect());
        }

        let mut remove_indices: HashMap<NodeId, Vec<usize>> = HashMap::new();
        let mut placed: HashSet<NodeId> = HashSet::new();

        for &owner in &node_ids {
            let mut seen: HashSet<NodeId> = HashSet::new();
            for (idx, entry) in raw_lists[&owner].iter().enumerate() {
                let Some(child_id) = *entry else {
                    remove_indices.entry(owner).or_default().push(idx);
                    continue;
                };
                if child_id == root {
                    issues.push(TreeIssue::RootInList { parent_id: owner });
                    remove_indices.entry(owner).or_default().push(idx);
                    continue;
                }
                if !node_set.contains(&child_id) {
                    issues.push(TreeIssue::MissingChild { parent_id: owner, child_id });
                    remove_indices.entry(owner).or_default().push(idx);
                    continue;
                }
                if !seen.insert(child_id) {
                    issues.push(TreeIssue::DuplicateInList { parent_id: owner, child_id });
                    remove_indices.entry(owner).or_default().push(idx);
                    continue;
                }
                let e = effective_parent[&child_id];
                if e != owner {
                    issues.push(TreeIssue::WrongList { parent_id: owner, child_id, effective_parent: e });
                    remove_indices.entry(owner).or_default().push(idx);
                    continue;
                }
                placed.insert(child_id);
            }
        }

        let mut appends: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        let mut missing: Vec<NodeId> = node_ids
            .iter()
            .copied()
            .filter(|&id| id != root && !placed.contains(&id))
            .collect();
        missing.sort_by_key(|id| id.to_string());
        for child_id in missing {
            let owner = effective_parent[&child_id];
            issues.push(TreeIssue::MissingFromList { parent_id: owner, child_id });
            appends.entry(owner).or_default().push(child_id);
        }

        let mut list_rewrites = Vec::new();
        for &owner in &node_ids {
            let mut removals = remove_indices.remove(&owner).unwrap_or_default();
            let owner_appends = appends.remove(&owner).unwrap_or_default();
            if removals.is_empty() && owner_appends.is_empty() {
                continue;
            }
            // Descending order: `repair` removes the highest index first so an
            // earlier removal never shifts the position of one still to come.
            removals.sort_unstable_by(|a, b| b.cmp(a));
            list_rewrites.push((owner, removals, owner_appends));
        }

        Ok(TreeAnalysis { issues, parent_rewrites, list_rewrites })
    }

    /// The raw string contents of a node's children array, in order, exactly as
    /// stored -- including any duplicate, dangling, or otherwise invalid entries
    /// `analyze` needs to see to report and fix them.
    fn raw_children_strings<T: ReadTxn>(&self, txn: &T, id: NodeId) -> Result<Vec<String>> {
        let children = self.children_array(txn, id)?;
        Ok(children.iter(txn).filter_map(|out| Self::string_value(Some(out))).collect())
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
        let touched: HashSet<_> = b.apply_update(&diff).unwrap().touched.into_iter().collect();
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

        let touched = b.apply_update(&diff).unwrap().touched;
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

        let touched: HashSet<_> = b.apply_update(&diff).unwrap().touched.into_iter().collect();
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

        let touched: HashSet<_> = b.apply_update(&diff).unwrap().touched.into_iter().collect();
        assert!(touched.contains(&child), "expected the removed node's id in {:?}", touched);
        assert!(!b.has_node(child));
    }

    // ── `diff_if_peer_lacks_it` (decision 7) ────────────────────────────

    #[test]
    fn diff_if_peer_lacks_it_is_none_once_both_sides_have_exchanged() {
        let root = NodeId::new();
        let mut local = StoreDocument::new("Store", root).unwrap();
        let child = NodeId::new();
        local.add_node(child, Some(root), "document", "Doc").unwrap();
        let remote = StoreDocument::load(&local.save()).unwrap(); // already has everything

        let remote_sv = remote.state_vector();
        let local_sv = local.state_vector();
        let remote_diff = remote.diff_since(&local_sv).unwrap();

        let push = local.diff_if_peer_lacks_it(&remote_sv, &remote_diff).unwrap();
        assert!(push.is_none(), "expected nothing to push, got {:?}", push);
    }

    #[test]
    fn diff_if_peer_lacks_it_is_some_when_a_struct_is_missing() {
        let root = NodeId::new();
        let local = StoreDocument::new("Store", root).unwrap();
        let remote = StoreDocument::load(&[]).unwrap(); // an uninitialized replica

        let remote_sv = remote.state_vector();
        let remote_diff = remote.diff_since(&remote_sv).unwrap();

        let push = local.diff_if_peer_lacks_it(&remote_sv, &remote_diff).unwrap();
        assert!(push.is_some(), "expected local's whole tree to be pushed to the empty remote");
    }

    #[test]
    fn diff_if_peer_lacks_it_is_some_when_a_deletion_is_missing() {
        let root = NodeId::new();
        let mut local = StoreDocument::new("Store", root).unwrap();
        let child = NodeId::new();
        local.add_node(child, Some(root), "document", "Doc").unwrap();
        let mut remote = StoreDocument::load(&local.save()).unwrap();

        // remote deletes a node local still has.
        remote.remove_node(child).unwrap();

        let local_sv = local.state_vector();
        let local_diff = local.diff_since(&local_sv).unwrap(); // local's self-diff: no new structs

        let push = remote.diff_if_peer_lacks_it(&local_sv, &local_diff).unwrap();
        assert!(push.is_some(), "expected remote's deletion to be pushed back to local");
    }

    // ── `repair`/`validate_tree` (decision 9) ───────────────────────────

    #[test]
    fn repair_fixes_a_self_referential_parent() {
        let root = NodeId::new();
        let mut doc = StoreDocument::new("Store", root).unwrap();
        let child = NodeId::new();
        doc.add_node(child, Some(root), "document", "Doc").unwrap();
        doc.set_parent_id(child, Some(child)).unwrap();

        let issues = doc.validate_tree().unwrap();
        assert!(
            issues.iter().any(|i| matches!(i, TreeIssue::OrphanNode { node_id, missing_parent } if *node_id == child && *missing_parent == child)),
            "expected an OrphanNode issue for the self-reference, got {:?}", issues
        );

        let repair = doc.repair().unwrap().expect("a self-referential parent needs a repair");
        assert!(repair.touched.contains(&child));
        assert!(doc.validate_tree().unwrap().is_empty());
        assert!(doc.repair().unwrap().is_none(), "repair should be idempotent");

        assert_eq!(doc.get_node_info(child).unwrap().parent_id, Some(root));
        assert_eq!(doc.get_children(root).unwrap(), vec![child]);
    }

    #[test]
    fn repair_fixes_a_dangling_parent_reference() {
        let root = NodeId::new();
        let mut doc = StoreDocument::new("Store", root).unwrap();
        let child = NodeId::new();
        doc.add_node(child, Some(root), "document", "Doc").unwrap();
        doc.set_parent_id(child, Some(NodeId::new())).unwrap(); // names no node in the document

        assert!(doc.validate_tree().unwrap().iter().any(|i| matches!(i, TreeIssue::OrphanNode { .. })));

        doc.repair().unwrap().expect("a dangling parent needs a repair");
        assert!(doc.validate_tree().unwrap().is_empty());
        assert_eq!(doc.get_node_info(child).unwrap().parent_id, Some(root));
        assert_eq!(doc.get_children(root).unwrap(), vec![child]);
    }

    #[test]
    fn repair_fixes_a_detached_node() {
        let root = NodeId::new();
        let mut doc = StoreDocument::new("Store", root).unwrap();
        let stray = NodeId::new();
        // No parent given at all: not in any list, no `parent_id` field.
        doc.add_node(stray, None, "document", "Stray").unwrap();

        let issues = doc.validate_tree().unwrap();
        assert!(issues.iter().any(|i| matches!(i, TreeIssue::DetachedNode { node_id } if *node_id == stray)));

        doc.repair().unwrap().expect("a detached node needs a repair");
        assert!(doc.validate_tree().unwrap().is_empty());
        assert_eq!(doc.get_node_info(stray).unwrap().parent_id, Some(root));
        assert_eq!(doc.get_children(root).unwrap(), vec![stray]);
    }

    #[test]
    fn repair_fixes_a_duplicate_within_one_list() {
        let root = NodeId::new();
        let mut doc = StoreDocument::new("Store", root).unwrap();
        let child = NodeId::new();
        doc.add_node(child, Some(root), "document", "Doc").unwrap();
        doc.append_child(root, child).unwrap(); // now listed twice under root

        assert!(
            doc.validate_tree().unwrap().iter().any(|i| matches!(i, TreeIssue::DuplicateInList { .. }))
        );

        doc.repair().unwrap().expect("a duplicate entry needs a repair");
        assert!(doc.validate_tree().unwrap().is_empty());
        assert_eq!(doc.get_children(root).unwrap(), vec![child]);
    }

    #[test]
    fn repair_fixes_a_child_in_the_wrong_list() {
        let root = NodeId::new();
        let mut doc = StoreDocument::new("Store", root).unwrap();
        let folder_a = NodeId::new();
        let folder_b = NodeId::new();
        let child = NodeId::new();
        doc.add_node(folder_a, Some(root), "folder", "A").unwrap();
        doc.add_node(folder_b, Some(root), "folder", "B").unwrap();
        doc.add_node(child, Some(folder_a), "document", "Doc").unwrap();
        doc.append_child(folder_b, child).unwrap(); // wrongly also listed under B

        assert!(doc.validate_tree().unwrap().iter().any(|i| matches!(i, TreeIssue::WrongList { .. })));

        doc.repair().unwrap().expect("a wrong-list entry needs a repair");
        assert!(doc.validate_tree().unwrap().is_empty());
        assert_eq!(doc.get_children(folder_a).unwrap(), vec![child]);
        assert!(doc.get_children(folder_b).unwrap().is_empty());
    }

    #[test]
    fn repair_fixes_the_root_appearing_in_a_list() {
        let root = NodeId::new();
        let mut doc = StoreDocument::new("Store", root).unwrap();
        let folder = NodeId::new();
        doc.add_node(folder, Some(root), "folder", "Folder").unwrap();
        doc.append_child(folder, root).unwrap();

        assert!(doc.validate_tree().unwrap().iter().any(|i| matches!(i, TreeIssue::RootInList { .. })));

        doc.repair().unwrap().expect("root-in-list needs a repair");
        assert!(doc.validate_tree().unwrap().is_empty());
        assert!(doc.get_children(folder).unwrap().is_empty());
    }

    #[test]
    fn repair_fixes_a_two_node_cycle() {
        let root = NodeId::new();
        let mut doc = StoreDocument::new("Store", root).unwrap();
        let folder_a = NodeId::new();
        let folder_b = NodeId::new();
        doc.add_node(folder_a, Some(root), "folder", "A").unwrap();
        doc.add_node(folder_b, Some(root), "folder", "B").unwrap();

        doc.set_parent_id(folder_a, Some(folder_b)).unwrap();
        doc.set_parent_id(folder_b, Some(folder_a)).unwrap();

        let issues = doc.validate_tree().unwrap();
        assert!(
            issues.iter().any(|i| matches!(i, TreeIssue::Cycle { .. })),
            "expected a Cycle issue, got {:?}", issues
        );

        let repair = doc.repair().unwrap().expect("a cycle needs a repair");
        assert!(!repair.touched.is_empty());
        assert!(doc.validate_tree().unwrap().is_empty());
        assert!(doc.repair().unwrap().is_none(), "repair should be idempotent");

        let (top, bottom) = if doc.get_node_info(folder_a).unwrap().parent_id == Some(root) {
            (folder_a, folder_b)
        } else {
            (folder_b, folder_a)
        };
        assert_eq!(doc.get_node_info(top).unwrap().parent_id, Some(root));
        assert_eq!(doc.get_node_info(bottom).unwrap().parent_id, Some(top));
        assert_eq!(doc.get_children(root).unwrap(), vec![top]);
        assert_eq!(doc.get_children(top).unwrap(), vec![bottom]);
        assert!(doc.get_children(bottom).unwrap().is_empty());
    }

    #[test]
    fn repair_is_a_no_op_on_a_well_formed_tree() {
        let root = NodeId::new();
        let mut doc = StoreDocument::new("Store", root).unwrap();
        let child = NodeId::new();
        doc.add_node(child, Some(root), "document", "Doc").unwrap();

        assert!(doc.repair().unwrap().is_none());
    }

    // ── Concurrency scenarios (decision 9): two replicas exchanging updates
    // converge on the same well-formed tree within two rounds of
    // exchange-and-repair. ─────────────────────────────────────────────

    /// Exchange every diff both ways (both replicas must already share a common base,
    /// e.g. one loaded from the other's `save()`, before diverging).
    fn sync(a: &mut StoreDocument, b: &mut StoreDocument) {
        let a_sv = a.state_vector();
        let b_sv = b.state_vector();
        let to_b = a.diff_since(&b_sv).unwrap();
        let to_a = b.diff_since(&a_sv).unwrap();
        b.apply_update(&to_b).unwrap();
        a.apply_update(&to_a).unwrap();
    }

    /// One round as the server actually performs it: exchange, then each side repairs
    /// its own (now-merged) state, and the repair's own update — like any other
    /// structural change with `source_client_id: None` — is forwarded to the other side
    /// too.
    fn sync_and_repair_round(a: &mut StoreDocument, b: &mut StoreDocument) {
        sync(a, b);
        let repair_a = a.repair().unwrap();
        let repair_b = b.repair().unwrap();
        if let Some(r) = &repair_a {
            b.apply_update(&r.update).unwrap();
        }
        if let Some(r) = &repair_b {
            a.apply_update(&r.update).unwrap();
        }
    }

    /// Both replicas are well formed, `repair` finds nothing left to do on either, and
    /// every node has the same parent and the same ordered children on both sides.
    fn assert_converged(a: &mut StoreDocument, b: &mut StoreDocument, root: NodeId) {
        let a_issues = a.validate_tree().unwrap();
        assert!(a_issues.is_empty(), "a not well formed: {:?}", a_issues);
        let b_issues = b.validate_tree().unwrap();
        assert!(b_issues.is_empty(), "b not well formed: {:?}", b_issues);
        assert!(a.repair().unwrap().is_none(), "a should have nothing left to repair");
        assert!(b.repair().unwrap().is_none(), "b should have nothing left to repair");

        let mut stack = vec![root];
        let mut seen = HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            assert_eq!(
                a.get_node_info(id).unwrap().parent_id,
                b.get_node_info(id).unwrap().parent_id,
                "node {} parent differs between replicas", id
            );
            let a_children = a.get_children(id).unwrap();
            let b_children = b.get_children(id).unwrap();
            assert_eq!(a_children, b_children, "node {} children differ between replicas", id);
            stack.extend(a_children);
        }
    }

    #[test]
    fn concurrent_moves_of_one_node_to_two_parents_converge() {
        let root = NodeId::new();
        let mut a = StoreDocument::new("Store", root).unwrap();
        let folder_a = NodeId::new();
        let folder_b = NodeId::new();
        let child = NodeId::new();
        a.add_node(folder_a, Some(root), "folder", "A").unwrap();
        a.add_node(folder_b, Some(root), "folder", "B").unwrap();
        a.add_node(child, Some(folder_a), "document", "Doc").unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();

        // Diverge: each replica moves the same node to a different destination without
        // having seen the other's move.
        a.move_node(child, folder_b, None).unwrap();
        b.move_node(child, root, None).unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);

        assert_converged(&mut a, &mut b, root);
        // The node landed under exactly one of the two contended destinations.
        let winner = a.get_node_info(child).unwrap().parent_id.unwrap();
        assert!(winner == folder_b || winner == root, "unexpected winner {}", winner);
        let winner_children = a.get_children(winner).unwrap();
        assert_eq!(
            winner_children.iter().filter(|&&id| id == child).count(), 1,
            "expected child exactly once under its winning parent, got {:?}", winner_children
        );
        assert!(a.get_children(folder_a).unwrap().is_empty(), "child must be gone from its original parent");
    }

    #[test]
    fn concurrent_moves_of_a_under_b_and_b_under_a_converge() {
        let root = NodeId::new();
        let mut a = StoreDocument::new("Store", root).unwrap();
        let folder_a = NodeId::new();
        let folder_b = NodeId::new();
        a.add_node(folder_a, Some(root), "folder", "A").unwrap();
        a.add_node(folder_b, Some(root), "folder", "B").unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();

        // Diverge: each replica moves the other folder underneath the first, forming a
        // 2-cycle once merged.
        a.move_node(folder_b, folder_a, None).unwrap();
        b.move_node(folder_a, folder_b, None).unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);

        assert_converged(&mut a, &mut b, root);
        // Exactly one of the two now sits directly under the root.
        let a_parent = a.get_node_info(folder_a).unwrap().parent_id;
        let b_parent = a.get_node_info(folder_b).unwrap().parent_id;
        assert!(
            (a_parent == Some(root)) ^ (b_parent == Some(root)),
            "expected exactly one of the cycle's members under the root, got {:?}/{:?}", a_parent, b_parent
        );
    }

    #[test]
    fn deleting_a_folder_while_the_other_side_adds_a_child_to_it_converges() {
        let root = NodeId::new();
        let mut a = StoreDocument::new("Store", root).unwrap();
        let folder = NodeId::new();
        a.add_node(folder, Some(root), "folder", "Folder").unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();

        // Diverge: a deletes the folder; b, unaware, adds a child to it.
        a.remove_node(folder).unwrap();
        let new_child = NodeId::new();
        b.add_node(new_child, Some(folder), "document", "New").unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);

        assert_converged(&mut a, &mut b, root);
        // The folder is gone; its would-be child survives, reparented to the root
        // rather than lost (decision 10).
        assert!(!a.has_node(folder));
        assert_eq!(a.get_node_info(new_child).unwrap().parent_id, Some(root));
        assert!(a.get_children(root).unwrap().contains(&new_child));
    }

    #[test]
    fn deleting_a_node_while_the_other_side_moves_it_converges() {
        let root = NodeId::new();
        let mut a = StoreDocument::new("Store", root).unwrap();
        let folder_a = NodeId::new();
        let folder_b = NodeId::new();
        let child = NodeId::new();
        a.add_node(folder_a, Some(root), "folder", "A").unwrap();
        a.add_node(folder_b, Some(root), "folder", "B").unwrap();
        a.add_node(child, Some(folder_a), "document", "Doc").unwrap();
        let mut b = StoreDocument::load(&a.save()).unwrap();

        // Diverge: a deletes the child; b, unaware, moves it elsewhere.
        a.remove_node(child).unwrap();
        b.move_node(child, folder_b, None).unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);

        assert_converged(&mut a, &mut b, root);
        // The delete wins: the node is gone from both replicas, and the list entry the
        // move left behind was cleaned up rather than left dangling.
        assert!(!a.has_node(child));
        assert!(!a.get_children(folder_b).unwrap().contains(&child));
    }
}
