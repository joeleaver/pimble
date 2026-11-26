//! What the UI asks of the backend: [`BackendCommand`] and what a command
//! carries ([`TagEdit`], [`NodeInit`]).

use pimble_core::{BlobUrl, NodeId, PimbleUrl, Place, RemoteEndpoint, StoreId};

use super::SearchPurpose;


/// One change of a node's tags (`BackendCommand::SetNodeAppearance`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagEdit {
    /// Add this tag, unless the node has it in any case.
    Add(String),
    /// Take this tag off, in whatever case the node has it.
    Remove(String),
}

impl TagEdit {
    /// `tags` with this change made.
    pub fn apply(&self, tags: &[String]) -> Vec<String> {
        let mut out = tags.to_vec();
        match self {
            TagEdit::Add(tag) => {
                let tag = tag.trim();
                if !tag.is_empty() && !out.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                    out.push(tag.to_string());
                }
            }
            TagEdit::Remove(tag) => out.retain(|t| !t.eq_ignore_ascii_case(tag.trim())),
        }
        out
    }
}

/// Commands sent from UI to backend
#[derive(Debug)]
pub enum BackendCommand {
    // Store operations
    CreateStore { path: String, name: String },
    OpenStore { path: String },
    CloseStore { store_id: StoreId },
    /// List every store the server currently has open. Used to discover a
    /// store the server opened implicitly to resolve a mount (decision 4) —
    /// the client sees its id in `ChildrenLoaded.children_store_id` before it
    /// knows anything else about it.
    ListStores,

