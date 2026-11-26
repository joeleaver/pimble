use super::*;

// ── `repair` / `validate_tree` (decision 9 over documents) ──────────

#[test]
fn repair_fixes_a_self_referential_parent() {
    let (mut a, root) = tree();
    let child = id();
    a.add_node(child, Some(root), Place::Last, "document", "Doc", T1).unwrap();
    a.doc_mut(child).unwrap().set_parent_id(Some(child), T1).unwrap();

    let issues = a.validate_tree();
    assert!(
        issues.iter().any(|i| matches!(i, TreeIssue::OrphanNode { node_id, missing_parent } if *node_id == child && *missing_parent == child)),
        "expected an OrphanNode issue for the self-reference, got {issues:?}"
    );
    let repair = a.repair(T2).unwrap().expect("a self-referential parent needs a repair");
    assert_eq!(repair.node_ids(), vec![child]);
    assert!(a.validate_tree().is_empty());
    assert!(a.repair(T2).unwrap().is_none(), "repair should be idempotent");
    assert_eq!(a.get_node_info(child).unwrap().parent_id, Some(root));
    assert_eq!(a.get_children(root).unwrap(), vec![child]);
    // Repair never touches timestamps.
    assert_eq!(a.get_node_info(child).unwrap().modified_at, T1);
}

/// A parent this device does not hold is unknown, not missing: documents
/// travel one by one, and a device may lack the key of one it was sent.
/// Re-parenting the node here would move it out of a folder every other
/// device can see it in.
#[test]
fn a_node_under_a_document_not_held_is_left_where_it_says_it_is() {
    let (mut a, root) = tree();
    let (child, folder) = (id(), id());
    a.add_node(child, Some(root), Place::Last, "document", "Doc", T1).unwrap();
    // Someone moved it into a folder whose document is not here (yet).
    a.doc_mut(child).unwrap().set_parent_id(Some(folder), T1).unwrap();
    assert!(a.validate_tree().is_empty(), "{:?}", a.validate_tree());
    assert!(a.repair(T2).unwrap().is_none(), "nothing known is wrong");
    assert_eq!(a.get_node_info(child).unwrap().parent_id, Some(folder));

    // The folder arrives, listing nothing (the move's other half is
    // still on its way): now everything is known and repair finishes it.
    let mut doc = NodeDoc::new();
    doc.init("folder", "Folder", Some(root), T1).unwrap();
    a.apply_update(folder, &doc.save()).unwrap();
    a.repair(T2).unwrap().expect("the folder is here: the node belongs in its list");
    assert!(a.validate_tree().is_empty());
    assert_eq!(a.get_children(folder).unwrap(), vec![child]);
    assert_eq!(a.get_children(root).unwrap(), vec![folder]);
}

/// The livelock found on 2026-09-21: a member made a note in a shared
/// folder; the owner's page held the folder and not the note (no key for
/// it yet), unlisted it as "missing", every device that did hold it
/// listed it again, six times a second, for as long as the page was open.
#[test]
fn a_listed_child_whose_document_is_not_held_stays_listed() {
    let (mut holds_all, root) = tree();
    let (folder, note) = (id(), id());
    holds_all.add_node(folder, Some(root), Place::Last, "folder", "Shared", T1).unwrap();
    let mut lacks_note = replica_of(&holds_all);
    let made = holds_all.add_node(note, Some(folder), Place::Last, "document", "A member's note", T2).unwrap();

    // The other device is sent the folder's new list, and not the note.
    for (doc_id, update) in &made.touched {
        if *doc_id != note {
            lacks_note.apply_update(*doc_id, update).unwrap();
        }
    }
    assert_eq!(lacks_note.doc(folder).unwrap().children(), vec![note]);
    assert!(lacks_note.validate_tree().is_empty(), "{:?}", lacks_note.validate_tree());
    assert!(lacks_note.repair(T3).unwrap().is_none(), "what is not held is not judged");
    assert!(lacks_note.get_children(folder).unwrap().is_empty(), "and is not shown either");
    assert!(holds_all.repair(T3).unwrap().is_none(), "so the device that holds it has nothing to put back");

    // The note arrives (its key did): both agree, and nothing was written.
    for (doc_id, update) in &made.touched {
        lacks_note.apply_update(*doc_id, update).unwrap();
    }
    assert!(lacks_note.repair(T3).unwrap().is_none());
    assert_eq!(lacks_note.get_children(folder).unwrap(), vec![note]);
}

