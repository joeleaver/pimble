//! RPC method definitions using jsonrpsee

use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;

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

    /// Set a node's text content (replaces all content)
    #[method(name = "setNodeText")]
    async fn set_node_text(&self, request: SetNodeTextRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

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
    // Search Operations
    // ========================================================================

    /// Search across stores
    #[method(name = "search")]
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, ErrorObjectOwned>;
}

/// Helper function to convert any error to ErrorObjectOwned
pub fn to_rpc_error(e: impl std::fmt::Display) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>)
}
