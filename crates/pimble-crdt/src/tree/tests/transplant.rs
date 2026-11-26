use super::*;

// ── `move_or_transplant` and `transplant` ───────────────────────────

#[test]
fn move_or_transplant_moves_inside_a_share_and_transplants_out_of_it() {
    let mut s = shared();
    let (now_id, edit) = s.tree.move_or_transplant(HERE, s.x, s.b, Place::Last, T2, &mut never_asked).unwrap();
    assert_eq!(now_id, s.x, "a plain move keeps the id");
    assert_eq!(ids_of(&edit), HashSet::from([s.a, s.b, s.x]));
    assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.x]);

    let (now_id, _) = s.tree.move_or_transplant(HERE, s.own, s.b, Place::First, T2, &mut never_asked).unwrap();
    assert_eq!(now_id, s.own, "entering a share is a plain move");
    assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.own, s.x]);

    let mut made = Vec::new();
    let (now_id, edit) = s.tree.move_or_transplant(HERE, s.x, s.private, Place::Last, T3, &mut fresh_ids(&mut made)).unwrap();
    assert_eq!(made, vec![now_id], "one node, one new id");
    assert_ne!(now_id, s.x);
    assert_eq!(edit.node_ids(), vec![now_id, s.private, s.x, s.b], "made, listed, tombstoned, unlisted");
    assert!(!s.tree.has_node(s.x));
    assert_eq!(s.tree.get_children(s.private).unwrap(), vec![now_id]);
    assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.own]);
    assert_eq!(s.tree.get_node_info(now_id).unwrap().title, "X");
    assert!(s.tree.validate_tree().is_empty());

    // What `move_node` refuses, this refuses, whichever way it would go.
    assert!(s.tree.move_or_transplant(HERE, s.root, s.b, Place::Last, T3, &mut never_asked).is_err());
    assert!(s.tree.move_or_transplant(HERE, s.a, s.r2, Place::Last, T3, &mut never_asked).is_err());
    assert!(s.tree.move_or_transplant(HERE, s.y, id(), Place::Last, T3, &mut never_asked).is_err());
    assert!(s.tree.has_node(s.y), "a refused transplant deletes nothing");
}

