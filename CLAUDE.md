# Pimble - Claude Context

This file provides context for Claude Code sessions working on this project.
Read `docs/RESTART_PLAN.md` first: it holds the vision, the diagnosis of what went wrong,
and the ordered plan.

## Project Overview

Pimble is an **offline-first personal information manager**:
- Nodes in a tree inside a store; stores are `.pimble` directories
- CRDT node content so devices and people can edit concurrently and merge
- Rust backend, **rinch** UI (the Slint and Makepad UIs are gone)
- Embedded JSON-RPC server (jsonrpsee over WebSocket) between UI and store
- Mounts: any subtree of any store can appear in any other store's tree
- Search: keyword and semantic (rhypedb is the planned index engine)
- WASM plugin system for node types (skeleton)

## Rules

- **rinch comes from GitHub `main`** (`joeleaver/rinch`), never a `path` dependency on
  `/home/joe/dev/rinch`. That directory is off limits. To read rinch source use the cargo
  checkout under `~/.cargo/git/checkouts/` or a fresh clone in the scratchpad. Move the
  pinned revision with `cargo update rinch rinch-tabler-icons rinch-editor-core`.
- rinch fixes go upstream as pull requests; point `Cargo.toml` at the branch until merged.
- Always build and run `pimble-app` with `--release`; debug builds are unusably slow.
- One way to write node content. If a second path appears, one of them is a bug.

## Current Status (2026-09-12)

| Crate | Purpose | Status |
| --- | --- | --- |
| `pimble-core` | Node, Store, Workspace, MountRef types | Complete |
| `pimble-crdt` | `ContentDoc` (yrs node content), `StoreDocument` (Automerge tree) | Complete for Phase A |
| `pimble-store` | `LocalStore` (`store.automerge` + `nodes/*.yrs`), `StoreManager`, legacy migration | Complete |
| `pimble-rpc` / `pimble-server` / `pimble-client` | RPC protocol, embedded server, WebSocket client | Complete |
| `pimble-search` | Search types | Skeleton |
| `pimble-plugins` | `NodePlugin` trait, built-ins | Skeleton |
| `pimble-app` | rinch desktop app | Works: two-window live editing, persistence verified |
| `pimble-cli` | server / create-store / list-stores | Excluded from workspace |
| `pimble-import` | Scrivener + RTF import | Excluded from workspace |

### Phase A landed (2026-09-12): node content is yrs

Node content is a yrs document (`pimble_crdt::ContentDoc`), stored as `nodes/{id}.yrs`.
Legacy `nodes/{id}.automerge` content is migrated best-effort on first access (text only)
and the legacy file is left in place. The server holds one `ContentDoc` per open node,
merges every `applyEdit` update, relays the raw bytes to other subscribers, and flushes
dirty content on a 750ms debounce and on stop. `syncNodeContent` is stateless
(state vector in, diff + server state vector out). `setNodeText` no longer exists.
The store document (tree + metadata) is still Automerge until Phase B.

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
- `docs/ARCHITECTURE.md` - the original architecture, including mounts
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
- `yrs` 0.27 and `rinch-editor-collab` (git main) for node content
- `automerge` 0.5 for the store document only (retired in Phase B)
- `jsonrpsee` 0.24
- `rhypedb` (git, `joeleaver/rhypedb`) planned for `pimble-search`
