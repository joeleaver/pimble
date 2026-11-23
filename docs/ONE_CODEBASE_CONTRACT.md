# One code base: the browser runs the same Pimble server (draft, 2026-11-22)

Status: **draft; wave 0 built** (2026-11-22, not merged or released; see "Built: wave 0" at the end). Decisions taken so far are under "Decisions"; the rest
are "Questions for Joe", each with a recommendation. Follows docs/CONVERGENCE_CONTRACT.md
("Next: one sync engine for both apps") and docs/STORE_FOLDERS_CONTRACT.md.

Joe, 2026-11-19: every change to how syncing works has been built twice and has drifted.
Joe, 2026-11-22: "we should be using almost the entire same code base in both places, for
everything, honestly the only differences should be tiny", and then: "shouldn't the
browser have pretty much the same server, just running in its own thread or something?"

## The rule

**The browser runs the same Pimble local server the desktop runs.** The handler,
`StoreManager` and `LocalStore`, the sync and vault links, share upkeep, convergence
reports, the keystore and the search index are compiled to wasm and run in a Web Worker.
Its disk is the origin private file system (OPFS), encrypted at rest with a key this browser
remembers. The page talks to it with the same JSON-RPC the desktop app speaks to its
embedded server, over a `MessagePort` instead of a loopback WebSocket. `pimble-app`'s
backend becomes one "local server client" on both targets, and `web/src/vault.rs` (6,695
lines), `vault_backend.rs`, `routed.rs` and the browser's own backend loop go away.

What is left per platform is a short list of adapters (section 6): a disk, a runtime
(spawn, timers, clock), an outbound HTTP client, where keys rest, the transport between app
and server, and the edges of the operating system (file dialogs, menus, the window).

The `CLAUDE.md` rules hold unchanged and become easier to keep: one way to write node
content is the handler's `applyEdit`, in both apps, because there is one handler; everything
stays co-editable because the browser's stores are ordinary replicas, as a desktop's are.
Nothing is hosted as a side effect: the browser's server keeps its stores in the browser,
and its links go only where the desktop's would.

## 1. Why: what each side does today

The UI is shared already (`crates/pimble-app`, about 49,000 lines; about 150 `cfg` lines,
nearly all at platform edges). The divergence is underneath. The desktop app talks to a
Pimble server that holds, indexes, keys and syncs its stores (`pimble-server`, about 20,000
lines, with `pimble-store` and `pimble-search`). The browser has no server, so `web/`
(about 11,000 lines) reimplements what it needs of one in the page: `vault.rs` is both an
encrypted store's only copy (answering `children`, `node`, `create_node`, `move_nodes`,
`transplant_node`, `list_deleted`, `search`, lines 2006-2760) and its sync link (pulling,
appending, reports, out of step, setting aside); plain stores go straight to the hosted
server through a `Router`.

### The sync halves, side by side

`crates/pimble-server/src/vault_link.rs` (3,575 lines) and `share.rs` (1,449) against the
link half of `web/src/vault.rs` (4,297 lines before its tests). The shared rules are
`pimble_rpc::vault` (1,874 lines, tests from 1374).

| Responsibility | Desktop | Browser |
| --- | --- | --- |
| Pulling | `full_reconcile` (1901), `pull_doc` (2059), from `last_seq`, one doc at a time | `open_store` (1237) from 0 at every page load, `catch_up` (1340) from the cursor, 32 at once (`fetch_many`, 4266) |
| Appending | `append_blob` (2451), `seal_plan` (2396) | `append_prepared` (2397), `prepare` (3483), `record_span` (3546) |
| Echoes | source id and remembered seqs (2257-2266, `EchoTracker` 3554) | source id only (1701) |
| Reconnect | every doc unseen, ahead of `pushed_sv`, or dirty (2003-2034) | only docs with a failed append (1413-1424) |
| Gap after a live append | caught up at once (2611-2615), once per state (2657) | nothing |
| Known remote state | `pushed_sv`, `dirty` in `vault-link.json` (1247) | `known_sv`, `unsent`, memory (247-262) |
| Read positions | `link.json` `last_seq` after every entry (1872) | memory; every log read again at each load |
| Full log (`-32006`), lost part group | snapshot and send again (2515-2526), resend whole (2508) | the append fails, resent into the same full log next connect |
| Refused append | kept, marked, waiting with a sentence (2529-2532) | document thrown away and read again (`repull` 2476, `refused` 3582) |
| Lost notifications | `recheck` every 60 s (2974) | none |
| Out of step | every `after/5` (3249) | every 30 s (552) |
| Setting aside | 24 h kept in `convergence.json`, then a snapshot clears the log (3146, 3223) | on another device's word only, no snapshot (3833) |
| Key retries | 5 per connection, a 5-60 s poll (`pull_pending` 3508) | `KeyLook`: 250 ms debounce, 10 s floor, 30 s retry (577) |
| Grants | `refresh_grant` (1035), `grant_shape` every 2 min (1014) | `learn_rows` (964), `take_rows` (1011) at token refresh |
| Share upkeep | `share.rs` riding the link | none, by design |
| Presence sealing | `presence_link.rs` | vault.rs 2600-2668 |
| Encrypted pictures | `pictures.rs` `VaultRemote` | vault.rs 777-845 |

Every row is a place where a fix landed on one side only. The browser lacks the live gap
catch-up (fault 2 of 2026-11-19, fixed in 0.28.2 on the desktop), the full reconnect
resend, `-32006` handling and the recheck (CONVERGENCE wave 3, bugs 1 and 2). The two take
opposite answers to a refused append: the same edit is kept on one device and silently
undone on the other. With the browser running `vault_link.rs` itself, none of these is a
thing to port: they are simply there.

### Found while reading (to fix whatever else happens)

- **A cursor can outrun the document on disk** (desktop). `pull_doc` and
  `handle_remote_notification` write `last_seq` right after an apply (vault_link.rs:2118,
  2314), while the document is written on the 750 ms flush debounce
  (`CONTENT_FLUSH_DEBOUNCE`, handler.rs:108). A crash in between leaves a device that has
  "read through" entries it does not hold, and it never fetches them again; reports heal it
  only while another device that holds them is online. The fault test cannot make this
  crash (its stop flushes). Fix: record a cursor only through what a flush has written.
  More pressing in a browser, where a tab is closed without warning.
