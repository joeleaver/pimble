# Pimble - Personal Information Manager Architecture

> **On branch `node-document` (2026-09-21) parts of this document describe the design it
> replaces.** A store no longer has a store document: every node is one yrs document
> (`pimble_crdt::NodeDoc`: content, fields, children, plugin data) and the tree is the graph
> of those documents (`pimble_crdt::Tree`). Wherever this file says `StoreDocument`,
> `ContentDoc`, `store.yrs`, `applyStoreUpdate`, `syncStoreDocument` or `syncNodeContents`,
> read `docs/NODE_DOCUMENT_CONTRACT.md` (sections 1 to 4) and the "Current design" section
> of `CLAUDE.md` instead: `applyEdit` carries an update of any part of a node's document,
> `syncNodes` is the one reconcile, repair runs over the documents. Mounts, replica links,
> hardening, search and the cloud sections stand as written. Rewriting the affected
> sections here is an open follow-up (`docs/NEXT_SESSION.md`).

## Overview

Pimble is an offline-first personal information manager with:
- CRDT-based data model (**yrs**) for both node content and the store's tree structure,
  so devices and people can edit concurrently and merge without conflicts
- Rust backend with Rinch UI framework
- JSON-RPC communication between components (embedded server)
- WASM plugin system for extensible node types
- Embedded vector database for semantic search (future)
- **Network-transparent mount points** — any node subtree from any store (local or remote) can appear as a first-class child in any other store's tree

---

## Project Structure (Cargo Workspace)

```
pimble/
├── Cargo.toml                 # Workspace root
├── crates/
│   ├── pimble-core/           # Core types, traits, node definitions
│   ├── pimble-crdt/           # yrs-backed CRDT documents (node content, store document)
│   ├── pimble-store/          # Store abstraction (local + remote)
│   ├── pimble-search/         # Vector DB, indexing, semantic search
│   ├── pimble-rpc/            # JSON-RPC protocol definitions
│   ├── pimble-server/         # Local/remote server implementation
│   ├── pimble-client/         # Client library for connecting to servers
│   ├── pimble-plugins/        # WASM plugin host + built-in plugins
│   ├── pimble-app/            # Rinch desktop application
│   └── pimble-cli/            # CLI tool for debugging and admin
├── plugins/                   # Example/default WASM plugins
│   └── document-node/         # Markdown document node type
└── docs/                      # Architecture documentation
```

---

## Core Architecture

### 1. Node System (`pimble-core`)

Every piece of content is a **Node**. Nodes form trees within Stores.

```rust
pub struct NodeId(pub Uuid);

pub struct Node {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub node_type: String,           // e.g., "document", "folder", "mount"
    pub metadata: NodeMetadata,
    pub content: Vec<u8>,            // CRDT document bytes (a yrs snapshot)
    pub children: Vec<NodeId>,       // Ordered child references
    pub links: Vec<NodeLink>,        // Outgoing links to other nodes
}

pub struct NodeMetadata {
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub custom: HashMap<String, Value>,
}

pub struct NodeLink {
    pub target: LinkTarget,
    pub link_type: String,           // "reference", "embed", etc.
    pub source_anchor: Option<String>,
}

pub enum LinkTarget {
    Node(NodeId),
    Deep { node_id: NodeId, anchor: String },
    External(Url),
}
```

Well-known node types: `"document"`, `"folder"`, `"store"`, `"image"`, `"canvas"`.

`parent_id`, `children`, and the metadata fields live in the store's tree document (see
§4); `content` is assembled on read from the node's own content document. A node with no
content document (a folder, say) reports empty `content`.

### 2. Store System (`pimble-store`)

A **Store** is a self-contained tree of nodes with its own identity and storage.

```rust
pub struct StoreId(pub Uuid);

pub struct Store {
    pub id: StoreId,
    pub name: String,
    pub location: StoreLocation,
    pub root_node_id: NodeId,
    pub sync_state: SyncState,
}

pub enum StoreLocation {
    Local { path: PathBuf },
    Remote { url: Url, auth: AuthMethod },
    Mounted { store_id: StoreId, node_id: NodeId },
}

pub enum SyncState {
    Offline,
    Syncing,
    Synced { last_sync: DateTime<Utc> },
    Conflict { details: Vec<ConflictInfo> },
}
```

`SyncState` reflects a store's replica sync link (see "Replica sync" below): `Offline` for a
store with no link. `Conflict` is modeled but unused — a yrs merge never produces one.

The `StoreManager` currently handles only `LocalStore` instances. It will be extended to support remote and mounted stores via the Mount Resolver (see below).

### 3. Workspace System (`pimble-core`)

A **Workspace** is what the user opens. It defines which stores are visible.

