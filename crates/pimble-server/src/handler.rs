//! RPC method handlers

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jsonrpsee::core::{async_trait, SubscriptionResult};
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::{PendingSubscriptionSink, SubscriptionMessage};
use pimble_core::{Node, MountRef, NodeId, StoreId, Workspace};
use pimble_rpc::{
    to_rpc_error, ApplyEditRequest, ApplyEditResponse, ApplyStoreUpdateRequest, CloseStoreRequest,
    CreateMountRequest, CreateMountResponse, CreateNodeRequest, CreateNodeResponse,
    CreateStoreRequest, CreateStoreResponse, CreateWorkspaceRequest, DeleteNodeRequest,
    EditOperation, EmptyResponse, GetChildrenRequest, GetChildrenResponse, GetMountStateRequest,
    GetMountStateResponse, GetNodeRequest, GetNodeResponse, GetNodesRequest, GetNodesResponse,
    ListStoresResponse, LoadWorkspaceRequest, LoadWorkspaceResponse, MoveNodeRequest,
    NodeContentChangedNotification, OpenStoreRequest, OpenStoreResponse, PimbleApiServer,
    SaveWorkspaceRequest, SearchRequest, SearchResponse, StoreChangeKind, StoreChangedNotification,
    SyncNodeContentRequest, SyncNodeContentResponse, SyncStoreDocumentRequest,
    SyncStoreDocumentResponse, UpdateNodeContentRequest, UpdateNodeMetadataRequest,
};
use pimble_store::StoreManager;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// How long to wait after a content edit before flushing it to disk. A burst
/// of keystrokes coalesces into at most one flush per window, instead of one
/// per edit.
const CONTENT_FLUSH_DEBOUNCE: Duration = Duration::from_millis(750);

/// Coalesces content flushes: `apply_edit` marks its store dirty and ensures
/// exactly one flush task is in flight. Edits that land after that task has
/// already drained the pending set are covered by a fresh task on the next
/// `apply_edit` call, since `scheduled` is reset to `false` only once the
/// flush has actually happened.
#[derive(Default)]
struct FlushDebouncer {
    /// Stores with content dirty since the last flush.
    pending: Mutex<HashSet<StoreId>>,
    /// Whether a flush task is currently sleeping/running.
    scheduled: Mutex<bool>,
}

/// Manages active subscription sinks for pushing notifications.
struct SubscriptionRegistry {
    /// Store change subscribers: store_id -> list of sinks
    store_subs: HashMap<StoreId, Vec<jsonrpsee::core::server::SubscriptionSink>>,
    /// Node content change subscribers: (store_id, node_id) -> list of sinks
    node_subs: HashMap<(StoreId, NodeId), Vec<jsonrpsee::core::server::SubscriptionSink>>,
}

impl SubscriptionRegistry {
    fn new() -> Self {
        Self {
            store_subs: HashMap::new(),
            node_subs: HashMap::new(),
        }
    }

    fn add_store_sub(&mut self, store_id: StoreId, sink: jsonrpsee::core::server::SubscriptionSink) {
        self.store_subs.entry(store_id).or_default().push(sink);
    }

    fn add_node_sub(&mut self, store_id: StoreId, node_id: NodeId, sink: jsonrpsee::core::server::SubscriptionSink) {
        self.node_subs.entry((store_id, node_id)).or_default().push(sink);
    }

    /// Notify all store subscribers about a change, removing closed sinks.
    async fn notify_store_change(&mut self, notification: &StoreChangedNotification) {
        if let Some(sinks) = self.store_subs.get_mut(&notification.store_id) {
            let msg = SubscriptionMessage::from_json(&notification).ok();
            if let Some(msg) = msg {
                let mut closed = Vec::new();
                for (i, sink) in sinks.iter().enumerate() {
                    if sink.is_closed() {
                        closed.push(i);
                    } else if let Err(_) = sink.send(msg.clone()).await {
                        closed.push(i);
                    }
                }
                for i in closed.into_iter().rev() {
                    sinks.swap_remove(i);
                }
            }
        }
    }

