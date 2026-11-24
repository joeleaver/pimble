use super::*;

// ── Operations ──────────────────────────────────────────────────────

#[test]
fn a_new_tree_has_a_root_folder() {
    let (mut tree, root) = tree();
    assert_eq!(tree.root(), root);
    assert!(tree.has_node(root));
    let info = tree.get_node_info(root).unwrap();
    assert_eq!(info.title, "Store");
    assert_eq!(info.node_type, "folder");
    assert_eq!(info.parent_id, None);
    assert_eq!(tree.list_node_ids(), vec![root]);
    assert!(tree.get_children(root).unwrap().is_empty());
    assert!(tree.validate_tree().is_empty());
    assert!(tree.repair(T1).unwrap().is_none());
}

#[test]
fn add_node_places_a_child_and_names_both_documents() {
    let (mut a, root) = tree();
    let mut b = replica_of(&a);
    let child = id();
    let edit = a.add_node(child, None, Place::Last, "document", "Doc", T1).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([child, root]));

    assert!(a.has_node(child));
    let info = a.get_node_info(child).unwrap();
    assert_eq!((info.title.as_str(), info.node_type.as_str(), info.parent_id), ("Doc", "document", Some(root)));
    assert_eq!((info.created_at.as_str(), info.modified_at.as_str()), (T1, T1));
    assert_eq!(a.get_children(root).unwrap(), vec![child]);
    assert_eq!(a.subtree_ids(root).unwrap(), vec![root, child]);
    assert!(a.validate_tree().is_empty());

    apply_edit(&mut b, &edit);
    assert_same(&a, &b);
    assert_eq!(b.get_children(root).unwrap(), vec![child]);
}

/// A new node copies nothing from its parent: how a tree reads is its
/// theme, looked up on the chain when it is drawn, never copied
/// (docs/THEMES_CONTRACT.md section 4, which ended the paragraphs copy of
/// docs/PARAGRAPHS_CONTRACT.md). A parent made spaced before themes
/// leaves its new children's documents without the key.
#[test]
fn a_new_node_copies_nothing_from_its_parent() {
    use pimble_core::custom_keys::{PARAGRAPHS, THEME};
    let spaced = serde_json::json!(pimble_core::SPACED_PARAGRAPHS);
    let custom = |t: &Tree, n: NodeId| t.doc(n).unwrap().fields().unwrap().custom;
    let (mut a, _) = tree();
    let (novel, chapter, notes) = (id(), id(), id());
    a.add_node(novel, None, Place::Last, "folder", "Novel", T1).unwrap();
    a.set_custom(novel, PARAGRAPHS, &spaced, T1).unwrap();
    a.set_custom(novel, THEME, &serde_json::json!("builtin:book"), T1).unwrap();
    a.add_node(chapter, Some(novel), Place::Last, "document", "Chapter", T2).unwrap();
    a.add_node_with_doc(notes, NodeDoc::new(), Some(novel), Place::Last, "document", "Notes", T2).unwrap();
    for node in [chapter, notes] {
        assert!(custom(&a, node).get(PARAGRAPHS).is_none());
        assert!(custom(&a, node).get(THEME).is_none());
    }
}

#[test]
fn add_node_at_a_place_under_a_folder() {
    let (mut a, root) = tree();
    let folder = id();
    a.add_node(folder, Some(root), Place::Last, "folder", "F", T1).unwrap();
    let (x, y, z) = (id(), id(), id());
    a.add_node(x, Some(folder), Place::Last, "document", "x", T1).unwrap();
    a.add_node(z, Some(folder), Place::Last, "document", "z", T1).unwrap();
    a.add_node(y, Some(folder), Place::After(x), "document", "y", T1).unwrap();
    assert_eq!(a.get_children(folder).unwrap(), vec![x, y, z]);
    assert_eq!(a.subtree_ids(root).unwrap(), vec![root, folder, x, y, z]);
}

#[test]
fn add_node_refuses_an_existing_id_and_a_missing_parent() {
    let (mut a, root) = tree();
    let child = id();
    a.add_node(child, None, Place::Last, "document", "Doc", T1).unwrap();
    assert!(a.add_node(child, None, Place::Last, "document", "Again", T1).is_err());
    assert!(a.add_node(id(), Some(id()), Place::Last, "document", "Orphan", T1).is_err());
    assert!(a.add_node(root, None, Place::Last, "document", "Root again", T1).is_err());
    assert_eq!(a.list_node_ids().len(), 2);
}

