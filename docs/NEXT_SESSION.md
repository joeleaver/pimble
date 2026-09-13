# Next session: start here

Written 2026-09-13 at the end of the restart session. Read this, then `CLAUDE.md`, then
`docs/RESTART_PLAN.md`.

## Where master stands

- Branch `master`, head `44e4c1d` ("Remove every legacy path"). Working tree clean.
- Both CRDT documents are yrs (`ContentDoc` for node content, `StoreDocument` for the
  tree). No Automerge, no migrations, no backwards compatibility, by decision.
- rinch is tracked from GitHub `main`, pinned in `Cargo.lock` at `743f8a0`.
- Workspace compiles with zero warnings. 43 tests pass in release.
- Verified live: two windows edit the same node in both directions, force-kill both,
  relaunch, text intact. Tree labels follow content in the editing window too.
- Restart plan steps 0 to 5 are done (step 5: search on rhypedb, 2026-09-13).

## Verify in five minutes

```bash
cargo check --workspace --all-targets      # expect zero warnings
cargo test --workspace --release           # expect 43 passed
cargo run -p pimble-app --release          # opens the stores listed in ~/.config/pimble/state.json
```

Headless check without the GUI (server, store, node, content round trip):

```bash
cargo run -p pimble-cli -- server &        # 127.0.0.1:7462
cargo run -p pimble-cli -- create-store /tmp/t.pimble "T"
cargo run -p pimble-cli -- create-node <store-id> <root-id> document "Note"
cargo run -p pimble-cli -- set-node-text <store-id> <node-id> "hello"
cargo run -p pimble-cli -- show-node <store-id> <node-id>
```

The app has rinch's `debug` feature on; the rinch MCP tools (`list_apps`, `connect`,
`screenshot`, `click`, `type_text`) drive a running instance. Blender's MCP add-on listens
on 9876; Pimble deliberately uses 7462.

## Step 5 decision: full-text goes into rhypedb first (decided 2026-09-13)

rhypedb (github.com/joeleaver/rhypedb, Joe's own engine) is the chosen index and query
layer: nodes as objects, tree edges and links as relationships (backlinks via `@inverse`),
tags, and `@vectorize` embeddings for semantic search. It is not the source of truth; the
yrs documents are. The index lives under `store.pimble/index/rhypedb/` and is rebuildable.

rhypedb had no keyword search, and a PIM needs exact matching. Joe decided to add it to
rhypedb itself rather than paper over it in Pimble. The design is filed as
**https://github.com/joeleaver/rhypedb/issues/16**: a `@fulltext` directive on `String`
fields, a `.matches(.field, "terms", k: N)` step ranked by BM25 (with `+required` terms
and `"phrases"`), a cheap `.filter(.field.contains("x"))` predicate, an inverted index
kept in the LSM keyspace inside the object's own transaction, and backfill on schema
reload. rhypedb's default branch is `master`.

rhypedb #16 landed on `master` on 2026-09-13 (commit 4f1bbc4). Pimble's workspace now
depends on `rhypedb-engine`, `rhypedb-schema` and `rhypedb-query` from that branch with
default features off; `pimble-search`'s `semantic` feature turns on the ONNX stack.
`docs/STEP5_CONTRACT.md` (search + graph index, chunked embeddings, extensible index
units for tables and structured data) is the active contract.

The other consideration stands: rhypedb's embedding stack (fastembed/ONNX) either
downloads the ONNX runtime at build time (`onnx-download`) or links a system library
(`onnx-static` / `onnx-dynamic`), and the model downloads from Hugging Face on first use.
Pimble keeps that behind a `semantic` feature.

## How the last session worked (keep doing this)

- The PM (Claude) writes a contract file first (`docs/history/PHASE_*_CONTRACT.md` are the
  examples), splits work by crate so agents never edit the same files, reserves the root
  `Cargo.toml` for itself, and reviews every report against the code before committing.
- Three Sonnet agents in parallel is the practical ceiling on this machine: a fourth
  concurrent release build pushed memory low enough that background waits were killed.
- A plain `cargo check` going green is not proof that another agent's API change has
  landed; agents should grep for the new signature.
- Two real bugs were only found in review, not by the agents' own tests: incremental
  edits were never flushed to disk, and a yrs `get_or_insert_map` inside an open
  transaction deadlocked every store-document call. Review the diff, then run the GUI.
- Never touch `/home/joe/dev/rinch`. Read rinch source from
  `~/.cargo/git/checkouts/rinch-*/<rev>/` or a scratch clone.

## Smaller follow-ups, in rough priority

0. Turn `semantic` on for real: choose the ONNX link mode for release builds
   (`onnx-download` for a self-contained binary), install a runtime here for a first live
   test, add a first-run model fetch into Pimble's data directory with visible progress,
   and ask rhypedb to fail soft (not panic) when the model cannot load. rhypedb #17 adds
   stemming and prefix terms for keyword search.

1. `ContentDoc::text()` re-projects the whole document on every call; tree labels call it
   per node. Cache the projected text per node in `LocalStore` and invalidate on update.
   The rhypedb indexer will want the same cache.
2. The importer flattens RTF formatting (bold, italic, links) to plain paragraphs. Add a
   `ContentDoc` constructor that takes marked-up runs once the editor's collab scope
   accepts them (rinch A22 scope today: paragraphs, headings, code blocks, bold, italic,
   link).
3. `pimble-client` still exposes `sync_node_content`, `sync_store_document`,
   `apply_store_update` and `get_nodes`; the app calls none of them. They exist for the
   reconnect-reconciliation path (state vector in, diff out). Wire them when reconnect
   handling is built, or delete them then.
4. Concurrent moves of the same node in `StoreDocument` can leave a duplicate child
   entry; `validate_tree` detects it, nothing repairs it yet.
5. The rinch `collaboration` scope rejects lists, block quotes and tables in a
   collaborating document (fails loud). The editor toolbar still shows list buttons.
6. The old `nodes/*.automerge` and `store.automerge` files still sit in the two converted
   stores. They are inert; delete them when convenient.

## Then the roadmap resumes

Mounts (local first), remote sync over the same state-vector/diff primitive, links UI and
backlinks (rhypedb relationships), plugins. See `docs/RESTART_PLAN.md` §6 and
`docs/ARCHITECTURE.md`.