    // Node operations
    /// `init` is what the node starts with, written in the edit that creates
    /// it; `None` makes an ordinary empty document.
    CreateNode { store_id: StoreId, parent_id: Option<NodeId>, title: String, init: Option<NodeInit>, place: Place },
    /// Make what a plugin's recipe says under `parent_id`, last
    /// (docs/KANBAN_CONTRACT.md "New Kanban"): a metanode with its viewer and
    /// source written in its create, then its first children in order.
    /// Answers with `NodeCreated` for the metanode.
    MakeFromRecipe { store_id: StoreId, parent_id: NodeId, recipe: pimble_plugin_abi::RecipeDecl, place: pimble_core::Place },
    /// Create an attachment of `host_id` (a discussion on it,
    /// docs/DISCUSSIONS_CONTRACT.md section 1): the host is not edited.
    /// Answers with `AttachmentCreated`.
    CreateAttachment { store_id: StoreId, host_id: NodeId, title: String, init: NodeInit },
    /// The live attachments of a node. Answers with `AttachmentsListed`.
    ListAttachments { store_id: StoreId, node_id: NodeId },
    /// Every discussion of a store. Answers with `DiscussionsListed`.
    ListDiscussions { store_id: StoreId },
    /// Merge the personal document with the signed-in account's other
    /// devices' (docs/DISCUSSIONS_CONTRACT.md section 8); `enabled: false`
    /// deletes the hosted copy. Answers with `PersonalSynced`, or nothing
    /// when nobody is signed in.
    SyncPersonal { state: Vec<u8>, enabled: bool, turn_on: bool },
    GetNode { store_id: StoreId, node_id: NodeId },
    /// The notes notices name, read from this device's documents for the
    /// names they are shown under (`crate::inbox`): answered with their
    /// names alone (`NotesNamed`), never as nodes for the tree.
    NameNodes { nodes: Vec<(StoreId, NodeId)> },
    /// What this device may do in a store changed (a role, a read-only
    /// root), which no document says: the server's judgement of these nodes
    /// (the ones the window shows) is asked again, once, and answered as
    /// `Shown`.
    Rejudge { store_id: StoreId, node_ids: Vec<NodeId> },
    GetChildren { store_id: StoreId, node_id: NodeId },
    /// The children of several nodes of one store at once, each answered as
    /// its own `ChildrenLoaded` as soon as it is in: a store's root and the
    /// rows this device left expanded, asked together when it opens instead
    /// of one level per round trip (docs/STARTUP_CONTRACT.md "Built").
    LoadChildren { store_id: StoreId, nodes: Vec<NodeId> },
    /// Merge `update` (a yrs update of the node's document) through
    /// `applyEdit` and answer `NodeContentUpdated` or `Error`: for a caller
    /// that must know the write landed (the importer, the editor seeding an
    /// empty document). `BroadcastChanges` is the same write, unanswered.
    ApplyUpdate { store_id: StoreId, node_id: NodeId, update: Vec<u8> },
    RenameNode { store_id: StoreId, node_id: NodeId, title: String },
    /// The forest a metanode's source finds (docs/METANODES_CONTRACT.md),
    /// answered as `MetanodeRead`.
    GetMetanode { store_id: StoreId, node_id: NodeId },
    /// Make a node a metanode shown with `viewer`, or with `None` an
    /// ordinary node again; `source` `None` keeps the source it has.
    /// Answers with `Shown`, as an appearance change does.
    SetMetanode { store_id: StoreId, node_id: NodeId, viewer: Option<String>, source: Option<pimble_core::metanode::Source> },
    /// A plugin's call through the Pimble API (docs/PLUGINS_CONTRACT.md wave
    /// 2), already checked against what the person approved for it: made
    /// through this window's own backend, so the server judges it with the
    /// person's role, and its edits attributed to `plugin_id`. Answered as
    /// `PluginReply` naming the instance and the call (a subscription once,
    /// then once per change, until it ends).
    PluginCall {
        instance: u64,
        plugin_id: String,
        plugin_name: String,
        approval: pimble_plugin_host::Approval,
        call_id: u64,
        call: pimble_plugin_host::abi::api::ApiCall,
    },
    /// A plugin instance no longer wants answers to one call (a subscription
    /// it dropped).
    PluginCancel { instance: u64, call_id: u64 },
    /// A plugin instance stopped: everything still in flight for it goes.
    PluginForget { instance: u64 },
    /// Set or clear a node's custom icon (a Tabler icon name) and/or colour
    /// (`#rrggbb`) in its metadata. `None` for a field leaves it as it is;
    /// `Some(None)` clears it. Answers with `Shown`: the node as the edit
    /// left it, for its row.
    SetNodeAppearance {
        store_id: StoreId,
        node_id: NodeId,
        icon: Option<Option<String>>,
        color: Option<Option<String>>,
        /// The row colour (`custom_keys::BACKGROUND`), the same way.
        background: Option<Option<String>>,
        /// A tag added or taken off (the Properties panel,
        /// docs/PROPERTIES_CONTRACT.md section 4), made on the node's tags as
        /// the edit finds them.
        tags: Option<TagEdit>,
        /// The document theme the node names (`custom_keys::THEME`,
        /// docs/THEMES_CONTRACT.md section 3.2), the same way: `Some(None)`
        /// clears it, so the node reads in its parent's.
        theme: Option<Option<String>>,
    },
    DeleteNode { store_id: StoreId, node_id: NodeId },
    /// Split a text node at `at`, a sticky index made at the editor's caret:
    /// the text from there on becomes a new node right below it, titled
    /// `title` when given (docs/SPLIT_CONTRACT.md). Answers `NodeSplit`.
    SplitNode { store_id: StoreId, node_id: NodeId, at: Vec<u8>, title: Option<String> },
    /// Join the text node below this one into it (docs/SPLIT_CONTRACT.md).
    JoinBelow { store_id: StoreId, node_id: NodeId },
    /// Answers `NodeMoved`, or `NodeTransplanted` when the move would have
    /// taken the node out of a share (docs/MOVE_CONTRACT.md).
    MoveNode { store_id: StoreId, node_id: NodeId, new_parent_id: NodeId, place: Place },
    /// Several nodes of one store under one parent, in this order, all or
    /// none (`moveNodes`, docs/TREE_CONTRACT.md). Answers a `NodeMoved` or
    /// `NodeTransplanted` per node.
    MoveNodes { store_id: StoreId, node_ids: Vec<NodeId>, new_parent_id: NodeId, place: Place },
    /// Bring a deleted node and what its deletion took with it back
    /// (`undeleteNode`): "Put Back" in "Recently Deleted...". Answers
    /// `NodeCreated` under the parent it went back to.
    UndeleteNode { store_id: StoreId, node_id: NodeId },
    /// Move a node into another store (docs/MOVE_CONTRACT.md "Between
    /// stores"): always a transplant. Answers `NodeTransplanted`.
    TransplantNode { from_store_id: StoreId, node_id: NodeId, to_store_id: StoreId, new_parent_id: NodeId, place: Place },
    /// What "Recently Deleted..." shows for a store. Answers `DeletedListed`.
    ListDeleted { store_id: StoreId },
    /// Where a Pimble link leads now (`resolveLink`, docs/LINKS_CONTRACT.md
    /// "Following a link"). Answers `LinkResolved`, whose `url` is this one.
    /// Asked of whoever holds `url.store`: the server, or for an encrypted
    /// store in the browser, the vault client. `hops` is how many times the
    /// app has asked again for this one link, so a chain passed between two
    /// holders cannot go round for ever.
    ResolveLink { url: PimbleUrl, hops: usize },
    /// The nodes above `node_id`, so the tree can open down to it (a followed
    /// link's target, docs/LINKS_CONTRACT.md). Answers `AncestorsLoaded`.
    GetAncestors { store_id: StoreId, node_id: NodeId },
    /// `node_id` and the nodes above it, as far as this device may read, for
    /// the theme in force on a node whose chain the app does not hold
    /// (docs/THEMES_CONTRACT.md section 3.2). Answers `ThemeChainLoaded`,
    /// quietly: a node it cannot read ends the chain and says nothing.
    GetThemeChain { store_id: StoreId, node_id: NodeId },
    /// What the Properties panel shows of a node besides its own signal
    /// (docs/PROPERTIES_CONTRACT.md sections 4 and 5): where it lives and the
    /// tags its store uses. Answers `PropertiesLoaded`.
    GetProperties { store_id: StoreId, node_id: NodeId },
    /// What points at a node, for the Properties panel's last sections
    /// (docs/PROPERTIES_CONTRACT.md section 5, wave 3): the Metanodes whose
    /// queries find it, the nodes whose text links to it, the table rows
    /// whose link columns choose it. Answers `ReferencesLoaded`.
    GetReferences { store_id: StoreId, node_id: NodeId },

