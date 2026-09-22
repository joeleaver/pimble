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
//!
//! Layout of Pimble's roots:
//! ```text
//! root map "node": { node_type: String, title: String,
//!                    parent_id: String | absent (the store's root),
//!                    created_at: String (rfc3339), modified_at: String (rfc3339),
//!                    deleted_at: String (rfc3339) | absent (the tombstone),
//!                    tags: Array<String>, custom: Map<String, String (json)> }
//! root array "children": [String (node id), ...]
//! root map "data": arbitrary JSON as nested Maps, Arrays and Texts
//! ```
//! Every root is resolved with its type when the document is built, before any
//! bytes are applied: yrs sends no type for a root, and a root it has never
//! been asked for by type reports no change (its events have no shape), which
//! would blind [`NodeDoc::apply_update`] to a peer's first write into it.
//! Timestamps are the caller's (`now`, rfc3339); this crate never reads a
//! clock, so what two replicas write is decided by their callers alone.

use std::collections::HashMap;

use pimble_core::{IndexUnit, NodeId};
use yrs::updates::encoder::Encode;
use yrs::{
    Any, Array, ArrayPrelim, ArrayRef, BranchID, Doc, GetString, In, Map, MapPrelim, MapRef, Out,
    ReadTxn, StateVector, TextPrelim, Transact, TransactionMut,
};

use crate::blocks::{blocks_from_plain_text, Block};
use crate::content_doc::{
    blocks_of_projection, decode_update, diff_since, fresh_snapshot, join_units, replacement_delta,
    snapshot_from_blocks, transaction_changed, units_of_projection,
};
use crate::error::{CrdtError, Result};

/// The root map holding a node's structure and metadata.
pub const ROOT_NODE: &str = "node";
/// The root array holding a node's child ids, in order.
pub const ROOT_CHILDREN: &str = "children";
/// The root map reserved for a plugin node type's JSON.
pub const ROOT_DATA: &str = "data";

/// rinch-editor-collab's roots (its `projection.rs`), named here to resolve
/// them by type and to tell a content change from a structural one. The
/// format tag it writes under `meta` is what says a document holds a
/// projection at all.
const ROOT_CONTENT: &str = "content";
const ROOT_META: &str = "meta";
const META_FORMAT: &str = "format";

const NODE_TYPE: &str = "node_type";
const TITLE: &str = "title";
const PARENT_ID: &str = "parent_id";
/// The parent this document was last placed under by an operation that also
/// edited the lists (a create, move, undelete, plant, or a repair's own
/// rewrite): written with `parent_id` in the same transaction. A `parent_id`
/// that differs from it was written on its own, which is the shape repair
/// judges (docs/MOVE_CONTRACT.md "Repair"); one that agrees is honoured by
/// every device and never rewritten, which is what keeps repair convergent.
const PLACED_UNDER: &str = "placed_under";
const CREATED_AT: &str = "created_at";
const MODIFIED_AT: &str = "modified_at";
const DELETED_AT: &str = "deleted_at";
const TAGS: &str = "tags";
const CUSTOM: &str = "custom";

/// What the `node` root holds, read out as plain values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeFields {
    pub node_type: String,
    pub title: String,
    /// Absent on a store's root node.
    pub parent_id: Option<NodeId>,
    /// The parent `parent_id` was last written together with (see
    /// `PLACED_UNDER`); absent on the root and on a document from before it
    /// was recorded.
    pub placed_under: Option<NodeId>,
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
    doc: Doc,
    node: MapRef,
    children: ArrayRef,
    data: MapRef,
    meta: MapRef,
}

