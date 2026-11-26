//! Moving a node (docs/MOVE_CONTRACT.md).

use super::*;

// ── Moving a node (docs/MOVE_CONTRACT.md) ────────────────────────────────

#[test]
fn a_move_that_leaves_a_share_appends_new_documents_with_their_own_keys_and_tombstones_the_original() {
    let (mut peer, root) = origin();
    let (share_a, share_b, note) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(share_a, Some(root), Place::Last, "folder", "Alpha", T0).unwrap();
    peer.add_node(share_b, Some(root), Place::Last, "folder", "Beta", T0).unwrap();
    peer.add_node(note, Some(share_a), Place::Last, "document", "Note", T0).unwrap();

    // The owner's page: the store key, and the key of each share.
    let mut scope = keyring_for(None);
    let store_key_id = scope.current.unwrap();
    let (key_a_id, key_a) = scope_key();
    let (key_b_id, key_b) = scope_key();
    scope.keys.insert(key_a_id, key_a);
    scope.keys.insert(key_b_id, key_b);
    scope.scopes.push((Some(share_a), key_a_id));
    scope.scopes.push((Some(share_b), key_b_id));

    let store_id = StoreId::new();
    let mut store = VaultStore::assemble(listed(root), StoreKeys::new(scope), Vec::new(), pull_of(&peer));
    mark_shared(&mut store.tree, share_a, key_a_id, "Alpha");
    mark_shared(&mut store.tree, share_b, key_b_id, "Beta");

    // Read before the move happens: afterwards the original is a
    // tombstone and answers nothing about the shares it used to be in.
    let left_shares = as_left_shares(&store.tree, store.tree.shares_left(note, share_b));
    assert_eq!(left_shares.len(), 1);
    assert_eq!(left_shares[0].root, share_a);
    assert_eq!(left_shares[0].name, "Alpha", "the first share it leaves, named from its marker");

    let (new_id, edit) = store.tree.move_or_transplant(store_id, note, share_b, Place::Last, T1, &mut NodeId::new).unwrap();
    assert_ne!(new_id, note, "moving into a different share is a transplant, not a move");
    assert!(store.tree.doc(note).unwrap().fields().unwrap().deleted_at.is_some(), "the original is tombstoned");

    for (id, update) in &edit.touched {
        let outgoing = store.prepare(store_id, *id, update).unwrap();
        if *id == new_id {
            let (keys, new) =
                outgoing.created.as_ref().expect("a document the server has never seen gets a key of its own");
            assert_eq!(new.parent_id, Some(share_b), "the append names the parent it lands under");
            let mut under: Vec<KeyId> = keys.wraps.iter().map(|w| w.scope_key_id).collect();
            under.sort_by_key(|id| id.to_string());
            let mut want = vec![store_key_id, key_b_id];
            want.sort_by_key(|id| id.to_string());
            assert_eq!(under, want, "the store key, and the key of the share it lands in — never Alpha's");
        } else {
            assert!(outgoing.created.is_none(), "{id} already existed on the server");
        }
    }
}

#[test]
fn a_transplant_between_two_vault_stores_lands_with_the_text_intact_and_the_source_tombstoned() {
    let (mut peer_from, root_from) = origin();
    let note = NodeId::new();
    peer_from.add_node(note, Some(root_from), Place::Last, "document", "Note", T0).unwrap();
    peer_from.doc_mut(note).unwrap().replace_plain_text("Hello, world").unwrap();

    let (peer_to, root_to) = origin();

    let from_store_id = StoreId::new();
    let to_store_id = StoreId::new();
    let mut from_store = opened(&peer_from);
    let mut to_store = opened(&peer_to);

    // Created first: the cutting read out of the source, planted in the
    // target with a fresh id, and appended there — as
    // `VaultClient::transplant_node` does, minus the network.
    let cutting = from_store.tree.take_cutting(note).unwrap();
    let (planted, edit_to) = to_store.tree.plant(cutting, root_to, Place::Last, T1, &mut NodeId::new).unwrap();
    let new_id = planted.root;
    assert_ne!(new_id, note);
    assert_eq!(to_store.tree.doc(new_id).unwrap().text(), "Hello, world", "the text survives a transplant");
    assert_eq!(to_store.tree.get_node_info(new_id).unwrap().title, "Note");

    for (id, update) in &edit_to.touched {
        let outgoing = to_store.prepare(to_store_id, *id, update).unwrap();
        if *id == new_id {
            assert!(outgoing.created.is_some(), "a document the target has never seen gets a key of its own");
        } else {
            assert!(outgoing.created.is_none(), "the target's own root already existed");
        }
    }

    // Deleted second, only once the new home would be on the server.
    let tombstone_edit = from_store.tree.remove_transplanted(note, to_store_id, &planted, T1).unwrap();
    let tombstone = from_store.tree.doc(note).unwrap().fields().unwrap();
    assert!(tombstone.deleted_at.is_some());
    assert_eq!(tombstone.became, Some(PimbleUrl::node(to_store_id, new_id)), "a link to the original follows it");
    for (id, update) in &tombstone_edit.touched {
        let outgoing = from_store.prepare(from_store_id, *id, update).unwrap();
        assert!(outgoing.created.is_none(), "the source's documents already existed");
    }
}

