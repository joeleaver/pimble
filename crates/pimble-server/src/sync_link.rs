//! Replica sync link: keeps one local store's node documents merged with the
//! same store held by a remote Pimble server (`docs/SYNC_CONTRACT.md`, over
//! the documents of docs/NODE_DOCUMENT_CONTRACT.md section 4).
//!
//! One [`SyncLink`] per linked store, owned by `RpcHandler::links`. It never
//! touches `LocalStore` directly for writes: a remote change is merged in
//! through the handler's own `apply_node_update_from` (the same path
//! `applyEdit` takes), so local subscribers, the search index, persistence
//! and the tree repair all behave exactly as if an ordinary client had sent
//! the change, with `client_id = "sync-link:<uuid>"`.
//!
//! There is one shape for everything: a node's text, its place in the tree
//! and its metadata are one document, so the link knows nothing about
//! content or structure. Reconcile (decision 5, per document): `syncNodes`
//! with every held document's state vector and `list_unknown`, apply what
//! the remote answers, fetch the documents it named that this store does not
//! hold, push back per document what the remote lacks
//! (`diff_if_peer_lacks_it`, decision 7) and every document the remote does
//! not hold at all, then repair the tree once. Live: every notification that
//! carries a document's bytes is forwarded as an `applyEdit` for that
//! document, in both directions, and a change bouncing back merges as a
//! no-op that sends nothing (decision 8), which is what lets an edit travel a
//! whole chain of servers without an echo storm.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, RemoteEndpoint, StoreId, SyncState};
use pimble_rpc::{EditOperation, StoreChangeKind, StoreChangedNotification, MAX_SYNC_NODE_CONTENTS};
use pimble_store::SyncConfig;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::handler::{LocalChange, Repair, RpcHandler};

/// Initial retry backoff on a dropped or failed connection; doubles up to
/// [`MAX_BACKOFF`] (decision 6).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Debounce window for a reconcile of one document triggered by a
/// notification about it that carried no bytes (an older peer's), so a burst
/// of them yields one round trip.
const RECONCILE_DEBOUNCE: Duration = Duration::from_millis(200);

/// A running sync link for one store. Always call [`SyncLinkHandle::stop`]
/// when the link should end (`closeStore`, `setStoreSync(None)`, or
/// replacing it with a link to a different remote) — dropping the handle
/// alone does not stop the background task.
pub struct SyncLinkHandle {
    state_rx: watch::Receiver<SyncState>,
    /// When this link last reached `Synced`, shared with the running task
    /// (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 4). Seeded from `sync.json`
    /// at [`SyncLink::start`], so a link that has never yet connected since
    /// the store reopened still knows when it last did.
    last_sync: Arc<Mutex<Option<DateTime<Utc>>>>,
    join: JoinHandle<()>,
}

impl SyncLinkHandle {
    /// The link's current state.
    pub fn state(&self) -> SyncState {
        self.state_rx.borrow().clone()
    }

