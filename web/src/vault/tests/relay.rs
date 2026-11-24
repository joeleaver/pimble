//! A store served from its owner's computer.

use super::*;

// ── A store served from its owner's computer ────────────────────────────

/// A page with one relayed share in the account's list: "Recipes", shared
/// by ann, which this account may edit.
fn page_with_a_relayed_share() -> (VaultClient, StoreId, NodeId) {
    let (store_id, root) = (StoreId::new(), NodeId::new());
    let mut client = VaultClient::new("me".to_string());
    client.rows.insert(store_id, relayed_row(vec![grant(Some(root), "editor", "Recipes")], Some("ann@example.com")));
    client.set_relayed(vec![store_id]);
    (client, store_id, root)
}

fn sync_of(event: &BackendEvent) -> (StoreAccess, RelaySide, bool) {
    match event {
        BackendEvent::StoreSyncChanged { access, relay, owner_offline, .. } => (*access, *relay, *owner_offline),
        other => panic!("not a sync answer: {other:?}"),
    }
}

/// The endpoint of a store this page never opened is down: the row comes
/// from the account's own list (the share's name, who shared it) and says
/// `owner offline`, once (docs/RELAY_CONTRACT.md, "The apps").
#[test]
fn a_store_whose_owner_is_offline_is_listed_from_the_account_s_row() {
    let (mut client, store_id, _) = page_with_a_relayed_share();

    let events = client.endpoint_down(&[store_id]);
    assert_eq!(events.len(), 2, "{events:?}");
    let BackendEvent::StoresListed { stores } = &events[0] else { panic!("no row: {events:?}") };
    let listed = &stores[0];
    assert_eq!(listed.id, store_id);
    assert_eq!(listed.name, "Recipes", "the share's own name");
    assert_eq!(listed.shared_by.as_deref(), Some("ann@example.com"));
    assert_eq!(listed.kind, StoreKind::Vault);
    assert_eq!(listed.relay, RelaySide::Member);
    assert_eq!(listed.access, StoreAccess::Read, "nothing can be changed of a store that is not there");
    assert!(listed.roots.is_empty(), "the row stands alone: nothing under it is known");
    assert_eq!(sync_of(&events[1]), (StoreAccess::Read, RelaySide::Member, true));

    // Every failed attempt after the first has nothing new to say.
    assert!(client.endpoint_down(&[store_id]).is_empty());

    // A row the account's list could not be read for still has a name,
    // the desktop's for the same thing.
    let unnamed = StoreId::new();
    client.set_relayed(vec![store_id, unnamed]);
    let events = client.endpoint_down(&[unnamed]);
    let BackendEvent::StoresListed { stores } = &events[0] else { panic!("no row: {events:?}") };
    assert_eq!(stores[0].name, "Shared from another computer");
    assert_eq!(stores[0].shared_by, None);
}

