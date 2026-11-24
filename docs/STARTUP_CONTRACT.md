# Starting up: fast, and saying what it is doing (draft, 2026-11-20)

Joe, 2026-11-20: "When I load the app on my desktop, a lot of time goes by where Pimble Cloud
shows disconnect (needs a spinner or something) and the explorer loads in in chunks, slowly, but
there's no feedback."

Status: **built 2026-11-20** (not released); see "Built (2026-11-20)" at the end.

## What the log of that start shows (Joe's desktop, 0.28.11, 2026-11-20 21:08)

| Time | What |
| --- | --- |
| 0.0 s | app starts |
| 0.3 s | the server on 7462 belongs to pimble-mcp: asked to hand over |
| 0.8 s | embedded server up, app connected, three saved stores asked to open |
| 1.1 s | Personal opened; **search index open failed: "data directory is locked"** |
| 11.2 s | Personal's index rebuilt from scratch (484 nodes); Personal shown |
| 11.5 s | Family Management opened; index locked again |
| 18.7 s | its index rebuilt (630 nodes); shown |
| 19.0 s | Apex (35 nodes) shown |
| 19.0 s | only now: "Cloud account: signed_in=true", sync states, the first children |

An earlier start the same day with no hand-over opened all three stores in 60 ms. So:

1. **The hand-over leaves the indexes locked.** The server pimble-mcp ran had the stores open;
   handing the port over did not release their rhypedb locks before the app opened the same
   index directories (now in `<data dir>/pimble/stores/<id>/index/`), so each open failed and the
   app rebuilt every index, about 10 s for 500 notes. (Where the rebuild wrote, while the
   directory was locked, needs checking too: it must not leave a second index or a stale lock.)
2. **Opening a store waits for its index.** An open that has to rebuild holds the store's
   answer for the whole rebuild, and the stores open one after another, so each one's rebuild
   delays the next and everything after.
3. **Everything else waits behind the opens.** The cloud status, sync states and first children
   are asked after the opens, in the same line, so "Pimble Cloud: disconnected" shows for 19 s
   while the connection itself was up in under a second.
4. **Nothing says any of it.** No spinner, no "opening", no "indexing".

## The rule

**Start-up shows the explorer and the account at once, and says what is still on its way.**
Nothing a person can see waits for search.

## What changes

### Faster
- **Hand-over releases everything.** A server asked to hand over closes its stores (flushing
  documents, closing each search index, releasing its locks) before it answers that the port is
  free; the taker opens nothing until then. A test: pimble-mcp's server with stores open hands over
  to an app server that opens the same stores, and no index is rebuilt.
- **A locked index is waited for, not rebuilt over.** Opening an index that another process of
  this computer holds retries for a few seconds; if it is still held, the store opens without
  search and the index is opened again in the background (said in the explorer, below), never
  rebuilt while another holds it.
- **Search never holds a store's opening.** A store answers as soon as its documents are open;
  opening or rebuilding its index runs after, in the background, and search answers "still
  indexing" for that store until it is ready (keyword results from what is indexed so far are
  fine).
- **The saved stores open side by side**, not one after another.
- **The account and sync states are asked at connect, beside the opens**, not after them.

### Said
- **Pimble Cloud in the status bar**: "Connecting..." with a small spinner from start until the
  first status answer; "Signed in as ..." after; "disconnected" only once a connection attempt
  has actually failed (and "sign-in expired" as now).
- **Each saved store has its row at once**, named from the saved list, with a spinner and
  "opening..." until it opens; then its tree. A store that fails to open says why on its row.
- **Indexing is said where search is**: while a store's index is opening or rebuilding, its row
  shows a quiet "indexing 484 notes" (with progress where rhypedb reports it), and a search
  covering it says "Personal is still being indexed; results may be incomplete."
- **Children loading**: a branch being expanded whose children have not arrived shows a spinner
  in place of its chevron (only after 150 ms, so fast loads do not flicker).
