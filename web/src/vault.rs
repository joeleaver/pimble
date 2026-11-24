//! The vault client: an encrypted store, driven from the browser.
//!
//! A vault store holds nothing the server can read. It keeps an append-only log
//! of opaque blobs per document, and everything that makes those blobs a tree of
//! notes happens here. Every node is one yrs document in this page's memory
//! (`pimble_crdt::NodeDoc`: its text, its place in the tree and its metadata in
//! one, docs/NODE_DOCUMENT_CONTRACT.md section 1), built by decrypting what
//! its log holds; the tree is `pimble_crdt::Tree` over those documents
//! (section 2), starting from the root the store's manifest names; and every
//! local change is encrypted before it is appended (docs/CRYPTO_CONTRACT.md,
//! "Web app"). There is no tree document any more. A hosted twin from before
//! may still list one, which nobody reads.
//!
//! It sits in front of `pimble_app::commands::process_command`. A command for a
//! vault store is answered from these documents and never reaches the plain
//! RPCs — which the server would refuse anyway (`-32005 encrypted_store`). A
//! command for a plain store is handed straight back, so today's path is
//! untouched.
//!
//! What the app never learns is that any of this happened: it sends the same
//! [`BackendCommand`]s and receives the same [`BackendEvent`]s, including the
//! collaboration pair (`BroadcastChanges` out, `RemoteChanges` in) that the
//! editor is wired to, and the same store-change kinds the server derives
//! from a merged update (`pimble_crdt::derive_changes`, the server's own).
//!
//! One way to write node content, and one way to write a node's place: a
//! document changes only through `NodeDoc::apply_update` (a peer's update, or
//! the editor's own delta) and through the `Tree` operations, whose
//! `TreeEdit` says exactly what to append.
//!
//! **A share is a scope of somebody else's store** (docs/NODE_DOCUMENT_CONTRACT.md
//! section 5). The account's store list is one row per grant, so one store can
//! reach this page as several shares; what it holds of such a store is only the
//! documents the server lists, each shared root being a root of the `Tree` whose
//! `parent_id` names a document this page will never hold. That is not an
//! orphan, so repair is done one scope at a time (see
//! [`VaultStore::repair_now`]: `pimble_crdt::Tree::repair_held`, the repair a
//! desktop's partial replica makes).
//!
//! **Keys** are a scope key per scope and a data key per document
//! ([`StoreKeys`]): a blob's header names its document's data key, which the
//! server hands over wrapped under each scope key that may read it. A document
//! this page creates gets a fresh data key wrapped under every scope key it
//! holds that covers the node.
//!
//! **The shares in a whole store.** An account that holds the whole store holds
//! the key of every share in it too, as its desktops do (`refresh_grant` in
//! `crates/pimble-server/src/vault_link.rs`). A share's member wraps a new
//! document's data key under the share's key and nothing else, and the store
//! key's wrap is added later by one of the owner's desktops; a page holding
//! only the store key could not read what members made while those were off
//! (found 2026-09-21). The shares are the documents whose `node` root carries a
//! share's marker, and the device that made a share sealed its key to the
//! owner's own account, so `GET /stores/{id}/keys?root=<node>` answers it.
//! Their keys are asked for at open, at every connect, and on the live path
//! when a marker arrives or a blob will not open (see [`KeyLook`]). A member's
//! page asks for its grants' keys and nothing else, as before, and no page
//! wraps anybody else's data key: that upkeep stays the desktops'.
//!
//! **A store served from its owner's computer** (docs/RELAY_CONTRACT.md) is
//! the same thing through another endpoint: the twin this page reads is on the
//! owner's machine and Pimble Cloud's relay pipes the connection to it. Two
//! things differ. The twin is disposable, so its logs can start again from 1
//! under a new [`epoch`](pimble_rpc::VaultListDocsResponse::epoch), and a page
//! that kept its cursors would skip them ([`VaultStore::take_epoch`]). And the
//! endpoint is down whenever the owner's computer is, which is an ordinary
//! state and not an error: the backend loop says so
//! ([`VaultClient::endpoint_down`]), the store's row reads `owner offline`,
//! and until it is back this page changes nothing of it. A store it never
//! opened is listed from the account's own row of it and opens empty.
//!
//! **State reports** (docs/CONVERGENCE_CONTRACT.md, wave 1), as the desktop's
//! link makes them: the page says, sealed, what it holds of each document
//! (`vaultReport`, after every open and connect, and 5 s after the last change
//! to a document), reads what every other device says (`vaultReports` then,
//! `VaultReported` live), and appends the diff from any report that lacks
//! something this page holds ([`VaultStore::answer_report`],
//! `pimble_rpc::vault::Comparing`). The page's device id is kept in
//! `localStorage`, so a reload reports into the same slot and never answers
//! its own last report as another device's.

