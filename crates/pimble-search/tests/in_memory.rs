//! The in-memory index (`SearchIndex::open_in_memory`, docs/ONE_CODEBASE_CONTRACT.md
//! section 3): what a browser's worker searches with, here natively. The same
//! schema and english analyzer as an index on disk, so the same words find
//! the same notes; nothing is written anywhere.

use pimble_core::{IndexUnit, NodeId, UnitKind};
use pimble_search::{IndexNode, SearchIndex, SearchQuery};

fn node(id: NodeId, title: &str, text: &str, parent: Option<NodeId>) -> IndexNode {
    IndexNode {
        node_id: id,
        kind: "document".to_string(),
        title: title.to_string(),
        text: text.to_string(),
        modified_at: 0,
        parent,
        tags: vec!["errand".to_string()],
        links: Vec::new(),
        units: vec![IndexUnit::new(UnitKind::Prose, "b:0", text)],
    }
}

#[test]
fn an_index_in_memory_finds_what_one_on_disk_finds() {
    let dir = tempfile::tempdir().unwrap();
    let on_disk = SearchIndex::open(dir.path(), false).unwrap();
    let in_memory = SearchIndex::open_in_memory().unwrap();
    assert!(in_memory.is_in_memory());
    assert!(!on_disk.is_in_memory());
    assert!(!in_memory.semantic_enabled());

    let root = NodeId::new();
    let garden = NodeId::new();
    let invoice = NodeId::new();
    let nodes = [
        node(root, "Home", "Everything about the house.", None),
        node(garden, "Garden plans", "The gardeners were gardening by the orchard wall.", Some(root)),
        node(invoice, "Plumber invoice", "Paid the plumber for the kitchen sink.", Some(root)),
    ];
    for index in [&on_disk, &in_memory] {
        for n in &nodes {
            index.upsert(n).unwrap();
        }
    }
    in_memory.run_deferred_work();

    // Stemming ("garden" finds "gardening"), a trailing prefix term
    // ("plumb" finds "plumber"), a title match, and nothing for a word no note has.
    for query in ["garden", "plumb", "invoice", "kitchen sink", "violin"] {
        let ids = |index: &SearchIndex| index.search(&SearchQuery::new(query)).unwrap().into_iter().map(|h| h.node_id).collect::<Vec<_>>();
        assert_eq!(ids(&in_memory), ids(&on_disk), "{query}");
    }
    let hits = in_memory.search(&SearchQuery::new("gardening")).unwrap();
    assert_eq!(hits.first().map(|h| h.node_id), Some(garden));

    // An update and a removal are seen at once.
    in_memory.upsert(&node(invoice, "Plumber invoice", "Paid the electrician instead.", Some(root))).unwrap();
    assert!(in_memory.search(&SearchQuery::new("sink")).unwrap().is_empty());
    assert_eq!(in_memory.search(&SearchQuery::new("electrician")).unwrap().len(), 1);
    in_memory.remove(garden).unwrap();
    in_memory.run_deferred_work();
    assert!(in_memory.search(&SearchQuery::new("orchard")).unwrap().is_empty());
}
