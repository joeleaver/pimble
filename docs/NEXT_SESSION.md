# Next session: start here

Written 2026-09-15 at the end of the sync session. Read this, then `CLAUDE.md`, then
`docs/RESTART_PLAN.md`.

## Where master stands

- Branch `master`, working tree clean (see `git log`).
- Both CRDT documents are yrs. No Automerge, no migrations, no backwards compatibility.
- rinch is tracked from GitHub `main`, pinned in `Cargo.lock` at `743f8a0`.
- Done since the restart: search on rhypedb (keyword + semantic), **local mounts
  (2026-09-14)**, **replica sync between Pimble servers (2026-09-15)**. Contracts and the
  decisions behind them: `docs/history/MOUNTS_CONTRACT.md`,
  `docs/history/SYNC_CONTRACT.md`; summaries in `CLAUDE.md`; architecture in
  `docs/ARCHITECTURE.md` ("Mount Architecture", "Replica sync").
- Workspace compiles with zero warnings. All tests pass in release except one known flake
  (below). `pimble-server` alone has 50 tests: 8 mounts, 9 sync (two real servers on
  port 0), 5 search, and the content/store sync tests.

## Verify in five minutes

```bash
cargo check --workspace --all-targets      # expect zero warnings
cargo test --workspace --release --no-fail-fast
cargo run -p pimble-app --release          # opens the stores listed in ~/.config/pimble/state.json
```

Headless replica sync (two servers on one machine):

```bash
cargo build -p pimble-cli --release
B=target/release/pimble-cli
$B server --addr 127.0.0.1:7463 &                               # the "remote"
PIMBLE_SERVER=http://127.0.0.1:7463 $B create-store /tmp/b.pimble "B"      # note ids
PIMBLE_SERVER=http://127.0.0.1:7463 $B create-node <B> <B-root> document "Doc"
PIMBLE_SERVER=http://127.0.0.1:7463 $B set-node-text <B> <doc> "hello"
$B server &                                                     # the "local", 7462
$B remote-stores http://127.0.0.1:7463
$B add-remote-store http://127.0.0.1:7463 <B>                   # replica under ~/.local/share/pimble/replicas/
$B show-node <B> <doc>                                          # hello
$B sync-state <B>                                               # Synced
```

GUI: File > "Add Remote Store...", type the URL, "List stores", pick, "Add". The store row
gets an "offline / syncing / synced" badge. Right-click a store root for "Link to
Remote..." / "Unlink from Remote". Nothing in that flow opens a native dialog, so the rinch
MCP tools (`connect`, `screenshot`, `click`, `right_click`, `type_text`, `key_press`) drive
it end to end; a second app instance joins the first one's server on 7462.

## What was verified in the GUI on 2026-09-15

Against a headless `pimble-cli server --addr 127.0.0.1:7463` holding a test store: added it
from the app (replica created under the data directory, Synced within a second, modal
closed, badge "synced"); typed in the app and saw it on the remote over the CLI; created a
node on the remote and saw it in the app's tree; stopped the remote (badge "offline"),
typed while it was down, restarted the remote with `--open`, link reconnected after its
backoff, remote had the offline edit; "Unlink from Remote" removed the badge and
"Link to Remote..." brought it back with `sync.json` rewritten.

## How the last sessions worked (keep doing this)

- The PM writes a contract first (`docs/history/*_CONTRACT.md`), lands the cross-crate
  interface itself, splits work by crate, and reviews every report against the diff before
  committing. Two Sonnet agents (server side, app side) per phase.
- Decisions Joe made mid-phase this time: "clone" became "Add Remote Store", and the user
  never chooses where a replica lives ("It wasn't at all clear from the interface that you
  were picking a location for the remote store to live locally"). Rename the interface in
  `pimble-rpc` and the contract yourself, then message each agent with what changed in its
  crate; agents do not see messages until their current turn ends, and one of them
  reported the old state before applying the change, so re-check the code, not the report.
- Review again found what the agents' tests did not: adding a replica for a store already
  open on the same server silently replaced the open original in the manager's map (Agent
  B's GUI test did exactly that against the app's own server); a lagged broadcast was
  logged and ignored; the search index was opened on an empty replica and failed its
  first walk. Run the GUI against a real second server, not the app's own.
- Agents stall on usage limits without reporting. If one is idle with no final report,
  check its diff and message it once; twice means finish it yourself.
- `pkill -f <pattern>` matches the shell running it; use a `[p]attern`.
- Never touch `/home/joe/dev/rinch`. rinch bugs go up as issues (latest: #714).

## Smaller follow-ups, in rough priority

1. Auth is plumbed, not enforced: `connect_with_auth` sends `Authorization: Bearer` or
   `X-Api-Key`; the server checks nothing. Any server on the LAN accepts any client. Add a
   token check in `PimbleServer` (jsonrpsee middleware) before exposing a server beyond
   loopback, and TLS (`wss://`) for anything beyond the LAN.
2. A full reconcile is one `syncNodeContent` round trip per node (674 on the family
   store) on every reconnect. Add a batch RPC (state vectors for many nodes in one call)
   when that becomes noticeable.
3. The sync link only reconciles from its remote; a store linked A→B and also B→A works
   (the sync-link source rule stops echoes) but is redundant. There is no server identity,
   so "remote is this server" is detected by comparing the twin's local path; two servers
   serving the same directory would be refused for the same reason, which is right.
4. Deleting a replica: unlink stops the link and removes `sync.json`, but the replica
   directory stays under `~/.local/share/pimble/replicas/` and in the open-store list.
   "Remove replica" (close, unlink, delete the directory) does not exist yet.
5. rinch #714: a `DropdownMenuItem` inside a reactive block never closes its `ContextMenu`;
   portals leak on unmount. Worked around (menu items always rendered, `disabled` set at
   render time, tree bumped on transitions).
6. Flaky test: `pimble-server tests/search.rs reopening_a_store_preserves_its_search_index`
   fails in most full `cargo test --workspace --release` runs and passes alone or in a
   two-test run. Timing of the index close/reopen under load; not investigated.
7. Remote structural changes name only the node, so the app refetches every loaded
   children list of that store plus every mount sourced from it. The parent id in
   `StoreChangedNotification` would make it exact.
8. The app discovers an implicitly opened mount source only from `ChildrenLoaded`; a
   `Live` mount state for an unknown store does not trigger `listStores`.
9. `ContentDoc::text()` re-projects the whole document on every call; tree labels call it
   per node. Cache per node in `LocalStore`.
10. The importer flattens RTF formatting to plain paragraphs.
11. `pimble-client` still exposes `sync_node_content`, `sync_store_document`,
    `apply_store_update`, `get_nodes`; the sync link now uses the first three, the app
    none of them.
12. Concurrent moves of the same node in `StoreDocument` can leave a duplicate child
    entry; `validate_tree` detects it, nothing repairs it.
13. The rinch `collaboration` scope rejects lists, block quotes and tables; the toolbar
    still shows list buttons.
14. Cross-store drag-and-drop is ignored with a warning.
15. `pimble-cli server` handles SIGINT only; SIGTERM kills it without a flush.
16. The old `nodes/*.automerge` and `store.automerge` files still sit in the two converted
    stores. Inert.

## Then the roadmap resumes

Remote mounts (a mount whose source is a store held by another server: `StoreEndpoint::
Remote`, `MountState::Cached`/`Connecting`; the natural shape is "add the source store as
a replica, then mount locally"), links UI and backlinks (rhypedb relationships), plugins.
See `docs/RESTART_PLAN.md` §6 and `docs/ARCHITECTURE.md`.
