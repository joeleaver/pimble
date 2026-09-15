//! Common RPC types

use std::path::PathBuf;

use pimble_core::{MountRef, MountState, Node, NodeId, NodeMetadata, RemoteEndpoint, Store, StoreId, SyncState, Workspace};
use serde::{Deserialize, Serialize};

// ============================================================================
// Store Operations
// ============================================================================

/// Request to create a new local store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateStoreRequest {
    pub path: PathBuf,
    pub name: String,
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
/// (`SyncState::Offline` for an unlinked store as well).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetStoreSyncResponse {
    pub remote: Option<RemoteEndpoint>,
    pub state: SyncState,
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

/// Request to sync a node's content document. Stateless: the client sends
/// its yrs state vector and the server responds with everything it has
/// beyond it. No per-client state is kept on the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncNodeContentRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// Base64-encoded yrs v1 state vector
    pub state_vector: String,
}

/// Response from node content sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncNodeContentResponse {
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

/// Kind of store change
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreChangeKind {
    NodeCreated { node_id: NodeId },
    NodeDeleted { node_id: NodeId },
    NodeMoved { node_id: NodeId },
    MetadataUpdated { node_id: NodeId },
    ContentUpdated { node_id: NodeId },
    TreeStructure,
    /// The store's replica sync link changed state (docs/SYNC_CONTRACT.md).
    SyncStateChanged { state: SyncState },
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
