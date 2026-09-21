//! RPC method handlers.
//!
//! A store is its node documents (docs/NODE_DOCUMENT_CONTRACT.md): one yrs
//! document per node holding its text, its place in the tree and its
//! metadata, read through `pimble_store::StoreManager`'s `Tree`. Every write
//! reaches a document by one of two paths, and both end the same way:
//!
//! - A tree RPC (`createNode`, `moveNode`, `deleteNode`, `undeleteNode`,
//!   `updateNodeMetadata`, `createMount`) asks the store for the operation
//!   and gets a `TreeEdit` back: per document it touched, the update its
//!   transaction produced. The handler flushes, then broadcasts one
//!   notification per touched document carrying that document's bytes: the
//!   RPC's own kind for the node it acted on, `TreeStructure { [id] }` for
//!   every other document (a parent's list, a descendant's tombstone).
//! - `applyEdit` (a client's keystroke, a sync or vault link relaying a
//!   peer's update) merges any update into a document through
//!   [`RpcHandler::apply_node_update_from`], which derives the kinds from
//!   what the merge changed (see [`derive_kinds`]) and broadcasts them with
//!   the same bytes. A merge that changed nothing does nothing at all
//!   (docs/history/HARDENING_CONTRACT.md decision 8).
//!
//! Who may reach what (docs/NODE_DOCUMENT_CONTRACT.md section 5): `authorize`
//! judges the store, and for a share's member (a grant with a role per
//! shared root) every RPC that names a document or a node judges that too,
//! against the scopes of the member's roots ([`Reach`]): a vault store's
//! published scope sets, a plain store's own tree. Outside every scope is
//! "no grant for this document", whether or not the document exists; in a
//! scope the member only reads, a write is the reader's one sentence. Lists
//! (`getChildren`, `getNodes`, `syncNodes`, `vaultListDocs`, `search`) leave
//! out what is not the member's, and a notification reaches a scoped
//! subscriber only when its document is in scope as it is sent
//! ([`Delivery`]), so a member never learns another document's id. The same
//! sentence refuses a write on a replica this device holds as a reader
//! (`reject_if_read_only`).
//!
//! So a subscriber never needs to refetch to stay in step: a sync link
//! forwards the bytes of every document notification as an `applyEdit`, and
//! an editor merges them. The tree is settled by `Tree::repair` after a
//! merged update touched structure, debounced so one peer's edit, which is
//! several documents' updates, is applied whole before repair judges it
//! (see [`Repair`]).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use jsonrpsee::core::{async_trait, SubscriptionResult};
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::{Extensions, PendingSubscriptionSink, SubscriptionMessage};
use pimble_client::{describe_connect_error, PimbleClient};
use pimble_core::{AuthMethod, Node, MountRef, MountState, NodeId, RelaySide, RemoteEndpoint, StoreAccess, StoreId, StoreKind, StoreLocation, SyncState, Workspace};
use pimble_crdt::{NodeDoc, NodeFields, NodeUpdateEffect, Tree, TreeEdit};
use pimble_plugins::PluginHost;
use pimble_rpc::{
    encrypted_store_error, index_building_error, snapshot_required_error, to_rpc_error, ApplyEditRequest, ApplyEditResponse,
    AddRemoteStoreRequest, CloseStoreRequest, CloudAddHostedStoreRequest, CloudHostStoreRequest,
    CloudHostStoreResponse, CloudHostedStoreInfo, CloudListHostedStoresResponse, CloudRelayStoreRequest, CloudRelayStoreResponse, CloudSignInRequest,
    CloudStatusResponse, CloudStopRelayingRequest,
    CreateMountRequest, CreateMountResponse,
    CreateNodeRequest, CreateNodeResponse, CreateStoreRequest, CreateStoreResponse,
    CreateWorkspaceRequest, DeleteNodeRequest, EditOperation, EmptyResponse, GetChildrenRequest,
    GetChildrenResponse, GetMountStateRequest, GetMountStateResponse, GetNodeRequest, GetStoreSyncRequest, GetStoreSyncResponse, SetStoreSyncRequest,
    GetNodeResponse, GetNodesRequest, GetNodesResponse, ListRemoteStoresRequest, ListStoresResponse, LoadWorkspaceRequest,
    LoadWorkspaceResponse, MoveNodeRequest, NodeContentChangedNotification, NodeContentDiff, OpenStoreRequest,
    OpenStoreResponse, PimbleApiServer, RebuildIndexRequest, RebuildIndexResponse, RemoveReplicaRequest,
    SaveWorkspaceRequest, SearchRequest, SearchResponse, SearchResultItem, StoreChangeKind,
    CloudShareInfoResponse, CloudShareInviteRequest, CloudShareNodeRequest, CloudShareRef, CloudShareRemoveMemberRequest, DeleteVaultStoreRequest,
    GetScopesRequest, GetScopesResponse, Scope, SetScopeRequest, StoreChangedNotification, SyncNodesRequest, SyncNodesResponse, UndeleteNodeRequest,
    UpdateNodeContentRequest, VaultDocKeys, VaultSetDocKeysRequest,
    UpdateNodeMetadataRequest,
    VaultAppendRequest, VaultAppendResponse, VaultDocId, VaultDocInfo, VaultEntry, VaultFetchRequest,
    VaultFetchResponse, VaultListDocsRequest, VaultListDocsResponse, VaultSnapshotRequest,
    MAX_SYNC_NODE_CONTENTS,
};
use pimble_search::{IndexNode, SearchError, SearchIndex, SearchQuery};
use pimble_store::{StoreEndpoint, StoreError, StoreManager, SyncConfig, SyncMode};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::keystore::Keystore;
use crate::principal::{
    authorize, authorize_owner, authorize_service_only, no_grant_for_document_error, principal_of, read_only_error, readable, scope_roots_of, service_extensions, Access,
    Principal,
};
use crate::sync_link::{SyncLink, SyncLinkHandle};
use crate::vault_link::{LinkEndpoint, VaultLink, VaultLinkHandle};

/// How long to wait, after a node's content last changed, before reading its
/// units and upserting them into the search index. `applyEdit` fires on every
/// keystroke; this coalesces a burst of edits into one re-index per node,
/// independent of the (unrelated) content-flush debounce above.
const CONTENT_INDEX_DEBOUNCE: Duration = Duration::from_millis(2_000);

/// How long to wait after a content edit before flushing it to disk. A burst
/// of keystrokes coalesces into at most one flush per window, instead of one
/// per edit. The `modified_at` stamp a person's content edit earns rides the
/// same window (see [`FlushDebouncer`]).
const CONTENT_FLUSH_DEBOUNCE: Duration = Duration::from_millis(750);

/// How many times `limit` a store's index is asked for when the caller is
/// scoped in it (`search` drops the hits outside the scope afterwards).
const SCOPED_SEARCH_OVERFETCH: usize = 10;

/// How long a merged structural update waits for the next one before the
/// tree is repaired (see [`Repair::Debounced`]).
const REPAIR_DEBOUNCE: Duration = Duration::from_millis(250);

/// The timestamp the server's own edits carry (a `modified_at` stamp),
/// rfc3339 like the store's.
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Whether an `applyEdit`'s `client_id` is a link of another server
/// relaying a peer's edit (`crate::sync_link`, `crate::vault_link`) rather
/// than a person editing here. The id is the one thing a relayed edit
/// carries that says so.
fn is_link_client(client_id: &str) -> bool {
    client_id.starts_with("sync-link:") || client_id.starts_with("vault-link:")
}

/// The documents a principal scoped to `roots` reaches in `store_id`
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5): a vault store's published
/// scope sets (the server holds no tree of it, so the owner's devices say
/// what is under each root, and each root is in its own scope); a plain
/// store's own tree under each root, computed and stored nowhere. A store
/// that is not open reaches nothing, and the RPC fails on that afterwards.
fn scope_set(manager: &StoreManager, store_id: StoreId, roots: &[NodeId]) -> HashSet<NodeId> {
    match manager.store_kind(store_id) {
        Some(StoreKind::Vault) => manager.vault_scope_union(store_id, roots).unwrap_or_default(),
        Some(StoreKind::Plain) => manager.tree(store_id).map(|tree| plain_scope(tree, roots)).unwrap_or_default(),
        None => HashSet::new(),
    }
}

/// A plain store's scope for `roots`: every root's subtree, plus every held
/// document whose stored parent chain leads into it. The second half is
/// what the subtree walk leaves out and a member must still reach: a
/// tombstone (a scoped member's delete is one, and the document stays theirs
/// to undelete, the same as a vault store's set keeps a deleted document's
/// id), and a document just created whose parent's list does not name it
/// yet (a create is two documents' updates, and `parent_id` is the
/// authoritative half).
pub(crate) fn plain_scope(tree: &Tree, roots: &[NodeId]) -> HashSet<NodeId> {
    let mut scope: HashSet<NodeId> = HashSet::new();
    for root in roots {
        if let Ok(ids) = tree.subtree_ids(*root) {
            scope.extend(ids);
        } else if tree.doc(*root).is_some() {
            // A tombstoned root is still the member's document.
            scope.insert(*root);
        }
    }
    let mut outside: Vec<(NodeId, NodeId)> = tree
        .ids()
        .into_iter()
        .filter(|id| !scope.contains(id))
        .filter_map(|id| Some((id, tree.doc(id)?.fields().ok()?.parent_id?)))
        .collect();
    // A deleted folder's deleted children point at the folder, so admit
    // until a pass admits none.
    loop {
        let before = scope.len();
        outside.retain(|(id, parent)| {
            if scope.contains(parent) {
                scope.insert(*id);
                false
            } else {
                true
            }
        });
        if scope.len() == before {
            break;
        }
    }
    scope
}

/// What a scoped principal reaches in one store: the documents it may read
/// (every root's scope) and, among them, the ones it may write (the scopes
/// of the roots it edits; a document in a read scope and a write scope
/// takes the wider role, docs/NODE_DOCUMENT_CONTRACT.md section 5).
struct Reach {
    readable: HashSet<NodeId>,
    writable: HashSet<NodeId>,
}

impl Reach {
    fn of(manager: &StoreManager, principal: &Principal, store_id: StoreId) -> Option<Self> {
        let read_roots = scope_roots_of(principal, store_id, Access::Read)?;
        let write_roots = scope_roots_of(principal, store_id, Access::Write).unwrap_or_default();
        let readable = scope_set(manager, store_id, &read_roots);
        // The common case, one role for every root, computes one set.
        let writable = if write_roots.len() == read_roots.len() {
            readable.clone()
        } else if write_roots.is_empty() {
            HashSet::new()
        } else {
            scope_set(manager, store_id, &write_roots)
        };
        Some(Self { readable, writable })
    }

    /// A document outside every scope is refused without saying whether it
    /// exists; one the member may read and not write, as a reader is.
    fn require(&self, id: NodeId, needed: Access) -> Result<(), ErrorObjectOwned> {
        if !self.readable.contains(&id) {
            return Err(no_grant_for_document_error());
        }
        match needed {
            Access::Write if !self.writable.contains(&id) => Err(read_only_error()),
            _ => Ok(()),
        }
    }
}

/// `Ok` when `reach` is `None` (the principal reaches the whole store, and
/// `authorize` has judged its role) or covers `id` for `needed`.
fn require_in_scope(reach: &Option<Reach>, id: NodeId, needed: Access) -> Result<(), ErrorObjectOwned> {
    match reach {
        Some(reach) => reach.require(id, needed),
        None => Ok(()),
    }
}

/// What `principal` may change of node `id` in plain store `store_id`: the
/// judgement every `Node` an RPC returns carries as [`Node::access`]
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"), so a client knows
/// before it offers an edit what a write of this node would be answered
/// with. It reports, and decides nothing: a write is judged again where it
/// lands. Both of the things that refuse one are asked, in the order a write
/// RPC asks them: the caller's role (`authorize` for a whole-store grant,
/// `reach` for a share's member, whose document under a reader's root nested
/// in an editor's takes the wider role) and how this device holds the store
/// (`write_refused`: a reader's replica, or a root held to read among roots
/// that are edited). `Read` if either refuses, else `Full`. `reach` is the
/// caller's in `store_id`, computed once for a list.
fn judge_access(manager: &StoreManager, principal: &Principal, reach: &Option<Reach>, store_id: StoreId, id: NodeId) -> StoreAccess {
    let role_allows = authorize(principal, store_id, Access::Write).is_ok() && require_in_scope(reach, id, Access::Write).is_ok();
    if role_allows && !manager.write_refused(store_id, &[id]) {
        StoreAccess::Full
    } else {
        StoreAccess::Read
    }
}

/// The parent a node document's update names, for admitting a scoped
/// member's `applyEdit` of a document the store does not have yet: the
/// update is read into a scratch document and its `node.parent_id` is the
/// parent the request names (the `createNode` path names it in the request
/// itself). `None` when the update does not initialise the node.
fn parent_named_by(update: &[u8]) -> Option<NodeId> {
    let mut scratch = NodeDoc::new();
    scratch.apply_update(update).ok()?;
    scratch.fields().ok()?.parent_id
}

/// Where the twins of relayed stores live unless the configuration says
/// otherwise (docs/RELAY_CONTRACT.md): `relay`, beside the replicas
/// directory, so `<data dir>/pimble/relay` by default and inside a test's
/// temp directory whenever its replicas are.
fn relay_dir_beside(replicas_dir: &std::path::Path) -> PathBuf {
    replicas_dir.parent().map(|parent| parent.join("relay")).unwrap_or_else(|| replicas_dir.join("relay"))
}

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
///
/// The same task stamps `modified_at` on every node a person edited in the
/// window, once each, just before it flushes: a content merge never stamps
/// anything in the store (a link relaying a peer's edit must not), so the
/// stamp is a tree edit of the server's own, and doing it here rather than
/// in `apply_edit` makes it one stamp and one broadcast per node per window
/// instead of one per keystroke.
#[derive(Default)]
struct FlushDebouncer {
    pending: Mutex<FlushPending>,
    /// Whether a flush task is currently sleeping/running.
    scheduled: Mutex<bool>,
}

#[derive(Default)]
struct FlushPending {
    /// Stores with content dirty since the last flush.
    stores: HashSet<StoreId>,
    /// Nodes a person edited since the last flush, to stamp `modified_at`.
    stamps: HashSet<(StoreId, NodeId)>,
}

/// Per-store generations for [`RpcHandler::schedule_repair`], the same
/// pattern as `StoreIndexer::content_gen`: a repair task fires only if no
/// newer structural update has arrived while it slept.
#[derive(Default)]
struct RepairDebouncer {
    generation: Mutex<HashMap<StoreId, u64>>,
}

