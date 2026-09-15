//! RPC method definitions using jsonrpsee

use jsonrpsee::core::SubscriptionResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::{NodeId, StoreId};

use crate::types::*;

/// Pimble RPC API
///
/// This defines all available RPC methods for the Pimble server.
#[rpc(server, client, namespace = "pimble")]
pub trait PimbleApi {
    // ========================================================================
    // Store Operations
    // ========================================================================

    /// Create a new local store
    #[method(name = "createStore")]
    async fn create_store(&self, request: CreateStoreRequest) -> Result<CreateStoreResponse, ErrorObjectOwned>;

    /// Open an existing store
    #[method(name = "openStore")]
    async fn open_store(&self, request: OpenStoreRequest) -> Result<OpenStoreResponse, ErrorObjectOwned>;

    /// Close a store
    #[method(name = "closeStore")]
    async fn close_store(&self, request: CloseStoreRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// List all open stores
    #[method(name = "listStores")]
    async fn list_stores(&self) -> Result<ListStoresResponse, ErrorObjectOwned>;

    // ========================================================================
    // Node Operations
    // ========================================================================

    /// Get a single node
    #[method(name = "getNode")]
    async fn get_node(&self, request: GetNodeRequest) -> Result<GetNodeResponse, ErrorObjectOwned>;

    /// Get multiple nodes
    #[method(name = "getNodes")]
    async fn get_nodes(&self, request: GetNodesRequest) -> Result<GetNodesResponse, ErrorObjectOwned>;

    /// Create a new node
    #[method(name = "createNode")]
    async fn create_node(&self, request: CreateNodeRequest) -> Result<CreateNodeResponse, ErrorObjectOwned>;

    /// Update a node's metadata
    #[method(name = "updateNodeMetadata")]
    async fn update_node_metadata(&self, request: UpdateNodeMetadataRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Update a node's content
    #[method(name = "updateNodeContent")]
    async fn update_node_content(&self, request: UpdateNodeContentRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Delete a node
    #[method(name = "deleteNode")]
    async fn delete_node(&self, request: DeleteNodeRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Move a node to a new parent
    #[method(name = "moveNode")]
    async fn move_node(&self, request: MoveNodeRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Get children of a node
    #[method(name = "getChildren")]
    async fn get_children(&self, request: GetChildrenRequest) -> Result<GetChildrenResponse, ErrorObjectOwned>;

    // ========================================================================
    // Mount Operations
    // ========================================================================

    /// Create a mount point node in a store
    #[method(name = "createMount")]
    async fn create_mount(&self, request: CreateMountRequest) -> Result<CreateMountResponse, ErrorObjectOwned>;

    /// Get the state of a mount point
    #[method(name = "getMountState")]
    async fn get_mount_state(&self, request: GetMountStateRequest) -> Result<GetMountStateResponse, ErrorObjectOwned>;

    // ========================================================================
    // Replica Sync Operations (docs/SYNC_CONTRACT.md)
    // ========================================================================

    /// Create a local replica of a store held by a remote Pimble server and
    /// link it. Answers once the first reconcile has finished (or timed out).
    #[method(name = "addRemoteStore")]
    async fn add_remote_store(&self, request: AddRemoteStoreRequest) -> Result<OpenStoreResponse, ErrorObjectOwned>;

    /// Link a local store to its twin on a remote server, or unlink it.
    #[method(name = "setStoreSync")]
    async fn set_store_sync(&self, request: SetStoreSyncRequest) -> Result<GetStoreSyncResponse, ErrorObjectOwned>;

    /// A store's sync link and the link's current state.
    #[method(name = "getStoreSync")]
    async fn get_store_sync(&self, request: GetStoreSyncRequest) -> Result<GetStoreSyncResponse, ErrorObjectOwned>;

    // ========================================================================
    // Workspace Operations
    // ========================================================================

    /// Load a workspace from file
    #[method(name = "loadWorkspace")]
    async fn load_workspace(&self, request: LoadWorkspaceRequest) -> Result<LoadWorkspaceResponse, ErrorObjectOwned>;

    /// Save a workspace to file
    #[method(name = "saveWorkspace")]
    async fn save_workspace(&self, request: SaveWorkspaceRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Create a new workspace
    #[method(name = "createWorkspace")]
    async fn create_workspace(&self, request: CreateWorkspaceRequest) -> Result<LoadWorkspaceResponse, ErrorObjectOwned>;

    // ========================================================================
    // Edit Operations (collaborative editing)
    // ========================================================================

    /// Apply an edit operation to a node and broadcast to other clients.
    #[method(name = "applyEdit")]
    async fn apply_edit(&self, request: ApplyEditRequest) -> Result<ApplyEditResponse, ErrorObjectOwned>;

    // ========================================================================
    // Sync Operations
    // ========================================================================

    /// Sync a store document (tree structure + metadata): send a yrs state
    /// vector, receive a diff of everything the server has beyond it.
    /// Stateless — the server keeps no per-client sync state. If the client
    /// also has local changes the server lacks, it sends those separately
    /// via `applyStoreUpdate`.
    #[method(name = "syncStoreDocument")]
    async fn sync_store_document(&self, request: SyncStoreDocumentRequest) -> Result<SyncStoreDocumentResponse, ErrorObjectOwned>;

    /// Sync a node's content document: send a yrs state vector, receive a
    /// diff of everything the server has beyond it. Stateless — the server
    /// keeps no per-client sync state for content documents.
    #[method(name = "syncNodeContent")]
    async fn sync_node_content(&self, request: SyncNodeContentRequest) -> Result<SyncNodeContentResponse, ErrorObjectOwned>;

    /// Apply a yrs update to the store document (tree structure + metadata)
    /// and broadcast it to the store's other subscribers.
    #[method(name = "applyStoreUpdate")]
    async fn apply_store_update(&self, request: ApplyStoreUpdateRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    // ========================================================================
    // Subscription Operations
    // ========================================================================

    /// Subscribe to changes in a store's tree structure and metadata.
    /// Notifications are sent whenever nodes are created, deleted, moved, or have metadata updated.
    #[subscription(name = "subscribeStoreChanges" => "storeChanged", unsubscribe = "unsubscribeStoreChanges", item = StoreChangedNotification)]
    async fn subscribe_store_changes(&self, store_id: StoreId) -> SubscriptionResult;

    /// Subscribe to changes in a specific node's content.
    /// Notifications are sent whenever the node's content document is modified.
    #[subscription(name = "subscribeNodeChanges" => "nodeChanged", unsubscribe = "unsubscribeNodeChanges", item = NodeContentChangedNotification)]
    async fn subscribe_node_changes(&self, store_id: StoreId, node_id: NodeId) -> SubscriptionResult;

    // ========================================================================
    // Search Operations
    // ========================================================================

    /// Search across stores
    #[method(name = "search")]
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, ErrorObjectOwned>;

    /// Rebuild a store's search index from scratch: delete the on-disk
    /// index and re-index every node from the store's documents. Returns
    /// the number of nodes indexed.
    #[method(name = "rebuildIndex")]
    async fn rebuild_index(&self, request: RebuildIndexRequest) -> Result<RebuildIndexResponse, ErrorObjectOwned>;
}

/// Helper function to convert any error to ErrorObjectOwned
pub fn to_rpc_error(e: impl std::fmt::Display) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>)
}

/// A store's search index is still building (backfilling a fulltext or
/// vector index). Carries `done`/`total` progress so the client can show a
/// "still indexing" state instead of a generic error.
pub fn index_building_error(done: usize, total: usize) -> ErrorObjectOwned {
    let err = crate::RpcError::IndexBuilding { done, total };
    ErrorObjectOwned::owned(
        err.code(),
        err.to_string(),
        Some(serde_json::json!({ "done": done, "total": total })),
    )
}
