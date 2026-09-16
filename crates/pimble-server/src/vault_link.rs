//! Vault link: keeps a local `Plain` store's tree and node content mirrored,
//! encrypted, to its hosted `Vault` twin under the same store id
//! (docs/CRYPTO_CONTRACT.md "Desktop (E, after B): sign-in and the
//! encrypting link"). Started by `RpcHandler::ensure_vault_link_started`
//! (`cloudHostStore`, `cloudAddHostedStore`, and `openStore` restarting one
//! from `sync.json`'s `mode: "vault"`), replacing [`crate::sync_link::SyncLink`]
//! for a vault-linked store.
//!
//! Unlike a plain sync link — two independent CRDT peers reconciling by yrs
//! state vector — a vault link has only one real peer: the local `Plain`
//! store. The hosted twin is an opaque, append-only, encrypted log this
//! link is the sole author of (until a second device links the same store):
//!
//! - **At start** (and after every reconnect): `vaultListDocs`, then for
//!   every document the remote already has, `vaultFetch` from
//!   `sync.json`'s remembered `last_seq` for that document, decrypt every
//!   snapshot/update with [`pimble_crypto::Blob::decrypt`] (the key looked
//!   up in the keystore by the blob's own key id — logged and skipped, not
//!   fatal, on an unknown one) and apply it locally through the handler's
//!   own `applyStoreUpdate`/`applyEdit` with `client_id =
//!   "vault-link:<uuid>"`. The store document is always reconciled first, so
//!   node ids it names are known locally before their own content is
//!   applied. Then, for every local document (the tree, and every node id
//!   the now-current local tree names) the remote does not have yet (head
//!   `0`), the link seeds the vault with that document's current full state
//!   (`ContentDoc`/`StoreDocument::save()`) — this is what makes
//!   `cloudHostStore` upload a store's pre-existing content, since nothing
//!   about creating the hosted twin itself produces local-change
//!   notifications for content that already existed before the link started.
//! - **Live**: subscribes to the hosted store's `storeChanged` and applies
//!   every `VaultAppended` blob the same way, recognizing (and dropping) its
//!   own appends echoed back through that same subscription by the
//!   `(doc_id, seq)` pairs it remembers handing back from `vaultAppend`.
//! - **Outbound**: subscribes to this server's local-change broadcast; a
//!   content edit (`ContentUpdated` with delta bytes) or a store-document
//!   delta (`TreeStructure` with bytes) is encrypted and appended directly;
//!   a structural change with no delta bytes to reuse (`NodeCreated`/
//!   `NodeDeleted`/`NodeMoved`/`MetadataUpdated`, or a full-snapshot
//!   `updateNodeContent`) instead pushes that document's current full state,
//!   debounced for the tree (a burst of moves/renames becomes one push).
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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::{DateTime, Utc};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreId, SyncState};
use pimble_crypto::Blob;
use pimble_rpc::{
    ApplyEditRequest, ApplyStoreUpdateRequest, EditOperation, PimbleApiServer, StoreChangeKind, StoreChangedNotification, VaultDocId,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use url::Url;
use uuid::Uuid;

use crate::handler::{LocalChange, RpcHandler};

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Debounce window for a structural tree change with no delta bytes to
/// reuse, so a burst of them (several renames/moves in a row) yields one
/// full-tree push.
const TREE_PUSH_DEBOUNCE: Duration = Duration::from_millis(200);
/// Upload a snapshot of a document whenever its head reaches a multiple of
/// this many appended updates (docs/CRYPTO_CONTRACT.md).
const SNAPSHOT_EVERY: u64 = 200;

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
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match connect_and_sync(&handler, &rpc_url, key_id, &link_id, &link).await {
            Ok(()) => backoff = INITIAL_BACKOFF,
            Err(e) => {
                warn!("Vault link for store {} to {} dropped: {}", store_id, rpc_url, e);
                set_state(&handler, &link, SyncState::Offline).await;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
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

async fn connect_and_sync(handler: &RpcHandler, rpc_url: &Url, key_id: Uuid, link_id: &str, link: &LinkState) -> anyhow::Result<()> {
    let store_id = link.store_id;
    set_state(handler, link, SyncState::Syncing).await;

    let client = connect(handler, rpc_url).await?;

    full_reconcile(handler, &client, store_id, key_id, link_id).await?;

    let mut remote_sub = client
        .subscribe_store_changes(store_id)
        .await
        .map_err(|e| anyhow::anyhow!("subscribe to {} storeChanged failed: {}", rpc_url, e))?;
    let mut local_rx = handler.subscribe_local_changes().await;

    let echoes = Arc::new(EchoTracker::new());
    let tree_debouncer = Arc::new(TreePushDebouncer::new());
    let (push_tx, mut push_rx) = mpsc::unbounded_channel::<()>();

    set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
    info!("Vault link for store {} connected to {}", store_id, rpc_url);

    loop {
        tokio::select! {
            item = remote_sub.next() => {
                match item {
                    Some(Ok(notif)) => {
                        handle_remote_notification(handler, store_id, link_id, notif, &echoes).await?;
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Some(Err(e)) => return Err(anyhow::anyhow!("remote notification decode error: {}", e)),
                    None => return Err(anyhow::anyhow!("remote subscription closed")),
                }
            }
            change = local_rx.recv() => {
                match change {
                    Ok(local_change) => {
                        forward_local_change(handler, &client, store_id, key_id, link_id, local_change, &tree_debouncer, &push_tx, &echoes).await?;
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        return Err(anyhow::anyhow!("missed {} local notifications (broadcast lag); reconnecting to reconcile", n));
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(anyhow::anyhow!("local change broadcast closed"));
                    }
                }
            }
            trigger = push_rx.recv() => {
                match trigger {
                    Some(()) => {
                        push_full(handler, &client, store_id, VaultDocId::Tree, key_id, link_id, &echoes).await?;
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    None => return Err(anyhow::anyhow!("internal tree-push channel closed")),
                }
            }
        }
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

/// Record `seq` as the last-applied sequence for `doc_id`, if it advances
/// what's already there (never regresses it, so an out-of-order live
/// notification racing a reconcile can't undo progress). Best effort: a
/// store closed or unlinked under us, or a write failure, is logged, never
/// propagated (bookkeeping, not part of the protocol).
async fn record_last_seq(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId, seq: u64) {
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
    if config.last_seq.get(&key).copied().unwrap_or(0) >= seq {
        return;
    }
    config.last_seq.insert(key, seq);
    if let Err(e) = manager.write_sync_config(store_id, &config).await {
        warn!("Could not record last_seq for store {} doc {:?}: {}", store_id, doc_id, e);
    }
}

// ── Full reconcile (link start) ──────────────────────────────────────────

/// Pull every document the remote already lists (tree first, so node ids it
/// names are known locally before their content is applied), then push the
/// current full state of every local document (the tree, and every node id
/// the now-current local tree names) the remote doesn't have yet — the
/// seeding step `cloudHostStore` depends on to upload a store's pre-existing
/// content.
async fn full_reconcile(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, key_id: Uuid, link_id: &str) -> anyhow::Result<()> {
    let remote_docs = client.vault_list_docs(store_id).await.map_err(|e| anyhow::anyhow!("remote vaultListDocs failed: {}", e))?;
    let remote_heads: HashMap<VaultDocId, u64> = remote_docs.into_iter().map(|d| (d.doc_id, d.head)).collect();

    if remote_heads.contains_key(&VaultDocId::Tree) {
        pull_doc(handler, client, store_id, &VaultDocId::Tree, link_id).await?;
    }

    let node_ids: Vec<NodeId> = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        manager.store_document(store_id).map(|doc| doc.list_node_ids().unwrap_or_default()).unwrap_or_default()
    };
    for node_id in &node_ids {
        let doc_id = VaultDocId::Node(*node_id);
        if remote_heads.contains_key(&doc_id) {
            pull_doc(handler, client, store_id, &doc_id, link_id).await?;
        }
    }

    // Seed anything the remote has never seen. No live subscription is
    // active yet during a full reconcile, so a disposable echo tracker is
    // fine here (nothing to echo against until `connect_and_sync` subscribes).
    if remote_heads.get(&VaultDocId::Tree).copied().unwrap_or(0) == 0 {
        push_full(handler, client, store_id, VaultDocId::Tree, key_id, link_id, &Arc::new(EchoTracker::new())).await?;
    }
    for node_id in &node_ids {
        let doc_id = VaultDocId::Node(*node_id);
        if remote_heads.get(&doc_id).copied().unwrap_or(0) == 0 {
            push_full(handler, client, store_id, doc_id, key_id, link_id, &Arc::new(EchoTracker::new())).await?;
        }
    }

    Ok(())
}

/// Fetch and apply everything `doc_id` has beyond its remembered `last_seq`.
async fn pull_doc(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, doc_id: &VaultDocId, link_id: &str) -> anyhow::Result<()> {
    let after_seq = read_last_seq(handler, store_id, doc_id).await;
    let fetch = client
        .vault_fetch(store_id, doc_id.clone(), after_seq)
        .await
        .map_err(|e| anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e))?;

    if let Some(entry) = &fetch.snapshot {
        apply_blob_locally(handler, store_id, doc_id, &entry.blob, link_id).await?;
        record_last_seq(handler, store_id, doc_id, entry.seq).await;
    }
    for entry in &fetch.updates {
        apply_blob_locally(handler, store_id, doc_id, &entry.blob, link_id).await?;
        record_last_seq(handler, store_id, doc_id, entry.seq).await;
    }
    Ok(())
}

/// Decrypt one base64url blob and apply it locally through the handler's
/// own `applyStoreUpdate`/`applyEdit`. An unknown key id or a decryption
/// failure is logged and skipped, never fatal to the link
/// (docs/CRYPTO_CONTRACT.md: "a blob with an unknown key id is logged and
/// skipped").
async fn apply_blob_locally(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId, blob_b64url: &str, link_id: &str) -> anyhow::Result<()> {
    let blob = URL_SAFE_NO_PAD
        .decode(blob_b64url)
        .map_err(|e| anyhow::anyhow!("blob for {:?} is not valid base64url: {}", doc_id, e))?;

    let key_id = match Blob::key_id(&blob) {
        Ok(id) => id,
        Err(e) => {
            warn!("Vault link for store {} doc {:?}: malformed blob header ({}); skipping it", store_id, doc_id, e);
            return Ok(());
        }
    };
    let Some(key) = handler.keystore().store_key(store_id, key_id).await else {
        warn!("Vault link for store {} doc {:?}: no key held for key id {}; skipping this blob", store_id, doc_id, key_id);
        return Ok(());
    };

    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &doc_id.as_str());
    let plaintext = match Blob::decrypt(&key, &aad, &blob) {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!("Vault link for store {} doc {:?}: decryption failed ({}); skipping this blob", store_id, doc_id, e);
            return Ok(());
        }
    };

    let ext = crate::principal::service_extensions();
    match doc_id {
        VaultDocId::Tree => {
            let update = STANDARD.encode(&plaintext);
            handler
                .apply_store_update(&ext, ApplyStoreUpdateRequest { store_id, client_id: link_id.to_string(), update })
                .await
                .map_err(|e| anyhow::anyhow!("local applyStoreUpdate failed: {}", e))?;
            // A replica created empty for this twin still carries its
            // placeholder manifest root until this point.
            handler.adopt_document_root(store_id).await;
        }
        VaultDocId::Node(node_id) => {
            let changes = STANDARD.encode(&plaintext);
            let op = EditOperation::IncrementalChanges { changes };
            handler
                .apply_edit(&ext, ApplyEditRequest { store_id, node_id: *node_id, client_id: link_id.to_string(), operation: op })
                .await
                .map_err(|e| anyhow::anyhow!("local applyEdit failed: {}", e))?;
        }
    }
    Ok(())
}

// ── Remote -> local (live) ───────────────────────────────────────────────

async fn handle_remote_notification(
    handler: &RpcHandler,
    store_id: StoreId,
    link_id: &str,
    notif: StoreChangedNotification,
    echoes: &Arc<EchoTracker>,
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
        return Ok(());
    }
    if echoes.take_if_present(doc_id, *seq) {
        debug!("Vault link for store {} doc {:?}: dropping our own echoed append by seq (seq {})", store_id, doc_id, seq);
        return Ok(());
    }

    let Some(blob) = &notif.update else {
        warn!("Vault link for store {} doc {:?}: VaultAppended with no blob; skipping", store_id, doc_id);
        return Ok(());
    };
    apply_blob_locally(handler, store_id, doc_id, blob, link_id).await?;
    record_last_seq(handler, store_id, doc_id, *seq).await;
    Ok(())
}

// ── Local -> remote (live) ────────────────────────────────────────────────

async fn forward_local_change(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    key_id: Uuid,
    link_id: &str,
    change: LocalChange,
    tree_debouncer: &Arc<TreePushDebouncer>,
    push_tx: &mpsc::UnboundedSender<()>,
    echoes: &Arc<EchoTracker>,
) -> anyhow::Result<()> {
    let LocalChange::Store(notif) = change else {
        return Ok(());
    };
    if notif.store_id != store_id {
        return Ok(());
    }
    // Our own applies (from `apply_blob_locally` above) carry this link's
    // own id; forwarding those back would just re-encrypt what we just
    // decrypted from the very same remote.
    if notif.source_client_id.as_deref() == Some(link_id) {
        return Ok(());
    }

    match (&notif.change_kind, &notif.update) {
        (StoreChangeKind::ContentUpdated { node_id }, Some(changes_b64)) => {
            let plaintext = STANDARD.decode(changes_b64)?;
            push_update(handler, client, store_id, VaultDocId::Node(*node_id), key_id, link_id, &plaintext, echoes).await?;
        }
        (StoreChangeKind::ContentUpdated { node_id }, None) => {
            // A full-snapshot content replacement (`updateNodeContent`): no
            // delta to reuse, push the node's current full state instead.
            push_full(handler, client, store_id, VaultDocId::Node(*node_id), key_id, link_id, echoes).await?;
        }
        (StoreChangeKind::TreeStructure { .. }, Some(update_b64)) => {
            let plaintext = STANDARD.decode(update_b64)?;
            push_update(handler, client, store_id, VaultDocId::Tree, key_id, link_id, &plaintext, echoes).await?;
        }
        (StoreChangeKind::NodeCreated { .. }, _)
        | (StoreChangeKind::NodeDeleted { .. }, _)
        | (StoreChangeKind::NodeMoved { .. }, _)
        | (StoreChangeKind::MetadataUpdated { .. }, _)
        | (StoreChangeKind::TreeStructure { .. }, None) => {
            schedule_tree_push(tree_debouncer, push_tx);
        }
        (StoreChangeKind::VaultAppended { .. }, _)
        | (StoreChangeKind::SyncStateChanged { .. }, _)
        | (StoreChangeKind::MountStateChanged { .. }, _) => {}
    }
    Ok(())
}

/// Encrypt `plaintext` and append it to `doc_id`'s vault log, recording the
/// echo and snapshotting if the returned sequence lands on a multiple of
/// [`SNAPSHOT_EVERY`].
async fn push_update(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    doc_id: VaultDocId,
    key_id: Uuid,
    link_id: &str,
    plaintext: &[u8],
    echoes: &Arc<EchoTracker>,
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
    record_last_seq(handler, store_id, &doc_id, seq).await;
    debug!("Vault link for store {} pushed doc {:?} update (seq {})", store_id, doc_id, seq);

    if seq % SNAPSHOT_EVERY == 0 {
        upload_snapshot(handler, client, store_id, doc_id, key_id, seq).await?;
    }
    Ok(())
}

/// Push a document's current full state as a fresh append (used to seed a
/// document the remote has never seen, and as the fallback for a structural
/// change with no delta bytes to reuse).
async fn push_full(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    doc_id: VaultDocId,
    key_id: Uuid,
    link_id: &str,
    echoes: &Arc<EchoTracker>,
) -> anyhow::Result<()> {
    let plaintext = full_doc_state(handler, store_id, &doc_id).await?;
    push_update(handler, client, store_id, doc_id, key_id, link_id, &plaintext, echoes).await
}

/// The current full state of `doc_id` (`ContentDoc`/`StoreDocument::save()`:
/// a yrs update encoding the whole document from an empty state vector).
async fn full_doc_state(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId) -> anyhow::Result<Vec<u8>> {
    let manager = handler.store_manager_handle();
    match doc_id {
        VaultDocId::Tree => {
            let manager = manager.read().await;
            let doc = manager.store_document(store_id).map_err(|e| anyhow::anyhow!("store document unavailable: {}", e))?;
            Ok(doc.save())
        }
        VaultDocId::Node(node_id) => {
            let mut manager = manager.write().await;
            let doc = manager
                .get_node_document(store_id, *node_id)
                .await
                .map_err(|e| anyhow::anyhow!("node document unavailable: {}", e))?;
            Ok(doc.save())
        }
    }
}

async fn upload_snapshot(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, doc_id: VaultDocId, key_id: Uuid, upto_seq: u64) -> anyhow::Result<()> {
    let Some(key) = handler.keystore().store_key(store_id, key_id).await else {
        return Ok(()); // Can't snapshot without the key; the log still has everything.
    };
    let plaintext = full_doc_state(handler, store_id, &doc_id).await?;
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

// ── Debounced tree push ──────────────────────────────────────────────────

struct TreePushDebouncer {
    generation: Mutex<u64>,
}

impl TreePushDebouncer {
    fn new() -> Self {
        Self { generation: Mutex::new(0) }
    }
}

fn schedule_tree_push(debouncer: &Arc<TreePushDebouncer>, push_tx: &mpsc::UnboundedSender<()>) {
    let generation = {
        let mut g = debouncer.generation.lock().unwrap();
        *g += 1;
        *g
    };
    let debouncer = Arc::clone(debouncer);
    let push_tx = push_tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(TREE_PUSH_DEBOUNCE).await;
        let still_current = *debouncer.generation.lock().unwrap() == generation;
        if still_current {
            let _ = push_tx.send(());
        }
    });
}
