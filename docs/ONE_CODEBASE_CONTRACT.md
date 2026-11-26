# One code base: the browser runs the same Pimble server (draft, 2026-11-22)

Status: **draft; waves 0, 1, 2 (with its runtime), 3 and 4 built** (2026-11-22 and 23, not merged or released; see the "Built" sections at the end; wave 4, the browser on its own server, is the web app's default since its follow-ups, the old path `?server=old` until wave 5; what it left undone and what needs Joe's word are at the end of each section). Decisions taken so far are under "Decisions"; the rest
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
  server, made by `cloudAddHostedStore` and held through the account (built in wave 5, part 1). Today the page reads them straight from the
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
  found (`plugins/place_desktop.rs` vs `plugins/place_browser.rs`).
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

## Built: wave 2 (2026-11-22, not merged or released): seams B, D, E and key rest

Desktop only, behavior unchanged. **The runtime (seam C) is not built**: see "What remains" at
the end. Everything else the wave names is: a disk under every file the server keeps, outbound
HTTP behind a trait, paths handed in, and where the keystore rests. A new crate,
`crates/pimble-host`, holds the seams; the server takes them in `ServerConfig` and the
handler's construction, never as globals.

### The seams (`crates/pimble-host`)

- **`Disk`** (`src/disk.rs`): a cheap `Clone` handle over `Arc<dyn DiskBackend>`.
  `DiskBackend` (async; `#[async_trait]`, `?Send` on wasm32) is what a host implements:
  `read`, `read_range(path, offset, len)`, `metadata -> Metadata { len, is_dir, modified }`,
  `list(dir) -> Vec<Entry { path, is_dir }>`, `create_dir`, `create_dir_all`, `write` (in
  place, `std::fs::write`), `write_private` (mode `0600`, set at creation and again after),
  `append` (creating), `write_at(path, offset, bytes)`, `sync_file -> Ok(bool)` (fsync; `false`
  when the file would not open), `sync_dir` (best effort), `rename`, `copy`, `remove_file`,
  `remove_dir_all`, `kind() -> DiskKind { Native, Memory, Other }`. Three provided
  compositions a host may override: `write_atomic(path, bytes, TmpName)` (temporary, rename,
  temporary removed on failure; `TmpName::Unique` = `<stem>.<12 hex>.tmp`, the store folder's
  and state directory's naming; `TmpName::Extension` = `with_extension("tmp")`, the vault
  files' naming), `write_durable` (the same with the temporary synced before the rename and the
  directory after: wave 0's `sync_state::write_durably`, now this), `write_private_atomic`
  (parent made, `<path>.tmp` at `0600`, renamed: `fs_util::write_atomic_0600`'s sequence).
  The handle adds `read_to_string`, `exists`, `is_dir`, `remove_path`.
  - `NativeDisk` (`src/disk/native.rs`): `tokio::fs`, each call the one the code made before.
    The one deliberate difference: `append`, `write_at` and `write_private` `flush()` the tokio
    file before dropping it (tokio otherwise finishes the write in the background after
    `write_all` returns; the vault append already flushed).
  - `MemoryDisk` (`src/disk/memory.rs`): a `BTreeMap` of normalized paths, keeping a
    filesystem's rules (a parent must be a directory, `create_dir` refuses what exists, a
    directory is not read, a rename replaces a file and moves a directory's subtree, a
    non-empty directory is not renamed over). `MemoryDisk::with_roots(pred)` takes directories
    a host hands it as existing. Synchronous `*_now` methods, which the async trait and the
    tests' `testfs` both call.
- **`Paths`** (`src/paths.rs`): `{ data, config, state }`, each already `.../pimble`;
  `Paths::native()` (the platform's `data_local_dir`/`config_dir`, `dirs`), `Paths::new`,
  `Paths::in_dir(dir)` (a test's layout), and `replicas()`, `relay()`, `keystore()`,
  `credentials()`, `agents()`.
- **`KeyRest`** (`src/key_rest.rs`): `load() -> io::Result<Option<Vec<u8>>>`, `save(bytes)`,
  `describe()`. `FileKeyRest { disk, path }` (`keys.json`, `write_private_atomic`) and
  `MemoryKeyRest`.
- **`Http`** (`src/http.rs`): `send(HttpRequest { method, url, bearer, json }) ->
  Result<HttpResponse { status, body }, HttpError>`. `ReqwestHttp`: one `reqwest` client,
  rustls, 15 s timeout, as `cloud.rs` built it.

### How each host implements them

- **Desktop** (done): `NativeDisk`, `Paths::native()`, `FileKeyRest` on the native disk at
  `<config>/pimble/keys.json`, `ReqwestHttp`. `ServerConfig::default()` gives exactly these.
- **The browser's worker** (sketch, wave 4): an `OpfsDisk` in the dedicated worker, walking a
  path's components as directory handles from the OPFS root (`Paths` rooted at OPFS folders:
  `data = /pimble`, `config = /pimble-config`). Reads through sync access handles; `write`
  and the three compositions overridden as one `createWritable()` + `close()` (atomic at close,
  so no temporary and no rename), `append`/`write_at` through a sync access handle at an
  offset (the per-document log of section 4 is an `append` before each answer), `sync_file` a
  handle `flush()`, `sync_dir` nothing; every file sealed with the disk key (section 7) in the
  backend, so everything above sees plain bytes. "Don't remember this browser": `MemoryDisk`.
  `KeyRest`: the keystore's bytes AES-GCM-wrapped under the device key in IndexedDB, or
  `MemoryKeyRest`. `Http`: `fetch` with `credentials: "include"` (the session cookie), the
  bearer left out.
- **A phone app hosting the server in-process** (sketch): `NativeDisk` as it is (a real
  filesystem, `tokio::fs` on a background runtime), `Paths::new(<app support>, <app
  support>/config)`, a `KeyRest` over the platform keychain (Keychain Services, Android
  Keystore) or `FileKeyRest` in the sandbox, `ReqwestHttp` or the platform's client.

### How the server takes them

- `ServerConfig` gains `disk: Disk`, `paths: Paths` and `http: Arc<dyn Http>` (defaults:
  `Disk::default()`, `Paths::native()`, `ReqwestHttp`). `ServerConfig::in_dir(dir)` sets
  `paths: Paths::in_dir(dir)` as well as the explicit paths it set before. Every `*_path`/
  `*_dir` left `None` now falls back to `config.paths`, not to `dirs`.
- `PimbleServer::with_config` makes the store manager on `config.disk`
  (`StoreManager::with_state_root(..).on_disk(disk)`); `make_handler` hands the handler
  `config.http`; the handler takes its disk from its manager (`try_read` at construction, the
  default when the manager is held), so the stores, their state directories, the keystore,
  the credentials, the devices' records, the people files, the folder's `computers/` files
  and the relay twins are on one disk. A relay face is made on its host's disk.
- `StoreManager::on_disk(disk)`, `disk()`; `LocalStore::create_on`, `create_replica_on`,
  `open_on`, `disk()`; `VaultStore::create_on`, `open_on`; `BlobStore::new_on`;
  `peek_manifest_on`. The old constructors (`LocalStore::create`, `open`, `open_in`, ...)
  stay, on `Disk::default()`, so the importer, the CLI, the MCP and the app are unchanged.
- `pimble_store::sync_state::{read, read_section, update, set_section, migrate}` take a
  `&Disk`; `exists(disk, state_dir)` is new; `write_durably` and `sync_dir` went into the disk.
- `Keystore::new(path)` (default disk), `Keystore::on_disk(disk, path)`,
  `Keystore::resting(Arc<dyn KeyRest>)`; it loads at the first use or at `load()`, which
  `PimbleServer::start` and `start_in_process` call before the first RPC, as the eager load
  in its constructor did. `CredentialStore::on_disk` loads at the first use the same way.
  `AgentRegistry::load(disk, path)` is async and so are `create`, `update`, `remove` and
  `touch` (the HTTP edge awaits `touch`).
- `crate::cloud`'s calls take `http: &dyn Http` first; `RpcHandler::http()`,
  `with_http`. `CloudError::Request`'s source is `HttpError` (the reqwest error's text) and
  `Decode`'s a `serde_json::Error`.
- `dirs` is gone from the server's core: the default-path functions
  (`default_keystore_path`, `default_credentials_path`, `default_agents_path`,
  `default_replicas_dir`) are `Paths::native()`'s, and the embedding model's cache stays
  `Paths::native().data/models` whatever the server's paths (a download, shared by every
  server in a process, tests included). `dirs` remains only in the native-only modules
  (`local.rs`, `auth.rs`, `logs.rs`, `diagnostics.rs`) and in `pimble-host`'s
  `Paths::native`.
