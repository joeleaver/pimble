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
//!
//! A *node* is a held document that is initialised and not tombstoned. Only
//! nodes are in the tree: a tombstoned document is never listed, appended to
//! a list or read as a parent, and neither is a content-only document (one
//! whose `node` root has not arrived yet). Tombstoned documents keep their
//! children lists as they were, so an undelete finds the subtree again.
//!
//! A node never leaves a share (docs/MOVE_CONTRACT.md). [`Tree::shares`] and
//! [`Tree::leaves_a_share`] say what a move would do, [`Tree::move_or_transplant`]
//! is what every caller moves a node with (a plain [`Tree::move_node`] inside a
//! share or outside all of them, a [`Tree::transplant`] when the move would
//! take the node out of one: new documents where it lands, a tombstone where
//! it was), and repair does not complete a move the operations would not have
//! made (`analyze`, "the list wins").

use std::collections::{BTreeMap, HashMap, HashSet};

use pimble_core::custom_keys::SHARE;
use pimble_core::NodeId;

use crate::error::{CrdtError, Result};
use crate::node_doc::{NodeDoc, NodeFields, NodeUpdateEffect};
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

    fn push(&mut self, id: NodeId, update: Option<Vec<u8>>) {
        if let Some(update) = update {
            self.touched.push((id, update));
        }
    }
}

/// A subtree read out of a tree as data ([`Tree::take_cutting`]), to be planted
/// once, in the same tree or another ([`Tree::plant`]): the two halves of a
/// transplant between two stores (docs/MOVE_CONTRACT.md "The operation:
/// transplant"). It holds nothing of the documents it was read from but what
/// they say now: no yrs history, no share marker, no ids but as a record of
/// where each node came from. Planting consumes it, and it cannot be cloned:
/// planted twice, the two plantings' content would be one CRDT history under
/// two ids.
pub struct Cutting {
    nodes: Vec<CuttingNode>,
}

/// One node of a [`Cutting`].
#[non_exhaustive]
pub struct CuttingNode {
    /// The id the node had where it was read. The planted node gets a new one.
    pub source_id: NodeId,
    /// Its parent's index in [`Cutting::nodes`]; `None` for the first, the
    /// subtree's root.
    pub parent: Option<usize>,
    pub node_type: String,
    pub title: String,
    pub tags: Vec<String>,
    /// Every custom field but the share marker.
    pub custom: HashMap<String, serde_json::Value>,
    pub created_at: String,
    /// The plugin root as JSON (`NodeDoc::data_json`).
    pub data: serde_json::Value,
    /// The content as a new document's bytes (`NodeDoc::fresh_content`);
    /// `None` when the node has no projection.
    content: Option<Vec<u8>>,
}

impl Cutting {
    /// The nodes in preorder: the subtree's root first, every node after its
    /// parent, siblings in their list's order.
    pub fn nodes(&self) -> &[CuttingNode] {
        &self.nodes
    }

    /// How many nodes it holds (at least one).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl CuttingNode {
    pub fn has_content(&self) -> bool {
        self.content.is_some()
    }
}

// A server carries a cutting from one store's lock to another's, across an
// `.await`. Fail here, not there.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Cutting>();
};

/// A walk up the tree from a document by stored `parent_id`s, through held,
/// initialised documents, tombstoned or not (a deleted document stays where it
/// was, and with its share).
struct Ancestry {
    /// The document the walk started at, then its ancestors, nearest first;
    /// with each, whether it carries a share marker.
    chain: Vec<(NodeId, bool)>,
    /// Whether the walk ended knowing everything above its start: at the
    /// tree's root, at a document with no parent, or where it met itself (a
    /// cycle, every member of it held). `false` when it ended at a document
    /// this device does not hold, or holds without its `node` root: what is
    /// above that is unknown here.
    complete: bool,
}

impl Ancestry {
    fn names(&self, id: NodeId) -> bool {
        self.chain.iter().any(|(member, _)| *member == id)
    }

    fn shares(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.chain.iter().filter(|(_, carries)| *carries).map(|(id, _)| *id)
    }
}

/// What the lists that name a node say against where its `parent_id` would
/// put it (`Tree::share_claim`).
enum ShareClaim {
    /// No list naming it is in a share the destination is outside of.
    None,
    /// This list is, and the destination is known to be outside: the list wins.
    Wins(NodeId),
    /// A list is in a share the destination is not known to be in, and what
    /// is above the destination is not all held: not judged.
    Unknown,
}

/// A store's node documents.
pub struct Tree {
    root: NodeId,
    docs: HashMap<NodeId, NodeDoc>,
}

/// What one document gets from a repair: the node, its new `parent_id` if
/// any, the list indices to remove (descending) and the ids to append.
type DocRepair = (NodeId, Option<NodeId>, Vec<usize>, Vec<NodeId>);

/// Read-only analysis of the tree's current shape, shared by
/// [`Tree::validate_tree`] and [`Tree::repair`] so the two can never drift
/// apart: `validate_tree` reports exactly the issues, `repair` applies exactly
/// the fixes computed alongside them.
struct TreeAnalysis {
    issues: Vec<TreeIssue>,
    /// `(node, new_parent)` pairs whose stored `parent_id` needs to change
    /// (decision 9 step 3).
    parent_rewrites: Vec<(NodeId, NodeId)>,
    /// Per-owner children-list surgery (decision 9 steps 4-5): `remove_indices`
    /// are indices into the *current* stored array, in descending order (so
    /// removing one never shifts one still to be removed); `appends` go at the
    /// end. Entries not named here are left alone: a wholesale rebuild would
    /// make fresh copies of the correct entries too, and two replicas
    /// rebuilding the same list would then hold two copies of each once
    /// exchanged.
    list_rewrites: Vec<(NodeId, Vec<usize>, Vec<NodeId>)>,
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
    pub fn apply_update(&mut self, id: NodeId, update: &[u8]) -> Result<NodeUpdateEffect> {
        self.docs.entry(id).or_default().apply_update(update)
    }

    // ── Reading ──────────────────────────────────────────────────────────

    /// Held, initialised and not deleted.
    pub fn has_node(&self, id: NodeId) -> bool {
        self.docs.get(&id).is_some_and(NodeDoc::is_live)
    }

    /// Every undeleted node.
    pub fn list_node_ids(&self) -> Vec<NodeId> {
        self.docs.keys().copied().filter(|&id| self.has_node(id)).collect()
    }

    pub fn get_node_info(&self, id: NodeId) -> Result<NodeInfo> {
        let f = self.require_node(id)?;
        Ok(NodeInfo {
            id,
            parent_id: f.parent_id,
            node_type: f.node_type,
            title: f.title,
            tags: f.tags,
            custom: f.custom,
            created_at: f.created_at,
            modified_at: f.modified_at,
        })
    }

    /// The undeleted, held children of `id` in stored order, each once: the
    /// stored list with every entry that is not a node dropped and a repeat
    /// collapsed to its first occurrence, which is what repair keeps of it.
    /// What repair would add (a node listed nowhere yet) or move (a node
    /// listed here whose `parent_id` says elsewhere) is not settled until it
    /// runs; between a merge and its repair a node can show in two lists or
    /// none (docs/NODE_DOCUMENT_CONTRACT.md section 2).
    pub fn get_children(&self, id: NodeId) -> Result<Vec<NodeId>> {
        self.require_node(id)?;
        let mut seen = HashSet::new();
        Ok(self.docs[&id]
            .children()
            .into_iter()
            .filter(|&child| child != id && self.has_node(child) && seen.insert(child))
            .collect())
    }

    /// `root` and everything under it, preorder, undeleted; a node counts as
    /// a child where its `parent_id` and the parent's list agree.
    pub fn subtree_ids(&self, root: NodeId) -> Result<Vec<NodeId>> {
        self.require_node(root)?;
        Ok(self.collect_subtree(root, &|f| f.deleted_at.is_none()))
    }

    // ── Editing ──────────────────────────────────────────────────────────

    /// Create `id` under `parent` (the root when `None`) at `position`
    /// (the end when `None`). An id already held and initialised is an error;
    /// one held but not initialised (its content arrived first) is
    /// initialised in place. The update for `id` is its `init` transaction.
    pub fn add_node(
        &mut self,
        id: NodeId,
        parent: Option<NodeId>,
        position: Option<usize>,
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
        edit.push(parent, self.list_edit(parent, |d, txn| d.list_insert(txn, position, id))?);
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
        position: Option<usize>,
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
        edit.push(parent, self.list_edit(parent, |d, txn| d.list_insert(txn, position, id))?);
        Ok(edit)
    }

