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

    /// `Plain` stores hold readable documents the server merges and indexes;
    /// `Vault` stores hold only encrypted blobs (docs/CRYPTO_CONTRACT.md) and
    /// answer nothing but the vault RPCs. Missing in older serializations:
    /// plain.
    #[serde(default)]
    pub kind: StoreKind,

    /// What this store is currently *linked* to, distinct from `kind` (this
    /// store's own kind, always `Plain` for a desktop store even when it is
    /// vault-linked — docs/CRYPTO_CONTRACT.md: "the local store itself stays
    /// plain on disk"). `Plain` means unlinked or linked by an ordinary
    /// replica sync link to a `Plain` twin; `Vault` means linked by an
    /// encrypting vault link to a `Vault` twin (`cloudHostStore`/
    /// `cloudAddHostedStore`). This is what a UI checks to show "encrypted"
    /// on the sync badge instead of the ordinary sync icon. Missing in
    /// older serializations: plain (unlinked).
    #[serde(default)]
    pub sync_mode: StoreKind,

    /// What this device may change in the store (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5): `Full` for one's own stores and for an editor's scope, `Read`
    /// for a reader. Missing in older serializations: full.
    #[serde(default)]
    pub access: StoreAccess,

    /// An owner's email when this store reached this device as someone else's
    /// share. `None` for one's own stores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_by: Option<String>,

    /// The roots this device holds of the store: `[root_node_id]` for a whole
    /// store, the scope roots for a partial replica (a share's recipient). Empty
    /// in older serializations: read it as `[root_node_id]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<NodeId>,

    /// The roots among [`Store::roots`] this device may only read while it
    /// edits others (a role per shared root), plus roots the account no
    /// longer holds at all: `sync.json`'s `read_only_roots`. Empty when
    /// `access` answers for the whole store. What a node under one of them
    /// may be used for is on the node itself ([`crate::Node::access`]); this
    /// is here so a client can tell when that judgement has changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only_roots: Vec<NodeId>,

    /// Whether the store is reached through Pimble Cloud's relay, and from
    /// which end (docs/RELAY_CONTRACT.md). Beside `sync_mode`, which stays
    /// `Vault` for a relayed store (its link is a vault link, and a client
    /// from before the relay reads it as the encrypted link it is). Missing
    /// in older serializations: not relayed.
    #[serde(default, skip_serializing_if = "RelaySide::is_none")]
    pub relay: RelaySide,
}

impl Store {
    /// The roots to show: [`Store::roots`], or the store's root when none is listed.
    pub fn shown_roots(&self) -> Vec<NodeId> {
        if self.roots.is_empty() {
            vec![self.root_node_id]
        } else {
            self.roots.clone()
        }
    }

    /// Create a new local store
    pub fn new_local(name: impl Into<String>, path: PathBuf) -> Self {
        Self {
            id: StoreId::new(),
            name: name.into(),
            location: StoreLocation::Local { path },
            root_node_id: NodeId::new(),
            sync_state: SyncState::Offline,
            is_replica: false,
            kind: StoreKind::Plain,
            sync_mode: StoreKind::Plain,
            access: StoreAccess::Full,
            shared_by: None,
            roots: Vec::new(),
            read_only_roots: Vec::new(),
            relay: RelaySide::None,
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
            kind: StoreKind::Plain,
            sync_mode: StoreKind::Plain,
            access: StoreAccess::Full,
            shared_by: None,
            roots: Vec::new(),
            read_only_roots: Vec::new(),
            relay: RelaySide::None,
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

/// What a store holds on the server (docs/CRYPTO_CONTRACT.md "Data model").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StoreKind {
    #[default]
    Plain,
    Vault,
}

/// Whether a store is reached through Pimble Cloud's relay, and which end of
/// it this device is (docs/RELAY_CONTRACT.md). A relayed store is not hosted:
/// its encrypted twin lives on its owner's own machine, and Pimble Cloud
/// pipes members' connections to it and keeps nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RelaySide {
    /// Not relayed: unlinked, linked to another Pimble server, or linked to
    /// a twin hosted on Pimble Cloud.
    #[default]
    None,
    /// Shared from this computer (`cloudRelayStore`): this server holds the
    /// twin and serves it through the relay. People reach it while this
    /// computer is on and online.
    Owner,
    /// Served from its owner's computer: this device's link reaches it
    /// through the relay, and is `Offline` whenever the owner's computer is.
    Member,
}

impl RelaySide {
    /// Whether this is `None`, the value an answer leaves out.
    pub fn is_none(&self) -> bool {
        matches!(self, RelaySide::None)
    }
}

/// What this device may change in a store (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5): an editor edits everything in scope, a reader reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StoreAccess {
    #[default]
    Full,
    Read,
}

