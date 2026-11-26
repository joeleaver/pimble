//! The store manager's nodes and documents: reads, the tree edits every
//! structural RPC makes (create, move, transplant, delete), and merges
//! into a node's document.

use super::*;

impl StoreManager {
    // ── Nodes ────────────────────────────────────────────────────────────

    /// Get a node from a store. `NodeNotFound` for a tombstone or a
    /// document whose `node` root has not arrived.
    pub fn get_node(&self, store_id: StoreId, node_id: NodeId) -> Result<Node> {
        self.local(store_id)?.get_node(node_id)
    }

    /// [`StoreManager::get_node`], but for a tombstone too (see
    /// [`LocalStore::get_node_any`]).
    pub fn get_node_any(&self, store_id: StoreId, node_id: NodeId) -> Result<Node> {
        self.local(store_id)?.get_node_any(node_id)
    }

    /// Bring a node's title, tags and custom fields to `metadata` (the
    /// whole metadata; a custom key absent from it is removed). See
    /// [`LocalStore::update_node_metadata`].
    pub fn update_node_metadata(&mut self, store_id: StoreId, node_id: NodeId, metadata: NodeMetadata) -> Result<TreeEdit> {
        self.local_mut(store_id)?.update_node_metadata(node_id, &metadata)
    }

    /// Create a node in a store, returning its id and the edit to persist
    /// and broadcast per document. A mount node has no children of its own
    /// (its subtree lives entirely in the source store), so creating under
    /// one is rejected; create under the mount's `mount_ref` instead. Nor
    /// does anything go inside an internal node (see `placement`).
    pub fn create_node(&mut self, store_id: StoreId, node: Node, parent_id: Option<NodeId>) -> Result<(NodeId, TreeEdit)> {
        self.create_node_at(store_id, node, parent_id, Place::Last)
    }

    /// [`StoreManager::create_node`] at `place` among the parent's children.
    pub fn create_node_at(&mut self, store_id: StoreId, node: Node, parent_id: Option<NodeId>, place: Place) -> Result<(NodeId, TreeEdit)> {
        let store = self.local_mut(store_id)?;

        if let Some(parent_id) = parent_id {
            let parent = store.tree().get_node_info(parent_id).map_err(|_| StoreError::NodeNotFound(parent_id))?;
            if parent.node_type == pimble_core::node_types::MOUNT {
                return Err(StoreError::MountHasNoChildren { node_id: parent_id });
            }
            placement::refuse_internal_parent(store.tree(), parent_id)?;
        }

        store.create_node_at(node, parent_id, place)
    }

    /// Create an attachment of `host` in a store ([`LocalStore::create_attachment`]).
    /// A mount holds nothing of its own, so it takes none.
    pub fn create_attachment(&mut self, store_id: StoreId, node: Node, host: NodeId) -> Result<(NodeId, TreeEdit)> {
        let store = self.local_mut(store_id)?;
        let info = store.tree().get_node_info(host).map_err(|_| StoreError::NodeNotFound(host))?;
        if info.node_type == pimble_core::node_types::MOUNT {
            return Err(StoreError::MountHasNoChildren { node_id: host });
        }
        store.create_attachment(node, host)
    }

    /// Move a node to a new parent in a store, deciding a plain move or a
    /// transplant as `LocalStore::move_node` does (docs/MOVE_CONTRACT.md
    /// "The rule"). Answers the id the node has now, the shares it left,
    /// and the edit; a caller that wants the old parent for a notification
    /// reads it off the tree first. Nothing goes inside an internal node
    /// (see `placement`).
    pub fn move_node(&mut self, store_id: StoreId, node_id: NodeId, new_parent_id: NodeId, place: Place) -> Result<(NodeId, Vec<LeftShare>, TreeEdit)> {
        self.check_parent(store_id, new_parent_id)?;
        self.local_mut(store_id)?.move_node(node_id, new_parent_id, place)
    }

    /// Move a node into another store (docs/MOVE_CONTRACT.md "Between
    /// stores"): always a transplant, since two stores hold different
    /// documents. `take_cutting` on the source, `plant_cutting` on the
    /// target, then the source's subtree tombstoned with `delete_node` —
    /// created first, deleted second, so a failure between the two leaves
    /// both, never neither. A mount node has no children of its own, so
    /// planting under one is rejected exactly as `create_node` rejects
    /// creating under one, and so is planting inside an internal node.
    pub fn transplant_node(
        &mut self,
        from_store_id: StoreId,
        node_id: NodeId,
        to_store_id: StoreId,
        new_parent_id: NodeId,
        place: Place,
    ) -> Result<TransplantOutcome> {
        let target_parent = self.local(to_store_id)?.tree().get_node_info(new_parent_id).map_err(|_| StoreError::NodeNotFound(new_parent_id))?;
        if target_parent.node_type == pimble_core::node_types::MOUNT {
            return Err(StoreError::MountHasNoChildren { node_id: new_parent_id });
        }
        self.check_parent(to_store_id, new_parent_id)?;

        let left_shares = self.local(from_store_id)?.shares_of(node_id);
        let cutting = self.local(from_store_id)?.take_cutting(node_id)?;
        let (planted, target_edit) = self.local_mut(to_store_id)?.plant_cutting(cutting, new_parent_id, place)?;
        let (source_removal, source_edit) = self.local_mut(from_store_id)?.remove_transplanted(node_id, to_store_id, &planted)?;
        Ok(TransplantOutcome { new_node_id: planted.root, left_shares, source_removal, source_edit, target_edit })
    }

