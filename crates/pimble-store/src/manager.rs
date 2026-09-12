//! Store manager - handles multiple open stores

use std::collections::HashMap;
use std::path::Path;

use pimble_core::{MountRef, MountState, Node, NodeId, NodeMetadata, Store, StoreId, StoreLocation, SyncState};
use pimble_crdt::{ContentDoc, StoreDocument};
use tracing::{info, warn};

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
}

impl StoreManager {
    /// Create a new store manager
    pub fn new() -> Self {
        Self {
            local_stores: HashMap::new(),
            registry: StoreRegistry::new(),
        }
    }

    /// Create a new local store
    pub async fn create_local_store(&mut self, path: impl AsRef<Path>, name: impl Into<String>) -> Result<StoreId> {
        let path = path.as_ref();
        let store = LocalStore::create(path, name).await?;
        let id = store.id;
        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
        self.local_stores.insert(id, store);
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
        Ok(id)
    }

    /// Close a store
    pub async fn close_store(&mut self, store_id: StoreId) -> Result<()> {
        if let Some(mut store) = self.local_stores.remove(&store_id) {
            store.flush().await?;
            info!("Closed store {}", store_id);
        }
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

    /// Create a node in a store
    pub async fn create_node(&mut self, store_id: StoreId, node: Node, parent_id: Option<NodeId>) -> Result<NodeId> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
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

    /// Get children of a node.
    ///
    /// If the node is a mount point, its children are transparently resolved
    /// from the source store instead of the local store.
    pub async fn get_children(&mut self, store_id: StoreId, node_id: NodeId) -> Result<Vec<Node>> {
        // First, check if this node is a mount
        let node = self.get_node(store_id, node_id).await?;
        if node.is_mount() {
            if let Some(mount_ref) = node.mount_ref() {
                return self.resolve_mount(&mount_ref).await;
            }
        }

        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.get_children(node_id).await
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

    /// Resolve a mount point, opening the source store if needed.
    /// Returns the source node's children (the mounted subtree's top level).
    pub async fn resolve_mount(&mut self, mount_ref: &MountRef) -> Result<Vec<Node>> {
        let mut chain = Vec::new();
        self.resolve_mount_with_chain(mount_ref, &mut chain, 0).await
    }

    /// Internal mount resolver with cycle detection.
    fn resolve_mount_with_chain<'a>(
        &'a mut self,
        mount_ref: &'a MountRef,
        chain: &'a mut Vec<(StoreId, NodeId)>,
        depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<Node>>> + Send + 'a>> {
        Box::pin(async move {
        let pair = (mount_ref.source_store, mount_ref.source_node);

        // Cycle check
        if chain.contains(&pair) {
            chain.push(pair);
            return Err(StoreError::MountCycle { chain: chain.clone() });
        }

        // Depth check
        if depth > MAX_MOUNT_DEPTH {
            return Err(StoreError::MountDepthExceeded { depth });
        }

        chain.push(pair);

        // Ensure the source store is open
        self.ensure_store_open(mount_ref.source_store).await?;

        // Get children
        let children = {
            let store = self.local_stores.get_mut(&mount_ref.source_store)
                .ok_or(StoreError::NotOpen(mount_ref.source_store))?;
            store.get_children(mount_ref.source_node).await?
        };

        // Validate any nested mounts
        for child in &children {
            if child.is_mount() {
                if let Some(nested_ref) = child.mount_ref() {
                    if let Err(e) = self.resolve_mount_with_chain(&nested_ref, chain, depth + 1).await {
                        warn!("Nested mount {} not resolvable: {}", child.id, e);
                    }
                }
            }
        }

        chain.pop();
        Ok(children)
        }) // end Box::pin
    }

    /// Ensure a store is open, opening it from the registry if necessary.
    async fn ensure_store_open(&mut self, store_id: StoreId) -> Result<()> {
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

    /// Get the current state of a mount point.
    pub fn mount_state(&self, mount_ref: &MountRef) -> MountState {
        if self.local_stores.contains_key(&mount_ref.source_store) {
            MountState::Live
        } else if self.registry.lookup(&mount_ref.source_store).is_some() {
            MountState::Live
        } else {
            MountState::Unavailable
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

        self.ensure_store_open(mount_ref.source_store).await?;

        let mut stack: Vec<(StoreId, NodeId, usize)> = vec![
            (mount_ref.source_store, mount_ref.source_node, 0),
        ];

        while let Some((current_store, current_node, depth)) = stack.pop() {
            if depth > MAX_MOUNT_DEPTH {
                return Err(StoreError::MountDepthExceeded { depth });
            }

            self.ensure_store_open(current_store).await?;

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
