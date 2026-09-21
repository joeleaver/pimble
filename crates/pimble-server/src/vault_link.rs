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
//! **Keys** (docs/NODE_DOCUMENT_CONTRACT.md section 5): a document has its
//! own data key, stored on the hosted server wrapped under every scope key
//! that may read it (the store key, and each share's). A blob's key is
//! resolved by its header's key id: among the document's wraps first
//! (unwrapped with a scope key the keystore holds, kept in memory for the
//! life of the link and never written down), then among the scope keys the
//! keystore holds directly, which is how a blob from before data keys, under
//! the store key itself, still reads. A blob goes out under the document's
//! data key when it has one and under the link's scope key when it does not
//! (a document from before data keys, until its owner gives it one). A
//! document the remote has never seen gets a data key from the device that
//! creates it, wrapped under every scope key this device holds that covers
//! the node: the store key on a whole replica, the share's key on a share's
//! recipient (see [`Keyring`] and `seal_plan`). Those wraps ride the
//! document's first append and the remote stores the two together, so no
//! blob is ever on the remote under a key it has no wrap of.
//!
//! **A share's recipient** links a partial replica: the hosted server lists
//! and sends it exactly its scope, so the pull needs to know nothing about
//! scopes; a document entering the scope later is fetched whole the first
//! time anything is heard of it, and asked for again while a held list
//! names a child that is not here (`pull_pending`). Held as a reader
//! (`sync.json`'s `access: read`), the link pulls and never pushes. A push
//! the server refuses for one document (it left the scope, or it is under
//! a root this account only reads) leaves that document marked and the link
//! up. How the account holds the store is read again from the accounts
//! service at every connect (`refresh_grant`), and asked about every two
//! minutes while connected (`grant_shape`): a role the owner changed,
//! another share of the store or a removal ends the connection, and the
//! next one is made with a token minted from the grant as it is now.
//!
//! **Share upkeep** (docs/NODE_DOCUMENT_CONTRACT.md section 5, the owner's
//! half; `crate::share`): on an owner's device the link's task also runs
//! the hosted-server half of keeping the store's shares up, the scope sets
//! and the wraps of data keys, at every connect and a moment after the
//! tree changes. It rides this connection, this keyring and this task on
//! purpose: giving a document from before data keys its data key has to be
//! serialized with this device's own appends to that document, and the
//! keyring already knows every document's wraps from the pull
//! ([`LinkSession`] is the narrow view of the link it works through). The
//! accounts-service half, the key sweep, is a task of its own beside the
//! link's (`crate::share::run_sweeper`), so a slow accounts service never
//! holds up an edit on its way out. None of it is in the path of anyone
//! else's edit.
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
use pimble_core::{AuthMethod, NodeId, StoreAccess, StoreId, SyncState};
use pimble_crypto::{Blob, SymmetricKey};
use pimble_rpc::{StoreChangeKind, StoreChangedNotification, VaultCursor, VaultDocId, VaultDocKeys};
use tokio::sync::{broadcast, mpsc, watch, Notify};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use url::Url;
use uuid::Uuid;

use crate::handler::{LocalChange, Repair, RpcHandler};
use crate::share::{ShareCommand, Upkeep};

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
/// How long a share's recipient waits before asking the remote again for a
/// document a held list names and the scope did not hold yet, doubling up
/// to [`MAX_AWAITED_POLL`] while nothing new arrives.
/// How often a connected link asks the accounts service whether the
/// account's grant on the store is still what this connection was made
/// with (see [`grant_shape`]).
const GRANT_CHECK_EVERY: Duration = Duration::from_secs(120);
const AWAITED_POLL: Duration = Duration::from_secs(5);
const MAX_AWAITED_POLL: Duration = Duration::from_secs(60);

/// A running vault link for one store, mirroring [`crate::sync_link::SyncLinkHandle`]'s API.
pub struct VaultLinkHandle {
    state_rx: watch::Receiver<SyncState>,
    last_sync: Arc<Mutex<Option<DateTime<Utc>>>>,
    join: JoinHandle<()>,
    /// The store's key sweep (`crate::share::run_sweeper`), which lives and
    /// dies with the link.
    sweep: JoinHandle<()>,
    sweep_kick: Arc<Notify>,
    /// What a `cloudShare*` RPC asks of the share upkeep in the link's task.
    share_tx: mpsc::UnboundedSender<ShareCommand>,
}

impl VaultLinkHandle {
    pub fn state(&self) -> SyncState {
        self.state_rx.borrow().clone()
    }

    pub fn last_sync(&self) -> Option<DateTime<Utc>> {
        *self.last_sync.lock().unwrap()
    }

    /// Stop the link's background tasks. Safe to call more than once.
    pub fn stop(&self) {
        self.join.abort();
        self.sweep.abort();
    }

    /// Hand the link's share upkeep a command. `false` when the link's task
    /// has ended.
    pub(crate) fn share_command(&self, command: ShareCommand) -> bool {
        self.share_tx.send(command).is_ok()
    }

    /// Run the key sweep now rather than at its next tick.
    pub(crate) fn kick_sweep(&self) {
        self.sweep_kick.notify_one();
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
        let sweep_kick = Arc::new(Notify::new());
        let (share_tx, share_rx) = mpsc::unbounded_channel();
        let sweep = tokio::spawn(crate::share::run_sweeper(handler.clone(), store_id, Arc::clone(&sweep_kick)));
        let shares = ShareSide { commands: share_rx, upkeep: Upkeep::new(), sweep_kick: Arc::clone(&sweep_kick) };
        let join = tokio::spawn(run_loop(handler, rpc_url, key_id, link_id, state, shares));

        VaultLinkHandle { state_rx, last_sync, join, sweep, sweep_kick, share_tx }
    }
}

/// What the link's task holds for the store's shares (see the module doc,
/// "Share upkeep"): the upkeep's state, which outlives a connection, the
/// commands the `cloudShare*` RPCs send it, and the key sweep's bell.
struct ShareSide {
    commands: mpsc::UnboundedReceiver<ShareCommand>,
    upkeep: Upkeep,
    sweep_kick: Arc<Notify>,
}

