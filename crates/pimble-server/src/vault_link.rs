//! Vault link: keeps a local `Plain` store's node documents mirrored,
//! encrypted, to its hosted `Vault` twin under the same store id
//! (docs/CRYPTO_CONTRACT.md "Desktop (E, after B): sign-in and the
//! encrypting link"). Started by `RpcHandler::ensure_vault_link_started`
//! (`cloudHostStore`, `cloudAddHostedStore`, and `openStore` restarting one
//! from `sync.json`'s `mode: "vault"`), replacing [`crate::sync_link::SyncLink`]
//! for a vault-linked store.
//!
//! Unlike a plain sync link — two independent CRDT peers reconciling by yrs
//! state vector — a vault link has only one real peer: the local `Plain`
//! store. The hosted twin is an opaque, append-only, encrypted log per
//! document that this link authors together with every other device on the
//! account. A vault document is a node document (`VaultDocId::Node`,
//! docs/NODE_DOCUMENT_CONTRACT.md section 4): text, place in the tree and
//! metadata in one, so the link knows nothing about content or structure.
//! The `tree` document of the layout before is never written; one a hosted
//! twin still lists is skipped on pull, its content superseded by the
//! migrated node documents.
//!
//! - **At start** (and after every reconnect): `vaultListDocs`, then for
//!   every node document the remote already has, `vaultFetch` from
//!   `sync.json`'s remembered `last_seq` for that document, decrypt every
//!   snapshot/update with [`pimble_crypto::Blob::decrypt`] (the key looked
//!   up in the keystore by the blob's own key id — logged and skipped, not
//!   fatal, on an unknown one) and apply it locally through the handler's
//!   own `apply_node_update_from` with `client_id = "vault-link:<uuid>"`,
//!   repairing the tree once every document is pulled (a repair between two
//!   documents would judge a half-arrived tree). Then, for every document
//!   this store holds, the link pushes whatever the remote lacks: the whole
//!   document when the remote has never seen it (head `0`) — this is what
//!   makes `cloudHostStore` upload a store's pre-existing content, since
//!   nothing about creating the hosted twin itself produces local-change
//!   notifications for what already existed before the link started — and
//!   otherwise the diff since the state the remote is known to hold (see
//!   `Progress`).
//! - **Live**: subscribes to the hosted store's `storeChanged` and applies
//!   every `VaultAppended` blob the same way, recognizing (and dropping) its
//!   own appends echoed back through that same subscription by the
//!   `(doc_id, seq)` pairs it remembers handing back from `vaultAppend`.
//! - **Outbound**: subscribes to this server's local-change broadcast; every
//!   notification about a document carries that document's update bytes,
//!   which are encrypted and appended as they are, structure and content
//!   alike; one with no bytes (an older server's) pushes that document's
//!   current whole state instead.
//! - **Snapshots**: whenever an append's returned sequence number is a
//!   multiple of 200, the link uploads that document's current full state as
//!   a `vaultSnapshot` covering everything up to that sequence, so a fresh
//!   peer's first `vaultFetch` never replays the whole history.
//!
//! **Connecting**: `sync.json` never holds a credential (matching
//! `crate::sync_link`'s decision 4) — a vault link instead asks
//! `RpcHandler::keystore` for the currently signed-in account fresh on
//! every connect attempt, mints a JWT via `POST {cloud url}/api/v1/token`,
//! and connects to the store's hosted RPC url with that as `Bearer`. No
//! signed-in account (or a mint failure) is treated like any other
//! connection failure: `Offline`, retried with backoff.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::{DateTime, Utc};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreId, SyncState};
use pimble_crypto::Blob;
use pimble_rpc::{StoreChangeKind, StoreChangedNotification, VaultCursor, VaultDocId};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use url::Url;
use uuid::Uuid;

use crate::handler::{LocalChange, Repair, RpcHandler};

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Upload a snapshot of a document once this many entries have been appended
/// since the last one (docs/CRYPTO_CONTRACT.md), and only when this device has
/// applied every one of them (see `pimble_rpc::VaultCursor`).
const SNAPSHOT_EVERY: u64 = 200;
/// `ProgressFile::cursor_version` once a replica's cursors are gap-aware. A
/// file below it (or none) was written when `sync.json`'s `last_seq` meant "the
/// highest number seen", which may sit beyond an entry that never arrived; one
/// full refetch of every document settles it.
const CURSOR_VERSION: u32 = 1;
/// A reconnect's catch-up push is one blob per document; the hosted server
/// refuses a blob over 4 MiB, and one refused blob must not take the link down.
const MAX_CATCH_UP_BLOB: usize = 3 * 1024 * 1024;
/// How often the live loop writes `vault-link.json` when it has changed.
const PROGRESS_SAVE_EVERY: Duration = Duration::from_secs(5);

/// A running vault link for one store, mirroring [`crate::sync_link::SyncLinkHandle`]'s API.
pub struct VaultLinkHandle {
    state_rx: watch::Receiver<SyncState>,
    last_sync: Arc<Mutex<Option<DateTime<Utc>>>>,
    join: JoinHandle<()>,
}

