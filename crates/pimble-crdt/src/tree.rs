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
//!
//! An *attachment* (docs/DISCUSSIONS_CONTRACT.md section 1: a discussion on a
//! node) is a live document whose `node` root names a host (`attached_to`)
//! instead of a parent. It is in no list, repair never places it, and it is
//! shown only while its host is a node ([`Tree::attachments_of`]). A delete of
//! its host hides it and an undelete brings it back; a transplant of its host
//! makes it again under the new host, like the rest of the subtree.

use std::collections::{HashMap, HashSet};

use pimble_core::NodeId;

use crate::error::Result;
use crate::node_doc::{NodeDoc, NodeFields, NodeUpdateEffect};

pub use crate::store_document::{NodeInfo, TreeIssue};

mod editing;
mod helpers;
mod shape;
mod shares;

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
    /// subtree's root. For an attachment, its host's index.
    pub parent: Option<usize>,
    /// An attachment of the node at `parent`, not a child of it: planted as
    /// one. Attachments come after every node of the subtree.
    pub attached: bool,
    pub node_type: String,
    pub title: String,
    pub tags: Vec<String>,
    /// Every custom field but the share marker.
    pub custom: HashMap<String, serde_json::Value>,
    /// The node's fields, name and value (`field:<name>` keys).
    pub fields: Vec<(String, String)>,
    /// A metanode's viewer and source as stored: a kanban stays a kanban
    /// (until 2026-11-04 a move between stores dropped both).
    pub metanode_viewer: Option<String>,
    pub metanode_source: Option<String>,
    pub created_at: String,
    /// The plugin root as JSON (`NodeDoc::data_json`).
    pub data: serde_json::Value,
    /// The plugin root copied with its yrs types (`NodeDoc::fresh_data`):
    /// what is planted, since a board's shapes do not survive JSON. `None`
    /// when it is empty.
    data_copy: Option<Vec<u8>>,
    /// The content as a new document's bytes (`NodeDoc::fresh_content`);
    /// `None` when the node has no projection.
    content: Option<Vec<u8>>,
}

/// What [`Tree::plant`] made: the new root and, per node of the cutting, the
/// id it had where it was read and the id it has now, in the cutting's
/// preorder. The source tree records the pairs on its tombstones
/// ([`Tree::remove_transplanted`]) so a link to an original follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planted {
    pub root: NodeId,
    pub ids: Vec<(NodeId, NodeId)>,
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
type DocRepair = (NodeId, Option<(NodeId, bool)>, Vec<usize>, Vec<NodeId>);

/// Read-only analysis of the tree's current shape, shared by
/// [`Tree::validate_tree`] and [`Tree::repair`] so the two can never drift
/// apart: `validate_tree` reports exactly the issues, `repair` applies exactly
/// the fixes computed alongside them.
struct TreeAnalysis {
    issues: Vec<TreeIssue>,
    /// `(node, new_parent, placed)` whose stored `parent_id` needs to change
    /// (decision 9 step 3). `placed` is whether `placed_under` is written with
    /// it: yes for a decision made from lists and shares (a share's claim, an
    /// orphan or a detached node put under the root), which every device is
    /// then to honour; no for a cycle broken by sending its smallest member
    /// to the root, which a list in a share may still claim (`NodeDoc::write_parent_only`).
    parent_rewrites: Vec<(NodeId, NodeId, bool)>,
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

    /// Every held document, tombstones included, whose stored `parent_id` is
    /// one of `parents`: the documents a repair of those lists judged. Sent
    /// again whole when two devices keep undoing each other's repair of a
    /// list (one of them lacks part of a child's document, most often its
    /// tombstone).
    pub fn documents_under(&self, parents: &HashSet<NodeId>) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.docs.iter().filter(|(_, d)| d.stored_parent().is_some_and(|p| parents.contains(&p))).map(|(&id, _)| id).collect();
        ids.sort_by_key(|id| id.to_string());
        ids
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

    /// The stored fields of a live node (what [`Tree::get_node_info`] is
    /// built from), for [`NodeFields::into_node`]: an error for an id that is
    /// not held, not initialised or deleted.
    /// The text tags are read here (docs/HASHTAGS_CONTRACT.md): the rest of
    /// the tree's reads leave them out.
    pub fn live_fields(&self, id: NodeId) -> Result<NodeFields> {
        let mut fields = self.require_node(id)?;
        fields.text_tags = self.docs[&id].text_tags();
        Ok(fields)
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
}

#[cfg(test)]
mod tests;
