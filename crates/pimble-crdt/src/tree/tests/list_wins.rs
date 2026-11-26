use super::*;

// ── The list wins (docs/MOVE_CONTRACT.md "Repair") ──────────────────

fn leaves_share_issue(issues: &[TreeIssue], list: NodeId, node: NodeId, stored: Option<NodeId>) -> bool {
    issues.iter().any(|i| matches!(i, TreeIssue::LeavesShare { parent_id, child_id, stored_parent } if *parent_id == list && *child_id == node && *stored_parent == stored))
}

#[test]
fn a_list_that_still_names_a_node_wins_over_a_parent_id_pointing_out_of_its_share() {
    let mut s = shared();
    tamper_parent(&mut s.tree, s.x, Some(s.private));

    let issues = s.tree.validate_tree();
    assert!(leaves_share_issue(&issues, s.a, s.x, Some(s.private)), "{issues:?}");
    assert_eq!(issues.len(), 1, "and nothing else: the list is right, so it is not touched: {issues:?}");
    let repair = s.tree.repair(T3).unwrap().expect("the parent_id needs rewriting");
    assert_eq!(repair.node_ids(), vec![s.x], "the node's own document, and no list");
    assert_eq!(s.tree.get_node_info(s.x).unwrap().parent_id, Some(s.a));
    assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.x, s.r2]);
    assert_eq!(s.tree.get_children(s.private).unwrap(), vec![s.own], "it never showed up outside");
    assert_eq!(s.tree.get_node_info(s.x).unwrap().modified_at, T1, "repair never touches a timestamp");
    assert!(s.tree.validate_tree().is_empty());
    assert!(s.tree.repair(T3).unwrap().is_none());

    // Out of the inner share alone is out of a share too.
    tamper_parent(&mut s.tree, s.y, Some(s.b));
    assert!(leaves_share_issue(&s.tree.validate_tree(), s.r2, s.y, Some(s.b)));
    s.tree.repair(T3).unwrap().unwrap();
    assert_eq!(s.tree.get_children(s.r2).unwrap(), vec![s.y]);
    assert!(s.tree.get_children(s.b).unwrap().is_empty());
    assert!(s.tree.validate_tree().is_empty());
}

#[test]
fn both_devices_decide_the_same_about_a_tampered_parent_id() {
    let mut s = shared();
    let mut other = replica_of(&s.tree);
    let mut tamperer = replica_of(&s.tree);
    let tampering = tamper_parent(&mut tamperer, s.x, Some(s.private));
    apply_edit(&mut s.tree, &tampering);
    apply_edit(&mut other, &tampering);

    // Each repairs what it holds before it hears from the other.
    let (ours, theirs) = (s.tree.repair(T3).unwrap().unwrap(), other.repair(T3).unwrap().unwrap());
    assert_eq!(ours.node_ids(), vec![s.x]);
    assert_eq!(theirs.node_ids(), vec![s.x]);
    apply_edit(&mut other, &ours);
    apply_edit(&mut s.tree, &theirs);
    sync_and_repair_round(&mut s.tree, &mut other);
    assert_converged(&mut s.tree, &mut other);
    assert_eq!(other.get_node_info(s.x).unwrap().parent_id, Some(s.a));
    assert_eq!(other.get_children(s.a).unwrap(), vec![s.x, s.r2]);

    // The tamperer's own device, once it runs an honest repair, agrees.
    sync_and_repair_round(&mut s.tree, &mut tamperer);
    sync_and_repair_round(&mut s.tree, &mut tamperer);
    assert_converged(&mut s.tree, &mut tamperer);
    assert_eq!(tamperer.get_children(s.private).unwrap(), vec![s.own]);
}