/// Such a store opens read-only and empty rather than erroring: its row,
/// an empty folder, no list, and a sentence for anything that would
/// change it. Nothing here needs a connection, and nothing is an `Error`
/// the status bar would show as one.
#[test]
fn a_store_whose_owner_is_offline_opens_empty_and_to_be_read() {
    let (mut client, store_id, _) = page_with_a_relayed_share();
    let placeholder = NodeId(store_id.0);

    // Before anyone has said it is down, it is nobody's to answer here.
    assert!(matches!(client.handle_unreachable(BackendCommand::GetChildren { store_id, node_id: placeholder }), Handled::No(_)));

    client.endpoint_down(&[store_id]);
    let answered = |handled: Handled| match handled {
        Handled::Yes(event) => event,
        Handled::No(cmd) => panic!("handed back: {cmd:?}"),
    };

    // Reads go through the router, which answers a store it cannot
    // reach and does not hold with its placeholder (`pimble_api::router`).
    let children = route_through(&mut client, BackendCommand::GetChildren { store_id, node_id: placeholder }).expect("routed");
    assert!(
        matches!(children.as_slice(), [BackendEvent::ChildrenLoaded { children, parent_id, .. }] if children.is_empty() && *parent_id == placeholder),
        "{children:?}"
    );
    let node = route_through(&mut client, BackendCommand::GetNode { store_id, node_id: placeholder }).expect("routed");
    let [BackendEvent::NodeLoaded { node, .. }] = node.as_slice() else { panic!("no node: {node:?}") };
    assert_eq!(node.metadata.title, "Recipes");
    assert_eq!(node.node_type, node_types::FOLDER);
    assert_eq!(node.access, StoreAccess::Read);
    assert!(node.children.is_empty());

    // The UI's registration of a store: nothing to subscribe to yet, and
    // a sync answer that says why.
    assert!(answered(client.handle_unreachable(BackendCommand::SubscribeStoreChanges { store_id })).is_none());
    let sync = answered(client.handle_unreachable(BackendCommand::GetStoreSync { store_id })).unwrap();
    assert_eq!(sync_of(&sync), (StoreAccess::Read, RelaySide::Member, true));

    // A write is refused with the sentence that says why, before anything
    // is asked of anyone; a read is not.
    let refused = refuse_write(&client, store_id, &BackendCommand::CreateNode { store_id, parent_id: None, title: "New".into(), init: None, place: Default::default() })
        .expect("nothing can be created in a store that cannot be reached");
    assert!(
        matches!(&refused, BackendEvent::Error { message } if *message == format!("Forbidden: {}", pimble_api::router::OWNER_OFFLINE_REFUSAL)),
        "{refused:?}"
    );
    assert!(refuse_write(&client, store_id, &BackendCommand::GetNode { store_id, node_id: placeholder }).is_none());

    // Another store's commands are none of this.
    let other = StoreId::new();
    assert!(matches!(client.handle_unreachable(BackendCommand::GetChildren { store_id: other, node_id: placeholder }), Handled::No(_)));
}

/// When the endpoint answers, the store is opened like any hosted one and
/// announced as the store it is, so the tree fetches what is in it with no
/// reload: the share's root under the row, the account's real access, and
/// `owner offline` gone.
#[test]
fn a_store_fills_in_when_its_owner_is_back() {
    let (mut client, store_id, _) = page_with_a_relayed_share();
    client.endpoint_down(&[store_id]);

    // What `open_listed` does once the endpoint lists the store: the
    // share's documents, pulled and held. (The relay face lists it to a
    // member as "Shared with you".)
    let (mut peer, root) = origin();
    peer.add_node(NodeId::new(), Some(root), Place::Last, "document", "Pasta", T0).unwrap();
    client.rows.insert(store_id, relayed_row(vec![grant(Some(root), "editor", "Recipes")], Some("ann@example.com")));
    let mut listed = Store::new_local("Shared with you", PathBuf::new());
    listed.id = store_id;
    listed.kind = StoreKind::Vault;
    let held = VaultStore::assemble(listed.clone(), store_keys(), vec![root], pull_of(&peer));
    client.stores.insert(store_id, held);

    let events = client.endpoint_up(&[store_id], &[listed]);
    assert_eq!(events.len(), 2, "{events:?}");
    let BackendEvent::StoreOpened { store } = &events[0] else { panic!("not announced: {events:?}") };
    assert_eq!(store.name, "Recipes");
    assert_eq!(store.roots, vec![root]);
    assert_eq!(store.access, StoreAccess::Full, "an editor edits again");
    assert_eq!(store.relay, RelaySide::Member);
    assert_eq!(sync_of(&events[1]), (StoreAccess::Full, RelaySide::Member, false));

    // And it answers like any open store from here on.
    assert!(matches!(client.handle_unreachable(BackendCommand::GetChildren { store_id, node_id: root }), Handled::No(_)));
    assert!(refuse_write(&client, store_id, &BackendCommand::CreateNode { store_id, parent_id: Some(root), title: "New".into(), init: None, place: Default::default() }).is_none());
    let children = client.children(store_id, root).expect("a list");
    assert!(!children.is_empty());
    assert!(children.iter().all(|node| node.access == StoreAccess::Full));

    // An endpoint that was never down has nothing to announce.
    assert!(client.endpoint_up(&[store_id], &[]).is_empty());
}

