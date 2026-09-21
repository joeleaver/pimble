//! Sharing, the owner's half (docs/NODE_DOCUMENT_CONTRACT.md section 5).
//!
//! A share is a scoped grant on the owner's own hosted store: the shared
//! documents live in one place, the hosted twin, and everyone who holds the
//! share's key edits them there directly. An owner's device is never in the
//! path of anyone's edit. What it does is hand things over, and keep them
//! handed over:
//!
//! - **The share key**, a scope key made when the node is shared, named by
//!   the [`ShareMarker`] the node carries, wrapped to the owner's own
//!   account (so their other devices hold it too) and to every member.
//! - **Wraps of data keys**: every document under the node has its data key
//!   wrapped under the share key, and a document a member created, which
//!   the hosted server holds wrapped under the share key alone, gets the
//!   store key's wrap, so the owner's other devices and web app read it.
//! - **The scope set**: which documents are under the node, published to
//!   the hosted server from this device's own tree.
//!
//! Three pieces:
//!
//! - The `cloudShare*` RPC bodies (the `impl RpcHandler` below), `Service`
//!   only, authorized by their stubs in `crate::handler`.
//! - [`Upkeep`], the hosted-server half of keeping a store's shares up. It
//!   rides the store's vault link (its connection, its keyring, its task;
//!   see `crate::vault_link`, "Share upkeep"), at every connect and a second
//!   after the tree last changed, whoever changed it. It reads the markers
//!   off the tree each time, so a marker that arrives through replication
//!   starts upkeep on that device and one that goes stops it, with nothing
//!   to keep in step.
//! - The key sweep ([`run_sweeper`]), a task of its own beside each vault
//!   link, because it talks to the accounts service and nothing else: at
//!   connect, every minute, and after an invite, every active member of a
//!   share who has no key yet is handed it. It also fetches the owner's own
//!   envelope of a share key this device lacks.
//!
//! **Nothing is hosted unless the person asked for it** (`CLAUDE.md`):
//! sharing never hosts. A store that is not hosted is refused with
//! [`NOT_HOSTED_REFUSAL`] before anything is asked of anyone.
//!
//! Only an owner's device keeps shares up: the accounts service takes
//! `PUT members` and the hosted server `setScope` from an owner and nobody
//! else, and only an owner's account holds its own envelope of a share key.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::{NodeId, ShareMarker, StoreId, StoreKind, SyncState};
use pimble_crdt::Tree;
use pimble_crypto::SymmetricKey;
use pimble_rpc::{
    encrypted_store_error, to_rpc_error, CloudShareInfoResponse, CloudShareInviteRequest, CloudShareNodeRequest, CloudShareRef, CloudShareRemoveMemberRequest,
    EmptyResponse, MemberRole, Scope, ShareInfo, ShareMember, ShareMemberStatus, StoreChangeKind, VaultDocKeys,
};
use pimble_store::{StoreError, StoreManager};
use tokio::sync::{oneshot, Notify};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::cloud::MemberView;
use crate::handler::{plain_scope, LocalChange, RpcHandler};
use crate::keystore::SignedInAccount;
use crate::principal::read_only_error;
use crate::vault_link::{LinkSession, Rekeyed, RemoteKeys};

/// What "Share..." answers on a store that is not hosted. Sharing never
/// hosts anything: only "Host on Pimble Cloud..." uploads a store (Joe,
/// 2026-09-18), and the relay that will serve an unhosted store's shares is
/// a later wave.
pub const NOT_HOSTED_REFUSAL: &str = "Sharing needs this store hosted on Pimble Cloud, or the relay, which is not built yet. Nothing was uploaded.";

/// What sharing from a store one does not own answers: a share's recipient,
/// or an editor of the whole store.
pub const ONLY_AN_OWNER_REFUSAL: &str = "Only an owner of this store can share from it. Nothing was changed.";

/// What changing a share (inviting, removing, stopping) answers on a device
/// that holds the store as a share's recipient or a reader. The accounts
/// service would refuse most of it anyway; all of it is refused here, so
/// that nothing half happens (a member may remove themself there, which is
/// leaving a share, not stopping one).
pub const NOT_YOURS_TO_CHANGE_REFUSAL: &str = "Only an owner of this store can change its shares. Nothing was changed.";

/// What stopping a share answers when Pimble Cloud cannot be reached:
/// stopping takes the members off the accounts service and the scope off
/// the hosted server, and half of that is worse than none.
pub const STOP_UNREACHABLE_REFUSAL: &str = "Pimble Cloud cannot be reached, so nothing was changed. Stop sharing again once this store is back online.";

const NOT_SIGNED_IN: &str = "no Pimble Cloud account is signed in";

/// How long after the tree last changed the upkeep waits before its pass
/// (one person's edit is several documents' updates), and how long a run of
/// changes can put it off.
const UPKEEP_DEBOUNCE: Duration = Duration::from_secs(1);
const UPKEEP_MAX_WAIT: Duration = Duration::from_secs(5);
/// How many round trips for documents' keys one pass makes before it lets
/// the link's loop turn: the pass runs in the link's task, and a folder of
/// hundreds of documents shared for the first time must not hold this
/// device's own edits up for the length of it. The pass picks up where it
/// left off (what is wrapped is remembered), and publishes once all is.
const PASS_BUDGET: usize = 64;
/// When a pass left something undone (a share key not here yet, a document
/// whose log this device has not read to its head), how soon it looks
/// again, doubling up to [`UPKEEP_MAX_RETRY`] while nothing else changes.
const UPKEEP_RETRY: Duration = Duration::from_secs(15);
const UPKEEP_MAX_RETRY: Duration = Duration::from_secs(300);
/// How often a store's key sweep runs unless the server's configuration
/// says otherwise (`ServerConfig::share_sweep_interval`).
pub(crate) const DEFAULT_SWEEP_EVERY: Duration = Duration::from_secs(60);
/// How long `cloudShareNode` waits for the pass it asked for. Past it the
/// answer says `Syncing`, and the pass still runs.
const PASS_WAIT: Duration = Duration::from_secs(30);
/// How long stopping a share waits for the link to take its scope down.
const RETIRE_WAIT: Duration = Duration::from_secs(20);
/// How long `deleteNode` lets the stopping of one share hold it up.
const STOP_BEFORE_DELETE_WAIT: Duration = Duration::from_secs(20);

/// The envelope context of a share key: what the key is for.
fn share_context(store_id: StoreId, root: NodeId) -> String {
    format!("share:{store_id}/{root}")
}

// ── Markers ──────────────────────────────────────────────────────────────

fn marker_of(tree: &Tree, id: NodeId) -> Option<ShareMarker> {
    let info = tree.get_node_info(id).ok()?;
    serde_json::from_value(info.custom.get(pimble_core::custom_keys::SHARE)?.clone()).ok()
}

