# Pimble Restart Plan

Written 2026-09-12. This is the reference for getting Pimble from "never quite worked" to
a small, correct core that the roadmap can build on.

## 1. The vision

Pimble is an **offline-first personal information manager**. Everything you keep is a
**node** in a tree inside a **store**.

- **Stores** are self-contained `.pimble` directories you can put anywhere: local disk, a
  synced folder, or behind a remote Pimble server. A **workspace** is the set of stores you
  have open.
- **Nodes are CRDT documents.** Two devices or two people can edit the same node at the
  same time and the result merges without conflicts. Offline is the normal state; sync is
  opportunistic. The server relays and persists; it never owns the data.
- **Mounts.** Any subtree of any store can appear as a first-class child in any other
  store's tree. Local, remote, cached, or unavailable, the tree still renders.
- **Links and backlinks**, including deep links to a position inside a document.
- **Search**: keyword and semantic, across stores and across mounts.
- **Plugins** (WASM) define node types beyond document and folder.
- **A rich-text editor** with real-time collaboration, provided by rinch.
- **Import** from Scrivener projects.

Non-goals: it is not a web app and not cloud-first. There is no account; a store is a
directory.

## 2. Where the code actually is

| Crate | What it holds | State on 2026-09-12 |
| --- | --- | --- |
| `pimble-core` | Node, Store, Workspace, MountRef types | Complete |
| `pimble-crdt` | Automerge wrappers: `CrdtDocument`, `StoreDocument`, `DocumentContent` | Complete, but built on the wrong CRDT for content (see §4) |
| `pimble-store` | `LocalStore` (v2 layout: `store.automerge` + `nodes/{id}.automerge`), `StoreManager`, registry | Complete for Automerge |
| `pimble-rpc` / `pimble-server` / `pimble-client` | jsonrpsee JSON-RPC over WebSocket, subscriptions, sync methods, embedded server | Complete for Automerge |
| `pimble-search` | Search types only | Skeleton |
| `pimble-plugins` | `NodePlugin` trait, built-ins | Skeleton |
| `pimble-app` | rinch desktop app: tree, inline rename, drag-and-drop, menus, editor pane | Compiles against rinch main; content editing does not persist (see §3) |
| `pimble-cli` | server / create-store / list-stores | Excluded from the workspace |
| `pimble-import` | Scrivener + RTF import | Excluded from the workspace (used a deleted rinch API) |

About 10.7k lines of Rust. 23 files are uncommitted (+1938 / -547): the editor migration to
rinch's `Editor` component and collaboration API, the v2 store format, the sync RPCs and
subscriptions.

## 3. Why it has never worked right