    /// Move `id` under `new_parent` at `position` (the end when `None`).
    /// The root cannot move; a move under a node's own descendant is refused.
    pub fn move_node(&mut self, id: NodeId, new_parent: NodeId, position: Option<usize>, now: &str) -> Result<TreeEdit> {
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
            edit.push(new_parent, self.list_edit(new_parent, |d, txn| {
                d.list_remove_all(txn, id);
                d.list_insert(txn, position, id);
            })?);
        } else {
            if let Some(old_parent) = old_parent {
                edit.push(old_parent, self.list_edit(old_parent, |d, txn| {
                    d.list_remove_all(txn, id);
                })?);
            }
            edit.push(new_parent, self.list_edit(new_parent, |d, txn| {
                d.list_remove_all(txn, id);
                d.list_insert(txn, position, id);
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
        let fields = self.require_node(id)?;
        let mut edit = TreeEdit::default();
        for member in self.collect_subtree(id, &|f| f.deleted_at.is_none()) {
            let (_, update) = self.docs[&member].edit(|d, txn| {
                d.write_deleted(txn, Some(now));
                Ok(())
            })?;
            edit.push(member, update);
        }
        if let Some(parent) = fields.parent_id.filter(|p| self.docs.contains_key(p)) {
            edit.push(parent, self.list_edit(parent, |d, txn| {
                d.list_remove_all(txn, id);
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
        let parent = fields.parent_id.filter(|&p| self.has_node(p)).unwrap_or(self.root);

        let mut edit = TreeEdit::default();
        // Going under the root instead of a parent that is gone is a move:
        // the old parent's list (a tombstone's, usually) loses the entry, or a
        // later undelete of that parent would show the node in two places.
        if let Some(old_parent) = fields.parent_id.filter(|&p| p != parent && self.docs.contains_key(&p)) {
            edit.push(old_parent, self.list_edit(old_parent, |d, txn| {
                d.list_remove_all(txn, id);
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
        })?);
        Ok(edit)
    }

    pub fn set_title(&mut self, id: NodeId, title: &str, now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| d.write_title(txn, title, now))
    }

    pub fn set_tags(&mut self, id: NodeId, tags: &[String], now: &str) -> Result<TreeEdit> {
        self.node_edit(id, |d, txn| d.write_tags(txn, tags, now))
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

    /// Move `id` under `new_parent` at `position`, as every caller moves a
    /// node: a plain [`Tree::move_node`] when the move leaves no share, a
    /// [`Tree::transplant`] when it does. Answers the id the node has now:
    /// `id` after a plain move, the new root's after a transplant. `new_id`
    /// is only called for a transplant.
    pub fn move_or_transplant(
        &mut self,
        id: NodeId,
        new_parent: NodeId,
        position: Option<usize>,
        now: &str,
        new_id: &mut dyn FnMut() -> NodeId,
    ) -> Result<(NodeId, TreeEdit)> {
        if self.leaves_a_share(id, new_parent) {
            self.transplant(id, new_parent, position, now, new_id)
        } else {
            self.move_node(id, new_parent, position, now).map(|edit| (id, edit))
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
    /// is which old one can pair the two. Answers the new root's id.
    pub fn transplant(
        &mut self,
        id: NodeId,
        new_parent: NodeId,
        position: Option<usize>,
        now: &str,
        new_id: &mut dyn FnMut() -> NodeId,
    ) -> Result<(NodeId, TreeEdit)> {
        self.require_node(new_parent)?;
        if new_parent == id || self.is_under(new_parent, id) {
            return Err(CrdtError::Serialization(format!("cannot transplant node {id} under its own descendant {new_parent}")));
        }
        let cutting = self.take_cutting(id)?;
        let (new_root, mut edit) = self.plant(cutting, new_parent, position, now, new_id)?;
        edit.touched.extend(self.remove_node(id, now)?.touched);
        Ok((new_root, edit))
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
            let fields = doc.fields()?;
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
                node_type: fields.node_type,
                title: fields.title,
                tags: fields.tags,
                custom,
                created_at: fields.created_at,
                data: doc.data_json(),
                content: doc.fresh_content()?,
            });
        }
        Ok(Cutting { nodes })
    }

    /// The second half of a transplant: make the nodes of `cutting` under
    /// `new_parent` at `position` (the end when `None`), in this tree, which
    /// need not be the one the cutting was taken from. A new document per
    /// node with an id from `new_id` (called once per node, in the cutting's
    /// preorder), `created_at` kept, `modified_at` = `now`, no share marker.
    /// The update for each new document is the whole document, since nothing
    /// else holds any of it; the last update lists the new root under
    /// `new_parent`. Every document is built before the tree is touched, so
    /// an error leaves nothing behind. Answers the new root's id.
    pub fn plant(
        &mut self,
        cutting: Cutting,
        new_parent: NodeId,
        position: Option<usize>,
        now: &str,
        new_id: &mut dyn FnMut() -> NodeId,
    ) -> Result<(NodeId, TreeEdit)> {
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
            if let Some(parent) = node.parent {
                children[parent].push(ids[index]);
            }
        }

        let mut built = Vec::with_capacity(ids.len());
        for (index, node) in cutting.nodes.into_iter().enumerate() {
            let doc = match &node.content {
                Some(bytes) => NodeDoc::load(bytes)?,
                None => NodeDoc::new(),
            };
            let parent = node.parent.map(|parent| ids[parent]).unwrap_or(new_parent);
            let created_at = if node.created_at.is_empty() { now } else { node.created_at.as_str() };
            doc.edit(|d, txn| {
                d.write_init_as(txn, &node.node_type, &node.title, Some(parent), created_at, now, &node.tags)?;
                for (key, value) in node.custom.iter().filter(|(key, _)| key.as_str() != SHARE) {
                    d.put_custom(txn, key, value)?;
                }
                if let serde_json::Value::Object(data) = &node.data {
                    for (key, value) in data {
                        d.put_data(txn, key, value);
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
        edit.push(new_parent, self.list_edit(new_parent, |d, txn| d.list_insert(txn, position, ids[0]))?);
        Ok((ids[0], edit))
    }

    // ── Shape ────────────────────────────────────────────────────────────

    /// Exactly the conditions [`Tree::repair`] fixes; empty after a repair.
    /// `OrphanNode` and `MissingChild` name a document this device KNOWS is
    /// deleted (a tombstone it holds). One it does not hold, or holds without
    /// its `node` root, is unknown here and is never reported or "fixed":
    /// see `analyze`.
    pub fn validate_tree(&self) -> Vec<TreeIssue> {
        self.analyze().issues
    }

    /// Decision 9 over documents: effective parents (a node's `parent_id`,
    /// except that a list which still names it wins over a `parent_id` that
    /// would take it out of a share: docs/MOVE_CONTRACT.md "Repair"), cycles
    /// broken at the smallest id, every list made to hold the undeleted nodes
    /// whose effective parent is its owner (first occurrence kept, missing
    /// ones appended in id order), tombstones and duplicates taken out, and
    /// every entry naming a document not held here left exactly where it is.
    /// Deterministic in the held state, and acting on knowledge only, so two
    /// devices that hold different subsets never undo each other. `None`
    /// when nothing needed fixing.
    ///
    /// Touches only `parent_id` and children lists, never a timestamp: the
    /// clock rule of the share mirror served a causal guard that no longer
    /// exists (docs/NODE_DOCUMENT_CONTRACT.md section 2), so `now` is unused.
    pub fn repair(&mut self, _now: &str) -> Result<Option<TreeEdit>> {
        let analysis = self.analyze();
        if analysis.parent_rewrites.is_empty() && analysis.list_rewrites.is_empty() {
            return Ok(None);
        }
        // One transaction per document, whatever it needs: its `parent_id`,
        // its list, or both. Ordered by id so two replicas produce the same
        // edit in the same order.
        let mut per_doc: BTreeMap<String, DocRepair> = BTreeMap::new();
        for (id, parent) in analysis.parent_rewrites {
            per_doc.entry(id.to_string()).or_insert((id, None, Vec::new(), Vec::new())).1 = Some(parent);
        }
        for (owner, removals, appends) in analysis.list_rewrites {
            let entry = per_doc.entry(owner.to_string()).or_insert((owner, None, Vec::new(), Vec::new()));
            entry.2 = removals;
            entry.3 = appends;
        }
        let mut edit = TreeEdit::default();
        for (id, parent, removals, appends) in per_doc.into_values() {
            let (_, update) = self.docs[&id].edit(|d, txn| {
                if let Some(parent) = parent {
                    d.write_parent(txn, Some(parent));
                }
                for index in removals {
                    d.list_remove_at(txn, index)?;
                }
                for child in appends {
                    d.list_insert(txn, None, child);
                }
                Ok(())
            })?;
            edit.push(id, update);
        }
        Ok((!edit.is_empty()).then_some(edit))
    }

    /// Read-only pass computing effective parents (after cycle resolution),
    /// every `TreeIssue` they and the children lists reveal, and exactly the
    /// writes `repair` would make. A tree whose root is not a node yet (a
    /// fresh replica before its first reconcile) analyzes as empty.
    fn analyze(&self) -> TreeAnalysis {
        let mut analysis = TreeAnalysis { issues: Vec::new(), parent_rewrites: Vec::new(), list_rewrites: Vec::new() };
        let root = self.root;
        if !self.has_node(root) {
            return analysis;
        }

        // The nodes with their stored parents, in id order so every list
        // below is deterministic.
        let mut nodes: Vec<(NodeId, Option<NodeId>)> = self
            .docs
            .iter()
            .filter_map(|(&id, doc)| doc.placement().map(|parent| (id, parent)))
            .collect();
        nodes.sort_by_key(|(id, _)| id.to_string());
        let node_ids: Vec<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
        let node_set: HashSet<NodeId> = node_ids.iter().copied().collect();
        // What this device KNOWS is deleted. Everything else that is not a
        // node (a document not held, or held and not initialised) is unknown
        // here, which is not the same as missing: node documents travel one
        // by one, a list can name a child before the child's document is
        // here, and a device may hold a document it has no key for yet (a
        // member's new document on an owner's page, until the share's key or
        // the store key's wrap reaches it). Repair acts on knowledge only. A
        // device that "fixed" what it merely lacks would be undone by every
        // device that holds it, for ever, and would move or unlist other
        // people's documents meanwhile (found 2026-09-21: an owner's page
        // unlisting a member's new note six times a second).
        let gone: HashSet<NodeId> = self.docs.iter().filter(|(_, doc)| doc.is_tombstone()).map(|(&id, _)| id).collect();

        // Every node's list as stored, read once, and from them the nodes
        // whose list names each id (in id order, each once): what step 1
        // needs to know before it believes a `parent_id`.
        let lists: HashMap<NodeId, Vec<Option<NodeId>>> = node_ids.iter().map(|&owner| (owner, self.docs[&owner].raw_children())).collect();
        let mut listed_by: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for &owner in &node_ids {
            for &child in lists[&owner].iter().flatten() {
                let owners = listed_by.entry(child).or_default();
                if owners.last() != Some(&owner) {
                    owners.push(owner);
                }
            }
        }

        // Step 1: effective parent for every node but the root.
        let mut raw_parent: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut effective_parent: HashMap<NodeId, NodeId> = HashMap::new();
        for &(id, parent) in &nodes {
            if id == root {
                continue;
            }
            raw_parent.insert(id, parent);
            // Where its `parent_id` puts the node, with the issue that says
            // so when that is not simply "under the node it names".
            let (destination, issue) = match parent {
                None => (root, Some(TreeIssue::DetachedNode { node_id: id })),
                Some(p) if node_set.contains(&p) && p != id => (p, None),
                // Its own parent, or under a document known to be deleted.
                Some(p) if p == id || gone.contains(&p) => (root, Some(TreeIssue::OrphanNode { node_id: id, missing_parent: p })),
                // Under a document this device does not hold: where it
                // belongs is not known here, so it is left exactly where it
                // says it is (no effective parent, no rewrite, no listing).
                Some(_) => continue,
            };
            // The one exception to "`parent_id` is the truth of where a node
            // is" (docs/MOVE_CONTRACT.md "Repair"): a list that still names
            // a node wins over a `parent_id` that would take it out of a
            // share. No operation makes such a move (one that leaves a share
            // is a transplant: the node stays and is tombstoned), so a
            // `parent_id` that says so was written by a client that does not
            // keep the rule, and completing its move would put a member's
            // node in the owner's private tree, out of every member's reach.
            // Every device that holds the list, the node, the marker and
            // what is above the destination decides the same, from the same
            // documents; one that does not hold all of the last decides
            // nothing (below), which is the 2026-09-21 rule again.
            let lists_naming = listed_by.get(&id).map(Vec::as_slice).unwrap_or_default();
            let e = match self.share_claim(id, destination, lists_naming) {
                ShareClaim::Wins(list) => {
                    analysis.issues.push(TreeIssue::LeavesShare { parent_id: list, child_id: id, stored_parent: parent });
                    list
                }
                // Whether the destination is outside the share is not known
                // here: some document above it is not held. Believing the
                // `parent_id` would unlist the node from the share, which
                // no later repair puts right; so nothing is decided until
                // the documents are here, as for a parent that is not held.
                ShareClaim::Unknown => continue,
                ShareClaim::None => {
                    analysis.issues.extend(issue);
                    destination
                }
            };
            effective_parent.insert(id, e);
        }

        // Step 2: break every cycle in the effective-parent graph by
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
                // A chain that ends at a node whose parent is not held ends:
                // it is no cycle.
                match effective_parent.get(&cur) {
                    Some(&next) => cur = next,
                    None => {
                        reaches_root = true;
                        break;
                    }
                }
            }
            if reaches_root {
                continue;
            }
            // `start`'s chain loops without reaching the root. Walk it again,
            // recording the path, to find exactly the cycle's members (a walk
            // from a node outside the cycle first crosses a tail that needs
            // no change of its own once the cycle it feeds is broken).
            let mut path = Vec::new();
            let mut index_of: HashMap<NodeId, usize> = HashMap::new();
            let mut cur = start;
            loop {
                if let Some(&i) = index_of.get(&cur) {
                    let members: Vec<NodeId> = path[i..].to_vec();
                    let smallest = members
                        .iter()
                        .min_by_key(|id| id.to_string())
                        .copied()
                        .expect("a cycle has at least one member");
                    effective_parent.insert(smallest, root);
                    analysis.issues.push(TreeIssue::Cycle { node_ids: members });
                    break;
                }
                index_of.insert(cur, path.len());
                path.push(cur);
                visited.insert(cur);
                // Every member of a chain that did not end has one (the walk
                // above would have ended at the first that does not).
                let Some(&next) = effective_parent.get(&cur) else { break };
                cur = next;
            }
        }

        // Step 3: `parent_id` rewrites wherever the stored value differs from
        // the (possibly cycle-corrected) effective parent.
        for &id in &node_ids {
            if id == root {
                continue;
            }
            let Some(&e) = effective_parent.get(&id) else { continue };
            if raw_parent[&id] != Some(e) {
                analysis.parent_rewrites.push((id, e));
            }
        }

        // Steps 4-5: surgery on every node's list. Only nodes' lists: a
        // tombstoned document keeps its list for an undelete, and a
        // content-only document has nothing to keep in order.
        let mut remove_indices: HashMap<NodeId, Vec<usize>> = HashMap::new();
        let mut placed: HashSet<NodeId> = HashSet::new();
        for &owner in &node_ids {
            let mut seen: HashSet<NodeId> = HashSet::new();
            for (index, &entry) in lists[&owner].iter().enumerate() {
                let Some(child) = entry else {
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                };
                if child == root {
                    analysis.issues.push(TreeIssue::RootInList { parent_id: owner });
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                }
                if gone.contains(&child) {
                    analysis.issues.push(TreeIssue::MissingChild { parent_id: owner, child_id: child });
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                }
                if !seen.insert(child) {
                    analysis.issues.push(TreeIssue::DuplicateInList { parent_id: owner, child_id: child });
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                }
                // Not held here, or a node whose own parent is not held:
                // unknown, so the entry stays where it is.
                let Some(&e) = effective_parent.get(&child) else { continue };
                if e != owner {
                    analysis.issues.push(TreeIssue::WrongList { parent_id: owner, child_id: child, effective_parent: e });
                    remove_indices.entry(owner).or_default().push(index);
                    continue;
                }
                placed.insert(child);
            }
        }

        let mut appends: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for &child in node_ids.iter().filter(|&&id| id != root && !placed.contains(&id)) {
            let Some(&owner) = effective_parent.get(&child) else { continue };
            analysis.issues.push(TreeIssue::MissingFromList { parent_id: owner, child_id: child });
            appends.entry(owner).or_default().push(child);
        }

        for &owner in &node_ids {
            let mut removals = remove_indices.remove(&owner).unwrap_or_default();
            let owner_appends = appends.remove(&owner).unwrap_or_default();
            if removals.is_empty() && owner_appends.is_empty() {
                continue;
            }
            removals.sort_unstable_by(|a, b| b.cmp(a));
            analysis.list_rewrites.push((owner, removals, owner_appends));
        }

        analysis
    }

    // ── Private helpers ──────────────────────────────────────────────────

    /// The fields of a held, initialised document (tombstoned or not).
    fn fields_of(&self, id: NodeId) -> Option<NodeFields> {
        self.docs.get(&id).and_then(|doc| doc.fields().ok())
    }

    /// The fields of a node, or an error saying why `id` is not one.
    fn require_node(&self, id: NodeId) -> Result<NodeFields> {
        let Some(doc) = self.docs.get(&id) else {
            return Err(CrdtError::KeyNotFound(format!("node {id}")));
        };
        let fields = doc
            .fields()
            .map_err(|_| CrdtError::KeyNotFound(format!("node {id} (document not initialised)")))?;
        if fields.deleted_at.is_some() {
            return Err(CrdtError::KeyNotFound(format!("node {id} (deleted)")));
        }
        Ok(fields)
    }

    /// `start` and its ancestors by stored `parent_id`s: see [`Ancestry`].
    /// Each document once, so the walk is bounded by what is held.
    fn ancestry(&self, start: NodeId) -> Ancestry {
        let mut chain = Vec::new();
        let mut seen = HashSet::new();
        let mut cur = start;
        let complete = loop {
            if !seen.insert(cur) {
                break true;
            }
            let Some((carries, parent)) = self.docs.get(&cur).and_then(|doc| doc.link(SHARE)) else {
                break false;
            };
            chain.push((cur, carries));
            // The tree's root is the top whatever its `parent_id` says: a
            // scope's tree is rooted at a node whose parent is never held.
            match parent.filter(|_| cur != self.root) {
                Some(parent) => cur = parent,
                None => break true,
            }
        };
        Ancestry { chain, complete }
    }

    /// Whether a list that names `id` wins over where its `parent_id` would
    /// put it (`destination`): see step 1 of `analyze`. `lists` are the nodes
    /// whose list names `id`, in id order. A list claims the node when it is
    /// in a share the destination is outside of; of several, the one that
    /// keeps the node in the most shares wins (a list in a share inside a
    /// share, over one in the outer share only), the first in id order among
    /// equals. A list whose owner is `id` or under it claims nothing: making
    /// it the parent would close a cycle.
    fn share_claim(&self, id: NodeId, destination: NodeId, lists: &[NodeId]) -> ShareClaim {
        let mut to: Option<Ancestry> = None;
        let mut best: Option<(usize, NodeId)> = None;
        for &list in lists.iter().filter(|&&list| list != destination) {
            let from = self.ancestry(list);
            if from.names(id) {
                continue;
            }
            let to = to.get_or_insert_with(|| self.ancestry(destination));
            let kept = from.shares().filter(|&share| !to.names(share)).count();
            if kept > best.map_or(0, |(most, _)| most) {
                best = Some((kept, list));
            }
        }
        match (best, to) {
            (Some((_, list)), Some(to)) if to.complete => ShareClaim::Wins(list),
            (Some(_), _) => ShareClaim::Unknown,
            _ => ShareClaim::None,
        }
    }

    /// Whether `id` is `ancestor` or under it, following stored parents
    /// through nodes only (a tombstoned or missing parent ends the chain, as
    /// it does for repair). Bounded, so an unrepaired cycle cannot loop.
    fn is_under(&self, id: NodeId, ancestor: NodeId) -> bool {
        let mut cur = id;
        for _ in 0..=self.docs.len() {
            if cur == ancestor {
                return true;
            }
            match self.fields_of(cur).and_then(|f| f.parent_id).filter(|&p| self.has_node(p)) {
                Some(p) => cur = p,
                None => return false,
            }
        }
        false
    }

    /// `start` and, preorder, every descendant reached through children that
    /// are held, initialised, listed by their parent, name it as `parent_id`
    /// and satisfy `accept`. Each id once, whatever the lists say.
    fn collect_subtree(&self, start: NodeId, accept: &dyn Fn(&NodeFields) -> bool) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        self.walk(start, accept, &mut seen, &mut out);
        out
    }

    fn walk(&self, id: NodeId, accept: &dyn Fn(&NodeFields) -> bool, seen: &mut HashSet<NodeId>, out: &mut Vec<NodeId>) {
        if !seen.insert(id) {
            return;
        }
        out.push(id);
        let Some(doc) = self.docs.get(&id) else { return };
        let mut listed = HashSet::new();
        for child in doc.children() {
            if !listed.insert(child) {
                continue;
            }
            let Some(fields) = self.fields_of(child) else { continue };
            if fields.parent_id != Some(id) || !accept(&fields) {
                continue;
            }
            self.walk(child, accept, seen, out);
        }
    }

    /// One transaction on `id`'s children list.
    fn list_edit(&self, id: NodeId, f: impl FnOnce(&NodeDoc, &mut yrs::TransactionMut)) -> Result<Option<Vec<u8>>> {
        let doc = self.docs.get(&id).ok_or_else(|| CrdtError::KeyNotFound(format!("node {id}")))?;
        doc.edit(|d, txn| {
            f(d, txn);
            Ok(())
        })
        .map(|(_, update)| update)
    }

    /// A metadata edit on a node, in one transaction on its document.
    fn node_edit(&mut self, id: NodeId, f: impl FnOnce(&NodeDoc, &mut yrs::TransactionMut) -> Result<()>) -> Result<TreeEdit> {
        self.require_node(id)?;
        let (_, update) = self.docs[&id].edit_node(f)?;
        let mut edit = TreeEdit::default();
        edit.push(id, update);
        Ok(edit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const T0: &str = "2026-09-18T10:00:00Z";
    const T1: &str = "2026-09-18T10:00:01Z";
    const T2: &str = "2026-09-18T10:00:02Z";
    const T3: &str = "2026-09-18T10:00:03Z";

    fn id() -> NodeId {
        NodeId::new()
    }

    fn tree() -> (Tree, NodeId) {
        let root = id();
        (Tree::new(root, "Store", T0).unwrap(), root)
    }

    /// A second tree holding copies of every document of `a`, as a replica
    /// that reconciled once would.
    fn replica_of(a: &Tree) -> Tree {
        let docs = a.ids().into_iter().map(|id| (id, NodeDoc::load(&a.doc(id).unwrap().save()).unwrap())).collect();
        Tree::from_docs(a.root(), docs)
    }

    fn apply_edit(tree: &mut Tree, edit: &TreeEdit) {
        for (id, update) in &edit.touched {
            tree.apply_update(*id, update).unwrap();
        }
    }

    fn ids_of(edit: &TreeEdit) -> HashSet<NodeId> {
        edit.node_ids().into_iter().collect()
    }

    /// Everything a tree holds, by id: the fields, the stored list, the text.
    type Snapshot = BTreeMap<String, (Option<NodeFields>, Vec<NodeId>, String)>;

    fn snapshot(tree: &Tree) -> Snapshot {
        tree.ids()
            .into_iter()
            .map(|id| {
                let doc = tree.doc(id).unwrap();
                (id.to_string(), (doc.fields().ok(), doc.children(), doc.text()))
            })
            .collect()
    }

    fn assert_same(a: &Tree, b: &Tree) {
        let (sa, sb) = (snapshot(a), snapshot(b));
        assert_eq!(sa.keys().collect::<Vec<_>>(), sb.keys().collect::<Vec<_>>(), "held documents differ");
        for (id, va) in &sa {
            assert_eq!(va, &sb[id], "document {id} differs");
        }
    }

    /// Exchange every document's diff both ways, creating documents the other
    /// side lacks, then repair both and exchange the repairs: one round as the
    /// server performs it.
    fn sync(a: &mut Tree, b: &mut Tree) {
        let ids: HashSet<NodeId> = a.ids().into_iter().chain(b.ids()).collect();
        for id in ids {
            if a.doc(id).is_none() {
                a.insert_doc(id, NodeDoc::new());
            }
            if b.doc(id).is_none() {
                b.insert_doc(id, NodeDoc::new());
            }
            let (a_sv, b_sv) = (a.doc(id).unwrap().state_vector(), b.doc(id).unwrap().state_vector());
            let to_b = a.doc(id).unwrap().diff_since(&b_sv).unwrap();
            let to_a = b.doc(id).unwrap().diff_since(&a_sv).unwrap();
            b.apply_update(id, &to_b).unwrap();
            a.apply_update(id, &to_a).unwrap();
        }
    }

    fn sync_and_repair_round(a: &mut Tree, b: &mut Tree) {
        sync(a, b);
        let repair_a = a.repair(T3).unwrap();
        let repair_b = b.repair(T3).unwrap();
        if let Some(edit) = &repair_a {
            apply_edit(b, edit);
        }
        if let Some(edit) = &repair_b {
            apply_edit(a, edit);
        }
    }

    /// Both trees are well formed, nothing is left to repair, and every
    /// document is the same on both sides.
    fn assert_converged(a: &mut Tree, b: &mut Tree) {
        let issues = a.validate_tree();
        assert!(issues.is_empty(), "a not well formed: {issues:?}");
        let issues = b.validate_tree();
        assert!(issues.is_empty(), "b not well formed: {issues:?}");
        assert!(a.repair(T3).unwrap().is_none(), "a should have nothing left to repair");
        assert!(b.repair(T3).unwrap().is_none(), "b should have nothing left to repair");
        assert_same(a, b);
    }

    // ── Operations ──────────────────────────────────────────────────────

    #[test]
    fn a_new_tree_has_a_root_folder() {
        let (mut tree, root) = tree();
        assert_eq!(tree.root(), root);
        assert!(tree.has_node(root));
        let info = tree.get_node_info(root).unwrap();
        assert_eq!(info.title, "Store");
        assert_eq!(info.node_type, "folder");
        assert_eq!(info.parent_id, None);
        assert_eq!(tree.list_node_ids(), vec![root]);
        assert!(tree.get_children(root).unwrap().is_empty());
        assert!(tree.validate_tree().is_empty());
        assert!(tree.repair(T1).unwrap().is_none());
    }

    #[test]
    fn add_node_places_a_child_and_names_both_documents() {
        let (mut a, root) = tree();
        let mut b = replica_of(&a);
        let child = id();
        let edit = a.add_node(child, None, None, "document", "Doc", T1).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([child, root]));

        assert!(a.has_node(child));
        let info = a.get_node_info(child).unwrap();
        assert_eq!((info.title.as_str(), info.node_type.as_str(), info.parent_id), ("Doc", "document", Some(root)));
        assert_eq!((info.created_at.as_str(), info.modified_at.as_str()), (T1, T1));
        assert_eq!(a.get_children(root).unwrap(), vec![child]);
        assert_eq!(a.subtree_ids(root).unwrap(), vec![root, child]);
        assert!(a.validate_tree().is_empty());

        apply_edit(&mut b, &edit);
        assert_same(&a, &b);
        assert_eq!(b.get_children(root).unwrap(), vec![child]);
    }

    #[test]
    fn add_node_at_a_position_under_a_folder() {
        let (mut a, root) = tree();
        let folder = id();
        a.add_node(folder, Some(root), None, "folder", "F", T1).unwrap();
        let (x, y, z) = (id(), id(), id());
        a.add_node(x, Some(folder), None, "document", "x", T1).unwrap();
        a.add_node(z, Some(folder), Some(99), "document", "z", T1).unwrap();
        a.add_node(y, Some(folder), Some(1), "document", "y", T1).unwrap();
        assert_eq!(a.get_children(folder).unwrap(), vec![x, y, z]);
        assert_eq!(a.subtree_ids(root).unwrap(), vec![root, folder, x, y, z]);
    }

    #[test]
    fn add_node_refuses_an_existing_id_and_a_missing_parent() {
        let (mut a, root) = tree();
        let child = id();
        a.add_node(child, None, None, "document", "Doc", T1).unwrap();
        assert!(a.add_node(child, None, None, "document", "Again", T1).is_err());
        assert!(a.add_node(id(), Some(id()), None, "document", "Orphan", T1).is_err());
        assert!(a.add_node(root, None, None, "document", "Root again", T1).is_err());
        assert_eq!(a.list_node_ids().len(), 2);
    }

    /// The content of a node can arrive before its place in the tree (a peer's
    /// `applyEdit` on a document this store has not created yet): `add_node`
    /// then initialises the document it already holds, content intact.
    #[test]
    fn add_node_initialises_a_held_content_only_document() {
        let (mut a, root) = tree();
        let child = id();
        let content = NodeDoc::from_plain_text("arrived first").unwrap();
        a.apply_update(child, &content.save()).unwrap();
        assert!(!a.has_node(child));
        assert!(!a.list_node_ids().contains(&child));

        let edit = a.add_node(child, None, None, "document", "Doc", T1).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([child, root]));
        assert!(a.has_node(child));
        assert_eq!(a.doc(child).unwrap().text(), "arrived first");
        assert_eq!(a.get_children(root).unwrap(), vec![child]);
    }

    #[test]
    fn add_node_with_doc_keeps_the_content_and_sends_the_whole_document() {
        let (mut a, root) = tree();
        let mut b = replica_of(&a);
        let child = id();
        let doc = NodeDoc::from_plain_text("imported").unwrap();
        let edit = a.add_node_with_doc(child, doc, None, None, "document", "Imported", T1).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([child, root]));
        assert_eq!(a.doc(child).unwrap().text(), "imported");
        assert_eq!(a.get_node_info(child).unwrap().title, "Imported");

        apply_edit(&mut b, &edit);
        assert_same(&a, &b);
        assert_eq!(b.doc(child).unwrap().text(), "imported");

        let again = NodeDoc::from_plain_text("twice").unwrap();
        assert!(a.add_node_with_doc(child, again, None, None, "document", "Twice", T1).is_err());
    }

    #[test]
    fn move_node_between_parents_names_three_documents() {
        let (mut a, root) = tree();
        let mut b = replica_of(&a);
        let (p, q, x) = (id(), id(), id());
        a.add_node(p, Some(root), None, "folder", "P", T1).unwrap();
        a.add_node(q, Some(root), None, "folder", "Q", T1).unwrap();
        a.add_node(x, Some(p), None, "document", "X", T1).unwrap();
        sync(&mut a, &mut b);

        let edit = a.move_node(x, q, None, T2).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([p, q, x]));
        assert!(a.get_children(p).unwrap().is_empty());
        assert_eq!(a.get_children(q).unwrap(), vec![x]);
        let info = a.get_node_info(x).unwrap();
        assert_eq!(info.parent_id, Some(q));
        assert_eq!(info.modified_at, T2);
        assert!(a.validate_tree().is_empty());

        apply_edit(&mut b, &edit);
        assert_same(&a, &b);
    }

    #[test]
    fn move_node_within_a_parent_to_a_position() {
        let (mut a, root) = tree();
        let (x, y, z) = (id(), id(), id());
        a.add_node(x, Some(root), None, "document", "x", T1).unwrap();
        a.add_node(y, Some(root), None, "document", "y", T1).unwrap();
        a.add_node(z, Some(root), None, "document", "z", T1).unwrap();
        let edit = a.move_node(z, root, Some(0), T2).unwrap();
        // One list touched once, plus the node's own `modified_at`.
        assert_eq!(ids_of(&edit), HashSet::from([root, z]));
        assert_eq!(a.get_children(root).unwrap(), vec![z, x, y]);
        a.move_node(z, root, None, T2).unwrap();
        assert_eq!(a.get_children(root).unwrap(), vec![x, y, z]);
        assert!(a.validate_tree().is_empty());
    }

    #[test]
    fn move_node_refuses_the_root_and_a_cycle_by_request() {
        let (mut a, root) = tree();
        let (p, x) = (id(), id());
        a.add_node(p, Some(root), None, "folder", "P", T1).unwrap();
        a.add_node(x, Some(p), None, "document", "X", T1).unwrap();
        assert!(a.move_node(root, p, None, T2).is_err());
        assert!(a.move_node(p, x, None, T2).is_err(), "under its own descendant");
        assert!(a.move_node(p, p, None, T2).is_err(), "under itself");
        assert!(a.move_node(x, id(), None, T2).is_err(), "under nothing");
        assert_eq!(a.get_node_info(p).unwrap().parent_id, Some(root));
        assert_eq!(a.get_node_info(x).unwrap().parent_id, Some(p));
    }

    #[test]
    fn remove_node_tombstones_the_subtree_and_keeps_lists_below() {
        let (mut a, root) = tree();
        let mut b = replica_of(&a);
        let (f, x, y, other) = (id(), id(), id(), id());
        a.add_node(f, Some(root), None, "folder", "F", T1).unwrap();
        a.add_node(x, Some(f), None, "document", "x", T1).unwrap();
        a.add_node(y, Some(x), None, "document", "y", T1).unwrap();
        a.add_node(other, Some(root), None, "document", "other", T1).unwrap();
        sync(&mut a, &mut b);

        let edit = a.remove_node(f, T2).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([f, x, y, root]));
        for n in [f, x, y] {
            assert!(!a.has_node(n));
            assert!(a.get_node_info(n).is_err());
            assert!(a.get_children(n).is_err());
            assert_eq!(a.doc(n).unwrap().fields().unwrap().deleted_at.as_deref(), Some(T2));
        }
        assert_eq!(a.get_children(root).unwrap(), vec![other]);
        assert_eq!(a.list_node_ids().len(), 2);
        // The lists below the deletion are untouched; the parent's lost the entry.
        assert_eq!(a.doc(f).unwrap().children(), vec![x]);
        assert_eq!(a.doc(x).unwrap().children(), vec![y]);
        assert_eq!(a.doc(root).unwrap().children(), vec![other]);
        assert!(a.validate_tree().is_empty());
        assert!(a.remove_node(root, T2).is_err());
        assert!(a.remove_node(f, T2).is_err(), "already deleted");

        apply_edit(&mut b, &edit);
        assert_same(&a, &b);
    }

    #[test]
    fn undelete_puts_the_subtree_back_at_the_end_of_its_parent() {
        let (mut a, root) = tree();
        let mut b = replica_of(&a);
        let (f, x, y, other) = (id(), id(), id(), id());
        a.add_node(f, Some(root), None, "folder", "F", T1).unwrap();
        a.add_node(x, Some(f), None, "document", "x", T1).unwrap();
        a.add_node(y, Some(x), None, "document", "y", T1).unwrap();
        a.add_node(other, Some(root), None, "document", "other", T1).unwrap();
        a.remove_node(f, T2).unwrap();
        sync(&mut a, &mut b);

        assert!(a.undelete_node(other, T3).is_err(), "not deleted");
        let edit = a.undelete_node(f, T3).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([f, x, y, root]));
        for n in [f, x, y] {
            assert!(a.has_node(n), "{n} should be back");
        }
        assert_eq!(a.get_children(root).unwrap(), vec![other, f]);
        assert_eq!(a.get_children(f).unwrap(), vec![x]);
        assert_eq!(a.get_children(x).unwrap(), vec![y]);
        assert_eq!(a.get_node_info(f).unwrap().modified_at, T3);
        assert!(a.validate_tree().is_empty());

        apply_edit(&mut b, &edit);
        assert_same(&a, &b);
    }

    #[test]
    fn undelete_goes_under_the_root_when_the_parent_is_gone() {
        let (mut a, root) = tree();
        let (f, x) = (id(), id());
        a.add_node(f, Some(root), None, "folder", "F", T1).unwrap();
        a.add_node(x, Some(f), None, "document", "x", T1).unwrap();
        a.remove_node(f, T2).unwrap();

        // Undeleting the child alone: its parent is still a tombstone, so it
        // goes under the root and leaves the tombstone's list.
        let edit = a.undelete_node(x, T3).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([x, root, f]));
        assert_eq!(a.get_node_info(x).unwrap().parent_id, Some(root));
        assert_eq!(a.get_children(root).unwrap(), vec![x]);
        assert!(!a.has_node(f));
        assert!(a.doc(f).unwrap().children().is_empty());
        assert!(a.validate_tree().is_empty());

        // Undeleting the folder now brings only itself back: `x` left it.
        let edit = a.undelete_node(f, T3).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([f, root]));
        assert_eq!(a.get_children(root).unwrap(), vec![x, f]);
        assert!(a.get_children(f).unwrap().is_empty());
        assert!(a.validate_tree().is_empty());
        assert!(a.repair(T3).unwrap().is_none());
    }

    #[test]
    fn undelete_leaves_a_child_deleted_earlier_deleted() {
        let (mut a, root) = tree();
        let (f, x, y) = (id(), id(), id());
        a.add_node(f, Some(root), None, "folder", "F", T1).unwrap();
        a.add_node(x, Some(f), None, "document", "x", T1).unwrap();
        a.add_node(y, Some(f), None, "document", "y", T1).unwrap();
        a.remove_node(x, T1).unwrap();
        a.remove_node(f, T2).unwrap();

        let edit = a.undelete_node(f, T3).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([f, y, root]));
        assert!(a.has_node(f) && a.has_node(y));
        assert!(!a.has_node(x), "deleted by an earlier deletion, stays deleted");
        assert_eq!(a.get_children(f).unwrap(), vec![y]);
        assert!(a.validate_tree().is_empty());
    }

    #[test]
    fn metadata_edits_name_the_one_document() {
        let (mut a, root) = tree();
        let mut b = replica_of(&a);
        let x = id();
        a.add_node(x, Some(root), None, "document", "x", T1).unwrap();
        sync(&mut a, &mut b);

        let edits = [
            a.set_title(x, "Renamed", T2).unwrap(),
            a.set_tags(x, &["one".into(), "two".into()], T2).unwrap(),
            a.set_custom(x, "icon", &serde_json::json!("star"), T2).unwrap(),
            a.set_custom(x, "gone", &serde_json::json!(1), T2).unwrap(),
            a.remove_custom(x, "gone", T2).unwrap(),
            a.set_node_type(x, "folder", T2).unwrap(),
            a.touch_modified(x, T3).unwrap(),
        ];
        for edit in &edits {
            assert_eq!(edit.node_ids(), vec![x]);
            apply_edit(&mut b, edit);
        }
        let info = a.get_node_info(x).unwrap();
        assert_eq!(info.title, "Renamed");
        assert_eq!(info.tags, vec!["one", "two"]);
        assert_eq!(info.custom.get("icon"), Some(&serde_json::json!("star")));
        assert!(!info.custom.contains_key("gone"));
        assert_eq!(info.node_type, "folder");
        assert_eq!(info.modified_at, T3);
        assert_same(&a, &b);

        assert!(a.remove_custom(x, "never", T3).unwrap().is_empty(), "an absent key touches nothing");
        assert!(a.set_title(id(), "?", T3).is_err());
        a.remove_node(x, T3).unwrap();
        assert!(a.set_title(x, "?", T3).is_err(), "a tombstone is not a node");
    }

    #[test]
    fn get_children_filters_and_dedups_the_stored_list() {
        let (mut a, root) = tree();
        let (x, y) = (id(), id());
        a.add_node(x, Some(root), None, "document", "x", T1).unwrap();
        a.add_node(y, Some(root), None, "document", "y", T1).unwrap();
        // What concurrency can leave in a list: a duplicate, an unknown id, a
        // content-only document, a tombstone, the owner itself.
        let unknown = id();
        let content_only = id();
        a.apply_update(content_only, &NodeDoc::from_plain_text("c").unwrap().save()).unwrap();
        let root_doc = a.doc_mut(root).unwrap();
        root_doc.insert_child(0, x).unwrap();
        root_doc.insert_child(99, unknown).unwrap();
        root_doc.insert_child(99, content_only).unwrap();
        root_doc.insert_child(99, root).unwrap();
        a.remove_node(y, T2).unwrap();
        root_doc_insert(&mut a, root, y);

        assert_eq!(a.get_children(root).unwrap(), vec![x]);
        assert_eq!(a.subtree_ids(root).unwrap(), vec![root, x]);
        assert!(a.get_children(unknown).is_err());
        assert!(a.get_children(content_only).is_err());
        assert!(a.list_node_ids().len() == 2);
    }

    fn root_doc_insert(tree: &mut Tree, owner: NodeId, child: NodeId) {
        tree.doc_mut(owner).unwrap().insert_child(99, child).unwrap();
    }

    #[test]
    fn subtree_ids_counts_a_child_only_where_list_and_parent_agree() {
        let (mut a, root) = tree();
        let (p, q, x) = (id(), id(), id());
        a.add_node(p, Some(root), None, "folder", "P", T1).unwrap();
        a.add_node(q, Some(root), None, "folder", "Q", T1).unwrap();
        a.add_node(x, Some(p), None, "document", "x", T1).unwrap();
        // Half a move: Q lists x, but x still says P.
        a.doc_mut(q).unwrap().insert_child(0, x).unwrap();
        assert_eq!(a.subtree_ids(q).unwrap(), vec![q]);
        assert_eq!(a.subtree_ids(p).unwrap(), vec![p, x]);
        assert_eq!(a.subtree_ids(root).unwrap(), vec![root, p, x, q]);
        // The other half: x says Q, but only P lists it.
        a.doc_mut(q).unwrap().remove_child(x).unwrap();
        a.doc_mut(x).unwrap().set_parent_id(Some(q), T2).unwrap();
        assert_eq!(a.subtree_ids(p).unwrap(), vec![p]);
        assert_eq!(a.subtree_ids(q).unwrap(), vec![q]);
    }

    #[test]
    fn apply_update_creates_an_unknown_document() {
        let (mut a, root) = tree();
        let (mut b, _) = tree();
        let x = id();
        let edit = a.add_node(x, Some(root), None, "document", "x", T1).unwrap();
        // b knows neither the root nor x; it holds both afterwards.
        let effects: Vec<_> = edit.touched.iter().map(|(id, u)| b.apply_update(*id, u).unwrap()).collect();
        assert!(effects.iter().all(|e| e.changed && e.structure && !e.content));
        assert!(b.doc(x).unwrap().is_initialised());
        assert_eq!(b.doc(root).unwrap().children(), vec![x]);
    }

    // ── `repair` / `validate_tree` (decision 9 over documents) ──────────

    #[test]
    fn repair_fixes_a_self_referential_parent() {
        let (mut a, root) = tree();
        let child = id();
        a.add_node(child, Some(root), None, "document", "Doc", T1).unwrap();
        a.doc_mut(child).unwrap().set_parent_id(Some(child), T1).unwrap();

        let issues = a.validate_tree();
        assert!(
            issues.iter().any(|i| matches!(i, TreeIssue::OrphanNode { node_id, missing_parent } if *node_id == child && *missing_parent == child)),
            "expected an OrphanNode issue for the self-reference, got {issues:?}"
        );
        let repair = a.repair(T2).unwrap().expect("a self-referential parent needs a repair");
        assert_eq!(repair.node_ids(), vec![child]);
        assert!(a.validate_tree().is_empty());
        assert!(a.repair(T2).unwrap().is_none(), "repair should be idempotent");
        assert_eq!(a.get_node_info(child).unwrap().parent_id, Some(root));
        assert_eq!(a.get_children(root).unwrap(), vec![child]);
        // Repair never touches timestamps.
        assert_eq!(a.get_node_info(child).unwrap().modified_at, T1);
    }

    /// A parent this device does not hold is unknown, not missing: documents
    /// travel one by one, and a device may lack the key of one it was sent.
    /// Re-parenting the node here would move it out of a folder every other
    /// device can see it in.
    #[test]
    fn a_node_under_a_document_not_held_is_left_where_it_says_it_is() {
        let (mut a, root) = tree();
        let (child, folder) = (id(), id());
        a.add_node(child, Some(root), None, "document", "Doc", T1).unwrap();
        // Someone moved it into a folder whose document is not here (yet).
        a.doc_mut(child).unwrap().set_parent_id(Some(folder), T1).unwrap();
        assert!(a.validate_tree().is_empty(), "{:?}", a.validate_tree());
        assert!(a.repair(T2).unwrap().is_none(), "nothing known is wrong");
        assert_eq!(a.get_node_info(child).unwrap().parent_id, Some(folder));

        // The folder arrives, listing nothing (the move's other half is
        // still on its way): now everything is known and repair finishes it.
        let mut doc = NodeDoc::new();
        doc.init("folder", "Folder", Some(root), T1).unwrap();
        a.apply_update(folder, &doc.save()).unwrap();
        a.repair(T2).unwrap().expect("the folder is here: the node belongs in its list");
        assert!(a.validate_tree().is_empty());
        assert_eq!(a.get_children(folder).unwrap(), vec![child]);
        assert_eq!(a.get_children(root).unwrap(), vec![folder]);
    }

    /// The livelock found on 2026-09-21: a member made a note in a shared
    /// folder; the owner's page held the folder and not the note (no key for
    /// it yet), unlisted it as "missing", every device that did hold it
    /// listed it again, six times a second, for as long as the page was open.
    #[test]
    fn a_listed_child_whose_document_is_not_held_stays_listed() {
        let (mut holds_all, root) = tree();
        let (folder, note) = (id(), id());
        holds_all.add_node(folder, Some(root), None, "folder", "Shared", T1).unwrap();
        let mut lacks_note = replica_of(&holds_all);
        let made = holds_all.add_node(note, Some(folder), None, "document", "A member's note", T2).unwrap();

        // The other device is sent the folder's new list, and not the note.
        for (doc_id, update) in &made.touched {
            if *doc_id != note {
                lacks_note.apply_update(*doc_id, update).unwrap();
            }
        }
        assert_eq!(lacks_note.doc(folder).unwrap().children(), vec![note]);
        assert!(lacks_note.validate_tree().is_empty(), "{:?}", lacks_note.validate_tree());
        assert!(lacks_note.repair(T3).unwrap().is_none(), "what is not held is not judged");
        assert!(lacks_note.get_children(folder).unwrap().is_empty(), "and is not shown either");
        assert!(holds_all.repair(T3).unwrap().is_none(), "so the device that holds it has nothing to put back");

        // The note arrives (its key did): both agree, and nothing was written.
        for (doc_id, update) in &made.touched {
            lacks_note.apply_update(*doc_id, update).unwrap();
        }
        assert!(lacks_note.repair(T3).unwrap().is_none());
        assert_eq!(lacks_note.get_children(folder).unwrap(), vec![note]);
    }

    #[test]
    fn repair_fixes_a_detached_node() {
        let (mut a, root) = tree();
        let stray = id();
        let mut doc = NodeDoc::new();
        doc.init("document", "Stray", None, T1).unwrap();
        a.insert_doc(stray, doc);
        let issues = a.validate_tree();
        assert!(issues.iter().any(|i| matches!(i, TreeIssue::DetachedNode { node_id } if *node_id == stray)), "{issues:?}");
        let repair = a.repair(T2).unwrap().expect("a detached node needs a repair");
        assert_eq!(ids_of(&repair), HashSet::from([stray, root]));
        assert!(a.validate_tree().is_empty());
        assert_eq!(a.get_node_info(stray).unwrap().parent_id, Some(root));
        assert_eq!(a.get_children(root).unwrap(), vec![stray]);
    }

    #[test]
    fn repair_fixes_a_duplicate_within_one_list() {
        let (mut a, root) = tree();
        let child = id();
        a.add_node(child, Some(root), None, "document", "Doc", T1).unwrap();
        a.doc_mut(root).unwrap().insert_child(99, child).unwrap();
        assert!(a.validate_tree().iter().any(|i| matches!(i, TreeIssue::DuplicateInList { .. })));
        let repair = a.repair(T2).unwrap().expect("a duplicate entry needs a repair");
        assert_eq!(repair.node_ids(), vec![root]);
        assert!(a.validate_tree().is_empty());
        assert_eq!(a.doc(root).unwrap().children(), vec![child]);
    }

    #[test]
    fn repair_fixes_a_child_in_the_wrong_list() {
        let (mut a, root) = tree();
        let (fa, fb, child) = (id(), id(), id());
        a.add_node(fa, Some(root), None, "folder", "A", T1).unwrap();
        a.add_node(fb, Some(root), None, "folder", "B", T1).unwrap();
        a.add_node(child, Some(fa), None, "document", "Doc", T1).unwrap();
        a.doc_mut(fb).unwrap().insert_child(0, child).unwrap();
        assert!(a.validate_tree().iter().any(|i| matches!(i, TreeIssue::WrongList { .. })));
        let repair = a.repair(T2).unwrap().expect("a wrong-list entry needs a repair");
        assert_eq!(repair.node_ids(), vec![fb]);
        assert!(a.validate_tree().is_empty());
        assert_eq!(a.get_children(fa).unwrap(), vec![child]);
        assert!(a.doc(fb).unwrap().children().is_empty());
    }

    #[test]
    fn repair_fixes_the_root_appearing_in_a_list() {
        let (mut a, root) = tree();
        let folder = id();
        a.add_node(folder, Some(root), None, "folder", "Folder", T1).unwrap();
        a.doc_mut(folder).unwrap().insert_child(0, root).unwrap();
        assert!(a.validate_tree().iter().any(|i| matches!(i, TreeIssue::RootInList { .. })));
        a.repair(T2).unwrap().expect("root-in-list needs a repair");
        assert!(a.validate_tree().is_empty());
        assert!(a.doc(folder).unwrap().children().is_empty());
    }

    #[test]
    fn repair_fixes_a_two_node_cycle() {
        let (mut a, root) = tree();
        let (fa, fb) = (id(), id());
        a.add_node(fa, Some(root), None, "folder", "A", T1).unwrap();
        a.add_node(fb, Some(root), None, "folder", "B", T1).unwrap();
        a.doc_mut(fa).unwrap().set_parent_id(Some(fb), T1).unwrap();
        a.doc_mut(fb).unwrap().set_parent_id(Some(fa), T1).unwrap();
        let issues = a.validate_tree();
        assert!(issues.iter().any(|i| matches!(i, TreeIssue::Cycle { .. })), "expected a Cycle issue, got {issues:?}");

        let repair = a.repair(T2).unwrap().expect("a cycle needs a repair");
        assert!(!repair.is_empty());
        assert!(a.validate_tree().is_empty());
        assert!(a.repair(T2).unwrap().is_none(), "repair should be idempotent");

        let (top, bottom) = if a.get_node_info(fa).unwrap().parent_id == Some(root) { (fa, fb) } else { (fb, fa) };
        assert_eq!(top.to_string(), fa.to_string().min(fb.to_string()), "the smallest id goes under the root");
        assert_eq!(a.get_node_info(bottom).unwrap().parent_id, Some(top));
        assert_eq!(a.get_children(root).unwrap(), vec![top]);
        assert_eq!(a.get_children(top).unwrap(), vec![bottom]);
        assert!(a.get_children(bottom).unwrap().is_empty());
    }

    /// The document-specific cases: a tombstone is known to be deleted and
    /// leaves the lists; a document that is not a node YET (content only:
    /// its `node` root has not arrived) is unknown and stays listed, unshown.
    #[test]
    fn repair_removes_entries_naming_tombstones_and_leaves_documents_that_are_not_nodes_yet() {
        let (mut a, root) = tree();
        let (folder, dead, content_only) = (id(), id(), id());
        a.add_node(folder, Some(root), None, "folder", "F", T1).unwrap();
        a.add_node(dead, Some(folder), None, "document", "dead", T1).unwrap();
        a.doc_mut(dead).unwrap().set_deleted(Some(T2)).unwrap(); // a tombstone still listed by its parent
        a.apply_update(content_only, &NodeDoc::from_plain_text("c").unwrap().save()).unwrap();
        a.doc_mut(folder).unwrap().insert_child(99, content_only).unwrap();
        // A tombstone whose list names a live node: the list is left alone, the
        // node is an orphan (its parent is not a node) and goes under the root.
        let orphan = id();
        a.add_node(orphan, Some(folder), None, "document", "orphan", T1).unwrap();
        a.doc_mut(orphan).unwrap().set_parent_id(Some(dead), T1).unwrap();
        a.doc_mut(dead).unwrap().insert_child(0, orphan).unwrap();

        let issues = a.validate_tree();
        assert!(issues.iter().any(|i| matches!(i, TreeIssue::MissingChild { child_id, .. } if *child_id == dead)), "{issues:?}");
        assert!(!issues.iter().any(|i| matches!(i, TreeIssue::MissingChild { child_id, .. } if *child_id == content_only)), "{issues:?}");
        assert!(issues.iter().any(|i| matches!(i, TreeIssue::OrphanNode { node_id, missing_parent } if *node_id == orphan && *missing_parent == dead)), "{issues:?}");

        let repair = a.repair(T3).unwrap().expect("needs a repair");
        assert_eq!(ids_of(&repair), HashSet::from([folder, orphan, root]));
        assert!(a.validate_tree().is_empty());
        assert!(a.repair(T3).unwrap().is_none());
        assert_eq!(a.doc(folder).unwrap().children(), vec![content_only], "the tombstone left the list; the document that is no node yet stayed");
        assert!(a.get_children(folder).unwrap().is_empty(), "and is not shown");
        assert_eq!(a.get_node_info(orphan).unwrap().parent_id, Some(root));
        assert_eq!(a.get_children(root).unwrap(), vec![folder, orphan]);
        assert_eq!(a.doc(dead).unwrap().children(), vec![orphan], "a tombstone's list is not touched");
        assert!(!a.has_node(dead) && !a.has_node(content_only));
    }

    #[test]
    fn repair_is_a_no_op_on_a_well_formed_tree_and_on_a_tree_without_a_root_yet() {
        let (mut a, root) = tree();
        a.add_node(id(), Some(root), None, "document", "Doc", T1).unwrap();
        assert!(a.repair(T2).unwrap().is_none());

        let mut fresh = Tree::from_docs(root, HashMap::new());
        assert!(fresh.validate_tree().is_empty());
        assert!(fresh.repair(T2).unwrap().is_none());
        assert!(!fresh.has_node(root));
        assert!(fresh.get_children(root).is_err());
    }

    // ── Concurrency scenarios (decision 9): two trees exchanging per-document
    // updates converge within two rounds of exchange-and-repair. ────────

    #[test]
    fn concurrent_moves_of_one_node_to_two_parents_converge() {
        let (mut a, root) = tree();
        let (fa, fb, child) = (id(), id(), id());
        a.add_node(fa, Some(root), None, "folder", "A", T1).unwrap();
        a.add_node(fb, Some(root), None, "folder", "B", T1).unwrap();
        a.add_node(child, Some(fa), None, "document", "Doc", T1).unwrap();
        let mut b = replica_of(&a);

        a.move_node(child, fb, None, T2).unwrap();
        b.move_node(child, root, None, T2).unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);
        assert_converged(&mut a, &mut b);

        let winner = a.get_node_info(child).unwrap().parent_id.unwrap();
        assert!(winner == fb || winner == root, "unexpected winner {winner}");
        assert_eq!(a.get_children(winner).unwrap().iter().filter(|&&c| c == child).count(), 1);
        assert!(a.get_children(fa).unwrap().is_empty());
        let loser = if winner == fb { root } else { fb };
        assert!(!a.get_children(loser).unwrap().contains(&child));
    }