mod account;
mod catch_up;
mod commands;
mod helpers;
mod live;
mod outgoing;
mod pictures;
mod reads;
mod reports;
mod shares;
mod store;
mod store_keys;
mod store_reads;
mod stores;
mod watch;
mod wire;
mod writes;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use crossbeam_channel::{unbounded, Receiver, Sender};
use pimble_app::protocol::{BackendCommand, BackendEvent};
use pimble_api::ApiError;
use pimble_core::{BlobId, NodeId, Store, StoreId};
use pimble_crdt::{NodeUpdateEffect, Tree};
use pimble_crypto::KeyId;
use pimble_rpc::vault::{self as protocol, DocKeys, Mark, NewDocument, PartBuffer, Sealed};
use pimble_rpc::{StoreChangeKind, StoreChangedNotification, VaultCursor, VaultDocId, VaultDocKeys};

use crate::keys::Keyring;

pub use account::{shares_ended, AccountGrant, AccountStore};
pub use helpers::store_id_of;
pub use wire::VaultWire;
use helpers::device_id;

/// How long a merged structural update waits for the next one before the tree
/// is repaired, the server's number (`REPAIR_DEBOUNCE` in
/// `crates/pimble-server/src/handler.rs`). One peer's tree edit is several
/// documents' updates and they arrive one at a time; a repair run between two
/// of them judges a half-applied edit, and its answer to a half-applied move
/// is a new `parent_id` written into the node's document, which then
/// propagates. So no repair runs per update; it runs once they have stopped.
const REPAIR_DEBOUNCE_MS: f64 = 250.0;

/// How many documents' logs are fetched at once. One socket carries them all
/// and jsonrpsee caps the requests in flight on it, so a store of hundreds of
/// nodes is pulled in windows rather than one request per round trip.
const FETCH_WINDOW: usize = 32;

/// How long before a share whose key has not arrived is asked for again. A
/// fresh share waits on one of its owner's devices coming online, which is
/// minutes rather than seconds; often enough to feel live, seldom enough to be
/// nothing on a tab left open. An ask for the keys of the shares in a whole
/// store that failed waits as long.
const KEY_RETRY_MS: f64 = 30_000.0;

/// How long a look for keys (see [`KeyLook`]) waits for whatever else is
/// arriving: one peer's edit is several documents' updates, and one look
/// answers for all of them.
const KEY_LOOK_DEBOUNCE_MS: f64 = 250.0;

/// The least time between two asks of the accounts service for the keys of one
/// store's shares, however many markers and unopened blobs want them.
const SHARE_KEY_FLOOR_MS: f64 = 10_000.0;

/// What a store's name gains while its key has not reached this account: the
/// row is listed, because the grant is real, and says why it will not open.
const WAITING_SUFFIX: &str = " (waiting for the key)";

/// What a command names a store this page has not opened (yet): its key may
/// still be on its way, or its owner's computer off.
fn not_open() -> ApiError {
    ApiError::not_found("That encrypted store is not open in this page.")
}

/// What [`VaultClient::handle`] decided about a command.
pub enum Handled {
    /// Not for a vault store. The command comes back untouched.
    No(BackendCommand),
    /// Answered here. The event, if there is one, is the command's reply.
    Yes(Option<BackendEvent>),
}

