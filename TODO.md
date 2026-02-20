# Pimble - Work Breakdown #plan

## Phase 1: Foundation ✅ COMPLETE

- [x] Initialize Cargo workspace
- [x] Create crate structure (10 crates)
- [x] `pimble-core`: Node, NodeId, NodeMetadata, NodeLink, LinkTarget
- [x] `pimble-core`: Store, StoreId, StoreLocation, SyncState
- [x] `pimble-core`: Workspace, WorkspaceStore, WorkspaceUiState
- [x] `pimble-crdt`: CrdtDocument wrapper around Automerge
- [x] `pimble-crdt`: DocumentContent, FolderContent helpers
- [x] `pimble-store`: LocalStore file-based implementation
- [x] `pimble-store`: StoreManager for multiple stores
- [x] `pimble-search`: Skeleton with SearchQuery, SearchResult types
- [x] `pimble-rpc`: PimbleApi trait with jsonrpsee
- [x] `pimble-rpc`: All request/response types
- [x] `pimble-server`: RpcHandler implementation
- [x] `pimble-server`: PimbleServer with start/stop
- [x] `pimble-client`: PimbleClient with all RPC methods
- [x] `pimble-plugins`: NodePlugin trait, PluginHost
- [x] `pimble-plugins`: Built-in DocumentPlugin, FolderPlugin
- [x] `pimble-app`: Makepad app skeleton with live_design!
- [x] `pimble-cli`: Basic commands (server, create-store, open-store, list-stores)
- [x] Verify workspace builds

---

## Phase 2: Basic UI 🔄 IN PROGRESS

### 2.1 Server Connection ✅ COMPLETE
- [x] Add tokio runtime to pimble-app (via background thread)
- [x] Create async client connection on app startup
- [x] Handle connection state (connecting, connected, error)
- [x] Display connection status in status bar

### 2.2 Workspace Management
- [ ] "New Workspace" dialog
- [ ] "Open Workspace" file picker
- [ ] Save workspace on close
- [ ] Recent workspaces list

### 2.3 Store Management
- [ ] "Create Store" dialog (name, path picker)
- [ ] "Open Store" file picker
- [ ] Store list in TreePanel header
- [ ] Close store context menu

### 2.4 TreePanel Implementation
- [ ] Fetch root node on store open
- [ ] Display node tree recursively
- [ ] Expand/collapse nodes (fetch children on expand)
- [ ] Select node → update NodeViewer
- [ ] Node icons by type (folder, document)
- [ ] Context menu (new child, rename, delete)

### 2.5 NodeViewer (Read-Only)
- [ ] Display node title in header
- [ ] Display node metadata (created, modified, tags)
- [ ] Render document content as text
- [ ] "No node selected" placeholder

### 2.6 Polish
- [ ] Keyboard navigation in tree (up/down/enter/left/right)
- [ ] Loading indicators
- [ ] Error toasts/notifications

---

## Phase 3: Document Editing

### 3.1 Text Editor Widget
- [ ] Basic multiline text input in NodeViewer
- [ ] Cursor positioning
- [ ] Text selection
- [ ] Copy/paste support

### 3.2 CRDT Integration
- [ ] Load document CRDT on node select
- [ ] Apply local edits to CRDT
- [ ] Save CRDT back to store
- [ ] Auto-save on idle (debounced)

### 3.3 Rich Text (Optional)
- [ ] Markdown syntax highlighting
- [ ] Bold/italic keyboard shortcuts
- [ ] Heading formatting

### 3.4 Node Operations
- [ ] Create new node (document/folder)
- [ ] Rename node
- [ ] Delete node (with confirmation)
- [ ] Move node (drag-drop or cut/paste)

---

## Phase 4: Search & Indexing

### 4.1 Full-Text Search
- [ ] Integrate Tantivy into pimble-search
- [ ] Index node title + content on save
- [ ] Remove from index on delete
- [ ] Basic text search query

### 4.2 Search UI
- [ ] Search input in toolbar (already exists)
- [ ] Search results dropdown/panel
- [ ] Navigate to result on click
- [ ] Highlight matches in results

### 4.3 Vector Search (Semantic)
- [ ] Add candle/ort for local inference
- [ ] Download all-MiniLM-L6-v2 model on first run
- [ ] Generate embeddings on index
- [ ] Store vectors (lance or custom)
- [ ] Semantic search query
- [ ] Hybrid ranking (text + vector)

### 4.4 Incremental Indexing
- [ ] Watch for file changes
- [ ] Re-index changed nodes
- [ ] Background indexing

---

## Phase 5: Linking & Navigation

### 5.1 Link Creation
- [ ] `[[node-title]]` wiki-link syntax in documents
- [ ] Link autocomplete popup
- [ ] Create link from selection

### 5.2 Link Display
- [ ] Render links as clickable in viewer
- [ ] Link preview on hover
- [ ] External link handling (open browser)

### 5.3 Deep Links
- [ ] Text anchor format (e.g., `node-id#paragraph:3`)
- [ ] Scroll to anchor on navigate
- [ ] Highlight linked text

### 5.4 Backlinks
- [ ] Track incoming links in index
- [ ] Backlinks panel in NodeViewer footer
- [ ] Navigate to linking node

---

## Phase 6: Plugin System

### 6.1 WASM Host
- [ ] Define WASM interface (wit-bindgen)
- [ ] Load .wasm plugin files
- [ ] Plugin sandbox with wasmtime

### 6.2 Plugin API
- [ ] Pass node content to plugin
- [ ] Receive render output
- [ ] Plugin actions (toolbar buttons)

### 6.3 Built-in as WASM
- [ ] Compile DocumentPlugin to WASM
- [ ] Load from embedded bytes
- [ ] Verify same behavior

### 6.4 Custom Plugins
- [ ] Plugin directory scanning
- [ ] Plugin manifest format
- [ ] Example: Canvas node plugin

---

## Phase 7: Remote Sync (Future)

### 7.1 Remote Server
- [ ] Deploy server to cloud
- [ ] User authentication (OAuth2)
- [ ] Per-user store storage

### 7.2 Sync Protocol
- [ ] Automerge sync protocol implementation
- [ ] Efficient change transfer
- [ ] Offline queue

### 7.3 Conflict Resolution
- [ ] Detect conflicts
- [ ] Conflict UI (show both versions)
- [ ] Manual merge option

### 7.4 Sharing
- [ ] Share store with other users
- [ ] Permission levels (read, write, admin)
- [ ] Realtime collaboration indicators

---

## Ongoing / Cross-Cutting

- [ ] Error handling improvements
- [ ] Logging and diagnostics
- [ ] Performance profiling
- [ ] Unit tests for core logic
- [ ] Integration tests for RPC
- [ ] CI/CD pipeline
- [ ] Release builds and packaging
- [ ] User documentation
