//! A query that is a heading's text finds the note with that heading at the
//! top, and every hit says which heading it sits under (docs/MCP_CONTRACT.md,
//! "Where a search hit is").

use pimble_core::{IndexUnit, NodeId, UnitKind};
use pimble_search::{IndexNode, SearchHit, SearchIndex, SearchQuery};

/// A node made of units, its text their join (as the server builds it).
fn node_of(id: NodeId, title: &str, units: Vec<IndexUnit>) -> IndexNode {
    let text = units.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join("\n");
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

fn units(blocks: &[(Option<u8>, &str)]) -> Vec<IndexUnit> {
    blocks
        .iter()
        .enumerate()
        .map(|(i, (level, text))| {
            let kind = match level {
                Some(l) => UnitKind::Heading(*l),
                None => UnitKind::Prose,
            };
            IndexUnit::new(kind, format!("b:{i}"), *text)
        })
        .collect()
}

/// Notes that say "county" and "edges" often and briefly (what BM25 likes),
/// and one long note with the heading "The county's edges" among many.
fn fixture(index: &SearchIndex) -> NodeId {
    let filler = "The surveyors walked the hedgerows and wrote down every gate, stile and \
                  footpath they found, with the date and the weather. "
        .repeat(12);
    let mut blocks: Vec<(Option<u8>, String)> = vec![(Some(1), "Design".into()), (None, filler.clone())];
    for (i, heading) in ["Roads", "Rivers", "Villages", "Markets", "Churches", "The county's edges", "Woods", "Mills", "Bridges", "Parishes"]
        .iter()
        .enumerate()
    {
        blocks.push((Some(2), heading.to_string()));
        let body = if *heading == "The county's edges" {
            "Where the map stops: the river on the west, the old ridge road on the east.".to_string()
        } else {
            format!("{filler} Section {i}.")
        };
        blocks.push((None, body));
    }
    let borrowed: Vec<(Option<u8>, &str)> = blocks.iter().map(|(l, t)| (*l, t.as_str())).collect();
    let atlas = NodeId::new();
    index.upsert(&node_of(atlas, "Atlas of the region", units(&borrowed))).unwrap();

    for (title, text) in [
        ("County edges", "county edges county edges, the edges of the county"),
        ("Edges", "edges and edges of every county; county edges again"),
        ("County lines", "the county's edges are where the county ends: edges, edges"),
        ("Map notes", "county county county edges edges"),
        ("Borders", "a county has edges; the county edges are its borders"),
        ("Edge cases", "edges, edges and more county edges"),
    ] {
        index.upsert(&node_of(NodeId::new(), title, units(&[(None, text)]))).unwrap();
    }
    atlas
}

fn rank_of(hits: &[SearchHit], id: NodeId) -> Option<usize> {
    hits.iter().position(|h| h.node_id == id)
}

#[test]
fn a_query_that_is_a_heading_puts_its_note_first() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();
    let atlas = fixture(&index);

    for query in ["The county's edges", "the county's edges", "The countys edges", "The county’s edges!"] {
        let hits = index.search(&SearchQuery::new(query).with_limit(10)).unwrap();
        let rank = rank_of(&hits, atlas);
        eprintln!("{query:?}: the atlas ranks {rank:?} of {}", hits.len());
        assert_eq!(rank, Some(0), "{query:?}: {:#?}", hits.iter().map(|h| &h.title).collect::<Vec<_>>());
        assert_eq!(hits[0].section, vec!["Design".to_string(), "The county's edges".to_string()], "{query:?}");
    }
}

#[test]
fn a_keyword_hit_names_the_headings_above_its_words() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), false).unwrap();
    let id = NodeId::new();
    index
        .upsert(&node_of(
            id,
            "Plan",
            units(&[
                (None, "An opening paragraph."),
                (Some(1), "Design"),
                (None, "Shapes and colors."),
                (Some(2), "The county's edges"),
                (None, "The ridge road marks the eastern boundary."),
                (Some(1), "Budget"),
                (None, "Money for fences."),
            ]),
        ))
        .unwrap();
    let section = |q: &str| index.search(&SearchQuery::new(q).with_limit(5)).unwrap()[0].section.clone();
    assert_eq!(section("ridge road"), vec!["Design", "The county's edges"]);
    assert_eq!(section("fences"), vec!["Budget"]);
    assert_eq!(section("opening"), Vec::<String>::new());
    assert_eq!(section("Budget"), vec!["Budget"]);
}
