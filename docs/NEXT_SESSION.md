# Next session: start here

Written 2026-09-14 at the end of the hardening session. Read this, then `CLAUDE.md`, then
`docs/RESTART_PLAN.md`.

## Where master stands

- Branch `master`, working tree clean (see `git log`).
- Both CRDT documents are yrs. No Automerge, no migrations, no backwards compatibility.
- rinch is tracked from GitHub `main`, pinned in `Cargo.lock` at `743f8a0` (main is about
  45 commits ahead, mostly rinch-dom style fixes; nothing Pimble needs yet).
- Done since the restart: search on rhypedb (keyword + semantic), local mounts, replica sync
  between Pimble servers, app polish (dark editor theme, failover to our own embedded
  server when a borrowed one quits, toolbar states, documents open from the cache and
  reconcile with the server), and **hardening (2026-09-14)**: auth at the HTTP edge,
  credentials out of store directories, `removeReplica`, batch content reconcile, no-op
  updates, tree repair after concurrent moves, subtree delete, exact tree notifications,
  deterministic index close. Contracts and the decisions behind them are in
  `docs/history/`; summaries in `CLAUDE.md`; architecture in `docs/ARCHITECTURE.md`
  ("Mount Architecture", "Replica sync", "Auth").
- Workspace compiles with zero warnings. `cargo test --workspace --release` passes: 166
  tests, three full runs in a row, the old search flake included.

## Verify in five minutes

```bash
cargo check --workspace --all-targets      # expect zero warnings
cargo test --workspace --release --no-fail-fast
cargo build -p pimble-app -p pimble-cli --release   # build together: see follow-up 9
```

Headless replica sync with auth (two servers on one machine):

```bash
B=target/release/pimble-cli
$B server --addr 127.0.0.1:7463 --token-file /tmp/t &            # the "remote", token required
export PIMBLE_SERVER=http://127.0.0.1:7463 PIMBLE_TOKEN=$(cat /tmp/t)
$B create-store /tmp/b.pimble "B"                                  # note ids
$B create-node <B> <B-root> document "Doc"; $B set-node-text <B> <doc> "hello"
curl -s -o /dev/null -w "%{http_code}\n" -H 'Origin: http://x' http://127.0.0.1:7463   # 403
unset PIMBLE_SERVER PIMBLE_TOKEN
$B server &                                                        # the "local", 7462, no token
$B remote-stores http://127.0.0.1:7463 --token $(cat /tmp/t)
$B add-remote-store http://127.0.0.1:7463 <B> --token $(cat /tmp/t)   # token saved in credentials.json
$B show-node <B> <doc>; $B sync-state <B>                          # hello; Synced
$B remove-replica <B>
```

To test the app without touching your real stores, run it with `XDG_CONFIG_HOME` and
`XDG_DATA_HOME` pointing at temp directories (symlink `~/.local/share/pimble/models` into
the temp data dir to skip the model download).

GUI: File > "Add Remote Store..." (URL, masked token, "List stores", pick, "Add"). Store
root context menu: "Link to Remote...", "Unlink from Remote", "Remove Replica...". Nothing
opens a native dialog, so the rinch MCP tools drive all of it.

## What was verified on 2026-09-14

Against a token-protected `pimble-cli server` on 7463, with the app isolated in temp config
and data directories: Origin 403, no token 401, token 200; "List stores" without a token
showed "refused the credentials", with it listed the store; "Add" created the replica
(synced badge), `sync.json` has `auth: none`, `credentials.json` holds the token at 0600;
the store row menu showed Unlink and Remove Replica enabled (stale before the fix below);
a move on the remote refetched exactly the two affected lists in the app (log lines);
SIGTERM stopped the remote with a flush; the reconnect after restarting it sent zero
`applyEdit`/`applyStoreUpdate` to the remote; restarting the app reached Synced from the
saved credential alone; Remove Replica deleted the directory, emptied the saved open-store
list, and left the store on the remote.

## How the sessions work (keep doing this)

- The PM writes a contract first (`docs/*_CONTRACT.md`, moved to `docs/history/` when done),
  lands the cross-crate interface itself and commits it, splits the work so no two agents
  own the same function, and reviews every diff before committing. This phase ran three
  Sonnet agents (edge: auth/credentials/CLI; data: crdt/store/repair/index; app).
