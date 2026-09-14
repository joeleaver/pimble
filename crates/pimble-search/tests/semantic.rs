//! Semantic-search integration tests. Every test here opens a `SearchIndex`
//! with `semantic: true`, which starts a background embedding worker that
//! downloads `all-MiniLM-L6-v2` from Hugging Face on first use — hence
//! `#[ignore]` on all of them, and the whole file behind the `semantic`
//! feature. Most of these tests never wait on or inspect an actual embedding
//! vector (that would need the model); they exercise the `Chunk` bookkeeping
//! (`SearchIndex::chunk_hashes`) that `reconcile_chunks` does regardless of
//! whether the background worker ever gets network access.
//!
//! `semantic_search_finds_the_topically_matching_node` below is the exception:
//! it actually drives the embedder end to end, so it additionally needs an
//! ONNX link feature (`onnx-download` or `onnx-dynamic`) — `semantic` alone
//! compiles the code paths but does not link an ONNX runtime, so a bare
//! `--features semantic` run panics inside `FastEmbedder::new`.
//!
//! Run explicitly with:
//!   cargo test -p pimble-search --features onnx-download --test semantic -- --ignored --nocapture
//!
//! No environment variables needed (rhypedb #18's cross-encoder is
//! `VectorizerConfig::cross_encoder`, explicit opt-in and off by default —
//! `SearchIndex::open` sets it `Off` explicitly — so there is no more
//! implicit reranker download or score-convention mismatch to work around
//! here; see `semantic_search_finds_the_topically_matching_node`'s doc
//! comment for the history).

#![cfg(feature = "semantic")]

use std::thread;
use std::time::{Duration, Instant};

use pimble_core::{IndexUnit, NodeId, UnitKind};
use pimble_search::{IndexNode, SearchIndex, SearchQuery};

/// `n` distinct filler words prefixed with `tag`, so a paragraph this long
/// sits right at (or over) the chunker's 200-word budget on its own and
/// won't silently merge with a neighboring paragraph in these tests.
fn long_paragraph(tag: &str, n: usize) -> String {
    (1..=n).map(|i| format!("{tag}{i}")).collect::<Vec<_>>().join(" ")
}

fn node_with_units(id: NodeId, title: &str, units: Vec<IndexUnit>) -> IndexNode {
    let text = units
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    IndexNode {
        node_id: id,
        kind: "document".to_string(),
        title: title.to_string(),
        text,
        modified_at: 0,
        parent: None,
        tags: Vec::new(),
        links: Vec::new(),
        units,
    }
}

/// Ensure the embedding model is downloaded, cached at a stable (non-tempdir)
/// location, and loaded exactly once for this whole test binary.
///
/// Every `#[test]` below opens a `SearchIndex` with `semantic: true`, whose
/// `upsert` enqueues a real `VectorizeJob` and starts a background worker
/// that lazily constructs the first `FastEmbedder` for "all-MiniLM-L6-v2" —
/// and Rust's test harness runs different `#[test]`s in parallel *threads*
/// within one process by default. Without a single shared, eager warm-up,
/// each test's worker races every other one to be the first to load the
/// model. On a cold cache that is exactly the bug reported against rhypedb:
/// the loser's hf-hub download attempt fails ("Failed to retrieve
/// model.onnx"), its worker thread panics, and joining that already-panicked
/// thread during `Vectorizer::stop_worker` at process teardown aborts the
/// *entire test binary* (SIGABRT) — even though by then every `#[test]`'s
/// own assertions had already passed. Reproduced directly by deleting
/// `target/.fastembed_cache` and running this file's tests in parallel (the
/// default) rather than with `--test-threads=1`; calling this function first
/// in every test, so the download+load happens once before any test's
/// `SearchIndex::open` can race on it, fixes it.
fn ensure_model_warmed_up() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // A stable, non-tempdir cache location so a repeat run of this test
        // binary (or of pimble-server, which points at the user's data dir
        // instead) reuses the download instead of re-fetching ~94MB every
        // time. `target/` is already gitignored workspace-wide, so this
        // needs no cleanup and is never committed.
        let cache_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(".fastembed_cache");
        pimble_search::set_model_cache_dir(&cache_dir).unwrap();

        let elapsed = pimble_search::warm_embedding_model("all-MiniLM-L6-v2").unwrap();
        eprintln!(
            "warm_embedding_model('all-MiniLM-L6-v2') ready in {:.1}s (model load + download)",
            elapsed.as_secs_f64()
        );
    });
}

#[test]
#[ignore = "downloads all-MiniLM-L6-v2 on first use"]
fn semantic_index_opens_with_a_vectorizer() {
    ensure_model_warmed_up();
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), true).unwrap();
    assert!(index.semantic_enabled());
}

