//! Store types - containers for trees of nodes

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use crate::NodeId;

/// Unique identifier for a store
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StoreId(pub Uuid);

impl StoreId {
    /// Create a new random StoreId
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create a StoreId from an existing UUID
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Parse a StoreId from a string
    pub fn parse(s: &str) -> Result<Self, uuid::Error> {
        Ok(Self(Uuid::parse_str(s)?))
    }

    /// Get the underlying UUID
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for StoreId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A Store represents an entry point to a tree of nodes
///
/// Stores can be:
/// - Local: Stored on the local filesystem
/// - Remote: Accessed via a remote server
/// - Mounted: A subtree of another store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    /// Unique identifier for this store
    pub id: StoreId,

    /// Display name for the store
    pub name: String,

    /// Where the store data lives
    pub location: StoreLocation,

    /// The root node of this store's tree
    pub root_node_id: NodeId,

    /// Current synchronization state
    pub sync_state: SyncState,

    /// Whether this is a replica the server created for a remote store
    /// (`addRemoteStore`), living in the server's replicas directory. Only
    /// a replica can be removed with `removeReplica`. Filled in by the
    /// server on every `Store` it returns.
    #[serde(default)]
    pub is_replica: bool,

    /// `Plain` stores hold readable documents the server merges and indexes;
    /// `Vault` stores hold only encrypted blobs (docs/CRYPTO_CONTRACT.md) and
    /// answer nothing but the vault RPCs. Missing in older serializations:
    /// plain.
    #[serde(default)]
    pub kind: StoreKind,

    /// What this store is currently *linked* to, distinct from `kind` (this
    /// store's own kind, always `Plain` for a desktop store even when it is
    /// vault-linked — docs/CRYPTO_CONTRACT.md: "the local store itself stays
    /// plain on disk"). `Plain` means unlinked or linked by an ordinary
    /// replica sync link to a `Plain` twin; `Vault` means linked by an
    /// encrypting vault link to a `Vault` twin (`cloudHostStore`/
    /// `cloudAddHostedStore`). This is what a UI checks to show "encrypted"
    /// on the sync badge instead of the ordinary sync icon. Missing in
    /// older serializations: plain (unlinked).
    #[serde(default)]
    pub sync_mode: StoreKind,

    /// What this device may change in the store (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5): `Full` for one's own stores and for an editor's scope, `Read`
    /// for a reader. Missing in older serializations: full.
    #[serde(default)]
    pub access: StoreAccess,

    /// An owner's email when this store reached this device as someone else's
    /// share. `None` for one's own stores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_by: Option<String>,

    /// The roots this device holds of the store: `[root_node_id]` for a whole
    /// store, the scope roots for a partial replica (a share's recipient). Empty
    /// in older serializations: read it as `[root_node_id]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<NodeId>,

    /// The roots among [`Store::roots`] this device may only read while it
    /// edits others (a role per shared root), plus roots the account no
    /// longer holds at all: `sync.json`'s `read_only_roots`. Empty when
    /// `access` answers for the whole store. What a node under one of them
    /// may be used for is on the node itself ([`crate::Node::access`]); this
    /// is here so a client can tell when that judgement has changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only_roots: Vec<NodeId>,

    /// The roots among [`Store::held_roots`] the account no longer holds: it
    /// was removed from that share, or the share was stopped
    /// (`StoreManifest::ended_roots`); for a whole replica, its own root once
    /// the account holds the store no more. They are still on this device,
    /// read only, until the replica is removed; they are not shown
    /// ([`Store::shown_roots`]). Missing in older serializations: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ended_roots: Vec<NodeId>,

    /// Whether the store is reached through Pimble Cloud's relay, and from
    /// which end (docs/RELAY_CONTRACT.md). Beside `sync_mode`, which stays
    /// `Vault` for a relayed store (its link is a vault link, and a client
    /// from before the relay reads it as the encrypted link it is). Missing
    /// in older serializations: not relayed.
    #[serde(default, skip_serializing_if = "RelaySide::is_none")]
    pub relay: RelaySide,

    /// The documents of this store its link is holding back, each with the
    /// sentence that says why (docs/SYNC_LIMITS_CONTRACT.md, "Saying what
    /// waits"): an update the remote would not take, a blob no key held
    /// here opens, parts made by a newer Pimble. Every other document keeps
    /// syncing. Missing in older serializations: nothing waits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting: Vec<WaitingDoc>,

