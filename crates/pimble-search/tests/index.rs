//! Integration tests for `SearchIndex` against a real (keyword-only) rhypedb
//! database in a temp directory. Semantic-search tests live in
//! `tests/semantic.rs`, gated behind the `semantic` feature and `#[ignore]`.

use pimble_core::{IndexUnit, NodeId, UnitKind};
use pimble_search::{IndexNode, SearchIndex, SearchQuery};

fn node(id: NodeId, title: &str, text: &str, parent: Option<NodeId>, links: Vec<NodeId>) -> IndexNode {
    IndexNode {
        node_id: id,
        kind: "document".to_string(),
        title: title.to_string(),
        text: text.to_string(),
        modified_at: 0,
        parent,
        tags: Vec::new(),
        links,
        units: vec![IndexUnit::new(UnitKind::Prose, "b:0", text)],
    }
}

#[test]
fn upsert_parent_chain_and_one_link() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();

    let root = NodeId::new();
    let child = NodeId::new();
    let grandchild = NodeId::new();

    index.upsert(&node(root, "Root", "root text", None, vec![])).unwrap();
    index
        .upsert(&node(child, "Child", "child text", Some(root), vec![]))
        .unwrap();
    index
        .upsert(&node(
            grandchild,
            "Grandchild",
            "grandchild text linking to root",
            Some(child),
            vec![root],
        ))
        .unwrap();

    // `root`'s backlinks: whoever links to it (grandchild).
    assert_eq!(index.backlinks(root).unwrap(), vec![grandchild]);
    // Nothing links to `child`.
    assert!(index.backlinks(child).unwrap().is_empty());
}

#[test]
fn keyword_hit_on_title_outranks_hit_on_text() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();

    let a = NodeId::new();
    let b = NodeId::new();
    index
        .upsert(&node(a, "Quarterly Report", "nothing special here", None, vec![]))
        .unwrap();
    index
        .upsert(&node(
            b,
            "Notes",
            "see the quarterly report for details",
            None,
            vec![],
        ))
        .unwrap();

    let hits = index.search(&SearchQuery::new("quarterly").with_limit(10)).unwrap();
    assert_eq!(hits.len(), 2, "both nodes mention 'quarterly' once");
    assert_eq!(hits[0].node_id, a, "a title hit should outrank a text hit");
}

#[test]
fn phrase_query_matches_only_the_node_with_the_phrase() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();

    let a = NodeId::new();
    let b = NodeId::new();
    index
        .upsert(&node(a, "A", "the quick brown fox jumps", None, vec![]))
        .unwrap();
    index
        .upsert(&node(
            b,
            "B",
            "quick and brown but not adjacent, and there is a fox too",
            None,
            vec![],
        ))
        .unwrap();

    let hits = index
        .search(&SearchQuery::new("\"quick brown\"").with_limit(10))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_id, a);
}

#[test]
fn snippet_contains_the_query_term() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();

    let a = NodeId::new();
    index
        .upsert(&node(
            a,
            "Doc",
            "some intro text before the important keyword appears here",
            None,
            vec![],
        ))
        .unwrap();

    let hits = index.search(&SearchQuery::new("keyword").with_limit(10)).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].snippet.to_lowercase().contains("keyword"),
        "snippet was: {:?}",
        hits[0].snippet
    );
    assert_eq!(hits[0].kind, "node");
    assert_eq!(hits[0].path, None);
}

#[test]
fn remove_drops_the_node_and_its_backlink() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();

    let a = NodeId::new();
    let b = NodeId::new();
    index.upsert(&node(a, "Alpha", "alpha text", None, vec![])).unwrap();
    index
        .upsert(&node(b, "Beta", "beta text linking to alpha", None, vec![a]))
        .unwrap();
    assert_eq!(index.backlinks(a).unwrap(), vec![b]);

    index.remove(a).unwrap();

    let hits = index.search(&SearchQuery::new("alpha").with_limit(10)).unwrap();
    assert!(hits.iter().all(|h| h.node_id != a));
    assert!(index.backlinks(a).unwrap().is_empty());

    // b is unaffected.
    let hits = index.search(&SearchQuery::new("beta").with_limit(10)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_id, b);
}

#[test]
fn clear_then_reupsert_works() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();

    let a = NodeId::new();
    index.upsert(&node(a, "Alpha", "alpha text", None, vec![])).unwrap();
    assert_eq!(
        index.search(&SearchQuery::new("alpha").with_limit(10)).unwrap().len(),
        1
    );

    index.clear().unwrap();
    assert!(index
        .search(&SearchQuery::new("alpha").with_limit(10))
        .unwrap()
        .is_empty());

    index
        .upsert(&node(a, "Alpha", "alpha text again", None, vec![]))
        .unwrap();
    let hits = index.search(&SearchQuery::new("alpha").with_limit(10)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_id, a);
}

#[test]
fn tags_reconcile_on_reupsert() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();

    let a = NodeId::new();
    let mut n = node(a, "Alpha", "alpha text", None, vec![]);
    n.tags = vec!["red".to_string(), "green".to_string()];
    index.upsert(&n).unwrap();

    // Re-upsert with a different tag set; should not error (create-or-find
    // Tag, unlink the dropped one).
    n.tags = vec!["green".to_string(), "blue".to_string()];
    index.upsert(&n).unwrap();
}

#[test]
fn clear_works_on_a_multi_level_tree() {
    // A parent has no `@on_delete` policy declared explicitly in a to-one
    // relation defaults to Deny in rhypedb (unlike a to-many field, which
    // defaults to Remove) — `clear()` (and `remove()`) must still be able to
    // delete a root node while it still has children, in `scan_type` order,
    // not just leaf-to-root.
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();

    let root = NodeId::new();
    let child = NodeId::new();
    let grandchild = NodeId::new();
    index.upsert(&node(root, "Root", "root text", None, vec![])).unwrap();
    index
        .upsert(&node(child, "Child", "child text", Some(root), vec![]))
        .unwrap();
    index
        .upsert(&node(grandchild, "Grandchild", "grandchild text", Some(child), vec![]))
        .unwrap();

    index.clear().unwrap();

    for id in [root, child, grandchild] {
        assert!(index.backlinks(id).unwrap().is_empty());
    }
    assert!(index
        .search(&SearchQuery::new("root").with_limit(10))
        .unwrap()
        .is_empty());
    assert!(index
        .search(&SearchQuery::new("grandchild").with_limit(10))
        .unwrap()
        .is_empty());

    // The index is still usable after a full clear of a deep tree.
    let fresh = NodeId::new();
    index
        .upsert(&node(fresh, "Fresh", "fresh text", None, vec![]))
        .unwrap();
    let hits = index.search(&SearchQuery::new("fresh").with_limit(10)).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].node_id, fresh);
}

#[test]
fn reopen_reuses_the_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    let a = NodeId::new();
    {
        let index = SearchIndex::open(dir.path(), false).unwrap();
        index.upsert(&node(a, "Alpha", "alpha text", None, vec![])).unwrap();
    }
    {
        let index = SearchIndex::open(dir.path(), false).unwrap();
        let hits = index.search(&SearchQuery::new("alpha").with_limit(10)).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].node_id, a);
    }
}
