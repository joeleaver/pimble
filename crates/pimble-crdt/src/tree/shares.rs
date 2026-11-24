use std::collections::{HashMap, HashSet};

use pimble_core::custom_keys::SHARE;
use pimble_core::{NodeId, Place, StoreId};
use pimble_core::metanode::{SOURCE_KEY, VIEWER_KEY};

use crate::error::{CrdtError, Result};
use crate::node_doc::{NodeDoc, FIELD_PREFIX};

use super::{Tree, TreeEdit, Cutting, CuttingNode, Planted};

impl Tree {
    // ── Shares: a node never leaves one (docs/MOVE_CONTRACT.md) ──────────

    /// The shares `id` is in: the documents carrying a share marker
    /// (`custom["share"]`) among `id` and its ancestors, nearest first. Read
    /// off what this device holds, by stored `parent_id`s: the walk ends at
    /// the tree's root, at a document that is not held (what is above it is
    /// unknown here, and "a share it holds nothing of is one it cannot write
    /// outside of either"), or where it meets itself, so an unrepaired cycle
    /// cannot loop. Empty when `id` is not held.
    pub fn shares(&self, id: NodeId) -> Vec<NodeId> {
        self.ancestry(id).shares().collect()
    }

    /// The shares that moving `id` under `new_parent` would take it out of,
    /// nearest first: every share `id` is in that `new_parent` is not in,
    /// not counting `id` itself or anything under it (a share's own root, and
    /// the shares inside the subtree, travel with it). Entering a share is
    /// not leaving one. Judged on held documents only, like [`Tree::shares`].
    pub fn shares_left(&self, id: NodeId, new_parent: NodeId) -> Vec<NodeId> {
        let target = self.ancestry(new_parent);
        self.ancestry(id)
            .shares()
            .filter(|&share| share != id && !target.names(share) && !self.ancestry(share).names(id))
            .collect()
    }

    /// Whether moving `id` under `new_parent` leaves a share
    /// (docs/MOVE_CONTRACT.md "What 'out of a share' means"): such a move is
    /// a [`Tree::transplant`], never a [`Tree::move_node`].
    pub fn leaves_a_share(&self, id: NodeId, new_parent: NodeId) -> bool {
        !self.shares_left(id, new_parent).is_empty()
    }

    /// Move `id` under `new_parent` at `place`, as every caller moves a
    /// node: a plain [`Tree::move_node`] when the move leaves no share, a
    /// [`Tree::transplant`] when it does. Answers the id the node has now:
    /// `id` after a plain move, the new root's after a transplant. `new_id`
    /// is only called for a transplant, and `store` (this tree's store) is
    /// only read for one: it is what the tombstones say the nodes became.
    pub fn move_or_transplant(
        &mut self,
        store: StoreId,
        id: NodeId,
        new_parent: NodeId,
        place: Place,
        now: &str,
        new_id: &mut dyn FnMut() -> NodeId,
    ) -> Result<(NodeId, TreeEdit)> {
        if self.leaves_a_share(id, new_parent) {
            self.transplant(store, id, new_parent, place, now, new_id)
        } else {
            self.move_node(id, new_parent, place, now).map(|edit| (id, edit))
        }
    }

    /// Take `id` out of the shares it is in by making it again under
    /// `new_parent` (docs/MOVE_CONTRACT.md "The operation: transplant"): a
    /// new document with a fresh id for `id` and for every live node under
    /// it ([`Tree::take_cutting`], [`Tree::plant`]), then the original
    /// subtree tombstoned ([`Tree::remove_node`]), which stays in its share
    /// and which any editor of it can undo; the two then exist side by side
    /// and go their own ways. The new nodes are made once and never kept in
    /// step with the old.
    ///
    /// One [`TreeEdit`] whose order is the order things happened in: the new
    /// documents in preorder, the list that names the new root, then the
    /// tombstones and the list the original left. Created first, deleted
    /// second: a failure between the two leaves both, never neither.
    /// Everything that can be refused is refused before anything is written.
    ///
    /// `new_id` is called once per node of the subtree, in the order of
    /// [`Tree::subtree_ids`], so a caller that wants to know which new node
    /// is which old one can pair the two. Every original's tombstone
    /// records the node it became in `store`, this tree's store
    /// ([`Tree::remove_transplanted`]). Answers the new root's id.
    pub fn transplant(
        &mut self,
        store: StoreId,
        id: NodeId,
        new_parent: NodeId,
        place: Place,
        now: &str,
        new_id: &mut dyn FnMut() -> NodeId,
    ) -> Result<(NodeId, TreeEdit)> {
        self.require_node(new_parent)?;
        if new_parent == id || self.is_under(new_parent, id) {
            return Err(CrdtError::Serialization(format!("cannot transplant node {id} under its own descendant {new_parent}")));
        }
        let cutting = self.take_cutting(id)?;
        let (planted, mut edit) = self.plant(cutting, new_parent, place, now, new_id)?;
        edit.touched.extend(self.remove_transplanted(id, store, &planted, now)?.touched);
        Ok((planted.root, edit))
    }