- The browser does the same from the same code (its stores open from the vault, and its account
  is the page's; same words, same spinners).

## Proving it

- A start-up timing test: an app server with three stores of 500 notes each, with and without a
  hand-over, answers every store's open and the account status within a bound (say 1 s), and no
  index is rebuilt in the hand-over case.
- On Joe's desktop: the log of a start after a hand-over shows every store opened within a second
  and no "Rebuilt search index".

## Target

On a machine like Joe's (busy: load averages of 40 to 190 while measuring), with three stores
of 35 to 650 notes, several branches left expanded and two panes holding notes: **ready within
about 2 s of the window appearing** (every saved store open, every row left expanded loaded,
every pane's note restored, the account answered), not counting the one-time model download
or a first compile of the plugins. The log says it: `startup: ... at +N ms` lines, the last
`startup: ready at +N ms`.

## Built (2026-11-20)

Branch: `master` working tree, not committed. All of "What changes" is built, desktop and
browser from the same `pimble-app` code, plus what the second look at the whole start found
(Joe, 2026-11-20: "startup in general seems incredibly slow ... with my 3, incredibly modest
stores").

### What the time went to

Joe's two starts that day after a hand-over: 19.0 s and 50.8 s before the last store
answered, and branches still arriving at +55 s. On copies of fixture stores (three stores of
about 630 notes, 32 rows left expanded, two panes) on the same busy machine, the build from
before this work (0.28.x of 2026-11-19), with every index already built and no hand-over,
against this one. The machine's load moved a lot while measuring, so each run says its own:

| Phase | Before, load 40-120 | Before, load 31 | After, load 34-39 (three runs) |
| --- | --- | --- | --- |
| window built | +0 s | +0 s | +0.00-0.01 s |
| connected | +2.5 s (the model warmed up before the server listened) | +0.7 s | +0.39-0.44 s |
| account answered | +9.1 s (behind the opens) | +1.8 s | +0.42-0.48 s |
| plugins loaded | +9.3 s (each compiled again, 1-2 s apiece) | +2.1 s | +0.39-0.44 s (compiled copies) |
| every saved store open | +9.0 s (one after another, each holding its index open) | +1.8 s | +0.59-0.68 s |
| rows left expanded loaded | +9.5 s (one level a round trip, in line) | +2.1 s | +0.72-0.88 s |
| **ready** | **about 9.5 s** | **about 2.1 s** | **0.72-0.88 s** |

A first run that compiles the plugins: ready at 3.0 s; one rebuilding all three indexes in the
background while screenshots were taken: 3.3 s (the indexes finished a few seconds later, said
on their rows). Joe's own starts lost most of their time to the rebuilds after a hand-over
(cause 1), which the test below shows gone.

The causes, each fixed:

1. **The hand-over left every index locked, and the taker deleted it.** `handOver` wrote the
   documents and stopped the port but kept the stores open in pimble-mcp's process, which goes
   on running. The app's open then failed "data directory is locked", and the fallback meant for
   a broken index caught that too: it `remove_dir_all`ed the directory the other process held
   and rebuilt it in place, 10 s for 500 notes, inside the open. No second index was left
   behind: the rebuild wrote into the same path, and the holder's own writes were fenced off by
   rhypedb's owner token in `LOCK` (its files were deleted under it). Now `handOver` closes every
   store and lets go of every index (`RpcHandler::release_all_stores`) before the port is free,
   and `SearchError::Locked` is never rebuilt over.
2. **Opens were in line, and each waited for its index.** The desktop's command loop ran one
   command at a time, `openStore` held the store manager's lock while reading every document,
   and then opened (or rebuilt) the index before answering.
3. **Everything else waited behind them**: the account, the sync states, the first children.
4. **Restored rows loaded one level per round trip**: a row left expanded asked for its children
   only when its parent's arrived, each request in line behind everything else, and each answer
   rebuilt the tree.
5. **The plugins were compiled on every start** (wasmtime, 1 to 4 s each on a busy machine,
   competing for the CPU with the opens).
6. **The embedding model warmed up before the server listened** (0.2 to 1.3 s).
7. **The saved stores' rows appeared only when the server answered**, not when the window did.

### What was built

- **Hand-over** (`handler.rs` `hand_over`, `release_all_stores`; `local.rs`): after writing,
  every pending index open is stopped, every index shut down and every store closed, then the
  port is freed. A taker waiting on a server that agreed to hand over looks again every 150 ms
  for up to 6 s before that counts as a failed attempt.
- **Indexes open in the background** (`start_index_open`, `open_index_in_background`,
  `open_waiting_for_lock`): `openStore`, `createStore`, adopted stores and new replicas answer
  once their documents are open. Edits made meanwhile queue on the index's channel
  (`PendingIndex`) and are applied once it is installed. A locked index is tried every 100 ms
  for 3 s, then said as `Held` and tried every second for as long as the store is open; it is
  never deleted. A rebuild walks the documents once, then indexes 32 notes at a time with the
  manager's lock held only to read each batch, on a blocking thread, saying how far it is every
  250 ms. A close (or a close and reopen) stops an open on its way.
- **`IndexState`** (`pimble-core`): `Ready`, `Opening`, `Rebuilding { done, total }`, `Held`,
  `Unavailable { reason }`, on `Store::index`, `GetStoreSyncResponse::index` and the new
  `StoreChangeKind::IndexStateChanged` (links never forward it). `SearchResponse::indexing`
  names the stores searched whose index is on its way; `PimbleClient::search_response` returns
  it; the MCP's `search` says the sentence above its hits.
- **Stores load side by side** (`open_store`): a plain store's documents are read outside the
  manager's lock (`LocalStore::open_in`, then `StoreManager::adopt_local_store`), one lock per
  store id so that two opens of one store never load it twice.
- **The model warms up when the first index wants it** (`server.rs`,
  `RpcHandler::with_semantic_warmup`): not before the server listens, and only an index open
  waits for its answer (loading it busies every core, which delayed the first connection by its
  whole length even when it ran beside the server).
- **The desktop's command loop** (`backend.rs`): `OpenStore`, `CloudStatus` and the new
  `LoadChildren` run as tasks of their own, answered when done; every other command keeps its
  turn. A command over 100 ms says so in the log, with how many waited behind it.
- **Restored rows load together** (`BackendCommand::LoadChildren`): when a store opens, its
  roots and every row of it this device left expanded are asked for at once (8 at a time inside
  the backend) and each answers as its own `ChildrenLoaded`; a row that appears expanded is not
  asked for again while its answer is on its way (`AppStore::children_pending`).
- **Compiled plugins are kept** (`pimble-plugin-host` `wasmtime_engine::compiled`): in
  `<cache dir>/pimble/plugins/<sha256 of the module>-<engine compatibility hash>.cwasm`,
  written atomically, compiled again when unreadable. `PIMBLE_PLUGIN_CACHE` moves it.
- **The timeline in the log** (`pimble-app/src/startup.rs`): `startup: window built`,
  `connected`, `account answered`, `plugins loaded`, `store <name> open`, `every saved store
  open`, `restored rows loaded`, `ready`, each `at +N ms`.

### What it says

- **Status bar**: the Pimble Cloud part says "connecting..." with a small spinner in place of
  its dot until the server answers `cloudStatus` (`AppStore::cloud_answer`), and "disconnected"
  only when asking failed (its tooltip says why); a lost connection to this computer's server
  is "connecting..." again, not "disconnected". The first part ("This computer", in the browser
  "Pimble Cloud") shows the same spinner while it connects.
- **Saved stores**: each has its row from the first frame (from the saved list, before any
  connection), named from its folder ("Personal"
  for `Personal.pimble`), with a spinner and "opening..."; a store that does not open says
  "could not open" in red with the server's reason as the tooltip (`StoreOpenFailed`). The rows
  keep the saved order.
- **Indexing**: a store row whose index is on its way says, quietly, "opening search",
  "indexing 484 notes", "indexing 120 of 484 notes" or "search waiting" (held by another
  program), with a sentence as tooltip; the search panel says "Personal is still being indexed;
  results may be incomplete." above its results while a store it covers is.
- **Children**: a branch whose children were asked for and have not come within 150 ms shows a
  spinner where its chevron goes.

### Decided here

- The opening row's name is the folder's name less `.pimble`: the saved list holds paths only,
  and reading each manifest first would be one more wait.
- A store that could not open is dropped from the saved list, as before (a store still opening
  is kept: `AppStore::store_paths_to_save`). Its row stays for the session with the reason.
- The children spinner sits on rinch's spacer: while it shows, the row's data has no
  placeholder child, so rinch draws the spacer where the chevron goes and Pimble positions a
  spinner over it (`.pimble-tree__children-loading`). No change to rinch; its `Loader` is used.
- "Ready" for the log is the last of: every saved store opened or failed, every branch asked
  for answered, every pane's note resolved.
- Only `OpenStore`, `CloudStatus` and `LoadChildren` leave the command line: the rest stay in
  order, since many of them depend on what came before.
- Search across stores still being indexed answers what is indexed so far, as the contract
  allowed; the app reads which stores are still indexing from their rows rather than from the
  answer, so the browser and the desktop say it alike.

### Proved

- `crates/pimble-server/tests/startup.rs`: three stores of 500 notes. First start with no
  index (each built in the background): every open and the account answered within 25 ms,
  each store answered before its index was ready, search named the stores still indexing, all
  three indexes built 1.1 s later. Then a newer client took the port over (connected 152 ms
  after asking); every open and the account answered within 19 ms, all three indexes ready
  21 ms after, the same index directories (the `LOCK` inodes unchanged: nothing rebuilt).
- `tests/search.rs` `a_locked_index_is_waited_for_and_never_rebuilt`: an index another holder
  keeps: the open answers at once, search names the store, the state becomes `Held`, the
  directory is untouched, and once let go the same index opens and finds what it held.
- `pimble-search` `a_held_index_is_locked_until_let_go`; `pimble-core`
  `index_states_say_what_they_are`; app: `a_saved_store_has_its_row_while_it_opens_and_says_why_if_it_cannot`,
  `a_branch_slow_to_load_shows_a_spinner_until_its_children_arrive`,
  `a_store_being_indexed_says_so_on_its_row_and_in_search`, `starting_up_is_said_as_it_happens`,
  and the status bar's words in `app/status.rs`.
- The real app on fixture copies (`PIMBLE_APP_ADDR=127.0.0.1:7483`, scratch config and data):
  the timeline above, and screenshots of the opening rows, a store that could not open, and the
  restored tree and panes.

### Known limits

- The hand-over was exercised in process (the test above), not with a real pimble-mcp, which
  needs an agent key and an MCP client to start its server.
- In the browser there are no opening rows (its stores arrive listed, already open from the
  vault) and no index states (an encrypted store is searched in the page).
- A rebuild's notes become searchable all at once when it is installed, not as they are
  indexed.
- Loading a store still reads every document (0.3 to 1.1 s for 630 notes on the busy machine);
  the stores now do it side by side.
- Several server test files start servers with `ServerConfig::default()`, whose state root
  is the real `<data dir>/pimble/stores`: those tests write indexes into the person's own data
  directory, and two servers of one process holding a store and its replica share one index
  directory. Before this work the replica's open deleted the other's index (the same bug as
  cause 1) and `tests/sync.rs` passed by it; it now gives each server its own directory, and
  the other files (`noop_updates`, `replica_removal`, `tree_repair`, `vault`,
  `large_documents`) still use the default and should too.
- After a plugin is replaced, its old compiled copy stays in the cache (named by the old
  module's hash) until the cache is cleared.
