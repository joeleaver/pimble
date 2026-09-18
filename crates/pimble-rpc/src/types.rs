//! Common RPC types

use std::path::PathBuf;

use pimble_core::{MountRef, MountState, Node, NodeId, NodeMetadata, RemoteEndpoint, Store, StoreId, StoreKind, SyncState, Workspace};
use serde::{Deserialize, Serialize};

// ============================================================================
// Store Operations
// ============================================================================

/// Request to create a new local store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateStoreRequest {
    pub path: PathBuf,
    pub name: String,
    /// `plain` (default) or `vault` (docs/CRYPTO_CONTRACT.md).
    #[serde(default)]
    pub kind: StoreKind,
    /// A chosen id, so the accounts service can create the hosted twin of a
    /// local store under the local store's id. Refused if already open here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<StoreId>,
}

// ── Vault (encrypted store) RPCs, docs/CRYPTO_CONTRACT.md ────────────────

/// A document inside a vault store: one node's document
/// (docs/NODE_DOCUMENT_CONTRACT.md section 4). `Tree` names the tree
/// document of the layout before it, which nothing writes any more; it stays
/// so a hosted twin migrated from that layout still lists, and a reader
/// skips it (its content is superseded by the node documents).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "id")]
pub enum VaultDocId {
    Node(NodeId),
    Tree,
}

impl VaultDocId {
    /// The on-disk and associated-data name: the node id, or `tree`.
    pub fn as_str(&self) -> String {
        match self {
            VaultDocId::Node(id) => id.to_string(),
            VaultDocId::Tree => "tree".to_string(),
        }
    }

    /// The inverse of [`VaultDocId::as_str`]: `"tree"` parses to
    /// [`VaultDocId::Tree`], anything else as a node id. Used to reconstruct
    /// a `VaultDocId` from the on-disk directory name `pimble-store`'s vault
    /// storage names each document by (`vaultListDocs`).
    pub fn parse(s: &str) -> Option<VaultDocId> {
        if s == "tree" {
            Some(VaultDocId::Tree)
        } else {
            NodeId::parse(s).ok().map(VaultDocId::Node)
        }
    }
}

/// One stored blob with its sequence number. `blob` is base64url (no padding) of the
/// `pimble_crypto::Blob` bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    pub seq: u64,
    pub blob: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultAppendRequest {
    pub store_id: StoreId,
    pub doc_id: VaultDocId,
    pub blob: String,
    /// The caller's id, echoed as `source_client_id` on the `VaultAppended`
    /// notification (the same way `applyEdit`'s `client_id` propagates), so
    /// a client can drop its own echo by identity rather than only by
    /// tracking sequence numbers it has already seen.
    #[serde(default)]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultAppendResponse {
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFetchRequest {
    pub store_id: StoreId,
    pub doc_id: VaultDocId,
    /// Everything after this sequence number (0 = from the beginning).
    #[serde(default)]
    pub after_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFetchResponse {
    /// The snapshot, when its seq is greater than `after_seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<VaultEntry>,
    /// Every update after `max(after_seq, snapshot.seq)`, ascending.
    pub updates: Vec<VaultEntry>,
    /// The document's latest sequence number (0 when empty).
    pub head: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSnapshotRequest {
    pub store_id: StoreId,
    pub doc_id: VaultDocId,
    /// The snapshot covers every update up to and including this seq.
    pub upto_seq: u64,
    pub blob: String,
    /// The sender vouches that its document reflected **every** entry
    /// `1..=upto_seq` when it made this blob (`VaultCursor::applied_through`),
    /// not merely that `upto_seq` is the number of its own latest append. The
    /// server deletes every log entry at or below `upto_seq`, so a snapshot
    /// without this guarantee destroys whatever it lacks; one that does not
    /// carry the flag (a client from before 2026-09-17) is acknowledged and
    /// ignored.
    #[serde(default)]
    pub covers_prefix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListDocsRequest {
    pub store_id: StoreId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDocInfo {
    pub doc_id: VaultDocId,
    pub head: u64,
    /// 0 when the document has no snapshot.
    pub snapshot_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListDocsResponse {
    pub docs: Vec<VaultDocInfo>,
}

/// Response after creating a store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateStoreResponse {
    pub store_id: StoreId,
    pub root_node_id: NodeId,
}

/// Request to open an existing store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenStoreRequest {
    pub path: PathBuf,
}

/// Response after opening a store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenStoreResponse {
    pub store: Store,
}

/// Request to close a store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseStoreRequest {
    pub store_id: StoreId,
}

/// Request to list all open stores
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListStoresRequest {}

/// Response with list of open stores
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListStoresResponse {
    pub stores: Vec<Store>,
}

// ============================================================================
// Node Operations
// ============================================================================

/// Request to get a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetNodeRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// Response with a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetNodeResponse {
    pub node: Node,
}

/// Request to get multiple nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetNodesRequest {
    pub store_id: StoreId,
    pub node_ids: Vec<NodeId>,
}

