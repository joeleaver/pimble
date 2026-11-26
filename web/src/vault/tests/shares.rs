//! A share is a scope of somebody else's store: access, scope roots, ended shares, refusals.

use super::*;

// ── A share is a scope of somebody else's store ─────────────────────────

#[test]
fn the_account_s_grants_decide_a_store_s_access_and_roots() {
    let (a, b) = (NodeId::new(), NodeId::new());

    // One's own store: one whole-store grant, nothing scoped.
    let mine = row(vec![grant(None, "owner", "Family")], None);
    assert_eq!(mine.access(), StoreAccess::Full);
    assert!(mine.roots().is_empty(), "a whole store has no scope roots");
    assert_eq!(mine.name(), None, "the server's name stands for a whole store");

    // A reader of one folder: read-only, one root, the share's own name.
    let reading = row(vec![grant(Some(a), "reader", "Recipes")], Some("ann@example.com"));
    assert_eq!(reading.access(), StoreAccess::Read);
    assert_eq!(reading.roots(), vec![a]);
    assert_eq!(reading.name().as_deref(), Some("Recipes"));
    assert_eq!(reading.shared_by.as_deref(), Some("ann@example.com"));

    // A reader of one folder and an editor of another: `Full`, because a
    // store-wide `Read` would take away the editing they do have. The
    // server refuses the writes under the reader's root one at a time.
    let mixed = row(vec![grant(Some(a), "reader", "Recipes"), grant(Some(b), "editor", "Plans")], Some("ann@example.com"));
    assert_eq!(mixed.access(), StoreAccess::Full);
    assert_eq!(mixed.roots(), vec![a, b], "both shares are roots of the same store");
    assert_eq!(mixed.name().as_deref(), Some("Shared by ann@example.com"), "several shares: the desktop's words, not the first share's name");

    // A whole-store grant beside a share covers everything, so the shares
    // stop being scopes at all.
    let both = row(vec![grant(Some(a), "reader", "Recipes"), grant(None, "editor", "Family")], None);
    assert_eq!(both.access(), StoreAccess::Full);
    assert!(both.roots().is_empty());
    assert_eq!(both.name(), None);

    // Nothing known about a store is not a reason to refuse writes: the
    // server is the authority and answers with the sentence itself.
    assert_eq!(AccountStore::default().access(), StoreAccess::Full);
}

#[test]
fn a_listed_store_reaches_the_ui_as_the_share_it_is() {
    let root = NodeId::new();
    let store_id = StoreId::new();
    let mut client = VaultClient::new("me".to_string());
    client.rows.insert(
        store_id,
        row(vec![grant(Some(root), "reader", "Recipes")], Some("ann@example.com")),
    );

    // What the hosted server lists: the owner's store name, which a
    // recipient must never see.
    let mut listed = Store::new_local("Ann's whole life", PathBuf::new());
    listed.id = store_id;
    listed.kind = StoreKind::Vault;
    client.describe(&mut listed);

    assert_eq!(listed.name, "Recipes", "the share's own name, never the owner's store's");
    assert_eq!(listed.shared_by.as_deref(), Some("ann@example.com"));
    assert_eq!(listed.access, StoreAccess::Read);
    assert_eq!(listed.roots, vec![root]);
    assert_eq!(listed.root_node_id, root, "older callers read the first root");

    // Waiting for the key says so on the row, and only once it is waiting.
    assert!(!listed.name.ends_with(WAITING_SUFFIX));
    client.waiting.insert(store_id, Waiting { listed: listed.clone(), due: 0.0 });
    let mut again = listed.clone();
    again.name = "Ann's whole life".to_string();
    client.describe(&mut again);
    assert_eq!(again.name, format!("Recipes{WAITING_SUFFIX}"));
}

// ── A scope root is a root, not an orphan ───────────────────────────────