- Native-only, still on the machine's disk directly, by design: the folder watch (`notify`;
  `run_folder_task` starts it only when `disk.is_native()`, the task's announcing of flush
  merges and its presence files run on any disk), the search index (seam F: rhypedb's own
  files under `<state dir>/index/`; `removeReplica` on a non-native disk also removes that
  directory from the machine's disk), the server token file (`auth.rs`, `fs_util`),
  `local.rs`, `logs.rs`, `diagnostics.rs`, the relay tunnel, the JWKS fetch (`jwt.rs`,
  the HTTP edge's).

### Changes by file

- `Cargo.toml`: the `pimble-host` member and workspace dependency.
- `crates/pimble-host/` (new): `disk.rs`, `disk/native.rs`, `disk/memory.rs`,
  `disk/testfs.rs` (feature `test-disk`), `paths.rs`, `key_rest.rs`, `http.rs`.
- `crates/pimble-store/`: `Cargo.toml` (the dependency, and the dev-dependency with
  `test-disk`); `local.rs` (a `disk` field; every file through it; the `_on` constructors),
  `local/folder.rs` (the folder migration, merges, conflict copies, temporaries' sweep),
  `blobs.rs` (async over the disk: the part file written, appended to, its hash written at
  its offset, synced, renamed; hashing still off the async threads), `vault.rs` (logs,
  snapshots, keys, reports, scopes; `read_range` for an entry), `sync_state.rs`,
  `manager.rs`; `tests/test_disk.rs` (new: the switch does what it says); the unit and
  integration tests' own file reads through `testfs`.
- `crates/pimble-server/`: `Cargo.toml`; `server.rs` (`ServerConfig.disk/paths/http`, the
  manager on the disk, the keystore loaded at start, agents loaded async); `handler.rs`
  (`disk`, `http`, construction, `removeReplica`, the agent RPCs awaited); `keystore.rs`
  (`KeyRest`, lazy); `credentials.rs`, `agents.rs`, `people.rs`, `devices.rs`,
  `vault_link.rs` (its record through the disk), `folder_watch.rs`, `relay_face.rs`,
  `cloud.rs`, `share.rs`, `auth.rs` (`touch().await`), `fs_util.rs` (doc only); the tests'
  file reads through `testfs`.

### Proof

- **The suites on both disks.** `PIMBLE_TEST_DISK=memory` (feature `test-disk`, turned on only
  by the dev-dependencies of `pimble-store` and `pimble-server`, so no shipped build has it)
  makes `Disk::default()` one `MemoryDisk` for the whole test process, taking the system temp
  directory's real subdirectories (each test's `TempDir`) as existing roots; `testfs` gives
  the tests' own looks at files (`read`, `write`, `exists`, `read_dir`, `names`, `copy_dir`,
  `set_modified`, ...) on that same disk. Unset, it is the machine's disk. Every test's
  `std::fs`/`tokio::fs` call on a server's or store's files and `Path::exists` on them went
  through `testfs` (the ones on the search index, real binaries and the spawned accounts
  service's log stay on the machine's disk). A helper that scanned a directory for plaintext
  (`contains_bytes_recursive`) reads through `testfs` too, so on the memory disk it scans what
  the server wrote rather than an empty directory.
  - `cargo test --release --no-fail-fast -p pimble-server -p pimble-store -p pimble-host`:
    365 passed, 0 failed, 1 ignored (13.5 min). An earlier run (before the `Http` seam) failed once in `vault_link.rs` `a_share_recipients_replica_holds_its_scope_and_edits_it_with_the_owner` ("wrapped under the scope key its maker holds": a second wrap under the store key had landed before the assertion read the keys); it passed three times alone and in the full run after. A race in the test's reading, not a disk effect; worth a look on its own.
  - the same with `PIMBLE_TEST_DISK=memory`: 364 passed, 1 failed, 1 ignored (14.5 min); the one failure was `store_folders.rs` `an_edit_through_one_server_reaches_the_other_through_the_folder`, two servers that meet only through the folder watch, which is native-only by design. It now says so and returns on the memory disk (`testfs::is_memory()`), and passes (skipped) in a rerun. Every other test, `pimble-store`'s randomized two-computer folder test included, passes on memory.
- **The fault sweep**: `PIMBLE_LINK_SEEDS=200 PIMBLE_LINK_PARALLEL=4 cargo test --release -p pimble-server --test link_faults`: all 200 seeds converged with 219 crashes among them (855 s), the same count as wave 0's sweeps. The same sweep on the memory disk: all 200 converged, 219 crashes (827 s). The crash fault copies the device's directory through `testfs`, so on memory it copies what the server wrote.
- **`test_isolation.rs`**: passes in both runs (no test builds a server or handler on the
  defaults; the new `paths` default is `Paths::native()`, which `in_dir` replaces).
- **Release builds**: `cargo build --release -p pimble-app -p pimble-cli -p pimble-mcp` builds (no caller outside the server needed a change: the old constructors stay). `cargo check --target wasm32-unknown-unknown -p pimble-host` checks; `cd web && cargo check --release --target wasm32-unknown-unknown` checks.
- **Format identity**: a capture test (not kept) run on `d3fb074` in a scratch worktree and on this tree: a server started in process writes a plain store (three nodes with text, a picture through `putBlob`), a vault store (three appends and a snapshot), an agent, then the store's link (`write_sync_config`), a signed-in keystore and saved credentials; the test lists every file under the directory with its mode, the bytes of binary files as hex (the blob file, header and image), JSON with ids and timestamps masked, `.yrs` files by their decoded title and text, and the secret files by their JSON shape. With blob names, the `computers/` presence name and the app version masked, the two listings are identical (26 entries): the same files, names, modes (`0600` for `keys.json`, `credentials.json`, `agents.json`), JSON text, key order and indentation, vault log and snapshot bytes, blob bytes, and no temporary left anywhere.

### Found while building

- **`PIMBLE_TEST_DISK=memory` was not enough by itself**: a test helper that scans a directory for text that must not be there (`contains_bytes_recursive`, in `common/mod.rs`, `relay.rs`, `vault_link.rs`) read the machine's disk, so on the memory disk it would have passed for scanning nothing. It reads through `testfs` now; noted in case another test grows the same shape.
- **tokio's `File` finishes a write in the background** after `write_all` returns unless flushed; the native disk's `append`, `write_at` and `write_private` flush before they answer (the vault append already did).
- **Joe's 0.29.3 release picked up this wave's uncommitted workspace member** (`crates/pimble-host` in `Cargo.toml`); 0.29.4 fixed it. Work in progress in the shared tree touches the root manifest: a release cut while a wave is in flight should come from a worktree.
- Not this wave's: `vault_link.rs`'s share-recipient test above reads the doc keys racily.

### What remains

- **Seam C, the runtime, is not built.** `spawn` (about 35 sites, several keeping a `JoinHandle` to `abort` or ask `is_finished`), `spawn_blocking` (blobs' hashing, pictures, the index), and `tokio::time` (`sleep`, `sleep_until`, `interval`, `interval_at`, `timeout`, `Instant` in about 70 places, many inside `select!`) across `handler.rs`, `vault_link.rs`, `share.rs`, `sync_link.rs`, `presence*.rs`, `pictures.rs`, `folder_watch.rs`, and `SystemTime::now` in the store's sweeps. The shape I would build: `pimble_host::Runtime` (`spawn(BoxFuture) -> Task` with `abort`/`is_finished`, `spawn_heavy(FnOnce) -> BoxFuture`, `sleep(Duration) -> BoxFuture`, `now() -> pimble_host::Instant` (a monotonic `Duration` since the runtime started), `wall() -> SystemTime`), `TokioRuntime` natively, held in `ServerConfig` and the handler beside the disk; `interval` and `sleep_until` written once over `sleep` and `now`. It is mechanical but touches every timer the links' convergence depends on, so it wants its own pass with the 200-seed sweep after it (on both disks: the switch is in place).
- **What still blocks wasm for the server**: the runtime above; `pimble-store`'s `tokio = full` (mio) and `notify` (`watch.rs` and `local.rs`'s use of it want `cfg(not(wasm32))`); in `pimble-server`, `jsonrpsee`'s server, `reqwest` (now only in `jwt.rs`, `diagnostics.rs` and `pimble-host`'s native `ReqwestHttp`), `tokio-tungstenite`, `libc`, and the native-only modules not yet behind `cfg` (`local.rs`, `auth.rs`, `jwt.rs`, `folder_watch.rs`'s watch, `relay_face.rs`, `relay_tunnel.rs`, `logs.rs`, `diagnostics.rs`, `memory.rs`), plus `crate::diagnostics::set_account` called from the keystore; `pimble-search` (seam F). The modules are not `cfg`'d in this wave: nothing builds the server for wasm32 yet to tell what else breaks, and wave 3 is where it first does.
- `MemoryDisk` has no modification times on wasm32 (`SystemTime::now` panics there) until the runtime's wall clock reaches it.
- The `OpfsDisk`, the device-key `KeyRest` and the `fetch` `Http` are wave 4's.
- Joe's by-hand check on his real stores after a backup (nothing here ran against them).

## Built: wave 2, the runtime (2026-11-22, not merged or released): seam C

Desktop only, behavior unchanged. Every task, timer and clock of the server's core goes
through a `pimble_host::Runtime` handed to it beside the disk (in `ServerConfig`, then the
store manager, then the handler), never a global. The shape is the one "What remains"
above planned.

### The runtime (`crates/pimble-host/src/runtime.rs`)

- **`RuntimeBackend`** (what a host implements, five calls): `spawn(BoxFuture<()>) -> Task`,
  `spawn_heavy(HeavyJob)` (a boxed `FnOnce()`; the job reports its own answer),
  `sleep_until(Instant) -> SleepFuture`, `now() -> Instant`, `wall() -> SystemTime`. Futures
  and jobs are `Send` natively and not on wasm32 (`MaybeSend`, `BoxFuture`, `SleepFuture`,
  `HeavyJob` are `cfg`'d aliases).
- **`Instant`**: a moment on the monotonic clock, a `Duration` since the clock's origin, with
  `Add`/`Sub<Duration>`, `Sub<Instant>` (saturating, as std's), `duration_since`,
  `checked_*`. There is no `elapsed()` on it: `Runtime::elapsed(earlier)` reads the clock.
- **`Task`**: a started task. `abort()`, `is_finished()`; awaiting it waits until it ended
  (finished or aborted); dropping it detaches it, as tokio's `JoinHandle`. A host's handle is
  a `TaskHandle` (`abort`, `is_finished`, `poll_ended`).
- **`Runtime`**, the cheap handle (`Arc<dyn RuntimeBackend>`), with what every caller builds
  from the five calls: `spawn(future)` (any output, dropped), `run_heavy(closure) ->
  Result<T, HeavyFailed>` (over a oneshot; a panicking job is `HeavyFailed`, where
  `spawn_blocking` gave a `JoinError`), `now`, `elapsed`, `wall`, `sleep(d)`,
  `sleep_until(at) -> Sleep` (`Unpin`, `reset(at)`, `deadline()`, `is_elapsed()`),
  `timeout(d, future) -> Result<_, Elapsed>` (the future polled before the timer, as tokio's,
  so `timeout(Duration::ZERO, ..)` is still a non-blocking poll), `interval(period)` and
  `interval_at(start, period) -> Interval` (tokio's default `Burst`: a tick is due a period
  after the one before, missed ticks fire at once; `tick()` is cancel-safe, the timer kept
  across a `select!` that took another branch; `reset()`).
  `Runtime::default()` is the target's own runtime.
- **`TaskSet`**: tokio's `JoinSet` as far as the server used it (`spawn`, `prune_finished` for
  `try_join_next`, `abort_all`, `join_all` for the `join_next` loop).
- `tokio::sync` stays as it was (it builds for wasm32 and needs no runtime).

### How each host implements it

- **Desktop** (`runtime/tokio_rt.rs`, `TokioRuntime`, done): `tokio::spawn`,
  `tokio::task::spawn_blocking`, `tokio::time::sleep_until`, on whichever tokio runtime the
  caller is in (each test's, the app's background one), never one captured at construction.
  The clock is tokio's `Instant` (std's: the workspace never turns on `test-util`) less a
  process-wide origin read once, and a deadline goes back to tokio's `Instant` by adding the
  origin again, so every sleep is the same `tokio::time::Sleep` at the same deadline as
  before. `wall()` is `SystemTime::now()`.
- **The browser's worker** (`runtime/local.rs`, `LocalRuntime`, wasm32, checks but runs in
  nothing yet): `wasm_bindgen_futures::spawn_local` with an `Abortable` and a shared ended
  flag for `abort`/`is_finished`; a heavy job runs inline (a worker has no thread to give
  it); timers are `gloo_timers::future::TimeoutFuture` (`setTimeout`, cleared when dropped);
  `now()` is `web_time::Instant` (`performance.now()`, which works in workers), `wall()`
  `web_time::SystemTime` (`Date.now()`). The memory disk's modification times on wasm32 come
  from this wall clock now (they were `None`).
- **A phone app hosting the server in-process** (sketch): `TokioRuntime` on its background
  tokio runtime, as the disk is `NativeDisk`.

### How the server takes it

