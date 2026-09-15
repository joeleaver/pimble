//! RPC method handlers

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jsonrpsee::core::{async_trait, SubscriptionResult};
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::{PendingSubscriptionSink, SubscriptionMessage};
use pimble_client::PimbleClient;
use pimble_core::{Node, MountRef, NodeId, RemoteEndpoint, StoreId, StoreLocation, SyncState, Workspace};
use pimble_plugins::PluginHost;
use pimble_rpc::{
    index_building_error, to_rpc_error, ApplyEditRequest, ApplyEditResponse,
    AddRemoteStoreRequest, ApplyStoreUpdateRequest, CloseStoreRequest, CreateMountRequest, CreateMountResponse,
    CreateNodeRequest, CreateNodeResponse, CreateStoreRequest, CreateStoreResponse,
    CreateWorkspaceRequest, DeleteNodeRequest, EditOperation, EmptyResponse, GetChildrenRequest,
    GetChildrenResponse, GetMountStateRequest, GetMountStateResponse, GetNodeRequest, GetStoreSyncRequest, GetStoreSyncResponse, SetStoreSyncRequest,
    GetNodeResponse, GetNodesRequest, GetNodesResponse, ListRemoteStoresRequest, ListStoresResponse, LoadWorkspaceRequest,
    LoadWorkspaceResponse, MoveNodeRequest, NodeContentChangedNotification, NodeContentDiff, OpenStoreRequest,
    OpenStoreResponse, PimbleApiServer, RebuildIndexRequest, RebuildIndexResponse, RemoveReplicaRequest,
    SaveWorkspaceRequest, SearchRequest, SearchResponse, SearchResultItem, StoreChangeKind,
    StoreChangedNotification, SyncNodeContentsRequest, SyncNodeContentsResponse,
    SyncStoreDocumentRequest, SyncStoreDocumentResponse, UpdateNodeContentRequest,
    UpdateNodeMetadataRequest, MAX_SYNC_NODE_CONTENTS,
};
use pimble_search::{IndexNode, SearchError, SearchIndex, SearchQuery};
use pimble_store::{StoreEndpoint, StoreManager, SyncConfig};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::sync_link::{SyncLink, SyncLinkHandle};

/// How long to wait, after a node's content last changed, before reading its
/// units and upserting them into the search index. `applyEdit` fires on every
/// keystroke; this coalesces a burst of edits into one re-index per node,
/// independent of the (unrelated) content-flush debounce above.
const CONTENT_INDEX_DEBOUNCE: Duration = Duration::from_millis(2_000);

/// How long to wait after a content edit before flushing it to disk. A burst
/// of keystrokes coalesces into at most one flush per window, instead of one
/// per edit.
const CONTENT_FLUSH_DEBOUNCE: Duration = Duration::from_millis(750);

/// Where `addRemoteStore` places a replica when the caller passes `path:
/// None` (docs/SYNC_CONTRACT.md decision 8): `<data dir>/pimble/replicas/
/// <store id>.pimble`. The user never chooses this location; a caller like
/// the CLI may still pass an explicit `path`. `LocalStore::create_replica`
/// creates every ancestor directory, so nothing here needs to pre-create
/// `pimble/replicas/`.
fn default_replica_path(store_id: StoreId) -> PathBuf {
    replicas_dir().join(format!("{}.pimble", store_id))
}

/// The directory this server creates replicas in: `<data dir>/pimble/
/// replicas/`. A store inside it is a replica (`Store::is_replica`), and
/// only such a store can be removed with `removeReplica`.
fn replicas_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pimble")
        .join("replicas")
}

/// Fill in the fields of a `Store` only the server knows: whether it is a
/// replica (its directory is inside [`replicas_dir`]).
fn mark_replica(store: &mut pimble_core::Store) {
    store.is_replica = store.local_path().map_or(false, |p| p.starts_with(replicas_dir()));
}

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

// ── Search index feed ──────────────────────────────────────────────────

/// In-process notification fed to a store's [`StoreIndexer`] task, enqueued
/// next to the existing `notify_store_change`/`notify_node_content_change`
/// calls (never over the WebSocket). `Upsert` and `Remove` are applied
/// immediately; `ContentChanged` is debounced per node.
enum IndexEvent {
    /// A node's metadata, tree position, or existence changed (created,
    /// title/tags edited, or moved — moving re-upserts the node with its new
    /// `parent`). Applied immediately: cheap and infrequent relative to
    /// keystrokes.
    Upsert(NodeId),
    /// A node's content changed (`applyEdit`/`updateNodeContent`). Debounced
    /// [`CONTENT_INDEX_DEBOUNCE`] per node so a burst of keystrokes re-indexes
    /// once, not per edit.
    ContentChanged(NodeId),
    /// A node was deleted.
    Remove(NodeId),
}

/// A store's open search index plus the channel that feeds its indexing
/// task.
struct IndexHandle {
    index: Arc<SearchIndex>,
    events: mpsc::UnboundedSender<IndexEvent>,
}

/// Owns one store's [`SearchIndex`] and reduces the store's mutations
/// (`IndexEvent`s) into `upsert`/`remove` calls on it. One instance is
/// spawned as a tokio task per open store.
struct StoreIndexer {
    store_id: StoreId,
    index: Arc<SearchIndex>,
    store_manager: Arc<RwLock<StoreManager>>,
    plugin_host: Arc<PluginHost>,
    /// Per-node debounce generation: `schedule_content_upsert` increments a
    /// node's counter and captures it; the sleeping task that follows only
    /// does the work if its captured value is still current when it wakes,
    /// so a newer edit silently supersedes an older, still-sleeping one.
    content_gen: Mutex<HashMap<NodeId, u64>>,
}