    /// The documents of this store that are out of step on this device
    /// (docs/CONVERGENCE_CONTRACT.md section 4): what this device holds and
    /// what the other devices say they hold have differed, and not closed,
    /// for five minutes of connection. Shown on the note, never on the store
    /// row. Missing in older serializations: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub out_of_step: Vec<OutOfStep>,

    /// The other computers that open this store's folder through a sync tool
    /// (docs/STORE_FOLDERS_CONTRACT.md rule 5), from the folder's
    /// `computers/` files, most recently seen first. Missing in older
    /// serializations: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_open_on: Vec<OtherComputer>,

    /// How this store's search index is doing on this device
    /// (docs/STARTUP_CONTRACT.md): a store answers its open before its index
    /// is ready, and its row says "indexing" until it is. Missing in older
    /// serializations (and for a store with no index here): ready.
    #[serde(default, skip_serializing_if = "IndexState::is_ready")]
    pub index: IndexState,
}

/// How a store's search index is doing on this device
/// (docs/STARTUP_CONTRACT.md "Search never holds a store's opening").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum IndexState {
    /// Searchable (and the state of a store that has no index here).
    #[default]
    Ready,
    /// Being opened.
    Opening,
    /// Being built from the store's notes: `done` of `total` so far.
    Rebuilding { done: usize, total: usize },
    /// Another program on this computer holds it (a Pimble server that is
    /// handing over): it is opened again when that one lets go, never
    /// rebuilt over.
    Held,
    /// It could not be opened: the sentence that says why. Search skips the
    /// store until "Rebuild search index".
    Unavailable { reason: String },
}

impl IndexState {
    pub fn is_ready(&self) -> bool {
        matches!(self, IndexState::Ready)
    }

    /// Whether search over the store may miss notes because its index is
    /// still on its way (not ready, and not given up on).
    pub fn is_indexing(&self) -> bool {
        matches!(self, IndexState::Opening | IndexState::Rebuilding { .. } | IndexState::Held)
    }

    /// The quiet words on the store's row while its index is on its way:
    /// "indexing 484 notes", "indexing 120 of 484 notes", "opening search";
    /// empty when there is nothing to say.
    pub fn row_words(&self) -> String {
        let notes = |n: usize| if n == 1 { "1 note".to_string() } else { format!("{n} notes") };
        match self {
            IndexState::Ready => String::new(),
            IndexState::Opening => "opening search".to_string(),
            IndexState::Rebuilding { done: 0, total } => format!("indexing {}", notes(*total)),
            IndexState::Rebuilding { done, total } => format!("indexing {done} of {}", notes(*total)),
            IndexState::Held => "search waiting".to_string(),
            IndexState::Unavailable { .. } => "search unavailable".to_string(),
        }
    }

    /// The row's tooltip for the same.
    pub fn row_tooltip(&self) -> String {
        match self {
            IndexState::Ready => String::new(),
            IndexState::Opening => "Opening this store's search index. Search may miss its notes until it is open.".to_string(),
            IndexState::Rebuilding { .. } => "Building this store's search index. Search may miss its notes until it is done.".to_string(),
            IndexState::Held => "Another Pimble program on this computer still holds this store's search index. It opens as soon as that program lets go.".to_string(),
            IndexState::Unavailable { reason } => format!("This store's search index could not be opened: {reason}. View > Rebuild search index tries again."),
        }
    }

    /// What a search covering stores still being indexed says above its
    /// results: "Personal is still being indexed; results may be
    /// incomplete." Empty for none.
    pub fn search_note(names: &[String]) -> String {
        match names {
            [] => String::new(),
            [one] => format!("{one} is still being indexed; results may be incomplete."),
            [init @ .., last] => format!("{} and {last} are still being indexed; results may be incomplete.", init.join(", ")),
        }
    }
}

/// Another computer that opens a store's folder (docs/STORE_FOLDERS_CONTRACT.md
/// rule 5): its name, when it last said it was there, and its Pimble.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherComputer {
    pub name: String,
    pub last_seen: chrono::DateTime<chrono::Utc>,
    pub version: String,
    /// Whether its Pimble is older than the one answering.
    #[serde(default)]
    pub older: bool,
}

