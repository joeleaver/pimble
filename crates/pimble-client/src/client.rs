//! RPC client implementation

use std::path::Path;

use jsonrpsee::core::client::SubscriptionClientT;
use jsonrpsee::ws_client::{WsClient, WsClientBuilder};
use pimble_core::{Node, NodeId, Store, StoreId, Workspace};
use pimble_core::MountRef;
use pimble_rpc::{
    ApplyEditRequest, ApplyStoreUpdateRequest, CloseStoreRequest, CreateMountRequest,
    CreateNodeRequest, CreateStoreRequest, CreateWorkspaceRequest, DeleteNodeRequest,
    EditOperation, GetChildrenRequest, GetMountStateRequest, GetNodeRequest, GetNodesRequest,
    LoadWorkspaceRequest, MoveNodeRequest, NodeContentChangedNotification, OpenStoreRequest,
    PimbleApiClient, RebuildIndexRequest, SaveWorkspaceRequest, SearchRequest, SearchResultItem,
    StoreChangedNotification, SyncNodeContentRequest, SyncStoreDocumentRequest,
    UpdateNodeContentRequest, UpdateNodeMetadataRequest,
};
use tracing::debug;
use url::Url;

use crate::error::{ClientError, Result};

/// Client for connecting to a Pimble server via WebSocket.
///
/// Uses WebSocket transport to support both RPC calls and subscriptions.
pub struct PimbleClient {
    client: WsClient,
    base_url: Url,
}

impl PimbleClient {
    /// Connect to a Pimble server via WebSocket.
    ///
    /// Accepts HTTP URLs (http://, https://) and automatically converts them
    /// to WebSocket URLs (ws://, wss://).
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        let base_url: Url = url
            .as_ref()
            .parse()
            .map_err(|e| ClientError::Connection(format!("Invalid URL: {}", e)))?;

        // Convert http:// to ws:// for WebSocket connection
        let ws_url = match base_url.scheme() {
            "http" => {
                let mut ws = base_url.clone();
                ws.set_scheme("ws").map_err(|_| ClientError::Connection("Failed to set ws scheme".into()))?;
                ws
            }
            "https" => {
                let mut ws = base_url.clone();
                ws.set_scheme("wss").map_err(|_| ClientError::Connection("Failed to set wss scheme".into()))?;
                ws
            }
            "ws" | "wss" => base_url.clone(),
            other => return Err(ClientError::Connection(format!("Unsupported scheme: {}", other))),
        };

        let client = WsClientBuilder::default()
            .build(&ws_url)
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        debug!("Connected to Pimble server at {} (ws: {})", base_url, ws_url);

