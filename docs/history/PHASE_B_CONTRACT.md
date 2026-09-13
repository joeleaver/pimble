# Phase B contract: store document on yrs

Goal (see `docs/RESTART_PLAN.md` §4, Phase B): the store document (tree structure and
node metadata) moves from Automerge to yrs, so both document kinds share one CRDT, one
wire encoding (yrs v1 updates) and one sync primitive (state vector in, diff out).
Automerge stays in the tree only as a **read-only legacy reader** used for one-time
migration of `store.automerge` and `nodes/{id}.automerge`.

Signatures below are fixed. Deviations are reported, not improvised.

## Ownership (parallel work, no overlap)

| Agent | Crates / files | Depends on |
| --- | --- | --- |
| A | `pimble-crdt`: rewrite `store_document.rs` on yrs; move the current Automerge implementation verbatim to `legacy_store_document.rs` as `LegacyStoreDocument` | nothing |
| B | `pimble-store` (migration + `store.yrs`), `pimble-rpc`, `pimble-server` (stateless store-doc sync, delete `sync_state.rs`), `pimble-client` | A |
| C | `pimble-app`, `pimble-cli` | A, B's client signature |

Nobody edits the root `Cargo.toml` (yrs, automerge, rinch-editor-* are already there).

## A. `pimble_crdt::StoreDocument` on yrs

Keep the **same public API** as today (so `pimble-store` barely changes), with these
exact differences:

Removed: `get_heads`, `inner`, `inner_mut`, `fork`, `merge`.

Added (mirroring `ContentDoc`):
```rust
pub fn save(&self) -> Vec<u8>;                         // note: &self, full v1 snapshot
pub fn state_vector(&self) -> Vec<u8>;                  // v1
pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>>;
pub fn apply_update(&mut self, update: &[u8]) -> Result<()>;
```

Unchanged (same names, same signatures, same semantics): `new(name, root_node_id)`,
`load(bytes)`, `root_node_id`, `add_node`, `add_node_bare`, `remove_node`, `move_node`,
`set_title`, `set_tags`, `set_custom`, `set_node_type`, `set_parent_id`,
`set_timestamps`, `touch_modified`, `append_child`, `get_node_info`, `get_children`,
`list_node_ids`, `has_node`, `validate_tree`, and the `NodeInfo` / `TreeIssue` types.

Layout inside the yrs `Doc` (built with `OffsetKind::Utf16`, like `ContentDoc`):
```
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
`move_node` = remove the id from the old parent's `children` array and insert into the new
parent's at the requested index, inside one transaction. Concurrent moves of the same
node can leave a duplicate or a stray entry; `validate_tree` must detect duplicates,
missing parents and cycles exactly as it does today, and `load` must call the existing
repair path if one exists (keep the current behaviour, do not add new repair logic).

`legacy_store_document.rs`: the current Automerge file moved verbatim, type renamed
`LegacyStoreDocument`, `#[doc(hidden)]`, module doc saying it exists only to migrate
`store.automerge`. Keep its full API (tests and the store migration need `new`/`add_node`
to build fixtures). `lib.rs` exports both.

Tests in `store_document.rs`: existing tests keep passing against the new engine; add
`round_trip_save_load`, `two_replicas_add_children_converge` (each replica adds a child
under root, exchange `diff_since`, both `get_children(root)` contain both ids and
`validate_tree` reports no issues), and `migrate_from_legacy_preserves_everything`
(build a `LegacyStoreDocument` with a folder, a document with tags/custom/timestamps, a
nested child; convert with `StoreDocument::from_legacy(&legacy) -> Result<StoreDocument>`
(new public fn, add it) and compare every `NodeInfo` and every `get_children` order).

`cargo test -p pimble-crdt` green.

## B. Store, RPC, server, client

### `pimble-store`
- `STORE_DOC_FILE` becomes `store.yrs`. On `open`: if `store.yrs` exists load it; else if
  `store.automerge` exists, `LegacyStoreDocument::load` then `StoreDocument::from_legacy`,
  write `store.yrs` atomically, log at info, leave `store.automerge` untouched; else
  (v1 json layout) run the existing v1 migration into the new `StoreDocument`.
  `manifest.version` becomes 3 when a store is created or migrated; opening version 2 or 1
  migrates as above.
- `flush` writes `store.yrs`. Everything else in `local.rs` compiles unchanged against
  A's identical API (drop any `get_heads`/`inner` use if present).
- `LocalStore` gains `store_doc_state_vector() -> Vec<u8>`, `store_doc_diff_since(&[u8])
  -> Result<Vec<u8>>`, `apply_store_doc_update(&[u8]) -> Result<()>` (marks the doc dirty
  and re-runs `validate_tree`, logging issues at warn). `StoreManager` mirrors the three.
- Tests: create store, add nodes, flush, reopen, tree identical; open a directory that has
  only `store.automerge` (built with `LegacyStoreDocument` in the test) and assert the
  tree, metadata and child order survive and `store.yrs` now exists.

### `pimble-rpc`
- `SyncStoreDocumentRequest { store_id, state_vector: String /* base64 v1 */ }`
- `SyncStoreDocumentResponse { diff: String /* base64, may be empty */, state_vector: String }`
- Update doc comments that still say Automerge.

### `pimble-server`
- `sync_store_document`: decode client state vector, `diff = store_doc_diff_since`, respond
  with diff + server state vector. If the client also has changes for the server, it sends
  them as a separate `applyStoreUpdate` call: add
  `#[method(name = "applyStoreUpdate")] async fn apply_store_update(&self, request:
  ApplyStoreUpdateRequest { store_id, client_id, update: String /* base64 */ }) ->
  EmptyResponse` which applies the update and broadcasts `StoreChangedNotification` to the
  store's other subscribers (existing notification type; add an optional
  `update: Option<String>` field carrying the raw bytes so subscribers can apply them
  without refetching; default None).
- Delete `sync_state.rs` and `ServerSyncManager` entirely.
- Test in `tests/store_sync.rs`: fresh client with empty state vector gets a diff that
  loads into a `StoreDocument` whose tree equals the server's; `apply_store_update` with a
  diff that adds a node makes `get_children` on the server include it.

### `pimble-client`
- `sync_store_document(store_id, state_vector: &[u8]) -> Result<(Vec<u8>, Vec<u8>)>`
- `apply_store_update(store_id, update: &[u8]) -> Result<()>`

## C. App and CLI

- `pimble-app/src/backend.rs`: adapt the `SyncStoreDocument` command to the new client
  signature. The app does not hold a `StoreDocument`; if the command is unused, keep it
  wired to the new signature and drop any code that decoded Automerge sync messages. Any
  `automerge` reference in `pimble-app` disappears.
- `pimble-cli`: compile against the new client; no new commands needed.
- `cargo check -p pimble-app -p pimble-cli` clean; `cargo test -p pimble-app --release
  collab_converges` still passes.

## Done means

`cargo tree -p pimble-server -i automerge` shows automerge reachable only through
`pimble-crdt` (legacy reader) and `pimble-store` (legacy content reader); the app opens
the existing test stores, migrates them to `store.yrs`, and two windows still edit live.