#[test]
fn transplant_makes_the_subtree_again_and_tombstones_the_original() {
    let mut s = shared();
    let mut replica = replica_of(&s.tree);
    // Under `a` already: x, r2 (a share, with y). More: a note deleted
    // earlier, a mount, tags, custom fields, plugin data, text.
    let dead = note(&mut s.tree, s.a, "Deleted earlier");
    s.tree.remove_node(dead, T1).unwrap();
    let mount = id();
    s.tree.add_node(mount, Some(s.a), Place::Last, pimble_core::node_types::MOUNT, "Elsewhere", T1).unwrap();
    let mount_ref = serde_json::json!({ "source_store": "6a7e6f52-0000-4000-8000-000000000001", "source_node": null, "source_path": "/stores/other.pimble" });
    s.tree.set_custom(mount, "mount", &mount_ref, T1).unwrap();
    s.tree.set_tags(s.x, &["one".into(), "two".into()], T1).unwrap();
    s.tree.set_custom(s.x, "icon", &serde_json::json!("star"), T1).unwrap();
    s.tree.set_custom(s.x, "explicit_title", &serde_json::json!(true), T1).unwrap();
    s.tree.doc_mut(s.x).unwrap().set_data("plugin", &serde_json::json!({ "n": 3, "list": ["a", { "b": null }] })).unwrap();
    s.tree.doc_mut(s.x).unwrap().replace_plain_text("first line\nsecond line").unwrap();
    s.tree.doc_mut(s.y).unwrap().replace_plain_text("inside the inner share").unwrap();
    sync(&mut s.tree, &mut replica);

    let before = s.tree.subtree_ids(s.a).unwrap();
    assert_eq!(before, vec![s.a, s.x, s.r2, s.y, mount], "preorder, the tombstone not among them");
    let mut made = Vec::new();
    let (new_a, edit) = s.tree.transplant(HERE, s.a, s.private, Place::First, T3, &mut fresh_ids(&mut made)).unwrap();

    // New ids, all of them, one per live node in the subtree's preorder.
    assert_eq!(made.len(), before.len());
    assert_eq!(made[0], new_a);
    assert!(made.iter().all(|new| !before.contains(new)));
    let new_of: HashMap<NodeId, NodeId> = before.iter().copied().zip(made.iter().copied()).collect();

    // Every original's tombstone names the node it became, in this store.
    for (&old, &new) in &new_of {
        assert_eq!(s.tree.doc(old).unwrap().fields().unwrap().became, Some(PimbleUrl::node(HERE, new)));
    }

    // The edit's order says what happened in what order: every new
    // document, the list that names the new root, then every tombstone
    // and the list the original left.
    let mut expected = made.clone();
    expected.push(s.private);
    expected.extend(&before);
    expected.push(s.r0);
    assert_eq!(edit.node_ids(), expected);

    // The same nodes again, under the new parent, in the same order.
    assert_eq!(s.tree.get_children(s.private).unwrap(), vec![new_a, s.own]);
    assert_eq!(s.tree.subtree_ids(new_a).unwrap(), made);
    for (&old, &new) in &new_of {
        let (was, is) = (s.tree.doc(old).unwrap().fields().unwrap(), s.tree.get_node_info(new).unwrap());
        assert_eq!((&is.node_type, &is.title, &is.tags), (&was.node_type, &was.title, &was.tags), "{}", was.title);
        assert_eq!(is.created_at, was.created_at, "created when the original was");
        assert_eq!(is.modified_at, T3, "modified now");
        let expected_parent = if old == s.a { s.private } else { new_of[&was.parent_id.unwrap()] };
        assert_eq!(is.parent_id, Some(expected_parent));
        // Every custom field but the share marker, on every node.
        let mut custom = was.custom.clone();
        custom.remove(SHARE);
        assert_eq!(is.custom, custom, "{}", was.title);
        assert_eq!(s.tree.doc(new).unwrap().data_json(), s.tree.doc(old).unwrap().data_json());
        assert_eq!(s.tree.doc(new).unwrap().text(), s.tree.doc(old).unwrap().text());
    }
    assert!(s.tree.shares(new_of[&s.y]).is_empty(), "no share marker survives: the new nodes are in no share");
    assert_eq!(s.tree.doc(new_of[&s.x]).unwrap().text(), "first line\nsecond line");
    assert_eq!(s.tree.get_node_info(new_of[&s.x]).unwrap().custom.get("icon"), Some(&serde_json::json!("star")));
    // A mount is copied as the reference it is.
    let new_mount = s.tree.get_node_info(new_of[&mount]).unwrap();
    assert_eq!(new_mount.node_type, pimble_core::node_types::MOUNT);
    assert_eq!(new_mount.custom.get("mount"), Some(&mount_ref));

    // The original: an ordinary delete. Tombstones with one stamp, lists
    // below kept, the marker still on the inner share's root, and the
    // note deleted earlier neither copied nor restamped.
    for &old in &before {
        assert!(!s.tree.has_node(old));
        assert_eq!(s.tree.doc(old).unwrap().fields().unwrap().deleted_at.as_deref(), Some(T3));
    }
    assert_eq!(s.tree.doc(dead).unwrap().fields().unwrap().deleted_at.as_deref(), Some(T1));
    assert!(s.tree.doc(s.r2).unwrap().fields().unwrap().custom.contains_key(SHARE));
    assert_eq!(s.tree.doc(s.a).unwrap().children(), vec![s.x, s.r2, mount], "an undelete finds the subtree again");
    assert_eq!(s.tree.get_children(s.r0).unwrap(), vec![s.b]);
    assert_eq!(s.tree.list_node_ids().len(), 5 + before.len(), "root, private, own, r0, b and the new nodes");
    assert!(s.tree.validate_tree().is_empty());
    assert!(s.tree.repair(T3).unwrap().is_none());

    // A peer that applies the edit holds the same.
    apply_edit(&mut replica, &edit);
    assert_same(&s.tree, &replica);
    assert!(replica.validate_tree().is_empty());
}

