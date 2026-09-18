//! The node document: one yrs `Doc` per node holding its rich text, its place
//! in the tree, its metadata, and a root reserved for a plugin's JSON
//! (docs/NODE_DOCUMENT_CONTRACT.md section 1). Everything a person can edit
//! about a node is in here and co-edited: this is what `CLAUDE.md`'s
//! "everything is co-editable" means in bytes.
//!
//! Roots: `content` and `meta` belong to rinch-editor-collab (the rich text and
//! its format tag; Pimble never writes `meta`); `node`, `children` and `data`
//! are Pimble's. A `NodeDoc` that has no `node` root yet is a content-only
//! document (a phase 2a `ContentDoc` on disk, or one made by the importer
//! before the tree places it); [`NodeDoc::init`] gives it one.

use std::collections::HashMap;

use pimble_core::{IndexUnit, NodeId};
use yrs::Doc;

use crate::blocks::Block;
use crate::error::{CrdtError, Result};

/// The root map holding a node's structure and metadata.
pub const ROOT_NODE: &str = "node";
/// The root array holding a node's child ids, in order.
pub const ROOT_CHILDREN: &str = "children";
/// The root map reserved for a plugin node type's JSON.
pub const ROOT_DATA: &str = "data";

/// What the `node` root holds, read out as plain values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeFields {
    pub node_type: String,
    pub title: String,
    /// Absent on a store's root node.
    pub parent_id: Option<NodeId>,
    pub created_at: String,
    pub modified_at: String,
    /// A tombstone (docs/NODE_DOCUMENT_CONTRACT.md section 2): set when the
    /// node was deleted, cleared by an undelete. The document stays.
    pub deleted_at: Option<String>,
    pub tags: Vec<String>,
    pub custom: HashMap<String, serde_json::Value>,
}

/// What merging a peer's update changed (decision 8 of
/// docs/history/HARDENING_CONTRACT.md, per root): `changed` is `false` exactly
/// when every part of the update was already reflected here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeUpdateEffect {
    pub changed: bool,
    /// The `node` or `children` root changed.
    pub structure: bool,
    /// The `content` root changed.
    pub content: bool,
    /// The `data` root changed.
    pub data: bool,
}

/// One node's document. See the module doc.
pub struct NodeDoc {
    #[allow(dead_code)]
    doc: Doc,
}

fn not_built(what: &str) -> CrdtError {
    CrdtError::Yrs(format!("NodeDoc::{what} is not built yet (docs/NODE_DOCUMENT_CONTRACT.md wave 1)"))
}

impl NodeDoc {
    // ── Construction and bytes (the `ContentDoc` surface, kept) ──────────

    /// An empty document: empty content, no `node` root yet.
    pub fn new() -> Self {
        let options = yrs::Options { offset_kind: yrs::OffsetKind::Utf16, ..Default::default() };
        Self { doc: Doc::with_options(options) }
    }

    /// Load from `bytes`, a yrs v1 update (a full snapshot or any update).
    /// Empty bytes are [`NodeDoc::new`].
    pub fn load(_bytes: &[u8]) -> Result<Self> {
        Err(not_built("load"))
    }

    /// One paragraph per line of `text`; no `node` root yet.
    pub fn from_plain_text(_text: &str) -> Result<Self> {
        Err(not_built("from_plain_text"))
    }

    /// Content from blocks (the importer's way in); no `node` root yet.
    pub fn from_blocks(_blocks: &[Block]) -> Result<Self> {
        Err(not_built("from_blocks"))
    }

    /// Full snapshot: the v1 update encoding of the whole document from an
    /// empty state vector. Round-trips through [`NodeDoc::load`].
    pub fn save(&self) -> Vec<u8> {
        Vec::new()
    }

    /// This document's state vector, v1-encoded.
    pub fn state_vector(&self) -> Vec<u8> {
        Vec::new()
    }

    /// Everything this document has that a peer at `state_vector` lacks.
    pub fn diff_since(&self, _state_vector: &[u8]) -> Result<Vec<u8>> {
        Err(not_built("diff_since"))
    }

    /// Merge a peer's v1 update (a delta, a reconciliation diff, or a whole
    /// snapshot), reporting what it changed and in which roots.
    pub fn apply_update(&mut self, _update: &[u8]) -> Result<NodeUpdateEffect> {
        Err(not_built("apply_update"))
    }

    /// Whether the peer lacks something of ours, given its state vector and
    /// the diff it sent (docs/history/HARDENING_CONTRACT.md decision 7).
    pub fn diff_if_peer_lacks_it(&self, _remote_sv: &[u8], _remote_diff: &[u8]) -> Result<Option<Vec<u8>>> {
        Err(not_built("diff_if_peer_lacks_it"))
    }

    // ── Content ──────────────────────────────────────────────────────────