    /// Parse picked files and write what they hold under `parent_id` (the
    /// store's root when `None`) as new nodes, through the same commands the
    /// app writes with, so an import is an ordinary edit in any store
    /// (docs/IMPORT_CONTRACT.md). Answers `Imported`, or `ImportFailed`.
    Import {
        store_id: StoreId,
        parent_id: Option<NodeId>,
        format: pimble_import::Format,
        /// What was picked: a file name, or a `.scriv` directory's.
        name: String,
        files: pimble_import::Files,
    },

    // Mount operations
    CreateMount {
        store_id: StoreId,
        parent_id: NodeId,
        source_store_id: StoreId,
        source_node_id: NodeId,
        title: Option<String>,
    },
    GetMountState {
        store_id: StoreId,
        node_id: NodeId,
    },

    /// A view of this window changed a node's document (typing, a stroke
    /// on a canvas, a sheet's cell): `update` is the yrs update it made,
    /// sent through `applyEdit`, unanswered (`crate::doc_session::send`).
    BroadcastChanges {
        store_id: StoreId,
        node_id: NodeId,
        update: Vec<u8>,
    },

    /// Reconcile the editor's collaboration session with the server's copy
    /// of a node's content: state vector in, the server's diff and state
    /// vector out (`NodeContentReconciled`). Stateless on both ends; the
    /// same primitive the replica sync link uses between servers.
    ReconcileNodeContent { store_id: StoreId, node_id: NodeId, state_vector: Vec<u8> },

