# Step 5 contract: search and graph index on rhypedb (draft, dispatch-ready)

Status: drafted 2026-09-13. Decision made the same day: Option 1, full-text search is
built into rhypedb first (https://github.com/joeleaver/rhypedb/issues/16). Dispatch this
contract once that lands, pointing the rhypedb git dependency at the branch or tag that
has it. The Option 2 paragraph under "Keyword search" is kept only as the fallback if the
rhypedb work slips.

## Ownership

| Agent | Scope |
| --- | --- |
| A | `pimble-search`: the index crate on rhypedb |
| B | `pimble-server` + `pimble-rpc` + `pimble-client`: indexer wiring, `search` and `rebuildIndex` RPCs, change feed |
| C | `pimble-app`: search box, results panel, navigate to result |

PM: root `Cargo.toml` (add `rhypedb-engine`, `rhypedb-schema`, `rhypedb-query` as git deps
from `https://github.com/joeleaver/rhypedb.git`, `default-features = false`; define a
workspace-level `semantic` feature story), remove `tantivy` from the workspace deps.

## Design

- One rhypedb `Database` per open store at `store.pimble/index/rhypedb/`, opened by the
  server when the store opens, closed with it. Derived and disposable: `rebuildIndex`
  deletes the directory and re-indexes every node from the store's documents.
- Schema (`pimble-search/schema.rhype`, embedded with `include_str!`):

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

  The `embedding` line is present only when the `semantic` feature is on; A ships two
  schema files or one with a marker the crate strips.

- Feed: the server already calls `notify_store_change` and `notify_node_content_change`
  for every mutation. B adds an in-process `IndexFeed` hook next to those calls (not over
  the WebSocket) that enqueues `(store_id, node_id, IndexEvent::{Upsert, Remove, Moved})`
  to a tokio task owning the store's `SearchIndex`. Content upserts are debounced per node
  (1s) because `applyEdit` fires per keystroke; the indexer reads `ContentDoc::text()` at
  flush time, never per delta.
- Query: `search(SearchRequest { query, stores, semantic, limit })` returns
  `SearchResultItem { node_id, store_id, score, title, snippet }`. `snippet` is the first
  match window in `text` (keyword) or the first 160 chars (semantic).
- Node text comes from `ContentDoc::text()`; A adds nothing to the CRDT crates. (Follow-up
  1 in `NEXT_SESSION.md`, the per-node text cache, can land in `pimble-store` first if the
  indexer is too slow on the 567-node store.)

## A. `pimble-search`

```rust
pub struct SearchIndex { /* Arc<rhypedb_engine::database::Database>, optional Vectorizer */ }
impl SearchIndex {
    pub fn open(dir: &Path, semantic: bool) -> Result<Self>;   // parse_schema + Database::open
    pub fn upsert(&self, node: &IndexNode) -> Result<()>;        // create or update by node_id; sets parent/tags/links relationships
    pub fn remove(&self, node_id: NodeId) -> Result<()>;
    pub fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>>;
    pub fn backlinks(&self, node_id: NodeId) -> Result<Vec<NodeId>>;
    pub fn clear(&self) -> Result<()>;
}
pub struct IndexNode { pub node_id: NodeId, pub kind: String, pub title: String, pub text: String,
    pub modified_at: i64, pub parent: Option<NodeId>, pub tags: Vec<String>, pub links: Vec<NodeId> }
pub struct SearchQuery { pub text: String, pub semantic: bool, pub limit: usize }
pub struct SearchHit { pub node_id: NodeId, pub score: f32, pub title: String, pub snippet: String }
```

Use `rhypedb_query::executor::{ExecContext, execute}` with `rhypedb_query::parser` for
queries where the query language is enough; fall back to `Database` calls (`get`,
`scan_type`, `create`, relationships) where it is not. Feature `semantic` enables
`rhypedb-engine/onnx-dynamic` (ORT_DYLIB_PATH at runtime) so day-to-day builds stay
offline; document `onnx-download` as the alternative.

Keyword search:
- Option 1 (chosen): `title: String @fulltext` and `text: String @fulltext` in the schema;
  keyword search is `Node.matches(.text, "<query>", k: limit)` merged with a title query
  (`Node.matches(.title, "<query>", k: limit)`, title hits weighted 2x), ranked by the
  engine's BM25 score. See rhypedb issue #16 for the exact step syntax once merged.
- Option 2: `scan_type("Node")` filtered case-insensitively on `title` then `text`,
  scored: title match 2.0, text match 1.0, ties by `modified_at` desc. Fine for
  thousands of nodes; a TODO points at Option 1.

Semantic: `Node.similar(.embedding, "<query>", k: limit)`; results carry the engine's
score. When `semantic` is off, `SearchQuery.semantic = true` returns an error the UI can
show ("semantic search is not built in").

Tests: tempdir index; upsert three nodes with a parent chain and one link; keyword hit on
title outranks hit on text; `backlinks` returns the linker; `remove` drops the node and
its backlink; `clear` + re-upsert works. Semantic tests behind the feature and `#[ignore]`
by default (they download a model).

## B. Server, RPC, client

- `RpcHandler` owns `HashMap<StoreId, IndexHandle>`; open/close with the store. On open,
  if `index/rhypedb/` is missing or its `schema_hash` file differs from the embedded
  schema, rebuild.
- `IndexFeed` hook + debounced upsert task as in Design. `Moved` re-upserts parent.
- `search` RPC implemented over the stores requested (all open stores when empty);
  results merged by score.
- New `#[method(name = "rebuildIndex")] rebuild_index(store_id) -> { indexed: usize }`.
- `pimble-client`: `search(query, stores, semantic, limit)`, `rebuild_index(store_id)`.
- `pimble-cli`: `search <query>` and `rebuild-index <store-id>` commands (small; B or C).
- Tests: `tests/search.rs`: create store, create two documents, set content, wait past
  the debounce, `search` finds the right one; delete it, `search` no longer does.

## C. App

- Search box in the toolbar (`Ctrl+K` focuses it); results panel replaces the tree while
  the query is non-empty (Esc clears); a result row shows title, store name, snippet;
  Enter or click selects the node in the tree and opens it. Semantic toggle only when the
  build has the feature (a `cfg!(feature = "semantic")` passthrough from the server's
  capabilities, or hide it until the server reports support in `Connected`).
- No new state beyond `search_query: Signal<String>` and `search_results: Signal<Vec<..>>`.
- `cargo check -p pimble-app` zero warnings; a manual GUI check by the PM.

## Done means

Import the Scrivener project, type a word that appears in one document, see it in the
results within a second, click it, land in that document. With `semantic` on, a
paraphrase finds the same document.
