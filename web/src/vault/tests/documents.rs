//! The tree from documents, sizes and parts, and what a tree edit appends.

use super::*;

// ── The tree from documents ─────────────────────────────────────────────

#[test]
fn the_tree_is_built_from_the_documents_and_follows_their_updates() {
    let (mut peer, root) = origin();
    let (folder, x, y) = (NodeId::new(), NodeId::new(), NodeId::new());
    peer.add_node(folder, Some(root), Place::Last, "folder", "F", T0).unwrap();
    peer.add_node(x, Some(root), Place::Last, "document", "x", T0).unwrap();
    peer.add_node(y, Some(folder), Place::Last, "document", "y", T0).unwrap();

    let store_id = StoreId::new();
    let mut store = opened(&peer);
    assert_eq!(store.tree.root(), root);
    assert_eq!(store.children_of(root).unwrap(), vec![folder, x]);
    assert_eq!(store.children_of(folder).unwrap(), vec![y]);
    assert!(store.tree.validate_tree().is_empty());
    assert!(store.repair_now(T1).is_none(), "a consistent pull needs no repair");
    for id in [root, folder, x, y] {
        assert_eq!(store.docs[&id].cursor.applied_through(), 1, "each document's cursor covers its snapshot");
    }

    let mut seq = 10;
    // A create arriving as two document updates.
    let z = NodeId::new();
    let edit = peer.add_node(z, Some(folder), Place::First, "document", "z", T1).unwrap();
    arrive(&mut store, store_id, &edit, &mut seq, None);
    assert_eq!(store.children_of(folder).unwrap(), vec![z, y]);
    assert_eq!(store.node_of(z).unwrap().metadata.title, "z");

    // A move: three documents' updates.
    let edit = peer.move_node(y, root, Place::Last, T1).unwrap();
    arrive(&mut store, store_id, &edit, &mut seq, None);
    assert_eq!(store.children_of(folder).unwrap(), vec![z]);
    assert_eq!(store.children_of(root).unwrap(), vec![folder, x, y]);
    assert_eq!(store.node_of(y).unwrap().parent_id, Some(root));

    // A delete: tombstones for the subtree, and the parent's list.
    let edit = peer.remove_node(folder, T2).unwrap();
    arrive(&mut store, store_id, &edit, &mut seq, None);
    assert_eq!(store.children_of(root).unwrap(), vec![x, y]);
    assert!(store.node_of(folder).is_err(), "a tombstone is not a node");
    assert!(store.node_of(z).is_err());
    assert!(store.tree.doc(z).is_some(), "the document stays");
    assert!(store.tree.validate_tree().is_empty());
    assert!(store.repair_now(T2).is_none());
}

#[test]
fn the_root_is_the_manifest_s_unless_the_documents_name_another() {
    let (mut peer, real_root) = origin();
    let x = NodeId::new();
    peer.add_node(x, Some(real_root), Place::Last, "document", "x", T0).unwrap();

    // A store hosted from a desktop: the manifest carries a placeholder
    // the desktop's documents never mention.
    let placeholder = NodeId::new();
    let store = VaultStore::assemble(listed(placeholder), store_keys(), Vec::new(), pull_of(&peer));
    assert_eq!(store.tree.root(), real_root);
    assert_eq!(store.view().root_node_id, real_root);
    assert_eq!(store.children_of(real_root).unwrap(), vec![x]);

    // An empty log keeps the manifest's root, answers an empty tree for
    // it, and describes it as a folder named after the store.
    let mut empty = VaultStore::assemble(listed(placeholder), store_keys(), Vec::new(), Vec::new());
    assert_eq!(empty.tree.root(), placeholder);
    assert_eq!(empty.children_of(placeholder).unwrap(), Vec::<NodeId>::new());
    assert_eq!(empty.node_of(placeholder).unwrap().metadata.title, "Vault");
    assert!(empty.repair_now(T1).is_none());

    // The first write under it seeds the root document, and only once.
    let seed = empty.seed_root(T1);
    assert_eq!(seed.node_ids(), vec![placeholder]);
    assert!(empty.tree.has_node(placeholder));
    assert!(empty.seed_root(T1).is_empty());
    let y = NodeId::new();
    empty.tree.add_node(y, Some(placeholder), Place::Last, "document", "y", T1).unwrap();
    assert_eq!(empty.children_of(placeholder).unwrap(), vec![y]);
}