impl OtherComputer {
    /// "Also open on joe-desktop (seen 2 minutes ago); changes arrive
    /// through the folder's sync."
    pub fn sentence(&self, now: chrono::DateTime<chrono::Utc>) -> String {
        format!("Also open on {} (seen {}); changes arrive through the folder's sync.", self.name, ago(now - self.last_seen))
    }

    /// For a computer running an older Pimble, the sentence that asks for it
    /// to be updated; `None` otherwise.
    pub fn update_sentence(&self) -> Option<String> {
        self.older.then(|| {
            format!(
                "{} runs Pimble {}, which can write over changes made here in this folder. Please update Pimble on {}.",
                self.name, self.version, self.name
            )
        })
    }
}

/// "just now", "1 minute ago", "3 hours ago", "2 days ago".
fn ago(elapsed: chrono::Duration) -> String {
    let minutes = elapsed.num_minutes().max(0);
    let plural = |n: i64, unit: &str| if n == 1 { format!("1 {unit} ago") } else { format!("{n} {unit}s ago") };
    match minutes {
        0 => "just now".to_string(),
        m if m < 60 => plural(m, "minute"),
        m if m < 60 * 24 => plural(m / 60, "hour"),
        m => plural(m / (60 * 24), "day"),
    }
}

/// Whether Pimble version `a` is older than `b` (dotted numbers; a part that
/// does not parse counts as 0, and a version that says nothing is older than
/// any that does).
pub fn version_older(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.trim().trim_start_matches('v').split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    let (mut a, mut b) = (parts(a), parts(b));
    a.resize(3, 0);
    b.resize(3, 0);
    a < b
}

/// One document a sync link is holding back, and why, in a sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingDoc {
    pub node_id: NodeId,
    pub reason: String,
}

/// One document out of step on this device (docs/CONVERGENCE_CONTRACT.md
/// section 4): since when, and why, in a sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutOfStep {
    pub node_id: NodeId,
    /// When the difference began (not when the five minutes ran out); for
    /// [`OutOfStepKind::SetAside`], when what waited was set aside.
    pub since: chrono::DateTime<chrono::Utc>,
    pub reason: String,
    /// Which of the reasons it is, which decides where it is said (Joe,
    /// 2026-11-20: a mark on a note only when someone can act). Absent in an
    /// entry from a Pimble before wave 3, which is read as shown.
    #[serde(default)]
    pub kind: OutOfStepKind,
}

/// Why a document is out of step (docs/CONVERGENCE_CONTRACT.md, "When 'out of
/// step' is shown"). The first two are shown on the note (its row, its pane,
/// its Properties, the MCP); the last two only in its Properties and the MCP,
/// quietly: nobody can do anything about them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutOfStepKind {
    /// Another device holds changes this one lacks.
    #[default]
    Behind,
    /// This device holds changes another lacks and could not send them.
    Unsent,
    /// It waits for earlier changes no device has sent.
    Waiting,
    /// What waited for changes no device holds was set aside (section 5).
    SetAside,
}

/// What a Pimble before wave 3 said of a parked update: an entry of an older
/// server carrying it is quiet like [`OutOfStepKind::Waiting`].
const PARKED_BEFORE_WAVE_3: &str = "Waits for earlier changes another device has not sent.";

impl OutOfStep {
    /// Whether the note shows it: a mark on its row and a line at the top of
    /// its pane. Every kind is said in its Properties and by the MCP.
    pub fn is_shown(&self) -> bool {
        matches!(self.kind, OutOfStepKind::Behind | OutOfStepKind::Unsent) && self.reason != PARKED_BEFORE_WAVE_3
    }

    /// The one line the app and the MCP say. Shown kinds: "Out of step since
    /// 19 Nov 2026, 14:05: <reason>". Quiet ones: "Some changes from another
    /// device never arrived (waiting since 26 Oct 2026)." and "Some changes
    /// from another device never arrived and were set aside on 27 Oct 2026."
    /// `when` is `since` as the caller shows it (the viewer's own time zone,
    /// or UTC for the MCP).
    pub fn sentence(&self, when: &str) -> String {
        let bare = self.reason.trim_end_matches('.');
        match self.kind {
            OutOfStepKind::Waiting => format!("{bare} (waiting since {when})."),
            OutOfStepKind::SetAside => format!("{bare} on {when}."),
            _ if self.reason == PARKED_BEFORE_WAVE_3 => format!("{bare} (waiting since {when})."),
            OutOfStepKind::Behind | OutOfStepKind::Unsent => format!("Out of step since {when}: {}", self.reason),
        }
    }
}