    // Pictures (docs/IMAGES_CONTRACT.md "Showing and inserting images")
    /// Store a picture that is about to be inserted in `node_id`: fitted
    /// under the size limit first (`pimble_image::fit`), then `putBlob`.
    /// Answers `BlobStored` with the same `request_id`, which is how the
    /// pane that asked finds the place the picture was aimed at. Nothing is
    /// written to the node; the insert is an ordinary edit made afterwards.
    PutBlob { store_id: StoreId, node_id: NodeId, bytes: Vec<u8>, request_id: u64 },
    /// `PutBlob` answered in line rather than beside the command loop: for a
    /// caller that writes the picture's URL into a document next, such as the
    /// importer (docs/IMAGES_CONTRACT.md wave 5). Answers `BlobStored` with
    /// `request_id` 0.
    StorePicture { store_id: StoreId, node_id: NodeId, bytes: Vec<u8> },
    /// Fetch the picture a `pimble-blob:` URL names, for showing. Answers
    /// `BlobLoaded`.
    GetBlob { url: BlobUrl },

    /// Hear a store's changes (`Pimble::watch`): every document's update
    /// arrives as `RemoteChanges` for the views holding it and as
    /// `RemoteStoreChange` for the tree, for as long as the store is open,
    /// across lost connections. There is no subscription per node: a view
    /// holding a node hears it through its store's.
    SubscribeStoreChanges { store_id: StoreId },

    // Search
    /// `purpose` says whose results these are, and comes back on
    /// `SearchResults`: the search box's or the link picker's.
    Search { query: String, stores: Vec<StoreId>, limit: usize, purpose: SearchPurpose },
    RebuildIndex { store_id: StoreId },

    // Replica sync (docs/history/HARDENING_CONTRACT.md "B: app") — routed through
    // this server's own `listRemoteStores`/`addRemoteStore` RPCs, never a
    // direct connection to the remote from the app itself (decision 5).
    /// Empty `token` means "use whatever this server already saved for that
    /// remote's origin" (decision 4); non-empty is sent as `AuthMethod::Bearer`.
    ListRemoteStores { url: String, token: String },
    /// Create a local replica of `remote_store_id` from `remote`, linked to
    /// it. No path: the server puts it in its own data directory. Same
    /// `token` semantics as `ListRemoteStores`.
    AddRemoteStore { url: String, remote_store_id: StoreId, token: String },
    /// "Mount Remote Store Here...": one user action, two RPCs
    /// (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 9). Adds `remote_store_id`
    /// from `url` as a replica unless this server already has it open, then
    /// mounts that store's root under `(target_store_id, target_parent_id)`
    /// with the store's name as the mount's title. Same `token` semantics as
    /// `ListRemoteStores`. Emits `StoreOpened` (only when it added the
    /// replica) and then `MountCreated`.
    MountRemoteStore {
        url: String,
        remote_store_id: StoreId,
        token: String,
        target_store_id: StoreId,
        target_parent_id: NodeId,
    },
    /// Link a local store to a remote (`Some`) or unlink it (`None`).
    SetStoreSync { store_id: StoreId, remote: Option<RemoteEndpoint> },
    /// Ask for a store's current sync link and state.
    GetStoreSync { store_id: StoreId },
    /// Stop a replica's sync link, close it, and delete its directory.
    /// `force` removes one whose link is not `Synced` (decision 6).
    RemoveReplica { store_id: StoreId, force: bool },

    /// Make a store on the account this client is signed in as
    /// (docs/CRYPTO_CONTRACT.md). `kind` is `"vault"` or `"plain"`.
    ///
    /// Not a Pimble server's business: the accounts service owns hosted
    /// stores, and for an encrypted one the client also has to mint the key
    /// and seal it to itself, which no server can do for it. The browser
    /// backend answers this itself; the desktop, whose stores are files it
    /// creates directly, refuses it.
    CreateHostedStore { name: String, kind: String },

    // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md, and
    // docs/RELAY_CONTRACT.md for the two relay ones). All of them are
    // `Service`-only RPCs on the server this client talks to; the desktop's
    // embedded server is that principal. Each answers with a `Cloud*` event
    // that names the operation, so an outcome never has to be guessed at.
    // AI agents (docs/AGENTS_CONTRACT.md): `Service`-only RPCs on this
    // device's own server. Each answers `AgentsListed`, `AgentCreated` or
    // `AgentsError`.
    /// This device's agents.
    ListAgents,
    /// A new agent with these grants; `AgentCreated` carries its key, once.
    CreateAgent { name: String, grants: Vec<pimble_rpc::AgentGrant> },
    /// Rename an agent and replace its grants (`AgentsListed`).
    UpdateAgent { id: String, name: String, grants: Vec<pimble_rpc::AgentGrant> },
    /// Remove an agent: its key stops working (`AgentsListed`).
    RemoveAgent { id: String },

    /// Whether an account is signed in, and as whom (`CloudStatusChanged`).
    /// The server's version, for Help > About. Answers with `ServerInfo`.
    ServerInfo,
    /// Help > "Send Diagnostics..." (docs/DIAGNOSTICS_CONTRACT.md section 4):
    /// the logs, the open stores as this window knows them, and the person's
    /// note, to Pimble's developers. Answers with `DiagnosticsSent`.
    SendDiagnostics { note: String, stores: serde_json::Value },
    CloudStatus,
    /// Sign in to the accounts service at `url` (`CloudStatusChanged` with
    /// `signed_in: true`, or `CloudError { op: SignIn }`).
    CloudSignIn { url: String, email: String, password: String },
    /// Forget the signed-in account (`CloudStatusChanged` with `signed_in: false`).
    CloudSignOut,
    /// The notices this device has received, and whether notifications are
    /// on (docs/NOTIFICATIONS_CONTRACT.md section 5). Answers `NoticesListed`.
    ListNotices,
    /// The person opened these notices: acknowledged to the accounts
    /// service. Answers nothing.
    AcknowledgeNotices { ids: Vec<String> },
    /// This window acted on the link the server handed it with `ack`
    /// (`BackendEvent::AppLink`): it came to the front and opened it, and the
    /// launch that handed it over may stop (`appLinkTaken`). Answers nothing.
    AppLinkTaken { ack: u64 },
    /// Notifications on or off for the account (section 6). Answers
    /// `NoticesListed`, or `CloudError { op: Notifications }`.
    SetNotifications { on: bool },
    /// Wave 2's channels for the account, one or both: the email fallback,
    /// web push. Answers `NoticesListed`, or `CloudError { op: Notifications }`.
    SetNotificationChannels { email: Option<bool>, push: Option<bool> },
    /// Wave 3: watched notes on or off for the account; off deletes every
    /// watch. Answers `NoticesListed` and then `WatchesListed`, or
    /// `CloudError { op: Notifications }`.
    SetWatchesOn { on: bool },
    /// Watch a node, alone or with its subtree (docs/NOTIFICATIONS_CONTRACT.md
    /// section 3). Answers `WatchesListed`, or `CloudError { op: Watches }`
    /// with the sentence that refused it.
    WatchNode { store_id: StoreId, node_id: NodeId, subtree: bool },
    /// Stop watching a node: its own watch, or else the subtree watch that
    /// reaches it from nearest above. Answers `WatchesListed`, or
    /// `CloudError { op: Watches }`.
    UnwatchNode { store_id: StoreId, node_id: NodeId },
    /// The watches the account chose. Answers `WatchesListed`.
    ListWatches,
    /// This browser subscribed to web push (wave 2): handed to the server,
    /// which gives it to the accounts service and takes it back at sign-out.
    /// Answers nothing.
    RegisterPush { endpoint: String, p256dh: String, auth: String },
    /// This browser's push subscription ended. Answers nothing.
    UnregisterPush { endpoint: String },
    /// Host a local store's encrypted twin on Pimble Cloud (`CloudStoreHosted`).
    CloudHostStore { store_id: StoreId },
    /// Every store the signed-in account has a grant on (`CloudHostedStoresListed`).
    CloudListHostedStores,
    /// Add an already-hosted store as a local replica; the store arrives as
    /// `StoreOpened`, exactly as `AddRemoteStore`'s does.
    CloudAddHostedStore { store_id: StoreId },
    /// The shares the account takes part in and the names it goes by in
    /// each (docs/PRESENCE_CONTRACT.md section 3; `CloudSharesListed`).
    CloudListShares,
    /// The account's default name, then the list again (`CloudSharesListed`).
    CloudSetDisplayName { name: String },
    /// The name the account goes by in one share, then the list again
    /// (`CloudSharesListed`). Empty means the default name.
    CloudSetShareName { store_id: StoreId, root: Option<NodeId>, name: String },
    /// Who is who in a store's shares (`PeopleLoaded`).
    GetPeople { store_id: StoreId },
    /// Say where pane `pane` is (docs/PRESENCE_CONTRACT.md): it holds
    /// `node_id`, or, `left`, lets it go; `text` is where it is in a note's
    /// text, `board` where it is on a board. The backend makes the frame.
    PublishPresence {
        store_id: StoreId,
        node_id: NodeId,
        /// The session within this client: a pane's index, or what a
        /// plugin in that pane is on (`presence::within_session`).
        session: String,
        focused: bool,
        left: bool,
        text: Option<pimble_rpc::TextPresence>,
        board: Option<pimble_rpc::BoardPresence>,
    },
    /// Hear who else is in the store's documents (`PresenceFrame`s, the ones
    /// already there first).
    SubscribePresence { store_id: StoreId },
    /// Share a local store from this computer (docs/RELAY_CONTRACT.md):
    /// nothing of it is uploaded, and the people it is shared with reach it
    /// through Pimble Cloud's relay while this computer is on and online
    /// (`CloudStoreRelayed`). Sent only because the person chose it.
    CloudRelayStore { store_id: StoreId },
    /// The way back from `CloudRelayStore` (`CloudRelayingStopped`). The
    /// server refuses it while the store still has shares.
    CloudStopRelaying { store_id: StoreId },

