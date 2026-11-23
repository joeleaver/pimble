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
//!   every node document the remote already has, `vaultFetch` from the
//!   remembered `last_seq` for that document (`sync-state.json`, written only
//!   once the documents it covers are on the disk), decrypt every
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
//!   own appends echoed back through that same subscription by their
//!   `client_id` (`pimble_rpc::vault::should_apply`) and, as a second guard,
//!   by the `(doc_id, seq)` pairs it remembers handing back from `vaultAppend`.
//! - **Outbound**: subscribes to this server's local-change broadcast; every
//!   notification about a document carries that document's update bytes,
//!   which are encrypted and appended as they are, structure and content
//!   alike; one with no bytes (an older server's) pushes that document's
//!   current whole state instead.
//! - **Snapshots**: whenever an append leaves a document's log
//!   `pimble_rpc::vault::SNAPSHOT_EVERY` entries past its last snapshot, or
//!   (by the sizes the append's answer gives) past twice its snapshot's size
//!   and at least 1 MiB, and this device has read through it
//!   (`pimble_rpc::vault::snapshot_due_by`), the link uploads that document's
//!   current full state as a `vaultSnapshot` covering everything up to that
//!   sequence, so a fresh peer's first `vaultFetch` never replays the whole
//!   history and no log grows to its limit.
//! - **Parts** (docs/SYNC_LIMITS_CONTRACT.md): a snapshot or an append larger
//!   than `pimble_rpc::vault::PART_BYTES` goes as parts when the remote takes
//!   them (`VaultListDocsResponse::parts`), and a group that arrives in parts
//!   is applied once every part is here (`pimble_rpc::vault::PartBuffer`).
//! - **State reports** (docs/CONVERGENCE_CONTRACT.md, wave 1): the twin
//!   cannot say what it holds, so the devices say what they hold. After the
//!   reconcile, and 5 s after the last change to a document
//!   (`pimble_rpc::vault::REPORT_DEBOUNCE_MS`), the link seals the
//!   document's state vector and delete digest under its key and stores it
//!   in this device's slot (`vaultReport`; the device id is kept in the
//!   store's `sync-state.json`, outside the store's folder, `crate::devices`). At
//!   connect it reads every device's reports of the documents it holds
//!   (`vaultReports`), and live it hears each new one (`VaultReported`):
//!   where this device holds something a report lacks, it appends the diff
//!   from that report (`pimble_rpc::vault::Comparing`). This is what heals a
//!   wrong `pushed_sv` (2026-11-19: twelve structs that never left one
//!   device). A remote from before reports answers method-not-found, and the
//!   link carries on without them.
//! - **What waits** ([`Waiting`]): a document the link holds back, for any
//!   reason (an update the remote would not take, a blob no key opens, parts
//!   made by a newer Pimble), is listed with a sentence that says why, and
//!   every other document keeps syncing. `getStoreSync` and `listStores`
//!   carry the list; a change to it is announced as the link's state again.
//!
//! **Keys** (docs/NODE_DOCUMENT_CONTRACT.md section 5): the protocol is
//! `pimble_rpc::vault`'s, shared with the browser's vault client: a blob's
//! key is resolved by its header's key id (the document's data key, kept in
//! memory for the life of the link and never written down; its wraps opened
//! with a scope key the keystore holds; a scope key held directly, for a
//! blob from before data keys), and a document the remote has never seen
//! gets a data key from the device that creates it, wrapped under every
//! scope key this device holds that covers the node. A blob goes out under
//! the document's data key when it has one and under the link's scope key
//! when it does not (a document from before data keys, until its owner
//! gives it one). What is particular to the link (which documents the
//! remote holds, which were pulled in this connection) is [`Keyring`]'s,
//! and how a blob is sealed is `seal_plan`'s.
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
//! next one is made with a token minted from the grant as it is now. A
//! scope root the account's rows no longer name (a removal, a share its
//! owner stopped) is recorded as ended in the manifest and announced once
//! (`record_ended_roots`, `StoreChangeKind::SharesEnded`): it stays on this
//! device, read only, until the replica is removed, and the app stops
//! showing it. The scope roots themselves are never shrunk.
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
//!
//! **The relay tier** (docs/RELAY_CONTRACT.md) is this same link with
//! another remote ([`LinkEndpoint`]). A store shared from this computer
//! (`sync.json`'s `mode: "relay"`) links to this process's own relay face
//! (`crate::relay_face`), which the link has started, with the store's twin
//! open on it, before every connect; once it has reconciled and the shares'
//! upkeep has settled it tells the relay host the twin is whole, and the
//! tunnel announces the store. A member's replica of such a store connects
//! where the accounts service's `/token` says the store is reached (its
//! entry in `stores`, with that entry's own `token` when it carries one),
//! which is Pimble Cloud's relay; an owner who is not there answers close
//! code 4404, `owner offline`, which is `Offline` with the usual backoff
//! and nothing to report. A twin is derived and disposable: one that was
//! deleted is built again by its owner's link, with logs that start again
//! from 1, and its new `epoch` (`vaultListDocs`) is how every link that
//! read the old logs knows to forget its cursors and what it believed the
//! remote held (`Progress::forget_remote_logs`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreAccess, StoreId, SyncState};
use pimble_crypto::SymmetricKey;
use pimble_core::WaitingDoc;
use pimble_rpc::vault::{self as protocol, DocKeys, LogSizes, NewDocument, PartBuffer, SealKey, Unit, Unopened};
use pimble_rpc::{StoreChangeKind, StoreChangedNotification, VaultAppendRequest, VaultCursor, VaultDocId, VaultDocKeys, VaultSnapshotRequest};
use tokio::sync::{broadcast, mpsc, watch, Notify};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use url::Url;
use uuid::Uuid;

use pimble_store::sync_state;

use crate::handler::{LocalChange, Repair, RpcHandler};
use crate::share::{ShareCommand, Upkeep};

pub(crate) const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How long a store shared from this computer waits, once its link has
/// reconciled, for the shares' upkeep to settle before the tunnel announces
/// it anyway (see [`crate::relay_face::RelayHost::ready`]). Upkeep that
/// cannot settle (a share key this device was never handed) must not keep
/// every other share of the store from its members.
const ANNOUNCE_WAIT: Duration = Duration::from_secs(20);
const ANNOUNCE_CHECK_EVERY: Duration = Duration::from_millis(250);
/// `ProgressFile::cursor_version` once a replica's cursors are gap-aware. A
/// record below it (or none) was written when `last_seq` meant "the
/// highest number seen", which may sit beyond an entry that never arrived; one
/// full refetch of every document settles it.
const CURSOR_VERSION: u32 = 1;
/// To a remote that takes nothing in parts (an older server), a reconnect's
/// catch-up push is one blob per document, kept under that server's blob
/// limit (256 MiB since 0.26.1): one refused blob must not take the link
/// down, and the document waits with a sentence. A remote that takes parts
/// has no such limit.
/// While the sign-in is expired, how often a link looks for a new one.
const SIGN_IN_CHECK: Duration = Duration::from_secs(5);
const MAX_CATCH_UP_BLOB: usize = 200 * 1024 * 1024;
/// JSON-RPC code of "upload a snapshot before appending more"
/// (`pimble_rpc::RpcError::SnapshotRequired`).
const SNAPSHOT_REQUIRED: i32 = -32006;
/// How often the live loop writes the link's state (`sync-state.json`) when
/// it has changed.
const PROGRESS_SAVE_EVERY: Duration = Duration::from_secs(5);
/// How soon after a cursor moves it is written (with its documents put on the
/// disk first): past the content flush's debounce, so usually there is
/// nothing left to write but the cursor.
const CURSOR_SAVE_AFTER: Duration = Duration::from_secs(1);
/// How often a connected link asks the accounts service whether the
/// account's grant on the store is still what this connection was made
/// with (see [`grant_shape`]), unless `ServerConfig::grant_check_interval`
/// says otherwise.
pub(crate) const GRANT_CHECK_EVERY: Duration = Duration::from_secs(120);
/// How long a share's recipient waits before asking the remote again for a
/// document a held list names and the scope did not hold yet, doubling up
/// to [`MAX_AWAITED_POLL`] while nothing new arrives.
const AWAITED_POLL: Duration = Duration::from_secs(5);
/// How long a document's difference from what the other devices hold must
/// last, with this link connected, before it is out of step
/// (docs/CONVERGENCE_CONTRACT.md section 4), unless
/// `ServerConfig::out_of_step_after` says otherwise.
pub(crate) const OUT_OF_STEP_AFTER: Duration = Duration::from_secs(protocol::OUT_OF_STEP_AFTER_SECS);

const MAX_AWAITED_POLL: Duration = Duration::from_secs(60);

/// A running vault link for one store, mirroring [`crate::sync_link::SyncLinkHandle`]'s API.
pub struct VaultLinkHandle {
    state_rx: watch::Receiver<SyncState>,
    last_sync: Arc<Mutex<Option<DateTime<Utc>>>>,
    /// Whether the link is down because the relay says the store's owner is
    /// not there (see `run_loop`).
    owner_offline: Arc<std::sync::atomic::AtomicBool>,
    join: JoinHandle<()>,
    /// The store's key sweep (`crate::share::run_sweeper`), which lives and
    /// dies with the link.
    sweep: JoinHandle<()>,
    sweep_kick: Arc<Notify>,
    /// What a `cloudShare*` RPC asks of the share upkeep in the link's task.
    share_tx: mpsc::UnboundedSender<ShareCommand>,
    /// The documents the link is holding back, and why.
    waiting: Waiting,
}

impl VaultLinkHandle {
    /// The documents the link is holding back, each with its reason
    /// (`GetStoreSyncResponse::waiting`).
    pub fn waiting(&self) -> Vec<WaitingDoc> {
        self.waiting.list()
    }

    /// The documents out of step on this device, each with since when and
    /// why (`GetStoreSyncResponse::out_of_step`). What this device could not
    /// send says the step that fits what stopped it, as the link knows it
    /// now (Joe, 2026-11-20): `session_expired` is the keystore's word.
    pub fn out_of_step(&self, session_expired: bool) -> Vec<pimble_core::OutOfStep> {
        let step = if session_expired {
            Some(protocol::STEP_SIGN_IN)
        } else if matches!(self.state(), SyncState::Offline) {
            Some(protocol::STEP_ONLINE)
        } else {
            None
        };
        self.waiting.out_of_step_list(step)
    }

    pub fn state(&self) -> SyncState {
        self.state_rx.borrow().clone()
    }

    /// True while the link is `Offline` and the reason is known to be that
    /// the store's owner's computer is off (`GetStoreSyncResponse::owner_offline`).
    pub fn owner_offline(&self) -> bool {
        self.owner_offline.load(std::sync::atomic::Ordering::Relaxed) && matches!(self.state(), SyncState::Offline)
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

/// Where a vault link's remote is.
#[derive(Debug, Clone)]
pub enum LinkEndpoint {
    /// A twin held elsewhere: the hosted server at this URL (`sync.json`'s
    /// `remote.url`), or, when the accounts service's `/token` names an
    /// endpoint of the store's own, that one (a store served from its
    /// owner's computer through Pimble Cloud's relay).
    Remote(Url),
    /// This process's own relay face: the store is shared from this
    /// computer (docs/RELAY_CONTRACT.md). No URL, because the face's port
    /// is new every run; the link asks for it at every connect.
    RelayFace,
}

impl std::fmt::Display for LinkEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkEndpoint::Remote(url) => write!(f, "{url}"),
            LinkEndpoint::RelayFace => f.write_str("this computer's relay face"),
        }
    }
}

/// Namespace for [`VaultLink::start`].
pub struct VaultLink;