/// Every live node that carries a marker, in id order.
fn markers_in(tree: &Tree) -> Vec<(NodeId, ShareMarker)> {
    let mut markers: Vec<(NodeId, ShareMarker)> = tree.list_node_ids().into_iter().filter_map(|id| Some((id, marker_of(tree, id)?))).collect();
    markers.sort_by_key(|(id, _)| id.to_string());
    markers
}

/// Whether this device holds `store_id` in a way that lets it keep shares
/// up at all: the whole store, writable. (Whether the account is an owner
/// is the accounts service's to say; see [`Upkeep::connected`].)
fn holds_whole_store(manager: &StoreManager, store_id: StoreId) -> bool {
    manager.scope_roots(store_id).is_empty() && manager.store_access(store_id).allows_write()
}

/// The share roots in `node_id`'s subtree (itself included), for
/// `deleteNode`. Empty on a device that could not stop a share anyway, and
/// for the store's root, whose deletion is refused: no share is stopped for
/// a deletion that will not happen.
pub(crate) fn shared_roots_under(manager: &StoreManager, store_id: StoreId, node_id: NodeId) -> Vec<NodeId> {
    if !holds_whole_store(manager, store_id) {
        return Vec::new();
    }
    let Ok(tree) = manager.tree(store_id) else { return Vec::new() };
    if tree.root() == node_id {
        return Vec::new();
    }
    tree.subtree_ids(node_id).unwrap_or_default().into_iter().filter(|id| marker_of(tree, *id).is_some()).collect()
}

/// Whether a marker's accounts service is the one the account is signed in
/// to. A share made under another service is not this account's to keep up.
fn same_service(marker: &ShareMarker, account: &SignedInAccount) -> bool {
    marker.url.trim_end_matches('/') == account.url.trim_end_matches('/')
}

// ── What this device knows of each share's state ─────────────────────────

/// How far the hosted-server half of a share's upkeep is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Reach {
    /// The store's vault link is down.
    #[default]
    Offline,
    /// Connected, and something is still owed: a wrap, the scope.
    Pending,
    /// Every document under the share is wrapped and the scope is published.
    Settled,
}

#[derive(Default)]
struct Status {
    reach: Reach,
    /// An active member is still without the key (the sweep hands it over).
    keys_pending: bool,
    settled_at: Option<DateTime<Utc>>,
    /// The kind of state last sent to subscribers: transitions only.
    told: Option<u8>,
}

impl Status {
    fn state(&self) -> SyncState {
        match self.reach {
            Reach::Offline => SyncState::Offline,
            Reach::Pending => SyncState::Syncing,
            Reach::Settled if self.keys_pending => SyncState::Syncing,
            Reach::Settled => SyncState::Synced { last_sync: self.settled_at.unwrap_or_else(Utc::now) },
        }
    }
}

fn state_kind(state: &SyncState) -> u8 {
    match state {
        SyncState::Offline => 0,
        SyncState::Syncing => 1,
        SyncState::Synced { .. } => 2,
        SyncState::Conflict { .. } => 3,
    }
}

/// The shares this server keeps up, as far as their state goes: what
/// `cloudShareInfo` answers and `ShareStateChanged` announces. Derived and
/// in memory only; the shares themselves are the markers in the tree.
pub struct Shares {
    sweep_every: Duration,
    statuses: Mutex<HashMap<(StoreId, NodeId), Status>>,
    /// Whether the signed-in account owns a store, as its vault link last
    /// read it at connect. The sweep asks nothing of the accounts service
    /// for a store the account does not own.
    owners: Mutex<HashMap<StoreId, bool>>,
}

impl Shares {
    pub(crate) fn new(sweep_every: Duration) -> Self {
        Self { sweep_every, statuses: Mutex::new(HashMap::new()), owners: Mutex::new(HashMap::new()) }
    }

    async fn update(&self, handler: &RpcHandler, store_id: StoreId, root: NodeId, change: impl FnOnce(&mut Status)) {
        let changed_to = {
            let mut statuses = self.statuses.lock().unwrap();
            let status = statuses.entry((store_id, root)).or_default();
            change(status);
            let state = status.state();
            let kind = state_kind(&state);
            if status.told == Some(kind) {
                None
            } else {
                status.told = Some(kind);
                Some(state)
            }
        };
        if let Some(state) = changed_to {
            info!("Share {} of store {} -> {:?}", root, store_id, state);
            handler.notify_share_state_changed(store_id, root, state).await;
        }
    }

    async fn set_reach(&self, handler: &RpcHandler, store_id: StoreId, root: NodeId, reach: Reach) {
        self.update(handler, store_id, root, |status| {
            if reach == Reach::Settled && status.reach != Reach::Settled {
                status.settled_at = Some(Utc::now());
            }
            status.reach = reach;
        })
        .await;
    }

    /// The sweep's half of a share's state. It says nothing by itself
    /// before the link's upkeep has reported on the share: at a connect the
    /// sweep may well finish first, and "offline" would be news of nothing.
    async fn set_keys_pending(&self, handler: &RpcHandler, store_id: StoreId, root: NodeId, pending: bool) {
        {
            let mut statuses = self.statuses.lock().unwrap();
            let status = statuses.entry((store_id, root)).or_default();
            if status.told.is_none() {
                status.keys_pending = pending;
                return;
            }
        }
        self.update(handler, store_id, root, |status| status.keys_pending = pending).await;
    }

    /// The share's state for an answer: what upkeep last reported, and
    /// until it has reported anything, whatever the link's state implies.
    async fn state_of(&self, handler: &RpcHandler, store_id: StoreId, root: NodeId) -> SyncState {
        if let Some(status) = self.statuses.lock().unwrap().get(&(store_id, root)) {
            return status.state();
        }
        match handler.vault_link_state(store_id).await {
            Some(SyncState::Synced { .. }) => SyncState::Syncing,
            _ => SyncState::Offline,
        }
    }

    fn roots_of(&self, store_id: StoreId) -> Vec<NodeId> {
        self.statuses.lock().unwrap().keys().filter(|(store, _)| *store == store_id).map(|(_, root)| *root).collect()
    }

    /// The share is gone (stopped, or its marker went): nothing to report on.
    fn forget(&self, store_id: StoreId, root: NodeId) {
        self.statuses.lock().unwrap().remove(&(store_id, root));
    }

    pub(crate) fn forget_store(&self, store_id: StoreId) {
        self.statuses.lock().unwrap().retain(|(store, _), _| *store != store_id);
        self.owners.lock().unwrap().remove(&store_id);
    }