- **A comment that says the opposite of the code** (vault_link.rs:2023): "A document's
  first blob is its whole state (`save`...)" above an append of `doc_diff` from an empty
  vector, which leaves parked updates out, while `push_full` (2681) sends `save()`, which
  carries them. Wave 2's rule ("a parked update is not sent on by the device that parked
  it") says the diff is right; `push_full` and the comment are the odd ones out.
- **Two tabs of one browser are one device.** The device id is per browser
  (`pimble.vault-device`, vault.rs:4178): two open tabs share a report slot and each reads
  the other's report as its own (vault.rs:3725), so they never compare. Section 4 removes
  this (one server per browser).
- **The link's state is four files saved at different moments** (`link.json`,
  `vault-link.json`, `convergence.json`, `devices.json`). Not wrong today; worth one record
  when the link is next touched (question 7).

## 2. What does not build for wasm32 today, and the seam each needs

From the manifests and a count of uses (no build was run).

| Crate | Blockers for `wasm32-unknown-unknown` | Seam |
| --- | --- | --- |
| `pimble-server` | `jsonrpsee` server (HTTP, tower, mio); `tokio` full (35 `spawn`/`spawn_blocking`, 72 `Instant`, `tokio::time`); `tokio::fs` (20) and `std::fs` (34); `dirs` (16); `reqwest` (13: tokens, accounts); `tokio-tungstenite` (relay tunnel); `libc` (Linux memory); `flate2` (diagnostics; its pure-Rust backend builds for wasm) | A, B, C, D, E |
| `pimble-store` | `std::fs` (19) and `tokio::fs` in `local.rs`, `local/folder.rs`, `blobs.rs`, `vault.rs`, `manager.rs`; `spawn_blocking`; `notify` (folder watch) | B, C |
| `pimble-search` | `rhypedb-*` (below); `std::fs` (3); threads (the embedder's worker); `tokio` | B, C, F |
| `pimble-plugins` | nothing (core, crdt, sheet, discussion) | none |
| `pimble-client` | nothing: already builds for wasm (jsonrpsee's wasm client over the browser's WebSocket) | adds a `MessagePort` transport (A) |
| `pimble-crypto` | nothing: pure Rust, `getrandom` with `js`, which works in workers | none |
| rhypedb (pinned `6975cb0`) | `rhypedb-storage`: `memmap2`, `std::fs`, a worker thread in `mem.rs:92`; `rhypedb-engine`: background threads (`database.rs:1534`, `2556`, `fulltext_build.rs:702`), `Instant::now`, `std::fs`; `rhypedb-embed`: fastembed and ONNX Runtime | F |

The seams:

- **A. The handler apart from its transport.** Today the handler is reached only through
  jsonrpsee's HTTP/WebSocket server, and `jsonrpsee-core`'s `server` feature spawns with
  `tokio::spawn` (`rpc_module.rs:834`), which has no runtime in a worker. So: a
  transport-free dispatch in `pimble-rpc` (method name and params in, an answer or a
  subscription stream out, the principal attached by the caller), generated from the same
  `methods.rs` trait so there is still one list of methods. The native server feeds it from
  jsonrpsee (auth at the HTTP edge as today); the worker feeds it from its `MessagePort`
  with the `Service` principal the loopback app has today. `PimbleClient` gains a
  `MessagePort` transport (jsonrpsee's client takes custom transports:
  `ClientBuilder::build_with_wasm`), so the app's client is the same type on both.
- **B. A disk.** A `Disk` trait under `LocalStore`, the per-computer state root, blobs,
  vault logs, the keystore's and devices' files: read a file, write a file atomically,
  list, remove, move. Async, because OPFS's directory and handle operations are. Native:
  `tokio::fs`. Browser: OPFS through `createWritable` (a whole-file write that commits
  atomically at `close`, which is exactly the store's write), in a dedicated worker.
  A memory disk for "don't remember this browser" and for tests. `notify`'s folder watch
  stays native (OPFS has no change events; one server per browser has nothing to watch).
- **C. A runtime.** `spawn` (tokio vs `wasm_bindgen_futures::spawn_local`), `sleep` and
  intervals (tokio vs `gloo-timers`), and a clock (`Instant`, which panics on
  wasm32-unknown-unknown, vs `performance.now()`). `tokio::sync` (locks, channels,
  `broadcast`, `Notify`) builds for wasm and stays. `spawn_blocking` becomes the
  runtime's "run this heavy thing" (a thread on the desktop, inline in the worker).
- **D. Outbound connections.** Links already connect with `PimbleClient`, which builds for
  wasm. What is native-only is the relay tunnel (`tokio-tungstenite`), which stays native
  (section 6), and HTTP: an `Http` trait for the accounts service (`reqwest` vs `fetch`,
  the browser sending its session cookie).
- **E. Paths and the process.** `dirs` becomes a `Paths` value handed to the server (the
  data, config and state roots: directories on the desktop, OPFS folders in the browser).
  `local.rs` (finding, joining and starting the embedded server), `auth.rs`'s HTTP edge,
  `folder_watch.rs`, `relay_face.rs`, `relay_tunnel.rs` and the memory probe are
  native-only modules, behind `cfg`.
- **F. Search.** See section 3.

## 3. Search in the browser

**Keyword.** rhypedb does not build for wasm32 today: its storage is files and `memmap2`
(`rhypedb-storage`), its engine runs background threads and reads `Instant`. The browser's
index can start smaller than the desktop's without losing anything it has now (today it is
a lowercase substring scan, vault.rs:2697): the index is derived and disposable, and a
browser's stores are as large as one person's, so **the browser's index lives in the
worker's memory and is built when a store opens**. That needs from rhypedb a build for
wasm32 with in-memory storage, no threads (compaction and the fulltext build inline, or on
a tick the caller drives) and a clock passed in. That is a change to your crate, upstream;
question 4. Until it lands, wave 3 can ship with `pimble-search` behind its existing
interface and a small in-memory BM25 index over the same `IndexUnit`s and the same
English analyzer for wasm, so the two apps rank the same words alike.

**Semantic.** The desktop runs int8 `all-MiniLM-L6-v2` through ONNX Runtime (about 23 MB
of model). In a worker, the options:

- **tract** (pure Rust, builds for wasm, reads the same ONNX file): no JavaScript, one
  model file for both apps; int8 operator coverage needs checking, and it is slower than
  ONNX Runtime (no SIMD threads in a worker without cross-origin isolation).
- **candle** (pure Rust, wasm, BERT models run in browsers today): a converted model
  (safetensors or GGUF, a quantized MiniLM is about 20-25 MB), not the same file as the
  desktop's.
- **onnxruntime-web** through JavaScript: the same model, fastest (WASM SIMD, WebGPU
  later), about 10 MB of runtime, and a JS dependency inside the server's worker.

*Recommended*: tract, downloaded on first use into OPFS, embedding in batches on the
worker's idle ticks, keyword-only until it is ready and wherever it fails, as the desktop
falls back today. The vectors never leave a device (each store owns its index), so the two
apps need not share a runtime, only the model.

## 4. The browser's server: where it runs and how tabs share it

**One server per browser profile**, because two servers on one OPFS store are two
computers on one folder: the store-folder rules would keep it safe, but OPFS sends no change
events, so each tab would see the other's edits only when it next wrote the document, each
would be its own device to the reports, and each would sync the same stores twice.

- A `SharedWorker` would be the natural home, but OPFS's synchronous access handles exist
  only in dedicated workers, and Chrome on Android has no `SharedWorker`.
- *Recommended*: **a leader tab.** Every tab asks for one Web Lock (`pimble-server`); the
  tab that holds it runs the server in a dedicated worker. Other tabs talk to it through a
  `BroadcastChannel` (JSON-RPC text, each tab its own client id); when the leader closes,
  the lock passes, the next tab starts its worker on the same OPFS files and the others
  reconnect, exactly as the desktop app reconnects when the embedded server it borrowed goes
  away (`local.rs`, "hand it back and forth"). A store-folder `computers/` entry is not
  needed: it is one server at a time.
- **When the leader tab closes** (Joe asked, 2026-11-22), nothing it acknowledged is lost:
  - The other tabs' edits live in their own editors' documents; on reconnect each
    reconciles with the new server (the desktop's reconnect path, `doc_session`), sending
    what it lacks.
  - The browser's server writes every update it takes to disk before it answers: appended
    to a per-document log in OPFS (synchronous access handles make that a few
    milliseconds), the debounced whole-document write compacting the log. A tab closed or
    killed inside the debounce loses nothing acknowledged. (The desktop's crash window, wave
    0's cursor bug, is the same class; whether the desktop adopts the log too is decided
    with wave 0.)
  - What the link still owes the cloud is in its state on disk; the next server sends it.
  - The gap is a second or two (loading the worker, opening the stores); editors keep
    taking edits meanwhile and hand them over on reconnect.
  - With the last tab closed nothing runs, as with the desktop app quit; the next open
    syncs from where it stopped.

The worker holds the account's keys, not the page: signing in and unlocking go to the
server as the desktop's `cloudSignIn` does, and no key crosses into the page.

## 5. The browser's stores

Every store in the browser is a replica, as on a desktop that added hosted stores:

- **Encrypted stores** (hosted or relayed): a plain `LocalStore` in OPFS with a vault link
  to the twin, made as `cloudAddHostedStore` makes one; members' partial replicas, scope
  roots, ended shares and `owner offline` all as on the desktop.
- **Plain stores hosted on Pimble Cloud**: a replica with a plain sync link to the hosted
  server, as `addRemoteStore` makes one. Today the page reads them straight from the
  hosted server; afterwards the page reads its own server, and the link keeps it level.
- **Stores the account owns, shared from a computer** (relay tier): the browser is a
  member-side replica like any other. Sharing *from* a browser is not offered: a tab is not
  on reliably, and the relay face is native-only.
- What the browser opens is what the account grants, as today; there is no "Open Store..."
  of a path.

A side effect, welcome: a browser that remembers itself works offline. A reload with no
network opens the stores from OPFS, edits are kept and sent when the link is back, exactly
as on the desktop.

## 6. What stays different, and only as adapters

Each item of the earlier inventory, framed as what becomes of it.

**Goes away when the browser runs the server:**

1. Encrypted-store sync (`vault.rs` link half, about 2,500 lines) and its drift (section 1).
2. The browser's copy of the store (`vault.rs` 2006-2760: reads, moves, transplants,
   deleted list, links) and its access judgement (`AccountStore::node_access`, 360;
   `routed.rs`'s write gate): the handler answers, with `Reach`, as for the desktop.
3. Grants and ended shares (`AccountStore`, `take_rows`, `shares_ended`, vault.rs 289-435,
   964-1064): the link's `refresh_grant`.
4. The `Router` between plain and encrypted backends, `vault_backend.rs` (276) and
   `endpoints.rs` (304): every store is the local server's; links know their endpoints
   from `/token` as the desktop's do.
5. Keys (`session.rs`, `keys.rs`, about 530): the server's keystore, resting on the
   browser's key storage adapter.
6. The personal document (`personal_sync.rs`, 105): the server's `cloudPersonalSync`.
7. Presence and pictures on encrypted stores (vault.rs 2600-2668, 777-845): the server's
   `presence_link.rs` and `pictures.rs`.
8. Search (vault.rs 2697): the server's `search`, with section 3's index.
9. The browser's backend loop (`web/src/backend.rs`, 1,223): replaced by the shared local
   server client. Connection supervision is then one thing: the app's client of its local
   server (`pimble-app/src/backend.rs`'s `Local` today), whatever the transport.
10. Diagnostics (`web/src/diagnostics.rs`, 359): the server's (`diagnostics.rs`, 799), its
    queue on the disk adapter.
11. Share upkeep: an owner's browser keeps shares up like an owner's desktop (question 3).

**Stays, essential, as an adapter:**

- `Disk`: directories and `tokio::fs`; OPFS, encrypted (section 7); memory.
- `Runtime`: tokio; `spawn_local`, `gloo-timers`, `performance.now()`.
- `Http`: `reqwest` with a session bearer; `fetch` with the session cookie.
- `KeyRest`: `keys.json` (0600); wrapped under the remembered device key in IndexedDB, or
  memory only (section 7).
- App-to-server transport: loopback WebSocket with the embedded-server hand-over
  (`local.rs`); `MessagePort` to the worker, `BroadcastChannel` from other tabs.
- The app's own settings (`persistence.rs`: `state.json` vs `localStorage`): one settings
  record over a key-value adapter.
- Plugins: wasmtime vs the page's WebAssembly (`pimble-plugin-host`), and where plugins are
  found (`plugins.rs` `place`, 1157 vs 1459).
- Operating-system edges: file dialogs and reading files (pictures, import, export, SVG
  import, pasting), the native menu bar vs rinch-web's (`menus.rs`, `web/src/menu.rs`), the
  window, self-update (`update.rs`), the MCP agents' config files (`agents.rs`).
- Desktop-only server parts: the folder watch, the relay face and tunnel, mounts of local
  paths, plain links to arbitrary servers, the HTTP edge and its token, the agents' keys.
- Account pages: signup, verification and recovery live in the browser today and nowhere
  on the desktop. *Recommended*: they become server RPCs over the shared accounts client,
  and one set of account views in `pimble-app`, so the desktop gains them too.

## 7. Data at rest in the browser: a remembered device (decided)

Decided by Joe, 2026-11-22: **encrypted at rest, and no password at every load.**

- **The device key.** On the first unlock in a browser, the worker makes an AES-GCM key
  with WebCrypto, `extractable: false`, and stores the `CryptoKey` in IndexedDB (workers
  have both). It never leaves WebCrypto. It wraps two things, kept beside it in IndexedDB:
  the account's unwrapped keys (and the store keys the keystore holds), and the **disk
  key**, a random 32-byte key that encrypts what the server keeps in OPFS.
- **The disk.** The OPFS adapter seals every file it writes with the disk key
  (XChaCha20-Poly1305 from `pimble-crypto`, the associated data the file's path and a
  format version) and opens every file it reads; `LocalStore` and everything above it see
  plain bytes. File and directory names stay as they are (store and node ids, never
  titles or text). The in-memory index is never written.
- **A reload** unwraps the account keys and the disk key with the device key and starts the
  server: no password.
- **The password is asked** on a new browser, after "Sign Out", and after "Forget This
  Browser" (Settings and the account page). Each deletes the device key first (so OPFS is
  unreadable even if clearing it fails or is interrupted), then clears OPFS and IndexedDB.
- **"Don't remember this browser"** at unlock (shared computers): no device key; the server
  runs on the memory disk and keeps keys in memory, today's behavior.
- **What it protects**: OPFS files copied or left behind without the profile's key store
  (a disk image or backup of the storage folder alone, leftovers after sign-out or
  forgetting). **What it does not**: anyone at the browser profile while it is signed in
  (the same as the session cookie), a copy of the whole profile (the key store travels
  with it), and script running in Pimble's own origin. Said in those words on the unlock
  page beside the checkbox.
- **The session and the device key.** A session lasts 30 days and is pushed back on use at
  most once a day (`pimble-cloud/src/session.rs:18-23`); the server mints an hour's JWT
  from it before every connect, as the desktop's `CloudSession` does. When the session has
  expired and the device key remains, the server does what the desktop does
  (`SessionExpired`, vault_link.rs:582-594): the stores open and edit from OPFS, links wait,
  and the app asks to sign in again. Signing in needs the password (it derives the
  `auth_key` the service checks); nothing else is asked, the device key and OPFS are kept,
  and what waited is sent. A sign-in as **another** account first forgets this browser.
- **`pimble-crypto`** needs nothing new beyond an associated-data helper for local files:
  it is pure Rust and builds for wasm already. WebCrypto (`SubtleCrypto`, `CryptoKey`,
  `AesGcmParams`) and IndexedDB are `web-sys` calls in the browser's `KeyRest` adapter.

## 8. Proving it, and keeping both apps on one path

- **The server's test suite runs on the memory disk too.** Every `pimble-server` and
  `pimble-store` integration test takes a disk; CI runs them on directories and on memory,
  so the browser's configuration is tested natively, at full speed.
- **The randomized fault test** (`crates/pimble-server/tests/link_faults.rs`) gains a
  device configured as a browser (memory disk, the browser's runtime timings, sign-in
  without a session token). Because the link is one code base, the guarantee the test gives
  holds for both. It also gains the crash it cannot make today: a device stopped without
  its final flush, its disk as of the last write (the cursor bug in section 1).
- **A wasm smoke test** in CI: the server built for `wasm32-unknown-unknown`, run headless
  in a worker (wasm-bindgen-test with a browser runner), opening a store on OPFS, applying
  an edit, reloading, finding it.
- **No second path.** After wave 5, `web/` holds only adapters and the page's entry: a
  check in CI fails if `web/src` calls any store RPC's implementation, a vault RPC, or
  `pimble_crdt::Tree` directly. A change to how stores, sync or keys work is a change to
  the server, and both apps ship it with the next build (the web is deployed with every
  release already).

## 9. Waves

Each ships working.

0. **The two bugs** (section 1), on the desktop, independent of the rest: cursors only
   through what is flushed; the first-blob comment and `push_full` agreed. Proven by a
   fault-test crash without a flush.
1. **The handler apart from its transport** (seam A), desktop only. The transport-free
   dispatch; the native server feeds it; `PimbleClient` over an in-process channel in tests.
   Proven by the whole server suite unchanged, and the app and MCP on a release build.
2. **Disk, runtime, HTTP, paths and key rest** (seams B-E) on the desktop. `LocalStore`,
   the state root, vault logs, blobs, keystore and devices over `Disk`; every spawn, timer
   and clock through `Runtime`; `reqwest` behind `Http`; `dirs` gone from the server's
   core. Proven by the suite on directories and on the memory disk, and Joe's stores by hand
   after a backup.
3. **The server builds for wasm32, with keyword search.** Native-only modules behind
   `cfg`; the in-memory index (rhypedb's or the stopgap); the worker entry point; the
   `MessagePort` transport. Proven by the wasm smoke test and the suite on the memory disk.
4. **The browser on its worker server, behind a switch**, beside the old path: the leader
   tab, OPFS with the device key, sign-in through the server, replicas of the account's
   stores, `?server=worker` or a Settings switch. The old device id and answered reports
   handed over from `localStorage` at the first start, so the browser keeps its report slot.
   Proven by the local stack in browsers (Chrome, Firefox, Safari): the sharing
   walk-through of `scripts/local-stack/README.md`, a reload with no network, two tabs and
   the leader closed, forget and sign-out leaving OPFS empty, and `link_faults.rs` with the
   browser-configured device.
5. **Delete the old path**: `vault.rs`, `vault_backend.rs`, `routed.rs`, `endpoints.rs`, the
   browser's backend loop, `session.rs`/`keys.rs`/`personal_sync.rs`/`diagnostics.rs` as
   they are, the switch. The no-second-path check turned on.
6. **Semantic search in the browser** (section 3).

Later, on their own: one record for the link's state (question 7); account pages as server
RPCs and shared views; a sans-I/O core for the links if a deterministic simulation is wanted
(question 8).

## 10. Risks

- **Size and start-up.** The worker's wasm carries the server, the crdt, crypto and the
  index: several MB, cached after the first load. A store opens by reading its documents
  from OPFS and decrypting them: Joe's 674-node store is a few MB, well within a worker,
  but a store's open time should be measured in wave 4.
- **OPFS write semantics differ by browser.** `createWritable` commits on `close`; a tab
  killed mid-write loses that write but never leaves half a file, which is what the store
  needs. Safari's support for `createWritable` in workers is recent; wave 4 checks the three
  engines and falls back to sync access handles (dedicated worker) where needed.
- **Storage eviction.** A browser may clear an origin's storage under pressure unless
  `navigator.storage.persist()` is granted. The browser's stores are replicas, so eviction
  loses only what was not sent yet; the app asks for persistence at the first remembered
  unlock and says when it was refused.
- **Wave 2 touches every edit on the desktop.** The suite unchanged, the fault test over
  200 seeds, a backup, and the release held a day on Joe's machine before it is tagged.
- **The leader tab's hand-over** has to survive a leader killed mid-write; the next server
  opens the same files, and the store-folder rule (merge before write) already covers a
  file it finds changed.

## Decisions

1. **The browser runs the same server, in a worker, on OPFS** (Joe, 2026-11-22: "shouldn't
   the browser have pretty much the same server, just running in its own thread or
   something?"). This document's primary design.
2. **At rest in the browser: encrypted, with a remembered device** (Joe, 2026-11-22, "I'd
   much prefer it didn't prompt for password every single time"): a non-extractable
   WebCrypto device key in IndexedDB wraps the account keys and the disk key; the password
   only on a new browser, after sign-out, or after "Forget This Browser"; "Don't remember
   this browser" keeps today's memory-only behavior. Section 7.

## Questions answered (Joe, 2026-11-22: "yes all recommendations")

Each below is decided as its recommendation says.


1. **One server per browser through a leader tab** (Web Locks, a dedicated worker,
   `BroadcastChannel` to the other tabs), rather than a `SharedWorker` or a server per tab.
   *Recommended*: the leader tab; it works on every engine, Android included, and keeps
   OPFS's synchronous handles available.
2. **Plain hosted stores become replicas in the browser** (section 5), so the page reads its
   own server for every store and works offline. *Recommended*: yes; it is what makes the
   app's backend one thing, and the replica is encrypted at rest like the rest.
3. **An owner's browser keeps shares up** (wraps, scope sets, handing keys to members), as
   it will be running the same `share.rs`. Today only an owner's desktop does, so an owner
   who uses only the web leaves members' new documents unreadable to their other devices
   until a desktop comes online. *Recommended*: yes, no switch.
4. **rhypedb for wasm32**: in-memory storage, no threads, a clock passed in, as a change to
   your crate (upstream). *Recommended*: yes, as its own piece of work beside waves 1-2; the
   stopgap BM25 index ships wave 3 only if rhypedb is not ready, and is deleted once it is.
5. **Semantic search in the browser with tract**, downloaded on first use (about 23 MB),
   keyword-only until then. *Recommended*: yes, wave 6; onnxruntime-web if tract cannot run
   the int8 model at a usable speed.
6. **Account pages in the desktop**: signup, verification and recovery as server RPCs and
   shared views. *Recommended*: yes, after wave 5; the desktop has no recovery today.
7. **One record for the link's state** on the desktop (`sync-state.json` in the state
   directory, migrated once from the four files, kept as `*.migrated` for a release).
   *Recommended*: yes, done with wave 0's cursor fix, since both change when cursors are
   written.
8. **A sans-I/O core for the links** (the earlier draft's engine: decisions free of I/O,
   driven by both apps), now that both apps run the same link. It is no longer needed for
   one code base; its value is a deterministic simulation whose seeds replay timing too.
   *Recommended*: not now; revisit if the fault test's timing-dependent seeds start to
   cost us.
   Joe asked (2026-11-22) whether it would prepare a mobile app, which he does not expect to
   run its own server process. It would not: after waves 1-3 the server is a library, not
   a process, and a phone app hosts it on a background thread in-process, a third host
   beside the desktop's loopback WebSocket and the browser's worker. Mobile is the easier
   host (a real filesystem, threads, native networking; rhypedb and the model run natively).
   A sans-I/O core would only serve a mobile client that syncs without the server, which is
   the browser's second copy again. Mobile's real constraint, a few seconds of background
   time on iOS, is met by a bounded server call ("sync what is pending, save, stop"). The
   adapters of wave 2 are designed with three hosts in mind.
9. **Plain replica links stay as they are** (`sync_link.rs`): they hold no belief about the
   remote, and in the browser they are simply the plain-store replicas of question 2.
   *Recommended*: yes.

## Built: wave 0 (2026-11-22, not merged or released)

Section 1's two bugs, and decision 7 with them. Desktop only (`pimble-store`, `pimble-server`).

### Cursors only through what is on the disk

**Before**: `record_last_seq` wrote a document's `last_seq` (to `link.json`) right after each
apply (`pull_doc`, `handle_remote_notification`) and after each of this device's own appends
(`append_blob`), while the document itself waited for the 750 ms content flush. A crash in
between left a device "read through" entries its disk did not hold; nothing ever fetched them
again, and only another device's answer to its report (while that device was online) healed it.

**Now** (`crates/pimble-server/src/vault_link.rs`, `Progress`): a cursor that moves is noted in
memory (`Progress::note_cursor`, `last_seq`) and written a moment later (`CURSOR_SAVE_AFTER`,
1 s, a branch of the live loop; also with every other save) by `Progress::save_if_unsaved`,
which first puts the documents of every cursor that moved forward on the disk
(`StoreManager::flush_nodes_durably` -> `LocalStore::flush_nodes_durably`: written if dirty,
each file and the `nodes/` directory synced) and only then writes the cursors, in the same
atomic, durable write as the rest of the link's state. A failed flush keeps the record's
older cursors (reading again is harmless). The reconcile reads cursors from memory
(`Progress::last_seq`), loaded from the record when the link starts. A clean stop aborts the
link task, so up to a second of cursors may be read again after a restart: entries that
merge to nothing.

**Checked for the same class** (link state that describes document contents, persisted
ahead of the documents): `pushed_sv` (what the remote holds), `dirty` and `answered` (what
was sent) describe the remote, which holds what they say whatever this disk lost; a lost
local write makes them understate this device, never overstate the remote. `dirty` saved
before its document only causes one more push. A set-aside record saved before its document
reparks the update at the next open and sets it aside again at once, quietly. This device's
own report can overstate after a crash, and is corrected at connect (`compare_at_connect`
sends every report that differs from what is held). So the cursor was the one place; the
own-append cursor (`append_blob`) was the same bug as the pulled one and goes the same way.

### The first blob and `push_full`

Wave 2's rule decides it: a parked update is not sent on by the device that parked it. The
reconcile's first blob was already the diff from an empty vector; `push_full` (a live change
with no bytes, a document the remote never saw reached from `push_update`, a repair loop's
`Resend`) sent `save()`, which carries parked updates. It now sends the diff from an empty
vector too, and the comment says so. Snapshots keep `save()` (`full_doc_state`): a snapshot
replaces the log entries that carried a parked update, so it must carry it.

### One record: `sync-state.json` (decision 7)

`<data dir>/pimble/stores/<store id>/sync-state.json`, `crates/pimble-store/src/sync_state.rs`:

```json
{
  "version": 1,
  "link":        { "last_sync": "...", "last_seq": { "node:<id>": 12 } },
  "device":      { "device": "<uuid>", "answered": { ... } },
  "vault":       { "epoch": "...", "pushed_sv": { ... }, "dirty": [], "dirty_all": false, "cursor_version": 1, "rekeying": [] },
  "convergence": { "parked": { ... }, "set_aside": { ... } }
}
```

Each section holds exactly what its old file held. **What moved**: `link.json` -> `link`
(written by `LocalStore::write_sync_config`/`clear_sync_config` for `last_sync`, by the vault
link for `last_seq`); `vault-link.json` -> `vault`; `convergence.json` -> `convergence`; this
store's entry of the server-wide `devices.json` -> `device` (`crate::devices` reads and writes
it there; the folder watch's presence name comes from it too). **What did not move**: the
folder's `sync.json` (remote, mode, `via_relay`, `vault_key_id`, access, roots: shared by every
computer opening the folder), `people.json` (derived) and `index/`. Writes go through
`sync_state::update`: one lock per file in the process, read, change one's own sections,
write to a temporary, `fsync`, rename, `fsync` the directory. The vault link writes its
sections in one such write. A record that does not parse is set aside as
`sync-state.json.unreadable` and the link starts over (as an unreadable file did before).
`forget_progress` (unlink, relink, a relay stopped or started) removes `vault`,
`convergence`, `link.last_seq` and the answers, keeping the device id. A store with no state
directory (a bare `LocalStore`; never on a server) keeps it all in memory.

**Migration** (`sync_state::migrate`, run by `LocalStore::open_in` right after the folder's
per-computer migration, once per store): with no record yet, the record is built from
`link.json`, `vault-link.json`, `convergence.json` and the store's entry of `devices.json`
(beside the state root) and written in one durable write; then each of the three old files
still there is renamed `<name>.migrated` (`.migrated.2`, ... if taken; never written over,
never deleted). With a record already there the old files are only renamed, never read: a
crash between the write and the renames finishes at the next open with nothing read twice.
Nothing to fold in (a store never linked) writes no record. An old file that does not parse
is not carried and is kept, renamed. `devices.json` holds every store's entry: it is read at
each store's migration and never written or renamed. The folder migration
(`migrate_per_computer_state`) counts an existing record as "this computer has its own".

### Proof

`crates/pimble-server/tests/link_faults.rs` gains **a crash** (rolls 76-81, taken from the
restart range): once the device is connected (its own edits on the remote), another device
edits a document it holds, and 50-500 ms later (inside the flush debounce) the device's
directory is copied while it runs, its state root first and the store's folder after (every
file is written whole, so a cursor written after its document is never copied newer than the
document), the server is stopped, what the stop wrote is thrown away, and the device starts
from the copy (no search index: built again). Then, with every other device deaf to reports
(`LinkFaults::set_deaf_to_reports`: the link hears reports and sends its own, and answers
none), it must hold again within 30 s every marker it held just before the crash: from the
log alone. The restart fault's "record of what the remote holds deleted / put back from an
older copy" now acts on the record's `vault` section.

- **Before the fix** (the original code, with only the deaf switch and this test added, in a
  scratch worktree): seeds 0..12 failed on **1, 5, 7 and 11**, each with the crashed device
  synced and lacking the change it held before the crash, e.g. seed 1: "after its crash
  alice-2 never got back from the log what it held before it (synced: true):
  "seed1-step2-bob-before-a-crash"".
- **After**: seeds 1, 5, 7 and 11 converge (2, 2, 1 and 4 crashes); the sweep over 200 seeds
  (`PIMBLE_LINK_SEEDS=200 PIMBLE_LINK_PARALLEL=4`, 854 s, run twice: before and after the
  parked-update fix below): all 200 converged both times, with 219 crashes
  among them.

Migration: `crates/pimble-store/tests/sync_state.rs`: a vault link's four files become one
record (sections byte-for-byte the old values, old files renamed and kept, `devices.json` and
the folder untouched, the store reads its link through the record, a later write keeps the
other sections); a relay link; a plain link (only `link`); a store never linked (no record;
linking it later writes the record, not the old files); runs once (a second open changes
nothing; a file an older Pimble writes again is renamed `.migrated.2`, the record wins); a
crash before the record (a half-written temporary) and a crash between the record and the
renames (the record is not rebuilt) lose nothing; an unreadable old file is kept and not
carried; state still in an older folder reaches the record in one open. Existing tests moved
to the record: `store_folders.rs`, `convergence.rs`, `remote_mounts.rs`, `relay.rs`,
`vault_link.rs`.

### What a first run does to existing stores

For every store that opens, in `<data dir>/pimble/stores/<store id>/`: `link.json`,
`vault-link.json` and `convergence.json` (whichever exist) are folded into a new
`sync-state.json` and renamed `link.json.migrated`, `vault-link.json.migrated`,
`convergence.json.migrated`. `<data dir>/pimble/devices.json` is read, not changed. Nothing in
any store's folder changes. Going back to an older version after that loses only positions
and beliefs that heal (it reads every log once from the start and offers every document
again); the device id survives because `devices.json` is untouched.

### Found beyond the brief, fixed

- **An update that only parks was never written** unless something else changed its
  document: `LocalStore::apply_node_update` marked a document dirty only when something
  integrated changed, and an update waiting behind a gap changes nothing integrated, though
  the saved document carries it. With the cursor written behind its documents, that would have
  let a cursor cover an entry the disk lacked: the same bug through another door. A parked
  update now marks its document dirty and schedules the content flush
  (`handler::apply_node_update_from`). Found by `convergence.rs`
  `a_gap_nobody_can_fill_is_set_aside_and_leaves_the_log`, which until now passed because the
  `modified_at` stamp of the debounced flush happened to dirty the document before its restart;
  saving the link's state sooner moved the restart ahead of it. Test:
  `pimble-store` `local::tests::a_parked_update_is_written`.

### Found beyond the brief, not done

- The hosted server's and the relay face's vault log appends are written and flushed but not
  `fsync`ed before the append is answered. A process crash keeps them; a power loss could drop
  the tail of a log after its answer, and the next append would reuse a sequence number that
  readers already passed. Not changed here (the hosted server's write cost is Joe's call).
- `CLAUDE.md`'s phase 2a paragraph still says the vault link "records in
  `<store>/vault-link.json`"; it lives in `sync-state.json` in the state directory now.
- Not done: the fault test's crash is the whole device's (a process crash); a crash between a
  document's write and its `fsync` on power loss is not simulated. The browser's vault client
  (`web/src/vault.rs`) keeps its cursors in memory and is unchanged (waves 4-5).

## Built: wave 1 (2026-11-22, not merged or released)

Seam A, desktop only: the handler apart from its transport. `pimble-rpc`, `pimble-rpc-macros`
(new), `pimble-server`, `pimble-client`.

### The dispatch

`crates/pimble-rpc/src/dispatch.rs` (with `dispatch/native.rs` and `dispatch/session.rs`):

- `PimbleApiServer`: the trait a server implements, the same method signatures as before
  (`&self`, `ext: &Extensions` when the method declares `with_extensions`, its arguments,
  `Result<R, ErrorObjectOwned>`). A subscription takes `pimble_rpc::dispatch::PendingSubscriptionSink`
  in place of jsonrpsee-server's and returns `dispatch::SubscriptionResult`. `Send + Sync`
  natively (`#[async_trait]`), not on wasm32 (`#[async_trait(?Send)]`).
- `dispatch::call(server, ext, method, params) -> Result<Box<RawValue>, ErrorObjectOwned>`: a
  wire name (`pimble_applyEdit`) and the raw params in, the bytes of the wire's `result` (serde_json
  of the method's answer, as jsonrpsee writes it) or the error out; an unknown name is `-32601`.
- `dispatch::subscribe(Arc<server>, ext, method, params) -> Option<Subscribing>`: the server's
  subscribe future (`run`), and an `Opening` with the server's `Decision` (accept or reject) and
  the receiver of its `SubscriptionMessage`s. `SubscriptionSink` (`send`, `is_closed`, `closed`)
  and `PendingSubscriptionSink` (`accept`, `reject`) are tokio `sync` channels, which build for
  wasm32; dropping the receiver is what the server reads as the subscriber gone.
- The principal is the caller's: `Extensions` (`http::Extensions`, the type jsonrpsee hands its
  methods) carrying the `Principal`, attached by whatever transport took the call.
