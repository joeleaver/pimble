//! RPC method handlers

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jsonrpsee::core::{async_trait, SubscriptionResult};
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::{Extensions, PendingSubscriptionSink, SubscriptionMessage};
use pimble_client::{describe_connect_error, PimbleClient};
use pimble_core::{AuthMethod, Node, MountRef, MountState, NodeId, RemoteEndpoint, StoreId, StoreKind, StoreLocation, SyncState, Workspace};
use pimble_plugins::PluginHost;
use pimble_rpc::{
    encrypted_store_error, index_building_error, snapshot_required_error, to_rpc_error, ApplyEditRequest, ApplyEditResponse,
    AddRemoteStoreRequest, ApplyStoreUpdateRequest, CloseStoreRequest, CloudAddHostedStoreRequest, CloudHostStoreRequest,
    CloudHostStoreResponse, CloudHostedStoreInfo, CloudListHostedStoresResponse, CloudSignInRequest, CloudStatusResponse,
    CreateMountRequest, CreateMountResponse,
    CreateNodeRequest, CreateNodeResponse, CreateStoreRequest, CreateStoreResponse,
    CreateWorkspaceRequest, DeleteNodeRequest, EditOperation, EmptyResponse, GetChildrenRequest,
    GetChildrenResponse, GetMountStateRequest, GetMountStateResponse, GetNodeRequest, GetStoreSyncRequest, GetStoreSyncResponse, SetStoreSyncRequest,
    GetNodeResponse, GetNodesRequest, GetNodesResponse, ListRemoteStoresRequest, ListStoresResponse, LoadWorkspaceRequest,
    LoadWorkspaceResponse, MoveNodeRequest, NodeContentChangedNotification, NodeContentDiff, OpenStoreRequest,
    OpenStoreResponse, PimbleApiServer, RebuildIndexRequest, RebuildIndexResponse, RemoveReplicaRequest,
    SaveWorkspaceRequest, SearchRequest, SearchResponse, SearchResultItem, StoreChangeKind,
    StoreChangedNotification, SyncNodeContentsRequest, SyncNodeContentsResponse,
    SyncStoreDocumentRequest, SyncStoreDocumentResponse, UpdateNodeContentRequest,
    UpdateNodeMetadataRequest,
    VaultAppendRequest, VaultAppendResponse, VaultDocId, VaultDocInfo, VaultEntry, VaultFetchRequest,
    VaultFetchResponse, VaultListDocsRequest, VaultListDocsResponse, VaultSnapshotRequest,
    MAX_SYNC_NODE_CONTENTS,
};
use pimble_search::{IndexNode, SearchError, SearchIndex, SearchQuery};
use pimble_store::{StoreEndpoint, StoreManager, SyncConfig, SyncMode};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::keystore::Keystore;
use crate::principal::{authorize, authorize_service_only, principal_of, readable, service_extensions, Access};
use crate::sync_link::{SyncLink, SyncLinkHandle};
use crate::vault_link::{VaultLink, VaultLinkHandle};

/// How long to wait, after a node's content last changed, before reading its
/// units and upserting them into the search index. `applyEdit` fires on every
/// keystroke; this coalesces a burst of edits into one re-index per node,
/// independent of the (unrelated) content-flush debounce above.
const CONTENT_INDEX_DEBOUNCE: Duration = Duration::from_millis(2_000);

/// How long to wait after a content edit before flushing it to disk. A burst
/// of keystrokes coalesces into at most one flush per window, instead of one
/// per edit.
const CONTENT_FLUSH_DEBOUNCE: Duration = Duration::from_millis(750);

/// The directory a server creates replicas in unless
/// [`crate::ServerConfig::replicas_dir`] says otherwise: `<data dir>/
/// pimble/replicas/`. A store inside it is a replica
/// (`Store::is_replica`), and only such a store can be removed with
/// `removeReplica`.
pub(crate) fn default_replicas_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pimble")
        .join("replicas")
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

/// A store's open search index, the channel that feeds its indexing task, and
/// everything needed to shut that task (and every debounced upsert it has
/// spawned) down completely — see [`RpcHandler::shutdown_index`]
/// (docs/history/HARDENING_CONTRACT.md decision 12).
struct IndexHandle {
    index: Arc<SearchIndex>,
    events: mpsc::UnboundedSender<IndexEvent>,
    /// The `StoreIndexer::run` task.
    main_task: tokio::task::JoinHandle<()>,
    /// Every debounced content-upsert task currently sleeping or running,
    /// shared with the `StoreIndexer` that spawns into it (`schedule_content_upsert`).
    debounce_tasks: Arc<tokio::sync::Mutex<tokio::task::JoinSet<()>>>,
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
    /// Shared with this store's [`IndexHandle`], so a shutdown can find and
    /// cancel every debounce task this indexer has spawned, not just the ones
    /// it happens to know about at the moment it starts shutting down.
    debounce_tasks: Arc<tokio::sync::Mutex<tokio::task::JoinSet<()>>>,
}

