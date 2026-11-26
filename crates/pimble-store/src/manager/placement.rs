//! Nothing goes inside an internal node (docs/THEMES_CONTRACT.md section
//! 3.1a): the store's own judgement of every place a node is put, so a
//! client that does not judge it (an older one, a script) cannot put a
//! child under a theme. The clients judge it first with
//! `pimble_core::plan::parent_of_row`, and the refusal reads the same:
//! [`InternalKind::holds_nothing`].
//!
//! A plain store's documents are here to read, so each way in is judged:
//! a create, a move and a transplant by the parent they name, and a merged
//! update by what the merge would leave ([`StoreManager::judge_placement`]).
//! A vault store's documents are ciphertext here and cannot be judged.

use pimble_core::internal::{internal_kind, InternalKind};

use super::*;

/// The internal kind of `id`'s document in `tree`, tombstone or not: a
/// child under a deleted theme is still under a theme.
fn kind_of(tree: &Tree, id: NodeId) -> Option<InternalKind> {
    let fields = tree.doc(id)?.stored_fields().ok()?;
    internal_kind(&fields.node_type)
}

/// Refuse `parent_id` as a parent when it is an internal node.
pub(super) fn refuse_internal_parent(tree: &Tree, parent_id: NodeId) -> Result<()> {
    match kind_of(tree, parent_id) {
        Some(kind) => Err(StoreError::InsideInternal { kind }),
        None => Ok(()),
    }
}

impl StoreManager {
    /// Refuse `parent_id` of plain store `store_id` as a place for a node
    /// when it is an internal node: what `create_node_at`, `move_node` and
    /// `transplant_node` judge, and `moveNodes` before it moves any.
    pub fn check_parent(&self, store_id: StoreId, parent_id: NodeId) -> Result<()> {
        refuse_internal_parent(self.local(store_id)?.tree(), parent_id)
    }

    /// Judge `update` to `node_id`'s document of plain store `store_id` by
    /// what merging it would leave, before it is merged: refused when it
    /// gives the node a `parent_id` naming an internal node it did not name
    /// before, adds an id to an internal node's children, or makes a node
    /// internal while it holds children. Only what the update changes is
    /// judged: a child an older client put under a theme before this was
    /// enforced keeps its place, and edits of it and of the theme still
    /// merge.
    ///
    /// An update that cannot place anything (`NodeDoc::may_place`: nothing
    /// it brings lands in the `node` or `children` root, which is every
    /// keystroke) is let through as it is. Any other is merged into a copy
    /// of the document, saved and loaded again, which is judged; the
    /// document is not touched. One whose merge reads differently from that
    /// reload is refused outright: merged here it would leave the live
    /// document disagreeing with its own file and with every peer.
    pub fn judge_placement(&self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<()> {
        let tree = self.local(store_id)?.tree();
        let held = tree.doc(node_id);
        if let Some(doc) = held {
            if !doc.may_place(update)? {
                return Ok(());
            }
        }
        let fields_before = held.and_then(|doc| doc.stored_fields().ok());
        let parent_before = fields_before.as_ref().and_then(|f| f.parent_id);
        let was_internal = fields_before.as_ref().is_some_and(|f| internal_kind(&f.node_type).is_some());
        let children_before = held.map(|doc| doc.children()).unwrap_or_default();
        // Judged as the document will be once persisted and loaded again,
        // which is what every peer and every later open sees: a forged
        // update can merge in memory to a state its own save does not keep,
        // and that one is refused (`NodeDoc::merged_as_saved`).
        let merged = NodeDoc::merged_as_saved(held, update)?;
        let Ok(fields) = merged.stored_fields() else {
            // No `node` root yet: nothing places this document.
            return Ok(());
        };
        if let Some(parent_id) = fields.parent_id.filter(|parent| Some(*parent) != parent_before) {
            refuse_internal_parent(tree, parent_id)?;
        }
        if let Some(kind) = internal_kind(&fields.node_type) {
            let children = merged.children();
            if children.iter().any(|child| !children_before.contains(child)) {
                return Err(StoreError::InsideInternal { kind });
            }
            // A node made internal by this update must hold nothing already:
            // no list entry, and no document naming it as its parent (which
            // repair would list under it).
            if !was_internal && (!children.is_empty() || !tree.documents_under(&HashSet::from([node_id])).is_empty()) {
                return Err(StoreError::InsideInternal { kind });
            }
        }
        Ok(())
    }
}