- `dispatch::METHODS`: every wire name, with its kind (a call, or a subscription with its
  notification and unsubscribe names).
- `dispatch::native::into_rpc` (native only, and the trait's provided `into_rpc()`): jsonrpsee's
  `RpcModule`, one registration per `METHODS` entry, each calling the dispatch. A subscription's
  decision is carried out on jsonrpsee's own pending sink and its messages forwarded to
  jsonrpsee's sink until either side goes. `PimbleServer::start` serves this module behind the
  same HTTP edge (`AuthLayer`), so auth and the principal reach every RPC as before.
- `dispatch::session::serve(server, ext, ServerEnd)` / `serve_with(server, ext, stream, send)`: a
  whole JSON-RPC connection over text in and text out: calls, batches of calls, subscriptions,
  notifications, unsubscribes, all polled inside its one future (no `spawn`, no clock).
  `session::channel()` makes an in-process pair.
- `PimbleClient::in_process(ClientEnd)` (`crates/pimble-client/src/in_process.rs`): the same
  `PimbleClient` over that channel pair, a jsonrpsee `Client` built with `build_with_tokio`
  natively and `build_with_wasm` on wasm32 (`TransportSenderT`/`TransportReceiverT` over the
  channels). `PimbleServer::start_in_process()` builds the handler without binding a port, and
  `PimbleServer::connect_in_process(ext)` serves one connection on a task and answers its client.

### One list of methods

`methods.rs`'s trait is still the list, written with jsonrpsee's `#[method]`/`#[subscription]`
attributes. `#[pimble_api(namespace = "pimble")]` (`crates/pimble-rpc-macros`, a proc macro)
reads it and emits the trait under jsonrpsee's `#[rpc(client, namespace = "pimble")]` (the
client is generated exactly as before), `PimbleApiServer`, `METHODS`, and the two dispatch
functions (`__call`, `__subscribe`), every one from the same items. A name defined twice is a
compile error; an attribute key the macro does not dispatch (aliases, blocking methods,
`param_kind`) is a compile error rather than half-honored. Params are decoded as jsonrpsee's
server macro decodes them (its `render_params_decoding`, over the same `jsonrpsee_types::Params`):
a JSON array by position with `optional_next` for an `Option`, a JSON object by name with
snake_case and lowerCamelCase aliases, the same `-32602` errors. The test
`pimble-rpc` `dispatch::tests::every_method_of_the_trait_is_dispatched` reads `methods.rs` as
text and fails unless every `#[method(name = ...)]` and `#[subscription(name = ...)]` it spells
out is in `METHODS` with the right kind, and every unsubscribe too.