    /// When this link last reached `Synced`, whether in this process or a
    /// previous one (decision 4). `None` means it has never synced. This is
    /// what turns a mount whose source's link is down into
    /// `MountState::Cached { last_sync }` rather than `Connecting`.
    pub fn last_sync(&self) -> Option<DateTime<Utc>> {
        *self.last_sync.lock().unwrap()
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
    /// [`SyncLinkHandle::stop`]. `last_sync` is the value this store's
    /// `sync.json` carries (decision 4), so the handle can answer
    /// [`SyncLinkHandle::last_sync`] before the link has connected even
    /// once.
    pub fn start(
        handler: RpcHandler,
        store_id: StoreId,
        remote: RemoteEndpoint,
        last_sync: Option<DateTime<Utc>>,
    ) -> SyncLinkHandle {
        let link_id = format!("sync-link:{}", Uuid::new_v4());
        let (state_tx, state_rx) = watch::channel(SyncState::Syncing);
        let last_sync = Arc::new(Mutex::new(last_sync));

        let link = LinkState { store_id, state_tx, last_sync: Arc::clone(&last_sync) };
        let join = tokio::spawn(run_loop(handler, remote, link_id, link));

        SyncLinkHandle { state_rx, last_sync, join }
    }
}

/// Everything publishing a state change needs: which store the link serves,
/// the watch channel its [`SyncLinkHandle`] reads, and the shared
/// `last_sync` that handle answers from and that `sync.json` mirrors
/// (decision 4).
struct LinkState {
    store_id: StoreId,
    state_tx: watch::Sender<SyncState>,
    last_sync: Arc<Mutex<Option<DateTime<Utc>>>>,
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
///
/// `last_sync` is kept current in memory on every `Synced` (decision 4 of
/// docs/history/REMOTE_MOUNTS_CONTRACT.md) and mirrored to `sync.json` only on a
/// transition into `Synced` (with the new time) or out of it (with the last
/// time), so an ordinary editing session doesn't rewrite that file once per
/// keystroke.
async fn set_state(handler: &RpcHandler, link: &LinkState, state: SyncState) {
    let previous = link.state_tx.borrow().clone();
    let transitioned = state_kind(&previous) != state_kind(&state);
    let crossed_synced =
        matches!(previous, SyncState::Synced { .. }) || matches!(state, SyncState::Synced { .. });

    if let SyncState::Synced { last_sync } = &state {
        *link.last_sync.lock().unwrap() = Some(*last_sync);
    }
    let _ = link.state_tx.send(state.clone());

    if !transitioned {
        return;
    }

    if crossed_synced {
        let remembered = *link.last_sync.lock().unwrap();
        persist_last_sync(handler, link.store_id, remembered).await;
    }

    info!("Sync link for store {} -> {:?}", link.store_id, state);
    handler.notify_sync_state_changed(link.store_id, state).await;
}

/// Mirror the link's `last_sync` into `<store>/sync.json`, keeping its
/// `remote` and forcing `auth: none` (decision 4: a credential never lands
/// on disk here). Best effort: a store closed or unlinked under us — in
/// which case there is no `sync.json` and recreating one would resurrect a
/// link the user just removed — and a failed write are both logged, never
/// propagated; this is bookkeeping, not part of the sync protocol.
async fn persist_last_sync(handler: &RpcHandler, store_id: StoreId, last_sync: Option<DateTime<Utc>>) {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let existing = match manager.read_sync_config(store_id).await {
        Ok(Some(config)) => config,
        Ok(None) => return,
        Err(e) => {
            debug!("Could not read sync.json for store {} to record last_sync: {}", store_id, e);
            return;
        }
    };
    let updated = SyncConfig {
        remote: RemoteEndpoint { url: existing.remote.url, auth: AuthMethod::None },
        last_sync,
        mode: existing.mode,
        last_seq: existing.last_seq,
        vault_key_id: existing.vault_key_id,
    };
    if let Err(e) = manager.write_sync_config(store_id, &updated).await {
        warn!("Could not record last_sync for store {}: {}", store_id, e);
    }
}

async fn run_loop(handler: RpcHandler, remote: RemoteEndpoint, link_id: String, link: LinkState) {
    let store_id = link.store_id;
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match connect_and_sync(&handler, &remote, &link_id, &link).await {
            Ok(()) => {
                // `connect_and_sync` only returns once its select loop hits
                // an error; a clean `Ok` should not happen, but treat it the
                // same as a dropped connection defensively rather than spin.
                backoff = INITIAL_BACKOFF;
            }
            Err(e) => {
                warn!("Sync link for store {} to {} dropped: {}", store_id, remote.url, e);
                set_state(&handler, &link, SyncState::Offline).await;
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
    remote: &RemoteEndpoint,
    link_id: &str,
    link: &LinkState,
) -> anyhow::Result<()> {
    let store_id = link.store_id;
    set_state(handler, link, SyncState::Syncing).await;

    // `remote.auth` is `AuthMethod::None` whenever this link was restarted
    // from `sync.json` (which never stores a real credential — docs/
    // HARDENING_CONTRACT.md decision 4); `resolve` finds the credential
    // saved for this origin, if any, the same way `addRemoteStore`/
    // `setStoreSync`/`listRemoteStores` do.
    let auth = handler.credentials().resolve(&remote.url, &remote.auth).await;
    let client = PimbleClient::connect_with_auth(remote.url.as_str(), &auth)
        .await
        .map_err(|e| anyhow::anyhow!("{}", pimble_client::describe_connect_error(&remote.url, &e)))?;

    // Both channels are open before the reconcile, not after it: what
    // happens during it (the remote repairing its tree after this side's
    // pushes; this side's own repair at the end) would otherwise be seen by
    // neither, and two replicas each holding a repair the other never got
    // do not converge (each would delete the other's duplicate and put its
    // own back, for ever).
    let remote_sub = client
        .subscribe_store_changes(store_id)
        .await
        .map_err(|e| anyhow::anyhow!("subscribe to {} storeChanged failed: {}", remote.url, e))?;
    let local_rx = handler.subscribe_local_changes().await;
    let (reconcile_tx, mut reconcile_rx) = mpsc::unbounded_channel::<NodeId>();
    let mut channels = Channels { remote_sub, local_rx, debouncer: Arc::new(ReconcileDebouncer::new()), reconcile_tx };

    full_reconcile(handler, &client, store_id, link_id, &mut channels).await?;

    set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
    info!("Sync link for store {} connected to {}", store_id, remote.url);

    loop {
        tokio::select! {
            item = channels.remote_sub.next() => {
                match item {
                    Some(Ok(notif)) => {
                        handle_remote_notification(handler, store_id, link_id, notif, &channels.debouncer, &channels.reconcile_tx, Repair::Debounced).await?;
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Some(Err(e)) => return Err(anyhow::anyhow!("remote notification decode error: {}", e)),
                    None => return Err(anyhow::anyhow!("remote subscription closed")),
                }
            }
            change = channels.local_rx.recv() => {
                match change {
                    Ok(local_change) => {
                        forward_local_change(&client, store_id, link_id, local_change, &channels.debouncer, &channels.reconcile_tx).await?;
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
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
                    Some(node_id) => {
                        reconcile_node(handler, &client, store_id, node_id, link_id).await?;
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    None => return Err(anyhow::anyhow!("internal reconcile channel closed")),
                }
            }
        }
    }
}

/// What the live loop reads: the remote's `storeChanged` subscription, this
/// server's local-change broadcast, and the debounced per-node reconcile
/// triggers. Opened before the reconcile (see `connect_and_sync`) and
/// drained between its batches, since the reconcile itself fills both: every
/// document it applies here is a local notification (this link's own, and
/// dropped as such), and every document it pushes comes back as the remote's
/// echo. Left alone, a store of a few thousand documents would lag the
/// broadcast (1024 slots) or overflow the subscription (which jsonrpsee then
/// closes), and cost a second connection cycle to finish.
struct Channels {
    remote_sub: jsonrpsee::core::client::Subscription<StoreChangedNotification>,
    local_rx: broadcast::Receiver<LocalChange>,
    debouncer: Arc<ReconcileDebouncer>,
    reconcile_tx: mpsc::UnboundedSender<NodeId>,
}

impl Channels {
    /// Handle whatever both channels hold right now, without waiting for
    /// more: a local change is forwarded, a remote one merged (with no
    /// repair, the reconcile's own at the end covers it). The same errors
    /// as the live loop's, for the same reasons.
    async fn drain(&mut self, handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, link_id: &str) -> anyhow::Result<()> {
        loop {
            match self.local_rx.try_recv() {
                Ok(change) => forward_local_change(client, store_id, link_id, change, &self.debouncer, &self.reconcile_tx).await?,
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    return Err(anyhow::anyhow!("missed {} local notifications (broadcast lag); reconnecting to reconcile", n));
                }
                Err(broadcast::error::TryRecvError::Closed) => return Err(anyhow::anyhow!("local change broadcast closed")),
            }
        }
        loop {
            // A zero timeout polls the subscription once and gives up if it
            // is pending, which is the non-blocking read it has no method for.
            match tokio::time::timeout(Duration::ZERO, self.remote_sub.next()).await {
                Ok(Some(Ok(notif))) => {
                    handle_remote_notification(handler, store_id, link_id, notif, &self.debouncer, &self.reconcile_tx, Repair::Later).await?;
                }
                Ok(Some(Err(e))) => return Err(anyhow::anyhow!("remote notification decode error: {}", e)),
                Ok(None) => return Err(anyhow::anyhow!("remote subscription closed")),
                Err(_) => break,
            }
        }
        Ok(())
    }
}

/// The document a notification is about, when it is about one: every kind
/// but the link and mount states, which are derived and never forwarded
/// (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 6), and `VaultAppended`,
/// which a plain link never sees (its remote is a plain twin).
fn document_of(kind: &StoreChangeKind) -> Option<NodeId> {
    match kind {
        StoreChangeKind::NodeCreated { node_id, .. }
        | StoreChangeKind::NodeDeleted { node_id, .. }
        | StoreChangeKind::NodeMoved { node_id, .. }
        | StoreChangeKind::MetadataUpdated { node_id }
        | StoreChangeKind::ContentUpdated { node_id } => Some(*node_id),
        StoreChangeKind::TreeStructure { node_ids } => node_ids.first().copied(),
        StoreChangeKind::SyncStateChanged { .. }
        | StoreChangeKind::MountStateChanged { .. }
        | StoreChangeKind::VaultAppended { .. } => None,
    }
}

// ── Remote -> local ─────────────────────────────────────────────────

/// Apply one notification received from the remote's `storeChanged`
/// subscription (decision 2, first bullet): its bytes are merged into the
/// document it names. A document notification with no bytes (an older peer)
/// reconciles that one document instead, after a debounce.
async fn handle_remote_notification(
    handler: &RpcHandler,
    store_id: StoreId,
    link_id: &str,
    notif: StoreChangedNotification,
    debouncer: &Arc<ReconcileDebouncer>,
    reconcile_tx: &mpsc::UnboundedSender<NodeId>,
    repair: Repair,
) -> anyhow::Result<()> {
    // Skip our own echo: a change we forwarded to the remote comes back to
    // us via our own subscription to its broadcast.
    if notif.source_client_id.as_deref() == Some(link_id) {
        return Ok(());
    }
    let Some(node_id) = document_of(&notif.change_kind) else {
        return Ok(());
    };
    match &notif.update {
        Some(update_b64) => {
            let bytes = base64::engine::general_purpose::STANDARD.decode(update_b64)?;
            apply_update_locally(handler, store_id, node_id, link_id, &bytes, repair).await?;
            debug!("Sync link for store {} applied a remote update to node {}", store_id, node_id);
        }
        None => schedule_node_reconcile(debouncer, reconcile_tx, node_id),
    }
    Ok(())
}

// ── Local -> remote ─────────────────────────────────────────────────

/// Forward one local notification to the remote (decision 2, second
/// bullet, as amended for chains: see the source check below). Node-content
/// notifications carry nothing a store subscriber doesn't already get (the
/// same bytes ride the store notification), so only `LocalChange::Store` is
/// acted on here.
async fn forward_local_change(
    client: &PimbleClient,
    store_id: StoreId,
    link_id: &str,
    change: LocalChange,
    debouncer: &Arc<ReconcileDebouncer>,
    reconcile_tx: &mpsc::UnboundedSender<NodeId>,
) -> anyhow::Result<()> {
    let LocalChange::Store(notif) = change else {
        return Ok(());
    };
    if notif.store_id != store_id {
        return Ok(());
    }
    // Skip only what this link itself applied: that change came from the
    // very remote it would be sent back to. A change another link applied
    // here is forwarded on, so an edit travels the whole length of a chain
    // (M -> L -> R, the shape a remote mount resolved through the mounting
    // store's own remote produces; docs/history/REMOTE_MOUNTS_CONTRACT.md). No echo
    // storm follows: a change bouncing back to a server that already has it
    // merges as a no-op there, and a no-op merge sends no notification
    // (docs/history/HARDENING_CONTRACT.md decision 8), so every path ends.
    if notif.source_client_id.as_deref() == Some(link_id) {
        return Ok(());
    }
    let Some(node_id) = document_of(&notif.change_kind) else {
        return Ok(());
    };
    match notif.update {
        Some(changes) => {
            client
                .apply_edit(store_id, node_id, link_id, EditOperation::IncrementalChanges { changes })
                .await
                .map_err(|e| anyhow::anyhow!("remote applyEdit for node {} failed: {}", node_id, e))?;
            debug!("Sync link for store {} forwarded a local update to node {}", store_id, node_id);
        }
        None => schedule_node_reconcile(debouncer, reconcile_tx, node_id),
    }
    Ok(())
}

/// Merge a remote update into the local document via the handler's own
/// path (so persistence, local subscribers, the search index and the tree
/// repair all follow, per decision 1). A sync link's own merge of a change
/// it fetched with its own credential has no per-request `Principal` to
/// forward; the handler's internal entry point authorises nothing
/// (docs/CLOUD_CONTRACT.md "B: pimble-server" item 4).
async fn apply_update_locally(handler: &RpcHandler, store_id: StoreId, node_id: NodeId, link_id: &str, update: &[u8], repair: Repair) -> anyhow::Result<()> {
    handler
        .apply_node_update_from(store_id, node_id, update, Some(link_id), repair)
        .await
        .map_err(|e| anyhow::anyhow!("local applyEdit for node {} failed: {}", node_id, e.message()))?;
    Ok(())
}

// ── Reconcile ────────────────────────────────────────────────────────

/// A full reconcile over documents (decision 5): every held document's
/// state vector goes to the remote in one `syncNodes` (batched by the
/// client) that also asks what it did not name; what comes back is merged,
/// the named unknowns are fetched in batches of [`MAX_SYNC_NODE_CONTENTS`]
/// and merged, every held document is pushed back to the extent the remote
/// lacks it (whole when the remote never heard of it), and the tree is
/// repaired once at the end (see `Repair::Later`: a repair between two
/// batches would judge a half-arrived tree). The live channels are drained
/// every batch (see [`Channels`]).
async fn full_reconcile(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, link_id: &str, channels: &mut Channels) -> anyhow::Result<()> {
    let manager = handler.store_manager_handle();

    let local_svs: Vec<(NodeId, Vec<u8>)> = {
        let manager = manager.read().await;
        let ids = manager.doc_ids(store_id)?;
        let mut svs = Vec::with_capacity(ids.len());
        for node_id in ids {
            svs.push((node_id, manager.node_state_vector(store_id, node_id)?));
        }
        svs
    };

    let (answers, unknown) = client
        .sync_nodes(store_id, &local_svs, true)
        .await
        .map_err(|e| anyhow::anyhow!("remote syncNodes failed: {}", e))?;

    // The pull always applies (decision 7's finding: a yrs v1 diff is never
    // actually empty — `[0, 0]` at minimum plus the sender's whole delete
    // set — so an `is_empty()` gate here would always be true); the handler
    // does its own no-op check (decision 8), so applying a diff that turns
    // out to carry nothing new is cheap. Then the push back, only where the
    // remote lacks something.
    let mut answered = std::collections::HashSet::with_capacity(answers.len());
    for chunk in answers.chunks(MAX_SYNC_NODE_CONTENTS) {
        for (node_id, diff, remote_sv) in chunk {
            answered.insert(*node_id);
            apply_update_locally(handler, store_id, *node_id, link_id, diff, Repair::Later).await?;
            push_node_diff(handler, client, store_id, *node_id, link_id, remote_sv, diff).await?;
        }
        channels.drain(handler, client, store_id, link_id).await?;
    }

    // Documents the remote holds and this store does not: asked for whole
    // (an empty state vector), and nothing to push back for them.
    let empty_sv = pimble_crdt::empty_state_vector();
    for chunk in unknown.chunks(MAX_SYNC_NODE_CONTENTS) {
        let wanted: Vec<(NodeId, Vec<u8>)> = chunk.iter().map(|id| (*id, empty_sv.clone())).collect();
        let (fetched, _) = client
            .sync_nodes(store_id, &wanted, false)
            .await
            .map_err(|e| anyhow::anyhow!("remote syncNodes for unknown documents failed: {}", e))?;
        for (node_id, diff, _) in fetched {
            apply_update_locally(handler, store_id, node_id, link_id, &diff, Repair::Later).await?;
        }
        channels.drain(handler, client, store_id, link_id).await?;
    }

    // Documents this store holds and the remote does not: its `applyEdit`
    // makes a document for an id it has never seen, so the whole state goes
    // (`save`, which unlike a diff from an empty vector carries what is
    // still pending here too).
    let unanswered: Vec<NodeId> = local_svs.iter().map(|(id, _)| *id).filter(|id| !answered.contains(id)).collect();
    for chunk in unanswered.chunks(MAX_SYNC_NODE_CONTENTS) {
        for node_id in chunk {
            let whole = {
                let manager = manager.read().await;
                match manager.tree(store_id)?.doc(*node_id) {
                    Some(doc) => doc.save(),
                    None => continue,
                }
            };
            push_update_remotely(client, store_id, *node_id, link_id, &whole).await?;
            debug!("Sync link for store {} pushed the whole document of node {} to the remote", store_id, node_id);
        }
        channels.drain(handler, client, store_id, link_id).await?;
    }

    handler.repair_store_tree(store_id).await;
    Ok(())
}

/// Reconcile one document with the remote (decision 5), the fallback for a
/// notification about it that carried no bytes.
async fn reconcile_node(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, node_id: NodeId, link_id: &str) -> anyhow::Result<()> {
    let manager = handler.store_manager_handle();

    let local_sv = {
        let manager = manager.read().await;
        // Not held here yet: everything the remote has of it is wanted.
        manager.node_state_vector(store_id, node_id).unwrap_or_else(|_| pimble_crdt::empty_state_vector())
    };

    let (diff, remote_sv) = client
        .sync_node_content(store_id, node_id, &local_sv)
        .await
        .map_err(|e| anyhow::anyhow!("remote syncNodes for node {} failed: {}", node_id, e))?;

    apply_update_locally(handler, store_id, node_id, link_id, &diff, Repair::Debounced).await?;

    push_node_diff(handler, client, store_id, node_id, link_id, &remote_sv, &diff).await
}

/// Send the remote everything this node's local document has beyond
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
        let manager = manager.read().await;
        let tree = manager.tree(store_id)?;
        match tree.doc(node_id) {
            Some(doc) => doc.diff_if_peer_lacks_it(remote_sv, remote_diff)?,
            None => None,
        }
    };

    if let Some(local_diff) = local_push {
        push_update_remotely(client, store_id, node_id, link_id, &local_diff).await?;
        debug!("Sync link for store {} pushed a diff for node {} to the remote", store_id, node_id);
    }

    Ok(())
}

async fn push_update_remotely(client: &PimbleClient, store_id: StoreId, node_id: NodeId, link_id: &str, update: &[u8]) -> anyhow::Result<()> {
    let changes = base64::engine::general_purpose::STANDARD.encode(update);
    client
        .apply_edit(store_id, node_id, link_id, EditOperation::IncrementalChanges { changes })
        .await
        .map_err(|e| anyhow::anyhow!("remote applyEdit for node {} failed during reconcile: {}", node_id, e))?;
    Ok(())
}

// ── Debounced reconcile triggers ────────────────────────────────────

/// Per-node debounce generations for [`schedule_node_reconcile`], same
/// pattern as `StoreIndexer::schedule_content_upsert` in `handler.rs`: bump
/// a counter, spawn a task that sleeps then fires only if no newer trigger
/// has arrived.
struct ReconcileDebouncer {
    node_generation: Mutex<HashMap<NodeId, u64>>,
}

impl ReconcileDebouncer {
    fn new() -> Self {
        Self { node_generation: Mutex::new(HashMap::new()) }
    }
}

fn schedule_node_reconcile(debouncer: &Arc<ReconcileDebouncer>, reconcile_tx: &mpsc::UnboundedSender<NodeId>, node_id: NodeId) {
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
            let _ = reconcile_tx.send(node_id);
        }
    });
}
