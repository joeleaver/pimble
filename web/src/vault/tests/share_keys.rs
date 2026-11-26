//! What a document this page creates is wrapped under, and the shares in a whole store.

use super::*;

// ── What a document this page creates is wrapped under ──────────────────

#[test]
fn a_document_a_scoped_member_creates_is_wrapped_under_the_share_key() {
    let (mut peer, root) = origin();
    let folder = NodeId::new();
    peer.add_node(folder, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    let child = NodeId::new();
    peer.add_node(child, Some(folder), Place::Last, "document", "Bread", T0).unwrap();

    // The recipient's page: the share's key, the share's documents, and
    // the shared node as its root.
    let scope = keyring_for(Some(folder));
    let share_key_id = scope.current.unwrap();
    let mut scoped = listed(folder);
    scoped.roots = vec![folder];
    let store_id = StoreId::new();
    let mut store =
        VaultStore::assemble(scoped, StoreKeys::new(scope), vec![folder], pull_subset(&peer, &[folder, child]));
    assert!(store.is_partial());
    assert_eq!(store.tree.root(), folder);

    // A node they make in the shared folder.
    let mine = NodeId::new();
    let edit = store.tree.add_node(mine, Some(folder), Place::Last, "document", "Soup", T1).unwrap();
    let outgoing = store.prepare(store_id, mine, &edit.touched[0].1).unwrap();
    let (keys, new) = outgoing.created.as_ref().expect("the server has never seen this document");
    assert_eq!(new.parent_id, Some(folder), "the append names the parent, so the new document joins the scope");
    assert_eq!(
        keys.wraps.iter().map(|w| w.scope_key_id).collect::<Vec<_>>(),
        vec![share_key_id],
        "one wrap, under the only scope key this page holds"
    );
    // The wrap opens with the share key, for this document and no other.
    let share_key = store.keys.scope.get(&share_key_id).unwrap().clone();
    let dek = unwrap_dek(&keys.wraps[0], &share_key, &dek_aad_of(store_id, mine)).unwrap();
    assert!(unwrap_dek(&keys.wraps[0], &share_key, &dek_aad_of(store_id, NodeId::new())).is_err());
    // And the blob is under that data key.
    let blob = decode_blob(outgoing.blob()).unwrap();
    assert_eq!(Blob::key_id(&blob).unwrap(), keys.dek_id);
    assert_eq!(
        Blob::decrypt(&dek, &blob_aad(&store_id.to_string(), &VaultDocId::Node(mine).as_str()), &blob).unwrap(),
        edit.touched[0].1
    );

    // The append failed, or its answer was lost: the create tried again
    // goes out under the same key and the same wraps, which are the ones
    // the server holds if the first try did land.
    store.record(mine, &outgoing, Err("the socket went away".to_string())).unwrap_err();
    let again = store.prepare(store_id, mine, &[]).unwrap();
    assert_eq!(again.created.as_ref().map(|(keys, _)| keys), Some(keys));
    assert_eq!(Blob::key_id(&decode_blob(again.blob()).unwrap()).unwrap(), keys.dek_id);

    // A document the store already has is not created again: its key is
    // its own, not a fresh one.
    let rename = store.tree.set_title(child, "Sourdough", T2).unwrap();
    assert!(store.prepare(store_id, child, &rename.touched[0].1).unwrap().created.is_none());
}

#[test]
fn an_owner_s_new_document_is_wrapped_under_the_store_key_and_every_share_above_it() {
    let (mut peer, root) = origin();
    let folder = NodeId::new();
    peer.add_node(folder, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();

    // The owner's page: the store key, and the key of the share sitting on
    // that folder, which its marker names.
    let mut scope = keyring_for(None);
    let store_key_id = scope.current.unwrap();
    let share_key_id = KeyId::new_v4();
    scope.keys.insert(share_key_id, SymmetricKey::generate());
    scope.scopes.push((Some(folder), share_key_id));

    let store_id = StoreId::new();
    let mut store = VaultStore::assemble(listed(root), StoreKeys::new(scope), Vec::new(), pull_of(&peer));
    mark_shared(&mut store.tree, folder, share_key_id, "Recipes");

    // Inside the share: both keys.
    let inside = NodeId::new();
    let edit = store.tree.add_node(inside, Some(folder), Place::Last, "document", "Bread", T1).unwrap();
    let outgoing = store.prepare(store_id, inside, &edit.touched[0].1).unwrap();
    let (keys, new) = outgoing.created.as_ref().expect("new to the server");
    assert_eq!(new.parent_id, Some(folder));
    let mut under: Vec<KeyId> = keys.wraps.iter().map(|w| w.scope_key_id).collect();
    under.sort_by_key(|id| id.to_string());
    let mut want = vec![store_key_id, share_key_id];
    want.sort_by_key(|id| id.to_string());
    assert_eq!(under, want, "the store key, and the key of the share it sits under");

    // Outside it: the store key alone.
    let outside = NodeId::new();
    let edit = store.tree.add_node(outside, Some(root), Place::Last, "document", "Notes", T1).unwrap();
    let outgoing = store.prepare(store_id, outside, &edit.touched[0].1).unwrap();
    let (keys, _) = outgoing.created.as_ref().expect("new to the server");
    assert_eq!(keys.wraps.iter().map(|w| w.scope_key_id).collect::<Vec<_>>(), vec![store_key_id]);
}

// ── The shares in a whole store ─────────────────────────────────────────

/// The keyring a fetch of one share's key answers with.
fn share_keyring(root: NodeId, key_id: KeyId, key: &SymmetricKey) -> Keyring {
    Keyring { keys: HashMap::from([(key_id, key.clone())]), current: Some(key_id), scopes: vec![(Some(root), key_id)] }
}

fn blob_under(store_id: StoreId, node_id: NodeId, dek_id: KeyId, dek: &SymmetricKey, payload: &[u8]) -> String {
    let aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
    URL_SAFE_NO_PAD.encode(Blob::encrypt(dek, dek_id, &aad, payload))
}

/// What `vaultFetch` answers for a document whose log is `payloads`, one
/// entry each from seq 1, under a fresh data key wrapped under each of
/// `under`; and that data key, for the blobs a test appends later.
fn fetched_under(
    store_id: StoreId,
    node_id: NodeId,
    payloads: &[Vec<u8>],
    under: &[(KeyId, &SymmetricKey)],
) -> (VaultFetchResponse, (KeyId, SymmetricKey)) {
    let (dek_id, dek) = scope_key();
    let aad = dek_aad_of(store_id, node_id);
    let wraps = under.iter().map(|(id, key)| wrap_dek(&dek, key, *id, &aad)).collect();
    let updates: Vec<pimble_rpc::VaultEntry> = payloads
        .iter()
        .enumerate()
        .map(|(i, payload)| pimble_rpc::VaultEntry {
            seq: i as u64 + 1,
            blob: blob_under(store_id, node_id, dek_id, &dek, payload),
            part: None,
        })
        .collect();
    let head = updates.len() as u64;
    (VaultFetchResponse { snapshot: None, snapshot_parts: Vec::new(), updates, head, keys: Some(VaultDocKeys { dek_id, wraps }), more: false, snapshot_seq: 0 }, (dek_id, dek))
}

/// An owner's page as `open_store` builds it, holding the store key only:
/// every document of `tree` but `members_docs` is wrapped under the store
/// key, and those under `share` alone, as a share's member makes them
/// while no desktop of the owner's is on to add the store key's wrap.
/// Answers the store, what the open could not open, and each document's
/// data key.
#[allow(clippy::type_complexity)]
fn owners_page(
    store_id: StoreId,
    tree: &Tree,
    members_docs: &[NodeId],
    share: (KeyId, &SymmetricKey),
) -> (VaultStore, Vec<(NodeId, VaultFetchResponse)>, HashMap<NodeId, (KeyId, SymmetricKey)>, KeyId) {
    let scope = keyring_for(None);
    let store_key_id = scope.current.unwrap();
    let store_key = scope.current_key().unwrap().clone();
    let mut deks = HashMap::new();
    let mut ids = tree.ids();
    ids.sort_by_key(|id| id.to_string());
    let fetched = ids
        .into_iter()
        .map(|id| {
            let under = if members_docs.contains(&id) { share } else { (store_key_id, &store_key) };
            let (fetched, dek) = fetched_under(store_id, id, &[tree.doc(id).unwrap().save()], &[under]);
            deks.insert(id, dek);
            (id, fetched)
        })
        .collect();
    let mut listed = listed(tree.root());
    listed.id = store_id;
    let (store, shut) = VaultStore::from_fetched(listed, StoreKeys::new(scope), Vec::new(), fetched);
    (store, shut, deks, store_key_id)
}

#[test]
fn an_owner_s_page_reads_what_a_member_made_once_it_holds_the_share_s_key() {
    let (mut peer, root) = origin();
    let (folder, bread, soup) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(folder, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    peer.add_node(bread, Some(folder), Place::Last, "document", "Bread", T0).unwrap();
    let (share_key_id, share_key) = scope_key();
    mark_shared(&mut peer, folder, share_key_id, "Recipes");
    // A member's note, made while every desktop of the owner's was off:
    // its data key is wrapped under the share's key and nothing else.
    peer.add_node(soup, Some(folder), Place::Last, "document", "Soup", T1).unwrap();

    let store_id = StoreId::new();
    let (mut store, shut, _, store_key_id) = owners_page(store_id, &peer, &[soup], (share_key_id, &share_key));

    // With the store key alone the note stays shut: not a node here, not
    // shown, and nothing about it is "repaired". The folder goes on
    // listing it, as every device that can read it needs it to.
    assert_eq!(shut.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![soup]);
    assert!(!store.tree.has_node(soup));
    assert!(store.node_of(soup).is_err());
    assert_eq!(store.children_of(folder).unwrap(), vec![bread]);
    assert!(store.repair_now(T2).is_none(), "a document not held is unknown, not missing");
    assert_eq!(store.tree.doc(folder).unwrap().children(), vec![bread, soup], "the list is untouched");
    assert_eq!(store.behind(), vec![(soup, 0)], "listed, and nothing of it read");

    // What the open does next: the folder carries a share's marker whose
    // key is not held, so that root is asked about.
    assert_eq!(store.missing_share_roots(), vec![folder]);
    assert_eq!(store.share_keys_to_ask_for(0.0), Some(vec![folder]));

    // The key arrives, the note opens from what was already fetched, and
    // it shows under its parent.
    assert!(store.take_keys(share_keyring(folder, share_key_id, &share_key)));
    assert!(!store.take_keys(share_keyring(folder, share_key_id, &share_key)), "the same key again is not news");
    let (_, fetched) = &shut[0];
    let merged = store.take_fetched(store_id, soup, fetched, 0.0);
    assert!(
        matches!(kinds_of(&merged.events)[..], [StoreChangeKind::NodeCreated { node_id, parent_id }] if node_id == soup && parent_id == folder),
        "{:?}",
        merged.events
    );
    assert_eq!(store.children_of(folder).unwrap(), vec![bread, soup]);
    assert_eq!(store.node_of(soup).unwrap().metadata.title, "Soup");
    assert!(store.behind().is_empty());
    assert!(store.missing_share_roots().is_empty());
    assert!(store.share_keys_to_ask_for(60_000.0).is_none(), "nothing left to ask for");
    assert!(store.repair_now(T2).is_none());
    assert_eq!(store.keys.scope.current, Some(store_key_id), "the store key stays the store's own");

    // A member's page is owed no key but its grants': the same marker in
    // its scope root asks for nothing.
    let mut scoped = listed(folder);
    scoped.roots = vec![folder];
    let member = VaultStore::assemble(scoped, StoreKeys::new(keyring_for(Some(folder))), vec![folder], pull_subset(&peer, &[folder, bread]));
    assert!(member.missing_share_roots().is_empty());
}

#[test]
fn a_marker_arriving_in_an_update_schedules_one_look_for_its_key() {
    let (mut peer, root) = origin();
    let folder = NodeId::new();
    peer.add_node(folder, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    let (share_key_id, share_key) = scope_key();
    let store_id = StoreId::new();
    let (mut store, shut, deks, _) = owners_page(store_id, &peer, &[], (share_key_id, &share_key));
    assert!(shut.is_empty());
    assert!(store.share_keys_to_ask_for(0.0).is_none(), "no share, nothing to ask");
    assert!(store.look.due.is_none());

    // One of the owner's desktops shares the folder while the page is
    // open: the marker reaches it as an update of the folder's document.
    let (dek_id, dek) = &deks[&folder];
    let edit = mark_shared(&mut peer, folder, share_key_id, "Recipes");
    let blob = blob_under(store_id, folder, *dek_id, dek, &edit.touched[0].1);
    let merged = store.take_blob(store_id, folder, 2, &blob, None, 1_000.0);
    assert!(merged.wants_key);
    assert_eq!(store.look.due, Some(1_000.0 + KEY_LOOK_DEBOUNCE_MS));

    // The share renamed a moment later names the same key: no second
    // reason, and the look is not put off.
    let edit = mark_shared(&mut peer, folder, share_key_id, "Family recipes");
    let blob = blob_under(store_id, folder, *dek_id, dek, &edit.touched[0].1);
    assert!(!store.take_blob(store_id, folder, 3, &blob, None, 1_100.0).wants_key);
    assert_eq!(store.look.due, Some(1_000.0 + KEY_LOOK_DEBOUNCE_MS));

    // The look asks once, and not again inside the floor however many
    // reasons arrive: it is put off until the floor has passed.
    store.look.due = None;
    assert_eq!(store.share_keys_to_ask_for(1_250.0), Some(vec![folder]));
    assert_eq!(store.share_keys_to_ask_for(5_000.0), None);
    assert_eq!(store.look.due, Some(1_250.0 + SHARE_KEY_FLOOR_MS));
    assert_eq!(store.share_keys_to_ask_for(1_250.0 + SHARE_KEY_FLOOR_MS), Some(vec![folder]));

    // Held, it is never asked for again; and a marker whose key is held
    // already wants nothing.
    assert!(store.take_keys(share_keyring(folder, share_key_id, &share_key)));
    assert!(store.share_keys_to_ask_for(60_000.0).is_none());
    store.look.due = None;
    let edit = mark_shared(&mut peer, folder, share_key_id, "Recipes");
    let blob = blob_under(store_id, folder, *dek_id, dek, &edit.touched[0].1);
    assert!(!store.take_blob(store_id, folder, 4, &blob, None, 61_000.0).wants_key);
    assert!(store.look.due.is_none());

    // A member's page holds its grants' keys and asks for no others,
    // whatever marker arrives.
    let other = KeyId::new_v4();
    let mut scoped = listed(folder);
    scoped.roots = vec![folder];
    let mut member = VaultStore::assemble(scoped, StoreKeys::new(keyring_for(Some(folder))), vec![folder], pull_subset(&peer, &[folder]));
    let edit = mark_shared(&mut peer, folder, other, "Recipes");
    let merged = member.merge(store_id, folder, vec![(Mark::One(5), Ok(edit.touched[0].1.clone()))], false, None);
    assert!(!merged.wants_key);
    assert!(member.share_keys_to_ask_for(0.0).is_none());
}

#[test]
fn a_blob_that_will_not_open_is_read_again_for_its_wraps_once() {
    let (mut peer, root) = origin();
    let folder = NodeId::new();
    peer.add_node(folder, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    let (share_key_id, share_key) = scope_key();
    mark_shared(&mut peer, folder, share_key_id, "Recipes");
    let store_id = StoreId::new();
    let (mut store, _, deks, _) = owners_page(store_id, &peer, &[], (share_key_id, &share_key));
    assert!(store.take_keys(share_keyring(folder, share_key_id, &share_key)), "held since the open");

    // A member makes a note while the page is open. The notification
    // carries the blob and never the wraps, so the key it names is one
    // this page has not heard of.
    let soup = NodeId::new();
    let edit = peer.add_node(soup, Some(folder), Place::Last, "document", "Soup", T1).unwrap();
    assert_eq!(edit.node_ids(), vec![soup, folder]);
    let (fetched, (soup_dek_id, soup_dek)) = fetched_under(store_id, soup, &[edit.touched[0].1.clone()], &[(share_key_id, &share_key)]);
    let first = &fetched.updates[0].blob;
    let merged = store.take_blob(store_id, soup, 1, first, None, 2_000.0);
    assert!(merged.events.is_empty() && !store.tree.has_node(soup));
    assert_eq!(store.look.due, Some(2_000.0 + KEY_LOOK_DEBOUNCE_MS));
    // The folder's list arrives and opens as ever; it names a document
    // not held yet, which no repair judges.
    let (folder_dek_id, folder_dek) = &deks[&folder];
    let list = blob_under(store_id, folder, *folder_dek_id, folder_dek, &edit.touched[1].1);
    assert!(store.take_blob(store_id, folder, 2, &list, None, 2_010.0).structure);
    assert!(store.repair_now(T2).is_none());

    // The look: no key to ask for, one document to read again, from the
    // start. The fetch brings the wraps, and the note opens.
    assert!(store.share_keys_to_ask_for(2_250.0).is_none());
    assert_eq!(store.reread_list(false), vec![(soup, 0)]);
    let merged = store.take_fetched(store_id, soup, &fetched, 2_300.0);
    assert!(
        matches!(kinds_of(&merged.events)[..], [StoreChangeKind::NodeCreated { node_id, parent_id }] if node_id == soup && parent_id == folder),
        "{:?}",
        merged.events
    );
    assert_eq!(store.children_of(folder).unwrap(), vec![soup]);
    assert!(store.behind().is_empty());
    // And the member's next edit opens as it arrives.
    let rename = peer.set_title(soup, "Leek soup", T2).unwrap();
    let blob = blob_under(store_id, soup, soup_dek_id, &soup_dek, &rename.touched[0].1);
    store.look.due = None;
    store.take_blob(store_id, soup, 2, &blob, None, 3_000.0);
    assert_eq!(store.node_of(soup).unwrap().metadata.title, "Leek soup");
    assert!(store.look.due.is_none(), "nothing is owed");

    // A document wrapped under nothing this page will ever hold is asked
    // about once, not once an append.
    let theirs = NodeId::new();
    let (stranger_id, stranger) = scope_key();
    let (shut, (shut_dek_id, shut_dek)) = fetched_under(store_id, theirs, &[b"one".to_vec()], &[(stranger_id, &stranger)]);
    store.take_blob(store_id, theirs, 1, &shut.updates[0].blob, None, 4_000.0);
    assert_eq!(store.reread_list(false), vec![(theirs, 0)]);
    store.look.due = None;
    store.take_fetched(store_id, theirs, &shut, 4_300.0);
    assert_eq!(store.behind(), vec![(theirs, 0)], "still shut");
    let more = blob_under(store_id, theirs, shut_dek_id, &shut_dek, b"two");
    store.take_blob(store_id, theirs, 2, &more, None, 5_000.0);
    assert!(store.look.due.is_none() && store.look.unread.is_empty(), "no share's key is missing and its wraps are known");
    // Until a scope key arrives, which may open anything: then everything
    // not read through is read again.
    assert!(store.take_keys(share_keyring(NodeId::new(), stranger_id, &stranger)));
    assert_eq!(store.reread_list(true), vec![(theirs, 0)]);
}

#[test]
fn an_owner_creating_under_a_shared_root_wraps_under_the_share_s_key_once_it_holds_it() {
    let (mut peer, root) = origin();
    let folder = NodeId::new();
    peer.add_node(folder, Some(root), Place::Last, "folder", "Recipes", T0).unwrap();
    let (share_key_id, share_key) = scope_key();
    mark_shared(&mut peer, folder, share_key_id, "Recipes");
    let store_id = StoreId::new();
    let (mut store, _, _, store_key_id) = owners_page(store_id, &peer, &[], (share_key_id, &share_key));
    let wraps_of = |store: &mut VaultStore, title: &str| {
        let id = NodeId::new();
        let edit = store.tree.add_node(id, Some(folder), Place::Last, "document", title, T1).unwrap();
        let outgoing = store.prepare(store_id, id, &edit.touched[0].1).unwrap();
        let (keys, _) = outgoing.created.expect("new to the server");
        let mut under: Vec<KeyId> = keys.wraps.iter().map(|w| w.scope_key_id).collect();
        under.sort_by_key(|id| id.to_string());
        under
    };

    // Before the share's key has reached the page there is only the store
    // key to wrap under; the owner's desktops add the share's.
    assert_eq!(wraps_of(&mut store, "Bread"), vec![store_key_id]);

    // Once it is in the keyring, the marker above the new node finds it:
    // the members read what the owner makes with no desktop in between.
    assert!(store.take_keys(share_keyring(folder, share_key_id, &share_key)));
    let mut both = vec![store_key_id, share_key_id];
    both.sort_by_key(|id| id.to_string());
    assert_eq!(wraps_of(&mut store, "Soup"), both);

    // Outside the share: the store key alone, as before.
    let outside = NodeId::new();
    let edit = store.tree.add_node(outside, Some(root), Place::Last, "document", "Notes", T1).unwrap();
    let outgoing = store.prepare(store_id, outside, &edit.touched[0].1).unwrap();
    let (keys, _) = outgoing.created.expect("new to the server");
    assert_eq!(keys.wraps.iter().map(|w| w.scope_key_id).collect::<Vec<_>>(), vec![store_key_id]);
}
