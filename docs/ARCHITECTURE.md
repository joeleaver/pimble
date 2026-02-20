# Pimble - Personal Information Manager Architecture Plan

## Overview

Pimble is an offline-first personal information manager with:
- CRDT-based data model (Automerge) for eventual online collaboration
- Rust backend and frontend (Makepad UI)
- JSON-RPC communication between components
- WASM plugin system for extensible node types
- Embedded vector database for semantic search

---

## Project Structure (Cargo Workspace)

```
pimble/
├── Cargo.toml                 # Workspace root
├── crates/
│   ├── pimble-core/           # Core types, traits, node definitions
│   ├── pimble-crdt/           # Automerge integration, CRDT operations
│   ├── pimble-store/          # Store abstraction (local + remote)
│   ├── pimble-search/         # Vector DB, indexing, semantic search
│   ├── pimble-rpc/            # JSON-RPC protocol definitions
│   ├── pimble-server/         # Local/remote server implementation
│   ├── pimble-client/         # Client library for connecting to servers
│   ├── pimble-plugins/        # WASM plugin host + built-in plugins
│   ├── pimble-app/            # Makepad desktop application
│   └── pimble-cli/            # Optional CLI tool for debugging
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
    pub node_type: String,           // e.g., "document", "image", "canvas"
    pub metadata: NodeMetadata,
    pub content: Vec<u8>,            // CRDT document bytes (Automerge)
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
    pub target: LinkTarget,          // NodeId or DeepLink
    pub link_type: String,           // "reference", "embed", etc.
    pub source_anchor: Option<String>,
}

pub enum LinkTarget {
    Node(NodeId),
    Deep { node_id: NodeId, anchor: String },
    External(Url),
}
```

### 2. Store System (`pimble-store`)

A **Store** represents an entry point to a tree of nodes.

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

All mutations go through Automerge for conflict-free merging.

```rust
pub struct CrdtDocument {
    doc: AutoCommit,
}

impl CrdtDocument {
    pub fn new() -> Self;
    pub fn load(bytes: &[u8]) -> Result<Self>;
    pub fn save(&mut self) -> Vec<u8>;
    pub fn get_heads(&mut self) -> Vec<ChangeHash>;
    pub fn merge(&mut self, other: &mut Self) -> Result<()>;
    // ... getters/setters for various types
}
```

### 5. Search & Indexing (`pimble-search`)

Each store maintains its own index locally. Cross-store search aggregates results.

```rust
pub struct SearchIndex {
    pub store_id: StoreId,
    // vector_db: EmbeddedVectorDb,  // Phase 4
    // fts_index: TantivyIndex,       // Phase 4
}

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

**Embedding model** (Phase 4): Local-only using `all-MiniLM-L6-v2` (384 dimensions, ~80MB model) via `candle` or `ort`.

### 6. RPC Protocol (`pimble-rpc`)

JSON-RPC 2.0 over HTTP (WebSocket for subscriptions in future).

```rust
#[rpc(server, client, namespace = "pimble")]
pub trait PimbleApi {
    // Store operations
    async fn create_store(&self, request: CreateStoreRequest) -> Result<CreateStoreResponse>;
    async fn open_store(&self, request: OpenStoreRequest) -> Result<OpenStoreResponse>;
    async fn close_store(&self, request: CloseStoreRequest) -> Result<EmptyResponse>;
    async fn list_stores(&self) -> Result<ListStoresResponse>;

    // Node operations
    async fn get_node(&self, request: GetNodeRequest) -> Result<GetNodeResponse>;
    async fn create_node(&self, request: CreateNodeRequest) -> Result<CreateNodeResponse>;
    async fn delete_node(&self, request: DeleteNodeRequest) -> Result<EmptyResponse>;
    async fn get_children(&self, request: GetChildrenRequest) -> Result<GetChildrenResponse>;

    // Workspace operations
    async fn load_workspace(&self, request: LoadWorkspaceRequest) -> Result<LoadWorkspaceResponse>;
    async fn save_workspace(&self, request: SaveWorkspaceRequest) -> Result<EmptyResponse>;