/// "1 note waiting", "3 notes waiting": what a store's badge adds while its
/// link holds documents back. Empty when nothing waits.
pub fn waiting_phrase(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => "1 note waiting".to_string(),
        n => format!("{n} notes waiting"),
    }
}

impl Store {
    /// The roots to show. A whole store (no root listed) shows its own root.
    /// A partial replica shows the roots it lists, less the ones that have
    /// ended ([`Store::ended_roots`]); when every one has ended that is no
    /// root at all, never the fallback to `root_node_id`, which is a whole
    /// store's alone (a partial replica's `root_node_id` is its first scope
    /// root: the very folder that is no longer shared).
    ///
    /// A whole replica whose account holds the store no more (removed from
    /// it, or another account signed in on this device) has its own root
    /// ended, and so shows nothing either: it is what a share's replica is
    /// once every share ended (Joe, 2026-09-21), the same row and the same
    /// "Remove Replica...".
    pub fn shown_roots(&self) -> Vec<NodeId> {
        self.held_roots().into_iter().filter(|root| !self.ended_roots.contains(root)).collect()
    }

    /// The roots this store can show: the ones a partial replica lists, or
    /// a whole store's own root. What [`Store::ended_roots`] is a subset of.
    pub fn held_roots(&self) -> Vec<NodeId> {
        if self.roots.is_empty() {
            vec![self.root_node_id]
        } else {
            self.roots.clone()
        }
    }

    /// Whether this is a replica the account holds nothing of any more: a
    /// share's replica whose every share has ended, or a whole replica whose
    /// root has. It shows no root at all.
    pub fn every_share_ended(&self) -> bool {
        !self.ended_roots.is_empty() && self.held_roots().iter().all(|root| self.ended_roots.contains(root))
    }

    /// Create a new local store
    pub fn new_local(name: impl Into<String>, path: PathBuf) -> Self {
        Self {
            id: StoreId::new(),
            name: name.into(),
            location: StoreLocation::Local { path },
            root_node_id: NodeId::new(),
            sync_state: SyncState::Offline,
            is_replica: false,
            kind: StoreKind::Plain,
            sync_mode: StoreKind::Plain,
            access: StoreAccess::Full,
            shared_by: None,
            roots: Vec::new(),
            read_only_roots: Vec::new(),
            ended_roots: Vec::new(),
            relay: RelaySide::None,
            waiting: Vec::new(),
            out_of_step: Vec::new(),
            also_open_on: Vec::new(),
            index: IndexState::Ready,
        }
    }

    /// Create a new remote store
    pub fn new_remote(name: impl Into<String>, url: Url, auth: AuthMethod) -> Self {
        Self {
            id: StoreId::new(),
            name: name.into(),
            location: StoreLocation::Remote { url, auth },
            root_node_id: NodeId::new(),
            sync_state: SyncState::Offline,
            is_replica: false,
            kind: StoreKind::Plain,
            sync_mode: StoreKind::Plain,
            access: StoreAccess::Full,
            shared_by: None,
            roots: Vec::new(),
            read_only_roots: Vec::new(),
            ended_roots: Vec::new(),
            relay: RelaySide::None,
            waiting: Vec::new(),
            out_of_step: Vec::new(),
            also_open_on: Vec::new(),
            index: IndexState::Ready,
        }
    }

    /// Check if this is a local store
    pub fn is_local(&self) -> bool {
        matches!(self.location, StoreLocation::Local { .. })
    }

    /// Check if this is a remote store
    pub fn is_remote(&self) -> bool {
        matches!(self.location, StoreLocation::Remote { .. })
    }

    /// Get the local path if this is a local store
    pub fn local_path(&self) -> Option<&PathBuf> {
        match &self.location {
            StoreLocation::Local { path } => Some(path),
            _ => None,
        }
    }
}

/// Where a store's data is located
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoreLocation {
    /// Local filesystem directory
    Local {
        /// Path to the .pimble directory
        path: PathBuf,
    },

    /// Remote server
    Remote {
        /// Server URL
        url: Url,
        /// Authentication method
        auth: AuthMethod,
    },

    /// Mounted subtree of another store
    Mounted {
        /// The parent store
        store_id: StoreId,
        /// The node to use as root
        node_id: NodeId,
    },
}