/// The content of a node can arrive before its place in the tree (a peer's
/// `applyEdit` on a document this store has not created yet): `add_node`
/// then initialises the document it already holds, content intact.
#[test]
fn add_node_initialises_a_held_content_only_document() {
    let (mut a, root) = tree();
    let child = id();
    let content = NodeDoc::from_plain_text("arrived first").unwrap();
    a.apply_update(child, &content.save()).unwrap();
    assert!(!a.has_node(child));
    assert!(!a.list_node_ids().contains(&child));

    let edit = a.add_node(child, None, Place::Last, "document", "Doc", T1).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([child, root]));
    assert!(a.has_node(child));
    assert_eq!(a.doc(child).unwrap().text(), "arrived first");
    assert_eq!(a.get_children(root).unwrap(), vec![child]);
}

#[test]
fn add_node_with_doc_keeps_the_content_and_sends_the_whole_document() {
    let (mut a, root) = tree();
    let mut b = replica_of(&a);
    let child = id();
    let doc = NodeDoc::from_plain_text("imported").unwrap();
    let edit = a.add_node_with_doc(child, doc, None, Place::Last, "document", "Imported", T1).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([child, root]));
    assert_eq!(a.doc(child).unwrap().text(), "imported");
    assert_eq!(a.get_node_info(child).unwrap().title, "Imported");

    apply_edit(&mut b, &edit);
    assert_same(&a, &b);
    assert_eq!(b.doc(child).unwrap().text(), "imported");

    let again = NodeDoc::from_plain_text("twice").unwrap();
    assert!(a.add_node_with_doc(child, again, None, Place::Last, "document", "Twice", T1).is_err());
}

#[test]
fn move_node_between_parents_names_three_documents() {
    let (mut a, root) = tree();
    let mut b = replica_of(&a);
    let (p, q, x) = (id(), id(), id());
    a.add_node(p, Some(root), Place::Last, "folder", "P", T1).unwrap();
    a.add_node(q, Some(root), Place::Last, "folder", "Q", T1).unwrap();
    a.add_node(x, Some(p), Place::Last, "document", "X", T1).unwrap();
    sync(&mut a, &mut b);

    let edit = a.move_node(x, q, Place::Last, T2).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([p, q, x]));
    assert!(a.get_children(p).unwrap().is_empty());
    assert_eq!(a.get_children(q).unwrap(), vec![x]);
    let info = a.get_node_info(x).unwrap();
    assert_eq!(info.parent_id, Some(q));
    assert_eq!(info.modified_at, T2);
    assert!(a.validate_tree().is_empty());

    apply_edit(&mut b, &edit);
    assert_same(&a, &b);
}

#[test]
fn move_node_within_a_parent_to_a_position() {
    let (mut a, root) = tree();
    let (x, y, z) = (id(), id(), id());
    a.add_node(x, Some(root), Place::Last, "document", "x", T1).unwrap();
    a.add_node(y, Some(root), Place::Last, "document", "y", T1).unwrap();
    a.add_node(z, Some(root), Place::Last, "document", "z", T1).unwrap();
    let edit = a.move_node(z, root, Place::First, T2).unwrap();
    // One list touched once, plus the node's own `modified_at`.
    assert_eq!(ids_of(&edit), HashSet::from([root, z]));
    assert_eq!(a.get_children(root).unwrap(), vec![z, x, y]);
    a.move_node(z, root, Place::Last, T2).unwrap();
    assert_eq!(a.get_children(root).unwrap(), vec![x, y, z]);
    assert!(a.validate_tree().is_empty());
}