#[test]
fn undeleting_the_original_after_a_transplant_leaves_both() {
    let mut s = shared();
    s.tree.doc_mut(s.y).unwrap().replace_plain_text("kept").unwrap();
    let mut made = Vec::new();
    let (new_a, _) = s.tree.transplant(HERE, s.a, s.private, Place::Last, T2, &mut fresh_ids(&mut made)).unwrap();

    let edit = s.tree.undelete_node(s.a, T3).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([s.a, s.x, s.r2, s.y, s.r0]));
    // The members' folder is back where it was, a share inside it still
    // a share; the new one is where it landed; they go their own ways.
    assert_eq!(s.tree.get_children(s.r0).unwrap(), vec![s.b, s.a]);
    assert_eq!(s.tree.subtree_ids(s.a).unwrap(), vec![s.a, s.x, s.r2, s.y]);
    assert_eq!(s.tree.shares(s.y), vec![s.r2, s.r0]);
    assert_eq!(s.tree.subtree_ids(new_a).unwrap(), made);
    assert!(s.tree.shares(*made.last().unwrap()).is_empty());
    s.tree.set_title(s.x, "Theirs", T3).unwrap();
    assert_eq!(s.tree.get_node_info(made[1]).unwrap().title, "X");
    assert!(s.tree.validate_tree().is_empty());
}

/// A transplant carries the `data` root with its yrs types (a board's
/// value written whole, its points as buffers, its text as text), not
/// through JSON, which would turn them into nested maps and numbers.
#[test]
fn transplant_keeps_the_shapes_of_data() {
    use yrs::{Any, Array, ArrayPrelim, ArrayRef, Map, MapPrelim, MapRef, Out, TextPrelim, Transact};
    let mut s = shared();
    {
        let doc = s.tree.doc_mut(s.x).unwrap();
        let data = doc.data_root();
        let mut txn = doc.ydoc().transact_mut();
        let board: MapRef = data.insert(&mut txn, "board:b1", MapPrelim::default());
        let mut geom = std::collections::HashMap::new();
        geom.insert("x".to_string(), Any::Number(1.5));
        board.insert(&mut txn, "geom", Any::Map(geom.into()));
        let points: ArrayRef = board.insert(&mut txn, "points", ArrayPrelim::default());
        points.push_back(&mut txn, Any::Buffer(vec![1u8, 2, 3].into()));
        board.insert(&mut txn, "text", TextPrelim::new("note"));
    }
    let mut made = Vec::new();
    let (new_a, _) = s.tree.transplant(HERE, s.a, s.private, Place::Last, T3, &mut fresh_ids(&mut made)).unwrap();
    let new_x = s.tree.subtree_ids(new_a).unwrap()[1];
    let doc = s.tree.doc(new_x).unwrap();
    let txn = doc.ydoc().transact();
    let Some(Out::YMap(board)) = doc.data_root().get(&txn, "board:b1") else { panic!("no board") };
    assert!(matches!(board.get(&txn, "geom"), Some(Out::Any(Any::Map(_)))), "a value written whole stays one");
    let Some(Out::YArray(points)) = board.get(&txn, "points") else { panic!("no points") };
    assert!(matches!(points.get(&txn, 0), Some(Out::Any(Any::Buffer(b))) if b.as_ref() == [1u8, 2, 3]));
    assert!(matches!(board.get(&txn, "text"), Some(Out::YText(_))));
}

