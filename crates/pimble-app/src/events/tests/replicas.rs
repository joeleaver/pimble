//! Replicas and partial replicas: hosted lists, shared roots, access, `Shown`, ended shares.

use super::*;

/// "Add Hosted Store..." lists a share of a store already open here when
/// this device does not hold that share's root yet — the server extends
/// the partial replica — and never one whose root it holds. Two pending
/// shares of one store are one row, because the RPC names a store rather
/// than a scope (docs/NODE_DOCUMENT_CONTRACT.md section 5).
#[test]
fn the_hosted_list_offers_a_second_share_of_an_open_store() {
    let (store, events, _commands) = store_with_events();
    let mut held = pimble_core::Store::new_local("Ann's notes", "/tmp/anns.pimble".into());
    let (recipes, trips) = (NodeId::new(), NodeId::new());
    held.root_node_id = recipes;
    held.roots = vec![recipes];
    let store_id = held.id;
    store.upsert_store(held);

    let other = uuid::Uuid::new_v4().to_string();
    let row = |id: &str, name: &str, root: Option<NodeId>| pimble_rpc::CloudHostedStoreInfo {
        store_id: id.to_string(),
        name: name.to_string(),
        role: "editor".to_string(),
        kind: "vault".to_string(),
        created_at: String::new(),
        root,
        shared_by: root.map(|_| "ann@example.com".to_string()),
    };
    events
        .send(BackendEvent::CloudHostedStoresListed {
            stores: vec![
                // Held already: not offered again.
                row(&store_id.to_string(), "Recipes", Some(recipes)),
                // A second share of the same store: offered, once.
                row(&store_id.to_string(), "Trips", Some(trips)),
                row(&store_id.to_string(), "Walks", Some(NodeId::new())),
                row(&other, "Ledger", None),
            ],
            // The rows served from their owner's computer ride beside
            // the list, and the modal's labels read them.
            relayed: vec![other.clone()],
        })
        .unwrap();
    pump(store);
    assert_eq!(store.hosted_modal_relayed.get(), vec![other.clone()]);

    let offered: Vec<(String, String)> =
        store.hosted_modal_stores.with(|v| v.iter().map(|s| (s.store_id.clone(), s.name.clone())).collect());
    assert_eq!(
        offered,
        vec![(store_id.to_string(), "Trips".to_string()), (other, "Ledger".to_string())]
    );
}

/// A store shared with this device arrives as a partial replica: the app
/// asks for each shared root and its children, not just the first, and a
/// second share of the same store extends the list in place
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "The recipient's replica").
#[test]
fn a_partial_replica_fetches_every_shared_root() {
    let (store, events, commands) = store_with_events();
    let mut shared = pimble_core::Store::new_local("Ann's notes", "/tmp/anns.pimble".into());
    let (recipes, trips) = (NodeId::new(), NodeId::new());
    shared.root_node_id = recipes;
    shared.roots = vec![recipes, trips];
    shared.shared_by = Some("ann@example.com".to_string());
    let store_id = shared.id;

    events.send(BackendEvent::StoreOpened { store: shared.clone() }).unwrap();
    pump(store);

    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    for root in [recipes, trips] {
        assert!(
            asked.iter().any(|c| asks_children(c, root)),
            "no children asked for {root}"
        );
        assert!(
            asked.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == root)),
            "the root node itself was never fetched for {root}"
        );
    }
    assert_eq!(store.shown_roots(store_id), vec![recipes, trips]);

    // A second share of the same store: the server extends the replica and
    // answers with the longer list, which has to replace the held one.
    let walks = NodeId::new();
    let mut extended = shared.clone();
    extended.roots = vec![recipes, trips, walks];
    events.send(BackendEvent::StoreOpened { store: extended }).unwrap();
    pump(store);
    assert_eq!(store.shown_roots(store_id), vec![recipes, trips, walks]);
    assert!(commands
        .try_iter()
        .any(|c| asks_children(&c, walks)));
}

