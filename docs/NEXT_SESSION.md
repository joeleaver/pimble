# Next session: start here

Written 2026-09-15 at the end of the remote mounts session. Read this, then `CLAUDE.md`,
then `docs/RESTART_PLAN.md`.

## Where master stands

- Branch `master`, working tree clean (see `git log`).
- Both CRDT documents are yrs. No Automerge, no migrations, no backwards compatibility.
- rinch is tracked from GitHub `main`, pinned in `Cargo.lock` at `743f8a0`.
- Done since the restart: search on rhypedb (keyword + semantic), local mounts, replica sync
  between Pimble servers, app polish, hardening (auth, credentials, replica removal, tree
  repair, no-op sync), and **remote mounts (2026-09-15)**: a mount whose source store is
  on another server makes the source a replica here and resolves locally; mount states
  `Live` / `Connecting` / `Cached { last_sync }` / `Unavailable { reason }` derived from
  the source's sync link and pushed to the app as `MountStateChanged`; "Mount Remote
  Store Here..." in the app; `mount-remote-store` in the CLI. Three fixes found on the way:
  a store opened implicitly now starts its sync link, a link forwards every change except
  its own so edits travel a whole chain of servers, and `set-node-text` is a real CRDT edit.
  Contracts and the decisions behind them are in `docs/history/`; summaries in
  `CLAUDE.md`; architecture in `docs/ARCHITECTURE.md` ("Mount Architecture", "Replica
  sync", "Auth").
- Workspace compiles with zero warnings. `cargo test --workspace --release` passes: 180
  tests (see the commit message for the exact run).

## Verify in five minutes

```bash
cargo check --workspace --all-targets      # expect zero warnings
cargo test --workspace --release --no-fail-fast
cargo build -p pimble-app -p pimble-cli --release   # build together: see follow-up 8
```

Headless remote mount, three servers on one machine (the binary is `target/release/pimble`
for the app, `pimble-cli` for the CLI). Give every server its own `XDG_CONFIG_HOME` and
`XDG_DATA_HOME` so replicas and credentials stay out of the real ones:

```bash
B=target/release/pimble-cli
XDG_CONFIG_HOME=/tmp/R/c XDG_DATA_HOME=/tmp/R/d $B server --addr 127.0.0.1:7463 --token-file /tmp/t &
r() { PIMBLE_SERVER=http://127.0.0.1:7463 PIMBLE_TOKEN=$(cat /tmp/t) $B "$@"; }
r create-store /tmp/b.pimble B; r create-node <B> <B-root> document Doc; r set-node-text <B> <doc> hello
XDG_CONFIG_HOME=/tmp/L/c XDG_DATA_HOME=/tmp/L/d $B server --addr 127.0.0.1:7464 &
l() { PIMBLE_SERVER=http://127.0.0.1:7464 PIMBLE_TOKEN= $B "$@"; }
l create-store /tmp/a.pimble A
l mount-remote-store <A> <A-root> http://127.0.0.1:7463 <B> --token $(cat /tmp/t)   # replica + mount
l mount-state <A> <mount>            # Live; Source remote: http://127.0.0.1:7463/
l list-children <A> <mount>          # B's doc, addressed by B
kill %1; sleep 3; l mount-state <A> <mount>    # Cached (last synced ...); show-node still answers
```

Two servers on one filesystem resolve a mount through `source_path` before any remote, so
to exercise the remote path headlessly either move the resolving server's data directory
(the hint goes dead) or drop the hint as `tests/remote_mounts.rs` does.

GUI: store root or node context menu > "Mount Remote Store Here..." (URL, masked token,
"List stores", pick, "Mount"). Kill the remote: the mount reads "(offline copy)" and its
documents still open; restart it: back to normal within the link's backoff. Restart the
app with only the local store in `state.json`: the mount resolves the replica by itself
and the replica relinks. Test the app with `XDG_CONFIG_HOME`/`XDG_DATA_HOME` pointing at
temp directories (symlink `~/.local/share/pimble/models` into the temp data dir).

## What was verified on 2026-09-15

`tests/remote_mounts.rs` (ten tests over two or three real servers): `source_remote`
recorded; resolution through `source_remote` and through the mounting store's own remote
with `Connecting` then `Live` and a `MountStateChanged` notification; `Cached` with
children still answering and back to `Live` after relinking; `Unavailable` with the URL in
the reason and a retry on the next resolution; `last_sync` persisted across a restart;
saved credentials used, missing ones reported as "refused the credentials"; two mounts of
one source producing one replica; a deleted mount no longer notified; an implicitly
reopened replica starting its link. `tests/sync.rs` 6b: an edit at the far end of a
three-server chain reaches the origin exactly once.

Headless, three servers (R token-protected with B; L mounting B from R into a local A; M
adding A from L with L's data directory moved so the path hint is dead): M resolved the
mount `Connecting` then `Live`; because M had no credential for R it fell back to A's own
remote, L, and replicated B from L (a chain R → L → M); killing R made L's mount `Cached`
while M stayed `Live` through L; edits propagated R → L → M and M → L → R after the
forwarding fix; `credentials.json` 0600, `sync.json` with `auth: none` and `last_sync`.

GUI against a token-protected `pimble-cli server` on 7463, app isolated in temp dirs:
"Mount Remote Store Here..." listed the remote's store and mounted it in one action (modal
closed, replica appeared synced, mount expanded to the source's children); a document
opened through the mount and a `set-node-text` on the remote appeared live in the editor;
killing the remote gave "(offline copy)" and an "offline" badge with the document still
open; restarting it cleared both within 2 s; restarting the app with only the local store
saved resolved the mount from the replica on disk, discovered the store, and relinked it
(this last step is what found the implicit-open link gap). "Paste Mount Here" enables
after "Copy as Mount Source" again.

## How the sessions work (keep doing this)

- The PM writes a contract first (`docs/*_CONTRACT.md`, moved to `docs/history/` when done),
  lands the cross-crate interface itself and commits it, splits the work so no two agents
  own the same function, and reviews every diff before committing. This phase ran two Opus
  agents (server + CLI + docs; app).
- What review and the PM's own passes found that the agents' tests did not: the replica a
  mount resolves implicitly never started its link (the GUI restart step); a chain of
  servers dropped edits at the middle hop (the old "never forward a sync-link-sourced
  change" rule); `set-node-text` replaced the document instead of editing it, so a replica
  showed both texts; "Paste Mount Here" never enabled (row data must change for rinch to
  re-render a row, and the re-render must be deferred past the menu close or rinch #714
  orphans the menu). Do the headless chain and the GUI restart yourself.
- Agents' idle notices often arrive after the report and repeat it; check the code, not the
  notice. An agent's broad `pkill -f pimble` will kill your servers too: tell them to match
  their own ports.
- `pkill -f`/`pgrep -f` match the shell running them; use a `[p]attern`.
- Never touch `/home/joe/dev/rinch`. rinch bugs go up as issues (latest: #714).

## Follow-ups, in rough priority

1. TLS (`wss://`). Tokens go over plain `ws://`; fine on a trusted LAN, not beyond it.
2. Two servers on one machine opening the same store directory (a mount resolved through
   `source_path` while another server holds the source) share `nodes/*.yrs` and fight over
   the rhypedb lock. A lock file per store directory, or refusing to open a directory
   another server holds, would make the same-machine case safe.
3. The app's embedded server trusts every local process (no token on loopback). Fine for a
   single-user machine; a multi-user one would want the app to use the server token file.
4. `getChildren` returns every child's full content bytes, and tree labels decode a yrs
   snapshot per node. A label/preview field from the server before big stores make it
   noticeable.
5. Every `applyEdit` writes `modified_at` into `store.yrs` (one store-document change per
   keystroke). Coalesce it.
6. A content update yrs can only stash as pending reports "unchanged", so the server
   neither relays nor flushes it until the missing update lands. A reconcile closes the gap.
7. A down link flaps `Syncing`/`Offline` on every backoff tick, sending two
   `SyncStateChanged` and two identical `MountStateChanged` per tick; the app dedups the
   latter by kind. Suppressing the flap at the link would be cleaner.
8. `pimble-cli` built on its own compiles `pimble-search` without `semantic`, so the same
   store's index schema flips between that binary and one built with the app.
9. `removeReplica` on a replica a mount depends on makes the next expansion of that mount
   recreate the replica in the background. Intended, but worth a confirmation in the app.
10. `updateNodeContent` still exists for the importer and tests; it is wrong for any node a
    replica already holds. Consider making the importer seed through `applyEdit` too and
    removing the RPC.
11. rinch #714: a `DropdownMenuItem` inside a reactive block never closes its `ContextMenu`;
    portals leak on unmount. Worked around (items always rendered, `disabled` at render
    time, row data changes for re-renders, bumps deferred past the menu close).
12. rinch repaint artifacts seen earlier (a strip below a shrunk modal, a stray line across
    the tree). Not isolated; reproduce and file.
13. The importer flattens RTF formatting to plain paragraphs.
14. The collaboration scope rejects lists, block quotes and tables.
15. Cross-store drag-and-drop is ignored with a warning.
16. `pimble-client::get_nodes` (and the `getNodes` RPC) are unused.
17. A CLI `show-node | head` panics on a broken pipe; harmless, cosmetic.

## Then the roadmap: links UI and backlinks, then plugins

`docs/RESTART_PLAN.md` §6 and `docs/ARCHITECTURE.md` "Phase 7: Linking & Navigation".
The index already stores backlinks (rhypedb relationships); the app has no link insertion
or backlink panel yet. Remote mounts are complete apart from replicating only the mounted
subtree, which needs partial replication first.
