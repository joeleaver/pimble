# Next session: start here

## 2026-09-23: desktop performance, released as v0.4.0 (branch `perf/gpu-and-caret`)

Joe reported the desktop "almost unusably slow". Profiled on a copy of the family store
(perf plus rinch's `RINCH_PERF`, driven over the rinch-debug port): pimble's own code was
about 1% of the UI thread; the time was rinch. As shipped, at 3840x2160, a keystroke cost
~175 ms in a short document and ~670 ms in a 9,500-word one; now ~12 ms and ~22 ms of
UI-thread CPU, and two frames per keystroke instead of four plus the blinks.

- **rinch PRs**, all merged upstream on 2026-09-23: joeleaver/rinch#857 (a text block laid
  out with no width constraint was reshaped on every layout pass: `(inf - inf).abs()` is
  NaN), #858 (the caret hid with `display`, a structural change that reshaped every text
  block in the window, twice per keystroke; now `visibility`), #869 (a `gpu` build keeps
  the software renderer, chooses at run time, falls back to it when the GPU will not
  start; `App::renderer`, `RINCH_RENDERER`), #871 (an identical attribute or style write
  restyles nothing). Upstream refined #869 as it merged it: an app that configured the GPU
  device itself ignores a software choice (pimble does not configure one). Every rinch
  dependency is back on `main`, both lock files at 4200dda (#869's merge); the interim
  `pimble/perf` branch is deleted. The web lock moved with it, so the browser build picks
  up rinch's web fixes since 651ecf7 (#862, #867) at the next jkbase deploy, which v0.4.0
  did not need (nothing server-side changed).
- **GPU again** (e061347): the desktop build turns `rinch/gpu` on (it was dropped on
  2026-09-12 without Joe's OK, which he says was the wrong call) and repeats rinch's wgpu and
  winit `[patch.crates-io]` in the root `Cargo.toml`. At 4K the software renderer spent
  ~100 ms repainting a frame; Vulkan 3-8 ms. `pimble --cpu` / `--renderer auto|gpu|cpu`
  (ea3d9ed) chooses; the default falls back to software with a warning when the GPU will
  not start (checked by forcing `WGPU_BACKEND=metal` on Linux).
- **Fewer redraws** (782d0ae): toolbar buttons restyle only when their own state flips
  (per-button `Signal<bool>` with `set_if_changed`; rinch's `Memo` does not filter equal
  values), tree rows watch only their own node's live label (per-node signals, leaked so
  they outlive the row that first asks), and an explicitly titled node's label no longer
  decodes its content.
- Checked on rinch `main`: `cargo test --workspace --release`, the Windows cross-check,
  the web crate's wasm32 check, and the app itself (Vulkan by default, `--cpu` software).
- **Next, in rinch, one at a time:** the caret's position as a paint-only offset (it still
  forces a second layout pass), keeping unchanged parts of the scene instead of repainting
  the whole window every frame (and on every blink), and not laying out a whole long
  document per keystroke. Also open: the search box costs ~70 ms per character; selecting
  text creates and deletes highlight elements.
- v0.4.0 is the first Windows package drawn on the GPU (DX12/Vulkan through the wgpu
  fork), with the software fallback if it will not start.

## Before that: v0.3.2 and v0.3.3

