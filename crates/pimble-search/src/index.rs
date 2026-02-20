//! Search index management

use pimble_core::{NodeId, StoreId};
use serde::{Deserialize, Serialize};

/// Search index for a single store
pub struct SearchIndex {
    pub store_id: StoreId,
    // Vector DB and FTS index will be added in Phase 4
}

impl SearchIndex {
    /// Create a new search index for a store
    pub fn new(store_id: StoreId) -> Self {
        Self { store_id }
    }

    /// Index a node's content
    pub async fn index_node(&mut self, _node_id: NodeId, _text: &str) -> crate::Result<()> {
        // TODO: Implement in Phase 4
        // 1. Generate embeddings using local model
        // 2. Add to vector database
        // 3. Add to full-text index
        Ok(())
    }

    /// Remove a node from the index
    pub async fn remove_node(&mut self, _node_id: NodeId) -> crate::Result<()> {
        // TODO: Implement in Phase 4
        Ok(())
    }

    /// Rebuild the entire index
    pub async fn rebuild(&mut self) -> crate::Result<()> {
        // TODO: Implement in Phase 4
        Ok(())
    }
}

/// Manages search indexes across multiple stores
pub struct SearchManager {
    // Store-specific indexes will be added in Phase 4
}

impl SearchManager {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn get_or_create_index(&mut self, store_id: StoreId) -> crate::Result<SearchIndex> {
        Ok(SearchIndex::new(store_id))
    }
}

impl Default for SearchManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Document to be indexed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDocument {
    pub node_id: NodeId,
    pub store_id: StoreId,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
}
