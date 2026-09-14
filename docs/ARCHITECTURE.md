# Pimble - Personal Information Manager Architecture

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
    pub fn apply_update(&mut self, update: &[u8]) -> Result<()>;
    pub fn state_vector(&self) -> Vec<u8>;
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>>;
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
    pub fn apply_update(&mut self, update: &[u8]) -> Result<()>;
    pub fn state_vector(&self) -> Vec<u8>;
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>>;

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
        │   StoreManager.ensure_store_open(mount_ref)
        │       │
        │       ├─ source store already open? → use it
        │       ├─ registry has a Local/Remote endpoint? → open it (Remote: unavailable)
        │       ├─ mount_ref.source_path hint set? → open it, registering on success
        │       └─ none of the above → MountSourceUnavailable
        │
        └─ regular node → return children from local store
```

This is a single hop, not a recursive resolve: `getChildren` on a mount returns exactly the source node's children, addressed by the source store's id (`GetChildrenResponse.store_id`). A nested mount among those children is itself an ordinary node in the (now open) source store, resolved the same way when the caller expands it in turn — so cycles are caught once, at `createMount` time (`validate_mount_creation`), not on every expansion.

**An implicitly opened source is an ordinary open store.** When resolving or validating a mount opens a store that wasn't already open, it gets the same treatment `openStore` gives one: a search index under its own `index/rhypedb/`, a place in `listStores`, and subscriptions work on it. `StoreManager::opened_since()` drains the list of stores opened since the last call, so the RPC handler can open a search index for each one after any call that might have opened stores implicitly (`getChildren`, `getMountState`, `createMount`).

The resolver maintains a **store registry** — a mapping from `StoreId` to connection information:

```rust
/// How to reach a store
pub enum StoreEndpoint {
    /// Local filesystem path
    Local { path: PathBuf },
    /// Remote server (defined; not yet used — remote mounts are out of scope)
    Remote { url: Url, auth: AuthMethod },
}

/// Registry of known stores and how to reach them
pub struct StoreRegistry {
    /// Known store endpoints
    endpoints: HashMap<StoreId, StoreEndpoint>,
}
```

An already-open store needs no registry lookup at all — `local_stores` is checked first. The registry is populated by every `create_local_store`/`open_local_store` call (including ones a mount triggers), so it has an entry for any store this process has opened this session. `MountRef.source_path` exists for the case the registry doesn't cover: a fresh process, before anything has reopened the source directly.

### Mount State & Degradation

Mounts can be in various states, especially when the source is remote:

```rust
pub enum MountState {
    /// Source is connected and live
    Live,
    /// Source is unavailable; showing cached data
    Cached { last_sync: DateTime<Utc> },
    /// Source is unavailable and no cache exists
    Unavailable,
    /// Currently connecting/syncing
    Connecting,
}
```

**Offline behavior**: When a remote mount's source is unreachable, Pimble shows cached data if available (the locally-replicated `.yrs` files). The UI indicates staleness but remains functional. When the source comes back online, yrs sync brings the local replica up to date.

### Data Ownership & Sync

- **Data lives in the source store.** The mount point's store does not copy or own the mounted data.
- **Local caching via CRDT replication.** For remote mounts, Pimble maintains a local replica of the mounted subtree's yrs documents. Since each node has its own `.yrs` file, only the mounted subtree's nodes need to be replicated — not the entire source store.
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
6. Local-to-local mounts first, then remote mounts

### Phase 5: Remote Sync
1. WebSocket transport for RPC
2. yrs sync protocol for per-node and per-store CRDT replication
3. Partial sync — replicate only mounted subtrees, not whole stores
4. Conflict resolution UI
5. Authentication (API key, Bearer token, OAuth2 — already modeled in `AuthMethod`)
6. Offline cache with staleness indicators

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
