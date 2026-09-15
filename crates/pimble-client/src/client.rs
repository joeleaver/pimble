//! RPC client implementation

use std::path::{Path, PathBuf};

use jsonrpsee::core::client::SubscriptionClientT;
use jsonrpsee::ws_client::{HeaderMap, HeaderValue, WsClient, WsClientBuilder};
use pimble_core::{AuthMethod, Node, NodeId, RemoteEndpoint, Store, StoreId, SyncState, Workspace};
use pimble_core::MountRef;
use pimble_rpc::{
    AddRemoteStoreRequest, ApplyEditRequest, ApplyStoreUpdateRequest, CloseStoreRequest, CreateMountRequest,
    CreateNodeRequest, CreateStoreRequest, CreateWorkspaceRequest, DeleteNodeRequest,
    EditOperation, GetChildrenRequest, GetMountStateRequest, GetNodeRequest, GetNodesRequest, GetStoreSyncRequest, SetStoreSyncRequest,
    ListRemoteStoresRequest, LoadWorkspaceRequest, MoveNodeRequest, NodeContentChangedNotification, NodeStateVector,
    OpenStoreRequest, PimbleApiClient, RebuildIndexRequest, RemoveReplicaRequest, SaveWorkspaceRequest,
    SearchRequest, SearchResultItem, StoreChangedNotification, SyncNodeContentsRequest, SyncStoreDocumentRequest,
    UpdateNodeContentRequest, UpdateNodeMetadataRequest, MAX_SYNC_NODE_CONTENTS,
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
    /// Connect to a Pimble server via WebSocket, sending no authentication.
    /// Equivalent to `connect_with_auth(url, &AuthMethod::None)`.
    ///
    /// Accepts HTTP URLs (http://, https://) and automatically converts them
    /// to WebSocket URLs (ws://, wss://).
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        Self::connect_with_auth(url, &AuthMethod::None).await
    }

    /// Connect to a Pimble server via WebSocket, authenticating the
    /// handshake per `auth` (docs/SYNC_CONTRACT.md decision 10):
    /// `Bearer { token }` sends `Authorization: Bearer <token>`, `ApiKey
    /// { key }` sends `X-Api-Key: <key>`, `None` sends nothing. `OAuth2` is
    /// not supported here and is an error. The server checks nothing yet;
    /// this only plumbs the header through.
    ///
    /// Accepts HTTP URLs (http://, https://) and automatically converts them
    /// to WebSocket URLs (ws://, wss://).
    pub async fn connect_with_auth(url: impl AsRef<str>, auth: &AuthMethod) -> Result<Self> {
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

        let mut builder = WsClientBuilder::default();
        match auth {
            AuthMethod::None => {}
            AuthMethod::Bearer { token } => {
                let mut headers = HeaderMap::new();
                let value = HeaderValue::from_str(&format!("Bearer {}", token))
                    .map_err(|e| ClientError::Connection(format!("Invalid bearer token: {}", e)))?;
                headers.insert("authorization", value);
                builder = builder.set_headers(headers);
            }
            AuthMethod::ApiKey { key } => {
                let mut headers = HeaderMap::new();
                let value = HeaderValue::from_str(key)
                    .map_err(|e| ClientError::Connection(format!("Invalid API key: {}", e)))?;
                headers.insert("x-api-key", value);
                builder = builder.set_headers(headers);
            }
            AuthMethod::OAuth2 { .. } => {
                return Err(ClientError::Connection(
                    "OAuth2 auth is not supported by connect_with_auth".into(),
                ));
            }
        }

        let client = builder
            .build(&ws_url)
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        debug!("Connected to Pimble server at {} (ws: {})", base_url, ws_url);

        Ok(Self { client, base_url })
    }

    /// Whether the WebSocket connection is still up. False once the server
    /// has gone away (the background task has closed), even before any call
    /// has failed.
    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    /// Resolves once the connection is lost (or immediately if it already
    /// is). Lets a caller notice a dead server without waiting for a call to
    /// fail.
    pub async fn on_disconnect(&self) {
        self.client.on_disconnect().await
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
    // Replica Sync Operations (docs/SYNC_CONTRACT.md)
    // ========================================================================

    /// Create a local replica of `remote_store_id` as held by `remote`,
    /// linked to it. Returns the opened store. `path: None` lets the server
    /// choose the replica's location (`<data dir>/pimble/replicas/<store
    /// id>.pimble`); `Some` places it there instead (refused if it exists).
    pub async fn add_remote_store(
        &self,
        remote: RemoteEndpoint,
        remote_store_id: StoreId,
        path: Option<PathBuf>,
    ) -> Result<Store> {
        let request = AddRemoteStoreRequest {
            remote,
            remote_store_id,
            path,
        };
        let response = self
            .client
            .add_remote_store(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;
        Ok(response.store)
    }

    /// Link a local store to its twin on `remote` (`Some`) or unlink it
    /// (`None`). Returns the link and its state.
    pub async fn set_store_sync(
        &self,
        store_id: StoreId,
        remote: Option<RemoteEndpoint>,
    ) -> Result<(Option<RemoteEndpoint>, SyncState)> {
        let request = SetStoreSyncRequest { store_id, remote };
        let response = self
            .client
            .set_store_sync(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;
        Ok((response.remote, response.state))
    }

    /// A store's sync link (`None` when unlinked) and its current state.
    pub async fn get_store_sync(&self, store_id: StoreId) -> Result<(Option<RemoteEndpoint>, SyncState)> {
        let request = GetStoreSyncRequest { store_id };
        let response = self
            .client
            .get_store_sync(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;
        Ok((response.remote, response.state))
    }

    /// The stores `remote` has open, fetched by the server this client is
    /// connected to (with `remote.auth`, or its saved credential for that
    /// remote when `remote.auth` is `None`).
    pub async fn list_remote_stores(&self, remote: RemoteEndpoint) -> Result<Vec<Store>> {
        let request = ListRemoteStoresRequest { remote };
        let response = self
            .client
            .list_remote_stores(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;
        Ok(response.stores)
    }

    /// Stop a replica's sync link, close it, and delete its directory.
    /// `force` removes a replica whose link is not `Synced` (its unsynced
    /// changes are lost).
    pub async fn remove_replica(&self, store_id: StoreId, force: bool) -> Result<()> {
        let request = RemoveReplicaRequest { store_id, force };
        self.client
            .remove_replica(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;
        Ok(())
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

    /// Sync one node's content document: send our yrs state vector, get back
    /// everything the server has beyond it plus the server's own state
    /// vector. Stateless on both ends. A one-node `syncNodeContents`; an
    /// error if the server does not have the node.
    pub async fn sync_node_content(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        state_vector: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut nodes = self.sync_node_contents(store_id, &[(node_id, state_vector.to_vec())]).await?;
        match nodes.pop() {
            Some((id, diff, server_sv)) if id == node_id => Ok((diff, server_sv)),
            _ => Err(ClientError::Rpc(format!("Node not found: {}", node_id))),
        }
    }

    /// Sync the content documents of many nodes: for each `(node, state
    /// vector)`, get back `(node, diff, server state vector)`, in request
    /// order, for every node the server has (others are left out). Splits
    /// the list into requests of at most `MAX_SYNC_NODE_CONTENTS` nodes.
    pub async fn sync_node_contents(
        &self,
        store_id: StoreId,
        nodes: &[(NodeId, Vec<u8>)],
    ) -> Result<Vec<(NodeId, Vec<u8>, Vec<u8>)>> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;

        let mut out = Vec::with_capacity(nodes.len());
        for chunk in nodes.chunks(MAX_SYNC_NODE_CONTENTS) {
            let request = SyncNodeContentsRequest {
                store_id,
                nodes: chunk
                    .iter()
                    .map(|(node_id, sv)| NodeStateVector { node_id: *node_id, state_vector: b64.encode(sv) })
                    .collect(),
            };

            let response = self
                .client
                .sync_node_contents(request)
                .await
                .map_err(|e| ClientError::Rpc(e.to_string()))?;

            for entry in response.nodes {
                let diff = b64
                    .decode(&entry.diff)
                    .map_err(|e| ClientError::Rpc(format!("Invalid base64 diff: {}", e)))?;
                let server_sv = b64
                    .decode(&entry.state_vector)
                    .map_err(|e| ClientError::Rpc(format!("Invalid base64 state vector: {}", e)))?;
                out.push((entry.node_id, diff, server_sv));
            }
        }

        Ok(out)
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