#[test]
fn repair_fixes_a_detached_node() {
    let (mut a, root) = tree();
    let stray = id();
    let mut doc = NodeDoc::new();
    doc.init("document", "Stray", None, T1).unwrap();
    a.insert_doc(stray, doc);
    let issues = a.validate_tree();
    assert!(issues.iter().any(|i| matches!(i, TreeIssue::DetachedNode { node_id } if *node_id == stray)), "{issues:?}");
    let repair = a.repair(T2).unwrap().expect("a detached node needs a repair");
    assert_eq!(ids_of(&repair), HashSet::from([stray, root]));
    assert!(a.validate_tree().is_empty());
    assert_eq!(a.get_node_info(stray).unwrap().parent_id, Some(root));
    assert_eq!(a.get_children(root).unwrap(), vec![stray]);
}

#[test]
fn repair_fixes_a_duplicate_within_one_list() {
    let (mut a, root) = tree();
    let child = id();
    a.add_node(child, Some(root), Place::Last, "document", "Doc", T1).unwrap();
    a.doc_mut(root).unwrap().insert_child(99, child).unwrap();
    assert!(a.validate_tree().iter().any(|i| matches!(i, TreeIssue::DuplicateInList { .. })));
    let repair = a.repair(T2).unwrap().expect("a duplicate entry needs a repair");
    assert_eq!(repair.node_ids(), vec![root]);
    assert!(a.validate_tree().is_empty());
    assert_eq!(a.doc(root).unwrap().children(), vec![child]);
}

#[test]
fn repair_fixes_a_child_in_the_wrong_list() {
    let (mut a, root) = tree();
    let (fa, fb, child) = (id(), id(), id());
    a.add_node(fa, Some(root), Place::Last, "folder", "A", T1).unwrap();
    a.add_node(fb, Some(root), Place::Last, "folder", "B", T1).unwrap();
    a.add_node(child, Some(fa), Place::Last, "document", "Doc", T1).unwrap();
    a.doc_mut(fb).unwrap().insert_child(0, child).unwrap();
    assert!(a.validate_tree().iter().any(|i| matches!(i, TreeIssue::WrongList { .. })));
    let repair = a.repair(T2).unwrap().expect("a wrong-list entry needs a repair");
    assert_eq!(repair.node_ids(), vec![fb]);
    assert!(a.validate_tree().is_empty());
    assert_eq!(a.get_children(fa).unwrap(), vec![child]);
    assert!(a.doc(fb).unwrap().children().is_empty());
}

#[test]
fn repair_fixes_the_root_appearing_in_a_list() {
    let (mut a, root) = tree();
    let folder = id();
    a.add_node(folder, Some(root), Place::Last, "folder", "Folder", T1).unwrap();
    a.doc_mut(folder).unwrap().insert_child(0, root).unwrap();
    assert!(a.validate_tree().iter().any(|i| matches!(i, TreeIssue::RootInList { .. })));
    a.repair(T2).unwrap().expect("root-in-list needs a repair");
    assert!(a.validate_tree().is_empty());
    assert!(a.doc(folder).unwrap().children().is_empty());
}

