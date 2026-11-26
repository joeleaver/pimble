use super::*;

fn hit(store_id: StoreId, node_id: NodeId, title: &str) -> SearchResultItem {
    SearchResultItem {
        node_id,
        store_id,
        score: 1.0,
        title: title.into(),
        snippet: String::new(),
        kind: "prose".into(),
        node_type: "document".into(),
        path: String::new(),
        section: Vec::new(),
    }
}

#[test]
fn a_url_is_offered_first_and_each_note_once() {
    let (s, a, b) = (StoreId::new(), NodeId::new(), NodeId::new());
    let hits = vec![hit(s, a, "Plan"), hit(s, a, "Plan"), hit(s, b, "")];
    let rows = picker_rows("example.com/plan", &hits, |_| Some("Notes".into()));
    assert_eq!(rows[0], PickerRow::Web { href: "https://example.com/plan".into() });
    assert_eq!(rows.len(), 3, "the web row, then each note once: {rows:?}");
    assert_eq!(rows[2].link_text("q"), "Untitled");
    assert_eq!(rows[1].href(), format!("pimble:{s}/{a}"));
    assert_eq!(rows[0].link_text(" example.com/plan "), "example.com/plan");

    let plain = picker_rows("plan", &hits, |_| None);
    assert!(matches!(plain[0], PickerRow::Node { .. }), "words are not a URL");
    let many: Vec<_> = (0..20).map(|i| hit(s, NodeId::new(), &format!("n{i}"))).collect();
    assert_eq!(picker_rows("n", &many, |_| None).len(), MAX_NODE_ROWS);
    assert_eq!(picker_rows("n.io", &many, |_| None).len(), MAX_NODE_ROWS + 1, "the web row does not take a note's place");
}

/// Each text run of the first paragraph with its link's href, if any.
fn runs(handle: &EditorHandle) -> Vec<(String, Option<String>)> {
    let doc = handle.doc();
    let para = doc.child(0);
    (0..para.child_count())
        .map(|i| {
            let run = para.child(i);
            let href = run.marks().iter().find(|m| m.type_name() == "link").and_then(|m| m.attrs.get_str("href")).map(str::to_string);
            (run.text().unwrap_or_default().to_string(), href)
        })
        .collect()
}

fn typed(handle: &EditorHandle, text: &str) {
    let text = text.to_string();
    assert!(handle.update(move |s| {
        let mut tr = s.tr();
        tr.insert_text(&text).ok()?;
        Some(tr)
    }));
}

#[test]
fn a_pick_replaces_the_query_with_linked_words_and_what_follows_is_not_linked() {
    let handle = crate::rinch_editor::create_editor();
    typed(&handle, "see [[pla");
    // The paragraph's text starts at 1: `[[pla` is 5..10.
    assert!(insert_link(&handle, 5, 10, "Plan", "pimble:x", false));
    typed(&handle, " next");
    assert_eq!(runs(&handle), vec![("see ".into(), None), ("Plan".into(), Some("pimble:x".into())), (" next".into(), None)]);
}

#[test]
fn a_pick_over_a_selection_links_its_words() {
    let handle = crate::rinch_editor::create_editor();
    typed(&handle, "read the plan today");
    assert!(insert_link(&handle, 6, 14, "ignored", "https://example.com", true));
    assert_eq!(
        runs(&handle),
        vec![("read ".into(), None), ("the plan".into(), Some("https://example.com".into())), (" today".into(), None)]
    );
}

#[test]
fn a_typed_pick_follows_the_answer_and_links_the_title() {
    let store = crate::state::AppStore::new();
    let handle = crate::rinch_editor::create_editor();
    typed(&handle, "see [[pla");
    let (s, a, b) = (StoreId::new(), NodeId::new(), NodeId::new());

    open(store, PickerKind::Typed { from: 5 }, 10.0, 20.0);
    set_query(store, "pla".into());
    assert!(store.link_picker.get().unwrap().rows.is_empty(), "nothing answered yet");
    search_answered(store, &Ok(vec![hit(s, a, "Plan"), hit(s, b, "Plants")]));
    step(store, 1);
    let view = store.link_picker.get().unwrap();
    assert_eq!((view.rows.len(), view.selected, view.x, view.y), (2, 1, 10.0, 20.0));

    assert!(accept(store, &handle, 1));
    assert_eq!(store.link_picker.get(), None, "a pick closes it");
    let href = format!("pimble:{s}/{b}");
    assert_eq!(runs(&handle), vec![("see ".into(), None), ("Plants".into(), Some(href))]);

    // An answer after it closed changes nothing.
    search_answered(store, &Ok(vec![hit(s, a, "Plan")]));
    assert_eq!(store.link_picker.get(), None);
}

#[test]
fn a_selection_is_linked_as_it_is_and_an_edited_link_can_be_removed() {
    let store = crate::state::AppStore::new();
    let handle = crate::rinch_editor::create_editor();
    typed(&handle, "read the plan today");
    open(store, PickerKind::Selection { from: 6, to: 14, editing: false }, 0.0, 0.0);
    set_query(store, "example.com".into());
    assert!(accept(store, &handle, 0));
    assert_eq!(
        runs(&handle),
        vec![("read ".into(), None), ("the plan".into(), Some("https://example.com".into())), (" today".into(), None)]
    );

    open(store, PickerKind::Selection { from: 6, to: 14, editing: true }, 0.0, 0.0);
    assert_eq!(store.link_picker.get().unwrap().rows, vec![PickerRow::RemoveLink]);
    assert!(accept(store, &handle, 0));
    assert_eq!(runs(&handle), vec![("read the plan today".into(), None)]);
}

