//! RPC client implementation

use std::path::Path;

use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use pimble_core::{Node, NodeId, Store, StoreId, Workspace};
use pimble_rpc::{
    CloseStoreRequest, CreateNodeRequest, CreateStoreRequest, CreateWorkspaceRequest,
    DeleteNodeRequest, GetChildrenRequest, GetNodeRequest, GetNodesRequest,
    LoadWorkspaceRequest, MoveNodeRequest, OpenStoreRequest, PimbleApiClient, SaveWorkspaceRequest,
    SearchRequest, SearchResultItem, SetNodeTextRequest, UpdateNodeContentRequest, UpdateNodeMetadataRequest,
};
use tracing::debug;
use url::Url;

use crate::error::{ClientError, Result};

/// Client for connecting to a Pimble server
pub struct PimbleClient {
    client: HttpClient,
    base_url: Url,
}

impl PimbleClient {
    /// Connect to a Pimble server
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        let base_url: Url = url
            .as_ref()
            .parse()
            .map_err(|e| ClientError::Connection(format!("Invalid URL: {}", e)))?;

        let client = HttpClientBuilder::default()
            .build(&base_url)
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        debug!("Connected to Pimble server at {}", base_url);

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

    /// Update a node's content
    pub async fn update_node_content(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        changes: Vec<String>,
    ) -> Result<()> {
        let request = UpdateNodeContentRequest {
            store_id,
            node_id,
            changes,
        };

        self.client
            .update_node_content(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(())
    }

    /// Set a node's text content (replaces all content)
    pub async fn set_node_text(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        text: String,
    ) -> Result<()> {
        let request = SetNodeTextRequest {
            store_id,
            node_id,
            text,
        };

        self.client
            .set_node_text(request)
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

    /// Get children of a node
    pub async fn get_children(&self, store_id: StoreId, node_id: NodeId) -> Result<Vec<Node>> {
        let request = GetChildrenRequest { store_id, node_id };

        let response = self
            .client
            .get_children(request)
            .await
            .map_err(|e| ClientError::Rpc(e.to_string()))?;

        Ok(response.children)
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
}