/// A store with two shared roots and a document under each, all loaded,
/// as the tree holds a partial replica.
fn a_replica_with_two_roots(store: AppStore) -> (StoreId, [NodeId; 2], [NodeId; 2]) {
    let mut shared = pimble_core::Store::new_local("Ann's notes", "/tmp/anns.pimble".into());
    let (recipes, trips) = (pimble_core::Node::folder("Recipes"), pimble_core::Node::folder("Trips"));
    let (pasta, rome) = (pimble_core::Node::document("Pasta"), pimble_core::Node::document("Rome"));
    shared.root_node_id = recipes.id;
    shared.roots = vec![recipes.id, trips.id];
    shared.shared_by = Some("ann@example.com".to_string());
    let store_id = shared.id;
    store.upsert_store(shared);
    let ids = ([recipes.id, trips.id], [pasta.id, rome.id]);
    store.set_children(store_id, recipes.id, vec![(store_id, pasta.id)]);
    store.set_children(store_id, trips.id, vec![(store_id, rome.id)]);
    for node in [recipes, trips, pasta, rome] {
        store.upsert_node(store_id, node);
    }
    (store_id, ids.0, ids.1)
}

/// A role the owner changes while the app runs reaches it with the next
/// `GetStoreSync` answer: the store's `access` and `read_only_roots` are
/// written, and because each node carries the server's judgement of
/// itself, the API is asked once to judge again every node the window
/// shows of the store (the open document's included), and its answer is
/// shown like any other. An answer that changes nothing asks for nothing.
#[test]
fn a_changed_access_is_kept_and_has_the_shown_nodes_judged_again() {
    use pimble_core::StoreAccess::{Full, Read};
    let (store, events, commands) = store_with_events();
    let (store_id, [recipes, trips], [pasta, rome]) = a_replica_with_two_roots(store);
    let opened = NodeId::new();
    store.panes[0].active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: opened }));

    events.send(sync_changed(store_id, Full, Vec::new())).unwrap();
    pump(store);
    assert!(
        !commands.try_iter().any(|c| matches!(c, BackendCommand::Rejudge { .. } | BackendCommand::GetChildren { .. } | BackendCommand::GetNode { .. })),
        "nothing changed, so nothing is asked"
    );

    // The owner made this account a reader of Trips.
    let before = store.tree_structure_version.get();
    events.send(sync_changed(store_id, Full, vec![trips])).unwrap();
    pump(store);
    let held = store.get_store_signal(store_id).unwrap();
    assert_eq!(held.with(|s| (s.access, s.read_only_roots.clone())), (Full, vec![trips]));
    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    let [BackendCommand::Rejudge { store_id: s, node_ids }] = asked.as_slice() else { panic!("one question, not {asked:?}") };
    assert_eq!(*s, store_id);
    for id in [recipes, trips, pasta, rome, opened] {
        assert!(node_ids.contains(&id), "{id} is not judged again");
    }
    assert!(store.tree_structure_version.get() > before);

    // The answer carries the judgement, and the app believes it: the
    // document under Trips takes no edits now, its neighbour still does.
    let mut rome_now = store.get_node_signal(store_id, rome).unwrap().with(|n| n.clone());
    rome_now.access = Read;
    let before = store.tree_structure_version.get();
    events.send(shown(store_id, vec![rome_now], Vec::new(), Vec::new())).unwrap();
    pump(store);
    assert_eq!(store.node_access(store_id, rome), Read);
    assert_eq!(store.node_access(store_id, pasta), Full);
    assert!(store.tree_structure_version.get() > before, "the row's menu never re-renders");

    // The whole replica to read: the store's own word covers every node.
    events.send(sync_changed(store_id, Read, Vec::new())).unwrap();
    pump(store);
    assert_eq!(held.with(|s| (s.access, s.read_only_roots.clone())), (Read, Vec::new()));
    assert_eq!(store.node_access(store_id, pasta), Read);
    assert!(commands.try_iter().any(|c| matches!(c, BackendCommand::Rejudge { .. })));
}

