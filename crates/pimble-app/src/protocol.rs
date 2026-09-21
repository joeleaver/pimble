//! The seam between the UI and whatever is behind it.
//!
//! The UI sends a [`BackendCommand`] and consumes a [`BackendEvent`], and knows
//! nothing else about how either travels. Two implementations fill that in:
//! the desktop's `backend` module (a background thread with a tokio runtime and
//! an embedded `PimbleServer`, behind the `native` feature) and the web app's
//! command loop on `wasm_bindgen_futures::spawn_local` against a hosted server.
//!
//! [`BackendHandle`] is the pair of channels itself, so both implementations
//! hand the UI the same thing. Only `BackendHandle::spawn` is native; `send`
//! and `try_recv` never block and work on every target.

use crossbeam_channel::{Receiver, Sender};
use pimble_core::{MountRef, MountState, Node, NodeId, RemoteEndpoint, Store, StoreId, SyncState};

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
    CreateNode { store_id: StoreId, parent_id: Option<NodeId>, title: String },
    GetNode { store_id: StoreId, node_id: NodeId },
    GetChildren { store_id: StoreId, node_id: NodeId },
    SetNodeContent { store_id: StoreId, node_id: NodeId, content: Vec<u8> },
    RenameNode { store_id: StoreId, node_id: NodeId, title: String },
    /// Set or clear a node's custom icon (a Tabler icon name) and/or colour
    /// (`#rrggbb`) in its metadata. `None` for a field leaves it as it is;
    /// `Some(None)` clears it. Answers with `NodeRenamed`, whose refetch
    /// carries the new metadata to the tree.
    SetNodeAppearance {
        store_id: StoreId,
        node_id: NodeId,
        icon: Option<Option<String>>,
        color: Option<Option<String>>,
        /// `Some(tags)` replaces the node's tags.
        tags: Option<Vec<String>>,
    },
    DeleteNode { store_id: StoreId, node_id: NodeId },
    MoveNode { store_id: StoreId, node_id: NodeId, new_parent_id: NodeId, position: Option<usize> },

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

    // Collaborative editing — broadcast incremental changes to server
    BroadcastChanges {
        store_id: StoreId,
        node_id: NodeId,
        /// Base64-encoded incremental yrs change bytes
        changes: String,
    },

    /// Reconcile the editor's collaboration session with the server's copy
    /// of a node's content: state vector in, the server's diff and state
    /// vector out (`NodeContentReconciled`). Stateless on both ends; the
    /// same primitive the replica sync link uses between servers.
    ReconcileNodeContent { store_id: StoreId, node_id: NodeId, state_vector: Vec<u8> },

    // Subscription operations
    SubscribeStoreChanges { store_id: StoreId },
    SubscribeNodeChanges { store_id: StoreId, node_id: NodeId },

    /// Internal: the connection watchdog saw the WebSocket close. Carries the
    /// connection generation it watched so a stale watchdog (from before a
    /// reconnect) is ignored.
    ConnectionLost { generation: u64 },

    // Search
    Search { query: String, stores: Vec<StoreId>, limit: usize },
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
    /// Whether an account is signed in, and as whom (`CloudStatusChanged`).
    CloudStatus,
    /// Sign in to the accounts service at `url` (`CloudStatusChanged` with
    /// `signed_in: true`, or `CloudError { op: SignIn }`).
    CloudSignIn { url: String, email: String, password: String },
    /// Forget the signed-in account (`CloudStatusChanged` with `signed_in: false`).
    CloudSignOut,
    /// Host a local store's encrypted twin on Pimble Cloud (`CloudStoreHosted`).
    CloudHostStore { store_id: StoreId },
    /// Every store the signed-in account has a grant on (`CloudHostedStoresListed`).
    CloudListHostedStores,
    /// Add an already-hosted store as a local replica; the store arrives as
    /// `StoreOpened`, exactly as `AddRemoteStore`'s does.
    CloudAddHostedStore { store_id: StoreId },
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

/// Which cloud request an outcome belongs to (docs/DESKTOP_ACCOUNT_CONTRACT.md
/// decision 3): every cloud event names its operation, so the event handler
/// routes an error to the modal that asked without a pending flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudOp {
    Status,
    SignIn,
    SignOut,
    HostStore,
    ListHostedStores,
    AddHostedStore,
    RelayStore,
    StopRelaying,
    Share,
    ShareInfo,
    ShareInvite,
    ShareRemoveMember,
    StopSharing,
}