impl ShareSide {
    /// The next command. A closed channel (the handle is gone, and this
    /// task about to be) never resolves, so the select it sits in does not spin.
    async fn next_command(&mut self) -> ShareCommand {
        match self.commands.recv().await {
            Some(command) => command,
            None => std::future::pending().await,
        }
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

async fn run_loop(handler: RpcHandler, rpc_url: Url, key_id: Uuid, link_id: String, link: LinkState, mut shares: ShareSide) {
    let store_id = link.store_id;
    // Subscribed once, for the life of the task: a change made while the link
    // is down still arrives here, which is how the link knows what to push
    // once it is back (see `Progress`).
    let mut local_rx = handler.subscribe_local_changes().await;
    let mut progress = Progress::load(&handler, store_id).await;
    let mut backoff = INITIAL_BACKOFF;
    loop {
        let mut reached_synced = false;
        let result = connect_and_sync(&handler, &rpc_url, key_id, &link_id, &link, &mut local_rx, &mut progress, &mut reached_synced, &mut shares).await;
        if reached_synced {
            backoff = INITIAL_BACKOFF;
        }
        // `Ok` is a connection given up on purpose (the grant changed):
        // the next one is made at once.
        if let Err(e) = result {
            warn!("Vault link for store {} to {} dropped: {}", store_id, rpc_url, e);
            set_state(&handler, &link, SyncState::Offline).await;
            shares.upkeep.link_down(&handler, store_id).await;
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
    shares: &mut ShareSide,
) -> anyhow::Result<()> {
    let store_id = link.store_id;
    set_state(handler, link, SyncState::Syncing).await;

    let owner = refresh_grant(handler, store_id).await;
    let connected_as = grant_shape(handler, store_id).await;
    let client = connect(handler, rpc_url).await?;
    // What the last connection learned of the remote's documents and their
    // wraps is asked again; data keys already unwrapped stay.
    progress.keyring.forget_remote();

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
    let mut grant_tick = tokio::time::interval_at(tokio::time::Instant::now() + GRANT_CHECK_EVERY, GRANT_CHECK_EVERY);
    let mut awaited_wait = AWAITED_POLL;
    let awaited_poll = tokio::time::sleep(awaited_wait);
    tokio::pin!(awaited_poll);

    set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
    *reached_synced = true;
    info!("Vault link for store {} connected to {}", store_id, rpc_url);

    // The shares' upkeep at connect: a pass as soon as the loop below turns
    // (reconciled first, so the pass judges the converged tree), and the
    // key sweep beside it.
    shares.upkeep.connected(handler, store_id, owner);
    shares.sweep_kick.notify_one();

    loop {
        tokio::select! {
            item = remote_sub.next() => {
                match item {
                    Some(Ok(notif)) => {
                        handle_remote_notification(handler, &client, store_id, link_id, notif, &echoes, progress).await?;
                        set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
                    }
                    Some(Err(e)) => return Err(anyhow::anyhow!("remote notification decode error: {}", e)),
                    None => return Err(anyhow::anyhow!("remote subscription closed")),
                }
            }
            change = local_rx.recv() => {
                match change {
                    Ok(local_change) => {
                        // Whoever made it, this link's own applies included:
                        // a member's create and another device's move change
                        // what is under a share as much as an edit made here.
                        shares.upkeep.note(store_id, &local_change);
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
            // A role the owner changed, another share of the store, a
            // removal: what this connection may do was settled by the token
            // it was made with, so the answer is to connect again, which
            // reads the grant and mints a token from it.
            _ = grant_tick.tick() => {
                let now = grant_shape(handler, store_id).await;
                if now.is_some() && now != connected_as {
                    info!("Vault link for store {}: the account's grant on it has changed; connecting again", store_id);
                    progress.save_if_unsaved().await;
                    return Ok(());
                }
            }
            // Upkeep that fails is tried again, and never takes the link
            // down with it: the link is the path of this device's own
            // edits, and a connection that really is gone shows in the
            // branches above.
            _ = crate::share::until(shares.upkeep.due()) => {
                let mut session = LinkSession { handler, client: &client, store_id, link_key_id: key_id, progress: &mut *progress, calls: 0 };
                if let Err(e) = shares.upkeep.run(&mut session, &shares.sweep_kick).await {
                    warn!("Vault link for store {}: share upkeep failed ({}); trying again shortly", store_id, e);
                    shares.upkeep.retry_later();
                }
            }
            command = shares.next_command() => {
                let mut session = LinkSession { handler, client: &client, store_id, link_key_id: key_id, progress: &mut *progress, calls: 0 };
                if let Err(e) = shares.upkeep.command(&mut session, command, &shares.sweep_kick).await {
                    warn!("Vault link for store {}: share upkeep failed ({}); trying again shortly", store_id, e);
                    shares.upkeep.retry_later();
                }
            }
            _ = &mut awaited_poll => {
                // Quiet again the moment nothing is awaited; slower while
                // what is awaited stays away.
                let pulled = pull_pending(handler, &client, store_id, link_id, progress).await?;
                awaited_wait = if pulled { AWAITED_POLL } else { (awaited_wait * 2).min(MAX_AWAITED_POLL) };
                awaited_poll.as_mut().reset(tokio::time::Instant::now() + awaited_wait);
            }
        }
    }
}

/// How the account holds the store, as far as the accounts service's rows
/// say: what a token minted now would carry. Compared between a connect and
/// now ([`GRANT_CHECK_EVERY`]); never stored.
#[derive(Debug, PartialEq, Eq)]
struct GrantShape {
    /// `None`: no row names the store.
    held: Option<(pimble_core::StoreAccess, std::collections::BTreeSet<String>, std::collections::BTreeSet<String>)>,
}

/// `None` when it cannot be read (no account, no network): nothing is
/// concluded from that, and the connection stands.
async fn grant_shape(handler: &RpcHandler, store_id: StoreId) -> Option<GrantShape> {
    let account = handler.keystore().account().await?;
    let rows = crate::cloud::list_stores(&account.url, &account.session).await.ok()?;
    let ids = |roots: &[NodeId]| roots.iter().map(|id| id.to_string()).collect::<std::collections::BTreeSet<_>>();
    Some(GrantShape {
        held: crate::cloud::HeldAs::from_rows(&rows, store_id).map(|held_as| (held_as.access, ids(&held_as.roots), ids(&held_as.read_only_roots))),
    })
}

/// Read again, from the accounts service, how the signed-in account holds
/// this store, and bring `sync.json` (`access`, `shared_by`, the roots only
/// read), a partial replica's scope roots and the keystore in line with it:
/// a role changed by the owner, another share of the same store, a share's
/// key handed over since the replica was added. Best effort: the token the
/// connect mints next is what the hosted server judges, and a link with no
/// account or no network fails there, with its backoff.
///
/// Answers whether the account is an owner of the store, as far as the
/// rows say (`None`: they could not be read), which is what decides whether
/// this device keeps the store's shares up (`crate::share`).
async fn refresh_grant(handler: &RpcHandler, store_id: StoreId) -> Option<bool> {
    let account = handler.keystore().account().await?;
    let rows = match crate::cloud::list_stores(&account.url, &account.session).await {
        Ok(rows) => rows,
        Err(e) => {
            debug!("Vault link for store {}: could not list the account's stores: {}", store_id, e);
            return None;
        }
    };
    let owner = crate::cloud::is_owner_of(&rows, store_id);
    let manager = handler.store_manager_handle();
    let local_roots = manager.read().await.scope_roots(store_id);
    let Some(mut held_as) = crate::cloud::HeldAs::from_rows(&rows, store_id) else {
        // No row at all. For a share's replica that is a removal from every
        // share of the store: the hosted server takes nothing from this
        // account any more, so nothing is taken here either (an edit
        // accepted here would sit on this device for good, looking saved).
        if !local_roots.is_empty() {
            let manager = manager.read().await;
            if let Ok(Some(mut config)) = manager.read_sync_config(store_id).await {
                if config.access != StoreAccess::Read {
                    info!("Vault link for store {}: the account holds no share of it any more; read only here now", store_id);
                    config.access = StoreAccess::Read;
                    if let Err(e) = manager.write_sync_config(store_id, &config).await {
                        warn!("Vault link for store {}: could not record how the store is held: {}", store_id, e);
                    }
                }
            }
        }
        return Some(owner);
    };
    // A root this replica holds and the account does not any more (removed
    // from that share, or its owner stopped sharing it) stays here as it
    // last was and takes no edits, for the same reason.
    for root in local_roots.iter().filter(|root| !held_as.roots.contains(root)) {
        if !held_as.read_only_roots.contains(root) {
            held_as.read_only_roots.push(*root);
        }
    }
    // Only a partial replica takes roots: a whole replica holds every
    // document already, whatever the account's grant has become since.
    if !local_roots.is_empty() {
        let mut added = false;
        for root in held_as.roots.iter().filter(|root| !local_roots.contains(root)) {
            match manager.write().await.add_scope_root(store_id, *root).await {
                Ok(()) => added = true,
                Err(e) => warn!("Vault link for store {}: could not add scope root {}: {}", store_id, root, e),
            }
        }
        // One share's replica carries that share's name; with another it is
        // "Shared by ...", as a replica added with both would be.
        if let (true, Some(name)) = (added, &held_as.name) {
            if let Err(e) = manager.write().await.set_partial_replica_name(store_id, name).await {
                warn!("Vault link for store {}: could not rename the replica: {}", store_id, e);
            }
        }
    }
    {
        let manager = manager.read().await;
        if let Ok(Some(mut config)) = manager.read_sync_config(store_id).await {
            let read_only_roots = if local_roots.is_empty() { Vec::new() } else { held_as.read_only_roots.clone() };
            if config.access != held_as.access || config.shared_by != held_as.shared_by || config.read_only_roots != read_only_roots {
                info!("Vault link for store {}: held as {:?} now (shared by {:?})", store_id, held_as.access, held_as.shared_by);
                config.access = held_as.access;
                config.shared_by = held_as.shared_by.clone();
                config.read_only_roots = read_only_roots;
                if let Err(e) = manager.write_sync_config(store_id, &config).await {
                    warn!("Vault link for store {}: could not record how the store is held: {}", store_id, e);
                }
            }
        }
    }

    // The scope keys: a share's recipient's shares', or the store key and,
    // on a device that holds the whole store, the key of every share in it
    // (the nodes that carry a share's marker): the device that made a share
    // uploads an envelope of its key for the owner's own account too, which
    // is how their other devices come to read what a recipient creates.
    let (roots, shared_nodes) = {
        let manager = manager.read().await;
        let roots = manager.scope_roots(store_id);
        let shared_nodes: Vec<NodeId> = match manager.tree(store_id) {
            Ok(tree) if roots.is_empty() => tree
                .list_node_ids()
                .into_iter()
                .filter(|id| tree.get_node_info(*id).is_ok_and(|info| info.custom.contains_key(pimble_core::custom_keys::SHARE)))
                .collect(),
            _ => Vec::new(),
        };
        (roots, shared_nodes)
    };
    // `roots` empty is the store key's scope (`fetch_scope_keys`).
    let mut scopes = vec![roots];
    if !shared_nodes.is_empty() {
        scopes.push(shared_nodes);
    }
    for scope in scopes {
        if let Err(e) = fetch_scope_keys(handler, &account, store_id, &scope).await {
            debug!("Vault link for store {}: could not fetch scope keys: {}", store_id, e);
        }
    }
    Some(owner)
}

/// Fetch and unwrap the scope keys the signed-in account has been handed
/// for `store_id` (the store key when `roots` is empty, else each share's)
/// into the keystore. An envelope is believed when the account itself or
/// one of the store's owners, as the accounts service lists them, signed
/// it; one that does not verify is skipped. Answers the key a link of this
/// replica encrypts under when a document has no data key: the store key
/// last listed, or the first share's first key; `None` when no key has
/// reached the account (a share whose owner's devices have not been online
/// since the grant).
pub(crate) async fn fetch_scope_keys(
    handler: &RpcHandler,
    account: &crate::keystore::SignedInAccount,
    store_id: StoreId,
    roots: &[NodeId],
) -> anyhow::Result<Option<Uuid>> {
    let own_signer = account.keys.public_keys().signing;
    let scopes: Vec<Option<&NodeId>> = if roots.is_empty() { vec![None] } else { roots.iter().map(Some).collect() };
    let mut link_key: Option<Uuid> = None;
    for scope in scopes {
        let grants = crate::cloud::get_store_keys(&account.url, &account.session, &store_id.to_string(), scope).await?;
        for grant in &grants.envelopes {
            let Ok(key_id) = grant.key_id.parse::<Uuid>() else {
                warn!("Store {}: the cloud service returned a bad key id {:?}; skipping it", store_id, grant.key_id);
                continue;
            };
            let signer = grants.expected_signer(&grant.envelope, &own_signer);
            match pimble_crypto::unwrap_key(&grant.envelope, &account.keys, signer) {
                Ok(key) => {
                    handler.keystore().add_store_key(store_id, key_id, &key).await?;
                    // The store key: the last listed, as always. A share's:
                    // the first root's, which is the replica's first root.
                    if roots.is_empty() || link_key.is_none() {
                        link_key = Some(key_id);
                    }
                }
                Err(e) => warn!("Store {}: a key envelope for key {} did not verify ({}); skipping it", store_id, key_id, e),
            }
        }
    }
    Ok(link_key)
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
    /// Documents from before data keys that this device gave one (they came
    /// under a share) and whose snapshot under it is not confirmed yet. The
    /// earlier blobs are under the store key, which no recipient holds;
    /// only a snapshot under the data key, replacing them, makes the
    /// document readable to the share. Written before the keys are set, so
    /// a crash in between is finished by the next pass
    /// ([`LinkSession::finish_rekey`]).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    rekeying: BTreeSet<String>,
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
    /// The documents' data keys, in memory only.
    keyring: Keyring,
}

/// What the link knows of the remote's documents and their data keys
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Keys"). In memory only: a
/// data key is unwrapped on demand from the document's wraps with a scope
/// key the keystore holds, and is gone with the process.
#[derive(Default)]
struct Keyring {
    /// Each document's wraps, as `vaultFetch` last gave them.
    wraps: HashMap<String, VaultDocKeys>,
    /// The data key id `vaultListDocs` names for a document whose wraps
    /// have not been fetched yet.
    listed_dek: HashMap<String, Uuid>,
    /// Data keys unwrapped (or made) so far: document -> (key id, key).
    deks: HashMap<String, (Uuid, SymmetricKey)>,
    /// The documents the remote is known to hold: listed, pulled, or
    /// appended to by anyone. One that is not here is this device's to
    /// create, data key and all.
    remote_docs: HashSet<String>,
    /// The documents read from their cursor in this connection. Anything
    /// heard of another (it entered the scope since) is fetched whole.
    pulled: HashSet<String>,
    /// Blob key ids a refetch of the document's wraps did not resolve, so a
    /// document this device cannot read is asked about once a connection,
    /// not once an append.
    unresolved: HashSet<(String, Uuid)>,
    /// Documents whose last pull left a blob unread for want of its key,
    /// with the retries they have left in this connection. A share's
    /// recipient that creates a document can only set its keys after its
    /// first append, so a peer that fetches in between finds a blob and no
    /// wraps; asking again a moment later finds both. A document whose key
    /// this device will never hold runs out of retries and waits for the
    /// next connect.
    unread: HashMap<String, u32>,
}

/// How often a document left unread for want of a key is asked for again
/// within one connection.
const UNREAD_RETRIES: u32 = 5;

impl Keyring {
    fn forget_remote(&mut self) {
        self.wraps.clear();
        self.listed_dek.clear();
        self.remote_docs.clear();
        self.pulled.clear();
        self.unresolved.clear();
        self.unread.clear();
    }

    fn note_wraps(&mut self, doc_id: &VaultDocId, keys: Option<VaultDocKeys>) {
        let doc = doc_id.as_str();
        match keys {
            Some(keys) => {
                // A rotation: the cached key is the old one's.
                if self.deks.get(&doc).is_some_and(|(id, _)| *id != keys.dek_id) {
                    self.deks.remove(&doc);
                }
                self.listed_dek.insert(doc.clone(), keys.dek_id);
                self.wraps.insert(doc, keys);
            }
            None => {
                self.wraps.remove(&doc);
                self.listed_dek.remove(&doc);
            }
        }
    }

    /// The id of the document's data key, as far as the remote has said.
    fn dek_id(&self, doc: &str) -> Option<Uuid> {
        self.wraps.get(doc).map(|keys| keys.dek_id).or_else(|| self.listed_dek.get(doc).copied())
    }
}

/// The key a blob of `doc_id` with `key_id` in its header reads with: the
/// document's data key when the id names it (cached, or unwrapped now with
/// whichever scope key the keystore holds among the wraps), else a scope
/// key held directly (a blob from before data keys).
async fn blob_key(handler: &RpcHandler, keyring: &mut Keyring, store_id: StoreId, doc_id: &VaultDocId, key_id: Uuid) -> Option<SymmetricKey> {
    let doc = doc_id.as_str();
    if let Some((id, key)) = keyring.deks.get(&doc) {
        if *id == key_id {
            return Some(key.clone());
        }
    }
    if let Some(keys) = keyring.wraps.get(&doc).filter(|keys| keys.dek_id == key_id) {
        let aad = pimble_crypto::dek_aad(&store_id.to_string(), &doc);
        for wrap in &keys.wraps {
            let Some(scope_key) = handler.keystore().store_key(store_id, wrap.scope_key_id).await else { continue };
            match pimble_crypto::unwrap_dek(wrap, &scope_key, &aad) {
                Ok(dek) => {
                    keyring.deks.insert(doc, (key_id, dek.clone()));
                    return Some(dek);
                }
                Err(e) => warn!("Vault link for store {} doc {:?}: the wrap under scope key {} did not open ({})", store_id, doc_id, wrap.scope_key_id, e),
            }
        }
    }
    handler.keystore().store_key(store_id, key_id).await
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
        Self { path, file, unsaved: false, cursors: HashMap::new(), snapshot_seqs: HashMap::new(), keyring: Keyring::default() }
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
        progress.keyring.remote_docs.insert(doc.doc_id.as_str());
        if let Some(dek_id) = doc.dek_id {
            progress.keyring.listed_dek.insert(doc.doc_id.as_str(), dek_id);
        }
        remote_heads.insert(doc.doc_id, doc.head);
    }

    for doc_id in remote_heads.keys() {
        pull_doc(handler, client, store_id, doc_id, link_id, progress).await?;
    }
    // Every document is in before the tree is judged (see `Repair::Later`).
    handler.repair_store_tree(store_id).await;

    // Held as a reader: nothing is pushed, and nothing marked is forgotten
    // (a repair this replica made for itself goes out if the role changes).
    let (doc_ids, access, partial): (Vec<VaultDocId>, StoreAccess, bool) = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        (
            manager.doc_ids(store_id).map(|ids| ids.into_iter().map(VaultDocId::Node).collect()).unwrap_or_default(),
            manager.store_access(store_id),
            !manager.scope_roots(store_id).is_empty(),
        )
    };
    if !access.allows_write() {
        if progress.file.cursor_version != CURSOR_VERSION {
            progress.file.cursor_version = CURSOR_VERSION;
            progress.unsaved = true;
        }
        progress.save_if_unsaved().await;
        return Ok(());
    }
    let mut refused = false;
    for doc_id in doc_ids {
        let unseen = remote_heads.get(&doc_id).copied().unwrap_or(0) == 0;
        // To a share's recipient "not listed" is also what a document its
        // owner moved out of the share looks like: held here, no longer
        // this account's. Only one that changed here is offered (a create
        // made while the link was down); the remote refuses the other kind,
        // and it stays marked.
        if partial && unseen && !progress.is_dirty(&doc_id) {
            continue;
        }
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
        // A document's first blob is its whole state (`save`: what a diff
        // from an empty vector leaves out, pending updates, rides along).
        match append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, &diff, echoes, progress).await? {
            Appended::Yes => {
                progress.set_known(&doc_id, &local_sv);
                progress.clear_dirty(&doc_id);
            }
            Appended::Refused => {
                progress.mark_dirty(&doc_id);
                refused = true;
            }
        }
    }
    if refused {
        // `dirty_all` stood for every document; the refused ones are
        // marked one by one now, so it can go and they stay.
        if progress.file.dirty_all {
            progress.file.dirty_all = false;
            progress.unsaved = true;
        }
    } else {
        progress.clear_all_dirty();
    }
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
    let fetch = match client.vault_fetch(store_id, doc_id.clone(), after_seq).await {
        Ok(fetch) => fetch,
        // Listed a moment ago and out of the scope now (its owner moved it
        // out of the share): not this replica's to read any more.
        Err(e) if is_refusal(&e) => {
            debug!("Vault link for store {} doc {:?}: fetch refused ({}); leaving it", store_id, doc_id, e);
            return Ok(());
        }
        Err(e) => return Err(anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e)),
    };
    progress.keyring.remote_docs.insert(doc_id.as_str());
    progress.keyring.pulled.insert(doc_id.as_str());
    progress.keyring.note_wraps(doc_id, fetch.keys.clone());

    let mut wants_a_key = false;
    if let Some(entry) = &fetch.snapshot {
        progress.note_snapshot(doc_id, entry.seq);
        match apply_blob_locally(handler, &mut progress.keyring, store_id, doc_id, &entry.blob, link_id, Repair::Later).await? {
            Applying::Applied(update) => {
                progress.advance(doc_id, &update);
                progress.cursor(doc_id).mark_through(entry.seq);
            }
            Applying::UnknownKey(_) => wants_a_key = true,
            Applying::Skipped => {}
        }
    }
    for entry in &fetch.updates {
        match apply_blob_locally(handler, &mut progress.keyring, store_id, doc_id, &entry.blob, link_id, Repair::Later).await? {
            Applying::Applied(update) => {
                progress.advance(doc_id, &update);
                progress.cursor(doc_id).mark(entry.seq);
            }
            Applying::UnknownKey(_) => wants_a_key = true,
            Applying::Skipped => {}
        }
    }
    if wants_a_key {
        progress.keyring.unread.entry(doc_id.as_str()).or_insert(UNREAD_RETRIES);
    } else {
        progress.keyring.unread.remove(&doc_id.as_str());
    }
    let applied_through = progress.cursor(doc_id).applied_through();
    record_last_seq(handler, store_id, doc_id, applied_through).await;
    Ok(())
}

/// What became of one blob.
enum Applying {
    /// Decrypted and merged: the update, for the caller to record that the
    /// remote holds it.
    Applied(Vec<u8>),
    /// No key this device can find opens it (its header's key id).
    UnknownKey(Uuid),
    /// Malformed, undecryptable under the key its header names, or the
    /// retired tree document's: logged, never fatal to the link.
    Skipped,
}

/// Whether the remote refused a call for this one document (it is out of
/// the account's scope now, or under a root the account only reads) as
/// opposed to failing: the document is left for later and the link stays up.
fn is_refusal(error: &pimble_client::ClientError) -> bool {
    let message = error.to_string();
    message.contains(crate::principal::NO_GRANT_FOR_DOCUMENT) || StoreAccess::refusal_in(&message).is_some()
}

/// Decrypt one base64url blob and apply it locally through the handler's
/// own `apply_node_update_from`. An unknown key id or a decryption failure
/// is logged and skipped, never fatal to the link (docs/CRYPTO_CONTRACT.md:
/// "a blob with an unknown key id is logged and skipped"); so is a blob of
/// the retired tree document.
async fn apply_blob_locally(
    handler: &RpcHandler,
    keyring: &mut Keyring,
    store_id: StoreId,
    doc_id: &VaultDocId,
    blob_b64url: &str,
    link_id: &str,
    repair: Repair,
) -> anyhow::Result<Applying> {
    let VaultDocId::Node(node_id) = doc_id else {
        debug!("Vault link for store {}: skipping a blob of the retired tree document", store_id);
        return Ok(Applying::Skipped);
    };

    let blob = URL_SAFE_NO_PAD
        .decode(blob_b64url)
        .map_err(|e| anyhow::anyhow!("blob for {:?} is not valid base64url: {}", doc_id, e))?;

    let key_id = match Blob::key_id(&blob) {
        Ok(id) => id,
        Err(e) => {
            warn!("Vault link for store {} doc {:?}: malformed blob header ({}); skipping it", store_id, doc_id, e);
            return Ok(Applying::Skipped);
        }
    };
    let Some(key) = blob_key(handler, keyring, store_id, doc_id, key_id).await else {
        warn!("Vault link for store {} doc {:?}: no key held for key id {}; skipping this blob", store_id, doc_id, key_id);
        return Ok(Applying::UnknownKey(key_id));
    };

    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &doc_id.as_str());
    let plaintext = match Blob::decrypt(&key, &aad, &blob) {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!("Vault link for store {} doc {:?}: decryption failed ({}); skipping this blob", store_id, doc_id, e);
            return Ok(Applying::Skipped);
        }
    };

    handler
        .apply_node_update_from(store_id, *node_id, &plaintext, Some(link_id), repair)
        .await
        .map_err(|e| anyhow::anyhow!("local applyEdit for node {} failed: {}", node_id, e.message()))?;
    Ok(Applying::Applied(plaintext))
}

// ── Remote -> local (live) ───────────────────────────────────────────────

async fn handle_remote_notification(
    handler: &RpcHandler,
    client: &PimbleClient,
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
    progress.keyring.remote_docs.insert(doc_id.as_str());

    // A document not read in this connection is one that entered this
    // account's scope since (created by another member, or moved into the
    // share): what came before this append never reached this replica, so
    // it is read from its cursor, which brings its wraps too.
    if !progress.keyring.pulled.contains(&doc_id.as_str()) {
        pull_doc(handler, client, store_id, doc_id, link_id, progress).await?;
        // Debounced like any live structural update: a create is two
        // documents' appends, and the parent's list is the next one.
        handler.schedule_repair(store_id);
        return Ok(());
    }

    match apply_blob_locally(handler, &mut progress.keyring, store_id, doc_id, blob, link_id, Repair::Debounced).await? {
        Applying::Applied(update) => {
            progress.advance(doc_id, &update);
            progress.cursor(doc_id).mark(*seq);
        }
        // A data key this link has not seen: the document was given one (or
        // a new one) since its wraps were fetched. Once per key.
        Applying::UnknownKey(key_id) if progress.keyring.unresolved.insert((doc_id.as_str(), key_id)) => {
            return pull_doc(handler, client, store_id, doc_id, link_id, progress).await;
        }
        Applying::UnknownKey(_) | Applying::Skipped => {}
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
    // Held as a reader: the local server refuses a person's write, so what
    // arrives here is this replica's own upkeep (a repair). It stays here,
    // marked, and goes out if the role ever changes.
    if !handler.store_manager_handle().read().await.store_access(store_id).allows_write() {
        progress.mark_dirty(&doc_id);
        return Ok(());
    }
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

/// Whether an append went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Appended {
    Yes,
    /// The remote refused this document (see [`is_refusal`]), or this
    /// device holds no key it may write it under: left for a later connect.
    Refused,
}

/// How a blob of one document goes out.
enum Seal {
    /// Under this key: the document's data key, or, for a document without
    /// one, the link's scope key as before data keys.
    With { key_id: Uuid, key: SymmetricKey },
    /// The remote has never seen the document: this device creates it, with
    /// a data key of its own making wrapped under every scope key it holds
    /// that covers the node.
    Create { dek_id: Uuid, dek: SymmetricKey, keys: VaultDocKeys, parent_id: Option<NodeId> },
    /// The document has a data key and no wrap this device can open.
    Unreadable,
}

/// Decide how `doc_id`'s next blob is sealed (module doc, "Keys").
async fn seal_plan(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, doc_id: &VaultDocId, link_key_id: Uuid, progress: &mut Progress) -> anyhow::Result<Seal> {
    let doc = doc_id.as_str();
    let VaultDocId::Node(node_id) = doc_id else {
        return Err(anyhow::anyhow!("the tree document is retired; nothing is written to it"));
    };

    if progress.keyring.remote_docs.contains(&doc) {
        // Wraps are fetched with the document; one only listed so far, or
        // heard of through another device's append, is asked for now.
        if progress.keyring.dek_id(&doc).is_some() && !progress.keyring.wraps.contains_key(&doc) {
            match client.vault_fetch(store_id, doc_id.clone(), u64::MAX).await {
                Ok(fetch) => progress.keyring.note_wraps(doc_id, fetch.keys),
                Err(e) if is_refusal(&e) => return Ok(Seal::Unreadable),
                Err(e) => return Err(anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e)),
            }
        }
        return Ok(match progress.keyring.dek_id(&doc) {
            Some(dek_id) => match blob_key(handler, &mut progress.keyring, store_id, doc_id, dek_id).await {
                Some(key) => Seal::With { key_id: dek_id, key },
                None => Seal::Unreadable,
            },
            // From before data keys: under the scope key, as it always was,
            // until its owner gives it a data key. A recipient never makes
            // keys for a document that exists.
            None => match handler.keystore().store_key(store_id, link_key_id).await {
                Some(key) => Seal::With { key_id: link_key_id, key },
                None => return Err(anyhow::anyhow!("no local key held for store {} key id {}; cannot encrypt an outgoing update", store_id, link_key_id)),
            },
        });
    }

    // A new document. The scope keys that cover it: the store key on a
    // whole replica (this device holds it: it is the owner's, or a member's
    // of the whole store), and the key of every share the node is under
    // that this device holds, which the share's root names in its marker.
    let (partial, parent_id, marker_keys) = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        let partial = !manager.scope_roots(store_id).is_empty();
        let tree = manager.tree(store_id).map_err(|e| anyhow::anyhow!("store unavailable: {}", e))?;
        let parent_id = tree.doc(*node_id).and_then(|doc| doc.fields().ok()).and_then(|fields| fields.parent_id);
        let mut marker_keys = Vec::new();
        let mut cur = Some(*node_id);
        for _ in 0..=tree.ids().len() {
            let Some(fields) = cur.and_then(|id| tree.doc(id)).and_then(|doc| doc.fields().ok()) else { break };
            if let Some(marker) = fields.custom.get(pimble_core::custom_keys::SHARE).and_then(|v| serde_json::from_value::<pimble_core::ShareMarker>(v.clone()).ok()) {
                marker_keys.push(marker.key_id);
            }
            cur = fields.parent_id;
        }
        (partial, parent_id, marker_keys)
    };
    let mut scope_key_ids: Vec<Uuid> = if partial { Vec::new() } else { vec![link_key_id] };
    scope_key_ids.extend(marker_keys);
    if partial && scope_key_ids.is_empty() {
        // No marker on the way up (a share from before markers, or a root
        // whose document has not arrived): the key this replica was added
        // with is its share's.
        scope_key_ids.push(link_key_id);
    }