/// A children list is read from the documents the API holds, so it is
/// taken as it is, with the latest judgement of each child: there is no
/// older copy to keep.
#[test]
fn a_children_list_is_taken_as_it_is() {
    use pimble_core::StoreAccess::{Full, Read};
    let (store, events, _commands) = store_with_events();
    let (store_id, [_, trips], [_, rome]) = a_replica_with_two_roots(store);

    let mut listed = store.get_node_signal(store_id, rome).unwrap().with(|n| n.clone());
    listed.metadata.title = "Rome in May".to_string();
    listed.access = Read;
    events
        .send(BackendEvent::ChildrenLoaded { store_id, parent_id: trips, children_store_id: store_id, children: vec![listed] })
        .unwrap();
    pump(store);

    let held = store.get_node_signal(store_id, rome).unwrap();
    assert_eq!(held.with(|n| n.metadata.title.clone()), "Rome in May");
    assert_eq!(store.node_access(store_id, rome), Read);
    assert_eq!(store.node_access(store_id, trips), Full);
}

/// What another device did, shown (docs/API_CONTRACT.md wave 5): the
/// rows it touched update, a loaded list and every loaded mount of it
/// take the new order, a list never loaded is not made up, a node that
/// left goes with what is under it, and nothing is fetched again.
#[test]
fn a_shown_change_updates_exactly_what_it_touched() {
    let (store, events, commands) = store_with_events();
    let (store_id, [recipes, trips], [pasta, rome]) = a_replica_with_two_roots(store);
    // A mount elsewhere showing Recipes.
    let (other_store, mount) = (StoreId::new(), NodeId::new());
    store.mount_data.update(|map| {
        map.insert(
            (other_store, mount),
            Signal::new(MountInfo {
                is_mount: true,
                mount_state: None,
                mount_ref: Some(pimble_core::MountRef { source_store: store_id, source_node: recipes, source_path: None, source_remote: None }),
            }),
        );
    });
    store.set_children(other_store, mount, vec![(store_id, pasta)]);

    let soup = pimble_core::Node::document("Soup");
    let mut renamed = store.get_node_signal(store_id, pasta).unwrap().with(|n| n.clone());
    renamed.metadata.title = "Pasta, fresh".to_string();
    let before = store.tree_structure_version.get();
    events.send(shown(store_id, vec![renamed.clone()], vec![(recipes, vec![soup.clone(), renamed])], Vec::new())).unwrap();
    pump(store);

    let order = |s: StoreId, p: NodeId| store.get_children_signal(s, p).unwrap().with(|c| c.clone());
    assert_eq!(order(store_id, recipes), vec![(store_id, soup.id), (store_id, pasta)]);
    assert_eq!(order(other_store, mount), vec![(store_id, soup.id), (store_id, pasta)], "the mount shows the same list");
    assert_eq!(store.display_label(store_id, pasta), "Pasta, fresh");
    assert!(store.tree_structure_version.get() > before);

    // A list this window never loaded (Soup's) is not made up.
    events.send(shown(store_id, Vec::new(), vec![(soup.id, vec![pimble_core::Node::document("Stock")])], Vec::new())).unwrap();
    pump(store);
    assert!(!store.has_children_loaded(store_id, soup.id));

    // Trips is deleted: its row and what is under it go.
    let mut trips_node = store.get_node_signal(store_id, trips).unwrap().with(|n| n.clone());
    trips_node.children = vec![rome];
    store.upsert_node(store_id, trips_node);
    events.send(shown(store_id, Vec::new(), Vec::new(), vec![trips])).unwrap();
    pump(store);
    assert!(store.get_node_signal(store_id, rome).is_none(), "what was under it went with it");

    assert!(!commands.try_iter().any(|c| matches!(c, BackendCommand::GetChildren { .. } | BackendCommand::GetNode { .. })), "nothing is fetched again");
}