/// What this page knows about one node document's log. The document itself
/// lives in the store's `Tree`.
struct VaultDoc {
    /// How far into the log the document has been read without a gap: where
    /// the next fetch starts, and the only number a snapshot may be stamped
    /// with (see `pimble_rpc::VaultCursor`).
    cursor: VaultCursor,
    /// The document's snapshot as last known here (fetched, or uploaded from
    /// here): what `pimble_rpc::vault::snapshot_due` counts the log from.
    snapshot_seq: u64,
    /// A state vector everything up to which the server is known to hold
    /// (see `advance`). What a resend after a failed append is a diff against.
    known_sv: Vec<u8>,
    /// An append failed: the document holds work the server does not. Every
    /// later edit depends on it, so until it is resent no other device can
    /// show anything this one wrote.
    unsent: bool,
}

impl Default for VaultDoc {
    fn default() -> Self {
        Self {
            cursor: VaultCursor::default(),
            snapshot_seq: 0,
            known_sv: pimble_crdt::empty_state_vector(),
            unsent: false,
        }
    }
}

impl VaultDoc {
    /// A fetched entry was applied: what it vouches for in the cursor, and a
    /// snapshot's number.
    fn applied(&mut self, mark: Mark) {
        mark.apply(&mut self.cursor);
        if let Some(seq) = mark.snapshot() {
            self.snapshot_seq = self.snapshot_seq.max(seq);
        }
    }
}

/// The keys of one encrypted store: the scope keys this account holds, and
/// every document's data key as blobs have asked for them.
///
/// Resolution order for a blob whose header names `key_id`
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Keys"):
///
/// 1. the document's data key, already unwrapped here;
/// 2. the document's wraps as `vaultFetch` handed them over, unwrapped with
///    whichever scope key this page holds;
/// 3. a scope key of that id held directly — a blob from before data keys,
///    whose header names the store key itself.
///
/// The order and the rest of the protocol are `pimble_rpc::vault`'s, shared
/// with the desktop's vault link.
struct StoreKeys {
    /// The scope keys: the store key, or each share's, or both.
    scope: Keyring,
    /// The documents' data keys and wraps.
    docs: DocKeys,
}

