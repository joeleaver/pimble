//! Server-side sync state management
//!
//! Tracks per-client Automerge sync states for store documents. Node content
//! (yrs) sync is stateless: the client sends a state vector and the server
//! computes a diff on the spot, so no per-client state is kept for it (see
//! `RpcHandler::sync_node_content`).

use std::collections::HashMap;

use automerge::sync;
use pimble_core::StoreId;

/// Key for store document sync state: (client_id, store_id)
type StoreDocKey = (String, StoreId);

/// Manages sync states for all connected clients.
///
/// Each client maintains independent sync state per store document. States
/// are created on first sync request and persist until the client
/// disconnects or the document is closed.
pub struct ServerSyncManager {
    /// Sync states for store documents
    store_doc_states: HashMap<StoreDocKey, sync::State>,
}

impl ServerSyncManager {
    pub fn new() -> Self {
        Self {
            store_doc_states: HashMap::new(),
        }
    }

    /// Get or create a sync state for a store document.
    pub fn store_doc_state(&mut self, client_id: &str, store_id: StoreId) -> &mut sync::State {
        self.store_doc_states
            .entry((client_id.to_string(), store_id))
            .or_insert_with(sync::State::new)
    }

    /// Remove all sync states for a client.
    pub fn remove_client(&mut self, client_id: &str) {
        self.store_doc_states
            .retain(|(cid, _), _| cid != client_id);
    }

    /// Remove all sync states for a store (e.g., when store is closed).
    pub fn remove_store(&mut self, store_id: StoreId) {
        self.store_doc_states
            .retain(|(_, sid), _| *sid != store_id);
    }
}

impl Default for ServerSyncManager {
    fn default() -> Self {
        Self::new()
    }
}