/// A metanode stays one and fields stay (Joe, 2026-11-04: a kanban moved
/// to another store came out a plain node with the kanban's icon).
#[test]
fn transplant_keeps_a_metanode_and_its_fields() {
    let mut s = shared();
    {
        let doc = s.tree.doc_mut(s.a).unwrap();
        doc.set_metanode_viewer(Some("kanban"), T3).unwrap();
        doc.set_metanode_source(Some(&pimble_core::metanode::Source::subtree(2)), T3).unwrap();
        doc.set_field("Due", Some("Friday"), T3).unwrap();
    }
    let mut made = Vec::new();
    let (new_a, _) = s.tree.transplant(HERE, s.a, s.private, Place::Last, T3, &mut fresh_ids(&mut made)).unwrap();
    let doc = s.tree.doc(new_a).unwrap();
    let spec = doc.metanode().expect("still a metanode");
    assert_eq!(spec.viewer, "kanban");
    assert_eq!(spec.source.depth(), 2);
    assert_eq!(doc.node_fields(), vec![("Due".to_string(), "Friday".to_string())]);
}

#[test]
fn transplant_refuses_before_it_writes_anything() {
    let mut s = shared();
    let before = snapshot(&s.tree);
    let mut no_ids = || -> NodeId { panic!("nothing should be made") };
    assert!(s.tree.transplant(HERE, s.root, s.private, Place::Last, T2, &mut no_ids).is_err(), "the root");
    assert!(s.tree.transplant(HERE, s.a, s.r2, Place::Last, T2, &mut no_ids).is_err(), "under its own descendant");
    assert!(s.tree.transplant(HERE, s.a, s.a, Place::Last, T2, &mut no_ids).is_err(), "under itself");
    assert!(s.tree.transplant(HERE, s.a, id(), Place::Last, T2, &mut no_ids).is_err(), "under nothing");
    assert!(s.tree.transplant(HERE, id(), s.private, Place::Last, T2, &mut no_ids).is_err(), "nothing");
    assert!(s.tree.take_cutting(s.root).is_err());
    // An id already held, and the same id twice.
    let mut held = || s.own;
    assert!(s.tree.transplant(HERE, s.a, s.private, Place::Last, T2, &mut held).is_err());
    let twice = id();
    let mut same = || twice;
    assert!(s.tree.transplant(HERE, s.a, s.private, Place::Last, T2, &mut same).is_err());
    assert_eq!(snapshot(&s.tree), before, "refused means untouched");
    assert!(s.tree.doc(twice).is_none());

    // Deleted already: not a node.
    s.tree.remove_node(s.x, T2).unwrap();
    assert!(s.tree.transplant(HERE, s.x, s.private, Place::Last, T3, &mut no_ids).is_err());
}

/// Content rinch's projection refuses (it fails loudly by design on what
/// is outside the collaboration scope) stops the transplant: better none
/// than one that loses the text and then deletes the note.
#[test]
fn content_that_cannot_be_projected_refuses_the_transplant() {
    use yrs::{Array, Map, ReadTxn, Transact};
    let mut s = shared();
    // The format tag, and a `content` entry that is no projected node.
    let junk = yrs::Doc::with_options(yrs::Options { offset_kind: yrs::OffsetKind::Utf16, ..Default::default() });
    let (meta, content) = (junk.get_or_insert_map("meta"), junk.get_or_insert_array("content"));
    {
        let mut txn = junk.transact_mut();
        meta.insert(&mut txn, "format", "rinch-editor-collab/yrs-1");
        content.insert(&mut txn, 0, "not a block");
    }
    let bytes = junk.transact().encode_state_as_update_v1(&yrs::StateVector::default());
    s.tree.apply_update(s.y, &bytes).unwrap();

    let before = snapshot(&s.tree);
    let mut made = Vec::new();
    let err = s.tree.transplant(HERE, s.a, s.private, Place::Last, T2, &mut fresh_ids(&mut made)).unwrap_err();
    assert!(matches!(err, CrdtError::Collab(_)), "{err}");
    assert_eq!(snapshot(&s.tree), before);
    assert!(s.tree.has_node(s.a) && s.tree.has_node(s.y));
}