/// `resolveLink` answered in the page, by the server's rules: through
/// a transplant's `became` into another store this page holds, a plain
/// delete, an id nobody holds, a store this page does not hold, and a
/// scoped page's document outside its shares.
#[test]
fn a_link_is_resolved_in_the_page_through_became_by_the_servers_rules() {
    let (mut peer_from, root_from) = origin();
    let note = NodeId::new();
    let gone = NodeId::new();
    peer_from.add_node(note, Some(root_from), Place::Last, "document", "Note", T0).unwrap();
    peer_from.add_node(gone, Some(root_from), Place::Last, "document", "Gone", T0).unwrap();
    let (peer_to, root_to) = origin();
    let (from_id, to_id) = (StoreId::new(), StoreId::new());
    let mut from_store = opened(&peer_from);
    let mut to_store = opened(&peer_to);

    let cutting = from_store.tree.take_cutting(note).unwrap();
    let (planted, _) = to_store.tree.plant(cutting, root_to, Place::Last, T1, &mut NodeId::new).unwrap();
    from_store.tree.remove_transplanted(note, to_id, &planted, T1).unwrap();
    from_store.tree.remove_node(gone, T1).unwrap();
    let mut stores = HashMap::from([(from_id, from_store), (to_id, to_store)]);

    assert_eq!(resolve_in(&stores, from_id, note), LinkResolution::Live { store_id: to_id, node_id: planted.root });
    assert_eq!(resolve_in(&stores, from_id, gone), LinkResolution::Deleted { store_id: from_id, node_id: gone });
    assert_eq!(resolve_in(&stores, from_id, NodeId::new()), LinkResolution::Missing);
    let (elsewhere, somewhere) = (StoreId::new(), NodeId::new());
    assert_eq!(resolve_in(&stores, elsewhere, somewhere), LinkResolution::StoreNotHere { store_id: elsewhere, node_id: somewhere });

    // Where the node went is a store this page does not hold: the app
    // asks whoever does.
    let to_store = stores.remove(&to_id).unwrap();
    assert_eq!(resolve_in(&stores, from_id, note), LinkResolution::StoreNotHere { store_id: to_id, node_id: planted.root });

    // A scoped page does not hold what is outside its shares.
    let mut scoped = to_store;
    scoped.scope_roots = vec![planted.root];
    stores.insert(to_id, scoped);
    assert_eq!(resolve_in(&stores, to_id, NodeId::new()), LinkResolution::NoAccess);
}

#[test]
fn list_deleted_names_a_tombstone_s_parent_and_an_unlisted_node_s_place_to_go_back_to() {
    let (mut peer, root) = origin();
    let folder = NodeId::new();
    peer.add_node(folder, Some(root), Place::Last, "folder", "Notes", T0).unwrap();
    let gone = NodeId::new();
    peer.add_node(gone, Some(folder), Place::Last, "document", "Gone", T0).unwrap();
    peer.remove_node(gone, T1).unwrap();

    let stray = NodeId::new();
    peer.add_node(stray, Some(root), Place::Last, "document", "Stray", T0).unwrap();
    // A tampered or half-applied edit: still under `root` by `parent_id`,
    // but `root`'s own list no longer names it.
    peer.doc_mut(root).unwrap().remove_child(stray).unwrap();

    let store = opened(&peer);
    let nodes = store.list_deleted(None);

    let tombstone = nodes.iter().find(|d| d.node.id == gone).expect("the tombstone is listed");
    assert_eq!(tombstone.parent_title.as_deref(), Some("Notes"));
    assert!(tombstone.deleted_at.is_some());
    assert!(tombstone.put_back_under.is_none(), "undeleteNode puts it back where it was");

    let unlisted = nodes.iter().find(|d| d.node.id == stray).expect("the unlisted node is listed");
    assert!(unlisted.deleted_at.is_none());
    assert_eq!(unlisted.put_back_under, Some(root));

    // Neither the folder (holds a tombstone but is not one itself) nor
    // the root (the store's own, never a candidate) is listed.
    assert!(!nodes.iter().any(|d| d.node.id == folder || d.node.id == root));
}

#[test]
fn undeleting_a_node_puts_it_back_under_the_parent_it_was_deleted_from() {
    let (mut peer, root) = origin();
    let folder = NodeId::new();
    peer.add_node(folder, Some(root), Place::Last, "folder", "Notes", T0).unwrap();
    let note = NodeId::new();
    peer.add_node(note, Some(folder), Place::Last, "document", "Note", T0).unwrap();
    peer.remove_node(note, T1).unwrap();
    assert!(peer.get_node_info(note).is_err(), "a tombstone answers nothing to get_node_info");

    // What `VaultClient::undelete_node` does: `Tree::undelete_node`, then
    // read the parent it went back to, which is what its `NodeCreated`
    // names.
    peer.undelete_node(note, T2).unwrap();
    let info = peer.get_node_info(note).unwrap();
    assert_eq!(info.parent_id, Some(folder));
    assert_eq!(peer.get_children(folder).unwrap(), vec![note]);
}

#[test]
fn a_reader_s_transplant_and_undelete_are_refused_with_the_read_only_sentence() {
    let (store_id, other_id) = (StoreId::new(), StoreId::new());
    let root = NodeId::new();
    let mut client = VaultClient::new("me".to_string());
    client.rows.insert(store_id, row(vec![grant(Some(root), "reader", "Recipes")], Some("ann@example.com")));

    let transplant = BackendCommand::TransplantNode {
        from_store_id: store_id,
        node_id: NodeId::new(),
        to_store_id: other_id,
        new_parent_id: NodeId::new(),
        place: Place::Last,
    };
    let refused = refuse_write(&client, store_id, &transplant).expect("a reader may not transplant out of their root");
    assert!(matches!(&refused, BackendEvent::Error { message } if message == StoreAccess::READ_ONLY_REFUSAL));

    let undelete = BackendCommand::UndeleteNode { store_id, node_id: NodeId::new() };
    let refused = refuse_write(&client, store_id, &undelete).expect("a reader may not undelete either");
    assert!(matches!(&refused, BackendEvent::Error { message } if message == StoreAccess::READ_ONLY_REFUSAL));
}