```rust
pub struct Workspace {
    pub id: Uuid,
    pub name: String,
    pub stores: Vec<WorkspaceStore>,
    pub ui_state: WorkspaceUiState,
}

pub struct WorkspaceStore {
    pub store: Store,
    pub display_name: Option<String>,
    pub position: usize,
    pub expanded_nodes: HashSet<NodeId>,
}
```

Workspace files: `.pimble-workspace` (JSON)

### 4. CRDT Layer (`pimble-crdt`)

There is one CRDT technology in Pimble, **yrs**, used for two kinds of document. Both are
a `yrs::Doc` built with `OffsetKind::Utf16` (matching Yjs/rinch semantics) and both expose
the same small set of primitives: load a document from bytes, save a full snapshot, apply
an incoming update, and compute a state vector / diff pair for stateless sync.

**`ContentDoc`** — one per node, the node's rich-text content:

```rust
pub struct ContentDoc { /* yrs::Doc */ }

impl ContentDoc {
    pub fn new() -> Self;
    pub fn load(bytes: &[u8]) -> Result<Self>;
    pub fn from_plain_text(text: &str) -> Result<Self>;
    pub fn save(&self) -> Vec<u8>;
    pub fn apply_update(&mut self, update: &[u8]) -> Result<bool>; // false: nothing new
    pub fn state_vector(&self) -> Vec<u8>;
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>>;
    pub fn diff_if_peer_lacks_it(&self, peer_sv: &[u8], peer_diff: &[u8]) -> Result<Option<Vec<u8>>>;
    pub fn text(&self) -> String; // flat-text projection for tree labels/search
}
```

Internally a `ContentDoc` is a `rinch-editor-collab` `CollabSession` wrapping a
`rinch-editor-core` document (paragraphs, headings, code blocks; bold/italic/link marks).
That internal shape is opaque outside `from_plain_text` and `text`; every other caller
(the server included) treats a `ContentDoc` as opaque yrs bytes and updates.

**`StoreDocument`** — one per store, the tree structure and node metadata, built directly
on `yrs::Map`/`yrs::Array` (no editor schema involved):

```rust
pub struct StoreDocument { /* yrs::Doc */ }

impl StoreDocument {
    pub fn new(name: &str, root_node_id: NodeId) -> Result<Self>;
    pub fn load(bytes: &[u8]) -> Result<Self>;
    pub fn save(&self) -> Vec<u8>;
    pub fn apply_update(&mut self, update: &[u8]) -> Result<StoreUpdateEffect>; // changed + touched ids
    pub fn state_vector(&self) -> Vec<u8>;
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>>;
    pub fn diff_if_peer_lacks_it(&self, peer_sv: &[u8], peer_diff: &[u8]) -> Result<Option<Vec<u8>>>;

    // Tree and metadata mutation/read
    pub fn add_node(&mut self, id: NodeId, parent_id: Option<NodeId>, node_type: &str, title: &str) -> Result<()>;
    pub fn remove_node(&mut self, id: NodeId) -> Result<()>;
    pub fn move_node(&mut self, id: NodeId, new_parent: NodeId, position: Option<usize>) -> Result<()>;
    pub fn set_title(&mut self, id: NodeId, title: &str) -> Result<()>;
    pub fn set_tags(&mut self, id: NodeId, tags: &[String]) -> Result<()>;
    pub fn set_custom(&mut self, id: NodeId, key: &str, value: &serde_json::Value) -> Result<()>;
    pub fn get_node_info(&self, id: NodeId) -> Result<NodeInfo>;
    pub fn get_children(&self, id: NodeId) -> Result<Vec<NodeId>>;
    pub fn validate_tree(&self) -> Result<Vec<TreeIssue>>;
    pub fn repair(&mut self) -> Result<Option<TreeRepair>>; // exactly what validate_tree reports
}
```

Schema:

```text
root map "meta":  { name: String, root_node_id: String }
root map "nodes": { <node-id>: Map {
    parent_id: String | absent,
    node_type: String, title: String,
    created_at: String (rfc3339), modified_at: String (rfc3339),
    tags: Array<String>,
    custom: Map<String, String (json)>,
    children: Array<String (node-id)>
} }
```

**Persistence.** A store keeps one `StoreDocument` (`store.yrs`) and one `ContentDoc` per
node that has content (`nodes/{node-id}.yrs`), loaded lazily and kept in memory while the
node is open. Because each node's content is its own document, nodes can be synced or
replicated individually — this is what makes mounting a subtree cheap (see the Mount
Architecture section below).

**Sync.** Both document kinds use the same stateless protocol: a client sends the state
vector it already has, the server (or peer) replies with a diff of everything beyond it
plus its own state vector, and neither side keeps per-client sync state between calls. See
§6 for the RPC methods that carry this (`syncNodeContent`, `syncStoreDocument`) and the
ones that carry live edits (`applyEdit`, `applyStoreUpdate`).