impl VaultLinkHandle {
    pub fn state(&self) -> SyncState {
        self.state_rx.borrow().clone()
    }

    pub fn last_sync(&self) -> Option<DateTime<Utc>> {
        *self.last_sync.lock().unwrap()
    }

    /// Stop the link's background task. Safe to call more than once.
    pub fn stop(&self) {
        self.join.abort();
    }
}

/// Namespace for [`VaultLink::start`].
pub struct VaultLink;

impl VaultLink {
    /// Start a vault link for `store_id`, whose hosted twin's RPC endpoint
    /// is `rpc_url` (from `mint_token`'s `rpc_url`, saved in `sync.json`'s
    /// `remote.url`) and whose blobs this link encrypts with `key_id` (the
    /// key `cloudHostStore`/`cloudAddHostedStore` recorded in `sync.json`
    /// as `vault_key_id`, looked up in the keystore fresh each connect —
    /// never cached here, so a key added to the keystore after this link
    /// started is picked up on the next reconnect).
    pub fn start(handler: RpcHandler, store_id: StoreId, rpc_url: Url, key_id: Uuid, last_sync: Option<DateTime<Utc>>) -> VaultLinkHandle {
        let link_id = format!("vault-link:{}", Uuid::new_v4());
        let (state_tx, state_rx) = watch::channel(SyncState::Syncing);
        let last_sync = Arc::new(Mutex::new(last_sync));

        let state = LinkState { store_id, state_tx, last_sync: Arc::clone(&last_sync) };
        let join = tokio::spawn(run_loop(handler, rpc_url, key_id, link_id, state));

        VaultLinkHandle { state_rx, last_sync, join }
    }
}

struct LinkState {
    store_id: StoreId,
    state_tx: watch::Sender<SyncState>,
    last_sync: Arc<Mutex<Option<DateTime<Utc>>>>,
}

fn state_kind(state: &SyncState) -> u8 {
    match state {
        SyncState::Offline => 0,
        SyncState::Syncing => 1,
        SyncState::Synced { .. } => 2,
        SyncState::Conflict { .. } => 3,
    }
}

async fn set_state(handler: &RpcHandler, link: &LinkState, state: SyncState) {
    let previous = link.state_tx.borrow().clone();
    let transitioned = state_kind(&previous) != state_kind(&state);

    if let SyncState::Synced { last_sync } = &state {
        *link.last_sync.lock().unwrap() = Some(*last_sync);
    }
    let _ = link.state_tx.send(state.clone());

    if !transitioned {
        return;
    }
    info!("Vault link for store {} -> {:?}", link.store_id, state);
    handler.notify_sync_state_changed(link.store_id, state).await;
}

async fn run_loop(handler: RpcHandler, rpc_url: Url, key_id: Uuid, link_id: String, link: LinkState) {
    let store_id = link.store_id;
    // Subscribed once, for the life of the task: a change made while the link
    // is down still arrives here, which is how the link knows what to push
    // once it is back (see `Progress`).
    let mut local_rx = handler.subscribe_local_changes().await;
    let mut progress = Progress::load(&handler, store_id).await;
    let mut backoff = INITIAL_BACKOFF;
    loop {
        let mut reached_synced = false;
        let result = connect_and_sync(&handler, &rpc_url, key_id, &link_id, &link, &mut local_rx, &mut progress, &mut reached_synced).await;
        if reached_synced {
            backoff = INITIAL_BACKOFF;
        }
        if let Err(e) = result {
            warn!("Vault link for store {} to {} dropped: {}", store_id, rpc_url, e);
            set_state(&handler, &link, SyncState::Offline).await;
            note_local_changes_for(&mut local_rx, &mut progress, store_id, &link_id, backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }
}

/// Wait out a backoff, recording which documents change locally meanwhile.
async fn note_local_changes_for(
    local_rx: &mut broadcast::Receiver<LocalChange>,
    progress: &mut Progress,
    store_id: StoreId,
    link_id: &str,
    wait: Duration,
) {
    let deadline = tokio::time::sleep(wait);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            change = local_rx.recv() => match change {
                Ok(change) => progress.note(store_id, link_id, &change),
                Err(broadcast::error::RecvError::Lagged(_)) => progress.mark_all_dirty(),
                Err(broadcast::error::RecvError::Closed) => {
                    (&mut deadline).await;
                    break;
                }
            },
        }
    }
    progress.save_if_unsaved().await;
}

/// Mint a fresh JWT from the currently signed-in account and connect to
/// `rpc_url` with it as `Bearer`. Fails (and so goes through the retry
/// loop's backoff) with no signed-in account, a mint failure, or a
/// connection failure — all indistinguishable to a caller beyond the error
/// text, same as `crate::sync_link`'s own connect failures.
async fn connect(handler: &RpcHandler, rpc_url: &Url) -> anyhow::Result<PimbleClient> {
    let account = handler
        .keystore()
        .account()
        .await
        .ok_or_else(|| anyhow::anyhow!("no Pimble Cloud account is signed in"))?;
    let minted = crate::cloud::mint_token(&account.url, &account.session)
        .await
        .map_err(|e| anyhow::anyhow!("minting a cloud token from {} failed: {}", account.url, e))?;
    let auth = AuthMethod::Bearer { token: minted.token };
    PimbleClient::connect_with_auth(rpc_url.as_str(), &auth)
        .await
        .map_err(|e| anyhow::anyhow!("{}", pimble_client::describe_connect_error(rpc_url, &e)))
}