    /// The first half of a transplant between two trees: `id` and its live
    /// subtree (exactly the nodes [`Tree::remove_node`] would tombstone) read
    /// out as data. Per node: its type, title, tags, custom fields without
    /// the share marker (a share is its root document, its key and its
    /// grants; none of that is duplicated), `created_at`, the plugin root as
    /// JSON, and its content as a new yrs document
    /// (`content_doc::fresh_snapshot` says why). A mount node is the
    /// reference it is: its type and its custom fields. Tombstoned
    /// descendants are not read, and neither is a listed document that is
    /// not held. Nothing is changed here.
    ///
    /// The tree's root is refused: it cannot be deleted, so it cannot be
    /// transplanted, and that must be said before anything is planted. An
    /// error when some node's content cannot be projected: better no
    /// transplant than one that loses a note's text and then deletes the note.
    ///
    /// The caller plants it in the other tree and then calls
    /// [`Tree::remove_node`] here, in that order.
    pub fn take_cutting(&self, id: NodeId) -> Result<Cutting> {
        if id == self.root {
            return Err(CrdtError::Serialization("cannot transplant the root node: it cannot be deleted".into()));
        }
        self.require_node(id)?;
        let members = self.collect_subtree(id, &|f| f.deleted_at.is_none());
        let index_of: HashMap<NodeId, usize> = members.iter().enumerate().map(|(index, member)| (*member, index)).collect();
        let mut nodes = Vec::with_capacity(members.len());
        for (index, &member) in members.iter().enumerate() {
            let doc = &self.docs[&member];
            let fields = doc.stored_fields()?;
            let mut custom = fields.custom;
            custom.remove(SHARE);
            // Every member but the first was reached from the node its
            // `parent_id` names (`walk`), which therefore came before it.
            let parent = match index {
                0 => None,
                _ => Some(
                    fields
                        .parent_id
                        .and_then(|parent| index_of.get(&parent).copied())
                        .filter(|&parent| parent < index)
                        .ok_or_else(|| CrdtError::Serialization(format!("node {member} is in the subtree of {id} under no node of it")))?,
                ),
            };
            nodes.push(CuttingNode {
                source_id: member,
                parent,
                attached: false,
                node_type: fields.node_type,
                title: fields.title,
                tags: fields.tags,
                custom,
                fields: fields.fields,
                metanode_viewer: fields.metanode_viewer,
                metanode_source: fields.metanode_source,
                created_at: fields.created_at,
                data: doc.data_json(),
                data_copy: doc.fresh_data(),
                content: doc.fresh_content()?,
            });
        }
        // The attachments of every member travel with it, after the subtree.
        for (index, &member) in members.iter().enumerate() {
            for attachment in self.attachments_of(member) {
                let doc = &self.docs[&attachment];
                let fields = doc.stored_fields()?;
                let mut custom = fields.custom;
                custom.remove(SHARE);
                nodes.push(CuttingNode {
                    source_id: attachment,
                    parent: Some(index),
                    attached: true,
                    node_type: fields.node_type,
                    title: fields.title,
                    tags: fields.tags,
                    custom,
                    fields: fields.fields,
                    metanode_viewer: fields.metanode_viewer,
                    metanode_source: fields.metanode_source,
                    created_at: fields.created_at,
                    data: doc.data_json(),
                    data_copy: doc.fresh_data(),
                    content: doc.fresh_content()?,
                });
            }
        }
        Ok(Cutting { nodes })
    }