    /// Delete a node and its subtree from a store: every member becomes a
    /// tombstone (docs/NODE_DOCUMENT_CONTRACT.md section 2; no file is
    /// deleted). Returns the parent it was removed from with every id
    /// tombstoned, and the edit. Refuses to delete the root.
    pub fn delete_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<(NodeRemoval, TreeEdit)> {
        self.local_mut(store_id)?.delete_node(node_id)
    }

    /// Bring a deleted node and its subtree back (see
    /// [`LocalStore::undelete_node`]).
    pub fn undelete_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<TreeEdit> {
        self.local_mut(store_id)?.undelete_node(node_id)
    }

    /// Repair a store's tree after a merge (see `Tree::repair`), marking
    /// only the documents it changed dirty (docs/history/HARDENING_CONTRACT.md
    /// decision 9). `None` when nothing needed fixing.
    pub fn repair_tree(&mut self, store_id: StoreId) -> Result<Option<TreeEdit>> {
        self.local_mut(store_id)?.repair_tree()
    }

    /// Merge a node document snapshot into a node's document (never a
    /// replacement; see [`LocalStore::update_node_content`]).
    pub fn update_node_content(&mut self, store_id: StoreId, node_id: NodeId, content: Vec<u8>) -> Result<NodeUpdateEffect> {
        self.local_mut(store_id)?.update_node_content(node_id, content)
    }

    /// Get children of a node, and the id of the store they canonically live
    /// in.
    ///
    /// For an ordinary node this is `(store_id, ...)`: the request's own
    /// store. For a mount point, the mount's source store is opened if
    /// necessary (see [`StoreManager::ensure_store_open`]) and the source
    /// node's children are returned instead, addressed by the source store's
    /// id — a mount resolves exactly one level; a nested mount inside the
    /// returned children is itself a source-store node, resolved the same
    /// way when the caller expands it in turn.
    pub async fn get_children(&mut self, store_id: StoreId, node_id: NodeId) -> Result<(StoreId, Vec<Node>)> {
        let node = self.get_node(store_id, node_id)?;
        if node.is_mount() {
            let mount_ref = node.mount_ref().ok_or_else(|| {
                StoreError::InvalidOperation(format!("mount node {} has no mount_ref", node_id))
            })?;
            let source_store = self.ensure_store_open(&mount_ref).await?;
            let children = self.local(source_store)?.get_children(mount_ref.source_node)?;
            return Ok((source_store, children));
        }

        let children = self.local(store_id)?.get_children(node_id)?;
        Ok((store_id, children))
    }

    // ── Documents ────────────────────────────────────────────────────────

    /// The tree over a store's documents.
    pub fn tree(&self, store_id: StoreId) -> Result<&Tree> {
        Ok(self.local(store_id)?.tree())
    }

    /// The tree, to edit. Nothing edited through it is marked dirty: call
    /// [`StoreManager::mark_dirty`] for every id the resulting `TreeEdit`
    /// names.
    pub fn tree_mut(&mut self, store_id: StoreId) -> Result<&mut Tree> {
        Ok(self.local_mut(store_id)?.tree_mut())
    }

    /// A held document, to edit; marked dirty at once. `NodeNotFound` when
    /// no document is held under `node_id`.
    pub fn node_doc(&mut self, store_id: StoreId, node_id: NodeId) -> Result<&mut NodeDoc> {
        self.local_mut(store_id)?.node_doc(node_id)
    }

    /// Merge a peer's update into a node's document, creating the document
    /// when unknown (authorise first). Marks it dirty only when the merge
    /// changed something (docs/history/HARDENING_CONTRACT.md decision 8);
    /// repairs nothing, so a caller applies a batch and then runs
    /// `repair_tree` once when any effect reports `structure`.
    pub fn apply_node_update(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<NodeUpdateEffect> {
        self.local_mut(store_id)?.apply_node_update(node_id, update)
    }

    /// v1-encoded state vector of a node's document (`NodeNotFound` when no
    /// document is held under the id).
    pub fn node_state_vector(&self, store_id: StoreId, node_id: NodeId) -> Result<Vec<u8>> {
        self.local(store_id)?.node_state_vector(node_id)
    }

    /// Everything a node's document has that a peer at `state_vector`
    /// (v1-encoded) lacks.
    pub fn node_diff_since(&self, store_id: StoreId, node_id: NodeId, state_vector: &[u8]) -> Result<Vec<u8>> {
        self.local(store_id)?.node_diff_since(node_id, state_vector)
    }

    /// Every held document's id, tombstones included: what sync names.
    pub fn doc_ids(&self, store_id: StoreId) -> Result<Vec<NodeId>> {
        Ok(self.local(store_id)?.doc_ids())
    }

    /// Every undeleted node's id.
    pub fn list_node_ids(&self, store_id: StoreId) -> Result<Vec<NodeId>> {
        Ok(self.local(store_id)?.list_node_ids())
    }

    /// Mark a node's document as needing a flush (an edit made through
    /// `tree_mut`).
    pub fn mark_dirty(&mut self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        self.local_mut(store_id)?.mark_dirty(node_id);
        Ok(())
    }

    /// The root a store's documents say it has (see
    /// [`LocalStore::document_root`]), for `adopt_document_root`.
    pub fn document_root(&self, store_id: StoreId) -> Result<Option<NodeId>> {
        Ok(self.local(store_id)?.document_root())
    }
}
