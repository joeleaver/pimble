# Next session: start here

Updated 2026-09-16 evening after **v0.1.0, the desktop account UI and two server
fixes**. Read this, then `CLAUDE.md` ("Cloud, phase 1", "Cloud, phase 2a", "Desktop
account UI"), then `docs/CRYPTO_CONTRACT.md`, `docs/DESKTOP_ACCOUNT_CONTRACT.md` and
`docs/DEPLOY.md`.

## Where things stand (2026-09-16 evening)

- **`cloud/phase-1` is merged into `master`** (fast-forward; `master` is at the workflow
  and release commits, the later commits below are on `cloud/phase-1` and fast-forward
  cleanly). CI on GitHub runs on `master` pushes and is green.
- **Tag `v0.1.0`** is pushed (at `master`'s head). Two CI facts learned the hard way: the
  static ONNX Runtime that `onnx-download` links (ort-sys 2.0.0-rc.12) references glibc
  2.38 symbols, so every Linux job runs on `ubuntu-24.04` and **the shipped Linux binary
  needs glibc 2.38 or newer** (Ubuntu 24.04, Debian 13, Fedora 39; the keyword-only build
  has no such floor). The **Windows build fails in `rhypedb-storage`** (memmap2's
  `Mmap::advise` is Unix-only): fixed upstream as joeleaver/rhypedb#23, not merged; until
  the pin moves past it the Windows job is `continue-on-error` and the release publishes
  with the Linux package alone (the download page hides a missing platform). Check the
  release run: `gh run list --workflow Release`; the GitHub release should exist with
  `pimble-0.1.0-linux-x86_64.tar.gz`.
- **Desktop account UI is done** (`docs/DESKTOP_ACCOUNT_CONTRACT.md`, one agent, PM
  verification in the GUI against the local stack with rinch's TCP debug protocol driven
  by a script, since the rinch MCP server binary is missing on this machine). The contract's
  verification list passed: sign-in, restart, "Add Hosted Store...", "Host on Pimble
  Cloud...", live edits landing on the hosted disk as `PB` ciphertext, sign-out and
  sign-in again.
- **Two server bugs found by that verification, fixed with a regression test**
  (`adopting_a_hosted_replica_starts_no_plain_link_beside_its_vault_link`): an adopted
  vault replica also got a plain sync link (flapping "refused the credentials"), and a
  vault replica's manifest kept its placeholder root so a reopened replica showed "Node
  not found" and no children. See the CLAUDE.md section.
- **Production (v15, 2026-09-16 21:25 UTC):** Joe reported the password-recovery mail
  never reaching Resend. The service never called the mailer (no warning in the log); the
  route has four silent no-send outcomes (unknown address, unverified, no key material,
  a repeat within a minute), most likely the first: the 03:09 UTC cleanup deleted seven
  legacy accounts, so an account from before phase 2a is gone. v15 logs which outcome a
  recovery request hits and trims the address in the lookup. **Open:** ask Joe which
  address and when the account was created; read `jkbase logs --service cloud` after the
  next attempt.
- **2026-09-17: edits made while a vault link is down were never pushed** (Joe: the web's
  typing reached the desktop, the desktop's never reached the web). A reconnect pulled and
  pushed only never-seen documents; the lost edit made every later one from that device
  pending everywhere else. Fixed in `vault_link.rs` (`Progress`, `vault-link.json`, push
  of the diff since the known state on every reconnect; a replica from before the file
  pushes each document's whole state once, which is what heals an already-broken one) and
  in the web vault client (`catch_up` on every connect, resend after a failed append).
  "Unlink from Remote" now also stops a vault link. Tests: a cuttable TCP relay in
  `tests/vault_link.rs` (`env.relay.cut()`/`restore()`), two new regression tests.
- **2026-09-17: a snapshot could destroy what it did not contain.** The hosted server
  deletes every log entry at or below a snapshot's number, and both clients stamped a
  snapshot with the number of their own latest append, whatever they had applied below it
  (another device's append still in flight, or a blob that would not decrypt). The same
  "highest number seen" was the fetch cursor, so an entry below an own append was never
  fetched after a drop. Fixed with `pimble_rpc::VaultCursor` (`applied_through`: the
  largest n with every entry 1..=n reflected locally; an unapplied entry holds it): the
  desktop link and the web client fetch from it and snapshot only when it equals their
  own append's number; `sync.json`'s `last_seq` now holds exactly that value, and a
  replica from before (`vault-link.json` `cursor_version` below 1) reads every log once
  from the start. The server keeps a held snapshot over one that is not newer, and
  **ignores (but acknowledges) a snapshot whose request lacks `covers_prefix`**, which is
  every client built before this change. Tests: `a_snapshot_never_covers_an_entry_this_
  device_did_not_apply` (fails on the old rule: "snapshot at 200, entry 2"), the store and
  RPC-level guards, the cursor's unit tests.
- **Local verification stack for the desktop** (`tools/dev-proxy.js` is the proxy, `tools/rinch-debug.py` drives the GUI):
  rhypedb-server, `pimble-cli server` on 7463 in JWT mode with a token file, `pimble-cloud`
  on 8080 with `PIMBLE_CLOUD_PUBLIC_URL=http://127.0.0.1:8090`, a 60-line node reverse
  proxy on 8090 (`/api/*` to 8080, `/rpc` to 7463 with WebSocket upgrade; the vault link
  connects to the `rpc_url` the token endpoint reports, so a one-origin proxy is
  required), the desktop app with `XDG_CONFIG_HOME`/`XDG_DATA_HOME` in temp dirs (embedded
  server on 7462, so the hosted server must not be on 7462). A signup body is built with a
  tiny scratch crate over `pimble-crypto` (copy `build_signup_body` from
  `crates/pimble-cloud/tests/integration.rs`) and POSTed with curl; the verification link
  is in the service log. rinch's debug protocol: `~/.rinch/debug/*.json` names the port;
  4-byte big-endian length-prefixed JSON frames; handshake `{"protocol":"rinch-debug",
  "version":1}`; then `{"id":n,"method":"screenshot"|"click"|"type_text"|"key_press"|
  "dom_tree"|"query_selector",...,"params":{...}}` (see
  `crates/rinch-mcp-server/src/client.rs` in the cargo checkout).

## Where things stood before (2026-09-16 morning)

- Branch `cloud/phase-1`, not merged to `master`. Every phase 1 and 2a commit is on it.
- **Live on pimble.app (deployment v9, 2026-09-16 02:31 UTC): phases 1 and 2a.** The PM
  verified on production: signup at `/app/signup` with the recovery code, the real
  verification mail (readable through Resend's API: `GET /emails/{id}` returns the body),
  login, an encrypted store from the explorer's `+`, a note typed in the browser, and the
  hosted server holding only vault documents while refusing plain RPCs on that store with
  `-32005`. The web menu bar (File, Edit, View) renders. Push-to-deploy with
  `git push jkbase` (force when the platform's `main` diverged); the build cache works
  with the exclude lists (a site-only change rebuilds nothing else; a changed Rust target
  still takes 4 to 12 minutes).
- **Incident 2026-09-16 02:40 UTC:** deployment v10 (the legacy-user fix) crash-looped the
  accounts service at startup because its cleanup read every user row and a phase 1 row has
  no `verified` field either; rolled back to v9 within two minutes (`jkbase rollback
  --version 9 --force`). Lesson, now a rule: a startup migration or cleanup must never be
  able to stop the service from serving (log and continue), and production holds rows of
  three generations (phase 1, phase 1b verification, phase 2a keys), so every row read must
  treat later fields as optional.
- **Fixed and deployed 2026-09-16 03:09 UTC:** the accounts service reads rows of every
  generation with later fields optional, its startup cleanup removed the seven keyless
  accounts (logged), a cleanup error can no longer stop the service, and `kdf` answers the
  decoy for an unknown or legacy address.
- **2026-09-16 afternoon:** rinch PR #791 merged and both workspaces are back on `main`
  (deployed as v11). A live two-tab bug (a concurrent typist's edits lost in one direction)
  traced to the server never attributing vault appends: `vaultAppend` now carries a client
  id stamped on `VaultAppended`, the web client applies everything not attributed to
  itself, the desktop link drops echoes by identity first (convergence test with two
  servers racing). Verified on production (v12) by the PM with two tabs typing alternately.
  The `TypeError ... reading 'length'` console errors (rinch-web's keydown/keyup
  listeners casting non-keyboard events, triggered by autofill) are fixed by rinch PR #810,
  merged and deployed as v14 2026-09-16 20:49 UTC: the same unlock-and-mount sequence now
  logs zero errors. Two lessons from that deploy: a target's `exclude` list must never name
  a workspace member crate (cargo cannot load the workspace without every member
  manifest; the excludes are `site`, `docs`, `*.md`, `jkbase.toml`, `web` for the servers),
  and the platform's monthly build-minute quota (200 minutes by default) silently drops
  pushes once exhausted; Joe raised it. Account recovery is built in the accounts service (`/recover/start`,
  `/recover/{token}`, `/recover/{token}/complete`, `/me/password`, `/me/recovery-code`,
  `/recover/{token}/delete-account`, 36 tests) and the web pages are built (`/app/forgot`, `/app/recover?token=`, change password and a
  new recovery code on `/app/account`), verified against the real service with logged
  links; deployed as v13 and verified on production by the PM (recovery link through
  Resend, old code accepted, new code issued, old password refused, new password signs in,
  the encrypted note still decrypts).
  The server targets' cache inputs now exclude `web/` as well.
- **Production test account** (the PM's, delete when there is a way): `pm-live-2a@resend.dev`
  with the encrypted store "Live vault".
- **Decided (Joe, 2026-09-16):** the store display name stays plaintext; it is listed as
  visible metadata in the crypto contract.
- **rinch PR #791 merged 2026-09-16**; both workspaces track `main` again (`a27ae8e`, which also carries PR #810 (the keydown guard)). It
  carried the web menu bar, native context-menu suppression, a stale-handler fix and the
  context-menu portal closing with its scope.
- Workspace compiles with zero warnings. `cargo test --workspace --release`: 312 passed
  (the accounts tests need `~/dev/rhypedb/target/debug/rhypedb-server`; the harness now
  retries port races and caps concurrent stacks).

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

0. Done today: `v0.1.0` tagged, `cloud/phase-1` merged, the desktop account UI, account
   recovery and password change. Next: merge joeleaver/rhypedb#23, move the rhypedb pin
   (`cargo update -p rhypedb-storage` and the other rhypedb crates), drop
   `continue-on-error` from the Windows job and re-tag or tag `v0.1.1`; the desktop
   "Account..." modal could offer signup (today it only signs in; signup is on the web);
   move the unwrapped keys from `keys.json` to the OS keychain; then phase 2b sharing
   (`docs/CRYPTO_CONTRACT.md`, `docs/CLOUD_CONTRACT.md` "Phase 2: sharing").
0a. **Hosted build time.** `pimble-cli` compiles about a thousand crates including
   wasmtime (the plugin skeleton, `pimble-plugins`), and jkbase keeps no compile cache
   between builds, so every push costs about 12 minutes. Make wasmtime optional in
   `pimble-plugins` (the hosted server loads no plugins) and ask jkbase for a per-project
   `target/` cache.
0b. **jkbase issues found on the first deploys** (Joe's platform, fix there): the trunk
   buildpack reads the wasm-bindgen version from the build-context root `Cargo.lock`, not
   the workspace containing `source` (both locks must agree for now); `[hosting]` is
   silently ignored once any `[sites.*]` exists (the README's kitchen-sink example shows
   both); a failed trunk target's log tail ends at cargo's "Finished" line, hiding trunk's
   own error; `jkbase deploy` stops polling after 12 minutes.
0c. **Web app notes:** jsonrpsee's wasm client reports connected before the socket opens
   (the backend now waits for `listStores` to answer); the desktop store-row menu after
   the `CAN_ADMINISTER_STORES` gate was verified by build and inspection, not by
   right-clicking a desktop row (agent A's embedded server port was taken at the time).
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
