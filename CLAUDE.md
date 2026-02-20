# Pimble - Claude Context

This file provides context for Claude Code sessions working on this project.

## Project Overview

Pimble is an **offline-first personal information manager** with:
- CRDT-based data model (Automerge) for eventual online collaboration
- Rust backend and Slint UI framework (migrated from Makepad)
- JSON-RPC communication between components
- WASM plugin system for extensible node types
- Embedded vector database for semantic search (future)

## Current Status: Phase 2 Complete (Basic UI)

### What's Built

| Crate | Purpose | Status |
| --- | --- | --- |
| `pimble-core` | Core types (Node, Store, Workspace) | ✅ Complete |
| `pimble-crdt` | Automerge CRDT integration | ✅ Complete |
| `pimble-store` | Local file-based storage | ✅ Complete |
| `pimble-search` | Search infrastructure | ⏳ Skeleton only |
| `pimble-rpc` | JSON-RPC protocol definitions | ✅ Complete |
| `pimble-server` | Local RPC server | ✅ Complete |
| `pimble-client` | Client library | ✅ Complete |
| `pimble-plugins` | WASM plugin host | ⏳ Skeleton + built-ins |
| `pimble-app` | Slint desktop app | ✅ Basic UI + VS Code-style menubar |
| `pimble-cli` | Command-line interface | ✅ Complete |

### Phase 2 Complete: Basic UI

- ✅ Backend connection with background thread + tokio runtime
- ✅ `BackendCommand` / `BackendEvent` enums for UI↔Backend communication
- ✅ `crossbeam-channel` for thread-safe message passing
- ✅ App connects to server on startup, displays connection status
- ✅ Store/node state management in `AppState` with `TreeItem` for display
- ✅ "Open Store" and "New Store" buttons in toolbar
- ✅ Interactive TreePanel with expand/collapse (click handling)
- ✅ Node selection updates viewer
- ✅ CRDT document content rendering in NodeViewer

### What's Next: Phase 3 (Document Editing)

1. Text editing in NodeViewer (cursor, selection, input)
2. Real-time CRDT sync back to server
3. Undo/redo support
4. Keyboard navigation in tree

### Future Phases

- **Phase 3**: Document editing with real-time CRDT sync
- **Phase 4**: Search & indexing with vector DB (`all-MiniLM-L6-v2`)
- **Phase 5**: Linking & navigation (deep links, backlinks)
- **Phase 6**: WASM plugin system
- **Phase 7**: Remote sync

## Architecture Quick Reference

### Data Model
- **Node**: Basic unit of content (has id, parent, type, metadata, CRDT content, children, links)
- **Store**: Container for a tree of nodes (local directory or remote)
- **Workspace**: User's view into multiple stores (`.pimble-workspace` file)

### Store Directory Structure
```
my-notes.pimble/
├── manifest.json           # Store metadata
├── nodes/
│   ├── {node-id}.json      # Node metadata
│   ├── {node-id}.automerge # CRDT content
├── assets/                 # Binary files
└── index/                  # Search indexes
```

### Communication Flow
```
Slint UI ←→ JSON-RPC (WebSocket/HTTP) ←→ Local Server ←→ Store (files)
```

## Key Files

- `crates/pimble-core/src/node.rs` - Node, NodeId, NodeLink types
- `crates/pimble-core/src/store.rs` - Store, StoreId, StoreLocation types
- `crates/pimble-core/src/workspace.rs` - Workspace type
- `crates/pimble-crdt/src/document.rs` - CrdtDocument wrapper
- `crates/pimble-store/src/local.rs` - LocalStore implementation
- `crates/pimble-rpc/src/methods.rs` - RPC API trait definition
- `crates/pimble-server/src/handler.rs` - RPC method implementations
- `crates/pimble-app/src/app.rs` - Slint app main logic
- `crates/pimble-app/ui/app.slint` - Slint UI definition
- `crates/pimble-app/ui/fonts/codicon.ttf` - VS Code icon font

## Next Session: "Continue with Phase 3"

**Phase 2 is complete.** The basic UI can:
- Connect to server and display connection status
- Open/create stores via buttons (hardcoded paths for now)
- Display tree of stores and nodes with expand/collapse
- Select nodes and view CRDT document content

**Next steps (Phase 3):**
- Replace read-only Text with TextInput for document editing in NodeViewer
- Handle text changes and sync back via CRDT
- Add file picker dialogs for Open/New Store buttons
- Keyboard navigation in tree panel
- Undo/redo support

## Build & Run

```powershell
# Check build
cargo check --workspace

# Run server
cargo run -p pimble-cli -- server

# Run desktop app
cargo run -p pimble-app

# Create a store (with server running)
cargo run -p pimble-cli -- create-store ./test.pimble "Test Store"
```

## Dependencies

Key external crates:
- `automerge` 0.5 - CRDT
- `jsonrpsee` 0.24 - JSON-RPC
- `slint` 1.9 - UI framework
- `i-slint-backend-winit` 1.9 - Winit backend for window control
- `tantivy` 0.22 - Full-text search (Phase 4)
- `wasmtime` 27 - WASM runtime (Phase 6)