### 5. Search & Indexing (`pimble-search`)

Each store maintains its own index locally. Cross-store search aggregates results.

```rust
pub struct SearchQuery {
    pub query: String,
    pub stores: Vec<StoreId>,
    pub semantic: bool,
    pub filters: SearchFilters,
    pub limit: usize,
}

pub struct SearchResult {
    pub node_id: NodeId,
    pub store_id: StoreId,
    pub score: f32,
    pub title: String,
    pub snippet: String,
    pub deep_link: Option<String>,
}
```

**Embedding model** (Phase 4): Local-only using `all-MiniLM-L6-v2` (384 dimensions, ~80MB model). See `docs/RESTART_PLAN.md` §5 for the current plan (rhypedb as the index and query engine) superseding the candle/lance sketch this section originally described.

### 6. RPC Protocol (`pimble-rpc`)

JSON-RPC 2.0 via `jsonrpsee`. The app currently uses an embedded server (in-process), but
the protocol supports HTTP/WebSocket for remote servers. This mirrors
`crates/pimble-rpc/src/methods.rs`:

```rust
#[rpc(server, client, namespace = "pimble")]
pub trait PimbleApi {
    // Store Operations
    async fn create_store(&self, request: CreateStoreRequest) -> Result<CreateStoreResponse, ErrorObjectOwned>;
    async fn open_store(&self, request: OpenStoreRequest) -> Result<OpenStoreResponse, ErrorObjectOwned>;
    async fn close_store(&self, request: CloseStoreRequest) -> Result<EmptyResponse, ErrorObjectOwned>;
    async fn list_stores(&self) -> Result<ListStoresResponse, ErrorObjectOwned>;

    // Node Operations
    async fn get_node(&self, request: GetNodeRequest) -> Result<GetNodeResponse, ErrorObjectOwned>;
    async fn get_nodes(&self, request: GetNodesRequest) -> Result<GetNodesResponse, ErrorObjectOwned>;
    async fn create_node(&self, request: CreateNodeRequest) -> Result<CreateNodeResponse, ErrorObjectOwned>;
    async fn update_node_metadata(&self, request: UpdateNodeMetadataRequest) -> Result<EmptyResponse, ErrorObjectOwned>;
    async fn update_node_content(&self, request: UpdateNodeContentRequest) -> Result<EmptyResponse, ErrorObjectOwned>;
    async fn delete_node(&self, request: DeleteNodeRequest) -> Result<EmptyResponse, ErrorObjectOwned>;
    async fn move_node(&self, request: MoveNodeRequest) -> Result<EmptyResponse, ErrorObjectOwned>;
    async fn get_children(&self, request: GetChildrenRequest) -> Result<GetChildrenResponse, ErrorObjectOwned>;

    // Mount Operations
    async fn create_mount(&self, request: CreateMountRequest) -> Result<CreateMountResponse, ErrorObjectOwned>;
    async fn get_mount_state(&self, request: GetMountStateRequest) -> Result<GetMountStateResponse, ErrorObjectOwned>;

    // Workspace Operations
    async fn load_workspace(&self, request: LoadWorkspaceRequest) -> Result<LoadWorkspaceResponse, ErrorObjectOwned>;
    async fn save_workspace(&self, request: SaveWorkspaceRequest) -> Result<EmptyResponse, ErrorObjectOwned>;
    async fn create_workspace(&self, request: CreateWorkspaceRequest) -> Result<LoadWorkspaceResponse, ErrorObjectOwned>;

    // Edit Operations (collaborative editing)
    /// Apply an edit operation to a node's content and broadcast it to other clients.
    async fn apply_edit(&self, request: ApplyEditRequest) -> Result<ApplyEditResponse, ErrorObjectOwned>;

    // Sync Operations
    /// Sync a store document (tree structure + metadata): send a yrs state
    /// vector, receive a diff of everything the server has beyond it.
    /// Stateless. If the client also has local changes the server lacks,
    /// it sends those separately via `applyStoreUpdate`.
    async fn sync_store_document(&self, request: SyncStoreDocumentRequest) -> Result<SyncStoreDocumentResponse, ErrorObjectOwned>;
    /// Sync a node's content document the same way: state vector in, diff out.
    async fn sync_node_content(&self, request: SyncNodeContentRequest) -> Result<SyncNodeContentResponse, ErrorObjectOwned>;
    /// Apply a yrs update to the store document and broadcast it to the
    /// store's other subscribers.
    async fn apply_store_update(&self, request: ApplyStoreUpdateRequest) -> Result<EmptyResponse, ErrorObjectOwned>;

    // Subscription Operations
    #[subscription(name = "subscribeStoreChanges" => "storeChanged", unsubscribe = "unsubscribeStoreChanges", item = StoreChangedNotification)]
    async fn subscribe_store_changes(&self, store_id: StoreId) -> SubscriptionResult;
    #[subscription(name = "subscribeNodeChanges" => "nodeChanged", unsubscribe = "unsubscribeNodeChanges", item = NodeContentChangedNotification)]
    async fn subscribe_node_changes(&self, store_id: StoreId, node_id: NodeId) -> SubscriptionResult;

    // Search Operations
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse, ErrorObjectOwned>;
}
```

