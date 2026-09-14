//! RPC method handlers

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jsonrpsee::core::{async_trait, SubscriptionResult};
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::{PendingSubscriptionSink, SubscriptionMessage};
use pimble_core::{Node, MountRef, NodeId, StoreId, StoreLocation, Workspace};
use pimble_plugins::PluginHost;
use pimble_rpc::{
    index_building_error, to_rpc_error, ApplyEditRequest, ApplyEditResponse,
    ApplyStoreUpdateRequest, CloseStoreRequest, CreateMountRequest, CreateMountResponse,
    CreateNodeRequest, CreateNodeResponse, CreateStoreRequest, CreateStoreResponse,
    CreateWorkspaceRequest, DeleteNodeRequest, EditOperation, EmptyResponse, GetChildrenRequest,
    GetChildrenResponse, GetMountStateRequest, GetMountStateResponse, GetNodeRequest,
    GetNodeResponse, GetNodesRequest, GetNodesResponse, ListStoresResponse, LoadWorkspaceRequest,
    LoadWorkspaceResponse, MoveNodeRequest, NodeContentChangedNotification, OpenStoreRequest,
    OpenStoreResponse, PimbleApiServer, RebuildIndexRequest, RebuildIndexResponse,
    SaveWorkspaceRequest, SearchRequest, SearchResponse, SearchResultItem, StoreChangeKind,
    StoreChangedNotification, SyncNodeContentRequest, SyncNodeContentResponse,
    SyncStoreDocumentRequest, SyncStoreDocumentResponse, UpdateNodeContentRequest,
    UpdateNodeMetadataRequest,
};
use pimble_search::{IndexNode, SearchError, SearchIndex, SearchQuery};
use pimble_store::StoreManager;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

/// How long to wait, after a node's content last changed, before reading its
/// units and upserting them into the search index. `applyEdit` fires on every
/// keystroke; this coalesces a burst of edits into one re-index per node,
/// independent of the (unrelated) content-flush debounce above.
const CONTENT_INDEX_DEBOUNCE: Duration = Duration::from_millis(2_000);

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

        let store = manager
            .get_store_info(store_id)
            .map_err(to_rpc_error)?;

        drop(manager);
        // `open_local_store` returns the id of an already-open store as-is
        // (no-op); skip re-opening its index so we never call
        // `SearchIndex::open` twice concurrently on the same directory.
        if !self.indexes.read().await.contains_key(&store_id) {
            if let Err(e) = self.open_index_for_store(store_id).await {
                warn!("Failed to open search index for store {}: {}", store_id, e);
            }
        }

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
        manager
            .delete_node(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        self.notify_store_change(request.store_id, StoreChangeKind::NodeDeleted { node_id: request.node_id }, None).await;
        self.enqueue_index_event(request.store_id, IndexEvent::Remove(request.node_id)).await;

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

        drop(manager);
        // No existing `notify_store_change` call sits next to this one (a
        // pre-existing gap, out of this step's scope) but the mount node
        // should still be indexed.
        self.enqueue_index_event(request.store_id, IndexEvent::Upsert(node_id)).await;

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