    fn set_owner(&self, store_id: StoreId, owner: bool) {
        self.owners.lock().unwrap().insert(store_id, owner);
    }

    fn known_not_owner(&self, store_id: StoreId) -> bool {
        self.owners.lock().unwrap().get(&store_id) == Some(&false)
    }
}

// ── Upkeep on the vault link: scopes and wraps ───────────────────────────

/// What a `cloudShare*` RPC asks of the upkeep in the link's task.
pub(crate) enum ShareCommand {
    /// Run a pass now (a node was just shared, a share key just arrived),
    /// and say when it has run.
    PassNow(Option<oneshot::Sender<()>>),
    /// The share rooted at `root` is being stopped: stop publishing it and
    /// take its scope off the hosted server. Answers whether every document
    /// of the scope is wrapped under the store key, which is what says the
    /// share key can be dropped without losing anything a member made.
    Retire { root: NodeId, done: oneshot::Sender<Result<bool, String>> },
}

/// Resolves when `due` comes; never, without one.
pub(crate) async fn until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

/// One share as a pass sees it: its root's marker and the documents under it.
struct SharePlan {
    root: NodeId,
    marker: ShareMarker,
    /// The scope to publish, without the root (which is in its own scope
    /// whether or not the set lists it).
    docs: HashSet<NodeId>,
}

/// What this device's tree says is shared. `None` on a device that keeps
/// no shares up (a share's own recipient, a reader).
///
/// A scope is what a member scoped to the root would reach in a plain
/// store (`plain_scope`): the live subtree, and every held document whose
/// stored parent chain leads into it, tombstones above all. A deleted
/// document has to stay in the set: a recipient that was offline for the
/// delete would otherwise never be sent the tombstone, keep a live node its
/// parent's list no longer names, and its repair would list it again, for
/// the owner's repair to unlist, for ever.
async fn plan(handler: &RpcHandler, store_id: StoreId) -> Option<Vec<SharePlan>> {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    if !holds_whole_store(&manager, store_id) {
        return None;
    }
    let tree = manager.tree(store_id).ok()?;
    Some(
        markers_in(tree)
            .into_iter()
            .map(|(root, marker)| {
                let mut docs = plain_scope(tree, &[root]);
                docs.remove(&root);
                SharePlan { root, marker, docs }
            })
            .collect(),
    )
}

/// What became of bringing one document's wraps up to what is wanted.
enum Settle {
    Done,
    /// Something is owed still; `wants_key` when what is missing is a share
    /// key this device does not hold (the sweep fetches it).
    NotYet { wants_key: bool },
}

/// The state of a store's share upkeep in its vault link's task. Lives as
/// long as the link's task; what it learned of the remote is forgotten at
/// every connect.
pub(crate) struct Upkeep {
    due: Option<Instant>,
    /// When the run of changes the pending pass waits out began.
    first_noted: Option<Instant>,
    /// The sets this connection has published, so an unchanged scope is not
    /// sent again by every pass. Every connect publishes afresh: the last
    /// publisher wins, and another device may have published since.
    published: HashMap<NodeId, HashSet<NodeId>>,
    /// Roots being stopped: not published again, even though the marker is
    /// still there for a moment (the scope goes first, the marker second).
    retired: HashSet<NodeId>,
    /// Whether the account owns the store, read at connect.
    owner: Option<bool>,
    /// The roots the last pass found, to tell a marker that went.
    roots: HashSet<NodeId>,
    /// A pass that ran out of its budget goes on where it stopped: what
    /// this round has looked at already, and what it found still owed. A
    /// document is looked at once a round, so one that stays owed cannot
    /// use up every slice's budget and keep the rest from their turn.
    round: Round,
    /// How long until the next look at what a pass left owed.
    retry_wait: Duration,
}

#[derive(Default)]
struct Round {
    settled: HashSet<NodeId>,
    rekeys_tried: HashSet<NodeId>,
    unsettled: HashSet<NodeId>,
}

impl Upkeep {
    pub(crate) fn new() -> Self {
        Self { due: None, first_noted: None, published: HashMap::new(), retired: HashSet::new(), owner: None, roots: HashSet::new(), round: Round::default(), retry_wait: UPKEEP_RETRY }
    }

    pub(crate) fn due(&self) -> Option<Instant> {
        self.due
    }

    /// A connection is up and reconciled: a pass is due at once.
    pub(crate) fn connected(&mut self, handler: &RpcHandler, store_id: StoreId, owner: Option<bool>) {
        self.published.clear();
        self.retired.clear();
        self.round = Round::default();
        self.retry_wait = UPKEEP_RETRY;
        self.owner = owner;
        if let Some(owner) = owner {
            handler.shares().set_owner(store_id, owner);
        }
        self.first_noted = None;
        self.due = Some(Instant::now());
    }

    /// A pass failed: another one before long.
    pub(crate) fn retry_later(&mut self) {
        self.due = Some(Instant::now() + self.retry_wait);
        self.retry_wait = (self.retry_wait * 2).min(UPKEEP_MAX_RETRY);
    }

    /// The link dropped: every share of the store is offline with it.
    pub(crate) async fn link_down(&mut self, handler: &RpcHandler, store_id: StoreId) {
        self.due = None;
        self.first_noted = None;
        for root in handler.shares().roots_of(store_id) {
            handler.shares().set_reach(handler, store_id, root, Reach::Offline).await;
        }
    }

    /// A local change went by. One that may have changed what is under a
    /// share (structure, or metadata, where markers live) makes a pass due
    /// a second after the last of them.
    pub(crate) fn note(&mut self, store_id: StoreId, change: &LocalChange) {
        self.note_at(Instant::now(), store_id, change);
    }

    fn note_at(&mut self, now: Instant, store_id: StoreId, change: &LocalChange) {
        let LocalChange::Store(notif) = change else { return };
        if notif.store_id != store_id {
            return;
        }
        match notif.change_kind {
            StoreChangeKind::NodeCreated { .. }
            | StoreChangeKind::NodeDeleted { .. }
            | StoreChangeKind::NodeMoved { .. }
            | StoreChangeKind::MetadataUpdated { .. }
            | StoreChangeKind::TreeStructure { .. } => {}
            StoreChangeKind::ContentUpdated { .. }
            | StoreChangeKind::SyncStateChanged { .. }
            | StoreChangeKind::MountStateChanged { .. }
            | StoreChangeKind::ShareStateChanged { .. }
            | StoreChangeKind::VaultAppended { .. } => return,
        }
        self.retry_wait = UPKEEP_RETRY;
        let first = *self.first_noted.get_or_insert(now);
        let debounced = (now + UPKEEP_DEBOUNCE).min(first + UPKEEP_MAX_WAIT);
        // Never later than a pass that is due already for another reason.
        self.due = Some(self.due.map_or(debounced, |due| if due < now { due } else { debounced }));
    }

