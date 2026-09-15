//! Replica sync link: keeps one local store's tree and node content merged
//! with the same store held by a remote Pimble server (`docs/SYNC_CONTRACT.md`).
//!
//! One [`SyncLink`] per linked store, owned by `RpcHandler::links`. It never
//! touches `LocalStore` directly for writes: a remote change is merged in
//! through the handler's own `apply_edit`/`apply_store_update` (the same
//! entry points any client uses), so local subscribers, the search index,
//! and persistence all behave exactly as if an ordinary client had sent the
//! change, with `client_id = "sync-link:<uuid>"`.
//!
//! Reconcile procedure (decision 5): for the store document,
//! `(d_r, sv_r) = remote.syncStoreDocument(sv_local)`; apply `d_r` locally
//! if not empty; `d_l = local diff since sv_r`; `remote.applyStoreUpdate(d_l)`
//! if not empty. A node's content document reconciles the same way via
//! `syncNodeContent`/`applyEdit`. A full reconcile does the store document
//! first, then every node id in the (now up to date) local store document.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, RemoteEndpoint, StoreId, SyncState};
use pimble_rpc::{
    ApplyEditRequest, ApplyStoreUpdateRequest, EditOperation, PimbleApiServer, StoreChangeKind,
    StoreChangedNotification, MAX_SYNC_NODE_CONTENTS,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::handler::{LocalChange, RpcHandler};

/// Initial retry backoff on a dropped or failed connection; doubles up to
/// [`MAX_BACKOFF`] (decision 6).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Debounce window for a reconcile triggered by a structural notification
/// with no update bytes (decision 5), so a burst of them yields one round
/// trip.
const RECONCILE_DEBOUNCE: Duration = Duration::from_millis(200);

/// A running sync link for one store. Always call [`SyncLinkHandle::stop`]
/// when the link should end (`closeStore`, `setStoreSync(None)`, or
/// replacing it with a link to a different remote) — dropping the handle
/// alone does not stop the background task.
pub struct SyncLinkHandle {
    state_rx: watch::Receiver<SyncState>,
    join: JoinHandle<()>,
}

impl SyncLinkHandle {
    /// The link's current state.
    pub fn state(&self) -> SyncState {
        self.state_rx.borrow().clone()
    }

    /// A receiver for state changes; cheap to clone, shares the same channel
    /// as every other clone.
    pub fn watch(&self) -> watch::Receiver<SyncState> {
        self.state_rx.clone()
    }

    /// Stop the link's background task. Safe to call more than once.
    pub fn stop(&self) {
        self.join.abort();
    }
}

/// Namespace for [`SyncLink::start`]; the running link itself is represented
/// by the [`SyncLinkHandle`] it returns plus the detached background task.
pub struct SyncLink;

impl SyncLink {
    /// Start a sync link for `store_id` to `remote`, returning a handle.
    /// `handler` is an `Arc`-backed clone of the server's `RpcHandler`
    /// (cheap); the link drives its own background task from it until
    /// [`SyncLinkHandle::stop`].
    pub fn start(handler: RpcHandler, store_id: StoreId, remote: RemoteEndpoint) -> SyncLinkHandle {
        let link_id = format!("sync-link:{}", Uuid::new_v4());
        let (state_tx, state_rx) = watch::channel(SyncState::Syncing);

        let join = tokio::spawn(run_loop(handler, store_id, remote, link_id, state_tx));

        SyncLinkHandle { state_rx, join }
    }
}

/// Whether `source_client_id` names any sync link (not necessarily this
/// one). Used to filter the local-to-remote forwarding direction: skipping
/// every `sync-link:`-sourced change here is what prevents echo storms when
/// two servers link to each other, or a chain of three exists (decision 2).
fn is_any_sync_link(source_client_id: &Option<String>) -> bool {
    source_client_id.as_deref().map(|s| s.starts_with("sync-link:")).unwrap_or(false)
}

/// Coarse state category, ignoring `Synced`'s embedded timestamp, so
/// [`set_state`] can tell a real transition (worth an `info!` log and a
/// local `SyncStateChanged` notification) from `last_sync` simply advancing
/// after another applied change.
fn state_kind(state: &SyncState) -> u8 {
    match state {
        SyncState::Offline => 0,
        SyncState::Syncing => 1,
        SyncState::Synced { .. } => 2,
        SyncState::Conflict { .. } => 3,
    }
}

/// Update the link's watch channel and, on an actual transition between
/// state categories, log it and notify the store's local subscribers
/// (decision 6). Called after every applied change too (to keep `last_sync`
/// current for `listStores`/`getStoreSync`), but that alone never logs or
/// notifies.
async fn set_state(handler: &RpcHandler, store_id: StoreId, state_tx: &watch::Sender<SyncState>, state: SyncState) {
    let transitioned = state_kind(&state_tx.borrow()) != state_kind(&state);
    let _ = state_tx.send(state.clone());
    if transitioned {
        info!("Sync link for store {} -> {:?}", store_id, state);
        handler.notify_sync_state_changed(store_id, state).await;
    }
}

async fn run_loop(handler: RpcHandler, store_id: StoreId, remote: RemoteEndpoint, link_id: String, state_tx: watch::Sender<SyncState>) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match connect_and_sync(&handler, store_id, &remote, &link_id, &state_tx).await {
            Ok(()) => {
                // `connect_and_sync` only returns once its select loop hits
                // an error; a clean `Ok` should not happen, but treat it the
                // same as a dropped connection defensively rather than spin.
                backoff = INITIAL_BACKOFF;
            }
            Err(e) => {
                warn!("Sync link for store {} to {} dropped: {}", store_id, remote.url, e);
                set_state(&handler, store_id, &state_tx, SyncState::Offline).await;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// One connection cycle: connect, full reconcile, subscribe, then process
/// remote notifications, local notifications, and debounced reconcile
/// triggers until something goes wrong. Returns `Err` on any failure that
/// means the connection can no longer be trusted, so the caller retries with
/// backoff (decision 6).
async fn connect_and_sync(
    handler: &RpcHandler,
    store_id: StoreId,
    remote: &RemoteEndpoint,
    link_id: &str,
    state_tx: &watch::Sender<SyncState>,
) -> anyhow::Result<()> {
    set_state(handler, store_id, state_tx, SyncState::Syncing).await;

    // `remote.auth` is `AuthMethod::None` whenever this link was restarted
    // from `sync.json` (which never stores a real credential — docs/
    // HARDENING_CONTRACT.md decision 4); `resolve` finds the credential
    // saved for this origin, if any, the same way `addRemoteStore`/
    // `setStoreSync`/`listRemoteStores` do.
    let auth = handler.credentials().resolve(&remote.url, &remote.auth).await;
    let client = PimbleClient::connect_with_auth(remote.url.as_str(), &auth)
        .await
        .map_err(|e| anyhow::anyhow!("{}", pimble_client::describe_connect_error(&remote.url, &e)))?;

    full_reconcile(handler, &client, store_id, link_id).await?;

    let mut remote_sub = client
        .subscribe_store_changes(store_id)
        .await
        .map_err(|e| anyhow::anyhow!("subscribe to {} storeChanged failed: {}", remote.url, e))?;
    let mut local_rx = handler.subscribe_local_changes().await;

    let debouncer = Arc::new(ReconcileDebouncer::new());
    let (reconcile_tx, mut reconcile_rx) = mpsc::unbounded_channel::<ReconcileTrigger>();

    set_state(handler, store_id, state_tx, SyncState::Synced { last_sync: Utc::now() }).await;
    info!("Sync link for store {} connected to {}", store_id, remote.url);

    loop {
        tokio::select! {
            item = remote_sub.next() => {
                match item {
                    Some(Ok(notif)) => {
                        handle_remote_notification(handler, &client, store_id, link_id, notif, &debouncer, &reconcile_tx).await?;
                        set_state(handler, store_id, state_tx, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Some(Err(e)) => return Err(anyhow::anyhow!("remote notification decode error: {}", e)),
                    None => return Err(anyhow::anyhow!("remote subscription closed")),
                }
            }
            change = local_rx.recv() => {
                match change {
                    Ok(local_change) => {
                        forward_local_change(handler, &client, store_id, link_id, local_change, &debouncer, &reconcile_tx).await?;
                        set_state(handler, store_id, state_tx, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Missed notifications may never be followed by
                        // another change to the same node; drop the
                        // connection so the retry loop's full reconcile
                        // closes the gap.
                        return Err(anyhow::anyhow!(
                            "missed {} local notifications (broadcast lag); reconnecting to reconcile",
                            n
                        ));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(anyhow::anyhow!("local change broadcast closed"));
                    }
                }
            }
            trigger = reconcile_rx.recv() => {
                match trigger {
                    Some(ReconcileTrigger::Store) => {
                        reconcile_store(handler, &client, store_id, link_id).await?;
                        set_state(handler, store_id, state_tx, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Some(ReconcileTrigger::Node(node_id)) => {
                        reconcile_node(handler, &client, store_id, node_id, link_id).await?;
                        set_state(handler, store_id, state_tx, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    None => return Err(anyhow::anyhow!("internal reconcile channel closed")),
                }
            }
        }
    }
}

// ── Remote -> local ─────────────────────────────────────────────────

/// Apply one notification received from the remote's `storeChanged`
/// subscription (decision 2, first bullet).
async fn handle_remote_notification(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    link_id: &str,
    notif: StoreChangedNotification,
    debouncer: &Arc<ReconcileDebouncer>,
    reconcile_tx: &mpsc::UnboundedSender<ReconcileTrigger>,
) -> anyhow::Result<()> {
    // Skip our own echo: a change we forwarded to the remote comes back to
    // us via our own subscription to its broadcast.
    if notif.source_client_id.as_deref() == Some(link_id) {
        return Ok(());
    }

    match (&notif.change_kind, &notif.update) {
        (StoreChangeKind::TreeStructure { .. }, Some(update_b64)) => {
            apply_store_update_locally(handler, store_id, link_id, update_b64.clone()).await?;
            debug!("Sync link for store {} applied a remote tree update", store_id);
        }
        (StoreChangeKind::ContentUpdated { node_id }, Some(changes_b64)) => {
            apply_edit_locally_with_fallback(handler, client, store_id, *node_id, link_id, changes_b64.clone()).await?;
            debug!("Sync link for store {} applied a remote content edit to node {}", store_id, node_id);
        }
        (StoreChangeKind::ContentUpdated { node_id }, None) => {
            // From `updateNodeContent` (a full snapshot, no delta bytes):
            // reconcile that node's content document directly.
            schedule_node_reconcile(debouncer, reconcile_tx, *node_id);
        }
        (StoreChangeKind::NodeCreated { .. }, _)
        | (StoreChangeKind::NodeDeleted { .. }, _)
        | (StoreChangeKind::NodeMoved { .. }, _)
        | (StoreChangeKind::MetadataUpdated { .. }, _)
        | (StoreChangeKind::TreeStructure { .. }, None) => {
            schedule_store_reconcile(debouncer, reconcile_tx);
        }
        (StoreChangeKind::SyncStateChanged { .. }, _) | (StoreChangeKind::MountStateChanged { .. }, _) => {
            // The remote's own link state, or the state of its mounts
            // (derived from its links); not ours to react to, and never
            // forwarded (docs/REMOTE_MOUNTS_CONTRACT.md decision 6).
        }
    }
    Ok(())
}

// ── Local -> remote ─────────────────────────────────────────────────

/// Forward one local notification to the remote (decision 2, second
/// bullet). Node-content notifications carry nothing a store subscriber
/// doesn't already get (decision 4 puts content deltas on the store
/// notification too), so only `LocalChange::Store` is acted on here.
async fn forward_local_change(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    link_id: &str,
    change: LocalChange,
    debouncer: &Arc<ReconcileDebouncer>,
    reconcile_tx: &mpsc::UnboundedSender<ReconcileTrigger>,
) -> anyhow::Result<()> {
    let LocalChange::Store(notif) = change else {
        return Ok(());
    };
    if notif.store_id != store_id {
        return Ok(());
    }
    // Skip every sync-link source, not just our own: this is what prevents
    // echo storms in a chain of linked servers (decision 2).
    if is_any_sync_link(&notif.source_client_id) {
        return Ok(());
    }

    match (&notif.change_kind, &notif.update) {
        (StoreChangeKind::TreeStructure { .. }, Some(update_b64)) => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(update_b64)?;
            client
                .apply_store_update(store_id, link_id, &bytes)
                .await
                .map_err(|e| anyhow::anyhow!("remote applyStoreUpdate failed: {}", e))?;
            debug!("Sync link for store {} forwarded a local tree update", store_id);
        }
        (StoreChangeKind::ContentUpdated { node_id }, Some(changes_b64)) => {
            apply_edit_remotely_with_fallback(handler, client, store_id, *node_id, link_id, changes_b64.clone()).await?;
            debug!("Sync link for store {} forwarded a local content edit for node {}", store_id, node_id);
        }
        (StoreChangeKind::ContentUpdated { node_id }, None) => {
            schedule_node_reconcile(debouncer, reconcile_tx, *node_id);
        }
        (StoreChangeKind::NodeCreated { .. }, _)
        | (StoreChangeKind::NodeDeleted { .. }, _)
        | (StoreChangeKind::NodeMoved { .. }, _)
        | (StoreChangeKind::MetadataUpdated { .. }, _)
        | (StoreChangeKind::TreeStructure { .. }, None) => {
            schedule_store_reconcile(debouncer, reconcile_tx);
        }
        (StoreChangeKind::SyncStateChanged { .. }, _) | (StoreChangeKind::MountStateChanged { .. }, _) => {}
    }
    Ok(())
}

// ── Applying a change, with a structural fallback ───────────────────

/// Merge a remote tree/metadata update into the local store document via
/// the handler's own `applyStoreUpdate` (so persistence, local subscribers,
/// and the search index all follow, per decision 1).
async fn apply_store_update_locally(handler: &RpcHandler, store_id: StoreId, link_id: &str, update_b64: String) -> anyhow::Result<()> {
    handler
        .apply_store_update(ApplyStoreUpdateRequest { store_id, client_id: link_id.to_string(), update: update_b64 })
        .await
        .map_err(|e| anyhow::anyhow!("local applyStoreUpdate failed: {}", e))?;
    Ok(())
}

/// Apply a remote content delta locally via the handler's own `applyEdit`.
/// If that fails — most likely because the node's tree entry has not
/// reached this replica yet, a race between the two notifications a single
/// remote `applyEdit` and its node's earlier `NodeCreated` produce — pull
/// the store document from the remote once and retry.
async fn apply_edit_locally_with_fallback(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    node_id: NodeId,
    link_id: &str,
    changes_b64: String,
) -> anyhow::Result<()> {
    let op = EditOperation::IncrementalChanges { changes: changes_b64 };
    let request = ApplyEditRequest { store_id, node_id, client_id: link_id.to_string(), operation: op.clone() };
    if handler.apply_edit(request).await.is_err() {
        reconcile_store(handler, client, store_id, link_id).await?;
        let retry = ApplyEditRequest { store_id, node_id, client_id: link_id.to_string(), operation: op };
        handler
            .apply_edit(retry)
            .await
            .map_err(|e| anyhow::anyhow!("local applyEdit failed even after reconciling the store document: {}", e))?;
    }
    Ok(())
}

/// The remote-side counterpart of [`apply_edit_locally_with_fallback`]: send
/// a content delta to the remote via `applyEdit`, falling back to a store
/// reconcile and one retry if the remote doesn't yet know the node.
async fn apply_edit_remotely_with_fallback(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    node_id: NodeId,
    link_id: &str,
    changes_b64: String,
) -> anyhow::Result<()> {
    let op = EditOperation::IncrementalChanges { changes: changes_b64 };
    if client.apply_edit(store_id, node_id, link_id, op.clone()).await.is_err() {
        reconcile_store(handler, client, store_id, link_id).await?;
        client
            .apply_edit(store_id, node_id, link_id, op)
            .await
            .map_err(|e| anyhow::anyhow!("remote applyEdit failed even after reconciling the store document: {}", e))?;
    }
    Ok(())
}

// ── Reconcile ────────────────────────────────────────────────────────

/// A full reconcile: the store document first, then the content of every
/// node id in the (now up to date) local store document (decision 5), in
/// `syncNodeContents` batches so a store of N nodes costs about N / 100
/// round trips plus one per node that actually differs.
async fn full_reconcile(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, link_id: &str) -> anyhow::Result<()> {
    reconcile_store(handler, client, store_id, link_id).await?;

    let manager = handler.store_manager_handle();
    let node_ids = {
        let manager = manager.read().await;
        manager.store_document(store_id)?.list_node_ids()?
    };

    for chunk in node_ids.chunks(MAX_SYNC_NODE_CONTENTS) {
        let local_svs = {
            let mut manager = manager.write().await;
            let mut svs = Vec::with_capacity(chunk.len());
            for node_id in chunk {
                svs.push((*node_id, manager.get_node_document(store_id, *node_id).await?.state_vector()));
            }
            svs
        };

        let remote_answers = client
            .sync_node_contents(store_id, &local_svs)
            .await
            .map_err(|e| anyhow::anyhow!("remote syncNodeContents failed: {}", e))?;

        // A node the remote left out is not in its store document yet (a
        // race with a structural change); the next reconcile covers it.
        //
        // The pull always runs (decision 7's finding: a yrs v1 diff is never
        // actually empty — `[0, 0]` at minimum plus the sender's whole
        // delete set — so an `is_empty()` gate here was always true anyway);
        // `apply_edit` on the receiving end now does its own no-op check
        // (decision 8), so applying a diff that turns out to carry nothing
        // new is cheap.
        for (node_id, diff, remote_sv) in remote_answers {
            let diff_b64 = base64::engine::general_purpose::STANDARD.encode(&diff);
            apply_edit_locally_with_fallback(handler, client, store_id, node_id, link_id, diff_b64).await?;
            push_node_diff(handler, client, store_id, node_id, link_id, &remote_sv, &diff).await?;
        }
    }

    Ok(())
}

/// Reconcile the store document with the remote: pull what it has that we
/// lack, then push what we have that it lacks (decision 5). The pull always
/// applies (see `full_reconcile`'s comment on why `is_empty()` never gated
/// anything); the push uses `diff_if_peer_lacks_it` (decision 7) instead of
/// the same dead `is_empty()` check, so a reconcile between two already-
/// synced servers makes no round trip at all here.
async fn reconcile_store(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, link_id: &str) -> anyhow::Result<()> {
    let manager = handler.store_manager_handle();

    let local_sv = {
        let manager = manager.read().await;
        manager.store_doc_state_vector(store_id)?
    };

    let (diff, remote_sv) = client
        .sync_store_document(store_id, &local_sv)
        .await
        .map_err(|e| anyhow::anyhow!("remote syncStoreDocument failed: {}", e))?;

    let diff_b64 = base64::engine::general_purpose::STANDARD.encode(&diff);
    apply_store_update_locally(handler, store_id, link_id, diff_b64).await?;

    let local_push = {
        let manager = manager.read().await;
        manager.store_document(store_id)?.diff_if_peer_lacks_it(&remote_sv, &diff)?
    };

    if let Some(local_diff) = local_push {
        client
            .apply_store_update(store_id, link_id, &local_diff)
            .await
            .map_err(|e| anyhow::anyhow!("remote applyStoreUpdate failed during reconcile: {}", e))?;
        debug!("Sync link for store {} pushed a store document diff to the remote", store_id);
    }

    Ok(())
}

/// Reconcile one node's content document with the remote, the same way
/// (decision 5). Only meaningful once both sides agree the node exists;
/// callers that reach this from a debounced trigger rather than
/// `full_reconcile` may still race a not-yet-arrived tree entry, in which
/// case this surfaces as an error and the outer retry loop's next full
/// reconcile catches it.
async fn reconcile_node(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, node_id: NodeId, link_id: &str) -> anyhow::Result<()> {
    let manager = handler.store_manager_handle();

    let local_sv = {
        let mut manager = manager.write().await;
        manager.get_node_document(store_id, node_id).await?.state_vector()
    };

    let (diff, remote_sv) = client
        .sync_node_content(store_id, node_id, &local_sv)
        .await
        .map_err(|e| anyhow::anyhow!("remote syncNodeContent failed: {}", e))?;

    let diff_b64 = base64::engine::general_purpose::STANDARD.encode(&diff);
    apply_edit_locally_with_fallback(handler, client, store_id, node_id, link_id, diff_b64).await?;

    push_node_diff(handler, client, store_id, node_id, link_id, &remote_sv, &diff).await
}

/// Send the remote everything this node's local content document has beyond
/// `remote_sv`, if anything — `remote_diff` (the diff the remote just sent
/// us, en route to `remote_sv`) is what `diff_if_peer_lacks_it` needs to
/// tell "nothing new" apart from "carries the remote's whole delete set, as
/// every yrs diff does" (decision 7).
async fn push_node_diff(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    node_id: NodeId,
    link_id: &str,
    remote_sv: &[u8],
    remote_diff: &[u8],
) -> anyhow::Result<()> {
    let local_push = {
        let manager = handler.store_manager_handle();
        let mut manager = manager.write().await;
        let doc = manager.get_node_document(store_id, node_id).await?;
        doc.diff_if_peer_lacks_it(remote_sv, remote_diff)?
    };

    if let Some(local_diff) = local_push {
        let diff_b64 = base64::engine::general_purpose::STANDARD.encode(&local_diff);
        apply_edit_remotely_with_fallback(handler, client, store_id, node_id, link_id, diff_b64).await?;
        debug!("Sync link for store {} pushed a content diff for node {} to the remote", store_id, node_id);
    }

    Ok(())
}

// ── Debounced reconcile triggers ────────────────────────────────────

enum ReconcileTrigger {
    Store,
    Node(NodeId),
}

/// Per-store and per-node debounce generations for [`schedule_store_reconcile`]
/// and [`schedule_node_reconcile`], same pattern as
/// `StoreIndexer::schedule_content_upsert` in `handler.rs`: bump a counter,
/// spawn a task that sleeps then fires only if no newer trigger has arrived.
struct ReconcileDebouncer {
    store_generation: Mutex<u64>,
    node_generation: Mutex<HashMap<NodeId, u64>>,
}

impl ReconcileDebouncer {
    fn new() -> Self {
        Self { store_generation: Mutex::new(0), node_generation: Mutex::new(HashMap::new()) }
    }
}

fn schedule_store_reconcile(debouncer: &Arc<ReconcileDebouncer>, reconcile_tx: &mpsc::UnboundedSender<ReconcileTrigger>) {
    let generation = {
        let mut g = debouncer.store_generation.lock().unwrap();
        *g += 1;
        *g
    };
    let debouncer = Arc::clone(debouncer);
    let reconcile_tx = reconcile_tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(RECONCILE_DEBOUNCE).await;
        let still_current = *debouncer.store_generation.lock().unwrap() == generation;
        if still_current {
            let _ = reconcile_tx.send(ReconcileTrigger::Store);
        }
    });
}

fn schedule_node_reconcile(debouncer: &Arc<ReconcileDebouncer>, reconcile_tx: &mpsc::UnboundedSender<ReconcileTrigger>, node_id: NodeId) {
    let generation = {
        let mut gens = debouncer.node_generation.lock().unwrap();
        let g = gens.entry(node_id).or_insert(0);
        *g += 1;
        *g
    };
    let debouncer = Arc::clone(debouncer);
    let reconcile_tx = reconcile_tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(RECONCILE_DEBOUNCE).await;
        let still_current = debouncer.node_generation.lock().unwrap().get(&node_id).copied() == Some(generation);
        if still_current {
            let _ = reconcile_tx.send(ReconcileTrigger::Node(node_id));
        }
    });
}