### Changes by file

- `Cargo.toml`: the `pimble-rpc-macros` member.
- `crates/pimble-rpc-macros/` (new): the macro.
- `crates/pimble-rpc/Cargo.toml`: the macro, `async-trait`, `futures`, `tokio` (`sync`,
  `macros`), `http`, serde_json's `raw_value`.
- `crates/pimble-rpc/src/methods.rs`: `#[pimble_api]` in place of the two `cfg_attr(rpc(...))`;
  the trait's items are unchanged. `lib.rs`: `pub mod dispatch`.
- `crates/pimble-rpc/src/dispatch.rs`, `dispatch/native.rs`, `dispatch/session.rs` (new).
- `crates/pimble-server/src/handler.rs`: the sink types from `pimble_rpc::dispatch` (imports
  only; no method changed). `presence.rs`: the same.
- `crates/pimble-server/src/server.rs`: the handler's construction moved out of `start` into
  `make_handler` (unchanged); `start_in_process`, `connect_in_process`. A server with no port
  says `hands_over: false`.
- `crates/pimble-client/`: `in_process.rs` (new), `PimbleClient::from_rpc_client`, `tokio`
  (`sync`).
- `crates/pimble-server/tests/in_process.rs` (new).

### Proof

- `crates/pimble-server/tests/in_process.rs`: a server started with no port, reached through
  `PimbleClient::in_process`: a store created, closed and opened again, a node read, an
  `applyEdit` by one connection heard as `ContentUpdated` (with its `source_client_id`) by
  another's `subscribeStoreChanges`, an unsubscribe answered; a reader principal reads and
  subscribes, and is refused a write with `READ_ONLY_REFUSAL` as the whole message, a store
  it has no grant on (read and subscription) and a `Service`-only method (`-32004`); and the
  same fifteen requests (unknown method, bad, missing and named params, reads, a refused and an
  accepted write, a failing `createStore`, a bad and a refused subscription, an unknown
  unsubscribe, `serverInfo`) answered by jsonrpsee's path (`into_rpc().raw_json_request`) and
  by the session give the same bytes, for a service and for a reader principal (subscription
  ids aside: jsonrpsee draws a random integer, the session counts).