/// Between two stores: read out of one, planted in the other, deleted in
/// the first, in that order; one `TreeEdit` per store.
#[test]
fn a_transplant_between_two_trees_is_a_cutting_planted_and_a_delete() {
    let mut s = shared();
    s.tree.doc_mut(s.y).unwrap().replace_plain_text("carried across").unwrap();
    s.tree.set_tags(s.y, &["t".into()], T1).unwrap();
    let (mut other, other_root) = tree();
    let landing = folder(&mut other, other_root, "Landing");
    let mut other_replica = replica_of(&other);

    let cutting = s.tree.take_cutting(s.a).unwrap();
    assert_eq!(cutting.len(), 4);
    assert!(!cutting.is_empty());
    let sources: Vec<NodeId> = cutting.nodes().iter().map(|n| n.source_id).collect();
    assert_eq!(sources, s.tree.subtree_ids(s.a).unwrap());
    assert_eq!(cutting.nodes().iter().map(|n| n.parent).collect::<Vec<_>>(), vec![None, Some(0), Some(0), Some(2)]);
    assert!(cutting.nodes().iter().all(|n| !n.custom.contains_key(SHARE)), "the marker is not even read out");
    assert!(cutting.nodes()[3].has_content() && !cutting.nodes()[0].has_content());
    assert!(s.tree.has_node(s.a), "reading changes nothing");

    let mut made = Vec::new();
    let (made_by, planted) = other.plant(cutting, landing, Place::Last, T2, &mut fresh_ids(&mut made)).unwrap();
    let new_a = made_by.root;
    assert_eq!(made_by.ids, sources.iter().copied().zip(made.iter().copied()).collect::<Vec<_>>());
    let mut expected = made.clone();
    expected.push(landing);
    assert_eq!(planted.node_ids(), expected);
    let removed = s.tree.remove_transplanted(s.a, THERE, &made_by, T2).unwrap();
    assert_eq!(removed.node_ids(), vec![s.a, s.x, s.r2, s.y, s.r0]);
    // Every original's tombstone names the node it became, in the other store.
    for (old, new) in &made_by.ids {
        let fields = s.tree.fields_of(*old).unwrap();
        assert_eq!(fields.deleted_at.as_deref(), Some(T2));
        assert_eq!(fields.became, Some(PimbleUrl::node(THERE, *new)), "{old}");
    }

    assert_eq!(other.get_children(landing).unwrap(), vec![new_a]);
    assert_eq!(other.subtree_ids(new_a).unwrap(), made);
    let new_y = other.get_node_info(made[3]).unwrap();
    assert_eq!((new_y.title.as_str(), new_y.parent_id, new_y.tags.clone()), ("Y", Some(made[2]), vec!["t".to_string()]));
    assert_eq!(other.doc(made[3]).unwrap().text(), "carried across");
    assert!(other.shares(made[3]).is_empty());
    assert!(other.validate_tree().is_empty() && s.tree.validate_tree().is_empty());
    assert!(!s.tree.has_node(s.y));

    apply_edit(&mut other_replica, &planted);
    assert_same(&other, &other_replica);

    // The second half refuses what `add_node` refuses.
    let again = s.tree.take_cutting(s.b).unwrap();
    assert!(other.plant(again, id(), Place::Last, T2, &mut NodeId::new).is_err(), "under nothing");
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle.as_bytes())
}

fn clients_of(doc: &NodeDoc) -> HashSet<yrs::ClientID> {
    use yrs::updates::decoder::Decode;
    yrs::StateVector::decode_v1(&doc.state_vector()).unwrap().iter().map(|(client, _)| *client).collect()
}