    #[test]
    fn concurrent_moves_of_a_under_b_and_b_under_a_converge() {
        let (mut a, root) = tree();
        let (fa, fb) = (id(), id());
        a.add_node(fa, Some(root), None, "folder", "A", T1).unwrap();
        a.add_node(fb, Some(root), None, "folder", "B", T1).unwrap();
        let mut b = replica_of(&a);

        a.move_node(fb, fa, None, T2).unwrap();
        b.move_node(fa, fb, None, T2).unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);
        assert_converged(&mut a, &mut b);

        let a_parent = a.get_node_info(fa).unwrap().parent_id;
        let b_parent = a.get_node_info(fb).unwrap().parent_id;
        assert!((a_parent == Some(root)) ^ (b_parent == Some(root)), "exactly one under the root: {a_parent:?}/{b_parent:?}");
    }

    #[test]
    fn deleting_a_folder_while_the_other_side_adds_a_child_to_it_converges() {
        let (mut a, root) = tree();
        let folder = id();
        a.add_node(folder, Some(root), None, "folder", "Folder", T1).unwrap();
        let mut b = replica_of(&a);

        a.remove_node(folder, T2).unwrap();
        let new_child = id();
        b.add_node(new_child, Some(folder), None, "document", "New", T2).unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);
        assert_converged(&mut a, &mut b);

        // The folder stays deleted; its would-be child survives under the root
        // rather than being lost (decision 10).
        assert!(!a.has_node(folder));
        assert!(a.has_node(new_child));
        assert_eq!(a.get_node_info(new_child).unwrap().parent_id, Some(root));
        assert!(a.get_children(root).unwrap().contains(&new_child));
    }

    #[test]
    fn deleting_a_node_while_the_other_side_moves_it_converges() {
        let (mut a, root) = tree();
        let (fa, fb, child) = (id(), id(), id());
        a.add_node(fa, Some(root), None, "folder", "A", T1).unwrap();
        a.add_node(fb, Some(root), None, "folder", "B", T1).unwrap();
        a.add_node(child, Some(fa), None, "document", "Doc", T1).unwrap();
        let mut b = replica_of(&a);

        a.remove_node(child, T2).unwrap();
        b.move_node(child, fb, None, T2).unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);
        assert_converged(&mut a, &mut b);

        // The delete wins: a tombstone on both sides, and the list entry the
        // move left behind is cleaned up rather than left dangling.
        assert!(!a.has_node(child));
        assert!(!b.has_node(child));
        assert!(!a.get_children(fb).unwrap().contains(&child));
        assert!(!a.doc(fb).unwrap().children().contains(&child));
    }

    #[test]
    fn both_sides_repairing_the_same_merged_state_concurrently_converge() {
        let (mut a, root) = tree();
        let (fa, fb, child) = (id(), id(), id());
        a.add_node(fa, Some(root), None, "folder", "A", T1).unwrap();
        a.add_node(fb, Some(root), None, "folder", "B", T1).unwrap();
        a.add_node(child, Some(fa), None, "document", "Doc", T1).unwrap();
        let mut b = replica_of(&a);

        // Every kind of damage at once, then both repair the same held state
        // before either sees the other's repair.
        a.move_node(child, fb, None, T2).unwrap();
        b.move_node(child, root, None, T2).unwrap();
        a.doc_mut(fa).unwrap().set_parent_id(Some(fb), T2).unwrap();
        b.doc_mut(fb).unwrap().set_parent_id(Some(fa), T2).unwrap();
        sync(&mut a, &mut b);
        assert_same(&a, &b);
        let ra = a.repair(T3).unwrap().expect("damage on a");
        let rb = b.repair(T3).unwrap().expect("damage on b");
        // Deterministic in the held state: the same documents, the same deletions.
        assert_eq!(ids_of(&ra), ids_of(&rb));
        apply_edit(&mut b, &ra);
        apply_edit(&mut a, &rb);
        sync_and_repair_round(&mut a, &mut b);
        assert_converged(&mut a, &mut b);
    }

    // ── Tombstones under concurrency ────────────────────────────────────

    #[test]
    fn a_rename_concurrent_with_a_delete_lands_in_the_tombstoned_document() {
        let (mut a, root) = tree();
        let x = id();
        a.add_node(x, Some(root), None, "document", "Old", T1).unwrap();
        let mut b = replica_of(&a);

        a.remove_node(x, T2).unwrap();
        b.set_title(x, "New", T2).unwrap();

        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);
        assert_converged(&mut a, &mut b);
        assert!(!a.has_node(x) && !b.has_node(x), "the node stays deleted");
        assert_eq!(a.doc(x).unwrap().fields().unwrap().title, "New", "the rename is in its document");
        assert!(a.get_children(root).unwrap().is_empty());
    }

    #[test]
    fn undelete_after_a_concurrent_move_puts_the_node_where_it_was_moved() {
        let (mut a, root) = tree();
        let (p, q, x) = (id(), id(), id());
        a.add_node(p, Some(root), None, "folder", "P", T1).unwrap();
        a.add_node(q, Some(root), None, "folder", "Q", T1).unwrap();
        a.add_node(x, Some(p), None, "document", "x", T1).unwrap();
        let mut b = replica_of(&a);

        a.remove_node(x, T2).unwrap();
        b.move_node(x, q, None, T2).unwrap();
        sync_and_repair_round(&mut a, &mut b);
        sync_and_repair_round(&mut a, &mut b);
        assert_converged(&mut a, &mut b);
        assert!(!a.has_node(x));

        let edit = b.undelete_node(x, T3).unwrap();
        apply_edit(&mut a, &edit);
        sync_and_repair_round(&mut a, &mut b);
        assert_converged(&mut a, &mut b);
        assert!(a.has_node(x));
        assert_eq!(a.get_node_info(x).unwrap().parent_id, Some(q));
        assert_eq!(a.get_children(q).unwrap(), vec![x]);
        assert!(a.get_children(p).unwrap().is_empty());
    }

    // ── Shares (docs/MOVE_CONTRACT.md "What 'out of a share' means") ────

    fn marker(name: &str) -> serde_json::Value {
        serde_json::json!({ "v": 1, "key_id": "5b0f6c1e-0000-4000-8000-00000000c0de", "url": "https://pimble.example", "name": name })
    }

    fn share(tree: &mut Tree, id: NodeId) -> TreeEdit {
        tree.set_custom(id, SHARE, &marker("Shared"), T1).unwrap()
    }

    fn folder(tree: &mut Tree, parent: NodeId, title: &str) -> NodeId {
        let new = id();
        tree.add_node(new, Some(parent), None, "folder", title, T1).unwrap();
        new
    }

    fn note(tree: &mut Tree, parent: NodeId, title: &str) -> NodeId {
        let new = id();
        tree.add_node(new, Some(parent), None, "document", title, T1).unwrap();
        new
    }

    /// An id source that records what it handed out.
    fn fresh_ids(handed_out: &mut Vec<NodeId>) -> impl FnMut() -> NodeId + '_ {
        move || {
            let new = NodeId::new();
            handed_out.push(new);
            new
        }
    }

    fn never_asked() -> NodeId {
        panic!("a plain move needs no new id")
    }

    /// What a client that does not keep the rule writes: a `parent_id`, and
    /// no list. `None` removes it.
    fn tamper_parent(tree: &mut Tree, node: NodeId, parent: Option<NodeId>) -> TreeEdit {
        let (_, update) = tree.docs[&node]
            .edit_node(|d, txn| {
                d.write_parent(txn, parent);
                Ok(())
            })
            .unwrap();
        let mut edit = TreeEdit::default();
        edit.push(node, update);
        edit
    }

    /// The store the share tests use:
    /// ```text
    /// root ─ private ─ own
    ///      └ r0* ─ a ─ x
    ///            │   └ r2* ─ y
    ///            └ b
    /// ```
    /// `*` carries a share marker; `r2` is a share inside a share.
    struct Shared {
        tree: Tree,
        root: NodeId,
        private: NodeId,
        own: NodeId,
        r0: NodeId,
        a: NodeId,
        b: NodeId,
        x: NodeId,
        r2: NodeId,
        y: NodeId,
    }

    fn shared() -> Shared {
        let (mut tree, root) = tree();
        let private = folder(&mut tree, root, "Private");
        let own = note(&mut tree, private, "Own");
        let r0 = folder(&mut tree, root, "Shared");
        let a = folder(&mut tree, r0, "A");
        let b = folder(&mut tree, r0, "B");
        let x = note(&mut tree, a, "X");
        let r2 = folder(&mut tree, a, "Shared inside");
        let y = note(&mut tree, r2, "Y");
        share(&mut tree, r0);
        share(&mut tree, r2);
        Shared { tree, root, private, own, r0, a, b, x, r2, y }
    }

    #[test]
    fn shares_are_the_markers_among_a_node_and_its_ancestors_nearest_first() {
        let s = shared();
        assert_eq!(s.tree.shares(s.y), vec![s.r2, s.r0]);
        assert_eq!(s.tree.shares(s.r2), vec![s.r2, s.r0], "a share's root is in its own share");
        assert_eq!(s.tree.shares(s.x), vec![s.r0]);
        assert_eq!(s.tree.shares(s.r0), vec![s.r0]);
        assert!(s.tree.shares(s.own).is_empty());
        assert!(s.tree.shares(s.root).is_empty());
        assert!(s.tree.shares(id()).is_empty(), "a document not held is in nothing known");
    }

    #[test]
    fn a_deleted_document_stays_in_its_share() {
        let mut s = shared();
        s.tree.remove_node(s.a, T2).unwrap();
        assert_eq!(s.tree.shares(s.y), vec![s.r2, s.r0], "tombstones are walked through, and a tombstoned root still marks its share");
    }

    #[test]
    fn a_move_inside_a_share_leaves_none_and_a_move_out_leaves_it() {
        let s = shared();
        assert!(!s.tree.leaves_a_share(s.x, s.b), "inside r0");
        assert!(!s.tree.leaves_a_share(s.x, s.r0));
        assert!(!s.tree.leaves_a_share(s.x, s.a), "a reorder under its own parent");
        assert!(s.tree.leaves_a_share(s.x, s.private));
        assert_eq!(s.tree.shares_left(s.x, s.private), vec![s.r0]);
        assert!(s.tree.leaves_a_share(s.x, s.root));
        // Out of the inner share alone, and out of both.
        assert_eq!(s.tree.shares_left(s.y, s.b), vec![s.r2]);
        assert_eq!(s.tree.shares_left(s.y, s.private), vec![s.r2, s.r0]);
        assert!(!s.tree.leaves_a_share(s.own, s.root), "a node in no share leaves none");
    }

    #[test]
    fn entering_a_share_is_not_leaving_one() {
        let s = shared();
        assert!(!s.tree.leaves_a_share(s.own, s.a));
        assert!(!s.tree.leaves_a_share(s.own, s.r2));
        assert!(!s.tree.leaves_a_share(s.x, s.r2), "deeper into a share inside the one it is in");
        assert!(s.tree.shares_left(s.own, s.r2).is_empty());
    }

    #[test]
    fn a_shares_own_root_travels_with_its_share() {
        let s = shared();
        assert!(!s.tree.leaves_a_share(s.r0, s.private), "r0 is in no share but its own");
        assert!(!s.tree.leaves_a_share(s.r2, s.b), "r2 stays inside r0");
        // The consequence the contract names: a share inside a share cannot
        // leave the outer one as itself.
        assert_eq!(s.tree.shares_left(s.r2, s.private), vec![s.r0]);
    }

    #[test]
    fn a_share_inside_the_moved_subtree_does_not_count() {
        let mut s = shared();
        assert!(!s.tree.leaves_a_share(s.a, s.b), "a holds r2 and stays in r0");
        assert_eq!(s.tree.shares_left(s.a, s.private), vec![s.r0], "r0 is left; r2 is under a and travels with it");
        // A private folder that holds a share leaves nothing wherever it goes.
        let holder = folder(&mut s.tree, s.root, "Holder");
        let r3 = folder(&mut s.tree, holder, "Held");
        share(&mut s.tree, r3);
        assert!(!s.tree.leaves_a_share(holder, s.private));
        assert!(!s.tree.leaves_a_share(holder, s.b), "into r0: entering");
    }

    /// A member's device holds the shares and nothing above them. It judges
    /// with what it holds: two shares of one store are two shares, and
    /// whatever is not held is outside everything known.
    #[test]
    fn a_parent_not_held_ends_the_walk() {
        let (mut full, root) = tree();
        let r1 = folder(&mut full, root, "Trips");
        let in_r1 = note(&mut full, r1, "Lisbon");
        let r2 = folder(&mut full, root, "Recipes");
        let sub = folder(&mut full, r2, "Soups");
        let in_r2 = note(&mut full, sub, "Leek");
        share(&mut full, r1);
        share(&mut full, r2);

        let held = [r1, in_r1, r2, sub, in_r2];
        let docs = held.iter().map(|&n| (n, NodeDoc::load(&full.doc(n).unwrap().save()).unwrap())).collect();
        let member = Tree::from_docs(r1, docs);
        assert!(member.doc(root).is_none());
        assert_eq!(member.shares(in_r2), vec![r2], "the walk ends where the store's root would be");
        assert_eq!(member.shares(in_r1), vec![r1]);
        assert!(member.leaves_a_share(in_r1, sub), "from one share into the other");
        assert_eq!(member.shares_left(in_r2, r1), vec![r2]);
        assert!(!member.leaves_a_share(in_r2, r2), "inside one");
        assert!(member.leaves_a_share(in_r1, root), "a parent not held is in no share this device knows");
        assert!(!member.leaves_a_share(root, r1), "and a node not held is in none to leave");

        // An ancestor missing in the middle: what is above it is not known.
        let (mut gap, gap_root) = tree();
        let top = folder(&mut gap, gap_root, "Top");
        let middle = folder(&mut gap, top, "Middle");
        let leaf = note(&mut gap, middle, "Leaf");
        share(&mut gap, top);
        gap.take_doc(middle);
        assert!(gap.shares(leaf).is_empty());
    }

    #[test]
    fn an_unrepaired_cycle_cannot_loop_the_walk() {
        let (mut t, root) = tree();
        let (fa, fb) = (folder(&mut t, root, "A"), folder(&mut t, root, "B"));
        let leaf = note(&mut t, fa, "Leaf");
        share(&mut t, fb);
        tamper_parent(&mut t, fa, Some(fb));
        tamper_parent(&mut t, fb, Some(fa));
        assert_eq!(t.shares(leaf), vec![fb]);
        assert_eq!(t.shares(fb), vec![fb]);
        assert!(!t.leaves_a_share(leaf, fb));
        assert!(t.leaves_a_share(leaf, root));
    }

    // ── `move_or_transplant` and `transplant` ───────────────────────────

    #[test]
    fn move_or_transplant_moves_inside_a_share_and_transplants_out_of_it() {
        let mut s = shared();
        let (now_id, edit) = s.tree.move_or_transplant(s.x, s.b, None, T2, &mut never_asked).unwrap();
        assert_eq!(now_id, s.x, "a plain move keeps the id");
        assert_eq!(ids_of(&edit), HashSet::from([s.a, s.b, s.x]));
        assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.x]);

        let (now_id, _) = s.tree.move_or_transplant(s.own, s.b, Some(0), T2, &mut never_asked).unwrap();
        assert_eq!(now_id, s.own, "entering a share is a plain move");
        assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.own, s.x]);

        let mut made = Vec::new();
        let (now_id, edit) = s.tree.move_or_transplant(s.x, s.private, None, T3, &mut fresh_ids(&mut made)).unwrap();
        assert_eq!(made, vec![now_id], "one node, one new id");
        assert_ne!(now_id, s.x);
        assert_eq!(edit.node_ids(), vec![now_id, s.private, s.x, s.b], "made, listed, tombstoned, unlisted");
        assert!(!s.tree.has_node(s.x));
        assert_eq!(s.tree.get_children(s.private).unwrap(), vec![now_id]);
        assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.own]);
        assert_eq!(s.tree.get_node_info(now_id).unwrap().title, "X");
        assert!(s.tree.validate_tree().is_empty());

        // What `move_node` refuses, this refuses, whichever way it would go.
        assert!(s.tree.move_or_transplant(s.root, s.b, None, T3, &mut never_asked).is_err());
        assert!(s.tree.move_or_transplant(s.a, s.r2, None, T3, &mut never_asked).is_err());
        assert!(s.tree.move_or_transplant(s.y, id(), None, T3, &mut never_asked).is_err());
        assert!(s.tree.has_node(s.y), "a refused transplant deletes nothing");
    }

    #[test]
    fn transplant_makes_the_subtree_again_and_tombstones_the_original() {
        let mut s = shared();
        let mut replica = replica_of(&s.tree);
        // Under `a` already: x, r2 (a share, with y). More: a note deleted
        // earlier, a mount, tags, custom fields, plugin data, text.
        let dead = note(&mut s.tree, s.a, "Deleted earlier");
        s.tree.remove_node(dead, T1).unwrap();
        let mount = id();
        s.tree.add_node(mount, Some(s.a), None, pimble_core::node_types::MOUNT, "Elsewhere", T1).unwrap();
        let mount_ref = serde_json::json!({ "source_store": "6a7e6f52-0000-4000-8000-000000000001", "source_node": null, "source_path": "/stores/other.pimble" });
        s.tree.set_custom(mount, "mount", &mount_ref, T1).unwrap();
        s.tree.set_tags(s.x, &["one".into(), "two".into()], T1).unwrap();
        s.tree.set_custom(s.x, "icon", &serde_json::json!("star"), T1).unwrap();
        s.tree.set_custom(s.x, "explicit_title", &serde_json::json!(true), T1).unwrap();
        s.tree.doc_mut(s.x).unwrap().set_data("plugin", &serde_json::json!({ "n": 3, "list": ["a", { "b": null }] })).unwrap();
        s.tree.doc_mut(s.x).unwrap().replace_plain_text("first line\nsecond line").unwrap();
        s.tree.doc_mut(s.y).unwrap().replace_plain_text("inside the inner share").unwrap();
        sync(&mut s.tree, &mut replica);

        let before = s.tree.subtree_ids(s.a).unwrap();
        assert_eq!(before, vec![s.a, s.x, s.r2, s.y, mount], "preorder, the tombstone not among them");
        let mut made = Vec::new();
        let (new_a, edit) = s.tree.transplant(s.a, s.private, Some(0), T3, &mut fresh_ids(&mut made)).unwrap();

        // New ids, all of them, one per live node in the subtree's preorder.
        assert_eq!(made.len(), before.len());
        assert_eq!(made[0], new_a);
        assert!(made.iter().all(|new| !before.contains(new)));
        let new_of: HashMap<NodeId, NodeId> = before.iter().copied().zip(made.iter().copied()).collect();

        // The edit's order says what happened in what order: every new
        // document, the list that names the new root, then every tombstone
        // and the list the original left.
        let mut expected = made.clone();
        expected.push(s.private);
        expected.extend(&before);
        expected.push(s.r0);
        assert_eq!(edit.node_ids(), expected);

        // The same nodes again, under the new parent, in the same order.
        assert_eq!(s.tree.get_children(s.private).unwrap(), vec![new_a, s.own]);
        assert_eq!(s.tree.subtree_ids(new_a).unwrap(), made);
        for (&old, &new) in &new_of {
            let (was, is) = (s.tree.doc(old).unwrap().fields().unwrap(), s.tree.get_node_info(new).unwrap());
            assert_eq!((&is.node_type, &is.title, &is.tags), (&was.node_type, &was.title, &was.tags), "{}", was.title);
            assert_eq!(is.created_at, was.created_at, "created when the original was");
            assert_eq!(is.modified_at, T3, "modified now");
            let expected_parent = if old == s.a { s.private } else { new_of[&was.parent_id.unwrap()] };
            assert_eq!(is.parent_id, Some(expected_parent));
            // Every custom field but the share marker, on every node.
            let mut custom = was.custom.clone();
            custom.remove(SHARE);
            assert_eq!(is.custom, custom, "{}", was.title);
            assert_eq!(s.tree.doc(new).unwrap().data_json(), s.tree.doc(old).unwrap().data_json());
            assert_eq!(s.tree.doc(new).unwrap().text(), s.tree.doc(old).unwrap().text());
        }
        assert!(s.tree.shares(new_of[&s.y]).is_empty(), "no share marker survives: the new nodes are in no share");
        assert_eq!(s.tree.doc(new_of[&s.x]).unwrap().text(), "first line\nsecond line");
        assert_eq!(s.tree.get_node_info(new_of[&s.x]).unwrap().custom.get("icon"), Some(&serde_json::json!("star")));
        // A mount is copied as the reference it is.
        let new_mount = s.tree.get_node_info(new_of[&mount]).unwrap();
        assert_eq!(new_mount.node_type, pimble_core::node_types::MOUNT);
        assert_eq!(new_mount.custom.get("mount"), Some(&mount_ref));

        // The original: an ordinary delete. Tombstones with one stamp, lists
        // below kept, the marker still on the inner share's root, and the
        // note deleted earlier neither copied nor restamped.
        for &old in &before {
            assert!(!s.tree.has_node(old));
            assert_eq!(s.tree.doc(old).unwrap().fields().unwrap().deleted_at.as_deref(), Some(T3));
        }
        assert_eq!(s.tree.doc(dead).unwrap().fields().unwrap().deleted_at.as_deref(), Some(T1));
        assert!(s.tree.doc(s.r2).unwrap().fields().unwrap().custom.contains_key(SHARE));
        assert_eq!(s.tree.doc(s.a).unwrap().children(), vec![s.x, s.r2, mount], "an undelete finds the subtree again");
        assert_eq!(s.tree.get_children(s.r0).unwrap(), vec![s.b]);
        assert_eq!(s.tree.list_node_ids().len(), 5 + before.len(), "root, private, own, r0, b and the new nodes");
        assert!(s.tree.validate_tree().is_empty());
        assert!(s.tree.repair(T3).unwrap().is_none());

        // A peer that applies the edit holds the same.
        apply_edit(&mut replica, &edit);
        assert_same(&s.tree, &replica);
        assert!(replica.validate_tree().is_empty());
    }

    #[test]
    fn undeleting_the_original_after_a_transplant_leaves_both() {
        let mut s = shared();
        s.tree.doc_mut(s.y).unwrap().replace_plain_text("kept").unwrap();
        let mut made = Vec::new();
        let (new_a, _) = s.tree.transplant(s.a, s.private, None, T2, &mut fresh_ids(&mut made)).unwrap();

        let edit = s.tree.undelete_node(s.a, T3).unwrap();
        assert_eq!(ids_of(&edit), HashSet::from([s.a, s.x, s.r2, s.y, s.r0]));
        // The members' folder is back where it was, a share inside it still
        // a share; the new one is where it landed; they go their own ways.
        assert_eq!(s.tree.get_children(s.r0).unwrap(), vec![s.b, s.a]);
        assert_eq!(s.tree.subtree_ids(s.a).unwrap(), vec![s.a, s.x, s.r2, s.y]);
        assert_eq!(s.tree.shares(s.y), vec![s.r2, s.r0]);
        assert_eq!(s.tree.subtree_ids(new_a).unwrap(), made);
        assert!(s.tree.shares(*made.last().unwrap()).is_empty());
        s.tree.set_title(s.x, "Theirs", T3).unwrap();
        assert_eq!(s.tree.get_node_info(made[1]).unwrap().title, "X");
        assert!(s.tree.validate_tree().is_empty());
    }

    #[test]
    fn transplant_refuses_before_it_writes_anything() {
        let mut s = shared();
        let before = snapshot(&s.tree);
        let mut no_ids = || -> NodeId { panic!("nothing should be made") };
        assert!(s.tree.transplant(s.root, s.private, None, T2, &mut no_ids).is_err(), "the root");
        assert!(s.tree.transplant(s.a, s.r2, None, T2, &mut no_ids).is_err(), "under its own descendant");
        assert!(s.tree.transplant(s.a, s.a, None, T2, &mut no_ids).is_err(), "under itself");
        assert!(s.tree.transplant(s.a, id(), None, T2, &mut no_ids).is_err(), "under nothing");
        assert!(s.tree.transplant(id(), s.private, None, T2, &mut no_ids).is_err(), "nothing");
        assert!(s.tree.take_cutting(s.root).is_err());
        // An id already held, and the same id twice.
        let mut held = || s.own;
        assert!(s.tree.transplant(s.a, s.private, None, T2, &mut held).is_err());
        let twice = id();
        let mut same = || twice;
        assert!(s.tree.transplant(s.a, s.private, None, T2, &mut same).is_err());
        assert_eq!(snapshot(&s.tree), before, "refused means untouched");
        assert!(s.tree.doc(twice).is_none());

        // Deleted already: not a node.
        s.tree.remove_node(s.x, T2).unwrap();
        assert!(s.tree.transplant(s.x, s.private, None, T3, &mut no_ids).is_err());
    }

    /// Content rinch's projection refuses (it fails loudly by design on what
    /// is outside the collaboration scope) stops the transplant: better none
    /// than one that loses the text and then deletes the note.
    #[test]
    fn content_that_cannot_be_projected_refuses_the_transplant() {
        use yrs::{Array, Map, ReadTxn, Transact};
        let mut s = shared();
        // The format tag, and a `content` entry that is no projected node.
        let junk = yrs::Doc::with_options(yrs::Options { offset_kind: yrs::OffsetKind::Utf16, ..Default::default() });
        let (meta, content) = (junk.get_or_insert_map("meta"), junk.get_or_insert_array("content"));
        {
            let mut txn = junk.transact_mut();
            meta.insert(&mut txn, "format", "rinch-editor-collab/yrs-1");
            content.insert(&mut txn, 0, "not a block");
        }
        let bytes = junk.transact().encode_state_as_update_v1(&yrs::StateVector::default());
        s.tree.apply_update(s.y, &bytes).unwrap();

        let before = snapshot(&s.tree);
        let mut made = Vec::new();
        let err = s.tree.transplant(s.a, s.private, None, T2, &mut fresh_ids(&mut made)).unwrap_err();
        assert!(matches!(err, CrdtError::Collab(_)), "{err}");
        assert_eq!(snapshot(&s.tree), before);
        assert!(s.tree.has_node(s.a) && s.tree.has_node(s.y));
    }

    /// Between two stores: read out of one, planted in the other, deleted in
    /// the first, in that order; one `TreeEdit` per store.
    #[test]
    fn a_transplant_between_two_trees_is_a_cutting_planted_and_a_delete() {
        let mut s = shared();
        s.tree.doc_mut(s.y).unwrap().replace_plain_text("carried across").unwrap();
        s.tree.set_tags(s.y, &["t".into()], T1).unwrap();
        let (mut other, other_root) = tree();
        let landing = folder(&mut other, other_root, "Landing");
        let mut other_replica = replica_of(&other);

        let cutting = s.tree.take_cutting(s.a).unwrap();
        assert_eq!(cutting.len(), 4);
        assert!(!cutting.is_empty());
        let sources: Vec<NodeId> = cutting.nodes().iter().map(|n| n.source_id).collect();
        assert_eq!(sources, s.tree.subtree_ids(s.a).unwrap());
        assert_eq!(cutting.nodes().iter().map(|n| n.parent).collect::<Vec<_>>(), vec![None, Some(0), Some(0), Some(2)]);
        assert!(cutting.nodes().iter().all(|n| !n.custom.contains_key(SHARE)), "the marker is not even read out");
        assert!(cutting.nodes()[3].has_content() && !cutting.nodes()[0].has_content());
        assert!(s.tree.has_node(s.a), "reading changes nothing");

        let mut made = Vec::new();
        let (new_a, planted) = other.plant(cutting, landing, None, T2, &mut fresh_ids(&mut made)).unwrap();
        let mut expected = made.clone();
        expected.push(landing);
        assert_eq!(planted.node_ids(), expected);
        let removed = s.tree.remove_node(s.a, T2).unwrap();
        assert_eq!(removed.node_ids(), vec![s.a, s.x, s.r2, s.y, s.r0]);

        assert_eq!(other.get_children(landing).unwrap(), vec![new_a]);
        assert_eq!(other.subtree_ids(new_a).unwrap(), made);
        let new_y = other.get_node_info(made[3]).unwrap();
        assert_eq!((new_y.title.as_str(), new_y.parent_id, new_y.tags.clone()), ("Y", Some(made[2]), vec!["t".to_string()]));
        assert_eq!(other.doc(made[3]).unwrap().text(), "carried across");
        assert!(other.shares(made[3]).is_empty());
        assert!(other.validate_tree().is_empty() && s.tree.validate_tree().is_empty());
        assert!(!s.tree.has_node(s.y));

        apply_edit(&mut other_replica, &planted);
        assert_same(&other, &other_replica);

        // The second half refuses what `add_node` refuses.
        let again = s.tree.take_cutting(s.b).unwrap();
        assert!(other.plant(again, id(), None, T2, &mut NodeId::new).is_err(), "under nothing");
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|window| window == needle.as_bytes())
    }

    fn clients_of(doc: &NodeDoc) -> HashSet<yrs::ClientID> {
        use yrs::updates::decoder::Decode;
        yrs::StateVector::decode_v1(&doc.state_vector()).unwrap().iter().map(|(client, _)| *client).collect()
    }

    /// The new document is a NEW yrs document: no struct of the old one, no
    /// deletion, nothing that was ever taken out of the old one.
    #[test]
    fn a_transplanted_document_has_its_own_history_and_nothing_that_was_deleted() {
        use yrs::updates::decoder::Decode;
        let mut s = shared();
        let mut others = replica_of(&s.tree);
        let key_id = marker("")["key_id"].as_str().unwrap().to_string();
        // A share's root with a past: a title it no longer has, text that was
        // deleted, a custom field that was removed, plugin data replaced.
        s.tree.set_title(s.r2, "WITHDRAWN-TITLE", T1).unwrap();
        s.tree.set_title(s.r2, "Recipes", T1).unwrap();
        s.tree.doc_mut(s.r2).unwrap().replace_plain_text("keep this\nand the REDACTED-PARAGRAPH").unwrap();
        s.tree.doc_mut(s.r2).unwrap().replace_plain_text("keep this").unwrap();
        s.tree.set_custom(s.r2, "note", &serde_json::json!("RETRACTED-FIELD"), T1).unwrap();
        s.tree.remove_custom(s.r2, "note", T1).unwrap();
        s.tree.doc_mut(s.r2).unwrap().set_data("k", &serde_json::json!("REPLACED-DATA")).unwrap();
        s.tree.doc_mut(s.r2).unwrap().set_data("k", &serde_json::json!("data now")).unwrap();
        assert!(contains(&s.tree.doc(s.r2).unwrap().save(), &key_id), "the original carries its marker");

        let mut made = Vec::new();
        let (new_r2, _) = s.tree.transplant(s.r2, s.private, None, T2, &mut fresh_ids(&mut made)).unwrap();
        let new_doc = s.tree.doc(new_r2).unwrap();
        assert_eq!(new_doc.text(), "keep this");
        assert_eq!(new_doc.data_json(), serde_json::json!({ "k": "data now" }));
        let bytes = new_doc.save();
        for gone in ["WITHDRAWN-TITLE", "REDACTED-PARAGRAPH", "RETRACTED-FIELD", "REPLACED-DATA", key_id.as_str(), "pimble.example"] {
            assert!(!contains(&bytes, gone), "{gone} is in the new document's bytes");
        }
        assert!(contains(&bytes, "keep this") && contains(&bytes, "Recipes"));
        // Nothing was ever deleted in it, and no client that wrote the old
        // document wrote any of it.
        let update = yrs::Update::decode_v1(&bytes).unwrap();
        assert!(update.delete_set().iter().all(|(_, ranges)| ranges.is_empty()), "a new document has no deletions");
        assert!(clients_of(new_doc).is_disjoint(&clients_of(s.tree.doc(s.r2).unwrap())));

        // So an edit of the one, misdirected at the other, merges into
        // nothing: it continues a history the other does not have.
        sync(&mut s.tree, &mut others);
        let theirs = others.doc_mut(s.r2).unwrap().replace_plain_text("the members' text moves on").unwrap();
        let mut misdirected = NodeDoc::load(&bytes).unwrap();
        misdirected.apply_update(&theirs).unwrap();
        assert_eq!(misdirected.text(), "keep this");
        assert_eq!(misdirected.fields().unwrap().title, "Recipes");
    }

    #[test]
    fn a_node_with_no_content_yet_is_transplanted_with_none() {
        let mut s = shared();
        assert_eq!(s.tree.doc(s.x).unwrap().text(), "");
        let (new_x, _) = s.tree.transplant(s.x, s.private, None, T2, &mut NodeId::new).unwrap();
        // No projection was made up for it: its first edit seeds it, as for
        // any node whose content was never written.
        let mut peer = NodeDoc::load(&s.tree.doc(new_x).unwrap().save()).unwrap();
        let seed = s.tree.doc_mut(new_x).unwrap().replace_plain_text("first words").unwrap();
        peer.apply_update(&seed).unwrap();
        assert_eq!(peer.text(), "first words");
    }

    // ── The list wins (docs/MOVE_CONTRACT.md "Repair") ──────────────────

    fn leaves_share_issue(issues: &[TreeIssue], list: NodeId, node: NodeId, stored: Option<NodeId>) -> bool {
        issues.iter().any(|i| matches!(i, TreeIssue::LeavesShare { parent_id, child_id, stored_parent } if *parent_id == list && *child_id == node && *stored_parent == stored))
    }

    #[test]
    fn a_list_that_still_names_a_node_wins_over_a_parent_id_pointing_out_of_its_share() {
        let mut s = shared();
        tamper_parent(&mut s.tree, s.x, Some(s.private));

        let issues = s.tree.validate_tree();
        assert!(leaves_share_issue(&issues, s.a, s.x, Some(s.private)), "{issues:?}");
        assert_eq!(issues.len(), 1, "and nothing else: the list is right, so it is not touched: {issues:?}");
        let repair = s.tree.repair(T3).unwrap().expect("the parent_id needs rewriting");
        assert_eq!(repair.node_ids(), vec![s.x], "the node's own document, and no list");
        assert_eq!(s.tree.get_node_info(s.x).unwrap().parent_id, Some(s.a));
        assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.x, s.r2]);
        assert_eq!(s.tree.get_children(s.private).unwrap(), vec![s.own], "it never showed up outside");
        assert_eq!(s.tree.get_node_info(s.x).unwrap().modified_at, T1, "repair never touches a timestamp");
        assert!(s.tree.validate_tree().is_empty());
        assert!(s.tree.repair(T3).unwrap().is_none());

        // Out of the inner share alone is out of a share too.
        tamper_parent(&mut s.tree, s.y, Some(s.b));
        assert!(leaves_share_issue(&s.tree.validate_tree(), s.r2, s.y, Some(s.b)));
        s.tree.repair(T3).unwrap().unwrap();
        assert_eq!(s.tree.get_children(s.r2).unwrap(), vec![s.y]);
        assert!(s.tree.get_children(s.b).unwrap().is_empty());
        assert!(s.tree.validate_tree().is_empty());
    }

    #[test]
    fn both_devices_decide_the_same_about_a_tampered_parent_id() {
        let mut s = shared();
        let mut other = replica_of(&s.tree);
        let mut tamperer = replica_of(&s.tree);
        let tampering = tamper_parent(&mut tamperer, s.x, Some(s.private));
        apply_edit(&mut s.tree, &tampering);
        apply_edit(&mut other, &tampering);

        // Each repairs what it holds before it hears from the other.
        let (ours, theirs) = (s.tree.repair(T3).unwrap().unwrap(), other.repair(T3).unwrap().unwrap());
        assert_eq!(ours.node_ids(), vec![s.x]);
        assert_eq!(theirs.node_ids(), vec![s.x]);
        apply_edit(&mut other, &ours);
        apply_edit(&mut s.tree, &theirs);
        sync_and_repair_round(&mut s.tree, &mut other);
        assert_converged(&mut s.tree, &mut other);
        assert_eq!(other.get_node_info(s.x).unwrap().parent_id, Some(s.a));
        assert_eq!(other.get_children(s.a).unwrap(), vec![s.x, s.r2]);

        // The tamperer's own device, once it runs an honest repair, agrees.
        sync_and_repair_round(&mut s.tree, &mut tamperer);
        sync_and_repair_round(&mut s.tree, &mut tamperer);
        assert_converged(&mut s.tree, &mut tamperer);
        assert_eq!(tamperer.get_children(s.private).unwrap(), vec![s.own]);
    }

    /// The same hole by another door: a `parent_id` that names a tombstone,
    /// the node itself, or nothing sends a node under the root, which is as
    /// far out of the share as any folder.
    #[test]
    fn a_parent_id_that_would_send_a_listed_node_to_the_root_loses_to_the_list_too() {
        let mut s = shared();
        let dead = note(&mut s.tree, s.b, "Deleted");
        s.tree.remove_node(dead, T2).unwrap();
        for stored in [Some(dead), Some(s.x), None] {
            tamper_parent(&mut s.tree, s.x, stored);
            let issues = s.tree.validate_tree();
            assert!(leaves_share_issue(&issues, s.a, s.x, stored), "{stored:?}: {issues:?}");
            assert_eq!(issues.len(), 1, "reported instead of the orphan or detached node it would be: {issues:?}");
            let repair = s.tree.repair(T3).unwrap().unwrap();
            assert_eq!(repair.node_ids(), vec![s.x]);
            assert_eq!(s.tree.get_node_info(s.x).unwrap().parent_id, Some(s.a));
            assert_eq!(s.tree.get_children(s.root).unwrap(), vec![s.private, s.r0]);
            assert!(s.tree.validate_tree().is_empty());
        }
        // Outside every share the orphan goes under the root, as ever.
        tamper_parent(&mut s.tree, s.own, Some(dead));
        assert!(s.tree.validate_tree().iter().any(|i| matches!(i, TreeIssue::OrphanNode { node_id, .. } if *node_id == s.own)));
        s.tree.repair(T3).unwrap().unwrap();
        assert_eq!(s.tree.get_node_info(s.own).unwrap().parent_id, Some(s.root));
    }

    #[test]
    fn a_plain_move_in_flight_inside_one_share_is_still_completed() {
        // The node's document arrived; neither list's did.
        let mut s = shared();
        tamper_parent(&mut s.tree, s.x, Some(s.b));
        let issues = s.tree.validate_tree();
        assert!(issues.iter().any(|i| matches!(i, TreeIssue::WrongList { parent_id, child_id, effective_parent } if *parent_id == s.a && *child_id == s.x && *effective_parent == s.b)), "{issues:?}");
        assert!(issues.iter().any(|i| matches!(i, TreeIssue::MissingFromList { parent_id, child_id } if *parent_id == s.b && *child_id == s.x)), "{issues:?}");
        let repair = s.tree.repair(T3).unwrap().unwrap();
        assert_eq!(ids_of(&repair), HashSet::from([s.a, s.b]));
        assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.x]);
        assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.r2]);
        assert!(s.tree.validate_tree().is_empty());

        // Into the share inside it, and into a share from outside: entering.
        tamper_parent(&mut s.tree, s.x, Some(s.r2));
        tamper_parent(&mut s.tree, s.own, Some(s.b));
        s.tree.repair(T3).unwrap().unwrap();
        assert_eq!(s.tree.get_children(s.r2).unwrap(), vec![s.y, s.x]);
        assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.own]);
        assert!(s.tree.get_children(s.private).unwrap().is_empty());
        assert!(s.tree.validate_tree().is_empty());

        // And as two devices see it: the mover's three updates arrive at the
        // other one document at a time, a repair after each.
        let mut s = shared();
        let mut other = replica_of(&s.tree);
        let moved = s.tree.move_node(s.x, s.b, None, T2).unwrap();
        for (doc_id, update) in moved.touched.iter().rev() {
            other.apply_update(*doc_id, update).unwrap();
            if let Some(repair) = other.repair(T3).unwrap() {
                apply_edit(&mut s.tree, &repair);
            }
        }
        sync_and_repair_round(&mut s.tree, &mut other);
        assert_converged(&mut s.tree, &mut other);
        assert_eq!(other.get_children(s.b).unwrap(), vec![s.x]);
    }

    #[test]
    fn a_node_named_by_no_list_is_adopted_where_its_parent_id_says() {
        // What remains (the contract says so): a client that also unlists
        // the node. No list names it, so there is nothing to win; it is
        // adopted as before, still in the share's scope, and any editor can
        // put it back.
        let mut s = shared();
        tamper_parent(&mut s.tree, s.x, Some(s.private));
        s.tree.doc_mut(s.a).unwrap().remove_child(s.x).unwrap();
        let issues = s.tree.validate_tree();
        assert!(issues.iter().all(|i| matches!(i, TreeIssue::MissingFromList { parent_id, child_id } if *parent_id == s.private && *child_id == s.x)), "{issues:?}");
        let repair = s.tree.repair(T3).unwrap().unwrap();
        assert_eq!(repair.node_ids(), vec![s.private]);
        assert_eq!(s.tree.get_children(s.private).unwrap(), vec![s.own, s.x]);
        assert!(s.tree.validate_tree().is_empty());
        // Putting it back is a plain move: it enters the share.
        let (back, _) = s.tree.move_or_transplant(s.x, s.a, None, T3, &mut never_asked).unwrap();
        assert_eq!(back, s.x);
    }

    /// The 2026-09-21 rule, for the exception too: whether the destination is
    /// outside the share is only known when everything above it is held.
    #[test]
    fn a_destination_whose_ancestors_are_not_all_held_is_not_judged() {
        for lands_inside in [true, false] {
            let mut s = shared();
            let mut holds_all = replica_of(&s.tree);
            // Someone made a folder and a folder in it, and the node says it
            // is in the inner one. Here: the inner folder, and not the outer.
            let outer = id();
            let made_outer = holds_all.add_node(outer, Some(if lands_inside { s.b } else { s.private }), None, "folder", "Outer", T2).unwrap();
            let inner = id();
            let made_inner = holds_all.add_node(inner, Some(outer), None, "folder", "Inner", T2).unwrap();
            let moved = tamper_parent(&mut holds_all, s.x, Some(inner));
            for (doc_id, update) in made_inner.touched.iter().chain(&moved.touched) {
                if *doc_id != outer {
                    s.tree.apply_update(*doc_id, update).unwrap();
                }
            }
            assert!(s.tree.validate_tree().is_empty(), "{:?}", s.tree.validate_tree());
            assert!(s.tree.repair(T3).unwrap().is_none(), "not known, so not decided");
            assert_eq!(s.tree.doc(s.a).unwrap().children(), vec![s.x, s.r2], "the list still names it");
            assert_eq!(s.tree.get_node_info(s.x).unwrap().parent_id, Some(inner), "and its parent_id is as it came");

            // The outer folder arrives, and with it the answer.
            apply_edit(&mut s.tree, &made_outer);
            apply_edit(&mut s.tree, &made_inner);
            s.tree.repair(T3).unwrap().expect("now it is known");
            assert!(s.tree.validate_tree().is_empty());
            if lands_inside {
                assert_eq!(s.tree.get_children(inner).unwrap(), vec![s.x], "inside the share: a move, completed");
                assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.r2]);
            } else {
                assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.x, s.r2], "outside: the list wins");
                assert!(s.tree.get_children(inner).unwrap().is_empty());
            }
            // The device that held everything all along decides the same.
            holds_all.repair(T3).unwrap().unwrap();
            sync_and_repair_round(&mut s.tree, &mut holds_all);
            assert_converged(&mut s.tree, &mut holds_all);
        }
    }

    /// A scope is repaired as a tree of its own, rooted at the share's root,
    /// whose `parent_id` names a document a member never holds. The root is
    /// the top: what is under it is known to be in it.
    #[test]
    fn in_a_scopes_tree_the_root_is_the_top() {
        let s = shared();
        let held = [s.r0, s.a, s.b, s.x, s.r2, s.y];
        let docs = held.iter().map(|&n| (n, NodeDoc::load(&s.tree.doc(n).unwrap().save()).unwrap())).collect();
        let mut scope = Tree::from_docs(s.r0, docs);
        assert!(scope.validate_tree().is_empty());
        // Out of the inner share, inside the outer: known, so the list wins.
        tamper_parent(&mut scope, s.y, Some(s.b));
        assert!(leaves_share_issue(&scope.validate_tree(), s.r2, s.y, Some(s.b)));
        scope.repair(T3).unwrap().unwrap();
        assert_eq!(scope.get_children(s.r2).unwrap(), vec![s.y]);
        // Inside the outer share: a move, completed.
        tamper_parent(&mut scope, s.x, Some(s.b));
        scope.repair(T3).unwrap().unwrap();
        assert_eq!(scope.get_children(s.b).unwrap(), vec![s.x]);
        // Out of everything this device holds: not held, not judged.
        tamper_parent(&mut scope, s.x, Some(s.private));
        assert!(scope.validate_tree().is_empty());
        assert!(scope.repair(T3).unwrap().is_none());
        assert_eq!(scope.doc(s.b).unwrap().children(), vec![s.x]);
    }

    #[test]
    fn of_two_lists_the_one_that_keeps_the_node_in_more_shares_wins() {
        // Both `a` (in r0) and `r2` (in r2 and r0) name the node; its
        // parent_id points outside both.
        let mut s = shared();
        s.tree.doc_mut(s.a).unwrap().insert_child(99, s.y).unwrap();
        tamper_parent(&mut s.tree, s.y, Some(s.private));
        assert!(leaves_share_issue(&s.tree.validate_tree(), s.r2, s.y, Some(s.private)));
        let repair = s.tree.repair(T3).unwrap().unwrap();
        assert_eq!(ids_of(&repair), HashSet::from([s.y, s.a]), "the parent_id, and the other list's entry");
        assert_eq!(s.tree.get_children(s.r2).unwrap(), vec![s.y]);
        assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.x, s.r2]);
        assert!(s.tree.validate_tree().is_empty());

        // Two shares side by side: the first list in id order, on every device.
        let (mut t, root) = tree();
        let (r1, r3) = (folder(&mut t, root, "One"), folder(&mut t, root, "Other"));
        let elsewhere = folder(&mut t, root, "Elsewhere");
        let n = note(&mut t, r1, "N");
        share(&mut t, r1);
        share(&mut t, r3);
        t.doc_mut(r3).unwrap().insert_child(0, n).unwrap();
        tamper_parent(&mut t, n, Some(elsewhere));
        let mut other = replica_of(&t);
        t.repair(T3).unwrap().unwrap();
        other.repair(T3).unwrap().unwrap();
        let winner = if r1.to_string() < r3.to_string() { r1 } else { r3 };
        assert_eq!(t.get_node_info(n).unwrap().parent_id, Some(winner));
        assert_eq!(other.get_node_info(n).unwrap().parent_id, Some(winner));
        sync_and_repair_round(&mut t, &mut other);
        assert_converged(&mut t, &mut other);
    }

    #[test]
    fn a_list_under_the_node_itself_claims_nothing() {
        // `a`'s parent_id points out of the share, and the only list that
        // names `a` is one under it. Making that list its parent would close
        // a cycle, so there is nothing to win: the move is completed.
        let mut s = shared();
        s.tree.doc_mut(s.r0).unwrap().remove_child(s.a).unwrap();
        s.tree.doc_mut(s.r2).unwrap().insert_child(0, s.a).unwrap();
        tamper_parent(&mut s.tree, s.a, Some(s.private));
        let issues = s.tree.validate_tree();
        assert!(!issues.iter().any(|i| matches!(i, TreeIssue::LeavesShare { .. } | TreeIssue::Cycle { .. })), "{issues:?}");
        s.tree.repair(T3).unwrap().unwrap();
        assert_eq!(s.tree.get_children(s.private).unwrap(), vec![s.own, s.a]);
        assert_eq!(s.tree.get_children(s.r2).unwrap(), vec![s.y]);
        assert!(s.tree.validate_tree().is_empty());
    }

    // ── Randomized convergence over three replicas ──────────────────────

    /// xorshift64*: enough randomness for a scripted network, no dependency.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Rng(seed.max(1))
        }

        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545F4914F6CDD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
            (!items.is_empty()).then(|| items[self.below(items.len())])
        }

        /// A node id from the generator, so a seed names the same ids every run.
        fn node_id(&mut self) -> NodeId {
            let (a, b) = (self.next(), self.next());
            NodeId::parse(&format!(
                "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
                a >> 32,
                (a >> 16) & 0xffff,
                a & 0xfff,
                (b >> 48) & 0xfff,
                b & 0xffff_ffff_ffff
            ))
            .unwrap()
        }
    }

    /// A queue's key: sender, receiver, document (as a string, for ordering).
    type Lane = (usize, usize, String);

    /// Per (sender, receiver, document) queues: in order per sender per
    /// document, any order across documents and senders (yrs 0.27 does not
    /// reliably integrate one sender's updates delivered out of order).
    #[derive(Default)]
    struct Network {
        queues: BTreeMap<Lane, VecDeque<(NodeId, Vec<u8>)>>,
    }

    impl Network {
        fn broadcast(&mut self, from: usize, replicas: usize, edit: &TreeEdit) {
            for to in (0..replicas).filter(|&r| r != from) {
                for (id, update) in &edit.touched {
                    self.queues.entry((from, to, id.to_string())).or_default().push_back((*id, update.clone()));
                }
            }
        }

        fn is_empty(&self) -> bool {
            self.queues.values().all(VecDeque::is_empty)
        }

        /// Deliver the next update of one random non-empty queue, then repair
        /// the receiver when the update touched its tree and broadcast that
        /// repair. Whether anything was delivered.
        fn deliver_one(&mut self, rng: &mut Rng, replicas: &mut [Tree], stats: &mut Stats) -> bool {
            let live: Vec<&Lane> = self.queues.iter().filter(|(_, q)| !q.is_empty()).map(|(k, _)| k).collect();
            let Some(&key) = live.get(rng.below(live.len().max(1))) else { return false };
            let key = key.clone();
            let (id, update) = self.queues.get_mut(&key).unwrap().pop_front().unwrap();
            let to = key.1;
            let effect = replicas[to].apply_update(id, &update).unwrap();
            stats.delivered += 1;
            if effect.structure {
                if let Some(repair) = checked_repair(&mut replicas[to], "repair", stats) {
                    stats.repairs += 1;
                    self.broadcast(to, replicas.len(), &repair);
                }
            }
            true
        }
    }

    /// A document's ancestors by stored `parent_id`s as the oracle reads
    /// them (through `fields()`, not through the tree's own walk), and
    /// whether the walk ended knowing everything above its start.
    fn chain_of(tree: &Tree, start: NodeId) -> (Vec<NodeId>, bool) {
        let mut chain = Vec::new();
        let mut cur = start;
        loop {
            if chain.contains(&cur) {
                return (chain, true);
            }
            let Some(fields) = tree.doc(cur).and_then(|doc| doc.fields().ok()) else { return (chain, false) };
            chain.push(cur);
            match fields.parent_id.filter(|_| cur != tree.root()) {
                Some(parent) => cur = parent,
                None => return (chain, true),
            }
        }
    }

    fn carries_marker(tree: &Tree, id: NodeId) -> bool {
        tree.doc(id).and_then(|doc| doc.fields().ok()).is_some_and(|fields| fields.custom.contains_key(SHARE))
    }

    /// Per live node, the live nodes whose stored list names it.
    fn naming_lists(tree: &Tree) -> HashMap<NodeId, Vec<NodeId>> {
        let mut naming: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for owner in sorted(tree.list_node_ids()) {
            for child in tree.doc(owner).unwrap().children() {
                if tree.has_node(child) && !naming.get(&child).is_some_and(|owners| owners.contains(&owner)) {
                    naming.entry(child).or_default().push(owner);
                }
            }
        }
        naming
    }

    /// [`Tree::repair`] under the move contract's oracle: **a repair never
    /// lists a node under a parent outside a share while a list in that share
    /// names it.** Shares are judged as the contract says, on the documents
    /// held when the repair runs. Two ways to break it, both checked:
    /// the repair newly listed the node outside the share; or the node was in
    /// two lists and the repair kept the outside one (allowed only when that
    /// one is in a share the losing list is outside of: two shares cannot
    /// both keep it). Breaking a cycle is its own rule and is left out.
    fn checked_repair(tree: &mut Tree, now: &str, stats: &mut Stats) -> Option<TreeEdit> {
        let issues = tree.validate_tree();
        let in_a_cycle: HashSet<NodeId> = issues.iter().filter_map(|i| if let TreeIssue::Cycle { node_ids } = i { Some(node_ids.clone()) } else { None }).flatten().collect();
        stats.list_wins += issues.iter().filter(|i| matches!(i, TreeIssue::LeavesShare { .. })).count();
        let before = naming_lists(tree);
        let chains: HashMap<NodeId, (Vec<NodeId>, bool)> = tree.list_node_ids().into_iter().map(|n| (n, chain_of(tree, n))).collect();
        let shares_of = |list: NodeId| -> Vec<NodeId> { chains[&list].0.iter().copied().filter(|&member| carries_marker(tree, member)).collect() };
        // (node, a list that names it, the shares that list is in)
        let mut facts: Vec<(NodeId, NodeId, Vec<NodeId>)> = Vec::new();
        for (&node, lists) in &before {
            for &list in lists {
                if !chains[&list].0.contains(&node) && !in_a_cycle.contains(&node) {
                    facts.push((node, list, shares_of(list)));
                }
            }
        }
        let competing: HashMap<(NodeId, NodeId), bool> = facts
            .iter()
            .flat_map(|(node, list, _)| before[node].iter().map(move |other| (*list, *other)))
            .map(|(list, other)| ((list, other), shares_of(other).iter().any(|share| !chains[&list].0.contains(share))))
            .collect();

        let repair = tree.repair(now).unwrap();
        if repair.is_some() {
            let after = naming_lists(tree);
            for (node, list, shares) in &facts {
                let Some(now_in) = after.get(node) else { continue };
                for holder in now_in.iter().filter(|holder| *holder != list) {
                    let (chain, complete) = &chains[holder];
                    let Some(share) = shares.iter().find(|share| !chain.contains(share)) else { continue };
                    if !complete {
                        continue;
                    }
                    let newly = !before[node].contains(holder);
                    let kept_instead = !newly && !now_in.contains(list) && !competing[&(*list, *holder)];
                    assert!(!newly && !kept_instead, "repair listed {node} under {holder}, outside share {share}, while {list} named it (newly: {newly})");
                }
            }
        }
        repair
    }

    #[derive(Default, Debug)]
    struct Stats {
        delivered: usize,
        repairs: usize,
        adds: usize,
        moves: usize,
        removes: usize,
        undeletes: usize,
        renames: usize,
        content_edits: usize,
        refused: usize,
        shares: usize,
        unshares: usize,
        transplants: usize,
        old_client_moves: usize,
        tampers: usize,
        list_wins: usize,
    }

    fn sorted(mut ids: Vec<NodeId>) -> Vec<NodeId> {
        ids.sort_by_key(|id| id.to_string());
        ids
    }

    fn run_convergence(seed: u64, steps: usize) -> Stats {
        // Every document's client id follows the seed too, so a seed names
        // one trajectory: which concurrent inserts win is decided by client
        // id inside yrs.
        crate::node_doc::seed_client_ids(seed * 1_000_000 + 1);
        const REPLICAS: usize = 3;
        let mut rng = Rng::new(seed);
        let root = rng.node_id();
        let first = Tree::new(root, "Fuzz", "t0").unwrap();
        let mut replicas: Vec<Tree> = (0..REPLICAS).map(|_| replica_of(&first)).collect();
        let mut net = Network::default();
        let mut stats = Stats::default();
        let mut created: Vec<NodeId> = Vec::new();

        for step in 0..steps {
            let now = format!("2026-09-18T00:00:00.{step:06}Z");
            let r = rng.below(REPLICAS);
            // A third of the steps deliver something in flight; when nothing
            // is, the step makes an edit instead.
            if rng.below(100) < 35 && net.deliver_one(&mut rng, &mut replicas, &mut stats) {
                continue;
            }
            let tree = &mut replicas[r];
            let nodes = sorted(tree.list_node_ids());
            let non_root: Vec<NodeId> = nodes.iter().copied().filter(|&n| n != root).collect();
            let dead = |tree: &Tree| -> Vec<NodeId> {
                sorted(tree.ids()).into_iter().filter(|&id| tree.doc(id).unwrap().fields().ok().is_some_and(|f| f.deleted_at.is_some())).collect()
            };
            let roll = rng.below(100);
            let result = if roll < 24 {
                let id = rng.node_id();
                let parent = rng.pick(&nodes).unwrap();
                let position = if rng.below(2) == 0 { None } else { Some(rng.below(4)) };
                let edit = tree.add_node(id, Some(parent), position, "document", &format!("n{step}"), &now);
                if edit.is_ok() {
                    created.push(id);
                    stats.adds += 1;
                }
                edit
            } else if roll < 46 {
                // Every move an honest client makes: a plain move, or a
                // transplant when it would leave a share.
                match (rng.pick(&non_root), rng.pick(&nodes)) {
                    (Some(id), Some(parent)) => {
                        let position = if rng.below(2) == 0 { None } else { Some(rng.below(4)) };
                        let mut made = Vec::new();
                        let moved = tree.move_or_transplant(id, parent, position, &now, &mut || {
                            made.push(rng.node_id());
                            *made.last().unwrap()
                        });
                        match moved {
                            Ok((now_id, edit)) => {
                                if now_id == id {
                                    stats.moves += 1;
                                } else {
                                    stats.transplants += 1;
                                }
                                created.extend(made);
                                Ok(edit)
                            }
                            Err(e) => Err(e),
                        }
                    }
                    _ => continue,
                }
            } else if roll < 54 {
                let Some(id) = rng.pick(&non_root) else { continue };
                stats.removes += 1;
                tree.remove_node(id, &now)
            } else if roll < 60 {
                let Some(id) = rng.pick(&dead(tree)) else { continue };
                stats.undeletes += 1;
                tree.undelete_node(id, &now)
            } else if roll < 66 {
                let Some(id) = rng.pick(&nodes) else { continue };
                stats.renames += 1;
                tree.set_title(id, &format!("t{step}"), &now)
            } else if roll < 74 {
                // A folder is shared, or a share is stopped.
                let Some(id) = rng.pick(&non_root) else { continue };
                if carries_marker(tree, id) {
                    stats.unshares += 1;
                    tree.remove_custom(id, SHARE, &now)
                } else if nodes.iter().filter(|&&n| carries_marker(tree, n)).count() < 4 {
                    stats.shares += 1;
                    tree.set_custom(id, SHARE, &marker(&format!("s{step}")), &now)
                } else {
                    continue;
                }
            } else if roll < 78 {
                // A client from before the contract: a plain move, wherever to.
                match (rng.pick(&non_root), rng.pick(&nodes)) {
                    (Some(id), Some(parent)) => {
                        let edit = tree.move_node(id, parent, None, &now);
                        if edit.is_ok() {
                            stats.old_client_moves += 1;
                        }
                        edit
                    }
                    _ => continue,
                }
            } else if roll < 90 {
                // A client that does not keep the rule: a `parent_id` written
                // on its own, pointing at any node (out of the node's share,
                // more often than not), a tombstone, the node itself, nothing.
                let Some(id) = rng.pick(&non_root) else { continue };
                let target = match rng.below(10) {
                    0 => Some(id),
                    1 => None,
                    2 | 3 => rng.pick(&dead(tree)).or(Some(root)),
                    _ => rng.pick(&nodes),
                };
                stats.tampers += 1;
                Ok(tamper_parent(tree, id, target))
            } else {
                let Some(id) = rng.pick(&nodes) else { continue };
                match tree.doc_mut(id).unwrap().replace_plain_text(&format!("text {step}\nline")) {
                    Ok(update) => {
                        stats.content_edits += 1;
                        Ok(TreeEdit { touched: vec![(id, update)] })
                    }
                    Err(e) => Err(e),
                }
            };
            match result {
                Ok(edit) => net.broadcast(r, REPLICAS, &edit),
                Err(_) => stats.refused += 1,
            }
        }

        // Everything delivered, repairs included, then rounds of repair and
        // exchange until nothing moves.
        for round in 0..10 {
            // A drain that never ends is repairs answering each other's
            // repairs; a whole run delivers some thousands of updates.
            let mut drained = 0usize;
            while net.deliver_one(&mut rng, &mut replicas, &mut stats) {
                drained += 1;
                assert!(
                    drained < 50_000,
                    "seed {seed}: the network never drained in round {round}: {stats:?}; issues per replica: {:?}",
                    replicas.iter().map(Tree::validate_tree).collect::<Vec<_>>()
                );
            }
            let mut quiet = true;
            for (r, tree) in replicas.iter_mut().enumerate() {
                if let Some(repair) = checked_repair(tree, "final", &mut stats) {
                    quiet = false;
                    stats.repairs += 1;
                    net.broadcast(r, REPLICAS, &repair);
                }
            }
            if quiet && net.is_empty() {
                break;
            }
            assert!(round < 9, "seed {seed}: repairs did not settle");
        }

        for (r, tree) in replicas.iter_mut().enumerate() {
            let issues = tree.validate_tree();
            assert!(issues.is_empty(), "seed {seed} replica {r}: {issues:?}");
            assert!(tree.repair("check").unwrap().is_none(), "seed {seed} replica {r}: repair not settled");
            for &id in &created {
                let fields = tree.doc(id).unwrap_or_else(|| panic!("seed {seed} replica {r}: lost {id}")).fields().unwrap();
                assert!(tree.has_node(id) || fields.deleted_at.is_some(), "seed {seed} replica {r}: {id} neither present nor tombstoned");
            }
        }
        for r in 1..REPLICAS {
            let (first, rest) = replicas.split_at_mut(r);
            assert_same(&first[0], &rest[0]);
        }
        stats
    }

    /// Eight seeds in a release build (five seconds); one in a debug build,
    /// where the oracle in `checked_repair` (chains and naming lists
    /// recomputed at every one of some thousand repairs per seed) takes
    /// minutes per seed. `cargo test --release -p pimble-crdt` runs the lot.
    #[test]
    fn three_replicas_converge_under_random_edits_delivered_in_random_order() {
        let started = std::time::Instant::now();
        // `PIMBLE_CONVERGENCE_SEEDS=1,5` runs exactly those seeds, in either build.
        let chosen: Vec<u64> = std::env::var("PIMBLE_CONVERGENCE_SEEDS")
            .ok()
            .map(|list| list.split(',').filter_map(|seed| seed.trim().parse().ok()).collect())
            .unwrap_or_default();
        let seeds: &[u64] = if !chosen.is_empty() {
            &chosen
        } else if cfg!(debug_assertions) {
            &[2]
        } else {
            &[1, 2, 3, 4, 5, 6, 7, 8]
        };
        let (mut transplants, mut list_wins) = (0, 0);
        for &seed in seeds {
            let stats = run_convergence(seed, 400);
            assert!(stats.adds > 20 && stats.moves > 10 && stats.removes > 5 && stats.tampers > 10 && stats.shares > 3, "seed {seed} did too little: {stats:?}");
            transplants += stats.transplants;
            list_wins += stats.list_wins;
            eprintln!("seed {seed}: {stats:?}");
        }
        let (least_transplants, least_wins) = (3 * seeds.len(), 10 * seeds.len());
        assert!(
            transplants >= least_transplants && list_wins >= least_wins,
            "the move contract was hardly exercised: {transplants} transplants, {list_wins} lists that won"
        );
        eprintln!("randomized convergence over {} seed(s): {:?}", seeds.len(), started.elapsed());
    }
}
