//! Common RPC types

use std::path::PathBuf;

use pimble_core::{Node, NodeId, NodeMetadata, Store, StoreId, Workspace};
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
    /// Base64-encoded CRDT changes
    pub changes: Vec<String>,
}

/// Request to set a node's text content (replaces all content)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetNodeTextRequest {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// The new text content
    pub text: String,
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