/// The same hole by another door: a `parent_id` that names a tombstone,
/// the node itself, or nothing sends a node under the root, which is as
/// far out of the share as any folder.
#[test]
fn a_parent_id_that_would_send_a_listed_node_to_the_root_loses_to_the_list_too() {
    let mut s = shared();
    let dead = note(&mut s.tree, s.b, "Deleted");
    s.tree.remove_node(dead, T2).unwrap();
    for stored in [Some(dead), Some(s.x), None] {
        tamper_parent(&mut s.tree, s.x, stored);
        let issues = s.tree.validate_tree();
        assert!(leaves_share_issue(&issues, s.a, s.x, stored), "{stored:?}: {issues:?}");
        assert_eq!(issues.len(), 1, "reported instead of the orphan or detached node it would be: {issues:?}");
        let repair = s.tree.repair(T3).unwrap().unwrap();
        assert_eq!(repair.node_ids(), vec![s.x]);
        assert_eq!(s.tree.get_node_info(s.x).unwrap().parent_id, Some(s.a));
        assert_eq!(s.tree.get_children(s.root).unwrap(), vec![s.private, s.r0]);
        assert!(s.tree.validate_tree().is_empty());
    }
    // Outside every share the orphan goes under the root, as ever.
    tamper_parent(&mut s.tree, s.own, Some(dead));
    assert!(s.tree.validate_tree().iter().any(|i| matches!(i, TreeIssue::OrphanNode { node_id, .. } if *node_id == s.own)));
    s.tree.repair(T3).unwrap().unwrap();
    assert_eq!(s.tree.get_node_info(s.own).unwrap().parent_id, Some(s.root));
}

#[test]
fn a_plain_move_in_flight_inside_one_share_is_still_completed() {
    // The node's document arrived; neither list's did.
    let mut s = shared();
    tamper_parent(&mut s.tree, s.x, Some(s.b));
    let issues = s.tree.validate_tree();
    assert!(issues.iter().any(|i| matches!(i, TreeIssue::WrongList { parent_id, child_id, effective_parent } if *parent_id == s.a && *child_id == s.x && *effective_parent == s.b)), "{issues:?}");
    assert!(issues.iter().any(|i| matches!(i, TreeIssue::MissingFromList { parent_id, child_id } if *parent_id == s.b && *child_id == s.x)), "{issues:?}");
    let repair = s.tree.repair(T3).unwrap().unwrap();
    assert_eq!(ids_of(&repair), HashSet::from([s.a, s.b]));
    assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.x]);
    assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.r2]);
    assert!(s.tree.validate_tree().is_empty());

    // Into the share inside it, and into a share from outside: entering.
    tamper_parent(&mut s.tree, s.x, Some(s.r2));
    tamper_parent(&mut s.tree, s.own, Some(s.b));
    s.tree.repair(T3).unwrap().unwrap();
    assert_eq!(s.tree.get_children(s.r2).unwrap(), vec![s.y, s.x]);
    assert_eq!(s.tree.get_children(s.b).unwrap(), vec![s.own]);
    assert!(s.tree.get_children(s.private).unwrap().is_empty());
    assert!(s.tree.validate_tree().is_empty());

    // And as two devices see it: the mover's three updates arrive at the
    // other one document at a time, a repair after each.
    let mut s = shared();
    let mut other = replica_of(&s.tree);
    let moved = s.tree.move_node(s.x, s.b, Place::Last, T2).unwrap();
    for (doc_id, update) in moved.touched.iter().rev() {
        other.apply_update(*doc_id, update).unwrap();
        if let Some(repair) = other.repair(T3).unwrap() {
            apply_edit(&mut s.tree, &repair);
        }
    }
    sync_and_repair_round(&mut s.tree, &mut other);
    assert_converged(&mut s.tree, &mut other);
    assert_eq!(other.get_children(s.b).unwrap(), vec![s.x]);
}

