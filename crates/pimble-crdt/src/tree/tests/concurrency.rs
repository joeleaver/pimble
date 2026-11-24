use super::*;

// ── Concurrency scenarios (decision 9): two trees exchanging per-document
// updates converge within two rounds of exchange-and-repair. ────────

#[test]
fn concurrent_moves_of_one_node_to_two_parents_converge() {
    let (mut a, root) = tree();
    let (fa, fb, child) = (id(), id(), id());
    a.add_node(fa, Some(root), Place::Last, "folder", "A", T1).unwrap();
    a.add_node(fb, Some(root), Place::Last, "folder", "B", T1).unwrap();
    a.add_node(child, Some(fa), Place::Last, "document", "Doc", T1).unwrap();
    let mut b = replica_of(&a);

    a.move_node(child, fb, Place::Last, T2).unwrap();
    b.move_node(child, root, Place::Last, T2).unwrap();

    sync_and_repair_round(&mut a, &mut b);
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);

    let winner = a.get_node_info(child).unwrap().parent_id.unwrap();
    assert!(winner == fb || winner == root, "unexpected winner {winner}");
    assert_eq!(a.get_children(winner).unwrap().iter().filter(|&&c| c == child).count(), 1);
    assert!(a.get_children(fa).unwrap().is_empty());
    let loser = if winner == fb { root } else { fb };
    assert!(!a.get_children(loser).unwrap().contains(&child));
}

/// Only the parent's half of an edit reached `b`: the list document, not
/// the child's own (2026-11-19's loop, a member's device lacking a
/// note's tombstone).
fn deliver_only(edit: &TreeEdit, to: &mut Tree, ids: &[NodeId]) {
    for (id, update) in &edit.touched {
        if ids.contains(id) {
            to.apply_update(*id, update).unwrap();
        }
    }
}

#[test]
fn a_device_without_the_tombstone_leaves_the_deleted_child_out_of_its_list() {
    let (mut a, root) = tree();
    let (folder, note) = (id(), id());
    a.add_node(folder, Some(root), Place::Last, "folder", "Shared", T1).unwrap();
    a.add_node(note, Some(folder), Place::Last, "document", "Blinking", T1).unwrap();
    let mut b = replica_of(&a);

    let delete = a.remove_node(note, T2).unwrap();
    deliver_only(&delete, &mut b, &[folder]);
    assert!(b.has_node(note), "b still holds the note as live");
    assert!(!b.doc(folder).unwrap().children().contains(&note));
    // b's repair puts nothing back, and a's has nothing to take out.
    if let Some(edit) = b.repair(T3).unwrap() {
        apply_edit(&mut a, &edit);
    }
    assert!(!a.doc(folder).unwrap().children().contains(&note), "the deleted note stays out of the list");
    assert!(a.repair(T3).unwrap().is_none() && b.repair(T3).unwrap().is_none(), "nothing for either to do: no loop");
    // The tombstone arrives at last: the same tree on both.
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);
    assert!(!b.has_node(note));
}

#[test]
fn a_device_without_the_move_leaves_the_moved_child_where_it_went() {
    let (mut a, root) = tree();
    let (from, to, note) = (id(), id(), id());
    a.add_node(from, Some(root), Place::Last, "folder", "From", T1).unwrap();
    a.add_node(to, Some(root), Place::Last, "folder", "To", T1).unwrap();
    a.add_node(note, Some(from), Place::Last, "document", "Note", T1).unwrap();
    let mut b = replica_of(&a);

    let moved = a.move_node(note, to, Place::Last, T2).unwrap();
    deliver_only(&moved, &mut b, &[from, to]);
    // b's copy of the note still names `from`; both lists say otherwise.
    if let Some(edit) = b.repair(T3).unwrap() {
        apply_edit(&mut a, &edit);
    }
    assert_eq!(a.doc(to).unwrap().children(), vec![note], "b took nothing out of `to`");
    assert!(a.doc(from).unwrap().children().is_empty(), "and put nothing back in `from`");
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);
    assert_eq!(b.get_node_info(note).unwrap().parent_id, Some(to));
}

#[test]
fn putting_a_child_back_clears_what_said_it_left() {
    let (mut a, root) = tree();
    let (folder, other, note) = (id(), id(), id());
    a.add_node(folder, Some(root), Place::Last, "folder", "F", T1).unwrap();
    a.add_node(other, Some(root), Place::Last, "folder", "O", T1).unwrap();
    a.add_node(note, Some(folder), Place::Last, "document", "N", T1).unwrap();
    a.remove_node(note, T2).unwrap();
    assert!(a.doc(folder).unwrap().left_children().contains(&note));
    a.undelete_node(note, T3).unwrap();
    assert!(a.doc(folder).unwrap().left_children().is_empty(), "undelete clears it");
    a.move_node(note, other, Place::Last, T3).unwrap();
    assert!(a.doc(folder).unwrap().left_children().contains(&note));
    a.move_node(note, folder, Place::Last, T3).unwrap();
    assert!(a.doc(folder).unwrap().left_children().is_empty(), "moving back clears it");
    assert!(a.doc(other).unwrap().left_children().contains(&note));
    assert!(a.repair(T3).unwrap().is_none());
}