/// One encrypted store.
struct VaultStore {
    /// The store as the server listed it: what the UI is told again when the
    /// documents turn out to name another root than the manifest does.
    listed: Store,
    keys: StoreKeys,
    /// The roots this page holds of the store: empty for a whole store, the
    /// shared roots for a scoped member (`Store::roots`). A scope root's
    /// `parent_id` names a document this page never holds.
    scope_roots: Vec<NodeId>,
    /// Every document this page holds, and the tree over them.
    tree: Tree,
    /// The log bookkeeping for each held document.
    docs: HashMap<NodeId, VaultDoc>,
    /// Each document's head as the server last reported it, so a document
    /// nothing was appended to is never fetched again. A document the server
    /// is known to have at all, which is what decides whether an append
    /// creates one.
    heads: HashMap<NodeId, u64>,
    /// When the debounced repair is due (the page's clock, ms), once a
    /// merged update has touched structure.
    repair_due: Option<f64>,
    /// What this page is still owed to open what it holds of the store.
    look: KeyLook,
    /// Which logs the cursors above are about: `vaultListDocs`'s `epoch` as
    /// last read (see [`VaultStore::take_epoch`]). `None` for a server that
    /// names none.
    epoch: Option<String>,
    /// What changed in its documents since the vault last said so to whoever
    /// listens through the API ([`VaultClient::watch`]): a plugin's `Pimble`,
    /// which holds copies of its own (docs/PLUGINS_CONTRACT.md wave 3).
    heard: Vec<pimble_api::StoreChange>,
    /// Whether the server takes snapshots and appends in parts, as its
    /// `vaultListDocs` last said (docs/SYNC_LIMITS_CONTRACT.md): an older one
    /// is sent whole blobs only.
    parts_ok: bool,
    /// The parts of groups announced live and not all here yet.
    parts: PartBuffer,
    /// The documents this page is holding back, each way, with the sentence
    /// that says why (docs/SYNC_LIMITS_CONTRACT.md, "Saying what waits"), as
    /// the desktop's link keeps them; and whether that changed since the UI
    /// was last told.
    held_back: HashMap<(NodeId, Side), String>,
    held_changed: bool,
    /// Whether the server takes state reports, as this connection found
    /// (docs/CONVERGENCE_CONTRACT.md wave 1).
    reports_ok: bool,
    /// When each document's report is due (the page's clock, ms): a debounce
    /// after the last change to it.
    report_due: HashMap<NodeId, f64>,
    /// What the server holds of this page's reports, as this connection has
    /// seen it: a report that says the same again is not sent.
    reported: HashMap<NodeId, Vec<u8>>,
    /// Which other devices' reports this page has answered, at which vector
    /// (kept in `localStorage`, as the desktop keeps it in `devices.json`).
    answered: protocol::Comparing,
    /// Other devices' reports heard live and not answered yet: the document,
    /// the device, the sealed report.
    reports_in: Vec<(NodeId, String, String, Option<String>)>,
    /// What the other devices last said they hold of each document (document
    /// -> device -> the report, its account, when it was stored, ms), as this
    /// page has heard it: what says whether the document is in step
    /// (docs/CONVERGENCE_CONTRACT.md section 4).
    reports_heard: HashMap<NodeId, HashMap<String, (protocol::Report, Option<String>, f64)>>,
    /// Who is who in the store's shares (account id -> the name they go by),
    /// asked of the accounts service every [`PEOPLE_EVERY_MS`], so a note out
    /// of step names whose device holds what ("Will's device"), as the
    /// desktop's does. When it was last asked, ms.
    people: HashMap<String, String>,
    people_at: Option<f64>,
    /// The documents that differ now: since when (ms), for how long while
    /// connected (ms), why, and whether that has been said.
    steps: HashMap<NodeId, StepCandidate>,
    /// The documents out of step on this page, said in the store's sync
    /// answer (`sync_event`) like what waits.
    out_of_step: HashMap<NodeId, pimble_core::OutOfStep>,
    /// When the documents were last judged (the page's clock, ms).
    steps_judged_at: Option<f64>,
    /// What each document's parked update waits for and since when this page
    /// first saw it wait for that (ms), for the quiet line in its Properties
    /// (docs/CONVERGENCE_CONTRACT.md section 5). The page's life only.
    parked_since: HashMap<NodeId, (Vec<u8>, f64)>,
    /// What this page set aside of each document, for the page's life. A page
    /// never sets aside on its own (it lives minutes, not the 24 hours): only
    /// what another device's report says it set aside, or what this page set
    /// aside before and parked again (decided 2026-11-20, building wave 3).
    set_aside: HashMap<NodeId, Vec<protocol::SetAsideRecord>>,
}

/// A document that differs from what the other devices hold.
struct StepCandidate {
    since_ms: f64,
    held_ms: f64,
    reason: String,
    kind: pimble_core::OutOfStepKind,
    marked: bool,
}

/// How long a difference lasts, with the page connected, before a document
/// is out of step.
/// How often a store's people are asked again, for naming devices.
const PEOPLE_EVERY_MS: f64 = 10.0 * 60.0 * 1000.0;
const OUT_OF_STEP_AFTER_MS: f64 = (protocol::OUT_OF_STEP_AFTER_SECS * 1000) as f64;
/// How often the page judges whether its documents are in step.
const STEPS_EVERY_MS: f64 = 30_000.0;

/// Which way a document is held back: what this page has not sent, or what
/// it has not been able to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Side {
    Send,
    Read,
}

