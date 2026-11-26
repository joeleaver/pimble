//! What the backend tells the UI: [`BackendEvent`] and what an event carries
//! ([`StoredBlob`]).

use pimble_core::{BlobUrl, DeletedNode, LeftShare, LinkResolution, MountRef, MountState, Node, NodeId, PimbleUrl, RemoteEndpoint, Store, StoreId, SyncState};

use super::{Asked, CloudOp, SearchPurpose};

/// What a note a notice names is called (`NotesNamed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteName {
    /// The label, as the explorer shows it.
    pub label: String,
    /// Whether the label is read from the note's text
    /// (`pimble_core::plan::label_follows_text`), so an edit of the text
    /// alone can change it.
    pub follows_text: bool,
}

/// A picture that was stored (`BackendEvent::BlobStored`).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredBlob {
    pub url: BlobUrl,
    /// Its size in pixels as stored (after any scaling), when it could be
    /// read: an image node places it at one board unit per pixel.
    pub size: Option<(u32, u32)>,
    /// "This picture was 34.2 MiB, so it was scaled to ...", when it was.
    pub notice: Option<String>,
}

/// Events sent from backend to UI
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Connected { server_addr: String, client_id: String },
    Disconnected,
    Error { message: String },
    /// A sentence for the status bar that is not an error: nothing failed
    /// and the connection is fine (the browser declining to keep its
    /// storage, docs/ONE_CODEBASE_CONTRACT.md wave 4).
    Notice { message: String },
    /// A request a modal waits on failed (`Asked::answer`): the sentence
    /// goes to that modal.
    Failed { asked: Asked, message: String },

    // Store events
    StoreCreated { store_id: StoreId, root_node_id: NodeId },
    StoreOpened { store: Store },
    /// `OpenStore` of `path` failed: its row says why
    /// (docs/STARTUP_CONTRACT.md "A store that fails to open says why on its
    /// row").
    StoreOpenFailed { path: String, message: String },
    /// A store's search index changed state on its device: the answer to
    /// `GetStoreSync` carries it, so a change to ready that happened before
    /// the store's subscription is never missed (docs/STARTUP_CONTRACT.md).
    IndexStateChanged { store_id: StoreId, state: pimble_core::IndexState },
    StoreClosed { store_id: StoreId },
    /// Answer to `ListStores`: every store the server currently has open.
    StoresListed { stores: Vec<Store> },

    // Node events
    /// `request` is the `NodeInit::request` the node was made with.
    NodeCreated { store_id: StoreId, parent_id: Option<NodeId>, node_id: NodeId, request: Option<u64> },
    /// An attachment was made (`CreateAttachment`); `request` is its init's.
    AttachmentCreated { store_id: StoreId, host_id: NodeId, node_id: NodeId, request: Option<u64> },
    /// A node's live attachments, oldest first, each with its content.
    AttachmentsListed { store_id: StoreId, node_id: NodeId, nodes: Vec<Node> },
    /// A store's discussions, each with its content.
    DiscussionsListed { store_id: StoreId, nodes: Vec<Node> },
    /// The personal document as merged with the account's other devices'.
    /// `off`: another device turned it off, and this one stops too.
    PersonalSynced { state: Vec<u8>, off: bool },
    NodeLoaded { store_id: StoreId, node: Node },
    /// `GetNode` failed: the node named could not be read (offline, gone,
    /// not readable here). The UI takes it as an `Error` and forgets that it
    /// asked, so a later need asks again.
    NodeNotLoaded { store_id: StoreId, node_id: NodeId, message: String },
    /// `children` live in `children_store_id`: the same as `store_id` for an
    /// ordinary parent, the mount's source store when `parent_id` is a mount point.
    ChildrenLoaded { store_id: StoreId, parent_id: NodeId, children_store_id: StoreId, children: Vec<Node> },
    NodeContentUpdated { store_id: StoreId, node_id: NodeId },
    /// What a change did to the rows and lists of a store's tree, read from
    /// the documents held after it was merged (`pimble_api::View`): the
    /// tree updates exactly those, whoever made the change, with nothing
    /// fetched again (docs/API_CONTRACT.md wave 5). A plain store's come
    /// from the API's change stream and from this window's own structural
    /// calls; an encrypted store's in the browser from what its vault
    /// decrypts.
    Shown { view: pimble_api::View },
    /// A metanode's forest as its source found it, or why it could not be
    /// read.
    /// `lives` says where each found node lives ("Personal / Projects"), for
    /// a found row's tooltip and refusals (docs/QUERY_METANODES_CONTRACT.md).
    MetanodeRead { store_id: StoreId, node_id: NodeId, forest: Result<pimble_api::Forest, String>, lives: Vec<(pimble_api::Addr, String)> },
    /// The answer to a plugin's call (`PluginCall`), for the instance that
    /// made it.
    PluginReply { instance: u64, call_id: u64, reply: pimble_plugin_host::abi::api::ApiReply },
    NodeDeleted { store_id: StoreId, node_id: NodeId, parent_id: NodeId },
    /// A split made `new_id` below `node_id` (`SplitNode`).
    NodeSplit { store_id: StoreId, node_id: NodeId, new_id: NodeId },
    NodeMoved { store_id: StoreId, node_id: NodeId, old_parent_id: NodeId, new_parent_id: NodeId },
    /// A move that crossed a share's or a store's boundary
    /// (docs/MOVE_CONTRACT.md): `old_node_id` is now a tombstone under
    /// `old_parent_id` in `from_store_id`, and `node_id` is the new node under
    /// `new_parent_id` in `to_store_id` (the same store for a move between
    /// shares). `left_shares` names the shares it left, nearest first, for the
    /// notice: `"<title>" was moved out of "<share's name>". The people it is
    /// shared with see it as deleted and can put it back.`
    NodeTransplanted {
        from_store_id: StoreId,
        old_node_id: NodeId,
        old_parent_id: NodeId,
        to_store_id: StoreId,
        node_id: NodeId,
        new_parent_id: NodeId,
        title: String,
        left_shares: Vec<LeftShare>,
    },
    /// Answer to `ListDeleted`.
    DeletedListed { store_id: StoreId, nodes: Vec<DeletedNode> },
    /// Answer to `ResolveLink`: `url` as asked, and where it leads. A
    /// `StoreNotHere` in another store the app holds is asked again there
    /// (whoever holds a store answers for it).
    LinkResolved { url: PimbleUrl, hops: usize, resolution: LinkResolution },
    /// Answer to `GetAncestors`: the store's root first, down to `node_id`'s
    /// parent. Empty for the root, or when the chain could not be read.
    /// `kind` is the node's internal kind, if it is one: its row is in its
    /// parent's Internal group, which the reveal opens too.
    AncestorsLoaded { store_id: StoreId, node_id: NodeId, ancestors: Vec<NodeId>, kind: Option<pimble_core::internal::InternalKind> },
    /// Answer to `GetThemeChain`: the node asked for, then its parent, and
    /// so on up, each taken as `NodeLoaded` takes one. Empty when the node
    /// itself could not be read.
    ThemeChainLoaded { store_id: StoreId, nodes: Vec<Node> },
    /// Every theme node of a store (`ThemeCommand::List`, and again
    /// whenever one changes or the store is caught up).
    ThemesListed { store_id: StoreId, themes: Vec<pimble_api::StoreTheme> },
    /// `ThemeCommand::List` could not read the store's themes (the
    /// connection dropped as it asked): the store is asked again when it is
    /// caught up (`Resynced`).
    ThemesNotListed { store_id: StoreId },
    /// A theme node was made to be edited (`ThemeCommand::New`, or a copy
    /// customized): open it in the focused pane.
    ThemeMade { store_id: StoreId, node_id: NodeId },
    /// Answer to `GetProperties`: the nodes above `node_id`, the store's top
    /// first, each with the label its row shows (empty for a store's root,
    /// or when the chain could not be read), and the tags used in the store.
    PropertiesLoaded { store_id: StoreId, node_id: NodeId, lives_in: Vec<(NodeId, String)>, store_tags: Vec<String> },
    /// Answer to `GetReferences`. A part that could not be read is empty,
    /// with `not_known` saying which and why ("What links to a node is not
    /// known here.").
    ReferencesLoaded { store_id: StoreId, node_id: NodeId, references: crate::properties::References },

    // Mount events
    MountCreated {
        store_id: StoreId,
        /// The node the mount was created under (the `Shown` before this
        /// answer carries its list with the new mount in it).
        parent_id: NodeId,
        node_id: NodeId,
        mount_ref: MountRef,
    },
    MountStateChanged {
        store_id: StoreId,
        node_id: NodeId,
        state: MountState,
        mount_ref: MountRef,
    },

    /// Answer to `ReconcileNodeContent`: everything the server has beyond
    /// the session's state vector (may be empty), and the server's own state
    /// vector, so the session can send back what the server lacks.
    NodeContentReconciled { store_id: StoreId, node_id: NodeId, diff: Vec<u8>, server_state_vector: Vec<u8> },

    // Remote change events (from subscriptions)
    RemoteStoreChange { store_id: StoreId, change_kind: pimble_rpc::StoreChangeKind, source_client_id: Option<String> },

    // Collaborative editing
    /// An update of one node's document that this window's views did not
    /// make (another device's, an agent's, a write the API made for this
    /// window such as a rename): every view holding the node merges it
    /// (`crate::doc_session::received`). The node is named because several
    /// views hold different documents at once.
    RemoteChanges { store_id: StoreId, node_id: NodeId, update: Vec<u8> },
    /// A store's changes were not heard for a while (its subscription was
    /// made again after the connection came back) and what the API holds
    /// of it was brought up to date: every view holding one of its nodes
    /// reconciles (what the catch-up changed in the tree follows as
    /// `Shown`).
    Resynced { store_id: StoreId },

    // Pictures
    /// Answer to `PutBlob`: the URL the document should carry and, when the
    /// picture was scaled down to fit, the sentence that says so; or the
    /// sentence that says why nothing was stored.
    BlobStored { request_id: u64, result: Result<StoredBlob, String> },
    /// Answer to `GetBlob`: the picture's encoded bytes, or why there are
    /// none (it has not arrived, or this device may not read it).
    BlobLoaded { url: BlobUrl, result: Result<std::sync::Arc<Vec<u8>>, String> },

    // Search
    /// The outcome of a `Search` command. Carries `Err` rather than folding
    /// into the generic `Error` event so a failed search (including "the
    /// index is still building") shows inline in the results panel without
    /// touching the connection status bar or the reconnect-on-error path.
    SearchResults { purpose: SearchPurpose, results: Result<Vec<pimble_rpc::SearchResultItem>, String> },
    IndexRebuilt { store_id: StoreId, indexed: usize },

    // Replica sync (docs/SYNC_CONTRACT.md "B: app side")
    /// Answer to `ListRemoteStores`: the remote's open stores, or the error
    /// connecting to / querying it (shown inline in the connect modal, never
    /// folded into the generic `Error` event).
    RemoteStoresListed { url: String, result: Result<Vec<Store>, String> },
    /// A store's sync link and state changed: the answer to `SetStoreSync` or
    /// `GetStoreSync`, or a live `SyncStateChanged` notification (which
    /// carries only the state — the event handler keeps the known `remote`).
    /// `sync_mode` is what the link is: `Plain` for an ordinary replica link
    /// (or unlinked), `Vault` for an encrypting vault link, which the store
    /// row's badge shows as "encrypted" (docs/DESKTOP_ACCOUNT_CONTRACT.md
    /// decision 4). `access` and `read_only_roots` are what this device may
    /// change in the store as the server has it now (`Store::access`,
    /// `Store::read_only_roots`): a role the owner changed while the app runs
    /// arrives here, and the handler refetches what it holds of the store
    /// when either differs, because every node carries the server's
    /// judgement of itself (`Node::access`). `ended_roots` are the shares of
    /// the store this account no longer holds (`Store::ended_roots`): a
    /// share's replica keeps their files until it is removed, and the
    /// explorer does not show them. `relay` is which end of Pimble
    /// Cloud's relay this device is for the store, if either
    /// (`Store::relay`, docs/RELAY_CONTRACT.md): the owner's row says
    /// `shared from here`. `owner_offline` is a backend that KNOWS the store
    /// is out of reach because its owner's computer is: the browser's, which
    /// has just heard from Pimble Cloud and cannot reach the store through
    /// its relay. A Pimble server reports a member's link as `Offline` and
    /// nothing more, which is also what no network looks like, so the
    /// desktop never sets it.
    StoreSyncChanged {
        store_id: StoreId,
        remote: Option<RemoteEndpoint>,
        state: SyncState,
        sync_mode: pimble_core::StoreKind,
        access: pimble_core::StoreAccess,
        read_only_roots: Vec<NodeId>,
        ended_roots: Vec<NodeId>,
        relay: pimble_core::RelaySide,
        owner_offline: bool,
        /// The documents the store's link is holding back, each with the
        /// sentence that says why (docs/SYNC_LIMITS_CONTRACT.md, "Saying what
        /// waits"): the row's badge counts them, and the Properties panel of
        /// a waiting node shows its reason.
        waiting: Vec<pimble_core::WaitingDoc>,
        /// The notes out of step on this device (docs/CONVERGENCE_CONTRACT.md
        /// section 4): each one's row, pane and Properties say so.
        out_of_step: Vec<pimble_core::OutOfStep>,
    },
    /// Answer to `RemoveReplica`: the replica is gone. Handled exactly like
    /// `StoreClosed` (tree + saved open-store list cleanup).
    ReplicaRemoved { store_id: StoreId },

    /// A `CreateHostedStore` succeeded. Carries only the name, because the
    /// store itself arrives the ordinary way: the backend mints a token that
    /// carries the new grant and reconnects, and `StoresListed` brings it in.
    HostedStoreCreated { name: String, store_id: Option<StoreId> },

    /// An `Import` finished: `node_id` is the top node it made under
    /// `parent_id`, and `count` how many nodes it made in all.
    Imported { store_id: StoreId, parent_id: Option<NodeId>, node_id: NodeId, title: String, count: usize },
    /// An `Import` stopped, with a sentence that says why (and how much of it
    /// was written before it did).
    ImportFailed { message: String },

    // AI agents (docs/AGENTS_CONTRACT.md).
    /// This device's agents, after any agent command but a create.
    AgentsListed { agents: Vec<pimble_rpc::AgentInfo> },
    /// A new agent and its key, shown this once.
    AgentCreated { agent: pimble_rpc::AgentInfo, key: String },
    /// An agent command failed, in a sentence.
    AgentsError { message: String },

    // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md).
    /// The answer to `CloudStatus`, `CloudSignIn` (`signed_in: true`) and
    /// `CloudSignOut` (`signed_in: false`).
    /// `user_id` is the account's `sub`, what board edits are stamped with.
    /// What `ServerInfo` asked: the server's version, `None` for a server
    /// from before 0.12.2, which has no `serverInfo`.
    ServerInfo { version: Option<String> },
    /// What `SendDiagnostics` did: the report's code, or why it did not go.
    DiagnosticsSent { result: Result<String, String> },
    CloudStatusChanged { signed_in: bool, email: Option<String>, user_id: Option<String>, url: Option<String> },
    /// How this computer's connections to Pimble Cloud are doing, heard from
    /// the server whenever it changes; `None` while signed out (desktop only:
    /// the browser's connection is to Pimble Cloud itself).
    CloudConnectionChanged { connection: Option<pimble_rpc::CloudConnection> },
    /// A notice arrived: someone mentioned this person, or answered them
    /// (docs/NOTIFICATIONS_CONTRACT.md section 4), heard from the server.
    NoticeReceived { notice: pimble_rpc::Notice },
    /// The names `NameNodes` asked for, as the explorer would show them;
    /// `None` for a note this device cannot read.
    NotesNamed { names: Vec<((StoreId, NodeId), Option<NoteName>)> },
    /// The notices this device keeps, newest first, and whether
    /// notifications are on (`ListNotices`, `SetNotifications`).
    NoticesListed { notices: Vec<pimble_rpc::Notice>, on: bool, email: bool, push: bool, push_key: Option<String>, watches: bool },
    /// The watches the account chose, and whether watched notes are on
    /// (`ListWatches`, `WatchNode`, `UnwatchNode`, `SetWatchesOn`).
    WatchesListed { watches: Vec<pimble_rpc::WatchEntry>, on: bool },
    /// A `pimble:` link this computer was asked to open, handed to this window
    /// by the server (`openInApp`; desktop only), with the activation token
    /// the program that was asked came with, to come to the front with, and
    /// the number to confirm it with once acted on (`AppLinkTaken`), when
    /// the launch that handed it waits to hear.
    AppLink { url: String, activation_token: Option<String>, ack: Option<u64> },
    /// A cloud request failed; `op` says which, so the error lands in the
    /// modal that sent it.
    CloudError { op: CloudOp, message: String },
    /// The answer to `CloudListHostedStores`: every store the account has a
    /// grant on, unfiltered (the event handler keeps the ones worth adding).
    /// `relayed` names the rows that are not hosted at all: served from
    /// their owner's computer through Pimble Cloud's relay
    /// (`CloudListHostedStoresResponse::relayed`).
    CloudHostedStoresListed { stores: Vec<pimble_rpc::CloudHostedStoreInfo>, relayed: Vec<String> },
    /// The answer to `CloudListShares`, `CloudSetDisplayName` and
    /// `CloudSetShareName`.
    CloudSharesListed { shares: pimble_rpc::CloudListSharesResponse },
    /// The answer to `GetPeople`.
    PeopleLoaded { store_id: StoreId, people: pimble_rpc::StorePeople },
    /// Someone else's presence in a document (never this client's own).
    PresenceFrame { frame: pimble_rpc::PresenceFrame },
    /// The answer to `CloudHostStore`: the store is hosted and linked in
    /// vault mode.
    CloudStoreHosted { store_id: StoreId },
    /// The answer to `CloudRelayStore`: the store is shared from this
    /// computer. Nothing was uploaded.
    CloudStoreRelayed { store_id: StoreId },
    /// The answer to `CloudStopRelaying`: the store is no longer shared from
    /// this computer, and is unlinked.
    CloudRelayingStopped { store_id: StoreId },
    /// The answer to `CloudShareNode`, `CloudShareInfo`, `CloudShareInvite` and
    /// `CloudShareRemoveMember`: the share and everyone on it.
    CloudShareUpdated { store_id: StoreId, node_id: NodeId, share: pimble_rpc::ShareInfo, members: Vec<pimble_rpc::ShareMember> },
    /// The answer to `CloudStopSharing`.
    CloudSharingStopped { store_id: StoreId, node_id: NodeId },
}