`EditOperation` (carried by `apply_edit` and relayed in `NodeContentChangedNotification`)
has one active variant, `IncrementalChanges { changes: String }` — a base64-encoded yrs v1
update (a delta or a reconciliation diff). Whole-document replacement goes through
`updateNodeContent`, which seeds or replaces a node's content with a full snapshot.

**Server-side relay.** `apply_edit` merges the incoming update into the server's
in-memory `ContentDoc` for that node, schedules a debounced flush to `nodes/{id}.yrs`
(750ms, coalescing a burst of edits into one write), and relays the same bytes verbatim to
the node's other subscribers — never re-encoded or reinterpreted. `apply_store_update`
does the equivalent for the store document: merge into the in-memory `StoreDocument`,
flush `store.yrs` immediately, and broadcast to the store's subscribers as a
`StoreChangedNotification` carrying the raw update.

### 7. Plugin System (`pimble-plugins`)

WASM plugins define new node types.

```rust
pub trait NodePlugin: Send + Sync {
    fn info(&self) -> PluginInfo;
    fn node_type(&self) -> &str;
    fn schema(&self) -> NodeSchema;
    fn render(&self, content: &[u8]) -> Result<RenderOutput>;
    fn extract_text(&self, content: &[u8]) -> Result<String>;
    fn validate(&self, content: &[u8]) -> Result<ValidationResult>;
    fn init_content(&self) -> Result<Vec<u8>>;
}
```

Built-in plugins: `DocumentPlugin` (markdown), `FolderPlugin` (container).

---

## Mount Architecture

### The Core Idea

Any node in any store can serve as a **mount point** — a leaf in the local tree that, when expanded, reveals a subtree from another store. The source store can be local or remote. Mounts are network-transparent: the UI treats mounted subtrees as first-class citizens, indistinguishable from local nodes.

This is analogous to filesystem mount points: you can mount any block device at any point in the VFS tree, and user-space programs don't know the difference.

### Mount Points

A mount point is a node with `node_type: "mount"` whose metadata contains a `MountRef`:

```rust
/// Reference to a subtree in another store
pub struct MountRef {
    /// Which store contains the source subtree
    pub source_store: StoreId,
    /// Which node to use as the root of the mounted subtree
    /// (and all its children come along)
    pub source_node: NodeId,
    /// A filesystem-path hint for reopening the source store after a
    /// restart, when it isn't otherwise in the store registry. `createMount`
    /// fills it from the registry's `Local` endpoint for the source store at
    /// creation time; `None` for a source that was never local or never
    /// open. Optional on the wire so an older-shaped `MountRef` still
    /// deserializes.
    pub source_path: Option<PathBuf>,
    /// The Pimble server the source store can be replicated from when it
    /// isn't on this machine. A URL only, never a credential: a mount ref
    /// replicates with its store and lands on machines that must not hold
    /// the token. `createMount` fills it when the source store is itself a
    /// linked replica on the creating server; `None` otherwise.
    pub source_remote: Option<Url>,
}
```

The mount point node is a **placeholder**: `getNode` on it returns the mount node itself (empty `children`, empty `content`); it has no children or content of its own. `getChildren` on it returns the *source* node's children instead — see `GetChildrenResponse.store_id` below. Rename, delete, and move addressed at the mount node act on the mount node in the mounting store only, never on the source; `createNode` with a mount node as parent is an error (create under `mount_ref` instead).

### Mount Resolver

The **Mount Resolver** is the layer that takes a `MountRef` and returns a live handle to the source subtree's data. It is the key new component in the architecture.

```
UI requests node expansion
        │
        ▼
  StoreManager.get_children(store_id, node_id)
        │
        ├─ node_type == "mount" ?
        │       │
        │       ▼
        │   RpcHandler.resolve_mount(mounting store, mount node, mount_ref)
        │       │
        │       ├─ StoreManager.ensure_store_open(mount_ref)
        │       │       ├─ source store already open? → use it
        │       │       ├─ registry has a Local endpoint? → open it (Remote: unavailable)
        │       │       └─ mount_ref.source_path hint set? → open it, registering on success
        │       │
        │       ├─ mount_ref.source_remote set? ──┐  replicate the source from the
        │       ├─ the mounting store's remote? ──┤  first of these that has it, in a
        │       │   (its own sync.json)           │  background task; answer Connecting
        │       │                                 │  now
        │       └─ nothing to try → Unavailable { reason }
        │
        └─ regular node → return children from local store
```