#[test]
fn a_root_arriving_after_the_open_re_roots_the_tree_once() {
    // Opened empty, before a desktop's link pushed anything; the
    // person only looked, so nothing was seeded here.
    let placeholder = NodeId::new();
    let store_id = StoreId::new();
    let mut store = VaultStore::assemble(listed(placeholder), store_keys(), Vec::new(), Vec::new());

    let (mut peer, real_root) = origin();
    let x = NodeId::new();
    let edit = peer.add_node(x, Some(real_root), Place::Last, "document", "x", T0).unwrap();
    let mut seq = 0;
    // The child's document lands first: no root to adopt yet.
    arrive(&mut store, store_id, &TreeEdit { touched: vec![edit.touched[0].clone()] }, &mut seq, None);
    assert!(store.adopt_root().is_none());
    // The root's document: the tree moves under it and the UI is told.
    let root_update = peer.doc(real_root).unwrap().save();
    let merged = store.merge(store_id, real_root, vec![(Mark::One(1), Ok(root_update))], false, None);
    assert!(merged.structure);
    let announced = store.adopt_root().expect("the documents' root replaces the placeholder");
    assert_eq!(announced.root_node_id, real_root);
    assert_eq!(store.tree.root(), real_root);
    assert_eq!(store.children_of(real_root).unwrap(), vec![x]);
    assert!(store.adopt_root().is_none(), "settled");
}

// ── Sizes and parts (docs/SYNC_LIMITS_CONTRACT.md) ──────────────────────