    /// Notify all node content subscribers about a change, removing closed sinks.
    async fn notify_node_change(&mut self, notification: &NodeContentChangedNotification) {
        let key = (notification.store_id, notification.node_id);
        if let Some(sinks) = self.node_subs.get_mut(&key) {
            tracing::info!("notify_node_change: {} sinks for {:?}/{:?}", sinks.len(), notification.store_id, notification.node_id);
            let msg = SubscriptionMessage::from_json(&notification).ok();
            if let Some(msg) = msg {
                let mut closed = Vec::new();
                for (i, sink) in sinks.iter().enumerate() {
                    if sink.is_closed() {
                        tracing::info!("  sink {} is closed", i);
                        closed.push(i);
                    } else if let Err(e) = sink.send(msg.clone()).await {
                        tracing::info!("  sink {} send failed: {}", i, e);
                        closed.push(i);
                    } else {
                        tracing::info!("  sink {} sent OK", i);
                    }
                }
                for i in closed.into_iter().rev() {
                    sinks.swap_remove(i);
                }
            } else {
                tracing::warn!("notify_node_change: failed to serialize notification");
            }
        } else {
            tracing::info!("notify_node_change: no sinks for {:?}/{:?}", notification.store_id, notification.node_id);
        }
    }

    /// Remove all subscriptions for a store.
    fn remove_store(&mut self, store_id: StoreId) {
        self.store_subs.remove(&store_id);
        self.node_subs.retain(|(sid, _), _| *sid != store_id);
    }
}

/// RPC handler implementation
pub struct RpcHandler {
    store_manager: Arc<RwLock<StoreManager>>,
    subscriptions: Arc<RwLock<SubscriptionRegistry>>,
    flush_debouncer: Arc<FlushDebouncer>,
}

impl RpcHandler {
    pub fn new(store_manager: Arc<RwLock<StoreManager>>) -> Self {
        Self {
            store_manager,
            subscriptions: Arc::new(RwLock::new(SubscriptionRegistry::new())),
            flush_debouncer: Arc::new(FlushDebouncer::default()),
        }
    }

    /// Mark `store_id` as having dirty content and, if no flush task is
    /// already scheduled, spawn one. The task sleeps for
    /// [`CONTENT_FLUSH_DEBOUNCE`], drains whichever stores are pending at
    /// that point, and flushes each in turn. At most one flush task runs at
    /// a time; edits that arrive after the drain (a narrow race) simply
    /// schedule a fresh task on their own next call.
    fn schedule_content_flush(&self, store_id: StoreId) {
        {
            let mut pending = self.flush_debouncer.pending.lock().unwrap();
            pending.insert(store_id);
        }

        {
            let mut scheduled = self.flush_debouncer.scheduled.lock().unwrap();
            if *scheduled {
                return;
            }
            *scheduled = true;
        }

        let store_manager = Arc::clone(&self.store_manager);
        let debouncer = Arc::clone(&self.flush_debouncer);
        tokio::spawn(async move {
            tokio::time::sleep(CONTENT_FLUSH_DEBOUNCE).await;

            let to_flush: Vec<StoreId> = {
                let mut pending = debouncer.pending.lock().unwrap();
                pending.drain().collect()
            };

            {
                let mut manager = store_manager.write().await;
                for store_id in to_flush {
                    if let Err(e) = manager.flush(store_id).await {
                        warn!("Debounced content flush failed for store {}: {}", store_id, e);
                    }
                }
            }

            let mut scheduled = debouncer.scheduled.lock().unwrap();
            *scheduled = false;
        });
    }

    /// Notify store subscribers about a change.
    async fn notify_store_change(&self, store_id: StoreId, kind: StoreChangeKind, source: Option<&str>) {
        let notification = StoreChangedNotification {
            store_id,
            change_kind: kind,
            source_client_id: source.map(String::from),
            update: None,
        };
        self.subscriptions.write().await.notify_store_change(&notification).await;
    }

    /// Notify node content subscribers AND store subscribers about a content change.
    async fn notify_node_content_change(&self, store_id: StoreId, node_id: NodeId, source: Option<&str>, operation: Option<EditOperation>) {
        let node_notif = NodeContentChangedNotification {
            store_id,
            node_id,
            source_client_id: source.map(String::from),
            operation,
        };
        let store_notif = StoreChangedNotification {
            store_id,
            change_kind: StoreChangeKind::ContentUpdated { node_id },
            source_client_id: source.map(String::from),
            update: None,
        };
        // Acquire lock once for both notification types
        let mut registry = self.subscriptions.write().await;
        registry.notify_node_change(&node_notif).await;
        registry.notify_store_change(&store_notif).await;
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
        drop(manager);

        // Clean up subscriptions for this store
        self.subscriptions.write().await.remove_store(request.store_id);

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

        drop(manager);
        self.notify_store_change(request.store_id, StoreChangeKind::NodeCreated { node_id }, None).await;

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
        manager
            .update_node_metadata(request.store_id, request.node_id, request.metadata)
            .await
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        self.notify_store_change(request.store_id, StoreChangeKind::MetadataUpdated { node_id: request.node_id }, None).await;

        Ok(EmptyResponse {})
    }