This is a single hop, not a recursive resolve: `getChildren` on a mount returns exactly the source node's children, addressed by the source store's id (`GetChildrenResponse.store_id`). A nested mount among those children is itself an ordinary node in the (now open) source store, resolved the same way when the caller expands it in turn — so cycles are caught once, at `createMount` time (`validate_mount_creation`), not on every expansion.

**A source that isn't on this machine becomes a replica, and then resolves locally.** The two remote steps are the handler's, not `StoreManager`'s: each candidate is asked for the store (`create_replica_from`, the same function `addRemoteStore` uses, so the "already open locally" guard and the `StoreDocument::new` prohibition both apply), and the first that has it wins. Creation runs in a detached task and the triggering RPC answers `Connecting` at once; a per-source in-flight set, checked under the same lock that records the mount, means two resolutions of one source start one task. The replica lands in the server's replicas directory (`ServerConfig::replicas_dir`, default `<data dir>/pimble/replicas/<store id>.pimble`), so it is `is_replica` and removable with `removeReplica` like any other. Nothing retries on a timer: the next resolution of the mount tries again.

**An implicitly opened source is an ordinary open store.** When resolving or validating a mount opens a store that wasn't already open, it gets the same treatment `openStore` gives one: a search index under its own `index/rhypedb/`, a place in `listStores`, and subscriptions work on it. `StoreManager::opened_since()` drains the list of stores opened since the last call, so the RPC handler can open a search index for each one after any call that might have opened stores implicitly (`getChildren`, `getMountState`, `createMount`).

The resolver maintains a **store registry** — a mapping from `StoreId` to connection information:

```rust
/// How to reach a store
pub enum StoreEndpoint {
    /// Local filesystem path
    Local { path: PathBuf },
    /// Remote server (defined; a mount never resolves through this — a
    /// source that isn't local becomes a replica and then resolves as a
    /// `Local` one)
    Remote { url: Url, auth: AuthMethod },
}

/// Registry of known stores and how to reach them
pub struct StoreRegistry {
    /// Known store endpoints
    endpoints: HashMap<StoreId, StoreEndpoint>,
}
```

An already-open store needs no registry lookup at all — `local_stores` is checked first. The registry is populated by every `create_local_store`/`open_local_store` call (including ones a mount triggers), so it has an entry for any store this process has opened this session. `MountRef.source_path` exists for the case the registry doesn't cover: a fresh process, before anything has reopened the source directly.

### Mount State & Degradation (`docs/history/REMOTE_MOUNTS_CONTRACT.md`)

```rust
pub enum MountState {
    /// The source is open here and its data is current.
    Live,
    /// The source's replica is here and readable, but its link is down or
    /// still reconciling.
    Cached { last_sync: DateTime<Utc> },
    /// Nothing can reach the source. `reason` says why, in words a user
    /// can act on ("http://host:7463 refused the credentials").
    Unavailable { reason: Option<String> },
    /// The source's replica is being created, or reconciling for the first
    /// time.
    Connecting,
}
```

- **Mount state is derived from the source's link**, never stored. For a source open on
  this server: no link, or a link that is `Synced`, is `Live`; a link that is `Offline` or
  `Syncing` is `Cached { last_sync }` once it has ever synced, and `Connecting` until then.
  For a source that isn't open: a replica creation in flight is `Connecting`; nothing to
  try, or a failed last attempt, is `Unavailable { reason }`.
- **`last_sync` is persisted.** `<store>/sync.json` gains `last_sync`, which the link seeds
  itself from at start and rewrites only on a transition into `Synced` (with now) or out of
  it (with the last value), always keeping `auth: none`. That is what makes a mount
  `Cached` rather than `Connecting` after a restart with the remote down.
