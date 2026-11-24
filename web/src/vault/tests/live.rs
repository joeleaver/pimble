//! What the UI hears, repair after a whole pull, and which notifications are applied.

use super::*;

// ── What the UI hears ───────────────────────────────────────────────────

#[test]
fn merged_structural_updates_earn_the_kinds_the_server_derives() {
    let (mut peer, root) = origin();
    let (p, q) = (NodeId::new(), NodeId::new());
    peer.add_node(p, Some(root), Place::Last, "folder", "P", T0).unwrap();
    peer.add_node(q, Some(root), Place::Last, "folder", "Q", T0).unwrap();
    let store_id = StoreId::new();
    let mut store = opened(&peer);
    let mut seq = 0;

    // Create: the node's own document is `NodeCreated`, the parent's
    // list `TreeStructure`.
    let x = NodeId::new();
    let edit = peer.add_node(x, Some(p), Place::Last, "document", "x", T1).unwrap();
    let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
    assert!(structure);
    assert!(matches!(kinds_of(&events)[..], [StoreChangeKind::NodeCreated { node_id, parent_id }, StoreChangeKind::TreeStructure { ref node_ids }] if node_id == x && parent_id == p && node_ids == &vec![p]), "{events:?}");

    // Rename: `MetadataUpdated`.
    let edit = peer.set_title(x, "Renamed", T1).unwrap();
    let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
    assert!(matches!(kinds_of(&events)[..], [StoreChangeKind::MetadataUpdated { node_id }] if node_id == x), "{events:?}");

    // Move: `NodeMoved` for the node, `TreeStructure` for both lists.
    let edit = peer.move_node(x, q, Place::Last, T1).unwrap();
    let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
    let kinds = kinds_of(&events);
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeMoved { node_id, old_parent_id, new_parent_id } if *node_id == x && *old_parent_id == p && *new_parent_id == q)), "{kinds:?}");
    assert_eq!(kinds.iter().filter(|k| matches!(k, StoreChangeKind::TreeStructure { .. })).count(), 2, "{kinds:?}");

    // Delete: `NodeDeleted` naming the parent it left.
    let edit = peer.remove_node(x, T2).unwrap();
    let (events, _) = arrive(&mut store, store_id, &edit, &mut seq, None);
    let kinds = kinds_of(&events);
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeDeleted { node_id, parent_id } if *node_id == x && *parent_id == q)), "{kinds:?}");

    // The same bytes again change nothing and say nothing.
    let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
    assert!(events.is_empty() && !structure);
}

#[test]
fn a_content_update_reaches_the_editor_only_for_the_node_it_has_open() {
    let (mut peer, root) = origin();
    let x = NodeId::new();
    peer.add_node(x, Some(root), Place::Last, "document", "x", T0).unwrap();
    let store_id = StoreId::new();
    let mut store = opened(&peer);

    let delta = peer.doc_mut(x).unwrap().replace_plain_text("typed elsewhere").unwrap();
    let edit = TreeEdit { touched: vec![(x, delta.clone())] };
    let mut seq = 0;

    // Not open here: the UI refetches the node.
    let (events, structure) = arrive(&mut store, store_id, &edit, &mut seq, None);
    assert!(!structure, "content is not structure");
    assert!(matches!(events[..], [BackendEvent::NodeContentUpdated { node_id, .. }] if node_id == x), "{events:?}");
    assert_eq!(store.tree.doc(x).unwrap().text(), "typed elsewhere");

    // Open in the editor: the delta itself, base64 as the plain path
    // carries it.
    let mut again = opened(&peer);
    let delta = peer.doc_mut(x).unwrap().replace_plain_text("and more").unwrap();
    let edit = TreeEdit { touched: vec![(x, delta.clone())] };
    let (events, _) = arrive(&mut again, store_id, &edit, &mut seq, Some(x));
    assert!(matches!(&events[..], [BackendEvent::RemoteChanges { update, .. }, ..] if *update == delta), "{events:?}");
}

// ── Repair only after a whole pull ──────────────────────────────────────