/// A folder that was never opened shows a chevron from its node's own
/// `children`. When its first child turns up (a document made on another
/// device), the folder's row is shown again with it, and a node that
/// starts listing children rebuilds its row, or the child stays out of
/// reach; its list, never loaded, is not loaded now.
#[test]
fn a_child_arriving_late_gives_its_unopened_folder_a_chevron() {
    let (store, events, commands) = store_with_events();
    let (store_id, [recipes, _], [pasta, _]) = a_replica_with_two_roots(store);
    let walks = pimble_core::Node::folder("Walks");
    let walks_id = walks.id;
    store.set_children(store_id, recipes, vec![(store_id, pasta), (store_id, walks_id)]);
    store.upsert_node(store_id, walks.clone());
    assert!(!store.has_children_loaded(store_id, walks_id));

    let storr = pimble_core::Node::document("The Storr");
    let mut listing = walks;
    listing.children = vec![storr.id];
    let before = untracked(|| store.tree_structure_version.get());
    events.send(shown(store_id, vec![storr.clone(), listing], vec![(walks_id, vec![storr])], Vec::new())).unwrap();
    pump(store);
    assert_ne!(untracked(|| store.tree_structure_version.get()), before, "a node that starts listing children rebuilds its row");
    assert!(!store.has_children_loaded(store_id, walks_id), "its list was never loaded and is not loaded now");
    assert!(!commands.try_iter().any(|c| matches!(c, BackendCommand::GetChildren { .. } | BackendCommand::GetNode { .. })));
}

/// A link reads how the account holds the store each time it connects,
/// and says only `SyncStateChanged` when it has: the app asks for the
/// rest. Not on `Syncing`, which a link announces before it has read
/// anything.
#[test]
fn a_links_transition_asks_how_the_store_is_held_now() {
    let (store, events, commands) = store_with_events();
    let (store_id, _, _) = a_replica_with_two_roots(store);
    let notify = |state: SyncState| BackendEvent::RemoteStoreChange {
        store_id,
        change_kind: pimble_rpc::StoreChangeKind::SyncStateChanged { state },
        source_client_id: None,
    };
    let asked = |commands: &crossbeam_channel::Receiver<BackendCommand>| {
        commands.try_iter().any(|c| matches!(c, BackendCommand::GetStoreSync { store_id: s } if s == store_id))
    };

    events.send(notify(SyncState::Syncing)).unwrap();
    pump(store);
    assert!(!asked(&commands));
    events.send(notify(synced())).unwrap();
    pump(store);
    assert!(asked(&commands), "connected again: the grant was read");
    events.send(notify(SyncState::Offline)).unwrap();
    pump(store);
    assert!(asked(&commands), "refused or unreachable: a removal from every share is recorded before the connect fails");
}

/// A node that has just been shared must re-render its row, and rinch
/// re-renders one only when its data changes — so the marker arriving
/// bumps the tree (docs/NODE_DOCUMENT_CONTRACT.md section 5).
#[test]
fn a_new_share_marker_rebuilds_the_row() {
    let (store, events, _commands) = store_with_events();
    let store_id = StoreId::new();
    let mut node = pimble_core::Node::document("Recipes");
    store.upsert_node(store_id, node.clone());
    let before = store.tree_structure_version.get();

    node.metadata.set_share(Some(&pimble_core::ShareMarker {
        v: pimble_core::ShareMarker::VERSION,
        key_id: uuid::Uuid::new_v4(),
        url: "https://pimble.app".to_string(),
        name: "Recipes".to_string(),
    }));
    events.send(BackendEvent::NodeLoaded { store_id, node: node.clone() }).unwrap();
    pump(store);
    assert!(store.tree_structure_version.get() > before, "the row never re-renders");

    // The same node again changes nothing the row draws.
    let after = store.tree_structure_version.get();
    events.send(BackendEvent::NodeLoaded { store_id, node }).unwrap();
    pump(store);
    assert_eq!(store.tree_structure_version.get(), after);
}

// ── A share that ended (Joe, 2026-09-21) ────────────────────────────