/// Response with multiple nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetNodesResponse {
    pub nodes: Vec<Node>,
}

/// Request to create a new node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateNodeRequest {
    pub store_id: StoreId,
    pub parent_id: Option<NodeId>,
    pub node_type: String,
    pub title: String,
}

/// Response after creating a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateNodeResponse {
    pub node_id: NodeId,
}

/// Request to update a node's metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateNodeMetadataRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub metadata: NodeMetadata,
}

/// Request to seed a node's content with a whole document snapshot. The
/// snapshot is merged into the node's document like any update, so this is
/// only right for a node whose content was never written (the importer): a
/// snapshot that shares no history with the copies replicas hold merges in
/// beside their paragraphs instead of replacing them. An edit of existing
/// content is an `applyEdit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateNodeContentRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// Base64-encoded yrs v1 update: a whole document snapshot
    pub content: String,
    /// Client ID for echo suppression in notifications
    #[serde(default)]
    pub client_id: Option<String>,
}

/// Request to delete a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteNodeRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// Request to undelete a node (docs/NODE_DOCUMENT_CONTRACT.md section 2:
/// a deletion is a tombstone, and this clears it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndeleteNodeRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// Request to move a node to a new parent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveNodeRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub new_parent_id: NodeId,
    /// Position within the new parent's children (None = append)
    pub position: Option<usize>,
}

/// Request to get children of a node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetChildrenRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// Response with children nodes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetChildrenResponse {
    /// The canonical store the returned children live in: the request's
    /// `store_id` for an ordinary node, the mount's source store for a mount
    /// point (its node documents are what actually hold them). Clients
    /// address every child by `(store_id, child.id)`.
    pub store_id: StoreId,
    pub children: Vec<Node>,
}

// ============================================================================
// Mount Operations
// ============================================================================

/// Request to create a mount point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMountRequest {
    pub store_id: StoreId,
    pub parent_id: NodeId,
    pub source_store_id: StoreId,
    pub source_node_id: NodeId,
    pub title: Option<String>,
}

/// Response after creating a mount point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateMountResponse {
    pub node_id: NodeId,
    /// The server's own `MountRef` (including any `source_path` hint it
    /// filled in from the registry). Callers use this directly rather than
    /// reconstructing one locally, since only the server knows the hint.
    pub mount_ref: MountRef,
}

/// Request to get mount state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMountStateRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// Response with mount state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMountStateResponse {
    pub state: MountState,
    pub mount_ref: MountRef,
}

// ============================================================================
// Replica Sync Operations (docs/SYNC_CONTRACT.md)
// ============================================================================

/// Create a local replica of `remote_store_id` as it exists on `remote`,
/// at `path`, linked to it. Answers with the opened store once the first
/// reconcile has finished (or after a timeout, with the current state).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddRemoteStoreRequest {
    pub remote: RemoteEndpoint,
    pub remote_store_id: StoreId,
    /// Where to create the replica. `None` lets the server choose:
    /// `<data dir>/pimble/replicas/<store id>.pimble`.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

