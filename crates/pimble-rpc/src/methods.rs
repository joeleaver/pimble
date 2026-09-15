//! RPC method definitions using jsonrpsee

// `SubscriptionResult` is the *server* half's return type for a
// `#[subscription]` method, and jsonrpsee-core only defines it under its
// `server` feature, which wasm32 doesn't build (see this crate's
// Cargo.toml). The client half the macro generates for wasm32 never names
// it.
#[cfg(not(target_arch = "wasm32"))]
use jsonrpsee::core::SubscriptionResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::{NodeId, StoreId};

use crate::types::*;

/// Pimble RPC API
///
/// This defines all available RPC methods for the Pimble server.
///
/// Both halves are generated natively. On wasm32 only the client half is:
/// nothing in a browser implements the server trait, and asking for it
/// would drag jsonrpsee-server (and so mio) into a target it cannot build
/// for. The method set, names and types are identical either way — this is
/// a build-target gate, not a protocol difference.
#[cfg_attr(not(target_arch = "wasm32"), rpc(server, client, namespace = "pimble"))]
#[cfg_attr(target_arch = "wasm32", rpc(client, namespace = "pimble"))]
pub trait PimbleApi {
    // ========================================================================
    // Store Operations
    // ========================================================================

    /// Create a new local store. `Service`-only (docs/CLOUD_CONTRACT.md "B:
    /// pimble-server" item 5): a user principal never calls this directly —
    /// the accounts service does, on its behalf.
    #[method(name = "createStore", with_extensions)]
    async fn create_store(&self, request: CreateStoreRequest) -> Result<CreateStoreResponse, ErrorObjectOwned>;

    /// Open an existing store. `Service`-only.
    #[method(name = "openStore", with_extensions)]
    async fn open_store(&self, request: OpenStoreRequest) -> Result<OpenStoreResponse, ErrorObjectOwned>;