1. **It was built on a moving editor.** Rinch's editor was rewritten four times while Pimble
   tracked it: ContentEditable, then `rinch-editor` (BlockData/EditorDocument), then
   `rinch-editor-core` + `rinch-editor-view` with collaboration on Automerge (rinch #69), then
   collaboration on **yrs** (rinch #197, "replace Automerge with yrs"). Each step invalidated
   Pimble's content path. The uncommitted tree assumes the Automerge version.
2. **Four ways to write content.** `updateNodeContent` (whole replace), `setNodeText`,
   `applyEdit` (incremental broadcast) and `syncNodeContent` (Automerge sync protocol) all
   exist. Content bytes are parsed by three readers that disagree on the format: the
   `DocumentContent` flat-text reader for tree labels, Automerge in the server, and the
   collaboration session in the editor.
3. **The concrete break today.** The editor's collab snapshot and deltas are yrs bytes. The
   server's `apply_edit` feeds them to Automerge `load_incremental` and `update_node_content`
   feeds the snapshot to `AutoCommit::load`. Both fail, so nothing typed in the editor is
   ever persisted. The only test in the repo (`collab_converges_through_a_server_relay`)
   asserts the Automerge assumption and fails. Seen live in the GUI too: selecting a document
   opens the editor and the status bar reports the server's "Invalid magic bytes" rejection.
4. **A port collision hid everything else.** The embedded server used `127.0.0.1:9876`, which
   is also the Blender MCP add-on's port. When Blender is running, Pimble's client connects to
   Blender, sends a WebSocket handshake, and waits forever, so the app sits at "Connecting..."
   and no store ever opens. Fixed today: the server is on `7462` and the probe of an existing
   server is bounded by a two-second timeout, so a foreign listener fails fast with a message.
5. **Path dependencies on the local rinch checkout.** What Pimble built against and what the
   ui-zoo ran were different code. Fixed today: the workspace points at GitHub `main`
   (locked at 743f8a0).
6. **No tests at the seams.** The GUI was the only proof, so regressions were found by
   typing into a window.

## 4. Decision 1: the CRDT is yrs, and it lives at the editor's boundary

Rinch has settled this. `rinch-editor-collab` is the only CRDT in the rinch workspace and it
is `yrs` 0.27. It ships a headless, `Send` `CollabSession` intended for servers:
`from_bytes(snapshot)`, `integrate_incremental(delta)`, `save_incremental()`,
`state_vector()`, `sync_diff(sv)`, `projected_doc(schema)`. One inbound entry point
(`apply_update`) covers broadcast deltas, reconciliation diffs and whole snapshots, and
reconciliation is stateless (send a state vector, get a diff back). Both crates are pure
Rust with no platform dependencies, so the server can link them.

Pimble should stop fighting this.

**Phase A (required to have a working app).** Per-node content becomes a yrs document.

- Store `nodes/{id}.yrs` (the full update encoding). Server keeps a `yrs::Doc` per open node.
- `applyEdit` becomes `apply_update` plus relay to other subscribers of that node.
- `syncNodeContent` becomes state-vector in, diff out. `ServerSyncManager` is deleted.
- `setNodeText` is deleted. `updateNodeContent` seeds or replaces a node with a snapshot.
- Tree labels and search text come from `CollabSession::projected_doc` and a text walk over
  the `rinch-editor-core` document, on the server. `DocumentContent` is deleted.
- The app's `editor.rs` already speaks this protocol; it only loses its legacy-text branch.

**Phase B (recommended, after A).** Move the store document (tree structure and node
metadata) from Automerge to yrs as well: a `Map` of nodes and an `Array` per parent for child
order. Then Automerge leaves the dependency tree entirely: one CRDT, one wire encoding, one
sync primitive for both document kinds.

**Rejected.** Bridging yrs deltas into Automerge on the server. That is the abstraction layer
that scrambled content before. Also rejected: pinning rinch to the last Automerge commit.
That freezes Pimble on an editor that no longer gets fixes.

**Constraint to design around.** Rinch's collaboration scope today is flat text blocks plus
marks (paragraph, heading, code block; bold, italic, link). Lists, block quotes and tables in
a collaborating document fail loudly by design. Scrivener import produces paragraphs, so it
is inside the scope. Anything richer waits on rinch.

## 5. Decision 2: rhypedb is the index and query engine, not the source of truth

**What rhypedb is** (github.com/joeleaver/rhypedb, last commit 2026-07-08, not on crates.io):
a strongly-typed object database with first-class relationships and native vectors. LSM tree
with write-ahead log, MVCC snapshot isolation, HNSW vector index with TurboQuant
compression, server-side text embedding through the `@vectorize` directive (fastembed/ONNX,
`all-MiniLM-L6-v2` built in), an optional reranker, and real-time subscriptions. It embeds
in-process: `Database::open(schema, dir)` plus `rhypedb_query::executor::execute`. No server
required.

**Where it fits.** As the per-store index and query layer.

- Nodes are objects; tree edges and links are relationships. Backlinks come for free from
  `@inverse`. Tags are a relationship to a `Tag` type.
- Semantic search is one schema line: `@vectorize(source: "text", model: "all-MiniLM-L6-v2")`.
  That is exactly the model the original architecture doc planned to wire by hand with
  candle and lance.
- Subscriptions can drive a live search results panel.
- It replaces the planned tantivy plus lance stack and gives the linking phase a real graph
  engine.

Proposed schema, one database per store under `store.pimble/index/rhypedb/`:

```
type Node {
  node_id: String @unique
  kind: String
  title: String
  text: String
  modified_at: i64
  parent: Node
  children: [Node] @inverse(Node.parent)
  links: [Node]
  backlinks: [Node] @inverse(Node.links)
  tags: [Tag] @on_delete(remove)
  embedding: Vector<384> @vectorize(source: "text", model: "all-MiniLM-L6-v2")
                         @index(hnsw, metric: cosine)
}

type Tag { name: String @unique }
```

The index is **derived and disposable**: delete the directory and it is rebuilt from the
CRDT documents on the next open. The server already emits node change notifications; the
indexer subscribes to them.

**Where it does not fit.** As the primary store. rhypedb is not a CRDT and has no
multi-master merge, so putting the tree in it would give up offline-first collaboration and
mount replication. The CRDT documents stay the truth.

**The gap: no keyword search.** rhypedb's comparison operators are `==`, `!=`, `<`, `<=`, `>`,
`>=`. There is no `contains`, no phrase match, no BM25. Text search in rhypedb means vector
similarity, optionally reranked. A PIM needs exact matching too ("the note with invoice
4471"). Options, in order of preference:

1. Add full-text indexing to rhypedb (a `@index(fulltext)` on `String` fields and a
   `.matches("...")` step). You own the engine; the LSM keyspace already has room for a
   postings prefix.
2. Keep tantivy in `pimble-search` for keyword search alongside rhypedb for graph and
   vectors.
3. Interim: substring scan over `title` and `text` from `scan_type`. Fine for a personal
   store of thousands of notes; not fine at scale.

**Costs to plan for.** The ONNX runtime either downloads at build time (`onnx-download`,
needs network) or links a system library (`onnx-static` / `onnx-dynamic`). The embedding
model downloads from Hugging Face on first use, which an offline-first app must pre-warm or
bundle. The rhypedb lockfile has 428 packages. Put all of it behind a `semantic` feature on
`pimble-search` so day-to-day builds stay lean.

## 6. The plan, in order

| Step | Deliverable | Proof |
| --- | --- | --- |
| 0 | rinch on GitHub main, app compiles, focus API updated, server port moved off Blender's, tree icons follow node type | Done today |
| 1 | Content on yrs (Phase A): store, server, RPC, client, app | Done 2026-09-12. Two windows edit live in both directions; force-killed both, relaunched, text intact. 40 tests pass, including server-boundary sync and debounced-flush tests. Interface: `docs/history/PHASE_A_CONTRACT.md`. |
| 2 | Commit the migration as coherent commits | Done 2026-09-12 on branch `restart/phase-a-yrs-content`. |
| 3 | Restore `pimble-cli` (headless testing) and `pimble-import` (port to `ContentDoc`) | Done 2026-09-12. CLI round-trips content through the real server; importer has an end-to-end test. |
| 4 | Store document on yrs (Phase B); Automerge removed | Done 2026-09-13. Interface: `docs/history/PHASE_B_CONTRACT.md`. |
| 5 | `pimble-search` on rhypedb: schema, indexer fed by change notifications, search RPC, search panel | Done 2026-09-13. Keyword search over `@fulltext` title and text (rhypedb #16), chunked embeddings behind the `semantic` feature, search box and results panel in the app, `search`/`rebuild-index` in the CLI. 82 tests pass. Follow-ups: rhypedb #17 (stemming, prefix terms), turning `semantic` on for release builds with a first-run model fetch. |
| 6 | Roadmap resumes: mounts (local first), remote sync, links UI, plugins | Per phase |

Legacy readers for the pre-Phase-A Automerge node content and the pre-Phase-B Automerge
store document, and the v1 migration path, were removed by decision on 2026-09-13: "no
legacy, no backwards compatibility" (`docs/CLEANUP_CONTRACT.md`). A store from before
Phase B does not open; the answer is to re-import from Scrivener.

## 7. Working agreements

- rinch comes from GitHub `main`, always. Never a path dependency on the local checkout.
  `Cargo.lock` pins the revision; move it with
  `cargo update rinch rinch-tabler-icons rinch-editor-core`.
- rinch fixes go upstream as pull requests; Pimble points at the branch until merged.
- Build and run `pimble-app` in `--release`.
- Every RPC method gets a test at the server boundary before it gets UI wiring.
- One way to write content. If a second appears, one of them is a bug.