#[cfg(test)]
thread_local! {
    /// Test only: while set, every new document on this thread takes the next
    /// client id from here instead of a random one, so a seeded test replays
    /// the same merges (yrs orders concurrent inserts by client id).
    static NEXT_CLIENT_ID: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Test only: from here on, documents made on this thread take client ids
/// `first`, `first + 1`, ... (see `NEXT_CLIENT_ID`).
#[cfg(test)]
pub(crate) fn seed_client_ids(first: u64) {
    NEXT_CLIENT_ID.with(|next| next.set(Some(first)));
}

impl NodeDoc {
    // ── Construction and bytes (the `ContentDoc` surface, kept) ──────────

    /// An empty document: empty content, no `node` root yet.
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut options = yrs::Options { offset_kind: yrs::OffsetKind::Utf16, ..Default::default() };
        #[cfg(test)]
        NEXT_CLIENT_ID.with(|next| {
            if let Some(id) = next.get() {
                options.client_id = yrs::ClientID::new(id);
                next.set(Some(id + 1));
            }
        });
        let doc = Doc::with_options(options);
        // Resolving a root opens its own transaction, so all five are resolved
        // here, before any caller can hold one (see the module doc for why the
        // collab roots are resolved at all). A root with no items adds nothing
        // to the saved bytes, so an empty `NodeDoc` saves as an empty document.
        let node = doc.get_or_insert_map(ROOT_NODE);
        let children = doc.get_or_insert_array(ROOT_CHILDREN);
        let data = doc.get_or_insert_map(ROOT_DATA);
        let meta = doc.get_or_insert_map(ROOT_META);
        let _content = doc.get_or_insert_array(ROOT_CONTENT);
        Self { doc, node, children, data, meta }
    }

    /// Load from `bytes`, a yrs v1 update (a full snapshot or any update).
    /// Empty bytes are [`NodeDoc::new`].
    pub fn load(bytes: &[u8]) -> Result<Self> {
        let mut doc = Self::new();
        if !bytes.is_empty() {
            doc.apply_update(bytes)?;
        }
        Ok(doc)
    }

    /// One paragraph per line of `text`; no `node` root yet.
    pub fn from_plain_text(text: &str) -> Result<Self> {
        Self::from_blocks(&blocks_from_plain_text(text))
    }

    /// Content from blocks (the importer's way in); no `node` root yet.
    pub fn from_blocks(blocks: &[Block]) -> Result<Self> {
        Self::load(&snapshot_from_blocks(blocks)?)
    }

    /// Full snapshot: the v1 update encoding of the whole document from an
    /// empty state vector. Round-trips through [`NodeDoc::load`].
    pub fn save(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }

    /// This document's state vector, v1-encoded.
    pub fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    /// Everything this document has that a peer at `state_vector` lacks.
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>> {
        diff_since(&self.doc, state_vector)
    }

    /// Merge a peer's v1 update (a delta, a reconciliation diff, or a whole
    /// snapshot), reporting what it changed and in which roots.
    ///
    /// `changed` comes from the merging transaction's before/after state and
    /// delete set, as `ContentDoc` computes it. The roots come from the same
    /// transaction: at commit yrs walks from every type an insert or delete
    /// touched up to its root and records the chain
    /// (`TransactionMut::changed_parent_types`), so a root's name there means
    /// something under it changed, however deep. An update yrs parks because
    /// it depends on structs not seen yet reports nothing now; its changes are
    /// reported by the transaction that finally integrates it.
    pub fn apply_update(&mut self, update: &[u8]) -> Result<NodeUpdateEffect> {
        let update = decode_update(update)?;
        let mut txn = self.doc.transact_mut();
        txn.apply_update(update).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        txn.commit();
        let mut effect = NodeUpdateEffect { changed: transaction_changed(&txn), ..Default::default() };
        for branch in txn.changed_parent_types() {
            if let BranchID::Root(name) = branch.id() {
                match &*name {
                    ROOT_NODE | ROOT_CHILDREN => effect.structure = true,
                    ROOT_CONTENT | ROOT_META => effect.content = true,
                    ROOT_DATA => effect.data = true,
                    _ => {}
                }
            }
        }
        Ok(effect)
    }

    /// Whether the peer lacks something of ours, given its state vector and
    /// the diff it sent (docs/history/HARDENING_CONTRACT.md decision 7).
    pub fn diff_if_peer_lacks_it(&self, remote_sv: &[u8], remote_diff: &[u8]) -> Result<Option<Vec<u8>>> {
        let local_diff = self.diff_since(remote_sv)?;
        if crate::sync_util::peer_lacks_something(&local_diff, remote_diff)? {
            Ok(Some(local_diff))
        } else {
            Ok(None)
        }
    }

    // ── Content ──────────────────────────────────────────────────────────

    /// Replace the content with one paragraph per line of `text`, as an edit
    /// of the existing document (it shares history with every copy). Returns
    /// the update the edit produced.
    ///
    /// A document with no projection yet (no format tag in `meta`: a node
    /// whose content was never written, whether or not its `node` root is)
    /// is seeded the way [`NodeDoc::from_plain_text`] builds one, and the
    /// update is that whole snapshot. `ContentDoc` decides the same by an
    /// empty state vector; here the `node` root may hold structs while the
    /// content holds none, so the tag is what counts.
    pub fn replace_plain_text(&mut self, text: &str) -> Result<Vec<u8>> {
        let seed = !self.has_projection();
        let delta = replacement_delta(&self.save(), seed, text)?;
        self.apply_update(&delta)?;
        Ok(delta)
    }

    /// The content as plain text, paragraphs joined with newlines.
    pub fn text(&self) -> String {
        join_units(&self.units())
    }

    /// [`NodeDoc::text`] of a serialized document, without keeping it.
    pub fn text_of(bytes: &[u8]) -> String {
        Self::load(bytes).map(|doc| doc.text()).unwrap_or_default()
    }

    /// The content as index units (prose, headings, code, ...), for the search
    /// index and the chunker. `[]` when there is no projection yet.
    pub fn units(&self) -> Vec<IndexUnit> {
        if !self.has_projection() {
            return Vec::new();
        }
        units_of_projection(&self.save(), "NodeDoc::units")
    }

    /// [`NodeDoc::units`] of a serialized document.
    pub fn units_of(bytes: &[u8]) -> Vec<IndexUnit> {
        Self::load(bytes).map(|doc| doc.units()).unwrap_or_default()
    }

    /// The content as [`Block`]s, the reverse of [`NodeDoc::from_blocks`]: `[]`
    /// when there is no projection yet, an error when the content cannot be
    /// projected or holds something `Block` has no variant for. `Block` cannot
    /// say everything the editor can (see `blocks::read_doc`), so this is for
    /// reading a document from outside the editor, never for copying one.
    pub fn blocks(&self) -> Result<Vec<Block>> {
        if !self.has_projection() {
            return Ok(Vec::new());
        }
        blocks_of_projection(&self.save())
    }

    /// The content as a NEW document's bytes (its own history, nothing that
    /// was deleted, everything the editor's model holds: see
    /// `content_doc::fresh_snapshot` for why a transplant carries content this
    /// way). `None` when there is no projection yet, which the new node keeps
    /// as it is: no projection, seeded by its first edit like any other.
    pub(crate) fn fresh_content(&self) -> Result<Option<Vec<u8>>> {
        if !self.has_projection() {
            return Ok(None);
        }
        fresh_snapshot(&self.save()).map(Some)
    }

    /// Whether rinch-editor-collab has written its format tag: the content
    /// roots hold a projection an editor can join.
    fn has_projection(&self) -> bool {
        let txn = self.doc.transact();
        matches!(self.meta.get(&txn, META_FORMAT), Some(Out::Any(Any::String(_))))
    }

    // ── Structure and metadata (the `node` and `children` roots) ─────────

    /// Whether the `node` root has been written (by [`NodeDoc::init`] here or
    /// on any peer).
    pub fn is_initialised(&self) -> bool {
        self.node.len(&self.doc.transact()) > 0
    }

    /// Write the `node` root for a node placed in a tree. An error when it is
    /// already initialised: a second `init` would fight the first.
    pub fn init(&mut self, node_type: &str, title: &str, parent_id: Option<NodeId>, now: &str) -> Result<()> {
        self.edit(|d, txn| d.write_init(txn, node_type, title, parent_id, now)).map(|_| ())
    }

    /// The `node` root as values. An error when not initialised. A
    /// `parent_id` this crate cannot parse reads as absent, so garbage in that
    /// field makes a detached node (which repair puts under the root), never
    /// a node the tree cannot read.
    pub fn fields(&self) -> Result<NodeFields> {
        let txn = self.doc.transact();
        self.read_fields(&txn)
    }

    pub fn set_title(&mut self, title: &str, now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_title(txn, title, now)).map(|_| ())
    }

    pub fn set_node_type(&mut self, node_type: &str, now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_node_type(txn, node_type, now)).map(|_| ())
    }

    /// `None` removes a stored parent (the node becomes a root).
    pub fn set_parent_id(&mut self, parent_id: Option<NodeId>, now: &str) -> Result<()> {
        self.edit_node(|d, txn| {
            d.write_parent(txn, parent_id);
            d.stamp_modified(txn, now);
            Ok(())
        })
        .map(|_| ())
    }

    /// Replace the tags wholesale.
    pub fn set_tags(&mut self, tags: &[String], now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_tags(txn, tags, now)).map(|_| ())
    }

    /// Stored as JSON text, as the store document stored `custom`: a value is
    /// replaced whole, and two replicas setting the same key converge on one
    /// of them. Field-by-field co-editing is what `data` is for.
    pub fn set_custom(&mut self, key: &str, value: &serde_json::Value, now: &str) -> Result<()> {
        self.edit_node(|d, txn| d.write_custom(txn, key, value, now)).map(|_| ())
    }

    /// Returns whether the key was there; an absent key writes nothing.
    pub fn remove_custom(&mut self, key: &str, now: &str) -> Result<bool> {
        self.edit_node(|d, txn| Ok(d.remove_custom_key(txn, key, now))).map(|(removed, _)| removed)
    }

    /// Both timestamps at once (the importer, the migration).
    pub fn set_timestamps(&mut self, created_at: &str, modified_at: &str) -> Result<()> {
        self.edit_node(|d, txn| {
            d.node.insert(txn, CREATED_AT, created_at.to_string());
            d.node.insert(txn, MODIFIED_AT, modified_at.to_string());
            Ok(())
        })
        .map(|_| ())
    }

    pub fn touch_modified(&mut self, now: &str) -> Result<()> {
        self.edit_node(|d, txn| {
            d.stamp_modified(txn, now);
            Ok(())
        })
        .map(|_| ())
    }

    /// Set the tombstone (`Some(now)`) or clear it (`None`, an undelete).
    pub fn set_deleted(&mut self, at: Option<&str>) -> Result<()> {
        self.edit_node(|d, txn| {
            d.write_deleted(txn, at);
            Ok(())
        })
        .map(|_| ())
    }

    /// The children as stored: duplicates, ids with no document, and deleted
    /// nodes included. `Tree` is what filters and repairs.
    pub fn children(&self) -> Vec<NodeId> {
        self.raw_children().into_iter().flatten().collect()
    }

    /// Insert `id` at `at` (clamped to the end).
    pub fn insert_child(&mut self, at: usize, id: NodeId) -> Result<()> {
        self.edit(|d, txn| {
            d.list_insert(txn, Some(at), id);
            Ok(())
        })
        .map(|_| ())
    }

    /// Remove every occurrence of `id`; returns whether there was one.
    pub fn remove_child(&mut self, id: NodeId) -> Result<bool> {
        self.edit(|d, txn| Ok(d.list_remove_all(txn, id))).map(|(removed, _)| removed)
    }

    /// Remove the entry at `at` (repair's surgical edit).
    pub fn remove_child_at(&mut self, at: usize) -> Result<()> {
        self.edit(|d, txn| d.list_remove_at(txn, at)).map(|_| ())
    }

    // ── The plugin root ──────────────────────────────────────────────────

    /// The `data` root as JSON (a plugin's read view). An empty object when
    /// nothing was ever written.
    pub fn data_json(&self) -> serde_json::Value {
        let txn = self.doc.transact();
        map_to_json(&txn, &self.data)
    }

    /// Set one top-level key of the `data` root from JSON: objects and arrays
    /// become nested yrs maps and arrays (co-editable field by field), strings
    /// become yrs text, scalars are values. The finer plugin API comes with
    /// the plugins; this is enough to round-trip.
    pub fn set_data(&mut self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.edit(|d, txn| {
            d.put_data(txn, key, value);
            Ok(())
        })
        .map(|_| ())
    }

    // ── Transactions (shared with `Tree`) ────────────────────────────────

    /// Run `f` in one write transaction and return its value with the update
    /// the transaction produced: the transaction's own structs and deletions
    /// (`TransactionMut::encode_update_v1`), not a diff, so it carries only
    /// this edit and never the document's whole delete set. `None` when the
    /// transaction changed nothing, so a caller can leave a no-op out of what
    /// it persists and broadcasts. `Tree` composes several writes to one
    /// document into one transaction through this.
    pub(crate) fn edit<R>(
        &self,
        f: impl FnOnce(&Self, &mut TransactionMut) -> Result<R>,
    ) -> Result<(R, Option<Vec<u8>>)> {
        let mut txn = self.doc.transact_mut();
        let value = f(self, &mut txn)?;
        txn.commit();
        let update = transaction_changed(&txn).then(|| txn.encode_update_v1());
        Ok((value, update))
    }

    /// [`NodeDoc::edit`] for a write to the `node` root, which must exist.
    pub(crate) fn edit_node<R>(
        &self,
        f: impl FnOnce(&Self, &mut TransactionMut) -> Result<R>,
    ) -> Result<(R, Option<Vec<u8>>)> {
        self.edit(|d, txn| {
            d.require_initialised(txn)?;
            f(d, txn)
        })
    }

    pub(crate) fn require_initialised<T: ReadTxn>(&self, txn: &T) -> Result<()> {
        if self.node.len(txn) == 0 {
            return Err(CrdtError::Serialization("node document is not initialised (no `node` root)".into()));
        }
        Ok(())
    }

    /// Initialised and not tombstoned: what `Tree` calls a node. Cheaper than
    /// [`NodeDoc::fields`], which the tree asks about every document often.
    pub(crate) fn is_live(&self) -> bool {
        self.placement().is_some()
    }

    /// Initialised and tombstoned: a document this device *knows* is deleted,
    /// which is a different thing from one it does not hold.
    pub(crate) fn is_tombstone(&self) -> bool {
        let txn = self.doc.transact();
        self.node.len(&txn) > 0 && self.node.get(&txn, DELETED_AT).is_some()
    }

    /// A node's stored `parent_id`, or `None` when this is not a node
    /// (uninitialised or tombstoned): all repair needs of the `node` root,
    /// read without the rest of the fields.
    pub(crate) fn placement(&self) -> Option<Option<NodeId>> {
        let txn = self.doc.transact();
        if self.node.len(&txn) == 0 || self.node.get(&txn, DELETED_AT).is_some() {
            return None;
        }
        Some(string_value(self.node.get(&txn, PARENT_ID)).and_then(|s| NodeId::parse(&s).ok()))
    }

    /// The stored `placed_under` (see `PLACED_UNDER`), read on its own.
    pub(crate) fn placed_under(&self) -> Option<NodeId> {
        let txn = self.doc.transact();
        string_value(self.node.get(&txn, PLACED_UNDER)).and_then(|s| NodeId::parse(&s).ok())
    }

    /// What walking up the tree needs of a held document, tombstoned or not:
    /// whether `custom` holds `key` (as [`NodeDoc::fields`] reports it: a
    /// value that parses) and the stored `parent_id`. `None` when the `node`
    /// root has not arrived, which is a document that says nothing yet.
    pub(crate) fn link(&self, key: &str) -> Option<(bool, Option<NodeId>)> {
        let txn = self.doc.transact();
        if self.node.len(&txn) == 0 {
            return None;
        }
        let carries = self
            .custom_map_if_present(&txn)
            .and_then(|custom| string_value(custom.get(&txn, key)))
            .is_some_and(|json| serde_json::from_str::<serde_json::Value>(&json).is_ok());
        let parent = string_value(self.node.get(&txn, PARENT_ID)).and_then(|s| NodeId::parse(&s).ok());
        Some((carries, parent))
    }

    pub(crate) fn write_title(&self, txn: &mut TransactionMut, title: &str, now: &str) -> Result<()> {
        self.node.insert(txn, TITLE, title.to_string());
        self.stamp_modified(txn, now);
        Ok(())
    }

    pub(crate) fn write_node_type(&self, txn: &mut TransactionMut, node_type: &str, now: &str) -> Result<()> {
        self.node.insert(txn, NODE_TYPE, node_type.to_string());
        self.stamp_modified(txn, now);
        Ok(())
    }

    pub(crate) fn write_tags(&self, txn: &mut TransactionMut, tags: &[String], now: &str) -> Result<()> {
        self.node.insert(txn, TAGS, ArrayPrelim::from(tags.iter().cloned()));
        self.stamp_modified(txn, now);
        Ok(())
    }

    pub(crate) fn write_custom(
        &self,
        txn: &mut TransactionMut,
        key: &str,
        value: &serde_json::Value,
        now: &str,
    ) -> Result<()> {
        self.put_custom(txn, key, value)?;
        self.stamp_modified(txn, now);
        Ok(())
    }

    /// [`NodeDoc::write_custom`] without the stamp, for a document being made.
    pub(crate) fn put_custom(&self, txn: &mut TransactionMut, key: &str, value: &serde_json::Value) -> Result<()> {
        let json = serde_json::to_string(value).map_err(|e| CrdtError::Serialization(e.to_string()))?;
        self.custom_map(txn).insert(txn, key.to_string(), json);
        Ok(())
    }

    /// One top-level key of the `data` root (see [`NodeDoc::set_data`]).
    pub(crate) fn put_data(&self, txn: &mut TransactionMut, key: &str, value: &serde_json::Value) {
        self.data.insert(txn, key.to_string(), json_to_in(value));
    }

    /// Whether the key was there; nothing is written when it was not.
    pub(crate) fn remove_custom_key(&self, txn: &mut TransactionMut, key: &str, now: &str) -> bool {
        let Some(custom) = self.custom_map_if_present(txn) else { return false };
        if custom.remove(txn, key).is_none() {
            return false;
        }
        self.stamp_modified(txn, now);
        true
    }

    /// The whole `node` root in one transaction: every field, an empty `tags`
    /// array and an empty `custom` map, both timestamps `now`.
    pub(crate) fn write_init(
        &self,
        txn: &mut TransactionMut,
        node_type: &str,
        title: &str,
        parent_id: Option<NodeId>,
        now: &str,
    ) -> Result<()> {
        self.write_init_as(txn, node_type, title, parent_id, now, now, &[])
    }

    /// [`NodeDoc::write_init`] for a node that has a past (a transplant): its
    /// own `created_at`, and its tags written once rather than as an empty
    /// array that is then replaced.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_init_as(
        &self,
        txn: &mut TransactionMut,
        node_type: &str,
        title: &str,
        parent_id: Option<NodeId>,
        created_at: &str,
        modified_at: &str,
        tags: &[String],
    ) -> Result<()> {
        if self.node.len(txn) > 0 {
            return Err(CrdtError::Serialization("node document is already initialised".into()));
        }
        self.node.insert(txn, NODE_TYPE, node_type.to_string());
        self.node.insert(txn, TITLE, title.to_string());
        if let Some(parent) = parent_id {
            self.node.insert(txn, PARENT_ID, parent.to_string());
            self.node.insert(txn, PLACED_UNDER, parent.to_string());
        }
        self.node.insert(txn, CREATED_AT, created_at.to_string());
        self.node.insert(txn, MODIFIED_AT, modified_at.to_string());
        self.node.insert(txn, TAGS, ArrayPrelim::from(tags.iter().cloned()));
        self.node.insert(txn, CUSTOM, MapPrelim::default());
        Ok(())
    }

    pub(crate) fn stamp_modified(&self, txn: &mut TransactionMut, now: &str) {
        self.node.insert(txn, MODIFIED_AT, now.to_string());
    }

    /// A placement: `parent_id = Some(p)` or removed, and `placed_under` with
    /// it (see `PLACED_UNDER`). Each key is written only when it differs: a
    /// rewrite of the same value is a concurrent write that could win over a
    /// real move of the node made elsewhere.
    pub(crate) fn write_parent(&self, txn: &mut TransactionMut, parent_id: Option<NodeId>) {
        self.write_key_if_changed(txn, PARENT_ID, parent_id);
        self.write_key_if_changed(txn, PLACED_UNDER, parent_id);
    }

    /// `parent_id` alone, `placed_under` untouched: the shape of a `parent_id`
    /// written by a client that keeps no rule. Repair's cycle-breaking writes
    /// this way on purpose, so that a list in a share that names the node can
    /// still claim it (a share is where a node broken out of a cycle belongs
    /// more than the root does).
    pub(crate) fn write_parent_only(&self, txn: &mut TransactionMut, parent_id: Option<NodeId>) {
        self.write_key_if_changed(txn, PARENT_ID, parent_id);
    }

    /// Test only: forget the placement, as a document written by a client
    /// from before `placed_under` looks.
    #[cfg(test)]
    pub(crate) fn clear_placement(&self, txn: &mut TransactionMut) {
        if self.node.get(txn, PLACED_UNDER).is_some() {
            self.node.remove(txn, PLACED_UNDER);
        }
    }

    fn write_key_if_changed(&self, txn: &mut TransactionMut, key: &str, value: Option<NodeId>) {
        let current = string_value(self.node.get(txn, key)).and_then(|s| NodeId::parse(&s).ok());
        if current == value {
            return;
        }
        match value {
            Some(id) => {
                self.node.insert(txn, key, id.to_string());
            }
            None => {
                self.node.remove(txn, key);
            }
        }
    }

    pub(crate) fn write_deleted(&self, txn: &mut TransactionMut, at: Option<&str>) {
        match at {
            Some(at) => {
                self.node.insert(txn, DELETED_AT, at.to_string());
            }
            None => {
                self.node.remove(txn, DELETED_AT);
            }
        }
    }

    /// The `custom` map, made if a peer's `node` root somehow lacks one.
    fn custom_map(&self, txn: &mut TransactionMut) -> MapRef {
        match self.custom_map_if_present(txn) {
            Some(map) => map,
            None => self.node.insert(txn, CUSTOM, MapPrelim::default()),
        }
    }

    fn custom_map_if_present<T: ReadTxn>(&self, txn: &T) -> Option<MapRef> {
        match self.node.get(txn, CUSTOM) {
            Some(Out::YMap(map)) => Some(map),
            _ => None,
        }
    }

    pub(crate) fn read_fields<T: ReadTxn>(&self, txn: &T) -> Result<NodeFields> {
        self.require_initialised(txn)?;
        let get = |key: &str| string_value(self.node.get(txn, key));
        let tags = match self.node.get(txn, TAGS) {
            Some(Out::YArray(array)) => array.iter(txn).filter_map(|out| string_value(Some(out))).collect(),
            _ => Vec::new(),
        };
        let mut custom = HashMap::new();
        if let Some(map) = self.custom_map_if_present(txn) {
            for (key, value) in map.iter(txn) {
                if let Some(json) = string_value(Some(value)) {
                    if let Ok(parsed) = serde_json::from_str(&json) {
                        custom.insert(key.to_string(), parsed);
                    }
                }
            }
        }
        Ok(NodeFields {
            node_type: get(NODE_TYPE).unwrap_or_default(),
            title: get(TITLE).unwrap_or_default(),
            parent_id: get(PARENT_ID).and_then(|s| NodeId::parse(&s).ok()),
            placed_under: get(PLACED_UNDER).and_then(|s| NodeId::parse(&s).ok()),
            created_at: get(CREATED_AT).unwrap_or_default(),
            modified_at: get(MODIFIED_AT).unwrap_or_default(),
            deleted_at: get(DELETED_AT),
            tags,
            custom,
        })
    }

    /// The children list exactly as stored, `None` for an entry that is not
    /// a node id (never written by this crate; repair removes it), so index
    /// `i` here is index `i` in the array.
    pub(crate) fn raw_children(&self) -> Vec<Option<NodeId>> {
        let txn = self.doc.transact();
        self.children
            .iter(&txn)
            .map(|out| string_value(Some(out)).and_then(|s| NodeId::parse(&s).ok()))
            .collect()
    }

    /// Insert `id` at `at` (the end when `None` or past it).
    pub(crate) fn list_insert(&self, txn: &mut TransactionMut, at: Option<usize>, id: NodeId) {
        let len = self.children.len(txn);
        let at = at.map(|at| (at.min(u32::MAX as usize) as u32).min(len)).unwrap_or(len);
        self.children.insert(txn, at, id.to_string());
    }

    /// Remove every entry naming `id`; whether there was one.
    pub(crate) fn list_remove_all(&self, txn: &mut TransactionMut, id: NodeId) -> bool {
        let wanted = id.to_string();
        let mut removed = false;
        for index in (0..self.children.len(txn)).rev() {
            if string_value(self.children.get(txn, index)).as_deref() == Some(wanted.as_str()) {
                self.children.remove(txn, index);
                removed = true;
            }
        }
        removed
    }

    pub(crate) fn list_remove_at(&self, txn: &mut TransactionMut, at: usize) -> Result<()> {
        let len = self.children.len(txn) as usize;
        if at >= len {
            return Err(CrdtError::KeyNotFound(format!("children[{at}] (length {len})")));
        }
        self.children.remove(txn, at as u32);
        Ok(())
    }
}