/// What an open store is still owed from outside, and when to go and look.
///
/// Two things leave a document shut on the live path. A share's key this page
/// does not hold: its marker arrived in an update, or a member's blob will not
/// open under anything held. And a document's wraps: a `VaultAppended` carries
/// the blob and not the keys, so the first blob of a document made elsewhere a
/// moment ago names a data key whose wraps this page was never handed. One
/// look answers both: ask the accounts service for the share keys that are
/// missing, never more often than [`SHARE_KEY_FLOOR_MS`], then read again the
/// documents that are owed (`VaultClient::look_for_keys`).
///
/// The backend loop drives it through the retry a waiting store already has
/// ([`VaultClient::key_retries_due`], [`VaultClient::retry_key`]), and awaits
/// each look, so a store never has two in flight.
#[derive(Default)]
struct KeyLook {
    /// When the next look is due (the page's clock, ms); `None` when nothing
    /// is wanted. Wanting one while one is due never makes it later, so a
    /// burst of reasons is one look.
    due: Option<f64>,
    /// When the accounts service was last asked for this store's share keys.
    asked_keys_at: Option<f64>,
    /// Documents with a blob nothing here opened, and the key it named, to be
    /// read again for their wraps.
    unread: HashMap<NodeId, KeyId>,
    /// What has been read again for that reason already: a document this page
    /// cannot open is asked about once a connection and once per new scope
    /// key, not once an append (the desktop link's `unresolved`).
    asked: HashSet<(NodeId, KeyId)>,
}

impl KeyLook {
    /// Want a look a debounce from `now`, or sooner when one is due sooner.
    fn want(&mut self, now: f64) {
        self.want_at(now + KEY_LOOK_DEBOUNCE_MS);
    }

    fn want_at(&mut self, at: f64) {
        self.due = Some(self.due.map_or(at, |due| due.min(at)));
    }

    fn is_due(&self, now: f64) -> bool {
        self.due.is_some_and(|due| due <= now)
    }
}

/// One document's log as fetched and decrypted, in log order.
struct Pulled {
    node_id: NodeId,
    entries: Vec<(Mark, Result<Vec<u8>, String>)>,
    head: u64,
}

/// One blob ready to append, and what to record once the server numbers it.
struct Outgoing {
    doc_id: VaultDocId,
    /// One whole blob, or the parts of one group in order.
    blobs: Vec<Sealed>,
    /// The plaintext that went into the blob, to move `known_sv` by.
    payload: Vec<u8>,
    /// The document's state vector when the blob was made: what the server
    /// holds once the append succeeds, when this was a resend.
    sent_sv: Vec<u8>,
    /// Whether this carried everything since `known_sv` rather than one edit.
    resend: bool,
    /// The server has never seen this document: the data key this page made
    /// for it, which rides the append, and what the append names besides
    /// (the parent, so a scoped member's new document joins their scope; a
    /// discussion's host).
    created: Option<(VaultDocKeys, NewDocument)>,
}

impl Outgoing {
    /// The first (for a small update, the only) blob.
    #[cfg(test)]
    fn blob(&self) -> &str {
        &self.blobs[0].blob
    }
}

/// A store this account holds a grant on whose key has not arrived yet.
struct Waiting {
    /// The store as the server listed it, so the retry needs no second
    /// `listStores` and the UI can be told the moment it opens.
    listed: Store,
    /// The page's clock, ms.
    due: f64,
}

/// What merging a peer's updates into one document came to.
struct Merged {
    events: Vec<BackendEvent>,
    /// The `node` or `children` root changed: the tree may need a repair.
    structure: bool,
    /// The merge put a share's marker on the node, or changed the key it
    /// names, and this page holds the whole store and not that key.
    wants_key: bool,
}