    // Search
    async fn search(&self, request: SearchRequest) -> Result<SearchResponse>;
}
```

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

## UI Architecture (`pimble-app`)

### Makepad Component Hierarchy

```
App
├── Window
│   ├── Toolbar
│   │   ├── Title
│   │   └── SearchInput
│   ├── Content (horizontal split)
│   │   ├── TreePanel (left, 250px)
│   │   │   └── StoreTree → TreeNode (recursive)
│   │   └── NodeViewer (right)
│   │       ├── NodeHeader
│   │       ├── NodeContent (plugin-rendered)
│   │       └── NodeFooter
│   └── StatusBar
```

### State Management

```rust
pub struct AppState {
    pub workspace: Option<Workspace>,
    pub selected_node: Option<(StoreId, NodeId)>,
    pub node_cache: HashMap<(StoreId, NodeId), CachedNode>,
    pub ui: UiState,
    pub connection: ConnectionState,
}
```

---

## Data Flow

```
┌─────────────────────────────────────────────────────────────┐
│                        Makepad UI                           │
│  (TreePanel, NodeViewer, SearchBar)                         │
└─────────────────────┬───────────────────────────────────────┘
                      │ JSON-RPC (HTTP)
                      ▼
┌─────────────────────────────────────────────────────────────┐
│                     Local Server                            │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ StoreManager│  │ SearchEngine│  │ PluginHost  │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         │                │                │                 │
│         ▼                ▼                ▼                 │
│  ┌─────────────────────────────────────────────────┐       │
│  │              CRDT Layer (Automerge)             │       │
│  └─────────────────────────────────────────────────┘       │
│         │                                                   │
│         ▼                                                   │
│  ┌─────────────┐                                           │
│  │ Local Store │                                           │
│  │ (files)     │                                           │
│  └─────────────┘                                           │
└─────────────────────────────────────────────────────────────┘
```

---

## Implementation Phases

### Phase 1: Foundation ✅ COMPLETE
1. Initialize Cargo workspace with basic crate structure
2. `pimble-core`: Define Node, Store, Workspace types
3. `pimble-crdt`: Integrate Automerge, implement basic document operations
4. `pimble-store`: Local file-based store
5. `pimble-rpc`: JSON-RPC types and basic client/server

### Phase 2: Basic UI
1. `pimble-app`: Connect to local server via JSON-RPC
2. TreePanel: Display node tree from store
3. NodeViewer: Basic document viewing (read-only first)
4. Store open/create dialogs

### Phase 3: Document Editing
1. Document node plugin: Markdown editing with Automerge
2. Real-time CRDT sync: Changes flow through system
3. Persistence: Save/load stores and workspaces

### Phase 4: Search & Indexing
1. `pimble-search`: Embedded vector DB setup
2. Text extraction from nodes
3. Embedding generation (local model: `all-MiniLM-L6-v2`)
4. Search UI: SearchBar, results panel

### Phase 5: Linking & Navigation
1. Node linking: Create links between nodes
2. Deep linking: Text anchors in documents
3. Link following: Navigate between nodes
4. Backlinks: Show nodes that link to current node

### Phase 6: Plugin System
1. WASM host setup with wasmtime
2. Plugin interface definition
3. Document plugin as WASM (proof of concept)
4. Plugin loading and registration

### Phase 7: Remote Sync (Future)
1. Remote server implementation
2. Automerge sync protocol
3. Conflict resolution UI
4. Authentication

---

## Key Dependencies

```toml
automerge = "0.5"           # CRDT
jsonrpsee = "0.24"          # JSON-RPC
makepad-widgets = "1.0"     # UI framework
wasmtime = "27"             # WASM runtime
tantivy = "0.22"            # Full-text search
# candle-core = "0.4"       # ML inference (Phase 4)
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
  "ui_state": { ... }
}
```

### Store Directory (`.pimble/`)
```
my-notes.pimble/
├── manifest.json           # Store metadata, schema version
├── nodes/
│   ├── {node-id}.json      # Node metadata
│   ├── {node-id}.automerge # CRDT content (separate for efficiency)
│   └── ...
├── assets/                 # Binary files (images, attachments)
│   ├── {hash}.png
│   └── {hash}.pdf
├── index/                  # Search indexes
│   ├── vectors.lance       # Vector embeddings (Phase 4)
│   └── fts/                # Tantivy full-text index
└── sync/                   # Sync state for remote collaboration
    └── heads.json          # Last known sync heads
```
