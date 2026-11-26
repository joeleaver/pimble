//! A twin built again, and which key opens a blob.

use super::*;

// ── A twin built again ──────────────────────────────────────────────────

/// A relayed store's twin is disposable: built again, its logs start from
/// 1 under a new epoch, and a page that kept its cursors would skip them.
/// The first epoch heard is only remembered, the same one again changes
/// nothing, and a server that names none concludes nothing.
#[test]
fn a_new_epoch_makes_the_page_forget_what_it_read() {
    let (mut peer, root) = origin();
    peer.add_node(NodeId::new(), Some(root), Place::Last, "document", "Pasta", T0).unwrap();
    let store_id = StoreId::new();
    let mut store = opened(&peer);
    let child = peer.get_children(root).unwrap()[0];
    store.keys.docs.note_listed(child, KeyId::new_v4());

    // An edit this page made and the old twin took, moments before its
    // owner's computer went off: the owner's own store never had it, so
    // the twin built again from that store does not either.
    let edit = store.tree.set_title(root, "Renamed here", T1).unwrap();
    let outgoing = store.prepare(store_id, root, &edit.touched[0].1).unwrap();
    store.record(root, &outgoing, Ok(2)).unwrap();
    let read_to = store.docs[&root].cursor.applied_through();
    let known = store.docs[&root].known_sv.clone();
    assert_eq!(read_to, 2);
    assert_eq!(known, store.tree.doc(root).unwrap().state_vector());

    assert!(!store.take_epoch(None, None));
    assert!(!store.take_epoch(Some("2026-09-21T10:00:00Z"), None), "the first epoch heard is remembered, nothing more");
    assert!(!store.take_epoch(Some("2026-09-21T10:00:00Z"), None));
    assert!(!store.take_epoch(None, None));
    assert_eq!(store.docs[&root].cursor.applied_through(), read_to);
    assert_eq!(store.docs[&root].known_sv, known);
    assert!(!store.docs[&root].unsent);

    // The twin was built again.
    assert!(store.take_epoch(Some("2026-09-21T11:30:00Z"), None));
    for id in peer.ids() {
        let doc = &store.docs[&id];
        assert_eq!(doc.cursor.applied_through(), 0, "{id} would be read from where the old log was read to");
        assert_eq!(doc.known_sv, pimble_crdt::empty_state_vector(), "{id}: the old twin's holdings are believed of the new one");
        assert_eq!(doc.snapshot_seq, 0);
        assert!(doc.unsent, "{id} is not offered to the new twin");
    }
    assert!(store.heads.is_empty());
    assert!(store.keys.docs.is_empty());
    assert!(store.keys.scope.current.is_some(), "the account's own keys went with the twin's");
    assert_eq!(store.tree.doc(root).unwrap().fields().unwrap().title, "Renamed here", "the notes are this page's, not the twin's");
    // Remembered: the same epoch again is not another rebuild.
    assert!(!store.take_epoch(Some("2026-09-21T11:30:00Z"), None));

    // Read again from the start (the new logs are one entry each, as the
    // owner's link pushed them): what this page already holds merges to
    // nothing and says nothing to the UI, and the cursors are the new
    // logs' own.
    for id in peer.ids() {
        let merged = store.merge(store_id, id, vec![(Mark::Through(1), Ok(peer.doc(id).unwrap().save()))], false, None);
        assert!(merged.events.is_empty() && !merged.structure, "{id}: a merge repeated said something");
        store.heads.insert(id, 1);
        assert_eq!(store.docs[&id].cursor.applied_through(), 1, "{id}");
    }

    // Offered again: everything beyond what the new twin is now known to
    // hold, which brings it the edit only this page had.
    let outgoing = store.prepare(store_id, root, &[]).unwrap();
    assert!(outgoing.resend);
    assert!(outgoing.created.is_none(), "the new twin lists it: an append must not try to create it");
    let mut twin = NodeDoc::load(&peer.doc(root).unwrap().save()).unwrap();
    assert_eq!(twin.fields().unwrap().title, "Vault");
    twin.apply_update(&outgoing.payload).unwrap();
    assert_eq!(twin.fields().unwrap().title, "Renamed here", "what only this page held was lost with the old twin");
    assert_eq!(store.record(root, &outgoing, Ok(2)).unwrap(), None);
    assert!(!store.docs[&root].unsent);
    assert_eq!(store.docs[&root].known_sv, store.tree.doc(root).unwrap().state_vector());
}