/// A replica of two shares with a document under each, parents and lists
/// as the server sends them, and the document under Trips open.
fn a_replica_with_rome_open(store: AppStore) -> (StoreId, [NodeId; 2], [NodeId; 2]) {
    let mut shared = pimble_core::Store::new_local("Shared by ann@example.com", "/tmp/anns.pimble".into());
    let (mut recipes, mut trips) = (pimble_core::Node::folder("Recipes"), pimble_core::Node::folder("Trips"));
    let (mut pasta, mut rome) = (pimble_core::Node::document("Pasta"), pimble_core::Node::document("Rome"));
    pasta.parent_id = Some(recipes.id);
    rome.parent_id = Some(trips.id);
    recipes.children = vec![pasta.id];
    trips.children = vec![rome.id];
    shared.root_node_id = recipes.id;
    shared.roots = vec![recipes.id, trips.id];
    shared.shared_by = Some("ann@example.com".to_string());
    shared.is_replica = true;
    let store_id = shared.id;
    store.upsert_store(shared);
    store.set_children(store_id, recipes.id, vec![(store_id, pasta.id)]);
    store.set_children(store_id, trips.id, vec![(store_id, rome.id)]);
    let ids = ([recipes.id, trips.id], [pasta.id, rome.id]);
    for node in [recipes, trips, pasta, rome] {
        store.upsert_node(store_id, node);
    }
    store.select_in_pane(crate::panes::PaneId::FIRST, Some(format!("node_{store_id}_{}", ids.1[1])));
    store.node_title.set("Rome".to_string());
    store.panes[0].show_editor.set(true);
    store.panes[0].active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: ids.1[1] }));
    (store_id, ids.0, ids.1)
}

fn shares_ended(store_id: StoreId, node_ids: Vec<NodeId>) -> BackendEvent {
    BackendEvent::RemoteStoreChange { store_id, change_kind: pimble_rpc::StoreChangeKind::SharesEnded { node_ids }, source_client_id: None }
}

fn row_values(store: AppStore) -> Vec<String> {
    untracked(|| store.build_tree_data_structural())[0].children.iter().map(|row| row.value.clone()).collect()
}

/// The owner removed this account from Trips while the app was running:
/// the folder leaves the explorer with one sentence that names it,
/// everything held of it is dropped, the document open under it closes,
/// and the share still held is untouched. Nothing is asked of the
/// server but how the store is held now.
#[test]
fn a_share_that_ended_leaves_the_explorer_with_a_notice() {
    let (store, events, commands) = store_with_events();
    let (store_id, [recipes, trips], [pasta, rome]) = a_replica_with_rome_open(store);
    let before = store.tree_structure_version.get();

    events.send(shares_ended(store_id, vec![trips])).unwrap();
    pump(store);

    assert_eq!(store.notice.get(), "\"Trips\" is no longer shared with you.");
    assert_eq!(store.shown_roots(store_id), vec![recipes]);
    assert_eq!(row_values(store), vec![format!("node_{store_id}_{recipes}")]);
    assert!(store.tree_structure_version.get() > before, "the tree is rebuilt");
    for dropped in [trips, rome] {
        assert!(store.get_node_signal(store_id, dropped).is_none(), "{dropped} is still held");
        assert!(!store.has_children_loaded(store_id, dropped));
    }
    for kept in [recipes, pasta] {
        assert!(store.get_node_signal(store_id, kept).is_some());
    }
    // The open document was under it: closed as a deleted node's is.
    assert_eq!(store.selected_id.get(), None);
    assert!(!store.panes[0].show_editor.get());
    assert_eq!(store.node_title.get(), "");

    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    assert!(asked.is_empty(), "nothing is asked of the server, and nothing deleted: the files stay until the person removes the replica ({} command(s))", asked.len());

    // Told again (the answer to `GetStoreSync` names it too): nothing more.
    let settled = store.tree_structure_version.get();
    store.notice.set(String::new());
    let mut answer = sync_changed(store_id, pimble_core::StoreAccess::Full, vec![trips]);
    if let BackendEvent::StoreSyncChanged { ended_roots, .. } = &mut answer {
        *ended_roots = vec![trips];
    }
    events.send(shares_ended(store_id, vec![trips])).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "", "one notice per folder");
    assert_eq!(store.tree_structure_version.get(), settled, "and the tree is left alone");
    events.send(answer).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "");
    assert_eq!(store.shown_roots(store_id), vec![recipes]);
}