    let dek = SymmetricKey::generate();
    let dek_id = Uuid::new_v4();
    let aad = pimble_crypto::dek_aad(&store_id.to_string(), &doc);
    let mut wraps = Vec::new();
    for scope_key_id in scope_key_ids {
        if wraps.iter().any(|w: &pimble_crypto::WrappedDek| w.scope_key_id == scope_key_id) {
            continue;
        }
        if let Some(scope_key) = handler.keystore().store_key(store_id, scope_key_id).await {
            wraps.push(pimble_crypto::wrap_dek(&dek, &scope_key, scope_key_id, &aad));
        }
    }
    if wraps.is_empty() {
        return Err(anyhow::anyhow!("no scope key held for store {}; cannot encrypt a new document", store_id));
    }
    Ok(Seal::Create { dek_id, dek, keys: VaultDocKeys { dek_id, wraps }, parent_id })
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
) -> anyhow::Result<Appended> {
    let doc = doc_id.as_str();
    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &doc);
    let plan = seal_plan(handler, client, store_id, &doc_id, key_id, progress).await?;
    let (blob, created) = match &plan {
        Seal::With { key_id, key } => (Blob::encrypt(key, *key_id, &aad, plaintext), None),
        Seal::Create { dek_id, dek, keys, parent_id } => (Blob::encrypt(dek, *dek_id, &aad, plaintext), Some((keys, *parent_id))),
        Seal::Unreadable => {
            warn!("Vault link for store {} doc {:?}: no wrap of its data key opens here; leaving the update for later", store_id, doc_id);
            return Ok(Appended::Refused);
        }
    };
    let blob_b64 = URL_SAFE_NO_PAD.encode(&blob);

    // A new document's first blob carries its wrapped data key, and the
    // remote stores the two together (`VaultAppendRequest::keys`): no blob
    // is ever there under a key the remote has no wrap of, whichever way
    // this call ends. Attributed to this link's own id so the notification
    // it produces is dropped by identity if it echoes back through this
    // link's own subscription; the seen-seq set below is kept as a second
    // guard for anything that reaches the server without a `client_id`. A
    // new document names its parent, which is what admits a scoped member's
    // create (and is ignored for anyone else's).
    let (keys, parent_id) = match &created {
        Some((keys, parent_id)) => (Some((*keys).clone()), *parent_id),
        None => (None, None),
    };
    let seq = match client.vault_append_with_keys(store_id, doc_id.clone(), blob_b64, Some(link_id.to_string()), parent_id, keys).await {
        Ok(seq) => seq,
        Err(e) if is_refusal(&e) => {
            debug!("Vault link for store {} doc {:?}: append refused ({}); leaving the document for later", store_id, doc_id, e);
            return Ok(Appended::Refused);
        }
        Err(e) => return Err(anyhow::anyhow!("remote vaultAppend for {:?} failed: {}", doc_id, e)),
    };
    echoes.remember(&doc_id, seq);
    progress.keyring.remote_docs.insert(doc.clone());
    progress.keyring.pulled.insert(doc.clone());
    if let Seal::Create { dek_id, dek, keys, .. } = plan {
        progress.keyring.deks.insert(doc.clone(), (dek_id, dek));
        progress.keyring.note_wraps(&doc_id, Some(keys));
    }
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
        upload_snapshot(handler, client, store_id, doc_id.clone(), key_id, seq, progress).await?;
        progress.note_snapshot(&doc_id, seq);
    }
    Ok(Appended::Yes)
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
    // The first blob of a document the remote has never seen has to be the
    // whole document: the update in hand is the last of however many made
    // it (a create is several transactions), and the ones before it were
    // never anyone's to push.
    if !progress.keyring.remote_docs.contains(&doc_id.as_str()) {
        return push_full(handler, client, store_id, doc_id, key_id, link_id, echoes, progress).await;
    }
    match append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, plaintext, echoes, progress).await? {
        Appended::Yes => progress.advance(&doc_id, plaintext),
        Appended::Refused => progress.mark_dirty(&doc_id),
    }
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
    match append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, &state, echoes, progress).await? {
        Appended::Yes => {
            progress.set_known(&doc_id, &local_sv);
            progress.clear_dirty(&doc_id);
        }
        Appended::Refused => progress.mark_dirty(&doc_id),
    }
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