- **The server tells clients when a mount's state changes; nobody polls.** It remembers
  which mounts it resolved per source store (added to by `getMountState`, `getChildren` on
  a mount, and `createMount`; a mounting store's entries go when it closes). When that
  source's link changes category, or a background replica creation ends either way, every
  mounting store gets a `StoreChangeKind::MountStateChanged { node_id, state }` with
  `source_client_id: None`. Sync links ignore `MountStateChanged` in both directions, like
  `SyncStateChanged`: it is derived state and every server computes its own.
- **`getChildren` on a mount whose source isn't open is an error** carrying the state
  ("mount source is connecting", "mount source unavailable: <reason>"). The client shows
  it and refetches when a `MountStateChanged` says the source is back. A `Cached` mount's
  source *is* open, so its children and documents keep answering from the replica.
- **Out of scope**: replicating only the mounted subtree (the whole source store is
  replicated), TLS, and any retry loop for a failed creation.

### Data Ownership & Sync

- **Data lives in the source store.** The mount point's store does not copy or own the mounted data.
- **Local caching via CRDT replication.** A remote mount's source becomes an ordinary replica of the whole source store on the mounting server (first cut: replicating only the mounted subtree is out of scope), kept current by the same `SyncLink` any replica has.
- **Writes go to the source.** Editing a mounted node writes to the source store (directly for local mounts, via RPC for remote mounts). The CRDT layer handles conflict resolution if multiple clients edit concurrently.
- **Tree structure is owned by the source.** You cannot reparent or reorder children within a mounted subtree from the mounting store's context. You can edit node content, but structural changes (add/move/delete children) must be authorized by the source store.

### Recursive Mounts

A mounted subtree may itself contain mount points to other stores. These are resolved transitively — the resolver follows the chain. To prevent cycles and infinite recursion:

- **Cycle detection**: The resolver tracks the resolution chain `[(StoreId, NodeId), ...]` and rejects if a `(store, node)` pair appears twice.
- **Depth limit**: A configurable maximum mount depth (e.g., 16) prevents runaway chains even without cycles.
- **Resolution is lazy**: Mounts are only resolved when the user expands them in the tree, so deeply-nested mounts don't incur cost until accessed.

### Node Addressing

With mounts, a node's identity becomes context-dependent. The same source node can appear in multiple places via different mount points. Addressing uses two schemes:

- **Canonical address**: `(source_store_id, node_id)` — the authoritative identity, used for CRDT sync, storage, and deduplication.
- **Path address**: The chain of mount points leading to the node in a particular workspace tree — used for UI state (expanded nodes, scroll position) and navigation history.

The `selected_node` in `WorkspaceUiState` currently uses `(StoreId, NodeId)` which is the canonical form. This remains correct — the UI resolves the canonical address through the mount chain for display purposes.

### Impact on Existing Components

| Component | Change needed |
| --- | --- |
| `StoreManager` | Add `MountResolver` and `StoreRegistry`. `get_node` / `get_children` must handle mount-point traversal. |
| `StoreLocation::Mounted` | Already exists — becomes the backing type for resolved mounts. |
| `Node` | New `node_type: "mount"`. Mount metadata stored in `metadata.custom` (or a dedicated field). |
| `PimbleApi` | `createMount`, `getMountState`. `getChildren` transparently resolves a mount server-side and reports the canonical store via `GetChildrenResponse.store_id`. |
| `Workspace` | The store registry is workspace-level state — it knows how to reach each store. |
| `pimble-app` UI | Mount point nodes get a distinct visual treatment (icon, connectivity indicator). Mounted subtrees render identically to local subtrees. |
| `pimble-search` | Cross-mount search: search can optionally traverse into mounted subtrees. Results include the canonical `(store_id, node_id)`. |

---

## UI Architecture (`pimble-app`)

### Rinch Component Hierarchy

```
App
├── Menubar (VS Code-style)
├── Toolbar
│   ├── Open Store / New Store buttons
│   └── Connection status
├── Content (horizontal split)
│   ├── TreePanel (left)
│   │   └── Tree with expand/collapse, inline rename, drag-and-drop
│   └── NodeViewer (right)
│       └── Rich text editor pane (rinch editor + collaboration)
└── StatusBar
```

### State Management

The app uses Rinch reactive signals for UI state and a background thread with `crossbeam-channel` for server communication:

```rust
// UI ←→ Backend communication
pub enum BackendCommand { ... }  // UI sends to backend
pub enum BackendEvent { ... }    // Backend sends to UI

// App state uses Rinch signals
pub struct AppState {
    pub stores: Signal<Vec<TreeItem>>,
    pub selected_node: Signal<Option<(StoreId, NodeId)>>,
    pub connection_status: Signal<String>,
    // ...
}
```

---

## Data Flow

```
┌─────────────────────────────────────────────────────────────┐
│                        Rinch UI                              │
│  (TreePanel, NodeViewer, SearchBar)                          │
└─────────────────────┬───────────────────────────────────────┘
                      │ BackendCommand / BackendEvent
                      │ (crossbeam-channel, in-process)
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                   Embedded Server                            │
│  ┌─────────────┐  ┌──────────────┐  ┌─────────────┐        │
│  │ StoreManager│  │MountResolver │  │ PluginHost  │        │
│  └──────┬──────┘  └──────┬───────┘  └──────┬──────┘        │
│         │                │                  │               │
│         ▼                ▼                  ▼               │
│  ┌─────────────────────────────────────────────────┐       │
│  │                CRDT Layer (yrs)                  │       │
│  └──────────────────────┬──────────────────────────┘       │
│                         │                                   │
│         ┌───────────────┼───────────────┐                  │
│         ▼               ▼               ▼                  │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐          │
│  │ Local Store │ │ Local Store │ │Remote Client│          │
│  │ (files)     │ │ (cached)    │ │ (RPC/WS)   │          │
│  └─────────────┘ └─────────────┘ └──────┬──────┘          │
└─────────────────────────────────────────┼──────────────────┘
                                          │ JSON-RPC (WebSocket)
                                          ▼
                                   ┌─────────────┐
                                   │Remote Server│
                                   │ (another    │
                                   │  Pimble     │
                                   │  instance)  │
                                   └─────────────┘
```

---

## Implementation Phases

### Phase 1: Foundation ✅ COMPLETE
1. Initialize Cargo workspace with basic crate structure
2. `pimble-core`: Define Node, Store, Workspace types
3. `pimble-crdt`: Integrate yrs, implement basic document operations
4. `pimble-store`: Local file-based store
5. `pimble-rpc`: JSON-RPC types and basic client/server

### Phase 2: Basic UI ✅ COMPLETE
1. `pimble-app`: Embedded server, Rinch UI with BackendCommand/BackendEvent
2. TreePanel: Display node tree with expand/collapse, inline rename, drag-and-drop
3. NodeViewer: rich text editor with CRDT sync
4. Store open/create via toolbar buttons
5. VS Code-style menubar, dark mode

### Phase 3: Document Editing (current)
1. Rich text editing improvements (formatting, undo/redo)
2. Real-time CRDT sync refinements
3. Keyboard navigation in tree panel

### Phase 4: Mount Points & Cross-Store References
1. `MountRef` type and `"mount"` node type
2. `StoreRegistry` — catalog of known stores and how to reach them
3. `MountResolver` — resolve mount refs to live store handles
4. `StoreManager` extension to transparently traverse mounts
5. UI: mount point rendering, connectivity indicators, mount creation UX
6. Local-to-local mounts first, then remote mounts (both done; see "Mount Architecture")

### Replica sync ✅ COMPLETE (`docs/history/SYNC_CONTRACT.md`)

A local store can be a **replica** of the same store (same `StoreId`) held by another Pimble
server. The server relays and persists; it never owns the data, and offline is normal —
both sides keep working and converge on reconnect with no conflicts, because everything is
a yrs merge.

- **One `SyncLink` per linked store**, owned by the server's `RpcHandler` (a `Clone`-able bag
  of `Arc`s). A remote change enters through the handler's own `apply_edit`/
  `apply_store_update` — the same entry points any client uses — with
  `client_id = "sync-link:<uuid>"`, so persistence, local subscribers, and the search index
  all follow automatically.