- Review found what the agents' tests did not: an empty token accepted as valid; secrets
  written world-readable for an instant; a subtree delete that could loop forever with no
  await point (hanging the runtime), fail halfway, or delete the root through a malformed
  list; a `JoinSet` growing one entry per keystroke. Driving the real app found a stale
  context menu no test covered. Do the GUI pass yourself, against a second server.
- Agents' idle notices often arrive after the report and repeat it; check the code, not the
  notice. A message to an agent is seen at its next turn; if a change you asked for is not in
  the tree after the agent goes idle twice, make it yourself.
- `pkill -f`/`pgrep -f` match the shell running them; use a `[p]attern`.
- Never touch `/home/joe/dev/rinch`. rinch bugs go up as issues (latest: #714).

## Follow-ups, in rough priority

1. TLS (`wss://`). Tokens go over plain `ws://`; fine on a trusted LAN, not beyond it.
2. The app's embedded server trusts every local process (no token on loopback). Fine for a
   single-user machine; a multi-user one would want the app to use the server token file.
3. `getChildren` returns every child's full content bytes, and tree labels decode a yrs
   snapshot per node (`ContentDoc::text_of`). A label/preview field from the server, or a
   per-node cache, before big stores make it noticeable.
4. Every `applyEdit` writes `modified_at` into `store.yrs` (one store-document change per
   keystroke, flushed on the debounce and replicated on reconcile). Coalesce it.
5. A content update yrs can only stash as pending (its dependencies have not arrived) reports
   "unchanged", so the server neither relays nor flushes it until the missing update lands.
   Ordered connections make this rare; a reconcile closes the gap.
6. Opening a document always sends the editor session's diff back (the app cannot see the
   session's delete set) and re-projects the view with `collab_receive` even when nothing
   changed. The server treats the push as a no-op; the round trip and re-projection remain.
7. rinch #714: a `DropdownMenuItem` inside a reactive block never closes its `ContextMenu`;
   portals leak on unmount. Worked around (items always rendered, `disabled` at render time,
   and the row's `TreeNodeData` must change for a re-render).
8. rinch repaint artifacts seen in the GUI pass: a strip left below a modal after it shrinks,
   and a stray one-pixel line across the tree after a modal closes. Not isolated; reproduce
   and file.
9. `pimble-cli` built on its own compiles `pimble-search` without `semantic`, so the same
   store's index schema flips between that binary and one built with the app, and each open
   rebuilds the index (the fallback handles it; it wastes the rebuild). Enable `semantic` in
   the CLI or make the schema tolerate a missing `Chunk` type.
10. The importer flattens RTF formatting to plain paragraphs.
11. The collaboration scope rejects lists, block quotes and tables; their toolbar buttons are
    hidden until rinch supports them.
12. Cross-store drag-and-drop is ignored with a warning.
13. `pimble-client::get_nodes` (and the `getNodes` RPC) are unused.
14. The old `nodes/*.automerge` and `store.automerge` files still sit in the two converted
    stores. Inert.

## Then the roadmap: remote mounts

A mount whose source store is held by another server. The shape that falls out of what
exists: the source becomes a replica on this server and the mount resolves locally.

- `MountRef` gains `source_remote: Option<Url>` (a URL only; credentials stay in
  `credentials.json`, since a mount ref is replicated with its store). `createMount` fills it
  when the source store is a linked replica.
- Resolution order: open; registry; `source_path`; `source_remote`, then the mounting
  store's own remote if it is a linked replica (a store and the stores it mounts often live
  on the same server). The last two create a replica in the background.
- States: `Connecting` while that replica is created or first reconciles; `Live` when the
  source is open and (if a replica) synced; `Cached { last_sync }` when its link is offline
  (persist the last synced time in `sync.json`); `Unavailable` otherwise.
- The server remembers which mounts it has resolved per source store and sends
  `MountStateChanged { node_id, state }` on the mounting store when a source's state
  changes, so the app never polls.
- App: "Mount Remote Store Here..." (the Add Remote Store modal plus a target node);
  "Copy as Mount Source" inside a replica already works.
- Out of scope for the first cut: replicating only the mounted subtree's content.

After that: links UI and backlinks (rhypedb relationships), plugins. See
`docs/RESTART_PLAN.md` §6 and `docs/ARCHITECTURE.md`.
