# Pimble - Claude Context

This file provides context for Claude Code sessions working on this project.
Read `docs/NEXT_SESSION.md` first (where things stand, the open decision, how to verify), then `docs/RESTART_PLAN.md`: it holds the vision, the diagnosis of what went wrong,
and the ordered plan.

## Project Overview

Pimble is an **offline-first personal information manager**:
- Nodes in a tree inside a store; stores are `.pimble` directories
- CRDT node content so devices and people can edit concurrently and merge
- Rust backend, **rinch** UI (the Slint and Makepad UIs are gone)
- Embedded JSON-RPC server (jsonrpsee over WebSocket) between UI and store
- Mounts: any subtree of any store can appear in any other store's tree
- Search: keyword (rhypedb full-text) and semantic (chunked embeddings, `semantic` feature)
- WASM plugin system for node types (skeleton)

## Rules

- **rinch comes from GitHub `main`** (`joeleaver/rinch`), never a `path` dependency on
  `/home/joe/dev/rinch`. That directory is off limits. To read rinch source use the cargo
  checkout under `~/.cargo/git/checkouts/` or a fresh clone in the scratchpad. Move the
  pinned revision with `cargo update rinch rinch-tabler-icons rinch-editor-core`.
- rinch fixes go upstream as pull requests; point `Cargo.toml` at the branch until merged.
- Always build and run `pimble-app` with `--release`; debug builds are unusably slow.
- One way to write node content. If a second path appears, one of them is a bug.

## Current design (2026-09-13)

Both CRDT documents in Pimble are yrs. There is no Automerge anywhere in the stack and no
legacy format to read: a store predating this design does not open, and the answer is to
re-import from Scrivener.

| Crate | Purpose | Status |
| --- | --- | --- |
| `pimble-core` | Node, Store, Workspace, MountRef types | Complete |
| `pimble-crdt` | `ContentDoc` (per-node content) and `StoreDocument` (tree + metadata), both yrs | Complete |
| `pimble-store` | `LocalStore` (`store.yrs` + `nodes/{id}.yrs`), `StoreManager` | Complete |
| `pimble-rpc` / `pimble-server` / `pimble-client` | RPC protocol, embedded server, WebSocket client | Complete |
| `pimble-search` | rhypedb index per store: keyword (`@fulltext`, BM25), chunked embeddings behind `semantic`, backlinks | Complete |
| `pimble-plugins` | `NodePlugin` trait, built-ins | Skeleton |
| `pimble-app` | rinch desktop app | Works: two-window live editing, persistence verified |
| `pimble-cli` | server / create-store / list-stores | Complete |
| `pimble-import` | Scrivener + RTF import | Complete |

Node content is a yrs document (`pimble_crdt::ContentDoc`), stored as `nodes/{id}.yrs`.
The server holds one `ContentDoc` per open node, merges every `applyEdit` update, relays
the raw bytes to other subscribers, and flushes dirty content on a 750ms debounce and on
stop. The store document (tree structure and node metadata) is a second yrs document per
store, stored as `store.yrs`; `applyStoreUpdate` merges and relays it the same way.
`syncNodeContent` and `syncStoreDocument` are both stateless: state vector in, diff plus
server state vector out. `setNodeText` and `ServerSyncManager` do not exist.

### Search index

Each open store has a rhypedb database at `<store>/index/rhypedb/`, derived and
disposable (`rebuildIndex` RPC, "Rebuild search index" in the View menu). The server feeds
it in-process from the same places it broadcasts change notifications, with a 2s per-node
debounce on content. Nodes index `title` and `text` with `@fulltext`. Semantic search is
on by default in `pimble-app` (`onnx-download`: the ONNX runtime is linked statically,
the int8 `all-MiniLM-L6-v2` model downloads once into the user's data dir under
`pimble/models`, and the server warms it before any store opens; if that fails the app
runs keyword-only). Measured on the 674-node family store after rhypedb #18: model ready
in about 4 s, peak 767 MB resident, UI responsive during the backfill. Content is chunked (about 200 words, block-aligned, heading context, per-chunk
hash so an edit re-embeds one chunk) into `Chunk` objects with `all-MiniLM-L6-v2`
embeddings. Chunking works over `IndexUnit`s (prose, heading, code, table row, field,
other) produced by `ContentDoc::units()` or a plugin's `index_units`, so tables and
structured node types can index later without redesign. Both fields use rhypedb's
`english` analyzer (stemming), and the last typed word is searched as a prefix term so
results update while typing (rhypedb #17).

### Collaboration shape (keep these invariants)

- The app has ONE editor pane and one thread-local `EditorHandle` (`pimble-app/src/editor.rs`).
- Local edits: `EditorHandle` outbound closure -> `BackendCommand::BroadcastChanges` ->
  server persists and relays -> peers receive `BackendEvent::RemoteChanges` ->
  `EditorHandle::collab_receive`.
- Never wrap the editor in a document-model layer in the sync path. Never call
  `load_html`/`load_doc` on a collaborating editor.
- Rinch's collab scope is flat blocks + marks (paragraph, heading, code block; bold, italic,
  link). Lists and tables in a collaborating document fail loudly by design.

## Key Files

- `docs/RESTART_PLAN.md` - vision, diagnosis, decisions, ordered plan
- `docs/ARCHITECTURE.md` - the architecture, including mounts
- `crates/pimble-core/src/node.rs` - Node, NodeId, NodeLink, MountRef
- `crates/pimble-crdt/src/store_document.rs` - StoreDocument (tree + metadata CRDT)
- `crates/pimble-store/src/local.rs` - LocalStore
- `crates/pimble-rpc/src/methods.rs` - RPC API trait
- `crates/pimble-server/src/handler.rs` - RPC method implementations
- `crates/pimble-app/src/app.rs` - UI tree, menus, rename, drag-and-drop
- `crates/pimble-app/src/editor.rs` - editor pane + collaboration wiring
- `crates/pimble-app/src/backend.rs` - background thread, embedded server, `BackendCommand`/`BackendEvent`
- `crates/pimble-app/src/events.rs` - `BackendEvent` -> UI state

## Build & Run

```bash
cargo check --workspace
cargo build -p pimble-app --release
cargo run -p pimble-app --release          # starts the embedded server itself
```

The app has the rinch `debug` feature on, so the rinch MCP tools (`list_apps`, `connect`,
`screenshot`, `dom_tree`, `click`, `type_text`) can drive a running instance.

## Dependencies

- `rinch` (git main) with features `desktop, components, theme, file-dialogs, clipboard, debug, collaboration`; software rendering (no `gpu`, which would need rinch's wgpu fork patch)
- `rinch-editor-core` (git main)
- `yrs` 0.27 for both CRDT documents; `rinch-editor-collab` (git main) wraps it for node
  content's rich-text schema, the store document uses `yrs` directly (Maps and Arrays)
- `jsonrpsee` 0.24
- `rhypedb-engine`/`-schema`/`-query`/`-embed` (git, currently the `feat/18-vectorizer-hardening` branch until PR #19 merges, then `master`) for `pimble-search`; `semantic` turns on the code paths, `onnx-download` or `onnx-dynamic` picks the ONNX link mode