/// How to reach another Pimble server: the URL of its RPC endpoint and how
/// to authenticate. Used by replica sync links (`docs/SYNC_CONTRACT.md`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteEndpoint {
    pub url: Url,
    pub auth: AuthMethod,
}

/// What a store holds on the server (docs/CRYPTO_CONTRACT.md "Data model").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StoreKind {
    #[default]
    Plain,
    Vault,
}

/// Whether a store is reached through Pimble Cloud's relay, and which end of
/// it this device is (docs/RELAY_CONTRACT.md). A relayed store is not hosted:
/// its encrypted twin lives on its owner's own machine, and Pimble Cloud
/// pipes members' connections to it and keeps nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RelaySide {
    /// Not relayed: unlinked, linked to another Pimble server, or linked to
    /// a twin hosted on Pimble Cloud.
    #[default]
    None,
    /// Shared from this computer (`cloudRelayStore`): this server holds the
    /// twin and serves it through the relay. People reach it while this
    /// computer is on and online.
    Owner,
    /// Served from its owner's computer: this device's link reaches it
    /// through the relay, and is `Offline` whenever the owner's computer is.
    Member,
}

impl RelaySide {
    /// Whether this is `None`, the value an answer leaves out.
    pub fn is_none(&self) -> bool {
        matches!(self, RelaySide::None)
    }
}

/// What this device may change in a store (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5): an editor edits everything in scope, a reader reads, a
/// commenter reads and writes discussions only (docs/DISCUSSIONS_CONTRACT.md
/// section 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StoreAccess {
    #[default]
    Full,
    Read,
    Comment,
}

impl StoreAccess {
    /// What a refused write answers with, everywhere (the server's `-32004`,
    /// the browser backend's own refusal, the app's notice): the sentence
    /// alone, written for the person, shown as it is.
    pub const READ_ONLY_REFUSAL: &'static str = "You can read this, not change it.";

    /// Whether `message` is the refusal (with or without a `Forbidden: ` in front).
    pub fn refusal_in(message: &str) -> Option<&'static str> {
        let sentence = message.strip_prefix("Forbidden: ").unwrap_or(message);
        [Self::READ_ONLY_REFUSAL, Self::COMMENT_ONLY_REFUSAL].into_iter().find(|r| *r == sentence)
    }

    /// What a commenter's refused write answers with.
    pub const COMMENT_ONLY_REFUSAL: &'static str = "You can comment here, not change it.";

    pub fn allows_write(self) -> bool {
        matches!(self, StoreAccess::Full)
    }

    /// The sentence a write refused under this access answers with: a
    /// commenter's, or a reader's.
    pub fn refusal(self) -> &'static str {
        match self {
            StoreAccess::Comment => Self::COMMENT_ONLY_REFUSAL,
            _ => Self::READ_ONLY_REFUSAL,
        }
    }

    /// Whether discussions may be started and written (a commenter's or an
    /// editor's).
    pub fn allows_comment(self) -> bool {
        matches!(self, StoreAccess::Full | StoreAccess::Comment)
    }

    /// Whether this is `Full`, the value an answer leaves out
    /// (`Node::access`'s `skip_serializing_if`).
    pub fn is_full(&self) -> bool {
        matches!(self, StoreAccess::Full)
    }

    // ── The role judgement (docs/NODE_DOCUMENT_CONTRACT.md section 5,
    // "Roles"; docs/DISCUSSIONS_CONTRACT.md section 7). One rule, read by
    // the hosted server, a desktop's replica and the browser's vault client:
    // a role's access, the widest of the roles that reach a document, held
    // no wider than the device holds the store, and a commenter writing a
    // discussion as an editor does.

    /// What a grant's role lets its holder do, by the accounts service's
    /// names: a reader reads, a commenter writes discussions, an editor or
    /// owner edits. Any other name reads as an editor's: the server that
    /// takes the write judges it again, from the token.
    pub fn of_role(role: &str) -> StoreAccess {
        match role {
            "reader" => StoreAccess::Read,
            "commenter" => StoreAccess::Comment,
            _ => StoreAccess::Full,
        }
    }

    /// How much this access allows, for comparing two.
    fn rank(self) -> u8 {
        match self {
            StoreAccess::Read => 0,
            StoreAccess::Comment => 1,
            StoreAccess::Full => 2,
        }
    }

    /// The widest of several roles' access (`None` for none): a document
    /// under a reader's root nested in an editor's takes the wider role,
    /// and a reader of one folder who edits another is `Full` in the store
    /// as a whole, the writes under the read-only root refused one at a time.
    pub fn widest(accesses: impl IntoIterator<Item = StoreAccess>) -> Option<StoreAccess> {
        accesses.into_iter().max_by_key(|access| access.rank())
    }

    /// The narrower of two: what a role allows, held no wider than the
    /// device holds the store (a replica held to read refuses an owner's
    /// writes too).
    pub fn narrower(self, other: StoreAccess) -> StoreAccess {
        if other.rank() < self.rank() {
            other
        } else {
            self
        }
    }

    /// What this access allows of one document: a commenter writes a
    /// discussion as an editor does, and only comments on anything else.
    pub fn on_document(self, is_discussion: bool) -> StoreAccess {
        match self {
            StoreAccess::Comment if is_discussion => StoreAccess::Full,
            access => access,
        }
    }
}