/// Link a local store to the same store on a remote server (`Some`) or
/// unlink it (`None`). The link is persisted in the store directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetStoreSyncRequest {
    pub store_id: StoreId,
    pub remote: Option<RemoteEndpoint>,
}

/// Ask for a store's sync link and its current state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetStoreSyncRequest {
    pub store_id: StoreId,
}

/// A store's sync link (`None` when unlinked) and the link's current state
/// (`SyncState::Offline` for an unlinked store as well). `remote.auth` is
/// always `AuthMethod::None`: a server never returns a credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetStoreSyncResponse {
    pub remote: Option<RemoteEndpoint>,
    pub state: SyncState,
    /// `Plain` (an ordinary sync link, or unlinked) or `Vault` (an
    /// encrypting vault link, docs/CRYPTO_CONTRACT.md) — mirrors
    /// `Store::sync_mode`. Missing in an older client's expectations: plain.
    #[serde(default)]
    pub sync_mode: StoreKind,
}

/// Ask this server for the stores a remote Pimble server has open. The
/// server connects to the remote itself, with `remote.auth` when it is not
/// `None` and otherwise with the credential it saved for that remote
/// (docs/history/HARDENING_CONTRACT.md decisions 4 and 5). Answers with
/// `ListStoresResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListRemoteStoresRequest {
    pub remote: RemoteEndpoint,
}

/// Remove a replica: stop its sync link, close it, and delete its
/// directory. Refused for a store outside the server's replicas directory,
/// and for a replica whose link is not `Synced` unless `force`
/// (docs/history/HARDENING_CONTRACT.md decision 6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveReplicaRequest {
    pub store_id: StoreId,
    #[serde(default)]
    pub force: bool,
}

// ============================================================================
// Workspace Operations
// ============================================================================

/// Request to load a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadWorkspaceRequest {
    pub path: PathBuf,
}

/// Response after loading a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadWorkspaceResponse {
    pub workspace: Workspace,
}

/// Request to save a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveWorkspaceRequest {
    pub workspace: Workspace,
    pub path: PathBuf,
}

/// Request to create a new workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub name: String,
    pub path: PathBuf,
}

// ============================================================================
// Search Operations
// ============================================================================

/// Request to search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub stores: Vec<StoreId>,
    pub semantic: bool,
    pub limit: usize,
}

/// A single search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResultItem {
    pub node_id: NodeId,
    pub store_id: StoreId,
    pub score: f32,
    pub title: String,
    pub snippet: String,
    /// The matched unit/chunk's kind, from the contract's vocabulary:
    /// "prose", "heading", "code", "table", "field", or "other". Drives how
    /// the UI renders the hit (e.g. as a table row vs. a paragraph).
    pub kind: String,
    /// The node's own type (e.g. "document", "folder"), for icon/rendering
    /// choices independent of the matched chunk's kind.
    pub node_type: String,
    /// Locator of the matched chunk/unit within the node (e.g. a block
    /// ordinal), for deep-linking and highlighting. Empty when the hit is a
    /// whole-node keyword match with no specific location.
    pub path: String,
}

/// Response with search results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultItem>,
    pub total: usize,
}

/// Request to rebuild a store's search index from scratch: delete
/// `index/rhypedb/` and re-index every node from the store's documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuildIndexRequest {
    pub store_id: StoreId,
}

/// Response after rebuilding a store's search index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuildIndexResponse {
    pub indexed: usize,
}

// ============================================================================
// Edit Operations (collaborative editing)
// ============================================================================

/// A document editing operation, carrying a yrs v1 update (base64-encoded)
/// to a node's document: an editor's delta, a reconciliation diff, a tree
/// operation's transaction, or a whole snapshot (docs/NODE_DOCUMENT_CONTRACT.md
/// section 4: one shape for content and structure alike). Applied on any
/// client or on the server by merging.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOperation {
    IncrementalChanges { changes: String },
}

