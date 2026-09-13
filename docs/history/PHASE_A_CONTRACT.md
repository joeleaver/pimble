# Phase A contract: node content on yrs

This is the interface every crate codes against during Phase A (see
`docs/RESTART_PLAN.md` §4). Signatures here are fixed. If an implementer must deviate,
they report it instead of silently changing the shape.

## Ownership (parallel work, no overlap)

| Agent | Crates / files | Depends on |
| --- | --- | --- |
| A | `pimble-crdt` (new `content_doc.rs`, delete `node_content.rs`, retire `document.rs` if unused), `pimble-plugins` fix-ups | nothing |
| B | `pimble-store`, `pimble-rpc`, `pimble-server`, `pimble-client` | A's `ContentDoc` |
| C | `pimble-app` | A's `ContentDoc`, B's RPC/client shape |

Nobody edits `Cargo.toml` at the workspace root; the dependencies are already added:
`yrs = "0.27"`, `rinch-editor-collab` (git main), `rinch-editor-core` (git main).
`pimble-crdt` and `pimble-store` manifests already carry what they need.

## A. `pimble_crdt::ContentDoc`

```rust
// crates/pimble-crdt/src/content_doc.rs
pub struct ContentDoc { /* yrs::Doc built with OffsetKind::Utf16 */ }

impl ContentDoc {
    /// Empty document.
    pub fn new() -> Self;
    /// `bytes` is a yrs v1 update (a full snapshot or any update). Empty bytes -> `new()`.
    pub fn load(bytes: &[u8]) -> Result<Self>;
    /// Build a document whose content is one paragraph per line of `text`
    /// (blank lines become empty paragraphs). Used to migrate legacy content.
    /// Implementation: rinch_editor_core Schema::starter_kit + EditorState::create,
    /// then rinch_editor_collab::CollabSession::new(&state).snapshot() -> load.
    pub fn from_plain_text(text: &str) -> Result<Self>;
    /// Full snapshot: v1 update encoding of the whole doc from an empty state vector.
    pub fn save(&self) -> Vec<u8>;
    /// Merge a peer's v1 update (delta, reconciliation diff, or whole snapshot).
    pub fn apply_update(&mut self, update: &[u8]) -> Result<()>;
    /// v1-encoded state vector.
    pub fn state_vector(&self) -> Vec<u8>;
    /// Everything this doc has that a peer at `state_vector` (v1-encoded) lacks.
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>>;
    /// Plain-text projection: blocks joined by '\n'. "" if the doc is empty or
    /// cannot be projected (log at debug, never panic).
    /// Implementation: CollabSession::from_bytes(&self.save())?.projected_doc(&Schema::starter_kit())
    /// then walk: text nodes contribute `text()`, top-level children are joined by '\n'.
    pub fn text(&self) -> String;
    /// Convenience for callers holding bytes: `Self::load(bytes).map(|d| d.text()).unwrap_or_default()`.
    pub fn text_of(bytes: &[u8]) -> String;
}
impl Default for ContentDoc { fn default() -> Self { Self::new() } }
```

`CrdtError` gains `Yrs(String)` and `Collab(String)` variants (`error.rs`). `lib.rs`
exports `content_doc::*`, keeps `store_document::*` and `error::*`, and drops
`node_content` (deleted). `document.rs` (`CrdtDocument`) is deleted unless
`StoreDocument` still needs it.

Tests in `content_doc.rs`: round trip `from_plain_text("a\nb") -> save -> load -> text() == "a\nb"`;
two docs converge after exchanging `diff_since` in both directions; `load(&[])` is empty;
`text_of(b"garbage")` is `""`.

## B. Store, RPC, server, client

### `pimble-store`
- `LocalStore.content_docs: HashMap<NodeId, ContentDoc>`. Content file is `nodes/{id}.yrs`
  holding `ContentDoc::save()`.
- `get_node_document(node_id) -> Result<&mut ContentDoc>`; loads `.yrs` if present. If
  `.yrs` is absent and `nodes/{id}.automerge` exists, migrate: extract legacy text
  best-effort from the Automerge doc (new private module `legacy.rs`: top-level `text`
  Text/Str value if present, else every Str/Text scalar in document order joined by
  `\n`), build `ContentDoc::from_plain_text`, write the `.yrs`, leave the `.automerge`
  file untouched. Log at info.
- `update_node_content(node_id, bytes)` -> `ContentDoc::load(&bytes)` replaces the doc.
- `apply_content_update(node_id, update: &[u8]) -> Result<()>` = get doc, `apply_update`,
  mark dirty.