#[test]
fn a_node_named_by_no_list_is_adopted_where_its_parent_id_says() {
    // What remains (the contract says so): a client that also unlists
    // the node. No list names it, so there is nothing to win; it is
    // adopted as before, still in the share's scope, and any editor can
    // put it back.
    let mut s = shared();
    tamper_parent(&mut s.tree, s.x, Some(s.private));
    s.tree.doc_mut(s.a).unwrap().remove_child(s.x).unwrap();
    let issues = s.tree.validate_tree();
    assert!(issues.iter().all(|i| matches!(i, TreeIssue::MissingFromList { parent_id, child_id } if *parent_id == s.private && *child_id == s.x)), "{issues:?}");
    let repair = s.tree.repair(T3).unwrap().unwrap();
    assert_eq!(repair.node_ids(), vec![s.private]);
    assert_eq!(s.tree.get_children(s.private).unwrap(), vec![s.own, s.x]);
    assert!(s.tree.validate_tree().is_empty());
    // Putting it back is a plain move: it enters the share.
    let (back, _) = s.tree.move_or_transplant(HERE, s.x, s.a, Place::Last, T3, &mut never_asked).unwrap();
    assert_eq!(back, s.x);
}

/// The 2026-09-21 rule, for the exception too: whether the destination is
/// outside the share is only known when everything above it is held.
#[test]
fn a_destination_whose_ancestors_are_not_all_held_is_not_judged() {
    for lands_inside in [true, false] {
        let mut s = shared();
        let mut holds_all = replica_of(&s.tree);
        // Someone made a folder and a folder in it, and the node says it
        // is in the inner one. Here: the inner folder, and not the outer.
        let outer = id();
        let made_outer = holds_all.add_node(outer, Some(if lands_inside { s.b } else { s.private }), Place::Last, "folder", "Outer", T2).unwrap();
        let inner = id();
        let made_inner = holds_all.add_node(inner, Some(outer), Place::Last, "folder", "Inner", T2).unwrap();
        let moved = tamper_parent(&mut holds_all, s.x, Some(inner));
        for (doc_id, update) in made_inner.touched.iter().chain(&moved.touched) {
            if *doc_id != outer {
                s.tree.apply_update(*doc_id, update).unwrap();
            }
        }
        assert!(s.tree.validate_tree().is_empty(), "{:?}", s.tree.validate_tree());
        assert!(s.tree.repair(T3).unwrap().is_none(), "not known, so not decided");
        assert_eq!(s.tree.doc(s.a).unwrap().children(), vec![s.x, s.r2], "the list still names it");
        assert_eq!(s.tree.get_node_info(s.x).unwrap().parent_id, Some(inner), "and its parent_id is as it came");

        // The outer folder arrives, and with it the answer.
        apply_edit(&mut s.tree, &made_outer);
        apply_edit(&mut s.tree, &made_inner);
        s.tree.repair(T3).unwrap().expect("now it is known");
        assert!(s.tree.validate_tree().is_empty());
        if lands_inside {
            assert_eq!(s.tree.get_children(inner).unwrap(), vec![s.x], "inside the share: a move, completed");
            assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.r2]);
        } else {
            assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.x, s.r2], "outside: the list wins");
            assert!(s.tree.get_children(inner).unwrap().is_empty());
        }
        // The device that held everything all along decides the same.
        holds_all.repair(T3).unwrap().unwrap();
        sync_and_repair_round(&mut s.tree, &mut holds_all);
        assert_converged(&mut s.tree, &mut holds_all);
    }
}