- `ServerConfig.runtime` (default `Runtime::default()`); `PimbleServer::with_config` makes the
  store manager `.on_runtime(runtime)`; the handler takes the manager's runtime with its disk
  (`RpcHandler::runtime()`), and hands it to what it builds: the presence hub
  (`PresenceHub::new(runtime)`), the relay host (`RelayHost::new(disk, runtime, dir)`, and the
  relay face's own `ServerConfig`), each store's indexer, each link's `Progress`, share
  `Upkeep` and reconcile debouncer.
- `pimble-store`: `StoreManager::on_runtime`/`runtime()`; the `_on` constructors take the
  runtime after the disk (`LocalStore::create_on(disk, runtime, ..)`, `create_replica_on`,
  `open_on`, `VaultStore::create_on`, `open_on`, `BlobStore::new_on(disk, runtime, ..)`); the
  old constructors pass `Runtime::default()`, so the importer, CLI, MCP and app are unchanged.
  A blob's hashing is a heavy job; the store's temporaries sweep and the blobs' part sweep
  read the runtime's wall clock (the part sweep only when it judges an age); a vault's groups
  of parts age by the runtime's clock.
- Clocks that lived in structs without a runtime take `now` from their caller
  (`RepairDebouncer::debounce(store, now)`, `note(store, lists, now)`) or hold the runtime
  (`Upkeep`, `Progress`, `PresenceHub`, `ReconcileDebouncer`, `StoreIndexer`, `VaultStore`,
  `BlobStore`). `crate::share::until(runtime, due)` is the one "sleep until, or never".
- `RpcHandler`'s `impl PimbleApiServer` is `async_trait` natively and `async_trait(?Send)` on
  wasm32 (`cfg_attr`), matching the trait pimble-rpc's macro emits.
- The timings are the constants they were: the 750 ms flush, 250 ms repair (30 s when a
  list loops), 2 s index debounce, 1 s/5 s share upkeep, link backoffs, the vault link's
  intervals (progress saves, grant checks, steps, rechecks, faults, the relay's announce),
  the folder task's settle and presence intervals, the presence sweep, every `timeout`.

### Changes by file

- `crates/pimble-host/`: `Cargo.toml` (wasm32 only: `wasm-bindgen-futures`, `gloo-timers`
  0.2 with `futures`, `web-time`, `futures`; all already in `Cargo.lock`, which only gains the
  package's dependency lines), `src/runtime.rs`, `src/runtime/tokio_rt.rs`,
  `src/runtime/local.rs` (new; five unit tests in `runtime.rs`), `src/lib.rs` (exports),
  `src/disk/memory.rs` (wasm32 modification times).
- `crates/pimble-store/src/`: `manager.rs` (`runtime`, `on_runtime`, the constructors),
  `local.rs` (constructors), `local/folder.rs` (the sweep's wall clock), `blobs.rs` (heavy
  hashing, wall clock), `vault.rs` (the parts' clock).
- `crates/pimble-server/src/`: `server.rs` (`ServerConfig.runtime`, the manager, the model
  warm-up's spawn and heavy job, `connect_in_process`), `handler.rs` (the field, construction,
  every spawn, sleep, timeout, heavy job and clock: mounts' replica creation, the debounced
  flush, repairs and their loop clock, the indexer's tasks as a `TaskSet`, the index open and
  rebuild, waits for a first sync, hand-over, diagnostics' store record, the subscription
  forwarders; the `?Send` `cfg_attr`), `share.rs`, `vault_link.rs`, `sync_link.rs`,
  `presence.rs`, `presence_link.rs`, `pictures.rs`, `folder_watch.rs` (its task; the
  `notify` watch stays native), `relay_face.rs` (its runtime and the tunnel's task).

### Proof

- `cargo test --release --no-fail-fast -p pimble-server -p pimble-store -p pimble-host`: 370
  passed, 0 failed, 1 ignored (wave 2's 365 and the runtime's five new tests). With
  `PIMBLE_TEST_DISK=memory`: 370 passed, 0 failed, 1 ignored (the folder-watch test returning
  early on memory, as wave 2 left it). The share-recipient test in `vault_link.rs` that
  flaked once in wave 2 passed in both runs.
- **The fault sweep**: `PIMBLE_LINK_SEEDS=200 PIMBLE_LINK_PARALLEL=4 cargo test --release -p
  pimble-server --test link_faults`: all 200 seeds converged on the machine's disk (848 s) and
  on the memory disk (830 s). A second run on the machine's disk with `-- --nocapture`, to count: 200 converged with 219 crashes among them (850 s), the count wave 0's and wave 2's sweeps gave.
- **Release builds**: `cargo build --release -p pimble-app -p pimble-cli -p pimble-mcp`
  builds. `cargo check --target wasm32-unknown-unknown -p pimble-host` checks (with
  `LocalRuntime`; clippy clean on both targets); `cd web && cargo check --release --target
  wasm32-unknown-unknown` checks (the web workspace does not depend on `pimble-host`).
- **What is left on tokio or std's clocks directly** (a grep for `tokio::spawn`,
  `tokio::time`, `Instant`, `spawn_blocking`, `thread::spawn`, `SystemTime::now` over
  `pimble-server`, `pimble-store` and `pimble-search`), all deliberate:
  - the native-only modules, which wave 3 puts behind `cfg`: `relay_tunnel.rs` (tokio-
    tungstenite, its `JoinSet`, timeouts and ping interval), `local.rs` (finding and joining
    the embedded server), `jwt.rs` (the JWKS refresh), `diagnostics.rs` (`SystemTime` stamps),
    and `server.rs`'s `wait_for_shutdown_signal` (signals);
  - `pimble-search`'s embedding warm-up (`warmup.rs`'s `Instant`) and the embedder's worker
    thread (`embed.rs`): seam F, semantic search, native only;
  - `pimble-store`'s tests (`manager.rs`' two `tokio::time::timeout`s) and every test's own
    timing: tests run on tokio and may use it.
  - `chrono::Utc::now()` (timestamps written into documents, manifests and records) is not a
    runtime clock and stays: chrono's default `wasmbind` feature reads `Date.now()` on wasm32.

### What remains for wave 3

- The native-only modules behind `cfg(not(wasm32))` (above, plus `auth.rs`, `logs.rs`,
  `memory.rs`, the folder watch's `notify`), and the dependencies that come with them
  (`jsonrpsee`'s server, `reqwest`, `tokio-tungstenite`, `libc`; `pimble-store`'s `tokio =
  full` and `notify`); `crate::diagnostics::set_account` from the keystore.
- `relay_face.rs` takes the runtime but starts a `PimbleServer` on jsonrpsee: native only.
- `LocalRuntime` has run in no browser yet: the first wasm build of the server is where it is
  exercised (a worker smoke test should cover a sleep, an interval, a timeout, an abort and
  a heavy job).
- Seam F (search) and the rest of wave 3's list are as wave 2 left them.

## Built: wave 3 (2026-11-22, not merged or released): the server builds for wasm32, with keyword search

`pimble-server`, `pimble-store`, `pimble-search`, `pimble-client` and `pimble-host` build for
`wasm32-unknown-unknown`, and a new crate, `crates/pimble-worker`, is the worker's entry point.
The server runs in Node under wasm-bindgen-test on the memory disk and `LocalRuntime`, reached
by `PimbleClient`s over `MessagePort`s, and finds what it was given with rhypedb's in-memory
index. The desktop's behavior is unchanged. Nothing of `web/` changed (its lock gains three
dependency lines of `pimble-client`'s).

### Native-only, behind `cfg(not(target_arch = "wasm32"))`

The target, not a feature, because that is what the earlier waves keyed on (`pimble-rpc`'s
`dispatch::native`, `pimble-client`'s transports, `pimble-host`'s adapters) and because nothing
native wants these off.

- **Modules** (`pimble-server/src/lib.rs`): `auth` (the HTTP edge and its token), `fs_util`,
  `jwt` (fetching the JWKS), `local` (finding, joining and starting the embedded server), `logs`,
  `diagnostics`, `memory`, `relay_tunnel`. What the core asks of them is answered on wasm32 by a
  few lines in `lib.rs`: `diagnostics::{ids, set_account, record}` record nothing (section 6
  item 10, the worker's own diagnostics, come later), `local::host_kind()` is `"worker"` and
  `handed_over` nothing, and `relay_tunnel::owner_is_offline` is `false` (a browser's WebSocket
  cannot send the bearer header the desktop's probe uses; a relayed store whose owner is off
  reads as plainly offline there, wave 4).
- **In modules that build on both**: `PimbleServer::start` (jsonrpsee's server, the HTTP edge),
  `wait`, the listener handle, `wait_for_shutdown_signal`, `run_server`, the embedding model's
  warm-up (a worker's indexes are keyword only, so nothing waits for a model: the warm-up
  channel says `false` at once, as for a relay face); `RpcHandler::set_server_handle` and what
  `handOver` stops (`ListenerHandle` is `Infallible` on wasm32: a server with no port never
  holds one); the index on disk (`index_dir_for`, `open_waiting_for_lock`, the lock waits, the
  schema-hash rebuild); `removeReplica`'s removal of an index directory; the folder watch's
  `notify` (the folder task runs on any disk, as wave 2 left it); the relay face
  (`start_face` refuses in a worker: "Sharing from this computer needs the Pimble app on a
  computer; a browser cannot share its stores itself.", section 5) and its tunnel's task
  (`RelayHost::start_tunnel`, none in a worker).
- **Dependencies by target**. `pimble-server`: jsonrpsee with `server`, tokio `full`,
  `tracing-subscriber`, `dirs`, `flate2`, `tower`, `http-body`, `bytes`, `reqwest`,
  `tokio-tungstenite` and `ed25519-dalek` are native only; on both, tokio `sync` + `macros`;
  on wasm32, jsonrpsee `client-core` + `macros` (as `pimble-rpc` has it) and getrandom 0.3 with
  `wasm_js` (rand 0.9). `pimble-store`: tokio `sync` on both, tokio `full` and `notify` native
  only (`watch.rs` and `watch_folder` are native only). `pimble-search`: its unused `tokio`
  dependency dropped. `libc` was already Linux-only.
- `Paths`: the default-path functions (`default_keystore_path`, `default_credentials_path`,
  `default_agents_path`, `default_replicas_dir`) and `ServerConfig::default()` read
  `server::default_paths()`: `Paths::native()` on the desktop, `/pimble` and `/pimble-config`
  on the worker's disk on wasm32. `ServerConfig::default().http` is `ReqwestHttp` natively and
  `FetchHttp` on wasm32; the handler's own default takes the same.
- `pimble_host::Task` is `Send` on wasm32 without the `atomics` target feature (one thread, no
  way to start another; `local.rs`'s handle was already `Sync` for the same reason), so the
  server's structs that hold one stay `Send` as they are natively. `PictureRemote` is
  `async_trait(?Send)` on wasm32, as the handler's trait is.

### Search on wasm32 (seam F)

Built against rhypedb's in-memory mode (question 4's own route, so no stopgap index):
`Database::open_in_memory`, `run_deferred_work`, `default-features = false`.

- `SearchIndex::open_in_memory()` (`pimble-search/src/index.rs`): the same composed schema
  and english analyzer as `SearchIndex::open`, keyword only (no `Chunk` type; semantic search
  in the browser is wave 6), nothing written anywhere. `is_in_memory()`,
  `run_deferred_work()`. `open` and the new constructor share the rest (`SearchIndex::over`).
- The handler on wasm32 opens every store's index in memory and builds it from the store's
  documents each time the store opens (`open_index_for_store`'s wasm32 twin; the same
  `reindex_in_batches`, `IndexState::Rebuilding` and `Ready` as a rebuild on the desktop), and
  `rebuildIndex` makes a new one (`fresh_index`). Search, its 2 s content debounce, upserts and
  removals are the desktop's code.
- What rhypedb leaves for later runs in `StoreIndexer::run`, between events: for an index in
  memory a `select!` takes an event first and otherwise ticks a 1 s interval of the runtime
  (`DEFERRED_INDEX_WORK_EVERY`) that calls `run_deferred_work`. An index on disk takes the old
  loop exactly (no interval is made).
- **rhypedb's `wasm-memory` branch** (194029d) was merged into rhypedb master as joeleaver/rhypedb#57
  (c73cc88, 2026-11-23), which the workspace's rhypedb lines follow again. While it was local, a temporary
  `[patch]` pointed at a worktree; it is gone. The branch is based on rhypedb master `6200e3c`
  (0.2.1), and the workspace was on `6975cb0`: **the desktop's rhypedb moves forward by
  master's 17 commits (14 without merges)** (0.2.0 and 0.2.1: link peers, cover generations,
  the generation race fix, k-way merges) as well as the branch's own commit; the native
  suites below ran on it. Nothing in rhypedb was changed by this wave.

### The worker entry point (`crates/pimble-worker`)

A crate of its own, `cdylib` + `rlib`, empty natively: it is a host of the server, as the
desktop app's backend is, and `web/` is the page; the worker's module is built from it and the
page's from `web/`, and neither links the other's code (section 6: `web/` keeps only the page's
adapters and entry).

- `startServer()` (wasm-bindgen, `start_server`): `PimbleServer::with_config(ServerConfig::default())`
  on wasm32 (a memory disk, `LocalRuntime`, `FetchHttp`, `/pimble` and `/pimble-config`), then
  `start_in_process`, and answers a `WorkerServer`. `start(config)` is the same with a host's
  own configuration (wave 4's OPFS disk; the tests).
- `WorkerServer::connect(port)`: serves one connection on a `MessagePort`, the worker's end of a
  `MessageChannel`, with the `Service` principal the desktop's loopback app has (the page is
  this browser's own app). It goes through `PimbleServer::serve_connection(ext, incoming, send)`
  (new, both targets): `dispatch::session::serve_with` on a task of the server's runtime, for
  any transport that is a stream of text and a send.
- How a page starts it (wave 4): a module worker (`new Worker("worker.js", { type: "module" })`)
  whose script does `await init(); const server = await startServer();` and, for each
  `postMessage("connect", [port])` from a page, `server.connect(event.ports[0])`; the page keeps
  the other end and makes `PimbleClient::over_message_port(port)`.
- `console_error_panic_hook` (already in `web/`'s lock) so a panic in the worker is said in its
  console. No tracing subscriber is installed: which one, and at what level, is the page's
  choice in wave 4.

### The `MessagePort` transport (`crates/pimble-client/src/message_port.rs`, wasm32)

- `PimbleClient::over_message_port(port)`: a jsonrpsee `Client` built with `build_with_wasm`
  over a second pair of `TransportSenderT`/`TransportReceiverT` beside wave 1's channels: one
  JSON-RPC text per message (`postMessage` of a string), `close` closes the port.
  `MESSAGE_PORT_URL` is what `url()` answers.
- `PortText::listen(port)` (`onmessage` into a channel, which also starts the port; a message
  that is not a string is skipped and logged; listening stops when it is dropped) and
  `post_text(port, text)`: the one way either side reads and writes a port; the worker's
  `connect` uses them too.
- A `MessagePort` says nothing when its other side goes: a client whose worker stopped sees
  calls left unanswered. Noticing (the leader tab's lock passing) is wave 4's.

### Found while building, fixed

- **`openStore` read the machine's disk, not the server's.** `RpcHandler::open_store` peeked the
  manifest with `peek_manifest` and loaded a plain store with `LocalStore::open_in`, both on
  `Disk::default()`, since wave 2. Natively the default disk is the server's, and under
  `PIMBLE_TEST_DISK=memory` both are the one test disk, so nothing showed it; on wasm32
  `Disk::default()` is a new memory disk each time, and reopening a store failed ("No manifest
  found"). Both now go through the handler's disk and runtime (`peek_manifest_on`,
  `LocalStore::open_on`). A grep for `Disk::default()` and the old constructors in the server's
  and store's non-test code finds no other caller. `Disk::default()` on wasm32 stays a fresh
  disk for that reason: a shared one would hide the next such bypass, and OPFS (wave 4) would
  not.
- The index's title for a note without an explicit title is derived from its text while the
  node's `metadata.title` keeps the name it was created with; the smoke test checks the hit's
  snippet rather than its title. The same on the desktop; not changed.

### Changes by file

- `Cargo.toml`: the `pimble-worker` member; rhypedb master with #57 (c73cc88).
  `Cargo.lock`: rhypedb 0.2.1 from the worktree (path entries, one copy), `web-time` and
  getrandom 0.3's `wasm_js` for its wasm32 build, `console_error_panic_hook`,
  `wasm-bindgen-test` lines for the worker, and dependency lines. `web/Cargo.lock`: three
  dependency lines of `pimble-client`.
- `crates/pimble-host/`: `Cargo.toml` (wasm32: `wasm-bindgen`, `js-sys`, `web-sys` with
  `Headers`, `Request`, `RequestCredentials`, `RequestInit`, `Response`); `src/http.rs`
  (`FetchHttp`: the global `fetch`, so a window, a worker and Node alike, `credentials:
  "include"`, the bearer when given, JSON body); `src/runtime.rs` (`Task` is `Send` on
  single-threaded wasm32); `src/lib.rs` (doc).
- `crates/pimble-store/`: `Cargo.toml` (tokio `sync`; tokio `full` and `notify` native);
  `src/lib.rs` (`watch` native only).
- `crates/pimble-search/`: `Cargo.toml` (unused tokio dropped); `src/index.rs`
  (`open_in_memory`, `is_in_memory`, `run_deferred_work`, `over`); `tests/in_memory.rs` (new).
- `crates/pimble-server/`: `Cargo.toml` (dependencies by target); `src/lib.rs` (native-only
  modules, the wasm32 answers); `src/server.rs` (`default_paths`, `default_http`, `start`/`wait`/
  shutdown native, the warm-up, `serve_connection`); `src/handler.rs` (`http::Extensions`,
  `ListenerHandle`/`stop_listener`, the index in memory, `DEFERRED_INDEX_WORK_EVERY` and the
  indexer's loop, `fresh_index`, `openStore` on the server's disk, the index-on-disk items
  native); `src/relay_face.rs` (`start_face` and `start_tunnel` per target); `src/folder_watch.rs`
  (the watch native); `src/pictures.rs` (`?Send` on wasm32); `src/principal.rs`
  (`http::Extensions`); `src/agents.rs`, `src/credentials.rs`, `src/keystore.rs`
  (`default_paths`).
- `crates/pimble-client/`: `Cargo.toml` (wasm32: `wasm-bindgen`, `web-sys` `MessageEvent`
  and `MessagePort`, `futures`); `src/message_port.rs` (new); `src/lib.rs`; `src/in_process.rs`
  (doc).
- `crates/pimble-worker/` (new): `Cargo.toml`, `src/lib.rs`, `src/worker.rs`,
  `tests/smoke.rs`.
- `crates/pimble-server/tests/test_isolation.rs`: `pimble-worker/src/worker.rs` joins the
  production files allowed `ServerConfig::default()` (its defaults are the worker's own disk,
  `/pimble` and `/pimble-config`, never the machine's directories); the guard is otherwise
  unchanged, and the smoke test builds on `ServerConfig::in_dir` as every test does.

### Proof

- **The wasm32 builds.** `cargo build --target wasm32-unknown-unknown -p pimble-server -p
  pimble-store -p pimble-search -p pimble-client -p pimble-host -p pimble-worker` builds, no
  warnings of these crates (the one warning is `pimble-crdt`'s unused `Encode` import, there
  natively too, not this wave's). `cargo build --release --target wasm32-unknown-unknown -p
  pimble-worker`: `pimble_worker.wasm` is 13.7 MB before wasm-bindgen, `wasm-opt` and
  compression (section 10's size risk, to measure in wave 4).
- **The smoke test**, `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo
  test --target wasm32-unknown-unknown -p pimble-worker --test smoke`, in Node 24: 2 passed.
  The runner must match the lock's `wasm-bindgen` exactly (0.2.128; the one installed in
  `~/.cargo/bin` is 0.2.126 and refuses the file, so a 0.2.128 was installed under the session
  scratchpad: `cargo install wasm-bindgen-cli --version 0.2.128 --locked --root <dir>`).
  - `the_server_answers_over_message_ports`: the server started by the worker's entry on the
    memory disk (`ServerConfig::in_dir("/smoke")`, checked to be `DiskKind::Memory`) and
    `LocalRuntime`; two `PimbleClient::over_message_port` clients, each on a new
    `MessageChannel` whose other end went to `WorkerServer::connect`; `serverInfo` (`host:
    "worker"`, `hands_over: false`); `createStore`, `createNode`, `listStores`; the second
    client subscribes (`subscribeStoreChanges`) and hears the first's `applyEdit` (an edit of
    the node's own document, `NodeDoc::replace_plain_text`) as `ContentUpdated` with its
    `source_client_id`; both read the text back (`getNode`, the document decoded); keyword
    search through the in-memory index finds it by a stem ("pomegranate" for "pomegranates")
    and a prefix ("water"), and finds nothing for a word it lacks; an unsubscribe is answered;
    the store closes and opens again from the memory disk with its text, and its index is
    built again and finds it (this step found the `openStore` bypass above).
  - `the_local_runtime_runs_in_wasm`: a sleep (at least as long as asked, by the runtime's
    clock), the wall clock, an interval's four ticks a period apart after the first at once, a
    timeout that elapses and one that does not, a task awaited to its end, a task aborted in
    its sleep (ended, and never past its await), a heavy job run inline.
- **`pimble-search` in memory, natively**: `tests/in_memory.rs` (new): the same nodes in an
  index on disk and one in memory answer five queries (stem, prefix, title, two words, none)
  with the same nodes in the same order; an update and a removal are seen at once.
- **The suites on the machine's disk**: `cargo test --release --no-fail-fast -p pimble-server -p
  pimble-store -p pimble-host -p pimble-search`: 410 passed, 2 failed, 1 ignored. The two:
  `test_isolation.rs` (the guard caught the worker's `ServerConfig::default()`, as it should;
  answered above, then passed) and the known `vault_link.rs`
  `a_share_recipients_replica_holds_its_scope_and_edits_it_with_the_owner` flake at the same
  assertion as in wave 2 ("wrapped under the scope key its maker holds"), which passed twice
  run alone. Wave 2's 371 for server, store and host, plus `pimble-search`'s own tests (its new
  one included).
- **The suites on the memory disk** (`PIMBLE_TEST_DISK=memory`, on the final tree): 412 passed,
  0 failed, 1 ignored.
- **The fault sweep**: `PIMBLE_LINK_SEEDS=200 PIMBLE_LINK_PARALLEL=4 cargo test --release -p
  pimble-server --test link_faults -- --nocapture` on the machine's disk: all 200 converged,
  219 crashes among them (848 s), the count of waves 0 and 2.
- **Release builds**: `cargo build --release -p pimble-app -p pimble-cli -p pimble-mcp` builds
  (7 min). `cd web && cargo check --release --target wasm32-unknown-unknown` checks.

### What remains for wave 4

- The worker as the browser's server, behind a switch: the module worker script and its build
  (trunk can build a worker from this crate), the leader tab (Web Locks, a `BroadcastChannel`
  from the other tabs, which needs a transport like `message_port.rs` over it), noticing a
  worker that has gone (a `MessagePort` does not say), and the app's backend on
  `PimbleClient::over_message_port`.
- `OpfsDisk` with the disk key, the device-key `KeyRest`, and `start(config)` with them;
  `Disk::default()` on wasm32 stays a fresh memory disk ("don't remember this browser" builds
  its own).
- `FetchHttp` is built and checks but has called nothing yet: sign-in through the server
  (`cloudSignIn` and the rest) is where it first runs, against the local stack.
- A tracing subscriber in the worker (which, at what level).
- A relayed store's owner being away reads as plainly offline in the worker
  (`owner_is_offline` is `false` there).
- The index is rebuilt from the documents at every store open: measure on Joe's 674-node store
  in wave 4 (section 10).
- Semantic search in the worker is wave 6 (`open_in_memory` is keyword only).
- The smoke test runs in Node, not yet in a browser worker or in CI (section 8 wants a browser
  runner and OPFS: wave 4).


## Built: wave 4 (2026-11-23, not merged or released): the browser on its worker server, behind a switch

**The default since 2026-11-23** (Joe: "There's no active users of the web app ... you might as
well just flip the switch"): see "Built: wave 4, the follow-ups" below; the old path is
`?server=old` until wave 5. As built first, and as the rest of this section says:
`?server=worker` in the web app's URL turned it on for that browser (kept in `localStorage` as
`pimble.server`; `?server=page` turned it off). Off, the page is exactly what it was: nothing of
this wave runs, and the worker's files, though built and served beside the page's, are never
fetched. On, the page runs the same Pimble server the desktop runs, in a dedicated worker of one
leader tab, keeps the account's stores in OPFS sealed with the remembered device's disk key (or
in memory), holds the account's keys in that worker, and reaches it through the same
`pimble_api::Pimble` and `pimble_app::commands` the desktop uses.

### The worker and its build

- **`crates/pimble-worker`** gains `entry.rs` (feature `entry`): a `#[wasm_bindgen(start)]` that
  installs the worker's tracing subscriber (a small layer of its own writing each event at INFO
  or above as one line to the worker's console) and answers the leader page's messages: `start { mode, inherited }` (answered `started {
  mode }` or `failed`), `connect` with a transferred `MessagePort` (held until the server is up),
  `stop` (every link stopped, every document written) and `forget`. The feature is off in the
  tests (the smoke test drives the server itself) and on in the page's build.
- `worker.rs`: `start_browser(keeping, inherited)` (`startBrowserServer` to JavaScript):
  `remember` makes or finds the device key and the disk key under it and starts the server on
  `SealedDisk(OpfsDisk, disk key)` with the keystore resting in IndexedDB; `memory` starts it on
  a `MemoryDisk` with `MemoryKeyRest`; `auto` is `remember` when this browser already remembers
  itself, else `memory` (what a tab asks for before anyone signs in). A remembered browser whose
  files do not open (its device key gone or changed underneath) is forgotten and starts on
  memory, said in the log. It then opens every store this browser keeps
  (`/pimble/replicas/*.pimble`): a browser has no list of open stores but its replicas.
- **The build**: `web/index.html` has a second trunk `rust` link,
  `../crates/pimble-worker/Cargo.toml` with `data-type="worker"`, `data-bindgen-target="no-modules"`,
  `data-loader-shim`, `data-cargo-features="entry"`, `data-cargo-profile-release="worker"` and
  `data-wasm-opt="z"`, so `trunk build --release` writes `pimble-worker_loader.js`,
  `pimble-worker.js` and `pimble-worker_bg.wasm` beside the page's files (not hashed: the hub
  names the loader). The worker is built from the root workspace with its lock (the same
  `wasm-bindgen` 0.2.128 as `web/`'s) and a new root profile, `[profile.worker]` (release, `opt-level
  = "s"`, LTO, one codegen unit, `panic = "abort"`); nothing native uses that profile.

### The leader tab, and the other tabs (`web/hub.js`)

A small script of the page's (plain JavaScript: the Web Locks API is unstable in `web-sys`, and
this is the transport adapter of section 6), loaded before the page's wasm.

- Every tab asks for one Web Lock, `pimble-server`. The tab granted it leads: it starts the
  worker (`new Worker("/app/pimble-worker_loader.js")`), sends `start`, and says on a
  `BroadcastChannel` (`pimble-server`) that it leads and how the stores are kept. The lock is
  held until the tab goes.
- **A connection** is `PimbleHub.connect()`: a `MessagePort` whose other end reaches the server.
  In the leader, the hub relays it to a port of its own to the worker; in another tab, over the
  channel, as `rpc { conn, text }` addressed by a connection id, which the leader relays to a
  worker port of its own for that connection. JSON-RPC text is passed through as it is.
- **When the leader goes** (closed, crashed: the lock passes with the tab), the next tab in the
  lock's queue becomes the leader, starts its worker on the same OPFS files and says so; every tab
  that hears another leader (or the old one's `bye`, sent on `pagehide` along with a `stop` to its
  worker if the tab is given the moment) ends its connections. Ending one is `null` posted on the
  client's port: `PortText` (`crates/pimble-client/src/message_port.rs`) now reads `null` as the
  other side gone and ends, so the client over it is disconnected exactly as one whose WebSocket
  closed. A tab whose connection ended was a follower whose relays end with it; the leader posts
  `null` to the worker's end too, so the server's session for it ends.
- The leader restarts its worker when it fails (`onerror`), and on `control`: `restart { mode }`
  (sign-in asks to keep the stores in this browser) and `forget`; another tab's control goes to
  the leader over the channel.
- `PimbleHub.take(name)`: a Web Lock held by the page's Rust until dropped (`hub::exclusive`),
  so only one tab at a time adds the account's stores.

### The page's side (`web/src/worker_mode/`)

- `mod.rs`: the switch; `begin()` starts the hub before routing, handing it the device this
  browser reported as before (below); `start_app()` connects, asks `cloudStatus`, and builds the
  app (the same `pimble_app::app::build_view`) with this backend, or sends the visitor to the
  sign-in page; the account and sign-out hooks; `ask_to_persist()`.
- `backend.rs`: the desktop's backend shape (`pimble-app/src/backend.rs`) over a `MessagePort`:
  `Local` (the connection: a port from the hub, proved with `serverInfo`, made again with backoff
  the moment it ends, `Connected`/`Disconnected` to the UI, the cloud connection's subscription
  for the status bar), `LocalConnector` for `RpcBackend`, the command loop through
  `process_command` (opens, the account and child loads beside the line, as on the desktop),
  imports through the shared importer, and after a reconnect `Pimble::rewatch` and the store list
  again. "New Store..." (`CreateHostedStore`) makes the store in this browser and hosts it
  (`createStore`, then `cloudHostStore`): what the button has always made in the browser, an
  encrypted store on the account, now by the desktop's path. Diagnostics answer that they are not
  sent from the worker yet.
- **The account's stores, kept here** (`keep_account_stores`, section 5): at connect and every
  two minutes, under the `pimble-account-stores` lock: `cloudListHostedStores`, and for each
  encrypted store not open here (or a share of one whose root is not), `cloudAddHostedStore`, the
  desktop's "Add Hosted Store..." call; then `StoresListed`. A plain hosted store was logged and
  skipped (below; replicated since wave 5, part 1).
- `pages.rs`: the sign-in page (email, password, "Don't remember this browser" with section 7's
  sentence beside it in its words) and the account page (who is signed in, how the stores are
  kept, "Sign Out", "Forget This Browser", "Sign in again"). Signing up, verifying and recovering
  stay the page's own pages, which need no keys in the page.
- `route.rs`: in worker mode `/app/login` and `/app/account` are these pages and the app starts
  through `start_app`; `app_page.rs`'s open-link keepers are shared.

### Sign-in through the server

The sign-in page sends `cloudSignIn { url: <this origin>, email, password }` to the worker's
server: the desktop's handler derives, logs in and unwraps through `FetchHttp` (which sends the
session cookie too, so the account pages keep working), and keeps the session and the account's
keys in its keystore, resting on the device key. No key enters the page. Before it: a sign-in as
another account than the one the server last held forgets this browser first (section 7);
"remember" with the server on memory asks the leader to restart it remembered; "don't remember"
with a remembered server forgets this browser first. A remembered sign-in then asks for
persistent storage (`navigator.storage.persist()`, section 10), and a refusal is said once in the
app ("This browser did not agree to keep Pimble's storage: ..."). Sign Out: `cloudSignOut`, then
forget (below), then the accounts service's logout (the cookie); Forget This Browser: forget.
Either reloads at the sign-in page.

### The disk: OPFS, sealed (`crates/pimble-host`)

- **`SealedDisk`** (`src/disk/sealed.rs`, both targets): any `DiskBackend` under a key. Each
  file is one `pimble_crypto::Blob` (XChaCha20-Poly1305) whose associated data is
  `pimble_crypto::local_file_aad(path)` (`pimble-local/v1/<path>`, new), the path normalized
  from the root; names pass through. The seal adds a constant 59 bytes, so `metadata` reports the
  plain length. A rename or copy reads and seals again under the new path (a file moved below the
  seal does not open); an append, a write at an offset and a range read are a read and a whole
  write. The atomic compositions go to the backend below with the bytes sealed under the final
  path.
- **`OpfsDisk`** (`src/disk/opfs.rs`, wasm32): paths walked as directory handles from
  `navigator.storage.getDirectory()`, the handles cached (a store's open reads hundreds of files
  from one directory; the cache is emptied whenever a directory is removed or moved). A whole file
  is written with `createWritable()` and `close()` (committed at `close`: a tab killed mid-write
  leaves the old file), so the atomic compositions are one write. Where an engine has no
  `createWritable` in a worker (Safari before 26), a sync access handle (truncate, write, flush;
  waiting out one still held by a worker that is going), the contract's fallback. Errors carry
  the kinds the server branches on (`NotFoundError` is `NotFound`). One file's operations are
  taken one at a time. `OpfsDisk::clear()` empties OPFS.
- **A file that does not open is a document that does not load**
  (`pimble-store` `load_node_docs`): a sealed file written only in part (the fallback killed
  midway) is skipped with a warning at open, as a document that does not decode always was, and a
  peer sends it whole at the next reconcile, rather than failing the store's open. Natively a
  read never answers `InvalidData`, so nothing changes there.

### The device key (`crates/pimble-worker/src/device.rs`)

One IndexedDB database, `pimble-device`, one object store, `keys`: `device` (an AES-GCM 256
`CryptoKey` made with `extractable: false`; it never leaves WebCrypto), `disk` and `keystore`
(each `{ iv, data }`, sealed under the device key with a fresh 12-byte iv). Opened per operation
and closed after; a lookup never makes the database (asking whether this browser remembers itself
after forgetting it leaves nothing behind). `DeviceKeyRest` is the worker's `KeyRest`.
**Forgetting** deletes the `device` entry first, then the database, then every entry of OPFS:
what remains if clearing is interrupted is unreadable. The page's own plugin database
(`pimble-plugins`, plugins added in this browser, never uploaded) is not the account's and stays,
as the desktop's plugins folder does.

### The server's two new settings (`pimble-server`)

- `ServerConfig.key_rest: Option<Arc<dyn KeyRest>>` (`RpcHandler::with_key_rest`): where the
  keystore rests; `None` is `keys.json` as before.
- `ServerConfig.inherited_device: Option<InheritedDevice>` (`RpcHandler::with_inherited_device`,
  `devices.rs`): a browser's device id and, per store, the reports it had answered, from before
  its server ran in a worker. A store's device entry made for the first time takes them; an entry
  already made keeps its own. **The hand-over**: the page reads `pimble.vault-device` and every
  `pimble.vault-answered.<store>` from its `localStorage` and passes them in `start`, so the
  browser keeps its report slot and answers nothing twice. Both default to `None`; the relay
  face's config names them.

### Proof

In Chromium (Playwright's, headless, on the local stack: `scripts/local-stack/up.sh`, alice's
desktop as the owner on 17491, carol's on 17493, bob in the browser; made-up accounts):

- **First light**: `/app/?server=worker` goes to the worker's sign-in page; bob signs in
  (remembered); his two shares of alice's `Family` arrive as one replica ("Shared by alice",
  never "Family") 1.9 s after the sign-in; a reload opens it with no password (the store from
  OPFS in 0.8 s, the tree's rows 1.8 s after the reload).
- **The sharing walk-through** (`scripts/local-stack/README.md`, steps 4-8, bob the browser):
  `cloudListShares` names "Holiday plans" (editor) and "Errands" (reader). The owner's computer
  stopped: bob makes a folder, a note, types into the note in the editor, moves nodes; carol's
  desktop has all of it; carol makes a note, moves it into bob's folder and deletes another, and
  bob's server has it; bob's write under the read-only share answers "You can read this, not
  change it." Nothing typed is under `$STACK/stores` (`grep` finds nothing). The owner back: its
  tree has the folder, both notes, the moves and bob's text. The owner made bob a reader of
  Holiday: within the link's two-minute grant check bob's replica reads as read-only and a write
  is refused.
- **The owner in a browser** (alice, remembered): both of her stores (Family, and Big: 681
  nodes) replicated; keyword search over Big; with alice's desktop off, a note her browser moved
  from her private folder into the share reached carol's desktop readable 9 s later (her
  browser's server keeping the share up: `share.rs`, question 3).
- **A reload with Pimble Cloud unreachable** (the hosted server and the accounts service stopped,
  the edge still serving `/app/`): the store and its tree from OPFS in 2.1 s, the link `offline`;
  a note made and text typed in the editor are kept across another reload; when Pimble Cloud is
  back carol's desktop has the note and its text 7 s later. **With no network at all** the page
  itself does not load (`net::ERR_INTERNET_DISCONNECTED`): nothing caches the page's own files
  (below).
- **Two tabs, the leader closed**: tab A leads, tab B follows (one server: B's node is in A's
  answers 41-129 ms later, and both reach carol). Tab A closed: B leads 262-551 ms later and
  reaches its own server 9-55 ms after that; a node it makes reaches carol and its tree shows it.
  **Typing across the hand-over**: in B's editor, text typed before, during and after A's close
  all reaches carol's desktop ("Before the close: apricot. During the close: bramble. After the
  close: cloudberry.").
- **Forget and sign-out**: remembered, OPFS held 26-35 files, every one sealed (`PB`) and none
  holding a typed word, a title, the store's name or the email. After "Forget This Browser":
  OPFS empty, `pimble-device` gone. Signed in again, then "Sign Out": the same. "Don't remember
  this browser": the server on memory, OPFS empty, no device database, and a reload asks for the
  password.
- **`link_faults.rs` with a browser-configured device**: Alice's second device is a browser on
  odd seeds (`PIMBLE_LINK_BROWSER=all|none` overrides): the server started in process as the
  worker starts it, reached over an in-process connection with the `Service` principal, its
  files on a memory disk that outlives each server (as OPFS outlives a leader tab) under a
  `SealedDisk`, its keystore resting apart from the disk, and the device id handed over (checked:
  the store's record reports as it). Its crash copies the disk below the seal, state directory
  first. `PIMBLE_LINK_SEEDS=200 PIMBLE_LINK_PARALLEL=4`: all 200 converged (852 s), 100 of them
  with the browser device, 219 crashes (38 of them the browser's), the count of the earlier waves.
- Unit tests: `pimble-host` `disk::sealed::tests` (4: round trip with only ciphertext below and
  the plain length; a file moved below the seal does not open; appends, writes at an offset,
  ranges, renames of files and directories, copies, the compositions with no temporary left;
  another key does not open it); `pimble-server` `devices::tests` (an inherited device is taken by
  first entries only, and kept); `pimble-store` `local::tests::a_sealed_file_that_does_not_open_is_skipped`.
- **The suites**: `cargo test --release --no-fail-fast -p pimble-server -p pimble-store -p
  pimble-host -p pimble-search`: 418 passed, 0 failed, 1 ignored, on the machine's disk and with
  `PIMBLE_TEST_DISK=memory` alike (the share-recipient test that flaked in waves 2 and 3 passed in
  both; they ran with `tracing-wasm` still in the lock, whose two moved packages are a Windows
  one and the desktop window's X11 loader, neither in these crates on Linux).
- **The wasm smoke test** (`CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
  cargo test --target wasm32-unknown-unknown -p pimble-worker --test smoke`, runner 0.2.128): 2
  passed. **Release builds**: `cargo build --release -p pimble-app -p pimble-cli -p pimble-mcp`
  builds; `cd web && trunk build --release` builds the page and the worker.
- **The switch off**: a fresh browser at `/app/login` signs in on the old path (keys in the page)
  and shows the tree; the hub stays idle and no worker is started.

### Measurements

- **The worker's wasm**: 12.4 MB as built (`worker` profile, before wasm-bindgen), 6.45 MB after
  wasm-bindgen and `wasm-opt -Oz`, **2.50 MB gzipped** (`gzip -9`); its JavaScript glue 51 KB (9.3 KB
  gzipped). For comparison the page's own wasm is 14.0 MB (5.40 MB gzipped). Fetched only with
  the switch on, by the leader tab, and cached after.
- **Cold start, remembered** (Chromium, the browser started afresh on alice's profile, local
  network, six runs over two builds): the worker loaded in 33-62 ms; the server up with both
  stores open in 0.89-1.27 s (Big, 681 nodes and 2.8 MB, 0.79-1.12 s; Family 61-82 ms); **rows in
  the tree 1.6-2.3 s after the page was asked for**. Before the directory-handle cache, Big alone took
  about 6 s (every file's path walked from the OPFS root).
- **Opening the biggest test store**: the 681-node store as above from OPFS, 0.8-1.1 s; its
  first replication (an empty browser, signed in) listed 1.0 s after the sign-in, its row 3.7 s.
  Its in-memory keyword index is built after the open, in the background (search found it
  at once in the run).

### Not done, and what is different from the contract

- **The per-document log of section 4 is not built.** The browser's server writes documents on
  the same 750 ms debounce as the desktop's. What a closed leader held only in memory is lost from
  its disk, but not in practice from anywhere: a vault link appends every local update to the
  hosted twin as it is taken (so only offline is there a window), and the other tabs' editors
  reconcile on reconnect (the typing test above). Whether the browser (or both apps) adopts the
  log is still wave 0's question.
- **Plain hosted stores** are not replicated in the browser yet (section 5's second bullet; built
  in wave 5, part 1, through `cloudAddHostedStore`): they are logged and skipped. The link would be `addRemoteStore` with `AuthMethod::CloudSession` to
  the session's `rpc_url`; not built or tried. The local stack makes only encrypted stores.
- **No Settings switch**: `?server=worker` only. "Forget This Browser" is on the account page,
  not in Settings.
- **A reload with no network at all** does not load the page: nothing caches the page's or the
  worker's files (no service worker; the edge says `no-store`). Offline use needs the page served
  (or a service worker, which is a decision of its own).
- A relayed store whose owner is away reads as plainly offline in the worker
  (`owner_is_offline` is `false` there, as wave 3 left it; shared since wave 5, part 1).
- Diagnostics are not sent from the worker; the old path's are untouched.
- Semantic search in the browser is wave 6.
- **Firefox and Safari were not run**: there is no Firefox in this machine's Playwright cache
  (getting one is a download) and no Safari on Linux. What stays unproven there (section 10):
  `createWritable` in a worker is missing before Safari 26, so the sync-access-handle fallback is
  written but has run in no browser; it is not atomic against a kill mid-write (the seal then
  reports the file as not opening, and the store's open skips it, above). Safari deletes all
  script-writable storage of a site not visited for seven days unless it is installed to the home
  screen (ITP), which would forget the browser and its unsent edits after a week away;
  `navigator.storage.persist()` is honoured from Safari 17. Firefox's private windows had no OPFS
  before 115 (the worker then starts on memory, said in the log). Web Locks and
  `BroadcastChannel` are in every current engine.

### Found while building

- **A reader's replica says "1 note waiting"** for a read-only share root right after it is
  added, on the desktop as in the browser (`getStoreSync.waiting`: the root with "You can read
  this, not change it."): something writes to a document this device may only read. Seen first
  in the browser; carol's desktop does the same. Not this wave's; worth a look.
- **The worker's files are not hashed** (`pimble-worker_loader.js`, `pimble-worker.js`,
  `pimble-worker_bg.wasm`): a deploy where the edge caches them could pair a new page with an old
  worker. The local edge sends `no-store`; jkbase's caching of `/app/` should be checked before
  the switch is turned on for anyone.
- **The deploy now builds the worker** (trunk builds `crates/pimble-worker` for wasm32 with the
  root lock, rhypedb included): the web build takes about two minutes more here.
- **`Cargo.lock` moves when `tracing-wasm` joins the root workspace**: cargo then resolved
  `windows-sys` afresh for about twenty packages and x11rb's `libloading` 0.8.9 -> 0.9.0, the
  desktop's dependencies. The worker writes its log through a layer of its own instead, and the
  lock gains dependency lines only.

### Changes by file

- `Cargo.toml`: `[profile.worker]`. `Cargo.lock`: dependency lines only (no package added or
  moved).
- `crates/pimble-crypto/src/lib.rs`: `LOCAL_FILE_VERSION`, `local_file_aad`.
- `crates/pimble-host/`: `Cargo.toml` (`pimble-crypto`; the OPFS `web-sys` features);
  `src/disk/sealed.rs`, `src/disk/opfs.rs` (new); `src/disk.rs`, `src/lib.rs` (exports).
- `crates/pimble-store/`: `src/local.rs` (`load_node_docs` skips a file that does not open; its
  test); `Cargo.toml` (`pimble-crypto` for the test).
- `crates/pimble-server/`: `src/server.rs` (`key_rest`, `inherited_device`), `src/handler.rs`
  (`with_key_rest`, `with_inherited_device`), `src/devices.rs` (`InheritedDevice`, its test),
  `src/lib.rs` (export), `src/relay_face.rs` (the face's config names both);
  `tests/link_faults.rs` (the browser-configured device).
- `crates/pimble-client/src/message_port.rs`: `null` ends a `PortText`; `post_gone`; a closing
  client says so.
- `crates/pimble-worker/`: `Cargo.toml` (the `entry` feature; IndexedDB, WebCrypto and worker
  `web-sys` features; `tracing-subscriber`, `getrandom`, `web-time`, `serde`, ...);
  `src/lib.rs`; `src/worker.rs` (`start_browser`, `Keeping`, `forget_browser`, opening the
  browser's stores, `stop`, `forget`); `src/device.rs`, `src/entry.rs` (new).
- `web/`: `hub.js` (new); `index.html` (the hub and the worker's build); `Cargo.toml` (`web-sys`
  features); `src/worker_mode/{mod,hub,backend,pages}.rs` (new); `src/main.rs`, `src/route.rs`,
  `src/app_page.rs`; `README.md`.
- `scripts/local-stack/README.md`: the switch, and checking the browser's server by hand.

### What is left for wave 5

- Delete the old path (`vault.rs`, `vault_backend.rs`, `routed.rs`, `endpoints.rs`, the old
  backend loop, `session.rs`/`keys.rs`/`personal_sync.rs`/`diagnostics.rs` as they are, the old
  login's unlock) and the switch, with the no-second-path check.
- Before that, or with it: plain hosted stores as replicas; diagnostics from the worker (its
  queue on the disk adapter); a relayed store's owner away; a Settings switch and "Forget This
  Browser" in Settings; the per-document log, if wanted; whether the page and worker are cached
  for offline (a service worker); Firefox and Safari runs (the fallback write path above);
  the worker's files hashed or their caching checked on jkbase.

## Built: wave 4, the follow-ups (2026-11-23, not merged or released)

Joe approved four things after wave 4 was committed: running it in Firefox, hashing the
worker's files, a gentler word when persistent storage is refused, and a service worker so the
page opens with no network; then a fifth: **the browser's own server is the web app's default**
("There's no active users of the web app ... you might as well just flip the switch", Joe,
2026-11-23). All five are done; Firefox found one bug of the shared link code.

### The default flipped

`worker_mode::enabled()` is true unless `?server=old` (or the earlier `?server=page`) asked for
the old path, which is remembered in `localStorage` (`pimble.server = "old"`) until
`?server=worker` forgets it. The old path is untouched and still whole, for wave 5 to delete;
on it the page only unregisters the service worker and drops its caches. A browser that was
signed in on the old path (keys in the page's memory, its session cookie, its device id in
`localStorage`, nothing remembered of a switch) lands on the new sign-in page: the old path's
keys never outlived a reload, the cookie is not read by this path, and the device id is handed
to the server at its first start. Checked (`migrate`, below).

### Firefox

Playwright's Firefox (151, `firefox-1532`) ran every browser check of wave 4 (`walk`, `tabs`,
`tabs2`, `offline`, `forget`, `alice`, `measure`, `oldpath`) and the service worker's
(`swcheck`, below). OPFS `createWritable` in a worker, Web Locks, `BroadcastChannel`,
IndexedDB with a `CryptoKey`, WebCrypto: all work as in Chromium; the sync-access-handle
fallback was not taken. What Firefox broke:

- **Signing in hung.** The sign-in page awaited `navigator.storage.persist()` before going to
  the app, and Firefox asks the person (a door-hanger; headless, never answered). The request
  is now made beside everything else and never waited on (`worker_mode::ask_to_persist`), so
  Chromium gains nothing and loses nothing.
- **A new build's service worker taking over while the page's worker loads** left the
  worker's script fetch hanging in Firefox (no `error`, no `loaded`). `hub.js` now gives a
  worker 10 s to load (longer on each retry) and starts it again; the second start loads the
  new build's files.
- **Opening a store was four times slower than in Chromium** (the 681-node store 9.4 s against
  1.0 s; rows in the tree 15 s after a cold start): each OPFS read is a round trip, and
  `LocalStore::load_node_docs` read a store's documents one after another. It now keeps 16
  reads in flight (`READS_AT_ONCE`; what is loaded does not depend on the order), in every host:
  Firefox 2.3 s, Chromium 0.5-0.7 s; the desktop's suites unchanged.
- Two things in the checks themselves, not the app: Playwright's Firefox driver asserts on a
  WebSocket a worker opens (its bookkeeping; the checks pass over that one assertion), and its
  offline switch leaves an open WebSocket running, so the service worker check cuts the
  network by stopping the stack's web server for the part that needs the link down.

### Found by it: a member's create made while down was never sent (shared link code)

`crates/pimble-server/src/vault_link.rs` `note_local_changes_for`: while the link waits out a
backoff it marks each document changed here, and it wrote those marks only when the wait ended.
To a share's member a document the remote never listed is offered at the next reconcile only
when it is marked (a document its owner moved out of the share looks the same), so a create
made while down, on a device stopped inside the wait (a tab reloaded or closed, a crash, a
desktop quit: a stop aborts the link), reached nobody, ever. Seen in the browser (a note made
with no network, the page reloaded before the network came back), and the desktop's the same.
The marks are now written as each document is first marked (a write only for a newly marked
document, not for every keystroke). Test: `share.rs`
`a_members_create_made_while_down_reaches_the_owner_after_a_stop_inside_the_wait` (fails on
the code before: the owner never has the create; passes now).

### Hashed worker files

Trunk hashes no worker. `web/build/post_build.sh`, a trunk `post_build` hook (`Trunk.toml`,
POSIX `sh` and coreutils, so the deploy's buildpack runs it too), renames
`pimble-worker.js` and `pimble-worker_bg.wasm` by a hash of their contents
(`pimble-worker-<hash>.js`, `pimble-worker-<hash>_bg.wasm`), writes the loader that imports
that pair (`pimble-worker-<hash>_loader.js`) and names it in index.html's
`<meta name="pimble-worker-loader">`, which `hub.js` reads. A page therefore always starts the
worker of its own build, whatever a cache kept; the hook fails the build if index.html names
no loader after it.

### Persistence refused: a notice

`BackendEvent::Notice { message }` (new, `pimble-app`): a sentence for the status bar that is
not an error (`events::show_notice`; the status bar's connection state is not touched). A
refused `persist()` says "This browser may clear its copy of your stores if it runs short of
space. Everything already sent is safe on Pimble Cloud." (`worker_mode::backend::Local::notice`
holds it until the UI listens.)

### The page with no network: a service worker

`web/build/sw.template.js`, written by the same hook into `dist/sw.js` with this build's id (a
hash of every file's name and bytes) and the list of its files (12: the page, its wasm and
JavaScript, the snippets, the hub, the worker's three, the icons; the app has no CSS files or
web fonts of its own: its styles are inline and its fonts the system's).

- **Scope: the browser's own server, the default.** `hub.js` registers it (scope `/app/`); on
  the old path (`?server=old`) the page unregisters it and deletes its caches
  (`PimbleHub.leaveOffline`). The old path needs the network to start at all (it mints a token
  and asks for the password), so a cache there would only add a way to run stale code until
  wave 5 deletes it.
- **What it keeps and answers**: on install, this build's files (fetched past the HTTP cache)
  into `pimble-app-<build>`; it answers only GETs under `/app/`, never `/api` or `/rpc` (not
  under the app's path) and nothing it did not list: no store data, no account data, no
  response kept at run time.
- **Never stale beside new**: the page itself (a navigation) is asked of the network first,
  with the kept page only when the network does not answer within 4 s; every other file is
  named by its contents, so a new page finds no old file under its name. A new build's service
  worker installs its cache whole, takes over at once (`skipWaiting`, `clients.claim`), deletes
  every older `pimble-app-*` cache, and tells the pages it took over from; the page says "A new
  version of Pimble is ready: reload the page to use it." (a notice).

### Proof (Chromium and Firefox, the local stack)

- **Both browsers, every check, with no query string** (the default path): `first`, `walk` (the sharing walk-through, steps 4-8: a role
  change reached the browser 106-107 s after the owner made it), `tabs`, `tabs2`, `offline`
  (a reload with no network at all now loads), `forget`, `alice`, `measure`, and `oldpath` with `?server=old` (the old path signs in and shows
  the tree, the hub idle, no worker): all pass in Chromium and in Firefox on the final build.
- **`migrate`**, in each browser: signed in on the old path (`?server=old`), the remembered
  switch then removed as a browser from before the flip has none; `/app/` lands on the new
  sign-in page in both browsers (two navigations, `/app/` and its replacement by
  `/app/login`; no error in the console; the hub on memory, nothing kept yet); signing in there
  works, and the old path's device id is handed to the server.
- **`swcheck`**, in each browser: signed in and remembered, the service worker in control, the
  network fully off: a reload opens the app on its own server from OPFS, tree and all; the
  stack's web server stopped: the same; a note made then (the link down) is kept across a
  reload and reaches Carol's desktop when the server is back; **a second build deployed** (the
  worker's code changed, renamed and given its service worker by the same hook): a reload
  online loads the new page, which starts the new build's worker, the kept files are the new
  build's only, and with the network off again the new build opens from the cache.
- **The suites** on the final tree: `cargo test --release --no-fail-fast -p pimble-server -p
  pimble-store -p pimble-host -p pimble-search`, 421 passed, 0 failed, 1 ignored, on the
  machine's disk and on memory. An earlier native run (before the concurrent reads, the machine
  at a load of 70-160) failed once in `vault_link.rs`
  `a_readers_replica_reads_everything_and_writes_nothing` ("a reader's link pushes nothing": the
  shared folder's hosted head was 3, not 2); run alone it failed 1 of 11 times with this wave's
  link change and 0 of 6 without, and passed in both final runs. Not traced: a write by the
  owner's share upkeep racing the test's first count is the likeliest; worth a look.
- **The fault sweep** (`PIMBLE_LINK_SEEDS=200 PIMBLE_LINK_PARALLEL=4`, after the link change):
  all 200 converged (846 s), 100 with the browser device, 219 crashes (38 the browser's).
- `cargo build --release -p pimble-app -p pimble-cli -p pimble-mcp` builds; the wasm smoke test
  passes (2); `cd web && trunk build --release` builds the page, the worker, its hashed names
  and `sw.js`.

### Measurements

On the local stack, the machine quiet (load about 10), the browser started afresh on alice's
profile (two stores: Family, and Big, 681 nodes and 2.8 MB):

| | Chromium | Firefox |
| --- | --- | --- |
| Big opened from OPFS | 0.47-0.73 s | 2.29-2.35 s |
| both stores open, server up | 0.51-0.81 s | 2.36-2.41 s |
| rows in the tree after the page was asked for | 1.3-1.8 s | 3.9-4.2 s |
| the app with the network fully off (swcheck) | 0.6-8 s | 1.4-8 s |

The last row's spread is the machine's load during the runs (it reached 160 while other work
ran), not the cache. The worker's files as hashed: 6.45 MB of wasm (2.50 MB gzipped), as
before.

### Left, or for Joe

- A new version takes over every open tab at once; the tab keeps running the code it loaded
  and says a new version is ready. A gentler update (waiting until every tab closes) would leave
  a page offline on the older build until then; this one never runs two builds' files in one
  page.
- Safari is still not run.

## Built: wave 5, part 1 (2026-11-23, not merged or released): plain hosted stores, and an owner away

The two things the worker's server still could not do that the old browser path could, built in
the shared server so the desktop and the browser do them by one code path. The old path is not
deleted yet (part 2).

### Plain hosted stores as replicas

- **The call is `cloudAddHostedStore`**, not `addRemoteStore` as section 5 and wave 4's notes
  guessed: the row's `kind` (`"plain"`) says which link the store gets, so the worker's
  `keep_account_stores` and the desktop's "Add Hosted Store..." make one call for every hosted
  store. The request and answer are unchanged; what the call accepts widened (a plain store was
  refused before, for want of key envelopes). Built in `pimble-server/src/handler/hosted_plain.rs`
  (`add_hosted_plain_store`): a whole replica for a whole-store grant (an empty replica with a
  placeholder root, corrected by `adopt_document_root` once the pull lands, as for a vault twin),
  a partial one rooted at the shared nodes for a share, another root added to the open partial
  replica for a share given since; `sync.json` is `mode: "sync"` with the new
  `via_account: true` (`pimble-store` `SyncConfig`), the hosted server's address and no
  credential, and how the account holds the store (`access`, `shared_by`, the roots read only or
  commented).
- **The link is the plain `SyncLink`**, with the vault link's way of reaching its remote
  (`pimble-server/src/sync_link/account.rs`): before every connect the grant is read again and a
  token minted from the account's session, sent as `Bearer` (`credentials.json` is neither read
  nor written); while connected the grant is asked about every `grant_check_interval` and a
  change connects again at once. A refused session waits for a sign-in, as the vault link does.
- **What the account's grant decides, now shared** (`pimble-server/src/grant.rs`, moved out of
  `vault_link.rs` and used by both links): `refresh` (rows read at connect: role, shares added,
  shares ended, `sync.json` kept in line), `grant_shape` and `GRANT_CHECK_EVERY`, `mint` with
  `SessionExpired`, `record_ended_roots`, and `may_push` (an editor or owner pushes, a commenter
  only discussions, a reader nothing).
- **The plain link pushes only what the account may write** (`sync_link/reconcile.rs`
  `send_edit`, every push of the reconcile and of the live loop): nothing for a reader's replica,
  and a document the hosted server refuses (`is_refusal`: out of scope now, or under a root only
  read) is left for a later connect rather than taking the link down. For a link with a saved
  credential nothing changes: its access is always full. `sync_link.rs` became
  `sync_link/{mod,reconcile,account}.rs` (594, 192, 54 lines).
- **A whole replica whose grant is gone ends like a share** (review fix, 2026-11-24). Before it,
  `grant::refresh` handled a store no row names only for a partial replica: a member removed
  from a whole store, or another account signed in on the device, kept `access: Full`, every
  connect was denied, and local edits looked saved and never left the device. The gap was older
  than this part: the code came from `vault_link.rs`, so encrypted whole replicas had it too, and
  the one fix covers both links. Now a whole *replica* (under the replicas directory; a person's
  own store is never ended by whoever is signed in) is made read only and its own root is
  recorded in `StoreManifest::ended_roots`, announced once with `SharesEnded`: the decided
  ended-share behavior (CLAUDE.md, "A member's desktop"), no new state or wording.
  `Store::held_roots` (the listed roots, or a whole store's own) is what `ended_roots` is a subset
  of, so `shown_roots` is empty and `every_share_ended` is true: the row says `no longer shared`,
  shows nothing under it, and its menu leaves "Remove Replica..." alone on. `LocalStore`
  keeps a whole replica's ended set to its root (`set_ended_roots`), follows the root the first
  pull adopts (`set_root_node_id`), and judges everything in it ended (`under_ended_root`: no
  link followed into it, no search hit). Rows naming the store again un-end it at the next
  connect, which also restores the access.
- **A mount's source, when the mounting store is held through the account** (a plain replica with
  `via_account`, or a vault link's), is added through the account the way `cloudAddHostedStore`
  adds a store (`handler/mount_sources.rs` `SourceRoute::Account`, then
  `handler/hosted.rs` `add_hosted_store`: a source the account holds nothing of is refused,
  never made a whole replica held as owner; since wave 5, part 3 `cloudAddHostedStore` refuses
  the same way, and the `Unlisted` choice is gone). Before it the source was
  tried at the mounting store's address with whatever `credentials.json` held for it, which for
  an account replica is nothing: the mount went `Unavailable`. A mount ref's own
  `source_remote` is still tried after the account when it names another server.
  `cloudAddHostedStore`'s body moved to `handler/hosted.rs`, and the mount-source resolution to
  `handler/mount_sources.rs`; `handler.rs` shrank by both.
- **Adding another share to an open replica keeps its `last_sync`** (and `last_seq`): the
  rewrite of `sync.json` in `hosted_plain.rs` reads what was there first. Before it a mount of the
  replica read `Connecting` rather than `Cached` until the next sync.
- **The browser**: `keep_account_stores` no longer skips plain rows. **The desktop**: "Add Hosted
  Store..." lists plain hosted stores too (`pimble-app/src/events.rs`), the same call.
- "New Store..." in the browser still makes an encrypted store only, as the button always has.

### A relayed store whose owner is away

- The relay accepts a member's upgrade and closes it with 4404 when the owner's computer is not
  connected; the desktop asked with a raw tokio-tungstenite socket. That question is now a seam
  method, `Http::websocket_close_code(url, token)` (`pimble-host/src/http.rs`): natively
  tokio-tungstenite with `Authorization: Bearer` (`http/native_socket.rs`), in the browser a
  `web_sys::WebSocket` with the token as `access_token` (`http/browser_socket.rs`; a code is read
  only after the socket opened, so a refused handshake's 1006 is never taken for an answer). Both
  answer a code only when the first thing after opening is a close, within
  `FIRST_WORD_WAIT` (2 s) after a `SOCKET_CONNECT_TIMEOUT` (15 s) connect.
- `owner_is_offline` is shared (`pimble-server/src/relay_probe.rs`); the wasm32 stub that
  answered `false` is gone, and the vault link asks through `handler.http()` on both targets.
- So the worker shows `owner offline` exactly as the desktop does, and, as on the desktop, the
  store stays editable from its replica (edits go when the owner is back; docs/RELAY_CONTRACT.md).
  The old browser path made such a store read-only because it held no copy of its own; the worker
  holds one, so it does not.

### Proof

- `crates/pimble-server/tests/hosted_plain.rs` (new, the sharing harness's stub accounts service
  and JWT-mode hosted server): a plain hosted store replicated through the account both ways,
  `sync.json` with `via_account` and no token, the placeholder root corrected, a restart whose
  link comes back from `sync.json` alone (a server opens no store by itself; its client opens
  the replica by path, as the app's open-store list and the worker's replicas folder do); a reader
  whose replica refuses edits and sends nothing (H is made to take Bob's writes through a stub
  knob, `token_role`, and a change slipped onto the replica's disk while it was off never reaches
  H until Bob is made an editor, when it goes), made an editor while connected and then writing
  to `H`; a share as a partial replica that never receives what is outside it, writes inside it,
  and records its removal as an ended share; a whole replica removed, granted again, then ended
  by another account signing in and held again by its own; another share joining the open
  replica with `last_sync` kept (the link held offline by `LinkFaults`); a mount in an account
  replica whose source resolves through the account. `pimble-store`
  `tests/ended_whole_replica.rs`, the core's and the app's tests of a whole store's ended root.
- `pimble-host` `http::socket` tests: a server that closes with 4404 answers `Some(4404)`, one that
  speaks first answers `None`, no listener answers `None`. The relay suite (`tests/relay.rs`)
  covers the owner-away path end to end on the desktop.
- The server suites (`pimble-host`, `pimble-store`, `pimble-server`) pass on the real disk and with
  `PIMBLE_TEST_DISK=memory`.
- In a browser (headless Chromium through Playwright, on `scripts/local-stack` with made-up
  accounts, the default worker path): a plain store created on the hosted server and held by
  Alice showed in her page as `synced`; text she typed into a note reached her desktop replica
  (added with `cloud-add-hosted`, `via_account` in its `sync.json`) and its search, and a note
  made on the desktop appeared in the page live. A relayed store's folder shared with Bob showed
  in his page as `shared by alice · encrypted · synced`; with the owner's desktop stopped, the
  row read `owner offline` within a second.

### Not done

- `handler.rs` (about 7,050 lines) and `vault_link.rs` (about 3,480) are still far over the
  700-line limit; this part moved code out of both (`grant.rs`, `hosted_plain.rs`) but did not
  split them.
- This part's browser proof ran in Chromium only, not Firefox.
- No test covers an expired sign-in on a plain link held through the account (the link waits
  while the session is expired, as the vault link does; the vault link's tests cover that path).
- `tests/in_process.rs` `a_reader_is_refused_in_process` fails on `master` (2ff7163) as on this
  branch, and was not caused by it: the store's index turns ready after the reader subscribes,
  and the first notification it hears is `IndexStateChanged { Ready }` (source `None`, since
  eb60d11's startup work), not the service's edit the test waits for.
- An ended whole replica is un-ended only by its link's next connect (the plain link backs off
  to 30 s); "Add Hosted Store..." does not list it, since it is open here.
- `tests/vault_link.rs` `a_share_recipients_replica_holds_its_scope_and_edits_it_with_the_owner`
  fails about one run in two at this part's base as with it (line 1408: the member's new
  document is found wrapped under a second scope key as well as the share's), a race between the
  test's look at H and the owner's upkeep; not caused by this part and not fixed here.
- The old browser path is still there (wave 5's later parts remove it).

## Built: wave 5, part 2 (2026-11-23, not merged or released): diagnostics, the account page, shared helpers

What the worker's browser path still lacked next to the old one, so that the old path can go in
its own step. The old path is not deleted yet.

### Diagnostics are the server's (section 6, item 10)

- **One core, three recorders.** `pimble_core::diagnostics` (`records.rs`) holds what every
  recorder shares, with no I/O: the envelope (`Envelope::record`, `Envelope::report`), the log
  budget (`LogBudget`: repeats at 1, 2, 3, then tens; 500 warnings, 2,000 info lines), the info
  targets, scrubbing a record's values, ids and the report code. The desktop's recorder
  (`pimble-server/src/diagnostics/native.rs`, `queue.rs`, `system.rs`, `report.rs`: the old
  799-line `diagnostics.rs` split by job), the worker's, and the page's (`web/src/diagnostics.rs`)
  are each built on it.
- **The worker's server records** (`diagnostics/worker.rs` over `diagnostics/worker_core.rs` and
  `diagnostics/on_disk.rs`, the latter two built for both targets and tested natively): a `tracing` layer installed beside the console's
  (`pimble-worker/src/entry.rs`) keeps the recent log and admits lines through the budget; the
  records wait in memory (at most 1 MiB) until the page has said its choice, then go every
  minute onto the server's own disk (`Paths::diagnostics()`,
  `records-<millis>-<session>-<owner>.ndjson`,
  at most 4 MiB, the oldest dropped), and are sent through the host's `Http` to the accounts
  service's `/api/v1/diagnostics/records` with the keystore's session as the bearer. The
  accounts service adds the send key as before; no browser carries it. Signed out, they wait.
  Records say `os: "web"`, and a `system` record says `program: "worker"` (once per session, the
  first time diagnostics are on, even after an off).
- **A batch is sent only as the account that recorded it** (review of e31e31e, 2026-11-25). A
  pump writes one batch per account that was signed in when its records were made; the batch's
  name carries the owner (`none`, or `u` and the account id in hex). A send takes the signed-in
  account's own batches and the ones recorded while no one was signed in (those carry no
  `account`, and wait for the next sign-in as before); another account's batches wait on the
  disk for that account (or the 4 MiB cap, or an off). A batch whose name names no owner is
  deleted unsent.
- **An off is never overtaken by a pump** (same review). `WorkerDiagnostics` holds the
  recorder, the choice, the install number and the queue's folder, and its two steps that touch
  the queue (applying a choice, a pump) take turns behind one async lock. `setDiagnostics`
  turns the switch at once; a pump already sending asks the switch before reading and before
  sending each batch, so after the off nothing more leaves (a request already on the wire when
  the person saved finishes); the off's clearing runs once that pump has ended, so nothing it
  wrote stays on the disk. A choice that comes before the server has given the queue's folder
  is kept and applied when the folder comes (`attach`, from `run`).
- **Every off owes a clear, even when an on follows it** (2026-11-25). Saving "off" and then
  "on" while a send was in flight kept only the later choice: the pump, asking only the switch,
  went on sending batches recorded before the off, and the step saw "on" and never cleared.
  Now every off advances a count (`State::offs`) and the queue records how many it has been
  cleared for (`State::cleared`). A pump notes the count when it takes its records and goes on
  (before each write, each read and each send) only while the switch is on and no off has come
  since; no pump starts while a clear is owed; and applying a choice clears the queue whenever
  `offs > cleared`, whatever the switch says by then. So nothing recorded before the latest off
  is sent, and none of it stays on the disk. Tested with an off then an on during a send (the
  service holding the request) and during a pump's write (a disk holding the batch).
- **An off is written down before it is waited on** (2026-11-25, part 5). The counts above are
  in memory, so a tab or app closed between an off and its clear left the batches recorded
  before it on the disk, and the next run sent them. Now `configure(off)` writes a mark,
  `clear-before-<wall ms>-<session>-<offs>` (outside the `records-*` names, so no pump takes
  it), in the queue's folder before it waits for the step (`on_disk::mark_cleared_before`).
  `attach` and `apply` delete every batch older than the newest mark (`clear_through`) and,
  once every one went, remove the marks they read; `pump` takes nothing while any mark exists.
  The in-memory counts keep their guarantees beside it. Tested by a worker ended between an
  off and its clear: the next worker clears the batch before it sends anything.
- **Batches and offs are numbered, never timed** (2026-11-25, review of part 5). Naming both by
  wall time let a clock that stepped back between a pump and an off name the off earlier than
  a batch from before it, and a worker that ended before the clear left that batch to be sent.
  Batches are now `records-s<seq>-<session>-<owner>.ndjson`, `seq` from the queue's own
  sequence: kept in `batch-seq` beside them (`Queue::keep_seq`, written before any batch is
  named by it) and read at `attach` as the greater of that and every number a batch or a mark
  on the disk names (`Queue::last_seq`). A pump takes its numbers when it takes its records, so
  an off's mark, `clear-through-<seq>-<session>-<offs>` naming the last number given
  (`Queue::mark_cleared_through`), covers every batch from before it; `clear_through` deletes
  the batches numbered at or below it. Nothing in the worker's queue reads the wall clock. A
  batch named by a version before this (`records-<wall ms>-...`) sorts first and is before any
  mark: every off clears it, and an old `clear-before-...` mark asks for everything. Tested
  with the runtime's wall clock stepped back between two pumps and the off, and the worker ended
  before the clear: the next worker deletes both batches, and the clock was never read.
- **The page notes a clear it owes** (2026-11-25, review of part 5). An off said before the
  server has given the queue's folder has nowhere to be written, so a worker that heard "off"
  and "on" and ended before `attach` lost the off, and the next one sent what came before it.
  Now saving "off" stores a tag beside the choice (`localStorage` `pimble.diagnostics`
  `clear_owed`, through `pimble_core::diagnostics::SavedChoice`, which keeps the rules: an off
  gives a new tag, an on keeps one owed, a confirmation drops only its own tag). The page sends
  `setDiagnostics { clear_owed: true }` while one is noted; the worker counts it as an off at
  once, waits for the folder, clears (`WorkerDiagnostics::clear_owed_by_the_page`) and answers
  `{ cleared: true }`, and only then does the page drop its note. The desktop's server answers
  `cleared: true` always (its off deletes at once). Tested by a restart after an off said
  before the folder: with the note nothing from before the off is sent; the control without
  it sends the earlier batch.
- **A send is never waited for longer than 15 s** (`SEND_TIMEOUT`). The step is held across a
  request, and a browser's `fetch` has no timeout, so a stalled request delayed an off's clear
  for as long as it stalled. The pump's requests go through `pimble_host::http::Bounded`, which
  ends a call on the server's `Runtime` clock alike on the desktop and in the worker (a request
  already on the wire is not taken back; only the waiting ends, and the batch waits for the
  next send or the clear). Tested with a request that never answers and an off: the clear runs
  within the timeout and nothing more is sent.
- **The choice stays the page's** (Settings > Diagnostics, `localStorage`): the page tells the
  server `setDiagnostics { enabled, install, clear_owed }` at every connection and whenever it
  saves. The
  install number is the page's, so the page's records and the worker's share one
  (`Queue::install`: an offered valid id wins and is kept). Off clears the worker's queue.
- **Send Diagnostics** in worker mode is `sendDiagnostics { note, stores, logs }`: the page hands
  over its recent log as `page.log`, the server adds `worker.log` and posts the report to
  `/api/v1/diagnostics/reports` as the signed-in account. It answers the code, or a sentence
  (signed out: "Sign in to send diagnostics from this browser."). On the desktop the same two
  RPCs save `diagnostics.json` and run `send_report` (the app still calls its server's
  `send_report` in process, as before). Both RPCs are Service-only.
- `HttpRequest` carries a `Body { content_type, bytes }` (`Body::json`, `Body::ndjson`) in
  place of `json`, so the NDJSON batches go through the same seam.
- The page's recorder keeps its own records (`program: "web"`, queued in `localStorage`, sent
  with the cookie) in both modes, and in worker mode takes the account's id from
  `CloudStatusChanged`.

### The account page

- **Keep what I've read in step** is already the server's in worker mode: Settings > Account's
  switch sends `cloudPersonalSync`, which the worker's server answers as the desktop's does. The
  worker's account page now says where the switch is; it has no second switch.
- **Change password, a new recovery code, members per store: not built. They need Joe's call**
  (the brief's stop rule: new security-sensitive RPCs, where keys live, roles). What the old
  page does in the page (derive, rewrap the account keys, `PUT` them) has to happen where the
  keys are, which in worker mode is the server's keystore. The proposal:
  - `cloudChangePassword { current_password, new_password }` → empty: in the keystore, derive
    the old and new password keys (`pimble_crypto`), unwrap and rewrap the account keys, and send
    the accounts service the new `auth_key` and wrapped blob, as `cloudSignIn` does in reverse.
    Service-only. The session stays.
  - `cloudNewRecoveryCode {}` → `{ code }`: the code is made in the server and wraps the account
    keys under its KEK; the answer is the one place it is shown. This is the first RPC that would
    answer a secret to the page. It is the person's own, shown once, as on the old page.
  - Members of a whole store: either the share RPCs (`cloudShareInfo`, `cloudInvite`,
    `cloudRemoveMember`) take `node_id: Option<NodeId>` (`None` meaning the whole store), or
    three new ones (`cloudStoreMembers`, `cloudStoreAddMember`, `cloudStoreRemoveMember`). Adding
    a member of a vault store seals the current store key for them. The old page could also
    make someone an owner; the share RPCs refuse Owner. Which roles a whole-store invite may
    give is the roles model, so it is Joe's.

### Shared helpers out of old-only files

- `web/src/open_link.rs`: `keep_open_link` and `take_open_link`, out of `app_page.rs`; both
  modes call them.
- `web/src/kdf.rs`: `derive_timed`, out of `session.rs`; the signup, recover and account pages
  and the old session call it.

### Proof

Chromium (Playwright), the local stack, a made-up account, in worker mode. The accounts service
was pointed at a stub diagnostics server on loopback that kept what it was sent and answered a
report code; nothing left the machine.

- Help > "Send Diagnostics..." with a note answered "Sent. Its code is STUB4242". The report
  reached the stub through `/api/v1/diagnostics/reports`, with the accounts service's key and
  the account's id. It carried the note, `page.log` (16 lines) and `worker.log` (4 lines), and
  its install number was the page's.
- The worker's own records arrived through `/api/v1/diagnostics/records` about ten seconds after
  it started (its `system` record says `program: "worker"`), with the same install number and
  account as the page's records. No address reached the stub.
- Settings > Account, "Keep what I've read in step": off sent `DELETE /api/v1/me/personal`
  from the worker, and on sent `GET` then `PUT`.
- `cargo test` passes for pimble-core, pimble-host, pimble-rpc, pimble-client and pimble-server
  (the new `diagnostics::on_disk` tests among them), with one exception:
  `tests/in_process.rs` `a_reader_is_refused_in_process` fails. It fails the same way at
  eed398e without this part's changes: the first notification has no source where the test
  expects `"service"`. `cargo check --workspace --tests` and both browser crates' checks for
  wasm32 are clean.

### Not done

- Change password, a new recovery code and members per store in worker mode wait for the
  decisions above.
- A panic in the worker is not recorded: a panic traps the wasm instance before any write to
  its disk can finish. The page's panic hook still records the page's own.
- A report's `page.log` can name the account's email local part: `pimble_app::boards` logs the
  author stamp at info ("board edits are stamped as ..."), and the scrubber takes out whole
  addresses only. The old path sends the same line.
- `a_reader_is_refused_in_process` fails at eed398e and here (above); not looked into.
- `handler.rs` (about 7,060 lines), `pimble-rpc/src/types.rs` (2,163) and
  `pimble-client/src/client.rs` (1,815) are still over the 700-line limit. This part put its
  code in new modules (`handler/diagnostics.rs`, `pimble-rpc/src/diagnostics.rs`) but split none
  of them.

## Built: wave 5, part 3 (2026-11-25, not merged or released): the account page's own sections, and adding only what the account holds

### Joe's decisions (2026-11-25)

- **(e) Change password happens in the worker**, through a Service-only `cloudChangePassword`:
  the server that holds the account keys derives the new password's keys, rewraps the account
  keys and updates the accounts service. The page sends what was typed and nothing else.
- **(f) A new recovery code: allowed, shown once.** `cloudNewRecoveryCode` answers the code once;
  it is never stored or logged. It is the only RPC that answers the page a secret.
- **(g) Whole-store members: an owner may make another owner** from the browser. A store always
  keeps at least one owner, which the accounts service enforces (409, said as a sentence). An
  owner lists a whole store's members and changes or removes them, as the old page did.
- **(h) `cloudAddHostedStore` refuses a store the account holds no row for**, with a whole
  sentence, in place of adding it whole and writable as an owner (`Unlisted::AsOwner`, which the
  link's first grant refresh then ended as "no longer shared").

### Changing the password (`crates/pimble-server/src/handler/account.rs`)

`cloudChangePassword { current_password, new_password }`, Service-only, signed in:

1. A new password under 8 characters is refused in a sentence before anything is derived.
2. The account's keys as the accounts service holds them (`GET /me/keys`) must be the keystore's
   (same public keys), or the change is refused: it would wrap keys this device does not hold.
3. Off the async thread (`runtime().run_heavy`), the current password's keys are derived and the
   served blob unwrapped with them. A wrong current password fails here, as "That is not your
   current password.", and nothing is sent. Then `pimble_crypto::new_password_material`
   (`crates/pimble-crypto/src/rotate.rs`) makes a fresh salt, the new `auth_key` and the account
   keys wrapped under the new KEK.
4. One `POST /api/v1/me/password { current_auth_key, auth_key, kdf, account_key_blob }`. The
   service checks the current auth key as login does and writes the hash, the kdf and the blob
   in one update.

**Atomic from the person's view**: the only write is that one update, so the account always
opens with exactly one password, old or new, whose blob holds the same keys. A change that never
reached the service leaves the old password; one whose answer was lost leaves the new one, and
asking again with the old password says "That is not your current password." Both are tested
through a proxy that refuses or drops the answer
(`crates/pimble-cloud/tests/integration/account_worker.rs`).

**A password change signs out the account's other sessions; the session that made it stays
signed in** (Joe, 2026-11-25: "Sign out the others"). Built in part 4 below, which replaces
what this part first built (other sessions stayed signed in, as the old page had them). The
account keys do not change with the password, so ending sessions does not take keys back
from a device that holds them; the keystore of this device needs no change.

### A new recovery code

`cloudNewRecoveryCode { current_password }` → `{ code }` (the password since part 5, Joe,
2026-11-25), Service-only: `pimble_crypto::new_recovery_material`
makes the code, a salt and the account keys wrapped under the code's KEK (run off the async
thread), the server sends `POST /me/recovery-code { current_auth_key, recovery_salt,
recovery_key_blob }`, and only then answers the code. Neither the request nor the response type prints it (`Debug` is
redacted, `NewRecoveryMaterial` zeroes it on drop), and it is never written to the keystore.
The page shows it with "Shown once. The old code no longer works.", a checkbox "I have written
this down somewhere safe" and "Done", which drops it.

### Whole-store members

Three new Service-only RPCs (the contract's second proposal; the share RPCs are left alone,
since a share's members are never owners): `cloudStoreMembers { store_id }`,
`cloudStoreSetMember { store_id, email, role }` (adds, or changes a role) and
`cloudStoreRemoveMember { store_id, email }` (a member, or an invitation withdrawn). Each answers
the store's members after it, so the page needs no second call.

- Only an owner of the store, by the account's rows, is answered; anyone else is told "Only an
  owner of this store can see and change who holds it."
- **Encrypted stores**: an address that holds nothing of the store yet must belong to a verified
  account (`GET /users/lookup`; a role change of a member asks no lookup, since lookups are
  limited to one per 200 ms per caller and a 429 is said as "Pimble Cloud asks to slow down."), or
  the answer is "No verified Pimble account has that address. An
  encrypted store is shared only with an account, whose key can be sealed to it." Once the
  member is put, the current store key (`fetch_scope_keys`) is sealed to their public key with
  the context `store:{id}` and `PUT` to the store's keys. If that hand-over fails, the person
  holds the store but not the key yet, and the sentence says to add them again.
- **Plain stores** take an invitation for an address with no account, as before.
- An owner needs a verified account (the service's own 400, as a sentence). The last owner is
  neither demoted nor removed (the service's 409: "A store always keeps at least one owner. Make
  someone else an owner first.").

The page (`web/src/worker_mode/account_sections.rs`) lists the stores this account owns whole
(`cloudListHostedStores`, `root` none and role `owner`), each with "Members": a row per member
with a role select (owner, editor, commenter, reader) and "Remove", and "Add by email" with a
role.

### (h) Adding only what the account holds

`handler/hosted.rs` `add_hosted_store` takes the account's rows for the store
(`HeldAs::from_rows`) or refuses with "The signed-in Pimble Cloud account does not hold store
<id>, so it cannot be added here." Nothing is created. The mount-source route
(`handler/mount_sources.rs`) uses the same call. Tests: `crates/pimble-server/tests/hosted_add.rs`
(a store with no row, and an unknown id, are refused and nothing is open; a store the account
owns, added on a second device, is still whole and writable after its first grant refreshes).

### Known limits

- Removing a member of an encrypted store does not rotate the store key, as on the old page
  (docs/NODE_DOCUMENT_CONTRACT.md's known limits).
- The service counts owners and then writes, outside a transaction: two owners demoting each
  other at the same moment could both pass the check. The old page had the same window.

### Proof

Chromium (Playwright), the local stack, made-up accounts (`erin.w5c@example.com`,
`frank.w5c@example.com`) with fixture passwords, in worker mode. Evidence (screenshots, step
logs, the desks' answers) is under the session's scratchpad, `wave5efg/`.

- **Members.** Erin's encrypted store "Garden" was hosted from a headless desk. On the account
  page "Your stores" listed it; "Members" showed Erin as owner. Adding Frank as **owner** showed
  him as owner, and Frank's own desk then listed the store as `role=owner`, added it and read
  the note's title "Tomato beds" from the encrypted replica: the key was sealed to him. His role
  changed to editor; demoting Erin, then the last owner, answered "A store always keeps at
  least one owner." and her row still read owner; removing Frank took his row away; removing
  Erin was refused with the same sentence.
- **Password.** A wrong current password answered "That is not your current password."; the
  change answered "Your password is changed.". After "Sign Out", the old password was refused
  at sign-in and the new one signed in. Erin's desk, signed in before the change, still listed
  her stores.
- **Recovery code.** "New Recovery Code" showed the code with "Shown once. The old code no
  longer works.", "Done" stayed disabled until the checkbox was ticked, and the code left the
  page at "Done" (screenshots mask it). Then Forgot -> the mailed link (logged by the stack) ->
  that code and a new password: the account was recovered, and the new password signed in.
- No password appears in the stack's logs or the pages' consoles.
- `cargo test`: pimble-crypto, pimble-rpc, pimble-client and pimble-server pass except the two
  tests below; pimble-cloud's integration suite runs against `rhypedb-server` and passes
  (104 tests, the four in `account_worker.rs` among them). `cargo check --workspace --tests` and
  `web/`'s `cargo check --workspace --target wasm32-unknown-unknown` are clean.

### Found while building

- The accounts service takes one `/users/lookup` per 200 ms per caller. The first cut looked up
  every address it was given, so changing a member's role right after adding them answered 429.
  Only an address that holds nothing of the store is looked up now, and a 429 is a sentence.
- After a refused role change the row's select kept showing the role just picked. Each row's
  select now reads the member's role from the last answer, and a refusal makes it read again.

### Not done, and open

- `tests/in_process.rs` `a_reader_is_refused_in_process` fails as at this part's base (part 2
  above). `tests/vault_link.rs` `a_share_recipients_replica_holds_its_scope_and_edits_it_with_the_owner`
  failed in every run here (four of four) at line 1408, the known race between the test's look
  at H and the owner's upkeep (part 1's notes); the test does not touch what this part changed.
- A sign-in refused for a wrong password showed the service's raw answer ("cloud service at
  ... answered 401: ...") on the worker's sign-in page. Part 4 makes it a sentence.
- `handler.rs`, `pimble-rpc/src/types.rs`, `pimble-client/src/client.rs` and
  `crates/pimble-cloud/tests/integration.rs` are still over the 700-line limit; this part's code
  is in new modules (`handler/account.rs`, `cloud/account.rs`, `pimble-rpc/src/account.rs`,
  `client/account.rs`, `web/src/worker_mode/account_sections.rs`,
  `tests/integration/account_worker.rs`).
- The old browser path is still there.

## Built: wave 5, part 4 (2026-11-25, not merged or released): a refused sign-in in a sentence, and a password change signs out the others

### Joe's decision (2026-11-25)

- **A password change signs out the account's other sessions** ("Sign out the others"). The
  session that made the change stays signed in. This replaces part 3's "Other devices'
  sessions stay signed in".

### A refused sign-in is a sentence (`crates/pimble-server/src/cloud/refusal.rs`)

The shared server turns the accounts service's refusals into `CloudError::Refused { status,
refusal }`, whose Display is a whole sentence (`Refusal`), and whose `status()` is still the
service's, so code that reads a 401 reads it as before. The browser's worker-mode sign-in page,
the account page and the desktop's Account modal show the server's answer as it is, so all of
them say:

- `POST /login` 401: "That email and password do not match." An address with no account
  reads exactly the same: `GET /kdf` answers it with a decoy and `/login` with the same 401, so
  the sentence cannot say whether an account exists, and does not try.
- `POST /login` 403 `email_unverified`: "Check your inbox: this address has not been verified
  yet." The service says it only once the password matched.
- A call made with the session (`GET /me/keys`, `POST /me/password`, `POST /me/recovery-code`)
  answered 401: "Your Pimble Cloud sign-in has ended. Sign in again.", and the keystore marks
  the session ended (as minting a token does), so the account's status asks to sign in again.
  `POST /me/password` with a wrong current password now answers 401 `wrong_current_password`
  (it was `unauthorized`, like an ended session), which reads "That is not your current
  password."
- 429 anywhere in these: "Pimble Cloud asks to slow down. Wait a moment and try again."; a 5xx:
  "Pimble Cloud could not answer just now. Try again in a moment."
- The members RPCs (part 3) say an ended session and a 5xx the same way.

The old browser path's wording (`web/src/session.rs`) was the model. `cloud.rs` (724 lines) is
split by responsibility into `cloud/` (`sessions`, `stores`, `keys`, `people`, `members`,
`account`, `notices`, `refusal`), with no change of behavior; the accounts service's recovery
and password routes moved from `routes/accounts.rs` (865 lines) to `routes/password.rs`.

### A password change ends every other session (`crates/pimble-cloud/src/routes/password.rs`)

`User.session_epoch` and `Session.epoch` (u64, absent on old rows: 0): a session is valid
while its epoch is not below its account's (`SessionRow::ended_for`, checked by
`AuthedUser`, so the cookie, a session bearer, `/me`, `/token` and the relay's owner tunnel
all refuse an ended session as "invalid session"). A session is made with the account's epoch
as read when its password was checked.

`POST /me/password`, after the current password is checked: the new epoch is the clock in
milliseconds (above the stored epoch should the clock be behind); the changing session's epoch
is raised to it first, which ends nothing; then the one `User.update` that writes the new
hash, the KDF parameters and the wrapped keys raises the account's epoch to it, which ends
every other session at that moment. So the change and the sign-out are one write: a change that
never reached that update leaves the old password and every session; one that did leaves the
new password and only the changing session. A sign-in that checked the old password and is
written after the change carries the old epoch and is ended too. The ended sessions' rows are
then deleted, as housekeeping (they are refused already; a failure there is logged).

**Recovery** (`POST /recover/{token}/complete`) already ended every session (docs/CRYPTO_CONTRACT.md
"Phase 2a-2", "deletes every session"), so it is the same decision and nothing about what it
does changed: it now raises the epoch in its own one update, so no session survives even a
racing sign-in, and deletes the rows after, as the password change does.

### Known limits

- **JWTs are not revoked.** Pimble servers keep no session state and verify hour-long JWTs
  against the JWKS, so a JWT minted before the change stays valid until it expires (at most an
  hour). A device whose session ended keeps a live connection until its token expires or it
  reconnects; it then fails to mint a new one and asks to sign in again. JWT revocation is not
  built.
- The relay's owner tunnel is authorized with the session when it opens; a tunnel opened before
  the change stays up until it drops, and is refused when it reconnects.
- ~~Two password changes from two sessions at the same moment~~ (fixed 2026-11-25, part 5):
  they run one at a time under the account's lock, and the second, whose session the first
  ended, writes nothing and is refused in a sentence.

## Built: wave 5, part 5 (2026-11-25, not merged or released): review fixes

### Joe's decision (2026-11-25: "Password and an email")

- **A new recovery code needs the current password**, checked as a password change checks it:
  the server worker proves it first (`prove_current_password` in
  `crates/pimble-server/src/handler/account.rs`: `GET /me/keys`, the password's keys derived and
  the account keys unwrapped with them) and the accounts service checks its `auth_key` again.
  `cloudNewRecoveryCode { current_password }`; an empty one reads "Enter your current password
  to make a new recovery code.", a wrong one "That is not your current password.", and nothing
  is sent then. The browser's account pages (both modes) ask for it beside "New Recovery Code".
- **Every change of an account's recovery code mails the account** (docs/CRYPTO_CONTRACT.md,
  "Every change of an account's recovery code is mailed"); the mail carries no secret.

### The rest

- **Last owner under races** (docs/CLOUD_CONTRACT.md, "A store always keeps an owner"), and
  **one write per account at a time** (docs/CRYPTO_CONTRACT.md): the per-key locks of
  `crates/pimble-cloud/src/locks.rs`, with the check made inside the lock on data read there.
- **A new code whose answer is lost** (no answer, an undecodable one, or a 5xx) reads "Pimble
  Cloud did not answer, so this computer cannot tell whether the new recovery code was saved.
  Your old recovery code may have stopped working, so make a new one now."
  (`RECOVERY_CODE_OUTCOME_UNKNOWN`, `cloud::outcome_unknown`). A refusal the service gave before
  writing (an ended session, a wrong password, a 409) keeps its own sentence.
- **An off in diagnostics is written down** before it is waited on (part 2 of this wave's
  diagnostics notes, "An off is written down before it is waited on").

### Proof

- `crates/pimble-cloud/tests/integration/sessions.rs`, against a real `rhypedb-server`
  (`RHYPEDB_SERVER_BIN=~/dev/rhypedb/target/debug/rhypedb-server`, not skipped): after a change,
  another session is refused as a cookie and as a bearer (`/me`, `/me/keys`, `/token` all 401);
  the changing session's `/me` and `/token` answer 200; only its row is left; a later sign-in
  works and ends nothing; a change from that session ends the first changer's. A refused change
  ends nothing and answers `wrong_current_password`. A session written for the old password
  after the change is refused. A recovery ends every session.
- `tests/integration/account_worker.rs`: the device that changes the password stays signed in
  (`cloudStatus`, `cloudListHostedStores`, `cloudNewRecoveryCode` work); another device signed
  in before is told "Your Pimble Cloud sign-in has ended. Sign in again.", changes nothing, and
  signs in again with the new password. A device's sign-in refused for a wrong password, for an
  unknown address, and for an unverified one reads as the sentences above, the first two
  identical.
- Unit tests: `cloud::refusal` (every mapping, the status kept) and `handler::account`.