/// A 20 MiB edit goes out as parts of one group (and whole to a server
/// that takes no parts); its answer's sizes make a snapshot due; a page
/// reading the parts applies them as one, from a fetch or live, and one
/// missing a part applies nothing and says why it waits; a reader that
/// knows nothing of parts opens none of them.
#[test]
fn a_large_edit_goes_in_parts_and_is_read_only_whole() {
    let (peer, root) = origin();
    let store_id = StoreId::new();
    let mut store = opened(&peer);
    let text: String = (0..20 * 1024 * 1024).map(|i| (b'a' + (i * 7 % 26) as u8) as char).collect();
    let mut big = NodeDoc::load(&store.tree.doc(root).unwrap().save()).unwrap();
    let before = big.state_vector();
    big.set_data("blob", &serde_json::Value::String(text.clone())).unwrap();
    let update = big.diff_since(&before).unwrap();
    store.tree.apply_update(root, &update).unwrap();

    assert_eq!(store.prepare(store_id, root, &update).unwrap().blobs.len(), 1, "whole to a server that takes no parts");
    store.parts_ok = true;
    let outgoing = store.prepare(store_id, root, &update).unwrap();
    assert_eq!(outgoing.blobs.len(), 3, "in parts");
    let sizes = LogSizes { log_bytes: 27 << 20, snapshot_bytes: 0 };
    assert_eq!(store.record_span(root, &outgoing, Ok((2, 4, Some(sizes)))).unwrap(), Some(4), "a snapshot is due by size");
    assert_eq!(store.docs[&root].cursor.applied_through(), 4, "the whole group counts as read");
    let snapshot = store.snapshot_blob(store_id, root).unwrap();
    assert!(snapshot.len() >= 3 && snapshot.iter().all(|s| s.part.is_some()), "the snapshot goes in parts too");

    // An older reader opens no part.
    for one in &outgoing.blobs {
        assert!(store.keys.decrypt_blob(store_id, root, &one.blob).is_err());
    }

    let entries: Vec<pimble_rpc::VaultEntry> = outgoing.blobs.iter().enumerate().map(|(i, one)| pimble_rpc::VaultEntry { seq: i as u64 + 2, blob: one.blob.clone(), part: one.part }).collect();
    let fetch = |updates: Vec<pimble_rpc::VaultEntry>| VaultFetchResponse { snapshot: None, snapshot_parts: Vec::new(), updates, head: 4, keys: None, more: false, snapshot_seq: 0 };

    // Fetched whole.
    let mut reader = VaultStore::assemble(listed(root), same_keys(&store), Vec::new(), pull_of(&peer));
    reader.take_fetched(store_id, root, &fetch(entries.clone()), 0.0);
    assert_eq!(reader.tree.doc(root).unwrap().data_json()["blob"].as_str().map(str::len), Some(text.len()));
    assert_eq!(reader.docs[&root].cursor.applied_through(), 4);
    assert!(reader.waiting_list().is_empty());

    // A part missing: nothing applies, the cursor stays, and it waits.
    let mut short = VaultStore::assemble(listed(root), same_keys(&store), Vec::new(), pull_of(&peer));
    let mut missing = entries.clone();
    missing.remove(1);
    short.take_fetched(store_id, root, &fetch(missing), 0.0);
    assert!(short.tree.doc(root).unwrap().data_json()["blob"].is_null());
    assert_eq!(short.docs[&root].cursor.applied_through(), 1);
    let waiting = short.waiting_list();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].node_id, root);
    assert!(short.held_changed, "the UI is told");

    // Live, one notification a part: applied once the last is in, and
    // what waited is let go.
    for entry in &entries {
        if let Some((mark, parts)) = short.parts.take(&root.to_string(), entry.seq, entry.part.unwrap(), entry.blob.clone()) {
            short.take_parts(store_id, root, mark, &parts, Some("another-page".into()), 0.0);
        }
    }
    assert_eq!(short.tree.doc(root).unwrap().data_json()["blob"].as_str().map(str::len), Some(text.len()));
    assert_eq!(short.docs[&root].cursor.applied_through(), 4);
    assert!(short.waiting_list().is_empty());
}

/// A blob this page cannot open keeps its document waiting with the
/// reason, in the store's sync answer, and a send that fails says so
/// too; both go when the document does.
#[test]
fn a_document_held_back_is_listed_with_its_reason() {
    let (peer, root) = origin();
    let store_id = StoreId::new();
    let mut store = opened(&peer);
    let merged = store.merge(store_id, root, vec![(Mark::One(2), Err(protocol::MADE_BY_NEWER.to_string()))], true, None);
    assert!(merged.events.is_empty());
    assert_eq!(store.waiting_list(), vec![pimble_core::WaitingDoc { node_id: root, reason: protocol::MADE_BY_NEWER.to_string() }]);
    let update = store.tree.set_title(root, "Renamed", T1).unwrap().touched[0].1.clone();
    let outgoing = store.prepare(store_id, root, &update).unwrap();
    assert!(store.record(root, &outgoing, Err("the socket went away".into())).is_err());
    assert_eq!(store.waiting_list().len(), 1, "one entry per document");
    assert!(store.record(root, &outgoing, Ok(3)).is_ok());
    assert_eq!(store.waiting_list()[0].reason, protocol::MADE_BY_NEWER, "the send is no longer held back; the read still is");
    let ok = store.tree.doc(root).unwrap().save();
    store.merge(store_id, root, vec![(Mark::One(2), Ok(ok))], true, None);
    assert!(store.waiting_list().is_empty());
}

// ── What a tree edit appends ────────────────────────────────────────────