/// A scope is repaired as a tree of its own, rooted at the share's root,
/// whose `parent_id` names a document a member never holds. The root is
/// the top: what is under it is known to be in it.
#[test]
fn in_a_scopes_tree_the_root_is_the_top() {
    let s = shared();
    let held = [s.r0, s.a, s.b, s.x, s.r2, s.y];
    let docs = held.iter().map(|&n| (n, NodeDoc::load(&s.tree.doc(n).unwrap().save()).unwrap())).collect();
    let mut scope = Tree::from_docs(s.r0, docs);
    assert!(scope.validate_tree().is_empty());
    // Out of the inner share, inside the outer: known, so the list wins.
    tamper_parent(&mut scope, s.y, Some(s.b));
    assert!(leaves_share_issue(&scope.validate_tree(), s.r2, s.y, Some(s.b)));
    scope.repair(T3).unwrap().unwrap();
    assert_eq!(scope.get_children(s.r2).unwrap(), vec![s.y]);
    // Inside the outer share: a move, completed.
    tamper_parent(&mut scope, s.x, Some(s.b));
    scope.repair(T3).unwrap().unwrap();
    assert_eq!(scope.get_children(s.b).unwrap(), vec![s.x]);
    // Out of everything this device holds: not held, not judged.
    tamper_parent(&mut scope, s.x, Some(s.private));
    assert!(scope.validate_tree().is_empty());
    assert!(scope.repair(T3).unwrap().is_none());
    assert_eq!(scope.doc(s.b).unwrap().children(), vec![s.x]);
}

#[test]
fn of_two_lists_the_one_that_keeps_the_node_in_more_shares_wins() {
    // Both `a` (in r0) and `r2` (in r2 and r0) name the node; its
    // parent_id points outside both.
    let mut s = shared();
    s.tree.doc_mut(s.a).unwrap().insert_child(99, s.y).unwrap();
    tamper_parent(&mut s.tree, s.y, Some(s.private));
    assert!(leaves_share_issue(&s.tree.validate_tree(), s.r2, s.y, Some(s.private)));
    let repair = s.tree.repair(T3).unwrap().unwrap();
    assert_eq!(ids_of(&repair), HashSet::from([s.y, s.a]), "the parent_id, and the other list's entry");
    assert_eq!(s.tree.get_children(s.r2).unwrap(), vec![s.y]);
    assert_eq!(s.tree.get_children(s.a).unwrap(), vec![s.x, s.r2]);
    assert!(s.tree.validate_tree().is_empty());

    // Two shares side by side: the first list in id order, on every device.
    let (mut t, root) = tree();
    let (r1, r3) = (folder(&mut t, root, "One"), folder(&mut t, root, "Other"));
    let elsewhere = folder(&mut t, root, "Elsewhere");
    let n = note(&mut t, r1, "N");
    share(&mut t, r1);
    share(&mut t, r3);
    t.doc_mut(r3).unwrap().insert_child(0, n).unwrap();
    tamper_parent(&mut t, n, Some(elsewhere));
    let mut other = replica_of(&t);
    t.repair(T3).unwrap().unwrap();
    other.repair(T3).unwrap().unwrap();
    let winner = if r1.to_string() < r3.to_string() { r1 } else { r3 };
    assert_eq!(t.get_node_info(n).unwrap().parent_id, Some(winner));
    assert_eq!(other.get_node_info(n).unwrap().parent_id, Some(winner));
    sync_and_repair_round(&mut t, &mut other);
    assert_converged(&mut t, &mut other);
}

#[test]
fn a_list_under_the_node_itself_claims_nothing() {
    // `a`'s parent_id points out of the share, and the only list that
    // names `a` is one under it. Making that list its parent would close
    // a cycle, so there is nothing to win: the move is completed.
    let mut s = shared();
    s.tree.doc_mut(s.r0).unwrap().remove_child(s.a).unwrap();
    s.tree.doc_mut(s.r2).unwrap().insert_child(0, s.a).unwrap();
    tamper_parent(&mut s.tree, s.a, Some(s.private));
    let issues = s.tree.validate_tree();
    assert!(!issues.iter().any(|i| matches!(i, TreeIssue::LeavesShare { .. } | TreeIssue::Cycle { .. })), "{issues:?}");
    s.tree.repair(T3).unwrap().unwrap();
    assert_eq!(s.tree.get_children(s.private).unwrap(), vec![s.own, s.a]);
    assert_eq!(s.tree.get_children(s.r2).unwrap(), vec![s.y]);
    assert!(s.tree.validate_tree().is_empty());
}