/// docs/TREE_CONTRACT.md "Places on the wire": `After` is found once the
/// node has left the list, so moving down past one sibling is no
/// off-by-one, and an anchor the list no longer names means the end.
#[test]
fn move_node_after_a_sibling() {
    let (mut a, root) = tree();
    let mut b = replica_of(&a);
    let (w, x, y, z, f) = (id(), id(), id(), id(), id());
    for (n, t) in [(w, "w"), (x, "x"), (y, "y"), (z, "z")] {
        a.add_node(n, Some(root), Place::Last, "document", t, T1).unwrap();
    }
    a.add_node(f, Some(root), Place::Last, "folder", "f", T1).unwrap();
    // Down one, within the parent.
    a.move_node(w, root, Place::After(x), T2).unwrap();
    assert_eq!(a.get_children(root).unwrap(), vec![x, w, y, z, f]);
    // Up, within the parent.
    a.move_node(z, root, Place::After(x), T2).unwrap();
    assert_eq!(a.get_children(root).unwrap(), vec![x, z, w, y, f]);
    // After itself: where it is.
    a.move_node(w, root, Place::After(w), T2).unwrap();
    assert_eq!(a.get_children(root).unwrap(), vec![x, z, w, y, f]);
    // Into another parent, after a child of it; then after a node the
    // list does not name, which is the end.
    a.move_node(x, f, Place::Last, T2).unwrap();
    a.move_node(y, f, Place::First, T2).unwrap();
    a.move_node(z, f, Place::After(y), T2).unwrap();
    assert_eq!(a.get_children(f).unwrap(), vec![y, z, x]);
    a.move_node(w, f, Place::After(root), T2).unwrap();
    assert_eq!(a.get_children(f).unwrap(), vec![y, z, x, w]);
    assert!(a.validate_tree().is_empty());
    sync(&mut a, &mut b);
    assert_same(&a, &b);
}

#[test]
fn place_on_the_wire() {
    let n = id();
    assert_eq!(serde_json::to_value(Place::First).unwrap(), serde_json::json!("first"));
    assert_eq!(serde_json::to_value(Place::Last).unwrap(), serde_json::json!("last"));
    assert_eq!(serde_json::to_value(Place::After(n)).unwrap(), serde_json::json!({ "after": n.to_string() }));
    assert_eq!(serde_json::from_value::<Place>(serde_json::json!({ "after": n.to_string() })).unwrap(), Place::After(n));
}

#[test]
fn move_node_refuses_the_root_and_a_cycle_by_request() {
    let (mut a, root) = tree();
    let (p, x) = (id(), id());
    a.add_node(p, Some(root), Place::Last, "folder", "P", T1).unwrap();
    a.add_node(x, Some(p), Place::Last, "document", "X", T1).unwrap();
    assert!(a.move_node(root, p, Place::Last, T2).is_err());
    assert!(a.move_node(p, x, Place::Last, T2).is_err(), "under its own descendant");
    assert!(a.move_node(p, p, Place::Last, T2).is_err(), "under itself");
    assert!(a.move_node(x, id(), Place::Last, T2).is_err(), "under nothing");
    assert_eq!(a.get_node_info(p).unwrap().parent_id, Some(root));
    assert_eq!(a.get_node_info(x).unwrap().parent_id, Some(p));
}

#[test]
fn remove_node_tombstones_the_subtree_and_keeps_lists_below() {
    let (mut a, root) = tree();
    let mut b = replica_of(&a);
    let (f, x, y, other) = (id(), id(), id(), id());
    a.add_node(f, Some(root), Place::Last, "folder", "F", T1).unwrap();
    a.add_node(x, Some(f), Place::Last, "document", "x", T1).unwrap();
    a.add_node(y, Some(x), Place::Last, "document", "y", T1).unwrap();
    a.add_node(other, Some(root), Place::Last, "document", "other", T1).unwrap();
    sync(&mut a, &mut b);

    let edit = a.remove_node(f, T2).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([f, x, y, root]));
    for n in [f, x, y] {
        assert!(!a.has_node(n));
        assert!(a.get_node_info(n).is_err());
        assert!(a.get_children(n).is_err());
        assert_eq!(a.doc(n).unwrap().fields().unwrap().deleted_at.as_deref(), Some(T2));
    }
    assert_eq!(a.get_children(root).unwrap(), vec![other]);
    assert_eq!(a.list_node_ids().len(), 2);
    // The lists below the deletion are untouched; the parent's lost the entry.
    assert_eq!(a.doc(f).unwrap().children(), vec![x]);
    assert_eq!(a.doc(x).unwrap().children(), vec![y]);
    assert_eq!(a.doc(root).unwrap().children(), vec![other]);
    assert!(a.validate_tree().is_empty());
    assert!(a.remove_node(root, T2).is_err());
    assert!(a.remove_node(f, T2).is_err(), "already deleted");

    apply_edit(&mut b, &edit);
    assert_same(&a, &b);
}