impl VaultLink {
    /// Start a vault link for `store_id`, whose twin is at `endpoint` (the
    /// hosted server's RPC endpoint from `mint_token`, saved in
    /// `sync.json`'s `remote.url`; or this process's relay face) and whose
    /// blobs this link encrypts with `key_id` (the key `cloudHostStore`/
    /// `cloudAddHostedStore`/`cloudRelayStore` recorded in `sync.json` as
    /// `vault_key_id`, looked up in the keystore fresh each connect — never
    /// cached here, so a key added to the keystore after this link started
    /// is picked up on the next reconnect).
    pub fn start(handler: RpcHandler, store_id: StoreId, endpoint: LinkEndpoint, key_id: Uuid, last_sync: Option<DateTime<Utc>>) -> VaultLinkHandle {
        let link_id = format!("vault-link:{}", Uuid::new_v4());
        let (state_tx, state_rx) = watch::channel(SyncState::Syncing);
        let last_sync = Arc::new(Mutex::new(last_sync));

        let owner_offline = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state = LinkState { store_id, state_tx, last_sync: Arc::clone(&last_sync), owner_offline: Arc::clone(&owner_offline) };
        let sweep_kick = Arc::new(Notify::new());
        let (share_tx, share_rx) = mpsc::unbounded_channel();
        let sweep = tokio::spawn(crate::share::run_sweeper(handler.clone(), store_id, Arc::clone(&sweep_kick)));
        let shares = ShareSide { commands: share_rx, upkeep: Upkeep::new(), sweep_kick: Arc::clone(&sweep_kick) };
        let waiting = Waiting::default();
        let join = tokio::spawn(run_loop(handler, endpoint, key_id, link_id, state, shares, waiting.clone()));

        VaultLinkHandle { state_rx, last_sync, owner_offline, join, sweep, sweep_kick, share_tx, waiting }
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
    owner_offline: Arc<std::sync::atomic::AtomicBool>,
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

async fn run_loop(handler: RpcHandler, endpoint: LinkEndpoint, key_id: Uuid, link_id: String, link: LinkState, mut shares: ShareSide, waiting: Waiting) {
    let store_id = link.store_id;
    // Subscribed once, for the life of the task: a change made while the link
    // is down still arrives here, which is how the link knows what to push
    // once it is back (see `Progress`).
    let mut local_rx = handler.subscribe_local_changes().await;
    let mut progress = Progress::load(&handler, store_id).await;
    progress.waiting = waiting;
    let mut backoff = INITIAL_BACKOFF;
    // A store served from its owner's computer is there while that computer
    // is, and for the rest of the time the relay answers `owner offline`:
    // this link being offline, the ordinary way, for hours or days. While
    // that is what the last attempt met, the next one asks the relay first
    // and stays `Offline`, quietly, until the owner is back (see `connect`).
    let mut owner_offline = false;
    loop {
        let mut reached_synced = false;
        let mut reached: Option<Reached> = None;
        let result =
            connect_and_sync(&handler, &endpoint, key_id, &link_id, &link, &mut local_rx, &mut progress, &mut reached_synced, &mut reached, owner_offline, &mut shares).await;
        if reached_synced {
            backoff = INITIAL_BACKOFF;
        }
        // `Ok` is a connection given up on purpose (the grant changed):
        // the next one is made at once.
        owner_offline = false;
        if let Err(e) = result {
            // Why, before saying so: whoever hears `Offline` asks at once
            // how the store is held (`getStoreSync`), and the answer has to
            // carry the reason already. A link that stays `Offline` while
            // the reason changes (the owner's computer came back and this
            // one lost its network, or the other way round) says `Offline`
            // again, which is what makes the app ask again.
            owner_offline = e.is::<OwnerOffline>()
                || match &reached {
                    Some(reached) if reached.via_relay => crate::relay_tunnel::owner_is_offline(&reached.url, &reached.token).await,
                    _ => false,
                };
            let reason_changed = link.owner_offline.swap(owner_offline, std::sync::atomic::Ordering::Relaxed) != owner_offline;
            let was_offline = matches!(*link.state_tx.borrow(), SyncState::Offline);
            set_state(&handler, &link, SyncState::Offline).await;
            if was_offline && reason_changed {
                handler.notify_sync_state_changed(store_id, SyncState::Offline).await;
            }
            shares.upkeep.link_down(&handler, store_id).await;
            progress.parts.clear();
            flush_waiting(&handler, &link, &progress).await;
            if e.is::<SessionExpired>() {
                // Said once, by `connect`; until someone signs in, the link
                // only gathers what changes here, checking every few seconds
                // whether a sign-in came.
                while handler.keystore().session_expired() {
                    note_local_changes_for(&mut local_rx, &mut progress, store_id, &link_id, SIGN_IN_CHECK).await;
                }
                backoff = INITIAL_BACKOFF;
                continue;
            }
            if owner_offline {
                debug!("Vault link for store {}: its owner's computer is offline ({}); trying again later", store_id, e);
            } else {
                warn!("Vault link for store {} to {} dropped: {}", store_id, endpoint, e);
            }
            note_local_changes_for(&mut local_rx, &mut progress, store_id, &link_id, backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }
}

// ── What waits (docs/SYNC_LIMITS_CONTRACT.md, "Saying what waits") ──────

/// Which way a document is held back: what this device has not sent, or what
/// it has not been able to read. Cleared separately: a send that goes through
/// says nothing about a blob that would not open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Side {
    Send,
    Read,
}

/// The documents a link is holding back, each with a sentence that says why;
/// shared between the link's task and its handle. Nothing is held back
/// silently: every per-document hold the link makes is recorded here, and
/// cleared when the document goes.
#[derive(Clone, Default)]
pub(crate) struct Waiting(Arc<Mutex<WaitingInner>>);

#[derive(Default)]
struct WaitingInner {
    docs: std::collections::BTreeMap<(String, Side), String>,
    /// The documents out of step on this device (docs/CONVERGENCE_CONTRACT.md
    /// section 4), kept here beside what waits because both are said the
    /// same way: in `getStoreSync`, and announced as the link's state again
    /// when they change.
    out_of_step: std::collections::BTreeMap<String, pimble_core::OutOfStep>,
    changed: bool,
}

impl Waiting {
    fn hold(&self, doc_id: &VaultDocId, side: Side, reason: impl Into<String>) {
        if !matches!(doc_id, VaultDocId::Node(_)) {
            return;
        }
        let reason = reason.into();
        let mut inner = self.0.lock().unwrap();
        if inner.docs.get(&(doc_id.as_str(), side)) != Some(&reason) {
            inner.docs.insert((doc_id.as_str(), side), reason);
            inner.changed = true;
        }
    }

    fn clear(&self, doc_id: &VaultDocId, side: Side) {
        let mut inner = self.0.lock().unwrap();
        if inner.docs.remove(&(doc_id.as_str(), side)).is_some() {
            inner.changed = true;
        }
    }

    /// One entry per document, the reason it cannot be sent first.
    fn list(&self) -> Vec<WaitingDoc> {
        let inner = self.0.lock().unwrap();
        let mut out: Vec<WaitingDoc> = Vec::new();
        for ((doc, _), reason) in &inner.docs {
            let Ok(node_id) = NodeId::parse(doc) else { continue };
            if !out.iter().any(|w| w.node_id == node_id) {
                out.push(WaitingDoc { node_id, reason: reason.clone() });
            }
        }
        out
    }

    fn take_changed(&self) -> bool {
        std::mem::take(&mut self.0.lock().unwrap().changed)
    }

    /// The documents out of step now, all of them: a list that differs from
    /// the one held is announced.
    fn set_out_of_step_all(&self, entries: std::collections::BTreeMap<String, pimble_core::OutOfStep>) {
        let mut inner = self.0.lock().unwrap();
        if inner.out_of_step != entries {
            inner.out_of_step = entries;
            inner.changed = true;
        }
    }

    /// The list, each "could not send" with its step: `step` when the link
    /// knows one for the whole store (signed out, offline), else what the
    /// document's own send is held back by.
    fn out_of_step_list(&self, step: Option<&str>) -> Vec<pimble_core::OutOfStep> {
        let inner = self.0.lock().unwrap();
        inner
            .out_of_step
            .values()
            .cloned()
            .map(|mut entry| {
                if entry.kind == pimble_core::OutOfStepKind::Unsent {
                    let held = inner.docs.get(&(VaultDocId::Node(entry.node_id).as_str(), Side::Send)).map(String::as_str);
                    entry.reason = protocol::with_step(&entry.reason, step.or(held));
                }
                entry
            })
            .collect()
    }
}

/// When what waits has changed, say so as the link's state again: whoever
/// hears it asks `getStoreSync`, which carries the list.
async fn flush_waiting(handler: &RpcHandler, link: &LinkState, progress: &Progress) {
    if progress.waiting.take_changed() {
        let state = link.state_tx.borrow().clone();
        debug!("Vault link for store {}: {} document(s) waiting", link.store_id, progress.waiting.list().len());
        handler.notify_sync_state_changed(link.store_id, state).await;
    }
}

/// What a connect attempt ends with when the relay says the store's owner
/// is not there (docs/RELAY_CONTRACT.md: close code 4404, `owner offline`).
/// Not a failure to report: the link is `Offline` and tries again.
#[derive(Debug)]
struct OwnerOffline;

impl std::fmt::Display for OwnerOffline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the relay answers `owner offline`")
    }
}

impl std::error::Error for OwnerOffline {}

/// Pimble Cloud refused the account's session: the link waits for a sign-in
/// rather than asking again every 30 seconds (2026-11-19: 58 refusals in a
/// quarter of an hour, and the status bar still said "Signed in").
#[derive(Debug)]
pub(crate) struct SessionExpired;

impl std::fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the Pimble Cloud sign-in expired; nothing is tried again until someone signs in")
    }
}

impl std::error::Error for SessionExpired {}

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

/// Where a connect attempt went, and with what: kept by the retry loop so
/// that a failure at a relayed endpoint can be asked about
/// (`crate::relay_tunnel::owner_is_offline`).
struct Reached {
    url: Url,
    token: String,
    via_relay: bool,
}

/// Mint a fresh JWT from the currently signed-in account and connect to the
/// store's twin with it as `Bearer`. Fails (and so goes through the retry
/// loop's backoff) with no signed-in account, a mint failure, or a
/// connection failure — all indistinguishable to a caller beyond the error
/// text, same as `crate::sync_link`'s own connect failures.
///
/// Where the twin is (see [`LinkEndpoint`]): this process's relay face,
/// started here if it is not running, with the store's twin open on it; or
/// the endpoint the accounts service names for the store in the same answer
/// that carries the token, and failing that the one `sync.json` holds.
///
/// `ask_first`: the last attempt met `owner offline`. The relay is asked
/// whether that still holds before anything else is tried, so a store whose
/// owner stays away costs one short connection a try and the link never
/// leaves `Offline` for it; `on_the_way` runs once the answer is that the
/// owner is there.
async fn connect(
    handler: &RpcHandler,
    store_id: StoreId,
    endpoint: &LinkEndpoint,
    reached: &mut Option<Reached>,
    ask_first: bool,
    on_the_way: impl std::future::Future<Output = ()>,
) -> anyhow::Result<PimbleClient> {
    if handler.link_faults().is_some_and(|faults| faults.is_offline()) {
        return Err(anyhow::anyhow!("this device has no network (a fault the test injected)"));
    }
    let account = handler
        .keystore()
        .account()
        .await
        .ok_or_else(|| anyhow::anyhow!("no Pimble Cloud account is signed in"))?;
    if handler.keystore().session_expired() {
        return Err(anyhow::Error::new(SessionExpired));
    }
    let minted = match crate::cloud::mint_token(handler.http(), &account.url, &account.session).await {
        Ok(minted) => minted,
        Err(e) if e.status() == Some(401) => {
            if !handler.keystore().session_expired() {
                handler.keystore().mark_session_expired();
                warn!("Pimble Cloud at {} refused this computer's sign-in (it expired, or was signed out elsewhere): sign in again to sync", account.url);
                handler.cloud_changed();
            }
            return Err(anyhow::Error::new(SessionExpired));
        }
        Err(e) => return Err(anyhow::anyhow!("minting a cloud token from {} failed: {}", account.url, e)),
    };
    let (url, token, via_relay) = match endpoint {
        LinkEndpoint::RelayFace => {
            let url = handler
                .relay()
                .prepare(handler, store_id, &account, &minted.token, false)
                .await
                .map_err(|e| anyhow::anyhow!("sharing from this computer is not up yet: {}", e))?;
            (url, minted.token.clone(), false)
        }
        LinkEndpoint::Remote(saved) => {
            let reach = minted.reach(store_id, Some(saved)).map_err(|e| anyhow::anyhow!(e))?;
            let via_relay = if reach.via_relay {
                note_endpoint(handler, store_id, &reach.url).await;
                true
            } else {
                // Not listed: the store is hosted, or this account holds
                // nothing of it any more. `sync.json` says which it was.
                let manager = handler.store_manager_handle();
                let manager = manager.read().await;
                manager.read_sync_config(store_id).await.ok().flatten().is_some_and(|config| config.via_relay)
            };
            (reach.url, reach.token, via_relay)
        }
    };
    if ask_first {
        if via_relay && crate::relay_tunnel::owner_is_offline(&url, &token).await {
            return Err(anyhow::Error::new(OwnerOffline));
        }
        on_the_way.await;
    }
    *reached = Some(Reached { url: url.clone(), token: token.clone(), via_relay });
    let auth = AuthMethod::Bearer { token };
    PimbleClient::connect_with_auth(url.as_str(), &auth)
        .await
        .map_err(|e| anyhow::anyhow!("{}", pimble_client::describe_connect_error(&url, &e)))
}

