# Next session: start here

Written 2026-09-15 at the end of the remote mounts session, updated the same day after the
Scrivener importer and tree appearance work, and again the same evening after **cloud
phase 1** (branch `cloud/phase-1`). Read this, then `CLAUDE.md`, then
`docs/CLOUD_CONTRACT.md` and `docs/DEPLOY.md`.

## Where things stand

- Branch `cloud/phase-1` off `master`, working tree clean. Not merged yet: merge once the
  first deploy has been exercised.
- **Cloud phase 1 is code-complete (2026-09-15), not deployed.** Joe decided to pause the
  roadmap for a website with downloads and signup, a web app, and a hosted server, all on
  jkbase (`~/dev/jkbase`, his own platform). Contract `docs/CLOUD_CONTRACT.md` (status
  header lists the decisions made during the work); summary in `CLAUDE.md` "Cloud,
  phase 1". Done, each in its own commit: JWT auth and per-store authorization in
  `pimble-server` (94 tests across server and CLI); the `pimble-cloud` accounts service
  (10 integration tests against a real `rhypedb-server`); `pimble-app` split into a UI
  library and the desktop binary, `pimble-client` on wasm, and `web/` (trunk) verified end
  to end with two browser tabs editing one document; `site/`, `jkbase.toml`,
  `.github/workflows/{ci,release}.yml`, `docs/DEPLOY.md`.
- **Live already:** the jkbase project `pimble` (id `pimble`) exists; `pimble.app` and
  `www.pimble.app` point at it (Cloudflare, DNS only), are verified, and have Let's Encrypt
  certificates. They answer 503 until the first deploy. `m.pimble.app` is a Resend sending
  domain for phase 2 email.
- **Not done:** the first `jkbase deploy` (needs `jkbase auth key create` and the secrets
  in `docs/DEPLOY.md`; Joe confirms before deploying); the Windows release job has never
  run; no `v*` tag exists, so the download page shows its empty state.
- Both CRDT documents are yrs. No Automerge, no migrations, no backwards compatibility.
- rinch is tracked from GitHub `main`, pinned in both `Cargo.lock`s at `743f8a0`; the root
  workspace no longer declares `rinch` itself (`crates/pimble-app/Cargo.toml` does).
- Everything before the cloud work (search on rhypedb, local and remote mounts, replica
  sync, hardening, the rich-text Scrivener importer, tree appearance) is summarised in
  `CLAUDE.md`, with contracts in `docs/history/`.
- Workspace compiles with zero warnings. `cargo test --workspace --release`: 235 passed,
  5 ignored.

## Verify in five minutes

```bash
cargo check --workspace --all-targets      # expect zero warnings
cargo test --workspace --release --no-fail-fast   # pimble-cloud tests need ~/dev/rhypedb/target/debug/rhypedb-server
cargo build -p pimble-app -p pimble-cli --release   # build together: see follow-up 8
cd web && trunk build --release && cd ..   # the browser build
```

Cloud, headless: `crates/pimble-cloud/README.md` and `web/README.md` have the four-process
local stack (accounts service on 8080 in development-signing mode, `pimble-cli server`
with `--jwks http://127.0.0.1:8080/api/v1/.well-known/jwks.json --issuer
http://127.0.0.1:8080/api/v1 --allow-origin http://127.0.0.1:8081 --stores-dir ...`, the
site from a static server, `trunk serve` for the web app). Sign up on the site, create a
store on the account page, open `/app/`, edit in two tabs.

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

0. **Deploy cloud phase 1** (`docs/DEPLOY.md`): issuer key, secrets, `jkbase deploy`, then
   sign up on pimble.app, create a store, open the web app, edit from two browsers. Then
   tag `v0.1.0` to exercise the release workflow (the Windows job is unverified) and merge
   `cloud/phase-1`. Then phase 2: desktop sign-in (`AuthMethod::CloudSession` so the sync
   link mints a fresh JWT before each connect), the relay, email through Resend.
1. TLS for a self-run `pimble-cli server` (`wss://`). Tokens go over plain `ws://`; fine on a trusted LAN, not beyond it. The hosted server on jkbase is behind the edge's TLS already.
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
    the tree), and once, right after View > "Toggle Dark Mode", the status bar's
    "Connected" badge drew without its text until the next restart. None isolated;
    reproduce and file.
13. The Scrivener importer (2026-09-15) keeps bold, italic, underline, strike, links,
    text colour, highlight, sub/superscript, monospace-as-code, nested bullet and ordered
    lists, alignment and indent, and stylesheet or bold-and-larger headings. It reduces
    what the content model cannot hold: tables become one tab-separated paragraph per
    row, pictures are skipped, a line break is a paragraph break. Scrivener's comments
    and footnotes (`\Scrv_` groups) are not in this project's RTF and are untested.
14. The collaboration scope rejects block quotes, tables, images and hard breaks; lists
    are in scope and their toolbar buttons are back.
15. Cross-store drag-and-drop is ignored with a warning.
16. `pimble-client::get_nodes` (and the `getNodes` RPC) are unused.
17. A CLI `show-node | head` panics on a broken pipe; harmless, cosmetic.
18. Tree appearance (2026-09-15): per-node and per-store icon and colour, a picker with
    search over every Tabler icon and a tags field, Scrivener labels and icons imported,
    and a runtime dark/light toggle (View menu, persisted). Tags are editable in the
    picker but not shown in the tree (Joe: the label chips were noise).
19. A search hit for a node the tree has not loaded now opens the editor pane (it used
    to start the session with the pane hidden); worth a look at whether the tree should
    also expand to and select that node.

## Then the roadmap: links UI and backlinks, then plugins

`docs/RESTART_PLAN.md` §6 and `docs/ARCHITECTURE.md` "Phase 7: Linking & Navigation".
The index already stores backlinks (rhypedb relationships); the app has no link insertion
or backlink panel yet. Remote mounts are complete apart from replicating only the mounted
subtree, which needs partial replication first.