#[allow(clippy::too_many_arguments)]
async fn connect_and_sync(
    handler: &RpcHandler,
    rpc_url: &Url,
    key_id: Uuid,
    link_id: &str,
    link: &LinkState,
    local_rx: &mut broadcast::Receiver<LocalChange>,
    progress: &mut Progress,
    reached_synced: &mut bool,
) -> anyhow::Result<()> {
    let store_id = link.store_id;
    set_state(handler, link, SyncState::Syncing).await;

    let client = connect(handler, rpc_url).await?;

    // Subscribed before the reconcile, not after it: an append that lands
    // between the pull and the subscription would otherwise be seen by
    // neither. Anything the pull already applied arrives again here and
    // merges to nothing.
    let mut remote_sub = client
        .subscribe_store_changes(store_id)
        .await
        .map_err(|e| anyhow::anyhow!("subscribe to {} storeChanged failed: {}", rpc_url, e))?;

    // Whatever changed locally while no connection was up is not pushed as
    // the deltas it arrived as (the link was not there to push them in
    // order); the documents are marked and the reconcile pushes what the
    // remote lacks of each.
    loop {
        match local_rx.try_recv() {
            Ok(change) => progress.note(store_id, link_id, &change),
            Err(broadcast::error::TryRecvError::Lagged(_)) => progress.mark_all_dirty(),
            Err(_) => break,
        }
    }

    let echoes = Arc::new(EchoTracker::new());
    full_reconcile(handler, &client, store_id, key_id, link_id, progress, &echoes).await?;

    let mut save_tick = tokio::time::interval(PROGRESS_SAVE_EVERY);

    set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
    *reached_synced = true;
    info!("Vault link for store {} connected to {}", store_id, rpc_url);

    loop {
        tokio::select! {
            item = remote_sub.next() => {
                match item {
                    Some(Ok(notif)) => {
                        handle_remote_notification(handler, store_id, link_id, notif, &echoes, progress).await?;
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Some(Err(e)) => return Err(anyhow::anyhow!("remote notification decode error: {}", e)),
                    None => return Err(anyhow::anyhow!("remote subscription closed")),
                }
            }
            change = local_rx.recv() => {
                match change {
                    Ok(local_change) => {
                        let doc = changed_doc(store_id, link_id, &local_change);
                        if let Err(e) = forward_local_change(handler, &client, store_id, key_id, link_id, local_change, &echoes, progress).await {
                            // Taken off the channel and not delivered: the
                            // next reconcile has to carry it.
                            if let Some(doc) = doc {
                                progress.mark_dirty(&doc);
                            }
                            progress.save_if_unsaved().await;
                            return Err(e);
                        }
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        progress.mark_all_dirty();
                        progress.save_if_unsaved().await;
                        return Err(anyhow::anyhow!("missed {} local notifications (broadcast lag); reconnecting to reconcile", n));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(anyhow::anyhow!("local change broadcast closed"));
                    }
                }
            }
            _ = save_tick.tick() => {
                progress.save_if_unsaved().await;
            }
        }
    }
}

// ── What the remote is known to hold ────────────────────────────────────
//
// The hosted twin stores ciphertext: it has no state vector to answer with,
// so "what does the remote lack?" cannot be asked of it the way a plain sync
// link asks. The link keeps the answer itself, per document, in
// `<store>/vault-link.json`:
//
// - `pushed_sv`: a state vector everything up to which is known to be on the
//   remote. It advances with every update this link appends or pulls, and only
//   when the update continues from what is already known
//   (`pimble_crdt::advance_state_vector`), so it can lag but never overstate.
//   A lagging vector costs a slightly larger catch-up push; an overstated one
//   would skip structs for good.
// - `dirty`: documents that changed locally while no connection was up, or
//   whose push failed. Needed beside the state vector because a deletion moves
//   no clock: a document edited only by deleting looks identical by vector.
//
// Until 2026-09-17 a reconnect pulled what it had missed and pushed only
// documents the remote had never seen, so an edit made while the link was down
// (a hosted-server restart is enough) never left the machine. Every later edit
// from that device depends on it, and every other device held those as pending
// and showed none of them: the desktop's typing never reached the web app.

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct ProgressFile {
    #[serde(default)]
    pushed_sv: HashMap<String, String>,
    #[serde(default)]
    dirty: BTreeSet<String>,
    #[serde(default)]
    dirty_all: bool,
    #[serde(default)]
    cursor_version: u32,
}

struct Progress {
    path: Option<PathBuf>,
    file: ProgressFile,
    unsaved: bool,
    /// How far into each document's log this device has read without a gap.
    /// Rebuilt at every reconcile from `sync.json`'s `last_seq`, which holds
    /// exactly `VaultCursor::applied_through` (a restart loses only the
    /// entries noted beyond a gap, which are fetched again and merge to
    /// nothing).
    cursors: HashMap<String, VaultCursor>,
    /// Each document's snapshot number as last known, for deciding when the
    /// next one is due.
    snapshot_seqs: HashMap<String, u64>,
}

impl Progress {
    const FILE_NAME: &'static str = "vault-link.json";

    async fn load(handler: &RpcHandler, store_id: StoreId) -> Self {
        let path = {
            let manager = handler.store_manager_handle();
            let manager = manager.read().await;
            manager
                .get_store_info(store_id)
                .ok()
                .and_then(|store| store.local_path().map(|p| p.join(Self::FILE_NAME)))
        };
        let file = match &path {
            Some(path) => match tokio::fs::read_to_string(path).await {
                Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                    warn!("Vault link for store {}: unreadable {} ({}); starting over", store_id, Self::FILE_NAME, e);
                    ProgressFile::default()
                }),
                Err(_) => ProgressFile::default(),
            },
            None => ProgressFile::default(),
        };
        Self { path, file, unsaved: false, cursors: HashMap::new(), snapshot_seqs: HashMap::new() }
    }

    fn cursor(&mut self, doc_id: &VaultDocId) -> &mut VaultCursor {
        self.cursors.entry(doc_id.as_str()).or_default()
    }

    fn snapshot_seq(&self, doc_id: &VaultDocId) -> u64 {
        self.snapshot_seqs.get(&doc_id.as_str()).copied().unwrap_or(0)
    }

    fn note_snapshot(&mut self, doc_id: &VaultDocId, seq: u64) {
        let held = self.snapshot_seqs.entry(doc_id.as_str()).or_insert(0);
        *held = (*held).max(seq);
    }

    /// The state vector the remote is known to hold for `doc_id` (empty when
    /// nothing is recorded: a replica from before this file existed pushes
    /// each document's whole state once, which is also what heals one whose
    /// earlier edits never left).
    fn known(&self, doc_id: &VaultDocId) -> Vec<u8> {
        self.file
            .pushed_sv
            .get(&doc_id.as_str())
            .and_then(|b64| STANDARD.decode(b64).ok())
            .unwrap_or_else(pimble_crdt::empty_state_vector)
    }

    fn set_known(&mut self, doc_id: &VaultDocId, state_vector: &[u8]) {
        let encoded = STANDARD.encode(state_vector);
        if self.file.pushed_sv.get(&doc_id.as_str()) != Some(&encoded) {
            self.file.pushed_sv.insert(doc_id.as_str(), encoded);
            self.unsaved = true;
        }
    }

    /// `update` is on the remote (this link appended it, or pulled it).
    fn advance(&mut self, doc_id: &VaultDocId, update: &[u8]) {
        match pimble_crdt::advance_state_vector(&self.known(doc_id), update) {
            Ok(state_vector) => self.set_known(doc_id, &state_vector),
            Err(e) => debug!("Vault link: could not advance the known state of {:?}: {}", doc_id, e),
        }
    }

    fn is_dirty(&self, doc_id: &VaultDocId) -> bool {
        self.file.dirty_all || self.file.dirty.contains(&doc_id.as_str())
    }

    fn mark_dirty(&mut self, doc_id: &VaultDocId) {
        if self.file.dirty.insert(doc_id.as_str()) {
            self.unsaved = true;
        }
    }

    fn mark_all_dirty(&mut self) {
        if !self.file.dirty_all {
            self.file.dirty_all = true;
            self.unsaved = true;
        }
    }

    fn clear_dirty(&mut self, doc_id: &VaultDocId) {
        if self.file.dirty.remove(&doc_id.as_str()) {
            self.unsaved = true;
        }
    }

    fn clear_all_dirty(&mut self) {
        if self.file.dirty_all || !self.file.dirty.is_empty() {
            self.file.dirty_all = false;
            self.file.dirty.clear();
            self.unsaved = true;
        }
    }

    /// Record a local change seen while no connection could carry it.
    fn note(&mut self, store_id: StoreId, link_id: &str, change: &LocalChange) {
        if let Some(doc_id) = changed_doc(store_id, link_id, change) {
            self.mark_dirty(&doc_id);
        }
    }

    /// Best effort, like `record_last_seq`: bookkeeping, never fatal.
    async fn save_if_unsaved(&mut self) {
        if !self.unsaved {
            return;
        }
        let Some(path) = &self.path else { return };
        let json = match serde_json::to_string(&self.file) {
            Ok(json) => json,
            Err(e) => {
                warn!("Vault link: could not encode {}: {}", Self::FILE_NAME, e);
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        let written = async {
            tokio::fs::write(&tmp, json).await?;
            tokio::fs::rename(&tmp, path).await
        }
        .await;
        match written {
            Ok(()) => self.unsaved = false,
            Err(e) => debug!("Vault link: could not write {}: {}", path.display(), e),
        }
    }
}

/// Forget what the remote was known to hold: the store is no longer linked to
/// that twin, and a record kept for one remote would overstate what another
/// holds. Called when a store is unlinked or linked elsewhere.
pub(crate) async fn forget_progress(handler: &RpcHandler, store_id: StoreId) {
    let progress = Progress::load(handler, store_id).await;
    if let Some(path) = progress.path {
        let _ = tokio::fs::remove_file(path).await;
    }
}

/// Which vault document a local change touches, if it is one this link would
/// push: this store's, about a document, and not this link's own apply coming
/// back around.
fn changed_doc(store_id: StoreId, link_id: &str, change: &LocalChange) -> Option<VaultDocId> {
    let LocalChange::Store(notif) = change else { return None };
    if notif.store_id != store_id || notif.source_client_id.as_deref() == Some(link_id) {
        return None;
    }
    document_of(&notif.change_kind).map(VaultDocId::Node)
}

/// The node document a notification is about, when it is about one (the
/// link's and the mounts' states are derived and never pushed; a
/// `VaultAppended` is the remote's, never a local change).
fn document_of(kind: &StoreChangeKind) -> Option<NodeId> {
    match kind {
        StoreChangeKind::NodeCreated { node_id, .. }
        | StoreChangeKind::NodeDeleted { node_id, .. }
        | StoreChangeKind::NodeMoved { node_id, .. }
        | StoreChangeKind::MetadataUpdated { node_id }
        | StoreChangeKind::ContentUpdated { node_id } => Some(*node_id),
        StoreChangeKind::TreeStructure { node_ids } => node_ids.first().copied(),
        StoreChangeKind::VaultAppended { .. }
        | StoreChangeKind::SyncStateChanged { .. }
        | StoreChangeKind::MountStateChanged { .. }
        | StoreChangeKind::ShareStateChanged { .. } => None,
    }
}

// ── sync.json's per-document last_seq ────────────────────────────────────

/// Read `sync.json`'s remembered `last_seq` for `doc_id` (0 if unset or
/// there's no `sync.json` at all — a defensive fallback; a vault-linked
/// store always has one by the time a link starts).
async fn read_last_seq(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId) -> u64 {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    match manager.read_sync_config(store_id).await {
        Ok(Some(config)) => config.last_seq.get(&doc_id.as_str()).copied().unwrap_or(0),
        _ => 0,
    }
}

/// Record how far `doc_id`'s log has been read without a gap
/// (`VaultCursor::applied_through`), which is where the next connect fetches
/// from. Best effort: a store closed or unlinked under us, or a write
/// failure, is logged, never propagated (bookkeeping, not part of the
/// protocol).
async fn record_last_seq(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId, applied_through: u64) {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let mut config = match manager.read_sync_config(store_id).await {
        Ok(Some(config)) => config,
        Ok(None) => return,
        Err(e) => {
            debug!("Could not read sync.json for store {} to record last_seq: {}", store_id, e);
            return;
        }
    };
    let key = doc_id.as_str();
    if config.last_seq.get(&key).copied().unwrap_or(0) == applied_through {
        return;
    }
    config.last_seq.insert(key, applied_through);
    if let Err(e) = manager.write_sync_config(store_id, &config).await {
        warn!("Could not record last_seq for store {} doc {:?}: {}", store_id, doc_id, e);
    }
}

// ── Full reconcile (link start) ──────────────────────────────────────────

/// Pull every node document the remote lists, repair the tree once they are
/// all in, then push, for every document this store holds, whatever the
/// remote lacks: the whole document when the remote has never seen it (the
/// seeding step `cloudHostStore` depends on), otherwise the diff since the
/// state the remote is known to hold, for a document that is ahead of it or
/// that changed while the link was down (see `Progress`).
async fn full_reconcile(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    key_id: Uuid,
    link_id: &str,
    progress: &mut Progress,
    echoes: &Arc<EchoTracker>,
) -> anyhow::Result<()> {
    let remote_docs = client.vault_list_docs(store_id).await.map_err(|e| anyhow::anyhow!("remote vaultListDocs failed: {}", e))?;

    // Where each document's log has been read to. A replica whose `last_seq`
    // predates gap-aware cursors reads everything once more from the start
    // (a merge repeated is nothing; an entry skipped is lost).
    let refetch_all = progress.file.cursor_version < CURSOR_VERSION;
    if refetch_all {
        info!("Vault link for store {}: reading every document's log once from the start", store_id);
    }
    progress.cursors.clear();
    let mut remote_heads: HashMap<VaultDocId, u64> = HashMap::new();
    for doc in remote_docs {
        if doc.doc_id == VaultDocId::Tree {
            // The layout before this one kept the tree in a document of its
            // own; a twin hosted then still lists it. Nothing reads it: the
            // migrated node documents carry the tree now.
            debug!("Vault link for store {}: skipping the retired tree document (head {})", store_id, doc.head);
            continue;
        }
        let applied_through = if refetch_all { 0 } else { read_last_seq(handler, store_id, &doc.doc_id).await };
        progress.cursors.insert(doc.doc_id.as_str(), VaultCursor::starting_at(applied_through));
        progress.note_snapshot(&doc.doc_id, doc.snapshot_seq);
        remote_heads.insert(doc.doc_id, doc.head);
    }

    for doc_id in remote_heads.keys() {
        pull_doc(handler, client, store_id, doc_id, link_id, progress).await?;
    }
    // Every document is in before the tree is judged (see `Repair::Later`).
    handler.repair_store_tree(store_id).await;

    let doc_ids: Vec<VaultDocId> = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        manager.doc_ids(store_id).map(|ids| ids.into_iter().map(VaultDocId::Node).collect()).unwrap_or_default()
    };
    for doc_id in doc_ids {
        let unseen = remote_heads.get(&doc_id).copied().unwrap_or(0) == 0;
        let known = if unseen { pimble_crdt::empty_state_vector() } else { progress.known(&doc_id) };
        let (local_sv, diff) = doc_diff(handler, store_id, &doc_id, &known).await?;
        // An undecodable vector reads as "ahead": pushing too much is a
        // merge to nothing, pushing too little is a lost edit.
        let ahead = pimble_crdt::state_vector_exceeds(&local_sv, &known).unwrap_or(true);
        if !(unseen || ahead || progress.is_dirty(&doc_id)) {
            continue;
        }
        if diff.len() > MAX_CATCH_UP_BLOB {
            warn!(
                "Vault link for store {} doc {:?}: a catch-up push of {} bytes is over the blob limit; leaving it for live updates",
                store_id, doc_id, diff.len()
            );
            continue;
        }
        append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, &diff, echoes, progress).await?;
        progress.set_known(&doc_id, &local_sv);
        progress.clear_dirty(&doc_id);
    }
    progress.clear_all_dirty();
    if progress.file.cursor_version != CURSOR_VERSION {
        progress.file.cursor_version = CURSOR_VERSION;
        progress.unsaved = true;
    }
    progress.save_if_unsaved().await;

    Ok(())
}