/// Answers whether the snapshot was stored.
async fn upload_snapshot(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, doc_id: VaultDocId, key_id: Uuid, upto_seq: u64, progress: &mut Progress) -> anyhow::Result<bool> {
    // Under the key the document's blobs go out under (it exists by now, so
    // never a create). Can't snapshot without it; the log still has
    // everything.
    let Seal::With { key_id, key } = seal_plan(handler, client, store_id, &doc_id, key_id, progress).await? else {
        return Ok(false);
    };
    let (_, plaintext) = full_doc_state(handler, store_id, &doc_id).await?;
    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &doc_id.as_str());
    let blob = Blob::encrypt(&key, key_id, &aad, &plaintext);
    let blob_b64 = URL_SAFE_NO_PAD.encode(&blob);
    match client.vault_snapshot(store_id, doc_id.clone(), upto_seq, blob_b64).await {
        Ok(()) => {}
        Err(e) if is_refusal(&e) => {
            debug!("Vault link for store {} doc {:?}: snapshot refused ({}); the log keeps everything", store_id, doc_id, e);
            return Ok(false);
        }
        Err(e) => return Err(anyhow::anyhow!("remote vaultSnapshot for {:?} failed: {}", doc_id, e)),
    }
    debug!("Vault link for store {} uploaded a snapshot for doc {:?} up to seq {}", store_id, doc_id, upto_seq);
    Ok(true)
}