    /// One pass: for every share this device's tree holds, the wraps its
    /// documents lack, then its scope. Wraps first, so a document is
    /// readable by the time the scope says it may be fetched. An error is
    /// the connection's; one document the remote refuses is left for later.
    pub(crate) async fn run(&mut self, link: &mut LinkSession<'_>, sweep_kick: &Notify) -> anyhow::Result<()> {
        self.due = None;
        self.first_noted = None;
        if self.owner == Some(false) {
            return Ok(());
        }
        let store_id = link.store_id();
        let Some(shares) = plan(link.handler(), store_id).await else { return Ok(()) };

        // A marker that went (stopped on another device, or deleted) takes
        // its share's upkeep with it.
        let roots: HashSet<NodeId> = shares.iter().map(|share| share.root).collect();
        for gone in self.roots.difference(&roots) {
            link.handler().shares().forget(store_id, *gone);
        }
        self.published.retain(|root, _| roots.contains(root));
        self.retired.retain(|root| roots.contains(root));
        self.roots = roots;
        if shares.is_empty() {
            return Ok(());
        }

        // The share keys each document wants a wrap under: one per share it
        // is in (nested and overlapping shares are just more wraps).
        let mut wanted: HashMap<NodeId, Vec<Uuid>> = HashMap::new();
        for share in shares.iter().filter(|share| !self.retired.contains(&share.root)) {
            for doc in share.docs.iter().chain(std::iter::once(&share.root)) {
                wanted.entry(*doc).or_default().push(share.marker.key_id);
            }
        }
        let mut wanted: Vec<(NodeId, Vec<Uuid>)> = wanted.into_iter().collect();
        wanted.sort_by_key(|(id, _)| id.to_string());

        // A marker that came through replication names a key this device
        // may not hold yet: the sweep fetches the owner's own envelope of it.
        let mut wants_key = false;
        for share in &shares {
            wants_key |= link.handler().keystore().store_key(store_id, share.marker.key_id).await.is_none();
        }
        let mut out_of_budget = false;
        for (doc, share_keys) in wanted {
            if self.round.settled.contains(&doc) {
                continue;
            }
            if link.calls() >= PASS_BUDGET {
                out_of_budget = true;
                break;
            }
            if let Settle::NotYet { wants_key: key } = settle(link, doc, &share_keys, true).await? {
                self.round.unsettled.insert(doc);
                wants_key |= key;
            }
            self.round.settled.insert(doc);
        }
        // A document given a data key by an earlier pass whose snapshot is
        // still owed (see `LinkSession::give_data_key`).
        for doc in link.rekeying() {
            if out_of_budget || self.round.rekeys_tried.contains(&doc) {
                continue;
            }
            if link.calls() >= PASS_BUDGET {
                out_of_budget = true;
                break;
            }
            if !link.finish_rekey(doc).await? {
                self.round.unsettled.insert(doc);
            }
            self.round.rekeys_tried.insert(doc);
        }
        if wants_key {
            sweep_kick.notify_one();
        }
        if out_of_budget {
            // More to wrap: the loop turns once, and the pass goes on. No
            // scope is published before its documents are readable.
            for share in shares.iter().filter(|share| !self.retired.contains(&share.root)) {
                link.handler().shares().set_reach(link.handler(), store_id, share.root, Reach::Pending).await;
            }
            self.due = Some(Instant::now() + Duration::from_millis(50));
            return Ok(());
        }
        let unsettled = std::mem::take(&mut self.round).unsettled;

        for share in &shares {
            if self.retired.contains(&share.root) {
                continue;
            }
            if self.published.get(&share.root) != Some(&share.docs) {
                let mut doc_ids: Vec<NodeId> = share.docs.iter().copied().collect();
                doc_ids.sort_by_key(|id| id.to_string());
                let scope = Scope { root: share.root, doc_ids };
                match link.client().set_scope(store_id, scope, false).await {
                    Ok(()) => {
                        debug!("Share {} of store {}: published a scope of {} document(s)", share.root, store_id, share.docs.len());
                        self.published.insert(share.root, share.docs.clone());
                    }
                    // Not an owner after all (the rows could not be read at
                    // connect, or the role changed since): not this
                    // device's to keep up.
                    Err(e) if is_forbidden(&e) => {
                        info!("Store {}: the hosted server takes no scope from this account ({}); leaving its shares to their owner", store_id, e);
                        self.owner = Some(false);
                        link.handler().shares().set_owner(store_id, false);
                        return Ok(());
                    }
                    Err(e) => return Err(anyhow::anyhow!("remote setScope for {} failed: {}", share.root, e)),
                }
            }
            let settled = !share.docs.iter().chain(std::iter::once(&share.root)).any(|doc| unsettled.contains(doc));
            link.handler().shares().set_reach(link.handler(), store_id, share.root, if settled { Reach::Settled } else { Reach::Pending }).await;
        }

        if unsettled.is_empty() {
            self.retry_wait = UPKEEP_RETRY;
        } else {
            self.retry_later();
        }
        Ok(())
    }

    pub(crate) async fn command(&mut self, link: &mut LinkSession<'_>, command: ShareCommand, sweep_kick: &Notify) -> anyhow::Result<()> {
        match command {
            ShareCommand::PassNow(done) => {
                let result = self.run(link, sweep_kick).await;
                if let Some(done) = done {
                    let _ = done.send(());
                }
                result
            }
            ShareCommand::Retire { root, done } => {
                // Asked while no connection was up and given up on since:
                // the RPC told its caller nothing was changed, so nothing is.
                if done.is_closed() {
                    return Ok(());
                }
                self.retired.insert(root);
                self.published.remove(&root);
                match retire(link, root).await {
                    Ok(all_wrapped) => {
                        let _ = done.send(Ok(all_wrapped));
                        Ok(())
                    }
                    Err(e) => {
                        self.retired.remove(&root);
                        let _ = done.send(Err(e.to_string()));
                        Err(e)
                    }
                }
            }
        }
    }
}

fn is_forbidden(error: &pimble_client::ClientError) -> bool {
    error.to_string().starts_with("Forbidden: ")
}