/// Fetch and apply everything `doc_id` has beyond what this device has read
/// without a gap. Only what was applied moves the cursor: a blob that would not
/// decrypt stays in front of it, to be asked for again. No repair here: the
/// caller repairs once every document is pulled.
async fn pull_doc(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, doc_id: &VaultDocId, link_id: &str, progress: &mut Progress) -> anyhow::Result<()> {
    let after_seq = progress.cursor(doc_id).applied_through();
    let fetch = client
        .vault_fetch(store_id, doc_id.clone(), after_seq)
        .await
        .map_err(|e| anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e))?;

    if let Some(entry) = &fetch.snapshot {
        progress.note_snapshot(doc_id, entry.seq);
        if let Some(update) = apply_blob_locally(handler, store_id, doc_id, &entry.blob, link_id, Repair::Later).await? {
            progress.advance(doc_id, &update);
            progress.cursor(doc_id).mark_through(entry.seq);
        }
    }
    for entry in &fetch.updates {
        if let Some(update) = apply_blob_locally(handler, store_id, doc_id, &entry.blob, link_id, Repair::Later).await? {
            progress.advance(doc_id, &update);
            progress.cursor(doc_id).mark(entry.seq);
        }
    }
    let applied_through = progress.cursor(doc_id).applied_through();
    record_last_seq(handler, store_id, doc_id, applied_through).await;
    Ok(())
}