/// When the tree is repaired after a merged update touched `node` or
/// `children` (docs/NODE_DOCUMENT_CONTRACT.md section 2). One peer's tree
/// edit is several documents' updates, and they arrive one at a time: a
/// repair run between two of them judges a half-applied edit. The bad case
/// is a deletion, where a descendant whose ancestor's tombstone has arrived
/// but whose own has not is an orphan, and decision 9's answer to an orphan
/// is a new parent written into its document, an edit that then propagates.
/// So no repair runs per update; it runs once the updates have stopped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repair {
    /// Repair once no structural update has landed for [`REPAIR_DEBOUNCE`]:
    /// the live path, where the few updates of one edit arrive within
    /// microseconds of each other.
    Debounced,
    /// Repair nothing now; the caller runs `repair_store_tree` itself once
    /// its whole batch is applied (a reconcile: possibly thousands of
    /// documents, with round trips between batches that no debounce window
    /// should be trusted to cover).
    Later,
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
            let manager = self.store_manager.read().await;
            match manager.get_node(self.store_id, node_id) {
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

/// Project a [`Node`] into the search index's [`IndexNode`]: its title and
/// metadata, and its content's [`pimble_core::IndexUnit`]s from the node
/// type's plugin (`NodeDoc::units()` for `document` nodes, via
/// `DocumentPlugin`; `node.content` is the node document's bytes, title and
/// text alike). A node type with no registered plugin (e.g. `mount`) indexes
/// with no units/text.
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

// ── Deriving notifications from a merged update ────────────────────────

/// What a node document says about the node before or after a merge: the
/// `node` root as fields (`None` while the document is not initialised) and
/// the children list as stored.
struct DocShape {
    fields: Option<NodeFields>,
    children: Vec<NodeId>,
}

/// The shape of `id`'s document, `None` when the store holds none.
fn shape_of(tree: &Tree, id: NodeId) -> Option<DocShape> {
    tree.doc(id).map(|doc| DocShape { fields: doc.fields().ok(), children: doc.children() })
}

/// The notifications a merged update earns, from what it changed in the
/// document (docs/NODE_DOCUMENT_CONTRACT.md section 2): the `node` root
/// newly written is `NodeCreated` (or `NodeDeleted` when it arrived as a
/// tombstone, or `TreeStructure` for a root, which has no parent to name);
/// a tombstone set is `NodeDeleted`, cleared is `NodeCreated` again (the
/// node is back under its parent); a changed `parent_id` is `NodeMoved`;
/// any other change to the `node` root is `MetadataUpdated`; a changed
/// children list is `TreeStructure { [node] }`; a changed `content` root,
/// or the plugin's `data` root, is `ContentUpdated`. One update can earn
/// several; a structural change nothing above names (a rewrite of the same
/// values) still earns `TreeStructure`, so that its bytes travel down a
/// chain of links. `root` stands in for a parent no document names.
fn derive_kinds(node_id: NodeId, root: NodeId, before: Option<&DocShape>, after: Option<&DocShape>, effect: NodeUpdateEffect) -> Vec<StoreChangeKind> {
    let mut kinds = Vec::new();
    if effect.structure {
        let before_fields = before.and_then(|s| s.fields.as_ref());
        let after_fields = after.and_then(|s| s.fields.as_ref());
        let node_kind = match (before_fields, after_fields) {
            (None, Some(a)) => Some(match (a.deleted_at.is_some(), a.parent_id) {
                (true, parent) => StoreChangeKind::NodeDeleted { node_id, parent_id: parent.unwrap_or(root) },
                (false, Some(parent_id)) => StoreChangeKind::NodeCreated { node_id, parent_id },
                (false, None) => StoreChangeKind::TreeStructure { node_ids: vec![node_id] },
            }),
            (Some(b), Some(a)) => {
                if b.deleted_at.is_none() && a.deleted_at.is_some() {
                    Some(StoreChangeKind::NodeDeleted { node_id, parent_id: b.parent_id.or(a.parent_id).unwrap_or(root) })
                } else if b.deleted_at.is_some() && a.deleted_at.is_none() {
                    Some(StoreChangeKind::NodeCreated { node_id, parent_id: a.parent_id.unwrap_or(root) })
                } else if b.parent_id != a.parent_id {
                    Some(StoreChangeKind::NodeMoved {
                        node_id,
                        old_parent_id: b.parent_id.unwrap_or(root),
                        new_parent_id: a.parent_id.unwrap_or(root),
                    })
                } else if b != a {
                    Some(StoreChangeKind::MetadataUpdated { node_id })
                } else {
                    None
                }
            }
            // An initialised `node` root cannot become uninitialised, and
            // a change that touched neither side of it is a list change.
            (Some(_), None) | (None, None) => None,
        };
        let named_the_list = matches!(node_kind, Some(StoreChangeKind::TreeStructure { .. }));
        kinds.extend(node_kind);
        let before_children = before.map(|s| s.children.as_slice()).unwrap_or(&[]);
        let after_children = after.map(|s| s.children.as_slice()).unwrap_or(&[]);
        if (before_children != after_children || kinds.is_empty()) && !named_the_list {
            kinds.push(StoreChangeKind::TreeStructure { node_ids: vec![node_id] });
        }
    }
    if effect.content || effect.data {
        kinds.push(StoreChangeKind::ContentUpdated { node_id });
    }
    if kinds.is_empty() {
        kinds.push(StoreChangeKind::TreeStructure { node_ids: vec![node_id] });
    }
    kinds
}

/// What the index does about a derived kind. A tombstone is removed; a
/// content change goes through the per-node debounce; everything else is an
/// upsert, which skips a node that is not there to index.
fn index_events_for(kind: &StoreChangeKind) -> Vec<IndexEvent> {
    match kind {
        StoreChangeKind::NodeCreated { node_id, .. }
        | StoreChangeKind::NodeMoved { node_id, .. }
        | StoreChangeKind::MetadataUpdated { node_id } => vec![IndexEvent::Upsert(*node_id)],
        StoreChangeKind::NodeDeleted { node_id, .. } => vec![IndexEvent::Remove(*node_id)],
        StoreChangeKind::ContentUpdated { node_id } => vec![IndexEvent::ContentChanged(*node_id)],
        StoreChangeKind::TreeStructure { node_ids } => node_ids.iter().map(|id| IndexEvent::Upsert(*id)).collect(),
        StoreChangeKind::SyncStateChanged { .. }
        | StoreChangeKind::MountStateChanged { .. }
        | StoreChangeKind::ShareStateChanged { .. }
        | StoreChangeKind::VaultAppended { .. } => Vec::new(),
    }
}

/// A [`TreeEdit`] as one update per document, in the order the documents
/// were first touched: a document the edit wrote in several transactions
/// gets them merged (see [`merge_updates`]); when that fails, which no
/// update this server made can cause, each is sent on its own.
fn coalesce_edit(edit: &TreeEdit) -> Vec<(NodeId, Vec<u8>)> {
    let mut order: Vec<NodeId> = Vec::new();
    let mut per_doc: HashMap<NodeId, Vec<&[u8]>> = HashMap::new();
    for (id, update) in &edit.touched {
        per_doc.entry(*id).or_insert_with(|| {
            order.push(*id);
            Vec::new()
        }).push(update.as_slice());
    }
    let mut out = Vec::with_capacity(order.len());
    for id in order {
        let updates = per_doc.remove(&id).unwrap_or_default();
        if updates.len() == 1 {
            out.push((id, updates[0].to_vec()));
            continue;
        }
        match merge_updates(&updates) {
            Ok(merged) => out.push((id, merged)),
            Err(e) => {
                warn!("Could not merge {} updates to node {}: {}; sending them one by one", updates.len(), id, e);
                out.extend(updates.into_iter().map(|u| (id, u.to_vec())));
            }
        }
    }
    out
}

/// Several updates to one document as one: applied to a scratch document
/// and read back as its whole state. An update that depends on structs the
/// scratch does not hold (the previous value of a map key, a list item to
/// delete) is kept pending there, and a yrs snapshot includes what is
/// pending, so nothing is lost; a peer merging the result ends up where it
/// would have applying each update in turn.
fn merge_updates(updates: &[&[u8]]) -> pimble_crdt::Result<Vec<u8>> {
    let mut scratch = NodeDoc::new();
    for update in updates {
        scratch.apply_update(update)?;
    }
    Ok(scratch.save())
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

/// One subscriber's sink and the scope roots of the grant it subscribed
/// with (`None`: it reaches the whole store). A scoped subscriber is sent a
/// notification about a document only when that document is in its scope
/// as of delivery, so it never learns another document's id, and a document
/// moved out of its share stops reaching it at once
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5).
struct ScopedSink {
    sink: jsonrpsee::core::server::SubscriptionSink,
    roots: Option<Vec<NodeId>>,
}

/// How a notification reaches one scoped subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// As it is: every document it names is in the subscriber's scope.
    Whole,
    /// Its document is in scope and another id it names is not (a share's
    /// root moved by its owner names parents the member has no grant on):
    /// sent as `TreeStructure { [document] }` with the same bytes, which
    /// says "this document changed, here is how" and names nothing else.
    DocumentOnly,
}

/// Which scoped subscribers a notification may reach and in which form,
/// resolved by the handler before the registry is locked (the scope sets
/// live in the store manager). A notification naming no document (a link's
/// or a mount's state) reaches everyone; an unscoped subscriber always gets
/// the notification as it is.
struct Delivery {
    admitted: Vec<(Vec<NodeId>, Form)>,
    to_everyone: bool,
}

impl Delivery {
    fn everyone() -> Self {
        Self { admitted: Vec::new(), to_everyone: true }
    }

    fn unscoped_only() -> Self {
        Self { admitted: Vec::new(), to_everyone: false }
    }

    fn form_for(&self, roots: &Option<Vec<NodeId>>) -> Option<Form> {
        match roots {
            None => Some(Form::Whole),
            Some(_) if self.to_everyone => Some(Form::Whole),
            Some(roots) => self.admitted.iter().find(|(r, _)| r == roots).map(|(_, form)| *form),
        }
    }
}

/// Manages active subscription sinks for pushing notifications.
struct SubscriptionRegistry {
    /// Store change subscribers: store_id -> list of sinks
    store_subs: HashMap<StoreId, Vec<ScopedSink>>,
    /// Node content change subscribers: (store_id, node_id) -> list of sinks
    node_subs: HashMap<(StoreId, NodeId), Vec<ScopedSink>>,
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

    fn add_store_sub(&mut self, store_id: StoreId, sink: jsonrpsee::core::server::SubscriptionSink, roots: Option<Vec<NodeId>>) {
        self.store_subs.entry(store_id).or_default().push(ScopedSink { sink, roots });
    }

    fn add_node_sub(&mut self, store_id: StoreId, node_id: NodeId, sink: jsonrpsee::core::server::SubscriptionSink, roots: Option<Vec<NodeId>>) {
        self.node_subs.entry((store_id, node_id)).or_default().push(ScopedSink { sink, roots });
    }

    /// The distinct root sets of `store_id`'s scoped subscribers (store and
    /// node subscriptions alike), for the handler to resolve a delivery
    /// against. Empty in the common case of no scoped subscriber at all.
    fn scoped_roots(&self, store_id: StoreId) -> Vec<Vec<NodeId>> {
        let mut out: Vec<Vec<NodeId>> = Vec::new();
        let store_sinks = self.store_subs.get(&store_id).into_iter().flatten();
        let node_sinks = self.node_subs.iter().filter(|((sid, _), _)| *sid == store_id).flat_map(|(_, sinks)| sinks.iter());
        for sink in store_sinks.chain(node_sinks) {
            if let Some(roots) = &sink.roots {
                if !out.contains(roots) {
                    out.push(roots.clone());
                }
            }
        }
        out
    }

    /// Send `msg` to every sink `delivery` admits, dropping closed ones. A
    /// sink admitted as [`Form::DocumentOnly`] gets `document_only` instead
    /// (and nothing when there is none to send).
    async fn send_to(sinks: &mut Vec<ScopedSink>, msg: &SubscriptionMessage, document_only: Option<&SubscriptionMessage>, delivery: &Delivery) {
        let mut closed = Vec::new();
        for (i, scoped) in sinks.iter().enumerate() {
            if scoped.sink.is_closed() {
                closed.push(i);
                continue;
            }
            let msg = match delivery.form_for(&scoped.roots) {
                Some(Form::Whole) => msg,
                Some(Form::DocumentOnly) => match document_only {
                    Some(msg) => msg,
                    None => continue,
                },
                None => continue,
            };
            if scoped.sink.send(msg.clone()).await.is_err() {
                closed.push(i);
            }
        }
        for i in closed.into_iter().rev() {
            sinks.swap_remove(i);
        }
    }

    /// Notify all store subscribers about a change, removing closed sinks.
    async fn notify_store_change(&mut self, notification: &StoreChangedNotification, delivery: &Delivery) {
        // Local, in-process broadcast (decision 3); harmless if no one (no
        // sync link) is currently subscribed. A link is this server's own
        // and sees everything: scope is a subscriber's concern.
        let _ = self.local_changes.send(LocalChange::Store(notification.clone()));

        if let Some(sinks) = self.store_subs.get_mut(&notification.store_id) {
            if let Ok(msg) = SubscriptionMessage::from_json(&notification) {
                // Built only when a subscriber needs it, which is rare.
                let document_only = delivery
                    .admitted
                    .iter()
                    .any(|(_, form)| *form == Form::DocumentOnly)
                    .then(|| named_by(&notification.change_kind))
                    .and_then(|named| match named {
                        Named::Document { id, .. } => SubscriptionMessage::from_json(&StoreChangedNotification {
                            change_kind: StoreChangeKind::TreeStructure { node_ids: vec![id] },
                            ..notification.clone()
                        })
                        .ok(),
                        _ => None,
                    });
                Self::send_to(sinks, &msg, document_only.as_ref(), delivery).await;
            }
        }
    }

    /// Notify all node content subscribers about a change, removing closed
    /// sinks. It names its node and nothing else, so it has one form.
    async fn notify_node_change(&mut self, notification: &NodeContentChangedNotification, delivery: &Delivery) {
        let _ = self.local_changes.send(LocalChange::Node(notification.clone()));

        let key = (notification.store_id, notification.node_id);
        if let Some(sinks) = self.node_subs.get_mut(&key) {
            debug!("notify_node_change: {} sinks for {:?}/{:?}", sinks.len(), notification.store_id, notification.node_id);
            match SubscriptionMessage::from_json(&notification) {
                Ok(msg) => Self::send_to(sinks, &msg, Some(&msg), delivery).await,
                Err(e) => warn!("notify_node_change: failed to serialize notification: {}", e),
            }
        }
    }

    /// Remove all subscriptions for a store.
    fn remove_store(&mut self, store_id: StoreId) {
        self.store_subs.remove(&store_id);
        self.node_subs.retain(|(sid, _), _| *sid != store_id);
    }
}

/// What a notification names, for scoped delivery.
enum Named {
    /// No document (a link's or a mount's state, which name nothing a
    /// scope could hide): it reaches every subscriber.
    Nothing,
    /// A document no scope can hold (the retired `tree` vault document,
    /// which is no node): it reaches unscoped subscribers only.
    Nobodys,
    /// The document it is about, and the other ids it names (the parents
    /// of a create, a delete or a move).
    Document { id: NodeId, others: Vec<NodeId> },
}