pub struct VaultClient {
    /// This client's id, as the server sees it, for recognising the
    /// notifications caused by this client's own appends.
    client_id: String,
    /// The device id this page's state reports go out under (see the module
    /// doc, "State reports").
    device: String,
    stores: HashMap<StoreId, VaultStore>,
    /// What the accounts service says about each store this account has a
    /// grant on: its kind (the authority when the RPC `Store` does not carry
    /// one yet), its grants, and who shared it. One entry per store, however
    /// many grants it holds on it.
    rows: HashMap<StoreId, AccountStore>,
    /// Stores whose key has not reached this account yet: the store as the
    /// server listed it, and when to ask again. A fresh share sits here until
    /// one of its owner's devices comes online to hand the key over.
    waiting: HashMap<StoreId, Waiting>,
    /// Which of those this session has already said out loud: once per store,
    /// not once per retry.
    announced: HashSet<StoreId>,
    /// The stores whose endpoint is down because the computer they are served
    /// from is (docs/RELAY_CONTRACT.md), as the backend loop last said
    /// ([`VaultClient::endpoint_down`]): read from what this page holds,
    /// which for one it never opened is nothing, and not changed.
    owner_offline: HashSet<StoreId>,
    /// The stores the session itself said are served somewhere other than its
    /// own endpoint (`POST /token`'s `stores`): known to be relayed even when
    /// the account's store list could not be read.
    relayed: HashSet<StoreId>,
    /// The stores every share of which ended while this page was open
    /// ([`VaultClient::take_rows`]), until the account's list names them
    /// again. The session may go on naming a relayed one's endpoint until its
    /// next token; that endpoint failing is not the store's owner being
    /// offline, and must not bring its row back.
    ended: HashSet<StoreId>,
    /// The stores subscribed to on the current socket (see `subscribe`).
    subscribed: HashSet<StoreId>,
    /// The stores closed in this page ([`VaultClient::close`]): not opened
    /// again, nor listed, until the page is loaded again.
    closed: HashSet<StoreId>,
    /// Events raised somewhere with no reply of its own to carry them — a
    /// document pulled again after a refused append — drained by [`pump`].
    pending: Vec<BackendEvent>,
    notices_tx: Sender<StoreChangedNotification>,
    notices_rx: Receiver<StoreChangedNotification>,
    /// Presence frames as the subscriptions deliver them, still sealed
    /// (docs/PRESENCE_CONTRACT.md section 9), opened by [`pump`].
    presence_tx: Sender<pimble_rpc::PresenceFrame>,
    presence_rx: Receiver<pimble_rpc::PresenceFrame>,
    /// Who listens to each store's changes through the API: a plugin's
    /// subscription (docs/PLUGINS_CONTRACT.md wave 3). The app's own views
    /// hear the vault through [`VaultClient::pump`]'s events instead.
    watchers: HashMap<StoreId, Vec<futures::channel::mpsc::UnboundedSender<pimble_api::StoreChange>>>,
    /// Who is writing, while a plugin's call holds the vault
    /// (`crate::vault_backend::VaultBackend::for_plugins`): what its writes
    /// are attributed to, and the sign that the app's views must be told of
    /// them, since no command of the app's own made them.
    pub(crate) writer: Option<String>,
    /// The changes plugins made, for [`VaultClient::pump`] to show.
    by_plugins: Vec<pimble_api::StoreChange>,
    /// Pictures this page has decrypted or put, by store and blob
    /// (docs/IMAGES_CONTRACT.md wave 4): a blob never changes, so one read is
    /// enough for the page's life. The app's own loader keeps the shown ones.
    pictures: HashMap<(StoreId, BlobId), (String, Vec<u8>)>,
}

/// The kind a change of one document is heard as through the API: one that
/// names that document first (`pimble_api::backend::document_of`), so its
/// update is merged into the right copy, and whose reach (`Affected::of`)
/// covers what the change can touch: the document's own row for content and
/// fields, its row, its list and its parent's list for structure.
fn heard_kind(node_id: NodeId, effect: NodeUpdateEffect) -> StoreChangeKind {
    if effect.structure {
        StoreChangeKind::TreeStructure { node_ids: vec![node_id] }
    } else if effect.content {
        StoreChangeKind::ContentUpdated { node_id }
    } else {
        StoreChangeKind::MetadataUpdated { node_id }
    }
}

impl VaultClient {
    pub fn new(client_id: String) -> Self {
        let (notices_tx, notices_rx) = unbounded();
        let (presence_tx, presence_rx) = unbounded();
        Self {
            client_id,
            device: device_id(),
            stores: HashMap::new(),
            rows: HashMap::new(),
            waiting: HashMap::new(),
            announced: HashSet::new(),
            owner_offline: HashSet::new(),
            relayed: HashSet::new(),
            ended: HashSet::new(),
            subscribed: HashSet::new(),
            closed: HashSet::new(),
            pending: Vec::new(),
            notices_tx,
            notices_rx,
            presence_tx,
            presence_rx,
            watchers: HashMap::new(),
            writer: None,
            by_plugins: Vec::new(),
            pictures: HashMap::new(),
        }
    }
}
