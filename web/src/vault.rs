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
//! from a merged update (see [`derive_kinds`]).
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
//! [`VaultStore::repair_now`], the browser's half of
//! `pimble_store::LocalStore::repair_scopes`).
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

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use crossbeam_channel::{unbounded, Receiver, Sender};
use pimble_app::protocol::{BackendCommand, BackendEvent};
use pimble_client::PimbleClient;
use pimble_core::{
    custom_keys, node_types, DeletedNode, LeftShare, Node, NodeId, NodeMetadata, RelaySide, Store, StoreAccess, StoreId, StoreKind,
};
use pimble_crdt::{NodeDoc, NodeFields, NodeUpdateEffect, Tree, TreeEdit};
use pimble_crypto::{blob_aad, dek_aad, unwrap_dek, wrap_dek, Blob, KeyId, SymmetricKey, WrappedDek};
use pimble_rpc::{
    SearchResultItem, StoreChangeKind, StoreChangedNotification, VaultCursor, VaultDocId,
    VaultDocKeys, VaultFetchResponse,
};

use crate::keys::{self, KeyError, Keyring};

/// How many appends to a document before its snapshot is refreshed. The
/// contract's number: a reader then replays at most this many updates.
const SNAPSHOT_EVERY: u32 = 200;

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

/// What a write is answered with while the computer a store is served from is
/// off (docs/RELAY_CONTRACT.md): nothing of such a store is on Pimble Cloud, a
/// browser keeps no copy of its own for an edit to wait in, and a change made
/// on this page would be gone with the tab.
pub const OWNER_OFFLINE_REFUSAL: &str =
    "This is shared from its owner's computer, which is offline. It can be changed again once that computer is back.";

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
    /// Appends since the last snapshot upload.
    appends: u32,
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
            appends: 0,
            known_sv: pimble_crdt::empty_state_vector(),
            unsent: false,
        }
    }
}

/// One grant this account holds on a store, as the accounts service lists it
/// (`GET /api/v1/stores`, one row per grant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGrant {
    /// The shared node for a share, `None` for the whole store.
    pub root: Option<NodeId>,
    pub role: String,
    /// The share's own name — never the owner's store name — for a scoped
    /// grant; the store's for a whole-store one.
    pub name: String,
}

/// What the account's store list says about one store: every grant it holds on
/// it, so a store shared twice is one entry with two roots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountStore {
    /// `"vault"` or `"plain"`.
    pub kind: String,
    pub grants: Vec<AccountGrant>,
    /// An owner's email when the store reached this account as a share.
    pub shared_by: Option<String>,
    /// The row's tier is `relay` (docs/RELAY_CONTRACT.md): nothing of the
    /// store is on Pimble Cloud, and it is reached at an endpoint of its own
    /// while its owner's computer is on.
    pub relayed: bool,
}

impl AccountStore {
    /// What this account may do in the store: `Read` only when every grant it
    /// holds is a reader's. A reader of one folder and an editor of another
    /// gets `Full`, and the server refuses the writes under the read-only root
    /// one at a time — a store-wide `Read` would take away the editing they do
    /// have (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Token").
    pub fn access(&self) -> StoreAccess {
        if !self.grants.is_empty() && self.grants.iter().all(|g| g.role == "reader") {
            StoreAccess::Read
        } else {
            StoreAccess::Full
        }
    }

    /// The shared roots, in the order the service listed them; empty when any
    /// grant is of the whole store, which covers every node in it.
    pub fn roots(&self) -> Vec<NodeId> {
        if self.grants.iter().any(|g| g.root.is_none()) {
            return Vec::new();
        }
        self.grants.iter().filter_map(|g| g.root).collect()
    }

    /// The shared roots this account only reads while it edits others, as a
    /// desktop replica's `sync.json` names its own (`Store::read_only_roots`);
    /// empty when [`AccountStore::access`] answers for the whole store.
    pub fn read_only_roots(&self) -> Vec<NodeId> {
        if !self.access().allows_write() || self.grants.iter().any(|g| g.root.is_none()) {
            return Vec::new();
        }
        self.grants.iter().filter(|g| g.role == "reader").filter_map(|g| g.root).collect()
    }

    /// What this account may change of one node, by the rule the hosted
    /// server judges a write with (docs/NODE_DOCUMENT_CONTRACT.md section 5,
    /// "Token"): a whole-store grant wins over any share and its role
    /// answers for every node; otherwise the roles of the shared roots the
    /// node is under (`reached`: the grant roots on its parent chain, itself
    /// included) decide, and a node under a reader's root nested in an
    /// editor's takes the wider role. A node that reaches no root this
    /// account holds is not this rule's to judge and reads as the store does.
    pub fn node_access(&self, reached: &[NodeId]) -> StoreAccess {
        let reads = |role: &str| role == "reader";
        if let Some(whole) = self.grants.iter().find(|g| g.root.is_none()) {
            return if reads(&whole.role) { StoreAccess::Read } else { StoreAccess::Full };
        }
        let mut roles = self.grants.iter().filter(|g| g.root.is_some_and(|root| reached.contains(&root))).map(|g| g.role.as_str()).peekable();
        if roles.peek().is_none() {
            return self.access();
        }
        if roles.all(reads) {
            StoreAccess::Read
        } else {
            StoreAccess::Full
        }
    }

    /// The name to show: the share's for a store reached through one share,
    /// "Shared by <owner>" for one reached through several (each root under
    /// it carries its own title), and `None` for a whole-store grant, whose
    /// name the server already carries. The same words the desktop gives a
    /// share's replica (`pimble_server::cloud::HeldAs::from_rows`), so one
    /// person's two devices do not call one thing by two names.
    pub fn name(&self) -> Option<String> {
        if self.grants.iter().any(|g| g.root.is_none()) {
            return None;
        }
        if self.grants.len() > 1 {
            return Some(match &self.shared_by {
                Some(owner) => format!("Shared by {owner}"),
                None => "Shared with you".to_string(),
            });
        }
        self.grants.iter().find(|g| !g.name.is_empty()).map(|g| g.name.clone())
    }
}

/// Shares of one store this page was showing that the account's store list
/// no longer names: the account was removed from them, or their owner
/// stopped sharing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedShares {
    pub store_id: StoreId,
    /// The shared roots that ended, in the order they were listed.
    pub roots: Vec<NodeId>,
    /// Nothing of the store is left to this account.
    pub every: bool,
}