/// Authentication method for remote stores
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AuthMethod {
    /// No authentication
    None,

    /// API key authentication
    ApiKey {
        /// The API key (should be stored securely)
        key: String,
    },

    /// Bearer token authentication
    Bearer {
        /// The bearer token
        token: String,
    },

    /// OAuth2 authentication
    OAuth2 {
        /// Client ID
        client_id: String,
        /// Refresh token (access token is obtained dynamically)
        refresh_token: String,
    },

    /// A Pimble Cloud account session (docs/CRYPTO_CONTRACT.md "Desktop (E,
    /// after B)"): `url` is the accounts service's base URL (not the Pimble
    /// server the link actually connects to — that is `RemoteEndpoint.url`),
    /// `session` is the long-lived session token `POST /api/v1/login`
    /// returned. Never connected with directly: a caller mints a short-lived
    /// JWT via `POST {url}/api/v1/token` (`Authorization: Bearer <session>`)
    /// first and connects with that as `Bearer`.
    CloudSession {
        url: String,
        session: String,
    },
}

/// Synchronization state of a store
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SyncState {
    /// Not connected to any remote
    Offline,

    /// Currently synchronizing
    Syncing,

    /// Successfully synchronized
    Synced {
        /// When the last sync completed
        last_sync: DateTime<Utc>,
    },

    /// Has unresolved conflicts
    Conflict {
        /// Details about each conflict
        details: Vec<ConflictInfo>,
    },
}

impl SyncState {
    /// Check if the store is currently synced
    pub fn is_synced(&self) -> bool {
        matches!(self, SyncState::Synced { .. })
    }

    /// Check if there are conflicts
    pub fn has_conflicts(&self) -> bool {
        matches!(self, SyncState::Conflict { .. })
    }
}

/// Information about a sync conflict
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictInfo {
    /// The node with the conflict
    pub node_id: NodeId,

    /// Description of the conflict
    pub description: String,

    /// When the conflict was detected
    pub detected_at: DateTime<Utc>,
}

/// Store manifest - metadata stored in manifest.json
///
/// Version 4 is the current layout (docs/NODE_DOCUMENT_CONTRACT.md section 3):
/// `manifest.json` plus `nodes/{id}.yrs`, one yrs document per node holding
/// its content, its place in the tree and its metadata. Version 3 (the same
/// plus `store.yrs`, the tree as a document of its own) is migrated at open;
/// there is no migration path from anything earlier, so such a store must
/// be re-imported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreManifest {
    /// Schema version. [`StoreManifest::CURRENT_VERSION`] once opened.
    pub version: u32,

    /// Store ID
    pub id: StoreId,

    /// Store name
    pub name: String,

    /// Root node ID. Meaningless for a `Vault` store (docs/CRYPTO_CONTRACT.md):
    /// a vault has no tree of its own on this server, but the field stays
    /// populated (a fresh id) so every manifest has the same shape.
    pub root_node_id: NodeId,

    /// When the store was created
    pub created_at: DateTime<Utc>,

    /// When the store was last modified
    pub modified_at: DateTime<Utc>,

    /// `Plain` (readable, indexed, CRDT tree + content) or `Vault`
    /// (encrypted blobs only, docs/CRYPTO_CONTRACT.md). Missing in older
    /// serializations: plain.
    #[serde(default)]
    pub kind: StoreKind,

    /// A partial replica's scope roots (docs/NODE_DOCUMENT_CONTRACT.md section
    /// 5): the shared nodes this replica holds, when it is a share's recipient
    /// and not a whole copy. Empty for a whole store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_roots: Vec<NodeId>,

    /// The scope roots this account no longer holds: it was removed from
    /// that share, or the share was stopped. A subset of `scope_roots`,
    /// which is never shrunk: it is what makes the replica partial, and a
    /// replica whose every share ended stays partial and read only. A whole
    /// replica (no scope roots) whose account holds the store no more lists
    /// its own `root_node_id` here, and nothing else. The documents stay on
    /// disk until the replica is removed. Missing in older serializations:
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ended_roots: Vec<NodeId>,
}