/// Decrypt one base64url blob and apply it locally through the handler's
/// own `apply_node_update_from`. An unknown key id or a decryption failure
/// is logged and skipped, never fatal to the link (docs/CRYPTO_CONTRACT.md:
/// "a blob with an unknown key id is logged and skipped"); so is a blob of
/// the retired tree document.
///
/// Returns the decrypted update when it was applied, `None` when the blob was
/// skipped, so the caller can record that the remote holds it.
async fn apply_blob_locally(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId, blob_b64url: &str, link_id: &str, repair: Repair) -> anyhow::Result<Option<Vec<u8>>> {
    let VaultDocId::Node(node_id) = doc_id else {
        debug!("Vault link for store {}: skipping a blob of the retired tree document", store_id);
        return Ok(None);
    };

    let blob = URL_SAFE_NO_PAD
        .decode(blob_b64url)
        .map_err(|e| anyhow::anyhow!("blob for {:?} is not valid base64url: {}", doc_id, e))?;

    let key_id = match Blob::key_id(&blob) {
        Ok(id) => id,
        Err(e) => {
            warn!("Vault link for store {} doc {:?}: malformed blob header ({}); skipping it", store_id, doc_id, e);
            return Ok(None);
        }
    };
    let Some(key) = handler.keystore().store_key(store_id, key_id).await else {
        warn!("Vault link for store {} doc {:?}: no key held for key id {}; skipping this blob", store_id, doc_id, key_id);
        return Ok(None);
    };

    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &doc_id.as_str());
    let plaintext = match Blob::decrypt(&key, &aad, &blob) {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!("Vault link for store {} doc {:?}: decryption failed ({}); skipping this blob", store_id, doc_id, e);
            return Ok(None);
        }
    };

    handler
        .apply_node_update_from(store_id, *node_id, &plaintext, Some(link_id), repair)
        .await
        .map_err(|e| anyhow::anyhow!("local applyEdit for node {} failed: {}", node_id, e.message()))?;
    Ok(Some(plaintext))
}

