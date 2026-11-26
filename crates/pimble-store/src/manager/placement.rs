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
    /// before, or adds an id to an internal node's children. The update is
    /// merged into a copy of the document; the document is not touched.
    ///
    /// Only what the update changes is judged: a child an older client put
    /// under a theme before this was enforced keeps its place, and edits of
    /// it and of the theme still merge.
    pub fn judge_placement(&self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<()> {
        let tree = self.local(store_id)?.tree();
        let held = tree.doc(node_id);
        let (parent_before, children_before) = match held {
            Some(doc) => (doc.stored_fields().ok().and_then(|f| f.parent_id), doc.children()),
            None => (None, Vec::new()),
        };
        let mut merged = match held {
            Some(doc) => NodeDoc::load(&doc.save())?,
            None => NodeDoc::new(),
        };
        merged.apply_update(update)?;
        let Ok(fields) = merged.stored_fields() else {
            // No `node` root yet: nothing places this document.
            return Ok(());
        };
        if let Some(parent_id) = fields.parent_id.filter(|parent| Some(*parent) != parent_before) {
            refuse_internal_parent(tree, parent_id)?;
        }
        if let Some(kind) = internal_kind(&fields.node_type) {
            if merged.children().iter().any(|child| !children_before.contains(child)) {
                return Err(StoreError::InsideInternal { kind });
            }
        }
        Ok(())
    }
}