/// Request to apply an edit operation to a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyEditRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub client_id: String,
    pub operation: EditOperation,
}

/// Response after applying an edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyEditResponse {}

// ============================================================================
// Sync Operations
// ============================================================================

/// The most documents one `syncNodes` request may name. The server refuses
/// more; `PimbleClient::sync_nodes` splits a longer list.
pub const MAX_SYNC_NODE_CONTENTS: usize = 100;

/// One node's state vector in a [`SyncNodesRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStateVector {
    pub node_id: NodeId,
    /// Base64-encoded yrs v1 state vector
    pub state_vector: String,
}

/// One node's answer in a [`SyncNodesResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeContentDiff {
    pub node_id: NodeId,
    /// Base64-encoded yrs v1 update: everything the server has that the
    /// client's state vector lacked. Never literally empty: a yrs diff
    /// carries the whole delete set even when the client is up to date, so
    /// only a merge can tell (docs/history/HARDENING_CONTRACT.md decision 7).
    pub diff: String,
    /// The server's own state vector (base64-encoded yrs v1), for the client
    /// to compute what it should send next.
    pub state_vector: String,
}

/// Sync whole node documents (structure and content together,
/// docs/NODE_DOCUMENT_CONTRACT.md section 4). Stateless: for each named node
/// the caller sends its yrs state vector and the server answers with
/// everything it has beyond it; with `list_unknown` the server also names
/// every document of the store the caller did not name, so a fresh replica
/// learns what to ask for next (it then names those with empty state
/// vectors, at most [`MAX_SYNC_NODE_CONTENTS`] per call). No per-client
/// state on the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncNodesRequest {
    pub store_id: StoreId,
    pub nodes: Vec<NodeStateVector>,
    #[serde(default)]
    pub list_unknown: bool,
}

/// One entry per requested node the server has, in request order (a node the
/// server does not have is left out), plus the ids it has that were not asked
/// about when `list_unknown` was set. Tombstoned documents are included: a
/// deletion is part of the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncNodesResponse {
    pub nodes: Vec<NodeContentDiff>,
    #[serde(default)]
    pub unknown_ids: Vec<NodeId>,
}

// ============================================================================
// Subscription Notification Types
// ============================================================================

/// Notification that one of a store's documents changed
/// (docs/NODE_DOCUMENT_CONTRACT.md section 4). Every document kind
/// (`NodeCreated`, `NodeDeleted`, `NodeMoved`, `MetadataUpdated`,
/// `ContentUpdated`, `TreeStructure`) is about exactly one node document and
/// carries that document's update bytes in `update`, so a subscriber (a sync
/// link, an editor) applies them and never refetches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreChangedNotification {
    pub store_id: StoreId,
    /// What changed (for client-side routing)
    pub change_kind: StoreChangeKind,
    /// Which client caused this change (for echo suppression). `None` for
    /// an edit the server made itself (a repair, a `modified_at` stamp).
    #[serde(default)]
    pub source_client_id: Option<String>,
    /// The yrs v1 update (base64-encoded) to the one node document the kind
    /// names, so subscribers can apply it directly instead of refetching.
    /// `None` only for the kinds that are not about a document
    /// (`SyncStateChanged`, `MountStateChanged`).
    #[serde(default)]
    pub update: Option<String>,
}