    /// Replace the content with one paragraph per line of `text`, as an edit
    /// of the existing document (it shares history with every copy). Returns
    /// the update the edit produced.
    pub fn replace_plain_text(&mut self, _text: &str) -> Result<Vec<u8>> {
        Err(not_built("replace_plain_text"))
    }

    /// The content as plain text, paragraphs joined with newlines.
    pub fn text(&self) -> String {
        String::new()
    }

    /// [`NodeDoc::text`] of a serialized document, without keeping it.
    pub fn text_of(bytes: &[u8]) -> String {
        Self::load(bytes).map(|doc| doc.text()).unwrap_or_default()
    }

    /// The content as index units (prose, headings, code, ...), for the search
    /// index and the chunker.
    pub fn units(&self) -> Vec<IndexUnit> {
        Vec::new()
    }

    /// [`NodeDoc::units`] of a serialized document.
    pub fn units_of(bytes: &[u8]) -> Vec<IndexUnit> {
        Self::load(bytes).map(|doc| doc.units()).unwrap_or_default()
    }

    // ── Structure and metadata (the `node` and `children` roots) ─────────

    /// Whether the `node` root has been written (by [`NodeDoc::init`] here or
    /// on any peer).
    pub fn is_initialised(&self) -> bool {
        false
    }

    /// Write the `node` root for a node placed in a tree. An error when it is
    /// already initialised: a second `init` would fight the first.
    pub fn init(&mut self, _node_type: &str, _title: &str, _parent_id: Option<NodeId>, _now: &str) -> Result<()> {
        Err(not_built("init"))
    }

    /// The `node` root as values. An error when not initialised.
    pub fn fields(&self) -> Result<NodeFields> {
        Err(not_built("fields"))
    }

    pub fn set_title(&mut self, _title: &str, _now: &str) -> Result<()> {
        Err(not_built("set_title"))
    }

    pub fn set_node_type(&mut self, _node_type: &str, _now: &str) -> Result<()> {
        Err(not_built("set_node_type"))
    }

    /// `None` removes a stored parent (the node becomes a root).
    pub fn set_parent_id(&mut self, _parent_id: Option<NodeId>, _now: &str) -> Result<()> {
        Err(not_built("set_parent_id"))
    }

    /// Replace the tags wholesale.
    pub fn set_tags(&mut self, _tags: &[String], _now: &str) -> Result<()> {
        Err(not_built("set_tags"))
    }

    pub fn set_custom(&mut self, _key: &str, _value: &serde_json::Value, _now: &str) -> Result<()> {
        Err(not_built("set_custom"))
    }

    /// Returns whether the key was there; an absent key writes nothing.
    pub fn remove_custom(&mut self, _key: &str, _now: &str) -> Result<bool> {
        Err(not_built("remove_custom"))
    }

    /// Both timestamps at once (the importer, the migration).
    pub fn set_timestamps(&mut self, _created_at: &str, _modified_at: &str) -> Result<()> {
        Err(not_built("set_timestamps"))
    }

    pub fn touch_modified(&mut self, _now: &str) -> Result<()> {
        Err(not_built("touch_modified"))
    }

    /// Set the tombstone (`Some(now)`) or clear it (`None`, an undelete).
    pub fn set_deleted(&mut self, _at: Option<&str>) -> Result<()> {
        Err(not_built("set_deleted"))
    }

    /// The children as stored: duplicates, ids with no document, and deleted
    /// nodes included. `Tree` is what filters and repairs.
    pub fn children(&self) -> Vec<NodeId> {
        Vec::new()
    }

    /// Insert `id` at `at` (clamped to the end).
    pub fn insert_child(&mut self, _at: usize, _id: NodeId) -> Result<()> {
        Err(not_built("insert_child"))
    }

    /// Remove every occurrence of `id`; returns whether there was one.
    pub fn remove_child(&mut self, _id: NodeId) -> Result<bool> {
        Err(not_built("remove_child"))
    }

    /// Remove the entry at `at` (repair's surgical edit).
    pub fn remove_child_at(&mut self, _at: usize) -> Result<()> {
        Err(not_built("remove_child_at"))
    }

    // ── The plugin root ──────────────────────────────────────────────────

    /// The `data` root as JSON (a plugin's read view). An empty object when
    /// nothing was ever written.
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::Value::Object(Default::default())
    }

    /// Set one top-level key of the `data` root from JSON: objects and arrays
    /// become nested yrs maps and arrays (co-editable field by field), strings
    /// become yrs text, scalars are values. The finer plugin API comes with
    /// the plugins; this is enough to round-trip.
    pub fn set_data(&mut self, _key: &str, _value: &serde_json::Value) -> Result<()> {
        Err(not_built("set_data"))
    }
}

impl Default for NodeDoc {
    fn default() -> Self {
        Self::new()
    }
}