        Ok(Self { client, base_url })
    }

    /// Get the server URL
    pub fn url(&self) -> &Url {
        &self.base_url
    }

    // ========================================================================
    // Store Operations
    // ========================================================================

    /// Create a new local store
    pub async fn create_store(&self, path: impl AsRef<Path>, name: impl Into<String>) -> Result<(StoreId, NodeId)> {
        let request = CreateStoreRequest {
            path: path.as_ref().to_path_buf(),
            name: name.into(),
        };

        let response = self
            .client
            .create_store(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok((response.store_id, response.root_node_id))
    }

    /// Open an existing store
    pub async fn open_store(&self, path: impl AsRef<Path>) -> Result<Store> {
        let request = OpenStoreRequest {
            path: path.as_ref().to_path_buf(),
        };

        let response = self
            .client
            .open_store(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.store)
    }

    /// Close a store
    pub async fn close_store(&self, store_id: StoreId) -> Result<()> {
        let request = CloseStoreRequest { store_id };

        self.client
            .close_store(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    /// List all open stores
    pub async fn list_stores(&self) -> Result<Vec<Store>> {
        let response = self
            .client
            .list_stores()
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.stores)
    }

    // ========================================================================
    // Node Operations
    // ========================================================================

    /// Get a single node
    pub async fn get_node(&self, store_id: StoreId, node_id: NodeId) -> Result<Node> {
        let request = GetNodeRequest { store_id, node_id };

        let response = self
            .client
            .get_node(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.node)
    }

    /// Get multiple nodes
    pub async fn get_nodes(&self, store_id: StoreId, node_ids: Vec<NodeId>) -> Result<Vec<Node>> {
        let request = GetNodesRequest { store_id, node_ids };

        let response = self
            .client
            .get_nodes(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.nodes)
    }

    /// Create a new node
    pub async fn create_node(
        &self,
        store_id: StoreId,
        parent_id: Option<NodeId>,
        node_type: impl Into<String>,
        title: impl Into<String>,
    ) -> Result<NodeId> {
        let request = CreateNodeRequest {
            store_id,
            parent_id,
            node_type: node_type.into(),
            title: title.into(),
        };

        let response = self
            .client
            .create_node(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.node_id)
    }

    /// Update a node's metadata
    pub async fn update_node_metadata(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        metadata: pimble_core::NodeMetadata,
    ) -> Result<()> {
        let request = UpdateNodeMetadataRequest {
            store_id,
            node_id,
            metadata,
        };

        self.client
            .update_node_metadata(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    /// Update a node's content with raw document bytes
    pub async fn set_node_content_bytes(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        content: Vec<u8>,
        client_id: Option<String>,
    ) -> Result<()> {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&content);

        let request = UpdateNodeContentRequest {
            store_id,
            node_id,
            content: encoded,
            client_id,
        };

        self.client
            .update_node_content(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    /// Delete a node
    pub async fn delete_node(&self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        let request = DeleteNodeRequest { store_id, node_id };

        self.client
            .delete_node(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    /// Move a node to a new parent
    pub async fn move_node(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> Result<()> {
        let request = MoveNodeRequest {
            store_id,
            node_id,
            new_parent_id,
            position,
        };

        self.client
            .move_node(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    /// Get children of a node. Returns the canonical store the children live
    /// in (the mount's source store when `node_id` is a mount point) and the
    /// children themselves; address each child by `(store, child.id)`.
    pub async fn get_children(&self, store_id: StoreId, node_id: NodeId) -> Result<(StoreId, Vec<Node>)> {
        let request = GetChildrenRequest { store_id, node_id };

        let response = self
            .client
            .get_children(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok((response.store_id, response.children))
    }

    // ========================================================================
    // Mount Operations
    // ========================================================================

    /// Create a mount point node in a store
    pub async fn create_mount(
        &self,
        store_id: StoreId,
        parent_id: NodeId,
        source_store_id: StoreId,
        source_node_id: NodeId,
        title: Option<String>,
    ) -> Result<(NodeId, MountRef)> {
        let request = CreateMountRequest {
            store_id,
            parent_id,
            source_store_id,
            source_node_id,
            title,
        };

        let response = self
            .client
            .create_mount(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok((response.node_id, response.mount_ref))
    }

    /// Get the state of a mount point
    pub async fn get_mount_state(
        &self,
        store_id: StoreId,
        node_id: NodeId,
    ) -> Result<(pimble_core::MountState, MountRef)> {
        let request = GetMountStateRequest { store_id, node_id };

        let response = self
            .client
            .get_mount_state(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok((response.state, response.mount_ref))
    }

    // ========================================================================
    // Workspace Operations
    // ========================================================================

    /// Load a workspace from file
    pub async fn load_workspace(&self, path: impl AsRef<Path>) -> Result<Workspace> {
        let request = LoadWorkspaceRequest {
            path: path.as_ref().to_path_buf(),
        };

        let response = self
            .client
            .load_workspace(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.workspace)
    }

    /// Save a workspace to file
    pub async fn save_workspace(&self, workspace: Workspace, path: impl AsRef<Path>) -> Result<()> {
        let request = SaveWorkspaceRequest {
            workspace,
            path: path.as_ref().to_path_buf(),
        };

        self.client
            .save_workspace(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    /// Create a new workspace
    pub async fn create_workspace(
        &self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<Workspace> {
        let request = CreateWorkspaceRequest {
            name: name.into(),
            path: path.as_ref().to_path_buf(),
        };

        let response = self
            .client
            .create_workspace(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.workspace)
    }

    // ========================================================================
    // Edit Operations (collaborative editing)
    // ========================================================================

    /// Apply an edit operation to a node and broadcast to other clients.
    pub async fn apply_edit(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        client_id: &str,
        operation: EditOperation,
    ) -> Result<()> {
        let request = ApplyEditRequest {
            store_id,
            node_id,
            client_id: client_id.to_string(),
            operation,
        };

        self.client
            .apply_edit(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    // ========================================================================
    // Sync Operations
    // ========================================================================

    /// Sync a store document (tree + metadata): send our yrs state vector,
    /// get back everything the server has beyond it plus the server's own
    /// state vector. Stateless on both ends.
    pub async fn sync_store_document(
        &self,
        store_id: StoreId,
        state_vector: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        use base64::Engine;

        let request = SyncStoreDocumentRequest {
            store_id,
            state_vector: base64::engine::general_purpose::STANDARD.encode(state_vector),
        };

        let response = self
            .client
            .sync_store_document(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        let diff = base64::engine::general_purpose::STANDARD
            .decode(&response.diff)
            .map_err(|e| ClientError::Rpc(format!("Invalid base64 diff: {}", e)))?;
        let server_sv = base64::engine::general_purpose::STANDARD
            .decode(&response.state_vector)
            .map_err(|e| ClientError::Rpc(format!("Invalid base64 state vector: {}", e)))?;

        Ok((diff, server_sv))
    }

    /// Apply a yrs update to the store document (a delta, reconciliation
    /// diff, or whole snapshot) and broadcast it to the store's other
    /// subscribers.
    ///
    /// Deviation from the Phase B contract's 2-arg signature
    /// (`apply_store_update(store_id, update)`): `ApplyStoreUpdateRequest`
    /// carries a `client_id` (for echo suppression via
    /// `StoreChangedNotification::source_client_id`, same as `apply_edit`),
    /// and `PimbleClient` has no stored client id field, so it is taken as
    /// an explicit parameter here, matching `apply_edit`'s existing pattern.
    pub async fn apply_store_update(&self, store_id: StoreId, client_id: &str, update: &[u8]) -> Result<()> {
        use base64::Engine;

        let request = ApplyStoreUpdateRequest {
            store_id,
            client_id: client_id.to_string(),
            update: base64::engine::general_purpose::STANDARD.encode(update),
        };

        self.client
            .apply_store_update(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    /// Sync a node's content document: send our yrs state vector, get back
    /// everything the server has beyond it plus the server's own state
    /// vector. Stateless on both ends.
    pub async fn sync_node_content(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        state_vector: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        use base64::Engine;

        let request = SyncNodeContentRequest {
            store_id,
            node_id,
            state_vector: base64::engine::general_purpose::STANDARD.encode(state_vector),
        };

        let response = self
            .client
            .sync_node_content(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        let diff = base64::engine::general_purpose::STANDARD
            .decode(&response.diff)
            .map_err(|e| ClientError::Rpc(format!("Invalid base64 diff: {}", e)))?;
        let server_sv = base64::engine::general_purpose::STANDARD
            .decode(&response.state_vector)
            .map_err(|e| ClientError::Rpc(format!("Invalid base64 state vector: {}", e)))?;

        Ok((diff, server_sv))
    }

    // ========================================================================
    // Subscription Operations
    // ========================================================================

    /// Subscribe to store changes (tree structure, metadata).
    /// Returns a subscription stream that yields `StoreChangedNotification`.
    pub async fn subscribe_store_changes(
        &self,
        store_id: StoreId,
    ) -> Result<jsonrpsee::core::client::Subscription<StoreChangedNotification>> {
        use jsonrpsee::core::params::ArrayParams;

        let mut params = ArrayParams::new();
        params.insert(store_id).map_err(|e| ClientError::Rpc(e.to_string()))?;

        let sub = self.client
            .subscribe::<StoreChangedNotification, _>(
                "pimble_subscribeStoreChanges",
                params,
                "pimble_unsubscribeStoreChanges",
            )
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(sub)
    }

    /// Subscribe to node content changes.
    /// Returns a subscription stream that yields `NodeContentChangedNotification`.
    pub async fn subscribe_node_changes(
        &self,
        store_id: StoreId,
        node_id: NodeId,
    ) -> Result<jsonrpsee::core::client::Subscription<NodeContentChangedNotification>> {
        use jsonrpsee::core::params::ArrayParams;

        let mut params = ArrayParams::new();
        params.insert(store_id).map_err(|e| ClientError::Rpc(e.to_string()))?;
        params.insert(node_id).map_err(|e| ClientError::Rpc(e.to_string()))?;

        let sub = self.client
            .subscribe::<NodeContentChangedNotification, _>(
                "pimble_subscribeNodeChanges",
                params,
                "pimble_unsubscribeNodeChanges",
            )
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(sub)
    }

    // ========================================================================
    // Search Operations
    // ========================================================================

    /// Search across stores
    pub async fn search(
        &self,
        query: impl Into<String>,
        stores: Vec<StoreId>,
        semantic: bool,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>> {
        let request = SearchRequest {
            query: query.into(),
            stores,
            semantic,
            limit,
        };

        let response = self
            .client
            .search(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.results)
    }

    /// Rebuild a store's search index from scratch: delete the on-disk
    /// index and re-index every node from the store's documents. Returns
    /// the number of nodes indexed.
    pub async fn rebuild_index(&self, store_id: StoreId) -> Result<usize> {
        let request = RebuildIndexRequest { store_id };

        let response = self
            .client
            .rebuild_index(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.indexed)
    }
}