/// The new document is a NEW yrs document: no struct of the old one, no
/// deletion, nothing that was ever taken out of the old one.
#[test]
fn a_transplanted_document_has_its_own_history_and_nothing_that_was_deleted() {
    use yrs::updates::decoder::Decode;
    let mut s = shared();
    let mut others = replica_of(&s.tree);
    let key_id = marker("")["key_id"].as_str().unwrap().to_string();
    // A share's root with a past: a title it no longer has, text that was
    // deleted, a custom field that was removed, plugin data replaced.
    s.tree.set_title(s.r2, "WITHDRAWN-TITLE", T1).unwrap();
    s.tree.set_title(s.r2, "Recipes", T1).unwrap();
    s.tree.doc_mut(s.r2).unwrap().replace_plain_text("keep this\nand the REDACTED-PARAGRAPH").unwrap();
    s.tree.doc_mut(s.r2).unwrap().replace_plain_text("keep this").unwrap();
    s.tree.set_custom(s.r2, "note", &serde_json::json!("RETRACTED-FIELD"), T1).unwrap();
    s.tree.remove_custom(s.r2, "note", T1).unwrap();
    s.tree.doc_mut(s.r2).unwrap().set_data("k", &serde_json::json!("REPLACED-DATA")).unwrap();
    s.tree.doc_mut(s.r2).unwrap().set_data("k", &serde_json::json!("data now")).unwrap();
    assert!(contains(&s.tree.doc(s.r2).unwrap().save(), &key_id), "the original carries its marker");

    let mut made = Vec::new();
    let (new_r2, _) = s.tree.transplant(HERE, s.r2, s.private, Place::Last, T2, &mut fresh_ids(&mut made)).unwrap();
    let new_doc = s.tree.doc(new_r2).unwrap();
    assert_eq!(new_doc.text(), "keep this");
    assert_eq!(new_doc.data_json(), serde_json::json!({ "k": "data now" }));
    let bytes = new_doc.save();
    for gone in ["WITHDRAWN-TITLE", "REDACTED-PARAGRAPH", "RETRACTED-FIELD", "REPLACED-DATA", key_id.as_str(), "pimble.example"] {
        assert!(!contains(&bytes, gone), "{gone} is in the new document's bytes");
    }
    assert!(contains(&bytes, "keep this") && contains(&bytes, "Recipes"));
    // Nothing was ever deleted in it, and no client that wrote the old
    // document wrote any of it.
    let update = yrs::Update::decode_v1(&bytes).unwrap();
    assert!(update.delete_set().iter().all(|(_, ranges)| ranges.is_empty()), "a new document has no deletions");
    assert!(clients_of(new_doc).is_disjoint(&clients_of(s.tree.doc(s.r2).unwrap())));

    // So an edit of the one, misdirected at the other, merges into
    // nothing: it continues a history the other does not have.
    sync(&mut s.tree, &mut others);
    let theirs = others.doc_mut(s.r2).unwrap().replace_plain_text("the members' text moves on").unwrap();
    let mut misdirected = NodeDoc::load(&bytes).unwrap();
    misdirected.apply_update(&theirs).unwrap();
    assert_eq!(misdirected.text(), "keep this");
    assert_eq!(misdirected.fields().unwrap().title, "Recipes");
}

#[test]
fn a_node_with_no_content_yet_is_transplanted_with_none() {
    let mut s = shared();
    assert_eq!(s.tree.doc(s.x).unwrap().text(), "");
    let (new_x, _) = s.tree.transplant(HERE, s.x, s.private, Place::Last, T2, &mut NodeId::new).unwrap();
    // No projection was made up for it: its first edit seeds it, as for
    // any node whose content was never written.
    let mut peer = NodeDoc::load(&s.tree.doc(new_x).unwrap().save()).unwrap();
    let seed = s.tree.doc_mut(new_x).unwrap().replace_plain_text("first words").unwrap();
    peer.apply_update(&seed).unwrap();
    assert_eq!(peer.text(), "first words");
}