// ── Remote -> local (live) ───────────────────────────────────────────────

async fn handle_remote_notification(
    handler: &RpcHandler,
    store_id: StoreId,
    link_id: &str,
    notif: StoreChangedNotification,
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let StoreChangeKind::VaultAppended { doc_id, seq } = &notif.change_kind else {
        // A vault store only ever produces `VaultAppended`; anything else
        // reaching here would be a plain store's kinds, which can't happen
        // for a store this link's remote considers a vault twin.
        return Ok(());
    };

    // Identity first: every `vaultAppend` this link makes carries its own
    // `link_id` (see `push_update`), so an echo of it back through this same
    // subscription is recognized directly, the same way `apply_edit`'s
    // `client_id` already works. The seen-seq set is a second guard for
    // anything that reaches the server without a `client_id` (an older
    // caller, or the CLI/web client), not the primary mechanism — dropping
    // by `seq >= last_seq` instead would be wrong the moment a peer is also
    // appending concurrently, since this link's own seq is then not
    // reliably the highest.
    if notif.source_client_id.as_deref() == Some(link_id) {
        debug!("Vault link for store {} doc {:?}: dropping our own echoed append by id (seq {})", store_id, doc_id, seq);
        progress.cursor(doc_id).mark(*seq);
        return Ok(());
    }
    if echoes.take_if_present(doc_id, *seq) {
        debug!("Vault link for store {} doc {:?}: dropping our own echoed append by seq (seq {})", store_id, doc_id, seq);
        progress.cursor(doc_id).mark(*seq);
        return Ok(());
    }

    let Some(blob) = &notif.update else {
        warn!("Vault link for store {} doc {:?}: VaultAppended with no blob; skipping", store_id, doc_id);
        return Ok(());
    };
    if let Some(update) = apply_blob_locally(handler, store_id, doc_id, blob, link_id, Repair::Debounced).await? {
        progress.advance(doc_id, &update);
        progress.cursor(doc_id).mark(*seq);
    }
    let applied_through = progress.cursor(doc_id).applied_through();
    record_last_seq(handler, store_id, doc_id, applied_through).await;
    Ok(())
}