impl StoreIndexer {
    /// Drive `rx` until the sender side (the store's `IndexHandle`) is
    /// dropped, e.g. on `closeStore`.
    async fn run(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<IndexEvent>) {
        while let Some(event) = rx.recv().await {
            match event {
                IndexEvent::Upsert(node_id) => {
                    if let Err(e) = self.upsert_now(node_id).await {
                        warn!("Indexing node {} in store {} failed: {}", node_id, self.store_id, e);
                    }
                }
                IndexEvent::Remove(node_id) => {
                    if let Err(e) = self.index.remove(node_id) {
                        warn!("Removing node {} from index for store {} failed: {}", node_id, self.store_id, e);
                    }
                }
                IndexEvent::ContentChanged(node_id) => {
                    Arc::clone(&self).schedule_content_upsert(node_id);
                }
            }
        }
    }

    /// Bump `node_id`'s debounce generation and spawn a task that, after
    /// [`CONTENT_INDEX_DEBOUNCE`], re-indexes the node if no newer edit has
    /// arrived in the meantime.
    fn schedule_content_upsert(self: Arc<Self>, node_id: NodeId) {
        let generation = {
            let mut gens = self.content_gen.lock().unwrap();
            let g = gens.entry(node_id).or_insert(0);
            *g += 1;
            *g
        };
        tokio::spawn(async move {
            tokio::time::sleep(CONTENT_INDEX_DEBOUNCE).await;
            let still_current = {
                let gens = self.content_gen.lock().unwrap();
                gens.get(&node_id).copied() == Some(generation)
            };
            if still_current {
                if let Err(e) = self.upsert_now(node_id).await {
                    warn!("Indexing node {} in store {} failed: {}", node_id, self.store_id, e);
                }
            }
        });
    }

    /// Fetch `node_id` fresh from the store and upsert it into the index.
    /// A node that no longer exists (deleted, or the store closed, before
    /// this ran) is silently skipped rather than treated as an error.
    async fn upsert_now(&self, node_id: NodeId) -> pimble_search::Result<()> {
        let node = {
            let mut manager = self.store_manager.write().await;
            match manager.get_node(self.store_id, node_id).await {
                Ok(node) => node,
                Err(_) => return Ok(()),
            }
        };
        let index_node = build_index_node(&node, &self.plugin_host);
        self.index.upsert(&index_node)
    }
}

/// The title a search result shows for `node`, given its already-projected
/// `content_text` (the same joined-units text as `IndexNode::text`): an
/// explicit title wins; otherwise the first non-empty line of the content
/// (truncated to 25 chars with "…"); otherwise the raw title; otherwise
/// `"Untitled"`. This is also what gets written to `IndexNode.title`, so
/// title search matches the same fallback.
///
/// Exactly mirrors `pimble_app::state::label_from_title_and_content` (the
/// tree's display label), so a search result's title agrees with what the
/// tree shows for a node with no explicit title. Duplicated here rather than
/// shared through a `pimble-core` helper: this step's scope excludes editing
/// `pimble-core` or `pimble-app`, so the ~20 lines are copied verbatim
/// instead of factored out.
fn index_title(node: &Node, content_text: &str) -> String {
    let has_explicit_title = node
        .metadata
        .custom
        .get("explicit_title")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let title = &node.metadata.title;

    if has_explicit_title && !title.is_empty() {
        return title.clone();
    }

    let first_line = content_text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if !first_line.is_empty() {
        let char_count = first_line.chars().count();
        return if char_count > 25 {
            let truncated: String = first_line.chars().take(25).collect();
            format!("{truncated}…")
        } else {
            first_line.to_string()
        };
    }

    if !title.is_empty() {
        return title.clone();
    }

    "Untitled".to_string()
}

/// Project a [`Node`] into the search index's [`IndexNode`]: metadata plus
/// its content's [`pimble_core::IndexUnit`]s, from the node type's plugin
/// (`ContentDoc::units()` for `document` nodes, via `DocumentPlugin`). A node
/// type with no registered plugin (e.g. `mount`) indexes with no units/text.
fn build_index_node(node: &Node, plugin_host: &PluginHost) -> IndexNode {
    let units = plugin_host
        .get(&node.node_type)
        .and_then(|plugin| plugin.index_units(&node.content).ok())
        .unwrap_or_default();
    let text = units
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let title = index_title(node, &text);
    let links = node
        .links
        .iter()
        .filter_map(|link| link.target.node_id())
        .collect();

    IndexNode {
        node_id: node.id,
        kind: node.node_type.clone(),
        title,
        text,
        modified_at: node.metadata.modified_at.timestamp_millis(),
        parent: node.parent_id,
        tags: node.metadata.tags.clone(),
        links,
        units,
    }
}

/// One local notification, broadcast in-process (docs/SYNC_CONTRACT.md
/// decision 3) wherever [`SubscriptionRegistry`] notifies its WebSocket
/// sinks. A store's [`crate::sync_link::SyncLink`] subscribes to this to
/// learn about local changes to forward to its remote, without a loopback
/// socket.
#[derive(Debug, Clone)]
pub enum LocalChange {
    Store(StoreChangedNotification),
    Node(NodeContentChangedNotification),
}

/// Manages active subscription sinks for pushing notifications.
struct SubscriptionRegistry {
    /// Store change subscribers: store_id -> list of sinks
    store_subs: HashMap<StoreId, Vec<jsonrpsee::core::server::SubscriptionSink>>,
    /// Node content change subscribers: (store_id, node_id) -> list of sinks
    node_subs: HashMap<(StoreId, NodeId), Vec<jsonrpsee::core::server::SubscriptionSink>>,
    /// In-process broadcast of every local notification (decision 3).
    local_changes: broadcast::Sender<LocalChange>,
}