- **Byte-for-byte with the code before**: a capture test (not kept) sent 21 requests per
  principal plus a subscription and an edit's notification through `into_rpc().raw_json_request`
  on this tree and on `884b834` in a scratch worktree; with only what differs per run masked
  (timestamps, yrs bytes, uuids, subscription ids, the index's progress), the 43 answers and
  notifications were identical, keys in the same order.
- `cargo test --release -p pimble-server -p pimble-rpc -p pimble-client`: 312 passed, 0
  failed, 1 ignored (every pimble-server test file unchanged, `test_isolation.rs` included,
  plus `in_process.rs`). The existing tests that call `handler.into_rpc()` and
  `RpcModule::call`/`subscribe_unbounded` now go through the dispatch.
- `cargo build --release -p pimble-app -p pimble-cli -p pimble-mcp`: builds.
- `cargo check --target wasm32-unknown-unknown -p pimble-rpc -p pimble-client` and
  `cd web && cargo check --release --target wasm32-unknown-unknown`: both check (`web/Cargo.lock`
  gains the macro crate). The dispatch and the session build for wasm32; `dispatch::native`
  does not exist there.

### What still needs a runtime, for wave 3

- **Nothing in the dispatch or the session spawns.** `dispatch::native` is the one place
  jsonrpsee-core's `server` feature runs (and with it jsonrpsee's `tokio::spawn` per
  subscription); it is native-only. Before this wave jsonrpsee's generated code also spawned
  `pending.reject(e)` when a subscription's params did not decode; the generated dispatch
  rejects inline.
- **The handler's own subscribe methods spawn**: `subscribe_presence`,
  `subscribe_cloud_connection` and `subscribe_app_links` start a forwarding task each with
  `tokio::spawn`, and `hand_over` spawns its stop. These, with the rest of the handler's spawns
  and timers, are wave 2's `Runtime`.
- `RpcHandler`'s `impl PimbleApiServer` uses `#[async_trait]` (Send); on wasm32 the trait is
  `?Send`, so the impl takes the same `cfg_attr` when the server first builds for wasm32.
- `PimbleClient::in_process` builds with `build_with_wasm` on wasm32 (`spawn_local`); the
  `MessagePort` transport is a second pair of `TransportSenderT`/`TransportReceiverT` beside
  the channels, and the worker's side is `session::serve_with` over the port's messages.

### Found beyond the brief, not done

- `subscribeNodeChanges` on a store that is not open is accepted for a `Service` principal (the
  parity test's request 13): `reject_if_vault` passes an unknown store and nothing else looks.
  A subscription to nothing; unchanged here, since the wire had to stay the same.