    // Sharing (docs/NODE_DOCUMENT_CONTRACT.md section 5). `Service`-only like
    // the six above. The first four answer `CloudShareUpdated`, the last
    // `CloudSharingStopped`, and any of them `CloudError` with its own `CloudOp`.
    /// Share a node under a display name Pimble Cloud and invitees will see.
    CloudShareNode { store_id: StoreId, node_id: NodeId, name: String },
    /// A shared node's share and members, as the accounts service has them now.
    CloudShareInfo { store_id: StoreId, node_id: NodeId },
    /// Invite an address (`Editor` or `Reader`), or change the role it has.
    CloudShareInvite { store_id: StoreId, node_id: NodeId, email: String, role: pimble_rpc::MemberRole },
    /// Remove a member or a pending invitation, by address.
    CloudShareRemoveMember { store_id: StoreId, node_id: NodeId, email: String },
    /// Stop sharing: the grants and the scope go; the documents stay where they are.
    CloudStopSharing { store_id: StoreId, node_id: NodeId },
}

/// What a new node starts with (`BackendCommand::CreateNode`'s `init`): its
/// type, a yrs update merged into its document, and `metadata.custom` keys,
/// all in the one edit that creates it. A board node's board is written this
/// way, never after (docs/IMAGE_MARKUP_CONTRACT.md section 1).
#[derive(Debug, Clone, PartialEq)]
pub struct NodeInit {
    pub node_type: String,
    pub content: Vec<u8>,
    pub custom: Vec<(String, serde_json::Value)>,
    /// Handed back on `NodeCreated`, for a caller that must know which node
    /// it made ("Convert to Node" writes the new node into the text).
    pub request: Option<u64>,
}

impl NodeInit {
    /// A recipe's node (`pimble_api::recipes`, the one place a new node of
    /// each kind is described) as the command carries it: its title goes on
    /// the command, `request` comes back on `NodeCreated`.
    pub fn of(recipe: pimble_api::NodeInit, request: Option<u64>) -> (String, NodeInit) {
        let init = NodeInit { node_type: recipe.node_type, content: recipe.content.unwrap_or_default(), custom: recipe.custom, request };
        (recipe.title, init)
    }
}