/// Bring `doc`'s wraps up to the store key's and `share_keys`'. A document
/// the remote has never seen is the link's to create, wrapped under all of
/// them as it goes (`seal_plan`). `rekey` says whether a document from
/// before data keys is given one: it must be to come under a share, and
/// need not be for anything else.
async fn settle(link: &mut LinkSession<'_>, doc: NodeId, share_keys: &[Uuid], rekey: bool) -> anyhow::Result<Settle> {
    let mut want: Vec<Uuid> = vec![link.link_key_id()];
    for key_id in share_keys {
        if !want.contains(key_id) {
            want.push(*key_id);
        }
    }
    let keys = match link.remote_keys(doc).await? {
        RemoteKeys::Absent => return Ok(Settle::Done),
        RemoteKeys::Keys(keys) => keys,
        RemoteKeys::Keyless if !rekey => return Ok(Settle::Done),
        RemoteKeys::Keyless => match link.give_data_key(doc, &want).await? {
            Rekeyed::Done => return Ok(Settle::Done),
            Rekeyed::Waiting => return Ok(Settle::NotYet { wants_key: false }),
            Rekeyed::HasKeys(keys) => keys,
        },
    };

    let missing: Vec<Uuid> = want.into_iter().filter(|id| !keys.wraps.iter().any(|wrap| wrap.scope_key_id == *id)).collect();
    if missing.is_empty() {
        return Ok(Settle::Done);
    }
    // Opened with whichever scope key this device holds among the wraps:
    // the store key for the owner's own documents, a share's for one a
    // member created.
    let Some(dek) = link.data_key(doc, keys.dek_id).await else {
        debug!("Store {} doc {}: no wrap of its data key opens here yet", link.store_id(), doc);
        return Ok(Settle::NotYet { wants_key: true });
    };
    let store_id = link.store_id();
    let aad = pimble_crypto::dek_aad(&store_id.to_string(), &pimble_rpc::VaultDocId::Node(doc).as_str());
    let keystore = link.handler().keystore();
    let mut wraps = Vec::new();
    let mut lacking = false;
    for scope_key_id in missing {
        match keystore.store_key(store_id, scope_key_id).await {
            Some(scope_key) => wraps.push(pimble_crypto::wrap_dek(&dek, &scope_key, scope_key_id, &aad)),
            None => lacking = true,
        }
    }
    if !wraps.is_empty() && !link.add_wraps(doc, VaultDocKeys { dek_id: keys.dek_id, wraps }).await? {
        return Ok(Settle::NotYet { wants_key: false });
    }
    Ok(if lacking { Settle::NotYet { wants_key: true } } else { Settle::Done })
}

/// Take `root`'s scope off the hosted server. First, every document the
/// scope holds, by the server's own account of it (what members created
/// included) and by this device's tree, gets the store key's wrap if it
/// lacks it: once the share key is dropped, that wrap is the only way the
/// owner reads what a member made. Answers whether all of them have it.
async fn retire(link: &mut LinkSession<'_>, root: NodeId) -> anyhow::Result<bool> {
    let store_id = link.store_id();
    let scopes = link.client().get_scopes(store_id).await.map_err(|e| anyhow::anyhow!("remote getScopes failed: {}", e))?;
    let mut docs: HashSet<NodeId> = scopes.into_iter().filter(|scope| scope.root == root).flat_map(|scope| scope.doc_ids).collect();
    docs.insert(root);
    {
        let manager = link.handler().store_manager_handle();
        let manager = manager.read().await;
        if let Ok(tree) = manager.tree(store_id) {
            docs.extend(plain_scope(tree, &[root]));
        }
    }
    let mut all_wrapped = true;
    for doc in docs {
        // `Absent` here is a document the server names and this link has
        // not heard of: nothing says it is wrapped.
        let settled = match link.remote_keys(doc).await? {
            RemoteKeys::Absent => false,
            _ => matches!(settle(link, doc, &[], false).await?, Settle::Done),
        };
        all_wrapped &= settled;
    }
    link.client()
        .set_scope(store_id, Scope { root, doc_ids: Vec::new() }, true)
        .await
        .map_err(|e| anyhow::anyhow!("remote setScope (remove) for {} failed: {}", root, e))?;
    Ok(all_wrapped)
}

// ── The key sweep ────────────────────────────────────────────────────────

/// A store's key sweep, for as long as its vault link lives: at the link's
/// every connect and after an invite (`kick`), and every
/// [`Shares::sweep_every`] besides, because a member invited before they
/// had an account appears without telling anyone.
pub(crate) async fn run_sweeper(handler: RpcHandler, store_id: StoreId, kick: Arc<Notify>) {
    let every = handler.shares().sweep_every;
    loop {
        tokio::select! {
            _ = kick.notified() => {}
            _ = tokio::time::sleep(every) => {}
        }
        sweep(&handler, store_id).await;
    }
}

/// Hand the share key to every active member of each of `store_id`'s shares
/// who has none. Best effort throughout: what fails is tried again by the
/// next sweep.
async fn sweep(handler: &RpcHandler, store_id: StoreId) {
    if handler.shares().known_not_owner(store_id) {
        return;
    }
    let markers = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        if !holds_whole_store(&manager, store_id) {
            return;
        }
        match manager.tree(store_id) {
            Ok(tree) => markers_in(tree),
            Err(_) => return,
        }
    };
    if markers.is_empty() {
        return;
    }
    let Some(account) = handler.keystore().account().await else { return };

    let mut fetched_a_key = false;
    for (root, marker) in markers {
        if !same_service(&marker, &account) {
            debug!("Share {} of store {} lives on {}, not the signed-in {}; leaving it", root, store_id, marker.url, account.url);
            continue;
        }
        let held = handler.keystore().store_key(store_id, marker.key_id).await.is_some();
        let Some(key) = share_key(handler, &account, store_id, root, &marker).await else {
            debug!("Share {} of store {}: its key has not reached this device yet", root, store_id);
            continue;
        };
        fetched_a_key |= !held;

        let listing = match crate::cloud::list_members(&account.url, &account.session, &store_id.to_string(), Some(&root)).await {
            Ok(listing) => listing,
            Err(e) => {
                debug!("Share {} of store {}: could not list its members: {}", root, store_id, e);
                continue;
            }
        };
        let mut pending = false;
        for member in listing.members.iter().filter(|member| member.is_active() && !member.has_key) {
            // An owner is given every active member's public keys; without
            // them the key is not this account's to hand over.
            let (Some(user_id), Some(public_keys)) = (&member.user_id, &member.public_keys) else { continue };
            match hand_key(&account, store_id, root, &marker, &key, user_id, public_keys).await {
                Ok(()) => info!("Share {} of store {}: handed the key to {}", root, store_id, member.email),
                Err(e) => {
                    warn!("Share {} of store {}: could not hand the key to {}: {}", root, store_id, member.email, e);
                    pending = true;
                }
            }
        }
        handler.shares().set_keys_pending(handler, store_id, root, pending).await;
    }
    // A share key that arrived lets the link's upkeep wrap what it could not.
    if fetched_a_key {
        handler.share_command(store_id, ShareCommand::PassNow(None)).await;
    }
}

