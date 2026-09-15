//! Store types - containers for trees of nodes

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::NodeId;

/// Unique identifier for a store
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoreId(pub Uuid);

impl StoreId {
    /// Create a new random StoreId
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create a StoreId from an existing UUID
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Parse a StoreId from a string
    pub fn parse(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(s)?))
    }

    /// Get the underlying UUID
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for StoreId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A Store represents an entry point to a tree of nodes
///
/// Stores can be:
/// - Local: Stored on the local filesystem
/// - Remote: Accessed via a remote server
/// - Mounted: A subtree of another store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    /// Unique identifier for this store
    pub id: StoreId,

    /// Display name for the store
    pub name: String,

    /// Where the store data lives
    pub location: StoreLocation,

    /// The root node of this store's tree
    pub root_node_id: NodeId,

    /// Current synchronization state
    pub sync_state: SyncState,

    /// Whether this is a replica the server created for a remote store
    /// (`addRemoteStore`), living in the server's replicas directory. Only
    /// a replica can be removed with `removeReplica`. Filled in by the
    /// server on every `Store` it returns.
    #[serde(default)]
    pub is_replica: bool,
}

impl Store {
    /// Create a new local store
    pub fn new_local(name: impl Into<String>, path: PathBuf) -> Self {
        Self {
            id: StoreId::new(),
            name: name.into(),
            location: StoreLocation::Local { path },
            root_node_id: NodeId::new(),
            sync_state: SyncState::Offline,
            is_replica: false,
        }
    }

    /// Create a new remote store
    pub fn new_remote(name: impl Into<String>, url: Url, auth: AuthMethod) -> Self {
        Self {
            id: StoreId::new(),
            name: name.into(),
            location: StoreLocation::Remote { url, auth },
            root_node_id: NodeId::new(),
            sync_state: SyncState::Offline,
            is_replica: false,
        }
    }

    /// Check if this is a local store
    pub fn is_local(&self) -> bool {
        matches!(self.location, StoreLocation::Local { .. })
    }

    /// Check if this is a remote store
    pub fn is_remote(&self) -> bool {
        matches!(self.location, StoreLocation::Remote { .. })
    }

    /// Get the local path if this is a local store
    pub fn local_path(&self) -> Option<&PathBuf> {
        match &self.location {
            StoreLocation::Local { path } => Some(path),
            _ => None,
        }
    }
}

/// Where a store's data is located
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoreLocation {
    /// Local filesystem directory
    Local {
        /// Path to the .pimble directory
        path: PathBuf,
    },

    /// Remote server
    Remote {
        /// Server URL
        url: Url,
        /// Authentication method
        auth: AuthMethod,
    },

    /// Mounted subtree of another store
    Mounted {
        /// The parent store
        store_id: StoreId,
        /// The node to use as root
        node_id: NodeId,
    },
}

/// How to reach another Pimble server: the URL of its RPC endpoint and how
/// to authenticate. Used by replica sync links (`docs/SYNC_CONTRACT.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEndpoint {
    pub url: Url,
    pub auth: AuthMethod,
}

/// Authentication method for remote stores
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AuthMethod {
    /// No authentication
    None,

    /// API key authentication
    ApiKey {
        /// The API key (should be stored securely)
        key: String,
    },

    /// Bearer token authentication
    Bearer {
        /// The bearer token
        token: String,
    },

    /// OAuth2 authentication
    OAuth2 {
        /// Client ID
        client_id: String,
        /// Refresh token (access token is obtained dynamically)
        refresh_token: String,
    },
}

/// Synchronization state of a store
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SyncState {
    /// Not connected to any remote
    Offline,

    /// Currently synchronizing
    Syncing,

    /// Successfully synchronized
    Synced {
        /// When the last sync completed
        last_sync: DateTime<Utc>,
    },

    /// Has unresolved conflicts
    Conflict {
        /// Details about each conflict
        details: Vec<ConflictInfo>,
    },
}

impl SyncState {
    /// Check if the store is currently synced
    pub fn is_synced(&self) -> bool {
        matches!(self, SyncState::Synced { .. })
    }

    /// Check if there are conflicts
    pub fn has_conflicts(&self) -> bool {
        matches!(self, SyncState::Conflict { .. })
    }
}

/// Information about a sync conflict
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictInfo {
    /// The node with the conflict
    pub node_id: NodeId,

    /// Description of the conflict
    pub description: String,

    /// When the conflict was detected
    pub detected_at: DateTime<Utc>,
}

/// Store manifest - metadata stored in manifest.json
///
/// Version 3 is the only supported layout: `manifest.json` plus `store.yrs`
/// (tree structure and node metadata) and `nodes/{id}.yrs` (per-node content),
/// both yrs documents. There is no migration path from an earlier layout; a
/// store must be re-imported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreManifest {
    /// Schema version. Always [`StoreManifest::CURRENT_VERSION`].
    pub version: u32,

    /// Store ID
    pub id: StoreId,

    /// Store name
    pub name: String,

    /// Root node ID
    pub root_node_id: NodeId,

    /// When the store was created
    pub created_at: DateTime<Utc>,

    /// When the store was last modified
    pub modified_at: DateTime<Utc>,
}

impl StoreManifest {
    /// Current (and only) schema version
    pub const CURRENT_VERSION: u32 = 3;

    /// Create a new manifest
    pub fn new(name: impl Into<String>, root_node_id: NodeId) -> Self {
        let now = Utc::now();
        Self {
            version: Self::CURRENT_VERSION,
            id: StoreId::new(),
            name: name.into(),
            root_node_id,
            created_at: now,
            modified_at: now,
        }
    }
}
