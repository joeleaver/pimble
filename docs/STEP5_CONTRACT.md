# Step 5 contract: search and graph index on rhypedb (draft, dispatch-ready)

Status: rhypedb issue #16 (`@fulltext`, `.matches` with BM25 scores and phrases,
`.contains`) landed on rhypedb `master` on 2026-09-13. This contract is dispatch-ready.
Chunking (below) was added the same day: rhypedb embeds a `@vectorize` source field whole,
and `all-MiniLM-L6-v2` truncates long inputs, so semantic search runs over chunks while
keyword search runs over whole nodes.

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
  title: String @fulltext
  text: String @fulltext
  modified_at: i64
  parent: Node
  children: [Node] @inverse(Node.parent)
  links: [Node]
  backlinks: [Node] @inverse(Node.links)
  tags: [Tag] @on_delete(remove)
  chunks: [Chunk] @inverse(Chunk.node)
}

type Chunk {
  chunk_id: String @unique          // "{node_id}:{ordinal}"
  node: Node @on_delete(cascade)
  ordinal: u32
  hash: String                      // content hash of `text`; unchanged chunks are not re-embedded
  text: String
  embedding: Vector<384> @vectorize(source: "text", model: "all-MiniLM-L6-v2")
                         @index(hnsw, metric: cosine)
}

type Tag { name: String @unique }
```

  Without the `semantic` feature the `Chunk` type is omitted from the schema and no chunks
  are written; A ships the schema as one file with a `// semantic:` marker the crate strips.

- Chunking (pure function in `pimble-search`, unit-tested, no I/O):
  - Input is the node's block list, not its flat text. A adds
    `ContentDoc::blocks(&self) -> Vec<Block { kind: BlockKind, text: String }>` to
    `pimble-crdt` (`BlockKind::{Paragraph, Heading(u8), CodeBlock}`), reusing the existing
    projection walk; `text()` becomes `blocks().join("\n")`.
  - Walk the blocks in order, accumulating consecutive blocks into a chunk until adding
    the next block would exceed 200 words (about 256 word-pieces, the model's training
    window). A single block longer than 200 words is split at sentence boundaries, then at
    word boundaries, into pieces of at most 200 words with a 30-word overlap between
    consecutive pieces.
  - Each chunk's embedding source is `"{title}\n{nearest preceding heading, if any}\n{chunk text}"`,
    so a chunk carries its section context; `Chunk.text` stores the chunk text only and the
    context prefix is passed as the vectorize source by writing it into a separate
    `source: String` field if rhypedb requires the source to be a stored field (A checks;
    if so, `embedding` vectorizes `source` and `text` stays the display text).
  - Ordinals are stable positions in the block order. On re-index, compute the new chunk
    list, `hash` each chunk, and per ordinal: unchanged hash means no write; changed hash
    means update `text` (rhypedb re-embeds on update); missing ordinals are deleted. This
    keeps a keystroke-debounced flush from re-embedding an entire document.
  - Empty nodes (folders, blank documents) produce zero chunks.

- Ranking:
  - Keyword: `Node.matches(.title, q, k)` and `Node.matches(.text, q, k)`, merged by node
    with title hits weighted 2x. Snippet: Pimble finds the first query term in the node's
    text and returns a window of about 160 characters around it, computed from
    `ContentDoc::text()` at query time (rhypedb returns scores, not snippets).
  - Semantic: `Chunk.similar(.embedding, q, k: 3 * limit)`, grouped by node keeping each
    node's best chunk; the snippet is that chunk's text.
  - `semantic: true` in `SearchRequest` means hybrid: reciprocal rank fusion (k = 60) of
    the keyword node list and the semantic node list, so an exact term and a paraphrase
    both surface. `semantic: false` is keyword only.

- Feed: the server already calls `notify_store_change` and `notify_node_content_change`
  for every mutation. B adds an in-process `IndexFeed` hook next to those calls (not over
  the WebSocket) that enqueues `(store_id, node_id, IndexEvent::{Upsert, Remove, Moved})`
  to a tokio task owning the store's `SearchIndex`. Content upserts are debounced per node
  (2s) because `applyEdit` fires per keystroke; the indexer reads `ContentDoc::blocks()` at
  flush time, never per delta, and the chunk hashes keep unchanged chunks from re-embedding.
- Query: `search(SearchRequest { query, stores, semantic, limit })` returns
  `SearchResultItem { node_id, store_id, score, title, snippet }`. `snippet` is the first
  match window in `text` (keyword) or the first 160 chars (semantic).
- Node text and blocks come from `ContentDoc`; the only CRDT-crate change is the new
  `blocks()` projection. (Follow-up 1 in `NEXT_SESSION.md`, the per-node projection cache in
  `pimble-store`, is worth doing in the same step: the indexer and the tree labels both
  read it.)

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

Keyword and semantic search are as specified under "Ranking" in the Design section.
`.matches` on a field whose index is still backfilling returns an error carrying progress;
`SearchIndex::search` maps that to `SearchError::IndexBuilding { done, total }` so the UI
can say so. When the `semantic` feature is off, a hybrid request degrades to keyword only
and the response says `semantic: false`.

`chunk_blocks(title, blocks) -> Vec<ChunkSpec { ordinal, text, source, hash }>` is public and
tested on its own: a 5-paragraph note under 200 words yields one chunk; a 900-word
paragraph yields five overlapping pieces; a heading followed by three paragraphs gives
chunks whose `source` starts with the title and that heading; identical input yields
identical hashes.

Tests: tempdir index; upsert three nodes with a parent chain and one link; keyword hit on
title outranks hit on text; a phrase query matches only the node with the phrase; the
snippet contains the query term; `backlinks` returns the linker; `remove` drops the node,
its chunks and its backlink; re-upsert with one changed paragraph rewrites exactly one
chunk; `clear` + re-upsert works. Semantic tests behind the feature and `#[ignore]` by
default (they download a model).

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
results within a second with a snippet around the word, click it, land in that document.
With `semantic` on, a paraphrase of a paragraph deep inside a long document finds that
document and shows that paragraph as the snippet; editing one paragraph re-embeds one chunk.