/// The share's key: from the keystore, or, on a device the marker reached
/// through replication, from the owner's own envelope of it.
async fn share_key(handler: &RpcHandler, account: &SignedInAccount, store_id: StoreId, root: NodeId, marker: &ShareMarker) -> Option<SymmetricKey> {
    if let Some(key) = handler.keystore().store_key(store_id, marker.key_id).await {
        return Some(key);
    }
    if let Err(e) = crate::vault_link::fetch_scope_keys(handler, account, store_id, &[root]).await {
        debug!("Share {} of store {}: could not fetch its key: {}", root, store_id, e);
    }
    handler.keystore().store_key(store_id, marker.key_id).await
}

/// Wrap the share key to one member and upload the envelope.
async fn hand_key(
    account: &SignedInAccount,
    store_id: StoreId,
    root: NodeId,
    marker: &ShareMarker,
    key: &SymmetricKey,
    user_id: &str,
    public_keys: &pimble_crypto::AccountPublicKeys,
) -> anyhow::Result<()> {
    let envelope = pimble_crypto::wrap_key(key, marker.key_id, public_keys, &account.keys, &share_context(store_id, root))?;
    crate::cloud::put_store_key(&account.url, &account.session, &store_id.to_string(), user_id, marker.key_id, &envelope, Some(&root)).await?;
    Ok(())
}

// ── The cloudShare* RPCs ─────────────────────────────────────────────────

fn member_row(member: &MemberView) -> Option<ShareMember> {
    let status = if !member.is_active() {
        ShareMemberStatus::Invited
    } else if !member.has_key {
        ShareMemberStatus::WaitingForKey
    } else {
        ShareMemberStatus::Active
    };
    // A role this build does not know is not shown as one it does.
    Some(ShareMember { email: member.email.clone(), role: MemberRole::parse(&member.role)?, status })
}

impl RpcHandler {
    async fn signed_in(&self) -> Result<SignedInAccount, ErrorObjectOwned> {
        self.keystore().account().await.ok_or_else(|| to_rpc_error(NOT_SIGNED_IN))
    }

    /// The marker `node_id` carries; an error for a node that is not shared.
    async fn marker_on(&self, store_id: StoreId, node_id: NodeId) -> Result<ShareMarker, ErrorObjectOwned> {
        let manager = self.store_manager_handle();
        let manager = manager.read().await;
        let tree = manager.tree(store_id).map_err(to_rpc_error)?;
        tree.get_node_info(node_id).map_err(|_| to_rpc_error(StoreError::NodeNotFound(node_id)))?;
        marker_of(tree, node_id).ok_or_else(|| to_rpc_error(format!("node {} is not shared", node_id)))
    }

    /// Refuse, before anything is asked of Pimble Cloud, a change to a
    /// share from a device that holds the store as anything less than the
    /// whole of it, writable.
    async fn require_owners_device(&self, store_id: StoreId) -> Result<(), ErrorObjectOwned> {
        let manager = self.store_manager_handle();
        let manager = manager.read().await;
        if !manager.is_open(store_id) {
            return Err(to_rpc_error(StoreError::NotOpen(store_id)));
        }
        if !holds_whole_store(&manager, store_id) {
            return Err(to_rpc_error(NOT_YOURS_TO_CHANGE_REFUSAL));
        }
        Ok(())
    }

