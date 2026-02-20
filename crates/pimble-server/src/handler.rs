//! RPC method handlers

use std::sync::Arc;

use jsonrpsee::core::async_trait;
use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::{Node, Workspace};
use pimble_crdt::DocumentContent;
use pimble_rpc::{
    to_rpc_error, CloseStoreRequest, CreateNodeRequest, CreateNodeResponse, CreateStoreRequest,
    CreateStoreResponse, CreateWorkspaceRequest, DeleteNodeRequest, EmptyResponse,
    GetChildrenRequest, GetChildrenResponse, GetNodeRequest, GetNodeResponse, GetNodesRequest,
    GetNodesResponse, ListStoresResponse, LoadWorkspaceRequest, LoadWorkspaceResponse,
    MoveNodeRequest, OpenStoreRequest, OpenStoreResponse, PimbleApiServer, SaveWorkspaceRequest,
    SearchRequest, SearchResponse, SetNodeTextRequest, UpdateNodeContentRequest, UpdateNodeMetadataRequest,
};
use pimble_store::StoreManager;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// RPC handler implementation
pub struct RpcHandler {
    store_manager: Arc<RwLock<StoreManager>>,
}

impl RpcHandler {
    pub fn new(store_manager: Arc<RwLock<StoreManager>>) -> Self {
        Self { store_manager }
    }
}

#[async_trait]
impl PimbleApiServer for RpcHandler {
    async fn create_store(
        &self,
        request: CreateStoreRequest,
    ) -> Result<CreateStoreResponse, ErrorObjectOwned> {
        info!("Creating store '{}' at {:?}", request.name, request.path);

        let mut manager = self.store_manager.write().await;
        let store_id = manager
            .create_local_store(&request.path, &request.name)
            .await
            .map_err(to_rpc_error)?;

        let root_node_id = manager
            .root_node_id(store_id)
            .map_err(to_rpc_error)?;

        Ok(CreateStoreResponse {
            store_id,
            root_node_id,
        })
    }

    async fn open_store(
        &self,
        request: OpenStoreRequest,
    ) -> Result<OpenStoreResponse, ErrorObjectOwned> {
        info!("Opening store at {:?}", request.path);

        let mut manager = self.store_manager.write().await;
        let store_id = manager
            .open_local_store(&request.path)
            .await
            .map_err(to_rpc_error)?;

        let store = manager
            .get_store_info(store_id)
            .map_err(to_rpc_error)?;

        Ok(OpenStoreResponse { store })
    }