    async fn update_node_content(
        &self,
        request: UpdateNodeContentRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!(
            "Updating content for node {} in store {}",
            request.node_id, request.store_id
        );

        use base64::Engine;
        let content = base64::engine::general_purpose::STANDARD
            .decode(&request.content)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;

        let mut manager = self.store_manager.write().await;
        manager
            .update_node_content(request.store_id, request.node_id, content)
            .await
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        self.notify_node_content_change(request.store_id, request.node_id, request.client_id.as_deref(), None).await;

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

        drop(manager);
        self.notify_store_change(request.store_id, StoreChangeKind::NodeDeleted { node_id: request.node_id }, None).await;

        Ok(EmptyResponse {})
    }

    async fn move_node(
        &self,
        request: MoveNodeRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!(
            "Moving node {} to parent {} in store {}",
            request.node_id, request.new_parent_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;
        manager
            .move_node(request.store_id, request.node_id, request.new_parent_id, request.position)
            .await
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        self.notify_store_change(request.store_id, StoreChangeKind::NodeMoved { node_id: request.node_id }, None).await;

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

    async fn create_mount(
        &self,
        request: CreateMountRequest,
    ) -> Result<CreateMountResponse, ErrorObjectOwned> {
        info!(
            "Creating mount in store {} under parent {}, source: {}:{}",
            request.store_id, request.parent_id, request.source_store_id, request.source_node_id
        );

        let mount_ref = MountRef {
            source_store: request.source_store_id,
            source_node: request.source_node_id,
        };

        let mut manager = self.store_manager.write().await;

        // Validate that this mount won't create a cycle
        manager
            .validate_mount_creation(request.store_id, request.parent_id, &mount_ref)
            .await
            .map_err(to_rpc_error)?;

        // Create the mount node
        let mut node = Node::mount(request.source_store_id, request.source_node_id);
        if let Some(title) = request.title {
            node.metadata.title = title;
        }

        let node_id = manager
            .create_node(request.store_id, node, Some(request.parent_id))
            .await
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(CreateMountResponse { node_id })
    }

    async fn get_mount_state(
        &self,
        request: GetMountStateRequest,
    ) -> Result<GetMountStateResponse, ErrorObjectOwned> {
        debug!(
            "Getting mount state for node {} in store {}",
            request.node_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;
        let node = manager
            .get_node(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        let mount_ref = node.mount_ref().ok_or_else(|| {
            to_rpc_error(format!("Node {} is not a mount point", request.node_id))
        })?;

        let state = manager.mount_state(&mount_ref);

        Ok(GetMountStateResponse { state, mount_ref })
    }

    async fn sync_store_document(
        &self,
        request: SyncStoreDocumentRequest,
    ) -> Result<SyncStoreDocumentResponse, ErrorObjectOwned> {
        debug!("Sync store document for store {}", request.store_id);

        use base64::Engine;

        let client_sv = base64::engine::general_purpose::STANDARD
            .decode(&request.state_vector)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;

        let store_manager = self.store_manager.read().await;

        // Stateless reconciliation: no per-client sync state is kept for the
        // store document. The client sends its state vector, we hand back
        // everything we have beyond it plus our own state vector. If the
        // client has local changes the server lacks, it sends those
        // separately via `applyStoreUpdate`.
        let diff = store_manager
            .store_doc_diff_since(request.store_id, &client_sv)
            .map_err(to_rpc_error)?;
        let server_sv = store_manager
            .store_doc_state_vector(request.store_id)
            .map_err(to_rpc_error)?;

        Ok(SyncStoreDocumentResponse {
            diff: base64::engine::general_purpose::STANDARD.encode(&diff),
            state_vector: base64::engine::general_purpose::STANDARD.encode(&server_sv),
        })
    }

    async fn apply_store_update(
        &self,
        request: ApplyStoreUpdateRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!(
            "Applying store update to store {} from client {}",
            request.store_id, request.client_id
        );

        use base64::Engine;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&request.update)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;

        let mut manager = self.store_manager.write().await;
        manager
            .apply_store_doc_update(request.store_id, &bytes)
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);

        // Broadcast to other subscribers, carrying the raw update bytes so
        // they can apply it directly instead of refetching.
        let notification = StoreChangedNotification {
            store_id: request.store_id,
            change_kind: StoreChangeKind::TreeStructure,
            source_client_id: Some(request.client_id.clone()),
            update: Some(request.update.clone()),
        };
        self.subscriptions.write().await.notify_store_change(&notification).await;

        Ok(EmptyResponse {})
    }

    async fn sync_node_content(
        &self,
        request: SyncNodeContentRequest,
    ) -> Result<SyncNodeContentResponse, ErrorObjectOwned> {
        debug!(
            "Sync node content for node {} in store {}",
            request.node_id, request.store_id
        );

        use base64::Engine;

        let client_sv = base64::engine::general_purpose::STANDARD
            .decode(&request.state_vector)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;

        let mut store_manager = self.store_manager.write().await;

        // Stateless reconciliation: no per-client sync state is kept for
        // node content. The client sends its state vector, we hand back
        // everything we have beyond it plus our own state vector.
        let doc = store_manager
            .get_node_document(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        let diff = doc
            .diff_since(&client_sv)
            .map_err(to_rpc_error)?;
        let server_sv = doc.state_vector();

        Ok(SyncNodeContentResponse {
            diff: base64::engine::general_purpose::STANDARD.encode(&diff),
            state_vector: base64::engine::general_purpose::STANDARD.encode(&server_sv),
        })
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

    async fn apply_edit(
        &self,
        request: ApplyEditRequest,
    ) -> Result<ApplyEditResponse, ErrorObjectOwned> {
        use base64::Engine;

        // Apply the edit to the server's persistent yrs document.
        match &request.operation {
            EditOperation::IncrementalChanges { changes } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(changes)
                    .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;
                let mut store_manager = self.store_manager.write().await;
                store_manager
                    .apply_content_update(request.store_id, request.node_id, &bytes)
                    .await
                    .map_err(to_rpc_error)?;
            }
            EditOperation::ReplaceContent { content } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(content)
                    .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;
                let mut store_manager = self.store_manager.write().await;
                store_manager
                    .update_node_content(request.store_id, request.node_id, bytes)
                    .await
                    .map_err(to_rpc_error)?;
            }
        }

        // Debounced persistence: coalesce a burst of edits into at most one
        // flush per CONTENT_FLUSH_DEBOUNCE window, rather than one per edit,
        // while still guaranteeing a crash never loses more than that window
        // of keystrokes.
        self.schedule_content_flush(request.store_id);

        // Broadcast to other clients (never re-encoded or reinterpreted —
        // the same operation bytes are relayed verbatim).
        self.notify_node_content_change(
            request.store_id,
            request.node_id,
            Some(&request.client_id),
            Some(request.operation),
        ).await;

        Ok(ApplyEditResponse {})
    }

    async fn subscribe_store_changes(
        &self,
        pending: PendingSubscriptionSink,
        store_id: StoreId,
    ) -> SubscriptionResult {
        info!("Client subscribing to store changes for {}", store_id);

        let sink = pending.accept().await?;
        self.subscriptions.write().await.add_store_sub(store_id, sink);

        Ok(())
    }

    async fn subscribe_node_changes(
        &self,
        pending: PendingSubscriptionSink,
        store_id: StoreId,
        node_id: NodeId,
    ) -> SubscriptionResult {
        info!("Client subscribing to node changes for {}:{}", store_id, node_id);

        let sink = pending.accept().await?;
        self.subscriptions.write().await.add_node_sub(store_id, node_id, sink);

        Ok(())
    }

    async fn search(
        &self,
        request: SearchRequest,
    ) -> Result<SearchResponse, ErrorObjectOwned> {
        debug!("Searching for '{}'", request.query);

        // TODO: Implement search
        Ok(SearchResponse {
            results: Vec::new(),
            total: 0,
        })
    }
}