/// Kind of store change. The server derives it from what an update changed
/// in a node document: newly initialised is `NodeCreated`, a set tombstone
/// is `NodeDeleted`, a changed `parent_id` is `NodeMoved`, any other change
/// to the `node` root is `MetadataUpdated`, a changed `content` (or plugin
/// `data`) root is `ContentUpdated`, and a changed children list is
/// `TreeStructure`. The tree RPCs emit the same kinds for the node they act
/// on. Structural kinds name every parent whose children list changed, so a
/// subscriber can refetch exactly those lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreChangeKind {
    NodeCreated { node_id: NodeId, parent_id: NodeId },
    /// `node_id` and its whole subtree were removed from `parent_id`.
    NodeDeleted { node_id: NodeId, parent_id: NodeId },
    NodeMoved { node_id: NodeId, old_parent_id: NodeId, new_parent_id: NodeId },
    MetadataUpdated { node_id: NodeId },
    ContentUpdated { node_id: NodeId },
    /// A node document changed in a way no other kind names: its children
    /// list (a parent of a created, moved or deleted node; a repair), or a
    /// root node arriving on a replica. `node_ids` names that document; a
    /// subscriber refetches its list and its parent's.
    TreeStructure { node_ids: Vec<NodeId> },
    /// The store's replica sync link changed state (docs/SYNC_CONTRACT.md).
    SyncStateChanged { state: SyncState },
    /// A mount node in this store changed state because its source store's
    /// link did, or its source's replica finished (or failed) being created
    /// (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 5). Derived state: sync
    /// links never forward it.
    MountStateChanged { node_id: NodeId, state: MountState },
    /// A blob was appended to a vault store's document (`vaultAppend`,
    /// docs/CRYPTO_CONTRACT.md); the notification's `update` carries the blob
    /// (base64url) so a live subscriber never re-fetches.
    VaultAppended { doc_id: VaultDocId, seq: u64 },
}

/// Notification that a node's content has changed.
/// Carries the edit operation so the receiving client can apply it directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeContentChangedNotification {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// Which client caused this change (for echo suppression)
    #[serde(default)]
    pub source_client_id: Option<String>,
    /// The edit operation to apply. When present, the client can apply it
    /// directly via the CE API without fetching the full document.
    #[serde(default)]
    pub operation: Option<EditOperation>,
}

// ============================================================================
// Common Response Types
// ============================================================================

/// Empty success response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmptyResponse {}

/// Generic error response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub code: i32,
    pub message: String,
}

// ============================================================================
// Cloud (Pimble Cloud account) Operations, docs/CRYPTO_CONTRACT.md
// "Desktop (E, after B): sign-in and the encrypting link"
// ============================================================================
//
// Service-only: a desktop app's local server calls these on its own behalf
// (there is one signed-in account per running server, held in
// `pimble_server::keystore::Keystore`), never a user principal over a
// hosted, JWT-verified connection.

/// Sign in to a Pimble Cloud account: derive `auth_key`/`kek` from
/// `password` against the account's KDF parameters, log in, fetch and
/// unwrap the account's keys with `kek`, and persist all of it (email,
/// cloud url, session token, unwrapped keys) in the local keystore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudSignInRequest {
    /// The accounts service's base URL, e.g. `https://pimble.app`.
    pub url: String,
    pub email: String,
    pub password: String,
}

/// Whether this server currently holds a signed-in Pimble Cloud account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudStatusResponse {
    pub signed_in: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Host a local store's encrypted twin on Pimble Cloud: create a hosted
/// store of kind `vault` under the local store's own id, generate a store
/// key, wrap it to the signed-in account, upload the envelope, and link the
/// local store to the hosted twin in vault mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudHostStoreRequest {
    pub store_id: StoreId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudHostStoreResponse {
    pub store_id: StoreId,
}

/// One store the signed-in account has a grant on, as the accounts service
/// reports it (`GET /api/v1/stores`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudHostedStoreInfo {
    pub store_id: String,
    pub name: String,
    pub role: String,
    /// `"plain"` or `"vault"`.
    pub kind: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudListHostedStoresResponse {
    pub stores: Vec<CloudHostedStoreInfo>,
}

/// Add an already-hosted store as a local replica: fetch the caller's key
/// envelopes for it, unwrap them, create an empty local `Plain` store under
/// the same id, and link it to the hosted twin in vault mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudAddHostedStoreRequest {
    pub store_id: StoreId,
}
