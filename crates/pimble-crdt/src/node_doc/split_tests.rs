//! Splitting a node's text in two and joining two nodes' text
//! (docs/SPLIT_CONTRACT.md).

use yrs::Map;

use super::*;

const T0: &str = "2026-11-06T00:00:00Z";

fn note(md: &str) -> NodeDoc {
    let mut doc = NodeDoc::new();
    doc.init("document", "Note", None, T0).unwrap();
    doc.write_markdown(md).unwrap();
    doc
}

fn rest_markdown(off: &SplitOff) -> String {
    let mut doc = NodeDoc::new();
    doc.apply_update(&off.rest).unwrap();
    doc.markdown().unwrap()
}

#[test]
fn a_split_inside_a_paragraph_leaves_each_half_its_part() {
    let mut doc = note("One two.\n\nThree four.\n\nFive.");
    let at = doc.sticky_at_quote("four.").unwrap();
    let off = doc.split_off(&at).unwrap();
    assert_eq!(doc.markdown().unwrap().trim(), "One two.\n\nThree");
    assert_eq!(rest_markdown(&off).trim(), "four.\n\nFive.");
}

#[test]
fn a_split_at_the_start_of_a_paragraph_leaves_no_empty_one() {
    let mut doc = note("One two.\n\nThree four.\n\nFive.");
    let at = doc.sticky_at_quote("Three").unwrap();
    let off = doc.split_off(&at).unwrap();
    assert_eq!(doc.markdown().unwrap().trim(), "One two.");
    assert_eq!(rest_markdown(&off).trim(), "Three four.\n\nFive.");
}

#[test]
fn a_split_with_nothing_before_the_caret_is_refused() {
    let mut doc = note("One two.\n\nThree.");
    let at = doc.sticky_at_quote("One").unwrap();
    let err = doc.split_off(&at).unwrap_err();
    assert!(err.to_string().contains("Nothing comes before the caret"), "{err}");
    assert_eq!(doc.markdown().unwrap().trim(), "One two.\n\nThree.", "nothing changed");
}

#[test]
fn a_canvas_goes_with_the_text_it_is_in() {
    let mut doc = NodeDoc::new();
    doc.init("document", "Note", None, T0).unwrap();
    {
        let mut txn = doc.ydoc().transact_mut();
        let board = doc.data_root().insert(&mut txn, "board:c1", yrs::MapPrelim::default());
        board.insert(&mut txn, "markup_visible", true);
    }
    doc.write_markdown("Intro\n\nMiddle\n\n![shot](pimble-blob:a/b#canvas=c1)\n\nEnd").unwrap();
    let before = NodeDoc::load(&doc.save()).unwrap();
    let at = doc.sticky_at_quote("Middle").unwrap();
    let off = doc.split_off(&at).unwrap();
    assert_eq!(off.keys, vec!["board:c1".to_string()]);
    assert!(doc.data_json().get("board:c1").is_none(), "the first half no longer holds it");
    let mut rest = NodeDoc::new();
    rest.init("document", "", None, T0).unwrap();
    rest.apply_update(&off.rest).unwrap();
    rest.copy_data_keys(&before, &off.keys);
    assert_eq!(rest.data_json()["board:c1"]["markup_visible"], true);
    assert!(rest.markdown().unwrap().contains("#canvas=c1"));
}

#[test]
fn a_split_inside_a_table_goes_before_the_table() {
    let mut doc = note("Intro\n\n| a | b |\n| --- | --- |\n| c | dee |\n\nEnd");
    // A sticky index inside the cell "dee", as the editor would make one
    // at a caret there.
    let at = {
        let schema = rinch_editor_core::Schema::starter_kit();
        let session = rinch_editor_collab::CollabSession::from_bytes(&doc.save()).unwrap();
        let model = session.projected_doc(&schema).unwrap();
        let mut inside = None;
        model.nodes_between(0, model.content_size(), &mut |node, pos, _| {
            if node.is_text() && node.text().map_or(false, |t| t == "dee") {
                inside = Some(pos + 1);
            }
            true
        });
        session.sticky_index(&model, rinch_editor_core::Pos(inside.expect("the cell"))).unwrap()
    };
    let off = doc.split_off(&at).unwrap();
    assert_eq!(doc.markdown().unwrap().trim(), "Intro");
    let rest = rest_markdown(&off);
    assert!(rest.trim_start().starts_with("| a"), "{rest}");
    assert!(rest.contains("dee") && rest.trim_end().ends_with("End"), "{rest}");
}

#[test]
fn joining_puts_the_other_text_after_this_one() {
    let mut a = note("One.\n\nTwo.");
    let b = note("Three.\n\nFour.");
    a.append_content_of(&b).unwrap();
    assert_eq!(a.markdown().unwrap().trim(), "One.\n\nTwo.\n\nThree.\n\nFour.");
    // Into a blank note, the other text alone.
    let mut empty = NodeDoc::new();
    empty.init("document", "", None, T0).unwrap();
    empty.append_content_of(&b).unwrap();
    assert_eq!(empty.markdown().unwrap().trim(), "Three.\n\nFour.");
}

#[test]
fn a_split_then_a_join_gives_the_text_back() {
    let md = "# Heading\n\nOne two three.\n\n- a\n- b\n\nLast.";
    let mut doc = note(md);
    let at = doc.sticky_at_quote("three.").unwrap();
    let off = doc.split_off(&at).unwrap();
    let mut rest = NodeDoc::new();
    rest.init("document", "", None, T0).unwrap();
    rest.apply_update(&off.rest).unwrap();
    doc.append_content_of(&rest).unwrap();
    // The paragraph the caret split stays two paragraphs (the space
    // before the caret kept where it was): joining puts blocks after
    // blocks.
    assert_eq!(doc.markdown().unwrap().trim(), "# Heading\n\nOne two \n\nthree.\n\n- a\n- b\n\nLast.");
}

/// docs/NOTIFICATIONS_CONTRACT.md "Built in wave 0": the hook wave 1
/// notifies from. An update that adds a mention names it; the same update
/// applied again, or one that only types elsewhere, names nothing; the
/// writer's own account is never named.
#[test]
fn an_update_names_the_mentions_it_adds() {
    let mut doc = NodeDoc::from_plain_text("Notes").unwrap();
    let before = doc.save();
    let adding = doc.append_markdown("Ask [@Will](pimble-person:will?m=w1) and [@Me](pimble-person:me?m=m1)").unwrap();
    let base = NodeDoc::load(&before).unwrap();
    let added = base.mentions_added_by(&adding, Some("me")).unwrap();
    assert_eq!(added.iter().map(|m| (m.account.as_str(), m.id.as_deref())).collect::<Vec<_>>(), vec![("will", Some("w1"))]);
    assert!(doc.mentions_added_by(&adding, Some("me")).unwrap().is_empty(), "already held: nothing new");
    let held = NodeDoc::load(&doc.save()).unwrap();
    let typing = doc.append_markdown("More words.").unwrap();
    assert!(held.mentions_added_by(&typing, None).unwrap().is_empty());
    assert_eq!(doc.mentions().len(), 2);
}