// ── What share upkeep works through (crate::share) ───────────────────────

/// The link's connection and keyring as the owner's share upkeep sees them
/// (module doc, "Share upkeep"): what the remote holds of a document's
/// keys, a document's data key, adding wraps, and giving a document from
/// before data keys one. Built for the length of one pass, inside the
/// link's own task, so nothing here races this device's appends.
pub(crate) struct LinkSession<'a> {
    handler: &'a RpcHandler,
    client: &'a PimbleClient,
    store_id: StoreId,
    link_key_id: Uuid,
    progress: &'a mut Progress,
    /// Round trips made for keys so far: a pass that has a whole folder to
    /// wrap works in slices, so the link's own forwarding gets its turn.
    calls: usize,
}

/// What the remote holds of one document's keys.
pub(crate) enum RemoteKeys {
    /// The remote has never seen the document: the link creates it when it
    /// pushes it, wrapped under every scope key that covers it (`seal_plan`).
    Absent,
    /// From before data keys: its blobs are under the store key itself.
    Keyless,
    Keys(VaultDocKeys),
}

/// What became of giving a document a data key.
pub(crate) enum Rekeyed {
    /// It has one now, and a snapshot under it stands for everything before.
    Done,
    /// Not yet: this device has not read the document's log to its head (a
    /// snapshot may only vouch for what it has applied), or the remote
    /// refused. Asked again by a later pass.
    Waiting,
    /// Someone else gave it one in the meantime: theirs stands.
    HasKeys(VaultDocKeys),
}