fn named_by(kind: &StoreChangeKind) -> Named {
    match kind {
        StoreChangeKind::NodeCreated { node_id, parent_id } | StoreChangeKind::NodeDeleted { node_id, parent_id } => {
            Named::Document { id: *node_id, others: vec![*parent_id] }
        }
        StoreChangeKind::NodeMoved { node_id, old_parent_id, new_parent_id } => {
            Named::Document { id: *node_id, others: vec![*old_parent_id, *new_parent_id] }
        }
        StoreChangeKind::MetadataUpdated { node_id } | StoreChangeKind::ContentUpdated { node_id } => {
            Named::Document { id: *node_id, others: Vec::new() }
        }
        // One document per notification wherever this server builds one; a
        // longer list is about its first, like the bytes it carries.
        StoreChangeKind::TreeStructure { node_ids } => match node_ids.split_first() {
            Some((id, others)) => Named::Document { id: *id, others: others.to_vec() },
            None => Named::Nobodys,
        },
        StoreChangeKind::VaultAppended { doc_id: VaultDocId::Node(node_id), .. } => Named::Document { id: *node_id, others: Vec::new() },
        StoreChangeKind::VaultAppended { doc_id: VaultDocId::Tree, .. } => Named::Nobodys,
        StoreChangeKind::SyncStateChanged { .. }
        | StoreChangeKind::MountStateChanged { .. }
        | StoreChangeKind::ShareStateChanged { .. } => Named::Nothing,
    }
}

/// RPC handler implementation.
///
/// A bag of `Arc`s, `Clone` for that reason: a store's `SyncLink` owns a
/// clone so it can apply a peer's updates on the same live state as every
/// other client (docs/SYNC_CONTRACT.md decision 1).
#[derive(Clone)]
pub struct RpcHandler {
    store_manager: Arc<RwLock<StoreManager>>,
    subscriptions: Arc<RwLock<SubscriptionRegistry>>,
    flush_debouncer: Arc<FlushDebouncer>,
    /// Debounced tree repairs after merged structural updates (see [`Repair`]).
    repairs: Arc<RepairDebouncer>,
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
    /// What this device knows of the shares it keeps up, as their owner's
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5; `crate::share`).
    shares: Arc<crate::share::Shares>,
    /// What is shared from this computer (docs/RELAY_CONTRACT.md): the relay
    /// face, the twins it holds and the tunnel to Pimble Cloud's relay
    /// (`crate::relay_face`). Nothing of it runs until a store is relayed.
    relay: Arc<crate::relay_face::RelayHost>,
}

/// What a share's member is told a store is called by this server, which
/// knows the owner's name for it and must not pass it on.
const SHARE_PLACEHOLDER_NAME: &str = "Shared with you";

