//! The transactions `Tree` shares with the node document: writes that run
//! inside a caller's transaction (placement, children lists, tombstones,
//! custom keys) and the reads `Tree::repair` judges by.

use std::collections::HashMap;

use pimble_core::metanode::{SOURCE_KEY, VIEWER_KEY};
use pimble_core::{NodeId, PimbleUrl, Place};
use yrs::{Array, ArrayPrelim, Doc, Map, MapPrelim, MapRef, Out, ReadTxn, StateVector, Transact, TransactionMut};

use crate::content_doc::transaction_changed;
use crate::error::{CrdtError, Result};

use super::data::{json_to_in, out_to_in};
use super::{
    string_value, NodeDoc, NodeFields, ATTACHED_TO, BECAME, CREATED_AT, CUSTOM, DELETED_AT, LEFT_PREFIX, MODIFIED_AT,
    NODE_TYPE, PARENT_ID, PLACED_UNDER, ROOT_DATA, TAGS, TITLE,
};

impl NodeDoc {
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

    /// The stored `parent_id` of an initialised document, tombstoned or not.
    pub(crate) fn stored_parent(&self) -> Option<NodeId> {
        let txn = self.doc.transact();
        string_value(self.node.get(&txn, PARENT_ID)).and_then(|s| NodeId::parse(&s).ok())
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

    /// The node this document is an attachment of (see `ATTACHED_TO`), read
    /// on its own; `None` for a tree node and for a document whose `node`
    /// root has not arrived.
    pub fn attached_to(&self) -> Option<NodeId> {
        let txn = self.doc.transact();
        string_value(self.node.get(&txn, ATTACHED_TO)).and_then(|s| NodeId::parse(&s).ok())
    }

    /// Write the `node` root for an attachment of `host`: no `parent_id`, no
    /// `placed_under`, `attached_to` in the same transaction. An error when
    /// already initialised.
    pub fn init_attached(&mut self, node_type: &str, title: &str, host: NodeId, now: &str) -> Result<()> {
        self.edit(|d, txn| d.write_init_attached(txn, node_type, title, host, now)).map(|_| ())
    }

    pub(crate) fn write_init_attached(&self, txn: &mut TransactionMut, node_type: &str, title: &str, host: NodeId, now: &str) -> Result<()> {
        self.write_init(txn, node_type, title, None, now)?;
        self.node.insert(txn, ATTACHED_TO, host.to_string());
        Ok(())
    }

    /// `attached_to` alone, for a document whose `node` root was just written
    /// in this transaction (a planted attachment).
    pub(crate) fn write_init_attached_key(&self, txn: &mut TransactionMut, host: NodeId) {
        self.node.insert(txn, ATTACHED_TO, host.to_string());
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

    /// A text key of the `node` root, for a document being made: a field
    /// (`field:<name>`) or a metanode's viewer or source.
    pub(crate) fn put_node_text(&self, txn: &mut TransactionMut, key: &str, value: &str) {
        self.node.insert(txn, key, value.to_string());
    }

    /// One top-level key of the `data` root (see [`NodeDoc::set_data`]).
    pub(crate) fn put_data(&self, txn: &mut TransactionMut, key: &str, value: &serde_json::Value) {
        self.data.insert(txn, key.to_string(), json_to_in(value));
    }

    /// The `data` root copied into a new document of its own, every value
    /// keeping its yrs type (a map stays a map, text stays text, a value
    /// written whole stays one value, a buffer stays a buffer), with a fresh
    /// history: what a transplant carries (a board, docs/IMAGE_MARKUP_CONTRACT.md
    /// section 2, "Moving between stores", lives in `data` in shapes JSON
    /// would lose). `None` when `data` is empty.
    pub(crate) fn fresh_data(&self) -> Option<Vec<u8>> {
        let txn = self.doc.transact();
        if self.data.len(&txn) == 0 {
            return None;
        }
        let copy = Doc::new();
        let root = copy.get_or_insert_map(ROOT_DATA);
        {
            let mut out = copy.transact_mut();
            for (key, value) in self.data.iter(&txn) {
                root.insert(&mut out, key.to_string(), out_to_in(&txn, value));
            }
        }
        let bytes = copy.transact().encode_state_as_update_v1(&StateVector::default());
        Some(bytes)
    }

    /// Write every key of a [`NodeDoc::fresh_data`] copy into this document's
    /// `data` root, types kept.
    pub(crate) fn put_data_copy(&self, txn: &mut TransactionMut, bytes: &[u8]) -> Result<()> {
        let source = Doc::new();
        let root = source.get_or_insert_map(ROOT_DATA);
        {
            let update = <yrs::Update as yrs::updates::decoder::Decode>::decode_v1(bytes).map_err(|e| CrdtError::Serialization(e.to_string()))?;
            source.transact_mut().apply_update(update).map_err(|e| CrdtError::Serialization(e.to_string()))?;
        }
        let read = source.transact();
        for (key, value) in root.iter(&read) {
            self.data.insert(txn, key.to_string(), out_to_in(&read, value));
        }
        Ok(())
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

    pub(crate) fn write_became(&self, txn: &mut TransactionMut, url: &PimbleUrl) {
        self.node.insert(txn, BECAME, url.to_string());
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
        let fields = self.read_node_fields(txn);
        Ok(NodeFields {
            node_type: get(NODE_TYPE).unwrap_or_default(),
            title: get(TITLE).unwrap_or_default(),
            parent_id: get(PARENT_ID).and_then(|s| NodeId::parse(&s).ok()),
            placed_under: get(PLACED_UNDER).and_then(|s| NodeId::parse(&s).ok()),
            created_at: get(CREATED_AT).unwrap_or_default(),
            modified_at: get(MODIFIED_AT).unwrap_or_default(),
            deleted_at: get(DELETED_AT),
            became: get(BECAME).and_then(|s| PimbleUrl::parse(&s)),
            attached_to: get(ATTACHED_TO).and_then(|s| NodeId::parse(&s).ok()),
            tags,
            text_tags: Vec::new(),
            custom,
            fields,
            metanode_viewer: get(VIEWER_KEY),
            metanode_source: get(SOURCE_KEY),
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

    /// Insert `id` at `place` (docs/TREE_CONTRACT.md "Places on the wire"):
    /// `After(x)` right after `x`'s last entry, the end when the list no
    /// longer names `x`. A caller moving within one list removes `id` first,
    /// so the anchor is found in the list as it will be.
    pub(crate) fn list_place(&self, txn: &mut TransactionMut, place: Place, id: NodeId) {
        let at = match place {
            Place::First => Some(0),
            Place::Last => None,
            Place::After(anchor) => {
                let wanted = anchor.to_string();
                (0..self.children.len(txn))
                    .rev()
                    .find(|&index| string_value(self.children.get(txn, index)).as_deref() == Some(wanted.as_str()))
                    .map(|index| index as usize + 1)
            }
        };
        self.list_insert(txn, at, id);
    }

    /// Record that an operation took `id` out of this list.
    pub(crate) fn note_left(&self, txn: &mut TransactionMut, id: NodeId) {
        self.node.insert(txn, format!("{LEFT_PREFIX}{id}"), true);
    }

    /// An operation put `id` (back) into this list: the record goes.
    pub(crate) fn clear_left(&self, txn: &mut TransactionMut, id: NodeId) {
        let key = format!("{LEFT_PREFIX}{id}");
        if self.node.get(txn, &key).is_some() {
            self.node.remove(txn, &key);
        }
    }

    /// The children an operation took out of this list and none put back.
    pub(crate) fn left_children(&self) -> std::collections::HashSet<NodeId> {
        let txn = self.doc.transact();
        self.node.keys(&txn).filter_map(|k| k.strip_prefix(LEFT_PREFIX).and_then(|id| NodeId::parse(id).ok())).collect()
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