impl Default for NodeDoc {
    fn default() -> Self {
        Self::new()
    }
}

// The server holds node documents across `.await`s, as it held `ContentDoc`
// and `StoreDocument`; the root handles kept here are `Send` only with yrs's
// `sync` feature, which rinch-editor-collab turns on. Fail here, not there.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<NodeDoc>();
};

fn string_value(out: Option<Out>) -> Option<String> {
    match out {
        Some(Out::Any(any)) => String::try_from(any).ok(),
        _ => None,
    }
}

// ── JSON in and out of the `data` root ──────────────────────────────────

/// A JSON value as something to insert: objects and arrays as nested shared
/// types, strings as shared text, scalars as values.
fn json_to_in(value: &serde_json::Value) -> In {
    use serde_json::Value;
    match value {
        Value::Null => In::Any(Any::Null),
        Value::Bool(b) => In::Any(Any::Bool(*b)),
        Value::Number(n) => In::Any(match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Any::BigInt(i),
            (None, Some(f)) => Any::Number(f),
            (None, None) => Any::Null,
        }),
        Value::String(s) => In::from(TextPrelim::new(s.clone())),
        Value::Array(items) => In::Array(items.iter().map(json_to_in).collect()),
        Value::Object(fields) => In::Map(fields.iter().map(|(k, v)| (k.as_str(), json_to_in(v))).collect()),
    }
}

