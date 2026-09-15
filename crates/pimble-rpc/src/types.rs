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

/// A document inside a vault store: one node's content, or the tree.
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

/// Request to update a node's content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateNodeContentRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// Base64-encoded full yrs snapshot (full replacement)
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
    /// point (its `store.yrs` is what actually holds them). Clients address
    /// every child by `(store_id, child.id)`.
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

/// A document editing operation, carrying a yrs v1 update (base64-encoded):
/// a delta or a reconciliation diff. Originates from the editor's
/// collaboration session and can be applied on any client or on the server.
/// The full-snapshot path is `updateNodeContent`, not an `EditOperation`.
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

/// Request to sync a store document (tree/metadata). Stateless: the client
/// sends its yrs state vector and the server responds with everything it has
/// beyond it. No per-client state is kept on the server. If the client also
/// has local changes the server lacks, it sends those separately via
/// `applyStoreUpdate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStoreDocumentRequest {
    pub store_id: StoreId,
    /// Base64-encoded yrs v1 state vector
    pub state_vector: String,
}

/// Response from store document sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStoreDocumentResponse {
    /// Base64-encoded yrs v1 update: everything the server has that the
    /// client's state vector lacked. May be empty (base64 of zero bytes) if
    /// the client was already up to date.
    pub diff: String,
    /// The server's own state vector (base64-encoded yrs v1), for the client
    /// to compute what it should send next.
    pub state_vector: String,
}

/// Request to apply a yrs update to the store document (a delta,
/// reconciliation diff, or whole snapshot) and broadcast it to the store's
/// other subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyStoreUpdateRequest {
    pub store_id: StoreId,
    pub client_id: String,
    /// Base64-encoded yrs v1 update
    pub update: String,
}

/// The most nodes one `syncNodeContents` request may name. The server
/// refuses more; `PimbleClient::sync_node_contents` splits a longer list.
pub const MAX_SYNC_NODE_CONTENTS: usize = 100;

/// Request to sync the content documents of several nodes in one round trip.
/// Stateless: for each node the client sends its yrs state vector and the
/// server responds with everything it has beyond it. No per-client state is
/// kept on the server. At most [`MAX_SYNC_NODE_CONTENTS`] nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncNodeContentsRequest {
    pub store_id: StoreId,
    pub nodes: Vec<NodeStateVector>,
}

/// One node's state vector in a [`SyncNodeContentsRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStateVector {
    pub node_id: NodeId,
    /// Base64-encoded yrs v1 state vector
    pub state_vector: String,
}

/// Response from node content sync: one entry per requested node the
/// server has, in request order. A node the server does not have is left
/// out (not an error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncNodeContentsResponse {
    pub nodes: Vec<NodeContentDiff>,
}

/// One node's answer in a [`SyncNodeContentsResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeContentDiff {
    pub node_id: NodeId,
    /// Base64-encoded yrs v1 update: everything the server has that the
    /// client's state vector lacked. May be empty (base64 of zero bytes) if
    /// the client was already up to date.
    pub diff: String,
    /// The server's own state vector (base64-encoded yrs v1), for the client
    /// to compute what it should send next.
    pub state_vector: String,
}

// ============================================================================
// Subscription Notification Types
// ============================================================================

/// Notification that a store's tree/metadata has changed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreChangedNotification {
    pub store_id: StoreId,
    /// What changed (for client-side routing)
    pub change_kind: StoreChangeKind,
    /// Which client caused this change (for echo suppression)
    #[serde(default)]
    pub source_client_id: Option<String>,
    /// The raw yrs update (base64-encoded) that caused the change, when one
    /// is available (currently: `applyStoreUpdate`), so subscribers can
    /// apply it directly instead of refetching. `None` otherwise.
    #[serde(default)]
    pub update: Option<String>,
}

/// Kind of store change. Structural kinds name every parent whose children
/// list changed, so a subscriber can refetch exactly those lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreChangeKind {
    NodeCreated { node_id: NodeId, parent_id: NodeId },
    /// `node_id` and its whole subtree were removed from `parent_id`.
    NodeDeleted { node_id: NodeId, parent_id: NodeId },
    NodeMoved { node_id: NodeId, old_parent_id: NodeId, new_parent_id: NodeId },
    MetadataUpdated { node_id: NodeId },
    ContentUpdated { node_id: NodeId },
    /// A yrs update to the store document (`applyStoreUpdate`, or a tree
    /// repair). `node_ids` are the node entries it created, removed or
    /// changed, including every parent whose children list changed.
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