#[test]
fn concurrent_moves_of_a_under_b_and_b_under_a_converge() {
    let (mut a, root) = tree();
    let (fa, fb) = (id(), id());
    a.add_node(fa, Some(root), Place::Last, "folder", "A", T1).unwrap();
    a.add_node(fb, Some(root), Place::Last, "folder", "B", T1).unwrap();
    let mut b = replica_of(&a);

    a.move_node(fb, fa, Place::Last, T2).unwrap();
    b.move_node(fa, fb, Place::Last, T2).unwrap();

    sync_and_repair_round(&mut a, &mut b);
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);

    let a_parent = a.get_node_info(fa).unwrap().parent_id;
    let b_parent = a.get_node_info(fb).unwrap().parent_id;
    assert!((a_parent == Some(root)) ^ (b_parent == Some(root)), "exactly one under the root: {a_parent:?}/{b_parent:?}");
}

#[test]
fn deleting_a_folder_while_the_other_side_adds_a_child_to_it_converges() {
    let (mut a, root) = tree();
    let folder = id();
    a.add_node(folder, Some(root), Place::Last, "folder", "Folder", T1).unwrap();
    let mut b = replica_of(&a);

    a.remove_node(folder, T2).unwrap();
    let new_child = id();
    b.add_node(new_child, Some(folder), Place::Last, "document", "New", T2).unwrap();

    sync_and_repair_round(&mut a, &mut b);
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);

    // The folder stays deleted; its would-be child survives under the root
    // rather than being lost (decision 10).
    assert!(!a.has_node(folder));
    assert!(a.has_node(new_child));
    assert_eq!(a.get_node_info(new_child).unwrap().parent_id, Some(root));
    assert!(a.get_children(root).unwrap().contains(&new_child));
}

#[test]
fn deleting_a_node_while_the_other_side_moves_it_converges() {
    let (mut a, root) = tree();
    let (fa, fb, child) = (id(), id(), id());
    a.add_node(fa, Some(root), Place::Last, "folder", "A", T1).unwrap();
    a.add_node(fb, Some(root), Place::Last, "folder", "B", T1).unwrap();
    a.add_node(child, Some(fa), Place::Last, "document", "Doc", T1).unwrap();
    let mut b = replica_of(&a);

    a.remove_node(child, T2).unwrap();
    b.move_node(child, fb, Place::Last, T2).unwrap();

    sync_and_repair_round(&mut a, &mut b);
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);

    // The delete wins: a tombstone on both sides, and the list entry the
    // move left behind is cleaned up rather than left dangling.
    assert!(!a.has_node(child));
    assert!(!b.has_node(child));
    assert!(!a.get_children(fb).unwrap().contains(&child));
    assert!(!a.doc(fb).unwrap().children().contains(&child));
}

#[test]
fn both_sides_repairing_the_same_merged_state_concurrently_converge() {
    let (mut a, root) = tree();
    let (fa, fb, child) = (id(), id(), id());
    a.add_node(fa, Some(root), Place::Last, "folder", "A", T1).unwrap();
    a.add_node(fb, Some(root), Place::Last, "folder", "B", T1).unwrap();
    a.add_node(child, Some(fa), Place::Last, "document", "Doc", T1).unwrap();
    let mut b = replica_of(&a);

    // Every kind of damage at once, then both repair the same held state
    // before either sees the other's repair.
    a.move_node(child, fb, Place::Last, T2).unwrap();
    b.move_node(child, root, Place::Last, T2).unwrap();
    a.doc_mut(fa).unwrap().set_parent_id(Some(fb), T2).unwrap();
    b.doc_mut(fb).unwrap().set_parent_id(Some(fa), T2).unwrap();
    sync(&mut a, &mut b);
    assert_same(&a, &b);
    let ra = a.repair(T3).unwrap().expect("damage on a");
    let rb = b.repair(T3).unwrap().expect("damage on b");
    // Deterministic in the held state: the same documents, the same deletions.
    assert_eq!(ids_of(&ra), ids_of(&rb));
    apply_edit(&mut b, &ra);
    apply_edit(&mut a, &rb);
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);
}

// ── Tombstones under concurrency ────────────────────────────────────

#[test]
fn a_rename_concurrent_with_a_delete_lands_in_the_tombstoned_document() {
    let (mut a, root) = tree();
    let x = id();
    a.add_node(x, Some(root), Place::Last, "document", "Old", T1).unwrap();
    let mut b = replica_of(&a);

    a.remove_node(x, T2).unwrap();
    b.set_title(x, "New", T2).unwrap();

    sync_and_repair_round(&mut a, &mut b);
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);
    assert!(!a.has_node(x) && !b.has_node(x), "the node stays deleted");
    assert_eq!(a.doc(x).unwrap().fields().unwrap().title, "New", "the rename is in its document");
    assert!(a.get_children(root).unwrap().is_empty());
}

#[test]
fn undelete_after_a_concurrent_move_puts_the_node_where_it_was_moved() {
    let (mut a, root) = tree();
    let (p, q, x) = (id(), id(), id());
    a.add_node(p, Some(root), Place::Last, "folder", "P", T1).unwrap();
    a.add_node(q, Some(root), Place::Last, "folder", "Q", T1).unwrap();
    a.add_node(x, Some(p), Place::Last, "document", "x", T1).unwrap();
    let mut b = replica_of(&a);

    a.remove_node(x, T2).unwrap();
    b.move_node(x, q, Place::Last, T2).unwrap();
    sync_and_repair_round(&mut a, &mut b);
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);
    assert!(!a.has_node(x));

    let edit = b.undelete_node(x, T3).unwrap();
    apply_edit(&mut a, &edit);
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);
    assert!(a.has_node(x));
    assert_eq!(a.get_node_info(x).unwrap().parent_id, Some(q));
    assert_eq!(a.get_children(q).unwrap(), vec![x]);
    assert!(a.get_children(p).unwrap().is_empty());
}