/// What this account may only read is read again and never offered: the
/// server would refuse it, and a refused append throws the local copy
/// away.
#[test]
fn a_new_epoch_offers_only_what_the_account_may_write() {
    let (peer, root) = origin();
    let mut reader = VaultStore::assemble(listed(root), store_keys(), vec![root], pull_of(&peer));
    let reads = relayed_row(vec![grant(Some(root), "reader", "Recipes")], Some("ann@example.com"));
    reader.take_epoch(Some("a"), Some(&reads));
    assert!(reader.take_epoch(Some("b"), Some(&reads)));
    for id in peer.ids() {
        assert_eq!(reader.docs[&id].cursor.applied_through(), 0);
        assert!(!reader.docs[&id].unsent, "a reader's page offered {id}");
    }

    let mut editor = VaultStore::assemble(listed(root), store_keys(), vec![root], pull_of(&peer));
    let edits = relayed_row(vec![grant(Some(root), "editor", "Recipes")], Some("ann@example.com"));
    editor.take_epoch(Some("a"), Some(&edits));
    assert!(editor.take_epoch(Some("b"), Some(&edits)));
    assert!(peer.ids().iter().all(|id| editor.docs[id].unsent));
}

// ── Which key opens a blob ──────────────────────────────────────────────

#[test]
fn a_blob_s_key_is_the_document_s_data_key_then_a_scope_key() {
    let store_id = StoreId::new();
    let node_id = NodeId::new();
    let doc = VaultDocId::Node(node_id);
    let aad = blob_aad(&store_id.to_string(), &doc.as_str());

    // This page holds one share's key and not the store's.
    let share_root = NodeId::new();
    let share = keyring_for(Some(share_root));
    let share_key_id = share.current.unwrap();
    let share_key = share.current_key().unwrap().clone();
    let mut keys = StoreKeys::new(share);

    // A document whose data key is wrapped under the share key, and under
    // a store key this page does not hold.
    let dek = SymmetricKey::generate();
    let dek_id = KeyId::new_v4();
    let dek_aad = dek_aad(&store_id.to_string(), &doc.as_str());
    let stranger = SymmetricKey::generate();
    let wraps = VaultDocKeys {
        dek_id,
        wraps: vec![
            wrap_dek(&dek, &stranger, KeyId::new_v4(), &dek_aad),
            wrap_dek(&dek, &share_key, share_key_id, &dek_aad),
        ],
    };
    keys.note_wraps(node_id, Some(wraps));

    let blob = URL_SAFE_NO_PAD.encode(Blob::encrypt(&dek, dek_id, &aad, b"under the data key"));
    assert_eq!(keys.decrypt_blob(store_id, node_id, &blob).unwrap(), b"under the data key");
    assert!(keys.docs.holds_data_key(node_id), "an unwrapped data key is kept");
    // And an outgoing blob goes under the same data key.
    assert_eq!(keys.encrypt_key(store_id, node_id).unwrap().0, dek_id);

    // A blob from before data keys names a scope key in its header; the
    // third step of the order finds it.
    let other = NodeId::new();
    let old_aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(other).as_str());
    let old = URL_SAFE_NO_PAD.encode(Blob::encrypt(&share_key, share_key_id, &old_aad, b"before data keys"));
    assert_eq!(keys.decrypt_blob(store_id, other, &old).unwrap(), b"before data keys");
    assert_eq!(
        keys.encrypt_key(store_id, other).unwrap().0,
        share_key_id,
        "a document with no data key keeps being written under the scope key"
    );

    // A data key wrapped under nothing this page holds is not a failure to
    // report, only a document that stays shut until the wrap arrives.
    let shut = NodeId::new();
    let shut_dek = SymmetricKey::generate();
    let shut_dek_id = KeyId::new_v4();
    let shut_doc = VaultDocId::Node(shut);
    let shut_dek_aad = dek_aad_of(store_id, shut);
    keys.note_wraps(
        shut,
        Some(VaultDocKeys { dek_id: shut_dek_id, wraps: vec![wrap_dek(&shut_dek, &stranger, KeyId::new_v4(), &shut_dek_aad)] }),
    );
    let shut_blob = URL_SAFE_NO_PAD.encode(Blob::encrypt(
        &shut_dek,
        shut_dek_id,
        &blob_aad(&store_id.to_string(), &shut_doc.as_str()),
        b"not for us",
    ));
    assert!(keys.decrypt_blob(store_id, shut, &shut_blob).is_err());
    assert!(keys.encrypt_key(store_id, shut).is_none(), "and nothing is written to it either");
}