/// Events sent from backend to UI
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Connected { server_addr: String, client_id: String },
    Disconnected,
    Error { message: String },

    // Store events
    StoreCreated { store_id: StoreId, root_node_id: NodeId },
    StoreOpened { store: Store },
    StoreClosed { store_id: StoreId },
    /// Answer to `ListStores`: every store the server currently has open.
    StoresListed { stores: Vec<Store> },

    // Node events
    NodeCreated { store_id: StoreId, parent_id: Option<NodeId>, node_id: NodeId },
    NodeLoaded { store_id: StoreId, node: Node },
    /// `children` live in `children_store_id`: the same as `store_id` for an
    /// ordinary parent, the mount's source store when `parent_id` is a mount point.
    ChildrenLoaded { store_id: StoreId, parent_id: NodeId, children_store_id: StoreId, children: Vec<Node> },
    NodeContentUpdated { store_id: StoreId, node_id: NodeId },
    NodeRenamed { store_id: StoreId, node_id: NodeId },
    NodeDeleted { store_id: StoreId, node_id: NodeId, parent_id: NodeId },
    NodeMoved { store_id: StoreId, node_id: NodeId, old_parent_id: NodeId, new_parent_id: NodeId },

    // Mount events
    MountCreated {
        store_id: StoreId,
        /// The node the mount was created under, so the tree can load that
        /// parent's children and show the new mount even when the parent had
        /// never been expanded (`RemoteStoreChange`'s `NodeCreated` refetch
        /// deliberately skips a parent whose children are not loaded).
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
    /// Remote incremental changes arrived — apply to the local editor's collab
    /// session. No node identity carried: pimble has one shared editor pane and
    /// the subscription that produces this is already scoped to that node.
    RemoteChanges { changes: String },

    // Search
    /// The outcome of a `Search` command. Carries `Err` rather than folding
    /// into the generic `Error` event so a failed search (including "the
    /// index is still building") shows inline in the results panel without
    /// touching the connection status bar or the reconnect-on-error path.
    SearchResults { results: Result<Vec<pimble_rpc::SearchResultItem>, String> },
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
    /// judgement of itself (`Node::access`). `relay` is which end of Pimble
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
        relay: pimble_core::RelaySide,
        owner_offline: bool,
    },
    /// Answer to `RemoveReplica`: the replica is gone. Handled exactly like
    /// `StoreClosed` (tree + saved open-store list cleanup).
    ReplicaRemoved { store_id: StoreId },

    /// A `CreateHostedStore` succeeded. Carries only the name, because the
    /// store itself arrives the ordinary way: the backend mints a token that
    /// carries the new grant and reconnects, and `StoresListed` brings it in.
    HostedStoreCreated { name: String },

    // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md).
    /// The answer to `CloudStatus`, `CloudSignIn` (`signed_in: true`) and
    /// `CloudSignOut` (`signed_in: false`).
    CloudStatusChanged { signed_in: bool, email: Option<String>, url: Option<String> },
    /// A cloud request failed; `op` says which, so the error lands in the
    /// modal that sent it.
    CloudError { op: CloudOp, message: String },
    /// The answer to `CloudListHostedStores`: every store the account has a
    /// grant on, unfiltered (the event handler keeps the ones worth adding).
    /// `relayed` names the rows that are not hosted at all: served from
    /// their owner's computer through Pimble Cloud's relay
    /// (`CloudListHostedStoresResponse::relayed`).
    CloudHostedStoresListed { stores: Vec<pimble_rpc::CloudHostedStoreInfo>, relayed: Vec<String> },
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

/// Handle to communicate with the backend
#[derive(Clone)]
pub struct BackendHandle {
    pub cmd_tx: Sender<BackendCommand>,
    pub event_rx: Receiver<BackendEvent>,
}

impl BackendHandle {
    /// Send a command to the backend (non-blocking), ignoring result
    pub fn send(&self, cmd: BackendCommand) {
        if let Err(e) = self.cmd_tx.try_send(cmd) {
            tracing::error!("Backend channel send failed: {}", e);
        }
    }

    /// Try to receive an event (non-blocking)
    pub fn try_recv(&self) -> Option<BackendEvent> {
        self.event_rx.try_recv().ok()
    }
}