    /// The share and everyone on it: the owner, whose whole-store grant no
    /// scope's listing names, then the scope's members and invitations as
    /// the accounts service reports them now. The owner is the signed-in
    /// account, or on a device that holds the store as someone else's
    /// (a recipient looking at the share they are in), whoever shared it.
    async fn share_answer(&self, account: &SignedInAccount, store_id: StoreId, node_id: NodeId, marker: &ShareMarker) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        let listing = crate::cloud::list_members(&account.url, &account.session, &store_id.to_string(), Some(&node_id)).await.map_err(to_rpc_error)?;
        let shared_by = self.store_manager_handle().read().await.get_store_info(store_id).ok().and_then(|store| store.shared_by);
        let owner = shared_by.unwrap_or_else(|| account.email.clone());
        let mut members = vec![ShareMember { email: owner.clone(), role: MemberRole::Owner, status: ShareMemberStatus::Active }];
        members.extend(listing.members.iter().filter(|member| !member.email.eq_ignore_ascii_case(&owner)).filter_map(member_row));
        // Someone looking at a member who waits for the key should not have
        // to wait for the sweep's next tick as well.
        if members.iter().any(|member| member.status == ShareMemberStatus::WaitingForKey) {
            self.kick_share_sweep(store_id).await;
        }
        let state = self.shares().state_of(self, store_id, node_id).await;
        Ok(CloudShareInfoResponse { share: ShareInfo { store_id, node_id, name: marker.name.clone(), state }, members })
    }

    pub(crate) async fn share_node(&self, request: CloudShareNodeRequest) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        let (store_id, node_id) = (request.store_id, request.node_id);
        info!("Sharing node {} of store {}", node_id, store_id);

        match self.store_manager_handle().read().await.store_kind(store_id) {
            Some(StoreKind::Plain) => {}
            Some(StoreKind::Vault) => return Err(encrypted_store_error(format!("store {} is encrypted; use the vault API", store_id))),
            None => return Err(to_rpc_error(StoreError::NotOpen(store_id))),
        }
        // Before anything is asked of anyone: sharing never hosts, and a
        // store the person has not hosted stays entirely on this machine.
        if self.sync_mode_of(store_id).await != StoreKind::Vault {
            return Err(to_rpc_error(NOT_HOSTED_REFUSAL));
        }
        let account = self.signed_in().await?;
        let name = request.name.trim();
        if name.is_empty() {
            return Err(to_rpc_error("A share needs a name."));
        }
        {
            let manager = self.store_manager_handle();
            let manager = manager.read().await;
            if !manager.store_access(store_id).allows_write() {
                return Err(read_only_error());
            }
            if !manager.scope_roots(store_id).is_empty() {
                return Err(to_rpc_error(ONLY_AN_OWNER_REFUSAL));
            }
            let tree = manager.tree(store_id).map_err(to_rpc_error)?;
            let info = tree.get_node_info(node_id).map_err(|_| to_rpc_error(StoreError::NodeNotFound(node_id)))?;
            if info.node_type == pimble_core::node_types::MOUNT {
                return Err(to_rpc_error("A mount cannot be shared. Share the node it shows, in the store it lives in."));
            }
            if info.custom.contains_key(pimble_core::custom_keys::SHARE) {
                return Err(to_rpc_error("This node is shared already."));
            }
        }
        // An editor of the whole store holds it as this device does, and
        // would get as far as a marker nobody can publish a scope for.
        let rows = crate::cloud::list_stores(&account.url, &account.session).await.map_err(to_rpc_error)?;
        if !crate::cloud::is_owner_of(&rows, store_id) {
            return Err(to_rpc_error(ONLY_AN_OWNER_REFUSAL));
        }

        // The share key, to the owner's own account first: it is how their
        // other devices come to hold it, and if Pimble Cloud cannot take it
        // nothing has been changed yet.
        let key = SymmetricKey::generate();
        let key_id = Uuid::new_v4();
        let envelope = pimble_crypto::wrap_key(&key, key_id, &account.keys.public_keys(), &account.keys, &share_context(store_id, node_id)).map_err(to_rpc_error)?;
        crate::cloud::put_store_key(&account.url, &account.session, &store_id.to_string(), &account.user_id, key_id, &envelope, Some(&node_id))
            .await
            .map_err(to_rpc_error)?;
        self.keystore().add_store_key(store_id, key_id, &key).await.map_err(to_rpc_error)?;

        let marker = ShareMarker { v: ShareMarker::VERSION, key_id, url: account.url.clone(), name: name.to_string() };
        self.edit_node_metadata(store_id, node_id, |metadata| metadata.set_share(Some(&marker))).await?;

        // The wraps and the scope are the link's upkeep's, which the marker
        // has just made due; asked for now and waited for, so the answer
        // says where the share stands. With the link down it all happens at
        // the next connect, and the answer says `Offline`.
        if matches!(self.vault_link_state(store_id).await, Some(SyncState::Synced { .. })) {
            let (done, ran) = oneshot::channel();
            if self.share_command(store_id, ShareCommand::PassNow(Some(done))).await {
                let _ = tokio::time::timeout(PASS_WAIT, ran).await;
            }
        }
        // The share is made; a listing that fails now must not say otherwise.
        match self.share_answer(&account, store_id, node_id, &marker).await {
            Ok(answer) => Ok(answer),
            Err(e) => {
                warn!("Share {} of store {}: made, but its members could not be listed: {}", node_id, store_id, e.message());
                let state = self.shares().state_of(self, store_id, node_id).await;
                Ok(CloudShareInfoResponse {
                    share: ShareInfo { store_id, node_id, name: marker.name.clone(), state },
                    members: vec![ShareMember { email: account.email.clone(), role: MemberRole::Owner, status: ShareMemberStatus::Active }],
                })
            }
        }
    }

    pub(crate) async fn share_info(&self, request: CloudShareRef) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        let account = self.signed_in().await?;
        let marker = self.marker_on(request.store_id, request.node_id).await?;
        self.share_answer(&account, request.store_id, request.node_id, &marker).await
    }

    pub(crate) async fn share_invite(&self, request: CloudShareInviteRequest) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        let (store_id, node_id) = (request.store_id, request.node_id);
        if request.role == MemberRole::Owner {
            return Err(to_rpc_error("A share's members are editors or readers; owner is a role on the whole store."));
        }
        self.require_owners_device(store_id).await?;
        let account = self.signed_in().await?;
        let marker = self.marker_on(store_id, node_id).await?;
        let email = request.email.trim();
        if email.eq_ignore_ascii_case(&account.email) {
            return Err(to_rpc_error("This is your own store; there is nothing to invite yourself to."));
        }
        info!("Inviting {} to share {} of store {} as {}", email, node_id, store_id, request.role.as_str());

        let member = crate::cloud::put_member(&account.url, &account.session, &store_id.to_string(), email, request.role.as_str(), Some(&node_id), Some(&marker.name))
            .await
            .map_err(to_rpc_error)?;
        // An address that has an account already gets the key at once. One
        // that does not is the sweep's, the moment the account appears.
        if member.is_active() && !member.has_key {
            if let (Some(user_id), Some(public_keys)) = (&member.user_id, &member.public_keys) {
                match share_key(self, &account, store_id, node_id, &marker).await {
                    Some(key) => {
                        if let Err(e) = hand_key(&account, store_id, node_id, &marker, &key, user_id, public_keys).await {
                            warn!("Share {} of store {}: could not hand the key to {} yet ({}); the sweep tries again", node_id, store_id, member.email, e);
                        }
                    }
                    None => warn!("Share {} of store {}: this device does not hold its key; {} waits for a device that does", node_id, store_id, member.email),
                }
            }
        }
        self.kick_share_sweep(store_id).await;
        self.share_answer(&account, store_id, node_id, &marker).await
    }

    pub(crate) async fn share_remove_member(&self, request: CloudShareRemoveMemberRequest) -> Result<CloudShareInfoResponse, ErrorObjectOwned> {
        let (store_id, node_id) = (request.store_id, request.node_id);
        self.require_owners_device(store_id).await?;
        let account = self.signed_in().await?;
        let marker = self.marker_on(store_id, node_id).await?;
        let email = request.email.trim();
        info!("Removing {} from share {} of store {}", email, node_id, store_id);

        let store = store_id.to_string();
        let listing = crate::cloud::list_members(&account.url, &account.session, &store, Some(&node_id)).await.map_err(to_rpc_error)?;
        let member = listing
            .members
            .iter()
            .find(|member| member.email.eq_ignore_ascii_case(email))
            .ok_or_else(|| to_rpc_error(format!("{} is not a member of this share and has no invitation to it", email)))?;
        match &member.user_id {
            Some(user_id) => crate::cloud::delete_member(&account.url, &account.session, &store, user_id, Some(&node_id)).await,
            None => crate::cloud::delete_invitation(&account.url, &account.session, &store, &member.email, Some(&node_id)).await,
        }
        .map_err(to_rpc_error)?;
        self.share_answer(&account, store_id, node_id, &marker).await
    }

    pub(crate) async fn stop_sharing(&self, request: CloudShareRef) -> Result<EmptyResponse, ErrorObjectOwned> {
        info!("Stopping share {} of store {}", request.node_id, request.store_id);
        self.stop_share(request.store_id, request.node_id).await.map_err(to_rpc_error)?;
        Ok(EmptyResponse {})
    }

    /// Stop the share rooted at `node_id`: its members and invitations off
    /// the accounts service, its scope off the hosted server, its marker
    /// off the node, its key out of the keystore. The documents stay
    /// exactly where they are: nothing is deleted on Pimble Cloud, and the
    /// store is still hosted, because the person hosted it.
    ///
    /// Both ends have to be reachable before anything is changed. Each step
    /// after that leaves a share that can be stopped again: members first
    /// (nobody is minted the root any more), then the scope (a token minted
    /// before that lives an hour; without the scope it reaches nothing),
    /// then the marker.
    async fn stop_share(&self, store_id: StoreId, node_id: NodeId) -> Result<(), String> {
        self.require_owners_device(store_id).await.map_err(|e| e.message().to_string())?;
        let account = self.keystore().account().await.ok_or_else(|| NOT_SIGNED_IN.to_string())?;
        let marker = self.marker_on(store_id, node_id).await.map_err(|e| e.message().to_string())?;
        if !matches!(self.vault_link_state(store_id).await, Some(SyncState::Synced { .. })) {
            return Err(STOP_UNREACHABLE_REFUSAL.to_string());
        }
        let store = store_id.to_string();
        let listing = crate::cloud::list_members(&account.url, &account.session, &store, Some(&node_id))
            .await
            .map_err(|e| format!("{STOP_UNREACHABLE_REFUSAL} ({e})"))?;

        // The owner's own grant is on the whole store and no scope's
        // listing names it; were one to, it is not a share's to remove.
        for member in listing.members.iter().filter(|member| member.user_id.as_deref() != Some(account.user_id.as_str())) {
            match &member.user_id {
                Some(user_id) => crate::cloud::delete_member(&account.url, &account.session, &store, user_id, Some(&node_id)).await,
                None => crate::cloud::delete_invitation(&account.url, &account.session, &store, &member.email, Some(&node_id)).await,
            }
            .map_err(|e| format!("{} could not be removed from the share ({}). The share is still in place; stop it again to finish.", member.email, e))?;
        }

        let (done, retired) = oneshot::channel();
        if !self.share_command(store_id, ShareCommand::Retire { root: node_id, done }).await {
            return Err(STOP_UNREACHABLE_REFUSAL.to_string());
        }
        let all_wrapped = match tokio::time::timeout(RETIRE_WAIT, retired).await {
            Ok(Ok(Ok(all_wrapped))) => all_wrapped,
            Ok(Ok(Err(e))) => return Err(format!("The share's members were removed, but its scope could not be taken off Pimble Cloud ({e}). Stop sharing again to finish.")),
            _ => return Err("The share's members were removed, but Pimble Cloud did not answer about its scope. Stop sharing again to finish.".to_string()),
        };

        self.edit_node_metadata(store_id, node_id, |metadata| metadata.set_share(None)).await.map_err(|e| e.message().to_string())?;
        if all_wrapped {
            if let Err(e) = self.keystore().remove_store_key(store_id, marker.key_id).await {
                warn!("Share {} of store {}: could not drop its key from the keystore: {}", node_id, store_id, e);
            }
        } else {
            // Something a member made is wrapped under the share key alone
            // (it arrived as the share was stopped): the key is what reads it.
            info!("Share {} of store {}: keeping its key, a document of it is not wrapped under the store key", node_id, store_id);
        }
        self.shares().forget(store_id, node_id);
        info!("Stopped share {} (\"{}\") of store {}", node_id, marker.name, store_id);
        Ok(())
    }

    /// `deleteNode`'s hook: stop the shares rooted in the subtree first,
    /// best effort. Signed out, offline, or slow, the deletion goes ahead,
    /// and the log says which share's grants were left on the account.
    pub(crate) async fn stop_shares_before_delete(&self, store_id: StoreId, deleting: NodeId, roots: Vec<NodeId>) {
        for root in roots {
            let name = self.marker_on(store_id, root).await.map(|marker| marker.name).unwrap_or_default();
            let stopped = match tokio::time::timeout(STOP_BEFORE_DELETE_WAIT, self.stop_share(store_id, root)).await {
                Ok(result) => result,
                Err(_) => Err("Pimble Cloud did not answer in time".to_string()),
            };
            if let Err(e) = stopped {
                warn!(
                    "Deleting node {} of store {}: the share \"{}\" rooted at {} could not be stopped first ({}); its members' grants are left on the account",
                    deleting, store_id, name, root, e
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_rpc::StoreChangedNotification;

    fn member(status: &str, has_key: bool, role: &str) -> MemberView {
        MemberView { user_id: None, email: "m@example.com".into(), role: role.into(), status: status.into(), has_key, public_keys: None }
    }

    #[test]
    fn a_member_is_invited_waiting_for_the_key_or_active() {
        assert_eq!(member_row(&member("invited", false, "editor")).unwrap().status, ShareMemberStatus::Invited);
        assert_eq!(member_row(&member("active", false, "reader")).unwrap().status, ShareMemberStatus::WaitingForKey);
        let active = member_row(&member("active", true, "reader")).unwrap();
        assert_eq!((active.status, active.role), (ShareMemberStatus::Active, MemberRole::Reader));
        assert!(member_row(&member("active", true, "contributor")).is_none(), "a role this build does not know is not shown as one it does");
    }

    #[test]
    fn a_shares_state_is_the_links_then_the_scopes_then_the_keys() {
        let mut status = Status::default();
        assert!(matches!(status.state(), SyncState::Offline));
        status.reach = Reach::Pending;
        assert!(matches!(status.state(), SyncState::Syncing));
        status.reach = Reach::Settled;
        assert!(matches!(status.state(), SyncState::Synced { .. }));
        status.keys_pending = true;
        assert!(matches!(status.state(), SyncState::Syncing), "a member still without the key");
        status.reach = Reach::Offline;
        assert!(matches!(status.state(), SyncState::Offline));
    }

    fn change(store_id: StoreId, kind: StoreChangeKind) -> LocalChange {
        LocalChange::Store(StoreChangedNotification { store_id, change_kind: kind, source_client_id: Some("vault-link:x".into()), update: None })
    }

    #[tokio::test]
    async fn a_tree_change_makes_a_pass_due_a_second_after_the_last_one_and_no_later_than_five() {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        let mut upkeep = Upkeep::new();
        assert!(upkeep.due().is_none());

        // Text is not the tree's business, nor is another store's tree this one's.
        let start = Instant::now();
        upkeep.note_at(start, store_id, &change(store_id, StoreChangeKind::ContentUpdated { node_id }));
        upkeep.note_at(start, StoreId::new(), &change(store_id, StoreChangeKind::MetadataUpdated { node_id }));
        assert!(upkeep.due().is_none());

        // Whoever made it: a link's own apply (a member's create) counts.
        upkeep.note_at(start, store_id, &change(store_id, StoreChangeKind::NodeCreated { node_id, parent_id: node_id }));
        assert_eq!(upkeep.due(), Some(start + UPKEEP_DEBOUNCE));

        // A run of changes puts it off, up to a point.
        for i in 1..=10 {
            upkeep.note_at(start + Duration::from_millis(800) * i, store_id, &change(store_id, StoreChangeKind::TreeStructure { node_ids: vec![node_id] }));
            assert!(upkeep.due() <= Some(start + UPKEEP_MAX_WAIT));
        }
        assert_eq!(upkeep.due(), Some(start + UPKEEP_MAX_WAIT));
    }
}