/// A store this page holds when its owner's computer goes off is read from
/// what is held, says so on its row, and takes no change until it is back,
/// except what the editor had already put on screen, which is kept for
/// the next connection rather than refused from under it.
#[test]
fn a_held_store_whose_owner_goes_offline_is_read_and_not_changed() {
    let (mut client, store_id, _) = page_with_a_relayed_share();
    let (mut peer, root) = origin();
    peer.add_node(NodeId::new(), Some(root), Place::Last, "document", "Pasta", T0).unwrap();
    client.rows.insert(store_id, relayed_row(vec![grant(None, "editor", "")], None));
    client.stores.insert(store_id, opened(&peer));

    let events = client.endpoint_down(&[store_id]);
    assert_eq!(events.len(), 1, "a store the UI already has is not listed again: {events:?}");
    assert_eq!(sync_of(&events[0]), (StoreAccess::Read, RelaySide::Member, true));

    let routed = route_through(&mut client, BackendCommand::GetChildren { store_id, node_id: root });
    let Some([BackendEvent::ChildrenLoaded { children, .. }]) = routed.as_deref() else {
        panic!("the held list was not served: {routed:?}")
    };
    assert!(!children.is_empty(), "what is held is still read");
    assert!(children.iter().all(|node| node.access == StoreAccess::Read));
    let mut described = listed(root);
    described.id = store_id;
    client.describe(&mut described);
    assert_eq!(described.access, StoreAccess::Read);

    let rename = BackendCommand::RenameNode { store_id, node_id: root, title: "No".into() };
    assert!(refuse_write(&client, store_id, &rename).is_some());

    // A keystroke already on its way: merged and kept, not refused.
    let mut editor = NodeDoc::load(&peer.doc(root).unwrap().save()).unwrap();
    let before = editor.state_vector();
    let typed = {
        let mut tree = Tree::from_docs(root, HashMap::from([(root, NodeDoc::load(&editor.save()).unwrap())]));
        let edit = tree.set_title(root, "Typed while it went", T1).unwrap();
        edit.touched[0].1.clone()
    };
    editor.apply_update(&typed).unwrap();
    assert_ne!(editor.state_vector(), before);
    let keystroke = BackendCommand::BroadcastChanges { store_id, node_id: root, update: typed.clone() };
    assert!(refuse_write(&client, store_id, &keystroke).is_none());
    assert_eq!(route_through(&mut client, keystroke).map(|events| events.len()), Some(0), "taken, with nothing to say");
    let held = &client.stores[&store_id];
    assert!(held.docs[&root].unsent, "the next connection has nothing to resend");
    assert_eq!(held.tree.doc(root).unwrap().fields().unwrap().title, "Typed while it went");
}

/// A store the session says is relayed is one whether or not the
/// account's list could be read, and a whole store with no name anywhere
/// but in its own documents is called by its root's title once that is
/// open here.
#[test]
fn a_relayed_store_is_described_as_one() {
    let store_id = StoreId::new();
    let mut client = VaultClient::new("me".to_string());
    client.set_relayed(vec![store_id]);
    assert!(client.is_relayed(store_id));
    assert!(client.is_encrypted(store_id));

    // The twin is made without a name, and Pimble Cloud holds none.
    let mut unnamed = Store::new_local("", PathBuf::new());
    unnamed.id = store_id;
    unnamed.kind = StoreKind::Vault;
    let mut before_open = unnamed.clone();
    client.describe(&mut before_open);
    assert_eq!(before_open.relay, RelaySide::Member);
    assert_eq!(before_open.name, "Shared from another computer");

    let (peer, _) = origin();
    let title = peer.get_node_info(peer.root()).unwrap().title;
    client.stores.insert(store_id, opened(&peer));
    let mut open = unnamed.clone();
    client.describe(&mut open);
    assert_eq!(open.name, title);

    // A hosted store is not called any of this.
    let mut hosted = Store::new_local("", PathBuf::new());
    hosted.kind = StoreKind::Vault;
    client.describe(&mut hosted);
    assert_eq!(hosted.relay, RelaySide::None);
    assert_eq!(hosted.name, "");

    // A session that stops naming a store stops waiting for it.
    client.endpoint_down(&[store_id]);
    client.set_relayed(Vec::new());
    assert!(matches!(client.handle_unreachable(BackendCommand::GetStoreSync { store_id }), Handled::No(_)));
}
