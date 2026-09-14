# Next session: start here

Written 2026-09-14 at the end of the mounts session. Read this, then `CLAUDE.md`, then
`docs/RESTART_PLAN.md`.

## Where master stands

- Branch `master`, working tree clean after the mounts commits (see `git log`).
- Both CRDT documents are yrs (`ContentDoc` for node content, `StoreDocument` for the
  tree). No Automerge, no migrations, no backwards compatibility, by decision.
- rinch is tracked from GitHub `main`, pinned in `Cargo.lock` at `743f8a0`.
- Search (restart plan step 5) is done: rhypedb index per store, keyword plus semantic
  (ONNX, `onnx-download`), search-as-you-type.
- **Local mounts are done (roadmap step 6, first item, 2026-09-14).** Contract and the
  decisions behind it: `docs/history/MOUNTS_CONTRACT.md`; architecture:
  `docs/ARCHITECTURE.md` "Mount Architecture"; summary in `CLAUDE.md`.
- Workspace compiles with zero warnings. All tests pass in release (41 in
  `pimble-server` alone, 8 of them in `tests/mounts.rs`).

## Verify in five minutes

```bash
cargo check --workspace --all-targets      # expect zero warnings
cargo test --workspace --release           # expect all green (one known flake, below)
cargo run -p pimble-app --release          # opens the stores listed in ~/.config/pimble/state.json
```

Headless mount check without the GUI:

```bash
cargo run -p pimble-cli -- server &        # 127.0.0.1:7462
cargo run -p pimble-cli -- create-store /tmp/a.pimble "A"      # note store id + root id
cargo run -p pimble-cli -- create-store /tmp/b.pimble "B"
cargo run -p pimble-cli -- create-node <B> <B-root> document "B Doc"
cargo run -p pimble-cli -- create-mount <A> <A-root> <B> <B-root> "B Mount"   # prints source path
cargo run -p pimble-cli -- list-children <A> <mount-id>        # B Doc, addressed in store B
kill -INT %1; cargo run -p pimble-cli -- server &              # restart, open A only
cargo run -p pimble-cli -- open-store /tmp/a.pimble
cargo run -p pimble-cli -- list-children <A> <mount-id>        # still B Doc; list-stores now shows B
cargo run -p pimble-cli -- mount-state <A> <mount-id>          # Live
```

GUI: right-click a node or store root, "Copy as Mount Source"; right-click the target,
"Paste Mount Here". "Mount Store..." (folder picker) still mounts a store that is not
open. The app has rinch's `debug` feature on; the rinch MCP tools (`list_apps`, `connect`,
`screenshot`, `click`, `right_click`, `type_text`) drive a running instance. A second
instance joins the first one's server on 7462, which is how two-window checks are done.
Blender's MCP add-on listens on 9876; Pimble deliberately uses 7462.

## What was verified in the GUI on 2026-09-14

Two windows on the family store (674 nodes) and "another store": MEDICAL mounted into
"another store" via copy/paste; the mount expands (children addressed in the family
store); nested folders expand through it; a document opened through the mount edits live
in both directions against a window that has it open directly; a node created directly in
the family store appears under the mount in the other window; the app restarted with the
family store removed from `state.json` still resolves the mount (through
`MountRef.source_path`), discovers the family store on expansion, and writes it back to
the open-store list.

## How the last sessions worked (keep doing this)

- The PM (Claude) writes a contract file first (`docs/history/*_CONTRACT.md` are the
  examples), lands any cross-crate interface change itself, splits work by crate so
  agents never edit the same files, and reviews every report against the code before
  committing.
- Two or three Sonnet agents in parallel; a fourth concurrent release build pushed memory
  low enough that background waits were killed.
- Agents stall when the account hits its usage limit and do not resume on their own. If
  an agent is idle with no final report, check its diff and either message it once to
  resume or finish the work yourself. Twice in one session means finish it yourself.
- Review found real bugs the agents' own tests did not: this time, a path hint that
  could open the wrong store, a mount-parent check that loaded content to read a type,
  `createNode` never flushing, a menu item inside a reactive block that never closed its
  menu, and a remote-change refresh that missed deep folders. Review the diff, then run
  the GUI.
- Never touch `/home/joe/dev/rinch`. Read rinch source from
  `~/.cargo/git/checkouts/rinch-*/<rev>/` or a scratch clone. rinch bugs go up as
  issues or PRs (latest: joeleaver/rinch#714).
- `pkill -f <pattern>` matches the shell running it if the pattern appears in the
  command line; use a `[t]arget`-style pattern.

## Smaller follow-ups, in rough priority

1. rinch #714: a `DropdownMenuItem` rendered inside a reactive block never closes its
   `ContextMenu`, and `ContextMenu` portals leak on unmount (48 portals after one tree
   rebuild of 24 rows). Pimble works around the first by never putting a menu item in a
   reactive block ("Paste Mount Here" is always rendered, `disabled` while nothing is
   copied, and copy/paste bump the tree so menus re-render). The leak is unaddressed.
2. Flaky test: `pimble-server tests/search.rs reopening_a_store_preserves_its_search_index`
   fails in a full `cargo test --workspace --release` run and passes alone (3 of 3).
   Timing of the close/reopen of the rhypedb index under load; not investigated.
3. Remote structural changes (`NodeCreated`/`Deleted`/`Moved`) name only the node, so the
   app refetches every loaded children list of that store plus every mount sourced from
   it. Exact refresh needs the parent id in `StoreChangedNotification`.
4. The app discovers an implicitly opened source store only from `ChildrenLoaded`
   (expanding the mount). A `Live` mount state for an unknown store does not trigger
   `listStores` yet; the contract allowed either.
5. `ContentDoc::text()` re-projects the whole document on every call; tree labels call it
   per node. Cache the projected text per node in `LocalStore` and invalidate on update.
6. The importer flattens RTF formatting (bold, italic, links) to plain paragraphs. Add a
   `ContentDoc` constructor that takes marked-up runs once the editor's collab scope
   accepts them.
7. `pimble-client` still exposes `sync_node_content`, `sync_store_document`,
   `apply_store_update` and `get_nodes`; the app calls none of them. Wire them when
   reconnect handling is built, or delete them then.
8. Concurrent moves of the same node in `StoreDocument` can leave a duplicate child
   entry; `validate_tree` detects it, nothing repairs it yet.
9. The rinch `collaboration` scope rejects lists, block quotes and tables in a
   collaborating document (fails loud). The editor toolbar still shows list buttons.
10. Cross-store drag-and-drop (moving a node into another store, or onto a mount node)
    is ignored with a warning. A real cross-store move is a copy plus delete and needs a
    decision about identity.
11. `pimble-cli server` handles SIGINT only; SIGTERM kills it without a flush.
12. The old `nodes/*.automerge` and `store.automerge` files still sit in the two
    converted stores. Inert; delete when convenient.

## Then the roadmap resumes

Remote sync over the same state-vector/diff primitive (`syncNodeContent`,
`syncStoreDocument`, `applyStoreUpdate` already exist and are stateless), then remote
mounts (`StoreEndpoint::Remote`, `MountState::Cached`/`Connecting`), links UI and
backlinks (rhypedb relationships), plugins. See `docs/RESTART_PLAN.md` §6 and
`docs/ARCHITECTURE.md`.
