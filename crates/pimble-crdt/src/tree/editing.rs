use std::collections::HashMap;

use pimble_core::{NodeId, PimbleUrl, Place, StoreId};

use crate::error::{CrdtError, Result};
use crate::node_doc::NodeDoc;

use super::{Tree, TreeEdit, Planted};

impl Tree {
    // ── Editing ──────────────────────────────────────────────────────────

    /// Create `id` under `parent` (the root when `None`) at `place`
    /// (docs/TREE_CONTRACT.md "Places on the wire"). An id already held and initialised is an error;
    /// one held but not initialised (its content arrived first) is
    /// initialised in place. The update for `id` is its `init` transaction.
    pub fn add_node(
        &mut self,
        id: NodeId,
        parent: Option<NodeId>,
        place: Place,
        node_type: &str,
        title: &str,
        now: &str,
    ) -> Result<TreeEdit> {
        let parent = parent.unwrap_or(self.root);
        self.require_node(parent)?;
        if self.docs.get(&id).is_some_and(NodeDoc::is_initialised) {
            return Err(CrdtError::Serialization(format!("node {id} already exists")));
        }
        let mut edit = TreeEdit::default();
        let doc = self.docs.entry(id).or_default();
        let (_, update) = doc.edit(|d, txn| d.write_init(txn, node_type, title, Some(parent), now))?;
        edit.push(id, update);
        edit.push(parent, self.list_edit(parent, |d, txn| d.list_place(txn, place, id))?);
        Ok(edit)
    }

    /// [`Tree::add_node`] for a document that already has content (the importer).
    /// `id` must not be held yet. The update for `id` is the whole document,
    /// since nothing else holds any of it.
    #[allow(clippy::too_many_arguments)]
    pub fn add_node_with_doc(
        &mut self,
        id: NodeId,
        mut doc: NodeDoc,
        parent: Option<NodeId>,
        place: Place,
        node_type: &str,
        title: &str,
        now: &str,
    ) -> Result<TreeEdit> {
        let parent = parent.unwrap_or(self.root);
        self.require_node(parent)?;
        if self.docs.contains_key(&id) {
            return Err(CrdtError::Serialization(format!("node {id} is already held")));
        }
        doc.init(node_type, title, Some(parent), now)?;
        let mut edit = TreeEdit::default();
        edit.push(id, Some(doc.save()));
        self.docs.insert(id, doc);
        edit.push(parent, self.list_edit(parent, |d, txn| d.list_place(txn, place, id))?);
        Ok(edit)
    }

    /// Create `id` as an attachment of `host` (docs/DISCUSSIONS_CONTRACT.md
    /// section 1): its `node` root names the host, and no list changes, so
    /// the host's document is not touched. `content`, when given, is merged
    /// into the new document in the same transaction as its `node` root (a
    /// discussion's containers, made once). An id already held and
    /// initialised is an error.
    pub fn add_attachment(&mut self, id: NodeId, host: NodeId, node_type: &str, title: &str, content: Option<&[u8]>, now: &str) -> Result<TreeEdit> {
        self.require_node(host)?;
        if self.docs.get(&host).is_some_and(|doc| doc.attached_to().is_some()) {
            return Err(CrdtError::Serialization(format!("node {host} is an attachment and cannot hold one")));
        }
        if self.docs.get(&id).is_some_and(NodeDoc::is_initialised) {
            return Err(CrdtError::Serialization(format!("node {id} already exists")));
        }
        let mut edit = TreeEdit::default();
        let doc = self.docs.entry(id).or_default();
        if let Some(bytes) = content {
            doc.apply_update(bytes)?;
        }
        let (_, update) = doc.edit(|d, txn| d.write_init_attached(txn, node_type, title, host, now))?;
        match content {
            // Nothing else holds any of it: the whole document is the update.
            Some(_) => edit.push(id, Some(doc.save())),
            None => edit.push(id, update),
        }
        Ok(edit)
    }

    /// Whether `id` is a held attachment (live or tombstoned).
    pub fn is_attachment(&self, id: NodeId) -> bool {
        self.docs.get(&id).is_some_and(|doc| doc.attached_to().is_some())
    }