#[test]
fn undelete_puts_the_subtree_back_at_the_end_of_its_parent() {
    let (mut a, root) = tree();
    let mut b = replica_of(&a);
    let (f, x, y, other) = (id(), id(), id(), id());
    a.add_node(f, Some(root), Place::Last, "folder", "F", T1).unwrap();
    a.add_node(x, Some(f), Place::Last, "document", "x", T1).unwrap();
    a.add_node(y, Some(x), Place::Last, "document", "y", T1).unwrap();
    a.add_node(other, Some(root), Place::Last, "document", "other", T1).unwrap();
    a.remove_node(f, T2).unwrap();
    sync(&mut a, &mut b);

    assert!(a.undelete_node(other, T3).is_err(), "not deleted");
    let edit = a.undelete_node(f, T3).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([f, x, y, root]));
    for n in [f, x, y] {
        assert!(a.has_node(n), "{n} should be back");
    }
    assert_eq!(a.get_children(root).unwrap(), vec![other, f]);
    assert_eq!(a.get_children(f).unwrap(), vec![x]);
    assert_eq!(a.get_children(x).unwrap(), vec![y]);
    assert_eq!(a.get_node_info(f).unwrap().modified_at, T3);
    assert!(a.validate_tree().is_empty());

    apply_edit(&mut b, &edit);
    assert_same(&a, &b);
}

#[test]
fn undelete_goes_under_the_root_when_the_parent_is_gone() {
    let (mut a, root) = tree();
    let (f, x) = (id(), id());
    a.add_node(f, Some(root), Place::Last, "folder", "F", T1).unwrap();
    a.add_node(x, Some(f), Place::Last, "document", "x", T1).unwrap();
    a.remove_node(f, T2).unwrap();

    // Undeleting the child alone: its parent is still a tombstone, so it
    // goes under the root and leaves the tombstone's list.
    let edit = a.undelete_node(x, T3).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([x, root, f]));
    assert_eq!(a.get_node_info(x).unwrap().parent_id, Some(root));
    assert_eq!(a.get_children(root).unwrap(), vec![x]);
    assert!(!a.has_node(f));
    assert!(a.doc(f).unwrap().children().is_empty());
    assert!(a.validate_tree().is_empty());

    // Undeleting the folder now brings only itself back: `x` left it.
    let edit = a.undelete_node(f, T3).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([f, root]));
    assert_eq!(a.get_children(root).unwrap(), vec![x, f]);
    assert!(a.get_children(f).unwrap().is_empty());
    assert!(a.validate_tree().is_empty());
    assert!(a.repair(T3).unwrap().is_none());
}

#[test]
fn undelete_leaves_a_child_deleted_earlier_deleted() {
    let (mut a, root) = tree();
    let (f, x, y) = (id(), id(), id());
    a.add_node(f, Some(root), Place::Last, "folder", "F", T1).unwrap();
    a.add_node(x, Some(f), Place::Last, "document", "x", T1).unwrap();
    a.add_node(y, Some(f), Place::Last, "document", "y", T1).unwrap();
    a.remove_node(x, T1).unwrap();
    a.remove_node(f, T2).unwrap();

    let edit = a.undelete_node(f, T3).unwrap();
    assert_eq!(ids_of(&edit), HashSet::from([f, y, root]));
    assert!(a.has_node(f) && a.has_node(y));
    assert!(!a.has_node(x), "deleted by an earlier deletion, stays deleted");
    assert_eq!(a.get_children(f).unwrap(), vec![y]);
    assert!(a.validate_tree().is_empty());
}