- **Local changes are broadcast in-process** (`SubscriptionRegistry`'s
  `tokio::sync::broadcast::Sender<LocalChange>`, published wherever a WebSocket sink would be
  notified). A link subscribes to this to learn what to forward, and to the remote's own
  `subscribeStoreChanges` to learn what to pull. When forwarding outward a link skips only
  the changes it applied itself (they came from the remote it would send them back to); a
  change another link applied here is forwarded on, so an edit travels a whole chain of
  servers (`M -> L -> R`). Echo storms cannot start because a change that bounces back to a
  server that already has it merges as a no-op, and a no-op merge sends no notification.
  `ContentUpdated` notifications carry the edit's delta bytes, so a store-level subscriber
  gets content changes without subscribing per node.
- **Reconcile** is `(diff, remote_sv) = remote.syncStoreDocument(local_sv)`; apply `diff`
  locally; push `local.diff_if_peer_lacks_it(remote_sv, diff)` back when it is `Some` — the
  same shape for node content via `syncNodeContents` (up to 100 nodes per request) and
  `applyEdit`. A yrs diff is never empty (`[0, 0]` plus the sender's whole delete set), so
  "does the peer lack anything" compares our structs beyond its state vector and our delete
  set against the delete set its diff carried; and a merge that changes nothing is a no-op
  on the server (no `modified_at`, flush, notification or re-index). A full reconcile does
  the store document first, then node content in batches, so both sides agree on the node
  set before content reconciles start.
- **Tree repair.** Concurrent moves on two replicas can merge into a child listed under two
  parents, a cycle, or an orphan. After every changing `applyStoreUpdate`, and when a store
  opens, the server runs `StoreDocument::repair`, which is deterministic in the merged state
  (so replicas repairing the same state make the same edits) and broadcasts the repair as
  `TreeStructure { node_ids }` with `source_client_id: None`, so links forward it.
  Structural notifications name every parent they change (`NodeCreated`/`NodeDeleted`
  `parent_id`, `NodeMoved` both parents, `TreeStructure` the touched entries), and deleting
  a node deletes its subtree.
- **Lifecycle**: connect, full reconcile (`Syncing`), subscribe, process (`Synced { last_sync
  }`); any error drops the connection and retries with backoff (1s doubling to 30s,
  `Offline` meanwhile). The link is persisted as `<store>/sync.json`; `openStore` starts it,
  `closeStore`/unlinking stops it.
- **`addRemoteStore`** creates an empty replica (never `StoreDocument::new` — two independent
  roots for the same id would merge into duplicated children) plus a link, and waits briefly
  for the first reconcile before answering.
- **`removeReplica`** (`docs/history/HARDENING_CONTRACT.md` decision 6) removes a replica this
  server created with `addRemoteStore`: refused for a store whose directory isn't under
  `<data dir>/pimble/replicas/` (`Store.is_replica`), and refused for a replica whose link
  isn't `Synced` unless `force` (its unsynced local changes are lost). It stops the link,
  closes the store the same way `closeStore` does, then deletes the directory. Saved
  credentials for that remote are untouched — other replicas may still need them.
- Out of scope: partial (subtree-only) replication and TLS. Remote mounts are built on
  this (see "Mount State & Degradation"): a mount whose source isn't on this machine
  replicates that source with the same machinery and then resolves it locally.

### Auth (`docs/history/HARDENING_CONTRACT.md` decisions 1-5)

Two checks run at the HTTP edge, before any request reaches JSON-RPC dispatch (a tower
layer on the jsonrpsee server, covering the WebSocket upgrade too): a request carrying an
`Origin` header is always refused with `403` (browsers always send one on a WebSocket
handshake; no legitimate Pimble client ever does), and, when the server has a token, a
request must carry it as `Authorization: Bearer <token>` or `X-Api-Key: <token>`
(constant-time compared) or is refused with `401`. A server refuses to bind a non-loopback
address without a token. The app's embedded server stays on `127.0.0.1:7462` with no
token — it trusts local processes the way any loopback-only service does, the same trust
boundary as the OS's other local sockets.

Credentials this server uses to reach a remote (a sync link, `addRemoteStore`,
`setStoreSync`, `listRemoteStores`) live in one file per user, keyed by the remote's
origin, never in a store directory and never echoed back in an RPC response — a store's
`sync.json` always records `auth: none`, and `getStoreSync`/`setStoreSync` never return a
credential. The app never connects to a remote itself; it always asks its own server to
via `listRemoteStores`.

### Phase 6: Search & Indexing
1. `pimble-search`: index and query engine (see `docs/RESTART_PLAN.md` §5)
2. Embedding generation (local model: `all-MiniLM-L6-v2`)
3. Cross-mount search traversal
4. Search UI: SearchBar, results panel

### Phase 7: Linking & Navigation
1. Node linking: Create links between nodes (including cross-store via mounts)
2. Deep linking: Text anchors in documents
3. Link following: Navigate between nodes
4. Backlinks: Show nodes that link to current node

### Phase 8: Plugin System
1. WASM host setup with wasmtime
2. Plugin interface definition
3. Document plugin as WASM (proof of concept)
4. Plugin loading and registration

---

## Key Dependencies

```toml
yrs = "0.27"                 # CRDT (node content and the store document)
jsonrpsee = "0.24"           # JSON-RPC
rinch = { git = "..." }      # UI framework
wasmtime = "27"              # WASM runtime (Phase 8)
```

---

## File Formats

### Workspace File (`.pimble-workspace`)
```json
{
  "version": 1,
  "id": "uuid",
  "name": "My Workspace",
  "stores": [
    {
      "id": "uuid",
      "name": "Local Notes",
      "location": { "type": "local", "path": "./notes.pimble" }
    }
  ],
  "registry": {
    "known_stores": {
      "store-uuid-1": { "type": "local", "path": "/home/user/notes.pimble" },
      "store-uuid-2": { "type": "remote", "url": "wss://alice.example/pimble", "auth": { "method": "bearer", "token": "..." } }
    }
  },
  "ui_state": { ... }
}
```

### Store Directory (`.pimble/`)
```
my-notes.pimble/
├── manifest.json           # Store metadata (version: 3)
├── store.yrs               # Tree structure + node metadata (CRDT, yrs)
├── nodes/
│   ├── {node-id}.yrs       # Per-node content documents (CRDT, yrs)
│   └── ...
├── assets/                 # Binary files (images, attachments)
│   ├── {hash}.png
│   └── {hash}.pdf
├── cache/                  # Cached data from remote mounts (future)
│   └── {store-id}/
│       └── {node-id}.yrs
└── index/                  # Search indexes (future)
```