impl LinkSession<'_> {
    pub(crate) fn handler(&self) -> &RpcHandler {
        self.handler
    }

    pub(crate) fn client(&self) -> &PimbleClient {
        self.client
    }

    pub(crate) fn store_id(&self) -> StoreId {
        self.store_id
    }

    /// The scope key this link seals under when a document has no data key:
    /// on a whole replica, the store key.
    pub(crate) fn link_key_id(&self) -> Uuid {
        self.link_key_id
    }

    /// How many round trips this session has made for documents' keys.
    pub(crate) fn calls(&self) -> usize {
        self.calls
    }

    /// `node`'s keys on the remote: as the pull left them in the keyring,
    /// asked for when the keyring has none (a document that had none when
    /// it was pulled may have been given them since, by the member who
    /// created it or by another of the owner's devices).
    pub(crate) async fn remote_keys(&mut self, node: NodeId) -> anyhow::Result<RemoteKeys> {
        let doc_id = VaultDocId::Node(node);
        let doc = doc_id.as_str();
        if !self.progress.keyring.remote_docs.contains(&doc) {
            return Ok(RemoteKeys::Absent);
        }
        if let Some(keys) = self.progress.keyring.wraps.get(&doc) {
            return Ok(RemoteKeys::Keys(keys.clone()));
        }
        self.calls += 1;
        match self.client.vault_fetch(self.store_id, doc_id.clone(), u64::MAX).await {
            Ok(fetch) => {
                self.progress.keyring.note_wraps(&doc_id, fetch.keys.clone());
                Ok(match fetch.keys {
                    Some(keys) => RemoteKeys::Keys(keys),
                    None if fetch.head == 0 => RemoteKeys::Absent,
                    None => RemoteKeys::Keyless,
                })
            }
            Err(e) if is_refusal(&e) => Ok(RemoteKeys::Absent),
            Err(e) => Err(anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e)),
        }
    }

    /// `node`'s data key, when some scope key this device holds opens a wrap of it.
    pub(crate) async fn data_key(&mut self, node: NodeId, dek_id: Uuid) -> Option<SymmetricKey> {
        let doc_id = VaultDocId::Node(node);
        // Only ever the data key: `blob_key`'s fall back to a scope key held
        // under that id is for reading old blobs, not for wrapping.
        if !self.progress.keyring.wraps.get(&doc_id.as_str()).is_some_and(|keys| keys.dek_id == dek_id) {
            return None;
        }
        blob_key(self.handler, &mut self.progress.keyring, self.store_id, &doc_id, dek_id).await
    }

    /// Add wraps of `node`'s data key (the remote merges them by scope
    /// key). `false` when the remote refused them.
    pub(crate) async fn add_wraps(&mut self, node: NodeId, keys: VaultDocKeys) -> anyhow::Result<bool> {
        let doc_id = VaultDocId::Node(node);
        self.calls += 1;
        match self.client.vault_set_doc_keys(self.store_id, doc_id.clone(), keys.clone()).await {
            Ok(()) => {}
            Err(e) if is_refusal(&e) => {
                debug!("Vault link for store {} doc {:?}: wraps refused ({})", self.store_id, doc_id, e);
                return Ok(false);
            }
            Err(e) => return Err(anyhow::anyhow!("remote vaultSetDocKeys for {:?} failed: {}", doc_id, e)),
        }
        // The keyring's copy follows the remote's merge.
        let doc = doc_id.as_str();
        let mut merged = keys;
        if let Some(held) = self.progress.keyring.wraps.get(&doc).filter(|held| held.dek_id == merged.dek_id) {
            for wrap in &held.wraps {
                if !merged.wraps.iter().any(|w| w.scope_key_id == wrap.scope_key_id) {
                    merged.wraps.push(wrap.clone());
                }
            }
        }
        self.progress.keyring.note_wraps(&doc_id, Some(merged));
        Ok(true)
    }

    /// Give `node`, a document from before data keys, a data key wrapped
    /// under `scope_key_ids`, and replace its log (blobs under the store
    /// key, which no recipient of a share holds) with a snapshot under the
    /// new key. Only when this device has applied the log to its head: the
    /// snapshot deletes what it covers. The keys go first and the snapshot
    /// second, because the other order could leave a snapshot nobody has
    /// the key of where the log used to be; in between, the document reads
    /// as it did to everyone who holds the store key.
    pub(crate) async fn give_data_key(&mut self, node: NodeId, scope_key_ids: &[Uuid]) -> anyhow::Result<Rekeyed> {
        let doc_id = VaultDocId::Node(node);
        let doc = doc_id.as_str();
        self.calls += 1;
        let fetch = match self.client.vault_fetch(self.store_id, doc_id.clone(), u64::MAX).await {
            Ok(fetch) => fetch,
            Err(e) if is_refusal(&e) => return Ok(Rekeyed::Waiting),
            Err(e) => return Err(anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e)),
        };
        if let Some(keys) = fetch.keys {
            self.progress.keyring.note_wraps(&doc_id, Some(keys.clone()));
            return Ok(Rekeyed::HasKeys(keys));
        }
        if fetch.head == 0 || self.progress.cursor(&doc_id).applied_through() != fetch.head {
            return Ok(Rekeyed::Waiting);
        }

        let dek = SymmetricKey::generate();
        let dek_id = Uuid::new_v4();
        let aad = pimble_crypto::dek_aad(&self.store_id.to_string(), &doc);
        let mut wraps: Vec<pimble_crypto::WrappedDek> = Vec::new();
        for scope_key_id in scope_key_ids {
            if wraps.iter().any(|w| w.scope_key_id == *scope_key_id) {
                continue;
            }
            if let Some(scope_key) = self.handler.keystore().store_key(self.store_id, *scope_key_id).await {
                wraps.push(pimble_crypto::wrap_dek(&dek, &scope_key, *scope_key_id, &aad));
            }
        }
        // Without the store key's wrap the owner's other devices could not
        // read what comes after.
        if !wraps.iter().any(|w| w.scope_key_id == self.link_key_id) {
            return Ok(Rekeyed::Waiting);
        }
        let keys = VaultDocKeys { dek_id, wraps };

        self.progress.file.rekeying.insert(doc.clone());
        self.progress.unsaved = true;
        self.progress.save_if_unsaved().await;
        match self.client.vault_set_doc_keys(self.store_id, doc_id.clone(), keys.clone()).await {
            Ok(()) => {}
            Err(e) if is_refusal(&e) => {
                self.progress.file.rekeying.remove(&doc);
                self.progress.unsaved = true;
                return Ok(Rekeyed::Waiting);
            }
            Err(e) => return Err(anyhow::anyhow!("remote vaultSetDocKeys for {:?} failed: {}", doc_id, e)),
        }
        // Another device of the owner's may have done the same a moment
        // ago, and the remote keeps whichever keys came last: look before
        // sealing anything under ours.
        let after = self
            .client
            .vault_fetch(self.store_id, doc_id.clone(), u64::MAX)
            .await
            .map_err(|e| anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e))?;
        match after.keys {
            Some(held) if held.dek_id == dek_id => {
                self.progress.keyring.deks.insert(doc.clone(), (dek_id, dek));
                self.progress.keyring.note_wraps(&doc_id, Some(held));
            }
            other => {
                self.progress.file.rekeying.remove(&doc);
                self.progress.unsaved = true;
                self.progress.keyring.note_wraps(&doc_id, other.clone());
                return Ok(match other {
                    Some(held) => Rekeyed::HasKeys(held),
                    None => Rekeyed::Waiting,
                });
            }
        }
        info!("Vault link for store {}: document {} has a data key now (it came under a share)", self.store_id, node);
        Ok(if self.finish_rekey(node).await? { Rekeyed::Done } else { Rekeyed::Waiting })
    }

    /// The documents whose snapshot under their new data key is still owed.
    pub(crate) fn rekeying(&self) -> Vec<NodeId> {
        self.progress.file.rekeying.iter().filter_map(|doc| match VaultDocId::parse(doc)? {
            VaultDocId::Node(id) => Some(id),
            VaultDocId::Tree => None,
        }).collect()
    }

    /// The second half of [`LinkSession::give_data_key`]: the snapshot.
    /// `false` while it cannot be made (the log has entries this device has
    /// not applied); the mark stays and a later pass asks again.
    pub(crate) async fn finish_rekey(&mut self, node: NodeId) -> anyhow::Result<bool> {
        let doc_id = VaultDocId::Node(node);
        let doc = doc_id.as_str();
        self.calls += 1;
        let fetch = match self.client.vault_fetch(self.store_id, doc_id.clone(), u64::MAX).await {
            Ok(fetch) => fetch,
            Err(e) if is_refusal(&e) => return Ok(false),
            Err(e) => return Err(anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e)),
        };
        self.progress.keyring.note_wraps(&doc_id, fetch.keys.clone());
        if fetch.keys.is_none() {
            // The keys never landed (a crash between the mark and the
            // call): the document is as it was, and the pass starts over.
            self.progress.file.rekeying.remove(&doc);
            self.progress.unsaved = true;
            return Ok(false);
        }
        if self.progress.cursor(&doc_id).applied_through() != fetch.head {
            return Ok(false);
        }
        if !upload_snapshot(self.handler, self.client, self.store_id, doc_id.clone(), self.link_key_id, fetch.head, self.progress).await? {
            return Ok(false);
        }
        self.progress.note_snapshot(&doc_id, fetch.head);
        self.progress.file.rekeying.remove(&doc);
        self.progress.unsaved = true;
        self.progress.save_if_unsaved().await;
        Ok(true)
    }
}