Updated 2026-09-22, night. **v0.3.2 and v0.3.3 are shipped** with Joe's word ("yes yes,
do it please"). v0.3.2 (release commit 2b604fe, deployment v22, backup before it
`bkp_1790107649039_3bc277db966e4da8`) is branch `rhypedb-pin` (the section below): the
rhypedb pin that makes Windows build, the accounts service waiting for RhypeDB, the mount
fix. **Its release is the first with a Windows package** (`pimble-0.3.2-windows-x86_64.zip`
beside the Linux one), and CI passed on the release commit. Its deployment showed the
RhypeDB wait working and a second dependency failing the same way (the hosted Pimble
server on 7462 was not listening yet), so the service still exited once; v0.3.3 (e2aeef3,
release commit 251ad26, backup `bkp_1790109106982_63cfd81d6a864353`) waits for that
connection too (a refusal, 401/403, is not waited out) and makes the Windows package a
required job. Verified on deployment v23: the site, `/app/`, health, JWKS and releases answer
200 and `/rpc` 401; the accounts service logged the one INFO wait for RhypeDB and was
listening 100 ms later with no exit (no `Error:` line after v22's); the hosted server
opened its three stores with no warning; CI passed on the release commit; the GitHub
release has both packages (Windows now a required job), and the download API was already
listing v0.3.2's Windows zip. Joe's part: install v0.3.3 where he wants it (Windows now
has a package); each store's search index rebuilds in the background at its first open
on the new version, and search says it is still building until then. Next: whatever Joe
reports; the `auth` test target flake ("A flake to watch") is the one unexplained item.

## 2026-09-22, night: branch `rhypedb-pin` (shipped as v0.3.2)

- **rhypedb on master, 48424ea** (8ebfc4a). joeleaver/rhypedb#23 (the Windows fix) was
  merged on 2026-09-17, but the pin stayed 17 commits behind it, so every Windows release
  job up to v0.3.1 failed on the same two errors. The commits in between are full-text
  hardening in the engine, query, storage and server crates; the wire protocol and client
  did not change. The analyzer changed, so a store's full-text index is rebuilt in the
  background at its first open after the upgrade (search answers `IndexBuilding` until it
  is done); an index written by the v0.3.1 CLI opened and answered at once under the new
  one (three notes, prefix search included). The Windows check can now run locally:
  `cargo xwin check -p pimble-app --release --target x86_64-pc-windows-msvc` with
  `llvm-lib` on PATH (`/usr/bin/llvm-lib-21` symlinked as `llvm-lib`). It fails on the old
  pin with CI's two errors and passes on this one. The Windows job built for the v0.3.2
  tag and is a required job from v0.3.3 on.
- **The accounts service waits for RhypeDB** (f1bcf4e): a refused dial is retried with
  backoff for up to 60 s and logged once at INFO, instead of exiting (the line in every
  deployment's log was `Error: ... connecting to rhypedb at 127.0.0.1:4201: connect
  failed: Connection refused`, then a restart five seconds later).
- **The remote-mounts flake, explained and fixed** (61e44ef). A replica is opened a moment
  before its link starts, and that moment read `Live` ("open, no link") over an empty
  replica. The test took that `Live`, stopped the remote, and the link, which had never
  synced, reported `Connecting` instead of the `Cached` the test waited for. A `sync.json`
  naming a plain link that is not running yet now counts as a link that has not synced.
  The new test `a_mount_is_not_live_before_its_source_replica_has_synced` failed 22 of 30
  runs before the fix and 0 of 30 after.
- Checked: `cargo check --workspace --all-targets` clean; `cargo test --workspace
  --release` (CI's step) on the branch's head exited 0: 636 passed, 49 targets.

## Before that: v0.3.1

Updated 2026-09-22, late. **v0.3.1 is shipped**: `follow-ups` fast-forwarded into `master`
with Joe's word, release commit 038f76e (tag `v0.3.1`), jkbase deployment v21 (verified:
site, `/app/`, health, JWKS and releases answer 200, `/rpc` 401 to the unauthenticated, the
hosted server opened every store, the accounts service up; backup before it:
`bkp_1790100112989_7578868ac966b8b0`, no migration), the GitHub release with the Linux
package (Windows still waits for rhypedb#23), the download page picking it up within its
ten-minute cache. Two things seen on the way: the accounts service logs one refused
connection to the managed RhypeDB at every deployment's start (v19, v20 and v21 each have
one line) and comes up five seconds later, so a startup retry would remove the line; and
CI on the release commit failed on the known flake
(`remote_mounts::a_mount_whose_source_link_is_down_is_cached_and_still_readable`, "A flake
to watch" below), which passed in the full local release run minutes earlier. The re-run of the failed job passed, and the download page offers v0.3.1.
Joe's part: install v0.3.1 where he wants the read-only switch; nothing breaks on a device
that stays on v0.3.0. Next: whatever Joe reports. Before it, **v0.3.0 was shipped**: the move contract (`docs/MOVE_CONTRACT.md`,
built, reviewed and verified that day; the first section below), plus three desktop fixes.
`master` is fast-forwarded from `node-document` and carries the release commit 3cf6521
(tag `v0.3.0`) and, after it, a test-only fix 01fd278 and these notes; jkbase's `main` is
at 3cf6521 (deployment v20, verified: every endpoint, the hosted server opening every
store, the accounts service up); the GitHub release has the Linux package and the download
page offers v0.3.0 (the Windows job still waits for rhypedb#23). CI failed once on the
release commit: the randomized test's floor for how often the share exception must fire
was too high for CI's eight seeds now that `placed_under` makes it rare; the floors are
lowered in 01fd278 (the convergence itself passed). Backup taken before the deploy:
`bkp_1790095295052_ea2d819bcc277fbd`. No migration this time. The section below was
written before the ship and still describes the work. Before it, **v0.2.0 was shipped**: `master` fast-forwarded to `node-document`
(Joe's go-ahead the same day), production is deployment v19 on jkbase, the desktop release
`v0.2.0` is on GitHub (Linux; the Windows job still waits for rhypedb#23). Read this, then
`CLAUDE.md` ("Current design", "Sharing on node documents"), then
`docs/NODE_DOCUMENT_CONTRACT.md`, `docs/RELAY_CONTRACT.md` and `docs/MOVE_CONTRACT.md`. To
see any of it run, `scripts/local-stack/README.md`.

## 2026-09-22, later: the follow-ups (branch `follow-ups` off `master`, verified, not merged)

Five commits, none pushed. Each was checked as it landed; the whole workspace was checked
(`cargo check --workspace --all-targets`, clean) and the suites it touches were run (crdt
121, store 47, share 17, share_upkeep 1, the web 60); the full release run is recorded at
the end of this section.

- **rinch is on `main` at 651ecf7** (67b9b0f), the one commit since c4845d3: joeleaver/rinch#832,
  the editor's runtime read-only switch. `editor::set_read_only` is driven by an effect
  in `app.rs` over the same judgement that swaps the toolbar for the sentence
  (`AppStore::node_access` of the active node), so a role that changes while a document
  is open flips it; the interim (a keystroke sent nothing and reopened the node from the
  server's copy 400 ms later) is gone, the outbound guard stays as the invariant and logs
  if anything reaches it, and the read-only line is the refusal sentence alone. Verified
  on the local stack on a reader's identity, desktop and browser: typing refused with the
  caret in the document while the owner's edit landed live; made an editor, the desktop
  unlocked at the next grant check (75 s) and its typing reached the owner; demoted with
  the note open, it locked again at the next check and the typing was refused; the
  browser the same both ways (focus in rinch-web's capture textarea with `readOnly` set),
  picking the new role up on a reload (a page learns of a role change at reconnect, not
  within two minutes as a desktop does). The wasm build with the new pin runs; both lock
  files agree on wasm-bindgen 0.2.128.
- **`tests/common`** (e940039): `share.rs` and `share_upkeep.rs` share one harness
  (`crates/pimble-server/tests/common/mod.rs`). `vault_link.rs` and `relay.rs` keep their
  own: an earlier stub shape and the relay's routes; folding them would be a rewrite of
  their tests, not an extraction, so they stay.
- **One node mapping** (54313d1): `NodeFields::into_node` and `parse_timestamp` in
  `pimble-crdt` (`Tree::live_fields` is the door to a live node's fields); `LocalStore`
  and the web vault client both use it, `get_node_any`/`node_of_any` included.
- **The web refuses a plain transplant across two servers** (ca19caf) with a sentence
  before anything is sent. Unreachable today (a store served from its owner's computer is
  always encrypted, so every plain store a page sees is the hosted server's); the move
  contract's "Between stores" paragraph says so.
- The signup helper's lock file (f29f4e8).

`cargo test --workspace --release` (CI's step) on the branch's head exited 0, every target
green, with the local stack down.

Not done: the partial-replica grouping limit (a follow-up only if it ever matters).

**Merged and shipped as v0.3.1 the same evening** (the top of this file).

## 2026-09-22: the move contract, built and being verified

- **Built** by four sonnet agents in parallel on disjoint files, from the seam the PM wrote
  first (3967585: `LeftShare`, `DeletedNode`, `MoveNodeResponse`, `transplantNode`,
  `listDeleted`, the app's `UndeleteNode`/`TransplantNode`/`ListDeleted` commands and
  `NodeTransplanted`/`DeletedListed` events): the server (00ada49), the share upkeep and
  CLI (9b51573), the web vault client (97799e8), the shared UI (ac0ddcc). The whole
  workspace passed (629 tests, 35 targets) and the web (59) with all four together.
  `CLAUDE.md` has the design summary ("Moves that leave a share").
- **The repair rule of wave 1 did not converge, and was narrowed** (cf1e1a1, 85a2d1e).
  The randomized convergence test could not replay a seed (yrs client ids were random),
  so one ten-minute run from the previous session could not be explained; with client
  ids seeded under test, a drain cap and an oracle that prints its facts, seeds 76 and 1
  showed two devices rewriting each other's `parent_id` rewrites for ever. Now every
  placement writes `placed_under` with `parent_id`; a `parent_id` that agrees with it (or
  whose named parent's list names the node) is honoured everywhere, and only one written
  on its own is judged, once. 600 seeds converge (`PIMBLE_CONVERGENCE_SEEDS=$(seq -s, 1
  600) cargo test --release -p pimble-crdt three_replicas_converge`). The contract's
  "Repair" section and `docs/NODE_DOCUMENT_CONTRACT.md` (the `node` root) say so.
- **A review of the combined diff** (nine findings) is being fixed by two agents as this
  is written: `moveNode`/`transplantNode` judged "the list the node leaves" by its
  `parent_id`'s owner instead of by the lists that name it (a member reading share F and
  editing P could move a note out of F when its `parent_id` had been pointed elsewhere);
  `listDeleted` shipped every tombstone's whole content; the "Recently Deleted..." modal
  took every error as its own. Deferred: a plain-to-plain transplant in the web assumes
  one endpoint serves both stores; the harness copied into `tests/share_upkeep.rs` should
  become a `tests/common` module; `get_node_any`/`node_of_any` duplicate the node mapping.
- **Known limits of the design as built**: on a member's replica holding two scopes,
  `repair_scopes` groups documents per scope by `parent_id`, so a tampered `parent_id`
  into the other scope is adopted there instead of corrected (the owner's devices hold
  the whole picture and decide right; a member can only reach shares they already write).
  A client that writes `placed_under` or appends to the destination's list too gets its
  move completed: the accepted residual (the node stays in scope, members put it back).
- **The review's fixes landed** (a13da3c server, ddb1f32 app): a move judges every list
  that names the node; `listDeleted` without content; the modal takes only its own
  errors; a test moves a two-node subtree between shares and the other share's member
  reads both planted documents.
- **Wave 4 passed on 2026-09-22** against the contract's "Verification (the bar)", on the
  local stack rebuilt from the committed state: headless with the CLI (a member moving a
  note between two shares, the owner moving a folder into a private one, a member moving
  a note into a store of his own: each an undoable delete for the share's other member,
  the text intact on the new node, nothing of the private copy on the member's disk);
  in the desktop app on bob's identity (the drag between stores, the notice word for
  word in the status bar, "Recently Deleted..." with "Put Back", the editor following an
  open note into the other share); in the browser on carol's (Put Back, a new encrypted
  store, the drag between stores). No typed word or title on the hosted disk. The steps
  are in `scripts/local-stack/README.md`, "The move walk-through".
- **Two desktop bugs Joe reported the same day, fixed** (d3cc539): documents did not
  scroll (`min-height: 0` on the editor's box let it shrink to the pane), and the
  maximize and minimize buttons did nothing (no callbacks wired). Joe then reported no
  scrollbar is drawn: fixed (1d5d9be), the editor pane is a stacking context of its own
  so its scrollbar paints last.
- **Shipped as v0.3.0 with Joe's go-ahead the same evening** (see the top of this file).
  `placed_under` is a new key a v0.2.0 client does not write, which the repair rule
  tolerates (judged by lists), so no migration and no lockstep update this time; a v0.2.0
  client's plain move out of a share is put back by the newer devices, which is the
  contract. Joe's part: install v0.3.0 where he wants the new behaviour; nothing breaks
  on a device that stays on v0.2.0.
- **Next**: the follow-ups above (a `tests/common` harness, the web plain-to-plain
  transplant across endpoints, the partial-replica grouping limit if it ever matters),
  and whatever Joe reports from using v0.3.0.

## The ship, 2026-09-21

- Before the push: a fresh backup of the production accounts database
  (`bkp_1790030572153_df62963df8ca5e70`; `jkbase db backups`, `jkbase db restore`), both
  lock files agreeing on wasm-bindgen 0.2.128 and rinch c4845d3, the whole workspace green
  (587 tests, web 53).
- After it: the site, `/app/`, `/api/v1/health`, the JWKS and the releases endpoint answer
  200; the relay's two routes and `/rpc` answer 401 to the unauthenticated; the accounts
  service came up on the new schema and reads users; the hosted server opened every store
  and migrated the one plain store it holds ("Live smoke", 2 nodes). `jkbase rollback
  --version 18` is the way back for the services; a migrated plain store would then need
  `store.yrs.migrated` renamed back by hand.
- **Joe's part**: back up his own stores, install v0.2.0 on every device before it syncs
  (0.1.1 cannot open or sync a migrated store), and let each desktop sync once so its
  hosted twin receives the node documents; until a store's desktop has done that, the new
  web app shows that store's tree empty.
- Not watched yet: a real account signing in on production after the deploy (the PM has
  none); `jkbase logs --service cloud` after Joe's first sign-in is the check that old
  grant and store rows read cleanly under the new schema.

## Where things stood before the ship (2026-09-21, branch `node-document`)

- **The redesign Joe approved on 2026-09-18 is built.** Every node is one co-authored yrs
  document; there is no store document; sharing is a scoped grant on the owner's own
  documents. `cloud/phase-2b` (the share mirror) is the rejected cut, kept as a record,
  never merged. `master` is still v0.1.1 with the old design.
- **Verified by the PM against the real local stack on 2026-09-21** (accounts service,
  hosted server in JWT mode, three desktops with their own keystores):
  - Headless, through the CLI: the unhosted refusal sentence with nothing uploaded; host,
    share, invite; members see the share's name and "shared by", never the owner's store
    name; **with every owner device off, two members create, move, delete and write in the
    shared folder and see each other's changes, a document one creates is readable by the
    other at once**; a reader's writes are refused with the sentence; no typed word, title
    or share name on the hosted disk; nothing of the owner's unshared folders on a member's
    disk; the owner's tree has everything on its return; a note moved into the share reaches
    members (keys included) and stops reaching them once moved out; a role change, a second
    and third share of the same store, a removal and a stopped share each reach a member's
    machine within two minutes.
  - In the desktop app (rinch's debug server built from the cargo checkout and driven by a
    script; the configured rinch MCP server binary is missing on this machine): the owner's
    share badges, the Share dialog against the real RPCs (members, invite), the unhosted
    refusal verbatim in the dialog, a member's replica with several shared roots under one
    store row, a member typing in the app and the owner's app receiving it.
- **The browser pass was run on 2026-09-21** (the built-in browser, two accounts side by
  side on `127.0.0.1` and `localhost`, test passwords typed by the PM: they are fixtures
  of the local stack, never a real account's). Passed: a share listed under a share's
  name with "shared by" and never the owner's store name; a member typing, creating and
  renaming with the owner's desktop off, arriving on the other member's desktop and in the
  owner's page; the owner's rename arriving live in the member's page; a reader's document
  taking no typing, its menu disabled, and **nothing sent** (the documents' heads on the
  hosted server did not move); the editable share still working right after a refusal; a
  reload asking for the password and recovering everything, the keys of documents the page
  itself created included; after a restart of the hosted server the open page reconnected,
  subscribed again and received a desktop edit live; no typed word or title readable on
  the hosted disk.
- **Found by the browser pass:**
  - *Fixed, 9b25614:* **repair treated "not held here" as "missing".** A member's new note
    was unlisted by the owner's page (which had no key for it yet) and listed again by
    every device that held it, six times a second, 1048 appends in three minutes. Repair
    now acts on knowledge only (tombstones it holds), never on absence; the mirror case
    (a node under a parent not held being moved to the root) is gone with it.
  - *Fixed, 83df753:* **the owner's page held only the store key**, so it could not read
    what a member created until one of the owner's desktops added the store key's wrap. It
    now fetches the key of every share whose marker it holds (at open, at every connect,
    and when a marker or an unopenable blob turns up). Checked in the browser: the owner's
    page shows folders and notes members made while every owner desktop was off.
  - *Fixed, 83df753:* **a document created by someone else did not appear in an open page
    until it reconnected** (a notification carries the blob and not the document's key
    wraps). Such a document is now read again once, which brings its wraps. Checked live.
  - *Fixed, bbdf75f:* a child that became readable a moment after its never-opened
    folder's own change left the folder without a chevron (the app refreshed a parent only
    when its list was loaded). Checked live.
  - *Fixed:* Enter in the password field signs in or unlocks (a9b29a3); the web logs at
    INFO, so the app's own warnings are readable in the console (0a3d2f7); with several
    shares of one store the web names the row "Shared by <owner>", as the desktop does.
  - **The bar was run in the browser** with the owner's desktop off: the owner's page, a
    member's page and a member's desktop renamed, created, moved and deleted in one shared
    folder close together; both desktops ended with identical lists, both pages showed
    them, and no document kept gaining appends afterwards. The owner's desktop, switched
    on again (a binary built from committed code only), converged to the same lists with
    no repair of its own to make, and added the store key's wrap to every document the
    members had made. No title or typed word of any of it is readable on the hosted disk.
- **Found and fixed by that verification** (all committed): a member never learns the
  owner's store name; a new document's wrapped key rides its first `vaultAppend` and is
  stored with it under one lock (a lost answer used to leave a blob nobody could open,
  stalling every reader); the vault link asks the accounts service every two minutes
  whether the grant changed and reconnects with a fresh token (a role change used to wait
  for the token to expire, up to an hour); a second share renames the replica; a root a
  member no longer holds takes no edits on their replica; **access is judged per node**
  (every `Node` an RPC returns carries `access`; a replica holding one share as an editor
  and another as a reader used to let the app's editor take typing in the read-only
  document, shown and saved nowhere), and a role change reaches the running app; a share's
  member gets a member's Share dialog instead of the owner's management dialog. Both were
  checked again in the running app after the fix.

## Decided on 2026-09-21

- **A token per relayed store** (6d95e38). The relay hands a member's token to the owner's
  machine; the account's general token would have been good for the member's other stores
  at the hosted server for an hour. `POST /token` now mints `stores[].token`, the account's
  token with `stores` cut down to that one store, and the relay refuses any token naming
  another. The general token is unchanged.

## Decided by Joe on 2026-09-21, after the verification

- **A removed member's machine**: the PM's recommendation. The folder leaves the explorer
  with a one-line notice ("<title>" is no longer shared with you.), nothing is deleted from
  disk until the replica is removed, and a replica whose every share has ended stays as a
  row that says so and offers "Remove Replica...". Built and tested
  (`manifest.ended_roots`, `Store.ended_roots`, `StoreChangeKind::SharesEnded`); not yet
  looked at in the running app or the browser. A desktop learns of a removal within the
  two-minute grant check, a browser page at its next token refresh or reconnect.
- **Members see each other's addresses**: yes, as today. Who people are to each other in a
  share (nicknames, presence and the rest) is a session of its own, later.
- **The store's name in the clear on the hosted manifest**: fine (it was decided on
  2026-09-16 in `docs/CRYPTO_CONTRACT.md`; a relayed store sends no name at all).

## Waiting for Joe (both answered on 2026-09-21: the go-ahead given and used, the move contract approved)

1. **The go-ahead to merge and deploy.** Not a design question, only "now or not yet":
   `node-document` replaces `master`'s store format, and the change is one way. The first
   time the new version opens a store it rewrites it as node documents (`store.yrs` is kept
   as `store.yrs.migrated`), after which the old version cannot open it; the same happens
   to every hosted store when the new hosted server starts. So shipping means, in this
   order: back up Joe's own stores and the production stores directory; push (hosted
   server, accounts service and web app deploy together; the accounts database gets the
   new optional `HostedStore.tier` field); cut the desktop release (the standing rule) and
   update every device of Joe's before opening a store that another device syncs, because
   a device still on v0.1.1 cannot sync with a migrated store. Nothing is merged until Joe
   says so.
2. **Moving a node out of a share** (Joe, 2026-09-21: "if someone moves a node out of the
   share, it needs to count as an undoable delete", and, correcting the PM: "both the
   desktop and the web app should allow for multiple stores to be open at once", so there
   IS somewhere outside a share on a member's screen). The facts, checked in the code:
   both apps show several stores at once; a drag from one store onto another is ignored
   with a log line and nothing on screen (`app.rs`, "cross-store move not supported yet");
   a member who holds two shares of one store can move a node from one into the other
   today (`moveNode` judges the three documents, all of which are theirs to write), and
   for the first share's other members it vanishes with no undo; the owner can do the
   same into their private part; and a tampered member's client can point a node's
   `parent_id` outside the share, which the owner's devices then complete.
   **The design that follows (the PM's reading B, taken as Joe's decision unless he says
   otherwise; not built):** a move that crosses a share's boundary, to another share, to
   the owner's private part or to another store, is an ordinary delete in the share it
   leaves (a tombstone, which stays in that share's scope and which any editor there can
   undo) and a new node where it lands, made once and never kept in step (so it is no
   mirror); inside one share a move stays a move. Nothing ever leaves a scope, so no
   data-key rotation is needed, and a `parent_id` pointing out of a share is never
   completed by repair. A move between stores cannot be anything else (different stores
   hold different documents), which also gives the cross-store drag its meaning. To
   build: the boundary-aware move in `Tree`/the store layer (subtree copied with new ids,
   content and `data` included), repair refusing to adopt across a boundary, the drag
   between stores in both apps, and somewhere to see and undo what was removed from a
   share (there is no app surface for `undeleteNode` at all yet).

## Known limits (none blocks the design; each is a follow-up)

- A tampered member's client can move a node out of the share by setting its `parent_id`
  to a node outside it (the hosted server sees ciphertext; the owner's devices complete
  the move). See "Waiting for Joe", item 2.
- No data-key rotation when a node or a member leaves a share (they keep what they had;
  the server stops serving them more). Two owner devices giving the same keyless document
  a data key at once need a compare-and-set on `vaultSetDocKeys`.
- A removed member's open connection is judged by its token until it expires (an hour at
  most) if their client does not ask; an honest client asks every two minutes.
- A co-owner is never handed a share's key. The accounts service cannot delete the owner's
  own scoped key grant.
- `derive_kinds`, `DocShape`, `merge_updates` and `document_root` exist in both the server
  and the web client and belong in `pimble-crdt`. Uninitialised orphan node files (481 in
  the family store) are synced as harmless documents and should be purged at migration.
- `docs/ARCHITECTURE.md` still describes the store document in several sections (it says
  so at its top).
- The store row's badge is cut off when the replica's name is long ("Shared by <address>").

## Next

1. **The relay tier is built and verified** (`docs/RELAY_CONTRACT.md`, status and
   "Verification"): headless, in the desktop app and with a member in the browser. What
   is left of it is small: `wss://` has never run locally; a browser refused for its
   origin reads `owner offline`; the twin holds the whole store, not only what is shared.
2. Joe's decisions, then merge, deploy (the accounts database needs `HostedStore.tier`
   applied, and the hosted server and accounts service go together) and release.
3. ~~When rinch #832 (read-only editor switch) merges: `cargo update` the rinch crates in
   the root and in `web/`, and replace the interim read-only handling in
   `crates/pimble-app/src/editor.rs` with `set_read_only`.~~ Done on 2026-09-22 (the
   section at the top).

## A flake to watch

One full `cargo test --workspace` run on 2026-09-21 (made while two local stacks, two app
windows and a browser were running) ended with the `pimble-server` `auth` test target
failing without naming a test or printing a result line. The target passed alone (20 of
20), the whole server suite passed twice, and a second full workspace run was clean. The
relay agent saw one like it in `remote_mounts::a_mount_whose_source_link_is_down...`.
Both look like load. On 2026-09-22 the remote-mounts one
fired on CI for the v0.3.1 release commit (a 10 s `wait_for_mount_state` for the
`Cached` notification after the source server stops; nothing in that release touches
mounts or links, and the full release run had passed locally minutes before). The re-run passed.
**The remote-mounts one is explained and fixed on branch `rhypedb-pin`** (a real race,
not load: the top of this file). The `auth` one is still unexplained.

## Process notes that cost time before

- Agents from an earlier session cannot be resumed after a restart: continue from
  `git diff` with a fresh agent. No history-changing git while an agent works in the tree.
- `pimble-cli` defaults to the person's own app on 7462. Use the wrappers in
  `scripts/local-stack/env.sh`; never the bare binary, never a broad `pkill`.
- The scratchpad does not survive a session: anything a later verification needs goes in
  `scripts/`.

## Earlier: where things stood on 2026-09-16 (master, before node documents)

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
- **2026-09-17: rinch is back on `main`** (`c4845d3`, which carries joeleaver/rinch#831, the
  collaboration caret fix) in the root `Cargo.toml`, `crates/pimble-app/Cargo.toml` and
  `web/Cargo.toml`, both locks at that revision with one `wasm-bindgen` (0.2.128). Joe had
  reported that one client backspacing the line the other's caret was on left that caret
  out in space: a remote change is a block-level replace and a caret inside the replaced
  paragraph was mapped to the start of the *next* one, and rinch-web refreshed the caret
  only from its input handlers. The pull request's branch was deleted at merge, which would
  have failed any fresh-checkout build (CI, the release, jkbase) of the commit that pinned it.
- **2026-09-17: v0.1.1 and a standing rule.** The public download was still v0.1.0, ten
  commits behind production and without the account UI. Joe: update the desktop builds
  every time we push a version (`docs/DEPLOY.md`, "A desktop release with every version";
  also a rule in `CLAUDE.md`). v0.1.1 is `master` with the pins on `main`.
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
