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
use pimble_rpc::{StoreChangeKind, StoreChangedNotification};
use pimble_store::SyncConfig;
use tokio::sync::{broadcast, mpsc, watch};
use pimble_host::{Runtime, Task};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::handler::{LocalChange, Repair, RpcHandler};

mod account;
mod reconcile;

/// Initial retry backoff on a dropped or failed connection; doubles up to
/// [`MAX_BACKOFF`] (decision 6).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Debounce window for a reconcile of one document triggered by a
/// notification about it that carried no bytes (an older peer's), so a burst
/// of them yields one round trip.
const RECONCILE_DEBOUNCE: Duration = Duration::from_millis(200);
/// How often a link held through the account checks whether someone has
/// signed in again after its session expired.
const SIGN_IN_CHECK: Duration = Duration::from_secs(5);

/// The next tick of a link's grant check, or never for a link that has none.
async fn next_grant_check(tick: &mut Option<pimble_host::Interval>) {
    match tick {
        Some(tick) => {
            tick.tick().await;
        }
        None => std::future::pending().await,
    }
}

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
    join: Task,
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
        let runtime = handler.runtime().clone();
        let join = runtime.spawn(run_loop(handler, remote, link_id, link));

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
        via_relay: existing.via_relay,
        via_account: existing.via_account,
        last_seq: existing.last_seq,
        vault_key_id: existing.vault_key_id,
        access: existing.access,
        shared_by: existing.shared_by,
        read_only_roots: existing.read_only_roots,
        comment_roots: existing.comment_roots,
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
                // The account's grant changed (`account`): connect again at
                // once, which reads it and mints a token from it.
                backoff = INITIAL_BACKOFF;
            }
            Err(e) if e.is::<crate::grant::SessionExpired>() => {
                // Said once, by `crate::grant::mint`; until someone signs in
                // nothing is tried, and the reconcile at the next connect
                // brings over what changed meanwhile.
                set_state(&handler, &link, SyncState::Offline).await;
                while handler.keystore().session_expired() {
                    handler.runtime().sleep(SIGN_IN_CHECK).await;
                }
                backoff = INITIAL_BACKOFF;
            }
            Err(e) => {
                warn!("Sync link for store {} to {} dropped: {}", store_id, remote.url, e);
                set_state(&handler, &link, SyncState::Offline).await;
                handler.runtime().sleep(backoff).await;
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

    // A plain store hosted on Pimble Cloud is reached with a token minted
    // from the signed-in account (see `account`); any other remote with the
    // credential saved for its origin. `remote.auth` is `AuthMethod::None`
    // whenever this link was restarted from `sync.json` (which never stores
    // a real credential — docs/HARDENING_CONTRACT.md decision 4); `resolve`
    // finds the credential saved for this origin, if any, the same way
    // `addRemoteStore`/`setStoreSync`/`listRemoteStores` do.
    let via_account = handler
        .store_manager_handle()
        .read()
        .await
        .read_sync_config(store_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|config| config.via_account);
    let (client, connected_as) = if via_account {
        let connection = account::connect(handler, store_id, &remote.url).await?;
        (connection.client, Some(connection.connected_as))
    } else {
        let auth = handler.credentials().resolve(&remote.url, &remote.auth).await;
        let client = PimbleClient::connect_with_auth(remote.url.as_str(), &auth)
            .await
            .map_err(|e| anyhow::anyhow!("{}", pimble_client::describe_connect_error(&remote.url, &e)))?;
        (client, None)
    };
    let client = Arc::new(client);
    // How often the grant is asked about: only for a link held through the
    // account (a link with a saved credential has no grant to change).
    let grant_check_every = handler.grant_check_every();
    let runtime = handler.runtime().clone();
    let mut grant_tick = connected_as.as_ref().map(|_| runtime.interval_at(runtime.now() + grant_check_every, grant_check_every));

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
    let mut channels = Channels { remote_sub, local_rx, debouncer: Arc::new(ReconcileDebouncer::new(handler.runtime().clone())), reconcile_tx };

    reconcile::full_reconcile(handler, &client, store_id, link_id, &mut channels).await?;

    set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
    info!("Sync link for store {} connected to {}", store_id, remote.url);
    // Presence rides beside the edits, for as long as this connection lives
    // (docs/PRESENCE_CONTRACT.md section 2).
    let _presence = crate::presence_link::spawn(handler, Arc::clone(&client), store_id, link_id, false);
    // Pictures ride beside the documents that name them, both ways
    // (docs/IMAGES_CONTRACT.md "Replication"), for as long as this
    // connection lives.
    let pictures = crate::pictures::spawn_copier(handler, store_id, crate::pictures::PlainRemote { client: Arc::clone(&client), store_id }).await;
    pictures.all();

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
                        forward_local_change(handler, &client, store_id, link_id, local_change, &channels.debouncer, &channels.reconcile_tx).await?;
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
            // A role the owner changed, another share of the store, a
            // removal: what this connection may do was settled by the token
            // it was made with, so the answer is to connect again, which
            // reads the grant and mints a token from it.
            _ = next_grant_check(&mut grant_tick) => {
                if let Some(connected_as) = &connected_as {
                    if account::grant_changed(handler, store_id, connected_as).await {
                        info!("Sync link for store {}: the account's grant on it has changed; connecting again", store_id);
                        return Ok(());
                    }
                }
            }
            trigger = reconcile_rx.recv() => {
                match trigger {
                    Some(node_id) => {
                        reconcile::reconcile_node(handler, &client, store_id, node_id, link_id).await?;
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
pub(super) struct Channels {
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
    pub(super) async fn drain(&mut self, handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, link_id: &str) -> anyhow::Result<()> {
        loop {
            match self.local_rx.try_recv() {
                Ok(change) => forward_local_change(handler, client, store_id, link_id, change, &self.debouncer, &self.reconcile_tx).await?,
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
            match handler.runtime().timeout(Duration::ZERO, self.remote_sub.next()).await {
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
        | StoreChangeKind::AttachmentChanged { node_id, .. }
        | StoreChangeKind::ContentUpdated { node_id } => Some(*node_id),
        StoreChangeKind::TreeStructure { node_ids } => node_ids.first().copied(),
        StoreChangeKind::SyncStateChanged { .. }
        | StoreChangeKind::MountStateChanged { .. }
        | StoreChangeKind::ShareStateChanged { .. }
        | StoreChangeKind::SharesEnded { .. }
        | StoreChangeKind::PeopleChanged
        | StoreChangeKind::IndexStateChanged { .. }
        | StoreChangeKind::VaultAppended { .. }
        | StoreChangeKind::VaultReported { .. } => None,
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
    handler: &RpcHandler,
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
            if reconcile::send_edit(handler, client, store_id, node_id, link_id, changes).await? {
                debug!("Sync link for store {} forwarded a local update to node {}", store_id, node_id);
            }
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
pub(super) async fn apply_update_locally(handler: &RpcHandler, store_id: StoreId, node_id: NodeId, link_id: &str, update: &[u8], repair: Repair) -> anyhow::Result<()> {
    handler
        .apply_node_update_from(store_id, node_id, update, Some(link_id), repair)
        .await
        .map_err(|e| anyhow::anyhow!("local applyEdit for node {} failed: {}", node_id, e.message()))?;
    Ok(())
}

// ── Debounced reconcile triggers ────────────────────────────────────

/// Per-node debounce generations for [`schedule_node_reconcile`], same
/// pattern as `StoreIndexer::schedule_content_upsert` in `handler.rs`: bump
/// a counter, spawn a task that sleeps then fires only if no newer trigger
/// has arrived.
struct ReconcileDebouncer {
    node_generation: Mutex<HashMap<NodeId, u64>>,
    /// What its tasks run and sleep on.
    runtime: Runtime,
}

impl ReconcileDebouncer {
    fn new(runtime: Runtime) -> Self {
        Self { node_generation: Mutex::new(HashMap::new()), runtime }
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
    let runtime = debouncer.runtime.clone();
    runtime.clone().spawn(async move {
        runtime.sleep(RECONCILE_DEBOUNCE).await;
        let still_current = debouncer.node_generation.lock().unwrap().get(&node_id).copied() == Some(generation);
        if still_current {
            let _ = reconcile_tx.send(node_id);
        }
    });
}
