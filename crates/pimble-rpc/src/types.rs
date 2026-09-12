//! Common RPC types

use std::path::PathBuf;

use pimble_core::{MountRef, MountState, Node, NodeId, NodeMetadata, Store, StoreId, Workspace};
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
}

/// Response with search results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResultItem>,
    pub total: usize,
}

// ============================================================================
// Subscription Types (for WebSocket)
// ============================================================================

/// Subscribe to node changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeNodeRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
}

/// Subscribe to store changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeStoreRequest {
    pub store_id: StoreId,
}

/// Notification of node change
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeChangedNotification {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub change_type: ChangeType,
}

/// Type of change
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    Created,
    Updated,
    Deleted,
    Moved,
}

// ============================================================================
// Edit Operations (collaborative editing)
// ============================================================================

/// A document editing operation, carrying yrs-encoded content bytes.
/// These originate from the editor's collaboration session and can be
/// applied on any client or on the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOperation {
    /// A yrs v1 update (base64-encoded): a delta, or a reconciliation diff.
    /// The primary collaboration primitive — just broadcast and apply.
    IncrementalChanges { changes: String },
    /// A full yrs v1 snapshot (base64-encoded), for initial load or complex
    /// structural changes.
    ReplaceContent { content: String },
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

/// Request to sync a store document (tree/metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStoreDocumentRequest {
    pub store_id: StoreId,
    pub client_id: String,
    /// Base64-encoded Automerge sync message, or None to initiate
    pub message: Option<String>,
}

/// Response from store document sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStoreDocumentResponse {
    /// Base64-encoded Automerge sync message, or None if in sync
    pub message: Option<String>,
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