/// The accounts service names an endpoint of the store's own: `sync.json`
/// says so from now on (`Store::relay`, and where the next connect falls
/// back to). Best effort, like every other note the link keeps there.
async fn note_endpoint(handler: &RpcHandler, store_id: StoreId, url: &Url) {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let Ok(Some(mut config)) = manager.read_sync_config(store_id).await else { return };
    if config.via_relay && config.remote.url == *url {
        return;
    }
    info!("Vault link for store {}: it is served from its owner's computer, through {}", store_id, url);
    config.via_relay = true;
    config.remote.url = url.clone();
    if let Err(e) = manager.write_sync_config(store_id, &config).await {
        warn!("Vault link for store {}: could not record where the store is reached: {}", store_id, e);
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_and_sync(
    handler: &RpcHandler,
    endpoint: &LinkEndpoint,
    key_id: Uuid,
    link_id: &str,
    link: &LinkState,
    local_rx: &mut broadcast::Receiver<LocalChange>,
    progress: &mut Progress,
    reached_synced: &mut bool,
    reached: &mut Option<Reached>,
    owner_was_offline: bool,
    shares: &mut ShareSide,
) -> anyhow::Result<()> {
    let store_id = link.store_id;
    // `Syncing` says a connection is being made. While a relayed store's
    // owner is away none is: the link stays `Offline` until the relay says
    // otherwise, and does not flap at every try.
    if !owner_was_offline {
        set_state(handler, link, SyncState::Syncing).await;
    }

    let owner = refresh_grant(handler, store_id).await;
    let connected_as = grant_shape(handler, store_id).await;
    // Who is who in the store's shares, beside the connection rather than
    // in its way (docs/PRESENCE_CONTRACT.md section 3).
    spawn_people_refresh(handler, store_id);
    let mut client = connect(handler, store_id, endpoint, reached, owner_was_offline, set_state(handler, link, SyncState::Syncing)).await?;
    if let Some(bytes) = handler.link_faults().and_then(|faults| faults.page_bytes()) {
        client.set_vault_page_bytes(bytes);
    }
    let client = Arc::new(client);
    // What the last connection learned of the remote's documents and their
    // wraps is asked again; data keys already unwrapped stay.
    progress.keyring.forget_remote();

    // Subscribed before the reconcile, not after it: an append that lands
    // between the pull and the subscription would otherwise be seen by
    // neither. Anything the pull already applied arrives again here and
    // merges to nothing.
    let subscribed = if progress.reports_enabled { client.subscribe_store_changes_with_reports(store_id).await } else { client.subscribe_store_changes(store_id).await };
    let mut remote_sub = subscribed.map_err(|e| anyhow::anyhow!("subscribe to {} storeChanged failed: {}", endpoint, e))?;

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
    let grant_check_every = handler.grant_check_every();
    let mut grant_tick = tokio::time::interval_at(tokio::time::Instant::now() + grant_check_every, grant_check_every);
    let mut awaited_wait = AWAITED_POLL;
    let awaited_poll = tokio::time::sleep(awaited_wait);
    tokio::pin!(awaited_poll);

    set_state(handler, link, SyncState::Synced { last_sync: Utc::now() }).await;
    *reached_synced = true;
    info!("Vault link for store {} connected to {}", store_id, endpoint);
    // Presence rides beside the edits, sealed under each document's data key
    // (docs/PRESENCE_CONTRACT.md sections 2 and 9), for as long as this
    // connection lives.
    let _presence = crate::presence_link::spawn(handler, Arc::clone(&client), store_id, link_id, true);
    // Pictures ride beside the documents that name them, each a document of
    // its own on the twin (docs/IMAGES_CONTRACT.md "Replication"), for as
    // long as this connection lives.
    let pictures = crate::pictures::spawn_copier(
        handler,
        store_id,
        crate::pictures::VaultRemote { handler: handler.clone(), client: Arc::clone(&client), store_id, link_key_id: key_id, link_id: link_id.to_string() },
    )
    .await;
    pictures.all();

    // The shares' upkeep at connect: a pass as soon as the loop below turns
    // (reconciled first, so the pass judges the converged tree), and the
    // key sweep beside it.
    shares.upkeep.connected(handler, store_id, owner);
    shares.sweep_kick.notify_one();

    // Shared from this computer: the twin now holds everything this store
    // does. Members are let in once the shares' upkeep has settled too (the
    // scopes published, every document wrapped under its shares' keys), so
    // that nobody connects to a twin that was built again a moment ago and
    // finds their share empty; or after a while regardless.
    let mut announce_by = matches!(endpoint, LinkEndpoint::RelayFace).then(|| tokio::time::Instant::now() + ANNOUNCE_WAIT);
    let mut announce_check = tokio::time::interval(ANNOUNCE_CHECK_EVERY);

    // What the other devices say they hold, answered, and then what this one
    // holds, said (docs/CONVERGENCE_CONTRACT.md sections 1-2).
    compare_at_connect(handler, &client, store_id, key_id, link_id, &echoes, progress).await?;

    let steps_every = steps_every(handler.out_of_step_after());
    let mut steps_tick = tokio::time::interval_at(tokio::time::Instant::now() + steps_every, steps_every);
    let mut steps_last = tokio::time::Instant::now();
    // A test's faults (`crate::faults`): this device's network going away
    // is noticed within a tick.
    let faults_on = handler.link_faults().is_some();
    let mut fault_tick = tokio::time::interval(Duration::from_millis(100));
    // What a lost notification would have brought (wave 3, found by the
    // fault test): asked again every so often, never only at a reconnect.
    let recheck_every = if faults_on { RECHECK_EVERY_UNDER_FAULTS } else { RECHECK_EVERY };
    let mut recheck_tick = tokio::time::interval_at(tokio::time::Instant::now() + recheck_every, recheck_every);

    loop {
        flush_waiting(handler, link, progress).await;
        let next_report = progress.next_report_due();
        let cursor_save = progress.cursor_save_due;
        tokio::select! {
            // Read positions, a moment after they moved, behind their
            // documents (`Progress::save_if_unsaved`).
            _ = tokio::time::sleep_until(cursor_save.unwrap_or_else(tokio::time::Instant::now)), if cursor_save.is_some() => {
                progress.save_if_unsaved().await;
            }
            _ = steps_tick.tick() => {
                let now = tokio::time::Instant::now();
                judge_steps(handler, store_id, progress, now - steps_last).await;
                steps_last = now;
                settle_parked(handler, &client, store_id, key_id, link_id, &echoes, progress).await?;
            }
            _ = tokio::time::sleep_until(next_report.unwrap_or_else(tokio::time::Instant::now)), if next_report.is_some() => {
                let due = progress.take_due_reports();
                send_reports(handler, &client, store_id, key_id, due, progress).await?;
            }
            _ = announce_check.tick(), if announce_by.is_some() => {
                let waited_out = announce_by.is_some_and(|by| tokio::time::Instant::now() >= by);
                if waited_out || crate::share::settled(handler, store_id).await {
                    handler.relay().ready(store_id).await;
                    announce_by = None;
                }
            }
            _ = recheck_tick.tick() => {
                recheck(handler, &client, store_id, key_id, link_id, &echoes, progress).await?;
            }
            _ = fault_tick.tick(), if faults_on => {
                if handler.link_faults().is_some_and(|faults| faults.is_offline()) {
                    progress.save_if_unsaved().await;
                    return Err(anyhow::anyhow!("this device lost its network (a fault the test injected)"));
                }
            }
            item = remote_sub.next() => {
                match item {
                    Some(Ok(_)) if handler.link_faults().is_some_and(|faults| faults.take_dropped_notification()) => {
                        debug!("Vault link for store {}: a notification from the remote was lost (a fault the test injected)", store_id);
                    }
                    Some(Ok(notif)) => {
                        handle_remote_notification(handler, &client, store_id, key_id, link_id, notif, &echoes, progress).await?;
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
                        progress.note_report_due(store_id, &local_change);
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
                spawn_people_refresh(handler, store_id);
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
                if let ShareCommand::Resend(ids) = command {
                    // Whole states as fresh appends: whatever part of them a
                    // device missed, it merges now.
                    for id in ids {
                        let doc_id = VaultDocId::Node(id);
                        if !progress.keyring.remote_docs.contains(&doc_id.as_str()) {
                            continue;
                        }
                        // Once per state: a document sent again whole and
                        // still as it was is in the log already, whole, for
                        // whoever lacks it. A loop that cannot settle (a gap
                        // nobody can fill) asked again every ten minutes and
                        // got the same bytes each time (wave 3).
                        let state = report_of(handler, store_id, id).await.map(|r| r.encode());
                        if state.is_some() && progress.resent.get(&doc_id.as_str()) == state.as_ref() {
                            debug!("Vault link for store {}: node {} was sent again whole as it is now already; not again", store_id, id);
                            continue;
                        }
                        match push_full(handler, &client, store_id, doc_id.clone(), key_id, link_id, &echoes, progress).await {
                            Ok(()) => {
                                if let Some(state) = state {
                                    progress.resent.insert(doc_id.as_str(), state);
                                }
                            }
                            Err(e) => warn!("Vault link for store {}: sending node {} again failed: {}", store_id, id, e),
                        }
                    }
                    continue;
                }
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

/// Ask the accounts service again who is who in the store's shares
/// (`crate::people`); a name someone chose reaches this device within one
/// [`GRANT_CHECK_EVERY`].
fn spawn_people_refresh(handler: &RpcHandler, store_id: StoreId) {
    let handler = handler.clone();
    tokio::spawn(async move {
        crate::people::refresh(&handler, store_id).await;
    });
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
    let rows = crate::cloud::list_stores(handler.http(), &account.url, &account.session).await.ok()?;
    let ids = |roots: &[NodeId]| roots.iter().map(|id| id.to_string()).collect::<std::collections::BTreeSet<_>>();
    Some(GrantShape {
        held: crate::cloud::HeldAs::from_rows(&rows, store_id).map(|held_as| (held_as.access, ids(&held_as.roots), ids(&held_as.read_only_roots))),
    })
}

/// Read again, from the accounts service, how the signed-in account holds
/// this store, and bring `sync.json` (`access`, `shared_by`, the roots only
/// read), a partial replica's scope roots, the roots among them that have
/// ended, and the keystore in line with it: a role changed by the owner,
/// another share of the same store, a removal from one, a share's key
/// handed over since the replica was added. Best effort: the token the
/// connect mints next is what the hosted server judges, and a link with no
/// account or no network fails there, with its backoff.
///
/// Answers whether the account is an owner of the store, as far as the
/// rows say (`None`: they could not be read), which is what decides whether
/// this device keeps the store's shares up (`crate::share`).
async fn refresh_grant(handler: &RpcHandler, store_id: StoreId) -> Option<bool> {
    let account = handler.keystore().account().await?;
    let rows = match crate::cloud::list_stores(handler.http(), &account.url, &account.session).await {
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
            drop(manager);
            // Every share of it has ended. The documents stay where they
            // are until the replica is removed; the app stops showing them.
            record_ended_roots(handler, store_id, local_roots.clone()).await;
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
            let comment_roots = if local_roots.is_empty() { Vec::new() } else { held_as.comment_roots.clone() };
            if config.access != held_as.access || config.shared_by != held_as.shared_by || config.read_only_roots != read_only_roots || config.comment_roots != comment_roots {
                info!("Vault link for store {}: held as {:?} now (shared by {:?})", store_id, held_as.access, held_as.shared_by);
                config.access = held_as.access;
                config.shared_by = held_as.shared_by.clone();
                config.read_only_roots = read_only_roots;
                config.comment_roots = comment_roots;
                if let Err(e) = manager.write_sync_config(store_id, &config).await {
                    warn!("Vault link for store {}: could not record how the store is held: {}", store_id, e);
                }
            }
        }
    }
    // The shares that ended: exactly the roots held here that the account's
    // rows no longer name, so one granted again is un-ended by the same
    // comparison. After `sync.json`, so that whoever hears of it and asks
    // how the store is held reads the ended root as read only already. A
    // whole-store grant names no root and leaves none out: nothing of the
    // store has stopped being this account's.
    if !local_roots.is_empty() {
        let ended = if held_as.roots.is_empty() { Vec::new() } else { local_roots.iter().filter(|root| !held_as.roots.contains(root)).copied().collect() };
        record_ended_roots(handler, store_id, ended).await;
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

/// Record in a partial replica's manifest which of its scope roots the
/// account no longer holds (`StoreManifest::ended_roots`; the scope roots
/// themselves are never shrunk, and nothing else on disk is touched), and
/// when that changed anything, tell the store's subscribers which roots
/// ended just now (`StoreChangeKind::SharesEnded`): the app stops showing
/// them and says so. A set that did not change, which is every connect
/// after the one that learned of it, says nothing.
async fn record_ended_roots(handler: &RpcHandler, store_id: StoreId, ended: Vec<NodeId>) {
    let manager = handler.store_manager_handle();
    let (before, changed) = {
        let mut manager = manager.write().await;
        let before = manager.ended_roots(store_id);
        (before, manager.set_ended_roots(store_id, ended.clone()).await)
    };
    match changed {
        Ok(false) => {}
        Ok(true) => {
            let just_now: Vec<NodeId> = ended.iter().filter(|root| !before.contains(root)).copied().collect();
            info!("Vault link for store {}: {} share(s) of it ended just now; {} of the shares held here have ended in all", store_id, just_now.len(), ended.len());
            handler.notify_shares_ended(store_id, just_now).await;
        }
        Err(e) => warn!("Vault link for store {}: could not record which shares have ended: {}", store_id, e),
    }
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
        let grants = crate::cloud::get_store_keys(handler.http(), &account.url, &account.session, &store_id.to_string(), scope).await?;
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
// link asks. The link keeps the answer itself, per document, in the `vault`
// section of `sync-state.json` in the store's state directory (`<data dir>/
// pimble/stores/<store id>/`, this computer's alone; `vault-link.json` before
// 2026-11-22, in the store's folder before 2026-11-19,
// docs/STORE_FOLDERS_CONTRACT.md rule 1):
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
    /// Which logs the rest of this section (and `link.last_seq`) is
    /// about: `vaultListDocs`'s `epoch` as last read. `None` in a file from
    /// before epochs, and for a remote that names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    epoch: Option<String>,
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
    /// The store's state directory, where the link's state is one record
    /// (`pimble_store::sync_state`, `sync-state.json`). `None` keeps it in
    /// memory (a store with no state directory; never on a server).
    state_dir: Option<PathBuf>,
    /// The disk the record is on.
    disk: pimble_host::Disk,
    /// Whose documents the cursors are about, to put them on the disk before
    /// a cursor that covers changes to them is written.
    manager: Arc<tokio::sync::RwLock<pimble_store::StoreManager>>,
    file: ProgressFile,
    unsaved: bool,
    /// How far into each document's log this device has read without a gap.
    /// Rebuilt at every reconcile from [`Progress::last_seq`], which holds
    /// exactly `VaultCursor::applied_through` (a restart loses only the
    /// entries noted beyond a gap, which are fetched again and merge to
    /// nothing).
    cursors: HashMap<String, VaultCursor>,
    /// Each document's `applied_through` as this process has applied it: what
    /// the next reconcile reads from. Written to the record's `link.last_seq`
    /// only after the documents whose cursor moved are on the disk
    /// ([`Progress::save_if_unsaved`]), so a crash between applying an entry
    /// and writing its document can never leave a cursor past what the disk
    /// holds (docs/ONE_CODEBASE_CONTRACT.md wave 0: before it, the cursor was
    /// written at once and the document 750 ms later, and a crash in between
    /// left entries "read" that this device never held again).
    last_seq: HashMap<String, u64>,
    /// What the record holds of `last_seq`.
    last_seq_saved: HashMap<String, u64>,
    /// When the cursors that moved are written (a moment after the first
    /// one moved, by which time the content flush has usually written their
    /// documents already).
    cursor_save_due: Option<tokio::time::Instant>,
    /// Each document's snapshot number as last known, for deciding when the
    /// next one is due.
    snapshot_seqs: HashMap<String, u64>,
    /// The documents' data keys, in memory only.
    keyring: Keyring,
    /// Whether the remote takes snapshots and appends in parts, as its
    /// `vaultListDocs` said at this connection's reconcile.
    parts_ok: bool,
    /// Each document's log and snapshot sizes on the remote, as its last
    /// append's answer gave them.
    sizes: HashMap<String, LogSizes>,
    /// The parts of groups announced live and not all here yet.
    parts: PartBuffer,
    /// The documents held back, and why (shared with the handle).
    waiting: Waiting,
    /// Whether this server's links report and compare at all
    /// (`ServerConfig::vault_reports`).
    reports_enabled: bool,
    /// Whether the remote takes state reports, as this connection found.
    reports_ok: bool,
    /// When each document's report is due: 5 s after the last change to it.
    report_due: HashMap<String, tokio::time::Instant>,
    /// What the remote holds of this device's reports, as this connection has
    /// seen it (read back at connect, then what it stored): a report that
    /// says the same again is not sent.
    reported: HashMap<String, Vec<u8>>,
    /// This computer's device id in the store, for its state reports
    /// (docs/CONVERGENCE_CONTRACT.md section 1): random, kept in the record's
    /// `device` section outside the store's folder (`crate::devices`), so two
    /// computers syncing one folder through Dropbox are two devices. Never
    /// the account id.
    device: String,
    /// Which other devices' reports this computer has answered with an
    /// append, at which vector and delete digest (`pimble_rpc::vault::Comparing`):
    /// kept beside the device id, so a report that never moves (a replica
    /// removed) is answered once and not at every connect. Forgotten with
    /// the logs.
    answered: protocol::Comparing,
    answered_unsaved: bool,
    store_id: StoreId,
    /// What the other devices last said they hold of each document, as this
    /// connection heard it (document -> device -> report).
    heard: HashMap<String, HashMap<String, Heard>>,
    /// The documents that differ now, each with since when, for how long
    /// while connected, and why (section 4); the ones that have lasted
    /// long enough are out of step (`Waiting::set_out_of_step_all`).
    steps: HashMap<NodeId, Candidate>,
    /// What waits parked here and since when, and what was set aside
    /// (section 5): this computer's, in the store's state directory.
    convergence: Convergence,
    /// How long after a change a document's report goes.
    report_debounce: Duration,
    /// What each document was last sent again whole as (its report), so a
    /// repair loop that cannot settle sends the same thing once
    /// (docs/CONVERGENCE_CONTRACT.md wave 3: 2026-11-20, a device re-sent one
    /// document four times on a gap nobody could fill).
    resent: HashMap<String, Vec<u8>>,
    /// The (what the remote was known to hold, what this device held) of each
    /// document's last catch-up: the same pair again is the same diff again.
    caught_up: HashMap<String, (Vec<u8>, Vec<u8>)>,
    /// Each document's head as the remote last listed it (the reconcile, or
    /// the last [`recheck`]).
    listed_heads: HashMap<String, u64>,
}

// ── What waits parked here, and what was set aside (section 5) ──────────

/// The `convergence` section of `sync-state.json` (`convergence.json` before
/// 2026-11-22) in the store's state directory
/// (`<data dir>/pimble/stores/<store id>/`, this computer's alone, never in the
/// store's folder nor in a document): when each parked update was first
/// seen waiting for what it waits for, which is what makes the 24 hours
/// survive a restart, and the record of what was set aside.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct ConvergenceFile {
    #[serde(default)]
    parked: std::collections::BTreeMap<String, ParkedSince>,
    #[serde(default)]
    set_aside: std::collections::BTreeMap<String, Vec<protocol::SetAsideRecord>>,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ParkedSince {
    /// What it waits for, base64 of the canonical vector.
    missing: String,
    since: DateTime<Utc>,
}

#[derive(Default)]
struct Convergence {
    file: ConvergenceFile,
    unsaved: bool,
}

impl Convergence {
    /// From the record's `convergence` section (`convergence.json` before
    /// 2026-11-22), saved with the rest of the link's state
    /// ([`Progress::save_if_unsaved`]).
    fn of(section: Option<&serde_json::Value>) -> Self {
        let file = match section {
            Some(value) => serde_json::from_value(value.clone()).unwrap_or_else(|e| {
                warn!("Vault link: unreadable {} section ({}); starting over", sync_state::CONVERGENCE, e);
                ConvergenceFile::default()
            }),
            None => ConvergenceFile::default(),
        };
        Self { file, unsaved: false }
    }

    /// `doc` waits for `missing` now: since when it has (now, if it waited
    /// for something else or nothing before).
    fn waiting_since(&mut self, doc: &str, missing: &[u8], now: DateTime<Utc>) -> DateTime<Utc> {
        let encoded = STANDARD.encode(missing);
        match self.file.parked.get(doc) {
            Some(held) if held.missing == encoded => held.since,
            _ => {
                self.file.parked.insert(doc.to_string(), ParkedSince { missing: encoded, since: now });
                self.unsaved = true;
                now
            }
        }
    }

    /// `doc` waits for nothing now.
    fn not_waiting(&mut self, doc: &str) {
        if self.file.parked.remove(doc).is_some() {
            self.unsaved = true;
        }
    }

    /// The 24 hours of `doc` start again (a device that reported holds what
    /// it waits for).
    fn restart(&mut self, doc: &str, now: DateTime<Utc>) {
        if let Some(held) = self.file.parked.get_mut(doc) {
            held.since = now;
            self.unsaved = true;
        }
    }

    /// Whether an update of `doc` waiting for exactly `missing` was set
    /// aside here before.
    fn set_aside_before(&self, doc: &str, missing: &[u8]) -> bool {
        self.file.set_aside.get(doc).is_some_and(|records| records.iter().any(|r| r.missing_bytes().as_deref() == Some(missing)))
    }

    fn record_set_aside(&mut self, doc: &str, record: protocol::SetAsideRecord) {
        self.file.parked.remove(doc);
        let records = self.file.set_aside.entry(doc.to_string()).or_default();
        if !records.iter().any(|r| r.missing == record.missing) {
            records.push(record);
        }
        self.unsaved = true;
    }

    /// What this device set aside of `doc`, as its reports say it.
    fn aside_of(&self, doc: &str) -> Vec<Vec<u8>> {
        self.file.set_aside.get(doc).map(|records| records.iter().filter_map(|r| r.missing_bytes()).collect()).unwrap_or_default()
    }

    /// The latest set-aside of `doc`, while its Properties still say it.
    fn latest_set_aside(&self, doc: &str, now: DateTime<Utc>) -> Option<&protocol::SetAsideRecord> {
        self.file
            .set_aside
            .get(doc)
            .and_then(|records| records.iter().max_by_key(|r| r.at))
            .filter(|r| (now - r.at).num_days() < protocol::SET_ASIDE_SAID_FOR_DAYS)
    }

}

/// Who a report came from, and when the server stored it.
#[derive(Clone, Copy)]
struct HeardFrom<'a> {
    device: &'a str,
    account: Option<&'a str>,
    at: DateTime<Utc>,
}

/// Another device's report of a document, as heard.
struct Heard {
    report: protocol::Report,
    account: Option<String>,
    at: DateTime<Utc>,
}

/// A document that differs from what the other devices hold.
struct Candidate {
    since: DateTime<Utc>,
    held: Duration,
    reason: String,
    kind: pimble_core::OutOfStepKind,
    marked: bool,
}

/// What the link knows of the remote's documents and their data keys
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Keys"). In memory only: a
/// data key is unwrapped on demand from the document's wraps with a scope
/// key the keystore holds, and is gone with the process.
#[derive(Default)]
struct Keyring {
    /// The documents' data keys and wraps (`pimble_rpc::vault`).
    docs: DocKeys,
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
    /// Everything about the remote's documents (the documents' data keys
    /// included: a twin built again gives each a new one). The scope keys are
    /// the keystore's and stay.
    fn forget_remote(&mut self) {
        self.docs.forget_all();
        self.remote_docs.clear();
        self.pulled.clear();
        self.unresolved.clear();
        self.unread.clear();
    }

    fn note_wraps(&mut self, doc_id: &VaultDocId, keys: Option<VaultDocKeys>) {
        if let VaultDocId::Node(node_id) = doc_id {
            self.docs.note_wraps(*node_id, keys);
        }
    }

    fn note_listed(&mut self, doc_id: &VaultDocId, dek_id: Uuid) {
        if let VaultDocId::Node(node_id) = doc_id {
            self.docs.note_listed(*node_id, dek_id);
        }
    }
}

impl Progress {
    async fn load(handler: &RpcHandler, store_id: StoreId) -> Self {
        let manager = handler.store_manager_handle();
        let (state_dir, last_seq) = {
            let manager = manager.read().await;
            // This computer's, in the store's state directory, never in the
            // store's folder (docs/STORE_FOLDERS_CONTRACT.md rule 1).
            let last_seq = match manager.read_sync_config(store_id).await {
                Ok(Some(config)) => config.last_seq,
                _ => HashMap::new(),
            };
            (manager.state_dir(store_id), last_seq)
        };
        let disk = handler.disk().clone();
        let record = match &state_dir {
            Some(dir) => sync_state::read(&disk, dir).await,
            None => serde_json::Map::new(),
        };
        let convergence = Convergence::of(record.get(sync_state::CONVERGENCE));
        let report_debounce = handler.link_faults().and_then(|f| f.report_debounce()).unwrap_or(Duration::from_millis(protocol::REPORT_DEBOUNCE_MS));
        let entry = handler.devices().entry(store_id, state_dir.as_deref()).await;
        let file = match record.get(sync_state::VAULT) {
            Some(value) => serde_json::from_value(value.clone()).unwrap_or_else(|e| {
                warn!("Vault link for store {}: unreadable {} section ({}); starting over", store_id, sync_state::VAULT, e);
                ProgressFile::default()
            }),
            None => ProgressFile::default(),
        };
        Self {
            state_dir,
            disk,
            manager,
            file,
            unsaved: false,
            cursors: HashMap::new(),
            last_seq_saved: last_seq.clone(),
            last_seq,
            cursor_save_due: None,
            snapshot_seqs: HashMap::new(),
            keyring: Keyring::default(),
            parts_ok: false,
            sizes: HashMap::new(),
            parts: PartBuffer::default(),
            waiting: Waiting::default(),
            reports_enabled: handler.vault_reports(),
            reports_ok: false,
            report_due: HashMap::new(),
            reported: HashMap::new(),
            device: entry.device,
            answered: entry.answered,
            answered_unsaved: false,
            store_id,
            heard: HashMap::new(),
            steps: HashMap::new(),
            convergence,
            report_debounce,
            resent: HashMap::new(),
            caught_up: HashMap::new(),
            listed_heads: HashMap::new(),
        }
    }

    /// `device`'s report of `doc` was answered at `reported`.
    fn note_answered(&mut self, doc: &str, device: &str, reported: &protocol::Report) {
        self.answered.answered(doc, device, reported);
        self.answered_unsaved = true;
    }

    /// A change to a document, whoever made it (this device, or another's
    /// applied here): its report is due a debounce from now.
    fn note_report_due(&mut self, store_id: StoreId, change: &LocalChange) {
        if !self.reports_ok {
            return;
        }
        let LocalChange::Store(notif) = change else { return };
        if notif.store_id != store_id {
            return;
        }
        if let Some(node_id) = document_of(&notif.change_kind) {
            let due = tokio::time::Instant::now() + self.report_debounce;
            self.report_due.insert(VaultDocId::Node(node_id).as_str(), due);
        }
    }

    /// The soonest report due, if any.
    fn next_report_due(&self) -> Option<tokio::time::Instant> {
        self.report_due.values().min().copied()
    }

    /// The documents whose reports are due now, taken off the list.
    fn take_due_reports(&mut self) -> Vec<VaultDocId> {
        let now = tokio::time::Instant::now();
        let due: Vec<String> = self.report_due.iter().filter(|(_, at)| **at <= now).map(|(doc, _)| doc.clone()).collect();
        due.into_iter().filter_map(|doc| {
            self.report_due.remove(&doc);
            VaultDocId::parse(&doc)
        }).collect()
    }

    fn cursor(&mut self, doc_id: &VaultDocId) -> &mut VaultCursor {
        self.cursors.entry(doc_id.as_str()).or_default()
    }

    /// The remote's logs are other logs than the ones this device read (see
    /// `ProgressFile::epoch`): where each was read to, what the remote was
    /// known to hold and the data keys of the documents as they were are
    /// all forgotten. Every document is read from the start (a merge
    /// repeated is nothing) and offered whole (what the remote already has
    /// of it merges to nothing there).
    async fn forget_remote_logs(&mut self, store_id: StoreId) {
        self.file.pushed_sv.clear();
        self.file.dirty.clear();
        self.file.dirty_all = true;
        self.file.rekeying.clear();
        self.answered.forget();
        self.answered_unsaved = true;
        self.cursors.clear();
        self.snapshot_seqs.clear();
        self.keyring = Keyring::default();
        self.unsaved = true;
        self.last_seq.clear();
        // Forgetting goes on the disk at once: nothing has to be flushed
        // before a cursor goes back.
        self.save_if_unsaved().await;
        if !self.last_seq_saved.is_empty() {
            warn!("Vault link for store {}: could not forget where the old logs were read to; they are read from there until it can", store_id);
        }
    }

    /// `doc_id`'s cursor moved: what it has been read through is kept, and
    /// written a moment from now (after its document is on the disk).
    fn note_cursor(&mut self, doc_id: &VaultDocId) {
        let applied_through = self.cursor(doc_id).applied_through();
        let key = doc_id.as_str();
        if self.last_seq.get(&key).copied().unwrap_or(0) == applied_through {
            return;
        }
        self.last_seq.insert(key, applied_through);
        self.cursor_save_due.get_or_insert_with(|| tokio::time::Instant::now() + CURSOR_SAVE_AFTER);
    }

    /// Where `doc_id`'s log has been read to without a gap, as this process
    /// knows it (0 when never).
    fn last_seq(&self, doc_id: &VaultDocId) -> u64 {
        self.last_seq.get(&doc_id.as_str()).copied().unwrap_or(0)
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

    /// Write what changed of the link's state, as one atomic, durable write of
    /// the record (`pimble_store::sync_state`): the `vault`, `convergence`
    /// and `device` sections, and the cursors (`link.last_seq`). A cursor that
    /// moved forward is written only after its document is on the disk
    /// (`StoreManager::flush_nodes_durably`); when that fails the cursors stay
    /// as the record had them, which only means reading again. Best effort:
    /// bookkeeping, never fatal.
    async fn save_if_unsaved(&mut self) {
        let cursors_moved = self.last_seq != self.last_seq_saved;
        if !(self.unsaved || self.convergence.unsaved || self.answered_unsaved || cursors_moved) {
            return;
        }
        let Some(state_dir) = self.state_dir.clone() else {
            // Kept in memory only.
            self.unsaved = false;
            self.convergence.unsaved = false;
            self.answered_unsaved = false;
            self.last_seq_saved = self.last_seq.clone();
            self.cursor_save_due = None;
            return;
        };
        let mut cursors = cursors_moved.then(|| self.last_seq.clone());
        if let Some(moved) = &cursors {
            let forward: Vec<NodeId> = moved
                .iter()
                .filter(|(doc, seq)| self.last_seq_saved.get(*doc).is_none_or(|saved| saved < *seq))
                .filter_map(|(doc, _)| match VaultDocId::parse(doc) {
                    Some(VaultDocId::Node(id)) => Some(id),
                    _ => None,
                })
                .collect();
            if !forward.is_empty() {
                if let Err(e) = self.manager.write().await.flush_nodes_durably(self.store_id, &forward).await {
                    debug!("Vault link for store {}: its documents could not be put on the disk ({}); the read positions wait", self.store_id, e);
                    cursors = None;
                }
            }
        }
        let vault = self.unsaved.then(|| serde_json::to_value(&self.file));
        let convergence = self.convergence.unsaved.then(|| serde_json::to_value(&self.convergence.file));
        let answered = self.answered_unsaved.then(|| crate::devices::DeviceEntry { device: self.device.clone(), answered: self.answered.clone() });
        let written = sync_state::update(&self.disk, &state_dir, |record| {
            if let Some(Ok(vault)) = vault {
                record.insert(sync_state::VAULT.to_string(), vault);
            }
            if let Some(Ok(convergence)) = convergence {
                record.insert(sync_state::CONVERGENCE.to_string(), convergence);
            }
            if let Some(entry) = &answered {
                entry.put(record);
            }
            if let Some(cursors) = &cursors {
                let mut link = match record.remove(sync_state::LINK) {
                    Some(serde_json::Value::Object(link)) => link,
                    _ => serde_json::Map::new(),
                };
                if cursors.is_empty() {
                    link.remove("last_seq");
                } else if let Ok(value) = serde_json::to_value(cursors) {
                    link.insert("last_seq".to_string(), value);
                }
                if !link.is_empty() {
                    record.insert(sync_state::LINK.to_string(), serde_json::Value::Object(link));
                }
            }
        })
        .await;
        match written {
            Ok(()) => {
                self.unsaved = false;
                self.convergence.unsaved = false;
                self.answered_unsaved = false;
                if let Some(cursors) = cursors {
                    self.last_seq_saved = cursors;
                }
                self.cursor_save_due = (self.last_seq != self.last_seq_saved).then(|| tokio::time::Instant::now() + CURSOR_SAVE_AFTER);
            }
            Err(e) => {
                debug!("Vault link for store {}: could not write {}: {}", self.store_id, sync_state::path(&state_dir).display(), e);
                self.cursor_save_due = Some(tokio::time::Instant::now() + CURSOR_SAVE_AFTER);
            }
        }
    }
}

/// Forget what the remote was known to hold: the store is no longer linked to
/// that twin, and a record kept for one remote would overstate what another
/// holds. Called when a store is unlinked or linked elsewhere: the record's
/// `vault` and `convergence` sections go, the cursors (`link.last_seq`) with
/// them, and what was answered (it was answered in that twin's logs); the
/// device id stays.
pub(crate) async fn forget_progress(handler: &RpcHandler, store_id: StoreId) {
    let state_dir = handler.store_manager_handle().read().await.state_dir(store_id);
    let Some(state_dir) = state_dir else { return };
    let forgotten = sync_state::update(handler.disk(), &state_dir, |record| {
        record.remove(sync_state::VAULT);
        record.remove(sync_state::CONVERGENCE);
        if let Some(serde_json::Value::Object(link)) = record.get_mut(sync_state::LINK) {
            link.remove("last_seq");
        }
        if let Some(mut entry) = crate::devices::DeviceEntry::of(record) {
            entry.answered = protocol::Comparing::default();
            entry.put(record);
        }
    })
    .await;
    if let Err(e) = forgotten {
        warn!("Store {}: could not forget what its last remote held: {}", store_id, e);
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
        | StoreChangeKind::AttachmentChanged { node_id, .. }
        | StoreChangeKind::ContentUpdated { node_id } => Some(*node_id),
        StoreChangeKind::TreeStructure { node_ids } => node_ids.first().copied(),
        StoreChangeKind::VaultAppended { .. }
        | StoreChangeKind::VaultReported { .. }
        | StoreChangeKind::SyncStateChanged { .. }
        | StoreChangeKind::MountStateChanged { .. }
        | StoreChangeKind::ShareStateChanged { .. }
        | StoreChangeKind::SharesEnded { .. }
        | StoreChangeKind::PeopleChanged
        | StoreChangeKind::IndexStateChanged { .. } => None,
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
    let listing = client.vault_list_docs_response(store_id).await.map_err(|e| anyhow::anyhow!("remote vaultListDocs failed: {}", e))?;
    // Which logs these are. A relayed store's twin that was deleted is built
    // again by its owner's link (docs/RELAY_CONTRACT.md), and its logs count
    // from 1 again: every number this device kept, and everything it
    // believed the remote held, was about logs that are gone.
    if let Some(epoch) = &listing.epoch {
        match &progress.file.epoch {
            Some(read) if read != epoch => {
                info!("Vault link for store {}: the remote's logs are not the ones this device read (its twin was built again); reading them from the start and offering every document again", store_id);
                progress.forget_remote_logs(store_id).await;
                progress.file.epoch = Some(epoch.clone());
            }
            Some(_) => {}
            None => {
                progress.file.epoch = Some(epoch.clone());
                progress.unsaved = true;
            }
        }
    }
    let remote_docs = listing.docs;
    progress.parts_ok = listing.parts;
    progress.parts.clear();

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
        let applied_through = if refetch_all { 0 } else { progress.last_seq(&doc.doc_id) };
        progress.cursors.insert(doc.doc_id.as_str(), VaultCursor::starting_at(applied_through));
        progress.note_snapshot(&doc.doc_id, doc.snapshot_seq);
        progress.keyring.remote_docs.insert(doc.doc_id.as_str());
        if let Some(dek_id) = doc.dek_id {
            progress.keyring.note_listed(&doc.doc_id, dek_id);
        }
        progress.listed_heads.insert(doc.doc_id.as_str(), doc.head);
        remote_heads.insert(doc.doc_id, doc.head);
    }

    for doc_id in remote_heads.keys() {
        pull_doc(handler, client, store_id, doc_id, link_id, progress).await?;
    }
    // Every document is in before the tree is judged (see `Repair::Later`).
    handler.repair_store_tree(store_id).await;

    // Held as a reader: nothing is pushed, and nothing marked is forgotten
    // (a repair this replica made for itself goes out if the role changes).
    // Held as a commenter: the discussions only, the same way.
    let (doc_ids, access, partial, discussions): (Vec<VaultDocId>, StoreAccess, bool, HashSet<NodeId>) = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        let ids = manager.doc_ids(store_id).unwrap_or_default();
        let discussions = ids.iter().copied().filter(|id| manager.is_discussion(store_id, *id)).collect();
        (ids.into_iter().map(VaultDocId::Node).collect(), manager.store_access(store_id), !manager.scope_roots(store_id).is_empty(), discussions)
    };
    if !access.allows_comment() {
        if progress.file.cursor_version != CURSOR_VERSION {
            progress.file.cursor_version = CURSOR_VERSION;
            progress.unsaved = true;
        }
        progress.save_if_unsaved().await;
        return Ok(());
    }
    let mut refused = false;
    for doc_id in doc_ids {
        if !access.allows_write() && !matches!(&doc_id, VaultDocId::Node(id) if discussions.contains(id)) {
            if progress.is_dirty(&doc_id) {
                progress.mark_dirty(&doc_id);
                refused = true;
            }
            continue;
        }
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
            // Nothing of it is owed: nothing of it waits to be sent.
            progress.waiting.clear(&doc_id, Side::Send);
            continue;
        }
        if !progress.parts_ok && diff.len() > MAX_CATCH_UP_BLOB {
            warn!(
                "Vault link for store {} doc {:?}: a catch-up push of {} bytes is over what the remote takes in one blob, and it takes nothing in parts; the document waits",
                store_id, doc_id, diff.len()
            );
            progress.mark_dirty(&doc_id);
            progress.waiting.hold(&doc_id, Side::Send, protocol::TOO_LARGE_FOR_REMOTE);
            refused = true;
            continue;
        }
        // A document's first blob is its whole integrated state: the diff from
        // an empty vector (`known` is empty for an unseen document). An update
        // parked here behind a gap is not in it: a parked update is not sent on
        // by the device that parked it (docs/CONVERGENCE_CONTRACT.md wave 2);
        // it came from a log, which still has it, and the device that holds
        // what it waits for sends both. `push_full` sends the same.
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
    // Read from the cursor: every group comes whole, so what was held of one
    // from the live path is not needed.
    progress.parts.forget(&doc_id.as_str());

    let mut wants_a_key = false;
    let mut incomplete = false;
    let mut unread: Option<String> = None;
    for unit in protocol::fetched_units(&fetch) {
        incomplete |= matches!(unit, Unit::Incomplete);
        if let Some(seq) = unit.mark().and_then(|mark| mark.snapshot()) {
            progress.note_snapshot(doc_id, seq);
        }
        match apply_unit_locally(handler, &mut progress.keyring, store_id, doc_id, &unit, link_id, Repair::Later).await? {
            Applying::Applied(update) => {
                progress.advance(doc_id, &update);
                if let Some(mark) = unit.mark() {
                    mark.apply(progress.cursor(doc_id));
                }
            }
            Applying::UnknownKey(_) => {
                wants_a_key = true;
                unread.get_or_insert_with(|| protocol::WAITING_FOR_KEY.to_string());
            }
            Applying::Unreadable(reason) => {
                unread.get_or_insert(reason);
            }
            Applying::Skipped => {}
        }
    }
    // A key that may come, or parts of a large change that did not all come
    // in this read (another device stored a snapshot while it was paged, or
    // the rest is still being appended): read again shortly, a few times,
    // rather than at the next connect. Before 2026-11-19 an incomplete one
    // waited for a reconnect, however long the connection lasted.
    if wants_a_key || incomplete {
        progress.keyring.unread.entry(doc_id.as_str()).or_insert(UNREAD_RETRIES);
    } else {
        progress.keyring.unread.remove(&doc_id.as_str());
    }
    match unread {
        Some(reason) => progress.waiting.hold(doc_id, Side::Read, reason),
        None => progress.waiting.clear(doc_id, Side::Read),
    }
    progress.note_cursor(doc_id);
    Ok(())
}

/// What became of one blob.
enum Applying {
    /// Decrypted and merged: the update, for the caller to record that the
    /// remote holds it.
    Applied(Vec<u8>),
    /// No key this device can find opens it (its header's key id).
    UnknownKey(Uuid),
    /// It will not open or apply here, for the reason given (malformed,
    /// undecryptable under the key its header names, made by a newer Pimble,
    /// parts that are not all here, an update that would not merge): logged,
    /// never fatal to the link, and the document waits with that reason.
    Unreadable(String),
    /// The retired tree document's: nothing reads it.
    Skipped,
}

/// Whether the remote asked for a snapshot before more appends.
fn snapshot_required(error: &pimble_client::ClientError) -> bool {
    matches!(error, pimble_client::ClientError::Server { code, .. } if *code == SNAPSHOT_REQUIRED)
}

/// Whether the remote refused this document's blob for its size: the
/// document waits, the link stays.
fn is_document_error(error: &pimble_client::ClientError) -> bool {
    matches!(error, pimble_client::ClientError::Server { message, .. } if message.contains("per-blob limit") || message.contains("send the group again"))
}

/// Whether the remote lost track of a group being sent in parts (it
/// restarted, or a part came out of order): the group is sent again whole.
fn group_lost(error: &pimble_client::ClientError) -> bool {
    matches!(error, pimble_client::ClientError::Server { message, .. } if message.contains("send the group again"))
}

/// The sentence a document waits with when the remote would not take it.
fn refusal_reason(error: &pimble_client::ClientError) -> String {
    let message = match error {
        pimble_client::ClientError::Server { message, .. } => message.clone(),
        other => other.to_string(),
    };
    if message.contains("per-blob limit") {
        return protocol::TOO_LARGE_FOR_REMOTE.to_string();
    }
    if let Some(refusal) = StoreAccess::refusal_in(&message) {
        return refusal.to_string();
    }
    format!("The server would not take a change to it: {message}")
}

/// Whether the remote refused a call for this one document (it is out of
/// the account's scope now, or under a root the account only reads) as
/// opposed to failing: the document is left for later and the link stays up.
pub(crate) fn is_refusal(error: &pimble_client::ClientError) -> bool {
    let message = error.to_string();
    message.contains(crate::principal::NO_GRANT_FOR_DOCUMENT) || StoreAccess::refusal_in(&message).is_some()
}

/// Decrypt one unit of a document's log (a whole blob, or every part of a
/// group) and apply it locally through the handler's own
/// `apply_node_update_from`. A unit that will not open (a malformed blob, an
/// unknown key id, a decryption failure, parts made by a newer Pimble or not
/// all here) is logged and reported, never fatal to the link
/// (docs/CRYPTO_CONTRACT.md: "a blob with an unknown key id is logged and
/// skipped"); so is a blob of the retired tree document.
async fn apply_unit_locally(
    handler: &RpcHandler,
    keyring: &mut Keyring,
    store_id: StoreId,
    doc_id: &VaultDocId,
    unit: &Unit<'_>,
    link_id: &str,
    repair: Repair,
) -> anyhow::Result<Applying> {
    let VaultDocId::Node(node_id) = doc_id else {
        debug!("Vault link for store {}: skipping a blob of the retired tree document", store_id);
        return Ok(Applying::Skipped);
    };

    let scope = handler.keystore().store_keys(store_id).await;
    let plaintext = match keyring.docs.open_unit(store_id, *node_id, unit, &scope) {
        Ok(bytes) => bytes,
        Err(Unopened::NoKey(key_id)) => {
            warn!("Vault link for store {} doc {:?}: no key held for key id {}; skipping this blob", store_id, doc_id, key_id);
            return Ok(Applying::UnknownKey(key_id));
        }
        Err(e) => {
            warn!("Vault link for store {} doc {:?}: {}; skipping this blob", store_id, doc_id, e);
            return Ok(Applying::Unreadable(e.reason()));
        }
    };

    if let Err(e) = handler.apply_node_update_from(store_id, *node_id, &plaintext, Some(link_id), repair).await {
        // One document's update that will not merge holds back that
        // document, never the link.
        warn!("Vault link for store {} doc {:?}: the update would not apply: {}", store_id, doc_id, e.message());
        return Ok(Applying::Unreadable(format!("A change to it would not apply here ({}).", e.message())));
    }
    Ok(Applying::Applied(plaintext))
}

// ── Remote -> local (live) ───────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_remote_notification(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    key_id: Uuid,
    link_id: &str,
    notif: StoreChangedNotification,
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    if let StoreChangeKind::VaultReported { doc_id, device, blob, account } = &notif.change_kind {
        if progress.reports_ok {
            let heard = HeardFrom { device, account: account.as_deref(), at: Utc::now() };
            answer_report(handler, client, store_id, key_id, link_id, doc_id, heard, blob, echoes, progress).await?;
        }
        return Ok(());
    }
    let StoreChangeKind::VaultAppended { doc_id, seq, part } = &notif.change_kind else {
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
    if !protocol::should_apply(notif.source_client_id.as_deref(), link_id) {
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

    // One part of an append sent in parts: held until its group is whole.
    let group = match part {
        Some(part) if progress.keyring.pulled.contains(&doc_id.as_str()) => match progress.parts.take(&doc_id.as_str(), *seq, *part, blob.clone()) {
            Some(group) => Some(group),
            None => return Ok(()),
        },
        _ => None,
    };

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

    let (mark, unit) = match &group {
        Some((mark, parts)) => (*mark, Unit::Parts { mark: *mark, parts: parts.iter().map(|(p, b)| (*p, b.as_str())).collect() }),
        None => (protocol::Mark::One(*seq), Unit::Whole { mark: protocol::Mark::One(*seq), blob: blob.as_str() }),
    };
    match apply_unit_locally(handler, &mut progress.keyring, store_id, doc_id, &unit, link_id, Repair::Debounced).await? {
        Applying::Applied(update) => {
            progress.advance(doc_id, &update);
            mark.apply(progress.cursor(doc_id));
        }
        // A data key this link has not seen: the document was given one (or
        // a new one) since its wraps were fetched. Once per key.
        Applying::UnknownKey(key_id) if progress.keyring.unresolved.insert((doc_id.as_str(), key_id)) => {
            return pull_doc(handler, client, store_id, doc_id, link_id, progress).await;
        }
        Applying::UnknownKey(_) => progress.waiting.hold(doc_id, Side::Read, protocol::WAITING_FOR_KEY),
        Applying::Unreadable(reason) => progress.waiting.hold(doc_id, Side::Read, reason),
        Applying::Skipped => {}
    }
    progress.note_cursor(doc_id);
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
    // marked, and goes out if the role ever changes. A commenter's
    // discussions go out (docs/DISCUSSIONS_CONTRACT.md section 7).
    if !may_push(handler, store_id, &doc_id).await {
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

/// Whether this replica sends changes to `doc_id` at all: held as an editor
/// or the owner, or as a commenter for a discussion. A reader's replica never
/// appends (the local server refuses a reader's writes; what changes here is
/// its own upkeep).
async fn may_push(handler: &RpcHandler, store_id: StoreId, doc_id: &VaultDocId) -> bool {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let access = manager.store_access(store_id);
    access.allows_write() || (access.allows_comment() && matches!(doc_id, VaultDocId::Node(id) if manager.is_discussion(store_id, *id)))
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
    /// that covers the node (a create tried again: the same key).
    Create { dek_id: Uuid, dek: SymmetricKey, keys: VaultDocKeys, new: NewDocument },
    /// The document has a data key and no wrap this device can open.
    Unreadable,
}

/// Decide how `doc_id`'s next blob is sealed (module doc, "Keys").
async fn seal_plan(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, doc_id: &VaultDocId, link_key_id: Uuid, progress: &mut Progress) -> anyhow::Result<Seal> {
    let doc = doc_id.as_str();
    let VaultDocId::Node(node_id) = doc_id else {
        return Err(anyhow::anyhow!("the tree document is retired; nothing is written to it"));
    };
    let node_id = *node_id;

    if progress.keyring.remote_docs.contains(&doc) {
        // Wraps are fetched with the document; one only listed so far, or
        // heard of through another device's append, is asked for now.
        if progress.keyring.docs.wants_wraps(node_id) {
            match client.vault_fetch(store_id, doc_id.clone(), u64::MAX).await {
                Ok(fetch) => progress.keyring.note_wraps(doc_id, fetch.keys),
                Err(e) if is_refusal(&e) => return Ok(Seal::Unreadable),
                Err(e) => return Err(anyhow::anyhow!("remote vaultFetch for {:?} failed: {}", doc_id, e)),
            }
        }
        // From before data keys: under the scope key, as it always was,
        // until its owner gives it a data key. A recipient never makes
        // keys for a document that exists.
        let scope = handler.keystore().store_keys(store_id).await;
        return match progress.keyring.docs.seal_key(store_id, node_id, Some(link_key_id), &scope) {
            SealKey::With(key_id, key) => Ok(Seal::With { key_id, key }),
            SealKey::Unreadable => Ok(Seal::Unreadable),
            SealKey::NoScopeKey => Err(anyhow::anyhow!("no local key held for store {} key id {}; cannot encrypt an outgoing update", store_id, link_key_id)),
        };
    }

    // A new document. The scope keys that cover it: the store key on a
    // whole replica (this device holds it: it is the owner's, or a member's
    // of the whole store), and the key of every share the node is under
    // that this device holds. On a share's recipient with no marker on the
    // way up (a share from before markers, or a root whose document has not
    // arrived), the key this replica was added with, which is its share's.
    let (scope_key_ids, new) = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        let partial = !manager.scope_roots(store_id).is_empty();
        let tree = manager.tree(store_id).map_err(|e| anyhow::anyhow!("store unavailable: {}", e))?;
        let store_key = (!partial).then_some(link_key_id);
        (protocol::scope_key_ids(tree, node_id, store_key, [link_key_id]), NewDocument::of(tree, node_id))
    };
    let scope = handler.keystore().store_keys(store_id).await;
    let Some((dek_id, dek, keys)) = progress.keyring.docs.create_key(store_id, node_id, &scope_key_ids, &scope) else {
        return Err(anyhow::anyhow!("no scope key held for store {}; cannot encrypt a new document", store_id));
    };
    Ok(Seal::Create { dek_id, dek, keys, new })
}

/// Encrypt `plaintext` and append it to `doc_id`'s vault log (in parts when
/// it is larger than a part and the remote takes them), recording the echo
/// and snapshotting when one is due (`pimble_rpc::vault::snapshot_due_by`).
/// Whatever the remote refuses holds back this document alone, with a
/// sentence ([`Waiting`]); anything else that fails is the connection's.
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
    let VaultDocId::Node(node_id) = doc_id else {
        return Err(anyhow::anyhow!("the tree document is retired; nothing is written to it"));
    };
    let (key_id, key, created) = match seal_plan(handler, client, store_id, &doc_id, key_id, progress).await? {
        Seal::With { key_id, key } => (key_id, key, None),
        Seal::Create { dek_id, dek, keys, new } => (dek_id, dek, Some((keys, new))),
        Seal::Unreadable => {
            warn!("Vault link for store {} doc {:?}: no wrap of its data key opens here; leaving the update for later", store_id, doc_id);
            progress.waiting.hold(&doc_id, Side::Send, protocol::WAITING_FOR_KEY);
            return Ok(Appended::Refused);
        }
    };
    let sealed = protocol::seal_parts(store_id, node_id, key_id, &key, plaintext, progress.parts_ok);

    // A new document's first blob carries its wrapped data key, and the
    // remote stores the two together (`VaultAppendRequest::keys`): no blob
    // is ever there under a key the remote has no wrap of, whichever way
    // this call ends. Attributed to this link's own id so the notification
    // it produces is dropped by identity if it echoes back through this
    // link's own subscription; the seen-seq set below is kept as a second
    // guard for anything that reaches the server without a `client_id`. A
    // new document names its parent, which is what admits a scoped member's
    // create (and is ignored for anyone else's). Every part of a group
    // carries the same; the remote stores the keys when the last lands.
    let keys = created.as_ref().map(|(keys, _)| keys.clone());
    let new = created.as_ref().map(|(_, new)| *new).unwrap_or_default();
    let send = || async {
        let mut answer = None;
        for one in &sealed {
            let request = VaultAppendRequest {
                store_id,
                doc_id: doc_id.clone(),
                blob: one.blob.clone(),
                client_id: Some(link_id.to_string()),
                parent_id: new.discussion_host.or(new.parent_id),
                keys: keys.clone(),
                discussion: new.discussion_host.is_some(),
                part: one.part,
            };
            answer = Some(client.vault_append_request(request).await?);
        }
        Ok::<_, pimble_client::ClientError>(answer.expect("a blob always seals to at least one"))
    };
    let mut appended = send().await;
    // The remote lost the group half way (it restarted): once more, whole.
    if matches!(&appended, Err(e) if group_lost(e)) {
        appended = send().await;
    }
    // The remote's log for this document is full: snapshot what this device
    // has applied (a snapshot holds the whole document, this update
    // included), then send the update once more. Whatever happens, only this
    // document waits: the link stays up for every other one.
    if matches!(&appended, Err(e) if snapshot_required(e)) {
        let upto = progress.cursor(&doc_id).applied_through();
        warn!("Vault link for store {} doc {:?}: the remote's log is full; snapshotting through seq {} and sending again", store_id, doc_id, upto);
        let snapped = upto > 0 && upload_snapshot(handler, client, store_id, doc_id.clone(), key_id, upto, progress).await.unwrap_or_else(|e| {
            warn!("Vault link for store {} doc {:?}: the snapshot failed: {}", store_id, doc_id, e);
            false
        });
        if snapped {
            progress.note_snapshot(&doc_id, upto);
            appended = send().await;
        }
    }
    let answer = match appended {
        Ok(answer) => answer,
        Err(e) if is_refusal(&e) => {
            debug!("Vault link for store {} doc {:?}: append refused ({}); leaving the document for later", store_id, doc_id, e);
            progress.waiting.hold(&doc_id, Side::Send, refusal_reason(&e));
            return Ok(Appended::Refused);
        }
        Err(e) if snapshot_required(&e) => {
            warn!("Vault link for store {} doc {:?}: the remote's log is full and no snapshot could empty it ({}); the document waits, the rest keeps syncing", store_id, doc_id, e);
            progress.waiting.hold(&doc_id, Side::Send, "Its log on the server is full, and this device could not yet send the snapshot that empties it.");
            return Ok(Appended::Refused);
        }
        Err(e) if is_document_error(&e) => {
            warn!("Vault link for store {} doc {:?}: the remote would not take this update ({}); leaving the document for later, the rest keeps syncing", store_id, doc_id, e);
            progress.waiting.hold(&doc_id, Side::Send, refusal_reason(&e));
            return Ok(Appended::Refused);
        }
        Err(e) => return Err(anyhow::anyhow!("remote vaultAppend for {:?} failed: {}", doc_id, e)),
    };
    // A test's fault: the remote stored it, and the answer never came back.
    if handler.link_faults().is_some_and(|faults| faults.take_lost_answer()) {
        return Err(anyhow::anyhow!("the connection dropped before the answer to an append of {:?} came (a fault the test injected)", doc_id));
    }
    let seq = answer.seq;
    let first = answer.first_seq.unwrap_or(seq);
    for one in first..=seq {
        echoes.remember(&doc_id, one);
    }
    progress.keyring.remote_docs.insert(doc.clone());
    progress.keyring.pulled.insert(doc.clone());
    if let Some((keys, _)) = &created {
        progress.keyring.docs.note_created(node_id, keys);
    }
    protocol::Mark::Span(first, seq).apply(progress.cursor(&doc_id));
    progress.note_cursor(&doc_id);
    progress.waiting.clear(&doc_id, Side::Send);
    let sizes = LogSizes::of(&answer);
    if let Some(sizes) = sizes {
        progress.sizes.insert(doc.clone(), sizes);
    }
    debug!("Vault link for store {} pushed doc {:?} update (seq {}, {} part(s))", store_id, doc_id, seq, sealed.len());

    // Stamped only with a number this device has read *through*
    // (`snapshot_due_by`): a snapshot deletes every log entry at or below it.
    let snapshot_seq = progress.snapshot_seq(&doc_id);
    let due_by_fault = handler
        .link_faults()
        .and_then(|faults| faults.snapshot_every())
        .is_some_and(|every| seq.saturating_sub(snapshot_seq) >= every && progress.cursor(&doc_id).applied_through() == seq);
    if due_by_fault || protocol::snapshot_due_by(seq, snapshot_seq, progress.cursor(&doc_id), sizes) {
        if upload_snapshot(handler, client, store_id, doc_id.clone(), key_id, seq, progress).await? {
            progress.note_snapshot(&doc_id, seq);
        }
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
        Appended::Yes => {
            // Only a real gap is caught up: the update started past what the
            // remote is known to hold, or the document is marked. The local
            // document running ahead of this link's queue (edits made since,
            // not forwarded yet) is no gap; their own appends follow.
            let gap = !pimble_crdt::continues_from(&progress.known(&doc_id), plaintext).unwrap_or(true);
            progress.advance(&doc_id, plaintext);
            if gap || progress.is_dirty(&doc_id) {
                catch_up(handler, client, store_id, doc_id, key_id, link_id, echoes, progress).await?;
            }
        }
        Appended::Refused => progress.mark_dirty(&doc_id),
    }
    Ok(())
}

/// After a live append: when the remote is known to lack something of the
/// document all the same (an earlier change refused or never forwarded, an
/// update merged here that started past a gap), send it now rather than at
/// the next connect. A connection can last all day, and every edit made on
/// top of the gap waits, unseen, on every other device until the gap is
/// filled (2026-11-19: twelve structs of a shared note, and everything typed
/// after them).
#[allow(clippy::too_many_arguments)]
async fn catch_up(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    doc_id: VaultDocId,
    key_id: Uuid,
    link_id: &str,
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let VaultDocId::Node(node_id) = &doc_id else { return Ok(()) };
    let known = progress.known(&doc_id);
    let local_sv = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        match manager.node_state_vector(store_id, *node_id) {
            Ok(sv) => sv,
            Err(_) => return Ok(()),
        }
    };
    let ahead = pimble_crdt::state_vector_exceeds(&local_sv, &known).unwrap_or(false);
    if !ahead && !progress.is_dirty(&doc_id) {
        return Ok(());
    }
    // The same gap as last time, from the same place: what was sent then is
    // in the log, and sending it again changes nothing for anyone (wave 3: a
    // gap nobody can fill made a device send the same catch-up again).
    if progress.caught_up.get(&doc_id.as_str()) == Some(&(known.clone(), local_sv.clone())) && !progress.is_dirty(&doc_id) {
        debug!("Vault link for store {} doc {:?}: this catch-up was sent already; not again", store_id, doc_id);
        return Ok(());
    }
    let (local_sv, diff) = doc_diff(handler, store_id, &doc_id, &known).await?;
    if !progress.parts_ok && diff.len() > MAX_CATCH_UP_BLOB {
        progress.mark_dirty(&doc_id);
        return Ok(());
    }
    info!("Vault link for store {} doc {:?}: the remote lacks part of it after a live append; sending {} bytes to fill the gap", store_id, doc_id, diff.len());
    match append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, &diff, echoes, progress).await? {
        Appended::Yes => {
            progress.caught_up.insert(doc_id.as_str(), (known, local_sv.clone()));
            progress.set_known(&doc_id, &local_sv);
            progress.clear_dirty(&doc_id);
        }
        Appended::Refused => progress.mark_dirty(&doc_id),
    }
    Ok(())
}

/// Push a document's current whole state as a fresh append (the fallback for a
/// change with no update bytes to reuse, a document the remote has never seen,
/// and a repair loop's `Resend`): everything it holds integrated, the diff from
/// an empty vector, as the reconcile's first blob is. Never `save()`: that
/// carries an update parked here behind a gap, which the device that parked it
/// does not send on (docs/CONVERGENCE_CONTRACT.md wave 2; until 2026-11-22 this
/// path sent it and the reconcile's did not). A snapshot is the one place a
/// parked update rides (`full_doc_state`): it replaces the log entries that
/// carried it.
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
    let (local_sv, state) = doc_diff(handler, store_id, &doc_id, &pimble_crdt::empty_state_vector()).await?;
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
    let VaultDocId::Node(node_id) = doc_id else { return Ok(false) };
    let (_, plaintext) = full_doc_state(handler, store_id, &doc_id).await?;
    // In parts when it is larger than a part and the remote takes them: the
    // remote replaces the snapshot it holds only when the last part lands.
    let sealed = protocol::seal_parts(store_id, node_id, key_id, &key, &plaintext, progress.parts_ok);
    for one in &sealed {
        let request = VaultSnapshotRequest { store_id, doc_id: doc_id.clone(), upto_seq, blob: one.blob.clone(), covers_prefix: true, part: one.part };
        match client.vault_snapshot_request(request).await {
            Ok(()) => {}
            Err(e) if is_refusal(&e) || is_document_error(&e) => {
                debug!("Vault link for store {} doc {:?}: snapshot refused ({}); the log keeps everything", store_id, doc_id, e);
                return Ok(false);
            }
            Err(e) => return Err(anyhow::anyhow!("remote vaultSnapshot for {:?} failed: {}", doc_id, e)),
        }
    }
    debug!("Vault link for store {} uploaded a snapshot for doc {:?} up to seq {} ({} part(s))", store_id, doc_id, upto_seq, sealed.len());
    Ok(true)
}

// ── State reports and comparing (docs/CONVERGENCE_CONTRACT.md, wave 1) ────

/// What this device holds of `node_id` now; `None` when it does not hold it.
async fn report_of(handler: &RpcHandler, store_id: StoreId, node_id: NodeId) -> Option<protocol::Report> {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let tree = manager.tree(store_id).ok()?;
    tree.doc(node_id).map(protocol::Report::of)
}

/// The documents this replica holds that the remote holds too: the ones it
/// reports on and compares.
async fn held_on_both(handler: &RpcHandler, store_id: StoreId, progress: &Progress) -> Vec<VaultDocId> {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    manager
        .doc_ids(store_id)
        .unwrap_or_default()
        .into_iter()
        .map(VaultDocId::Node)
        .filter(|doc_id| progress.keyring.remote_docs.contains(&doc_id.as_str()))
        .collect()
}

/// At connect, once the reconcile is done: read every device's reports of
/// the documents this replica holds and answer each that lacks something
/// held here, then report every document whose report the remote does not
/// already hold as it is. A remote from before reports answers
/// method-not-found, and the link goes on as it did before them.
async fn compare_at_connect(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    key_id: Uuid,
    link_id: &str,
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    progress.reported.clear();
    progress.report_due.clear();
    progress.heard.clear();
    progress.reports_ok = progress.reports_enabled;
    if !progress.reports_ok {
        return Ok(());
    }
    let held = held_on_both(handler, store_id, progress).await;
    for batch in held.chunks(protocol::REPORTS_BATCH) {
        let reports = match client.vault_reports(store_id, batch.to_vec()).await {
            Ok(reports) => reports,
            Err(e) if e.is_method_not_found() => {
                info!("Vault link for store {}: the remote takes no state reports (an older Pimble server); syncing without them", store_id);
                progress.reports_ok = false;
                return Ok(());
            }
            Err(e) => return Err(anyhow::anyhow!("remote vaultReports failed: {}", e)),
        };
        for report in reports {
            let heard = HeardFrom { device: &report.device, account: report.account.as_deref(), at: report.at };
            answer_report(handler, client, store_id, key_id, link_id, &report.doc_id, heard, &report.blob, echoes, progress).await?;
        }
    }
    send_reports(handler, client, store_id, key_id, held, progress).await?;
    progress.save_if_unsaved().await;
    Ok(())
}

/// Seal and store this device's report of each of `docs` that the remote
/// holds and whose report it does not already hold as it is now, several at
/// a time. A document with no key this device may seal under is not
/// reported; one the remote will not take a report of is left; anything else
/// that fails is the connection's.
async fn send_reports(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, key_id: Uuid, docs: Vec<VaultDocId>, progress: &mut Progress) -> anyhow::Result<()> {
    if !progress.reports_ok {
        return Ok(());
    }
    let mut sealed: Vec<(VaultDocId, Vec<u8>, String)> = Vec::new();
    for doc_id in docs {
        let VaultDocId::Node(node_id) = doc_id else { continue };
        let doc = doc_id.as_str();
        // Never a document the remote lacks: sealing one would make it a
        // data key, and a report is no way to create a document.
        if !progress.keyring.remote_docs.contains(&doc) {
            continue;
        }
        let Some(report) = report_of(handler, store_id, node_id).await else { continue };
        // What this device set aside of it, so a device holding the same
        // parked update sets it aside too (section 5).
        let report = report.with_aside(progress.convergence.aside_of(&doc));
        let plaintext = report.encode();
        if progress.reported.get(&doc) == Some(&plaintext) {
            continue;
        }
        let (seal_key_id, key) = match seal_plan(handler, client, store_id, &doc_id, key_id, progress).await {
            Ok(Seal::With { key_id, key }) => (key_id, key),
            Ok(_) => continue,
            Err(e) => {
                debug!("Vault link for store {} doc {:?}: not reported ({})", store_id, doc_id, e);
                continue;
            }
        };
        let blob = protocol::seal_report(store_id, node_id, seal_key_id, &key, &report);
        sealed.push((doc_id, plaintext, blob));
    }
    let device = progress.device.clone();
    for batch in sealed.chunks(protocol::REPORTS_IN_FLIGHT) {
        let sent = futures::future::join_all(batch.iter().map(|(doc_id, _, blob)| client.vault_report(store_id, doc_id.clone(), &device, blob.clone()))).await;
        for ((doc_id, plaintext, _), result) in batch.iter().zip(sent) {
            match result {
                Ok(()) => {
                    progress.reported.insert(doc_id.as_str(), plaintext.clone());
                }
                Err(e) if e.is_method_not_found() => {
                    info!("Vault link for store {}: the remote takes no state reports (an older Pimble server); syncing without them", store_id);
                    progress.reports_ok = false;
                    return Ok(());
                }
                Err(e @ pimble_client::ClientError::Server { .. }) => {
                    debug!("Vault link for store {} doc {:?}: the remote would not take its report ({})", store_id, doc_id, e);
                }
                Err(e) => return Err(anyhow::anyhow!("remote vaultReport for {:?} failed: {}", doc_id, e)),
            }
        }
    }
    Ok(())
}

/// Another device's report of `doc_id` (read at connect, or heard live):
/// when this device holds something it lacks, and has not answered that
/// same report already, append the diff from it
/// (`pimble_rpc::vault::Comparing`). A report that will not open here is
/// ignored, and so is this device's own (beyond noting what the remote holds
/// of it). A reader's replica answers nothing: it never appends.
#[allow(clippy::too_many_arguments)]
async fn answer_report(
    handler: &RpcHandler,
    client: &PimbleClient,
    store_id: StoreId,
    key_id: Uuid,
    link_id: &str,
    doc_id: &VaultDocId,
    from_device: HeardFrom<'_>,
    blob: &str,
    echoes: &Arc<EchoTracker>,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let device = from_device.device;
    let VaultDocId::Node(node_id) = doc_id else { return Ok(()) };
    let doc = doc_id.as_str();
    let scope = handler.keystore().store_keys(store_id).await;
    let report = match progress.keyring.docs.open_report(store_id, *node_id, blob, &scope) {
        Ok(report) => report,
        Err(e) => {
            debug!("Vault link for store {} doc {:?}: device {}'s report does not open here ({}); ignoring it", store_id, doc_id, device, e);
            return Ok(());
        }
    };
    if device == progress.device {
        progress.reported.insert(doc, report.encode());
        return Ok(());
    }
    // Heard, whatever comes of it: what tells whether the document is in
    // step (section 4).
    progress
        .heard
        .entry(doc.clone())
        .or_default()
        .insert(device.to_string(), Heard { report: report.clone(), account: from_device.account.map(str::to_string), at: from_device.at });
    if !may_push(handler, store_id, doc_id).await {
        return Ok(());
    }
    // A test's fault: this device answers nobody for now, so another device
    // has only the log to read from (what a crash test checks).
    if handler.link_faults().is_some_and(|faults| faults.deaf_to_reports()) {
        return Ok(());
    }
    let Some(local) = report_of(handler, store_id, *node_id).await else { return Ok(()) };
    let Some(from) = progress.answered.owed(&progress.device, &doc, device, &local, &report) else { return Ok(()) };
    let (_, diff) = doc_diff(handler, store_id, doc_id, &from).await?;
    if !progress.parts_ok && diff.len() > MAX_CATCH_UP_BLOB {
        progress.mark_dirty(doc_id);
        progress.waiting.hold(doc_id, Side::Send, protocol::TOO_LARGE_FOR_REMOTE);
        return Ok(());
    }
    info!(
        "Vault link for store {} doc {:?}: device {} lacks part of what this device holds ({}); sending {} bytes",
        store_id,
        doc_id,
        device,
        pimble_crdt::describe_state_vector(&report.state_vector),
        diff.len()
    );
    match append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, &diff, echoes, progress).await? {
        Appended::Yes => {
            progress.advance(doc_id, &diff);
            progress.note_answered(&doc, device, &report);
        }
        Appended::Refused => progress.mark_dirty(doc_id),
    }
    Ok(())
}

/// How often a connected link asks again what a notification it never got
/// would have told it ([`recheck`]).
const RECHECK_EVERY: Duration = Duration::from_secs(60);
/// The same under a test's faults (`crate::faults`), so a run settles in seconds.
const RECHECK_EVERY_UNDER_FAULTS: Duration = Duration::from_secs(2);

/// What a lost notification would have brought, asked for again (wave 3,
/// found by the fault test). A `VaultAppended` that never reached this link
/// leaves the document behind its log until the next append to it or the
/// next reconnect, and a `VaultReported` that never came leaves another
/// device's lack unanswered just as long; a connection can last all day.
/// So every [`RECHECK_EVERY`]: the remote's listing, and every document whose
/// head moved since it was last listed and that this device has not read
/// through is read from its cursor; then every device's reports again, each
/// answered as at a connect (what was answered once is not answered again,
/// so this sends nothing that has not changed).
#[allow(clippy::too_many_arguments)]
async fn recheck(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, key_id: Uuid, link_id: &str, echoes: &Arc<EchoTracker>, progress: &mut Progress) -> anyhow::Result<()> {
    let listing = client.vault_list_docs_response(store_id).await.map_err(|e| anyhow::anyhow!("remote vaultListDocs failed: {}", e))?;
    if listing.epoch.is_some() && listing.epoch != progress.file.epoch {
        // The twin was built again: the next connect reads it from the start.
        return Err(anyhow::anyhow!("the remote's logs changed under this connection"));
    }
    let mut behind = false;
    for doc in listing.docs {
        if doc.doc_id == VaultDocId::Tree {
            continue;
        }
        let key = doc.doc_id.as_str();
        let moved = progress.listed_heads.insert(key.clone(), doc.head) != Some(doc.head);
        if moved && doc.head > progress.cursor(&doc.doc_id).applied_through() {
            info!("Vault link for store {} doc {:?}: the log holds entries this device never heard of (through {}); reading them", store_id, doc.doc_id, doc.head);
            pull_doc(handler, client, store_id, &doc.doc_id, link_id, progress).await?;
            behind = true;
        }
    }
    if behind {
        handler.schedule_repair(store_id);
    }
    if !progress.reports_ok {
        return Ok(());
    }
    let held = held_on_both(handler, store_id, progress).await;
    for batch in held.chunks(protocol::REPORTS_BATCH) {
        let reports = match client.vault_reports(store_id, batch.to_vec()).await {
            Ok(reports) => reports,
            Err(e) if e.is_method_not_found() => return Ok(()),
            Err(e) => return Err(anyhow::anyhow!("remote vaultReports failed: {}", e)),
        };
        for report in reports {
            let heard = HeardFrom { device: &report.device, account: report.account.as_deref(), at: report.at };
            answer_report(handler, client, store_id, key_id, link_id, &report.doc_id, heard, &report.blob, echoes, progress).await?;
        }
    }
    // And this device's own, where the remote's is not what it holds now (a
    // report that did not go through).
    send_reports(handler, client, store_id, key_id, held, progress).await?;
    progress.save_if_unsaved().await;
    Ok(())
}

// ── Out of step, said (docs/CONVERGENCE_CONTRACT.md section 4) ───────────

/// Why `node_id` differs from what the other devices hold, when it does
/// (`pimble_rpc::vault::out_of_step_reason`, the browser's judgement too),
/// and which kind of reason it is.
/// What this device holds counts as unsent while the remote is not known to
/// hold it (`pushed_sv`) or a send of it was refused (marked).
#[allow(clippy::too_many_arguments)]
fn reason_for(
    doc: &pimble_crdt::NodeDoc,
    heard: Option<&HashMap<String, Heard>>,
    progress: &Progress,
    doc_id: &VaultDocId,
    pushes: bool,
    names: &dyn Fn(Option<&str>) -> protocol::Whose,
    now: DateTime<Utc>,
) -> Option<(pimble_core::OutOfStepKind, String)> {
    let local = protocol::Report::of(doc);
    let doc_key = doc_id.as_str();
    let unsent = progress.is_dirty(doc_id) || pimble_crdt::state_vector_exceeds(&local.state_vector, &progress.known(doc_id)).unwrap_or(true);
    let heard = heard.into_iter().flatten().map(|(device, h)| (device.as_str(), &h.report, h.account.as_deref(), (now - h.at).num_days()));
    protocol::out_of_step_reason(&progress.device, &doc_key, &local, doc.missing().is_some(), heard, &progress.answered, unsent, pushes, names)
}

/// Judge every document again, `elapsed` after the last time, with the
/// link connected: a difference that has lasted `ServerConfig::out_of_step_after`
/// of connected time makes the document out of step (a warning in the log,
/// and the list `getStoreSync` carries), and one that closes clears it at
/// once (an info line). A document waiting parked is said with since when it
/// has waited (kept across restarts, in `sync-state.json`), and one whose
/// parked update was set aside says so for a month (section 5).
async fn judge_steps(handler: &RpcHandler, store_id: StoreId, progress: &mut Progress, elapsed: Duration) {
    let after = handler.out_of_step_after();
    let now = Utc::now();
    let people = crate::people::cached(handler, store_id).await;
    let own_account = handler.keystore().account().await.map(|account| account.user_id);
    let names = |account: Option<&str>| protocol::whose(account, own_account.as_deref(), |id| people.name_of(id, None).map(str::to_string));
    let (reasons, parked) = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        let Ok(tree) = manager.tree(store_id) else { return };
        let access = manager.store_access(store_id);
        let mut reasons: HashMap<NodeId, (pimble_core::OutOfStepKind, String)> = HashMap::new();
        // Every document held, tombstones too: what is set aside is judged
        // over all of them (`settle_parked`).
        let parked: HashMap<NodeId, Vec<u8>> = manager.doc_ids(store_id).unwrap_or_default().into_iter().filter_map(|id| tree.doc(id).and_then(|d| d.missing()).map(|m| (id, m))).collect();
        for node_id in tree.list_node_ids() {
            let Some(doc) = tree.doc(node_id) else { continue };
            let doc_id = VaultDocId::Node(node_id);
            let pushes = access.allows_write() || (access.allows_comment() && manager.is_discussion(store_id, node_id));
            if let Some(reason) = reason_for(doc, progress.heard.get(&doc_id.as_str()), progress, &doc_id, pushes, &names, now) {
                reasons.insert(node_id, reason);
            }
        }
        (reasons, parked)
    };
    // Since when each parked update has waited for what it waits for: the
    // 24 hours of section 5 count from here, across restarts.
    let gone: Vec<String> = progress.convergence.file.parked.keys().filter(|doc| NodeId::parse(doc).map_or(true, |id| !parked.contains_key(&id))).cloned().collect();
    for doc in gone {
        progress.convergence.not_waiting(&doc);
    }
    let mut waiting_since: HashMap<NodeId, DateTime<Utc>> = HashMap::new();
    for (node_id, missing) in &parked {
        waiting_since.insert(*node_id, progress.convergence.waiting_since(&VaultDocId::Node(*node_id).as_str(), missing, now));
    }

    progress.steps.retain(|node_id, candidate| {
        if reasons.contains_key(node_id) {
            return true;
        }
        if candidate.marked {
            info!("Vault link for store {} doc {}: in step again (it was out of step since {})", store_id, node_id, candidate.since.to_rfc3339());
        }
        false
    });
    for (node_id, (kind, reason)) in reasons {
        // A difference first seen now has lasted no time yet: the time since
        // the last judgment was before it was seen.
        let candidate = match progress.steps.entry(node_id) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let candidate = entry.into_mut();
                candidate.held += elapsed;
                candidate
            }
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(Candidate { since: now, held: Duration::ZERO, reason: reason.clone(), kind, marked: false }),
        };
        candidate.reason = reason;
        candidate.kind = kind;
        if candidate.held >= after && !candidate.marked {
            // Every kind is logged: diagnostics see what the note does not say.
            warn!("Vault link for store {} doc {}: out of step since {}: {}", store_id, node_id, candidate.since.to_rfc3339(), candidate.reason);
            candidate.marked = true;
        }
    }
    let mut list: std::collections::BTreeMap<String, pimble_core::OutOfStep> = std::collections::BTreeMap::new();
    for (node_id, candidate) in progress.steps.iter().filter(|(_, c)| c.marked) {
        let since = match candidate.kind {
            pimble_core::OutOfStepKind::Waiting => waiting_since.get(node_id).copied().unwrap_or(candidate.since),
            _ => candidate.since,
        };
        list.insert(node_id.to_string(), pimble_core::OutOfStep { node_id: *node_id, since, reason: candidate.reason.clone(), kind: candidate.kind });
    }
    // What was set aside, said quietly where nothing else is said of the note.
    for (doc, _) in progress.convergence.file.set_aside.iter() {
        let Ok(node_id) = NodeId::parse(doc) else { continue };
        if list.contains_key(doc) || parked.contains_key(&node_id) {
            continue;
        }
        if let Some(record) = progress.convergence.latest_set_aside(doc, now) {
            list.insert(doc.clone(), pimble_core::OutOfStep { node_id, since: record.at, reason: protocol::SET_ASIDE_REASON.to_string(), kind: pimble_core::OutOfStepKind::SetAside });
        }
    }
    progress.waiting.set_out_of_step_all(list);
}

/// Section 5, what cannot be had: every parked update that has waited
/// `ServerConfig::set_aside_after` (24 hours) with every device that reported
/// the document lacking what it waits for, or that another device's report
/// says it set aside, or that this device set aside before and has parked
/// again (read again from a log or snapshot that still carried it), is
/// dropped from the document. The log says what was set aside, the record
/// (`sync-state.json`'s `convergence`) feeds the note's Properties, this device reports
/// again at once (saying what it set aside), and a snapshot through what it
/// has read takes the entry that carried it out of the log, so no fresh
/// reader parks it again. Only with reports working: without them nobody can
/// say who holds what.
#[allow(clippy::too_many_arguments)]
async fn settle_parked(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, key_id: Uuid, link_id: &str, echoes: &Arc<EchoTracker>, progress: &mut Progress) -> anyhow::Result<()> {
    let now = Utc::now();
    let after = chrono::Duration::from_std(handler.set_aside_after()).unwrap_or_else(|_| chrono::Duration::seconds(protocol::SET_ASIDE_AFTER_SECS));
    let parked: Vec<(NodeId, Vec<u8>)> = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        let Ok(tree) = manager.tree(store_id) else { return Ok(()) };
        manager.doc_ids(store_id).unwrap_or_default().into_iter().filter_map(|id| tree.doc(id).and_then(|d| d.missing()).map(|m| (id, m))).collect()
    };
    for (node_id, missing) in parked {
        let doc_id = VaultDocId::Node(node_id);
        let doc = doc_id.as_str();
        let why = if progress.convergence.set_aside_before(&doc, &missing) {
            "set aside here before"
        } else {
            if !progress.reports_ok {
                continue;
            }
            let since = progress.convergence.waiting_since(&doc, &missing, now);
            let heard = progress.heard.get(&doc).into_iter().flatten().map(|(_, h)| (&h.report, (now - h.at).num_days()));
            match protocol::set_aside_due(&missing, since, now, after, heard) {
                protocol::SetAsideDue::Held => {
                    progress.convergence.restart(&doc, now);
                    continue;
                }
                protocol::SetAsideDue::Wait => continue,
                protocol::SetAsideDue::Waited => "it waited the whole time",
                protocol::SetAsideDue::SetAsideElsewhere => "another device set it aside",
            }
        };
        let since = progress.convergence.file.parked.get(&doc).map(|p| p.since);
        let dropped = {
            let manager = handler.store_manager_handle();
            let mut manager = manager.write().await;
            let dropped = manager.tree_mut(store_id).ok().and_then(|tree| tree.doc_mut(node_id)).and_then(|d| d.set_aside_parked());
            if dropped.is_some() {
                let _ = manager.mark_dirty(store_id, node_id);
            }
            dropped
        };
        let Some(dropped) = dropped else { continue };
        handler.schedule_content_flush(store_id);
        let whose = "another device";
        if why == "set aside here before" {
            info!("Vault link for store {} doc {}: set aside again an update this device set aside before, waiting for {} ({}'s changes)", store_id, node_id, pimble_crdt::describe_state_vector(&dropped), whose);
        } else {
            warn!(
                "Vault link for store {} doc {}: set aside an update from {} that waited since {} for changes no device that reported holds ({}): {}",
                store_id,
                node_id,
                whose,
                since.map(|s| s.to_rfc3339()).unwrap_or_else(|| "now".into()),
                pimble_crdt::describe_state_vector(&dropped),
                why
            );
            progress.convergence.record_set_aside(&doc, protocol::SetAsideRecord::new(&dropped, now));
        }
        progress.convergence.not_waiting(&doc);
        if progress.reports_ok {
            progress.report_due.insert(doc.clone(), tokio::time::Instant::now());
        }
        if let Err(e) = snapshot_after_set_aside(handler, client, store_id, key_id, link_id, &doc_id, echoes, progress).await {
            warn!("Vault link for store {} doc {}: the snapshot after setting aside failed ({}); the log still carries what was set aside", store_id, node_id, e);
        }
    }
    progress.save_if_unsaved().await;
    Ok(())
}

/// Take what was set aside out of the log: a snapshot of the document as it
/// is now, through every entry this device has read (`applied_through`: the
/// entry that carried the set-aside update is one of them, dealt with). When
/// nothing newer than the snapshot held is there to stamp, a change that
/// changes nothing (the diff from this document's own vector: no structs,
/// its delete set) is appended first, so the new snapshot replaces the old
/// one that may carry it. Only on a device that writes the document.
#[allow(clippy::too_many_arguments)]
async fn snapshot_after_set_aside(handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, key_id: Uuid, link_id: &str, doc_id: &VaultDocId, echoes: &Arc<EchoTracker>, progress: &mut Progress) -> anyhow::Result<()> {
    if !progress.keyring.remote_docs.contains(&doc_id.as_str()) || !may_push(handler, store_id, doc_id).await {
        return Ok(());
    }
    if progress.cursor(doc_id).applied_through() <= progress.snapshot_seq(doc_id) {
        let VaultDocId::Node(node_id) = doc_id else { return Ok(()) };
        let own = {
            let manager = handler.store_manager_handle();
            let manager = manager.read().await;
            manager.node_state_vector(store_id, *node_id).map_err(|e| anyhow::anyhow!("{}", e))?
        };
        let (_, nothing_new) = doc_diff(handler, store_id, doc_id, &own).await?;
        if append_blob(handler, client, store_id, doc_id.clone(), key_id, link_id, &nothing_new, echoes, progress).await? != Appended::Yes {
            return Ok(());
        }
    }
    let upto = progress.cursor(doc_id).applied_through();
    if upto > progress.snapshot_seq(doc_id) && upload_snapshot(handler, client, store_id, doc_id.clone(), key_id, upto, progress).await? {
        progress.note_snapshot(doc_id, upto);
        info!("Vault link for store {} doc {:?}: a snapshot through seq {} takes what was set aside out of the log", store_id, doc_id, upto);
    }
    Ok(())
}

/// How often a connected link judges whether its documents are in step: a
/// fifth of the window, between a tenth of a second and half a minute.
fn steps_every(after: Duration) -> Duration {
    (after / 5).clamp(Duration::from_millis(100), Duration::from_secs(30))
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
        if let Some(keys) = self.progress.keyring.docs.wraps(node) {
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
        // Only ever the data key: a blob's fall back to a scope key held
        // under that id is for reading old blobs, not for wrapping.
        let scope = self.handler.keystore().store_keys(self.store_id).await;
        self.progress.keyring.docs.data_key(self.store_id, node, dek_id, &scope)
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
        let mut merged = keys;
        if let Some(held) = self.progress.keyring.docs.wraps(node).filter(|held| held.dek_id == merged.dek_id) {
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

        let scope = self.handler.keystore().store_keys(self.store_id).await;
        let Some((dek_id, dek, keys)) = protocol::new_data_key(self.store_id, node, scope_key_ids, &scope) else {
            return Ok(Rekeyed::Waiting);
        };
        // Without the store key's wrap the owner's other devices could not
        // read what comes after.
        if !keys.wraps.iter().any(|w| w.scope_key_id == self.link_key_id) {
            return Ok(Rekeyed::Waiting);
        }

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
                self.progress.keyring.docs.hold(node, dek_id, dek);
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
            VaultDocId::Tree | VaultDocId::Blob(_) => None,
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
                progress.keyring.note_listed(&doc.doc_id, dek_id);
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