#[test]
fn repair_fixes_a_two_node_cycle() {
    let (mut a, root) = tree();
    let (fa, fb) = (id(), id());
    a.add_node(fa, Some(root), Place::Last, "folder", "A", T1).unwrap();
    a.add_node(fb, Some(root), Place::Last, "folder", "B", T1).unwrap();
    a.doc_mut(fa).unwrap().set_parent_id(Some(fb), T1).unwrap();
    a.doc_mut(fb).unwrap().set_parent_id(Some(fa), T1).unwrap();
    let issues = a.validate_tree();
    assert!(issues.iter().any(|i| matches!(i, TreeIssue::Cycle { .. })), "expected a Cycle issue, got {issues:?}");

    let repair = a.repair(T2).unwrap().expect("a cycle needs a repair");
    assert!(!repair.is_empty());
    assert!(a.validate_tree().is_empty());
    assert!(a.repair(T2).unwrap().is_none(), "repair should be idempotent");

    let (top, bottom) = if a.get_node_info(fa).unwrap().parent_id == Some(root) { (fa, fb) } else { (fb, fa) };
    assert_eq!(top.to_string(), fa.to_string().min(fb.to_string()), "the smallest id goes under the root");
    assert_eq!(a.get_node_info(bottom).unwrap().parent_id, Some(top));
    assert_eq!(a.get_children(root).unwrap(), vec![top]);
    assert_eq!(a.get_children(top).unwrap(), vec![bottom]);
    assert!(a.get_children(bottom).unwrap().is_empty());
}

/// The document-specific cases: a tombstone is known to be deleted and
/// leaves the lists; a document that is not a node YET (content only:
/// its `node` root has not arrived) is unknown and stays listed, unshown.
#[test]
fn repair_removes_entries_naming_tombstones_and_leaves_documents_that_are_not_nodes_yet() {
    let (mut a, root) = tree();
    let (folder, dead, content_only) = (id(), id(), id());
    a.add_node(folder, Some(root), Place::Last, "folder", "F", T1).unwrap();
    a.add_node(dead, Some(folder), Place::Last, "document", "dead", T1).unwrap();
    a.doc_mut(dead).unwrap().set_deleted(Some(T2)).unwrap(); // a tombstone still listed by its parent
    a.apply_update(content_only, &NodeDoc::from_plain_text("c").unwrap().save()).unwrap();
    a.doc_mut(folder).unwrap().insert_child(99, content_only).unwrap();
    // A tombstone whose list names a live node: the list is left alone, the
    // node is an orphan (its parent is not a node) and goes under the root.
    let orphan = id();
    a.add_node(orphan, Some(folder), Place::Last, "document", "orphan", T1).unwrap();
    a.doc_mut(orphan).unwrap().set_parent_id(Some(dead), T1).unwrap();
    a.doc_mut(dead).unwrap().insert_child(0, orphan).unwrap();

    let issues = a.validate_tree();
    assert!(issues.iter().any(|i| matches!(i, TreeIssue::MissingChild { child_id, .. } if *child_id == dead)), "{issues:?}");
    assert!(!issues.iter().any(|i| matches!(i, TreeIssue::MissingChild { child_id, .. } if *child_id == content_only)), "{issues:?}");
    assert!(issues.iter().any(|i| matches!(i, TreeIssue::OrphanNode { node_id, missing_parent } if *node_id == orphan && *missing_parent == dead)), "{issues:?}");

    let repair = a.repair(T3).unwrap().expect("needs a repair");
    assert_eq!(ids_of(&repair), HashSet::from([folder, orphan, root]));
    assert!(a.validate_tree().is_empty());
    assert!(a.repair(T3).unwrap().is_none());
    assert_eq!(a.doc(folder).unwrap().children(), vec![content_only], "the tombstone left the list; the document that is no node yet stayed");
    assert!(a.get_children(folder).unwrap().is_empty(), "and is not shown");
    assert_eq!(a.get_node_info(orphan).unwrap().parent_id, Some(root));
    assert_eq!(a.get_children(root).unwrap(), vec![folder, orphan]);
    assert_eq!(a.doc(dead).unwrap().children(), vec![orphan], "a tombstone's list is not touched");
    assert!(!a.has_node(dead) && !a.has_node(content_only));
}

#[test]
fn repair_is_a_no_op_on_a_well_formed_tree_and_on_a_tree_without_a_root_yet() {
    let (mut a, root) = tree();
    a.add_node(id(), Some(root), Place::Last, "document", "Doc", T1).unwrap();
    assert!(a.repair(T2).unwrap().is_none());

    let mut fresh = Tree::from_docs(root, HashMap::new());
    assert!(fresh.validate_tree().is_empty());
    assert!(fresh.repair(T2).unwrap().is_none());
    assert!(!fresh.has_node(root));
    assert!(fresh.get_children(root).is_err());
}