- `set_node_text` and anything that reads `DocumentContent` is deleted. `save_node_document`
  writes the `.yrs`.
- `StoreManager` mirrors: `get_node_document`, `update_node_content`, `apply_content_update`,
  `mark_content_dirty`; delete `set_node_text`.
- Tests (tempfile): create store + document node, `update_node_content(from_plain_text("hi"))`,
  `apply_content_update` with a diff from a second `ContentDoc`, `flush`, reopen store,
  `get_node(...).content` loads to a doc whose `text()` is the merged text. Legacy test:
  write an Automerge doc with a top-level `text` Text object under `nodes/{id}.automerge`,
  open, `get_node_document`, `text()` equals it and `nodes/{id}.yrs` now exists.

### `pimble-rpc`
- Delete `setNodeText` (`SetNodeTextRequest`) from `methods.rs` and `types.rs`.
- `EditOperation::IncrementalChanges { changes }`: base64 yrs v1 update.
  `EditOperation::ReplaceContent { content }`: base64 yrs full snapshot. Update doc comments.
- `SyncNodeContentRequest { store_id, node_id, state_vector: String /* base64 v1 */ }`
  `SyncNodeContentResponse { diff: String /* base64 v1 update, may be empty */, state_vector: String /* server's, base64 */ }`
- `UpdateNodeContentRequest.content` stays base64 and is a yrs snapshot.

### `pimble-server`
- `apply_edit`: `IncrementalChanges` -> decode -> `store_manager.apply_content_update`;
  `ReplaceContent` -> decode -> `update_node_content`. Then broadcast the same operation
  to the node's subscribers except the source client (unchanged shape).
- `sync_node_content`: decode state vector, `diff = doc.diff_since(sv)`, respond with
  `diff` and the server's `state_vector()`. No per-client state.
- `sync_state.rs`: remove the node-content states; keep store-document states.
- Delete `set_node_text`.
- Test in `handler.rs` or a new `tests/content_sync.rs`: two `ContentDoc`s standing in for
  two clients; client A `from_plain_text("Hello")` -> `update_node_content`; A edits by
  applying an update built from a third doc? Keep it simple: A snapshot -> server; B calls
  `sync_node_content` with an empty state vector and receives a diff whose `text()` is
  "Hello"; `apply_edit(IncrementalChanges)` with a diff from a doc that has more text
  results in the server doc's `text()` containing it.

### `pimble-client`
- Delete `set_node_text`. `sync_node_content(store_id, node_id, state_vector: &[u8]) ->
  Result<(Vec<u8> /* diff */, Vec<u8> /* server sv */)>`. `set_node_content_bytes` and
  `apply_edit` keep their signatures.

## C. `pimble-app`

- `state.rs`: `get_node_content_text(bytes)` -> `pimble_crdt::ContentDoc::text_of(bytes)`.
  Remove `DocumentContent` imports.
- `editor.rs` `start_editing`: if `content_bytes` is empty -> `load_html("")` and host;
  else `start_collaboration_guest(content_bytes, outbound)`; on `Err` log a warning with
  the error, `load_html("")` and host (the server persists the host snapshot through
  `SetNodeContent` exactly as today). Delete the legacy plain-text/`html_escape` branch.
- `backend.rs` / `events.rs`: remove the `SetNodeText` command and any use of the old
  `sync_node_content`; if a `SyncNodeContent` command exists, adapt it to the new client
  signature and, on the event side, feed the diff to `apply_remote` when non-empty.
- `main.rs` test `collab_converges_through_a_server_relay`: replace the Automerge
  `server_doc` with `pimble_crdt::ContentDoc::load(&snapshot)`, apply each relayed delta
  with `apply_update`, and at the end assert `server_doc.text() == doc_text(&a)`.
- `cargo check -p pimble-app` clean of errors; `cargo test -p pimble-app --release
  collab_converges` passes.

## Wire summary

| RPC | Request bytes | Response bytes |
| --- | --- | --- |
| `updateNodeContent` | full yrs snapshot (base64) | none |
| `applyEdit` `incremental_changes` | yrs v1 update (base64) | none; relayed verbatim to peers |
| `applyEdit` `replace_content` | full snapshot (base64) | none; relayed |
| `syncNodeContent` | client state vector (base64) | server diff + server state vector (base64) |
| `getNode` / `getChildren` | | `Node.content` = full yrs snapshot |
