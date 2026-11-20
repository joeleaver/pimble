# Starting up: fast, and saying what it is doing (draft, 2026-11-20)

Joe, 2026-11-20: "When I load the app on my desktop, a lot of time goes by where Pimble Cloud
shows disconnect (needs a spinner or something) and the explorer loads in in chunks, slowly, but
there's no feedback."

Status: **draft**, queued after hashtags.

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