    async fn close_store(
        &self,
        request: CloseStoreRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!("Closing store {}", request.store_id);

        let mut manager = self.store_manager.write().await;
        manager
            .close_store(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn list_stores(&self) -> Result<ListStoresResponse, ErrorObjectOwned> {
        debug!("Listing stores");

        let manager = self.store_manager.read().await;
        let store_ids = manager.list_stores();

        let mut stores = Vec::new();
        for id in store_ids {
            if let Ok(store) = manager.get_store_info(id) {
                stores.push(store);
            }
        }

        Ok(ListStoresResponse { stores })
    }

    async fn get_node(
        &self,
        request: GetNodeRequest,
    ) -> Result<GetNodeResponse, ErrorObjectOwned> {
        debug!("Getting node {} from store {}", request.node_id, request.store_id);

        let mut manager = self.store_manager.write().await;
        let node = manager
            .get_node(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(GetNodeResponse { node })
    }

    async fn get_nodes(
        &self,
        request: GetNodesRequest,
    ) -> Result<GetNodesResponse, ErrorObjectOwned> {
        debug!(
            "Getting {} nodes from store {}",
            request.node_ids.len(),
            request.store_id
        );

        let mut manager = self.store_manager.write().await;
        let mut nodes = Vec::new();

        for node_id in request.node_ids {
            match manager.get_node(request.store_id, node_id).await {
                Ok(node) => nodes.push(node),
                Err(e) => {
                    debug!("Failed to get node {}: {}", node_id, e);
                }
            }
        }

        Ok(GetNodesResponse { nodes })
    }

    async fn create_node(
        &self,
        request: CreateNodeRequest,
    ) -> Result<CreateNodeResponse, ErrorObjectOwned> {
        info!(
            "Creating {} node '{}' in store {}",
            request.node_type, request.title, request.store_id
        );

        let mut node = Node::new(&request.node_type);
        node.metadata.title = request.title;

        let mut manager = self.store_manager.write().await;
        let node_id = manager
            .create_node(request.store_id, node, request.parent_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(CreateNodeResponse { node_id })
    }

    async fn update_node_metadata(
        &self,
        request: UpdateNodeMetadataRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        debug!(
            "Updating metadata for node {} in store {}",
            request.node_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;
        let mut node = manager
            .get_node(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        node.metadata = request.metadata;
        node.touch();

        // Re-save the node (the manager will mark it dirty)
        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn update_node_content(
        &self,
        _request: UpdateNodeContentRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        // TODO: Apply CRDT changes
        Ok(EmptyResponse {})
    }

    async fn set_node_text(
        &self,
        request: SetNodeTextRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!(
            "Setting text content for node {} in store {}",
            request.node_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;

        // Create new document content with the text
        let mut doc_content = DocumentContent::new();
        doc_content.set_text(&request.text).map_err(to_rpc_error)?;

        // Save the document to the node
        manager
            .save_node_document(request.store_id, request.node_id, doc_content.document_mut())
            .await
            .map_err(to_rpc_error)?;

        // Flush changes to disk
        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn delete_node(
        &self,
        request: DeleteNodeRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!(
            "Deleting node {} from store {}",
            request.node_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;
        manager
            .delete_node(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn move_node(
        &self,
        _request: MoveNodeRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        // TODO: Implement node moving
        Ok(EmptyResponse {})
    }

    async fn get_children(
        &self,
        request: GetChildrenRequest,
    ) -> Result<GetChildrenResponse, ErrorObjectOwned> {
        debug!(
            "Getting children of node {} in store {}",
            request.node_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;
        let children = manager
            .get_children(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(GetChildrenResponse { children })
    }

    async fn load_workspace(
        &self,
        request: LoadWorkspaceRequest,
    ) -> Result<LoadWorkspaceResponse, ErrorObjectOwned> {
        info!("Loading workspace from {:?}", request.path);

        let content = tokio::fs::read_to_string(&request.path)
            .await
            .map_err(to_rpc_error)?;

        let workspace: Workspace = serde_json::from_str(&content)
            .map_err(to_rpc_error)?;

        Ok(LoadWorkspaceResponse { workspace })
    }

    async fn save_workspace(
        &self,
        request: SaveWorkspaceRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!("Saving workspace to {:?}", request.path);

        let content = serde_json::to_string_pretty(&request.workspace)
            .map_err(to_rpc_error)?;

        tokio::fs::write(&request.path, content)
            .await
            .map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn create_workspace(
        &self,
        request: CreateWorkspaceRequest,
    ) -> Result<LoadWorkspaceResponse, ErrorObjectOwned> {
        info!("Creating workspace '{}' at {:?}", request.name, request.path);

        let workspace = Workspace::new(&request.name);

        let content = serde_json::to_string_pretty(&workspace)
            .map_err(to_rpc_error)?;

        tokio::fs::write(&request.path, content)
            .await
            .map_err(to_rpc_error)?;

        Ok(LoadWorkspaceResponse { workspace })
    }

    async fn search(
        &self,
        request: SearchRequest,
    ) -> Result<SearchResponse, ErrorObjectOwned> {
        debug!("Searching for '{}'", request.query);

        // TODO: Implement search in Phase 4
        Ok(SearchResponse {
            results: Vec::new(),
            total: 0,
        })
    }
}
