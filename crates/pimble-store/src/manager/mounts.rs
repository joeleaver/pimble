//! The store manager's mounts (docs/history/MOUNTS_CONTRACT.md): opening
//! a mount's source store, and judging a new mount before it is made.

use super::*;

impl StoreManager {
    // ── Mount resolution ────────────────────────────────────────────

    /// Ensure a mount's source store is open, returning its `StoreId`
    /// (always `mount_ref.source_store` on success). Resolution order:
    /// already open; a registry entry (works for a store opened earlier in
    /// this process, local or previously-registered); `mount_ref`'s
    /// `source_path` hint, opened directly and thereby registered (this is
    /// what lets a mount resolve after a restart even when its source was
    /// not itself in the app's open-store list). `Err` only when none of
    /// those apply, or the registry's entry is `Remote` (out of scope).
    pub async fn ensure_store_open(&mut self, mount_ref: &MountRef) -> Result<StoreId> {
        let store_id = mount_ref.source_store;

        match self.open_registered_store(store_id).await {
            Ok(()) => Ok(store_id),
            Err(StoreError::MountSourceUnavailable { .. }) => {
                let Some(path) = &mount_ref.source_path else {
                    return Err(StoreError::MountSourceUnavailable { store_id });
                };
                // The hint is only a hint: the directory may now hold a
                // different store. Open it, and if it is not the store the
                // mount names, close it again and report the source missing.
                let opened = self.open_local_store(path).await
                    .map_err(|_| StoreError::MountSourceUnavailable { store_id })?;
                if opened != store_id {
                    self.close_store(opened).await?;
                    return Err(StoreError::MountSourceUnavailable { store_id });
                }
                Ok(store_id)
            }
            Err(e) => Err(e),
        }
    }

    /// Open `store_id` if it is already open or known to the registry;
    /// otherwise `MountSourceUnavailable`. Does not consult a `MountRef`'s
    /// `source_path` hint — see [`StoreManager::ensure_store_open`] for the
    /// full resolution order used when resolving a specific mount.
    pub async fn open_registered_store(&mut self, store_id: StoreId) -> Result<()> {
        if self.local_stores.contains_key(&store_id) {
            return Ok(());
        }

        match self.registry.lookup(&store_id).cloned() {
            Some(StoreEndpoint::Local { path }) => {
                self.open_local_store(&path).await?;
                Ok(())
            }
            Some(StoreEndpoint::Remote { .. }) => {
                Err(StoreError::MountSourceUnavailable { store_id })
            }
            None => Err(StoreError::MountSourceUnavailable { store_id }),
        }
    }

    /// Check if creating a mount would create a cycle.
    pub async fn validate_mount_creation(
        &mut self,
        mounting_store: StoreId,
        mounting_node: NodeId,
        mount_ref: &MountRef,
    ) -> Result<()> {
        let ancestors = self.collect_ancestors(mounting_store, mounting_node)?;

        self.ensure_store_open(mount_ref).await?;

        let mut stack: Vec<(StoreId, NodeId, usize)> = vec![
            (mount_ref.source_store, mount_ref.source_node, 0),
        ];

        while let Some((current_store, current_node, depth)) = stack.pop() {
            if depth > MAX_MOUNT_DEPTH {
                return Err(StoreError::MountDepthExceeded { depth });
            }

            self.open_registered_store(current_store).await?;

            let node = self.local(current_store)?.get_node(current_node)?;

            if node.is_mount() {
                if let Some(nested_ref) = node.mount_ref() {
                    if nested_ref.source_store == mounting_store
                        && ancestors.contains(&nested_ref.source_node)
                    {
                        return Err(StoreError::MountCycle {
                            chain: vec![
                                (mounting_store, mounting_node),
                                (mount_ref.source_store, mount_ref.source_node),
                                (nested_ref.source_store, nested_ref.source_node),
                            ],
                        });
                    }

                    if self.registry.lookup(&nested_ref.source_store).is_some()
                        || self.local_stores.contains_key(&nested_ref.source_store)
                    {
                        stack.push((nested_ref.source_store, nested_ref.source_node, depth + 1));
                    }
                }
            }

            let children_ids = node.children.clone();
            for child_id in children_ids {
                stack.push((current_store, child_id, depth + 1));
            }
        }

        Ok(())
    }

    /// Collect the set of ancestor node IDs for a given node in a store.
    fn collect_ancestors(&self, store_id: StoreId, node_id: NodeId) -> Result<Vec<NodeId>> {
        let mut ancestors = vec![node_id];
        let store = self.local(store_id)?;

        let mut current = node_id;
        loop {
            let node = store.get_node(current)?;
            match node.parent_id {
                Some(parent_id) => {
                    ancestors.push(parent_id);
                    current = parent_id;
                }
                None => break,
            }
        }

        Ok(ancestors)
    }
}