#[test]
fn a_tree_edit_is_appended_per_document_in_order() {
    let (mut peer, root) = origin();
    let store_id = StoreId::new();
    let mut store = opened(&peer);
    let x = NodeId::new();
    let edit = store.tree.add_node(x, Some(root), Place::Last, "document", "x", T1).unwrap();
    assert_eq!(edit.node_ids(), vec![x, root], "the new document before its parent's list");

    let mut seq = 0;
    for (node_id, update) in &edit.touched {
        let outgoing = store.prepare(store_id, *node_id, update).unwrap();
        assert_eq!(outgoing.doc_id, VaultDocId::Node(*node_id));
        assert!(!outgoing.resend);
        // The blob is that document's update and nothing else, under
        // this store's and this document's associated data.
        let opened = store.keys.decrypt_blob(store_id, *node_id, outgoing.blob()).unwrap();
        assert_eq!(&opened, update);
        assert!(
            store.keys.decrypt_blob(store_id, NodeId::new(), outgoing.blob()).is_err(),
            "bound to its document"
        );
        seq += 1;
        assert_eq!(store.record(*node_id, &outgoing, Ok(seq)).unwrap(), None);
        let doc = &store.docs[node_id];
        assert_eq!(doc.cursor.applied_through(), seq, "our own append counts as read");
        assert!(!doc.unsent);
    }
    // A peer applying the blobs in that order ends with the same tree.
    for (node_id, update) in &edit.touched {
        peer.apply_update(*node_id, update).unwrap();
    }
    assert_eq!(peer.get_children(root).unwrap(), vec![x]);
    assert_eq!(peer.get_node_info(x).unwrap().title, "x");
    assert!(peer.validate_tree().is_empty());
}

#[test]
fn a_failed_append_is_resent_as_everything_the_server_lacks() {
    let (peer, root) = origin();
    let store_id = StoreId::new();
    let mut store = opened(&peer);
    let known_before = store.docs[&root].known_sv.clone();

    // A rename whose append fails.
    let first = store.tree.set_title(root, "One", T1).unwrap();
    let (_, update) = &first.touched[0];
    let outgoing = store.prepare(store_id, root, update).unwrap();
    assert!(store.record(root, &outgoing, Err("socket closed".into())).is_err());
    assert!(store.docs[&root].unsent);
    assert_eq!(store.docs[&root].known_sv, known_before, "nothing moved");

    // The next edit carries both: a diff since what the server holds.
    let second = store.tree.set_title(root, "Two", T2).unwrap();
    let (_, update) = &second.touched[0];
    let outgoing = store.prepare(store_id, root, update).unwrap();
    assert!(outgoing.resend);
    let mut fresh = NodeDoc::load(&peer.doc(root).unwrap().save()).unwrap();
    fresh.apply_update(&outgoing.payload).unwrap();
    assert_eq!(fresh.fields().unwrap().title, "Two", "the resend brings the server level");
    assert_eq!(store.record(root, &outgoing, Ok(7)).unwrap(), None);
    assert!(!store.docs[&root].unsent);
    assert_eq!(store.docs[&root].known_sv, store.tree.doc(root).unwrap().state_vector());
}

#[test]
fn a_snapshot_is_due_only_when_the_cursor_has_read_through_its_number() {
    let (peer, root) = origin();
    let store_id = StoreId::new();
    let mut store = opened(&peer);
    let mut seq = 1;
    for i in 0..SNAPSHOT_EVERY {
        let edit = store.tree.set_title(root, &format!("t{i}"), T1).unwrap();
        let (_, update) = &edit.touched[0];
        let outgoing = store.prepare(store_id, root, update).unwrap();
        seq += 1;
        // Somebody else's append at 100 never arrived: from then on the
        // cursor stops short of our own numbers.
        if seq == 100 {
            seq += 1;
        }
        let due = store.record(root, &outgoing, Ok(seq)).unwrap();
        assert_eq!(due, None, "append {i}: not until the gap closes");
    }
    // The missing entry lands; the next append may snapshot.
    store.docs.get_mut(&root).unwrap().cursor.mark(100);
    let edit = store.tree.set_title(root, "last", T2).unwrap();
    let outgoing = store.prepare(store_id, root, &edit.touched[0].1).unwrap();
    seq += 1;
    assert_eq!(store.record(root, &outgoing, Ok(seq)).unwrap(), Some(seq));
}