    /// Close a store. `Service`-only.
    #[method(name = "closeStore", with_extensions)]
    async fn close_store(&self, request: CloseStoreRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// List all open stores. A user principal sees only the stores it has a
    /// grant on.
    #[method(name = "listStores", with_extensions)]
    async fn list_stores(&self) -> Result<ListStoresResponse, ErrorObjectOwned>;

    // ========================================================================
    // Node Operations
    // ========================================================================

    /// Get a single node. Read.
    #[method(name = "getNode", with_extensions)]
    async fn get_node(&self, request: GetNodeRequest) -> Result<GetNodeResponse, ErrorObjectOwned>;

    /// Get multiple nodes. Read.
    #[method(name = "getNodes", with_extensions)]
    async fn get_nodes(&self, request: GetNodesRequest) -> Result<GetNodesResponse, ErrorObjectOwned>;

    /// Create a new node. Write.
    #[method(name = "createNode", with_extensions)]
    async fn create_node(&self, request: CreateNodeRequest) -> Result<CreateNodeResponse, ErrorObjectOwned>;

    /// Update a node's metadata. Write.
    #[method(name = "updateNodeMetadata", with_extensions)]
    async fn update_node_metadata(&self, request: UpdateNodeMetadataRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Update a node's content. Write.
    #[method(name = "updateNodeContent", with_extensions)]
    async fn update_node_content(&self, request: UpdateNodeContentRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Delete a node. Write.
    #[method(name = "deleteNode", with_extensions)]
    async fn delete_node(&self, request: DeleteNodeRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Move a node to a new parent. Write.
    #[method(name = "moveNode", with_extensions)]
    async fn move_node(&self, request: MoveNodeRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    /// Get children of a node. Read, and — when the node is a mount —
    /// also checked against the mount's source store.
    #[method(name = "getChildren", with_extensions)]
    async fn get_children(&self, request: GetChildrenRequest) -> Result<GetChildrenResponse, ErrorObjectOwned>;

    // ========================================================================
    // Mount Operations
    // ========================================================================

    /// Create a mount point node in a store. Write on the mounting store.
    #[method(name = "createMount", with_extensions)]
    async fn create_mount(&self, request: CreateMountRequest) -> Result<CreateMountResponse, ErrorObjectOwned>;

    /// Get the state of a mount point. Read.
    #[method(name = "getMountState", with_extensions)]
    async fn get_mount_state(&self, request: GetMountStateRequest) -> Result<GetMountStateResponse, ErrorObjectOwned>;

    // ========================================================================
    // Replica Sync Operations (docs/SYNC_CONTRACT.md)
    // ========================================================================

    /// Create a local replica of a store held by a remote Pimble server and
    /// link it. Answers once the first reconcile has finished (or timed out).
    /// `Service`-only: it reaches an arbitrary remote with this server's own
    /// saved credentials.
    #[method(name = "addRemoteStore", with_extensions)]
    async fn add_remote_store(&self, request: AddRemoteStoreRequest) -> Result<OpenStoreResponse, ErrorObjectOwned>;

    /// Link a local store to its twin on a remote server, or unlink it.
    /// `Service`-only (same reason as `addRemoteStore`).
    #[method(name = "setStoreSync", with_extensions)]
    async fn set_store_sync(&self, request: SetStoreSyncRequest) -> Result<GetStoreSyncResponse, ErrorObjectOwned>;

    /// A store's sync link and the link's current state. Read.
    #[method(name = "getStoreSync", with_extensions)]
    async fn get_store_sync(&self, request: GetStoreSyncRequest) -> Result<GetStoreSyncResponse, ErrorObjectOwned>;

    /// The stores a remote Pimble server has open, fetched by this server
    /// (which holds the saved credentials), so a client never connects to a
    /// remote itself. `Service`-only, for the same reason as
    /// `addRemoteStore`: it spends this server's own saved credentials
    /// against a remote the caller names, not a store this server holds.
    #[method(name = "listRemoteStores", with_extensions)]
    async fn list_remote_stores(&self, request: ListRemoteStoresRequest) -> Result<ListStoresResponse, ErrorObjectOwned>;

    /// Stop a replica's sync link, close it and delete its directory.
    /// `Service`-only.
    #[method(name = "removeReplica", with_extensions)]
    async fn remove_replica(&self, request: RemoveReplicaRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

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
    /// Write.
    #[method(name = "applyEdit", with_extensions)]
    async fn apply_edit(&self, request: ApplyEditRequest) -> Result<ApplyEditResponse, ErrorObjectOwned>;

    // ========================================================================
    // Sync Operations
    // ========================================================================

    /// Sync a store document (tree structure + metadata): send a yrs state
    /// vector, receive a diff of everything the server has beyond it.
    /// Stateless — the server keeps no per-client sync state. If the client
    /// also has local changes the server lacks, it sends those separately
    /// via `applyStoreUpdate`.
    /// Read (a user principal pulling is a reader operation; the push
    /// direction, `applyStoreUpdate`, is write).
    #[method(name = "syncStoreDocument", with_extensions)]
    async fn sync_store_document(&self, request: SyncStoreDocumentRequest) -> Result<SyncStoreDocumentResponse, ErrorObjectOwned>;

    /// Sync the content documents of up to `MAX_SYNC_NODE_CONTENTS` nodes:
    /// send a yrs state vector per node, receive a diff of everything the
    /// server has beyond each. Stateless — the server keeps no per-client
    /// sync state for content documents. Read (see `syncStoreDocument`).
    #[method(name = "syncNodeContents", with_extensions)]
    async fn sync_node_contents(&self, request: SyncNodeContentsRequest) -> Result<SyncNodeContentsResponse, ErrorObjectOwned>;

    /// Apply a yrs update to the store document (tree structure + metadata)
    /// and broadcast it to the store's other subscribers. Write.
    #[method(name = "applyStoreUpdate", with_extensions)]
    async fn apply_store_update(&self, request: ApplyStoreUpdateRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    // ========================================================================
    // Subscription Operations
    // ========================================================================

    /// Subscribe to changes in a store's tree structure and metadata.
    /// Notifications are sent whenever nodes are created, deleted, moved, or
    /// have metadata updated. Read.
    #[subscription(name = "subscribeStoreChanges" => "storeChanged", unsubscribe = "unsubscribeStoreChanges", item = StoreChangedNotification, with_extensions)]
    async fn subscribe_store_changes(&self, store_id: StoreId) -> SubscriptionResult;

    /// Subscribe to changes in a specific node's content.
    /// Notifications are sent whenever the node's content document is
    /// modified. Read.
    #[subscription(name = "subscribeNodeChanges" => "nodeChanged", unsubscribe = "unsubscribeNodeChanges", item = NodeContentChangedNotification, with_extensions)]
    async fn subscribe_node_changes(&self, store_id: StoreId, node_id: NodeId) -> SubscriptionResult;

    // ========================================================================
    // Search Operations
    // ========================================================================

    /// Search across stores. Read: the stores actually searched are
    /// filtered to what the principal may read (an empty `request.stores`
    /// means "every store the principal may read", not literally every
    /// open store, for a user principal).
    #[method(name = "search", with_extensions)]
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, ErrorObjectOwned>;

    /// Rebuild a store's search index from scratch: delete the on-disk
    /// index and re-index every node from the store's documents. Returns
    /// the number of nodes indexed. Write.
    #[method(name = "rebuildIndex", with_extensions)]
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

/// The connection's principal is authenticated but may not perform the
/// requested operation (docs/CLOUD_CONTRACT.md "B: pimble-server" item 5):
/// a user principal reaching a `Service`-only method, or one without the
/// role a store operation needs. JSON-RPC code `-32004`.
pub fn forbidden_error(message: impl Into<String>) -> ErrorObjectOwned {
    let err = crate::RpcError::Forbidden(message.into());
    ErrorObjectOwned::owned(err.code(), err.to_string(), None::<()>)
}