/// A document open under the share that is still held stays open, and a
/// folder whose title the app never loaded is not named.
#[test]
fn a_share_that_ended_closes_nothing_else_and_names_only_what_it_knows() {
    let (store, events, _commands) = store_with_events();
    let (store_id, [recipes, trips], [pasta, _]) = a_replica_with_rome_open(store);
    store.select_in_pane(crate::panes::PaneId::FIRST, Some(format!("node_{store_id}_{pasta}")));
    store.panes[0].active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: pasta }));
    store.remove_node(store_id, trips);

    events.send(shares_ended(store_id, vec![trips])).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "A shared folder is no longer shared with you.");
    assert_eq!(store.shown_roots(store_id), vec![recipes]);
    assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{pasta}")));
    assert!(store.panes[0].show_editor.get());
    assert!(store.panes[0].active_edit.get().is_some_and(|active| active.node_id == pasta));
}

/// Removed from every share at once: a sentence for each, no folder
/// left, and the store row stays with "no longer shared" and a menu that
/// offers "Remove Replica..." and nothing that changes anything.
#[test]
fn the_last_share_to_end_leaves_the_row_and_its_removal() {
    let (store, events, _commands) = store_with_events();
    let (store_id, [recipes, trips], _) = a_replica_with_rome_open(store);

    events.send(shares_ended(store_id, vec![recipes, trips])).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "\"Recipes\" is no longer shared with you. \"Trips\" is no longer shared with you.");
    assert!(store.every_share_ended(store_id));
    let data = untracked(|| store.build_tree_data_structural());
    assert_eq!(data.len(), 1, "the store row stays");
    assert!(data[0].children.is_empty());
    assert!(store.selected_id.get().is_none() && !store.panes[0].show_editor.get());

    let held = store.get_store_signal(store_id).unwrap();
    assert_eq!(held.with(crate::state::shared_by_words), "no longer shared");
    let menu = pimble_core::plan::capabilities(&store.view(), store_id, None).store_row_menu(false);
    assert!(!menu.remove_replica);
    assert!(menu.new_node && menu.unlink && menu.link_to_remote && menu.share && menu.appearance && menu.copy_as_mount_source);
}

/// Overlapping shares: a folder shared inside a shared folder. When the
/// inner share ends its row under the store row goes and the notice is
/// shown, and everything in it is still there under the outer share,
/// the open document included.
#[test]
fn a_share_inside_another_that_ended_is_still_shown_under_the_outer_one() {
    let (store, events, _commands) = store_with_events();
    let (store_id, [recipes, trips], [_, rome]) = a_replica_with_rome_open(store);
    // Trips is inside Recipes, and shared on its own as well.
    store.get_node_signal(store_id, trips).unwrap().update(|n| n.parent_id = Some(recipes));

    events.send(shares_ended(store_id, vec![trips])).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "\"Trips\" is no longer shared with you.");
    assert_eq!(store.shown_roots(store_id), vec![recipes]);
    assert!(store.get_node_signal(store_id, trips).is_some() && store.get_node_signal(store_id, rome).is_some(), "still held: they are under Recipes");
    assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{rome}")), "and the open document stays open");
    assert!(store.panes[0].show_editor.get());
}