#[test]
#[ignore = "downloads all-MiniLM-L6-v2 on first use"]
fn reupsert_with_one_changed_paragraph_rewrites_exactly_one_chunk() {
    ensure_model_warmed_up();
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), true).unwrap();

    let a = NodeId::new();
    // Each paragraph alone is ~180 words — too big to share a chunk with a
    // neighbor under the 200-word budget — so three paragraphs reliably
    // become three chunks.
    let units = vec![
        IndexUnit::new(UnitKind::Prose, "b:0", &long_paragraph("a", 180)),
        IndexUnit::new(UnitKind::Prose, "b:1", &long_paragraph("b", 180)),
        IndexUnit::new(UnitKind::Prose, "b:2", &long_paragraph("c", 180)),
    ];
    index.upsert(&node_with_units(a, "Doc", units.clone())).unwrap();
    let before = index.chunk_hashes(a).unwrap();
    assert_eq!(before.len(), 3);

    let mut changed = units;
    changed[1] = IndexUnit::new(UnitKind::Prose, "b:1", &long_paragraph("EDITED", 180));
    index.upsert(&node_with_units(a, "Doc", changed)).unwrap();
    let after = index.chunk_hashes(a).unwrap();

    assert_eq!(after.len(), 3);
    assert_eq!(before[0], after[0], "chunk 0 should be untouched");
    assert_eq!(before[2], after[2], "chunk 2 should be untouched");
    assert_ne!(before[1].1, after[1].1, "chunk 1's hash should change");
}

#[test]
#[ignore = "downloads all-MiniLM-L6-v2 on first use"]
fn dropping_a_unit_deletes_its_chunk() {
    ensure_model_warmed_up();
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), true).unwrap();
    let a = NodeId::new();

    let units = vec![
        IndexUnit::new(UnitKind::Prose, "b:0", &long_paragraph("keep", 180)),
        IndexUnit::new(UnitKind::Prose, "b:1", &long_paragraph("drop", 180)),
    ];
    index.upsert(&node_with_units(a, "Doc", units)).unwrap();
    assert_eq!(index.chunk_hashes(a).unwrap().len(), 2);

    let units = vec![IndexUnit::new(UnitKind::Prose, "b:0", &long_paragraph("keep", 180))];
    index.upsert(&node_with_units(a, "Doc", units)).unwrap();
    assert_eq!(index.chunk_hashes(a).unwrap().len(), 1);
}

#[test]
#[ignore = "downloads all-MiniLM-L6-v2 on first use"]
fn removing_a_node_drops_its_chunks() {
    ensure_model_warmed_up();
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), true).unwrap();
    let a = NodeId::new();
    let units = vec![IndexUnit::new(UnitKind::Prose, "b:0", "Some text about a topic.")];
    index.upsert(&node_with_units(a, "Doc", units)).unwrap();
    assert_eq!(index.chunk_hashes(a).unwrap().len(), 1);

    index.remove(a).unwrap();
    assert!(index.chunk_hashes(a).unwrap().is_empty());
}

