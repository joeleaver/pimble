use super::*;

// ── Shares (docs/MOVE_CONTRACT.md "What 'out of a share' means") ────

#[test]
fn shares_are_the_markers_among_a_node_and_its_ancestors_nearest_first() {
    let s = shared();
    assert_eq!(s.tree.shares(s.y), vec![s.r2, s.r0]);
    assert_eq!(s.tree.shares(s.r2), vec![s.r2, s.r0], "a share's root is in its own share");
    assert_eq!(s.tree.shares(s.x), vec![s.r0]);
    assert_eq!(s.tree.shares(s.r0), vec![s.r0]);
    assert!(s.tree.shares(s.own).is_empty());
    assert!(s.tree.shares(s.root).is_empty());
    assert!(s.tree.shares(id()).is_empty(), "a document not held is in nothing known");
}

#[test]
fn a_deleted_document_stays_in_its_share() {
    let mut s = shared();
    s.tree.remove_node(s.a, T2).unwrap();
    assert_eq!(s.tree.shares(s.y), vec![s.r2, s.r0], "tombstones are walked through, and a tombstoned root still marks its share");
}

#[test]
fn a_move_inside_a_share_leaves_none_and_a_move_out_leaves_it() {
    let s = shared();
    assert!(!s.tree.leaves_a_share(s.x, s.b), "inside r0");
    assert!(!s.tree.leaves_a_share(s.x, s.r0));
    assert!(!s.tree.leaves_a_share(s.x, s.a), "a reorder under its own parent");
    assert!(s.tree.leaves_a_share(s.x, s.private));
    assert_eq!(s.tree.shares_left(s.x, s.private), vec![s.r0]);
    assert!(s.tree.leaves_a_share(s.x, s.root));
    // Out of the inner share alone, and out of both.
    assert_eq!(s.tree.shares_left(s.y, s.b), vec![s.r2]);
    assert_eq!(s.tree.shares_left(s.y, s.private), vec![s.r2, s.r0]);
    assert!(!s.tree.leaves_a_share(s.own, s.root), "a node in no share leaves none");
}

#[test]
fn entering_a_share_is_not_leaving_one() {
    let s = shared();
    assert!(!s.tree.leaves_a_share(s.own, s.a));
    assert!(!s.tree.leaves_a_share(s.own, s.r2));
    assert!(!s.tree.leaves_a_share(s.x, s.r2), "deeper into a share inside the one it is in");
    assert!(s.tree.shares_left(s.own, s.r2).is_empty());
}

#[test]
fn a_shares_own_root_travels_with_its_share() {
    let s = shared();
    assert!(!s.tree.leaves_a_share(s.r0, s.private), "r0 is in no share but its own");
    assert!(!s.tree.leaves_a_share(s.r2, s.b), "r2 stays inside r0");
    // The consequence the contract names: a share inside a share cannot
    // leave the outer one as itself.
    assert_eq!(s.tree.shares_left(s.r2, s.private), vec![s.r0]);
}

#[test]
fn a_share_inside_the_moved_subtree_does_not_count() {
    let mut s = shared();
    assert!(!s.tree.leaves_a_share(s.a, s.b), "a holds r2 and stays in r0");
    assert_eq!(s.tree.shares_left(s.a, s.private), vec![s.r0], "r0 is left; r2 is under a and travels with it");
    // A private folder that holds a share leaves nothing wherever it goes.
    let holder = folder(&mut s.tree, s.root, "Holder");
    let r3 = folder(&mut s.tree, holder, "Held");
    share(&mut s.tree, r3);
    assert!(!s.tree.leaves_a_share(holder, s.private));
    assert!(!s.tree.leaves_a_share(holder, s.b), "into r0: entering");
}

/// A member's device holds the shares and nothing above them. It judges
/// with what it holds: two shares of one store are two shares, and
/// whatever is not held is outside everything known.
#[test]
fn a_parent_not_held_ends_the_walk() {
    let (mut full, root) = tree();
    let r1 = folder(&mut full, root, "Trips");
    let in_r1 = note(&mut full, r1, "Lisbon");
    let r2 = folder(&mut full, root, "Recipes");
    let sub = folder(&mut full, r2, "Soups");
    let in_r2 = note(&mut full, sub, "Leek");
    share(&mut full, r1);
    share(&mut full, r2);

    let held = [r1, in_r1, r2, sub, in_r2];
    let docs = held.iter().map(|&n| (n, NodeDoc::load(&full.doc(n).unwrap().save()).unwrap())).collect();
    let member = Tree::from_docs(r1, docs);
    assert!(member.doc(root).is_none());
    assert_eq!(member.shares(in_r2), vec![r2], "the walk ends where the store's root would be");
    assert_eq!(member.shares(in_r1), vec![r1]);
    assert!(member.leaves_a_share(in_r1, sub), "from one share into the other");
    assert_eq!(member.shares_left(in_r2, r1), vec![r2]);
    assert!(!member.leaves_a_share(in_r2, r2), "inside one");
    assert!(member.leaves_a_share(in_r1, root), "a parent not held is in no share this device knows");
    assert!(!member.leaves_a_share(root, r1), "and a node not held is in none to leave");

    // An ancestor missing in the middle: what is above it is not known.
    let (mut gap, gap_root) = tree();
    let top = folder(&mut gap, gap_root, "Top");
    let middle = folder(&mut gap, top, "Middle");
    let leaf = note(&mut gap, middle, "Leaf");
    share(&mut gap, top);
    gap.take_doc(middle);
    assert!(gap.shares(leaf).is_empty());
}

#[test]
fn an_unrepaired_cycle_cannot_loop_the_walk() {
    let (mut t, root) = tree();
    let (fa, fb) = (folder(&mut t, root, "A"), folder(&mut t, root, "B"));
    let leaf = note(&mut t, fa, "Leaf");
    share(&mut t, fb);
    tamper_parent(&mut t, fa, Some(fb));
    tamper_parent(&mut t, fb, Some(fa));
    assert_eq!(t.shares(leaf), vec![fb]);
    assert_eq!(t.shares(fb), vec![fb]);
    assert!(!t.leaves_a_share(leaf, fb));
    assert!(t.leaves_a_share(leaf, root));
}
