//! Store registry - maps store IDs to connection endpoints

use std::collections::HashMap;
use std::path::PathBuf;

use pimble_core::{AuthMethod, StoreId};
use serde::{Deserialize, Serialize};
use url::Url;

/// How to reach a store
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoreEndpoint {
    /// Local filesystem path
    Local { path: PathBuf },
    /// Remote server (future, but define the variant now)
    Remote { url: Url, auth: AuthMethod },
}

/// Registry of known stores and how to reach them.
///
/// The registry is populated from:
/// 1. Stores opened in the current session (auto-registered on open)
/// 2. The workspace file (persisted across sessions)
pub struct StoreRegistry {
    endpoints: HashMap<StoreId, StoreEndpoint>,
}

impl StoreRegistry {
    /// Create a new empty registry
    pub fn new() -> Self {
        Self {
            endpoints: HashMap::new(),
        }
    }

    /// Register a store endpoint
    pub fn register(&mut self, store_id: StoreId, endpoint: StoreEndpoint) {
        self.endpoints.insert(store_id, endpoint);
    }

    /// Unregister a store endpoint
    pub fn unregister(&mut self, store_id: &StoreId) {
        self.endpoints.remove(store_id);
    }

    /// Look up how to reach a store
    pub fn lookup(&self, store_id: &StoreId) -> Option<&StoreEndpoint> {
        self.endpoints.get(store_id)
    }

    /// Get all registered store endpoints
    pub fn all(&self) -> &HashMap<StoreId, StoreEndpoint> {
        &self.endpoints
    }
}

impl Default for StoreRegistry {
    fn default() -> Self {
        Self::new()
    }
}