fn map_to_json<T: ReadTxn>(txn: &T, map: &MapRef) -> serde_json::Value {
    serde_json::Value::Object(map.iter(txn).map(|(key, value)| (key.to_string(), out_to_json(txn, value))).collect())
}

fn out_to_json<T: ReadTxn>(txn: &T, out: Out) -> serde_json::Value {
    use serde_json::Value;
    match out {
        Out::Any(any) => any_to_json(&any),
        Out::YText(text) => Value::String(text.get_string(txn)),
        Out::YArray(array) => Value::Array(array.iter(txn).map(|item| out_to_json(txn, item)).collect()),
        Out::YMap(map) => map_to_json(txn, &map),
        // XML types, subdocuments and links are never written here.
        _ => Value::Null,
    }
}

fn any_to_json(any: &Any) -> serde_json::Value {
    use serde_json::Value;
    match any {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::Number(f) => serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
        Any::BigInt(i) => Value::from(*i),
        Any::String(s) => Value::String(s.to_string()),
        Any::Buffer(bytes) => Value::Array(bytes.iter().map(|b| Value::from(*b)).collect()),
        Any::Array(items) => Value::Array(items.iter().map(any_to_json).collect()),
        Any::Map(fields) => Value::Object(fields.iter().map(|(k, v)| (k.clone(), any_to_json(v))).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentDoc;
    use serde_json::json;

    const T0: &str = "2026-09-18T10:00:00Z";
    const T1: &str = "2026-09-18T10:00:01Z";
    const T2: &str = "2026-09-18T10:00:02Z";

    fn id() -> NodeId {
        NodeId::new()
    }

    #[test]
    fn a_new_document_is_uninitialised_and_saves_empty() {
        let doc = NodeDoc::new();
        assert!(!doc.is_initialised());
        assert!(doc.fields().is_err());
        assert_eq!(doc.text(), "");
        assert!(doc.units().is_empty());
        assert!(doc.children().is_empty());
        assert_eq!(doc.data_json(), json!({}));
        // Declaring the roots writes nothing: the bytes are an empty document.
        assert_eq!(doc.save(), ContentDoc::new().save());
        assert_eq!(NodeDoc::text_of(b"garbage"), "");
    }

    #[test]
    fn every_field_round_trips_through_save_and_load() {
        let parent = id();
        let mut doc = NodeDoc::from_plain_text("body").unwrap();
        doc.init("document", "Title", Some(parent), T0).unwrap();
        doc.set_tags(&["a".into(), "b".into()], T1).unwrap();
        doc.set_custom("icon", &json!("star"), T1).unwrap();
        doc.set_custom("nested", &json!({"x": [1, 2, {"y": null}]}), T1).unwrap();
        doc.set_custom("gone", &json!(true), T1).unwrap();
        assert!(doc.remove_custom("gone", T1).unwrap());
        assert!(!doc.remove_custom("gone", T1).unwrap(), "an absent key is reported absent");
        doc.set_node_type("folder", T1).unwrap();
        doc.set_title("Renamed", T1).unwrap();
        doc.set_timestamps("2020-01-01T00:00:00Z", "2020-01-02T00:00:00Z").unwrap();
        doc.set_deleted(Some(T2)).unwrap();
        let child = id();
        doc.insert_child(0, child).unwrap();
        doc.set_data("count", &json!(3)).unwrap();

        let expected = NodeFields {
            node_type: "folder".into(),
            title: "Renamed".into(),
            parent_id: Some(parent),
            placed_under: Some(parent),
            created_at: "2020-01-01T00:00:00Z".into(),
            modified_at: "2020-01-02T00:00:00Z".into(),
            deleted_at: Some(T2.into()),
            tags: vec!["a".into(), "b".into()],
            custom: HashMap::from([
                ("icon".to_string(), json!("star")),
                ("nested".to_string(), json!({"x": [1, 2, {"y": null}]})),
            ]),
        };
        assert_eq!(doc.fields().unwrap(), expected);

        let loaded = NodeDoc::load(&doc.save()).unwrap();
        assert!(loaded.is_initialised());
        assert_eq!(loaded.fields().unwrap(), expected);
        assert_eq!(loaded.children(), vec![child]);
        assert_eq!(loaded.text(), "body");
        assert_eq!(loaded.data_json(), json!({"count": 3}));

        // The tombstone clears, the parent can go, tags replace wholesale.
        let mut loaded = loaded;
        loaded.set_deleted(None).unwrap();
        loaded.set_parent_id(None, T2).unwrap();
        loaded.set_tags(&["c".into()], T2).unwrap();
        let fields = loaded.fields().unwrap();
        assert_eq!(fields.deleted_at, None);
        assert_eq!(fields.parent_id, None);
        assert_eq!(fields.tags, vec!["c".to_string()]);
        assert_eq!(fields.modified_at, T2);
    }

    #[test]
    fn init_twice_is_an_error() {
        let mut doc = NodeDoc::new();
        doc.init("document", "One", None, T0).unwrap();
        let err = doc.init("document", "Two", None, T1).unwrap_err();
        assert!(err.to_string().contains("already initialised"), "{err}");
        assert_eq!(doc.fields().unwrap().title, "One");
    }

    #[test]
    fn setters_stamp_modified_at_and_refuse_an_uninitialised_document() {
        let mut doc = NodeDoc::new();
        assert!(doc.set_title("x", T0).is_err());
        assert!(doc.set_deleted(Some(T0)).is_err());
        assert!(doc.touch_modified(T0).is_err());
        doc.init("document", "x", None, T0).unwrap();
        assert_eq!(doc.fields().unwrap().modified_at, T0);
        doc.set_title("y", T1).unwrap();
        assert_eq!(doc.fields().unwrap().modified_at, T1);
        doc.touch_modified(T2).unwrap();
        assert_eq!(doc.fields().unwrap().modified_at, T2);
    }

    #[test]
    fn children_list_edits() {
        let mut doc = NodeDoc::new();
        let (a, b, c) = (id(), id(), id());
        doc.insert_child(0, a).unwrap();
        doc.insert_child(99, c).unwrap(); // clamped to the end
        doc.insert_child(1, b).unwrap();
        doc.insert_child(3, a).unwrap(); // a duplicate, as a concurrent append could leave
        assert_eq!(doc.children(), vec![a, b, c, a]);
        assert!(doc.remove_child(a).unwrap(), "removes every occurrence");
        assert_eq!(doc.children(), vec![b, c]);
        assert!(!doc.remove_child(a).unwrap());
        doc.remove_child_at(0).unwrap();
        assert_eq!(doc.children(), vec![c]);
        assert!(doc.remove_child_at(1).is_err());
    }

    /// The whole point of per-root effects: the server derives `ContentUpdated`
    /// from `content` and the tree notifications from `structure`.
    #[test]
    fn apply_update_reports_the_roots_an_update_changed() {
        let mut a = NodeDoc::from_plain_text("one").unwrap();
        a.init("document", "Doc", None, T0).unwrap();
        let mut b = NodeDoc::load(&a.save()).unwrap();

        // A content-only edit.
        let delta = a.replace_plain_text("two").unwrap();
        let effect = b.apply_update(&delta).unwrap();
        assert_eq!(effect, NodeUpdateEffect { changed: true, structure: false, content: true, data: false });
        assert_eq!(b.text(), "two");

        // A rename.
        let sv = b.state_vector();
        a.set_title("Renamed", T1).unwrap();
        let effect = b.apply_update(&a.diff_since(&sv).unwrap()).unwrap();
        assert_eq!(effect, NodeUpdateEffect { changed: true, structure: true, content: false, data: false });

        // A child list change.
        let sv = b.state_vector();
        a.insert_child(0, id()).unwrap();
        let effect = b.apply_update(&a.diff_since(&sv).unwrap()).unwrap();
        assert_eq!(effect, NodeUpdateEffect { changed: true, structure: true, content: false, data: false });

        // A plugin's write.
        let sv = b.state_vector();
        a.set_data("k", &json!({"v": "text"})).unwrap();
        let diff = a.diff_since(&sv).unwrap();
        let effect = b.apply_update(&diff).unwrap();
        assert_eq!(effect, NodeUpdateEffect { changed: true, structure: false, content: false, data: true });

        // The same bytes again change nothing and name no root.
        assert_eq!(b.apply_update(&diff).unwrap(), NodeUpdateEffect::default());
        assert_eq!(b.apply_update(&delta).unwrap(), NodeUpdateEffect::default());

        // A deletion only (the tombstone cleared) is a structural change too.
        a.set_deleted(Some(T1)).unwrap();
        let sv = b.state_vector();
        b.apply_update(&a.diff_since(&sv).unwrap()).unwrap();
        let sv = b.state_vector();
        a.set_deleted(None).unwrap();
        let effect = b.apply_update(&a.diff_since(&sv).unwrap()).unwrap();
        assert!(effect.changed && effect.structure && !effect.content && !effect.data, "{effect:?}");
        assert_eq!(b.fields().unwrap().deleted_at, None);
    }

    /// A phase 2a content document is a node document without a `node` root:
    /// the migration loads it, reads the text, and `init`s it.
    #[test]
    fn a_phase_2a_content_document_loads_uninitialised_with_its_text() {
        let content = ContentDoc::from_plain_text("hello\nworld").unwrap();
        let mut doc = NodeDoc::load(&content.save()).unwrap();
        assert!(!doc.is_initialised());
        assert_eq!(doc.text(), "hello\nworld");
        assert_eq!(doc.units().len(), 2);
        assert_eq!(NodeDoc::units_of(&content.save()).len(), 2);
        doc.init("document", "Hello", Some(id()), T0).unwrap();
        assert!(doc.is_initialised());
        assert_eq!(doc.text(), "hello\nworld");
        // And back the other way: the content document reads the same text.
        assert_eq!(ContentDoc::text_of(&doc.save()), "hello\nworld");
    }

    /// The contract's claim, proven against the crate: rinch-editor-collab
    /// tolerates roots it does not own, so the editor joins a node document as
    /// it joined a content document, and Pimble's roots ride along untouched.
    #[test]
    fn a_node_document_still_loads_as_a_rinch_collab_doc() {
        use rinch_editor_collab::{CollabDoc, CollabSession};
        use rinch_editor_core::Schema;

        let mut doc = NodeDoc::from_plain_text("hello\nworld").unwrap();
        doc.init("document", "Hello", Some(id()), T0).unwrap();
        doc.set_tags(&["t".into()], T0).unwrap();
        doc.set_custom("k", &json!(1), T0).unwrap();
        doc.insert_child(0, id()).unwrap();
        doc.set_data("plugin", &json!({"a": [1, "b"]})).unwrap();

        let bytes = doc.save();
        CollabDoc::load(&bytes).expect("CollabDoc::load checks only its own roots");
        let session = CollabSession::from_bytes(&bytes).unwrap();
        let projected = session.projected_doc(&Schema::starter_kit()).unwrap();
        assert_eq!(projected.child_count(), 2);
        assert_eq!(projected.child(0).child(0).text(), Some("hello"));
        assert_eq!(projected.child(1).child(0).text(), Some("world"));

        // The editor's snapshot of the shared document still has everything.
        let back = NodeDoc::load(&session.snapshot()).unwrap();
        assert_eq!(back.fields().unwrap().title, "Hello");
        assert_eq!(back.children().len(), 1);
        assert_eq!(back.data_json(), json!({"plugin": {"a": [1, "b"]}}));
    }

    #[test]
    fn replace_plain_text_seeds_an_initialised_document_and_then_edits_it() {
        let mut doc = NodeDoc::new();
        doc.init("document", "Doc", None, T0).unwrap();
        let mut peer = NodeDoc::load(&doc.save()).unwrap();
        assert_eq!(doc.text(), "");

        // No projection yet: the delta is a fresh snapshot.
        let delta = doc.replace_plain_text("first").unwrap();
        assert!(peer.apply_update(&delta).unwrap().content);
        assert_eq!(doc.text(), "first");
        assert_eq!(peer.text(), "first");
        assert!(doc.is_initialised() && peer.is_initialised());

        // Now an edit of the shared history: peers converge on exactly the new text.
        let delta = doc.replace_plain_text("second\nthird").unwrap();
        peer.apply_update(&delta).unwrap();
        assert_eq!(doc.text(), "second\nthird");
        assert_eq!(peer.text(), "second\nthird");
        assert_eq!(peer.fields().unwrap().title, "Doc");
    }

    #[test]
    fn data_json_round_trips_nested_values() {
        let value = json!({
            "string": "text",
            "int": 42,
            "negative": -7,
            "float": 2.5,
            "yes": true,
            "no": false,
            "nothing": null,
            "list": [1, "two", [3.25, false], {"deep": null}],
            "object": {"a": {"b": {"c": "d"}}, "empty": {}, "none": []},
        });
        let mut doc = NodeDoc::new();
        doc.set_data("v", &value).unwrap();
        assert_eq!(doc.data_json(), json!({"v": value}));
        let loaded = NodeDoc::load(&doc.save()).unwrap();
        assert_eq!(loaded.data_json(), json!({"v": value}));
        // A key is replaced whole.
        let mut loaded = loaded;
        loaded.set_data("v", &json!("flat")).unwrap();
        assert_eq!(loaded.data_json(), json!({"v": "flat"}));
    }

    #[test]
    fn two_documents_setting_different_data_keys_merge_to_both() {
        let mut a = NodeDoc::new();
        a.init("plugin", "P", None, T0).unwrap();
        let mut b = NodeDoc::load(&a.save()).unwrap();
        a.set_data("left", &json!({"x": 1})).unwrap();
        b.set_data("right", &json!(["y"])).unwrap();
        let (a_sv, b_sv) = (a.state_vector(), b.state_vector());
        let to_b = a.diff_since(&b_sv).unwrap();
        let to_a = b.diff_since(&a_sv).unwrap();
        assert!(b.apply_update(&to_b).unwrap().data);
        assert!(a.apply_update(&to_a).unwrap().data);
        let merged = json!({"left": {"x": 1}, "right": ["y"]});
        assert_eq!(a.data_json(), merged);
        assert_eq!(b.data_json(), merged);
    }

    #[test]
    fn diff_if_peer_lacks_it_is_none_once_both_sides_have_exchanged() {
        let mut local = NodeDoc::from_plain_text("hello").unwrap();
        local.init("document", "Doc", None, T0).unwrap();
        let remote = NodeDoc::load(&local.save()).unwrap();
        let remote_diff = remote.diff_since(&local.state_vector()).unwrap();
        assert!(local.diff_if_peer_lacks_it(&remote.state_vector(), &remote_diff).unwrap().is_none());

        // A structural deletion the peer lacks is pushed.
        let mut local = local;
        local.set_deleted(Some(T1)).unwrap();
        local.set_deleted(None).unwrap();
        let remote_diff = remote.diff_since(&local.state_vector()).unwrap();
        assert!(local.diff_if_peer_lacks_it(&remote.state_vector(), &remote_diff).unwrap().is_some());
    }
}