#[test]
fn a_pull_is_repaired_once_after_the_whole_of_it() {
    let (mut peer, root) = origin();
    let (p, q, x) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(p, Some(root), Place::Last, "folder", "P", T0).unwrap();
    peer.add_node(q, Some(root), Place::Last, "folder", "Q", T0).unwrap();
    peer.add_node(x, Some(p), Place::Last, "document", "x", T0).unwrap();
    let before = pull_of(&peer);
    let before_the_move = opened(&peer);
    let edit = peer.move_node(x, q, Place::Last, T1).unwrap();
    assert_eq!(edit.touched.len(), 3);

    // The pull holds the snapshots and, after them, the move's three
    // updates in log order. Applied whole, the tree is consistent and
    // the one repair afterwards has nothing to do.
    let mut pulled = before;
    for (id, update) in &edit.touched {
        let doc = pulled.iter_mut().find(|d| d.node_id == *id).unwrap();
        doc.entries.push((Mark::One(2), Ok(update.clone())));
        doc.head = 2;
    }
    let mut store = VaultStore::assemble(listed(root), store_keys(), Vec::new(), pulled);
    assert!(store.tree.validate_tree().is_empty(), "{:?}", store.tree.validate_tree());
    assert!(store.repair_now(T2).is_none());
    assert_eq!(store.children_of(q).unwrap(), vec![x]);
    assert!(store.children_of(p).unwrap().is_empty());

    // Judged after each update: after P's removal alone, x is listed
    // nowhere. Before 0.28.5 repair's answer was to append it to P again,
    // an edit that then travelled; now P's own document records that x
    // left (`left:`, docs/MOVE_CONTRACT.md "Repair"), so x is a copy
    // older than its parent's and nothing is decided about it. On the
    // live path the merge itself never repairs either; it only says a
    // repair is wanted, and by the time the debounce fires the rest of
    // the edit is in.
    let mut half = before_the_move;
    let store_id = StoreId::new();
    let (id, update) = &edit.touched[0];
    assert_eq!(*id, p, "the old parent's list goes first");
    let merged = half.merge(store_id, *id, vec![(Mark::One(2), Ok(update.clone()))], false, None);
    assert!(merged.structure, "flagged for the debounced repair");
    assert!(half.tree.validate_tree().is_empty(), "half a move leaves nothing to repair: P says x left");
    assert!(half.repair_due.is_none(), "the merge itself schedules nothing");
    for (id, update) in &edit.touched[1..] {
        half.merge(store_id, *id, vec![(Mark::One(2), Ok(update.clone()))], false, None);
    }
    assert!(half.tree.validate_tree().is_empty());
    assert!(half.repair_now(T2).is_none(), "nothing left to write once the whole edit is in");
    assert_eq!(half.children_of(q).unwrap(), vec![x]);
}

#[test]
fn a_repair_writes_what_the_tree_needs_and_names_it() {
    let (mut peer, root) = origin();
    let x = NodeId::new();
    peer.add_node(x, Some(root), Place::Last, "document", "x", T0).unwrap();
    let mut store = opened(&peer);
    // A duplicate a concurrent append left in the root's list.
    store.tree.doc_mut(root).unwrap().insert_child(99, x).unwrap();
    let edit = store.repair_now(T1).expect("a duplicate needs a repair");
    assert_eq!(edit.node_ids(), vec![root]);
    assert!(store.tree.validate_tree().is_empty());
    assert_eq!(store.children_of(root).unwrap(), vec![x]);
}

// ── Which notifications are applied ─────────────────────────────────────

#[test]
fn a_client_skips_only_its_own_work() {
    assert!(!should_apply(Some("me"), "me"));
    assert!(should_apply(Some("someone-else"), "me"));
}

#[test]
fn an_unattributed_notification_is_applied() {
    // What the server sends today: `VaultAppendRequest` carries no client
    // id, so nothing is suppressed and every blob is merged.
    assert!(should_apply(None, "me"));
}

#[test]
fn interleaved_appends_from_two_clients_all_arrive() {
    // Two tabs typing in turn. The server numbers appends in arrival
    // order, so each client's own ids are scattered through the other's —
    // which is why a sequence number cannot stand in for authorship. Every
    // update written by the other client must be applied, whether its
    // number is above or below anything this client was last given.
    let log: [(u64, &str); 6] = [
        (1, "a"),
        (2, "b"),
        (3, "a"),
        (4, "b"),
        (5, "b"),
        (6, "a"),
    ];

    let seen_by_a: Vec<u64> = log
        .iter()
        .filter(|(_, who)| should_apply(Some(who), "a"))
        .map(|(seq, _)| *seq)
        .collect();
    let seen_by_b: Vec<u64> = log
        .iter()
        .filter(|(_, who)| should_apply(Some(who), "b"))
        .map(|(seq, _)| *seq)
        .collect();

    assert_eq!(seen_by_a, vec![2, 4, 5], "a must see every append b made");
    assert_eq!(seen_by_b, vec![1, 3, 6], "b must see every append a made");
}

#[test]
fn a_late_number_is_not_a_reason_to_drop() {
    // The shape of the bug this replaced: b's append lands between two of
    // a's, so its number is below the last one a was given. It is still b's
    // work and still has to be applied.
    assert!(should_apply(Some("b"), "a"));
}
