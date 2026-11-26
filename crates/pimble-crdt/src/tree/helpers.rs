use std::collections::HashSet;

use pimble_core::custom_keys::SHARE;
use pimble_core::NodeId;

use crate::error::{CrdtError, Result};
use crate::node_doc::{NodeDoc, NodeFields};

use super::{Tree, TreeEdit, Ancestry, ShareClaim};

impl Tree {
    // ── Private helpers ──────────────────────────────────────────────────

    /// The fields of a held, initialised document (tombstoned or not).
    pub(super) fn fields_of(&self, id: NodeId) -> Option<NodeFields> {
        self.docs.get(&id).and_then(|doc| doc.stored_fields().ok())
    }

    /// The fields of a node, or an error saying why `id` is not one.
    pub(super) fn require_node(&self, id: NodeId) -> Result<NodeFields> {
        let Some(doc) = self.docs.get(&id) else {
            return Err(CrdtError::KeyNotFound(format!("node {id}")));
        };
        let fields = doc
            .stored_fields()
            .map_err(|_| CrdtError::KeyNotFound(format!("node {id} (document not initialised)")))?;
        if fields.deleted_at.is_some() {
            return Err(CrdtError::KeyNotFound(format!("node {id} (deleted)")));
        }
        Ok(fields)
    }

    /// `start` and its ancestors by stored `parent_id`s: see [`Ancestry`].
    /// Each document once, so the walk is bounded by what is held.
    pub(super) fn ancestry(&self, start: NodeId) -> Ancestry {
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
    pub(super) fn share_claim(&self, id: NodeId, destination: NodeId, lists: &[NodeId]) -> ShareClaim {
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
    pub(super) fn is_under(&self, id: NodeId, ancestor: NodeId) -> bool {
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
    pub(super) fn collect_subtree(&self, start: NodeId, accept: &dyn Fn(&NodeFields) -> bool) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        self.walk(start, accept, &mut seen, &mut out);
        out
    }

    pub(super) fn walk(&self, id: NodeId, accept: &dyn Fn(&NodeFields) -> bool, seen: &mut HashSet<NodeId>, out: &mut Vec<NodeId>) {
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
    pub(super) fn list_edit(&self, id: NodeId, f: impl FnOnce(&NodeDoc, &mut yrs::TransactionMut)) -> Result<Option<Vec<u8>>> {
        let doc = self.docs.get(&id).ok_or_else(|| CrdtError::KeyNotFound(format!("node {id}")))?;
        doc.edit(|d, txn| {
            f(d, txn);
            Ok(())
        })
        .map(|(_, update)| update)
    }

    /// A metadata edit on a node, in one transaction on its document.
    pub(super) fn node_edit(&mut self, id: NodeId, f: impl FnOnce(&NodeDoc, &mut yrs::TransactionMut) -> Result<()>) -> Result<TreeEdit> {
        self.require_node(id)?;
        let (_, update) = self.docs[&id].edit_node(f)?;
        let mut edit = TreeEdit::default();
        edit.push(id, update);
        Ok(edit)
    }
}
