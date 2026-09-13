# Cleanup contract: no legacy, no backwards compatibility

Joe's direction (2026-09-13): "We don't want to keep anything legacy, we don't want any
backwards compatibility. Ruthlessly get rid of old stuff, replace with shiny new stuff."

The two existing stores have already been converted to the new format by running the
current build once. From here on, an old-format store does not open; the answer is to
re-import from Scrivener.

## Ownership

| Agent | Scope |
| --- | --- |
| A | `pimble-crdt`, `pimble-plugins`, `pimble-core` |
| B | `pimble-store`, `pimble-rpc`, `pimble-server`, `pimble-client` |
| C | `pimble-app`, `pimble-cli`, `pimble-import` |
| D | `docs/`, `TODO.md`, `CLAUDE.md`, `README` if any |

The PM removes `automerge` from the root `Cargo.toml` once A and B report.

## A. `pimble-crdt`, `pimble-plugins`, `pimble-core`

- Delete `legacy_store_document.rs` and `StoreDocument::from_legacy`. Remove `automerge`
  from `crates/pimble-crdt/Cargo.toml`. Remove every `Automerge` word from doc comments
  (the crate doc, `error.rs` variants such as `CrdtError::Automerge(...)` if present, and
  anything else); rename error variants to what they are now.
- `pimble-core`: remove `SyncState`/`ConflictInfo` fields or types only if nothing uses
  them (grep the workspace); remove any comment that says Automerge or `.json` node
  files; `StoreManifest` docs say version 3 is the only version.
- `pimble-plugins`: no legacy content; make sure nothing mentions `DocumentContent`.
- `cargo check -p pimble-crdt -p pimble-plugins -p pimble-core` clean; `cargo test
  -p pimble-crdt` green with the legacy test module gone.

## B. `pimble-store`, `pimble-rpc`, `pimble-server`, `pimble-client`

- `pimble-store`: delete `legacy.rs`, `LEGACY_STORE_DOC_FILE`, the legacy content path
  (`legacy_content_path`, the `.automerge` branch in `get_node_document`, the cleanup in
  `delete_node`), `migrate_v1` and everything v1, and the legacy migration test fixtures.
  `open` requires `manifest.version == 3` and `store.yrs` present; otherwise return a new
  `StoreError::UnsupportedFormat { path, version }` whose message says the store predates
  the current format and must be re-imported. Remove `automerge` from
  `crates/pimble-store/Cargo.toml`. Doc comments describe only the current layout:
  `manifest.json` (version 3), `store.yrs`, `nodes/{id}.yrs`, `assets/`, `index/`.
- `pimble-rpc`: delete any request/response/notification field that exists only for
  compatibility (`#[serde(default)]` fields added to keep old clients working, such as
  `NodeContentChangedNotification.operation` being optional if it is always present now;
  keep `source_client_id` because echo suppression needs it). Delete
  `EditOperation::ReplaceContent` if no caller sends it (grep app, cli, server tests);
  the full-snapshot path is `updateNodeContent`. Remove every Automerge mention.
- `pimble-server`: remove anything that referenced the deleted pieces; `apply_edit` is
  one arm if `ReplaceContent` goes. Tests updated.
- `pimble-client`: same; remove dead methods (grep for callers in app and cli).
- `cargo check` on the four crates clean; `cargo test -p pimble-store -p pimble-server`
  green.

## C. `pimble-app`, `pimble-cli`, `pimble-import`

- `pimble-app`: replace the deprecated `rinch::run_with_window_props_and_menu` with the
  `App::new(component).window_props(props).theme(theme).menu(menus).run()` builder
  (see rinch's CLAUDE.md "Application Entry Point" in
  ~/.cargo/git/checkouts/rinch-0c8f72f80791bb29/743f8a0/CLAUDE.md). Delete vestigial
  code: `html_cache` and `invalidate_html_cache`, the unused `BackendCommand::Connect`,
  `BackendEvent::RemoteContentChange`, `send_command`, `sync_status`, the `backend-winit`
  feature, `ConnectionState::as_str` if unused, and every other item the compiler lists as
  dead in `cargo check -p pimble-app` (16 warnings today). Zero warnings is the target.
  Remove any comment that mentions Automerge, ContentEditable/CE, Slint, Makepad or
  `DocumentContent`.
- `pimble-cli`: zero warnings; remove commands that no longer make sense.
- `pimble-import`: zero warnings; remove any mention of the old editor API.
- `cargo check -p pimble-app -p pimble-cli -p pimble-import` with zero warnings;
  `cargo test -p pimble-app --release collab_converges` and `cargo test -p pimble-import`
  green.

## D. Docs

- Delete `docs/PHASE2_IMPLEMENTATION.md`, `docs/MIGRATION_SLINT_TO_RINCH.md`, `TODO.md`
  (all pre-rinch, pre-yrs). Keep `docs/menubar-component.md` only if it still describes
  the current rinch menubar; otherwise delete it.
- `docs/ARCHITECTURE.md`: rewrite every CRDT/storage passage for the current design: yrs
  for both node content (`ContentDoc`) and the store document (`StoreDocument`),
  `store.yrs` + `nodes/{id}.yrs`, stateless sync (state vector in, diff out) for both,
  `applyEdit`/`applyStoreUpdate` relays, no `.json` node files, no Automerge. Keep the
  mount architecture section. Update the RPC trait listing to match
  `crates/pimble-rpc/src/methods.rs` exactly.
- `CLAUDE.md`: the status table and "Phase A landed" section become a "Current design"
  section describing the yrs-only design; delete the sentence about the store document
  still being Automerge; no migration mentions.
- `docs/RESTART_PLAN.md`: mark step 4 done, add a line that legacy readers were removed
  by decision on 2026-09-13.
- `docs/PHASE_A_CONTRACT.md` and `docs/PHASE_B_CONTRACT.md` are historical; move them to
  `docs/history/`.