// ── Local -> remote (live) ────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn forward_local_change(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    key_id: Uuid,
    link_id: &str,
    change: LocalChange,
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let LocalChange::Store(notif) = change else {
        return Ok(());
    };
    // Our own applies (from `apply_blob_locally` above) carry this link's
    // own id; forwarding those back would just re-encrypt what we just
    // decrypted from the very same remote.
    let Some(doc_id) = changed_doc(store_id, link_id, &LocalChange::Store(notif.clone())) else {
        return Ok(());
    };
    match notif.update {
        Some(update_b64) => {
            let plaintext = STANDARD.decode(update_b64)?;
            push_update(handler, client, store_id, doc_id, key_id, link_id, &plaintext, echoes, progress).await?;
        }
        None => {
            // No bytes to reuse (an older server's notification): push the
            // document's current whole state instead.
            push_full(handler, client, store_id, doc_id, key_id, link_id, echoes, progress).await?;
        }
    }
    Ok(())
}

/// Encrypt `plaintext` and append it to `doc_id`'s vault log, recording the
/// echo and snapshotting if the returned sequence lands on a multiple of
/// [`SNAPSHOT_EVERY`].
#[allow(clippy::too_many_arguments)]
async fn append_blob(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    doc_id: VaultDocId,
    key_id: Uuid,
    link_id: &str,
    plaintext: &[u8],
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let Some(key) = handler.keystore().store_key(store_id, key_id).await else {
        return Err(anyhow::anyhow!("no local key held for store {} key id {}; cannot encrypt an outgoing update", store_id, key_id));
    };
    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &doc_id.as_str());
    let blob = Blob::encrypt(&key, key_id, &aad, plaintext);
    let blob_b64 = URL_SAFE_NO_PAD.encode(&blob);

    // Attributed to this link's own id (`PimbleClient::vault_append_from`)
    // so the notification it produces is dropped by identity if it echoes
    // back through this link's own subscription; the seen-seq set below is
    // kept as a second guard for anything that reaches the server without a
    // `client_id`.
    let seq = client
        .vault_append_from(store_id, doc_id.clone(), blob_b64, Some(link_id.to_string()))
        .await
        .map_err(|e| anyhow::anyhow!("remote vaultAppend for {:?} failed: {}", doc_id, e))?;
    echoes.remember(&doc_id, seq);
    progress.cursor(&doc_id).mark(seq);
    let applied_through = progress.cursor(&doc_id).applied_through();
    record_last_seq(handler, store_id, &doc_id, applied_through).await;
    debug!("Vault link for store {} pushed doc {:?} update (seq {})", store_id, doc_id, seq);

    // A snapshot deletes every log entry at or below its number, so it may
    // only be stamped with a number this device has read *through*. Our own
    // append being `seq` says nothing about another device's `seq - 1`, which
    // may still be on its way here; until it lands `applied_through` stays
    // below `seq` and the snapshot waits for a later append.
    let due = seq.saturating_sub(progress.snapshot_seq(&doc_id)) >= SNAPSHOT_EVERY;
    if due && applied_through == seq {
        upload_snapshot(handler, client, store_id, doc_id.clone(), key_id, seq).await?;
        progress.note_snapshot(&doc_id, seq);
    }
    Ok(())
}

