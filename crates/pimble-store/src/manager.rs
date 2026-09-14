//! Store manager - handles multiple open stores

use std::collections::HashMap;
use std::path::Path;

use pimble_core::{MountRef, MountState, Node, NodeId, NodeMetadata, Store, StoreId, StoreLocation, SyncState};
use pimble_crdt::{ContentDoc, StoreDocument};
use tracing::info;

/// Maximum depth for transitive mount resolution
const MAX_MOUNT_DEPTH: usize = 16;

use crate::error::{Result, StoreError};
use crate::local::LocalStore;
use crate::registry::{StoreEndpoint, StoreRegistry};

/// Manages multiple open stores
pub struct StoreManager {
    /// Open local stores
    local_stores: HashMap<StoreId, LocalStore>,
    /// Registry of known stores and how to reach them
    registry: StoreRegistry,
    /// Stores opened since the last call to [`StoreManager::opened_since`],
    /// which drains it. Populated by `create_local_store` and
    /// `open_local_store` (including when called transitively while
    /// resolving or validating a mount), so a caller that may have triggered
    /// an implicit open — e.g. `get_children` or `mount_state` on a mount
    /// node — can learn which stores it must now treat as ordinary open
    /// stores (search index, `listStores`, subscriptions) without diffing
    /// `list_stores()` before and after.
    newly_opened: Vec<StoreId>,
}

impl StoreManager {
    /// Create a new store manager
    pub fn new() -> Self {
        Self {
            local_stores: HashMap::new(),
            registry: StoreRegistry::new(),
            newly_opened: Vec::new(),
        }
    }

    /// Create a new local store
    pub async fn create_local_store(&mut self, path: impl AsRef<Path>, name: impl Into<String>) -> Result<StoreId> {
        let path = path.as_ref();
        let store = LocalStore::create(path, name).await?;
        let id = store.id;
        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
        self.local_stores.insert(id, store);
        self.newly_opened.push(id);
        Ok(id)
    }