impl StoreIndexer {
    /// Drive `rx` until the sender side (the store's `IndexHandle`) is
    /// dropped, e.g. on `closeStore`. Ends only once `rx` is both closed and
    /// drained, so every `IndexEvent` sent before the handle was dropped —
    /// including one that spawns a fresh debounce task — is still seen.
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
                    Arc::clone(&self).schedule_content_upsert(node_id).await;
                }
            }
        }
    }

    /// Bump `node_id`'s debounce generation and spawn a task that, after
    /// [`CONTENT_INDEX_DEBOUNCE`], re-indexes the node if no newer edit has
    /// arrived in the meantime. The task is spawned into `debounce_tasks`
    /// (not bare `tokio::spawn`) so a shutdown can find and cancel it instead
    /// of it quietly outliving the `SearchIndex` handle it holds.
    async fn schedule_content_upsert(self: Arc<Self>, node_id: NodeId) {
        let generation = {
            let mut gens = self.content_gen.lock().unwrap();
            let g = gens.entry(node_id).or_insert(0);
            *g += 1;
            *g
        };
        let indexer = Arc::clone(&self);
        let mut tasks = self.debounce_tasks.lock().await;
        // `JoinSet` keeps a finished task's slot until it's joined, and
        // `applyEdit` calls this once per keystroke — without reaping here,
        // a long editing session would grow the set by one dead entry per
        // keystroke, all sitting unjoined until the store closes.
        // `try_join_next` is non-blocking (only pops entries already
        // notified as done), so this never waits on a still-sleeping task.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            tokio::time::sleep(CONTENT_INDEX_DEBOUNCE).await;
            let still_current = {
                let gens = indexer.content_gen.lock().unwrap();
                gens.get(&node_id).copied() == Some(generation)
            };
            if still_current {
                if let Err(e) = indexer.upsert_now(node_id).await {
                    warn!("Indexing node {} in store {} failed: {}", node_id, indexer.store_id, e);
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

/// What this server knows about the mounts it has resolved
/// (docs/history/REMOTE_MOUNTS_CONTRACT.md decisions 5 and 8), all keyed by the
/// mount's *source* store: that is the store whose link state, or whose
/// replica creation, decides every one of those mounts' states.
///
/// Guarded by one `std::sync::Mutex` so the "is a creation already in
/// flight?" check and the "mark one in flight" write cannot interleave with
/// another resolution of the same source. Nothing in here is ever held
/// across an `.await`.
#[derive(Default)]
struct MountTracking {
    /// Source store -> every `(mounting store, mount node)` this server has
    /// resolved from it. Added to by every resolution; a mounting store's
    /// entries go when that store closes. An entry for a mount node that
    /// has since been deleted is harmless: the notification it produces
    /// names a node no client has.
    resolved: HashMap<StoreId, HashSet<(StoreId, NodeId)>>,
    /// Source stores whose replica a background task is creating right now
    /// (decision 8). Two resolutions of the same source start one task.
    in_flight: HashSet<StoreId>,
    /// Why the last background replica creation for a source failed, in
    /// words a user can act on (decision 3). Cleared when a fresh attempt
    /// starts, so a retry never reports a stale reason.
    failed: HashMap<StoreId, String>,
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
    /// Saved per-remote credentials (docs/history/HARDENING_CONTRACT.md decision 4),
    /// resolved whenever this server connects to a remote and consulted (or
    /// updated) only by `add_remote_store`, `set_store_sync`,
    /// `list_remote_stores` and the sync link's own connect.
    credentials: Arc<crate::credentials::CredentialStore>,
    /// Where this server creates replicas (`addRemoteStore` with
    /// `path: None`, and every replica a remote mount's resolution creates)
    /// and, equivalently, which directory makes a store a replica for
    /// `Store::is_replica` and `removeReplica`. [`default_replicas_dir`]
    /// unless `ServerConfig::replicas_dir` overrides it.
    replicas_dir: Arc<PathBuf>,
    /// Mounts resolved so far and replica creations in flight
    /// (docs/history/REMOTE_MOUNTS_CONTRACT.md decisions 5 and 8).
    mounts: Arc<Mutex<MountTracking>>,
    /// One running [`VaultLinkHandle`] per vault-linked store
    /// (docs/CRYPTO_CONTRACT.md), the vault-mode counterpart of `links`.
    /// Populated by `openStore` (from `sync.json`'s `mode: "vault"`),
    /// `cloudHostStore` and `cloudAddHostedStore`; removed (after `stop()`)
    /// by `closeStore`.
    vault_links: Arc<RwLock<HashMap<StoreId, VaultLinkHandle>>>,
    /// This server's signed-in Pimble Cloud account and unwrapped store
    /// keys (docs/CRYPTO_CONTRACT.md), consulted by the `cloud*` RPCs and by
    /// every `VaultLink`.
    keystore: Arc<Keystore>,
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
        Self::with_credentials_path(store_manager, semantic_available, crate::credentials::default_credentials_path())
    }

    /// Like [`RpcHandler::with_semantic_available`], but with the saved
    /// credentials file loaded from `credentials_path` instead of
    /// [`crate::credentials::default_credentials_path`]. `PimbleServer::start`
    /// uses this so `ServerConfig::credentials_path` actually takes effect;
    /// tests use it to keep credentials in a temp directory.
    pub fn with_credentials_path(store_manager: Arc<RwLock<StoreManager>>, semantic_available: bool, credentials_path: PathBuf) -> Self {
        Self::with_paths(store_manager, semantic_available, credentials_path, default_replicas_dir())
    }

    /// Like [`RpcHandler::with_credentials_path`], but with the replicas
    /// directory given explicitly instead of [`default_replicas_dir`].
    /// `PimbleServer::start` uses this so `ServerConfig::replicas_dir`
    /// takes effect; tests point it at a temp directory so a replica this
    /// server creates never lands in the real data directory. The keystore
    /// path defaults to [`crate::keystore::default_keystore_path`]; see
    /// [`RpcHandler::with_all_paths`] for a caller (`PimbleServer::start`,
    /// and tests) that wants that overridden too.
    pub fn with_paths(
        store_manager: Arc<RwLock<StoreManager>>,
        semantic_available: bool,
        credentials_path: PathBuf,
        replicas_dir: PathBuf,
    ) -> Self {
        Self::with_all_paths(store_manager, semantic_available, credentials_path, replicas_dir, crate::keystore::default_keystore_path())
    }

    /// Like [`RpcHandler::with_paths`], but with the keystore path given
    /// explicitly instead of [`crate::keystore::default_keystore_path`].
    /// `PimbleServer::start` uses this so `ServerConfig::keystore_path`
    /// takes effect; tests point it at a temp path so a sign-in never
    /// touches the real config directory.
    pub fn with_all_paths(
        store_manager: Arc<RwLock<StoreManager>>,
        semantic_available: bool,
        credentials_path: PathBuf,
        replicas_dir: PathBuf,
        keystore_path: PathBuf,
    ) -> Self {
        Self {
            store_manager,
            subscriptions: Arc::new(RwLock::new(SubscriptionRegistry::new())),
            flush_debouncer: Arc::new(FlushDebouncer::default()),
            plugin_host: Arc::new(pimble_plugins::create_default_host()),
            indexes: Arc::new(RwLock::new(HashMap::new())),
            semantic_available,
            links: Arc::new(RwLock::new(HashMap::new())),
            credentials: Arc::new(crate::credentials::CredentialStore::new(credentials_path)),
            replicas_dir: Arc::new(replicas_dir),
            mounts: Arc::new(Mutex::new(MountTracking::default())),
            vault_links: Arc::new(RwLock::new(HashMap::new())),
            keystore: Arc::new(Keystore::new(keystore_path)),
        }
    }

    /// Where `addRemoteStore` places a replica when the caller passes
    /// `path: None` (docs/SYNC_CONTRACT.md decision 8):
    /// `<replicas dir>/<store id>.pimble`. The user never chooses this
    /// location; a caller like the CLI may still pass an explicit `path`.
    /// `LocalStore::create_replica` creates every ancestor directory, so
    /// nothing here needs to pre-create the replicas directory.
    fn default_replica_path(&self, store_id: StoreId) -> PathBuf {
        self.replicas_dir.join(format!("{}.pimble", store_id))
    }

    /// Fill in the field of a `Store` only the server knows: whether it is
    /// a replica (its directory is inside this server's replicas
    /// directory).
    fn mark_replica(&self, store: &mut pimble_core::Store) {
        store.is_replica = store.local_path().map_or(false, |p| p.starts_with(self.replicas_dir.as_path()));
    }

    /// The guard every store-scoped RPC but the four vault ones needs
    /// (docs/CRYPTO_CONTRACT.md "Pimble server, a store of kind `vault`"): a
    /// vault store has no `StoreDocument`, `ContentDoc` or search index, so
    /// none of those RPCs may touch it. `Ok(())` when the store is `Plain`
    /// or not open at all — a missing store still fails downstream with its
    /// own, more specific `NotOpen`/`StoreNotFound` error.
    async fn reject_if_vault(&self, store_id: StoreId) -> Result<(), ErrorObjectOwned> {
        if self.store_manager.read().await.store_kind(store_id) == Some(StoreKind::Vault) {
            return Err(encrypted_store_error(format!(
                "store {} is encrypted; use the vault API", store_id
            )));
        }
        Ok(())
    }

    // ── Replica sync (docs/SYNC_CONTRACT.md) ─────────────────────────

    /// Shared handle to the store manager, for [`crate::sync_link`] to read
    /// and write CRDT documents directly (state vectors, diffs) alongside
    /// the handler's own `apply_store_update`/`apply_edit` (used to actually
    /// merge, persist, broadcast, and index a remote change).
    pub(crate) fn store_manager_handle(&self) -> Arc<RwLock<StoreManager>> {
        Arc::clone(&self.store_manager)
    }

    /// Shared handle to the saved-credentials store, for [`crate::sync_link`]
    /// to resolve auth the same way `add_remote_store`/`set_store_sync`/
    /// `list_remote_stores` do (docs/history/HARDENING_CONTRACT.md decision 4).
    pub(crate) fn credentials(&self) -> Arc<crate::credentials::CredentialStore> {
        Arc::clone(&self.credentials)
    }

    /// Subscribe to this server's in-process broadcast of local
    /// notifications (decision 3), used by a sync link to learn about local
    /// changes to forward to its remote.
    pub(crate) async fn subscribe_local_changes(&self) -> broadcast::Receiver<LocalChange> {
        self.subscriptions.read().await.subscribe_local()
    }

    /// Notify a store's local subscribers that its sync link's state
    /// changed, and every mount sourced from that store that its own state
    /// changed with it (docs/history/REMOTE_MOUNTS_CONTRACT.md decisions 3 and 5:
    /// a mount's state is derived from its source's link, so this is the
    /// one place both are published from).
    pub(crate) async fn notify_sync_state_changed(&self, store_id: StoreId, state: SyncState) {
        self.notify_store_change(store_id, StoreChangeKind::SyncStateChanged { state }, None).await;
        self.notify_mount_states_for_source(store_id).await;
    }

    /// A store's sync link state, `Offline` if it has neither a sync link
    /// nor a vault link (a store is linked as at most one of the two).
    async fn sync_state_of(&self, store_id: StoreId) -> SyncState {
        if let Some(handle) = self.links.read().await.get(&store_id) {
            return handle.state();
        }
        if let Some(handle) = self.vault_links.read().await.get(&store_id) {
            return handle.state();
        }
        SyncState::Offline
    }

    /// What `store_id` is currently linked to: `Plain` (unlinked, or an
    /// ordinary sync link to a `Plain` twin) or `Vault` (a vault link,
    /// docs/CRYPTO_CONTRACT.md), read from `sync.json`'s `mode`. `Plain` for
    /// a store with no `sync.json` at all (unlinked, or itself a `Vault`
    /// store, which never has one) or one that fails to read.
    async fn sync_mode_of(&self, store_id: StoreId) -> StoreKind {
        let manager = self.store_manager.read().await;
        match manager.read_sync_config(store_id).await {
            Ok(Some(config)) => match config.mode {
                SyncMode::Sync => StoreKind::Plain,
                SyncMode::Vault => StoreKind::Vault,
            },
            _ => StoreKind::Plain,
        }
    }

    /// Start a sync link for `store_id` if one isn't already running.
    /// `openStore`, `addRemoteStore`, `set_store_sync(Some(remote))` and no-op if
    /// a link is already present.
    async fn ensure_link_started(&self, store_id: StoreId, remote: RemoteEndpoint) {
        // Decision 4 of docs/history/REMOTE_MOUNTS_CONTRACT.md: the link answers
        // `last_sync` from `sync.json` until it next reaches `Synced`, so a
        // mount sourced from this store reports `Cached { last_sync }`
        // rather than `Connecting` after a restart with the remote down.
        let last_sync = {
            let manager = self.store_manager.read().await;
            manager.read_sync_config(store_id).await.ok().flatten().and_then(|c| c.last_sync)
        };
        let mut links = self.links.write().await;
        if links.contains_key(&store_id) {
            return;
        }
        let handle = SyncLink::start(self.clone(), store_id, remote, last_sync);
        links.insert(store_id, handle);
    }

    /// Stop and remove `store_id`'s sync link, if any.
    async fn stop_link(&self, store_id: StoreId) {
        if let Some(handle) = self.links.write().await.remove(&store_id) {
            handle.stop();
        }
    }

    // ── Cloud / vault links (docs/CRYPTO_CONTRACT.md) ────────────────────

    /// Shared handle to this server's keystore, for [`crate::vault_link`] to
    /// look up the signed-in account's session (to mint a JWT) and store
    /// keys (to encrypt/decrypt blobs).
    pub(crate) fn keystore(&self) -> Arc<Keystore> {
        Arc::clone(&self.keystore)
    }

    /// Start a vault link for `store_id` if one isn't already running
    /// (`openStore` restarting one from `sync.json`, `cloudHostStore`,
    /// `cloudAddHostedStore`). `rpc_url` is the hosted twin's Pimble RPC
    /// endpoint (`mint_token`'s `rpc_url`); the link mints its own bearer
    /// fresh from the keystore on every connect, so no credential is passed
    /// in here.
    async fn ensure_vault_link_started(&self, store_id: StoreId, rpc_url: url::Url, key_id: Uuid) {
        let last_sync = {
            let manager = self.store_manager.read().await;
            manager.read_sync_config(store_id).await.ok().flatten().and_then(|c| c.last_sync)
        };
        let mut vault_links = self.vault_links.write().await;
        if vault_links.contains_key(&store_id) {
            return;
        }
        let handle = VaultLink::start(self.clone(), store_id, rpc_url, key_id, last_sync);
        vault_links.insert(store_id, handle);
    }

    /// Stop and remove `store_id`'s vault link, if any.
    async fn stop_vault_link(&self, store_id: StoreId) {
        if let Some(handle) = self.vault_links.write().await.remove(&store_id) {
            handle.stop();
        }
    }

    /// Mint a fresh JWT (for its `rpc_url`) and write `store_id`'s
    /// `sync.json` as a vault-mode link to it under `key_id`, stripped of
    /// any credential (`auth: none`, matching `crate::sync_link`'s own
    /// decision 4) — a vault link never trusts what's on disk for a
    /// credential, only the keystore. Used by `cloudHostStore` and
    /// `cloudAddHostedStore`, both of which then call
    /// `ensure_vault_link_started` with the returned url.
    async fn link_hosted_store(&self, store_id: StoreId, account: &crate::keystore::SignedInAccount, key_id: Uuid) -> Result<url::Url, ErrorObjectOwned> {
        let minted = crate::cloud::mint_token(&account.url, &account.session).await.map_err(to_rpc_error)?;
        let rpc_url: url::Url = minted
            .rpc_url
            .parse()
            .map_err(|e| to_rpc_error(format!("cloud service returned an invalid rpc_url {:?}: {}", minted.rpc_url, e)))?;

        let manager = self.store_manager.read().await;
        manager
            .write_sync_config(
                store_id,
                &SyncConfig {
                    remote: RemoteEndpoint { url: rpc_url.clone(), auth: AuthMethod::None },
                    last_sync: None,
                    mode: SyncMode::Vault,
                    last_seq: Default::default(),
                    vault_key_id: Some(key_id),
                },
            )
            .await
            .map_err(to_rpc_error)?;

        Ok(rpc_url)
    }

    /// Connect to `remote`: the credential used is `remote.auth` if it is
    /// not `AuthMethod::None`, else whatever was last saved for its origin
    /// (docs/history/HARDENING_CONTRACT.md decision 4). On a successful connection,
    /// if `remote.auth` itself was not `None` (an explicit credential, not
    /// one already reused from the saved store), it is saved for the
    /// origin — a connection that actually worked is the only signal a
    /// credential is any good. A failed connection is translated to
    /// decision 5's wording ("refused the credentials" for `401`, "refused
    /// the connection" for `403`) naming `remote.url`.
    async fn connect_to_remote(&self, remote: &RemoteEndpoint) -> Result<PimbleClient, ErrorObjectOwned> {
        let auth = self.credentials.resolve(&remote.url, &remote.auth).await;
        let client = PimbleClient::connect_with_auth(remote.url.as_str(), &auth)
            .await
            .map_err(|e| to_rpc_error(describe_connect_error(&remote.url, &e)))?;

        if !matches!(remote.auth, AuthMethod::None) {
            if let Err(e) = self.credentials.save(&remote.url, remote.auth.clone()).await {
                warn!("Failed to save credential for {}: {}", remote.url, e);
            }
        }

        Ok(client)
    }

    /// `remote` as it belongs on disk (`sync.json`) or in an RPC response:
    /// its credential stripped to `AuthMethod::None` (decision 4). The real
    /// credential, if any, lives only in the credentials store, keyed by
    /// origin, resolved fresh by [`Self::connect_to_remote`] every time it's
    /// needed.
    fn without_auth(remote: &RemoteEndpoint) -> RemoteEndpoint {
        RemoteEndpoint { url: remote.url.clone(), auth: AuthMethod::None }
    }

    // ── Remote mounts (docs/history/REMOTE_MOUNTS_CONTRACT.md) ───────────────

    /// Resolve `mount_node`'s source and report the mount's state
    /// (decisions 1 and 3). The single resolution path: `get_mount_state`,
    /// `get_children` on a mount and `create_mount` all come through here,
    /// so every one of them records the mount for later fan-out and every
    /// one of them can trigger a replica creation.
    ///
    /// Order: the three local steps (`StoreManager::ensure_store_open`:
    /// already open, a registry entry, the `source_path` hint), then the
    /// two remote ones — the mount ref's own `source_remote`, then the
    /// mounting store's remote, because a store and the stores it mounts
    /// usually live on the same server. A remote candidate means creating a
    /// replica, which happens in a detached task: this returns `Connecting`
    /// at once rather than making the caller's RPC wait on a round trip.
    ///
    /// Never holds the store-manager lock across a remote call: the only
    /// thing it does under that lock is the local resolution.
    async fn resolve_mount(&self, mounting_store: StoreId, mount_node: NodeId, mount_ref: &MountRef) -> MountState {
        let source = mount_ref.source_store;
        self.record_mount(source, mounting_store, mount_node);

        let (resolved_locally, newly_opened) = {
            let mut manager = self.store_manager.write().await;
            let resolved = manager.ensure_store_open(mount_ref).await.is_ok();
            (resolved, manager.opened_since())
        };
        // A source store just opened implicitly is an ordinary open store
        // (docs/history/MOUNTS_CONTRACT.md decision 4), search index included.
        self.adopt_newly_opened(newly_opened).await;

        if resolved_locally {
            return self.mount_state_of_open_source(source).await;
        }

        let candidates = self.remote_candidates_for(mounting_store, mount_ref).await;
        if candidates.is_empty() {
            return MountState::Unavailable {
                reason: Some(format!(
                    "source store {} is not on this server and no remote is known for it",
                    source
                )),
            };
        }

        // Decision 8: one creation per source however many mounts ask for
        // it at once, decided under the same lock that records it.
        {
            let mut mounts = self.mounts.lock().unwrap();
            if mounts.in_flight.contains(&source) {
                return MountState::Connecting;
            }
            mounts.in_flight.insert(source);
            mounts.failed.remove(&source);
        }

        let handler = self.clone();
        tokio::spawn(async move {
            handler.create_mount_source_replica(source, candidates).await;
        });

        MountState::Connecting
    }

    /// The remotes that might hold `mount_ref`'s source, in the order
    /// decision 1 tries them: the mount ref's `source_remote` (a URL only,
    /// never a credential), then the mounting store's own remote from its
    /// `sync.json`. Both are returned with `AuthMethod::None`, so
    /// [`Self::connect_to_remote`] resolves whatever credential this server
    /// has saved for that origin.
    async fn remote_candidates_for(&self, mounting_store: StoreId, mount_ref: &MountRef) -> Vec<RemoteEndpoint> {
        let mut candidates: Vec<RemoteEndpoint> = Vec::new();
        if let Some(url) = &mount_ref.source_remote {
            candidates.push(RemoteEndpoint { url: url.clone(), auth: AuthMethod::None });
        }

        let mounting_remote = {
            let manager = self.store_manager.read().await;
            manager.read_sync_config(mounting_store).await.ok().flatten()
        };
        if let Some(config) = mounting_remote {
            if !candidates.iter().any(|c| c.url == config.remote.url) {
                candidates.push(Self::without_auth(&config.remote));
            }
        }

        candidates
    }

    /// Create a replica of mount source `source` from the first of
    /// `candidates` that has it, then tell every mount of that source what
    /// happened (decision 5). Runs detached: whichever RPC triggered it has
    /// already answered `Connecting`.
    ///
    /// A candidate that cannot be reached, or that has no such store, is
    /// just the next one's turn; when none works the reason is kept so the
    /// notification says why, in the remote's own words ("... refused the
    /// credentials"). Nothing retries on a timer — the next resolution of
    /// the same mount tries again (decision 3's "the last creation attempt
    /// failed" is what a fan-out reports in the meantime).
    async fn create_mount_source_replica(&self, source: StoreId, candidates: Vec<RemoteEndpoint>) {
        let mut errors: Vec<String> = Vec::new();
        let mut created = false;

        for remote in &candidates {
            match self.create_replica_from(remote.clone(), source, None, false).await {
                Ok(_) => {
                    info!("Created a replica of mount source {} from {}", source, remote.url);
                    created = true;
                    break;
                }
                Err(e) => {
                    let message = e.message().to_string();
                    debug!("Mount source {} not available from {}: {}", source, remote.url, message);
                    errors.push(message);
                }
            }
        }

        {
            let mut mounts = self.mounts.lock().unwrap();
            mounts.in_flight.remove(&source);
            if !created {
                let reason = if errors.iter().all(|e| e.contains("has no open store")) {
                    format!("no remote has store {}", source)
                } else {
                    errors.join("; ")
                };
                warn!("Could not replicate mount source {}: {}", source, reason);
                mounts.failed.insert(source, reason);
            }
        }

        self.notify_mount_states_for_source(source).await;
    }

    /// The state of a mount whose source store is open here (decision 3):
    /// no link at all, or a link that is `Synced`, is `Live`; a link that
    /// is down or still reconciling is `Cached { last_sync }` once it has
    /// ever synced, and `Connecting` until then.
    async fn mount_state_of_open_source(&self, source: StoreId) -> MountState {
        let link = self.links.read().await.get(&source).map(|h| (h.state(), h.last_sync()));
        match link {
            None => MountState::Live,
            Some((SyncState::Synced { .. }, _)) => MountState::Live,
            Some((_, Some(last_sync))) => MountState::Cached { last_sync },
            Some((_, None)) => MountState::Connecting,
        }
    }

    /// The state every mount of `source` currently has, including the case
    /// the source is not open here: a creation in flight is `Connecting`,
    /// and anything else is `Unavailable` with the last failure's reason
    /// (decision 3). Used by the fan-out; a resolution uses
    /// [`Self::resolve_mount`], which also retries.
    async fn mount_state_for_source(&self, source: StoreId) -> MountState {
        if self.store_manager.read().await.is_open(source) {
            return self.mount_state_of_open_source(source).await;
        }
        let mounts = self.mounts.lock().unwrap();
        if mounts.in_flight.contains(&source) {
            return MountState::Connecting;
        }
        MountState::Unavailable { reason: mounts.failed.get(&source).cloned() }
    }

    /// Tell every mount sourced from `source` what its state is now
    /// (decision 5), on each mounting store's own `storeChanged`
    /// subscription with `source_client_id: None`. Called when the source's
    /// link changes category and when a background replica creation ends,
    /// successfully or not — a failure is a state a client can show, never
    /// something only the log knows.
    async fn notify_mount_states_for_source(&self, source: StoreId) {
        let mounts: Vec<(StoreId, NodeId)> = {
            let tracking = self.mounts.lock().unwrap();
            tracking.resolved.get(&source).map(|set| set.iter().copied().collect()).unwrap_or_default()
        };
        if mounts.is_empty() {
            return;
        }

        let state = self.mount_state_for_source(source).await;
        for (mounting_store, node_id) in mounts {
            self.notify_store_change(
                mounting_store,
                StoreChangeKind::MountStateChanged { node_id, state: state.clone() },
                None,
            )
            .await;
        }
    }

    /// Remember that `(mounting_store, mount_node)` is a mount of `source`,
    /// so a later change to that source's state reaches it (decision 5).
    fn record_mount(&self, source: StoreId, mounting_store: StoreId, mount_node: NodeId) {
        self.mounts.lock().unwrap().resolved.entry(source).or_default().insert((mounting_store, mount_node));
    }

    /// Drop the record of specific mount nodes in `store_id`, because they
    /// have been deleted. A stale entry is harmless to the server — it
    /// names a node no client has — but it keeps producing
    /// `MountStateChanged` notifications for a node the client has to
    /// recognise and discard, so the cheap thing is not to send them.
    fn forget_mounts(&self, store_id: StoreId, node_ids: &[NodeId]) {
        if node_ids.is_empty() {
            return;
        }
        let mut tracking = self.mounts.lock().unwrap();
        tracking.resolved.retain(|_, mounts| {
            mounts.retain(|(mounting_store, node_id)| *mounting_store != store_id || !node_ids.contains(node_id));
            !mounts.is_empty()
        });
    }

    /// Drop every mount `store_id` holds, because it is closing. Its
    /// entries as a *source* stay: the next resolution reopens or recreates
    /// it, which is what a local mount has always done.
    fn forget_mounting_store(&self, store_id: StoreId) {
        let mut tracking = self.mounts.lock().unwrap();
        tracking.resolved.retain(|_, mounts| {
            mounts.retain(|(mounting_store, _)| *mounting_store != store_id);
            !mounts.is_empty()
        });
    }

    /// Create a local replica of `store_id` as `remote` holds it, link it,
    /// and return the opened store (docs/SYNC_CONTRACT.md decision 8). The
    /// `addRemoteStore` RPC is this with `wait: true`; a mount resolving
    /// its source in the background is this with `wait: false`, because the
    /// link's own `Connecting` -> `Live` transitions are what tell that
    /// caller it finished.
    ///
    /// `path: None` puts the replica in this server's replicas directory,
    /// which is also what makes it removable with `removeReplica`.
    async fn create_replica_from(
        &self,
        remote: RemoteEndpoint,
        store_id: StoreId,
        path: Option<PathBuf>,
        wait: bool,
    ) -> Result<pimble_core::Store, ErrorObjectOwned> {
        let path = path.unwrap_or_else(|| self.default_replica_path(store_id));

        info!("Adding remote store {} from {} at {:?}", store_id, remote.url, path);

        // A store this server already holds cannot also be added as a
        // replica (the manager refuses too); this also covers pointing the
        // request at this very server.
        {
            let manager = self.store_manager.read().await;
            if manager.is_open(store_id) {
                let where_ = manager
                    .get_store_info(store_id)
                    .ok()
                    .and_then(|s| s.local_path().cloned())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                return Err(to_rpc_error(format!(
                    "store {} is already open locally at {}; use setStoreSync to link it",
                    store_id, where_
                )));
            }
        }

        // Ask the remote for the store (decision 8): its name and root node
        // id, matched by id via `listStores`.
        let remote_client = self.connect_to_remote(&remote).await?;
        let remote_stores = remote_client.list_stores().await.map_err(to_rpc_error)?;
        let remote_store = remote_stores
            .into_iter()
            .find(|s| s.id == store_id)
            .ok_or_else(|| to_rpc_error(format!("Remote {} has no open store {}", remote.url, store_id)))?;
        drop(remote_client);

        // An empty replica, never `StoreDocument::new` (decision 8: two
        // independently created roots for the same id would merge into
        // duplicated children).
        let mut manager = self.store_manager.write().await;
        let created_id = manager
            .create_replica(&path, remote_store.id, &remote_store.name, remote_store.root_node_id)
            .await
            .map_err(to_rpc_error)?;
        manager
            .write_sync_config(created_id, &SyncConfig { remote: Self::without_auth(&remote), last_sync: None, mode: pimble_store::SyncMode::Sync, last_seq: Default::default(), vault_key_id: None })
            .await
            .map_err(to_rpc_error)?;
        let mut store = manager.get_store_info(created_id).map_err(to_rpc_error)?;
        let newly_opened = manager.opened_since();
        drop(manager);

        self.adopt_newly_opened(newly_opened).await;

        self.ensure_link_started(created_id, remote).await;

        // Wait up to 10s for the first full reconcile to reach `Synced`
        // (decision 8), then answer anyway with the current state.
        if wait {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                let state = self.sync_state_of(created_id).await;
                let is_synced = matches!(state, SyncState::Synced { .. });
                if is_synced || std::time::Instant::now() >= deadline {
                    store.sync_state = state;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } else {
            store.sync_state = self.sync_state_of(created_id).await;
        }
        store.sync_mode = self.sync_mode_of(created_id).await;

        self.mark_replica(&mut store);
        Ok(store)
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

    /// Repair `store_id`'s tree (see `StoreDocument::repair`) if it needs it,
    /// then flush, broadcast, and re-index exactly like any other structural
    /// change (docs/history/HARDENING_CONTRACT.md decision 9). Called at `openStore`
    /// and after every `applyStoreUpdate` that changed the document. `None`
    /// (nothing to repair) is by far the common case, and costs one cheap
    /// read-only pass over the tree. Logs and returns on error rather than
    /// failing its caller's RPC — like search indexing, this is best-effort
    /// upkeep, not a precondition for the operation that triggered it.
    async fn repair_store_tree(&self, store_id: StoreId) {
        let repair = {
            let mut manager = self.store_manager.write().await;
            match manager.repair_tree(store_id) {
                Ok(repair) => repair,
                Err(e) => {
                    warn!("Tree repair failed for store {}: {}", store_id, e);
                    return;
                }
            }
        };
        let Some(repair) = repair else {
            return;
        };

        if let Err(e) = self.store_manager.write().await.flush(store_id).await {
            warn!("Failed to flush store {} after tree repair: {}", store_id, e);
        }
        info!("Repaired tree for store {}: {} node(s) touched", store_id, repair.touched.len());

        // `source_client_id: None`, like any change with no single originating
        // client, so a sync link forwards it rather than treating it as its
        // own echo.
        use base64::Engine;
        let notification = StoreChangedNotification {
            store_id,
            change_kind: StoreChangeKind::TreeStructure { node_ids: repair.touched.clone() },
            source_client_id: None,
            update: Some(base64::engine::general_purpose::STANDARD.encode(&repair.update)),
        };
        self.subscriptions.write().await.notify_store_change(&notification).await;

        // A repair only ever reassigns a node's parent or reorders/fixes a
        // children list, never removes a node entry — every touched id is
        // still there to upsert.
        for node_id in repair.touched {
            self.enqueue_index_event(store_id, IndexEvent::Upsert(node_id)).await;
        }
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
    /// thereby stopping) any previous one for that store. The previous
    /// handle, if any, is dropped without being shut down first — every
    /// caller of `install_index_handle` has already removed and shut down
    /// whatever was there (`open_index_for_store`'s own callers never race
    /// it, and `rebuild_store_index` calls `shutdown_index` explicitly).
    async fn install_index_handle(&self, store_id: StoreId, index: Arc<SearchIndex>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let debounce_tasks = Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new()));
        let indexer = Arc::new(StoreIndexer {
            store_id,
            index: Arc::clone(&index),
            store_manager: Arc::clone(&self.store_manager),
            plugin_host: Arc::clone(&self.plugin_host),
            content_gen: Mutex::new(HashMap::new()),
            debounce_tasks: Arc::clone(&debounce_tasks),
        });
        let main_task = tokio::spawn(indexer.run(rx));
        self.indexes.write().await.insert(store_id, IndexHandle { index, events: tx, main_task, debounce_tasks });
    }

    /// Shut an [`IndexHandle`] down completely: by the time this returns, no
    /// task anywhere holds its `Arc<SearchIndex>` (docs/history/HARDENING_CONTRACT.md
    /// decision 12) — the fix for the flaky
    /// `reopening_a_store_preserves_its_search_index`, whose real cause was
    /// this never having been guaranteed before: `closeStore` dropped the
    /// `IndexHandle`, which only *starts* `StoreIndexer::run` winding down
    /// (its channel closing) without waiting for that to finish, and never
    /// touched debounced upsert tasks at all — both `run` and any number of
    /// them could still be mid-flight, each holding its own clone of the
    /// `Arc<SearchIndex>`, when an immediate reopen tried to open the same
    /// rhypedb directory again.
    ///
    /// Order matters: dropping `events` first lets `run` drain whatever was
    /// already buffered (which can itself spawn fresh debounce tasks) and
    /// exit; only once `run` has actually finished can spawning of further
    /// debounce tasks be ruled out, which is what makes clearing
    /// `debounce_tasks` afterward exhaustive rather than racing new arrivals.
    /// Debounce tasks are aborted rather than awaited to their natural
    /// completion — there is no reason to sit out a content re-index's
    /// debounce window just because the store is closing.
    async fn shutdown_index(handle: IndexHandle) {
        drop(handle.events);
        let _ = handle.main_task.await;

        let mut tasks = handle.debounce_tasks.lock().await;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
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

        // Whether the open-failure fallback below ran: it always wipes the directory
        // clean, so the reopened index is empty regardless of what `old_hash` says —
        // comparing hashes afterward could otherwise conclude "no schema change" (the
        // wiped-and-reopened index typically has the very same schema) and skip
        // reindexing an index that is, in fact, now blank
        // (docs/history/HARDENING_CONTRACT.md decision 12).
        let mut forced_rebuild = false;

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
                forced_rebuild = true;
                SearchIndex::open(&index_dir, self.semantic_available)?
            }
        };

        let new_hash = std::fs::read_to_string(&hash_path).ok();
        let needs_rebuild = forced_rebuild || old_hash.is_none() || old_hash != new_hash;

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

    /// Give every store in `store_ids` what `openStore` gives one: a search
    /// index, its sync link if `sync.json` names a remote, and a tree repair.
    /// Call after any `StoreManager` operation that may have opened stores
    /// implicitly (resolving or validating a mount, creating a replica), with
    /// the list drained via `StoreManager::opened_since`, so an implicitly
    /// opened source store is an ordinary open store
    /// (docs/history/MOUNTS_CONTRACT.md decision 4). A replica resolved as a
    /// mount's source through its `source_path` after a restart depends on
    /// the link part: without it the replica sits `Offline` for good and the
    /// mount reads `Live` while nothing flows.
    async fn adopt_newly_opened(&self, store_ids: Vec<StoreId>) {
        for store_id in store_ids {
            if !self.indexes.read().await.contains_key(&store_id) {
                if let Err(e) = self.open_index_for_store(store_id).await {
                    warn!("Failed to open search index for newly opened store {}: {}", store_id, e);
                }
            }
            let sync_config = {
                let manager = self.store_manager.read().await;
                manager.read_sync_config(store_id).await.ok().flatten()
            };
            if let Some(config) = sync_config {
                self.ensure_link_started(store_id, config.remote).await;
            }
            self.repair_store_tree(store_id).await;
        }
    }

    /// Delete and rebuild `store_id`'s search index from scratch. Returns the
    /// number of nodes indexed.
    ///
    /// Deletes the directory and reopens fresh rather than calling
    /// `SearchIndex::clear()`, which cannot yet delete a `Node` another
    /// `Node.parent` still references (see `open_index_for_store`'s doc
    /// comment). Shuts down any existing handle first (`shutdown_index`) so
    /// no task anywhere still holds the old `Arc<SearchIndex>` — and so
    /// nothing can land a background upsert mid-delete — before the
    /// directory is removed (docs/history/HARDENING_CONTRACT.md decision 12).
    async fn rebuild_store_index(&self, store_id: StoreId) -> anyhow::Result<usize> {
        if let Some(handle) = self.indexes.write().await.remove(&store_id) {
            Self::shutdown_index(handle).await;
        }

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
    // ── Vault (encrypted store) API, docs/CRYPTO_CONTRACT.md ─────────────

    async fn vault_append(&self, ext: &Extensions, request: VaultAppendRequest) -> Result<VaultAppendResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        debug!("Vault append to store {} doc {:?}", request.store_id, request.doc_id);

        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let blob = URL_SAFE_NO_PAD
            .decode(&request.blob)
            .map_err(|e| to_rpc_error(format!("invalid base64url blob: {}", e)))?;

        let mut manager = self.store_manager.write().await;
        let seq = manager
            .vault_append(request.store_id, &request.doc_id.as_str(), blob)
            .await
            .map_err(|e| match e {
                pimble_store::StoreError::VaultSnapshotRequired => snapshot_required_error(format!(
                    "store {} document {:?}: log is at its size limit; upload a snapshot before appending more",
                    request.store_id, request.doc_id
                )),
                other => to_rpc_error(other),
            })?;
        drop(manager);

        // The blob rides the notification verbatim (still base64url) so a
        // live subscriber never re-fetches; there is no per-caller client id
        // in `VaultAppendRequest` to echo-suppress on, so `source_client_id`
        // is always `None` here.
        let notification = StoreChangedNotification {
            store_id: request.store_id,
            change_kind: StoreChangeKind::VaultAppended { doc_id: request.doc_id.clone(), seq },
            source_client_id: None,
            update: Some(request.blob.clone()),
        };
        self.subscriptions.write().await.notify_store_change(&notification).await;

        Ok(VaultAppendResponse { seq })
    }

    async fn vault_fetch(&self, ext: &Extensions, request: VaultFetchRequest) -> Result<VaultFetchResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Read)?;
        debug!("Vault fetch from store {} doc {:?} after {}", request.store_id, request.doc_id, request.after_seq);

        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let manager = self.store_manager.read().await;
        let (snapshot, updates, head) = manager
            .vault_fetch(request.store_id, &request.doc_id.as_str(), request.after_seq)
            .await
            .map_err(to_rpc_error)?;

        Ok(VaultFetchResponse {
            snapshot: snapshot.map(|(seq, blob)| VaultEntry { seq, blob: URL_SAFE_NO_PAD.encode(blob) }),
            updates: updates
                .into_iter()
                .map(|(seq, blob)| VaultEntry { seq, blob: URL_SAFE_NO_PAD.encode(blob) })
                .collect(),
            head,
        })
    }

    async fn vault_snapshot(&self, ext: &Extensions, request: VaultSnapshotRequest) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        info!("Vault snapshot for store {} doc {:?} upto {}", request.store_id, request.doc_id, request.upto_seq);

        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let blob = URL_SAFE_NO_PAD
            .decode(&request.blob)
            .map_err(|e| to_rpc_error(format!("invalid base64url blob: {}", e)))?;

        let mut manager = self.store_manager.write().await;
        manager
            .vault_snapshot(request.store_id, &request.doc_id.as_str(), request.upto_seq, blob)
            .await
            .map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn vault_list_docs(&self, ext: &Extensions, request: VaultListDocsRequest) -> Result<VaultListDocsResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Read)?;
        debug!("Vault list docs for store {}", request.store_id);

        let manager = self.store_manager.read().await;
        let docs = manager.vault_list_docs(request.store_id).map_err(to_rpc_error)?;

        Ok(VaultListDocsResponse {
            docs: docs
                .into_iter()
                .filter_map(|(doc_id, head, snapshot_seq)| {
                    VaultDocId::parse(&doc_id).map(|doc_id| VaultDocInfo { doc_id, head, snapshot_seq })
                })
                .collect(),
        })
    }

    // ── Cloud (Pimble Cloud account) API, docs/CRYPTO_CONTRACT.md ────────

    async fn cloud_sign_in(&self, ext: &Extensions, request: CloudSignInRequest) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudSignIn")?;
        info!("Signing in to Pimble Cloud at {} as {}", request.url, request.email);

        let kdf_params = crate::cloud::kdf(&request.url, &request.email).await.map_err(to_rpc_error)?;
        let password_keys = pimble_crypto::derive_password_keys(&request.password, &kdf_params).map_err(to_rpc_error)?;
        let auth_key = pimble_crypto::encode_auth_key(&password_keys.auth_key);

        let login = crate::cloud::login(&request.url, &request.email, &auth_key).await.map_err(to_rpc_error)?;
        let me_keys = crate::cloud::me_keys(&request.url, &login.session).await.map_err(to_rpc_error)?;
        let account_keys = pimble_crypto::unwrap_account_keys(&me_keys.account_key_blob, &password_keys.kek).map_err(to_rpc_error)?;

        self.keystore
            .sign_in(request.url.clone(), login.user.email.clone(), login.user.id.clone(), login.session.clone(), &account_keys)
            .await
            .map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn cloud_sign_out(&self, ext: &Extensions) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudSignOut")?;
        info!("Signing out of Pimble Cloud");
        self.keystore.sign_out().await.map_err(to_rpc_error)?;
        Ok(EmptyResponse {})
    }

    async fn cloud_status(&self, ext: &Extensions) -> Result<CloudStatusResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudStatus")?;
        match self.keystore.account().await {
            Some(account) => Ok(CloudStatusResponse { signed_in: true, email: Some(account.email), url: Some(account.url) }),
            None => Ok(CloudStatusResponse { signed_in: false, email: None, url: None }),
        }
    }

    async fn cloud_host_store(&self, ext: &Extensions, request: CloudHostStoreRequest) -> Result<CloudHostStoreResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudHostStore")?;
        let store_id = request.store_id;
        info!("Hosting store {} on Pimble Cloud", store_id);

        let account = self.keystore.account().await.ok_or_else(|| to_rpc_error("no Pimble Cloud account is signed in"))?;

        let name = {
            let manager = self.store_manager.read().await;
            manager.get_store_info(store_id).map_err(to_rpc_error)?.name
        };

        let store_view = crate::cloud::create_store(&account.url, &account.session, &name, "vault", Some(&store_id.to_string()))
            .await
            .map_err(to_rpc_error)?;
        if store_view.store_id != store_id.to_string() {
            return Err(to_rpc_error(format!(
                "cloud service created store {} instead of the requested {}",
                store_view.store_id, store_id
            )));
        }

        let key = pimble_crypto::SymmetricKey::generate();
        let key_id = Uuid::new_v4();
        let recipient = account.keys.public_keys();
        let envelope = pimble_crypto::wrap_key(&key, key_id, &recipient, &account.keys, &format!("store:{}", store_id)).map_err(to_rpc_error)?;
        crate::cloud::put_store_key(&account.url, &account.session, &store_id.to_string(), &account.user_id, key_id, &envelope)
            .await
            .map_err(to_rpc_error)?;
        self.keystore.add_store_key(store_id, key_id, &key).await.map_err(to_rpc_error)?;

        let rpc_url = self.link_hosted_store(store_id, &account, key_id).await?;
        self.ensure_vault_link_started(store_id, rpc_url, key_id).await;

        Ok(CloudHostStoreResponse { store_id })
    }

    async fn cloud_list_hosted_stores(&self, ext: &Extensions) -> Result<CloudListHostedStoresResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudListHostedStores")?;
        let account = self.keystore.account().await.ok_or_else(|| to_rpc_error("no Pimble Cloud account is signed in"))?;
        let stores = crate::cloud::list_stores(&account.url, &account.session).await.map_err(to_rpc_error)?;
        Ok(CloudListHostedStoresResponse {
            stores: stores
                .into_iter()
                .map(|s| CloudHostedStoreInfo { store_id: s.store_id, name: s.name, role: s.role, kind: s.kind, created_at: s.created_at })
                .collect(),
        })
    }

    async fn cloud_add_hosted_store(&self, ext: &Extensions, request: CloudAddHostedStoreRequest) -> Result<OpenStoreResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudAddHostedStore")?;
        let store_id = request.store_id;
        info!("Adding hosted store {} as a local replica", store_id);

        let account = self.keystore.account().await.ok_or_else(|| to_rpc_error("no Pimble Cloud account is signed in"))?;

        {
            let manager = self.store_manager.read().await;
            if manager.is_open(store_id) {
                return Err(to_rpc_error(format!("store {} is already open here", store_id)));
            }
        }

        let key_grants = crate::cloud::get_store_keys(&account.url, &account.session, &store_id.to_string()).await.map_err(to_rpc_error)?;
        let signer = account.keys.public_keys().signing;
        let mut key_id: Option<Uuid> = None;
        for grant in key_grants.envelopes {
            let id: Uuid = grant.key_id.parse().map_err(|e| to_rpc_error(format!("cloud service returned a bad key id: {}", e)))?;
            let key = pimble_crypto::unwrap_key(&grant.envelope, &account.keys, &signer).map_err(to_rpc_error)?;
            self.keystore.add_store_key(store_id, id, &key).await.map_err(to_rpc_error)?;
            key_id = Some(id);
        }
        let key_id = key_id.ok_or_else(|| to_rpc_error(format!("no key envelopes for store {} on this account", store_id)))?;

        let name = crate::cloud::list_stores(&account.url, &account.session)
            .await
            .map_err(to_rpc_error)?
            .into_iter()
            .find(|s| s.store_id == store_id.to_string())
            .map(|s| s.name)
            .unwrap_or_else(|| store_id.to_string());

        // An *empty* store document (never `create_local_store_with`, which
        // would give it its own freshly generated root): a vault store has
        // no plaintext root id to ask for ahead of time the way a `Plain`
        // remote's does for `addRemoteStore`, so this mirrors
        // `StoreManager::create_replica`'s own reasoning exactly — two
        // independently created roots for the same store id would merge
        // into a duplicated, disconnected tree once the vault link pulls
        // the real one. The placeholder root id here is manifest-only
        // bookkeeping, corrected below once the pull lands the real one.
        let path = self.default_replica_path(store_id);
        let mut manager = self.store_manager.write().await;
        let created_id = manager.create_replica(&path, store_id, &name, NodeId::new()).await.map_err(to_rpc_error)?;
        let mut store = manager.get_store_info(created_id).map_err(to_rpc_error)?;
        drop(manager);

        if !self.indexes.read().await.contains_key(&created_id) {
            if let Err(e) = self.open_index_for_store(created_id).await {
                warn!("Failed to open search index for store {}: {}", created_id, e);
            }
        }

        let rpc_url = self.link_hosted_store(created_id, &account, key_id).await?;
        self.ensure_vault_link_started(created_id, rpc_url, key_id).await;

        // Wait up to 10s for the first pull to land, same as
        // `addRemoteStore` (`create_replica_from`) does for a plain replica,
        // so the response's `root_node_id` reflects the real tree rather
        // than the placeholder above whenever the pull is fast enough.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let state = self.sync_state_of(created_id).await;
            let is_synced = matches!(state, SyncState::Synced { .. });
            if is_synced || std::time::Instant::now() >= deadline {
                store.sync_state = state;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Always `Vault`: `link_hosted_store` above just wrote `sync.json`
        // with `mode: "vault"`.
        store.sync_mode = StoreKind::Vault;

        // The manifest's `root_node_id` is never rewritten once the pull
        // merges the real tree in; read the live store document's own idea
        // of its root instead (docs/CRYPTO_CONTRACT.md: `StoreDocument`
        // tracks this itself, in its "meta" map, distinct from the
        // manifest).
        {
            let manager = self.store_manager.read().await;
            if let Ok(doc) = manager.store_document(created_id) {
                if let Ok(root) = doc.root_node_id() {
                    store.root_node_id = root;
                }
            }
        }

        self.mark_replica(&mut store);

        Ok(OpenStoreResponse { store })
    }

    async fn create_store(
        &self,
        ext: &Extensions,
        request: CreateStoreRequest,
    ) -> Result<CreateStoreResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "createStore")?;
        info!("Creating {:?} store '{}' at {:?}", request.kind, request.name, request.path);

        let mut manager = self.store_manager.write().await;
        let store_id = manager
            .create_local_store_with(&request.path, &request.name, request.kind, request.store_id)
            .await
            .map_err(to_rpc_error)?;

        // `root_node_id` is meaningless for a vault store (it has no tree of
        // its own here), but `Store`/`CreateStoreResponse` always carry one;
        // `get_store_info` works uniformly for either kind, unlike
        // `root_node_id`, which only knows about `Plain` stores.
        let root_node_id = manager
            .get_store_info(store_id)
            .map_err(to_rpc_error)?
            .root_node_id;

        drop(manager);

        // A vault store has no search index at all (docs/CRYPTO_CONTRACT.md).
        if request.kind == StoreKind::Plain && !self.indexes.read().await.contains_key(&store_id) {
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
        ext: &Extensions,
        request: OpenStoreRequest,
    ) -> Result<OpenStoreResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "openStore")?;
        info!("Opening store at {:?}", request.path);

        let mut manager = self.store_manager.write().await;
        let store_id = manager
            .open_local_store(&request.path)
            .await
            .map_err(to_rpc_error)?;

        let mut store = manager
            .get_store_info(store_id)
            .map_err(to_rpc_error)?;

        // A vault store (docs/CRYPTO_CONTRACT.md) has no `sync.json`, search
        // index, or tree to repair — `read_sync_config` in particular only
        // knows about `Plain` stores and would fail outright for one.
        if store.kind != StoreKind::Plain {
            drop(manager);
            store.sync_state = self.sync_state_of(store_id).await;
            self.mark_replica(&mut store);
            return Ok(OpenStoreResponse { store });
        }

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

        // Start the store's sync or vault link from `sync.json`, if present
        // (docs/SYNC_CONTRACT.md decision 7; docs/CRYPTO_CONTRACT.md for
        // `mode: "vault"`). Both `ensure_*_started` no-op if a link of that
        // kind is already running (e.g. `open_local_store` above was a
        // no-op for an already-open store).
        if let Some(config) = sync_config {
            match config.mode {
                SyncMode::Sync => self.ensure_link_started(store_id, config.remote).await,
                SyncMode::Vault => match config.vault_key_id {
                    Some(key_id) => self.ensure_vault_link_started(store_id, config.remote.url, key_id).await,
                    None => warn!(
                        "store {} sync.json has mode: vault but no vault_key_id; not starting a vault link",
                        store_id
                    ),
                },
            }
        }

        // Decision 9: repair a store's tree when it opens (a store closed
        // mid-repair, or reopened straight from disk after a crash, may
        // still be carrying an issue nothing has fixed yet).
        self.repair_store_tree(store_id).await;

        store.sync_state = self.sync_state_of(store_id).await;
        store.sync_mode = self.sync_mode_of(store_id).await;
        self.mark_replica(&mut store);

        Ok(OpenStoreResponse { store })
    }

    async fn close_store(
        &self,
        ext: &Extensions,
        request: CloseStoreRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "closeStore")?;
        info!("Closing store {}", request.store_id);

        self.stop_link(request.store_id).await;
        self.stop_vault_link(request.store_id).await;
        // Decision 5: a closing store's mounts are no longer this server's
        // to report on. Its entries as a mount *source* stay — the next
        // resolution reopens it.
        self.forget_mounting_store(request.store_id);

        let mut manager = self.store_manager.write().await;
        manager
            .close_store(request.store_id)
            .await
            .map_err(to_rpc_error)?;
        drop(manager);

        // Clean up subscriptions for this store
        self.subscriptions.write().await.remove_store(request.store_id);
        // `shutdown_index` (decision 12) doesn't return until no task anywhere
        // still holds this store's `Arc<SearchIndex>`, so a reopen right
        // after this response never meets a second live handle on the same
        // rhypedb directory.
        if let Some(handle) = self.indexes.write().await.remove(&request.store_id) {
            Self::shutdown_index(handle).await;
        }

        Ok(EmptyResponse {})
    }

    async fn list_stores(&self, ext: &Extensions) -> Result<ListStoresResponse, ErrorObjectOwned> {
        debug!("Listing stores");

        let manager = self.store_manager.read().await;
        let store_ids = readable(&principal_of(ext), manager.list_stores().iter());

        let mut stores = Vec::new();
        for id in store_ids {
            if let Ok(mut store) = manager.get_store_info(id) {
                store.sync_state = self.sync_state_of(id).await;
                store.sync_mode = self.sync_mode_of(id).await;
                self.mark_replica(&mut store);
                stores.push(store);
            }
        }

        Ok(ListStoresResponse { stores })
    }

    async fn get_node(
        &self,
        ext: &Extensions,
        request: GetNodeRequest,
    ) -> Result<GetNodeResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
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
        ext: &Extensions,
        request: GetNodesRequest,
    ) -> Result<GetNodesResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
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
        ext: &Extensions,
        request: CreateNodeRequest,
    ) -> Result<CreateNodeResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
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
        ext: &Extensions,
        request: UpdateNodeMetadataRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
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
        ext: &Extensions,
        request: UpdateNodeContentRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
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
        ext: &Extensions,
        request: DeleteNodeRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        info!(
            "Deleting node {} from store {}",
            request.node_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;
        let removal = manager
            .delete_node(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;

        // Unlike every other structural mutation here (createNode, moveNode,
        // updateNodeMetadata/Content all flush immediately below), this call
        // was missing its flush: the deletion only ever existed in the
        // in-memory `StoreDocument` until something else happened to flush
        // the store later (another edit, or a clean `closeStore`/shutdown).
        // A hard kill, or simply never touching the store again before the
        // app exits, lost the delete and the store.yrs on disk still had
        // the node (a mount node included) — which is exactly the bug where
        // a deleted mount reappears after restarting.
        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        // A deleted subtree may contain mount nodes; stop reporting their
        // source's state to a client that no longer has them.
        self.forget_mounts(request.store_id, &removal.removed);
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
        ext: &Extensions,
        request: MoveNodeRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
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
        ext: &Extensions,
        request: GetChildrenRequest,
    ) -> Result<GetChildrenResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
        debug!(
            "Getting children of node {} in store {}",
            request.node_id, request.store_id
        );

        // A mount node's children live in its source store, which may need
        // resolving first — opening it from disk, or replicating it from a
        // remote (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 1). Resolution
        // also records the mount, so a later change to the source's link
        // state reaches this client as `MountStateChanged`.
        let mount_ref = {
            let mut manager = self.store_manager.write().await;
            let node = manager
                .get_node(request.store_id, request.node_id)
                .await
                .map_err(to_rpc_error)?;
            node.mount_ref().filter(|_| node.is_mount())
        };

        if let Some(mount_ref) = mount_ref {
            // A mount's children are its source's; a principal that can't
            // read the source has no business seeing them just because it
            // can read the mounting store (docs/CLOUD_CONTRACT.md "B:
            // pimble-server" item 5).
            authorize(&principal, mount_ref.source_store, Access::Read)?;
            let state = self.resolve_mount(request.store_id, request.node_id, &mount_ref).await;
            if !self.store_manager.read().await.is_open(mount_ref.source_store) {
                // Decision 7: the error carries the state, because that is
                // what a client can act on — it refetches when a
                // `MountStateChanged` says the source is back.
                return Err(to_rpc_error(match state {
                    MountState::Unavailable { reason: Some(reason) } => {
                        format!("mount source unavailable: {}", reason)
                    }
                    MountState::Unavailable { reason: None } => "mount source unavailable".to_string(),
                    _ => "mount source is connecting".to_string(),
                }));
            }
        }

        let mut manager = self.store_manager.write().await;
        let (store_id, children) = manager
            .get_children(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;
        let newly_opened = manager.opened_since();
        drop(manager);

        // A mount's source store may have just been opened implicitly to
        // resolve it; give it a search index like any other open store.
        self.adopt_newly_opened(newly_opened).await;

        Ok(GetChildrenResponse { store_id, children })
    }

    async fn create_mount(
        &self,
        ext: &Extensions,
        request: CreateMountRequest,
    ) -> Result<CreateMountResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        // A mount hands its contents to whoever can see the mounting store;
        // a principal that can't even read the source has no business
        // mounting it in (docs/CLOUD_CONTRACT.md "B: pimble-server" item 5,
        // extended to `createMount`/`getMountState` uniformly with
        // `getChildren`).
        authorize(&principal, request.source_store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
        self.reject_if_vault(request.source_store_id).await?;
        info!(
            "Creating mount in store {} under parent {}, source: {}:{}",
            request.store_id, request.parent_id, request.source_store_id, request.source_node_id
        );

        let mut manager = self.store_manager.write().await;

        // Validate that this mount won't create a cycle. This also rejects
        // a mount-node parent transitively: `create_node` below is the
        // authoritative check, but validating first avoids opening/walking
        // stores for a request that's going to fail anyway. It opens the
        // source store, which is what makes reading the source's path and
        // remote below possible.
        let probe = MountRef {
            source_store: request.source_store_id,
            source_node: request.source_node_id,
            source_path: None,
            source_remote: None,
        };
        manager
            .validate_mount_creation(request.store_id, request.parent_id, &probe)
            .await
            .map_err(to_rpc_error)?;

        // Fill `source_path` from the registry's Local endpoint for the
        // source store, if it has one: this is what lets the mount resolve
        // after a restart even if the source isn't otherwise reopened (see
        // `StoreManager::ensure_store_open`).
        let source_path = match manager.registry().lookup(&request.source_store_id) {
            Some(StoreEndpoint::Local { path }) => Some(path.clone()),
            _ => None,
        };

        // Fill `source_remote` when the source store is itself a linked
        // replica here (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 2): the URL
        // only, never the credential, because this mount ref replicates
        // with its store to machines that must not hold the token.
        let source_remote = manager
            .read_sync_config(request.source_store_id)
            .await
            .ok()
            .flatten()
            .map(|config| config.remote.url);

        let mount_ref = MountRef {
            source_store: request.source_store_id,
            source_node: request.source_node_id,
            source_path,
            source_remote,
        };

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

        self.adopt_newly_opened(newly_opened).await;
        self.notify_store_change(request.store_id, StoreChangeKind::NodeCreated { node_id, parent_id: request.parent_id }, None).await;
        self.enqueue_index_event(request.store_id, IndexEvent::Upsert(node_id)).await;

        // Record the new mount so a later change to its source's link
        // state reaches this store's subscribers (decision 5). The source
        // is open by now (`validate_mount_creation` opened it), so this
        // resolves locally and starts nothing.
        let _ = self.resolve_mount(request.store_id, node_id, &mount_ref).await;

        Ok(CreateMountResponse { node_id, mount_ref })
    }

    // ── Replica sync (docs/SYNC_CONTRACT.md) ─────────────────────────

    async fn add_remote_store(
        &self,
        ext: &Extensions,
        request: AddRemoteStoreRequest,
    ) -> Result<OpenStoreResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "addRemoteStore")?;
        // `path: None` lets the server choose the replica's location; the
        // user never picks one (docs/SYNC_CONTRACT.md decision 8). `wait:
        // true`: this call answers only once the first full reconcile has
        // landed (or 10s have passed), so the store comes back populated.
        let store = self.create_replica_from(request.remote, request.remote_store_id, request.path, true).await?;
        Ok(OpenStoreResponse { store })
    }

    async fn set_store_sync(
        &self,
        ext: &Extensions,
        request: SetStoreSyncRequest,
    ) -> Result<GetStoreSyncResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "setStoreSync")?;
        info!("Setting sync for store {}: {:?}", request.store_id, request.remote.as_ref().map(|r| &r.url));

        match request.remote {
            Some(remote) => {
                // Refuse if the remote has no store with this id.
                let remote_client = self.connect_to_remote(&remote).await?;
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
                    .write_sync_config(request.store_id, &SyncConfig { remote: Self::without_auth(&remote), last_sync: None, mode: pimble_store::SyncMode::Sync, last_seq: Default::default(), vault_key_id: None })
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
        // Never `remote.auth` as saved (decision 4): sync.json is already
        // written with `auth: none`, but strip it here too so an older
        // sync.json written before that fix can't leak a credential.
        let remote_now = manager
            .read_sync_config(request.store_id)
            .await
            .map_err(to_rpc_error)?
            .map(|c| Self::without_auth(&c.remote));
        drop(manager);
        let state = self.sync_state_of(request.store_id).await;
        let sync_mode = self.sync_mode_of(request.store_id).await;

        Ok(GetStoreSyncResponse { remote: remote_now, state, sync_mode })
    }

    async fn get_store_sync(
        &self,
        ext: &Extensions,
        request: GetStoreSyncRequest,
    ) -> Result<GetStoreSyncResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Read)?;
        let manager = self.store_manager.read().await;
        let remote = manager
            .read_sync_config(request.store_id)
            .await
            .map_err(to_rpc_error)?
            .map(|c| Self::without_auth(&c.remote));
        drop(manager);
        let state = self.sync_state_of(request.store_id).await;
        let sync_mode = self.sync_mode_of(request.store_id).await;

        Ok(GetStoreSyncResponse { remote, state, sync_mode })
    }

    async fn list_remote_stores(
        &self,
        ext: &Extensions,
        request: ListRemoteStoresRequest,
    ) -> Result<ListStoresResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "listRemoteStores")?;
        debug!("Listing remote stores on {}", request.remote.url);

        let client = self.connect_to_remote(&request.remote).await?;
        let stores = client.list_stores().await.map_err(to_rpc_error)?;

        Ok(ListStoresResponse { stores })
    }

    async fn remove_replica(
        &self,
        ext: &Extensions,
        request: RemoveReplicaRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "removeReplica")?;
        info!("Removing replica {} (force: {})", request.store_id, request.force);

        let mut store = {
            let manager = self.store_manager.read().await;
            manager.get_store_info(request.store_id).map_err(to_rpc_error)?
        };
        self.mark_replica(&mut store);
        if !store.is_replica {
            return Err(to_rpc_error(format!(
                "store {} is not a replica (its directory is not under this server's replicas directory); \
                 removeReplica only removes a replica this server created with addRemoteStore",
                request.store_id
            )));
        }

        let state = self.sync_state_of(request.store_id).await;
        if !matches!(state, SyncState::Synced { .. }) && !request.force {
            return Err(to_rpc_error(format!(
                "replica {} is not fully synced (currently {:?}); it may have changes the remote does not have yet. \
                 Pass force to remove it anyway.",
                request.store_id, state
            )));
        }

        let path = store.local_path().cloned();

        // Closes exactly as `closeStore` does (it already stops the link
        // too), so `removeReplica` inherits whatever `closeStore` does to
        // shut its search index down cleanly before the directory under it
        // is deleted (decision 6). `removeReplica` is already `Service`-only
        // (checked above), so this internal call carries a `Service`
        // extension rather than forwarding the caller's — there is no HTTP
        // request behind it to have attached one in the first place.
        self.close_store(&service_extensions(), CloseStoreRequest { store_id: request.store_id }).await?;

        if let Some(path) = path {
            if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                // The store is already closed and unlinked at this point,
                // so this can't be silently swallowed into a success: the
                // caller (app or CLI) needs to know the directory is still
                // there (permissions, a file still open on it, ...).
                return Err(to_rpc_error(format!(
                    "replica {} was closed but its directory {} could not be deleted: {}",
                    request.store_id,
                    path.display(),
                    e
                )));
            }
        }

        Ok(EmptyResponse {})
    }

    async fn get_mount_state(
        &self,
        ext: &Extensions,
        request: GetMountStateRequest,
    ) -> Result<GetMountStateResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
        debug!(
            "Getting mount state for node {} in store {}",
            request.node_id, request.store_id
        );

        let node = {
            let mut manager = self.store_manager.write().await;
            manager
                .get_node(request.store_id, request.node_id)
                .await
                .map_err(to_rpc_error)?
        };

        let mount_ref = node.mount_ref().filter(|_| node.is_mount()).ok_or_else(|| {
            to_rpc_error(format!("Node {} is not a mount point", request.node_id))
        })?;
        // Read on the source too, uniformly with `getChildren`/`createMount`
        // (docs/CLOUD_CONTRACT.md "B: pimble-server" item 5, extended).
        authorize(&principal, mount_ref.source_store, Access::Read)?;

        // Attempts resolution rather than reading a cached answer
        // (docs/history/MOUNTS_CONTRACT.md decision 5), which for a source that is
        // not on this machine means starting its replica and answering
        // `Connecting` (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 1).
        let state = self.resolve_mount(request.store_id, request.node_id, &mount_ref).await;

        Ok(GetMountStateResponse { state, mount_ref })
    }

    async fn sync_store_document(
        &self,
        ext: &Extensions,
        request: SyncStoreDocumentRequest,
    ) -> Result<SyncStoreDocumentResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
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
        ext: &Extensions,
        request: ApplyStoreUpdateRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        info!(
            "Applying store update to store {} from client {}",
            request.store_id, request.client_id
        );

        use base64::Engine;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&request.update)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;

        let mut manager = self.store_manager.write().await;
        let effect = manager
            .apply_store_doc_update(request.store_id, &bytes)
            .map_err(to_rpc_error)?;

        if !effect.changed {
            // Decision 8: every part of this update was already reflected
            // here (a yrs diff is never actually empty, so this can't be
            // told apart by the request's byte length) — no flush, no
            // notification, no re-index, no repair.
            return Ok(EmptyResponse {});
        }

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        // Decision 9 (of docs/SYNC_CONTRACT.md): an id whose node entry
        // still exists gets upserted (this is what makes a title change
        // arriving as a store update get reindexed); an id no longer
        // present was removed.
        let (upserts, removals): (Vec<NodeId>, Vec<NodeId>) = {
            let doc = manager.store_document(request.store_id).map_err(to_rpc_error)?;
            effect.touched.iter().copied().partition(|id| doc.has_node(*id))
        };

        drop(manager);

        // Broadcast to other subscribers, carrying the raw update bytes so
        // they can apply it directly instead of refetching.
        let notification = StoreChangedNotification {
            store_id: request.store_id,
            change_kind: StoreChangeKind::TreeStructure { node_ids: effect.touched },
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

        // Decision 9 (of docs/history/HARDENING_CONTRACT.md): repair after a
        // changing applyStoreUpdate.
        self.repair_store_tree(request.store_id).await;

        Ok(EmptyResponse {})
    }

    async fn sync_node_contents(
        &self,
        ext: &Extensions,
        request: SyncNodeContentsRequest,
    ) -> Result<SyncNodeContentsResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
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
        ext: &Extensions,
        request: ApplyEditRequest,
    ) -> Result<ApplyEditResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        use base64::Engine;

        // Apply the edit to the server's persistent yrs document. The
        // full-snapshot path is `updateNodeContent`, not an `EditOperation`.
        let EditOperation::IncrementalChanges { ref changes } = request.operation;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(changes)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;
        let mut store_manager = self.store_manager.write().await;
        let changed = store_manager
            .apply_content_update(request.store_id, request.node_id, &bytes)
            .await
            .map_err(to_rpc_error)?;
        drop(store_manager);

        if !changed {
            // Decision 8: every part of this edit was already reflected here
            // — no flush, no broadcast, no re-index.
            return Ok(ApplyEditResponse {});
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

        // Content upserts are debounced per node (2s) here, independent of
        // the disk-flush debounce above: `applyEdit` fires per keystroke.
        self.enqueue_index_event(request.store_id, IndexEvent::ContentChanged(request.node_id)).await;

        Ok(ApplyEditResponse {})
    }

    async fn subscribe_store_changes(
        &self,
        pending: PendingSubscriptionSink,
        ext: &Extensions,
        store_id: StoreId,
    ) -> SubscriptionResult {
        if let Err(e) = authorize(&principal_of(ext), store_id, Access::Read) {
            pending.reject(e).await;
            return Ok(());
        }
        // Unlike every other store-scoped RPC, `subscribeStoreChanges` works
        // on a vault store: it's how a live client hears `VaultAppended`
        // without re-fetching (docs/CRYPTO_CONTRACT.md).
        info!("Client subscribing to store changes for {}", store_id);

        let sink = pending.accept().await?;
        self.subscriptions.write().await.add_store_sub(store_id, sink);

        Ok(())
    }

    async fn subscribe_node_changes(
        &self,
        pending: PendingSubscriptionSink,
        ext: &Extensions,
        store_id: StoreId,
        node_id: NodeId,
    ) -> SubscriptionResult {
        if let Err(e) = authorize(&principal_of(ext), store_id, Access::Read) {
            pending.reject(e).await;
            return Ok(());
        }
        if let Err(e) = self.reject_if_vault(store_id).await {
            pending.reject(e).await;
            return Ok(());
        }
        info!("Client subscribing to node changes for {}:{}", store_id, node_id);

        let sink = pending.accept().await?;
        self.subscriptions.write().await.add_node_sub(store_id, node_id, sink);

        Ok(())
    }

    async fn search(
        &self,
        ext: &Extensions,
        request: SearchRequest,
    ) -> Result<SearchResponse, ErrorObjectOwned> {
        debug!("Searching for '{}'", request.query);
        let principal = principal_of(ext);

        let limit = request.limit.max(1);
        let query = SearchQuery {
            text: request.query.clone(),
            semantic: request.semantic,
            limit,
        };

        // Empty request.stores means every open store the principal may
        // read (not literally every open store: a user principal must never
        // learn of a hit in a store it has no grant on). An explicit list is
        // still filtered the same way, so naming an unauthorized store id
        // silently searches nothing there instead of leaking whether it
        // exists.
        let store_ids: Vec<StoreId> = if request.stores.is_empty() {
            let open: Vec<StoreId> = self.indexes.read().await.keys().copied().collect();
            readable(&principal, open.iter())
        } else {
            readable(&principal, request.stores.iter())
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
        ext: &Extensions,
        request: RebuildIndexRequest,
    ) -> Result<RebuildIndexResponse, ErrorObjectOwned> {
        authorize(&principal_of(ext), request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        info!("Rebuilding search index for store {}", request.store_id);

        let indexed = self
            .rebuild_store_index(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        Ok(RebuildIndexResponse { indexed })
    }
}