impl StoreAccess {
    /// What a refused write answers with, everywhere (the server's `-32004`,
    /// the browser backend's own refusal, the app's notice): the sentence
    /// alone, written for the person, shown as it is.
    pub const READ_ONLY_REFUSAL: &'static str = "You can read this, not change it.";

    /// Whether `message` is the refusal (with or without a `Forbidden: ` in front).
    pub fn refusal_in(message: &str) -> Option<&'static str> {
        let sentence = message.strip_prefix("Forbidden: ").unwrap_or(message);
        (sentence == Self::READ_ONLY_REFUSAL).then_some(Self::READ_ONLY_REFUSAL)
    }

    pub fn allows_write(self) -> bool {
        matches!(self, StoreAccess::Full)
    }

    /// Whether this is `Full`, the value an answer leaves out
    /// (`Node::access`'s `skip_serializing_if`).
    pub fn is_full(&self) -> bool {
        matches!(self, StoreAccess::Full)
    }
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

    /// A Pimble Cloud account session (docs/CRYPTO_CONTRACT.md "Desktop (E,
    /// after B)"): `url` is the accounts service's base URL (not the Pimble
    /// server the link actually connects to — that is `RemoteEndpoint.url`),
    /// `session` is the long-lived session token `POST /api/v1/login`
    /// returned. Never connected with directly: a caller mints a short-lived
    /// JWT via `POST {url}/api/v1/token` (`Authorization: Bearer <session>`)
    /// first and connects with that as `Bearer`.
    CloudSession {
        url: String,
        session: String,
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
/// Version 4 is the current layout (docs/NODE_DOCUMENT_CONTRACT.md section 3):
/// `manifest.json` plus `nodes/{id}.yrs`, one yrs document per node holding
/// its content, its place in the tree and its metadata. Version 3 (the same
/// plus `store.yrs`, the tree as a document of its own) is migrated at open;
/// there is no migration path from anything earlier, so such a store must
/// be re-imported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreManifest {
    /// Schema version. [`StoreManifest::CURRENT_VERSION`] once opened.
    pub version: u32,

    /// Store ID
    pub id: StoreId,

    /// Store name
    pub name: String,

    /// Root node ID. Meaningless for a `Vault` store (docs/CRYPTO_CONTRACT.md):
    /// a vault has no tree of its own on this server, but the field stays
    /// populated (a fresh id) so every manifest has the same shape.
    pub root_node_id: NodeId,

    /// When the store was created
    pub created_at: DateTime<Utc>,

    /// When the store was last modified
    pub modified_at: DateTime<Utc>,

    /// `Plain` (readable, indexed, CRDT tree + content) or `Vault`
    /// (encrypted blobs only, docs/CRYPTO_CONTRACT.md). Missing in older
    /// serializations: plain.
    #[serde(default)]
    pub kind: StoreKind,

    /// A partial replica's scope roots (docs/NODE_DOCUMENT_CONTRACT.md section
    /// 5): the shared nodes this replica holds, when it is a share's recipient
    /// and not a whole copy. Empty for a whole store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_roots: Vec<NodeId>,
}

impl StoreManifest {
    /// Current schema version: the node-document layout.
    pub const CURRENT_VERSION: u32 = 4;

    /// Create a new `Plain` manifest. See [`StoreManifest::new_with_kind`]
    /// for a `Vault` one.
    pub fn new(name: impl Into<String>, root_node_id: NodeId) -> Self {
        Self::new_with_kind(name, root_node_id, StoreKind::Plain)
    }

    /// Create a new manifest of a given [`StoreKind`] (docs/CRYPTO_CONTRACT.md
    /// "Pimble server, a store of kind `vault`").
    pub fn new_with_kind(name: impl Into<String>, root_node_id: NodeId, kind: StoreKind) -> Self {
        let now = Utc::now();
        Self {
            version: Self::CURRENT_VERSION,
            id: StoreId::new(),
            name: name.into(),
            root_node_id,
            created_at: now,
            modified_at: now,
            kind,
            scope_roots: Vec::new(),
        }
    }
}