/// What ended between two readings of the account's store list: for every
/// store held as shares, the roots the new list no longer names. A store the
/// account holds whole now (a whole-store grant covers every node) has lost
/// nothing, and a store held whole before was never a share.
pub fn shares_ended(was: &HashMap<StoreId, AccountStore>, now: &HashMap<StoreId, AccountStore>) -> Vec<EndedShares> {
    let mut ended = Vec::new();
    for (store_id, held) in was {
        let held_roots = held.roots();
        if held_roots.is_empty() {
            continue;
        }
        let Some(row) = now.get(store_id) else {
            ended.push(EndedShares { store_id: *store_id, roots: held_roots, every: true });
            continue;
        };
        if row.grants.iter().any(|g| g.root.is_none()) {
            continue;
        }
        let left = row.roots();
        let roots: Vec<NodeId> = held_roots.into_iter().filter(|root| !left.contains(root)).collect();
        if !roots.is_empty() {
            ended.push(EndedShares { store_id: *store_id, roots, every: left.is_empty() });
        }
    }
    ended
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
struct StoreKeys {
    /// The scope keys: the store key, or each share's, or both.
    scope: Keyring,
    /// Each document's wrapped data keys, as the server last served them.
    wraps: HashMap<NodeId, VaultDocKeys>,
    /// Data keys already unwrapped, with the id the blobs name.
    deks: HashMap<NodeId, (KeyId, SymmetricKey)>,
    /// Each document's data-key id as `vaultListDocs` reported it, for a
    /// document whose wraps have not been fetched yet.
    listed_dek: HashMap<NodeId, KeyId>,
    /// The wraps this page made for a document whose create has not been
    /// confirmed: a create tried again goes out under the same key, which
    /// is the one the server holds if the first try's answer was lost.
    creating: HashMap<NodeId, VaultDocKeys>,
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
    blob: String,
    /// The plaintext that went into the blob, to move `known_sv` by.
    payload: Vec<u8>,
    /// The document's state vector when the blob was made: what the server
    /// holds once the append succeeds, when this was a resend.
    sent_sv: Vec<u8>,
    /// Whether this carried everything since `known_sv` rather than one edit.
    resend: bool,
    /// The server has never seen this document: the data key this page made
    /// for it, to set once the append has landed (the server admits keys only
    /// for a document it has), and the parent the append must name so a scoped
    /// member's new document joins their scope.
    created: Option<(VaultDocKeys, Option<NodeId>)>,
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
    /// The node the editor currently has open, learned from
    /// `SubscribeNodeChanges`. A decrypted content update is only turned into
    /// `RemoteChanges` for this node: the app has one editor pane and that
    /// event carries no node identity, so applying another node's update to it
    /// would corrupt what is on screen.
    active: Option<(StoreId, NodeId)>,
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
    /// Events raised somewhere with no reply of its own to carry them — a
    /// document pulled again after a refused append — drained by [`pump`].
    pending: Vec<BackendEvent>,
    notices_tx: Sender<StoreChangedNotification>,
    notices_rx: Receiver<StoreChangedNotification>,
}

impl VaultClient {
    pub fn new(client_id: String) -> Self {
        let (notices_tx, notices_rx) = unbounded();
        Self {
            client_id,
            stores: HashMap::new(),
            rows: HashMap::new(),
            waiting: HashMap::new(),
            announced: HashSet::new(),
            active: None,
            owner_offline: HashSet::new(),
            relayed: HashSet::new(),
            ended: HashSet::new(),
            subscribed: HashSet::new(),
            pending: Vec::new(),
            notices_tx,
            notices_rx,
        }
    }

    /// Whether this store is one the vault client answers for.
    pub fn owns(&self, store_id: StoreId) -> bool {
        self.stores.contains_key(&store_id)
    }

    /// Whether a store's changes are this client's to subscribe to: an
    /// encrypted store, whether it is open here or only known to be encrypted
    /// from the account's store list. A relayed store is always one.
    pub fn is_encrypted(&self, store_id: StoreId) -> bool {
        self.owns(store_id) || self.is_relayed(store_id) || self.rows.get(&store_id).is_some_and(|row| row.kind == "vault")
    }

    /// The stores the freshly minted session says are served from their
    /// owners' computers, each at an endpoint of its own.
    pub fn set_relayed(&mut self, store_ids: Vec<StoreId>) {
        self.relayed = store_ids.into_iter().collect();
        // A store the session no longer names is not waited for.
        let relayed = &self.relayed;
        self.owner_offline.retain(|id| relayed.contains(id));
    }

    /// Whether a store is served from its owner's computer through Pimble
    /// Cloud's relay (docs/RELAY_CONTRACT.md), as the session or the
    /// account's store list says.
    pub fn is_relayed(&self, store_id: StoreId) -> bool {
        self.relayed.contains(&store_id) || self.rows.get(&store_id).is_some_and(|row| row.relayed)
    }

    /// What this account may change in a store, from the grants it holds;
    /// nothing while the computer it is served from is off.
    pub fn access(&self, store_id: StoreId) -> StoreAccess {
        if self.owner_offline.contains(&store_id) {
            return StoreAccess::Read;
        }
        self.rows.get(&store_id).map(AccountStore::access).unwrap_or(StoreAccess::Full)
    }

    /// The roots of a store this account only reads while it edits others
    /// (`Store::read_only_roots`); none while [`VaultClient::access`] answers
    /// for the whole store.
    fn read_only_roots(&self, store_id: StoreId) -> Vec<NodeId> {
        if self.owner_offline.contains(&store_id) {
            return Vec::new();
        }
        self.rows.get(&store_id).map(AccountStore::read_only_roots).unwrap_or_default()
    }

    /// Read the account's store list: which stores are encrypted, and every
    /// grant this account holds on each.
    ///
    /// Asked separately from the RPC store list because only the accounts
    /// service knows a share's own name, who shared it and which roots the
    /// account holds — the hosted server answers with the *owner's* store
    /// name, which a recipient has no business learning. One row per grant, so
    /// the rows of one store are collected here.
    ///
    /// Answers what the UI should hear about shares that ended since the
    /// list was last read ([`VaultClient::take_rows`]). A list that cannot be
    /// read changes nothing and ends nothing.
    pub async fn learn_rows(&mut self) -> Vec<BackendEvent> {
        let list = match crate::accounts::list_stores().await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!("Could not read the account's store list: {}", e);
                return Vec::new();
            }
        };

        let mut rows: HashMap<StoreId, AccountStore> = HashMap::new();
        for view in list {
            let Ok(store_id) = StoreId::parse(&view.store_id) else {
                tracing::warn!("The account's store list named {}, which is not a store id", view.store_id);
                continue;
            };
            // A row whose `root` will not parse is a scoped grant this page
            // cannot address; dropping the row is safer than reading it as a
            // whole-store grant, which is what `None` would mean.
            let root = match view.root.as_deref() {
                Some(raw) => match NodeId::parse(raw) {
                    Ok(root) => Some(root),
                    Err(_) => {
                        tracing::warn!("A grant on {} named the root {}, which is not a node id", store_id, raw);
                        continue;
                    }
                },
                None => None,
            };
            let row = rows.entry(store_id).or_default();
            row.kind = view.kind;
            row.relayed |= view.tier == "relay";
            if row.shared_by.is_none() {
                row.shared_by = view.shared_by;
            }
            row.grants.push(AccountGrant { root, role: view.role, name: view.name });
        }
        self.take_rows(rows)
    }

    /// Take the account's store list as just read, and let go of every share
    /// it no longer names (Joe, 2026-09-21: the folder leaves the explorer
    /// with a notice). This page holds no replica, so there is nothing to
    /// keep: the documents under an ended root are dropped from memory, and
    /// a store none of whose shares is left is closed. The UI is told which
    /// roots ended (`StoreChangeKind::SharesEnded`, which it answers with the
    /// notice and by dropping them) and, for a store with nothing left, that
    /// it is closed, before the list that no longer names them reaches it.
    fn take_rows(&mut self, rows: HashMap<StoreId, AccountStore>) -> Vec<BackendEvent> {
        let ended = shares_ended(&self.rows, &rows);
        self.ended.retain(|store_id| !rows.contains_key(store_id));
        self.rows = rows;
        let mut events = Vec::new();
        for share in ended {
            tracing::info!("Store {}: {} share(s) of it ended{}", share.store_id, share.roots.len(), if share.every { ", the last of them" } else { "" });
            events.push(BackendEvent::RemoteStoreChange {
                store_id: share.store_id,
                change_kind: StoreChangeKind::SharesEnded { node_ids: share.roots.clone() },
                source_client_id: None,
            });
            // A store left with no scope root would read as a whole store,
            // so one that would be is closed here and opened again from
            // the list as it is now.
            let still_open = !share.every && self.stores.get_mut(&share.store_id).is_none_or(|open| open.drop_roots(&share.roots));
            if !still_open {
                self.stores.remove(&share.store_id);
            }
            if self.active.is_some_and(|(store_id, node_id)| store_id == share.store_id && !self.stores.get(&store_id).is_some_and(|open| open.tree.doc(node_id).is_some())) {
                self.active = None;
            }
            if share.every {
                self.waiting.remove(&share.store_id);
                self.subscribed.remove(&share.store_id);
                self.owner_offline.remove(&share.store_id);
                self.ended.insert(share.store_id);
                events.push(BackendEvent::StoreClosed { store_id: share.store_id });
            }
        }
        events
    }

    /// Fill in what this page knows about a store before the UI sees it: the
    /// root its documents name, and what the account's grants say about it.
    ///
    /// The root matters. A hosted store's manifest carries the root the
    /// server minted when the store was created, which is the real one only
    /// when this browser also made the tree. For a store hosted from a
    /// desktop the tree was made elsewhere and names a different root, and
    /// the hosted server, holding only ciphertext, never learns it. The
    /// documents are the authority and this is where they replace the
    /// placeholder. A store whose documents are not open here keeps the
    /// server's root: there is nothing better to say until they are.
    ///
    /// For a share the row decides the rest: the name (the share's own, since
    /// the server's is the owner's store's), who shared it, the roots this
    /// account holds and whether any of them may be written to.
    ///
    /// A store served from its owner's computer (docs/RELAY_CONTRACT.md) says
    /// so (`Store::relay`), and has no name anywhere but in its own documents:
    /// Pimble Cloud holds none for it and its twin is made without one. An
    /// account that holds the whole of it reads the root's title once that is
    /// open here, and until then it is called what the desktop calls it.
    /// While that computer is off nothing of the store may be changed from
    /// this page ([`VaultClient::access`]).
    pub fn describe(&self, store: &mut Store) {
        if let Some(row) = self.rows.get(&store.id) {
            store.access = row.access();
            store.read_only_roots = row.read_only_roots();
            store.shared_by = row.shared_by.clone();
            let roots = row.roots();
            if !roots.is_empty() {
                store.root_node_id = roots[0];
                store.roots = roots;
            }
            if let Some(name) = row.name() {
                store.name = name;
            }
        }
        if self.is_relayed(store.id) {
            store.relay = RelaySide::Member;
        }
        if self.owner_offline.contains(&store.id) {
            store.access = StoreAccess::Read;
            store.read_only_roots.clear();
        }
        if store.relay == RelaySide::Member && store.name.trim().is_empty() {
            store.name = self
                .stores
                .get(&store.id)
                .filter(|open| !open.is_partial())
                .and_then(|open| open.tree.get_node_info(open.tree.root()).ok())
                .map(|root| root.title)
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| pimble_app::state::RELAYED_PLACEHOLDER_NAME.to_string());
        }
        if let Some(open) = self.stores.get(&store.id) {
            if open.scope_roots.is_empty() {
                store.root_node_id = open.tree.root();
            }
        } else if self.waiting.contains_key(&store.id) && !store.name.ends_with(WAITING_SUFFIX) {
            // The grant is real; only the key is late. Listing the row says so
            // rather than leaving a store that silently never opens.
            store.name.push_str(WAITING_SUFFIX);
        }
    }

    /// [`describe`](Self::describe) over a whole list.
    pub fn describe_all(&self, stores: &mut [Store]) {
        for store in stores.iter_mut() {
            self.describe(store);
        }
    }

    /// Open every vault store in `stores` that is not open yet.
    ///
    /// Called before `StoresListed` reaches the UI, so the tree's first
    /// `GetChildren` already has documents to read. Answers what the UI should
    /// hear about the ones that did not open: an error for a real failure, and
    /// for a share still waiting on its key a sentence saying so, once per
    /// store per session.
    pub async fn open_listed(
        &mut self,
        client: &Arc<PimbleClient>,
        stores: &[Store],
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        for store in stores {
            // Either source saying "vault" is enough: the accounts service
            // knows a store's kind before any grant of it reaches a token, and
            // the RPC `Store` knows it for one the account list has not been
            // re-read for yet.
            if store.kind == StoreKind::Vault {
                self.rows.entry(store.id).or_default().kind = "vault".to_string();
            }
            if !self.is_encrypted(store.id) || self.stores.contains_key(&store.id) {
                continue;
            }
            match self.open_store(client, store, signal_ui).await {
                Ok(()) => {}
                Err(KeyError::NoneYet) => events.extend(self.now_waiting(store)),
                Err(KeyError::Failed(message)) => {
                    tracing::error!("Opening the encrypted store {}: {}", store.name, message);
                    events.push(BackendEvent::Error {
                        message: format!("Could not open an encrypted store ({}: {})", store.name, message),
                    });
                }
            }
        }
        events
    }

    /// Record that a store is waiting for its key, and say so the first time.
    ///
    /// A grant with no envelope yet is the ordinary state of a fresh share:
    /// the owner's Pimble has not been online since to hand the key over. It
    /// is a sentence for the person, not an error, so it goes through the
    /// notice channel (see [`notice`]).
    fn now_waiting(&mut self, listed: &Store) -> Option<BackendEvent> {
        let store_id = listed.id;
        self.waiting.insert(store_id, Waiting { listed: listed.clone(), due: now_ms() + KEY_RETRY_MS });
        if !self.announced.insert(store_id) {
            return None;
        }
        let who = self
            .rows
            .get(&store_id)
            .and_then(|row| row.shared_by.clone())
            .unwrap_or_else(|| "the owner".to_string());
        Some(notice(format!(
            "Waiting for {who}'s Pimble to come online to finish sharing"
        )))
    }

    /// The stores with keys worth asking for again: one still waiting for its
    /// own (see [`KEY_RETRY_MS`]), and an open one whose look for a share's
    /// key or a document's wraps has come due (see [`KeyLook`]).
    pub fn key_retries_due(&self) -> Vec<StoreId> {
        let now = now_ms();
        let waiting = self.waiting.iter().filter(|(_, waiting)| waiting.due <= now).map(|(id, _)| *id);
        let looking = self.stores.iter().filter(|(_, store)| store.look.is_due(now)).map(|(id, _)| *id);
        waiting.chain(looking).collect()
    }

    /// Ask again for a waiting store's key and open it if it has arrived; for
    /// a store that is open, look for what it is still owed
    /// ([`VaultClient::look_for_keys`]).
    ///
    /// The store is announced to the UI when it opens, since `StoresListed`
    /// has long since been and gone — and it is announced without the
    /// "waiting" suffix its name carried, which is what makes the row settle.
    pub async fn retry_key(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Vec<BackendEvent> {
        if self.stores.contains_key(&store_id) {
            return self.look_for_keys(client, store_id).await;
        }
        let Some(listed) = self.waiting.get(&store_id).map(|w| w.listed.clone()) else {
            return Vec::new();
        };
        match self.open_store(client, &listed, signal_ui).await {
            Ok(()) => {
                let mut store = listed;
                self.describe(&mut store);
                tracing::info!("Store {}: the key arrived; it is open", store_id);
                vec![BackendEvent::StoreOpened { store }]
            }
            Err(e) => {
                // Still worth retrying either way: a request that failed may
                // not fail next time, and a key that has not arrived may.
                if let Some(waiting) = self.waiting.get_mut(&store_id) {
                    waiting.due = now_ms() + KEY_RETRY_MS;
                }
                if let KeyError::Failed(message) = e {
                    tracing::warn!("Asking again for the key of {} failed: {}", store_id, message);
                }
                Vec::new()
            }
        }
    }

    /// Fetch this store's keys and every document's log, decrypt, build the
    /// tree, and hold it.
    ///
    /// Every document is pulled at open, not lazily: titles and children
    /// lists live in the documents, so nothing of the tree can be drawn until
    /// they are here. That costs one `vaultFetch` per node (in windows of
    /// [`FETCH_WINDOW`]); a store of thousands of nodes wants a fetch that
    /// answers for many documents at once, which the vault RPCs do not offer
    /// yet.
    ///
    /// The subscription is made first, so an append that lands during the
    /// pull is queued for `pump` rather than missed until the next connect;
    /// one the pull already applied merges again to nothing.
    async fn open_store(
        &mut self,
        client: &Arc<PimbleClient>,
        store: &Store,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), KeyError> {
        let store_id = store.id;
        // The roots to fetch keys for are the account's grants: a share's key
        // lives under its own root, the store key under none.
        let scope_roots = self.rows.get(&store_id).map(AccountStore::roots).unwrap_or_default();
        let keyring = keys::fetch_scope_keyring(&store_id.to_string(), &scope_roots).await?;

        if let Some(BackendEvent::Error { message }) = self.subscribe(client, store_id, signal_ui).await {
            return Err(KeyError::Failed(message));
        }

        let listing = client
            .vault_list_docs_response(store_id)
            .await
            .map_err(|e| KeyError::Failed(format!("listing the vault's documents failed: {e}")))?;
        // Which logs these are: everything read below is numbered in them.
        let epoch = listing.epoch;
        let mut keys = StoreKeys::new(keyring);
        let mut wanted: Vec<(NodeId, u64)> = Vec::new();
        for info in listing.docs {
            match info.doc_id {
                VaultDocId::Node(id) => {
                    if let Some(dek_id) = info.dek_id {
                        keys.listed_dek.insert(id, dek_id);
                    }
                    if info.head > 0 {
                        wanted.push((id, 0));
                    }
                }
                // A hosted twin from before this layout keeps its tree
                // document, superseded by the node documents.
                VaultDocId::Tree => tracing::debug!("Store {} lists a tree document; skipping it", store_id),
            }
        }

        let mut fetched_docs = Vec::with_capacity(wanted.len());
        for (node_id, fetched) in fetch_many(client, store_id, wanted).await {
            match fetched {
                Ok(fetched) => fetched_docs.push((node_id, fetched)),
                // Left unheld: the next catch-up sees it listed and fetches
                // it from the start.
                Err(message) => tracing::warn!("Fetching the document {} failed: {}", node_id, message),
            }
        }

        let (mut vault_store, shut) = VaultStore::from_fetched(store.clone(), keys, scope_roots, fetched_docs);
        vault_store.epoch = epoch;
        // The shares in a whole store: what the first pass opened says which
        // nodes are shared, and their keys open what members made that no
        // desktop of the owner's has wrapped under the store key yet. Before
        // the repair, so it judges the tree with those documents in it.
        if fetch_share_keys(store_id, &mut vault_store).await {
            let now = now_ms();
            for (node_id, fetched) in &shut {
                vault_store.take_fetched(store_id, *node_id, fetched, false, now);
            }
        }
        // Once, after the whole pull, never between the updates of one edit.
        let repair = vault_store.repair_now(&now_rfc3339());
        self.stores.insert(store_id, vault_store);
        self.waiting.remove(&store_id);
        if let Some(edit) = repair {
            if let Err(message) = self.append_edit(client, store_id, &edit).await {
                tracing::warn!("Appending the repair of {} failed: {}", store_id, message);
            }
        }
        Ok(())
    }

    // ── After a reconnect ───────────────────────────────────────────────────

    /// Bring every open store level with the server again, both ways.
    ///
    /// This client outlives its connection: the socket is replaced on every
    /// token refresh and after every drop, while the decrypted documents stay.
    /// Nothing else closes the gap in between. Appends the server took while
    /// the socket was down were never notified here, and an append of this
    /// client's that failed was, until 2026-09-17, simply lost, along with
    /// every later edit's chance of being shown anywhere else (each depends on
    /// the one before). Called on every connect, after the subscriptions are
    /// made again and before `open_listed`.
    ///
    /// `served` says which stores `client`'s endpoint serves: a store served
    /// from its owner's computer has an endpoint of its own, and is caught up
    /// when that one connects (docs/RELAY_CONTRACT.md).
    ///
    /// Such a store's twin is disposable, and one built again numbers its
    /// logs from 1 under a new epoch. Every cursor this page kept would skip
    /// what those logs hold, and what it believed the server held is about
    /// logs that are gone, so both are forgotten
    /// ([`VaultStore::take_epoch`]): everything is read again from the start,
    /// and every document this account may write is then offered again, as
    /// whatever it holds beyond what the new logs turned out to.
    pub async fn catch_up(&mut self, client: &Arc<PimbleClient>, served: impl Fn(StoreId) -> bool) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        let store_ids: Vec<StoreId> = self.stores.keys().copied().filter(|id| served(*id)).collect();
        for store_id in store_ids {
            // The shares in a whole store first, so what is read below opens.
            // A new connection may also ask again about every document it
            // could not open, and the pull below reads each of them anyway.
            if let Some(store) = self.stores.get_mut(&store_id) {
                store.look.unread.clear();
                store.look.asked.clear();
                fetch_share_keys(store_id, store).await;
            }

            // Down: every document the server lists, from where this page
            // stopped reading it, or from the start for one it never held (a
            // node created elsewhere while the socket was down) or could not
            // open.
            let listing = match client.vault_list_docs_response(store_id).await {
                Ok(listing) => listing,
                Err(e) => {
                    tracing::warn!("Listing the documents of {} failed: {}", store_id, e);
                    continue;
                }
            };
            let wanted: Vec<(NodeId, u64)> = {
                let row = self.rows.get(&store_id);
                let Some(store) = self.stores.get_mut(&store_id) else { continue };
                let rebuilt = store.take_epoch(listing.epoch.as_deref(), row);
                if rebuilt {
                    tracing::info!("Store {}: the server's logs are not the ones this page read (its twin was built again); reading them from the start and offering every document again", store_id);
                }
                let mut wanted = Vec::new();
                for info in listing.docs {
                    let VaultDocId::Node(id) = info.doc_id else { continue };
                    if rebuilt {
                        // Known to be there, whether or not the fetch below
                        // succeeds: an append to it must not try to create it.
                        store.heads.insert(id, info.head);
                    }
                    if let Some(dek_id) = info.dek_id {
                        store.keys.listed_dek.insert(id, dek_id);
                    }
                    let from = store.docs.get(&id).map(|d| d.cursor.applied_through()).unwrap_or(0);
                    // A document whose head has not moved is still fetched
                    // when its wraps are missing: a fetch answers with them
                    // whether or not it carries an entry, and without them
                    // nothing can be encrypted for that document.
                    let wants_keys = info.dek_id.is_some() && !store.keys.wraps.contains_key(&id);
                    if info.head > from || wants_keys {
                        wanted.push((id, from));
                    }
                }
                wanted
            };
            let (pulled, structure) = self.pull(client, store_id, wanted).await;
            events.extend(pulled);

            // A pull is applied whole, then judged once: as at open, and as
            // the desktop's vault link does after its reconnect pull.
            if structure {
                if let Some(store) = self.stores.get_mut(&store_id) {
                    store.repair_due = None;
                    events.extend(store.adopt_root().map(|store| BackendEvent::StoreOpened { store }));
                }
                events.extend(self.repair(client, store_id).await);
            }

            // Up: whatever an append failed to deliver.
            let unsent: Vec<NodeId> = self
                .stores
                .get(&store_id)
                .map(|s| s.docs.iter().filter(|(_, doc)| doc.unsent).map(|(id, _)| *id).collect())
                .unwrap_or_default();
            for node_id in unsent {
                if let Err(e) = self.resend(client, store_id, node_id).await {
                    tracing::warn!("Resending the document {} failed: {}", node_id, e);
                }
            }
        }
        events
    }

    /// Fetch documents from where this page stopped reading each, and merge
    /// what comes: the events the UI should see, and whether any of it
    /// touched structure.
    async fn pull(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        wanted: Vec<(NodeId, u64)>,
    ) -> (Vec<BackendEvent>, bool) {
        let mut events = Vec::new();
        let mut structure = false;
        let fetched = fetch_many(client, store_id, wanted).await;
        let now = now_ms();
        for (node_id, fetched) in fetched {
            let Ok(fetched) = fetched else { continue };
            let active = self.active == Some((store_id, node_id));
            let Some(store) = self.stores.get_mut(&store_id) else { continue };
            let merged = store.take_fetched(store_id, node_id, &fetched, active, now);
            structure |= merged.structure;
            events.extend(merged.events);
        }
        (events, structure)
    }

    // ── What an open store is still owed ────────────────────────────────────

    /// One look for an open store (see [`KeyLook`]): the keys of the shares in
    /// it this page has seen a marker of and holds no key for, then the
    /// documents worth reading again: every one not read through when a key
    /// came, and otherwise the ones whose wraps this page never had.
    ///
    /// What opens reaches the UI as it would from a notification: the kinds a
    /// merge derives, and a repair once the updates have stopped.
    async fn look_for_keys(&mut self, client: &Arc<PimbleClient>, store_id: StoreId) -> Vec<BackendEvent> {
        let wanted = {
            let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
            store.look.due = None;
            let new_key = fetch_share_keys(store_id, store).await;
            store.reread_list(new_key)
        };
        if wanted.is_empty() {
            return Vec::new();
        }
        tracing::debug!("Store {}: reading {} documents again", store_id, wanted.len());
        let (mut events, structure) = self.pull(client, store_id, wanted).await;
        if structure {
            if let Some(store) = self.stores.get_mut(&store_id) {
                store.repair_due = Some(now_ms() + REPAIR_DEBOUNCE_MS);
                events.extend(store.adopt_root().map(|store| BackendEvent::StoreOpened { store }));
            }
        }
        events
    }

    // ── Live updates ────────────────────────────────────────────────────────

    /// Apply whatever the subscriptions have delivered since the last pass.
    ///
    /// Returns the events the UI should see. Called once per turn of the
    /// backend loop, which is what keeps `&mut self` out of the subscription
    /// task (it only forwards raw notifications down a channel).
    pub fn pump(&mut self) -> Vec<BackendEvent> {
        let mut events = std::mem::take(&mut self.pending);
        while let Ok(notification) = self.notices_rx.try_recv() {
            let StoreChangeKind::VaultAppended { doc_id, seq } = &notification.change_kind else {
                // A vault store emits nothing else, but forward anything that
                // does arrive rather than swallowing it.
                events.push(BackendEvent::RemoteStoreChange {
                    store_id: notification.store_id,
                    change_kind: notification.change_kind.clone(),
                    source_client_id: notification.source_client_id.clone(),
                });
                continue;
            };
            let store_id = notification.store_id;
            let node_id = match doc_id {
                VaultDocId::Node(id) => *id,
                VaultDocId::Tree => {
                    tracing::debug!("An append to the tree document of {}; nothing reads it", store_id);
                    continue;
                }
            };
            let seq = *seq;
            let Some(blob) = notification.update.clone() else {
                tracing::warn!("A VaultAppended notification carried no blob; ignoring it");
                continue;
            };
            let source = notification.source_client_id.clone();
            events.extend(self.apply_notification(store_id, node_id, seq, &blob, source));
        }
        events
    }

    fn apply_notification(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        seq: u64,
        blob: &str,
        source_client_id: Option<String>,
    ) -> Vec<BackendEvent> {
        let active = self.active == Some((store_id, node_id));
        let apply = should_apply(source_client_id.as_deref(), &self.client_id);
        let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };

        // Sequence numbers say how far this client has read, never who wrote
        // what; `should_apply` is the whole of that decision. This client's own
        // append is already in its document, so its number counts as read.
        if !apply {
            if let Some(doc) = store.docs.get_mut(&node_id) {
                doc.cursor.mark(seq);
            }
            return Vec::new();
        }

        let merged = store.take_blob(store_id, node_id, seq, blob, active, source_client_id, now_ms());

        let mut events = merged.events;
        if merged.structure {
            store.repair_due = Some(now_ms() + REPAIR_DEBOUNCE_MS);
            events.extend(store.adopt_root().map(|store| BackendEvent::StoreOpened { store }));
        }
        events
    }

    /// The stores whose debounced repair has come due.
    pub fn repairs_due(&self) -> Vec<StoreId> {
        let now = now_ms();
        self.stores
            .iter()
            .filter(|(_, store)| store.repair_due.is_some_and(|due| due <= now))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Repair a store's tree if it needs it and append what the repair wrote,
    /// as any edit is appended. The UI hears `TreeStructure` for the
    /// documents it touched, as it would from the server.
    pub async fn repair(&mut self, client: &Arc<PimbleClient>, store_id: StoreId) -> Vec<BackendEvent> {
        let edit = {
            let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
            store.repair_due = None;
            store.repair_now(&now_rfc3339())
        };
        let Some(edit) = edit else { return Vec::new() };
        tracing::info!("Store {}: repaired the tree ({} documents)", store_id, edit.touched.len());
        if let Err(message) = self.append_edit(client, store_id, &edit).await {
            tracing::warn!("Appending the repair of {} failed: {}", store_id, message);
        }
        vec![BackendEvent::RemoteStoreChange {
            store_id,
            change_kind: StoreChangeKind::TreeStructure { node_ids: edit.node_ids() },
            source_client_id: None,
        }]
    }

    // ── Commands ────────────────────────────────────────────────────────────

    /// Answer `cmd` if it names a vault store; otherwise hand it back.
    pub async fn handle(
        &mut self,
        client: &Arc<PimbleClient>,
        cmd: BackendCommand,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Handled {
        // A search spans every store, so it is decided before the store id is.
        if let BackendCommand::Search { query, stores, limit } = &cmd {
            if stores.iter().any(|id| self.owns(*id)) {
                let event = self.search(client, query, stores, *limit).await;
                return Handled::Yes(Some(event));
            }
            return Handled::No(cmd);
        }

        let Some(store_id) = store_id_of(&cmd) else { return Handled::No(cmd) };
        if !self.owns(store_id) {
            return Handled::No(cmd);
        }

        // A reader's write never reaches here: the backend loop asks
        // [`VaultClient::refuse_write`] before it hands a command anywhere, so
        // that one gate covers an encrypted store and a plain one alike.
        Handled::Yes(match cmd {
            BackendCommand::GetChildren { store_id, node_id } => Some(self.get_children(store_id, node_id)),

            BackendCommand::GetNode { store_id, node_id } => Some(self.get_node(store_id, node_id)),

            BackendCommand::CreateNode { store_id, parent_id, title } => {
                Some(self.create_node(client, store_id, parent_id, title).await)
            }

            BackendCommand::RenameNode { store_id, node_id, title } => {
                Some(self.rename_node(client, store_id, node_id, title).await)
            }

            BackendCommand::SetNodeAppearance { store_id, node_id, icon, color, tags } => {
                Some(self.set_appearance(client, store_id, node_id, icon, color, tags).await)
            }

            BackendCommand::DeleteNode { store_id, node_id } => {
                Some(self.delete_node(client, store_id, node_id).await)
            }

            BackendCommand::MoveNode { store_id, node_id, new_parent_id, position } => {
                Some(self.move_node(client, store_id, node_id, new_parent_id, position).await)
            }

            BackendCommand::UndeleteNode { store_id, node_id } => {
                Some(self.undelete_node(client, store_id, node_id).await)
            }

            BackendCommand::ListDeleted { store_id } => Some(self.list_deleted(store_id)),

            BackendCommand::BroadcastChanges { store_id, node_id, changes } => {
                match self.broadcast(client, store_id, node_id, &changes).await {
                    Ok(()) => None,
                    // A refusal is the server saying no, and no later attempt
                    // will change its mind, so the person is told. Any other
                    // failure is recorded against the document and resent on
                    // the next connection, which needs no announcement.
                    Err(message) if is_refusal(&message) => Some(BackendEvent::Error { message }),
                    Err(message) => {
                        tracing::warn!("Appending an encrypted edit failed: {}", message);
                        None
                    }
                }
            }

            BackendCommand::SetNodeContent { store_id, node_id, content } => {
                match self.append_update(client, store_id, node_id, &content).await {
                    Ok(()) => Some(BackendEvent::NodeContentUpdated { store_id, node_id }),
                    Err(message) => Some(BackendEvent::Error { message }),
                }
            }

            BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector } => {
                Some(self.reconcile(store_id, node_id, &state_vector))
            }

            BackendCommand::SubscribeStoreChanges { store_id } => {
                self.subscribe(client, store_id, signal_ui).await
            }

            // One editor pane, one open node: this is how the vault client
            // learns which node a decrypted update belongs on screen. No
            // server subscription is needed — the store's already delivers
            // every blob.
            BackendCommand::SubscribeNodeChanges { store_id, node_id } => {
                self.active = Some((store_id, node_id));
                None
            }

            // A vault store has no sync link and no server-side index. Both are
            // answered here rather than refused, so the app's ordinary
            // registration path does not light up the status bar with errors.
            // What the account may change is what the store was described
            // with (`describe`), from the same rows.
            BackendCommand::GetStoreSync { store_id } => Some(self.sync_event(store_id)),
            BackendCommand::RebuildIndex { store_id } => {
                Some(BackendEvent::IndexRebuilt { store_id, indexed: 0 })
            }

            other => Some(BackendEvent::Error {
                message: format!("{} is not available on an encrypted store", name_of(&other)),
            }),
        })
    }

    /// The refusal a writing command earns on a store this account may only
    /// read, or `None` when there is nothing to refuse.
    ///
    /// The backend loop's one gate, asked before a command is handed anywhere,
    /// so an encrypted store and a plain one are refused the same way and for
    /// the same reason. It answers from the account's grants, so it does not
    /// depend on the store being one this client holds.
    ///
    /// A store whose owner's computer is off takes no change either, and says
    /// that instead ([`OWNER_OFFLINE_REFUSAL`]). With one exception: what the
    /// editor has already put on screen. The app stops sending those the
    /// moment it hears the store is to be read, but a keystroke can be on its
    /// way before that, and refusing it would leave the editor holding text
    /// its document here lacks, with every later edit built on it. Those are
    /// kept ([`VaultClient::handle_unreachable`]) and go up when the owner's
    /// computer is back.
    pub fn refuse_write(&self, store_id: StoreId, cmd: &BackendCommand) -> Option<BackendEvent> {
        if !writes(cmd) {
            return None;
        }
        let row_access = self.rows.get(&store_id).map(AccountStore::access).unwrap_or(StoreAccess::Full);
        if !row_access.allows_write() {
            return Some(BackendEvent::Error { message: StoreAccess::READ_ONLY_REFUSAL.to_string() });
        }
        if self.owner_offline.contains(&store_id) && !(self.owns(store_id) && is_content_edit(cmd)) {
            return Some(notice(OWNER_OFFLINE_REFUSAL.to_string()));
        }
        None
    }

    // ── A store whose owner's computer is off ───────────────────────────────

    /// The backend loop could not reach the endpoint that serves these stores
    /// (docs/RELAY_CONTRACT.md): the computer they are shared from is off, or
    /// offline. Pimble Cloud itself has just answered this page, so that is
    /// what it means, and it is an ordinary state, not an error.
    ///
    /// A store this page never opened is listed all the same, from what the
    /// account's own list says of it: the share's name and who shared it. It
    /// opens empty and to be read, and fills in when the endpoint is back
    /// ([`VaultClient::endpoint_up`]). Every one of them says `owner offline`
    /// on its row, and takes no change until then. Answers only what is news.
    pub fn endpoint_down(&mut self, store_ids: &[StoreId]) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        for &store_id in store_ids {
            // Nothing of it is this account's any more: its endpoint not
            // answering says nothing about its owner's computer.
            if self.ended.contains(&store_id) {
                continue;
            }
            if !self.owner_offline.insert(store_id) {
                continue;
            }
            tracing::info!("Store {}: its owner's computer is offline", store_id);
            if !self.owns(store_id) {
                events.push(BackendEvent::StoresListed { stores: vec![self.offline_row(store_id)] });
            }
            events.push(self.sync_event(store_id));
        }
        events
    }

    /// The endpoint that serves these stores is connected again, and what it
    /// lists has been opened (`listed`, already described). A store shown from
    /// the account's row while it was down is announced as the store it is, so
    /// the tree fetches what is in it with no reload; one this page held all
    /// along only has its row say so. Answers nothing for a store that was
    /// never down.
    pub fn endpoint_up(&mut self, store_ids: &[StoreId], listed: &[Store]) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        for &store_id in store_ids {
            if !self.owner_offline.remove(&store_id) {
                continue;
            }
            tracing::info!("Store {}: its owner's computer is back", store_id);
            // `listed` was described while this store still read as offline.
            if let Some(store) = listed.iter().find(|store| store.id == store_id) {
                let mut store = store.clone();
                self.describe(&mut store);
                events.push(BackendEvent::StoreOpened { store });
            }
            events.push(self.sync_event(store_id));
        }
        events
    }

    /// The row of a store this page has never opened and cannot reach: what
    /// the account's own list says of it, which for a share is the share's
    /// name and who shared it. No root of its own is known (the twin's is on
    /// the computer that is off), so the row stands alone, a placeholder under
    /// it that answers as an empty folder.
    fn offline_row(&self, store_id: StoreId) -> Store {
        let row = self.rows.get(&store_id);
        let name = row
            .and_then(|row| row.name().or_else(|| row.grants.iter().map(|g| g.name.trim()).find(|name| !name.is_empty()).map(str::to_string)))
            .unwrap_or_default();
        let mut store = Store::new_local(name, std::path::PathBuf::new());
        store.id = store_id;
        store.kind = StoreKind::Vault;
        store.root_node_id = offline_root(store_id);
        store.shared_by = row.and_then(|row| row.shared_by.clone());
        store.access = StoreAccess::Read;
        store.relay = RelaySide::Member;
        if store.name.trim().is_empty() {
            store.name = pimble_app::state::RELAYED_PLACEHOLDER_NAME.to_string();
        }
        store
    }

    /// What `GetStoreSync` is answered with for a store this client answers
    /// for. A vault store in the browser has no link, so there is no badge,
    /// except the one that says its owner's computer is off.
    fn sync_event(&self, store_id: StoreId) -> BackendEvent {
        BackendEvent::StoreSyncChanged {
            store_id,
            remote: None,
            state: pimble_core::SyncState::Offline,
            sync_mode: pimble_core::StoreKind::Plain,
            access: self.access(store_id),
            read_only_roots: self.read_only_roots(store_id),
            // This page holds no replica: a share that ended is dropped and
            // is no longer one of the store's roots at all (`take_rows`).
            ended_roots: Vec::new(),
            relay: if self.is_relayed(store_id) { RelaySide::Member } else { RelaySide::None },
            owner_offline: self.owner_offline.contains(&store_id),
        }
    }

    /// Answer `cmd` for a store whose endpoint is down because its owner's
    /// computer is, with no connection to answer through; any other command
    /// comes back untouched.
    ///
    /// A store this page holds is read from what it holds. One it never
    /// opened (or whose key has not arrived) opens empty: its row, an empty
    /// folder under it, nothing else, and no error, because nothing has gone
    /// wrong. Subscriptions wait for the endpoint (the backend loop makes
    /// them again when it connects). An edit the editor had already made is
    /// merged and kept as unsent, which the next connection delivers; every
    /// other write was refused before it got here
    /// ([`VaultClient::refuse_write`]).
    pub fn handle_unreachable(&mut self, cmd: BackendCommand) -> Handled {
        let Some(store_id) = store_id_of(&cmd).filter(|id| self.owner_offline.contains(id)) else {
            return Handled::No(cmd);
        };
        let held = self.owns(store_id);
        Handled::Yes(match cmd {
            BackendCommand::GetChildren { store_id, node_id } if held => Some(self.get_children(store_id, node_id)),
            BackendCommand::GetChildren { store_id, node_id } => {
                Some(BackendEvent::ChildrenLoaded { store_id, parent_id: node_id, children_store_id: store_id, children: Vec::new() })
            }
            BackendCommand::GetNode { store_id, node_id } if held => Some(self.get_node(store_id, node_id)),
            BackendCommand::GetNode { store_id, node_id } => {
                let title = self.offline_row(store_id).name;
                Some(BackendEvent::NodeLoaded { store_id, node: empty_folder(node_id, &title) })
            }
            BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector } if held => {
                Some(self.reconcile(store_id, node_id, &state_vector))
            }
            BackendCommand::BroadcastChanges { store_id, node_id, changes } if held => {
                match STANDARD.decode(&changes).map_err(|e| e.to_string()).and_then(|update| self.keep_unsent(store_id, node_id, &update)) {
                    Ok(()) => None,
                    Err(message) => {
                        tracing::warn!("Keeping an edit of {} for later failed: {}", node_id, message);
                        None
                    }
                }
            }
            BackendCommand::SetNodeContent { store_id, node_id, content } if held => {
                match self.keep_unsent(store_id, node_id, &content) {
                    Ok(()) => Some(BackendEvent::NodeContentUpdated { store_id, node_id }),
                    Err(message) => Some(BackendEvent::Error { message }),
                }
            }
            BackendCommand::SubscribeNodeChanges { store_id, node_id } => {
                self.active = Some((store_id, node_id));
                None
            }
            BackendCommand::SubscribeStoreChanges { .. } => None,
            BackendCommand::GetStoreSync { store_id } => Some(self.sync_event(store_id)),
            BackendCommand::RebuildIndex { store_id } => Some(BackendEvent::IndexRebuilt { store_id, indexed: 0 }),
            BackendCommand::ListDeleted { store_id } if held => Some(self.list_deleted(store_id)),
            BackendCommand::ListDeleted { store_id } => Some(BackendEvent::DeletedListed { store_id, nodes: Vec::new() }),
            _ => Some(notice(OWNER_OFFLINE_REFUSAL.to_string())),
        })
    }

    /// Merge an edit the editor has already made into its document and mark
    /// the document unsent, with nothing to append it through: the next
    /// connection's catch-up sends everything the server lacks of it.
    fn keep_unsent(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<(), String> {
        let Some(store) = self.stores.get_mut(&store_id) else {
            return Err("no such encrypted store".to_string());
        };
        store.tree.apply_update(node_id, update).map_err(|e| e.to_string())?;
        store.doc_entry(node_id).unsent = true;
        Ok(())
    }

    // ── Reads ───────────────────────────────────────────────────────────────

    /// Answered from the documents held: every child arrives with its
    /// document's bytes, because the app derives a document's tree label from
    /// its first line and opens it from the bytes it is handed here, with no
    /// second fetch in between.
    fn get_children(&self, store_id: StoreId, node_id: NodeId) -> BackendEvent {
        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        let row = self.rows.get(&store_id);
        let children = match store.children_of(node_id) {
            Ok(ids) => ids.iter().filter_map(|id| store.node_for(*id, row).ok()).map(|node| self.judged(store_id, node)).collect(),
            Err(message) => return BackendEvent::Error { message },
        };
        BackendEvent::ChildrenLoaded { store_id, parent_id: node_id, children_store_id: store_id, children }
    }

    fn get_node(&self, store_id: StoreId, node_id: NodeId) -> BackendEvent {
        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        match store.node_for(node_id, self.rows.get(&store_id)) {
            Ok(node) => BackendEvent::NodeLoaded { store_id, node: self.judged(store_id, node) },
            Err(message) => BackendEvent::Error { message },
        }
    }

    /// A node as the UI is handed it while its store's owner's computer is
    /// off: to be read, whatever the account's grants say, because nothing
    /// written to it here would reach anyone ([`OWNER_OFFLINE_REFUSAL`]).
    fn judged(&self, store_id: StoreId, mut node: Node) -> Node {
        if self.owner_offline.contains(&store_id) {
            node.access = StoreAccess::Read;
        }
        node
    }

    // ── Tree writes ─────────────────────────────────────────────────────────

    async fn create_node(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        parent_id: Option<NodeId>,
        title: String,
    ) -> BackendEvent {
        let node_id = NodeId::new();
        let now = now_rfc3339();
        let (parent, edit) = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let parent = parent_id.unwrap_or(store.tree.root());
            let mut edit = store.seed_root(&now);
            match store.tree.add_node(node_id, Some(parent), None, node_types::DOCUMENT, &title, &now) {
                Ok(more) => edit.touched.extend(more.touched),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
            (parent, edit)
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeCreated { store_id, parent_id: Some(parent), node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn rename_node(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        title: String,
    ) -> BackendEvent {
        let now = now_rfc3339();
        let edit = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let mut edit = store.seed_root(&now);
            match rename_edit(&mut store.tree, node_id, &title, &now) {
                Ok(more) => edit.touched.extend(more.touched),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
            edit
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeRenamed { store_id, node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    async fn set_appearance(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        icon: Option<Option<String>>,
        color: Option<Option<String>>,
        tags: Option<Vec<String>>,
    ) -> BackendEvent {
        let now = now_rfc3339();
        let edit = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let mut edit = store.seed_root(&now);
            match appearance_edit(&mut store.tree, node_id, icon, color, tags, &now) {
                Ok(more) => edit.touched.extend(more.touched),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
            edit
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeRenamed { store_id, node_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    /// The whole subtree becomes tombstones, as it does server-side; the
    /// documents stay, so an undelete elsewhere finds them.
    async fn delete_node(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, node_id: NodeId) -> BackendEvent {
        let now = now_rfc3339();
        let (parent_id, edit) = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let parent_id = match store.tree.get_node_info(node_id) {
                Ok(info) => info.parent_id,
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            };
            let Some(parent_id) = parent_id else {
                return BackendEvent::Error { message: "the root node cannot be deleted".into() };
            };
            match store.tree.remove_node(node_id, &now) {
                Ok(edit) => (parent_id, edit),
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) => BackendEvent::NodeDeleted { store_id, node_id, parent_id },
            Err(message) => BackendEvent::Error { message },
        }
    }

    /// `Tree::move_or_transplant` decides, as every caller of a move does
    /// (docs/MOVE_CONTRACT.md): a plain move inside a share or outside all of
    /// them, or a transplant when it would take the node out of one, with a
    /// fresh document (its key riding its first append, exactly as
    /// [`VaultClient::create_node`] makes one) for every node of the subtree
    /// and the original tombstoned. `Tree::transplant` orders the touched
    /// documents new-first, tombstones-last, and one `append_edit` call sends
    /// them in that order; a document an append fails on is marked unsent and
    /// resent by the existing mechanism, whichever half of the move it was.
    async fn move_node(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> BackendEvent {
        let now = now_rfc3339();
        let (old_parent_id, title, left_shares, new_node_id, edit) = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let info = match store.tree.get_node_info(node_id) {
                Ok(info) => info,
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            };
            let old_parent_id = info.parent_id.unwrap_or(new_parent_id);
            // What the move would leave, read before it happens: a
            // transplant's original is a tombstone afterwards and answers
            // nothing about the shares it used to be in.
            let left_shares = as_left_shares(&store.tree, store.tree.shares_left(node_id, new_parent_id));
            let mut edit = store.seed_root(&now);
            let (new_node_id, more) = match store.tree.move_or_transplant(node_id, new_parent_id, position, &now, &mut NodeId::new) {
                Ok(v) => v,
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            };
            edit.touched.extend(more.touched);
            (old_parent_id, info.title, left_shares, new_node_id, edit)
        };
        match self.append_edit(client, store_id, &edit).await {
            Ok(()) if new_node_id == node_id => {
                BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id }
            }
            Ok(()) => BackendEvent::NodeTransplanted {
                from_store_id: store_id,
                old_node_id: node_id,
                old_parent_id,
                to_store_id: store_id,
                node_id: new_node_id,
                new_parent_id,
                title,
                left_shares,
            },
            Err(message) => BackendEvent::Error { message },
        }
    }

    /// "Put Back": clear the tombstone `undelete_node` finds and everything
    /// the same deletion took with it, and answer where it landed, exactly as
    /// the desktop reads the same answer off `undeleteNode` plus a `getNode`
    /// (`crates/pimble-app/src/commands.rs`).
    async fn undelete_node(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, node_id: NodeId) -> BackendEvent {
        let now = now_rfc3339();
        let edit = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            match store.tree.undelete_node(node_id, &now) {
                Ok(edit) => edit,
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
        };
        if let Err(message) = self.append_edit(client, store_id, &edit).await {
            return BackendEvent::Error { message };
        }
        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        match store.tree.get_node_info(node_id) {
            Ok(info) => BackendEvent::NodeCreated { store_id, parent_id: info.parent_id, node_id },
            Err(e) => BackendEvent::Error { message: e.to_string() },
        }
    }

    /// What "Recently Deleted..." shows, answered from the tree this page
    /// holds — a scoped member's page holds only its scope's documents to
    /// begin with, so nothing further is judged here.
    fn list_deleted(&self, store_id: StoreId) -> BackendEvent {
        let Some(store) = self.stores.get(&store_id) else {
            return BackendEvent::Error { message: "no such encrypted store".into() };
        };
        let nodes = store.list_deleted(self.rows.get(&store_id));
        BackendEvent::DeletedListed { store_id, nodes }
    }

    /// `TransplantNode` between two vault stores this page holds
    /// (docs/MOVE_CONTRACT.md "Between stores"): the subtree read out of the
    /// source as data ([`pimble_crdt::Tree::take_cutting`]), planted under
    /// `new_parent_id` in the destination with a fresh id and a fresh key per
    /// node ([`pimble_crdt::Tree::plant`]), appended there, and only once
    /// that whole append succeeds is the source tombstoned and its own
    /// tombstones appended: created first, deleted second, so a failure
    /// between the two leaves the original exactly where it was.
    ///
    /// `left_shares` is every share the node was in: leaving the store leaves
    /// all of them, since two stores never share an ancestor.
    ///
    /// The caller (`web/src/backend.rs`) is the one place this is reached
    /// from: it is not routed through [`VaultClient::handle`], which answers
    /// only a single store's commands, because a transplant needs the
    /// destination's endpoint too.
    #[allow(clippy::too_many_arguments)]
    pub async fn transplant_node(
        &mut self,
        from_client: &Arc<PimbleClient>,
        to_client: &Arc<PimbleClient>,
        from_store_id: StoreId,
        node_id: NodeId,
        to_store_id: StoreId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> BackendEvent {
        let now = now_rfc3339();

        let (old_parent_id, title, left_shares, cutting) = {
            let Some(from_store) = self.stores.get(&from_store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let info = match from_store.tree.get_node_info(node_id) {
                Ok(info) => info,
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            };
            let Some(old_parent_id) = info.parent_id else {
                return BackendEvent::Error { message: "the root node cannot be moved into another store".into() };
            };
            let left_shares = as_left_shares(&from_store.tree, from_store.tree.shares(node_id));
            let cutting = match from_store.tree.take_cutting(node_id) {
                Ok(cutting) => cutting,
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            };
            (old_parent_id, info.title, left_shares, cutting)
        };

        let (new_node_id, edit_to) = {
            let Some(to_store) = self.stores.get_mut(&to_store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            let mut edit = to_store.seed_root(&now);
            match to_store.tree.plant(cutting, new_parent_id, position, &now, &mut NodeId::new) {
                Ok((new_root, more)) => {
                    edit.touched.extend(more.touched);
                    (new_root, edit)
                }
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
        };

        // Created first: a failure appending the new documents leaves the
        // original untouched, never tombstoned with nothing to show for it.
        if let Err(message) = self.append_edit(to_client, to_store_id, &edit_to).await {
            return BackendEvent::Error { message };
        }

        // Deleted second, only now that the new home is on the server.
        let tombstone_edit = {
            let Some(from_store) = self.stores.get_mut(&from_store_id) else {
                return BackendEvent::Error { message: "no such encrypted store".into() };
            };
            match from_store.tree.remove_node(node_id, &now) {
                Ok(edit) => edit,
                Err(e) => return BackendEvent::Error { message: e.to_string() },
            }
        };
        if let Err(message) = self.append_edit(from_client, from_store_id, &tombstone_edit).await {
            // The new node is already live and answered below; a failed
            // tombstone is unsent like any other failed append and resent by
            // the same mechanism, not a reason to tell the person their move
            // did not happen.
            tracing::warn!("Appending the tombstone of {} after a transplant failed: {}", node_id, message);
        }

        BackendEvent::NodeTransplanted {
            from_store_id,
            old_node_id: node_id,
            old_parent_id,
            to_store_id,
            node_id: new_node_id,
            new_parent_id,
            title,
            left_shares,
        }
    }

    /// Append what a tree operation wrote: one `vaultAppend` per touched
    /// document, in the order the edit names them (a new document before its
    /// parent's list, so a peer never lists a node it cannot read yet).
    ///
    /// Every document is attempted even after one fails, so each failure is
    /// recorded against its own document and resent from there; stopping at
    /// the first would leave the rest holding work nothing knows is unsent.
    async fn append_edit(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, edit: &TreeEdit) -> Result<(), String> {
        let mut first_error = None;
        for (node_id, update) in &edit.touched {
            if let Err(message) = self.append_prepared(client, store_id, *node_id, update).await {
                tracing::warn!("Appending to the document {} failed: {}", node_id, message);
                first_error.get_or_insert(message);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    // ── Content ─────────────────────────────────────────────────────────────

    /// A local edit from the editor: merge it, encrypt it, append it.
    async fn broadcast(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        changes: &str,
    ) -> Result<(), String> {
        // The editor's outbound closure encodes with the standard alphabet,
        // exactly as the plain path's `applyEdit` carries it.
        let update = STANDARD
            .decode(changes)
            .map_err(|e| format!("the editor's delta would not decode: {e}"))?;
        self.append_update(client, store_id, node_id, &update).await
    }

    /// Merge `update` into the node's document (the editor's delta, or a whole
    /// document from a session that started from nothing), then append it.
    async fn append_update(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        update: &[u8],
    ) -> Result<(), String> {
        {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return Err("no such encrypted store".to_string());
            };
            store.tree.apply_update(node_id, update).map_err(|e| e.to_string())?;
        }
        self.append_prepared(client, store_id, node_id, update).await
    }

    /// Send everything `node_id`'s document holds that the server does not,
    /// for a document an earlier append failed to deliver.
    async fn resend(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, node_id: NodeId) -> Result<(), String> {
        // An empty update: `prepare` sends the diff since `known_sv` for an
        // unsent document whatever it is handed.
        self.append_prepared(client, store_id, node_id, &[]).await
    }

    /// Append one document's update, already merged into the document here:
    /// encrypt, `vaultAppend`, record the outcome, snapshot when due.
    ///
    /// A document the server has never seen is created: its append names the
    /// parent (which is how a scoped member's new document joins their scope)
    /// and carries its wrapped data key, which the server stores together
    /// with the blob, so nothing is ever there under a key it has no wrap of.
    async fn append_prepared(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        node_id: NodeId,
        update: &[u8],
    ) -> Result<(), String> {
        let outgoing = {
            let Some(store) = self.stores.get_mut(&store_id) else {
                return Err("no such encrypted store".to_string());
            };
            store.prepare(store_id, node_id, update)?
        };
        let parent_id = outgoing.created.as_ref().and_then(|(_, parent)| *parent);
        let keys = outgoing.created.as_ref().map(|(keys, _)| keys.clone());
        let appended = client
            .vault_append_with_keys(
                store_id,
                outgoing.doc_id.clone(),
                outgoing.blob.clone(),
                Some(self.client_id.clone()),
                parent_id,
                keys,
            )
            .await
            .map_err(|e| e.to_string());

        // A refusal is the server saying no — a reader's root inside a store
        // this account edits elsewhere, or a document in nobody's scope. There
        // is nothing to resend, so the document is not left `unsent` (which
        // would push the same refused bytes on every reconnect); instead it is
        // pulled again, so what is on screen is what the server holds.
        if let Err(message) = &appended {
            if is_refusal(message) {
                tracing::warn!("Store {} refused an append to {}: {}", store_id, node_id, message);
                let events = self.repull(client, store_id, node_id).await;
                self.pending.extend(events);
                return Err(message.clone());
            }
        }

        let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
        let snapshot = store.record(node_id, &outgoing, appended)?;

        // The document is on the server now, and its wraps with it.
        if let Some((keys, _)) = &outgoing.created {
            store.keys.note_created(node_id, keys);
        }

        if let Some(upto_seq) = snapshot {
            let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
            let blob = store.snapshot_blob(store_id, node_id)?;
            match client.vault_snapshot(store_id, outgoing.doc_id, upto_seq, blob).await {
                Ok(()) => {
                    if let Some(doc) = self.stores.get_mut(&store_id).and_then(|s| s.docs.get_mut(&node_id)) {
                        doc.appends = 0;
                    }
                }
                Err(e) => tracing::warn!("Uploading a snapshot of {} failed: {}", node_id, e),
            }
        }
        Ok(())
    }

    /// Throw one document away and fetch it again from the start, so what this
    /// page holds is what the server holds.
    ///
    /// The only caller is a refused append. A yrs document cannot un-merge the
    /// edit the server would not take, and leaving it would show work that
    /// exists nowhere else and never will; the honest answer is the server's
    /// copy. A document the server refuses to serve either is simply dropped:
    /// it is not this account's to see.
    async fn repull(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, node_id: NodeId) -> Vec<BackendEvent> {
        // The parent before the document goes: what the UI has drawn of that
        // list is what changes, whether the document comes back or not.
        let touched = {
            let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
            let mut touched = vec![node_id];
            touched.extend(
                store
                    .tree
                    .doc(node_id)
                    .and_then(|doc| doc.fields().ok())
                    .and_then(|fields| fields.parent_id),
            );
            store.refused(node_id);
            touched
        };
        let fetched = client.vault_fetch(store_id, VaultDocId::Node(node_id), 0).await;
        let active = self.active == Some((store_id, node_id));
        let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
        let fetched = match fetched {
            Ok(fetched) => fetched,
            Err(e) => {
                tracing::warn!("Pulling {} again after a refusal failed: {}", node_id, e);
                return vec![BackendEvent::RemoteStoreChange {
                    store_id,
                    change_kind: StoreChangeKind::TreeStructure { node_ids: touched },
                    source_client_id: None,
                }];
            }
        };
        let merged = store.take_fetched(store_id, node_id, &fetched, active, now_ms());
        if merged.structure {
            store.repair_due = Some(now_ms() + REPAIR_DEBOUNCE_MS);
        }
        let mut events = merged.events;
        // The tree holds one document fewer or one different: what the UI has
        // drawn of it, and of the list it was in, is stale either way.
        events.push(BackendEvent::RemoteStoreChange {
            store_id,
            change_kind: StoreChangeKind::TreeStructure { node_ids: touched },
            source_client_id: None,
        });
        events
    }

    /// The same stateless reconcile the plain path does, answered from the
    /// node document this page holds.
    fn reconcile(&self, store_id: StoreId, node_id: NodeId, state_vector: &[u8]) -> BackendEvent {
        let Some(doc) = self.stores.get(&store_id).and_then(|s| s.tree.doc(node_id)) else {
            return BackendEvent::Error { message: "that node's document is not held here".into() };
        };
        match doc.diff_since(state_vector) {
            Ok(diff) => BackendEvent::NodeContentReconciled {
                store_id,
                node_id,
                diff,
                server_state_vector: doc.state_vector(),
            },
            Err(e) => BackendEvent::Error { message: e.to_string() },
        }
    }

    // ── Subscription ────────────────────────────────────────────────────────

    /// Subscribe to the store's changes and forward every notification into the
    /// channel `pump` drains.
    ///
    /// The task deliberately holds nothing but the sender: decryption needs
    /// `&mut self`, which a detached task cannot have and the backend loop
    /// already does.
    ///
    /// Three things ask for this — the client itself when it opens a store,
    /// the UI when it registers one, and the backend loop when it restores
    /// what a dead socket carried — and one per store per connection is what
    /// is wanted. `forget_subscriptions` is what makes a new socket start
    /// again.
    pub async fn subscribe(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Option<BackendEvent> {
        if !self.subscribed.insert(store_id) {
            return None;
        }
        match client.subscribe_store_changes(store_id).await {
            Ok(mut sub) => {
                let tx = self.notices_tx.clone();
                let signal = signal_ui.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    while let Some(Ok(notification)) = sub.next().await {
                        if tx.send(notification).is_err() {
                            break;
                        }
                        signal();
                    }
                });
                None
            }
            Err(e) => {
                self.subscribed.remove(&store_id);
                tracing::warn!("Subscribing to the changes of {} failed: {}", store_id, e);
                Some(BackendEvent::Error { message: format!("Subscribe failed: {e}") })
            }
        }
    }

    /// Forget which of the stores an endpoint serves are subscribed, because
    /// the socket that carried those subscriptions is gone. Called once per
    /// new connection, before anything subscribes again on it. `served` says
    /// which stores are that endpoint's: another endpoint's socket is its own.
    pub fn forget_subscriptions(&mut self, served: impl Fn(StoreId) -> bool) {
        self.subscribed.retain(|id| !served(*id));
    }

    // ── Search ──────────────────────────────────────────────────────────────

    /// Client-side search: a case-insensitive substring over decrypted titles
    /// and the text of every document held, which is every node's.
    ///
    /// There is no server index for a vault store and there cannot be one — the
    /// server has never seen a word of it. Plain stores in the same query still
    /// go to the server, and the two sets of hits are merged.
    async fn search(
        &self,
        client: &Arc<PimbleClient>,
        query: &str,
        stores: &[StoreId],
        limit: usize,
    ) -> BackendEvent {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return BackendEvent::SearchResults { results: Ok(Vec::new()) };
        }

        let mut results = Vec::new();
        for store_id in stores.iter().filter(|id| self.owns(**id)) {
            let Some(store) = self.stores.get(store_id) else { continue };
            for node_id in store.tree.list_node_ids() {
                let Ok(info) = store.tree.get_node_info(node_id) else { continue };
                let text = store.tree.doc(node_id).map(NodeDoc::text).unwrap_or_default();

                let in_title = info.title.to_lowercase().contains(&needle);
                let at = text.to_lowercase().find(&needle);
                if !in_title && at.is_none() {
                    continue;
                }

                results.push(SearchResultItem {
                    node_id,
                    store_id: *store_id,
                    // A title match ranks above a body match; there is no
                    // scoring model here and pretending otherwise would be
                    // worse than saying so.
                    score: if in_title { 1.0 } else { 0.5 },
                    title: if info.title.is_empty() { "Untitled".to_string() } else { info.title },
                    snippet: snippet_around(&text, at),
                    kind: "prose".to_string(),
                    node_type: info.node_type,
                    path: String::new(),
                });
            }
        }

        // Plain stores in the same query keep their server-side index. An
        // encrypted store not open here (its key or its owner's computer is
        // still to come) has nothing anywhere to search.
        let plain: Vec<StoreId> = stores.iter().copied().filter(|id| !self.is_encrypted(*id)).collect();
        if !plain.is_empty() {
            match client.search(query.to_string(), plain, true, limit).await {
                Ok(mut items) => results.append(&mut items),
                Err(e) => tracing::warn!("Searching the plain stores failed: {}", e),
            }
        }

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        BackendEvent::SearchResults { results: Ok(results) }
    }
}

// ── The store's documents, without the network ───────────────────────────────
//
// Everything below is decided from what this page holds and what it is handed,
// so the rules the vault client was debugged for (cursors, what is known to be
// on the server, what to resend, when a snapshot may be stamped) are testable
// on their own.

impl VaultStore {
    /// A store from what `vaultFetch` answered for each of its documents: take
    /// each document's wraps, decrypt its log, [`assemble`](Self::assemble).
    ///
    /// Also answers the documents with a blob nothing held opened, as they
    /// were fetched: a key that arrives a moment later (the key of a share in
    /// a whole store, see [`fetch_share_keys`]) opens them through
    /// [`VaultStore::take_fetched`] with no second round trip.
    fn from_fetched(
        listed: Store,
        mut keys: StoreKeys,
        scope_roots: Vec<NodeId>,
        fetched: Vec<(NodeId, VaultFetchResponse)>,
    ) -> (Self, Vec<(NodeId, VaultFetchResponse)>) {
        let store_id = listed.id;
        let mut pulled = Vec::with_capacity(fetched.len());
        let mut shut = Vec::new();
        for (node_id, fetched) in fetched {
            keys.note_wraps(node_id, fetched.keys.clone());
            let entries = keys.decrypt_entries(store_id, node_id, &fetched);
            let head = fetched.head;
            if entries.iter().any(|(_, opened)| opened.is_err()) {
                shut.push((node_id, fetched));
            }
            pulled.push(Pulled { node_id, entries, head });
        }
        (Self::assemble(listed, keys, scope_roots, pulled), shut)
    }

    /// A store from what its logs held: every decrypted entry, per document
    /// in log order. Picks the root, builds the tree, records each
    /// document's cursor. Nothing is repaired here; the caller runs
    /// [`VaultStore::repair_now`] once the whole pull is in.
    fn assemble(listed: Store, keys: StoreKeys, scope_roots: Vec<NodeId>, pulled: Vec<Pulled>) -> Self {
        let mut node_docs: HashMap<NodeId, NodeDoc> = HashMap::new();
        let mut docs: HashMap<NodeId, VaultDoc> = HashMap::new();
        let mut heads = HashMap::new();
        for Pulled { node_id, entries, head } in pulled {
            let node_doc = node_docs.entry(node_id).or_default();
            let doc = docs.entry(node_id).or_default();
            for (mark, update) in entries {
                match update {
                    Ok(bytes) => match node_doc.apply_update(&bytes) {
                        Ok(_) => {
                            advance(&mut doc.known_sv, &bytes);
                            mark.apply(&mut doc.cursor);
                        }
                        Err(e) => tracing::warn!("Skipping an unreadable update of {}: {}", node_id, e),
                    },
                    Err(message) => tracing::warn!("Skipping a blob of {}: {}", node_id, message),
                }
            }
            heads.insert(node_id, head);
        }
        // A scope's tree starts at its first shared root, whose own
        // `parent_id` names a document this page will never hold. Nothing may
        // look for another root: there is none to find, and the documents'
        // rule below would answer with whichever node happens to be missing
        // its parent.
        let start = scope_roots.first().copied().unwrap_or(listed.root_node_id);
        let mut tree = Tree::from_docs(start, node_docs);
        if scope_roots.is_empty() {
            let root = document_root(&tree, listed.root_node_id);
            if root != tree.root() {
                tracing::info!("Store {}: manifest root {} replaced by the documents' root {}", listed.id, listed.root_node_id, root);
                tree = rerooted(tree, root);
            }
        }
        Self { listed, keys, scope_roots, tree, docs, heads, repair_due: None, look: KeyLook::default(), epoch: None }
    }

    /// Whether this page holds only a scope of the store: a share's recipient.
    fn is_partial(&self) -> bool {
        !self.scope_roots.is_empty()
    }

    fn doc_entry(&mut self, node_id: NodeId) -> &mut VaultDoc {
        self.docs.entry(node_id).or_default()
    }

    /// Merge a peer's decrypted updates of one document, in log order, and
    /// say what the UI should hear: the kinds the server would derive from
    /// what the merge changed (see [`derive_kinds`]), with a content change
    /// handed to the editor as `RemoteChanges` when it has the node open and
    /// as `NodeContentUpdated` otherwise. `source` is who wrote it, when the
    /// notification said.
    ///
    /// An entry that would not decrypt or merge is never marked, so the
    /// cursor stops in front of it and a later snapshot cannot vouch for it.
    fn merge(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        entries: Vec<(Mark, Result<Vec<u8>, String>)>,
        active: bool,
        source: Option<String>,
    ) -> Merged {
        let before = shape_of(&self.tree, node_id);
        let marker_before = before.as_ref().and_then(DocShape::share_key_id);
        let mut effect = NodeUpdateEffect::default();
        let mut content_updates = Vec::new();
        for (mark, update) in entries {
            let bytes = match update {
                Ok(bytes) => bytes,
                Err(message) => {
                    tracing::warn!("Skipping a blob of {}: {}", node_id, message);
                    continue;
                }
            };
            match self.tree.apply_update(node_id, &bytes) {
                Ok(one) => {
                    let doc = self.doc_entry(node_id);
                    advance(&mut doc.known_sv, &bytes);
                    mark.apply(&mut doc.cursor);
                    effect.changed |= one.changed;
                    effect.structure |= one.structure;
                    effect.content |= one.content;
                    effect.data |= one.data;
                    if one.content || one.data {
                        content_updates.push(bytes);
                    }
                }
                Err(e) => tracing::warn!("An update of {} would not apply: {}", node_id, e),
            }
        }
        if !effect.changed {
            return Merged { events: Vec::new(), structure: false, wants_key: false };
        }

        let after = shape_of(&self.tree, node_id);
        // A share made, or made again under another key, while this page was
        // looking: a whole store's page is owed its key (see [`KeyLook`]).
        let marker_after = after.as_ref().and_then(DocShape::share_key_id);
        let wants_key = !self.is_partial()
            && marker_after != marker_before
            && marker_after.is_some_and(|key_id| self.keys.scope.get(&key_id).is_none());
        let mut events = Vec::new();
        for kind in derive_kinds(node_id, self.tree.root(), before.as_ref(), after.as_ref(), effect) {
            match kind {
                StoreChangeKind::ContentUpdated { .. } if active => {
                    // The editor has this node open, so hand it each delta
                    // the same way a plain store's subscription would.
                    for bytes in &content_updates {
                        events.push(BackendEvent::RemoteChanges { changes: STANDARD.encode(bytes) });
                    }
                }
                StoreChangeKind::ContentUpdated { .. } => {
                    events.push(BackendEvent::NodeContentUpdated { store_id, node_id });
                }
                change_kind => events.push(BackendEvent::RemoteStoreChange {
                    store_id,
                    change_kind,
                    source_client_id: source.clone(),
                }),
            }
        }
        Merged { events, structure: effect.structure, wants_key }
    }

    /// Take one document's `vaultFetch` answer: its wraps, then its log
    /// decrypted and merged, then its head. A marker it brought whose key
    /// this page lacks wants a look.
    fn take_fetched(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        fetched: &VaultFetchResponse,
        active: bool,
        now: f64,
    ) -> Merged {
        self.keys.note_wraps(node_id, fetched.keys.clone());
        let entries = self.keys.decrypt_entries(store_id, node_id, fetched);
        let merged = self.merge(store_id, node_id, entries, active, None);
        self.heads.insert(node_id, fetched.head);
        if merged.wants_key {
            self.look.want(now);
        }
        merged
    }

    /// Take one blob from the live path: decrypt, merge, note the head.
    ///
    /// An unknown key id is not fatal: a rotation this device has not been
    /// granted yet, a document whose wraps have not been fetched, or a
    /// member's document under a share whose key is not here yet looks
    /// exactly like this. The cursor stops in front of the entry, so a later
    /// snapshot cannot vouch for it, and a look is wanted for what would open
    /// it (see [`KeyLook`]).
    #[allow(clippy::too_many_arguments)]
    fn take_blob(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        seq: u64,
        blob: &str,
        active: bool,
        source: Option<String>,
        now: f64,
    ) -> Merged {
        let update = self.keys.decrypt_blob(store_id, node_id, blob);
        let unopened = match &update {
            Ok(_) => None,
            Err(_) => self.keys.key_wanted(store_id, node_id, blob),
        };
        let merged = self.merge(store_id, node_id, vec![(Mark::One(seq), update)], active, source);
        let head = self.heads.entry(node_id).or_insert(0);
        *head = (*head).max(seq);
        if merged.wants_key {
            self.look.want(now);
        }
        if let Some(key_id) = unopened {
            self.note_unopened(node_id, key_id, now);
        }
        merged
    }

    /// A blob of `node_id` named `key_id` and nothing here opened it.
    ///
    /// The first time, the document is worth reading again: a fetch answers
    /// with its wraps, which a notification never carries. After that only a
    /// scope key can help, which is worth a look while a share in this store
    /// still has none here; the floor on those asks is the look's.
    fn note_unopened(&mut self, node_id: NodeId, key_id: KeyId, now: f64) {
        if !self.look.asked.contains(&(node_id, key_id)) {
            self.look.unread.insert(node_id, key_id);
            self.look.want(now);
        } else if self.look.due.is_none() && !self.missing_share_roots().is_empty() {
            self.look.want(now);
        }
    }

    // ── The shares in a whole store ─────────────────────────────────────────

    /// The shared nodes whose key this page does not hold: every document it
    /// holds of a whole store whose `node` root carries a share's marker
    /// naming a key that is not in the keyring. Tombstones too: a deleted
    /// folder's share may still be the only thing a member's document is
    /// wrapped under. Nothing for a member's page, which holds its grants'
    /// keys and is owed no others.
    fn missing_share_roots(&self) -> Vec<NodeId> {
        if self.is_partial() {
            return Vec::new();
        }
        let mut roots: Vec<NodeId> = self
            .tree
            .ids()
            .into_iter()
            .filter(|id| {
                self.tree
                    .doc(*id)
                    .and_then(|doc| doc.fields().ok())
                    .and_then(|fields| share_marker_of(&fields))
                    .is_some_and(|marker| self.keys.scope.get(&marker.key_id).is_none())
            })
            .collect();
        roots.sort_by_key(|id| id.to_string());
        roots
    }

    /// The shared roots to ask the accounts service about now, or `None`:
    /// nothing is missing, or the last ask was under [`SHARE_KEY_FLOOR_MS`]
    /// ago, in which case the look is put off until the floor has passed.
    /// Answering `Some` records the ask.
    fn share_keys_to_ask_for(&mut self, now: f64) -> Option<Vec<NodeId>> {
        let missing = self.missing_share_roots();
        if missing.is_empty() {
            return None;
        }
        if let Some(asked_at) = self.look.asked_keys_at {
            if now - asked_at < SHARE_KEY_FLOOR_MS {
                self.look.want_at(asked_at + SHARE_KEY_FLOOR_MS);
                return None;
            }
        }
        self.look.asked_keys_at = Some(now);
        Some(missing)
    }

    /// Take more scope keys into the keyring; whether any of them is new. A
    /// new key may open what has been asked about before, so that is
    /// forgotten.
    fn take_keys(&mut self, more: Keyring) -> bool {
        let new = self.keys.scope.absorb(more);
        if new {
            self.look.asked.clear();
        }
        new
    }

    // ── A twin built again ──────────────────────────────────────────────────

    /// Take the `epoch` a `vaultListDocs` answered with: which logs the
    /// server's sequence numbers belong to. Whether they turned out to be
    /// other logs than the ones this page read, in which case everything
    /// about the old ones has just been forgotten
    /// ([`VaultStore::forget_remote_logs`]).
    ///
    /// A store served from its owner's computer keeps its twin there, derived
    /// and disposable (docs/RELAY_CONTRACT.md): deleted, the owner's link
    /// builds it again, and every log starts again from 1. A page that kept
    /// its cursors would read each new log from wherever it had read the old
    /// one to, and skip what lies before. The first epoch heard is simply
    /// remembered, and a server that names none concludes nothing, as on the
    /// desktop (`full_reconcile` in `crates/pimble-server/src/vault_link.rs`).
    fn take_epoch(&mut self, epoch: Option<&str>, row: Option<&AccountStore>) -> bool {
        let Some(epoch) = epoch else { return false };
        let rebuilt = self.epoch.as_deref().is_some_and(|read| read != epoch);
        if rebuilt {
            self.forget_remote_logs(row);
        }
        self.epoch = Some(epoch.to_string());
        rebuilt
    }

    /// The server's logs are other logs than the ones this page read: where
    /// each was read to, how many appends it has had, what the server was
    /// known to hold of each document and the data keys of the documents as
    /// they were are all forgotten, as the desktop's link forgets them
    /// (`Progress::forget_remote_logs`). The documents themselves stay: they
    /// are this page's copy of the notes, not of the logs.
    ///
    /// What follows (`VaultClient::catch_up`) is every document read from the
    /// start (a merge repeated is nothing), which is also what says what the
    /// new logs hold, and then every document this account may write offered
    /// again: a diff from there, so what the server already has merges to
    /// nothing, and what only this page held is not lost with the old twin.
    /// One it may only read is never offered: the server would refuse it, and
    /// a refused append throws the local copy away to read the server's.
    fn forget_remote_logs(&mut self, row: Option<&AccountStore>) {
        let held: Vec<NodeId> = self.tree.ids();
        let writable: HashSet<NodeId> = held.iter().copied().filter(|id| self.may_write(*id, row)).collect();
        self.docs.clear();
        for node_id in held {
            let doc = self.docs.entry(node_id).or_default();
            doc.unsent = writable.contains(&node_id);
        }
        self.heads.clear();
        self.keys.forget_documents();
        self.look.unread.clear();
        self.look.asked.clear();
    }

    /// Whether this account may write `node_id`, by the rule
    /// [`VaultStore::node_for`] puts on every node it hands the UI.
    fn may_write(&self, node_id: NodeId, row: Option<&AccountStore>) -> bool {
        let Some(row) = row else { return true };
        let grant_roots: Vec<NodeId> = row.grants.iter().filter_map(|g| g.root).collect();
        row.node_access(&roots_above(&self.tree, &grant_roots, node_id)).allows_write()
    }

    /// How far `node_id`'s log has been read without a gap.
    fn read_through(&self, node_id: NodeId) -> u64 {
        self.docs.get(&node_id).map(|doc| doc.cursor.applied_through()).unwrap_or(0)
    }

    /// The documents the server is known to hold more of than this page has
    /// read, with where to read each from: listed and never opened, or held
    /// with a blob nothing here opened.
    fn behind(&self) -> Vec<(NodeId, u64)> {
        let mut behind: Vec<(NodeId, u64)> = self
            .heads
            .iter()
            .map(|(id, head)| (*id, *head, self.read_through(*id)))
            .filter(|(_, head, from)| head > from)
            .map(|(id, _, from)| (id, from))
            .collect();
        behind.sort_by_key(|(id, _)| id.to_string());
        behind
    }

    /// What a look reads again: everything [`behind`](Self::behind) when a
    /// scope key has just arrived, and otherwise the documents whose wraps
    /// this page never had. Either way those are now asked about.
    fn reread_list(&mut self, new_key: bool) -> Vec<(NodeId, u64)> {
        let unread = std::mem::take(&mut self.look.unread);
        self.look.asked.extend(unread.iter().map(|(id, key_id)| (*id, *key_id)));
        if new_key {
            return self.behind();
        }
        let mut wanted: Vec<(NodeId, u64)> = unread.into_keys().map(|id| (id, self.read_through(id))).collect();
        wanted.sort_by_key(|(id, _)| id.to_string());
        wanted
    }

    /// Repair the tree if it needs it (`Tree::repair`): what it wrote, to be
    /// appended like any edit.
    fn repair_now(&mut self, now: &str) -> Option<TreeEdit> {
        let repaired = if self.is_partial() { self.repair_scopes(now) } else { self.tree.repair(now) };
        match repaired {
            Ok(edit) => edit,
            Err(e) => {
                tracing::warn!("Repairing the tree of {} failed: {}", self.listed.id, e);
                None
            }
        }
    }

    /// Repair a scoped store one scope at a time, the browser's half of
    /// `pimble_store::LocalStore::repair_scopes` (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5).
    ///
    /// Every repair is an edit that travels to the owner and to every other
    /// member, so it must be one a device holding the whole store would make
    /// too; two devices that disagree about a fix undo each other's for ever.
    /// `Tree::repair` reads "not held" as "missing", which is true of a whole
    /// store and false here by design, so each scope is handed to it as the
    /// honest tree it is — rooted at its scope root, holding exactly the
    /// documents that lead to it. Three things follow:
    ///
    /// - **A scope root is a root.** Its `parent_id` names a document this
    ///   page never holds, which is no orphan, and as the root of its own tree
    ///   it is never re-parented nor listed. A scope root held under another
    ///   (overlapping shares) is an ordinary node of that one's tree.
    /// - **A document that leads to no scope root is left alone**: moved out
    ///   of the share by its owner, or not under it yet.
    /// - **A scope whose lists name a document not held yet is not judged
    ///   yet**: that entry is a document on its way, and removing it would
    ///   delete a child from the owner's folder.
    fn repair_scopes(&mut self, now: &str) -> pimble_crdt::Result<Option<TreeEdit>> {
        let groups = scope_groups(&self.tree, &self.scope_roots);

        let mut edit = TreeEdit::default();
        let mut tops: Vec<NodeId> = groups.keys().copied().collect();
        tops.sort_by_key(|id| id.to_string());
        for top in tops {
            let group = &groups[&top];
            // Only a node's list is repaired (a tombstone keeps its own for an
            // undelete), so only a node's list can ask for a removal.
            let awaited = group
                .iter()
                .filter(|id| self.tree.has_node(**id))
                .filter_map(|id| self.tree.doc(*id))
                .any(|doc| {
                    doc.children()
                        .into_iter()
                        .any(|child| !self.tree.doc(child).is_some_and(|held| held.fields().is_ok()))
                });
            if awaited {
                tracing::debug!("Store {}: scope {} lists a document not held yet; not repairing it now", self.listed.id, top);
                continue;
            }

            let mut docs = HashMap::with_capacity(group.len());
            for id in group {
                if let Some(doc) = self.tree.take_doc(*id) {
                    docs.insert(*id, doc);
                }
            }
            let mut scope_tree = Tree::from_docs(top, docs);
            let repaired = scope_tree.repair(now);
            for id in group {
                if let Some(doc) = scope_tree.take_doc(*id) {
                    self.tree.insert_doc(*id, doc);
                }
            }
            if let Some(scope_edit) = repaired? {
                edit.touched.extend(scope_edit.touched);
            }
        }
        Ok((!edit.is_empty()).then_some(edit))
    }

    /// Re-root the tree when the documents say the root is another node
    /// than the one it started from: the manifest's root is a placeholder
    /// nothing has written, and exactly one node has no parent. The store
    /// as the UI should now see it, to be announced again so the tree is
    /// fetched under the real root.
    ///
    /// A store opened before its documents arrived — hosted from a desktop
    /// whose link had not pushed yet — starts from the placeholder; as the
    /// desktop's root document lands this is what moves the tree under it.
    fn adopt_root(&mut self) -> Option<Store> {
        if self.is_partial() {
            // A scope's root is the grant's, not the documents'. Nothing this
            // page holds has no parent, and the first node whose parent is
            // simply not here is not a root.
            return None;
        }
        let current = self.tree.root();
        let root = document_root(&self.tree, current);
        if root == current {
            return None;
        }
        tracing::info!("Store {}: root {} replaced by the documents' root {}", self.listed.id, current, root);
        let tree = std::mem::replace(&mut self.tree, Tree::from_docs(root, HashMap::new()));
        self.tree = rerooted(tree, root);
        Some(self.view())
    }

    /// The store as the UI should see it: what the server listed, with the
    /// tree's root.
    fn view(&self) -> Store {
        let mut store = self.listed.clone();
        store.root_node_id = self.tree.root();
        store
    }

    /// Write the root's document when the store has none: a store the
    /// accounts service has just created has an empty log and a root id in
    /// its manifest, and nothing else will ever write that document.
    ///
    /// Done at the first write under the root rather than at open, so that
    /// a store whose documents are on their way from elsewhere (a desktop's
    /// link pushing them) is not given a second root by a page that merely
    /// looked at it; two roots each repair the other under themselves and
    /// never agree again. The update is the whole document, since nothing
    /// else holds any of it.
    fn seed_root(&mut self, now: &str) -> TreeEdit {
        let root = self.tree.root();
        let mut edit = TreeEdit::default();
        // Never for a share: the shared node's document exists in the owner's
        // store, and writing one here would make a second node of the same id
        // that no repair could reconcile.
        if self.is_partial() || self.tree.has_node(root) || self.tree.doc(root).is_some_and(NodeDoc::is_initialised) {
            return edit;
        }
        let mut doc = self.tree.take_doc(root).unwrap_or_default();
        match doc.init(node_types::FOLDER, &self.listed.name, None, now) {
            Ok(()) => edit.touched.push((root, doc.save())),
            Err(e) => tracing::warn!("Seeding the root of {} failed: {}", self.listed.id, e),
        }
        self.tree.insert_doc(root, doc);
        edit
    }

    /// The children of `node_id` from the tree. The root with no document
    /// yet (see [`VaultStore::seed_root`]) has none rather than being an
    /// error: the row is there, the store is simply empty.
    fn children_of(&self, node_id: NodeId) -> Result<Vec<NodeId>, String> {
        if node_id == self.tree.root() && !self.tree.has_node(node_id) && !self.is_partial() {
            return Ok(Vec::new());
        }
        self.tree.get_children(node_id).map_err(|e| e.to_string())
    }

    /// [`VaultStore::node_of`] as the UI is handed it: with what this account
    /// may change of the node (`Node::access`), which a server puts on every
    /// node it returns and this page, holding the documents itself, judges
    /// from the account's grants (`row`) by the hosted server's own rule. It
    /// is the app's one source for what to offer: a store can hold a root the
    /// account reads beside one it edits, and the hosted server refuses the
    /// writes under the first one at a time. No row is a store nothing is
    /// known about, which is no reason to disable anything.
    fn node_for(&self, node_id: NodeId, row: Option<&AccountStore>) -> Result<Node, String> {
        let mut node = self.node_of(node_id)?;
        if !self.may_write(node_id, row) {
            node.access = StoreAccess::Read;
        }
        Ok(node)
    }

    /// Assemble the `Node` the app expects from the tree's view of a node and
    /// its document's bytes, as the server assembles one: the content is the
    /// whole node document, which the editor joins as it joined a content
    /// document (Pimble's roots ride along). The root with no document yet
    /// is a folder named after the store.
    fn node_of(&self, node_id: NodeId) -> Result<Node, String> {
        let root = self.tree.root();
        if node_id == root && !self.tree.has_node(root) && !self.is_partial() {
            let now = chrono::Utc::now();
            return Ok(Node {
                id: root,
                parent_id: None,
                node_type: node_types::FOLDER.to_string(),
                metadata: NodeMetadata {
                    title: self.listed.name.clone(),
                    created_at: now,
                    modified_at: now,
                    tags: Vec::new(),
                    custom: HashMap::new(),
                },
                content: Vec::new(),
                children: Vec::new(),
                links: Vec::new(),
                access: StoreAccess::Full,
            });
        }
        // `NodeFields::into_node` is the one mapping of stored fields to a
        // node, here and in `LocalStore`; `access` is judged per account in
        // `node_for`, never part of the node.
        let fields = self.tree.live_fields(node_id).map_err(|e| e.to_string())?;
        let children = self.tree.get_children(node_id).map_err(|e| e.to_string())?;
        let content = self.tree.doc(node_id).map(NodeDoc::save).unwrap_or_default();
        let mut node = fields.into_node(node_id);
        node.content = content;
        node.children = children;
        Ok(node)
    }

    /// [`VaultStore::node_of`] without requiring the node to be live and
    /// connected: a tombstone, or a live node no held list names, still has
    /// fields worth showing in "Recently Deleted..." even though
    /// `Tree::get_node_info`/`Tree::get_children` (which `node_of` uses)
    /// refuse a deleted document. `None` only for a document not held here at
    /// all. Content and children are left empty: the list is a name and a
    /// place to put it back, not a place to read the note itself.
    fn node_of_any(&self, node_id: NodeId) -> Option<Node> {
        let doc = self.tree.doc(node_id)?;
        let fields = doc.fields().ok()?;
        Some(fields.into_node(node_id))
    }

    /// The title of a held document, whatever its state.
    fn title_of(&self, node_id: NodeId) -> Option<String> {
        self.tree.doc(node_id).and_then(|doc| doc.fields().ok()).map(|fields| fields.title)
    }

    /// "Recently Deleted..." (docs/MOVE_CONTRACT.md "Seeing and undoing what
    /// was removed"): the top-most tombstones — a tombstoned node whose held
    /// parent is not itself a tombstone, or whose parent is not held — most
    /// recently deleted first, then the live nodes no held list names,
    /// excluding the store's root and, on a partial replica, its scope roots
    /// (named by no list by design). A share that has ended has already taken
    /// its documents with it ([`VaultStore::drop_roots`]), so nothing further
    /// is excluded for that here.
    fn list_deleted(&self, row: Option<&AccountStore>) -> Vec<DeletedNode> {
        let mut tombstones: Vec<(NodeId, NodeFields)> = self
            .tree
            .ids()
            .into_iter()
            .filter_map(|id| self.tree.doc(id).and_then(|doc| doc.fields().ok()).map(|fields| (id, fields)))
            .filter(|(_, fields)| fields.deleted_at.is_some())
            .filter(|(_, fields)| {
                fields
                    .parent_id
                    .and_then(|parent| self.tree.doc(parent))
                    .and_then(|doc| doc.fields().ok())
                    .is_none_or(|parent_fields| parent_fields.deleted_at.is_none())
            })
            .collect();
        tombstones.sort_by(|(_, a), (_, b)| b.deleted_at.cmp(&a.deleted_at));

        let mut out = Vec::with_capacity(tombstones.len());
        for (id, fields) in tombstones {
            let Some(mut node) = self.node_of_any(id) else { continue };
            if !self.may_write(id, row) {
                node.access = StoreAccess::Read;
            }
            let parent_title = fields.parent_id.and_then(|parent| self.title_of(parent));
            out.push(DeletedNode { node, parent_title, deleted_at: fields.deleted_at, put_back_under: None });
        }

        // A live node no held list names: built the way repair reads a list
        // (only a node's own list is ever repaired, so only a node's own list
        // is worth reading here — `Tree::repair`'s doc comment).
        let mut listed: HashSet<NodeId> = HashSet::new();
        for id in self.tree.list_node_ids() {
            if let Some(doc) = self.tree.doc(id) {
                listed.extend(doc.children());
            }
        }
        let roots: Vec<NodeId> = if self.is_partial() { self.scope_roots.clone() } else { vec![self.tree.root()] };
        let mut unlisted: Vec<NodeId> = self
            .tree
            .list_node_ids()
            .into_iter()
            .filter(|id| !listed.contains(id) && !roots.contains(id))
            .collect();
        unlisted.sort_by_key(|id| id.to_string());

        for id in unlisted {
            let Some(mut node) = self.node_of_any(id) else { continue };
            if !self.may_write(id, row) {
                node.access = StoreAccess::Read;
            }
            let put_back_under = if self.is_partial() {
                roots_above(&self.tree, &self.scope_roots, id).first().copied().unwrap_or_else(|| self.tree.root())
            } else {
                self.tree.root()
            };
            let parent_title = node.parent_id.and_then(|parent| self.title_of(parent));
            out.push(DeletedNode { node, parent_title, deleted_at: None, put_back_under: Some(put_back_under) });
        }

        out
    }

    /// Encrypt what goes to the server for one document: `update`, already
    /// merged into the document here, or — after a failed append, when the
    /// server is behind by more than this one change — everything it lacks.
    ///
    /// The key is the document's data key: the one this page has already
    /// resolved, the one its wraps yield, or a fresh one made here for a
    /// document the server has never seen, wrapped under every scope key this
    /// page holds that covers the node. A document from before data keys,
    /// whose blobs name a scope key directly, keeps being written under that
    /// key: giving it one is its owner's business, not a member's.
    fn prepare(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<Outgoing, String> {
        let Some(node_doc) = self.tree.doc(node_id) else {
            return Err("that node's document is not held here".to_string());
        };
        let sent_sv = node_doc.state_vector();
        let resend = self.docs.entry(node_id).or_default().unsent;
        let payload = if resend {
            let known = self.docs[&node_id].known_sv.clone();
            self.tree.doc(node_id).expect("held above").diff_since(&known).map_err(|e| e.to_string())?
        } else {
            update.to_vec()
        };

        let creating = !self.heads.contains_key(&node_id);
        let sealed = if creating {
            let parent_id = self
                .tree
                .doc(node_id)
                .and_then(|doc| doc.fields().ok())
                .and_then(|fields| fields.parent_id);
            let scope_key_ids = self.scope_key_ids_for(node_id);
            self.keys
                .create_document_key(store_id, node_id, &scope_key_ids)
                .map(|(key_id, key, keys)| (key_id, key, Some((keys, parent_id))))
        } else {
            self.keys.encrypt_key(store_id, node_id).map(|(key_id, key)| (key_id, key, None))
        };
        let Some((key_id, key, created)) = sealed else {
            // No key for this document yet: its wraps have not been fetched,
            // or none of them is under a scope key this page holds. The work
            // is already in the document, so it is marked unsent and the next
            // connection — which fetches the wraps — resends it.
            self.docs.entry(node_id).or_default().unsent = true;
            return Err(format!("no key for the document {node_id} on this device yet"));
        };

        let doc_id = VaultDocId::Node(node_id);
        let aad = blob_aad(&store_id.to_string(), &doc_id.as_str());
        let blob = URL_SAFE_NO_PAD.encode(Blob::encrypt(&key, key_id, &aad, &payload));
        Ok(Outgoing { doc_id, blob, payload, sent_sv, resend, created })
    }

    /// The scope keys that cover a node: the store key, when this page holds
    /// the whole store, and the key of every share the node sits under, which
    /// each share's root names in its own marker (`NodeMetadata::share()`).
    ///
    /// A scoped member with no marker on the way up — a share from before
    /// markers, or a root whose document has not arrived — falls back to the
    /// keys it was given, which are its shares'.
    fn scope_key_ids_for(&self, node_id: NodeId) -> Vec<KeyId> {
        let mut ids: Vec<KeyId> = Vec::new();
        if !self.is_partial() {
            ids.extend(self.keys.scope.key_id_for(None));
        }
        let mut cur = Some(node_id);
        for _ in 0..=self.tree.ids().len() {
            let Some(fields) = cur.and_then(|id| self.tree.doc(id)).and_then(|doc| doc.fields().ok()) else { break };
            if let Some(marker) = share_marker_of(&fields) {
                ids.push(marker.key_id);
            }
            cur = fields.parent_id;
        }
        if ids.is_empty() {
            ids.extend(self.keys.scope.scopes.iter().map(|(_, id)| *id));
        }
        ids.retain(|id| self.keys.scope.get(id).is_some());
        ids.dedup();
        ids
    }

    /// Record how an append went. `Ok(Some(seq))` says a snapshot stamped
    /// `seq` is due; `Err` is the append's failure, recorded against the
    /// document so the next append (or the next connect) resends it.
    fn record(&mut self, node_id: NodeId, outgoing: &Outgoing, appended: Result<u64, String>) -> Result<Option<u64>, String> {
        let doc = self.docs.entry(node_id).or_default();
        let seq = match appended {
            Ok(seq) => seq,
            Err(message) => {
                doc.unsent = true;
                return Err(message);
            }
        };
        if outgoing.resend {
            doc.known_sv = outgoing.sent_sv.clone();
            doc.unsent = false;
        } else {
            advance(&mut doc.known_sv, &outgoing.payload);
        }
        doc.cursor.mark(seq);
        doc.appends += 1;
        let head = self.heads.entry(node_id).or_insert(0);
        *head = (*head).max(seq);

        // The server deletes every log entry at or below a snapshot's number,
        // so it may only be stamped with a number read *through*. Another
        // client's append just below ours may still be on its way; until it
        // lands the snapshot waits for a later append.
        let due = doc.appends >= SNAPSHOT_EVERY && doc.cursor.applied_through() == seq;
        Ok(due.then_some(seq))
    }

    /// Forget everything this page holds of one document, because the server
    /// refused an append to it.
    ///
    /// There is nothing to resend — the same bytes would be refused for ever,
    /// and a document left `unsent` would offer them again on every reconnect
    /// — and nothing in the local document is worth keeping either: a yrs
    /// document cannot un-merge the edit that was refused, so showing it would
    /// show work that exists nowhere and never will. Dropping it is what makes
    /// the next fetch start from the beginning and end at what the server
    /// holds.
    fn refused(&mut self, node_id: NodeId) {
        self.drop_doc(node_id);
    }

    /// Let go of one document and everything kept about it.
    fn drop_doc(&mut self, node_id: NodeId) {
        self.tree.take_doc(node_id);
        self.docs.remove(&node_id);
        self.heads.remove(&node_id);
        self.keys.forget(node_id);
        self.look.unread.remove(&node_id);
        self.look.asked.retain(|(id, _)| *id != node_id);
    }

    /// Let go of the shares rooted at `ended`: they are no longer scope roots
    /// of this page, and every document under one of them and under no root
    /// still held is dropped (overlapping shares: what is also under a share
    /// that is left stays). Answers `false`, having changed nothing, when no
    /// scope root would be left: such a store is not this function's to
    /// keep open (with no scope root it would read as a whole store).
    fn drop_roots(&mut self, ended: &[NodeId]) -> bool {
        let left: Vec<NodeId> = self.scope_roots.iter().copied().filter(|root| !ended.contains(root)).collect();
        if left.is_empty() {
            return false;
        }
        let kept: HashSet<NodeId> = left.iter().flat_map(|root| self.tree.subtree_ids(*root).unwrap_or_default()).collect();
        let dropped: Vec<NodeId> = ended
            .iter()
            .filter(|root| self.scope_roots.contains(root))
            .flat_map(|root| self.tree.subtree_ids(*root).unwrap_or_else(|_| vec![*root]))
            .filter(|id| !kept.contains(id))
            .collect();
        for id in dropped {
            self.drop_doc(id);
        }
        self.scope_roots = left;
        self.listed.roots.retain(|root| !ended.contains(root));
        // The tree starts from the first scope root for its life; when that
        // one went, it starts from the first that is left.
        if ended.contains(&self.tree.root()) {
            let root = self.scope_roots[0];
            self.listed.root_node_id = root;
            let tree = std::mem::replace(&mut self.tree, Tree::from_docs(root, HashMap::new()));
            self.tree = rerooted(tree, root);
        }
        true
    }

    /// The document's whole state, encrypted, for a snapshot. Under the same
    /// key its updates go out under: a snapshot replaces them.
    fn snapshot_blob(&mut self, store_id: StoreId, node_id: NodeId) -> Result<String, String> {
        let full = self.tree.doc(node_id).map(NodeDoc::save).unwrap_or_default();
        let (key_id, key) = self
            .keys
            .encrypt_key(store_id, node_id)
            .ok_or_else(|| format!("no key for the document {node_id} on this device"))?;
        let aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
        Ok(URL_SAFE_NO_PAD.encode(Blob::encrypt(&key, key_id, &aad, &full)))
    }
}

/// The share's marker a node carries, when it is a share's root
/// (`pimble_core::NodeMetadata::share`, read from a document's fields).
fn share_marker_of(fields: &NodeFields) -> Option<pimble_core::ShareMarker> {
    fields
        .custom
        .get(custom_keys::SHARE)
        .and_then(|value| serde_json::from_value::<pimble_core::ShareMarker>(value.clone()).ok())
}

/// `roots` (nearest first, from [`pimble_crdt::Tree::shares`] or
/// [`pimble_crdt::Tree::shares_left`]) resolved to what the move notice and
/// "Recently Deleted..." show: each root's [`pimble_core::ShareMarker`] name,
/// or its title when the marker does not parse (docs/MOVE_CONTRACT.md
/// "Seeing and undoing what was removed").
fn as_left_shares(tree: &Tree, roots: Vec<NodeId>) -> Vec<LeftShare> {
    roots
        .into_iter()
        .map(|root| {
            let name = tree
                .doc(root)
                .and_then(|doc| doc.fields().ok())
                .map(|fields| share_marker_of(&fields).map(|marker| marker.name).unwrap_or(fields.title))
                .unwrap_or_default();
            LeftShare { root, name }
        })
        .collect()
}

/// Ask the accounts service for the keys of the shares in a whole store that
/// this page has seen a marker of and holds no key for, and take what comes
/// into the keyring. Whether a key came that was not held.
///
/// `GET /stores/{id}/keys?root=<node>` answers the caller's own envelopes for
/// that root, and a whole-store grant may ask about any root. An owner has
/// one for every share: the device that made the share sealed its key to the
/// owner's own account before it wrote the marker. Nothing there
/// ([`KeyError::NoneYet`]) is normal for anyone else who holds the whole store
/// and is quiet; a request that failed is asked again like a waiting store's.
///
/// Never for a member's page, and never more often than
/// [`SHARE_KEY_FLOOR_MS`] per store: [`VaultStore::share_keys_to_ask_for`].
async fn fetch_share_keys(store_id: StoreId, store: &mut VaultStore) -> bool {
    let now = now_ms();
    let Some(roots) = store.share_keys_to_ask_for(now) else { return false };
    match keys::fetch_scope_keyring(&store_id.to_string(), &roots).await {
        Ok(more) => {
            let new = store.take_keys(more);
            if new {
                tracing::info!("Store {}: holding the key of a share in it that was not held", store_id);
            }
            new
        }
        Err(KeyError::NoneYet) => false,
        Err(KeyError::Failed(message)) => {
            tracing::warn!("Asking for the keys of the shares in {} failed: {}", store_id, message);
            store.look.want_at(now + KEY_RETRY_MS);
            false
        }
    }
}

/// Which scope each document belongs to: the last scope root on its stored
/// parent chain through held documents (tombstones included — a deleted
/// document stays with its scope), and nothing when it reaches none. Bounded,
/// so an unrepaired cycle cannot loop. The grouping
/// [`VaultStore::repair_scopes`] repairs one tree at a time.
fn scope_groups(tree: &Tree, scope_roots: &[NodeId]) -> HashMap<NodeId, Vec<NodeId>> {
    let roots: HashSet<NodeId> = scope_roots.iter().copied().collect();
    let ids = tree.ids();
    let bound = ids.len();
    let mut groups: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for &id in &ids {
        let mut cur = id;
        let mut top = None;
        for _ in 0..=bound {
            if roots.contains(&cur) {
                top = Some(cur);
            }
            match tree.doc(cur).and_then(|d| d.fields().ok()).and_then(|f| f.parent_id) {
                Some(parent) if tree.doc(parent).is_some() => cur = parent,
                _ => break,
            }
        }
        if let Some(top) = top {
            groups.entry(top).or_default().push(id);
        }
    }
    groups
}

/// The `roots` on `id`'s stored parent chain, itself included, through held
/// documents (tombstones too), nearest first: the scopes a document is in, as
/// the desktop's `LocalStore` reads them for the same judgement. Bounded, so
/// an unrepaired cycle cannot loop.
fn roots_above(tree: &Tree, roots: &[NodeId], id: NodeId) -> Vec<NodeId> {
    let mut reached = Vec::new();
    if roots.is_empty() {
        return reached;
    }
    let mut cur = id;
    for _ in 0..=tree.ids().len() {
        if roots.contains(&cur) && !reached.contains(&cur) {
            reached.push(cur);
        }
        match tree.doc(cur).and_then(|d| d.fields().ok()).and_then(|f| f.parent_id) {
            Some(parent) if tree.doc(parent).is_some() => cur = parent,
            _ => break,
        }
    }
    reached
}

// ── Keys ────────────────────────────────────────────────────────────────────

impl StoreKeys {
    fn new(scope: Keyring) -> Self {
        Self { scope, wraps: HashMap::new(), deks: HashMap::new(), listed_dek: HashMap::new(), creating: HashMap::new() }
    }

    /// Take a `vaultFetch`'s answer about a document's data key. `None` is a
    /// document from before data keys, whose blobs name a scope key; anything
    /// held under a different `dek_id` is a rotation and is forgotten.
    fn note_wraps(&mut self, node_id: NodeId, keys: Option<VaultDocKeys>) {
        match keys {
            Some(keys) => {
                if self.deks.get(&node_id).is_some_and(|(id, _)| *id != keys.dek_id) {
                    self.deks.remove(&node_id);
                }
                self.listed_dek.insert(node_id, keys.dek_id);
                self.wraps.insert(node_id, keys);
            }
            None => {
                self.wraps.remove(&node_id);
                self.listed_dek.remove(&node_id);
            }
        }
    }

    /// Remember the key this page made for a document it created, so its own
    /// next blob resolves without a round trip.
    fn note_created(&mut self, node_id: NodeId, keys: &VaultDocKeys) {
        self.creating.remove(&node_id);
        self.listed_dek.insert(node_id, keys.dek_id);
        self.wraps.insert(node_id, keys.clone());
    }

    /// Every document's data key as it was is forgotten: the logs they were
    /// read from are gone (`VaultStore::forget_remote_logs`), and a twin
    /// built again gives each document a new one. The scope keys stay; they
    /// are the account's, not the twin's.
    fn forget_documents(&mut self) {
        self.wraps.clear();
        self.deks.clear();
        self.listed_dek.clear();
        self.creating.clear();
    }

    /// Forget one document's keys, for a document being pulled again from the
    /// beginning: whatever the server says about it then is the truth,
    /// including a data key this page made for a create it refused.
    fn forget(&mut self, node_id: NodeId) {
        self.wraps.remove(&node_id);
        self.deks.remove(&node_id);
        self.listed_dek.remove(&node_id);
        self.creating.remove(&node_id);
    }

    /// The key a blob whose header names `key_id` was written under, in the
    /// order the contract gives (see the struct's own documentation).
    fn blob_key(&mut self, store_id: StoreId, node_id: NodeId, key_id: KeyId) -> Option<SymmetricKey> {
        if let Some((id, key)) = self.deks.get(&node_id) {
            if *id == key_id {
                return Some(key.clone());
            }
        }
        if let Some(keys) = self.wraps.get(&node_id).filter(|keys| keys.dek_id == key_id) {
            let aad = dek_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
            for wrap in &keys.wraps {
                let Some(scope_key) = self.scope.get(&wrap.scope_key_id) else { continue };
                match unwrap_dek(wrap, scope_key, &aad) {
                    Ok(dek) => {
                        self.deks.insert(node_id, (key_id, dek.clone()));
                        return Some(dek);
                    }
                    Err(e) => tracing::warn!(
                        "The wrap of {} under the scope key {} did not open ({})",
                        node_id,
                        wrap.scope_key_id,
                        e
                    ),
                }
            }
        }
        // A blob from before data keys names a scope key itself.
        self.scope.get(&key_id).cloned()
    }

    /// The key an outgoing blob for `node_id` is written under: its data key
    /// when it has one this page can open, and otherwise the scope key a
    /// document from before data keys is still written under. `None` when the
    /// document has a data key nothing here opens — its wraps have not been
    /// fetched, or none is under a key this account holds.
    fn encrypt_key(&mut self, store_id: StoreId, node_id: NodeId) -> Option<(KeyId, SymmetricKey)> {
        match self.listed_dek.get(&node_id).copied() {
            Some(dek_id) => self.blob_key(store_id, node_id, dek_id).map(|key| (dek_id, key)),
            None => self.scope.current.and_then(|id| self.scope.get(&id).cloned().map(|key| (id, key))),
        }
    }

    /// A fresh data key for a document nothing has seen, wrapped under every
    /// scope key in `scope_key_ids` this page holds.
    fn create_document_key(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        scope_key_ids: &[KeyId],
    ) -> Option<(KeyId, SymmetricKey, VaultDocKeys)> {
        // A create being tried again: the same key as the first try.
        if let (Some(keys), Some((dek_id, dek))) = (self.creating.get(&node_id), self.deks.get(&node_id)) {
            if keys.dek_id == *dek_id {
                return Some((*dek_id, dek.clone(), keys.clone()));
            }
        }
        let dek = SymmetricKey::generate();
        let dek_id = KeyId::new_v4();
        let aad = dek_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
        let mut wraps: Vec<WrappedDek> = Vec::new();
        for scope_key_id in scope_key_ids {
            if wraps.iter().any(|w| w.scope_key_id == *scope_key_id) {
                continue;
            }
            if let Some(scope_key) = self.scope.get(scope_key_id) {
                wraps.push(wrap_dek(&dek, scope_key, *scope_key_id, &aad));
            }
        }
        if wraps.is_empty() {
            return None;
        }
        let keys = VaultDocKeys { dek_id, wraps };
        self.deks.insert(node_id, (dek_id, dek.clone()));
        self.creating.insert(node_id, keys.clone());
        Some((dek_id, dek, keys))
    }

    /// The key a blob names, when nothing here yields it: what told a blob
    /// that will not open for want of a key from one that is simply bad.
    fn key_wanted(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str) -> Option<KeyId> {
        let blob = decode_blob(encoded).ok()?;
        let key_id = Blob::key_id(&blob).ok()?;
        self.blob_key(store_id, node_id, key_id).is_none().then_some(key_id)
    }

    /// Open one blob of one document.
    fn decrypt_blob(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str) -> Result<Vec<u8>, String> {
        let blob = decode_blob(encoded)?;
        let key_id = Blob::key_id(&blob).map_err(|e| e.to_string())?;
        let key = self
            .blob_key(store_id, node_id, key_id)
            .ok_or_else(|| format!("no key {key_id} for the document {node_id} on this device"))?;
        let aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
        Blob::decrypt(&key, &aad, &blob).map_err(|e| e.to_string())
    }

    /// Every blob in a fetch, snapshot first, decrypted in order.
    fn decrypt_entries(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        fetched: &VaultFetchResponse,
    ) -> Vec<(Mark, Result<Vec<u8>, String>)> {
        let mut out = Vec::with_capacity(fetched.updates.len() + 1);
        if let Some(entry) = &fetched.snapshot {
            out.push((Mark::Through(entry.seq), self.decrypt_blob(store_id, node_id, &entry.blob)));
        }
        for entry in &fetched.updates {
            out.push((Mark::One(entry.seq), self.decrypt_blob(store_id, node_id, &entry.blob)));
        }
        out
    }
}

/// A rename as the shared command path makes one: the title, and the flag
/// that stops the tree deriving a label from the content. Two transactions
/// on one document, appended as two blobs.
fn rename_edit(tree: &mut Tree, node_id: NodeId, title: &str, now: &str) -> pimble_crdt::Result<TreeEdit> {
    let mut edit = tree.set_title(node_id, title, now)?;
    let flag = tree.set_custom(node_id, custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
    edit.touched.extend(flag.touched);
    Ok(edit)
}

/// An appearance change: `None` for a field leaves it alone; `Some(None)`
/// (or an empty string, which `NodeMetadata::icon` and `color` already read
/// as "not set") removes the key, as `NodeMetadata::set_icon` does.
fn appearance_edit(
    tree: &mut Tree,
    node_id: NodeId,
    icon: Option<Option<String>>,
    color: Option<Option<String>>,
    tags: Option<Vec<String>>,
    now: &str,
) -> pimble_crdt::Result<TreeEdit> {
    let mut edit = TreeEdit::default();
    for (key, value) in [(custom_keys::ICON, icon), (custom_keys::COLOR, color)] {
        let Some(value) = value else { continue };
        let more = match value.filter(|s| !s.is_empty()) {
            Some(value) => tree.set_custom(node_id, key, &serde_json::Value::String(value), now)?,
            None => tree.remove_custom(node_id, key, now)?,
        };
        edit.touched.extend(more.touched);
    }
    if let Some(tags) = tags {
        edit.touched.extend(tree.set_tags(node_id, &tags, now)?.touched);
    }
    Ok(edit)
}

/// The root the documents say a store has: `current` (the manifest's root,
/// or what the tree starts from now) when it is a node with no `parent_id`;
/// otherwise the one node with no `parent_id`, when there is exactly one;
/// otherwise `current` (no documents yet, or several claim to be it, which
/// only a repair from a known root settles). The rule of
/// `pimble_store::LocalStore::document_root`, applied here.
fn document_root(tree: &Tree, current: NodeId) -> NodeId {
    let is_root_like = |id: NodeId| tree.get_node_info(id).is_ok_and(|info| info.parent_id.is_none());
    if is_root_like(current) {
        return current;
    }
    let mut candidates = tree.list_node_ids().into_iter().filter(|&id| is_root_like(id));
    match (candidates.next(), candidates.next()) {
        (Some(only), None) => only,
        _ => current,
    }
}

/// The same documents under another root. `Tree` starts from one root for
/// its life, so a tree that learns its real root is rebuilt around it.
fn rerooted(mut tree: Tree, root: NodeId) -> Tree {
    let ids = tree.ids();
    let mut docs: HashMap<NodeId, NodeDoc> = HashMap::with_capacity(ids.len());
    for id in ids {
        if let Some(doc) = tree.take_doc(id) {
            docs.insert(id, doc);
        }
    }
    Tree::from_docs(root, docs)
}

// ── Deriving notifications from a merged update ─────────────────────────────
//
// Copied from `crates/pimble-server/src/handler.rs` (`DocShape`, `shape_of`,
// `derive_kinds`), so a browser holding the documents tells the UI exactly
// what the server would. The two should become one implementation in
// `pimble-crdt`.

/// What a node document says about the node before or after a merge: the
/// `node` root as fields (`None` while the document is not initialised) and
/// the children list as stored.
struct DocShape {
    fields: Option<NodeFields>,
    children: Vec<NodeId>,
}

impl DocShape {
    /// The key the node's share marker names, when it carries one.
    fn share_key_id(&self) -> Option<KeyId> {
        self.fields.as_ref().and_then(share_marker_of).map(|marker| marker.key_id)
    }
}

/// The shape of `id`'s document, `None` when the tree holds none.
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
/// values) still earns `TreeStructure`. `root` stands in for a parent no
/// document names.
fn derive_kinds(
    node_id: NodeId,
    root: NodeId,
    before: Option<&DocShape>,
    after: Option<&DocShape>,
    effect: NodeUpdateEffect,
) -> Vec<StoreChangeKind> {
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

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Which store a command is about, when it is about one.
///
/// Public because the backend loop needs it before the vault client does: a
/// command is answered through the endpoint that serves its store, whichever
/// kind of store that is.
pub fn store_id_of(cmd: &BackendCommand) -> Option<StoreId> {
    use BackendCommand::*;
    Some(match cmd {
        CloseStore { store_id }
        | CreateNode { store_id, .. }
        | GetNode { store_id, .. }
        | GetChildren { store_id, .. }
        | SetNodeContent { store_id, .. }
        | RenameNode { store_id, .. }
        | SetNodeAppearance { store_id, .. }
        | DeleteNode { store_id, .. }
        | MoveNode { store_id, .. }
        | UndeleteNode { store_id, .. }
        | ListDeleted { store_id }
        | CreateMount { store_id, .. }
        | GetMountState { store_id, .. }
        | BroadcastChanges { store_id, .. }
        | ReconcileNodeContent { store_id, .. }
        | SubscribeStoreChanges { store_id }
        | SubscribeNodeChanges { store_id, .. }
        | RebuildIndex { store_id }
        | SetStoreSync { store_id, .. }
        | GetStoreSync { store_id }
        | RemoveReplica { store_id, .. } => *store_id,
        MountRemoteStore { target_store_id, .. } => *target_store_id,
        _ => return None,
    })
}

/// Whether a command changes anything. A reader's is refused before it is
/// sent; everything else a `Read` store answers as it always did.
///
/// Listed by hand rather than by exclusion: a command added later should have
/// to be thought about once rather than quietly become a write a reader may
/// make.
pub fn writes(cmd: &BackendCommand) -> bool {
    use BackendCommand::*;
    matches!(
        cmd,
        CreateNode { .. }
            | RenameNode { .. }
            | SetNodeAppearance { .. }
            | DeleteNode { .. }
            | UndeleteNode { .. }
            | MoveNode { .. }
            | TransplantNode { .. }
            | SetNodeContent { .. }
            | BroadcastChanges { .. }
            | CreateMount { .. }
            | MountRemoteStore { .. }
            | SetStoreSync { .. }
            | RemoveReplica { .. }
    )
}

/// Whether a command carries what the editor has already put on screen: the
/// two writes a store whose owner's computer is off still takes in
/// ([`VaultClient::refuse_write`]).
fn is_content_edit(cmd: &BackendCommand) -> bool {
    matches!(cmd, BackendCommand::BroadcastChanges { .. } | BackendCommand::SetNodeContent { .. })
}

/// The placeholder root of a store listed from the account's row alone
/// ([`VaultClient::offline_row`]): the store's own id, so it is the same every
/// time and names no node of anybody's.
fn offline_root(store_id: StoreId) -> NodeId {
    NodeId(store_id.0)
}

/// An empty folder to be read: what the placeholder root of a store that
/// cannot be reached answers as.
fn empty_folder(node_id: NodeId, title: &str) -> Node {
    let now = chrono::Utc::now();
    Node {
        id: node_id,
        parent_id: None,
        node_type: node_types::FOLDER.to_string(),
        metadata: NodeMetadata { title: title.to_string(), created_at: now, modified_at: now, tags: Vec::new(), custom: HashMap::new() },
        content: Vec::new(),
        children: Vec::new(),
        links: Vec::new(),
        access: StoreAccess::Read,
    }
}

/// Whether a failed request was the server refusing rather than failing: a
/// reader's write, or a document in none of this account's scopes. Nothing
/// later changes such an answer, so it is never retried and never left
/// pending — it is shown.
pub fn is_refusal(message: &str) -> bool {
    StoreAccess::refusal_in(message).is_some() || message.starts_with("Forbidden: ")
}

/// What a `TransplantNode` earns when one of its stores is encrypted and the
/// other is plain (docs/MOVE_CONTRACT.md "Between stores"): the vault client
/// does a transplant between two stores it holds, and a plain store's is the
/// hosted server's to do; the two are not one operation yet.
pub const MIXED_TRANSPLANT_REFUSAL: &str = "Moving a node between an encrypted store and a plain one is not supported yet.";

/// Whether a `TransplantNode` needs refusing because its two stores are not
/// the same kind. `None` when both are encrypted (the vault client's to do,
/// [`VaultClient::transplant_node`]) or both are plain (`process_command`'s,
/// through whichever endpoint serves the source, as any other plain-store
/// write).
pub fn refuse_mixed_transplant(from_vault: bool, to_vault: bool) -> Option<BackendEvent> {
    (from_vault != to_vault).then(|| BackendEvent::Error { message: MIXED_TRANSPLANT_REFUSAL.to_string() })
}

/// What a `TransplantNode` between two plain stores earns when different
/// endpoints serve them (docs/MOVE_CONTRACT.md "Between stores"): the server
/// holding the source does the transplant, and it can only plant into a
/// store it holds too. Today every plain store a page sees is the hosted
/// server's (a store served from its owner's computer is always encrypted),
/// so this is the fence for the day that changes, not a path anyone reaches.
pub const SPLIT_TRANSPLANT_REFUSAL: &str = "Moving a node between stores on different servers is not supported yet.";

/// Whether a plain-to-plain `TransplantNode` needs refusing because its two
/// stores are served by different endpoints. `None` when one server holds
/// both, which is when `process_command` can hand it to that server.
pub fn refuse_split_transplant(from_url: &str, to_url: &str) -> Option<BackendEvent> {
    (from_url != to_url).then(|| BackendEvent::Error { message: SPLIT_TRANSPLANT_REFUSAL.to_string() })
}

/// A sentence written for the person rather than an error: the app shows it in
/// the status bar and clears it again on a timer
/// (`pimble_app::events::show_notice`, reached by a message it recognises as
/// meant to be read as it is). There is no `BackendEvent` of its own for this
/// yet; when one lands, this is the single place that changes.
fn notice(sentence: String) -> BackendEvent {
    BackendEvent::Error { message: format!("Forbidden: {sentence}") }
}

/// A command's name, for the one error a vault store answers with.
fn name_of(cmd: &BackendCommand) -> &'static str {
    use BackendCommand::*;
    match cmd {
        CreateMount { .. } => "Mounting",
        GetMountState { .. } => "Mount state",
        SetStoreSync { .. } => "Linking to a remote",
        RemoveReplica { .. } => "Removing a replica",
        CloseStore { .. } => "Closing a store",
        _ => "That",
    }
}

/// The timestamp this page's edits carry (`created_at`, `modified_at`,
/// `deleted_at`), rfc3339 like the server's. The crdt crate never reads a
/// clock; what two replicas write is decided by their callers alone.
fn now_rfc3339() -> String {
    js_sys::Date::new_0().to_iso_string().as_string().unwrap_or_default()
}

/// The page's clock, in milliseconds.
fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// About 120 characters around the match, or the start of the text.
fn snippet_around(text: &str, at: Option<usize>) -> String {
    if text.is_empty() {
        return String::new();
    }
    let start = at.unwrap_or(0).saturating_sub(40);
    let start = floor_char_boundary(text, start);
    let end = floor_char_boundary(text, (start + 160).min(text.len()));
    let mut snippet = text[start..end].replace('\n', " ");
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < text.len() {
        snippet.push('…');
    }
    snippet
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// base64url without padding is what the contract specifies; the padded and
/// standard alphabets are accepted too, so a server that encodes either way
/// still reads.
fn decode_blob(encoded: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(encoded)
        .or_else(|_| URL_SAFE.decode(encoded))
        .or_else(|_| STANDARD.decode(encoded))
        .map_err(|e| format!("the blob would not decode: {e}"))
}

/// What applying a fetched entry says about the log: a snapshot stands for
/// everything up to its number, an update for its own number only.
#[derive(Clone, Copy)]
enum Mark {
    Through(u64),
    One(u64),
}

impl Mark {
    /// Called only once the entry has been applied; an entry that would not
    /// decrypt or merge is never marked, so the cursor stops in front of it.
    fn apply(self, cursor: &mut VaultCursor) {
        match self {
            Mark::Through(seq) => cursor.mark_through(seq),
            Mark::One(seq) => cursor.mark(seq),
        }
    }
}

/// One document's `vaultFetch` answer, or why there is none.
type Fetched = (NodeId, Result<VaultFetchResponse, String>);

/// `vaultFetch` for several documents at once: `(node id, after seq)` in,
/// each answer (or failure) out, in windows of [`FETCH_WINDOW`].
///
/// The browser has one thread and the futures have to be driven together, so
/// each is handed to the page as a promise and the window is `Promise.all`;
/// every promise resolves whatever the fetch did, with the outcome kept
/// aside, so one refused document never fails the window.
async fn fetch_many(client: &Arc<PimbleClient>, store_id: StoreId, wanted: Vec<(NodeId, u64)>) -> Vec<Fetched> {
    let results: Rc<RefCell<Vec<Fetched>>> = Rc::new(RefCell::new(Vec::with_capacity(wanted.len())));
    for window in wanted.chunks(FETCH_WINDOW) {
        let promises = js_sys::Array::new();
        for &(node_id, after_seq) in window {
            let client = client.clone();
            let results = results.clone();
            promises.push(&wasm_bindgen_futures::future_to_promise(async move {
                let fetched = client
                    .vault_fetch(store_id, VaultDocId::Node(node_id), after_seq)
                    .await
                    .map_err(|e| e.to_string());
                results.borrow_mut().push((node_id, fetched));
                Ok(wasm_bindgen::JsValue::NULL)
            }));
        }
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::all(&promises)).await;
    }
    let collected = std::mem::take(&mut *results.borrow_mut());
    collected
}

/// `update` is on the server (this client appended it, or fetched it): move
/// the record of what the server holds. It only moves when the update continues
/// from what is already known, so it can lag, never overstate.
fn advance(known_sv: &mut Vec<u8>, update: &[u8]) {
    if let Ok(next) = pimble_crdt::advance_state_vector(known_sv, update) {
        *known_sv = next;
    }
}

/// Whether an incoming `VaultAppended` should be applied.
///
/// Authorship is the only thing that can decide this, and the notification
/// carries it: a blob this client sent comes back with its own id on it, and
/// everything else is somebody else's work. A sequence number cannot stand in
/// for authorship. Several clients append to one document and the server hands
/// out its numbers in arrival order, so one client's appends are interleaved
/// with another's, and "not newer than the last number I was given" throws away
/// exactly the updates that arrived while this client was typing.
///
/// When in doubt this applies. A yrs merge is idempotent, so a genuine echo
/// applied twice changes nothing, while a dropped update is gone for good.
pub fn should_apply(source_client_id: Option<&str>, my_client_id: &str) -> bool {
    match source_client_id {
        Some(source) => source != my_client_id,
        // Unattributed: an append made by something that did not name itself,
        // or by a server that does not stamp one. Applying is the safe
        // reading — a merge repeated is nothing, a merge missed is a lost
        // edit.
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_crypto::SymmetricKey;
    use std::path::PathBuf;

    const T0: &str = "2026-09-18T10:00:00Z";
    const T1: &str = "2026-09-18T10:00:01Z";
    const T2: &str = "2026-09-18T10:00:02Z";

    // ── Fixtures ────────────────────────────────────────────────────────────

    /// A keyring holding one scope key, as an owner's page has for a whole
    /// store (`root` `None`) or a member's for one share.
    fn keyring_for(root: Option<NodeId>) -> Keyring {
        let key_id = KeyId::new_v4();
        Keyring {
            keys: HashMap::from([(key_id, SymmetricKey::generate())]),
            current: Some(key_id),
            scopes: vec![(root, key_id)],
        }
    }

    fn keyring() -> Keyring {
        keyring_for(None)
    }

    fn store_keys() -> StoreKeys {
        StoreKeys::new(keyring())
    }

    fn listed(root: NodeId) -> Store {
        let mut store = Store::new_local("Vault", PathBuf::new());
        store.root_node_id = root;
        store.kind = StoreKind::Vault;
        store
    }

    /// A peer's tree, whose edits play the part of what another device
    /// appended to the hosted logs.
    fn origin() -> (Tree, NodeId) {
        let root = NodeId::new();
        (Tree::new(root, "Vault", T0).unwrap(), root)
    }

    /// The whole state of every document `tree` holds, as one pull: what a
    /// fresh page fetches (each document's log collapsed to a snapshot).
    fn pull_of(tree: &Tree) -> Vec<Pulled> {
        tree.ids()
            .into_iter()
            .map(|id| Pulled {
                node_id: id,
                entries: vec![(Mark::Through(1), Ok(tree.doc(id).unwrap().save()))],
                head: 1,
            })
            .collect()
    }

    /// A store page holding a copy of `tree`, as it is after an open.
    fn opened(tree: &Tree) -> VaultStore {
        VaultStore::assemble(listed(tree.root()), store_keys(), Vec::new(), pull_of(tree))
    }

    /// Feed the updates of `edit` to `store` one at a time, as the
    /// notifications of one peer's edit arrive; `seq` numbers them.
    fn arrive(store: &mut VaultStore, store_id: StoreId, edit: &TreeEdit, seq: &mut u64, active: Option<NodeId>) -> (Vec<BackendEvent>, bool) {
        let mut events = Vec::new();
        let mut structure = false;
        for (id, update) in &edit.touched {
            *seq += 1;
            let merged = store.merge(store_id, *id, vec![(Mark::One(*seq), Ok(update.clone()))], active == Some(*id), None);
            events.extend(merged.events);
            structure |= merged.structure;
        }
        (events, structure)
    }

    fn kinds_of(events: &[BackendEvent]) -> Vec<StoreChangeKind> {
        events
            .iter()
            .filter_map(|e| match e {
                BackendEvent::RemoteStoreChange { change_kind, .. } => Some(change_kind.clone()),
                _ => None,
            })
            .collect()
    }

    // ── The tree from documents ─────────────────────────────────────────────

    #[test]
    fn the_tree_is_built_from_the_documents_and_follows_their_updates() {
        let (mut peer, root) = origin();
        let (folder, x, y) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(folder, Some(root), None, "folder", "F", T0).unwrap();
        peer.add_node(x, Some(root), None, "document", "x", T0).unwrap();
        peer.add_node(y, Some(folder), None, "document", "y", T0).unwrap();

        let store_id = StoreId::new();
        let mut store = opened(&peer);
        assert_eq!(store.tree.root(), root);
        assert_eq!(store.children_of(root).unwrap(), vec![folder, x]);
        assert_eq!(store.children_of(folder).unwrap(), vec![y]);
        assert!(store.tree.validate_tree().is_empty());
        assert!(store.repair_now(T1).is_none(), "a consistent pull needs no repair");
        for id in [root, folder, x, y] {
            assert_eq!(store.docs[&id].cursor.applied_through(), 1, "each document's cursor covers its snapshot");
        }

        let mut seq = 10;
        // A create arriving as two document updates.
        let z = NodeId::new();
        let edit = peer.add_node(z, Some(folder), Some(0), "document", "z", T1).unwrap();
        arrive(&mut store, store_id, &edit, &mut seq, None);
        assert_eq!(store.children_of(folder).unwrap(), vec![z, y]);
        assert_eq!(store.node_of(z).unwrap().metadata.title, "z");

        // A move: three documents' updates.
        let edit = peer.move_node(y, root, None, T1).unwrap();
        arrive(&mut store, store_id, &edit, &mut seq, None);
        assert_eq!(store.children_of(folder).unwrap(), vec![z]);
        assert_eq!(store.children_of(root).unwrap(), vec![folder, x, y]);
        assert_eq!(store.node_of(y).unwrap().parent_id, Some(root));

        // A delete: tombstones for the subtree, and the parent's list.
        let edit = peer.remove_node(folder, T2).unwrap();
        arrive(&mut store, store_id, &edit, &mut seq, None);
        assert_eq!(store.children_of(root).unwrap(), vec![x, y]);
        assert!(store.node_of(folder).is_err(), "a tombstone is not a node");
        assert!(store.node_of(z).is_err());
        assert!(store.tree.doc(z).is_some(), "the document stays");
        assert!(store.tree.validate_tree().is_empty());
        assert!(store.repair_now(T2).is_none());
    }

    #[test]
    fn the_root_is_the_manifest_s_unless_the_documents_name_another() {
        let (mut peer, real_root) = origin();
        let x = NodeId::new();
        peer.add_node(x, Some(real_root), None, "document", "x", T0).unwrap();

        // A store hosted from a desktop: the manifest carries a placeholder
        // the desktop's documents never mention.
        let placeholder = NodeId::new();
        let store = VaultStore::assemble(listed(placeholder), store_keys(), Vec::new(), pull_of(&peer));
        assert_eq!(store.tree.root(), real_root);
        assert_eq!(store.view().root_node_id, real_root);
        assert_eq!(store.children_of(real_root).unwrap(), vec![x]);

        // An empty log keeps the manifest's root, answers an empty tree for
        // it, and describes it as a folder named after the store.
        let mut empty = VaultStore::assemble(listed(placeholder), store_keys(), Vec::new(), Vec::new());
        assert_eq!(empty.tree.root(), placeholder);
        assert_eq!(empty.children_of(placeholder).unwrap(), Vec::<NodeId>::new());
        assert_eq!(empty.node_of(placeholder).unwrap().metadata.title, "Vault");
        assert!(empty.repair_now(T1).is_none());

        // The first write under it seeds the root document, and only once.
        let seed = empty.seed_root(T1);
        assert_eq!(seed.node_ids(), vec![placeholder]);
        assert!(empty.tree.has_node(placeholder));
        assert!(empty.seed_root(T1).is_empty());
        let y = NodeId::new();
        empty.tree.add_node(y, Some(placeholder), None, "document", "y", T1).unwrap();
        assert_eq!(empty.children_of(placeholder).unwrap(), vec![y]);
    }

    #[test]
    fn a_root_arriving_after_the_open_re_roots_the_tree_once() {
        // Opened empty, before a desktop's link pushed anything; the
        // person only looked, so nothing was seeded here.
        let placeholder = NodeId::new();
        let store_id = StoreId::new();
        let mut store = VaultStore::assemble(listed(placeholder), store_keys(), Vec::new(), Vec::new());

        let (mut peer, real_root) = origin();
        let x = NodeId::new();
        let edit = peer.add_node(x, Some(real_root), None, "document", "x", T0).unwrap();
        let mut seq = 0;
        // The child's document lands first: no root to adopt yet.
        arrive(&mut store, store_id, &TreeEdit { touched: vec![edit.touched[0].clone()] }, &mut seq, None);
        assert!(store.adopt_root().is_none());
        // The root's document: the tree moves under it and the UI is told.
        let root_update = peer.doc(real_root).unwrap().save();
        let merged = store.merge(store_id, real_root, vec![(Mark::One(1), Ok(root_update))], false, None);
        assert!(merged.structure);
        let announced = store.adopt_root().expect("the documents' root replaces the placeholder");
        assert_eq!(announced.root_node_id, real_root);
        assert_eq!(store.tree.root(), real_root);
        assert_eq!(store.children_of(real_root).unwrap(), vec![x]);
        assert!(store.adopt_root().is_none(), "settled");
    }

    // ── What a tree edit appends ────────────────────────────────────────────

    #[test]
    fn a_tree_edit_is_appended_per_document_in_order() {
        let (mut peer, root) = origin();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let x = NodeId::new();
        let edit = store.tree.add_node(x, Some(root), None, "document", "x", T1).unwrap();
        assert_eq!(edit.node_ids(), vec![x, root], "the new document before its parent's list");

        let mut seq = 0;
        for (node_id, update) in &edit.touched {
            let outgoing = store.prepare(store_id, *node_id, update).unwrap();
            assert_eq!(outgoing.doc_id, VaultDocId::Node(*node_id));
            assert!(!outgoing.resend);
            // The blob is that document's update and nothing else, under
            // this store's and this document's associated data.
            let opened = store.keys.decrypt_blob(store_id, *node_id, &outgoing.blob).unwrap();
            assert_eq!(&opened, update);
            assert!(
                store.keys.decrypt_blob(store_id, NodeId::new(), &outgoing.blob).is_err(),
                "bound to its document"
            );
            seq += 1;
            assert_eq!(store.record(*node_id, &outgoing, Ok(seq)).unwrap(), None);
            let doc = &store.docs[node_id];
            assert_eq!(doc.cursor.applied_through(), seq, "our own append counts as read");
            assert!(!doc.unsent);
        }
        // A peer applying the blobs in that order ends with the same tree.
        for (node_id, update) in &edit.touched {
            peer.apply_update(*node_id, update).unwrap();
        }
        assert_eq!(peer.get_children(root).unwrap(), vec![x]);
        assert_eq!(peer.get_node_info(x).unwrap().title, "x");
        assert!(peer.validate_tree().is_empty());
    }

    #[test]
    fn a_failed_append_is_resent_as_everything_the_server_lacks() {
        let (peer, root) = origin();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let known_before = store.docs[&root].known_sv.clone();

        // A rename whose append fails.
        let first = store.tree.set_title(root, "One", T1).unwrap();
        let (_, update) = &first.touched[0];
        let outgoing = store.prepare(store_id, root, update).unwrap();
        assert!(store.record(root, &outgoing, Err("socket closed".into())).is_err());
        assert!(store.docs[&root].unsent);
        assert_eq!(store.docs[&root].known_sv, known_before, "nothing moved");

        // The next edit carries both: a diff since what the server holds.
        let second = store.tree.set_title(root, "Two", T2).unwrap();
        let (_, update) = &second.touched[0];
        let outgoing = store.prepare(store_id, root, update).unwrap();
        assert!(outgoing.resend);
        let mut fresh = NodeDoc::load(&peer.doc(root).unwrap().save()).unwrap();
        fresh.apply_update(&outgoing.payload).unwrap();
        assert_eq!(fresh.fields().unwrap().title, "Two", "the resend brings the server level");
        assert_eq!(store.record(root, &outgoing, Ok(7)).unwrap(), None);
        assert!(!store.docs[&root].unsent);
        assert_eq!(store.docs[&root].known_sv, store.tree.doc(root).unwrap().state_vector());
    }

    #[test]
    fn a_snapshot_is_due_only_when_the_cursor_has_read_through_its_number() {
        let (peer, root) = origin();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let mut seq = 1;
        for i in 0..SNAPSHOT_EVERY {
            let edit = store.tree.set_title(root, &format!("t{i}"), T1).unwrap();
            let (_, update) = &edit.touched[0];
            let outgoing = store.prepare(store_id, root, update).unwrap();
            seq += 1;
            // Somebody else's append at 100 never arrived: from then on the
            // cursor stops short of our own numbers.
            if seq == 100 {
                seq += 1;
            }
            let due = store.record(root, &outgoing, Ok(seq)).unwrap();
            assert_eq!(due, None, "append {i}: not until the gap closes");
        }
        // The missing entry lands; the next append may snapshot.
        store.docs.get_mut(&root).unwrap().cursor.mark(100);
        let edit = store.tree.set_title(root, "last", T2).unwrap();
        let outgoing = store.prepare(store_id, root, &edit.touched[0].1).unwrap();
        seq += 1;
        assert_eq!(store.record(root, &outgoing, Ok(seq)).unwrap(), Some(seq));
    }

    // ── What the UI hears ───────────────────────────────────────────────────

    #[test]
    fn merged_structural_updates_earn_the_kinds_the_server_derives() {
        let (mut peer, root) = origin();
        let (p, q) = (NodeId::new(), NodeId::new());
        peer.add_node(p, Some(root), None, "folder", "P", T0).unwrap();
        peer.add_node(q, Some(root), None, "folder", "Q", T0).unwrap();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let mut seq = 0;

        // Create: the node's own document is `NodeCreated`, the parent's
        // list `TreeStructure`.
        let x = NodeId::new();
        let edit = peer.add_node(x, Some(p), None, "document", "x", T1).unwrap();
        let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
        assert!(structure);
        assert!(matches!(kinds_of(&events)[..], [StoreChangeKind::NodeCreated { node_id, parent_id }, StoreChangeKind::TreeStructure { ref node_ids }] if node_id == x && parent_id == p && node_ids == &vec![p]), "{events:?}");

        // Rename: `MetadataUpdated`.
        let edit = peer.set_title(x, "Renamed", T1).unwrap();
        let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
        assert!(matches!(kinds_of(&events)[..], [StoreChangeKind::MetadataUpdated { node_id }] if node_id == x), "{events:?}");

        // Move: `NodeMoved` for the node, `TreeStructure` for both lists.
        let edit = peer.move_node(x, q, None, T1).unwrap();
        let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
        let kinds = kinds_of(&events);
        assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeMoved { node_id, old_parent_id, new_parent_id } if *node_id == x && *old_parent_id == p && *new_parent_id == q)), "{kinds:?}");
        assert_eq!(kinds.iter().filter(|k| matches!(k, StoreChangeKind::TreeStructure { .. })).count(), 2, "{kinds:?}");

        // Delete: `NodeDeleted` naming the parent it left.
        let edit = peer.remove_node(x, T2).unwrap();
        let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
        let kinds = kinds_of(&events);
        assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeDeleted { node_id, parent_id } if *node_id == x && *parent_id == q)), "{kinds:?}");

        // The same bytes again change nothing and say nothing.
        let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
        assert!(events.is_empty() && !structure);
    }

    #[test]
    fn a_content_update_reaches_the_editor_only_for_the_node_it_has_open() {
        let (mut peer, root) = origin();
        let x = NodeId::new();
        peer.add_node(x, Some(root), None, "document", "x", T0).unwrap();
        let store_id = StoreId::new();
        let mut store = opened(&peer);

        let delta = peer.doc_mut(x).unwrap().replace_plain_text("typed elsewhere").unwrap();
        let edit = TreeEdit { touched: vec![(x, delta.clone())] };
        let mut seq = 0;

        // Not open here: the UI refetches the node.
        let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
        assert!(!structure, "content is not structure");
        assert!(matches!(events[..], [BackendEvent::NodeContentUpdated { node_id, .. }] if node_id == x), "{events:?}");
        assert_eq!(store.tree.doc(x).unwrap().text(), "typed elsewhere");

        // Open in the editor: the delta itself, base64 as the plain path
        // carries it.
        let mut again = opened(&peer);
        let delta = peer.doc_mut(x).unwrap().replace_plain_text("and more").unwrap();
        let edit = TreeEdit { touched: vec![(x, delta.clone())] };
        let (events, _) = arrive(&mut again, store_id, &edit, &mut seq, Some(x));
        assert!(matches!(&events[..], [BackendEvent::RemoteChanges { changes }] if *changes == STANDARD.encode(&delta)), "{events:?}");
    }

    // ── Repair only after a whole pull ──────────────────────────────────────

    #[test]
    fn a_pull_is_repaired_once_after_the_whole_of_it() {
        let (mut peer, root) = origin();
        let (p, q, x) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(p, Some(root), None, "folder", "P", T0).unwrap();
        peer.add_node(q, Some(root), None, "folder", "Q", T0).unwrap();
        peer.add_node(x, Some(p), None, "document", "x", T0).unwrap();
        let before = pull_of(&peer);
        let before_the_move = opened(&peer);
        let edit = peer.move_node(x, q, None, T1).unwrap();
        assert_eq!(edit.touched.len(), 3);

        // The pull holds the snapshots and, after them, the move's three
        // updates in log order. Applied whole, the tree is consistent and
        // the one repair afterwards has nothing to do.
        let mut pulled = before;
        for (id, update) in &edit.touched {
            let doc = pulled.iter_mut().find(|d| d.node_id == *id).unwrap();
            doc.entries.push((Mark::One(2), Ok(update.clone())));
            doc.head = 2;
        }
        let mut store = VaultStore::assemble(listed(root), store_keys(), Vec::new(), pulled);
        assert!(store.tree.validate_tree().is_empty(), "{:?}", store.tree.validate_tree());
        assert!(store.repair_now(T2).is_none());
        assert_eq!(store.children_of(q).unwrap(), vec![x]);
        assert!(store.children_of(p).unwrap().is_empty());

        // Judged after each update it would have written something: after
        // P's removal alone, x is listed nowhere, and repair's answer is to
        // append it to P again, an edit that then travels. On the live path
        // the merge itself never repairs; it only says a repair is wanted,
        // and by the time the debounce fires the rest of the edit is in.
        let mut half = before_the_move;
        let store_id = StoreId::new();
        let (id, update) = &edit.touched[0];
        assert_eq!(*id, p, "the old parent's list goes first");
        let merged = half.merge(store_id, *id, vec![(Mark::One(2), Ok(update.clone()))], false, None);
        assert!(merged.structure, "flagged for the debounced repair");
        assert!(!half.tree.validate_tree().is_empty(), "half a move is inconsistent");
        assert!(half.repair_due.is_none(), "the merge itself schedules nothing");
        for (id, update) in &edit.touched[1..] {
            half.merge(store_id, *id, vec![(Mark::One(2), Ok(update.clone()))], false, None);
        }
        assert!(half.tree.validate_tree().is_empty());
        assert!(half.repair_now(T2).is_none(), "nothing left to write once the whole edit is in");
        assert_eq!(half.children_of(q).unwrap(), vec![x]);
    }

    #[test]
    fn a_repair_writes_what_the_tree_needs_and_names_it() {
        let (mut peer, root) = origin();
        let x = NodeId::new();
        peer.add_node(x, Some(root), None, "document", "x", T0).unwrap();
        let mut store = opened(&peer);
        // A duplicate a concurrent append left in the root's list.
        store.tree.doc_mut(root).unwrap().insert_child(99, x).unwrap();
        let edit = store.repair_now(T1).expect("a duplicate needs a repair");
        assert_eq!(edit.node_ids(), vec![root]);
        assert!(store.tree.validate_tree().is_empty());
        assert_eq!(store.children_of(root).unwrap(), vec![x]);
    }

    // ── Which notifications are applied ─────────────────────────────────────

    #[test]
    fn a_client_skips_only_its_own_work() {
        assert!(!should_apply(Some("me"), "me"));
        assert!(should_apply(Some("someone-else"), "me"));
    }

    #[test]
    fn an_unattributed_notification_is_applied() {
        // What the server sends today: `VaultAppendRequest` carries no client
        // id, so nothing is suppressed and every blob is merged.
        assert!(should_apply(None, "me"));
    }

    #[test]
    fn interleaved_appends_from_two_clients_all_arrive() {
        // Two tabs typing in turn. The server numbers appends in arrival
        // order, so each client's own ids are scattered through the other's —
        // which is why a sequence number cannot stand in for authorship. Every
        // update written by the other client must be applied, whether its
        // number is above or below anything this client was last given.
        let log: [(u64, &str); 6] = [
            (1, "a"),
            (2, "b"),
            (3, "a"),
            (4, "b"),
            (5, "b"),
            (6, "a"),
        ];

        let seen_by_a: Vec<u64> = log
            .iter()
            .filter(|(_, who)| should_apply(Some(who), "a"))
            .map(|(seq, _)| *seq)
            .collect();
        let seen_by_b: Vec<u64> = log
            .iter()
            .filter(|(_, who)| should_apply(Some(who), "b"))
            .map(|(seq, _)| *seq)
            .collect();

        assert_eq!(seen_by_a, vec![2, 4, 5], "a must see every append b made");
        assert_eq!(seen_by_b, vec![1, 3, 6], "b must see every append a made");
    }

    #[test]
    fn a_late_number_is_not_a_reason_to_drop() {
        // The shape of the bug this replaced: b's append lands between two of
        // a's, so its number is below the last one a was given. It is still b's
        // work and still has to be applied.
        assert!(should_apply(Some("b"), "a"));
    }

    // ── A share is a scope of somebody else's store ─────────────────────────

    fn grant(root: Option<NodeId>, role: &str, name: &str) -> AccountGrant {
        AccountGrant { root, role: role.to_string(), name: name.to_string() }
    }

    fn row(grants: Vec<AccountGrant>, shared_by: Option<&str>) -> AccountStore {
        AccountStore { kind: "vault".to_string(), grants, shared_by: shared_by.map(str::to_string), relayed: false }
    }

    /// The same row for a store served from its owner's computer.
    fn relayed_row(grants: Vec<AccountGrant>, shared_by: Option<&str>) -> AccountStore {
        AccountStore { relayed: true, ..row(grants, shared_by) }
    }

    #[test]
    fn the_account_s_grants_decide_a_store_s_access_and_roots() {
        let (a, b) = (NodeId::new(), NodeId::new());

        // One's own store: one whole-store grant, nothing scoped.
        let mine = row(vec![grant(None, "owner", "Family")], None);
        assert_eq!(mine.access(), StoreAccess::Full);
        assert!(mine.roots().is_empty(), "a whole store has no scope roots");
        assert_eq!(mine.name(), None, "the server's name stands for a whole store");

        // A reader of one folder: read-only, one root, the share's own name.
        let reading = row(vec![grant(Some(a), "reader", "Recipes")], Some("ann@example.com"));
        assert_eq!(reading.access(), StoreAccess::Read);
        assert_eq!(reading.roots(), vec![a]);
        assert_eq!(reading.name().as_deref(), Some("Recipes"));
        assert_eq!(reading.shared_by.as_deref(), Some("ann@example.com"));

        // A reader of one folder and an editor of another: `Full`, because a
        // store-wide `Read` would take away the editing they do have. The
        // server refuses the writes under the reader's root one at a time.
        let mixed = row(vec![grant(Some(a), "reader", "Recipes"), grant(Some(b), "editor", "Plans")], Some("ann@example.com"));
        assert_eq!(mixed.access(), StoreAccess::Full);
        assert_eq!(mixed.roots(), vec![a, b], "both shares are roots of the same store");
        assert_eq!(mixed.name().as_deref(), Some("Shared by ann@example.com"), "several shares: the desktop's words, not the first share's name");

        // A whole-store grant beside a share covers everything, so the shares
        // stop being scopes at all.
        let both = row(vec![grant(Some(a), "reader", "Recipes"), grant(None, "editor", "Family")], None);
        assert_eq!(both.access(), StoreAccess::Full);
        assert!(both.roots().is_empty());
        assert_eq!(both.name(), None);

        // Nothing known about a store is not a reason to refuse writes: the
        // server is the authority and answers with the sentence itself.
        assert_eq!(AccountStore::default().access(), StoreAccess::Full);
    }

    #[test]
    fn a_listed_store_reaches_the_ui_as_the_share_it_is() {
        let root = NodeId::new();
        let store_id = StoreId::new();
        let mut client = VaultClient::new("me".to_string());
        client.rows.insert(
            store_id,
            row(vec![grant(Some(root), "reader", "Recipes")], Some("ann@example.com")),
        );

        // What the hosted server lists: the owner's store name, which a
        // recipient must never see.
        let mut listed = Store::new_local("Ann's whole life", PathBuf::new());
        listed.id = store_id;
        listed.kind = StoreKind::Vault;
        client.describe(&mut listed);

        assert_eq!(listed.name, "Recipes", "the share's own name, never the owner's store's");
        assert_eq!(listed.shared_by.as_deref(), Some("ann@example.com"));
        assert_eq!(listed.access, StoreAccess::Read);
        assert_eq!(listed.roots, vec![root]);
        assert_eq!(listed.root_node_id, root, "older callers read the first root");

        // Waiting for the key says so on the row, and only once it is waiting.
        assert!(!listed.name.ends_with(WAITING_SUFFIX));
        client.waiting.insert(store_id, Waiting { listed: listed.clone(), due: 0.0 });
        let mut again = listed.clone();
        again.name = "Ann's whole life".to_string();
        client.describe(&mut again);
        assert_eq!(again.name, format!("Recipes{WAITING_SUFFIX}"));
    }

    // ── A store served from its owner's computer ────────────────────────────

    /// A page with one relayed share in the account's list: "Recipes", shared
    /// by ann, which this account may edit.
    fn page_with_a_relayed_share() -> (VaultClient, StoreId, NodeId) {
        let (store_id, root) = (StoreId::new(), NodeId::new());
        let mut client = VaultClient::new("me".to_string());
        client.rows.insert(store_id, relayed_row(vec![grant(Some(root), "editor", "Recipes")], Some("ann@example.com")));
        client.set_relayed(vec![store_id]);
        (client, store_id, root)
    }

    fn sync_of(event: &BackendEvent) -> (StoreAccess, RelaySide, bool) {
        match event {
            BackendEvent::StoreSyncChanged { access, relay, owner_offline, .. } => (*access, *relay, *owner_offline),
            other => panic!("not a sync answer: {other:?}"),
        }
    }

    /// The endpoint of a store this page never opened is down: the row comes
    /// from the account's own list (the share's name, who shared it) and says
    /// `owner offline`, once (docs/RELAY_CONTRACT.md, "The apps").
    #[test]
    fn a_store_whose_owner_is_offline_is_listed_from_the_account_s_row() {
        let (mut client, store_id, _) = page_with_a_relayed_share();

        let events = client.endpoint_down(&[store_id]);
        assert_eq!(events.len(), 2, "{events:?}");
        let BackendEvent::StoresListed { stores } = &events[0] else { panic!("no row: {events:?}") };
        let listed = &stores[0];
        assert_eq!(listed.id, store_id);
        assert_eq!(listed.name, "Recipes", "the share's own name");
        assert_eq!(listed.shared_by.as_deref(), Some("ann@example.com"));
        assert_eq!(listed.kind, StoreKind::Vault);
        assert_eq!(listed.relay, RelaySide::Member);
        assert_eq!(listed.access, StoreAccess::Read, "nothing can be changed of a store that is not there");
        assert!(listed.roots.is_empty(), "the row stands alone: nothing under it is known");
        assert_eq!(sync_of(&events[1]), (StoreAccess::Read, RelaySide::Member, true));

        // Every failed attempt after the first has nothing new to say.
        assert!(client.endpoint_down(&[store_id]).is_empty());

        // A row the account's list could not be read for still has a name,
        // the desktop's for the same thing.
        let unnamed = StoreId::new();
        client.set_relayed(vec![store_id, unnamed]);
        let events = client.endpoint_down(&[unnamed]);
        let BackendEvent::StoresListed { stores } = &events[0] else { panic!("no row: {events:?}") };
        assert_eq!(stores[0].name, "Shared from another computer");
        assert_eq!(stores[0].shared_by, None);
    }

    /// Such a store opens read-only and empty rather than erroring: its row,
    /// an empty folder, no list, and a sentence for anything that would
    /// change it. Nothing here needs a connection, and nothing is an `Error`
    /// the status bar would show as one.
    #[test]
    fn a_store_whose_owner_is_offline_opens_empty_and_to_be_read() {
        let (mut client, store_id, _) = page_with_a_relayed_share();
        let placeholder = NodeId(store_id.0);

        // Before anyone has said it is down, it is nobody's to answer here.
        assert!(matches!(client.handle_unreachable(BackendCommand::GetChildren { store_id, node_id: placeholder }), Handled::No(_)));

        client.endpoint_down(&[store_id]);
        let answered = |handled: Handled| match handled {
            Handled::Yes(event) => event,
            Handled::No(cmd) => panic!("handed back: {cmd:?}"),
        };

        let children = answered(client.handle_unreachable(BackendCommand::GetChildren { store_id, node_id: placeholder }));
        assert!(
            matches!(&children, Some(BackendEvent::ChildrenLoaded { children, parent_id, .. }) if children.is_empty() && *parent_id == placeholder),
            "{children:?}"
        );
        let node = answered(client.handle_unreachable(BackendCommand::GetNode { store_id, node_id: placeholder }));
        let Some(BackendEvent::NodeLoaded { node, .. }) = node else { panic!("no node: {node:?}") };
        assert_eq!(node.metadata.title, "Recipes");
        assert_eq!(node.node_type, node_types::FOLDER);
        assert_eq!(node.access, StoreAccess::Read);
        assert!(node.children.is_empty());

        // The UI's registration of a store: nothing to subscribe to yet, and
        // a sync answer that says why.
        assert!(answered(client.handle_unreachable(BackendCommand::SubscribeStoreChanges { store_id })).is_none());
        let sync = answered(client.handle_unreachable(BackendCommand::GetStoreSync { store_id })).unwrap();
        assert_eq!(sync_of(&sync), (StoreAccess::Read, RelaySide::Member, true));

        // A write is refused with the sentence that says why, before anything
        // is asked of anyone; a read is not.
        let refused = client
            .refuse_write(store_id, &BackendCommand::CreateNode { store_id, parent_id: None, title: "New".into() })
            .expect("nothing can be created in a store that cannot be reached");
        assert!(
            matches!(&refused, BackendEvent::Error { message } if message == &format!("Forbidden: {OWNER_OFFLINE_REFUSAL}")),
            "{refused:?}"
        );
        assert!(client.refuse_write(store_id, &BackendCommand::GetNode { store_id, node_id: placeholder }).is_none());

        // Another store's commands are none of this.
        let other = StoreId::new();
        assert!(matches!(client.handle_unreachable(BackendCommand::GetChildren { store_id: other, node_id: placeholder }), Handled::No(_)));
    }

    /// When the endpoint answers, the store is opened like any hosted one and
    /// announced as the store it is, so the tree fetches what is in it with no
    /// reload: the share's root under the row, the account's real access, and
    /// `owner offline` gone.
    #[test]
    fn a_store_fills_in_when_its_owner_is_back() {
        let (mut client, store_id, _) = page_with_a_relayed_share();
        client.endpoint_down(&[store_id]);

        // What `open_listed` does once the endpoint lists the store: the
        // share's documents, pulled and held. (The relay face lists it to a
        // member as "Shared with you".)
        let (mut peer, root) = origin();
        peer.add_node(NodeId::new(), Some(root), None, "document", "Pasta", T0).unwrap();
        client.rows.insert(store_id, relayed_row(vec![grant(Some(root), "editor", "Recipes")], Some("ann@example.com")));
        let mut listed = Store::new_local("Shared with you", PathBuf::new());
        listed.id = store_id;
        listed.kind = StoreKind::Vault;
        let held = VaultStore::assemble(listed.clone(), store_keys(), vec![root], pull_of(&peer));
        client.stores.insert(store_id, held);

        let events = client.endpoint_up(&[store_id], &[listed]);
        assert_eq!(events.len(), 2, "{events:?}");
        let BackendEvent::StoreOpened { store } = &events[0] else { panic!("not announced: {events:?}") };
        assert_eq!(store.name, "Recipes");
        assert_eq!(store.roots, vec![root]);
        assert_eq!(store.access, StoreAccess::Full, "an editor edits again");
        assert_eq!(store.relay, RelaySide::Member);
        assert_eq!(sync_of(&events[1]), (StoreAccess::Full, RelaySide::Member, false));

        // And it answers like any open store from here on.
        assert!(matches!(client.handle_unreachable(BackendCommand::GetChildren { store_id, node_id: root }), Handled::No(_)));
        assert!(client.refuse_write(store_id, &BackendCommand::CreateNode { store_id, parent_id: Some(root), title: "New".into() }).is_none());
        let BackendEvent::ChildrenLoaded { children, .. } = client.get_children(store_id, root) else { panic!("no list") };
        assert!(!children.is_empty());
        assert!(children.iter().all(|node| node.access == StoreAccess::Full));

        // An endpoint that was never down has nothing to announce.
        assert!(client.endpoint_up(&[store_id], &[]).is_empty());
    }

    /// A store this page holds when its owner's computer goes off is read from
    /// what is held, says so on its row, and takes no change until it is back,
    /// except what the editor had already put on screen, which is kept for
    /// the next connection rather than refused from under it.
    #[test]
    fn a_held_store_whose_owner_goes_offline_is_read_and_not_changed() {
        let (mut client, store_id, _) = page_with_a_relayed_share();
        let (mut peer, root) = origin();
        peer.add_node(NodeId::new(), Some(root), None, "document", "Pasta", T0).unwrap();
        client.rows.insert(store_id, relayed_row(vec![grant(None, "editor", "")], None));
        client.stores.insert(store_id, opened(&peer));

        let events = client.endpoint_down(&[store_id]);
        assert_eq!(events.len(), 1, "a store the UI already has is not listed again: {events:?}");
        assert_eq!(sync_of(&events[0]), (StoreAccess::Read, RelaySide::Member, true));

        let Handled::Yes(Some(BackendEvent::ChildrenLoaded { children, .. })) =
            client.handle_unreachable(BackendCommand::GetChildren { store_id, node_id: root })
        else {
            panic!("the held list was not served")
        };
        assert!(!children.is_empty(), "what is held is still read");
        assert!(children.iter().all(|node| node.access == StoreAccess::Read));
        let mut described = listed(root);
        described.id = store_id;
        client.describe(&mut described);
        assert_eq!(described.access, StoreAccess::Read);

        let rename = BackendCommand::RenameNode { store_id, node_id: root, title: "No".into() };
        assert!(client.refuse_write(store_id, &rename).is_some());

        // A keystroke already on its way: merged and kept, not refused.
        let mut editor = NodeDoc::load(&peer.doc(root).unwrap().save()).unwrap();
        let before = editor.state_vector();
        let typed = {
            let mut tree = Tree::from_docs(root, HashMap::from([(root, NodeDoc::load(&editor.save()).unwrap())]));
            let edit = tree.set_title(root, "Typed while it went", T1).unwrap();
            edit.touched[0].1.clone()
        };
        editor.apply_update(&typed).unwrap();
        assert_ne!(editor.state_vector(), before);
        let keystroke = BackendCommand::BroadcastChanges { store_id, node_id: root, changes: STANDARD.encode(&typed) };
        assert!(client.refuse_write(store_id, &keystroke).is_none());
        assert!(matches!(client.handle_unreachable(keystroke), Handled::Yes(None)));
        let held = &client.stores[&store_id];
        assert!(held.docs[&root].unsent, "the next connection has nothing to resend");
        assert_eq!(held.tree.doc(root).unwrap().fields().unwrap().title, "Typed while it went");
    }

    /// A store the session says is relayed is one whether or not the
    /// account's list could be read, and a whole store with no name anywhere
    /// but in its own documents is called by its root's title once that is
    /// open here.
    #[test]
    fn a_relayed_store_is_described_as_one() {
        let store_id = StoreId::new();
        let mut client = VaultClient::new("me".to_string());
        client.set_relayed(vec![store_id]);
        assert!(client.is_relayed(store_id));
        assert!(client.is_encrypted(store_id));

        // The twin is made without a name, and Pimble Cloud holds none.
        let mut unnamed = Store::new_local("", PathBuf::new());
        unnamed.id = store_id;
        unnamed.kind = StoreKind::Vault;
        let mut before_open = unnamed.clone();
        client.describe(&mut before_open);
        assert_eq!(before_open.relay, RelaySide::Member);
        assert_eq!(before_open.name, "Shared from another computer");

        let (peer, _) = origin();
        let title = peer.get_node_info(peer.root()).unwrap().title;
        client.stores.insert(store_id, opened(&peer));
        let mut open = unnamed.clone();
        client.describe(&mut open);
        assert_eq!(open.name, title);

        // A hosted store is not called any of this.
        let mut hosted = Store::new_local("", PathBuf::new());
        hosted.kind = StoreKind::Vault;
        client.describe(&mut hosted);
        assert_eq!(hosted.relay, RelaySide::None);
        assert_eq!(hosted.name, "");

        // A session that stops naming a store stops waiting for it.
        client.endpoint_down(&[store_id]);
        client.set_relayed(Vec::new());
        assert!(matches!(client.handle_unreachable(BackendCommand::GetStoreSync { store_id }), Handled::No(_)));
    }

    // ── A twin built again ──────────────────────────────────────────────────

    /// A relayed store's twin is disposable: built again, its logs start from
    /// 1 under a new epoch, and a page that kept its cursors would skip them.
    /// The first epoch heard is only remembered, the same one again changes
    /// nothing, and a server that names none concludes nothing.
    #[test]
    fn a_new_epoch_makes_the_page_forget_what_it_read() {
        let (mut peer, root) = origin();
        peer.add_node(NodeId::new(), Some(root), None, "document", "Pasta", T0).unwrap();
        let store_id = StoreId::new();
        let mut store = opened(&peer);
        let child = peer.get_children(root).unwrap()[0];
        store.keys.listed_dek.insert(child, KeyId::new_v4());

        // An edit this page made and the old twin took, moments before its
        // owner's computer went off: the owner's own store never had it, so
        // the twin built again from that store does not either.
        let edit = store.tree.set_title(root, "Renamed here", T1).unwrap();
        let outgoing = store.prepare(store_id, root, &edit.touched[0].1).unwrap();
        store.record(root, &outgoing, Ok(2)).unwrap();
        let read_to = store.docs[&root].cursor.applied_through();
        let known = store.docs[&root].known_sv.clone();
        assert_eq!(read_to, 2);
        assert_eq!(known, store.tree.doc(root).unwrap().state_vector());

        assert!(!store.take_epoch(None, None));
        assert!(!store.take_epoch(Some("2026-09-21T10:00:00Z"), None), "the first epoch heard is remembered, nothing more");
        assert!(!store.take_epoch(Some("2026-09-21T10:00:00Z"), None));
        assert!(!store.take_epoch(None, None));
        assert_eq!(store.docs[&root].cursor.applied_through(), read_to);
        assert_eq!(store.docs[&root].known_sv, known);
        assert!(!store.docs[&root].unsent);

        // The twin was built again.
        assert!(store.take_epoch(Some("2026-09-21T11:30:00Z"), None));
        for id in peer.ids() {
            let doc = &store.docs[&id];
            assert_eq!(doc.cursor.applied_through(), 0, "{id} would be read from where the old log was read to");
            assert_eq!(doc.known_sv, pimble_crdt::empty_state_vector(), "{id}: the old twin's holdings are believed of the new one");
            assert_eq!(doc.appends, 0);
            assert!(doc.unsent, "{id} is not offered to the new twin");
        }
        assert!(store.heads.is_empty());
        assert!(store.keys.listed_dek.is_empty() && store.keys.wraps.is_empty() && store.keys.deks.is_empty());
        assert!(store.keys.scope.current.is_some(), "the account's own keys went with the twin's");
        assert_eq!(store.tree.doc(root).unwrap().fields().unwrap().title, "Renamed here", "the notes are this page's, not the twin's");
        // Remembered: the same epoch again is not another rebuild.
        assert!(!store.take_epoch(Some("2026-09-21T11:30:00Z"), None));

        // Read again from the start (the new logs are one entry each, as the
        // owner's link pushed them): what this page already holds merges to
        // nothing and says nothing to the UI, and the cursors are the new
        // logs' own.
        for id in peer.ids() {
            let merged = store.merge(store_id, id, vec![(Mark::Through(1), Ok(peer.doc(id).unwrap().save()))], false, None);
            assert!(merged.events.is_empty() && !merged.structure, "{id}: a merge repeated said something");
            store.heads.insert(id, 1);
            assert_eq!(store.docs[&id].cursor.applied_through(), 1, "{id}");
        }

        // Offered again: everything beyond what the new twin is now known to
        // hold, which brings it the edit only this page had.
        let outgoing = store.prepare(store_id, root, &[]).unwrap();
        assert!(outgoing.resend);
        assert!(outgoing.created.is_none(), "the new twin lists it: an append must not try to create it");
        let mut twin = NodeDoc::load(&peer.doc(root).unwrap().save()).unwrap();
        assert_eq!(twin.fields().unwrap().title, "Vault");
        twin.apply_update(&outgoing.payload).unwrap();
        assert_eq!(twin.fields().unwrap().title, "Renamed here", "what only this page held was lost with the old twin");
        assert_eq!(store.record(root, &outgoing, Ok(2)).unwrap(), None);
        assert!(!store.docs[&root].unsent);
        assert_eq!(store.docs[&root].known_sv, store.tree.doc(root).unwrap().state_vector());
    }

    /// What this account may only read is read again and never offered: the
    /// server would refuse it, and a refused append throws the local copy
    /// away.
    #[test]
    fn a_new_epoch_offers_only_what_the_account_may_write() {
        let (peer, root) = origin();
        let mut reader = VaultStore::assemble(listed(root), store_keys(), vec![root], pull_of(&peer));
        let reads = relayed_row(vec![grant(Some(root), "reader", "Recipes")], Some("ann@example.com"));
        reader.take_epoch(Some("a"), Some(&reads));
        assert!(reader.take_epoch(Some("b"), Some(&reads)));
        for id in peer.ids() {
            assert_eq!(reader.docs[&id].cursor.applied_through(), 0);
            assert!(!reader.docs[&id].unsent, "a reader's page offered {id}");
        }

        let mut editor = VaultStore::assemble(listed(root), store_keys(), vec![root], pull_of(&peer));
        let edits = relayed_row(vec![grant(Some(root), "editor", "Recipes")], Some("ann@example.com"));
        editor.take_epoch(Some("a"), Some(&edits));
        assert!(editor.take_epoch(Some("b"), Some(&edits)));
        assert!(peer.ids().iter().all(|id| editor.docs[id].unsent));
    }

    // ── Which key opens a blob ──────────────────────────────────────────────

    #[test]
    fn a_blob_s_key_is_the_document_s_data_key_then_a_scope_key() {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        let doc = VaultDocId::Node(node_id);
        let aad = blob_aad(&store_id.to_string(), &doc.as_str());

        // This page holds one share's key and not the store's.
        let share_root = NodeId::new();
        let share = keyring_for(Some(share_root));
        let share_key_id = share.current.unwrap();
        let share_key = share.current_key().unwrap().clone();
        let mut keys = StoreKeys::new(share);

        // A document whose data key is wrapped under the share key, and under
        // a store key this page does not hold.
        let dek = SymmetricKey::generate();
        let dek_id = KeyId::new_v4();
        let dek_aad = dek_aad(&store_id.to_string(), &doc.as_str());
        let stranger = SymmetricKey::generate();
        let wraps = VaultDocKeys {
            dek_id,
            wraps: vec![
                wrap_dek(&dek, &stranger, KeyId::new_v4(), &dek_aad),
                wrap_dek(&dek, &share_key, share_key_id, &dek_aad),
            ],
        };
        keys.note_wraps(node_id, Some(wraps));

        let blob = URL_SAFE_NO_PAD.encode(Blob::encrypt(&dek, dek_id, &aad, b"under the data key"));
        assert_eq!(keys.decrypt_blob(store_id, node_id, &blob).unwrap(), b"under the data key");
        assert!(keys.deks.contains_key(&node_id), "an unwrapped data key is kept");
        // And an outgoing blob goes under the same data key.
        assert_eq!(keys.encrypt_key(store_id, node_id).unwrap().0, dek_id);

        // A blob from before data keys names a scope key in its header; the
        // third step of the order finds it.
        let other = NodeId::new();
        let old_aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(other).as_str());
        let old = URL_SAFE_NO_PAD.encode(Blob::encrypt(&share_key, share_key_id, &old_aad, b"before data keys"));
        assert_eq!(keys.decrypt_blob(store_id, other, &old).unwrap(), b"before data keys");
        assert_eq!(
            keys.encrypt_key(store_id, other).unwrap().0,
            share_key_id,
            "a document with no data key keeps being written under the scope key"
        );

        // A data key wrapped under nothing this page holds is not a failure to
        // report, only a document that stays shut until the wrap arrives.
        let shut = NodeId::new();
        let shut_dek = SymmetricKey::generate();
        let shut_dek_id = KeyId::new_v4();
        let shut_doc = VaultDocId::Node(shut);
        let shut_dek_aad = dek_aad_of(store_id, shut);
        keys.note_wraps(
            shut,
            Some(VaultDocKeys { dek_id: shut_dek_id, wraps: vec![wrap_dek(&shut_dek, &stranger, KeyId::new_v4(), &shut_dek_aad)] }),
        );
        let shut_blob = URL_SAFE_NO_PAD.encode(Blob::encrypt(
            &shut_dek,
            shut_dek_id,
            &blob_aad(&store_id.to_string(), &shut_doc.as_str()),
            b"not for us",
        ));
        assert!(keys.decrypt_blob(store_id, shut, &shut_blob).is_err());
        assert!(keys.encrypt_key(store_id, shut).is_none(), "and nothing is written to it either");
    }

    fn dek_aad_of(store_id: StoreId, node_id: NodeId) -> Vec<u8> {
        dek_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str())
    }

    // ── What a document this page creates is wrapped under ──────────────────

    /// The documents of `ids` only, as a share's recipient pulls a scope.
    fn pull_subset(tree: &Tree, ids: &[NodeId]) -> Vec<Pulled> {
        ids.iter()
            .map(|id| Pulled {
                node_id: *id,
                entries: vec![(Mark::Through(1), Ok(tree.doc(*id).unwrap().save()))],
                head: 1,
            })
            .collect()
    }

    /// A share's marker on `node`, naming `key_id` as the share key: the edit
    /// that wrote it, for a test that plays it to another page.
    fn mark_shared(tree: &mut Tree, node: NodeId, key_id: KeyId, name: &str) -> TreeEdit {
        let marker = pimble_core::ShareMarker {
            v: pimble_core::ShareMarker::VERSION,
            key_id,
            url: "https://pimble.app".to_string(),
            name: name.to_string(),
        };
        tree.set_custom(node, custom_keys::SHARE, &serde_json::to_value(&marker).unwrap(), T1).unwrap()
    }

    #[test]
    fn a_document_a_scoped_member_creates_is_wrapped_under_the_share_key() {
        let (mut peer, root) = origin();
        let folder = NodeId::new();
        peer.add_node(folder, Some(root), None, "folder", "Recipes", T0).unwrap();
        let child = NodeId::new();
        peer.add_node(child, Some(folder), None, "document", "Bread", T0).unwrap();

        // The recipient's page: the share's key, the share's documents, and
        // the shared node as its root.
        let scope = keyring_for(Some(folder));
        let share_key_id = scope.current.unwrap();
        let mut scoped = listed(folder);
        scoped.roots = vec![folder];
        let store_id = StoreId::new();
        let mut store =
            VaultStore::assemble(scoped, StoreKeys::new(scope), vec![folder], pull_subset(&peer, &[folder, child]));
        assert!(store.is_partial());
        assert_eq!(store.tree.root(), folder);

        // A node they make in the shared folder.
        let mine = NodeId::new();
        let edit = store.tree.add_node(mine, Some(folder), None, "document", "Soup", T1).unwrap();
        let outgoing = store.prepare(store_id, mine, &edit.touched[0].1).unwrap();
        let (keys, parent_id) = outgoing.created.as_ref().expect("the server has never seen this document");
        assert_eq!(*parent_id, Some(folder), "the append names the parent, so the new document joins the scope");
        assert_eq!(
            keys.wraps.iter().map(|w| w.scope_key_id).collect::<Vec<_>>(),
            vec![share_key_id],
            "one wrap, under the only scope key this page holds"
        );
        // The wrap opens with the share key, for this document and no other.
        let share_key = store.keys.scope.get(&share_key_id).unwrap().clone();
        let dek = unwrap_dek(&keys.wraps[0], &share_key, &dek_aad_of(store_id, mine)).unwrap();
        assert!(unwrap_dek(&keys.wraps[0], &share_key, &dek_aad_of(store_id, NodeId::new())).is_err());
        // And the blob is under that data key.
        let blob = decode_blob(&outgoing.blob).unwrap();
        assert_eq!(Blob::key_id(&blob).unwrap(), keys.dek_id);
        assert_eq!(
            Blob::decrypt(&dek, &blob_aad(&store_id.to_string(), &VaultDocId::Node(mine).as_str()), &blob).unwrap(),
            edit.touched[0].1
        );

        // The append failed, or its answer was lost: the create tried again
        // goes out under the same key and the same wraps, which are the ones
        // the server holds if the first try did land.
        store.record(mine, &outgoing, Err("the socket went away".to_string())).unwrap_err();
        let again = store.prepare(store_id, mine, &[]).unwrap();
        assert_eq!(again.created.as_ref().map(|(keys, _)| keys), Some(keys));
        assert_eq!(Blob::key_id(&decode_blob(&again.blob).unwrap()).unwrap(), keys.dek_id);

        // A document the store already has is not created again: its key is
        // its own, not a fresh one.
        let rename = store.tree.set_title(child, "Sourdough", T2).unwrap();
        assert!(store.prepare(store_id, child, &rename.touched[0].1).unwrap().created.is_none());
    }

    #[test]
    fn an_owner_s_new_document_is_wrapped_under_the_store_key_and_every_share_above_it() {
        let (mut peer, root) = origin();
        let folder = NodeId::new();
        peer.add_node(folder, Some(root), None, "folder", "Recipes", T0).unwrap();

        // The owner's page: the store key, and the key of the share sitting on
        // that folder, which its marker names.
        let mut scope = keyring_for(None);
        let store_key_id = scope.current.unwrap();
        let share_key_id = KeyId::new_v4();
        scope.keys.insert(share_key_id, SymmetricKey::generate());
        scope.scopes.push((Some(folder), share_key_id));

        let store_id = StoreId::new();
        let mut store = VaultStore::assemble(listed(root), StoreKeys::new(scope), Vec::new(), pull_of(&peer));
        mark_shared(&mut store.tree, folder, share_key_id, "Recipes");

        // Inside the share: both keys.
        let inside = NodeId::new();
        let edit = store.tree.add_node(inside, Some(folder), None, "document", "Bread", T1).unwrap();
        let outgoing = store.prepare(store_id, inside, &edit.touched[0].1).unwrap();
        let (keys, parent_id) = outgoing.created.as_ref().expect("new to the server");
        assert_eq!(*parent_id, Some(folder));
        let mut under: Vec<KeyId> = keys.wraps.iter().map(|w| w.scope_key_id).collect();
        under.sort_by_key(|id| id.to_string());
        let mut want = vec![store_key_id, share_key_id];
        want.sort_by_key(|id| id.to_string());
        assert_eq!(under, want, "the store key, and the key of the share it sits under");

        // Outside it: the store key alone.
        let outside = NodeId::new();
        let edit = store.tree.add_node(outside, Some(root), None, "document", "Notes", T1).unwrap();
        let outgoing = store.prepare(store_id, outside, &edit.touched[0].1).unwrap();
        let (keys, _) = outgoing.created.as_ref().expect("new to the server");
        assert_eq!(keys.wraps.iter().map(|w| w.scope_key_id).collect::<Vec<_>>(), vec![store_key_id]);
    }

    // ── The shares in a whole store ─────────────────────────────────────────

    /// A scope key with its id, as the accounts service hands one over.
    fn scope_key() -> (KeyId, SymmetricKey) {
        (KeyId::new_v4(), SymmetricKey::generate())
    }

    /// The keyring a fetch of one share's key answers with.
    fn share_keyring(root: NodeId, key_id: KeyId, key: &SymmetricKey) -> Keyring {
        Keyring { keys: HashMap::from([(key_id, key.clone())]), current: Some(key_id), scopes: vec![(Some(root), key_id)] }
    }

    fn blob_under(store_id: StoreId, node_id: NodeId, dek_id: KeyId, dek: &SymmetricKey, payload: &[u8]) -> String {
        let aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
        URL_SAFE_NO_PAD.encode(Blob::encrypt(dek, dek_id, &aad, payload))
    }

    /// What `vaultFetch` answers for a document whose log is `payloads`, one
    /// entry each from seq 1, under a fresh data key wrapped under each of
    /// `under`; and that data key, for the blobs a test appends later.
    fn fetched_under(
        store_id: StoreId,
        node_id: NodeId,
        payloads: &[Vec<u8>],
        under: &[(KeyId, &SymmetricKey)],
    ) -> (VaultFetchResponse, (KeyId, SymmetricKey)) {
        let (dek_id, dek) = scope_key();
        let aad = dek_aad_of(store_id, node_id);
        let wraps = under.iter().map(|(id, key)| wrap_dek(&dek, key, *id, &aad)).collect();
        let updates: Vec<pimble_rpc::VaultEntry> = payloads
            .iter()
            .enumerate()
            .map(|(i, payload)| pimble_rpc::VaultEntry {
                seq: i as u64 + 1,
                blob: blob_under(store_id, node_id, dek_id, &dek, payload),
            })
            .collect();
        let head = updates.len() as u64;
        (VaultFetchResponse { snapshot: None, updates, head, keys: Some(VaultDocKeys { dek_id, wraps }) }, (dek_id, dek))
    }

    /// An owner's page as `open_store` builds it, holding the store key only:
    /// every document of `tree` but `members_docs` is wrapped under the store
    /// key, and those under `share` alone, as a share's member makes them
    /// while no desktop of the owner's is on to add the store key's wrap.
    /// Answers the store, what the open could not open, and each document's
    /// data key.
    #[allow(clippy::type_complexity)]
    fn owners_page(
        store_id: StoreId,
        tree: &Tree,
        members_docs: &[NodeId],
        share: (KeyId, &SymmetricKey),
    ) -> (VaultStore, Vec<(NodeId, VaultFetchResponse)>, HashMap<NodeId, (KeyId, SymmetricKey)>, KeyId) {
        let scope = keyring_for(None);
        let store_key_id = scope.current.unwrap();
        let store_key = scope.current_key().unwrap().clone();
        let mut deks = HashMap::new();
        let mut ids = tree.ids();
        ids.sort_by_key(|id| id.to_string());
        let fetched = ids
            .into_iter()
            .map(|id| {
                let under = if members_docs.contains(&id) { share } else { (store_key_id, &store_key) };
                let (fetched, dek) = fetched_under(store_id, id, &[tree.doc(id).unwrap().save()], &[under]);
                deks.insert(id, dek);
                (id, fetched)
            })
            .collect();
        let mut listed = listed(tree.root());
        listed.id = store_id;
        let (store, shut) = VaultStore::from_fetched(listed, StoreKeys::new(scope), Vec::new(), fetched);
        (store, shut, deks, store_key_id)
    }

    #[test]
    fn an_owner_s_page_reads_what_a_member_made_once_it_holds_the_share_s_key() {
        let (mut peer, root) = origin();
        let (folder, bread, soup) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(folder, Some(root), None, "folder", "Recipes", T0).unwrap();
        peer.add_node(bread, Some(folder), None, "document", "Bread", T0).unwrap();
        let (share_key_id, share_key) = scope_key();
        mark_shared(&mut peer, folder, share_key_id, "Recipes");
        // A member's note, made while every desktop of the owner's was off:
        // its data key is wrapped under the share's key and nothing else.
        peer.add_node(soup, Some(folder), None, "document", "Soup", T1).unwrap();

        let store_id = StoreId::new();
        let (mut store, shut, _, store_key_id) = owners_page(store_id, &peer, &[soup], (share_key_id, &share_key));

        // With the store key alone the note stays shut: not a node here, not
        // shown, and nothing about it is "repaired". The folder goes on
        // listing it, as every device that can read it needs it to.
        assert_eq!(shut.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![soup]);
        assert!(!store.tree.has_node(soup));
        assert!(store.node_of(soup).is_err());
        assert_eq!(store.children_of(folder).unwrap(), vec![bread]);
        assert!(store.repair_now(T2).is_none(), "a document not held is unknown, not missing");
        assert_eq!(store.tree.doc(folder).unwrap().children(), vec![bread, soup], "the list is untouched");
        assert_eq!(store.behind(), vec![(soup, 0)], "listed, and nothing of it read");

        // What the open does next: the folder carries a share's marker whose
        // key is not held, so that root is asked about.
        assert_eq!(store.missing_share_roots(), vec![folder]);
        assert_eq!(store.share_keys_to_ask_for(0.0), Some(vec![folder]));

        // The key arrives, the note opens from what was already fetched, and
        // it shows under its parent.
        assert!(store.take_keys(share_keyring(folder, share_key_id, &share_key)));
        assert!(!store.take_keys(share_keyring(folder, share_key_id, &share_key)), "the same key again is not news");
        let (_, fetched) = &shut[0];
        let merged = store.take_fetched(store_id, soup, fetched, false, 0.0);
        assert!(
            matches!(kinds_of(&merged.events)[..], [StoreChangeKind::NodeCreated { node_id, parent_id }] if node_id == soup && parent_id == folder),
            "{:?}",
            merged.events
        );
        assert_eq!(store.children_of(folder).unwrap(), vec![bread, soup]);
        assert_eq!(store.node_of(soup).unwrap().metadata.title, "Soup");
        assert!(store.behind().is_empty());
        assert!(store.missing_share_roots().is_empty());
        assert!(store.share_keys_to_ask_for(60_000.0).is_none(), "nothing left to ask for");
        assert!(store.repair_now(T2).is_none());
        assert_eq!(store.keys.scope.current, Some(store_key_id), "the store key stays the store's own");

        // A member's page is owed no key but its grants': the same marker in
        // its scope root asks for nothing.
        let mut scoped = listed(folder);
        scoped.roots = vec![folder];
        let member = VaultStore::assemble(scoped, StoreKeys::new(keyring_for(Some(folder))), vec![folder], pull_subset(&peer, &[folder, bread]));
        assert!(member.missing_share_roots().is_empty());
    }

    #[test]
    fn a_marker_arriving_in_an_update_schedules_one_look_for_its_key() {
        let (mut peer, root) = origin();
        let folder = NodeId::new();
        peer.add_node(folder, Some(root), None, "folder", "Recipes", T0).unwrap();
        let (share_key_id, share_key) = scope_key();
        let store_id = StoreId::new();
        let (mut store, shut, deks, _) = owners_page(store_id, &peer, &[], (share_key_id, &share_key));
        assert!(shut.is_empty());
        assert!(store.share_keys_to_ask_for(0.0).is_none(), "no share, nothing to ask");
        assert!(store.look.due.is_none());

        // One of the owner's desktops shares the folder while the page is
        // open: the marker reaches it as an update of the folder's document.
        let (dek_id, dek) = &deks[&folder];
        let edit = mark_shared(&mut peer, folder, share_key_id, "Recipes");
        let blob = blob_under(store_id, folder, *dek_id, dek, &edit.touched[0].1);
        let merged = store.take_blob(store_id, folder, 2, &blob, false, None, 1_000.0);
        assert!(merged.wants_key);
        assert_eq!(store.look.due, Some(1_000.0 + KEY_LOOK_DEBOUNCE_MS));

        // The share renamed a moment later names the same key: no second
        // reason, and the look is not put off.
        let edit = mark_shared(&mut peer, folder, share_key_id, "Family recipes");
        let blob = blob_under(store_id, folder, *dek_id, dek, &edit.touched[0].1);
        assert!(!store.take_blob(store_id, folder, 3, &blob, false, None, 1_100.0).wants_key);
        assert_eq!(store.look.due, Some(1_000.0 + KEY_LOOK_DEBOUNCE_MS));

        // The look asks once, and not again inside the floor however many
        // reasons arrive: it is put off until the floor has passed.
        store.look.due = None;
        assert_eq!(store.share_keys_to_ask_for(1_250.0), Some(vec![folder]));
        assert_eq!(store.share_keys_to_ask_for(5_000.0), None);
        assert_eq!(store.look.due, Some(1_250.0 + SHARE_KEY_FLOOR_MS));
        assert_eq!(store.share_keys_to_ask_for(1_250.0 + SHARE_KEY_FLOOR_MS), Some(vec![folder]));

        // Held, it is never asked for again; and a marker whose key is held
        // already wants nothing.
        assert!(store.take_keys(share_keyring(folder, share_key_id, &share_key)));
        assert!(store.share_keys_to_ask_for(60_000.0).is_none());
        store.look.due = None;
        let edit = mark_shared(&mut peer, folder, share_key_id, "Recipes");
        let blob = blob_under(store_id, folder, *dek_id, dek, &edit.touched[0].1);
        assert!(!store.take_blob(store_id, folder, 4, &blob, false, None, 61_000.0).wants_key);
        assert!(store.look.due.is_none());

        // A member's page holds its grants' keys and asks for no others,
        // whatever marker arrives.
        let other = KeyId::new_v4();
        let mut scoped = listed(folder);
        scoped.roots = vec![folder];
        let mut member = VaultStore::assemble(scoped, StoreKeys::new(keyring_for(Some(folder))), vec![folder], pull_subset(&peer, &[folder]));
        let edit = mark_shared(&mut peer, folder, other, "Recipes");
        let merged = member.merge(store_id, folder, vec![(Mark::One(5), Ok(edit.touched[0].1.clone()))], false, None);
        assert!(!merged.wants_key);
        assert!(member.share_keys_to_ask_for(0.0).is_none());
    }

    #[test]
    fn a_blob_that_will_not_open_is_read_again_for_its_wraps_once() {
        let (mut peer, root) = origin();
        let folder = NodeId::new();
        peer.add_node(folder, Some(root), None, "folder", "Recipes", T0).unwrap();
        let (share_key_id, share_key) = scope_key();
        mark_shared(&mut peer, folder, share_key_id, "Recipes");
        let store_id = StoreId::new();
        let (mut store, _, deks, _) = owners_page(store_id, &peer, &[], (share_key_id, &share_key));
        assert!(store.take_keys(share_keyring(folder, share_key_id, &share_key)), "held since the open");

        // A member makes a note while the page is open. The notification
        // carries the blob and never the wraps, so the key it names is one
        // this page has not heard of.
        let soup = NodeId::new();
        let edit = peer.add_node(soup, Some(folder), None, "document", "Soup", T1).unwrap();
        assert_eq!(edit.node_ids(), vec![soup, folder]);
        let (fetched, (soup_dek_id, soup_dek)) = fetched_under(store_id, soup, &[edit.touched[0].1.clone()], &[(share_key_id, &share_key)]);
        let first = &fetched.updates[0].blob;
        let merged = store.take_blob(store_id, soup, 1, first, false, None, 2_000.0);
        assert!(merged.events.is_empty() && !store.tree.has_node(soup));
        assert_eq!(store.look.due, Some(2_000.0 + KEY_LOOK_DEBOUNCE_MS));
        // The folder's list arrives and opens as ever; it names a document
        // not held yet, which no repair judges.
        let (folder_dek_id, folder_dek) = &deks[&folder];
        let list = blob_under(store_id, folder, *folder_dek_id, folder_dek, &edit.touched[1].1);
        assert!(store.take_blob(store_id, folder, 2, &list, false, None, 2_010.0).structure);
        assert!(store.repair_now(T2).is_none());

        // The look: no key to ask for, one document to read again, from the
        // start. The fetch brings the wraps, and the note opens.
        assert!(store.share_keys_to_ask_for(2_250.0).is_none());
        assert_eq!(store.reread_list(false), vec![(soup, 0)]);
        let merged = store.take_fetched(store_id, soup, &fetched, false, 2_300.0);
        assert!(
            matches!(kinds_of(&merged.events)[..], [StoreChangeKind::NodeCreated { node_id, parent_id }] if node_id == soup && parent_id == folder),
            "{:?}",
            merged.events
        );
        assert_eq!(store.children_of(folder).unwrap(), vec![soup]);
        assert!(store.behind().is_empty());
        // And the member's next edit opens as it arrives.
        let rename = peer.set_title(soup, "Leek soup", T2).unwrap();
        let blob = blob_under(store_id, soup, soup_dek_id, &soup_dek, &rename.touched[0].1);
        store.look.due = None;
        store.take_blob(store_id, soup, 2, &blob, false, None, 3_000.0);
        assert_eq!(store.node_of(soup).unwrap().metadata.title, "Leek soup");
        assert!(store.look.due.is_none(), "nothing is owed");

        // A document wrapped under nothing this page will ever hold is asked
        // about once, not once an append.
        let theirs = NodeId::new();
        let (stranger_id, stranger) = scope_key();
        let (shut, (shut_dek_id, shut_dek)) = fetched_under(store_id, theirs, &[b"one".to_vec()], &[(stranger_id, &stranger)]);
        store.take_blob(store_id, theirs, 1, &shut.updates[0].blob, false, None, 4_000.0);
        assert_eq!(store.reread_list(false), vec![(theirs, 0)]);
        store.look.due = None;
        store.take_fetched(store_id, theirs, &shut, false, 4_300.0);
        assert_eq!(store.behind(), vec![(theirs, 0)], "still shut");
        let more = blob_under(store_id, theirs, shut_dek_id, &shut_dek, b"two");
        store.take_blob(store_id, theirs, 2, &more, false, None, 5_000.0);
        assert!(store.look.due.is_none() && store.look.unread.is_empty(), "no share's key is missing and its wraps are known");
        // Until a scope key arrives, which may open anything: then everything
        // not read through is read again.
        assert!(store.take_keys(share_keyring(NodeId::new(), stranger_id, &stranger)));
        assert_eq!(store.reread_list(true), vec![(theirs, 0)]);
    }

    #[test]
    fn an_owner_creating_under_a_shared_root_wraps_under_the_share_s_key_once_it_holds_it() {
        let (mut peer, root) = origin();
        let folder = NodeId::new();
        peer.add_node(folder, Some(root), None, "folder", "Recipes", T0).unwrap();
        let (share_key_id, share_key) = scope_key();
        mark_shared(&mut peer, folder, share_key_id, "Recipes");
        let store_id = StoreId::new();
        let (mut store, _, _, store_key_id) = owners_page(store_id, &peer, &[], (share_key_id, &share_key));
        let wraps_of = |store: &mut VaultStore, title: &str| {
            let id = NodeId::new();
            let edit = store.tree.add_node(id, Some(folder), None, "document", title, T1).unwrap();
            let outgoing = store.prepare(store_id, id, &edit.touched[0].1).unwrap();
            let (keys, _) = outgoing.created.expect("new to the server");
            let mut under: Vec<KeyId> = keys.wraps.iter().map(|w| w.scope_key_id).collect();
            under.sort_by_key(|id| id.to_string());
            under
        };

        // Before the share's key has reached the page there is only the store
        // key to wrap under; the owner's desktops add the share's.
        assert_eq!(wraps_of(&mut store, "Bread"), vec![store_key_id]);

        // Once it is in the keyring, the marker above the new node finds it:
        // the members read what the owner makes with no desktop in between.
        assert!(store.take_keys(share_keyring(folder, share_key_id, &share_key)));
        let mut both = vec![store_key_id, share_key_id];
        both.sort_by_key(|id| id.to_string());
        assert_eq!(wraps_of(&mut store, "Soup"), both);

        // Outside the share: the store key alone, as before.
        let outside = NodeId::new();
        let edit = store.tree.add_node(outside, Some(root), None, "document", "Notes", T1).unwrap();
        let outgoing = store.prepare(store_id, outside, &edit.touched[0].1).unwrap();
        let (keys, _) = outgoing.created.expect("new to the server");
        assert_eq!(keys.wraps.iter().map(|w| w.scope_key_id).collect::<Vec<_>>(), vec![store_key_id]);
    }

    // ── A scope root is a root, not an orphan ───────────────────────────────

    #[test]
    fn repair_leaves_a_scope_root_whose_parent_is_not_held_where_it_is() {
        let (mut peer, root) = origin();
        let (recipes, plans, elsewhere) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(recipes, Some(root), None, "folder", "Recipes", T0).unwrap();
        peer.add_node(plans, Some(root), None, "folder", "Plans", T0).unwrap();
        peer.add_node(elsewhere, Some(root), None, "folder", "Elsewhere", T0).unwrap();
        let (bread, monday, stray) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(bread, Some(recipes), None, "document", "Bread", T0).unwrap();
        peer.add_node(monday, Some(plans), None, "document", "Monday", T0).unwrap();
        peer.add_node(stray, Some(elsewhere), None, "document", "Stray", T0).unwrap();

        // Two shares of one store, and — as can happen while an owner's move
        // and their scope publish are in flight — one document that leads to
        // neither.
        let mut scoped = listed(recipes);
        scoped.roots = vec![recipes, plans];
        let mut store = VaultStore::assemble(
            scoped,
            StoreKeys::new(keyring_for(Some(recipes))),
            vec![recipes, plans],
            pull_subset(&peer, &[recipes, bread, plans, monday, stray]),
        );

        assert!(store.repair_now(T1).is_none(), "a consistent scope needs no repair");
        for (scope_root, parent) in [(recipes, root), (plans, root)] {
            assert_eq!(
                store.tree.get_node_info(scope_root).unwrap().parent_id,
                Some(parent),
                "a scope root's parent is the owner's folder and is never rewritten"
            );
        }
        assert_eq!(store.children_of(recipes).unwrap(), vec![bread]);
        assert_eq!(store.children_of(plans).unwrap(), vec![monday]);
        assert_eq!(
            store.tree.get_node_info(stray).unwrap().parent_id,
            Some(elsewhere),
            "a document that leads to no scope root is not adopted into one"
        );
        assert!(store.tree.get_children(recipes).unwrap().iter().all(|id| *id != stray));

        // Until 2026-09-21 a repair that took these for a whole store judged
        // all three (the scope roots as orphans, `stray` adopted). No repair
        // judges what it does not hold any more, rooted or not: a parent that
        // is not here is unknown, not missing.
        let mut whole = VaultStore::assemble(
            listed(recipes),
            StoreKeys::new(keyring()),
            Vec::new(),
            pull_subset(&peer, &[recipes, bread, plans, monday, stray]),
        );
        assert!(whole.repair_now(T1).is_none(), "nothing known is wrong");
    }

    /// The page assembles its own nodes, so it is the one to say what the
    /// account may change of each (`Node::access`), by the hosted server's
    /// rule: the role of the shared root a node is under, the wider role
    /// where two roots cover it, a whole-store grant over any share. The app
    /// believes it: a document under a reader's root takes no typing, in a
    /// store whose other root is edited.
    #[test]
    fn an_assembled_node_says_what_the_account_may_change_of_it() {
        use StoreAccess::{Full, Read};
        let (mut peer, root) = origin();
        let (recipes, plans, inner) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(recipes, Some(root), None, "folder", "Recipes", T0).unwrap();
        peer.add_node(plans, Some(root), None, "folder", "Plans", T0).unwrap();
        // A folder the account edits inside the one it reads.
        peer.add_node(inner, Some(recipes), None, "folder", "Ours", T0).unwrap();
        let (bread, monday, shared_list) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(bread, Some(recipes), None, "document", "Bread", T0).unwrap();
        peer.add_node(monday, Some(plans), None, "document", "Monday", T0).unwrap();
        peer.add_node(shared_list, Some(inner), None, "document", "Shopping", T0).unwrap();

        let store_id = StoreId::new();
        let mut scoped = listed(recipes);
        scoped.id = store_id;
        scoped.roots = vec![recipes, plans, inner];
        let held = [recipes, bread, plans, monday, inner, shared_list];
        let store = VaultStore::assemble(scoped.clone(), StoreKeys::new(keyring_for(Some(recipes))), vec![recipes, plans, inner], pull_subset(&peer, &held));
        let mixed = row(
            vec![grant(Some(recipes), "reader", "Recipes"), grant(Some(plans), "editor", "Plans"), grant(Some(inner), "editor", "Ours")],
            Some("ann@example.com"),
        );
        assert_eq!(mixed.access(), Full, "something here may be written");
        assert_eq!(mixed.read_only_roots(), vec![recipes]);

        let access = |id: NodeId, row: &AccountStore| store.node_for(id, Some(row)).unwrap().access;
        for (id, expected, what) in [
            (recipes, Read, "the root it reads"),
            (bread, Read, "a document under it"),
            (plans, Full, "the root it edits"),
            (monday, Full, "a document under it"),
            (inner, Full, "an edited root inside the read one"),
            (shared_list, Full, "in both scopes: the wider role"),
        ] {
            assert_eq!(access(id, &mixed), expected, "{what}");
        }

        // Every grant a reader's: the store's own word. A whole-store grant
        // beside the shares answers for every node, either way.
        let reading = row(vec![grant(Some(recipes), "reader", "Recipes"), grant(Some(plans), "reader", "Plans")], None);
        assert!(held.iter().all(|id| access(*id, &reading) == Read));
        assert!(reading.read_only_roots().is_empty(), "one answer covers the store");
        let whole_editor = row(vec![grant(Some(recipes), "reader", "Recipes"), grant(None, "editor", "Family")], None);
        assert!(held.iter().all(|id| access(*id, &whole_editor) == Full));
        let whole_reader = row(vec![grant(Some(plans), "editor", "Plans"), grant(None, "reader", "Family")], None);
        assert!(held.iter().all(|id| access(*id, &whole_reader) == Read));
        // Nothing known about the store disables nothing.
        assert_eq!(store.node_for(bread, None).unwrap().access, Full);

        // What the app is handed: both answers, and the store described with
        // the roots only read.
        let mut client = VaultClient::new("me".to_string());
        client.stores.insert(store_id, store);
        client.rows.insert(store_id, mixed);
        let BackendEvent::ChildrenLoaded { children, .. } = client.get_children(store_id, recipes) else { panic!("children of a held root") };
        let mut listed_access: Vec<(NodeId, StoreAccess)> = children.iter().map(|n| (n.id, n.access)).collect();
        listed_access.sort_by_key(|(id, _)| *id != bread);
        assert_eq!(listed_access, vec![(bread, Read), (inner, Full)]);
        let BackendEvent::NodeLoaded { node, .. } = client.get_node(store_id, bread) else { panic!("a held node") };
        assert_eq!(node.access, Read);
        let mut described = scoped;
        client.describe(&mut described);
        assert_eq!((described.access, described.read_only_roots), (Full, vec![recipes]));
    }

    // ── A share that ended (Joe, 2026-09-21) ────────────────────────────────

    /// What ended between two readings of the account's store list.
    #[test]
    fn the_shares_the_rows_no_longer_name_have_ended() {
        let (store_id, other) = (StoreId::new(), StoreId::new());
        let (recipes, plans) = (NodeId::new(), NodeId::new());
        let both = row(vec![grant(Some(recipes), "editor", "Recipes"), grant(Some(plans), "reader", "Plans")], Some("ann@example.com"));
        let one = row(vec![grant(Some(plans), "reader", "Plans")], Some("ann@example.com"));
        let whole = row(vec![grant(None, "editor", "Family")], None);
        let rows = |entries: Vec<(StoreId, &AccountStore)>| entries.into_iter().map(|(id, row)| (id, row.clone())).collect::<HashMap<_, _>>();

        assert!(shares_ended(&rows(vec![(store_id, &both)]), &rows(vec![(store_id, &both)])).is_empty());
        assert_eq!(
            shares_ended(&rows(vec![(store_id, &both)]), &rows(vec![(store_id, &one)])),
            vec![EndedShares { store_id, roots: vec![recipes], every: false }]
        );
        assert_eq!(
            shares_ended(&rows(vec![(store_id, &both), (other, &whole)]), &rows(vec![(other, &whole)])),
            vec![EndedShares { store_id, roots: vec![recipes, plans], every: true }],
            "no row names the store at all: every share of it ended"
        );
        assert!(shares_ended(&rows(vec![(store_id, &one)]), &rows(vec![(store_id, &both)])).is_empty(), "another share arrived: nothing ended");
        assert!(shares_ended(&rows(vec![(store_id, &both)]), &rows(vec![(store_id, &whole)])).is_empty(), "held whole now: nothing of it was lost");
        assert!(shares_ended(&rows(vec![(other, &whole)]), &HashMap::new()).is_empty(), "a store held whole was never a share");
    }

    /// The page holds no replica: a share the rows stop naming is said to
    /// the UI, its documents are dropped from memory, and what is left of
    /// the store goes on; when it was the last, the store is closed.
    #[test]
    fn a_share_that_ended_is_said_and_dropped_and_the_last_one_closes_the_store() {
        let (mut peer, root) = origin();
        let (recipes, plans) = (NodeId::new(), NodeId::new());
        peer.add_node(recipes, Some(root), None, "folder", "Recipes", T0).unwrap();
        peer.add_node(plans, Some(root), None, "folder", "Plans", T0).unwrap();
        let (bread, monday) = (NodeId::new(), NodeId::new());
        peer.add_node(bread, Some(recipes), None, "document", "Bread", T0).unwrap();
        peer.add_node(monday, Some(plans), None, "document", "Monday", T0).unwrap();

        let store_id = StoreId::new();
        let mut scoped = listed(recipes);
        scoped.id = store_id;
        scoped.roots = vec![recipes, plans];
        let store = VaultStore::assemble(scoped.clone(), StoreKeys::new(keyring_for(Some(recipes))), vec![recipes, plans], pull_subset(&peer, &[recipes, bread, plans, monday]));
        let both = row(vec![grant(Some(recipes), "editor", "Recipes"), grant(Some(plans), "editor", "Plans")], Some("ann@example.com"));
        let mut client = VaultClient::new("me".to_string());
        client.stores.insert(store_id, store);
        client.rows.insert(store_id, both.clone());
        client.active = Some((store_id, bread));

        // The same list again: nothing to say.
        assert!(client.take_rows(HashMap::from([(store_id, both)])).is_empty());

        // Removed from Recipes, the tree's first root.
        let plans_only = row(vec![grant(Some(plans), "editor", "Plans")], Some("ann@example.com"));
        let events = client.take_rows(HashMap::from([(store_id, plans_only.clone())]));
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(matches!(&events[0], BackendEvent::RemoteStoreChange { store_id: said, change_kind: StoreChangeKind::SharesEnded { node_ids }, source_client_id: None } if *said == store_id && node_ids == &vec![recipes]));
        let open = client.stores.get(&store_id).expect("the share that is left keeps the store open");
        assert_eq!(open.scope_roots, vec![plans]);
        assert!(open.is_partial());
        assert_eq!(open.tree.root(), plans, "the tree starts from a root that is held");
        for gone in [recipes, bread] {
            assert!(open.tree.doc(gone).is_none() && !open.docs.contains_key(&gone) && !open.heads.contains_key(&gone), "{gone} is still in memory");
        }
        assert!(open.tree.doc(monday).is_some() && open.tree.doc(plans).is_some());
        assert_eq!(client.active, None, "the open document was under it");
        let mut described = scoped.clone();
        client.describe(&mut described);
        assert_eq!((described.roots.clone(), described.root_node_id), (vec![plans], plans));
        assert!(matches!(client.get_node(store_id, bread), BackendEvent::Error { .. }), "nothing of it is answered for");
        assert!(matches!(client.get_node(store_id, monday), BackendEvent::NodeLoaded { .. }));

        // Removed from the last one: said, and the store closed.
        let events = client.take_rows(HashMap::new());
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(&events[0], BackendEvent::RemoteStoreChange { change_kind: StoreChangeKind::SharesEnded { node_ids }, .. } if node_ids == &vec![plans]));
        assert!(matches!(&events[1], BackendEvent::StoreClosed { store_id: closed } if *closed == store_id));
        assert!(!client.owns(store_id), "nothing of it is kept");
        // Its endpoint, if the session still names one, failing afterwards
        // is not its owner being offline: the row does not come back.
        assert!(client.endpoint_down(&[store_id]).is_empty());
        // Invited again: the list names it, and it is a store like any other.
        assert!(client.take_rows(HashMap::from([(store_id, plans_only)])).is_empty());
        assert!(!client.endpoint_down(&[store_id]).is_empty());
    }

    /// Overlapping shares: a document also under a share that is left stays.
    #[test]
    fn what_is_also_under_a_share_that_is_left_is_kept() {
        let (mut peer, root) = origin();
        let (outer, inner, leaf) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(outer, Some(root), None, "folder", "Outer", T0).unwrap();
        peer.add_node(inner, Some(outer), None, "folder", "Inner", T0).unwrap();
        peer.add_node(leaf, Some(inner), None, "document", "Leaf", T0).unwrap();
        let mut scoped = listed(inner);
        scoped.roots = vec![inner, outer];
        let mut store = VaultStore::assemble(scoped, StoreKeys::new(keyring_for(Some(outer))), vec![inner, outer], pull_subset(&peer, &[outer, inner, leaf]));

        assert!(store.drop_roots(&[inner]));
        assert_eq!(store.scope_roots, vec![outer]);
        assert!(store.tree.doc(inner).is_some() && store.tree.doc(leaf).is_some(), "still in the outer share");
        assert_eq!(store.tree.root(), outer);
        assert!(!store.drop_roots(&[outer]), "the last root is not this function's to drop");
        assert_eq!(store.scope_roots, vec![outer]);
    }

    #[test]
    fn a_scope_that_lists_a_document_not_held_yet_is_not_judged_yet() {
        let (mut peer, root) = origin();
        let recipes = NodeId::new();
        peer.add_node(recipes, Some(root), None, "folder", "Recipes", T0).unwrap();
        let (bread, coming) = (NodeId::new(), NodeId::new());
        peer.add_node(bread, Some(recipes), None, "document", "Bread", T0).unwrap();
        // A document somebody else created a moment ago: the folder's list
        // names it and its own document has not arrived.
        peer.add_node(coming, Some(recipes), None, "document", "Soup", T1).unwrap();

        let mut scoped = listed(recipes);
        scoped.roots = vec![recipes];
        let mut store = VaultStore::assemble(
            scoped,
            StoreKeys::new(keyring_for(Some(recipes))),
            vec![recipes],
            pull_subset(&peer, &[recipes, bread]),
        );
        assert!(
            store.repair_now(T1).is_none(),
            "removing that entry would delete a child from the owner's folder"
        );
        assert!(store.tree.doc(recipes).unwrap().children().contains(&coming));

        // Once it is here, the scope is judged again and has nothing to fix.
        store.tree.apply_update(coming, &peer.doc(coming).unwrap().save()).unwrap();
        assert!(store.repair_now(T2).is_none());
        assert_eq!(store.children_of(recipes).unwrap(), vec![bread, coming]);
    }

    // ── Refusals ────────────────────────────────────────────────────────────

    #[test]
    fn a_reader_s_write_is_refused_here_with_no_request_made() {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        let mut client = VaultClient::new("me".to_string());
        client.rows.insert(store_id, row(vec![grant(Some(NodeId::new()), "reader", "Recipes")], Some("ann@example.com")));

        let refused = client
            .refuse_write(store_id, &BackendCommand::RenameNode { store_id, node_id, title: "No".into() })
            .expect("a reader may not rename");
        assert!(
            matches!(&refused, BackendEvent::Error { message } if message == StoreAccess::READ_ONLY_REFUSAL),
            "the sentence alone, as the person sees it: {refused:?}"
        );
        // Reads are not refused, and neither is anything on a store this
        // account edits.
        assert!(client.refuse_write(store_id, &BackendCommand::GetNode { store_id, node_id }).is_none());
        assert!(client.refuse_write(StoreId::new(), &BackendCommand::DeleteNode { store_id, node_id }).is_none());
    }

    #[test]
    fn a_refusal_is_told_apart_from_a_failure() {
        assert!(is_refusal(StoreAccess::READ_ONLY_REFUSAL));
        assert!(is_refusal("Forbidden: no grant for this document"));
        assert!(!is_refusal("Connection error: the socket closed"));
        assert!(!is_refusal("listing the vault's documents failed"));
    }

    #[test]
    fn a_refused_append_leaves_nothing_pending_and_nothing_local() {
        let (mut peer, root) = origin();
        let x = NodeId::new();
        peer.add_node(x, Some(root), None, "document", "x", T0).unwrap();
        let store_id = StoreId::new();
        let mut store = opened(&peer);

        // An edit whose append the server refuses — a reader's root inside a
        // store this account edits elsewhere, say.
        let edit = store.tree.set_title(x, "Mine now", T1).unwrap();
        let outgoing = store.prepare(store_id, x, &edit.touched[0].1).unwrap();
        assert!(store.record(x, &outgoing, Err("Forbidden: no grant for this document".into())).is_err());
        assert!(store.docs[&x].unsent, "an ordinary failure is resent");

        // The refusal path drops the document instead, so the pull that
        // follows starts from the beginning and ends at what the server holds.
        store.refused(x);
        assert!(store.tree.doc(x).is_none(), "the local document goes");
        assert!(!store.docs.contains_key(&x), "with its cursor and its unsent flag");
        assert!(!store.heads.contains_key(&x), "so the next fetch asks from 0");
        assert!(store.keys.wraps.get(&x).is_none());
    }

    #[test]
    fn only_the_commands_that_change_something_are_writes() {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        assert!(writes(&BackendCommand::CreateNode { store_id, parent_id: None, title: String::new() }));
        assert!(writes(&BackendCommand::MoveNode { store_id, node_id, new_parent_id: node_id, position: None }));
        assert!(writes(&BackendCommand::BroadcastChanges { store_id, node_id, changes: String::new() }));
        assert!(!writes(&BackendCommand::GetChildren { store_id, node_id }));
        assert!(!writes(&BackendCommand::SubscribeStoreChanges { store_id }));
        assert!(!writes(&BackendCommand::ListStores));
    }

    // ── Moving a node (docs/MOVE_CONTRACT.md) ────────────────────────────────

    #[test]
    fn a_move_that_leaves_a_share_appends_new_documents_with_their_own_keys_and_tombstones_the_original() {
        let (mut peer, root) = origin();
        let (share_a, share_b, note) = (NodeId::new(), NodeId::new(), NodeId::new());
        peer.add_node(share_a, Some(root), None, "folder", "Alpha", T0).unwrap();
        peer.add_node(share_b, Some(root), None, "folder", "Beta", T0).unwrap();
        peer.add_node(note, Some(share_a), None, "document", "Note", T0).unwrap();

        // The owner's page: the store key, and the key of each share.
        let mut scope = keyring_for(None);
        let store_key_id = scope.current.unwrap();
        let (key_a_id, key_a) = scope_key();
        let (key_b_id, key_b) = scope_key();
        scope.keys.insert(key_a_id, key_a);
        scope.keys.insert(key_b_id, key_b);
        scope.scopes.push((Some(share_a), key_a_id));
        scope.scopes.push((Some(share_b), key_b_id));

        let store_id = StoreId::new();
        let mut store = VaultStore::assemble(listed(root), StoreKeys::new(scope), Vec::new(), pull_of(&peer));
        mark_shared(&mut store.tree, share_a, key_a_id, "Alpha");
        mark_shared(&mut store.tree, share_b, key_b_id, "Beta");

        // Read before the move happens: afterwards the original is a
        // tombstone and answers nothing about the shares it used to be in.
        let left_shares = as_left_shares(&store.tree, store.tree.shares_left(note, share_b));
        assert_eq!(left_shares.len(), 1);
        assert_eq!(left_shares[0].root, share_a);
        assert_eq!(left_shares[0].name, "Alpha", "the first share it leaves, named from its marker");

        let (new_id, edit) = store.tree.move_or_transplant(note, share_b, None, T1, &mut NodeId::new).unwrap();
        assert_ne!(new_id, note, "moving into a different share is a transplant, not a move");
        assert!(store.tree.doc(note).unwrap().fields().unwrap().deleted_at.is_some(), "the original is tombstoned");

        for (id, update) in &edit.touched {
            let outgoing = store.prepare(store_id, *id, update).unwrap();
            if *id == new_id {
                let (keys, parent_id) =
                    outgoing.created.as_ref().expect("a document the server has never seen gets a key of its own");
                assert_eq!(*parent_id, Some(share_b), "the append names the parent it lands under");
                let mut under: Vec<KeyId> = keys.wraps.iter().map(|w| w.scope_key_id).collect();
                under.sort_by_key(|id| id.to_string());
                let mut want = vec![store_key_id, key_b_id];
                want.sort_by_key(|id| id.to_string());
                assert_eq!(under, want, "the store key, and the key of the share it lands in — never Alpha's");
            } else {
                assert!(outgoing.created.is_none(), "{id} already existed on the server");
            }
        }
    }

    #[test]
    fn a_transplant_between_two_vault_stores_lands_with_the_text_intact_and_the_source_tombstoned() {
        let (mut peer_from, root_from) = origin();
        let note = NodeId::new();
        peer_from.add_node(note, Some(root_from), None, "document", "Note", T0).unwrap();
        peer_from.doc_mut(note).unwrap().replace_plain_text("Hello, world").unwrap();

        let (peer_to, root_to) = origin();

        let from_store_id = StoreId::new();
        let to_store_id = StoreId::new();
        let mut from_store = opened(&peer_from);
        let mut to_store = opened(&peer_to);

        // Created first: the cutting read out of the source, planted in the
        // target with a fresh id, and appended there — as
        // `VaultClient::transplant_node` does, minus the network.
        let cutting = from_store.tree.take_cutting(note).unwrap();
        let (new_id, edit_to) = to_store.tree.plant(cutting, root_to, None, T1, &mut NodeId::new).unwrap();
        assert_ne!(new_id, note);
        assert_eq!(to_store.tree.doc(new_id).unwrap().text(), "Hello, world", "the text survives a transplant");
        assert_eq!(to_store.tree.get_node_info(new_id).unwrap().title, "Note");

        for (id, update) in &edit_to.touched {
            let outgoing = to_store.prepare(to_store_id, *id, update).unwrap();
            if *id == new_id {
                assert!(outgoing.created.is_some(), "a document the target has never seen gets a key of its own");
            } else {
                assert!(outgoing.created.is_none(), "the target's own root already existed");
            }
        }

        // Deleted second, only once the new home would be on the server.
        let tombstone_edit = from_store.tree.remove_node(note, T1).unwrap();
        assert!(from_store.tree.doc(note).unwrap().fields().unwrap().deleted_at.is_some());
        for (id, update) in &tombstone_edit.touched {
            let outgoing = from_store.prepare(from_store_id, *id, update).unwrap();
            assert!(outgoing.created.is_none(), "the source's documents already existed");
        }
    }

    #[test]
    fn a_plain_store_on_either_side_of_a_transplant_is_refused_with_the_sentence() {
        assert!(refuse_mixed_transplant(true, true).is_none(), "both encrypted: the vault client's to do");
        assert!(refuse_mixed_transplant(false, false).is_none(), "both plain: the server's to do");
        for (from_vault, to_vault) in [(true, false), (false, true)] {
            let refused = refuse_mixed_transplant(from_vault, to_vault).expect("one of each store kind is refused");
            assert!(
                matches!(&refused, BackendEvent::Error { message } if message == MIXED_TRANSPLANT_REFUSAL),
                "the sentence alone: {refused:?}"
            );
        }
    }

    #[test]
    fn a_plain_transplant_across_two_servers_is_refused_with_the_sentence() {
        assert!(refuse_split_transplant("wss://a.example/rpc", "wss://a.example/rpc").is_none(), "one server holds both: its to do");
        let refused = refuse_split_transplant("wss://a.example/rpc", "wss://b.example/rpc").expect("two servers are refused");
        assert!(
            matches!(&refused, BackendEvent::Error { message } if message == SPLIT_TRANSPLANT_REFUSAL),
            "the sentence alone: {refused:?}"
        );
    }

    #[test]
    fn list_deleted_names_a_tombstone_s_parent_and_an_unlisted_node_s_place_to_go_back_to() {
        let (mut peer, root) = origin();
        let folder = NodeId::new();
        peer.add_node(folder, Some(root), None, "folder", "Notes", T0).unwrap();
        let gone = NodeId::new();
        peer.add_node(gone, Some(folder), None, "document", "Gone", T0).unwrap();
        peer.remove_node(gone, T1).unwrap();

        let stray = NodeId::new();
        peer.add_node(stray, Some(root), None, "document", "Stray", T0).unwrap();
        // A tampered or half-applied edit: still under `root` by `parent_id`,
        // but `root`'s own list no longer names it.
        peer.doc_mut(root).unwrap().remove_child(stray).unwrap();

        let store = opened(&peer);
        let nodes = store.list_deleted(None);

        let tombstone = nodes.iter().find(|d| d.node.id == gone).expect("the tombstone is listed");
        assert_eq!(tombstone.parent_title.as_deref(), Some("Notes"));
        assert!(tombstone.deleted_at.is_some());
        assert!(tombstone.put_back_under.is_none(), "undeleteNode puts it back where it was");

        let unlisted = nodes.iter().find(|d| d.node.id == stray).expect("the unlisted node is listed");
        assert!(unlisted.deleted_at.is_none());
        assert_eq!(unlisted.put_back_under, Some(root));

        // Neither the folder (holds a tombstone but is not one itself) nor
        // the root (the store's own, never a candidate) is listed.
        assert!(!nodes.iter().any(|d| d.node.id == folder || d.node.id == root));
    }

    #[test]
    fn undeleting_a_node_puts_it_back_under_the_parent_it_was_deleted_from() {
        let (mut peer, root) = origin();
        let folder = NodeId::new();
        peer.add_node(folder, Some(root), None, "folder", "Notes", T0).unwrap();
        let note = NodeId::new();
        peer.add_node(note, Some(folder), None, "document", "Note", T0).unwrap();
        peer.remove_node(note, T1).unwrap();
        assert!(peer.get_node_info(note).is_err(), "a tombstone answers nothing to get_node_info");

        // What `VaultClient::undelete_node` does: `Tree::undelete_node`, then
        // read the parent it went back to, which is what its `NodeCreated`
        // names.
        peer.undelete_node(note, T2).unwrap();
        let info = peer.get_node_info(note).unwrap();
        assert_eq!(info.parent_id, Some(folder));
        assert_eq!(peer.get_children(folder).unwrap(), vec![note]);
    }

    #[test]
    fn a_reader_s_transplant_and_undelete_are_refused_with_the_read_only_sentence() {
        let (store_id, other_id) = (StoreId::new(), StoreId::new());
        let root = NodeId::new();
        let mut client = VaultClient::new("me".to_string());
        client.rows.insert(store_id, row(vec![grant(Some(root), "reader", "Recipes")], Some("ann@example.com")));

        let transplant = BackendCommand::TransplantNode {
            from_store_id: store_id,
            node_id: NodeId::new(),
            to_store_id: other_id,
            new_parent_id: NodeId::new(),
            position: None,
        };
        let refused = client.refuse_write(store_id, &transplant).expect("a reader may not transplant out of their root");
        assert!(matches!(&refused, BackendEvent::Error { message } if message == StoreAccess::READ_ONLY_REFUSAL));

        let undelete = BackendCommand::UndeleteNode { store_id, node_id: NodeId::new() };
        let refused = client.refuse_write(store_id, &undelete).expect("a reader may not undelete either");
        assert!(matches!(&refused, BackendEvent::Error { message } if message == StoreAccess::READ_ONLY_REFUSAL));
    }
}