/// What a whole replica of a store shared from another computer is called
/// here until the person names it: the accounts service holds no name for a
/// relayed store (docs/RELAY_CONTRACT.md), by design.
const RELAYED_PLACEHOLDER_NAME: &str = "Shared from another computer";

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
            repairs: Arc::new(RepairDebouncer::default()),
            plugin_host: Arc::new(pimble_plugins::create_default_host()),
            indexes: Arc::new(RwLock::new(HashMap::new())),
            semantic_available,
            links: Arc::new(RwLock::new(HashMap::new())),
            credentials: Arc::new(crate::credentials::CredentialStore::new(credentials_path)),
            mounts: Arc::new(Mutex::new(MountTracking::default())),
            vault_links: Arc::new(RwLock::new(HashMap::new())),
            keystore: Arc::new(Keystore::new(keystore_path)),
            shares: Arc::new(crate::share::Shares::new(crate::share::DEFAULT_SWEEP_EVERY)),
            relay: Arc::new(crate::relay_face::RelayHost::new(relay_dir_beside(&replicas_dir))),
            replicas_dir: Arc::new(replicas_dir),
        }
    }

    /// Where the twins of the stores shared from this computer live
    /// (`ServerConfig::relay_dir`), in place of `relay` beside the replicas
    /// directory. Call before the handler is cloned or serves anything.
    pub fn with_relay_dir(mut self, dir: PathBuf) -> Self {
        self.relay = Arc::new(crate::relay_face::RelayHost::new(dir));
        self
    }

    /// How often each hosted store's key sweep runs (`crate::share`), in
    /// place of the default minute. `PimbleServer::start` passes
    /// `ServerConfig::share_sweep_interval` through here; tests shorten it.
    /// Call before the handler is cloned or serves anything.
    pub fn with_share_sweep_interval(mut self, every: Duration) -> Self {
        self.shares = Arc::new(crate::share::Shares::new(every));
        self
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
    pub(crate) fn mark_replica(&self, store: &mut pimble_core::Store) {
        store.is_replica = store.local_path().map_or(false, |p| p.starts_with(self.replicas_dir.as_path()));
    }

    /// The guard every store-scoped RPC but the four vault ones needs
    /// (docs/CRYPTO_CONTRACT.md "Pimble server, a store of kind `vault`"): a
    /// vault store has no node documents or search index here, so none of
    /// those RPCs may touch it. `Ok(())` when the store is `Plain` or not
    /// open at all — a missing store still fails downstream with its own,
    /// more specific `NotOpen`/`StoreNotFound` error.
    async fn reject_if_vault(&self, store_id: StoreId) -> Result<(), ErrorObjectOwned> {
        if self.store_manager.read().await.store_kind(store_id) == Some(StoreKind::Vault) {
            return Err(encrypted_store_error(format!(
                "store {} is encrypted; use the vault API", store_id
            )));
        }
        Ok(())
    }

    // ── Scoped grants and read-only replicas (docs/NODE_DOCUMENT_CONTRACT.md
    // section 5) ─────────────────────────────────────────────────────────

    /// The guard every write RPC on a plain store runs after `authorize`:
    /// a replica this device holds as a share's reader (`sync.json`'s
    /// `access: read`, or a reader's root among several) refuses a write
    /// touching `ids` with the reader's own sentence, as the hosted server
    /// would refuse the push. A link's applies never come this way
    /// (`apply_node_update_from`), so a reader's replica still receives
    /// everything.
    async fn reject_if_read_only(&self, store_id: StoreId, ids: &[NodeId]) -> Result<(), ErrorObjectOwned> {
        if self.store_manager.read().await.write_refused(store_id, ids) {
            return Err(read_only_error());
        }
        Ok(())
    }

    /// Whether `principal` may delete `node_id`. A delete edits the node
    /// (and everything under it, which is in every scope the node is in)
    /// and its parent's list: a member cannot delete their share's own
    /// root, whose parent is not theirs.
    fn may_delete(manager: &StoreManager, principal: &Principal, store_id: StoreId, node_id: NodeId) -> Result<(), ErrorObjectOwned> {
        if let Some(reach) = Reach::of(manager, principal, store_id) {
            reach.require(node_id, Access::Write)?;
            if let Some(parent_id) = manager.get_node(store_id, node_id).map_err(to_rpc_error)?.parent_id {
                reach.require(parent_id, Access::Write)?;
            }
        }
        if manager.write_refused(store_id, &[node_id]) {
            return Err(read_only_error());
        }
        Ok(())
    }

    /// A `Store` as `principal` is to see it (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5): a share's member is shown the roots of their grant, never
    /// the store's own root (a document they have no grant on), and what
    /// they may change is the narrower of what this device may and what
    /// their role may.
    fn present_store_to(principal: &Principal, store: &mut pimble_core::Store) {
        let Principal::User { grants, .. } = principal else { return };
        let Some(grant) = grants.get(&store.id) else { return };
        if !grant.allows(Access::Write) {
            store.access = StoreAccess::Read;
        }
        if let Some(roots) = grant.roots_allowing(Access::Read) {
            if let Some(first) = roots.first() {
                store.root_node_id = *first;
            }
            // A reader of one root who edits another: the roots only read,
            // named as a replica's `sync.json` names its own. Nothing to
            // name when one answer (`access`) covers every root.
            let edited = grant.roots_allowing(Access::Write).unwrap_or_default();
            if !edited.is_empty() {
                for root in roots.iter().filter(|root| !edited.contains(root)) {
                    if !store.read_only_roots.contains(root) {
                        store.read_only_roots.push(*root);
                    }
                }
            }
            store.roots = roots;
            // The owner's store name is not a share's member's to learn
            // (docs/NODE_DOCUMENT_CONTRACT.md "A share has a name of its own"):
            // this server does not know the share's name, the accounts
            // service does, and a client puts it here from its own row.
            store.name = SHARE_PLACEHOLDER_NAME.to_string();
        }
    }

    /// A scoped principal's read or write of vault document `doc_id`: it must
    /// be a node document in the principal's scope. The retired tree
    /// document is in no scope, and a document that does not exist answers
    /// exactly as one that is someone else's.
    fn require_vault_doc_in_scope(
        &self,
        manager: &StoreManager,
        principal: &Principal,
        store_id: StoreId,
        doc_id: &VaultDocId,
        needed: Access,
    ) -> Result<(), ErrorObjectOwned> {
        let Some(reach) = Reach::of(manager, principal, store_id) else {
            return Ok(());
        };
        match doc_id {
            VaultDocId::Node(id) => reach.require(*id, needed),
            VaultDocId::Tree => Err(no_grant_for_document_error()),
        }
    }

    /// What `principal` reaches in `store_id`: `None` when it reaches the
    /// whole store (see [`Reach`]). Call after `authorize`.
    async fn reach_of(&self, principal: &Principal, store_id: StoreId) -> Option<Reach> {
        let manager = self.store_manager.read().await;
        Reach::of(&manager, principal, store_id)
    }

    /// Which scoped subscribers of `store_id` a notification of `kind` may
    /// reach, and in which form (see [`Delivery`]). Resolved before the
    /// registry is locked, with a read of the store manager, which no
    /// caller holds at that point. Cheap when nobody scoped is subscribed,
    /// the common case.
    async fn delivery_for(&self, store_id: StoreId, kind: &StoreChangeKind) -> Delivery {
        let (id, others) = match named_by(kind) {
            Named::Nothing => return Delivery::everyone(),
            Named::Nobodys => return Delivery::unscoped_only(),
            Named::Document { id, others } => (id, others),
        };
        let scoped = self.subscriptions.read().await.scoped_roots(store_id);
        if scoped.is_empty() {
            return Delivery::unscoped_only();
        }
        let manager = self.store_manager.read().await;
        let admitted = scoped
            .into_iter()
            .filter_map(|roots| {
                let scope = scope_set(&manager, store_id, &roots);
                if !scope.contains(&id) {
                    return None;
                }
                let form = if others.iter().all(|other| scope.contains(other)) { Form::Whole } else { Form::DocumentOnly };
                Some((roots, form))
            })
            .collect();
        Delivery { admitted, to_everyone: false }
    }

    // ── Replica sync (docs/SYNC_CONTRACT.md) ─────────────────────────

    /// Shared handle to the store manager, for [`crate::sync_link`] and
    /// [`crate::vault_link`] to read the node documents directly (ids,
    /// state vectors, diffs) alongside the handler's own
    /// [`RpcHandler::apply_node_update_from`] (used to actually merge,
    /// persist, broadcast, and index a remote change).
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
    pub(crate) async fn sync_mode_of(&self, store_id: StoreId) -> StoreKind {
        self.link_kind_of(store_id).await.0
    }

    /// [`Self::sync_mode_of`], and whether the store is relayed and from
    /// which end (`Store::relay`, docs/RELAY_CONTRACT.md): `Owner` for a
    /// store shared from this computer (`mode: "relay"`), `Member` for a
    /// replica whose vault link goes through Pimble Cloud's relay to its
    /// owner's computer (`via_relay`). A relayed store's link is a vault
    /// link either way, so its `sync_mode` is `Vault`.
    pub(crate) async fn link_kind_of(&self, store_id: StoreId) -> (StoreKind, RelaySide) {
        let manager = self.store_manager.read().await;
        self.link_kind_of_locked(&manager, store_id).await
    }

    /// [`Self::link_kind_of`] for a caller that holds the manager's lock.
    async fn link_kind_of_locked(&self, manager: &StoreManager, store_id: StoreId) -> (StoreKind, RelaySide) {
        match manager.read_sync_config(store_id).await {
            Ok(Some(config)) => match config.mode {
                SyncMode::Sync => (StoreKind::Plain, RelaySide::None),
                SyncMode::Vault if config.via_relay => (StoreKind::Vault, RelaySide::Member),
                SyncMode::Vault => (StoreKind::Vault, RelaySide::None),
                SyncMode::Relay => (StoreKind::Vault, RelaySide::Owner),
            },
            _ => (StoreKind::Plain, RelaySide::None),
        }
    }

    /// Fill in what only the server knows of a `Store`'s link: its state,
    /// its mode and whether it is relayed.
    async fn describe_link(&self, store: &mut pimble_core::Store) {
        store.sync_state = self.sync_state_of(store.id).await;
        (store.sync_mode, store.relay) = self.link_kind_of(store.id).await;
    }

    /// Start a sync link for `store_id` if one isn't already running.
    /// `openStore`, `addRemoteStore`, `set_store_sync(Some(remote))` and no-op if
    /// a link is already present.
    async fn ensure_link_started(&self, store_id: StoreId, remote: RemoteEndpoint) {
        // Decision 4 of docs/history/REMOTE_MOUNTS_CONTRACT.md: the link answers
        // `last_sync` from `sync.json` until it next reaches `Synced`, so a
        // mount sourced from this store reports `Cached { last_sync }`
        // rather than `Connecting` after a restart with the remote down.
        let config = {
            let manager = self.store_manager.read().await;
            manager.read_sync_config(store_id).await.ok().flatten()
        };
        let last_sync = config.as_ref().and_then(|c| c.last_sync);
        // A store has one link. A vault link (docs/CRYPTO_CONTRACT.md) talks
        // to the hosted twin with a minted JWT; a plain link started beside
        // it would connect with no credential, be refused, and flap. The
        // same for a store shared from this computer
        // (docs/RELAY_CONTRACT.md), whose vault link's remote is this
        // process's own relay face: whether that link runs yet or not,
        // `sync.json` says what the store's one link is.
        if self.vault_links.read().await.contains_key(&store_id) {
            warn!("store {} has a vault link; not starting a plain sync link beside it", store_id);
            return;
        }
        if let Some(mode) = config.map(|c| c.mode).filter(|mode| mode.is_vault_link()) {
            warn!("store {} is linked in {:?} mode; not starting a plain sync link for it", store_id, mode);
            return;
        }
        let mut links = self.links.write().await;
        if links.contains_key(&store_id) {
            return;
        }
        let handle = SyncLink::start(self.clone(), store_id, remote, last_sync);
        links.insert(store_id, handle);
    }

    /// Stop and remove `store_id`'s sync link, if any.
    /// Stop every link, plain and vault (`PimbleServer::stop`).
    pub(crate) async fn stop_links(&self) {
        for (_, handle) in self.links.write().await.drain() {
            handle.stop();
        }
        for (_, handle) in self.vault_links.write().await.drain() {
            handle.stop();
        }
    }

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

    // ── Sharing, the owner's half (crate::share) ─────────────────────────

    pub(crate) fn shares(&self) -> &crate::share::Shares {
        &self.shares
    }

    // ── Sharing from this computer (crate::relay_face) ───────────────────

    pub(crate) fn relay(&self) -> &crate::relay_face::RelayHost {
        &self.relay
    }

    /// `store_id`'s vault link state; `None` when it has no vault link.
    pub(crate) async fn vault_link_state(&self, store_id: StoreId) -> Option<SyncState> {
        self.vault_links.read().await.get(&store_id).map(|handle| handle.state())
    }

    /// Hand `store_id`'s vault link a command for its share upkeep. `false`
    /// when the store has no vault link to take it.
    pub(crate) async fn share_command(&self, store_id: StoreId, command: crate::share::ShareCommand) -> bool {
        self.vault_links.read().await.get(&store_id).is_some_and(|handle| handle.share_command(command))
    }

    /// Run `store_id`'s key sweep now (a member was just invited).
    pub(crate) async fn kick_share_sweep(&self, store_id: StoreId) {
        if let Some(handle) = self.vault_links.read().await.get(&store_id) {
            handle.kick_sweep();
        }
    }

    /// Tell a store's subscribers that one of its shares changed state on
    /// this device. Derived state, like a link's: it carries no bytes and
    /// no link forwards it.
    pub(crate) async fn notify_share_state_changed(&self, store_id: StoreId, node_id: NodeId, state: SyncState) {
        self.notify_store_change(store_id, StoreChangeKind::ShareStateChanged { node_id, state }, None).await;
    }

    /// Change `node_id`'s metadata under one lock and send the change out
    /// exactly as `updateNodeMetadata` does (it is the same write): how a
    /// share's marker is set and removed, so that it replicates like any
    /// other metadata.
    pub(crate) async fn edit_node_metadata(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        change: impl FnOnce(&mut pimble_core::NodeMetadata),
    ) -> Result<(), ErrorObjectOwned> {
        let mut manager = self.store_manager.write().await;
        let mut metadata = manager.get_node(store_id, node_id).map_err(to_rpc_error)?.metadata;
        change(&mut metadata);
        let edit = manager.update_node_metadata(store_id, node_id, metadata).map_err(to_rpc_error)?;
        self.publish_metadata_edit(manager, store_id, node_id, edit).await
    }

    /// The tail of every metadata write: flush, broadcast, index. Takes the
    /// manager's lock from the caller that made `edit` under it.
    async fn publish_metadata_edit(
        &self,
        mut manager: tokio::sync::RwLockWriteGuard<'_, StoreManager>,
        store_id: StoreId,
        node_id: NodeId,
        edit: TreeEdit,
    ) -> Result<(), ErrorObjectOwned> {
        if edit.is_empty() {
            // The store writes only what differs, and nothing did: no
            // flush, no broadcast (decision 8 applies to a client's resend
            // of the same metadata as much as to a peer's).
            return Ok(());
        }
        manager.flush(store_id).await.map_err(to_rpc_error)?;
        drop(manager);

        self.broadcast_tree_edit(store_id, &edit, Some((node_id, StoreChangeKind::MetadataUpdated { node_id })), None).await;
        self.enqueue_index_event(store_id, IndexEvent::Upsert(node_id)).await;
        Ok(())
    }

    /// Start a vault link for `store_id` if one isn't already running
    /// (`openStore` restarting one from `sync.json`, `cloudHostStore`,
    /// `cloudAddHostedStore`, `cloudRelayStore`). `endpoint` is where the
    /// twin is: the hosted server (`mint_token`'s `rpc_url`, or the store's
    /// own endpoint when the accounts service names one), or this process's
    /// relay face for a store shared from this computer, which the link
    /// starts before every connect (docs/RELAY_CONTRACT.md: the face first,
    /// then the link). The link mints its own bearer fresh from the keystore
    /// on every connect, so no credential is passed in here.
    pub(crate) async fn ensure_vault_link_started(&self, store_id: StoreId, endpoint: LinkEndpoint, key_id: Uuid) {
        // A replica whose tree was pulled before the manifest root was kept
        // in step (see `adopt_document_root`) heals on its next open: the
        // documents on disk already hold the real root.
        self.adopt_document_root(store_id).await;
        let last_sync = {
            let manager = self.store_manager.read().await;
            manager.read_sync_config(store_id).await.ok().flatten().and_then(|c| c.last_sync)
        };
        let mut vault_links = self.vault_links.write().await;
        if vault_links.contains_key(&store_id) {
            return;
        }
        let handle = VaultLink::start(self.clone(), store_id, endpoint, key_id, last_sync);
        vault_links.insert(store_id, handle);
    }

    /// Stop and remove `store_id`'s vault link, if any.
    pub(crate) async fn stop_vault_link(&self, store_id: StoreId) {
        if let Some(handle) = self.vault_links.write().await.remove(&store_id) {
            handle.stop();
        }
    }

    /// Mint a fresh JWT (for where the store is reached) and write
    /// `store_id`'s `sync.json` as a vault-mode link to it under `key_id`,
    /// stripped of any credential (`auth: none`, matching
    /// `crate::sync_link`'s own decision 4) — a vault link never trusts
    /// what's on disk for a credential, only the keystore. Used by
    /// `cloudHostStore` and `cloudAddHostedStore`, both of which then call
    /// `ensure_vault_link_started` with the returned url.
    ///
    /// Where the store is reached: its own endpoint when the accounts
    /// service's `/token` lists one (a relay-tier store, served from its
    /// owner's computer; `via_relay`), else `rpc_url`, the hosted server
    /// (docs/RELAY_CONTRACT.md, "Members' side").
    async fn link_hosted_store(
        &self,
        store_id: StoreId,
        account: &crate::keystore::SignedInAccount,
        key_id: Uuid,
        held_as: &crate::cloud::HeldAs,
    ) -> Result<url::Url, ErrorObjectOwned> {
        let minted = crate::cloud::mint_token(&account.url, &account.session).await.map_err(to_rpc_error)?;
        let reach = minted.reach(store_id, None).map_err(to_rpc_error)?;
        let rpc_url = reach.url;

        let manager = self.store_manager.read().await;
        manager
            .write_sync_config(
                store_id,
                &SyncConfig {
                    remote: RemoteEndpoint { url: rpc_url.clone(), auth: AuthMethod::None },
                    last_sync: None,
                    mode: SyncMode::Vault,
                    via_relay: reach.via_relay,
                    last_seq: Default::default(),
                    vault_key_id: Some(key_id),
                    access: held_as.access,
                    shared_by: held_as.shared_by.clone(),
                    read_only_roots: held_as.read_only_roots.clone(),
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

        // An empty replica, without a root document of its own (decision 8:
        // two independently initialised roots for the same id would merge
        // field by field, and their lists into duplicated children).
        let mut manager = self.store_manager.write().await;
        let created_id = manager
            .create_replica(&path, remote_store.id, &remote_store.name, remote_store.root_node_id)
            .await
            .map_err(to_rpc_error)?;
        manager
            .write_sync_config(created_id, &SyncConfig { remote: Self::without_auth(&remote), last_sync: None, mode: pimble_store::SyncMode::Sync, via_relay: false, last_seq: Default::default(), vault_key_id: None, access: StoreAccess::Full, shared_by: None, read_only_roots: Vec::new() })
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

    /// Mark `store_id` as having dirty documents and, if no flush task is
    /// already scheduled, spawn one. The task sleeps for
    /// [`CONTENT_FLUSH_DEBOUNCE`], drains whichever stores (and
    /// `modified_at` stamps) are pending at that point, stamps, and flushes
    /// each store in turn. At most one flush task runs at a time; edits that
    /// arrive after the drain (a narrow race) simply schedule a fresh task
    /// on their own next call.
    fn schedule_content_flush(&self, store_id: StoreId) {
        self.flush_debouncer.pending.lock().unwrap().stores.insert(store_id);
        self.ensure_flush_scheduled();
    }

    /// A person edited `node_id`'s content: stamp its `modified_at` with the
    /// next debounced flush (see [`FlushDebouncer`]).
    fn note_content_edit(&self, store_id: StoreId, node_id: NodeId) {
        {
            let mut pending = self.flush_debouncer.pending.lock().unwrap();
            pending.stores.insert(store_id);
            pending.stamps.insert((store_id, node_id));
        }
        self.ensure_flush_scheduled();
    }

    fn ensure_flush_scheduled(&self) {
        {
            let mut scheduled = self.flush_debouncer.scheduled.lock().unwrap();
            if *scheduled {
                return;
            }
            *scheduled = true;
        }
        let handler = self.clone();
        tokio::spawn(async move { handler.run_debounced_flush().await });
    }

    /// The flush task: stamp every pending node once, flush every pending
    /// store, then broadcast the stamps as the server's own metadata edits
    /// (`source_client_id: None`, so a link forwards them: the peer's copy
    /// of the document gets the stamp from here, never one of its own).
    async fn run_debounced_flush(self) {
        tokio::time::sleep(CONTENT_FLUSH_DEBOUNCE).await;

        let FlushPending { stores, stamps } = {
            let mut pending = self.flush_debouncer.pending.lock().unwrap();
            std::mem::take(&mut *pending)
        };

        let mut stamped: Vec<(StoreId, NodeId, TreeEdit)> = Vec::new();
        {
            let mut manager = self.store_manager.write().await;
            let now = now();
            for (store_id, node_id) in stamps {
                // A node deleted, or a store closed, since the edit: nothing
                // to stamp, and nothing to say about it.
                let Some(edit) = manager.tree_mut(store_id).ok().and_then(|tree| tree.touch_modified(node_id, &now).ok()) else {
                    continue;
                };
                for id in edit.node_ids() {
                    let _ = manager.mark_dirty(store_id, id);
                }
                stamped.push((store_id, node_id, edit));
            }
            for store_id in stores {
                if let Err(e) = manager.flush(store_id).await {
                    warn!("Debounced content flush failed for store {}: {}", store_id, e);
                }
            }
        }

        *self.flush_debouncer.scheduled.lock().unwrap() = false;

        for (store_id, node_id, edit) in stamped {
            self.broadcast_tree_edit(store_id, &edit, Some((node_id, StoreChangeKind::MetadataUpdated { node_id })), None).await;
        }
    }

    /// Notify store subscribers about a change that is not about a document
    /// (a link's or a mount's state); it carries no bytes.
    async fn notify_store_change(&self, store_id: StoreId, kind: StoreChangeKind, source: Option<&str>) {
        let delivery = self.delivery_for(store_id, &kind).await;
        let notification = StoreChangedNotification {
            store_id,
            change_kind: kind,
            source_client_id: source.map(String::from),
            update: None,
        };
        self.subscriptions.write().await.notify_store_change(&notification, &delivery).await;
    }

    /// Broadcast one document's change: `kind` names the document, and
    /// `update_b64` is the update that changed it, so a subscriber applies
    /// the bytes instead of refetching. `ContentUpdated` also reaches the
    /// node's own subscribers (the editor), carrying the same bytes as its
    /// operation.
    async fn broadcast_document_change(&self, store_id: StoreId, kind: StoreChangeKind, source: Option<&str>, update_b64: Option<String>) {
        let delivery = self.delivery_for(store_id, &kind).await;
        let store_notif = StoreChangedNotification {
            store_id,
            change_kind: kind.clone(),
            source_client_id: source.map(String::from),
            update: update_b64.clone(),
        };
        // Acquire lock once for both notification types
        let mut registry = self.subscriptions.write().await;
        if let StoreChangeKind::ContentUpdated { node_id } = kind {
            let node_notif = NodeContentChangedNotification {
                store_id,
                node_id,
                source_client_id: source.map(String::from),
                operation: update_b64.map(|changes| EditOperation::IncrementalChanges { changes }),
            };
            registry.notify_node_change(&node_notif, &delivery).await;
        }
        registry.notify_store_change(&store_notif, &delivery).await;
    }

    /// Broadcast a [`TreeEdit`]: one notification per document it touched,
    /// carrying that document's update, so a link forwards bytes and never
    /// reconciles for a live edit. The document named by `primary` gets that
    /// kind (the RPC's own: `NodeCreated` for the node it created), every
    /// other one `TreeStructure { [id] }` (a parent's list, a descendant's
    /// tombstone). An edit that wrote one document in several transactions
    /// (a create with tags, a metadata update of several fields) names it
    /// once, with the transactions merged into one update.
    async fn broadcast_tree_edit(&self, store_id: StoreId, edit: &TreeEdit, primary: Option<(NodeId, StoreChangeKind)>, source: Option<&str>) {
        for (node_id, update) in coalesce_edit(edit) {
            let kind = match &primary {
                Some((primary_id, kind)) if *primary_id == node_id => kind.clone(),
                _ => StoreChangeKind::TreeStructure { node_ids: vec![node_id] },
            };
            let update_b64 = base64::engine::general_purpose::STANDARD.encode(&update);
            self.broadcast_document_change(store_id, kind, source, Some(update_b64)).await;
        }
    }

    /// Merge `update` into `node_id`'s document (creating it when unknown),
    /// persist on the flush debounce, derive the notifications from what it
    /// changed (see [`derive_kinds`]) and broadcast them with `update` as
    /// their bytes, feed the index, and settle the tree per `repair`. The
    /// one path a peer's update takes into a store: `applyEdit` from a
    /// client, and a sync or vault link applying what its remote sent.
    /// Nothing at all happens for an update the document already reflected
    /// (docs/history/HARDENING_CONTRACT.md decision 8: no flush, no
    /// broadcast, no re-index, no repair). Authorisation is the caller's.
    pub(crate) async fn apply_node_update_from(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        update: &[u8],
        source: Option<&str>,
        repair: Repair,
    ) -> Result<NodeUpdateEffect, ErrorObjectOwned> {
        let (effect, kinds) = {
            let mut manager = self.store_manager.write().await;
            let before = shape_of(manager.tree(store_id).map_err(to_rpc_error)?, node_id);
            let effect = manager.apply_node_update(store_id, node_id, update).map_err(to_rpc_error)?;
            if !effect.changed {
                return Ok(effect);
            }
            let tree = manager.tree(store_id).map_err(to_rpc_error)?;
            let after = shape_of(tree, node_id);
            (effect, derive_kinds(node_id, tree.root(), before.as_ref(), after.as_ref(), effect))
        };

        self.schedule_content_flush(store_id);

        let update_b64 = base64::engine::general_purpose::STANDARD.encode(update);
        for kind in kinds {
            for event in index_events_for(&kind) {
                self.enqueue_index_event(store_id, event).await;
            }
            self.broadcast_document_change(store_id, kind, source, Some(update_b64.clone())).await;
        }

        if effect.structure {
            // A replica created around a placeholder root (a vault twin's)
            // learns its real root from the documents as they arrive.
            self.adopt_document_root(store_id).await;
            match repair {
                Repair::Debounced => self.schedule_repair(store_id),
                Repair::Later => {}
            }
        }

        Ok(effect)
    }

    /// Repair `store_id`'s tree once no structural update has landed for
    /// [`REPAIR_DEBOUNCE`] (see [`Repair`]).
    pub(crate) fn schedule_repair(&self, store_id: StoreId) {
        let generation = {
            let mut gens = self.repairs.generation.lock().unwrap();
            let g = gens.entry(store_id).or_insert(0);
            *g += 1;
            *g
        };
        let handler = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(REPAIR_DEBOUNCE).await;
            let still_current = handler.repairs.generation.lock().unwrap().get(&store_id).copied() == Some(generation);
            if still_current {
                handler.repair_store_tree(store_id).await;
            }
        });
    }

    /// Repair `store_id`'s tree (see `Tree::repair`) if it needs it, then
    /// flush, broadcast, and re-index exactly like any other structural
    /// change (docs/history/HARDENING_CONTRACT.md decision 9). Called at
    /// `openStore`, after a reconcile, and (debounced) after merged
    /// structural updates. `None` (nothing to repair) is by far the common
    /// case, and costs one cheap read-only pass over the tree. Logs and
    /// returns on error rather than failing its caller's RPC — like search
    /// indexing, this is best-effort upkeep, not a precondition for the
    /// operation that triggered it.
    pub(crate) async fn repair_store_tree(&self, store_id: StoreId) {
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
        info!("Repaired tree for store {}: {} document(s) touched", store_id, repair.touched.len());

        // `source_client_id: None`, like any change with no single originating
        // client, so a sync link forwards it rather than treating it as its
        // own echo.
        self.broadcast_tree_edit(store_id, &repair, None, None).await;

        // A repair only ever reassigns a node's parent or reorders/fixes a
        // children list, never removes a node entry — every touched id is
        // still there to upsert.
        for node_id in repair.node_ids() {
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
        let manager = self.store_manager.read().await;
        let root_id = manager.root_node_id(store_id)?;
        // A freshly added remote store has a root id in its manifest but no
        // documents until its first reconcile: nothing to index yet, and not
        // an error. The reconcile's updates feed the index.
        if !manager.tree(store_id)?.has_node(root_id) {
            return Ok(0);
        }
        let mut stack = vec![root_id];
        let mut count = 0usize;
        while let Some(node_id) = stack.pop() {
            let node = manager.get_node(store_id, node_id)?;
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
                self.start_link_from_config(store_id, config).await;
            }
            self.repair_store_tree(store_id).await;
        }
    }

    /// Bring a store's manifest root in line with the root its documents
    /// say it has (`StoreManager::document_root`: the manifest's root when
    /// that is a node with no parent, else the one parentless node). A
    /// replica created empty for a vault twin carries a placeholder root
    /// until the pull brings the real root's document; until 2026-09-16 the
    /// manifest kept the placeholder, so every later `openStore`/`listStores`
    /// handed the app a root id that no node had ("Node not found" in the
    /// status bar, an empty store row). Called after every merged
    /// structural update and when a vault link starts. Nothing to adopt
    /// (no root yet, or several candidates) changes nothing.
    pub(crate) async fn adopt_document_root(&self, store_id: StoreId) {
        let mut manager = self.store_manager.write().await;
        let Ok(Some(doc_root)) = manager.document_root(store_id) else {
            return;
        };
        let Ok(manifest_root) = manager.root_node_id(store_id) else { return };
        if doc_root != manifest_root {
            info!("Store {}: manifest root {} replaced by the documents' root {}", store_id, manifest_root, doc_root);
            if let Err(e) = manager.set_root_node_id(store_id, doc_root).await {
                warn!("Store {}: rewriting the manifest root failed: {}", store_id, e);
            }
        }
    }

    /// Start whichever link `sync.json` describes: a plain sync link for
    /// `mode: "sync"`, a vault link for `mode: "vault"`
    /// (docs/CRYPTO_CONTRACT.md). Both `ensure_*_started` no-op when a link
    /// of that kind is already running. `openStore` and `adopt_newly_opened`
    /// share this so an implicitly adopted vault replica never gets a plain
    /// link beside its vault link (2026-09-16: `cloudAddHostedStore`'s
    /// replica, adopted by the next `listStores`, flapped Syncing/Offline
    /// with "refused the credentials" while its vault link sat Synced).
    async fn start_link_from_config(&self, store_id: StoreId, config: SyncConfig) {
        match config.mode {
            SyncMode::Sync => self.ensure_link_started(store_id, config.remote).await,
            SyncMode::Vault => match config.vault_key_id {
                Some(key_id) => self.ensure_vault_link_started(store_id, LinkEndpoint::Remote(config.remote.url), key_id).await,
                None => warn!(
                    "store {} sync.json has mode: vault but no vault_key_id; not starting a vault link",
                    store_id
                ),
            },
            // Shared from this computer (docs/RELAY_CONTRACT.md): the same
            // vault link, to this process's own relay face. `remote.url` is
            // not where it connects: the face's port is new every run.
            SyncMode::Relay => match config.vault_key_id {
                Some(key_id) => self.ensure_vault_link_started(store_id, LinkEndpoint::RelayFace, key_id).await,
                None => warn!(
                    "store {} sync.json has mode: relay but no vault_key_id; not starting its link",
                    store_id
                ),
            },
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
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        debug!("Vault append to store {} doc {:?}", request.store_id, request.doc_id);

        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let blob = URL_SAFE_NO_PAD
            .decode(&request.blob)
            .map_err(|e| to_rpc_error(format!("invalid base64url blob: {}", e)))?;

        let mut manager = self.store_manager.write().await;

        // A scoped member reaches its scope's documents, and may add one
        // under a parent it may write: the new id then joins every scope of
        // the member's that holds the parent, before the owner's devices
        // see it, so every other member's reads of it are in scope too
        // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Scope sets"). The
        // retired tree document is in no scope, and a document not in the
        // store whose request names no parent, or a parent outside the
        // scope, is refused like any other.
        let mut joins_scopes: Option<(Vec<NodeId>, NodeId, NodeId)> = None;
        if let Some(reach) = Reach::of(&manager, &principal, request.store_id) {
            let VaultDocId::Node(node_id) = &request.doc_id else {
                return Err(no_grant_for_document_error());
            };
            if reach.readable.contains(node_id) {
                reach.require(*node_id, Access::Write)?;
            } else {
                let held = manager.vault_has_doc(request.store_id, &request.doc_id.as_str()).map_err(to_rpc_error)?;
                match request.parent_id {
                    Some(parent_id) if !held => {
                        reach.require(parent_id, Access::Write)?;
                        let roots = scope_roots_of(&principal, request.store_id, Access::Read).unwrap_or_default();
                        joins_scopes = Some((roots, parent_id, *node_id));
                    }
                    _ => return Err(no_grant_for_document_error()),
                }
            }
        }

        // A document's first append carries its wrapped data key, stored
        // before the blob and under the same lock, so nobody can ever fetch a
        // blob whose key the server does not hold. For a document that has
        // blobs already, carried keys must name the key it is under (a retry
        // of a create whose answer was lost) and are otherwise ignored: such
        // a document's keys change only through `vaultSetDocKeys`, which
        // merges. Any other key would put a blob in the log that nobody but
        // its sender can open, and every reader's cursor would stop at it.
        if let Some(keys) = &request.keys {
            let doc = request.doc_id.as_str();
            if !manager.vault_has_blobs(request.store_id, &doc).map_err(to_rpc_error)? {
                let json = serde_json::to_string(keys).map_err(to_rpc_error)?;
                manager.vault_set_doc_keys(request.store_id, &doc, json).await.map_err(to_rpc_error)?;
            } else {
                let held = manager
                    .vault_doc_keys(request.store_id, &doc)
                    .map_err(to_rpc_error)?
                    .and_then(|record| serde_json::from_str::<VaultDocKeys>(&record.json).ok());
                if held.map(|held| held.dek_id) != Some(keys.dek_id) {
                    return Err(to_rpc_error(format!(
                        "document {:?} of store {} already exists under another key; fetch it before writing to it",
                        request.doc_id, request.store_id
                    )));
                }
            }
        }

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
        if let Some((roots, parent_id, node_id)) = joins_scopes {
            manager.vault_extend_scopes(request.store_id, &roots, parent_id, node_id).await.map_err(to_rpc_error)?;
        }
        drop(manager);

        // The blob rides the notification verbatim (still base64url) so a
        // live subscriber never re-fetches; `source_client_id` carries the
        // caller's id (the same way `applyEdit`'s `client_id` propagates),
        // so a client can drop its own echo by identity, not only by seq.
        let kind = StoreChangeKind::VaultAppended { doc_id: request.doc_id.clone(), seq };
        let delivery = self.delivery_for(request.store_id, &kind).await;
        let notification = StoreChangedNotification {
            store_id: request.store_id,
            change_kind: kind,
            source_client_id: request.client_id.clone(),
            update: Some(request.blob.clone()),
        };
        self.subscriptions.write().await.notify_store_change(&notification, &delivery).await;

        Ok(VaultAppendResponse { seq })
    }

    async fn vault_fetch(&self, ext: &Extensions, request: VaultFetchRequest) -> Result<VaultFetchResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Read)?;
        debug!("Vault fetch from store {} doc {:?} after {}", request.store_id, request.doc_id, request.after_seq);

        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        let manager = self.store_manager.read().await;
        self.require_vault_doc_in_scope(&manager, &principal, request.store_id, &request.doc_id, Access::Read)?;
        let (snapshot, updates, head) = manager
            .vault_fetch(request.store_id, &request.doc_id.as_str(), request.after_seq)
            .await
            .map_err(to_rpc_error)?;
        let keys = manager
            .vault_doc_keys(request.store_id, &request.doc_id.as_str())
            .map_err(to_rpc_error)?
            .and_then(|record| serde_json::from_str::<VaultDocKeys>(&record.json).ok());

        Ok(VaultFetchResponse {
            snapshot: snapshot.map(|(seq, blob)| VaultEntry { seq, blob: URL_SAFE_NO_PAD.encode(blob) }),
            updates: updates
                .into_iter()
                .map(|(seq, blob)| VaultEntry { seq, blob: URL_SAFE_NO_PAD.encode(blob) })
                .collect(),
            head,
            keys,
        })
    }

    async fn vault_snapshot(&self, ext: &Extensions, request: VaultSnapshotRequest) -> Result<EmptyResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        info!("Vault snapshot for store {} doc {:?} upto {}", request.store_id, request.doc_id, request.upto_seq);
        if !request.covers_prefix {
            // A client from before 2026-09-17 stamps a snapshot with its own
            // latest append's number, whatever it had applied below it, and
            // storing one deletes the log entries it may lack. Acknowledged
            // (that client treats a refusal as an error worth surfacing) and
            // not stored; the log keeps everything.
            warn!(
                "Ignoring a vault snapshot for store {} doc {:?}: the sender does not vouch that it covers every entry up to {}",
                request.store_id, request.doc_id, request.upto_seq
            );
            return Ok(EmptyResponse {});
        }

        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let blob = URL_SAFE_NO_PAD
            .decode(&request.blob)
            .map_err(|e| to_rpc_error(format!("invalid base64url blob: {}", e)))?;

        let mut manager = self.store_manager.write().await;
        self.require_vault_doc_in_scope(&manager, &principal, request.store_id, &request.doc_id, Access::Write)?;
        manager
            .vault_snapshot(request.store_id, &request.doc_id.as_str(), request.upto_seq, blob)
            .await
            .map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn vault_list_docs(&self, ext: &Extensions, request: VaultListDocsRequest) -> Result<VaultListDocsResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Read)?;
        debug!("Vault list docs for store {}", request.store_id);

        let manager = self.store_manager.read().await;
        let docs = manager.vault_list_docs(request.store_id).map_err(to_rpc_error)?;
        let epoch = manager.vault_epoch(request.store_id).ok();
        // A scoped member is listed its scope and nothing else: the list is
        // how a recipient's replica learns which documents exist at all.
        let scope = scope_roots_of(&principal, request.store_id, Access::Read).map(|roots| scope_set(&manager, request.store_id, &roots));

        Ok(VaultListDocsResponse {
            docs: docs
                .into_iter()
                .filter_map(|doc| {
                    let doc_id = VaultDocId::parse(&doc.doc_id)?;
                    if let Some(scope) = &scope {
                        match &doc_id {
                            VaultDocId::Node(id) if scope.contains(id) => {}
                            _ => return None,
                        }
                    }
                    Some(VaultDocInfo { doc_id, head: doc.head, snapshot_seq: doc.snapshot_seq, dek_id: doc.dek_id })
                })
                .collect(),
            epoch,
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
        // A tunnel to the relay is opened with the account's session.
        self.relay.account_changed(self).await;

        Ok(EmptyResponse {})
    }

    async fn cloud_sign_out(&self, ext: &Extensions) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudSignOut")?;
        info!("Signing out of Pimble Cloud");
        self.keystore.sign_out().await.map_err(to_rpc_error)?;
        // Nothing is served to anyone on a session that was given up.
        self.relay.account_changed(self).await;
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

        // Shared from this computer: its record on the accounts service says
        // `relay`, and its link is to the twin on this machine. One or the
        // other, and the person says which.
        if self.link_kind_of(store_id).await.1 == RelaySide::Owner {
            return Err(to_rpc_error(crate::relay_face::RELAYED_LINK_REFUSAL));
        }

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
        crate::cloud::put_store_key(&account.url, &account.session, &store_id.to_string(), &account.user_id, key_id, &envelope, None)
            .await
            .map_err(to_rpc_error)?;
        self.keystore.add_store_key(store_id, key_id, &key).await.map_err(to_rpc_error)?;

        let rpc_url = self.link_hosted_store(store_id, &account, key_id, &crate::cloud::HeldAs::owner()).await?;
        self.ensure_vault_link_started(store_id, LinkEndpoint::Remote(rpc_url), key_id).await;

        Ok(CloudHostStoreResponse { store_id })
    }

    async fn cloud_relay_store(&self, ext: &Extensions, request: CloudRelayStoreRequest) -> Result<CloudRelayStoreResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudRelayStore")?;
        self.relay_store(request.store_id).await?;
        Ok(CloudRelayStoreResponse { store_id: request.store_id })
    }

    async fn cloud_stop_relaying(&self, ext: &Extensions, request: CloudStopRelayingRequest) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudStopRelaying")?;
        self.stop_relaying(request.store_id).await?;
        Ok(EmptyResponse {})
    }

    async fn cloud_list_hosted_stores(&self, ext: &Extensions) -> Result<CloudListHostedStoresResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudListHostedStores")?;
        let account = self.keystore.account().await.ok_or_else(|| to_rpc_error("no Pimble Cloud account is signed in"))?;
        let stores = crate::cloud::list_stores(&account.url, &account.session).await.map_err(to_rpc_error)?;
        // One id however many rows name the store (a row per grant).
        let mut relayed: Vec<String> = stores.iter().filter(|s| s.is_relayed()).map(|s| s.store_id.clone()).collect();
        relayed.sort();
        relayed.dedup();
        Ok(CloudListHostedStoresResponse {
            relayed,
            stores: stores
                .into_iter()
                .map(|s| CloudHostedStoreInfo {
                    root: s.scope_root(),
                    store_id: s.store_id,
                    name: s.name,
                    role: s.role,
                    kind: s.kind,
                    created_at: s.created_at,
                    shared_by: s.shared_by,
                })
                .collect(),
        })
    }

    async fn cloud_add_hosted_store(&self, ext: &Extensions, request: CloudAddHostedStoreRequest) -> Result<OpenStoreResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudAddHostedStore")?;
        let store_id = request.store_id;
        info!("Adding hosted store {} as a local replica", store_id);

        let account = self.keystore.account().await.ok_or_else(|| to_rpc_error("no Pimble Cloud account is signed in"))?;

        // How the account holds the store: the whole of it, or shares of it
        // (a row per grant, docs/NODE_DOCUMENT_CONTRACT.md section 5).
        let rows = crate::cloud::list_stores(&account.url, &account.session).await.map_err(to_rpc_error)?;
        // No row for it (an accounts service that lists nothing): held as
        // it always was, whole and writable.
        let held_as = crate::cloud::HeldAs::from_rows(&rows, store_id).unwrap_or_else(crate::cloud::HeldAs::owner);
        // A store shared from its owner's computer has no name on Pimble
        // Cloud (docs/RELAY_CONTRACT.md), and a store's name is in no
        // document: held whole (the owner's other device), it is called what
        // it is until the person renames it here.
        let name = held_as.name.clone().filter(|name| !name.trim().is_empty()).unwrap_or_else(|| {
            if rows.iter().any(|row| row.store_id == store_id.to_string() && row.is_relayed()) {
                RELAYED_PLACEHOLDER_NAME.to_string()
            } else {
                store_id.to_string()
            }
        });

        // Open here already: an error for a whole store, and for a share's
        // replica unless the account has been given another share of the
        // same store since, whose root joins the replica.
        let open_roots = {
            let manager = self.store_manager.read().await;
            manager.is_open(store_id).then(|| manager.scope_roots(store_id))
        };
        if let Some(open_roots) = &open_roots {
            let adds_a_root = !open_roots.is_empty() && held_as.roots.iter().any(|root| !open_roots.contains(root));
            if !adds_a_root {
                return Err(to_rpc_error(format!("store {} is already open here", store_id)));
            }
        }

        // The scope keys this account has been handed: the store key, or
        // each share's. An envelope is believed when the account itself or
        // one of the store's owners signed it.
        let key_id = crate::vault_link::fetch_scope_keys(self, &account, store_id, &held_as.roots)
            .await
            .map_err(to_rpc_error)?
            .ok_or_else(|| {
                to_rpc_error(if held_as.roots.is_empty() {
                    format!("no key envelopes for store {} on this account", store_id)
                } else {
                    format!(
                        "no key for this share has reached this account yet (store {}): it is handed over by one of the owner's devices the next time one is online",
                        store_id
                    )
                })
            })?;

        if open_roots.is_some() {
            // Another root for the replica that is here: the link is
            // restarted so that it lists again and pulls the new scope.
            {
                let mut manager = self.store_manager.write().await;
                for root in &held_as.roots {
                    manager.add_scope_root(store_id, *root).await.map_err(to_rpc_error)?;
                }
                // One share's replica carries that share's name; with
                // another it is "Shared by ...", as one added with both is.
                manager.set_partial_replica_name(store_id, &name).await.map_err(to_rpc_error)?;
            }
            self.stop_vault_link(store_id).await;
            let rpc_url = self.link_hosted_store(store_id, &account, key_id, &held_as).await?;
            self.ensure_vault_link_started(store_id, LinkEndpoint::Remote(rpc_url), key_id).await;
            let mut store = self.store_manager.read().await.get_store_info(store_id).map_err(to_rpc_error)?;
            // `Vault`, and relayed or not: `link_hosted_store` has just
            // written `sync.json`.
            self.describe_link(&mut store).await;
            self.mark_replica(&mut store);
            return Ok(OpenStoreResponse { store });
        }

        // An *empty* replica (never `create_local_store_with`, which would
        // give it its own freshly generated root): a vault store has
        // no plaintext root id to ask for ahead of time the way a `Plain`
        // remote's does for `addRemoteStore`, so this mirrors
        // `StoreManager::create_replica`'s own reasoning exactly — two
        // independently created roots for the same store id would merge
        // into a duplicated, disconnected tree once the vault link pulls
        // the real one. The placeholder root id here is manifest-only
        // bookkeeping, corrected below once the pull lands the real one. A
        // share's replica is partial: its roots are the shared nodes, known
        // now, and the first of them is the root older callers are shown.
        let path = self.default_replica_path(store_id);
        let created_id = {
            let mut manager = self.store_manager.write().await;
            if held_as.roots.is_empty() {
                manager.create_replica(&path, store_id, &name, NodeId::new()).await.map_err(to_rpc_error)?
            } else {
                manager.create_partial_replica(&path, store_id, &name, held_as.roots.clone()).await.map_err(to_rpc_error)?
            }
        };

        if !self.indexes.read().await.contains_key(&created_id) {
            if let Err(e) = self.open_index_for_store(created_id).await {
                warn!("Failed to open search index for store {}: {}", created_id, e);
            }
        }

        let rpc_url = self.link_hosted_store(created_id, &account, key_id, &held_as).await?;
        self.ensure_vault_link_started(created_id, LinkEndpoint::Remote(rpc_url), key_id).await;

        // Wait up to 10s for the first pull to land, same as
        // `addRemoteStore` (`create_replica_from`) does for a plain replica,
        // so the response's `root_node_id` reflects the real tree rather
        // than the placeholder above whenever the pull is fast enough.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let sync_state = loop {
            let state = self.sync_state_of(created_id).await;
            let is_synced = matches!(state, SyncState::Synced { .. });
            if is_synced || std::time::Instant::now() >= deadline {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        // The vault link rewrites the manifest root once the pull merges the
        // real tree in (`adopt_document_root`); do the same here so the
        // answer carries it even when the pull was fast. Read after
        // `sync.json` is written, so the answer says how the store is held
        // (`access`, `shared_by`, `roots`).
        self.adopt_document_root(created_id).await;
        let mut store = self.store_manager.read().await.get_store_info(created_id).map_err(to_rpc_error)?;
        // Always `Vault`: `link_hosted_store` above just wrote `sync.json`
        // with `mode: "vault"`, and with whether the store is relayed.
        self.describe_link(&mut store).await;
        store.sync_state = sync_state;
        self.mark_replica(&mut store);

        Ok(OpenStoreResponse { store })
    }

    /// Hosted side (docs/NODE_DOCUMENT_CONTRACT.md section 5): the accounts
    /// service deleting a hosted store reaches the ciphertext through this.
    /// The store is named by id and must be a vault open here; the
    /// directory removed is that open store's own, never a path a caller
    /// gives.
    async fn delete_vault_store(&self, ext: &Extensions, request: DeleteVaultStoreRequest) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "deleteVaultStore")?;
        let store_id = request.store_id;
        info!("Deleting vault store {}", store_id);

        self.store_manager.write().await.delete_vault_store(store_id).await.map_err(to_rpc_error)?;
        // Its subscribers hold sinks to a store that is gone.
        self.subscriptions.write().await.remove_store(store_id);

        Ok(EmptyResponse {})
    }

    // ── Sharing on node documents, docs/NODE_DOCUMENT_CONTRACT.md section 5 ──

    async fn vault_set_doc_keys(&self, ext: &Extensions, request: VaultSetDocKeysRequest) -> Result<EmptyResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        debug!("Vault set keys for store {} doc {:?} (dek {})", request.store_id, request.doc_id, request.keys.dek_id);

        let mut manager = self.store_manager.write().await;
        self.require_vault_doc_in_scope(&manager, &principal, request.store_id, &request.doc_id, Access::Write)?;

        // Wraps of the same data key are merged by the scope key that wraps
        // (the owner's devices add the store key's wrap to a document a
        // recipient created, and a share's wrap to every document entering
        // it); a different data key is a rotation and replaces the record.
        let doc_id = request.doc_id.as_str();
        let held = manager
            .vault_doc_keys(request.store_id, &doc_id)
            .map_err(to_rpc_error)?
            .and_then(|record| serde_json::from_str::<VaultDocKeys>(&record.json).ok());
        let mut keys = request.keys;
        if let Some(held) = held.filter(|held| held.dek_id == keys.dek_id) {
            for wrap in held.wraps {
                if !keys.wraps.iter().any(|w| w.scope_key_id == wrap.scope_key_id) {
                    keys.wraps.push(wrap);
                }
            }
        }
        let json = serde_json::to_string(&keys).map_err(to_rpc_error)?;
        manager.vault_set_doc_keys(request.store_id, &doc_id, json).await.map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn set_scope(&self, ext: &Extensions, request: SetScopeRequest) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_owner(&principal_of(ext), request.store_id, "setScope")?;
        debug!("Set scope {} of store {}: {} document(s), remove: {}", request.scope.root, request.store_id, request.scope.doc_ids.len(), request.remove);

        let mut manager = self.store_manager.write().await;
        match manager.store_kind(request.store_id) {
            Some(StoreKind::Vault) => {}
            // A plain store's scopes are read off its own tree; there is
            // nothing to publish, and accepting a set would be keeping a
            // copy of the tree beside the tree.
            Some(StoreKind::Plain) => return Ok(EmptyResponse {}),
            None => return Err(to_rpc_error(StoreError::NotOpen(request.store_id))),
        }
        let docs = (!request.remove).then_some(request.scope.doc_ids);
        manager.vault_set_scope(request.store_id, request.scope.root, docs).await.map_err(to_rpc_error)?;

        Ok(EmptyResponse {})
    }

    async fn get_scopes(&self, ext: &Extensions, request: GetScopesRequest) -> Result<GetScopesResponse, ErrorObjectOwned> {
        authorize_owner(&principal_of(ext), request.store_id, "getScopes")?;

        let manager = self.store_manager.read().await;
        let mut scopes: Vec<Scope> = match manager.store_kind(request.store_id) {
            Some(StoreKind::Vault) => manager
                .vault_scopes(request.store_id)
                .map_err(to_rpc_error)?
                .into_iter()
                .map(|(root, doc_ids)| Scope { root, doc_ids })
                .collect(),
            // A plain store's shares are the nodes that carry a share
            // marker, and each one's scope is what a member scoped to it
            // reaches: computed here, stored nowhere.
            Some(StoreKind::Plain) => {
                let tree = manager.tree(request.store_id).map_err(to_rpc_error)?;
                tree.list_node_ids()
                    .into_iter()
                    .filter(|id| tree.get_node_info(*id).is_ok_and(|info| info.custom.contains_key(pimble_core::custom_keys::SHARE)))
                    .map(|root| Scope { root, doc_ids: plain_scope(tree, &[root]).into_iter().collect() })
                    .collect()
            }
            None => return Err(to_rpc_error(StoreError::NotOpen(request.store_id))),
        };
        // Deterministic, so two reads of an unchanged store compare equal.
        scopes.sort_by_key(|scope| scope.root.to_string());
        for scope in &mut scopes {
            scope.doc_ids.sort_by_key(|id| id.to_string());
        }

        Ok(GetScopesResponse { scopes })
    }

    async fn cloud_share_node(&self, ext: &Extensions, request: CloudShareNodeRequest) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudShareNode")?;
        self.share_node(request).await
    }

    async fn cloud_share_info(&self, ext: &Extensions, request: CloudShareRef) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudShareInfo")?;
        self.share_info(request).await
    }

    async fn cloud_share_invite(&self, ext: &Extensions, request: CloudShareInviteRequest) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudShareInvite")?;
        self.share_invite(request).await
    }

    async fn cloud_share_remove_member(&self, ext: &Extensions, request: CloudShareRemoveMemberRequest) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudShareRemoveMember")?;
        self.share_remove_member(request).await
    }

    async fn cloud_stop_sharing(&self, ext: &Extensions, request: CloudShareRef) -> Result<EmptyResponse, ErrorObjectOwned> {
        authorize_service_only(&principal_of(ext), "cloudStopSharing")?;
        self.stop_sharing(request).await
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
            self.start_link_from_config(store_id, config).await;
        }

        // Decision 9: repair a store's tree when it opens (a store closed
        // mid-repair, or reopened straight from disk after a crash, may
        // still be carrying an issue nothing has fixed yet).
        self.repair_store_tree(store_id).await;

        // `store` was read before the link started; starting a vault link
        // may have just corrected the manifest root (`adopt_document_root`),
        // and the answer must carry the root the app will ask for.
        if let Ok(root) = self.store_manager.read().await.root_node_id(store_id) {
            store.root_node_id = root;
        }

        self.describe_link(&mut store).await;
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
        self.shares.forget_store(request.store_id);
        // Shared from this computer (docs/RELAY_CONTRACT.md): a store that is
        // closed here is withdrawn from the relay and its twin closed with
        // it. Members are told `owner offline` until it is open again.
        if self.relay.is_relaying(request.store_id).await {
            self.relay.withdraw(request.store_id, false).await;
        }
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

        let principal = principal_of(ext);
        let manager = self.store_manager.read().await;
        let store_ids = readable(&principal, manager.list_stores().iter());

        let mut stores = Vec::new();
        for id in store_ids {
            if let Ok(mut store) = manager.get_store_info(id) {
                store.sync_state = self.sync_state_of(id).await;
                (store.sync_mode, store.relay) = self.link_kind_of_locked(&manager, id).await;
                self.mark_replica(&mut store);
                Self::present_store_to(&principal, &mut store);
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
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
        debug!("Getting node {} from store {}", request.node_id, request.store_id);

        let manager = self.store_manager.read().await;
        let reach = Reach::of(&manager, &principal, request.store_id);
        require_in_scope(&reach, request.node_id, Access::Read)?;
        let mut node = manager
            .get_node(request.store_id, request.node_id)
            .map_err(to_rpc_error)?;
        node.access = judge_access(&manager, &principal, &reach, request.store_id, node.id);

        Ok(GetNodeResponse { node })
    }

    async fn get_nodes(
        &self,
        ext: &Extensions,
        request: GetNodesRequest,
    ) -> Result<GetNodesResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
        debug!(
            "Getting {} nodes from store {}",
            request.node_ids.len(),
            request.store_id
        );

        let manager = self.store_manager.read().await;
        let reach = Reach::of(&manager, &principal, request.store_id);
        let mut nodes = Vec::new();

        for node_id in request.node_ids {
            // Left out like a node that is not there, which is all this
            // call ever says about one it cannot return.
            if require_in_scope(&reach, node_id, Access::Read).is_err() {
                continue;
            }
            match manager.get_node(request.store_id, node_id) {
                Ok(mut node) => {
                    node.access = judge_access(&manager, &principal, &reach, request.store_id, node.id);
                    nodes.push(node);
                }
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
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
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
        // The parent is the document a create edits; the new node is under
        // it, so in every scope the parent is in.
        require_in_scope(&Reach::of(&manager, &principal, request.store_id), parent_id, Access::Write)?;
        if manager.write_refused(request.store_id, &[parent_id]) {
            return Err(read_only_error());
        }
        let (node_id, edit) = manager
            .create_node(request.store_id, node, Some(parent_id))
            .map_err(to_rpc_error)?;

        // A node that is created and never edited again has no other flush
        // point; without this it exists only in memory until shutdown.
        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        self.broadcast_tree_edit(request.store_id, &edit, Some((node_id, StoreChangeKind::NodeCreated { node_id, parent_id })), None).await;
        self.enqueue_index_event(request.store_id, IndexEvent::Upsert(node_id)).await;

        Ok(CreateNodeResponse { node_id })
    }

    async fn update_node_metadata(
        &self,
        ext: &Extensions,
        request: UpdateNodeMetadataRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        debug!(
            "Updating metadata for node {} in store {}",
            request.node_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;
        require_in_scope(&Reach::of(&manager, &principal, request.store_id), request.node_id, Access::Write)?;
        if manager.write_refused(request.store_id, &[request.node_id]) {
            return Err(read_only_error());
        }
        let edit = manager
            .update_node_metadata(request.store_id, request.node_id, request.metadata)
            .map_err(to_rpc_error)?;
        self.publish_metadata_edit(manager, request.store_id, request.node_id, edit).await?;

        Ok(EmptyResponse {})
    }

    async fn update_node_content(
        &self,
        ext: &Extensions,
        request: UpdateNodeContentRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        info!(
            "Updating content for node {} in store {}",
            request.node_id, request.store_id
        );
        require_in_scope(&self.reach_of(&principal, request.store_id).await, request.node_id, Access::Write)?;
        self.reject_if_read_only(request.store_id, &[request.node_id]).await?;

        let content = base64::engine::general_purpose::STANDARD
            .decode(&request.content)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;
        let (store_id, node_id) = (request.store_id, request.node_id);

        // A merge like any other, so the snapshot's bytes ride the
        // notification as an update a subscriber applies; and a person's
        // write, so the node is stamped, here and now rather than on the
        // flush debounce, since this is one call and not a keystroke.
        let stamp = {
            let mut manager = self.store_manager.write().await;
            let effect = manager.update_node_content(store_id, node_id, content).map_err(to_rpc_error)?;
            if !effect.changed {
                return Ok(EmptyResponse {});
            }
            let stamp = manager
                .tree_mut(store_id)
                .map_err(to_rpc_error)?
                .touch_modified(node_id, &now())
                .map_err(to_rpc_error)?;
            for id in stamp.node_ids() {
                manager.mark_dirty(store_id, id).map_err(to_rpc_error)?;
            }
            manager.flush(store_id).await.map_err(to_rpc_error)?;
            stamp
        };

        self.broadcast_document_change(store_id, StoreChangeKind::ContentUpdated { node_id }, request.client_id.as_deref(), Some(request.content))
            .await;
        self.broadcast_tree_edit(store_id, &stamp, Some((node_id, StoreChangeKind::MetadataUpdated { node_id })), None).await;
        self.enqueue_index_event(store_id, IndexEvent::ContentChanged(node_id)).await;

        Ok(EmptyResponse {})
    }

    async fn delete_node(
        &self,
        ext: &Extensions,
        request: DeleteNodeRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        info!(
            "Deleting node {} from store {}",
            request.node_id, request.store_id
        );

        // A share rooted in what is about to go is stopped first, best
        // effort (`crate::share`): its members and its scope would
        // otherwise outlive the node they are of. Judged before anything
        // is asked of Pimble Cloud, and again under the lock the deletion
        // is made under.
        let shared_roots = {
            let manager = self.store_manager.read().await;
            Self::may_delete(&manager, &principal, request.store_id, request.node_id)?;
            crate::share::shared_roots_under(&manager, request.store_id, request.node_id)
        };
        if !shared_roots.is_empty() {
            self.stop_shares_before_delete(request.store_id, request.node_id, shared_roots).await;
        }

        let mut manager = self.store_manager.write().await;
        Self::may_delete(&manager, &principal, request.store_id, request.node_id)?;
        let (removal, edit) = manager
            .delete_node(request.store_id, request.node_id)
            .map_err(to_rpc_error)?;

        // Flushed at once like every other tree RPC: until 2026-09-14 this
        // one was not, and a deletion lived only in memory until something
        // else happened to flush the store, so a hard kill brought a deleted
        // node (a mount, in the bug report) back on restart.
        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        // A deleted subtree may contain mount nodes; stop reporting their
        // source's state to a client that no longer has them.
        self.forget_mounts(request.store_id, &removal.removed);
        // The node's own tombstone is the `NodeDeleted`; every descendant's
        // tombstone and the parent's list go as `TreeStructure`.
        let node_id = request.node_id;
        self.broadcast_tree_edit(
            request.store_id,
            &edit,
            Some((node_id, StoreChangeKind::NodeDeleted { node_id, parent_id: removal.parent_id })),
            None,
        )
        .await;
        for node_id in removal.removed {
            self.enqueue_index_event(request.store_id, IndexEvent::Remove(node_id)).await;
        }

        Ok(EmptyResponse {})
    }

    async fn undelete_node(
        &self,
        ext: &Extensions,
        request: UndeleteNodeRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        info!("Undeleting node {} in store {}", request.node_id, request.store_id);
        let (store_id, node_id) = (request.store_id, request.node_id);

        let (edit, parent_id, restored) = {
            let mut manager = self.store_manager.write().await;
            // The tombstone and the list it goes back into, like a delete.
            if let Some(reach) = Reach::of(&manager, &principal, store_id) {
                reach.require(node_id, Access::Write)?;
                let stored_parent = manager.tree(store_id).ok().and_then(|tree| tree.doc(node_id)?.fields().ok()?.parent_id);
                if let Some(parent_id) = stored_parent {
                    reach.require(parent_id, Access::Write)?;
                }
            }
            if manager.write_refused(store_id, &[node_id]) {
                return Err(read_only_error());
            }
            let edit = manager.undelete_node(store_id, node_id).map_err(to_rpc_error)?;
            let node = manager.get_node(store_id, node_id).map_err(to_rpc_error)?;
            let parent_id = node.parent_id.unwrap_or_else(|| manager.root_node_id(store_id).unwrap_or(node_id));
            let restored = manager.tree(store_id).map_err(to_rpc_error)?.subtree_ids(node_id).unwrap_or_else(|_| vec![node_id]);
            manager.flush(store_id).await.map_err(to_rpc_error)?;
            (edit, parent_id, restored)
        };

        // Back under its parent: `NodeCreated` is what a subscriber does
        // about a node it does not have appearing in a list.
        self.broadcast_tree_edit(store_id, &edit, Some((node_id, StoreChangeKind::NodeCreated { node_id, parent_id })), None).await;
        for id in restored {
            self.enqueue_index_event(store_id, IndexEvent::Upsert(id)).await;
        }

        Ok(EmptyResponse {})
    }

    async fn move_node(
        &self,
        ext: &Extensions,
        request: MoveNodeRequest,
    ) -> Result<EmptyResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;
        info!(
            "Moving node {} to parent {} in store {}",
            request.node_id, request.new_parent_id, request.store_id
        );

        let mut manager = self.store_manager.write().await;
        let reach = Reach::of(&manager, &principal, request.store_id);
        require_in_scope(&reach, request.node_id, Access::Write)?;
        // The old parent is read off the tree before the move rewrites it.
        let old_parent_id = manager
            .get_node(request.store_id, request.node_id)
            .map_err(to_rpc_error)?
            .parent_id
            .ok_or_else(|| to_rpc_error("Cannot move the root node"))?;
        // A move edits three documents, and both parents are among them: a
        // member moves within what they may write, never into or out of it
        // (a parent outside the scope is not theirs to edit).
        require_in_scope(&reach, old_parent_id, Access::Write)?;
        require_in_scope(&reach, request.new_parent_id, Access::Write)?;
        if manager.write_refused(request.store_id, &[request.node_id, old_parent_id, request.new_parent_id]) {
            return Err(read_only_error());
        }
        let edit = manager
            .move_node(request.store_id, request.node_id, request.new_parent_id, request.position)
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        drop(manager);
        let node_id = request.node_id;
        self.broadcast_tree_edit(
            request.store_id,
            &edit,
            Some((node_id, StoreChangeKind::NodeMoved { node_id, old_parent_id, new_parent_id: request.new_parent_id })),
            None,
        )
        .await;
        // Re-upsert the moved node: its `parent` relationship is what changed.
        self.enqueue_index_event(request.store_id, IndexEvent::Upsert(node_id)).await;

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
            let manager = self.store_manager.read().await;
            require_in_scope(&Reach::of(&manager, &principal, request.store_id), request.node_id, Access::Read)?;
            let node = manager
                .get_node(request.store_id, request.node_id)
                .map_err(to_rpc_error)?;
            node.mount_ref().filter(|_| node.is_mount())
        };

        if let Some(mount_ref) = mount_ref {
            // A mount's children are its source's; a principal that can't
            // read the source has no business seeing them just because it
            // can read the mounting store (docs/CLOUD_CONTRACT.md "B:
            // pimble-server" item 5), and one scoped in the source store
            // reaches the mounted node only when it is in that scope.
            authorize(&principal, mount_ref.source_store, Access::Read)?;
            require_in_scope(&self.reach_of(&principal, mount_ref.source_store).await, mount_ref.source_node, Access::Read)?;
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
        let (store_id, mut children) = manager
            .get_children(request.store_id, request.node_id)
            .await
            .map_err(to_rpc_error)?;
        // Children outside the caller's scope (in the store they live in,
        // a mount's source for a mount) are left out: a member never learns
        // another document's id.
        let reach = Reach::of(&manager, &principal, store_id);
        if let Some(reach) = &reach {
            children.retain(|child| reach.readable.contains(&child.id));
        }
        // Each child is judged in the store it lives in: through a mount
        // that is the source store, which is where a write of it would go.
        for child in &mut children {
            child.access = judge_access(&manager, &principal, &reach, store_id, child.id);
        }
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
        // A create under `parent_id` like any other, of a node that shows
        // `source_node`: each in the caller's scope in its own store.
        require_in_scope(&Reach::of(&manager, &principal, request.store_id), request.parent_id, Access::Write)?;
        require_in_scope(&Reach::of(&manager, &principal, request.source_store_id), request.source_node_id, Access::Read)?;
        if manager.write_refused(request.store_id, &[request.parent_id]) {
            return Err(read_only_error());
        }

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

        let (node_id, edit) = manager
            .create_node(request.store_id, node, Some(request.parent_id))
            .map_err(to_rpc_error)?;

        manager
            .flush(request.store_id)
            .await
            .map_err(to_rpc_error)?;

        let newly_opened = manager.opened_since();
        drop(manager);

        self.adopt_newly_opened(newly_opened).await;
        self.broadcast_tree_edit(request.store_id, &edit, Some((node_id, StoreChangeKind::NodeCreated { node_id, parent_id: request.parent_id })), None).await;
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

        // A store shared from this computer has a twin on this machine, a
        // record on the accounts service and a place in the tunnel, none of
        // which unlinking or linking elsewhere would take down.
        if self.link_kind_of(request.store_id).await.1 == RelaySide::Owner {
            return Err(to_rpc_error(crate::relay_face::RELAYED_LINK_REFUSAL));
        }

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
                    .write_sync_config(request.store_id, &SyncConfig { remote: Self::without_auth(&remote), last_sync: None, mode: pimble_store::SyncMode::Sync, via_relay: false, last_seq: Default::default(), vault_key_id: None, access: StoreAccess::Full, shared_by: None, read_only_roots: Vec::new() })
                    .await
                    .map_err(to_rpc_error)?;
                drop(manager);

                // Replace any existing link so it points at the new remote.
                self.stop_link(request.store_id).await;
                self.stop_vault_link(request.store_id).await;
                crate::vault_link::forget_progress(self, request.store_id).await;
                self.ensure_link_started(request.store_id, remote).await;
            }
            None => {
                // Whichever kind of link the store has: until 2026-09-17 only
                // the plain one was stopped, so "Unlink from Remote" on a
                // hosted store left its vault link running against a
                // `sync.json` that no longer existed.
                self.stop_link(request.store_id).await;
                self.stop_vault_link(request.store_id).await;
                crate::vault_link::forget_progress(self, request.store_id).await;
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
        let (sync_mode, relay) = self.link_kind_of(request.store_id).await;

        // `Service` only, and a store just linked or unlinked this way is
        // held whole: `sync.json` says `full` or is gone.
        Ok(GetStoreSyncResponse { remote: remote_now, state, sync_mode, access: pimble_core::StoreAccess::Full, read_only_roots: Vec::new(), relay })
    }

    async fn get_store_sync(
        &self,
        ext: &Extensions,
        request: GetStoreSyncRequest,
    ) -> Result<GetStoreSyncResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Read)?;
        let manager = self.store_manager.read().await;
        let remote = manager
            .read_sync_config(request.store_id)
            .await
            .map_err(to_rpc_error)?
            .map(|c| Self::without_auth(&c.remote));
        // What this device may change (`sync.json`), narrowed to what the
        // caller's role may, as `Store::access` is everywhere.
        let mut store = manager.get_store_info(request.store_id).map_err(to_rpc_error)?;
        drop(manager);
        Self::present_store_to(&principal, &mut store);
        let state = self.sync_state_of(request.store_id).await;
        let (sync_mode, relay) = self.link_kind_of(request.store_id).await;

        Ok(GetStoreSyncResponse { remote, state, sync_mode, access: store.access, read_only_roots: store.read_only_roots, relay })
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
            let manager = self.store_manager.read().await;
            require_in_scope(&Reach::of(&manager, &principal, request.store_id), request.node_id, Access::Read)?;
            manager
                .get_node(request.store_id, request.node_id)
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

    async fn sync_nodes(&self, ext: &Extensions, request: SyncNodesRequest) -> Result<SyncNodesResponse, ErrorObjectOwned> {
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Read)?;
        self.reject_if_vault(request.store_id).await?;
        debug!(
            "Sync {} node document(s) in store {} (list unknown: {})",
            request.nodes.len(), request.store_id, request.list_unknown
        );

        let b64 = base64::engine::general_purpose::STANDARD;

        if request.nodes.len() > MAX_SYNC_NODE_CONTENTS {
            return Err(to_rpc_error(format!(
                "syncNodes takes at most {} nodes per request, got {}",
                MAX_SYNC_NODE_CONTENTS,
                request.nodes.len()
            )));
        }

        let manager = self.store_manager.read().await;
        if !manager.is_open(request.store_id) {
            return Err(to_rpc_error(StoreError::NotOpen(request.store_id)));
        }

        // Stateless reconciliation: no per-client sync state is kept. For
        // each document the client names it sends its state vector, and
        // gets back everything this store has beyond it plus this store's
        // own state vector; a document this store does not hold is left
        // out. Tombstones are documents like any other: a deletion is in
        // the document, and a peer that never hears of it brings the node
        // back.
        //
        // A scoped member reconciles its scope: a document outside it is
        // left out exactly as one this store does not hold, and is never
        // listed as unknown.
        let reach = Reach::of(&manager, &principal, request.store_id);
        let mut named: HashSet<NodeId> = HashSet::with_capacity(request.nodes.len());
        let mut nodes = Vec::with_capacity(request.nodes.len());
        for entry in request.nodes {
            named.insert(entry.node_id);
            if require_in_scope(&reach, entry.node_id, Access::Read).is_err() {
                continue;
            }
            let client_sv = b64
                .decode(&entry.state_vector)
                .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;
            let diff = match manager.node_diff_since(request.store_id, entry.node_id, &client_sv) {
                Ok(diff) => diff,
                Err(StoreError::NodeNotFound(_)) => continue,
                Err(e) => return Err(to_rpc_error(e)),
            };
            let state_vector = manager.node_state_vector(request.store_id, entry.node_id).map_err(to_rpc_error)?;
            nodes.push(NodeContentDiff {
                node_id: entry.node_id,
                diff: b64.encode(&diff),
                state_vector: b64.encode(&state_vector),
            });
        }

        // What the caller did not name is what it does not hold at all (a
        // fresh replica: everything), for it to ask for next.
        let unknown_ids = if request.list_unknown {
            manager
                .doc_ids(request.store_id)
                .map_err(to_rpc_error)?
                .into_iter()
                .filter(|id| !named.contains(id))
                .filter(|id| reach.as_ref().is_none_or(|reach| reach.readable.contains(id)))
                .collect()
        } else {
            Vec::new()
        };

        Ok(SyncNodesResponse { nodes, unknown_ids })
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
        let principal = principal_of(ext);
        authorize(&principal, request.store_id, Access::Write)?;
        self.reject_if_vault(request.store_id).await?;

        let EditOperation::IncrementalChanges { ref changes } = request.operation;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(changes)
            .map_err(|e| to_rpc_error(format!("Invalid base64: {}", e)))?;

        {
            let manager = self.store_manager.read().await;
            let held = manager.tree(request.store_id).is_ok_and(|tree| tree.doc(request.node_id).is_some());
            // A document the store holds is judged by where it is; one it
            // does not hold yet, by the parent its first update names (a
            // create is the node's document and the parent's list, and a
            // member may make one under a parent they may write).
            let judged_by = if held { Some(request.node_id) } else { parent_named_by(&bytes) };
            if let Some(reach) = Reach::of(&manager, &principal, request.store_id) {
                match judged_by {
                    Some(id) => reach.require(id, Access::Write)?,
                    None => return Err(no_grant_for_document_error()),
                }
            }
            // A relayed edit is not this device's write: a reader's replica
            // receives everything (`is_link_client`).
            if !is_link_client(&request.client_id) && judged_by.is_some_and(|id| manager.write_refused(request.store_id, &[id])) {
                return Err(read_only_error());
            }
        }

        // Merged, persisted on the flush debounce, broadcast with the same
        // bytes (never re-encoded or reinterpreted), indexed, repaired: all
        // in one place, shared with the links.
        let effect = self
            .apply_node_update_from(request.store_id, request.node_id, &bytes, Some(&request.client_id), Repair::Debounced)
            .await?;

        // A person's content edit stamps the node's `modified_at`: as a tree
        // edit of the server's own, coalesced with the flush so a burst of
        // keystrokes is one stamp, not one each. A link relaying a peer's
        // edit is not a person: the stamp was made where the edit was, and
        // travels as its own update.
        if (effect.content || effect.data) && !is_link_client(&request.client_id) {
            self.note_content_edit(request.store_id, request.node_id);
        }

        Ok(ApplyEditResponse {})
    }

    async fn subscribe_store_changes(
        &self,
        pending: PendingSubscriptionSink,
        ext: &Extensions,
        store_id: StoreId,
    ) -> SubscriptionResult {
        let principal = principal_of(ext);
        if let Err(e) = authorize(&principal, store_id, Access::Read) {
            pending.reject(e).await;
            return Ok(());
        }
        // Unlike every other store-scoped RPC, `subscribeStoreChanges` works
        // on a vault store: it's how a live client hears `VaultAppended`
        // without re-fetching (docs/CRYPTO_CONTRACT.md).
        info!("Client subscribing to store changes for {}", store_id);

        // A share's member subscribes with the roots of their grant, and
        // each notification is judged against those roots' scopes as they
        // are when it is sent (`delivery_for`).
        let roots = scope_roots_of(&principal, store_id, Access::Read);
        let sink = pending.accept().await?;
        self.subscriptions.write().await.add_store_sub(store_id, sink, roots);

        Ok(())
    }

    async fn subscribe_node_changes(
        &self,
        pending: PendingSubscriptionSink,
        ext: &Extensions,
        store_id: StoreId,
        node_id: NodeId,
    ) -> SubscriptionResult {
        let principal = principal_of(ext);
        if let Err(e) = authorize(&principal, store_id, Access::Read) {
            pending.reject(e).await;
            return Ok(());
        }
        if let Err(e) = self.reject_if_vault(store_id).await {
            pending.reject(e).await;
            return Ok(());
        }
        if let Err(e) = require_in_scope(&self.reach_of(&principal, store_id).await, node_id, Access::Read) {
            pending.reject(e).await;
            return Ok(());
        }
        info!("Client subscribing to node changes for {}:{}", store_id, node_id);

        // In scope now; every delivery checks again, so a node moved out of
        // the share stops reaching this subscriber.
        let roots = scope_roots_of(&principal, store_id, Access::Read);
        let sink = pending.accept().await?;
        self.subscriptions.write().await.add_node_sub(store_id, node_id, sink, roots);

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

        // What the principal reaches of each store it is scoped in: a hit
        // outside it is dropped, and since the index ranks the whole store,
        // such a store is asked for more than `limit` so that a small share
        // of a large store still fills its page.
        let reaches: HashMap<StoreId, Reach> = {
            let manager = self.store_manager.read().await;
            store_ids.iter().filter_map(|id| Some((*id, Reach::of(&manager, &principal, *id)?))).collect()
        };

        let mut hits: Vec<(StoreId, pimble_search::SearchHit)> = Vec::new();
        {
            let indexes = self.indexes.read().await;
            for store_id in &store_ids {
                let Some(handle) = indexes.get(store_id) else {
                    continue; // no index open for this store; nothing to search
                };
                let reach = reaches.get(store_id);
                let scoped_query;
                let query = match reach {
                    Some(_) => {
                        scoped_query = SearchQuery { limit: limit.saturating_mul(SCOPED_SEARCH_OVERFETCH), ..query.clone() };
                        &scoped_query
                    }
                    None => &query,
                };
                match handle.index.search(query) {
                    Ok(store_hits) => hits.extend(
                        store_hits
                            .into_iter()
                            .filter(|hit| reach.is_none_or(|reach| reach.readable.contains(&hit.node_id)))
                            .map(|h| (*store_id, h)),
                    ),
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
        let manager = self.store_manager.read().await;
        let mut results = Vec::with_capacity(hits.len());
        for (store_id, hit) in &hits {
            let node_type = manager
                .get_node(*store_id, hit.node_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_crdt::Tree;

    const T0: &str = "2026-09-18T10:00:00Z";
    const T1: &str = "2026-09-18T10:00:01Z";

    fn replica_of(tree: &Tree) -> Tree {
        let docs = tree.ids().into_iter().map(|id| (id, NodeDoc::load(&tree.doc(id).unwrap().save()).unwrap())).collect();
        Tree::from_docs(tree.root(), docs)
    }

    /// A tree edit that wrote one document in several transactions (a
    /// create, then its tags and a custom key) is broadcast as one update
    /// per document, and a peer merging the merged update ends up exactly
    /// where it would applying each transaction in turn.
    #[test]
    fn coalesce_edit_merges_a_documents_transactions_into_one_update() {
        let root = NodeId::new();
        let mut tree = Tree::new(root, "Store", T0).unwrap();
        let mut by_turns = replica_of(&tree);
        let mut merged = replica_of(&tree);

        let x = NodeId::new();
        let mut edit = tree.add_node(x, Some(root), None, "document", "X", T0).unwrap();
        edit.touched.extend(tree.set_tags(x, &["a".into(), "b".into()], T1).unwrap().touched);
        edit.touched.extend(tree.set_custom(x, "icon", &serde_json::json!("star"), T1).unwrap().touched);
        edit.touched.extend(tree.set_title(x, "Renamed", T1).unwrap().touched);
        assert_eq!(edit.touched.iter().filter(|(id, _)| *id == x).count(), 4);

        for (id, update) in &edit.touched {
            by_turns.apply_update(*id, update).unwrap();
        }
        let coalesced = coalesce_edit(&edit);
        assert_eq!(coalesced.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![x, root], "each once, first touched first");
        for (id, update) in &coalesced {
            assert!(merged.apply_update(*id, update).unwrap().changed);
        }

        for id in [x, root] {
            let a = by_turns.doc(id).unwrap();
            let b = merged.doc(id).unwrap();
            assert_eq!(a.fields().ok(), b.fields().ok(), "document {id}");
            assert_eq!(a.children(), b.children(), "document {id}");
            assert_eq!(a.state_vector(), b.state_vector(), "document {id}");
        }
        let fields = merged.doc(x).unwrap().fields().unwrap();
        assert_eq!(fields.title, "Renamed");
        assert_eq!(fields.tags, vec!["a", "b"]);
        assert_eq!(fields.custom.get("icon"), Some(&serde_json::json!("star")));
        assert_eq!(merged.get_children(root).unwrap(), vec![x]);
    }

    fn shapes(before: &Tree, after: &Tree, id: NodeId) -> (Option<DocShape>, Option<DocShape>) {
        (shape_of(before, id), shape_of(after, id))
    }

    fn structural() -> NodeUpdateEffect {
        NodeUpdateEffect { changed: true, structure: true, content: false, data: false }
    }

    #[test]
    fn derive_kinds_names_what_a_merge_changed() {
        let root = NodeId::new();
        let mut tree = Tree::new(root, "Store", T0).unwrap();
        let (p, q) = (NodeId::new(), NodeId::new());
        tree.add_node(p, Some(root), None, "folder", "P", T0).unwrap();
        tree.add_node(q, Some(root), None, "folder", "Q", T0).unwrap();
        let x = NodeId::new();

        // Created: the node is new, the parent's list changed.
        let before = replica_of(&tree);
        tree.add_node(x, Some(p), None, "document", "X", T0).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert!(matches!(derive_kinds(x, root, b.as_ref(), a.as_ref(), structural())[..], [StoreChangeKind::NodeCreated { node_id, parent_id }] if node_id == x && parent_id == p));
        let (b, a) = shapes(&before, &tree, p);
        assert!(matches!(derive_kinds(p, root, b.as_ref(), a.as_ref(), structural())[..], [StoreChangeKind::TreeStructure { ref node_ids }] if node_ids == &vec![p]));

        // Moved.
        let before = replica_of(&tree);
        tree.move_node(x, q, None, T1).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert!(matches!(
            derive_kinds(x, root, b.as_ref(), a.as_ref(), structural())[..],
            [StoreChangeKind::NodeMoved { node_id, old_parent_id, new_parent_id }] if node_id == x && old_parent_id == p && new_parent_id == q
        ));

        // Renamed.
        let before = replica_of(&tree);
        tree.set_title(x, "Y", T1).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert!(matches!(derive_kinds(x, root, b.as_ref(), a.as_ref(), structural())[..], [StoreChangeKind::MetadataUpdated { node_id }] if node_id == x));

        // Content, on top of nothing structural.
        let content = NodeUpdateEffect { changed: true, structure: false, content: true, data: false };
        let (b, a) = shapes(&tree, &tree, x);
        assert!(matches!(derive_kinds(x, root, b.as_ref(), a.as_ref(), content)[..], [StoreChangeKind::ContentUpdated { node_id }] if node_id == x));

        // Deleted, and back.
        let before = replica_of(&tree);
        tree.remove_node(x, T1).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert!(matches!(derive_kinds(x, root, b.as_ref(), a.as_ref(), structural())[..], [StoreChangeKind::NodeDeleted { node_id, parent_id }] if node_id == x && parent_id == q));
        let before = replica_of(&tree);
        tree.undelete_node(x, T1).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert!(matches!(derive_kinds(x, root, b.as_ref(), a.as_ref(), structural())[..], [StoreChangeKind::NodeCreated { node_id, parent_id }] if node_id == x && parent_id == q));

        // A tombstone arriving whole on a replica that never had the node.
        let mut fresh = Tree::from_docs(root, HashMap::new());
        tree.remove_node(x, T1).unwrap();
        fresh.apply_update(x, &tree.doc(x).unwrap().save()).unwrap();
        let (b, a) = (None, shape_of(&fresh, x));
        assert!(matches!(derive_kinds(x, root, b, a.as_ref(), structural())[..], [StoreChangeKind::NodeDeleted { node_id, parent_id }] if node_id == x && parent_id == q));

        // The root arriving on an empty replica: no parent to name.
        fresh.apply_update(root, &tree.doc(root).unwrap().save()).unwrap();
        let a = shape_of(&fresh, root);
        assert!(matches!(derive_kinds(root, root, None, a.as_ref(), structural())[..], [StoreChangeKind::TreeStructure { ref node_ids }] if node_ids == &vec![root]));

        // A structural change that reads the same still earns bytes to forward.
        let (b, a) = shapes(&tree, &tree, root);
        assert!(matches!(derive_kinds(root, root, b.as_ref(), a.as_ref(), structural())[..], [StoreChangeKind::TreeStructure { .. }]));
    }

    #[test]
    fn link_clients_are_told_apart_by_their_id() {
        assert!(is_link_client("sync-link:1234"));
        assert!(is_link_client("vault-link:1234"));
        assert!(!is_link_client("pimble-cli"));
        assert!(!is_link_client("app-7f3a"));
    }
}