impl SubscriptionRegistry {
    fn new() -> Self {
        let (local_changes, _) = broadcast::channel(1024);
        Self {
            store_subs: HashMap::new(),
            node_subs: HashMap::new(),
            local_changes,
        }
    }

    /// Subscribe to the in-process local-change broadcast.
    fn subscribe_local(&self) -> broadcast::Receiver<LocalChange> {
        self.local_changes.subscribe()
    }

    fn add_store_sub(&mut self, store_id: StoreId, sink: jsonrpsee::core::server::SubscriptionSink) {
        self.store_subs.entry(store_id).or_default().push(sink);
    }

    fn add_node_sub(&mut self, store_id: StoreId, node_id: NodeId, sink: jsonrpsee::core::server::SubscriptionSink) {
        self.node_subs.entry((store_id, node_id)).or_default().push(sink);
    }

    /// Notify all store subscribers about a change, removing closed sinks.
    async fn notify_store_change(&mut self, notification: &StoreChangedNotification) {
        // Local, in-process broadcast (decision 3); harmless if no one (no
        // sync link) is currently subscribed.
        let _ = self.local_changes.send(LocalChange::Store(notification.clone()));

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
        let _ = self.local_changes.send(LocalChange::Node(notification.clone()));

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

/// RPC handler implementation.
///
/// A bag of `Arc`s, `Clone` for that reason: a store's `SyncLink` owns a
/// clone so it can call `apply_edit`/`apply_store_update` on the same live
/// state as every other client (docs/SYNC_CONTRACT.md decision 1).
#[derive(Clone)]
pub struct RpcHandler {
    store_manager: Arc<RwLock<StoreManager>>,
    subscriptions: Arc<RwLock<SubscriptionRegistry>>,
    flush_debouncer: Arc<FlushDebouncer>,
    /// Built-in node-type plugins (document, folder), shared across every
    /// store, used to project a node's content into `IndexUnit`s for search.
    plugin_host: Arc<PluginHost>,
    /// One open `SearchIndex` per open store, under `<store dir>/index/rhypedb/`.
    /// Opened when a store opens, closed (removed) when it closes.
    indexes: Arc<RwLock<HashMap<StoreId, IndexHandle>>>,
    /// Whether every `SearchIndex::open` call below should ask for semantic
    /// search. `true` unless `PimbleServer::start`'s model warm-up (see
    /// `warm_embedding_model` there) failed — a failure means the model
    /// isn't cached and probably can't be downloaded, so passing `true`
    /// anyway would just start a background worker that lazily retries (and
    /// re-fails) the same download per store. `false` here makes every store
    /// open keyword-only instead; it never disables an already-open index.
    semantic_available: bool,
    /// One running [`SyncLinkHandle`] per replica-linked store
    /// (docs/SYNC_CONTRACT.md). Populated by `openStore` (from `sync.json`)
    /// and `setStoreSync`/`addRemoteStore`; removed (after `stop()`) by
    /// `closeStore` and `setStoreSync(None)`.
    links: Arc<RwLock<HashMap<StoreId, SyncLinkHandle>>>,
}

impl RpcHandler {
    pub fn new(store_manager: Arc<RwLock<StoreManager>>) -> Self {
        Self::with_semantic_available(store_manager, true)
    }

    /// Like [`RpcHandler::new`], but with `semantic_available` set
    /// explicitly (see that field's doc comment) instead of defaulting to
    /// `true`. Used by `PimbleServer::start` after its own warm-up attempt;
    /// `new` (unconditionally `true`, same as every `SearchIndex::open` call
    /// used to pass before this field existed) covers every other caller,
    /// including the test suite.
    pub fn with_semantic_available(store_manager: Arc<RwLock<StoreManager>>, semantic_available: bool) -> Self {
        Self {
            store_manager,
            subscriptions: Arc::new(RwLock::new(SubscriptionRegistry::new())),
            flush_debouncer: Arc::new(FlushDebouncer::default()),
            plugin_host: Arc::new(pimble_plugins::create_default_host()),
            indexes: Arc::new(RwLock::new(HashMap::new())),
            semantic_available,
            links: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // ── Replica sync (docs/SYNC_CONTRACT.md) ─────────────────────────

    /// Shared handle to the store manager, for [`crate::sync_link`] to read
    /// and write CRDT documents directly (state vectors, diffs) alongside
    /// the handler's own `apply_store_update`/`apply_edit` (used to actually
    /// merge, persist, broadcast, and index a remote change).
    pub(crate) fn store_manager_handle(&self) -> Arc<RwLock<StoreManager>> {
        Arc::clone(&self.store_manager)
    }

    /// Subscribe to this server's in-process broadcast of local
    /// notifications (decision 3), used by a sync link to learn about local
    /// changes to forward to its remote.
    pub(crate) async fn subscribe_local_changes(&self) -> broadcast::Receiver<LocalChange> {
        self.subscriptions.read().await.subscribe_local()
    }

    /// Notify a store's local subscribers that its sync link's state
    /// changed.
    pub(crate) async fn notify_sync_state_changed(&self, store_id: StoreId, state: SyncState) {
        self.notify_store_change(store_id, StoreChangeKind::SyncStateChanged { state }, None).await;
    }

    /// A store's sync link state, `Offline` if it has none.
    async fn sync_state_of(&self, store_id: StoreId) -> SyncState {
        match self.links.read().await.get(&store_id) {
            Some(handle) => handle.state(),
            None => SyncState::Offline,
        }
    }

    /// Start a sync link for `store_id` if one isn't already running.
    /// `openStore`, `addRemoteStore`, `set_store_sync(Some(remote))` and no-op if
    /// a link is already present.
    async fn ensure_link_started(&self, store_id: StoreId, remote: RemoteEndpoint) {
        let mut links = self.links.write().await;
        if links.contains_key(&store_id) {
            return;
        }
        let handle = SyncLink::start(self.clone(), store_id, remote);
        links.insert(store_id, handle);
    }

    /// Stop and remove `store_id`'s sync link, if any.
    async fn stop_link(&self, store_id: StoreId) {
        if let Some(handle) = self.links.write().await.remove(&store_id) {
            handle.stop();
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
    ///
    /// Decision 4: when `operation` is an `applyEdit` delta, its bytes are
    /// also put on the store-level notification's `update` field, so a
    /// subscriber to the store alone (e.g. a sync link) gets content deltas
    /// without subscribing per node. `updateNodeContent` (the full-snapshot
    /// path) passes `None` here and still yields `update: None`.
    async fn notify_node_content_change(&self, store_id: StoreId, node_id: NodeId, source: Option<&str>, operation: Option<EditOperation>) {
        let node_notif = NodeContentChangedNotification {
            store_id,
            node_id,
            source_client_id: source.map(String::from),
            operation: operation.clone(),
        };
        let update_bytes = match &operation {
            Some(EditOperation::IncrementalChanges { changes }) => Some(changes.clone()),
            None => None,
        };
        let store_notif = StoreChangedNotification {
            store_id,
            change_kind: StoreChangeKind::ContentUpdated { node_id },
            source_client_id: source.map(String::from),
            update: update_bytes,
        };
        // Acquire lock once for both notification types
        let mut registry = self.subscriptions.write().await;
        registry.notify_node_change(&node_notif).await;
        registry.notify_store_change(&store_notif).await;
    }

    // ── Search index feed ────────────────────────────────────────────

    /// Send an [`IndexEvent`] to `store_id`'s indexing task, if one is open.
    /// A store with indexing not (yet) available (index open/rebuild failed)
    /// silently has no handle and this is a no-op — search over that store
    /// just returns nothing until the next successful open.
    async fn enqueue_index_event(&self, store_id: StoreId, event: IndexEvent) {
        let indexes = self.indexes.read().await;
        if let Some(handle) = indexes.get(&store_id) {
            // An unbounded channel only fails to send if the receiving task
            // has ended (e.g. a race with `closeStore`); harmless to drop.
            let _ = handle.events.send(event);
        }
    }

    /// The directory a store's search index lives in:
    /// `<store dir>/index/rhypedb/`. Only local stores have one.
    async fn index_dir_for(&self, store_id: StoreId) -> anyhow::Result<PathBuf> {
        let store = self.store_manager.read().await.get_store_info(store_id)?;
        match store.location {
            StoreLocation::Local { path } => Ok(path.join("index").join("rhypedb")),
            StoreLocation::Remote { .. } | StoreLocation::Mounted { .. } => {
                anyhow::bail!("store {} has no local directory; it has no local search index", store_id)
            }
        }
    }

    /// Wrap an opened [`SearchIndex`] in a fresh [`StoreIndexer`] task and
    /// install it as `store_id`'s current [`IndexHandle`], replacing (and
    /// thereby stopping) any previous one for that store.
    async fn install_index_handle(&self, store_id: StoreId, index: Arc<SearchIndex>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let indexer = Arc::new(StoreIndexer {
            store_id,
            index: Arc::clone(&index),
            store_manager: Arc::clone(&self.store_manager),
            plugin_host: Arc::clone(&self.plugin_host),
            content_gen: Mutex::new(HashMap::new()),
        });
        tokio::spawn(indexer.run(rx));
        self.indexes.write().await.insert(store_id, IndexHandle { index, events: tx });
    }

    /// Walk every node in `store_id` from its root (mount nodes are indexed
    /// themselves but not descended into — their children belong to another
    /// store) and upsert each into `index`. Returns the count indexed.
    async fn reindex_all_nodes(&self, store_id: StoreId, index: &SearchIndex) -> anyhow::Result<usize> {
        let root_id = self.store_manager.read().await.root_node_id(store_id)?;
        let mut manager = self.store_manager.write().await;
        // A freshly added remote store has a root id in its manifest but an
        // empty store document until its first reconcile: nothing to index
        // yet, and not an error. The reconcile's updates feed the index.
        if !manager.store_document(store_id)?.has_node(root_id) {
            return Ok(0);
        }
        let mut stack = vec![root_id];
        let mut count = 0usize;
        while let Some(node_id) = stack.pop() {
            let node = manager.get_node(store_id, node_id).await?;
            let index_node = build_index_node(&node, &self.plugin_host);
            index.upsert(&index_node)?;
            count += 1;
            if !node.is_mount() {
                stack.extend(node.children.iter().copied());
            }
        }
        Ok(count)
    }

    /// Open (or rebuild, if missing or schema-stale) `store_id`'s search
    /// index and install it. Called when a store opens; logs and leaves the
    /// store without a search index on failure rather than failing the open.
    ///
    /// Schema drift is detected by diffing `SCHEMA_HASH_FILE`'s content
    /// before vs. after a successful [`SearchIndex::open`] call, rather than
    /// pre-computing an "expected" hash ourselves: `open` internally ANDs
    /// the `semantic` bool we pass it with its own crate's `semantic`
    /// feature (a cfg gate this crate cannot observe from outside), then
    /// composes and persists the schema that's actually in effect.
    /// `self.semantic_available` is passed through unconditionally — never
    /// gated further here — matching what `open` alone can decide.
    /// Before/after diffing sidesteps needing to replicate that gate:
    /// whatever `open` just wrote is definitionally correct for this build,
    /// whether or not `semantic` ends up honored.
    async fn open_index_for_store(&self, store_id: StoreId) -> anyhow::Result<()> {
        let index_dir = self.index_dir_for(store_id).await?;
        let hash_path = index_dir.join(pimble_search::SCHEMA_HASH_FILE);
        let old_hash = std::fs::read_to_string(&hash_path).ok();

        let index = match SearchIndex::open(&index_dir, self.semantic_available) {
            Ok(index) => index,
            Err(e) => {
                // `Database::open` couldn't tolerate whatever is on disk
                // (typically a schema mismatch from an older build): wipe
                // the directory and start fresh rather than fail the store
                // open over a derived, rebuildable index.
                warn!(
                    "Search index open failed for store {} ({}); rebuilding the index from scratch",
                    store_id, e
                );
                if index_dir.exists() {
                    std::fs::remove_dir_all(&index_dir)?;
                }
                SearchIndex::open(&index_dir, self.semantic_available)?
            }
        };

        let new_hash = std::fs::read_to_string(&hash_path).ok();
        let needs_rebuild = old_hash.is_none() || old_hash != new_hash;

        let index = if needs_rebuild {
            // `SearchIndex::clear()` deletes `Node`s in scan order without
            // regard for `Node.parent` still referencing an as-yet-undeleted
            // parent, and rhypedb's delete-restrict policy rejects that
            // (`delete denied: Node:N is referenced by Node.parent`) —
            // reported upstream (see report to Agent A/team lead). Route
            // around it: drop this handle (releasing its file lock) and
            // recreate the directory from scratch instead of calling
            // `clear()` on a populated index.
            drop(index);
            std::fs::remove_dir_all(&index_dir)?;
            let fresh = SearchIndex::open(&index_dir, self.semantic_available)?;
            let count = self.reindex_all_nodes(store_id, &fresh).await?;
            info!("Rebuilt search index for store {}: {} node(s) indexed", store_id, count);
            fresh
        } else {
            index
        };

        self.install_index_handle(store_id, Arc::new(index)).await;
        Ok(())
    }

    /// Open a search index for every store id in `store_ids` that doesn't
    /// already have one open. Call after any `StoreManager` operation that
    /// may have opened stores implicitly (resolving or validating a mount),
    /// with the list drained via `StoreManager::opened_since`, so an
    /// implicitly-opened source store gets the same treatment as one opened
    /// via `openStore`.
    async fn open_indexes_for_newly_opened(&self, store_ids: Vec<StoreId>) {
        for store_id in store_ids {
            if !self.indexes.read().await.contains_key(&store_id) {
                if let Err(e) = self.open_index_for_store(store_id).await {
                    warn!("Failed to open search index for newly opened store {}: {}", store_id, e);
                }
            }
        }
    }

    /// Delete and rebuild `store_id`'s search index from scratch. Returns the
    /// number of nodes indexed.
    ///
    /// Deletes the directory and reopens fresh rather than calling
    /// `SearchIndex::clear()`, which cannot yet delete a `Node` another
    /// `Node.parent` still references (see `open_index_for_store`'s doc
    /// comment). Drops any existing handle first so its `Arc<SearchIndex>`
    /// starts releasing its file lock before the directory is removed; a
    /// background upsert from the outgoing indexer task landing mid-delete
    /// is a benign, logged no-op (same acceptable race `closeStore` already
    /// has with in-flight debounced upserts).
    async fn rebuild_store_index(&self, store_id: StoreId) -> anyhow::Result<usize> {
        self.indexes.write().await.remove(&store_id);

        let index_dir = self.index_dir_for(store_id).await?;
        if index_dir.exists() {
            std::fs::remove_dir_all(&index_dir)?;
        }
        let index = Arc::new(SearchIndex::open(&index_dir, self.semantic_available)?);
        let count = self.reindex_all_nodes(store_id, &index).await?;
        self.install_index_handle(store_id, index).await;
        Ok(count)
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

        drop(manager);
        if !self.indexes.read().await.contains_key(&store_id) {
            if let Err(e) = self.open_index_for_store(store_id).await {
                warn!("Failed to open search index for store {}: {}", store_id, e);
            }
        }

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

        let mut store = manager
            .get_store_info(store_id)
            .map_err(to_rpc_error)?;
        let sync_config = manager.read_sync_config(store_id).await.map_err(to_rpc_error)?;

        drop(manager);
        // `open_local_store` returns the id of an already-open store as-is
        // (no-op); skip re-opening its index so we never call
        // `SearchIndex::open` twice concurrently on the same directory.
        if !self.indexes.read().await.contains_key(&store_id) {
            if let Err(e) = self.open_index_for_store(store_id).await {
                warn!("Failed to open search index for store {}: {}", store_id, e);
            }
        }

        // Start the store's sync link from `sync.json`, if present
        // (docs/SYNC_CONTRACT.md decision 7). `ensure_link_started` no-ops
        // if one is already running (e.g. `open_local_store` above was a
        // no-op for an already-open store).
        if let Some(config) = sync_config {
            self.ensure_link_started(store_id, config.remote).await;
        }
        store.sync_state = self.sync_state_of(store_id).await;
        mark_replica(&mut store);

        Ok(OpenStoreResponse { store })
    }

    async fn close_store(
        &self,
        request: CloseStoreRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!("Closing store {}", request.store_id);

        self.stop_link(request.store_id).await;

        let mut manager = self.store_manager.write().await;
        manager
            .close_store(request.store_id)
            .await
            .map_err(to_rpc_error)?;
        drop(manager);

        // Clean up subscriptions for this store
        self.subscriptions.write().await.remove_store(request.store_id);
        // Dropping the handle drops its event sender, ending the store's
        // indexing task; the `Arc<SearchIndex>` itself closes once every
        // in-flight debounced-upsert task referencing it finishes.
        self.indexes.write().await.remove(&request.store_id);

        Ok(EmptyResponse {})
    }

    async fn list_stores(&self) -> Result<ListStoresResponse, ErrorObjectOwned> {
        debug!("Listing stores");

        let manager = self.store_manager.read().await;
        let store_ids = manager.list_stores();

        let mut stores = Vec::new();
        for id in store_ids {
            if let Ok(mut store) = manager.get_store_info(id) {
                store.sync_state = self.sync_state_of(id).await;
                mark_replica(&mut store);
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
        // `parent_id: None` creates under the root.
        let parent_id = match request.parent_id {
            Some(parent_id) => parent_id,
            None => manager.root_node_id(request.store_id).map_err(to_rpc_error)?,
        };
        let node_id = manager
            .create_node(request.store_id, node, Some(parent_id))
            .await
            .map_err(to_rpc_error)?;

        // A node that is created and never edited again has no other flush
        // point; without this it exists only in memory until shutdown.
        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        self.notify_store_change(request.store_id, StoreChangeKind::NodeCreated { node_id, parent_id }, None).await;
        self.enqueue_index_event(request.store_id, IndexEvent::Upsert(node_id)).await;

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
        self.enqueue_index_event(request.store_id, IndexEvent::Upsert(request.node_id)).await;

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
        self.enqueue_index_event(request.store_id, IndexEvent::ContentChanged(request.node_id)).await;

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
        let removal = manager
            .delete_node(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        self.notify_store_change(
            request.store_id,
            StoreChangeKind::NodeDeleted { node_id: request.node_id, parent_id: removal.parent_id },
            None,
        )
        .await;
        for node_id in removal.removed {
            self.enqueue_index_event(request.store_id, IndexEvent::Remove(node_id)).await;
        }

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
        let old_parent_id = manager
            .move_node(request.store_id, request.node_id, request.new_parent_id, request.position)
            .await
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        self.notify_store_change(
            request.store_id,
            StoreChangeKind::NodeMoved { node_id: request.node_id, old_parent_id, new_parent_id: request.new_parent_id },
            None,
        )
        .await;
        // Re-upsert the moved node: its `parent` relationship is what changed.
        self.enqueue_index_event(request.store_id, IndexEvent::Upsert(request.node_id)).await;

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
        let (store_id, children) = manager
            .get_children(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;
        let newly_opened = manager.opened_since();
        drop(manager);

        // A mount's source store may have just been opened implicitly to
        // resolve it; give it a search index like any other open store.
        self.open_indexes_for_newly_opened(newly_opened).await;

        Ok(GetChildrenResponse { store_id, children })
    }

    async fn create_mount(
        &self,
        request: CreateMountRequest,
    ) -> Result<CreateMountResponse, ErrorObjectOwned> {
        info!(
            "Creating mount in store {} under parent {}, source: {}:{}",
            request.store_id, request.parent_id, request.source_store_id, request.source_node_id
        );

        let mut manager = self.store_manager.write().await;

        // Fill `source_path` from the registry's Local endpoint for the
        // source store, if it has one: this is what lets the mount resolve
        // after a restart even if the source isn't otherwise reopened (see
        // `StoreManager::ensure_store_open`).
        let source_path = match manager.registry().lookup(&request.source_store_id) {
            Some(StoreEndpoint::Local { path }) => Some(path.clone()),
            _ => None,
        };

        let mount_ref = MountRef {
            source_store: request.source_store_id,
            source_node: request.source_node_id,
            source_path,
        };

        // Validate that this mount won't create a cycle. This also rejects
        // a mount-node parent transitively: `create_node` below is the
        // authoritative check, but validating first avoids opening/walking
        // stores for a request that's going to fail anyway.
        manager
            .validate_mount_creation(request.store_id, request.parent_id, &mount_ref)
            .await
            .map_err(to_rpc_error)?;

        // Create the mount node. `create_node` rejects a mount-node parent
        // (`StoreError::MountHasNoChildren`), surfaced here as an RPC error.
        let mut node = Node::mount_with_ref(mount_ref.clone());
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

        let newly_opened = manager.opened_since();
        drop(manager);

        self.open_indexes_for_newly_opened(newly_opened).await;
        self.notify_store_change(request.store_id, StoreChangeKind::NodeCreated { node_id, parent_id: request.parent_id }, None).await;
        self.enqueue_index_event(request.store_id, IndexEvent::Upsert(node_id)).await;

        Ok(CreateMountResponse { node_id, mount_ref })
    }

    // ── Replica sync (docs/SYNC_CONTRACT.md) ─────────────────────────

    async fn add_remote_store(
        &self,
        request: AddRemoteStoreRequest,
    ) -> Result<OpenStoreResponse, ErrorObjectOwned> {
        // `None` lets the server choose the replica's location; the user
        // never picks one (decision 8).
        let path = request.path.clone().unwrap_or_else(|| default_replica_path(request.remote_store_id));

        info!(
            "Adding remote store {} from {} at {:?}",
            request.remote_store_id, request.remote.url, path
        );

        // A store this server already holds cannot also be added as a
        // replica (the manager refuses too); this also covers pointing the
        // request at this very server.
        {
            let manager = self.store_manager.read().await;
            if manager.is_open(request.remote_store_id) {
                let where_ = manager
                    .get_store_info(request.remote_store_id)
                    .ok()
                    .and_then(|s| s.local_path().cloned())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                return Err(to_rpc_error(format!(
                    "store {} is already open locally at {}; use setStoreSync to link it",
                    request.remote_store_id, where_
                )));
            }
        }

        // Ask the remote for the store (decision 8): its name and root node
        // id, matched by id via `listStores`.
        let remote_client = PimbleClient::connect_with_auth(request.remote.url.as_str(), &request.remote.auth)
            .await
            .map_err(|e| to_rpc_error(format!("Failed to connect to remote {}: {}", request.remote.url, e)))?;
        let remote_stores = remote_client.list_stores().await.map_err(to_rpc_error)?;
        let remote_store = remote_stores
            .into_iter()
            .find(|s| s.id == request.remote_store_id)
            .ok_or_else(|| {
                to_rpc_error(format!(
                    "Remote {} has no open store {}",
                    request.remote.url, request.remote_store_id
                ))
            })?;
        drop(remote_client);

        // An empty replica, never `StoreDocument::new` (decision 8: two
        // independently created roots for the same id would merge into
        // duplicated children).
        let mut manager = self.store_manager.write().await;
        let store_id = manager
            .create_replica(&path, remote_store.id, &remote_store.name, remote_store.root_node_id)
            .await
            .map_err(to_rpc_error)?;
        manager
            .write_sync_config(store_id, &SyncConfig { remote: request.remote.clone() })
            .await
            .map_err(to_rpc_error)?;
        let mut store = manager.get_store_info(store_id).map_err(to_rpc_error)?;
        let newly_opened = manager.opened_since();
        drop(manager);

        self.open_indexes_for_newly_opened(newly_opened).await;

        self.ensure_link_started(store_id, request.remote.clone()).await;

        // Wait up to 10s for the first full reconcile to reach `Synced`
        // (decision 8), then answer anyway with the current state.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let state = self.sync_state_of(store_id).await;
            let is_synced = matches!(state, SyncState::Synced { .. });
            if is_synced || std::time::Instant::now() >= deadline {
                store.sync_state = state;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        mark_replica(&mut store);
        Ok(OpenStoreResponse { store })
    }

    async fn set_store_sync(
        &self,
        request: SetStoreSyncRequest,
    ) -> Result<GetStoreSyncResponse, ErrorObjectOwned> {
        info!("Setting sync for store {}: {:?}", request.store_id, request.remote.as_ref().map(|r| &r.url));

        match request.remote {
            Some(remote) => {
                // Refuse if the remote has no store with this id.
                let remote_client = PimbleClient::connect_with_auth(remote.url.as_str(), &remote.auth)
                    .await
                    .map_err(|e| to_rpc_error(format!("Failed to connect to remote {}: {}", remote.url, e)))?;
                let remote_stores = remote_client.list_stores().await.map_err(to_rpc_error)?;
                let Some(twin) = remote_stores.iter().find(|s| s.id == request.store_id) else {
                    return Err(to_rpc_error(format!(
                        "Remote {} has no open store {}",
                        remote.url, request.store_id
                    )));
                };
                // The remote's copy must be a different directory. The same
                // path means the remote is this server (or another server on
                // this machine serving the very same directory): a link would
                // reconcile a store with itself.
                let local_path = self
                    .store_manager
                    .read()
                    .await
                    .get_store_info(request.store_id)
                    .map_err(to_rpc_error)?
                    .local_path()
                    .cloned();
                if twin.local_path().is_some() && twin.local_path() == local_path.as_ref() {
                    return Err(to_rpc_error(format!(
                        "remote {} is this server: store {} lives at {} on both ends",
                        remote.url,
                        request.store_id,
                        local_path.map(|p| p.display().to_string()).unwrap_or_default()
                    )));
                }
                drop(remote_client);

                let manager = self.store_manager.read().await;
                manager
                    .write_sync_config(request.store_id, &SyncConfig { remote: remote.clone() })
                    .await
                    .map_err(to_rpc_error)?;
                drop(manager);

                // Replace any existing link so it points at the new remote.
                self.stop_link(request.store_id).await;
                self.ensure_link_started(request.store_id, remote).await;
            }
            None => {
                self.stop_link(request.store_id).await;
                let manager = self.store_manager.read().await;
                manager.clear_sync_config(request.store_id).await.map_err(to_rpc_error)?;
                drop(manager);
                self.notify_sync_state_changed(request.store_id, SyncState::Offline).await;
            }
        }

        let manager = self.store_manager.read().await;
        let remote_now = manager
            .read_sync_config(request.store_id)
            .await
            .map_err(to_rpc_error)?
            .map(|c| c.remote);
        drop(manager);
        let state = self.sync_state_of(request.store_id).await;

        Ok(GetStoreSyncResponse { remote: remote_now, state })
    }

    async fn get_store_sync(
        &self,
        request: GetStoreSyncRequest,
    ) -> Result<GetStoreSyncResponse, ErrorObjectOwned> {
        let manager = self.store_manager.read().await;
        let remote = manager
            .read_sync_config(request.store_id)
            .await
            .map_err(to_rpc_error)?
            .map(|c| c.remote);
        drop(manager);
        let state = self.sync_state_of(request.store_id).await;

        Ok(GetStoreSyncResponse { remote, state })
    }

    async fn list_remote_stores(
        &self,
        request: ListRemoteStoresRequest,
    ) -> Result<ListStoresResponse, ErrorObjectOwned> {
        // Interface stub: agent A implements (docs/HARDENING_CONTRACT.md).
        Err(to_rpc_error(format!("listRemoteStores for {} is not implemented yet", request.remote.url)))
    }

    async fn remove_replica(
        &self,
        request: RemoveReplicaRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        // Interface stub: agent A implements (docs/HARDENING_CONTRACT.md).
        Err(to_rpc_error(format!("removeReplica for store {} is not implemented yet", request.store_id)))
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

        let state = manager.mount_state(&mount_ref).await;
        let newly_opened = manager.opened_since();
        drop(manager);

        self.open_indexes_for_newly_opened(newly_opened).await;

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
        let touched = manager
            .apply_store_doc_update(request.store_id, &bytes)
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        // Decision 9: an id whose node entry still exists gets upserted
        // (this is what makes a title change arriving as a store update get
        // reindexed); an id no longer present was removed.
        let (upserts, removals): (Vec<NodeId>, Vec<NodeId>) = {
            let doc = manager.store_document(request.store_id).map_err(to_rpc_error)?;
            touched.iter().copied().partition(|id| doc.has_node(*id))
        };

        drop(manager);

        // Broadcast to other subscribers, carrying the raw update bytes so
        // they can apply it directly instead of refetching.
        let notification = StoreChangedNotification {
            store_id: request.store_id,
            change_kind: StoreChangeKind::TreeStructure { node_ids: touched },
            source_client_id: Some(request.client_id.clone()),
            update: Some(request.update.clone()),
        };
        self.subscriptions.write().await.notify_store_change(&notification).await;

        for node_id in upserts {
            self.enqueue_index_event(request.store_id, IndexEvent::Upsert(node_id)).await;
        }
        for node_id in removals {
            self.enqueue_index_event(request.store_id, IndexEvent::Remove(node_id)).await;
        }

        Ok(EmptyResponse {})
    }

    async fn sync_node_contents(
        &self,
        request: SyncNodeContentsRequest,
    ) -> Result<SyncNodeContentsResponse, ErrorObjectOwned> {
        debug!(
            "Sync content of {} node(s) in store {}",
            request.nodes.len(), request.store_id
        );

        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;

        if request.nodes.len() > MAX_SYNC_NODE_CONTENTS {
            return Err(to_rpc_error(format!(
                "syncNodeContents takes at most {} nodes per request, got {}",
                MAX_SYNC_NODE_CONTENTS,
                request.nodes.len()
            )));
        }

        let mut store_manager = self.store_manager.write().await;
        if !store_manager.is_open(request.store_id) {
            return Err(to_rpc_error(pimble_store::StoreError::NotOpen(request.store_id)));
        }

        // Stateless reconciliation: no per-client sync state is kept for
        // node content. For each node the client sends its state vector, we
        // hand back everything we have beyond it plus our own state vector.
        // A node this store does not have is left out.
        let mut nodes = Vec::with_capacity(request.nodes.len());
        for entry in request.nodes {
            let client_sv = b64
                .decode(&entry.state_vector)
                .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;
            let doc = match store_manager.get_node_document(request.store_id, entry.node_id).await {
                Ok(doc) => doc,
                Err(pimble_store::StoreError::NodeNotFound(_)) => continue,
                Err(e) => return Err(to_rpc_error(e)),
            };
            let diff = doc.diff_since(&client_sv).map_err(to_rpc_error)?;
            nodes.push(NodeContentDiff {
                node_id: entry.node_id,
                diff: b64.encode(&diff),
                state_vector: b64.encode(doc.state_vector()),
            });
        }

        Ok(SyncNodeContentsResponse { nodes })
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

        // Apply the edit to the server's persistent yrs document. The
        // full-snapshot path is `updateNodeContent`, not an `EditOperation`.
        let EditOperation::IncrementalChanges { ref changes } = request.operation;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(changes)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;
        let mut store_manager = self.store_manager.write().await;
        store_manager
            .apply_content_update(request.store_id, request.node_id, &bytes)
            .await
            .map_err(to_rpc_error)?;
        drop(store_manager);

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

        // Content upserts are debounced per node (2s) here, independent of
        // the disk-flush debounce above: `applyEdit` fires per keystroke.
        self.enqueue_index_event(request.store_id, IndexEvent::ContentChanged(request.node_id)).await;

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

        let limit = request.limit.max(1);
        let query = SearchQuery {
            text: request.query.clone(),
            semantic: request.semantic,
            limit,
        };

        // Empty request.stores means every open store.
        let store_ids: Vec<StoreId> = if request.stores.is_empty() {
            self.indexes.read().await.keys().copied().collect()
        } else {
            request.stores.clone()
        };

        let mut hits: Vec<(StoreId, pimble_search::SearchHit)> = Vec::new();
        {
            let indexes = self.indexes.read().await;
            for store_id in &store_ids {
                let Some(handle) = indexes.get(store_id) else {
                    continue; // no index open for this store; nothing to search
                };
                match handle.index.search(&query) {
                    Ok(store_hits) => hits.extend(store_hits.into_iter().map(|h| (*store_id, h))),
                    Err(SearchError::IndexBuilding { done, total }) => {
                        return Err(index_building_error(done as usize, total as usize));
                    }
                    Err(e) => return Err(to_rpc_error(e)),
                }
            }
        }

        // Merge by score across stores, then take the overall top `limit`.
        hits.sort_by(|a, b| b.1.score.partial_cmp(&a.1.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);

        // Look up each hit's node type (the index's own `kind` field means
        // the matched chunk's kind here, not the node type — see
        // `SearchResultItem::node_type`).
        let mut manager = self.store_manager.write().await;
        let mut results = Vec::with_capacity(hits.len());
        for (store_id, hit) in &hits {
            let node_type = manager
                .get_node(*store_id, hit.node_id)
                .await
                .map(|n| n.node_type)
                .unwrap_or_default();
            results.push(SearchResultItem {
                node_id: hit.node_id,
                store_id: *store_id,
                score: hit.score,
                title: hit.title.clone(),
                snippet: hit.snippet.clone(),
                kind: hit.kind.clone(),
                node_type,
                path: hit.path.clone().unwrap_or_default(),
            });
        }

        let total = results.len();
        Ok(SearchResponse { results, total })
    }

    async fn rebuild_index(
        &self,
        request: RebuildIndexRequest,
    ) -> Result<RebuildIndexResponse, ErrorObjectOwned> {
        info!("Rebuilding search index for store {}", request.store_id);

        let indexed = self
            .rebuild_store_index(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(RebuildIndexResponse { indexed })
    }
}