/// Append one local update as it arrived, and record that the remote holds it.
#[allow(clippy::too_many_arguments)]
async fn push_update(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    doc_id: VaultDocId,
    key_id: Uuid,
    link_id: &str,
    plaintext: &[u8],
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, plaintext, echoes, progress).await?;
    progress.advance(&doc_id, plaintext);
    Ok(())
}

/// Push a document's current full state as a fresh append (the fallback for a
/// change with no update bytes to reuse).
#[allow(clippy::too_many_arguments)]
async fn push_full(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    doc_id: VaultDocId,
    key_id: Uuid,
    link_id: &str,
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let (local_sv, state) = full_doc_state(handler, store_id, &doc_id).await?;
    append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, &state, echoes, progress).await?;
    progress.set_known(&doc_id, &local_sv);
    progress.clear_dirty(&doc_id);
    Ok(())
}

/// `doc_id`'s state vector and everything it has beyond `known`, read under
/// one lock so the vector describes exactly what the diff carries.
async fn doc_diff(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId, known: &[u8]) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let VaultDocId::Node(node_id) = doc_id else {
        return Err(anyhow::anyhow!("the tree document is retired; nothing to read of it"));
    };
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let state_vector = manager
        .node_state_vector(store_id, *node_id)
        .map_err(|e| anyhow::anyhow!("node document unavailable: {}", e))?;
    let diff = manager
        .node_diff_since(store_id, *node_id, known)
        .map_err(|e| anyhow::anyhow!("node document diff failed: {}", e))?;
    Ok((state_vector, diff))
}

/// `doc_id`'s state vector and its whole state (`NodeDoc::save`: a yrs
/// update encoding the document from an empty state vector, pending updates
/// included, which a diff from an empty vector leaves out), under one lock.
async fn full_doc_state(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let VaultDocId::Node(node_id) = doc_id else {
        return Err(anyhow::anyhow!("the tree document is retired; nothing to read of it"));
    };
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let tree = manager.tree(store_id).map_err(|e| anyhow::anyhow!("store unavailable: {}", e))?;
    let doc = tree.doc(*node_id).ok_or_else(|| anyhow::anyhow!("node document {} unavailable", node_id))?;
    Ok((doc.state_vector(), doc.save()))
}

async fn upload_snapshot(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, doc_id: VaultDocId, key_id: Uuid, upto_seq: u64) -> anyhow::Result<()> {
    let Some(key) = handler.keystore().store_key(store_id, key_id).await else {
        return Ok(()); // Can't snapshot without the key; the log still has everything.
    };
    let (_, plaintext) = full_doc_state(handler, store_id, &doc_id).await?;
    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &doc_id.as_str());
    let blob = Blob::encrypt(&key, key_id, &aad, &plaintext);
    let blob_b64 = URL_SAFE_NO_PAD.encode(&blob);
    client
        .vault_snapshot(store_id, doc_id.clone(), upto_seq, blob_b64)
        .await
        .map_err(|e| anyhow::anyhow!("remote vaultSnapshot for {:?} failed: {}", doc_id, e))?;
    debug!("Vault link for store {} uploaded a snapshot for doc {:?} up to seq {}", store_id, doc_id, upto_seq);
    Ok(())
}

// ── Echo tracking (decision: docs/CRYPTO_CONTRACT.md "remembering its own
// appended seqs to drop echoes") ─────────────────────────────────────────

/// Remembers `(doc_id, seq)` pairs this link itself just appended, so the
/// `VaultAppended` notification the hosted server echoes back through this
/// same link's own subscription is recognized and dropped rather than
/// re-applied.
struct EchoTracker {
    seqs: Mutex<HashMap<String, HashSet<u64>>>,
}

impl EchoTracker {
    fn new() -> Self {
        Self { seqs: Mutex::new(HashMap::new()) }
    }

    fn remember(&self, doc_id: &VaultDocId, seq: u64) {
        self.seqs.lock().unwrap().entry(doc_id.as_str()).or_default().insert(seq);
    }

    /// Removes and reports whether `(doc_id, seq)` was remembered.
    fn take_if_present(&self, doc_id: &VaultDocId, seq: u64) -> bool {
        let mut seqs = self.seqs.lock().unwrap();
        match seqs.get_mut(&doc_id.as_str()) {
            Some(set) => set.remove(&seq),
            None => false,
        }
    }
}
