//! Semantic-search integration tests. Every test here opens a `SearchIndex`
//! with `semantic: true`, which starts a background embedding worker that
//! downloads `all-MiniLM-L6-v2` from Hugging Face on first use — hence
//! `#[ignore]` on all of them, and the whole file behind the `semantic`
//! feature. None of these tests waits on or inspects an actual embedding
//! vector (that would need the model); they exercise the `Chunk` bookkeeping
//! (`SearchIndex::chunk_hashes`) that `reconcile_chunks` does regardless of
//! whether the background worker ever gets network access.
//!
//! Run explicitly with:
//!   cargo test -p pimble-search --features semantic --test semantic -- --ignored

#![cfg(feature = "semantic")]

use pimble_core::{IndexUnit, NodeId, UnitKind};
use pimble_search::{IndexNode, SearchIndex};

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

#[test]
#[ignore = "downloads all-MiniLM-L6-v2 on first use"]
fn semantic_index_opens_with_a_vectorizer() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), true).unwrap();
    assert!(index.semantic_enabled());
}

#[test]
#[ignore = "downloads all-MiniLM-L6-v2 on first use"]
fn reupsert_with_one_changed_paragraph_rewrites_exactly_one_chunk() {
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
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), true).unwrap();
    let a = NodeId::new();
    let units = vec![IndexUnit::new(UnitKind::Prose, "b:0", "Some text.")];
    index.upsert(&node_with_units(a, "Doc", units)).unwrap();
    assert_eq!(index.chunk_hashes(a).unwrap().len(), 1);

    index.remove(a).unwrap();
    assert!(index.chunk_hashes(a).unwrap().is_empty());
}