/// At start-up an ended share is simply not shown: no notice (the person
/// was told when it happened, or was not there), and nothing of it is
/// fetched.
#[test]
fn an_ended_share_is_not_shown_at_start_up() {
    let (store, events, commands) = store_with_events();
    let mut opened = pimble_core::Store::new_local("Shared by ann@example.com", "/tmp/anns.pimble".into());
    let (recipes, trips) = (NodeId::new(), NodeId::new());
    opened.root_node_id = recipes;
    opened.roots = vec![recipes, trips];
    opened.ended_roots = vec![trips];
    let store_id = opened.id;

    events.send(BackendEvent::StoreOpened { store: opened.clone() }).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "");
    assert_eq!(store.shown_roots(store_id), vec![recipes]);
    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    assert!(asked.iter().any(|c| asks_children(c, recipes)));
    assert!(!asked.iter().any(|c| asks_children(c, trips) || matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == trips)));

    // The same store told again with the root it was showing ended: that
    // one was on the screen, so it is said.
    let mut recipes_node = pimble_core::Node::folder("Recipes");
    recipes_node.id = recipes;
    store.upsert_node(store_id, recipes_node);
    opened.ended_roots = vec![trips, recipes];
    events.send(BackendEvent::StoreOpened { store: opened }).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "\"Recipes\" is no longer shared with you.");
    assert!(store.every_share_ended(store_id));
    assert!(store.get_node_signal(store_id, recipes).is_none());
}

/// How the store is held, asked of the server, names the ended shares
/// too: one that ended while nothing was listening leaves the explorer
/// on that answer, and one granted again comes back and is fetched.
#[test]
fn the_answer_to_how_a_store_is_held_ends_and_un_ends_shares() {
    let (store, events, commands) = store_with_events();
    let (store_id, [recipes, trips], _) = a_replica_with_rome_open(store);
    let answer = |ended: Vec<NodeId>| {
        let mut answer = sync_changed(store_id, pimble_core::StoreAccess::Full, ended.clone());
        if let BackendEvent::StoreSyncChanged { ended_roots, .. } = &mut answer {
            *ended_roots = ended;
        }
        answer
    };

    events.send(answer(vec![trips])).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "\"Trips\" is no longer shared with you.");
    assert_eq!(store.shown_roots(store_id), vec![recipes]);
    assert!(store.selected_id.get().is_none() && !store.panes[0].show_editor.get(), "the document open under it closed");
    let _ = commands.try_iter().count();

    // A share granted again: the server says the set changed and names
    // nothing that ended, and the app asks how the store is held.
    events.send(shares_ended(store_id, Vec::new())).unwrap();
    pump(store);
    assert!(commands.try_iter().any(|c| matches!(c, BackendCommand::GetStoreSync { store_id: asked_for } if asked_for == store_id)));
    assert_eq!(store.shown_roots(store_id), vec![recipes], "nothing changes until the answer");

    events.send(answer(Vec::new())).unwrap();
    pump(store);
    assert_eq!(store.shown_roots(store_id), vec![recipes, trips]);
    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == trips)), "the root that came back is fetched");
    assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == trips)));
}

/// The browser holds no replica: its backend says which shares ended and
/// then lists the store with the roots that are left. The listing drops
/// nothing twice and says nothing twice; a root that is simply gone from
/// a listing leaves without a word.
#[test]
fn a_listing_without_a_root_drops_it_and_says_nothing_more() {
    let (store, events, _commands) = store_with_events();
    let (store_id, [recipes, trips], _) = a_replica_with_rome_open(store);
    let listed = |roots: Vec<NodeId>| {
        let mut listed = store.get_store_signal(store_id).unwrap().with(|s| s.clone());
        listed.roots = roots;
        listed.ended_roots = Vec::new();
        BackendEvent::StoresListed { stores: vec![listed] }
    };

    events.send(shares_ended(store_id, vec![trips])).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "\"Trips\" is no longer shared with you.");
    store.notice.set(String::new());
    events.send(listed(vec![recipes])).unwrap();
    pump(store);
    assert_eq!(store.notice.get(), "");
    assert_eq!(store.shown_roots(store_id), vec![recipes]);
    assert_eq!(row_values(store), vec![format!("node_{store_id}_{recipes}")]);

    // The same listing again changes nothing.
    let settled = store.tree_structure_version.get();
    events.send(listed(vec![recipes])).unwrap();
    pump(store);
    assert_eq!(store.tree_structure_version.get(), settled);
}