/// The real end-to-end path: three topically distinct nodes, real
/// `all-MiniLM-L6-v2` embeddings (downloaded on first use), a hybrid query
/// (`with_semantic(true)` always blends keyword + semantic via
/// `SearchIndex::search`'s reciprocal rank fusion — there is no pure-semantic
/// mode in the public API) that shares no *topical* vocabulary with its
/// target node, and a second hybrid query that leans on a keyword only one
/// node contains.
///
/// `SearchIndex::open` already starts a background worker
/// (`Vectorizer::start_worker(1)`) that races this thread to claim embedding
/// jobs, so a single `process_pending_embeddings` returning 0 does not prove
/// embedding is finished — it may just mean the worker claimed the batch
/// first and is still loading the model. So the readiness loop below treats
/// "the cameras query returns all three nodes" (there are exactly three
/// chunks, one per node) as the actual done signal, while still calling
/// `process_pending_embeddings` on every iteration so this thread pulls its
/// weight if the worker hasn't started yet.
///
/// Two non-obvious ways this test can fail even when semantic search itself
/// is fine, both hit while writing it:
/// - Any node's text containing a word the query also contains (including a
///   bare function word like "of" — rhypedb's `english` analyzer does no
///   stopword removal, see rhypedb #17) gives that node a keyword-side RRF
///   hit that outweighs a real semantic match. Keep distractor text free of
///   every query word, not just the topical ones.
/// - (Historical, fixed by rhypedb #18 — kept here so nobody rediscovers it.)
///   Before #18, `Vectorizer::search_text`'s cross-encoder reranker was
///   active by default (only an undocumented `RHYPEDB_DISABLE_RERANK=1` env
///   var turned it off, unrelated to the `rerank: bool` parameter this
///   crate actually passed). When it activated, `search_text` returned a
///   cross-encoder relevance score (higher = better) in the slot this
///   crate's old score-conversion code treated as a *distance* (lower =
///   better), inverting the ranking. rhypedb #18 replaced that boolean with
///   `VectorizerConfig::cross_encoder` (explicit `On`/`Off`, default `Off`,
///   never an implicit download) and `search_text`'s return type with
///   `SimilarHit { distance, rerank_score: Option<f32> }` in already-correct
///   rank order, so `SearchIndex::open` can (and does) ask for `Off`
///   explicitly and `semantic_search` no longer re-sorts anything itself.
#[test]
#[ignore = "downloads all-MiniLM-L6-v2 on first use"]
fn semantic_search_finds_the_topically_matching_node() {
    ensure_model_warmed_up();
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), true).unwrap();
    assert!(index.semantic_enabled());

    let cameras = NodeId::new();
    let pasta = NodeId::new();
    let taxes = NodeId::new();

    let cameras_text = "We installed a new home security system with several outdoor \
        cameras covering the driveway and the back yard, plus motion-activated alarms on \
        every door and window. Footage from the cameras is stored in the cloud so we can \
        review anything a burglar or intruder might trigger while we are away, and the \
        doorbell camera sends a notification the moment someone approaches the front porch \
        at night.";
    // Deliberately free of "of"/"my"/"house"/"video"/"surveillance": rhypedb's
    // "english" fulltext analyzer does no stopword removal (see CLAUDE.md's
    // note on rhypedb #17), so even a bare function word shared with the
    // query would give this node a keyword-side hit. Reciprocal rank fusion
    // then can't tell that hit apart from a real topical match, and it
    // drowns out the (correct) semantic ranking this test is checking.
    let pasta_text = "Tonight's dinner is a simple spaghetti with garlic, olive oil, and a \
        generous handful grated parmesan cheese. Salt the boiling water well before adding \
        the pasta, cook it until just al dente, then toss it with fresh basil and a splash \
        from the starchy pasta water to bring the sauce together.";
    let taxes_text = "It's almost April, so it's time to gather W-2 forms and receipts and \
        start filing the annual income tax return. The accountant wants last year's \
        deductions documented before the IRS deadline, and we should double check whether \
        the home office deduction still applies this year.";

    index
        .upsert(&node_with_units(
            cameras,
            "Home Security",
            vec![IndexUnit::new(UnitKind::Prose, "b:0", cameras_text)],
        ))
        .unwrap();
    index
        .upsert(&node_with_units(
            pasta,
            "Pasta Night",
            vec![IndexUnit::new(UnitKind::Prose, "b:0", pasta_text)],
        ))
        .unwrap();
    index
        .upsert(&node_with_units(
            taxes,
            "Tax Filing",
            vec![IndexUnit::new(UnitKind::Prose, "b:0", taxes_text)],
        ))
        .unwrap();

    let cameras_query = SearchQuery::new("surveillance video of my house")
        .with_semantic(true)
        .with_limit(3);

    let start = Instant::now();
    let timeout = Duration::from_secs(300);
    let indexing_elapsed;
    loop {
        index.process_pending_embeddings().unwrap();
        let hits = index.search(&cameras_query).unwrap();
        if hits.len() >= 3 {
            indexing_elapsed = start.elapsed();
            break;
        }
        assert!(
            start.elapsed() < timeout,
            "embeddings for all three nodes did not become ready within 5 minutes \
             (only {} of 3 nodes indexed so far)",
            hits.len()
        );
        thread::sleep(Duration::from_millis(500));
    }
    // The model was already warmed up above, so this is chunking + embedding
    // + HNSW-indexing three short chunks, not a model load.
    eprintln!(
        "all three chunks embedded and searchable {:.2}s after upsert",
        indexing_elapsed.as_secs_f64()
    );

    let hits = index.search(&cameras_query).unwrap();
    assert_eq!(hits.len(), 3, "expected one hit per node: {hits:?}");
    assert_eq!(
        hits[0].node_id, cameras,
        "top hit for a surveillance query should be the cameras node: {hits:?}"
    );
    assert_ne!(
        hits[0].kind, "node",
        "a semantic hit should resolve to a chunk kind (prose/heading/code/table/field/other), not a whole-node match"
    );
    assert!(!hits[0].snippet.is_empty(), "expected a non-empty snippet on the top hit");

    // Hybrid query: "parmesan" appears only in the pasta node's text, so
    // keyword scoring should carry it to the top even blended (via
    // reciprocal rank fusion) with semantic scoring.
    let hybrid_hits = index
        .search(&SearchQuery::new("parmesan").with_semantic(true).with_limit(3))
        .unwrap();
    assert!(!hybrid_hits.is_empty(), "expected at least one hybrid hit");
    assert_eq!(
        hybrid_hits[0].node_id, pasta,
        "top hybrid hit for 'parmesan' should be the pasta node: {hybrid_hits:?}"
    );

    let _ = taxes; // the third node exists only as a distractor for both queries above.
}