#[test]
fn repair_leaves_a_scope_root_whose_parent_is_not_held_where_it_is() {
    let (mut peer, root) = origin();
    let (recipes, plans, elsewhere) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(recipes, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    peer.add_node(plans, Some(root), Place::Last, "folder", "Plans", T0).unwrap();
    peer.add_node(elsewhere, Some(root), Place::Last, "folder", "Elsewhere", T0).unwrap();
    let (bread, monday, stray) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(bread, Some(recipes), Place::Last, "document", "Bread", T0).unwrap();
    peer.add_node(monday, Some(plans), Place::Last, "document", "Monday", T0).unwrap();
    peer.add_node(stray, Some(elsewhere), Place::Last, "document", "Stray", T0).unwrap();

    // Two shares of one store, and — as can happen while an owner's move
    // and their scope publish are in flight — one document that leads to
    // neither.
    let mut scoped = listed(recipes);
    scoped.roots = vec![recipes, plans];
    let mut store = VaultStore::assemble(
        scoped,
        StoreKeys::new(keyring_for(Some(recipes))),
        vec![recipes, plans],
        pull_subset(&peer, &[recipes, bread, plans, monday, stray]),
    );

    assert!(store.repair_now(T1).is_none(), "a consistent scope needs no repair");
    for (scope_root, parent) in [(recipes, root), (plans, root)] {
        assert_eq!(
            store.tree.get_node_info(scope_root).unwrap().parent_id,
            Some(parent),
            "a scope root's parent is the owner's folder and is never rewritten"
        );
    }
    assert_eq!(store.children_of(recipes).unwrap(), vec![bread]);
    assert_eq!(store.children_of(plans).unwrap(), vec![monday]);
    assert_eq!(
        store.tree.get_node_info(stray).unwrap().parent_id,
        Some(elsewhere),
        "a document that leads to no scope root is not adopted into one"
    );
    assert!(store.tree.get_children(recipes).unwrap().iter().all(|id| *id != stray));

    // Until 2026-09-21 a repair that took these for a whole store judged
    // all three (the scope roots as orphans, `stray` adopted). No repair
    // judges what it does not hold any more, rooted or not: a parent that
    // is not here is unknown, not missing.
    let mut whole = VaultStore::assemble(
        listed(recipes),
        StoreKeys::new(keyring()),
        Vec::new(),
        pull_subset(&peer, &[recipes, bread, plans, monday, stray]),
    );
    assert!(whole.repair_now(T1).is_none(), "nothing known is wrong");
}

/// The page assembles its own nodes, so it is the one to say what the
/// account may change of each (`Node::access`), by the hosted server's
/// rule: the role of the shared root a node is under, the wider role
/// where two roots cover it, a whole-store grant over any share. The app
/// believes it: a document under a reader's root takes no typing, in a
/// store whose other root is edited.
#[test]
fn an_assembled_node_says_what_the_account_may_change_of_it() {
    use StoreAccess::{Full, Read};
    let (mut peer, root) = origin();
    let (recipes, plans, inner) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(recipes, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    peer.add_node(plans, Some(root), Place::Last, "folder", "Plans", T0).unwrap();
    // A folder the account edits inside the one it reads.
    peer.add_node(inner, Some(recipes), Place::Last, "folder", "Ours", T0).unwrap();
    let (bread, monday, shared_list) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(bread, Some(recipes), Place::Last, "document", "Bread", T0).unwrap();
    peer.add_node(monday, Some(plans), Place::Last, "document", "Monday", T0).unwrap();
    peer.add_node(shared_list, Some(inner), Place::Last, "document", "Shopping", T0).unwrap();

    let store_id = StoreId::new();
    let mut scoped = listed(recipes);
    scoped.id = store_id;
    scoped.roots = vec![recipes, plans, inner];
    let held = [recipes, bread, plans, monday, inner, shared_list];
    let store = VaultStore::assemble(scoped.clone(), StoreKeys::new(keyring_for(Some(recipes))), vec![recipes, plans, inner], pull_subset(&peer, &held));
    let mixed = row(
        vec![grant(Some(recipes), "reader", "Recipes"), grant(Some(plans), "editor", "Plans"), grant(Some(inner), "editor", "Ours")],
        Some("ann@example.com"),
    );
    assert_eq!(mixed.access(), Full, "something here may be written");
    assert_eq!(mixed.read_only_roots(), vec![recipes]);

    let access = |id: NodeId, row: &AccountStore| store.node_for(id, Some(row)).unwrap().access;
    for (id, expected, what) in [
        (recipes, Read, "the root it reads"),
        (bread, Read, "a document under it"),
        (plans, Full, "the root it edits"),
        (monday, Full, "a document under it"),
        (inner, Full, "an edited root inside the read one"),
        (shared_list, Full, "in both scopes: the wider role"),
    ] {
        assert_eq!(access(id, &mixed), expected, "{what}");
    }

    // Every grant a reader's: the store's own word. A whole-store grant
    // beside the shares answers for every node, either way.
    let reading = row(vec![grant(Some(recipes), "reader", "Recipes"), grant(Some(plans), "reader", "Plans")], None);
    assert!(held.iter().all(|id| access(*id, &reading) == Read));
    assert!(reading.read_only_roots().is_empty(), "one answer covers the store");
    let whole_editor = row(vec![grant(Some(recipes), "reader", "Recipes"), grant(None, "editor", "Family")], None);
    assert!(held.iter().all(|id| access(*id, &whole_editor) == Full));
    let whole_reader = row(vec![grant(Some(plans), "editor", "Plans"), grant(None, "reader", "Family")], None);
    assert!(held.iter().all(|id| access(*id, &whole_reader) == Read));
    // Nothing known about the store disables nothing.
    assert_eq!(store.node_for(bread, None).unwrap().access, Full);

    // What the app is handed: both answers, and the store described with
    // the roots only read.
    let mut client = VaultClient::new("me".to_string());
    client.stores.insert(store_id, store);
    client.rows.insert(store_id, mixed);
    let children = client.children(store_id, recipes).expect("children of a held root");
    let mut listed_access: Vec<(NodeId, StoreAccess)> = children.iter().map(|n| (n.id, n.access)).collect();
    listed_access.sort_by_key(|(id, _)| *id != bread);
    assert_eq!(listed_access, vec![(bread, Read), (inner, Full)]);
    let node = client.node(store_id, bread).expect("a held node");
    assert_eq!(node.access, Read);
    let mut described = scoped;
    client.describe(&mut described);
    assert_eq!((described.access, described.read_only_roots), (Full, vec![recipes]));
}

/// A commenter (docs/DISCUSSIONS_CONTRACT.md section 7) reads the notes
/// and writes discussions: a node under a commenter's root says
/// `Comment`, a discussion on it is the commenter's to write, the store
/// is `Comment` when nothing in it is edited, and the write gate lets a
/// new discussion and a discussion's edits through and nothing else.
#[test]
fn a_commenter_writes_discussions_and_nothing_else() {
    use StoreAccess::{Comment, Full, Read};
    let (mut peer, root) = origin();
    let (recipes, bread, plans) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(recipes, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    peer.add_node(bread, Some(recipes), Place::Last, "document", "Bread", T0).unwrap();
    peer.add_node(plans, Some(root), Place::Last, "folder", "Plans", T0).unwrap();
    let talk = NodeId::new();
    peer.add_attachment(talk, bread, node_types::DISCUSSION, "Crust?", None, T0).unwrap();

    let store_id = StoreId::new();
    let mut scoped = listed(recipes);
    scoped.id = store_id;
    scoped.roots = vec![recipes];
    let held = [recipes, bread, talk];
    let store = VaultStore::assemble(scoped.clone(), StoreKeys::new(keyring_for(Some(recipes))), vec![recipes], pull_subset(&peer, &held));
    let commenting = row(vec![grant(Some(recipes), "commenter", "Recipes")], Some("ann@example.com"));
    assert_eq!(commenting.access(), Comment);
    assert_eq!(commenting.read_only_roots(), vec![recipes], "the roots say which are a commenter's");
    let access = |id: NodeId, row: &AccountStore| store.node_for(id, Some(row)).unwrap().access;
    assert_eq!(access(recipes, &commenting), Comment);
    assert_eq!(access(bread, &commenting), Comment);
    assert_eq!(access(talk, &commenting), Full, "a discussion is the commenter's to write");
    let reading = row(vec![grant(Some(recipes), "reader", "Recipes")], None);
    assert_eq!(access(talk, &reading), Read, "a reader writes no discussion");
    let mixed = row(vec![grant(Some(recipes), "commenter", "Recipes"), grant(Some(plans), "editor", "Plans")], None);
    assert_eq!(mixed.access(), Full);
    assert_eq!(access(bread, &mixed), Comment);
    let whole = row(vec![grant(None, "commenter", "Family")], None);
    assert_eq!(whole.access(), Comment);
    assert!(whole.read_only_roots().is_empty());

    let talk_state = store.tree.doc(talk).unwrap().save();
    let bread_state = store.tree.doc(bread).unwrap().save();
    let mut client = VaultClient::new("me".to_string());
    client.stores.insert(store_id, store);
    client.rows.insert(store_id, commenting);
    // A new discussion passes the gate (it is then made through the
    // endpoint, which these tests do not have).
    let init = |node_type: &str| pimble_app::protocol::NodeInit { node_type: node_type.to_string(), content: Vec::new(), custom: Vec::new(), request: None };
    let discussion = BackendCommand::CreateAttachment { store_id, host_id: bread, title: "Why?".into(), init: init(node_types::DISCUSSION) };
    assert!(refuse_write(&client, store_id, &discussion).is_none());
    // The router's gate, then the vault judging the document an edit is
    // of, as the backend loop asks them.
    let mut refused = |cmd: BackendCommand| match route_through(&mut client, cmd).unwrap_or_default().into_iter().next() {
        Some(BackendEvent::Error { message }) => Some(message),
        _ => None,
    };
    assert_eq!(refused(BackendCommand::ApplyUpdate { store_id, node_id: talk, update: talk_state }), None);
    let comment_only = Some(StoreAccess::COMMENT_ONLY_REFUSAL.to_string());
    assert_eq!(refused(BackendCommand::ApplyUpdate { store_id, node_id: bread, update: bread_state }), comment_only);
    assert_eq!(refused(BackendCommand::RenameNode { store_id, node_id: bread, title: "No".into() }), comment_only);
    assert_eq!(refused(BackendCommand::CreateNode { store_id, parent_id: Some(recipes), title: "No".into(), init: None, place: Place::Last }), comment_only);
    let mut described = scoped;
    client.describe(&mut described);
    assert_eq!((described.access, described.read_only_roots), (Comment, vec![recipes]));
}

// ── A share that ended (Joe, 2026-09-21) ────────────────────────────────

/// What ended between two readings of the account's store list.
#[test]
fn the_shares_the_rows_no_longer_name_have_ended() {
    let (store_id, other) = (StoreId::new(), StoreId::new());
    let (recipes, plans) = (NodeId::new(), NodeId::new());
    let both = row(vec![grant(Some(recipes), "editor", "Recipes"), grant(Some(plans), "reader", "Plans")], Some("ann@example.com"));
    let one = row(vec![grant(Some(plans), "reader", "Plans")], Some("ann@example.com"));
    let whole = row(vec![grant(None, "editor", "Family")], None);
    let rows = |entries: Vec<(StoreId, &AccountStore)>| entries.into_iter().map(|(id, row)| (id, row.clone())).collect::<HashMap<_, _>>();

    assert!(shares_ended(&rows(vec![(store_id, &both)]), &rows(vec![(store_id, &both)])).is_empty());
    assert_eq!(
        shares_ended(&rows(vec![(store_id, &both)]), &rows(vec![(store_id, &one)])),
        vec![EndedShares { store_id, roots: vec![recipes], every: false }]
    );
    assert_eq!(
        shares_ended(&rows(vec![(store_id, &both), (other, &whole)]), &rows(vec![(other, &whole)])),
        vec![EndedShares { store_id, roots: vec![recipes, plans], every: true }],
        "no row names the store at all: every share of it ended"
    );
    assert!(shares_ended(&rows(vec![(store_id, &one)]), &rows(vec![(store_id, &both)])).is_empty(), "another share arrived: nothing ended");
    assert!(shares_ended(&rows(vec![(store_id, &both)]), &rows(vec![(store_id, &whole)])).is_empty(), "held whole now: nothing of it was lost");
    assert!(shares_ended(&rows(vec![(other, &whole)]), &HashMap::new()).is_empty(), "a store held whole was never a share");
}

/// The page holds no replica: a share the rows stop naming is said to
/// the UI, its documents are dropped from memory, and what is left of
/// the store goes on; when it was the last, the store is closed.
#[test]
fn a_share_that_ended_is_said_and_dropped_and_the_last_one_closes_the_store() {
    let (mut peer, root) = origin();
    let (recipes, plans) = (NodeId::new(), NodeId::new());
    peer.add_node(recipes, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    peer.add_node(plans, Some(root), Place::Last, "folder", "Plans", T0).unwrap();
    let (bread, monday) = (NodeId::new(), NodeId::new());
    peer.add_node(bread, Some(recipes), Place::Last, "document", "Bread", T0).unwrap();
    peer.add_node(monday, Some(plans), Place::Last, "document", "Monday", T0).unwrap();

    let store_id = StoreId::new();
    let mut scoped = listed(recipes);
    scoped.id = store_id;
    scoped.roots = vec![recipes, plans];
    let store = VaultStore::assemble(scoped.clone(), StoreKeys::new(keyring_for(Some(recipes))), vec![recipes, plans], pull_subset(&peer, &[recipes, bread, plans, monday]));
    let both = row(vec![grant(Some(recipes), "editor", "Recipes"), grant(Some(plans), "editor", "Plans")], Some("ann@example.com"));
    let mut client = VaultClient::new("me".to_string());
    client.stores.insert(store_id, store);
    client.rows.insert(store_id, both.clone());

    // The same list again: nothing to say.
    assert!(client.take_rows(HashMap::from([(store_id, both)])).is_empty());

    // Removed from Recipes, the tree's first root.
    let plans_only = row(vec![grant(Some(plans), "editor", "Plans")], Some("ann@example.com"));
    let events = client.take_rows(HashMap::from([(store_id, plans_only.clone())]));
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(&events[0], BackendEvent::RemoteStoreChange { store_id: said, change_kind: StoreChangeKind::SharesEnded { node_ids }, source_client_id: None } if *said == store_id && node_ids == &vec![recipes]));
    let open = client.stores.get(&store_id).expect("the share that is left keeps the store open");
    assert_eq!(open.scope_roots, vec![plans]);
    assert!(open.is_partial());
    assert_eq!(open.tree.root(), plans, "the tree starts from a root that is held");
    for gone in [recipes, bread] {
        assert!(open.tree.doc(gone).is_none() && !open.docs.contains_key(&gone) && !open.heads.contains_key(&gone), "{gone} is still in memory");
    }
    assert!(open.tree.doc(monday).is_some() && open.tree.doc(plans).is_some());
    let mut described = scoped.clone();
    client.describe(&mut described);
    assert_eq!((described.roots.clone(), described.root_node_id), (vec![plans], plans));
    assert!(client.node(store_id, bread).is_err(), "nothing of it is answered for");
    assert!(client.node(store_id, monday).is_ok());

    // Removed from the last one: said, and the store closed.
    let events = client.take_rows(HashMap::new());
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(&events[0], BackendEvent::RemoteStoreChange { change_kind: StoreChangeKind::SharesEnded { node_ids }, .. } if node_ids == &vec![plans]));
    assert!(matches!(&events[1], BackendEvent::StoreClosed { store_id: closed } if *closed == store_id));
    assert!(!client.owns(store_id), "nothing of it is kept");
    // Its endpoint, if the session still names one, failing afterwards
    // is not its owner being offline: the row does not come back.
    assert!(client.endpoint_down(&[store_id]).is_empty());
    // Invited again: the list names it, and it is a store like any other.
    assert!(client.take_rows(HashMap::from([(store_id, plans_only)])).is_empty());
    assert!(!client.endpoint_down(&[store_id]).is_empty());
}

/// Overlapping shares: a document also under a share that is left stays.
#[test]
fn what_is_also_under_a_share_that_is_left_is_kept() {
    let (mut peer, root) = origin();
    let (outer, inner, leaf) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(outer, Some(root), Place::Last, "folder", "Outer", T0).unwrap();
    peer.add_node(inner, Some(outer), Place::Last, "folder", "Inner", T0).unwrap();
    peer.add_node(leaf, Some(inner), Place::Last, "document", "Leaf", T0).unwrap();
    let mut scoped = listed(inner);
    scoped.roots = vec![inner, outer];
    let mut store = VaultStore::assemble(scoped, StoreKeys::new(keyring_for(Some(outer))), vec![inner, outer], pull_subset(&peer, &[outer, inner, leaf]));

    assert!(store.drop_roots(&[inner]));
    assert_eq!(store.scope_roots, vec![outer]);
    assert!(store.tree.doc(inner).is_some() && store.tree.doc(leaf).is_some(), "still in the outer share");
    assert_eq!(store.tree.root(), outer);
    assert!(!store.drop_roots(&[outer]), "the last root is not this function's to drop");
    assert_eq!(store.scope_roots, vec![outer]);
}

#[test]
fn a_scope_that_lists_a_document_not_held_yet_is_not_judged_yet() {
    let (mut peer, root) = origin();
    let recipes = NodeId::new();
    peer.add_node(recipes, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    let (bread, coming) = (NodeId::new(), NodeId::new());
    peer.add_node(bread, Some(recipes), Place::Last, "document", "Bread", T0).unwrap();
    // A document somebody else created a moment ago: the folder's list
    // names it and its own document has not arrived.
    peer.add_node(coming, Some(recipes), Place::Last, "document", "Soup", T1).unwrap();

    let mut scoped = listed(recipes);
    scoped.roots = vec![recipes];
    let mut store = VaultStore::assemble(
        scoped,
        StoreKeys::new(keyring_for(Some(recipes))),
        vec![recipes],
        pull_subset(&peer, &[recipes, bread]),
    );
    assert!(
        store.repair_now(T1).is_none(),
        "removing that entry would delete a child from the owner's folder"
    );
    assert!(store.tree.doc(recipes).unwrap().children().contains(&coming));

    // Once it is here, the scope is judged again and has nothing to fix.
    store.tree.apply_update(coming, &peer.doc(coming).unwrap().save()).unwrap();
    assert!(store.repair_now(T2).is_none());
    assert_eq!(store.children_of(recipes).unwrap(), vec![bread, coming]);
}

// ── Refusals ────────────────────────────────────────────────────────────

#[test]
fn a_reader_s_write_is_refused_here_with_no_request_made() {
    let store_id = StoreId::new();
    let node_id = NodeId::new();
    let mut client = VaultClient::new("me".to_string());
    client.rows.insert(store_id, row(vec![grant(Some(NodeId::new()), "reader", "Recipes")], Some("ann@example.com")));

    let refused = refuse_write(&client, store_id, &BackendCommand::RenameNode { store_id, node_id, title: "No".into() })
        .expect("a reader may not rename");
    assert!(
        matches!(&refused, BackendEvent::Error { message } if message == StoreAccess::READ_ONLY_REFUSAL),
        "the sentence alone, as the person sees it: {refused:?}"
    );
    // Reads are not refused, and neither is anything on a store this
    // account edits.
    assert!(refuse_write(&client, store_id, &BackendCommand::GetNode { store_id, node_id }).is_none());
    assert!(refuse_write(&client, StoreId::new(), &BackendCommand::DeleteNode { store_id, node_id }).is_none());
}

#[test]
fn a_refused_append_leaves_nothing_pending_and_nothing_local() {
    let (mut peer, root) = origin();
    let x = NodeId::new();
    peer.add_node(x, Some(root), Place::Last, "document", "x", T0).unwrap();
    let store_id = StoreId::new();
    let mut store = opened(&peer);

    // An edit whose append the server refuses — a reader's root inside a
    // store this account edits elsewhere, say.
    let edit = store.tree.set_title(x, "Mine now", T1).unwrap();
    let outgoing = store.prepare(store_id, x, &edit.touched[0].1).unwrap();
    assert!(store.record(x, &outgoing, Err("Forbidden: no grant for this document".into())).is_err());
    assert!(store.docs[&x].unsent, "an ordinary failure is resent");

    // The refusal path drops the document instead, so the pull that
    // follows starts from the beginning and ends at what the server holds.
    store.refused(x);
    assert!(store.tree.doc(x).is_none(), "the local document goes");
    assert!(!store.docs.contains_key(&x), "with its cursor and its unsent flag");
    assert!(!store.heads.contains_key(&x), "so the next fetch asks from 0");
    assert!(store.keys.docs.wraps(x).is_none());
}