#[test]
fn metadata_edits_name_the_one_document() {
    let (mut a, root) = tree();
    let mut b = replica_of(&a);
    let x = id();
    a.add_node(x, Some(root), Place::Last, "document", "x", T1).unwrap();
    sync(&mut a, &mut b);

    let edits = [
        a.set_title(x, "Renamed", T2).unwrap(),
        a.set_tags(x, &["one".into(), "two".into()], T2).unwrap(),
        a.set_custom(x, "icon", &serde_json::json!("star"), T2).unwrap(),
        a.set_custom(x, "gone", &serde_json::json!(1), T2).unwrap(),
        a.remove_custom(x, "gone", T2).unwrap(),
        a.set_node_type(x, "folder", T2).unwrap(),
        a.touch_modified(x, T3).unwrap(),
    ];
    for edit in &edits {
        assert_eq!(edit.node_ids(), vec![x]);
        apply_edit(&mut b, edit);
    }
    let info = a.get_node_info(x).unwrap();
    assert_eq!(info.title, "Renamed");
    assert_eq!(info.tags, vec!["one", "two"]);
    assert_eq!(info.custom.get("icon"), Some(&serde_json::json!("star")));
    assert!(!info.custom.contains_key("gone"));
    assert_eq!(info.node_type, "folder");
    assert_eq!(info.modified_at, T3);
    assert_same(&a, &b);

    assert!(a.remove_custom(x, "never", T3).unwrap().is_empty(), "an absent key touches nothing");
    assert!(a.set_title(id(), "?", T3).is_err());
    a.remove_node(x, T3).unwrap();
    assert!(a.set_title(x, "?", T3).is_err(), "a tombstone is not a node");
}

#[test]
fn get_children_filters_and_dedups_the_stored_list() {
    let (mut a, root) = tree();
    let (x, y) = (id(), id());
    a.add_node(x, Some(root), Place::Last, "document", "x", T1).unwrap();
    a.add_node(y, Some(root), Place::Last, "document", "y", T1).unwrap();
    // What concurrency can leave in a list: a duplicate, an unknown id, a
    // content-only document, a tombstone, the owner itself.
    let unknown = id();
    let content_only = id();
    a.apply_update(content_only, &NodeDoc::from_plain_text("c").unwrap().save()).unwrap();
    let root_doc = a.doc_mut(root).unwrap();
    root_doc.insert_child(0, x).unwrap();
    root_doc.insert_child(99, unknown).unwrap();
    root_doc.insert_child(99, content_only).unwrap();
    root_doc.insert_child(99, root).unwrap();
    a.remove_node(y, T2).unwrap();
    root_doc_insert(&mut a, root, y);

    assert_eq!(a.get_children(root).unwrap(), vec![x]);
    assert_eq!(a.subtree_ids(root).unwrap(), vec![root, x]);
    assert!(a.get_children(unknown).is_err());
    assert!(a.get_children(content_only).is_err());
    assert!(a.list_node_ids().len() == 2);
}

fn root_doc_insert(tree: &mut Tree, owner: NodeId, child: NodeId) {
    tree.doc_mut(owner).unwrap().insert_child(99, child).unwrap();
}

#[test]
fn subtree_ids_counts_a_child_only_where_list_and_parent_agree() {
    let (mut a, root) = tree();
    let (p, q, x) = (id(), id(), id());
    a.add_node(p, Some(root), Place::Last, "folder", "P", T1).unwrap();
    a.add_node(q, Some(root), Place::Last, "folder", "Q", T1).unwrap();
    a.add_node(x, Some(p), Place::Last, "document", "x", T1).unwrap();
    // Half a move: Q lists x, but x still says P.
    a.doc_mut(q).unwrap().insert_child(0, x).unwrap();
    assert_eq!(a.subtree_ids(q).unwrap(), vec![q]);
    assert_eq!(a.subtree_ids(p).unwrap(), vec![p, x]);
    assert_eq!(a.subtree_ids(root).unwrap(), vec![root, p, x, q]);
    // The other half: x says Q, but only P lists it.
    a.doc_mut(q).unwrap().remove_child(x).unwrap();
    a.doc_mut(x).unwrap().set_parent_id(Some(q), T2).unwrap();
    assert_eq!(a.subtree_ids(p).unwrap(), vec![p]);
    assert_eq!(a.subtree_ids(q).unwrap(), vec![q]);
}

#[test]
fn apply_update_creates_an_unknown_document() {
    let (mut a, root) = tree();
    let (mut b, _) = tree();
    let x = id();
    let edit = a.add_node(x, Some(root), Place::Last, "document", "x", T1).unwrap();
    // b knows neither the root nor x; it holds both afterwards.
    let effects: Vec<_> = edit.touched.iter().map(|(id, u)| b.apply_update(*id, u).unwrap()).collect();
    assert!(effects.iter().all(|e| e.changed && e.structure && !e.content));
    assert!(b.doc(x).unwrap().is_initialised());
    assert_eq!(b.doc(root).unwrap().children(), vec![x]);
}