/// What a connection's first pass could not finish, asked for again on a
/// timer. Answers whether anything new was read.
///
/// - A document left unread for want of its key (see `Keyring::unread`).
/// - A share's recipient holds a list that names a child it does not hold:
///   a document another member created whose append was sent before it was
///   in the scope, or one its owner moved into the share (the scope is
///   published a moment after the tree edit, docs/NODE_DOCUMENT_CONTRACT.md
///   section 5: "a fetch that fails and is retried on the next set
///   publish"). Nothing announces a publish, so the remote is asked what it
///   lists now, and whatever is new is read. A whole replica awaits
///   nothing: what its lists name and it does not hold is missing, and
///   repair's to settle.
async fn pull_pending(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, link_id: &str, progress: &mut Progress) -> anyhow::Result<bool> {
    let mut pulled = false;

    let unread: Vec<String> = progress.keyring.unread.iter().filter(|(_, left)| **left > 0).map(|(doc, _)| doc.clone()).collect();
    for doc in unread {
        if let Some(left) = progress.keyring.unread.get_mut(&doc) {
            *left -= 1;
        }
        let Some(doc_id) = VaultDocId::parse(&doc) else { continue };
        pull_doc(handler, client, store_id, &doc_id, link_id, progress).await?;
        pulled |= !progress.keyring.unread.contains_key(&doc);
    }

    let awaited = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        manager.awaited_docs(store_id)
    };
    if !awaited.is_empty() {
        let listed = client.vault_list_docs(store_id).await.map_err(|e| anyhow::anyhow!("remote vaultListDocs failed: {}", e))?;
        for doc in listed {
            if doc.doc_id == VaultDocId::Tree || progress.keyring.pulled.contains(&doc.doc_id.as_str()) {
                continue;
            }
            progress.keyring.remote_docs.insert(doc.doc_id.as_str());
            if let Some(dek_id) = doc.dek_id {
                progress.keyring.listed_dek.insert(doc.doc_id.as_str(), dek_id);
            }
            progress.note_snapshot(&doc.doc_id, doc.snapshot_seq);
            pull_doc(handler, client, store_id, &doc.doc_id, link_id, progress).await?;
            pulled = true;
        }
    }
    if pulled {
        handler.repair_store_tree(store_id).await;
    }
    Ok(pulled)
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