    /// The live attachments of `host`, oldest first (then by id): empty when
    /// `host` is not a node, since a deleted host hides its attachments.
    pub fn attachments_of(&self, host: NodeId) -> Vec<NodeId> {
        if !self.has_node(host) {
            return Vec::new();
        }
        let mut out: Vec<(String, NodeId)> = self
            .docs
            .iter()
            .filter(|(_, doc)| doc.attached_to() == Some(host) && doc.is_live())
            .map(|(&id, doc)| (doc.stored_fields().map(|f| f.created_at).unwrap_or_default(), id))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.to_string().cmp(&b.1.to_string())));
        out.into_iter().map(|(_, id)| id).collect()
    }

    /// How many live attachments each live host has (hosts with none are
    /// not named).
    pub fn attachment_counts(&self) -> HashMap<NodeId, usize> {
        let mut counts: HashMap<NodeId, usize> = HashMap::new();
        for doc in self.docs.values() {
            if let Some(host) = doc.attached_to() {
                if doc.is_live() && self.has_node(host) {
                    *counts.entry(host).or_default() += 1;
                }
            }
        }
        counts
    }

    /// Move `id` under `new_parent` at `place` (docs/TREE_CONTRACT.md).
    /// The root cannot move; a move under a node's own descendant is refused.
    pub fn move_node(&mut self, id: NodeId, new_parent: NodeId, place: Place, now: &str) -> Result<TreeEdit> {
        if id == self.root {
            return Err(CrdtError::Serialization("cannot move the root node".into()));
        }
        let fields = self.require_node(id)?;
        self.require_node(new_parent)?;
        if new_parent == id || self.is_under(new_parent, id) {
            return Err(CrdtError::Serialization(format!("cannot move node {id} under its own descendant {new_parent}")));
        }

        let mut edit = TreeEdit::default();
        // The stored parent's list loses the entry; a parent that is not held
        // has no list here. A stale entry anywhere else is repair's to remove.
        let old_parent = fields.parent_id.filter(|p| self.docs.contains_key(p));
        if old_parent == Some(new_parent) {
            // After itself is where it is: after whatever comes before it,
            // since the anchor is looked for once the node has left the list.
            let place = match place {
                Place::After(anchor) if anchor == id => {
                    let list = self.docs[&new_parent].children();
                    match list.iter().position(|&n| n == id) {
                        Some(0) => Place::First,
                        Some(at) => Place::After(list[at - 1]),
                        None => Place::Last,
                    }
                }
                place => place,
            };
            edit.push(new_parent, self.list_edit(new_parent, |d, txn| {
                d.list_remove_all(txn, id);
                d.list_place(txn, place, id);
            })?);
        } else {
            if let Some(old_parent) = old_parent {
                edit.push(old_parent, self.list_edit(old_parent, |d, txn| {
                    d.list_remove_all(txn, id);
                    d.note_left(txn, id);
                })?);
            }
            edit.push(new_parent, self.list_edit(new_parent, |d, txn| {
                d.list_remove_all(txn, id);
                d.list_place(txn, place, id);
                d.clear_left(txn, id);
            })?);
        }
        let (_, update) = self.docs[&id].edit(|d, txn| {
            d.write_parent(txn, Some(new_parent));
            d.stamp_modified(txn, now);
            Ok(())
        })?;
        edit.push(id, update);
        Ok(edit)
    }

    /// Delete `id` and its subtree: tombstones, and `id` leaves its parent's
    /// list. The root cannot be deleted. Every node of the subtree gets the
    /// same `now`, which is how [`Tree::undelete_node`] knows what one
    /// deletion covered.
    pub fn remove_node(&mut self, id: NodeId, now: &str) -> Result<TreeEdit> {
        if id == self.root {
            return Err(CrdtError::Serialization("cannot delete the root node".into()));
        }
        self.tombstone(id, now, &HashMap::new())
    }

    /// The second half of a transplant, after [`Tree::plant`] made `planted`
    /// in `store` (this tree's or another): [`Tree::remove_node`], and in the
    /// same transaction as each original's tombstone, the node it became
    /// (`became`, docs/LINKS_CONTRACT.md), so a link to the original follows
    /// it. Nothing of the new place but its ids is written.
    pub fn remove_transplanted(&mut self, id: NodeId, store: StoreId, planted: &Planted, now: &str) -> Result<TreeEdit> {
        if id == self.root {
            return Err(CrdtError::Serialization("cannot delete the root node".into()));
        }
        let became: HashMap<NodeId, PimbleUrl> =
            planted.ids.iter().map(|&(old, new)| (old, PimbleUrl::node(store, new))).collect();
        self.tombstone(id, now, &became)
    }

    pub(super) fn tombstone(&mut self, id: NodeId, now: &str, became: &HashMap<NodeId, PimbleUrl>) -> Result<TreeEdit> {
        let fields = self.require_node(id)?;
        let mut edit = TreeEdit::default();
        for member in self.collect_subtree(id, &|f| f.deleted_at.is_none()) {
            let (_, update) = self.docs[&member].edit(|d, txn| {
                d.write_deleted(txn, Some(now));
                if let Some(url) = became.get(&member) {
                    d.write_became(txn, url);
                }
                Ok(())
            })?;
            edit.push(member, update);
        }
        if let Some(parent) = fields.parent_id.filter(|p| self.docs.contains_key(p)) {
            edit.push(parent, self.list_edit(parent, |d, txn| {
                d.list_remove_all(txn, id);
                d.note_left(txn, id);
            })?);
        }
        Ok(edit)
    }

    /// Clear the tombstones of `id` and its subtree and put `id` back in its
    /// parent's list (the root when the parent is gone). The subtree is what
    /// the same deletion tombstoned: descendants carrying `id`'s own
    /// `deleted_at`. One deleted earlier, with its own stamp, stays deleted.
    pub fn undelete_node(&mut self, id: NodeId, now: &str) -> Result<TreeEdit> {
        let fields = self.fields_of(id).ok_or_else(|| CrdtError::KeyNotFound(format!("node {id}")))?;
        let Some(stamp) = fields.deleted_at.clone() else {
            return Err(CrdtError::Serialization(format!("node {id} is not deleted")));
        };
        // An attachment has no place in a list: only its tombstone goes.
        if fields.attached_to.is_some() {
            let (_, update) = self.docs[&id].edit(|d, txn| {
                d.write_deleted(txn, None);
                d.stamp_modified(txn, now);
                Ok(())
            })?;
            let mut edit = TreeEdit::default();
            edit.push(id, update);
            return Ok(edit);
        }
        let parent = fields.parent_id.filter(|&p| self.has_node(p)).unwrap_or(self.root);

        let mut edit = TreeEdit::default();
        // Going under the root instead of a parent that is gone is a move:
        // the old parent's list (a tombstone's, usually) loses the entry, or a
        // later undelete of that parent would show the node in two places.
        if let Some(old_parent) = fields.parent_id.filter(|&p| p != parent && self.docs.contains_key(&p)) {
            edit.push(old_parent, self.list_edit(old_parent, |d, txn| {
                d.list_remove_all(txn, id);
                d.note_left(txn, id);
            })?);
        }
        for member in self.collect_subtree(id, &|f| f.deleted_at.as_deref() == Some(stamp.as_str())) {
            let (_, update) = self.docs[&member].edit(|d, txn| {
                d.write_deleted(txn, None);
                if member == id {
                    d.write_parent(txn, Some(parent));
                    d.stamp_modified(txn, now);
                }
                Ok(())
            })?;
            edit.push(member, update);
        }
        // At the end of the list, once: an entry a repair never got to remove
        // goes too, so the node is not listed twice.
        edit.push(parent, self.list_edit(parent, |d, txn| {
            d.list_remove_all(txn, id);
            d.list_insert(txn, None, id);
            d.clear_left(txn, id);
        })?);
        Ok(edit)
    }

    pub fn set_title(&mut self, id: NodeId, title: &str, now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| d.write_title(txn, title, now))
    }

    pub fn set_tags(&mut self, id: NodeId, tags: &[String], now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| d.write_tags(txn, tags, now))
    }

    /// Set a node's tags to `wanted` as [`NodeDoc::set_all_tags`] does: the
    /// entered tags written, each #hashtag of the text left out removed from
    /// the text (docs/HASHTAGS_CONTRACT.md).
    pub fn set_all_tags(&mut self, id: NodeId, wanted: &[String], now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| d.write_all_tags(txn, wanted, now))
    }

    pub fn set_custom(&mut self, id: NodeId, key: &str, value: &serde_json::Value, now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| d.write_custom(txn, key, value, now))
    }

    /// Empty when the key was not there.
    pub fn remove_custom(&mut self, id: NodeId, key: &str, now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| {
            d.remove_custom_key(txn, key, now);
            Ok(())
        })
    }

    pub fn set_node_type(&mut self, id: NodeId, node_type: &str, now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| d.write_node_type(txn, node_type, now))
    }

    pub fn touch_modified(&mut self, id: NodeId, now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| {
            d.stamp_modified(txn, now);
            Ok(())
        })
    }
}
