//! Store manager - handles multiple open stores

use std::collections::HashMap;
use std::path::Path;

use pimble_core::{Node, NodeId, Store, StoreId, StoreLocation, SyncState};
use pimble_crdt::CrdtDocument;
use tracing::info;

use crate::error::{Result, StoreError};
use crate::local::LocalStore;

/// Manages multiple open stores
pub struct StoreManager {
    /// Open local stores
    local_stores: HashMap<StoreId, LocalStore>,
}

impl StoreManager {
    /// Create a new store manager
    pub fn new() -> Self {
        Self {
            local_stores: HashMap::new(),
        }
    }

    /// Create a new local store
    pub async fn create_local_store(&mut self, path: impl AsRef<Path>, name: impl Into<String>) -> Result<StoreId> {
        let store = LocalStore::create(path.as_ref(), name).await?;
        let id = store.id;
        self.local_stores.insert(id, store);
        Ok(id)
    }

    /// Open an existing local store
    pub async fn open_local_store(&mut self, path: impl AsRef<Path>) -> Result<StoreId> {
        let store = LocalStore::open(path.as_ref()).await?;
        let id = store.id;

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
        store.get_node(node_id).await.map(|n| n.clone())
    }

    /// Create a node in a store
    pub async fn create_node(&mut self, store_id: StoreId, node: Node, parent_id: Option<NodeId>) -> Result<NodeId> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.create_node(node, parent_id).await
    }

    /// Delete a node from a store
    pub async fn delete_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.delete_node(node_id).await
    }

    /// Get a node's CRDT document
    pub async fn get_node_document(&mut self, store_id: StoreId, node_id: NodeId) -> Result<CrdtDocument> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.get_node_document(node_id).await
    }

    /// Save a node's CRDT document
    pub async fn save_node_document(&mut self, store_id: StoreId, node_id: NodeId, doc: &mut CrdtDocument) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.save_node_document(node_id, doc).await
    }

    /// Get children of a node
    pub async fn get_children(&mut self, store_id: StoreId, node_id: NodeId) -> Result<Vec<Node>> {
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
}

impl Default for StoreManager {
    fn default() -> Self {
        Self::new()
    }
}

// LocalStore.path is now public, no helper needed