    /// The second half of a transplant: make the nodes of `cutting` under
    /// `new_parent` at `place`, in this tree, which
    /// need not be the one the cutting was taken from. A new document per
    /// node with an id from `new_id` (called once per node, in the cutting's
    /// preorder), `created_at` kept, `modified_at` = `now`, no share marker.
    /// The update for each new document is the whole document, since nothing
    /// else holds any of it; the last update lists the new root under
    /// `new_parent`. Every document is built before the tree is touched, so
    /// an error leaves nothing behind. Answers what was made: the new root
    /// and each node's old and new id.
    pub fn plant(
        &mut self,
        cutting: Cutting,
        new_parent: NodeId,
        place: Place,
        now: &str,
        new_id: &mut dyn FnMut() -> NodeId,
    ) -> Result<(Planted, TreeEdit)> {
        self.require_node(new_parent)?;
        if cutting.nodes.is_empty() {
            return Err(CrdtError::Serialization("an empty cutting cannot be planted".into()));
        }
        let ids: Vec<NodeId> = cutting.nodes.iter().map(|_| new_id()).collect();
        let mut distinct = HashSet::new();
        for id in &ids {
            if self.docs.contains_key(id) || !distinct.insert(*id) {
                return Err(CrdtError::Serialization(format!("node {id} is already held")));
            }
        }
        let mut children: Vec<Vec<NodeId>> = vec![Vec::new(); ids.len()];
        for (index, node) in cutting.nodes.iter().enumerate() {
            if let Some(parent) = node.parent.filter(|_| !node.attached) {
                children[parent].push(ids[index]);
            }
        }

        let pairs: Vec<(NodeId, NodeId)> = cutting.nodes.iter().zip(&ids).map(|(node, &new)| (node.source_id, new)).collect();
        let mut built = Vec::with_capacity(ids.len());
        for (index, node) in cutting.nodes.into_iter().enumerate() {
            let doc = match &node.content {
                Some(bytes) => NodeDoc::load(bytes)?,
                None => NodeDoc::new(),
            };
            let parent = node.parent.map(|parent| ids[parent]).unwrap_or(new_parent);
            let created_at = if node.created_at.is_empty() { now } else { node.created_at.as_str() };
            doc.edit(|d, txn| {
                if node.attached {
                    d.write_init_as(txn, &node.node_type, &node.title, None, created_at, now, &node.tags)?;
                    d.write_init_attached_key(txn, parent);
                } else {
                    d.write_init_as(txn, &node.node_type, &node.title, Some(parent), created_at, now, &node.tags)?;
                }
                for (key, value) in node.custom.iter().filter(|(key, _)| key.as_str() != SHARE) {
                    d.put_custom(txn, key, value)?;
                }
                for (name, value) in &node.fields {
                    d.put_node_text(txn, &format!("{FIELD_PREFIX}{name}"), value);
                }
                if let Some(viewer) = &node.metanode_viewer {
                    d.put_node_text(txn, VIEWER_KEY, viewer);
                }
                if let Some(source) = &node.metanode_source {
                    d.put_node_text(txn, SOURCE_KEY, source);
                }
                match &node.data_copy {
                    Some(bytes) => d.put_data_copy(txn, bytes)?,
                    None => {
                        if let serde_json::Value::Object(data) = &node.data {
                            for (key, value) in data {
                                d.put_data(txn, key, value);
                            }
                        }
                    }
                }
                for &child in &children[index] {
                    d.list_insert(txn, None, child);
                }
                Ok(())
            })?;
            built.push(doc);
        }

        let mut edit = TreeEdit::default();
        for (&id, doc) in ids.iter().zip(built) {
            edit.push(id, Some(doc.save()));
            self.docs.insert(id, doc);
        }
        edit.push(new_parent, self.list_edit(new_parent, |d, txn| d.list_place(txn, place, ids[0]))?);
        Ok((Planted { root: ids[0], ids: pairs }, edit))
    }
}