#[test]
fn the_typed_query_is_what_follows_the_brackets_up_to_the_caret() {
    use rinch_editor_core::selection::Selection;
    use rinch_editor_core::Pos;
    let handle = crate::rinch_editor::create_editor();
    typed(&handle, "see [[ven dors");
    let doc = handle.doc();
    // The brackets are 5..7; the caret moves along the text.
    assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(7))).as_deref(), Some(""));
    assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(10))).as_deref(), Some("ven"));
    assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(15))).as_deref(), Some("ven dors"));
    assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(6))), None, "inside the brackets");
    assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(3))), None, "before them");
    assert_eq!(typed_query(&doc, 5, Selection::text(Pos(7), Pos(10))), None, "a range");
    assert_eq!(typed_query(&doc, 4, Selection::cursor(Pos(10))), None, "no brackets there");
}

/// An app whose focused pane holds a note of a store shared with Will and
/// Sam (and this account, which `@` never offers).
fn shared_note(people: bool) -> (crate::state::AppStore, StoreId, NodeId) {
    let store = crate::state::AppStore::new();
    let (s, n) = (StoreId::new(), NodeId::new());
    if people {
        let person = |id: &str, name: &str| pimble_rpc::Person { user_id: id.into(), name: name.into(), notice_key: None };
        let scope = pimble_rpc::ScopePeople {
            root: None,
            people: vec![person("will", "Will"), person("sam", "Sam"), person("wilma", "Wilma")],
            my_name: "Joe".into(),
            my_name_chosen: true,
            share_name: String::new(),
        };
        store.shares_ui.people.set(std::collections::HashMap::from([(s, pimble_rpc::StorePeople { scopes: vec![scope], ..Default::default() })]));
    }
    store.pane(store.focused()).active_edit.set(Some(crate::state::ActiveEdit { store_id: s, node_id: n }));
    (store, s, n)
}

#[test]
fn at_offers_the_notes_people_and_a_pick_writes_a_mention() {
    let (store, _, _) = shared_note(true);
    let handle = crate::rinch_editor::create_editor();
    typed(&handle, "ask @wi");
    // "ask " is 1..5: the `@` is at 5.
    open(store, PickerKind::Mention { from: 5 }, 0.0, 0.0);
    set_query(store, "wi".into());
    let rows = store.link_picker.get().unwrap().rows;
    assert_eq!(rows, vec![PickerRow::Person { account: "will".into(), name: "Will".into() }, PickerRow::Person { account: "wilma".into(), name: "Wilma".into() }]);
    assert!(accept(store, &handle, 0));
    let r = runs(&handle);
    assert_eq!(r.len(), 3, "{r:?}");
    assert_eq!((r[0].0.as_str(), r[1].0.as_str(), r[2].0.as_str()), ("ask ", "@Will", " "));
    let href = r[1].1.clone().unwrap();
    let mention = pimble_core::mention::Mention::from_link(&href, &r[1].0).unwrap();
    assert_eq!((mention.account.as_str(), mention.name.as_str()), ("will", "Will"));
    assert!(mention.id.is_some(), "a picked mention has its id");
    assert_eq!(pimble_crdt::mentions::of_model(&handle.doc()), vec![mention.clone()]);

    // Undo and redo put the same mention back, id and all: nobody is
    // told twice (pimble_core::mention::added says why).
    assert!(handle.command("undo"));
    assert!(pimble_crdt::mentions::of_model(&handle.doc()).is_empty());
    assert!(handle.command("redo"));
    assert_eq!(pimble_crdt::mentions::of_model(&handle.doc()), vec![mention]);
}

#[test]
fn at_steps_aside_for_words_nobody_is_called() {
    let (store, _, _) = shared_note(true);
    open(store, PickerKind::Mention { from: 5 }, 0.0, 0.0);
    set_query(store, "home".into());
    assert_eq!(store.link_picker.get(), None, "no Will called home: the words are words");
}

#[test]
fn at_in_a_note_nobody_else_can_open_offers_only_sharing_and_enter_is_a_new_line() {
    let (store, _, _) = shared_note(false);
    let handle = crate::rinch_editor::create_editor();
    open(store, PickerKind::Mention { from: 1 }, 0.0, 0.0);
    let view = store.link_picker.get().unwrap();
    assert_eq!(view.rows, vec![PickerRow::ShareThis]);
    assert_eq!(quiet_line(&view), "Only you can open this note");
    assert!(!key(store, &handle, "Enter", false), "Enter goes to the note");
    assert_eq!(store.link_picker.get(), None);
    // Chosen with the arrows, it is taken.
    open(store, PickerKind::Mention { from: 1 }, 0.0, 0.0);
    step(store, 1);
    assert!(store.link_picker.get().unwrap().moved);
}

#[test]
fn the_mention_query_is_what_follows_the_at() {
    use rinch_editor_core::selection::Selection;
    use rinch_editor_core::Pos;
    let handle = crate::rinch_editor::create_editor();
    typed(&handle, "hi @Will Sm");
    let doc = handle.doc();
    assert_eq!(typed_mention_query(&doc, 4, Selection::cursor(Pos(5))).as_deref(), Some(""));
    assert_eq!(typed_mention_query(&doc, 4, Selection::cursor(Pos(12))).as_deref(), Some("Will Sm"));
    assert_eq!(typed_mention_query(&doc, 4, Selection::cursor(Pos(3))), None, "before it");
    assert_eq!(typed_mention_query(&doc, 3, Selection::cursor(Pos(8))), None, "no @ there");
}

#[test]
fn the_highlight_wraps_at_both_ends() {
    assert_eq!(step_selection(0, 3, -1), 2);
    assert_eq!(step_selection(2, 3, 1), 0);
    assert_eq!(step_selection(1, 3, 1), 2);
    assert_eq!(step_selection(0, 0, 1), 0);
}
