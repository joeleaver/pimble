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

use std::collections::{BTreeMap, HashMap, HashSet};

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

    // ── Shape ────────────────────────────────────────────────────────────

    /// Exactly the conditions [`Tree::repair`] fixes; empty after a repair.
    /// `OrphanNode` and `MissingChild` name a document this device KNOWS is
    /// deleted (a tombstone it holds). One it does not hold, or holds without
    /// its `node` root, is unknown here and is never reported or "fixed":
    /// see `analyze`.
    pub fn validate_tree(&self) -> Vec<TreeIssue> {
        self.analyze().issues
    }

    /// Decision 9 over documents: effective parents, cycles broken at the
    /// smallest id, every list made to hold the undeleted nodes whose
    /// effective parent is its owner (first occurrence kept, missing ones
    /// appended in id order), tombstones and duplicates taken out, and every
    /// entry naming a document not held here left exactly where it is.
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

        // Step 1: effective parent for every node but the root.
        let mut raw_parent: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut effective_parent: HashMap<NodeId, NodeId> = HashMap::new();
        for &(id, parent) in &nodes {
            if id == root {
                continue;
            }
            raw_parent.insert(id, parent);
            let e = match parent {
                None => {
                    analysis.issues.push(TreeIssue::DetachedNode { node_id: id });
                    root
                }
                Some(p) if node_set.contains(&p) && p != id => p,
                // Its own parent, or under a document known to be deleted.
                Some(p) if p == id || gone.contains(&p) => {
                    analysis.issues.push(TreeIssue::OrphanNode { node_id: id, missing_parent: p });
                    root
                }
                // Under a document this device does not hold: where it
                // belongs is not known here, so it is left exactly where it
                // says it is (no effective parent, no rewrite, no listing).
                Some(_) => continue,
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
            for (index, entry) in self.docs[&owner].raw_children().into_iter().enumerate() {
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
                if let Some(repair) = replicas[to].repair("repair").unwrap() {
                    stats.repairs += 1;
                    self.broadcast(to, replicas.len(), &repair);
                }
            }
            true
        }
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
    }

    fn sorted(mut ids: Vec<NodeId>) -> Vec<NodeId> {
        ids.sort_by_key(|id| id.to_string());
        ids
    }

    fn run_convergence(seed: u64, steps: usize) -> Stats {
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
            let roll = rng.below(100);
            let result = if roll < 30 {
                let id = rng.node_id();
                let parent = rng.pick(&nodes).unwrap();
                let position = if rng.below(2) == 0 { None } else { Some(rng.below(4)) };
                let edit = tree.add_node(id, Some(parent), position, "document", &format!("n{step}"), &now);
                if edit.is_ok() {
                    created.push(id);
                    stats.adds += 1;
                }
                edit
            } else if roll < 55 {
                match (rng.pick(&non_root), rng.pick(&nodes)) {
                    (Some(id), Some(parent)) => {
                        let position = if rng.below(2) == 0 { None } else { Some(rng.below(4)) };
                        let edit = tree.move_node(id, parent, position, &now);
                        if edit.is_ok() {
                            stats.moves += 1;
                        }
                        edit
                    }
                    _ => continue,
                }
            } else if roll < 65 {
                let Some(id) = rng.pick(&non_root) else { continue };
                stats.removes += 1;
                tree.remove_node(id, &now)
            } else if roll < 72 {
                let dead: Vec<NodeId> = sorted(tree.ids())
                    .into_iter()
                    .filter(|&id| tree.doc(id).unwrap().fields().ok().is_some_and(|f| f.deleted_at.is_some()))
                    .collect();
                let Some(id) = rng.pick(&dead) else { continue };
                stats.undeletes += 1;
                tree.undelete_node(id, &now)
            } else if roll < 82 {
                let Some(id) = rng.pick(&nodes) else { continue };
                stats.renames += 1;
                tree.set_title(id, &format!("t{step}"), &now)
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
            while net.deliver_one(&mut rng, &mut replicas, &mut stats) {}
            let mut quiet = true;
            for (r, tree) in replicas.iter_mut().enumerate() {
                if let Some(repair) = tree.repair("final").unwrap() {
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

    #[test]
    fn three_replicas_converge_under_random_edits_delivered_in_random_order() {
        let started = std::time::Instant::now();
        for seed in [1, 2, 3, 4, 5, 6] {
            let stats = run_convergence(seed, 400);
            assert!(stats.adds > 20 && stats.moves > 10 && stats.removes > 5, "seed {seed} did too little: {stats:?}");
            eprintln!("seed {seed}: {stats:?}");
        }
        eprintln!("randomized convergence: {:?}", started.elapsed());
    }
}