impl StoreManifest {
    /// Current schema version: the node-document layout.
    pub const CURRENT_VERSION: u32 = 4;

    /// Create a new `Plain` manifest. See [`StoreManifest::new_with_kind`]
    /// for a `Vault` one.
    pub fn new(name: impl Into<String>, root_node_id: NodeId) -> Self {
        Self::new_with_kind(name, root_node_id, StoreKind::Plain)
    }

    /// Create a new manifest of a given [`StoreKind`] (docs/CRYPTO_CONTRACT.md
    /// "Pimble server, a store of kind `vault`").
    pub fn new_with_kind(name: impl Into<String>, root_node_id: NodeId, kind: StoreKind) -> Self {
        let now = Utc::now();
        Self {
            version: Self::CURRENT_VERSION,
            id: StoreId::new(),
            name: name.into(),
            root_node_id,
            created_at: now,
            modified_at: now,
            kind,
            scope_roots: Vec::new(),
            ended_roots: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    /// docs/STARTUP_CONTRACT.md: what a store row and a search say while a
    /// store's index is on its way, and that an older server's store (no
    /// `index`) reads as ready.
    #[test]
    fn index_states_say_what_they_are() {
        use super::IndexState;
        assert_eq!(IndexState::Rebuilding { done: 0, total: 484 }.row_words(), "indexing 484 notes");
        assert_eq!(IndexState::Rebuilding { done: 120, total: 484 }.row_words(), "indexing 120 of 484 notes");
        assert_eq!(IndexState::Rebuilding { done: 0, total: 1 }.row_words(), "indexing 1 note");
        assert_eq!(IndexState::Held.row_words(), "search waiting");
        assert_eq!(IndexState::Ready.row_words(), "");
        assert!(IndexState::Held.is_indexing() && IndexState::Opening.is_indexing());
        assert!(!IndexState::Unavailable { reason: "x".into() }.is_indexing());
        assert_eq!(IndexState::search_note(&["Personal".into()]), "Personal is still being indexed; results may be incomplete.");
        assert_eq!(
            IndexState::search_note(&["Personal".into(), "Family".into(), "Work".into()]),
            "Personal, Family and Work are still being indexed; results may be incomplete."
        );
        assert_eq!(IndexState::search_note(&[]), "");
        let mut store = super::Store::new_local("Notes", "/tmp/n.pimble".into());
        let json = serde_json::to_value(&store).unwrap();
        assert!(json.get("index").is_none(), "ready is not written");
        store.index = IndexState::Rebuilding { done: 3, total: 9 };
        let back: super::Store = serde_json::from_value(serde_json::to_value(&store).unwrap()).unwrap();
        assert_eq!(back.index, store.index);
        let older: super::Store = serde_json::from_value(json).unwrap();
        assert!(older.index.is_ready());
    }

    use super::*;

    #[test]
    fn other_computers_are_named_in_sentences() {
        let now = chrono::Utc::now();
        let seen = |minutes: i64, version: &str, older: bool| OtherComputer {
            name: "joe-desktop".into(),
            last_seen: now - chrono::Duration::minutes(minutes),
            version: version.into(),
            older,
        };
        assert_eq!(seen(2, "0.29.0", false).sentence(now), "Also open on joe-desktop (seen 2 minutes ago); changes arrive through the folder's sync.");
        assert_eq!(seen(0, "0.29.0", false).sentence(now), "Also open on joe-desktop (seen just now); changes arrive through the folder's sync.");
        assert!(seen(61, "0.29.0", false).sentence(now).contains("seen 1 hour ago"));
        assert!(seen(3 * 24 * 60, "0.29.0", false).sentence(now).contains("seen 3 days ago"));
        assert_eq!(seen(2, "0.29.0", false).update_sentence(), None);
        assert_eq!(
            seen(2, "0.28.8", true).update_sentence().as_deref(),
            Some("joe-desktop runs Pimble 0.28.8, which can write over changes made here in this folder. Please update Pimble on joe-desktop.")
        );
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(version_older("0.28.8", "0.28.9"));
        assert!(version_older("0.9.0", "0.10.0"));
        assert!(version_older("", "0.1.0"));
        assert!(!version_older("0.28.9", "0.28.9"));
        assert!(!version_older("v1.0.0", "0.99.99"));
        assert!(!version_older("0.29.0-beta", "0.29.0"));
    }

    fn partial(roots: Vec<NodeId>, ended: Vec<NodeId>) -> Store {
        let mut store = Store::new_local("Shared by ana@example.com", PathBuf::from("/tmp/replica.pimble"));
        store.root_node_id = roots[0];
        store.roots = roots;
        store.ended_roots = ended;
        store
    }

    #[test]
    fn a_whole_store_shows_its_own_root() {
        let store = Store::new_local("Notes", PathBuf::from("/tmp/notes.pimble"));
        assert_eq!(store.shown_roots(), vec![store.root_node_id]);
        assert!(!store.every_share_ended());
    }

    #[test]
    fn an_ended_root_is_not_shown() {
        let (trips, recipes) = (NodeId::new(), NodeId::new());
        let store = partial(vec![trips, recipes], vec![trips]);
        assert_eq!(store.shown_roots(), vec![recipes]);
        assert!(!store.every_share_ended());
    }

    /// The fallback to `root_node_id` is a whole store's: a partial replica's
    /// `root_node_id` is its first scope root, the folder that ended.
    #[test]
    fn a_replica_whose_every_root_ended_shows_nothing() {
        let (trips, recipes) = (NodeId::new(), NodeId::new());
        let store = partial(vec![trips, recipes], vec![recipes, trips]);
        assert_eq!(store.shown_roots(), Vec::<NodeId>::new());
        assert!(store.every_share_ended());
    }

    /// A whole replica the account holds no more (Joe, 2026-09-21's ended
    /// share, for a whole-store grant): its own root has ended, so it shows
    /// nothing and reads as a replica whose every share ended.
    #[test]
    fn a_whole_replica_whose_root_ended_shows_nothing() {
        let mut store = Store::new_local("Notes", PathBuf::from("/tmp/notes.pimble"));
        assert_eq!(store.held_roots(), vec![store.root_node_id]);
        store.ended_roots = vec![store.root_node_id];
        assert_eq!(store.shown_roots(), Vec::<NodeId>::new());
        assert!(store.every_share_ended());
        store.ended_roots = vec![NodeId::new()];
        assert_eq!(store.shown_roots(), vec![store.root_node_id], "only its own root ends a whole store");
        assert!(!store.every_share_ended());
    }

    #[test]
    fn ended_roots_are_absent_from_older_serializations_and_skipped_when_empty() {
        let trips = NodeId::new();
        let store = partial(vec![trips], Vec::new());
        let json = serde_json::to_value(&store).unwrap();
        assert!(json.get("ended_roots").is_none(), "skipped when empty: {json}");
        let back: Store = serde_json::from_value(json).unwrap();
        assert!(back.ended_roots.is_empty());

        let ended = partial(vec![trips], vec![trips]);
        let back: Store = serde_json::from_value(serde_json::to_value(&ended).unwrap()).unwrap();
        assert_eq!(back.ended_roots, vec![trips]);
    }

    #[test]
    fn a_manifest_from_before_ended_roots_reads_as_none() {
        let manifest = StoreManifest::new("Trips", NodeId::new());
        let mut json = serde_json::to_value(&manifest).unwrap();
        assert!(json.get("ended_roots").is_none(), "skipped when empty: {json}");
        json.as_object_mut().unwrap().insert("scope_roots".into(), serde_json::json!([manifest.root_node_id]));
        let old: StoreManifest = serde_json::from_value(json).unwrap();
        assert_eq!(old.scope_roots, vec![manifest.root_node_id]);
        assert!(old.ended_roots.is_empty());
    }
}