    /// Open an existing local store
    pub async fn open_local_store(&mut self, path: impl AsRef<Path>) -> Result<StoreId> {
        let path = path.as_ref();
        let store = LocalStore::open(path).await?;
        let id = store.id;

        // Always register (updates path if it changed)
        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });

        if self.local_stores.contains_key(&id) {
            info!("Store {} is already open", id);
            return Ok(id);
        }

        self.local_stores.insert(id, store);
        self.newly_opened.push(id);
        Ok(id)
    }

    /// Drain and return the list of stores opened since the last call to
    /// this method. See the [`StoreManager::newly_opened`] field doc for why
    /// this exists.
    pub fn opened_since(&mut self) -> Vec<StoreId> {
        std::mem::take(&mut self.newly_opened)
    }

    /// Close a store
    pub async fn close_store(&mut self, store_id: StoreId) -> Result<()> {
        if let Some(mut store) = self.local_stores.remove(&store_id) {
            store.flush().await?;
            info!("Closed store {}", store_id);
        }
        self.newly_opened.retain(|id| *id != store_id);
        Ok(())
    }

    /// Get store info
    pub fn get_store_info(&self, store_id: StoreId) -> Result<Store> {
        if let Some(store) = self.local_stores.get(&store_id) {
            let manifest = store.manifest();
            Ok(Store {
                id: store_id,
                name: manifest.name.clone(),
                location: StoreLocation::Local {
                    path: store.path.clone(),
                },
                root_node_id: manifest.root_node_id,
                sync_state: SyncState::Offline,
            })
        } else {
            Err(StoreError::StoreNotFound(store_id))
        }
    }

    /// List all open stores
    pub fn list_stores(&self) -> Vec<StoreId> {
        self.local_stores.keys().copied().collect()
    }

    /// Check if a store is open
    pub fn is_open(&self, store_id: StoreId) -> bool {
        self.local_stores.contains_key(&store_id)
    }

    /// Get a node from a store
    pub async fn get_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<Node> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.get_node(node_id).await
    }

    /// Update a node's metadata in-place and mark it dirty
    pub async fn update_node_metadata(&mut self, store_id: StoreId, node_id: NodeId, metadata: NodeMetadata) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.update_node_metadata(node_id, &metadata).await
    }

    /// Create a node in a store. A mount node has no children of its own
    /// (its subtree lives entirely in the source store), so creating under
    /// one is rejected; create under the mount's `mount_ref` instead.
    pub async fn create_node(&mut self, store_id: StoreId, node: Node, parent_id: Option<NodeId>) -> Result<NodeId> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;

        if let Some(parent_id) = parent_id {
            let parent = store.store_document().get_node_info(parent_id)
                .map_err(StoreError::Crdt)?;
            if parent.node_type == pimble_core::node_types::MOUNT {
                return Err(StoreError::MountHasNoChildren { node_id: parent_id });
            }
        }

        store.create_node(node, parent_id).await
    }

    /// Move a node to a new parent in a store
    pub async fn move_node(&mut self, store_id: StoreId, node_id: NodeId, new_parent_id: NodeId, position: Option<usize>) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.move_node(node_id, new_parent_id, position).await
    }

    /// Delete a node from a store
    pub async fn delete_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.delete_node(node_id).await
    }

    /// Replace a node's content with a full yrs snapshot.
    pub async fn update_node_content(&mut self, store_id: StoreId, node_id: NodeId, content: Vec<u8>) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.update_node_content(node_id, content).await
    }

    /// Merge a yrs update (delta, reconciliation diff, or whole snapshot)
    /// into a node's content document.
    pub async fn apply_content_update(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.apply_content_update(node_id, update).await
    }

    /// Get a node's persistent CRDT content document (mutable reference)
    pub async fn get_node_document(&mut self, store_id: StoreId, node_id: NodeId) -> Result<&mut ContentDoc> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.get_node_document(node_id).await
    }

    /// Save a node's CRDT document
    pub async fn save_node_document(&mut self, store_id: StoreId, node_id: NodeId, doc: &mut ContentDoc) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.save_node_document(node_id, doc).await
    }

    /// Mark a node's content as dirty
    pub fn mark_content_dirty(&mut self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.mark_content_dirty(node_id);
        Ok(())
    }

    /// Get a reference to a store's StoreDocument
    pub fn store_document(&self, store_id: StoreId) -> Result<&StoreDocument> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.store_document())
    }

    /// Get a mutable reference to a store's StoreDocument
    pub fn store_document_mut(&mut self, store_id: StoreId) -> Result<&mut StoreDocument> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.store_document_mut())
    }

    /// v1-encoded state vector for a store's store document.
    pub fn store_doc_state_vector(&self, store_id: StoreId) -> Result<Vec<u8>> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.store_doc_state_vector())
    }

    /// Everything a store's store document has that a peer at `state_vector`
    /// (v1-encoded) lacks.
    pub fn store_doc_diff_since(&self, store_id: StoreId, state_vector: &[u8]) -> Result<Vec<u8>> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.store_doc_diff_since(state_vector)
    }

    /// Merge a peer's yrs update into a store's store document.
    pub fn apply_store_doc_update(&mut self, store_id: StoreId, update: &[u8]) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.apply_store_doc_update(update)
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
        let node = self.get_node(store_id, node_id).await?;
        if node.is_mount() {
            let mount_ref = node.mount_ref().ok_or_else(|| {
                StoreError::InvalidOperation(format!("mount node {} has no mount_ref", node_id))
            })?;
            let source_store = self.ensure_store_open(&mount_ref).await?;
            let store = self.local_stores.get_mut(&source_store)
                .ok_or(StoreError::NotOpen(source_store))?;
            let children = store.get_children(mount_ref.source_node).await?;
            return Ok((source_store, children));
        }

        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        let children = store.get_children(node_id).await?;
        Ok((store_id, children))
    }

    /// Flush a store to disk
    pub async fn flush(&mut self, store_id: StoreId) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.flush().await
    }

    /// Flush all stores to disk
    pub async fn flush_all(&mut self) -> Result<()> {
        for store in self.local_stores.values_mut() {
            store.flush().await?;
        }
        Ok(())
    }

    /// Get the root node ID for a store
    pub fn root_node_id(&self, store_id: StoreId) -> Result<NodeId> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.root_node_id())
    }

    /// Get a reference to the store registry
    pub fn registry(&self) -> &StoreRegistry {
        &self.registry
    }

    /// Get a mutable reference to the store registry
    pub fn registry_mut(&mut self) -> &mut StoreRegistry {
        &mut self.registry
    }

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
    async fn open_registered_store(&mut self, store_id: StoreId) -> Result<()> {
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

    /// The current state of a mount point: `Live` if its source store is
    /// open or could be opened (see [`StoreManager::ensure_store_open`]),
    /// `Unavailable` otherwise. Actually attempts resolution rather than
    /// checking the registry alone, so a `Live` result means the source can
    /// really be reached right now.
    pub async fn mount_state(&mut self, mount_ref: &MountRef) -> MountState {
        match self.ensure_store_open(mount_ref).await {
            Ok(_) => MountState::Live,
            Err(_) => MountState::Unavailable,
        }
    }

    /// Check if creating a mount would create a cycle.
    pub async fn validate_mount_creation(
        &mut self,
        mounting_store: StoreId,
        mounting_node: NodeId,
        mount_ref: &MountRef,
    ) -> Result<()> {
        let ancestors = self.collect_ancestors(mounting_store, mounting_node).await?;

        self.ensure_store_open(mount_ref).await?;

        let mut stack: Vec<(StoreId, NodeId, usize)> = vec![
            (mount_ref.source_store, mount_ref.source_node, 0),
        ];

        while let Some((current_store, current_node, depth)) = stack.pop() {
            if depth > MAX_MOUNT_DEPTH {
                return Err(StoreError::MountDepthExceeded { depth });
            }

            self.open_registered_store(current_store).await?;

            let node = {
                let store = self.local_stores.get_mut(&current_store)
                    .ok_or(StoreError::NotOpen(current_store))?;
                store.get_node(current_node).await?
            };

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
    async fn collect_ancestors(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
    ) -> Result<Vec<NodeId>> {
        let mut ancestors = vec![node_id];
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;

        let mut current = node_id;
        loop {
            let node = store.get_node(current).await?;
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

impl Default for StoreManager {
    fn default() -> Self {
        Self::new()
    }
}
