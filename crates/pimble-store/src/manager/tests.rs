//! Tests of the store manager.

use super::*;
use pimble_host::disk::testfs as tfs;
use std::collections::HashSet;

const T: &str = "2026-09-18T10:00:00+00:00";

/// Reproduces the bug an app-level `addRemoteStore` self-reference
/// exposed: `create_replica` for an id that's already open would
/// `HashMap::insert` over the open `LocalStore`, silently dropping its
/// in-memory tree (any unflushed edits with it) and leaving the store id
/// pointing at a fresh, empty replica. Refused here, not only at the RPC
/// boundary, since this is `StoreManager`'s own invariant to keep.
#[tokio::test]
async fn create_replica_refuses_an_id_that_is_already_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("existing.pimble"), "Existing").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();

    let replica_dir = tempfile::tempdir().unwrap();
    let err = manager
        .create_replica(replica_dir.path().join("replica.pimble"), store_id, "Existing", root_id)
        .await
        .expect_err("create_replica should refuse an id that's already open");
    assert!(
        matches!(err, StoreError::InvalidOperation(_)),
        "expected InvalidOperation, got {:?}", err
    );

    // The original store must be untouched: still open, same root.
    assert!(manager.is_open(store_id));
    assert_eq!(manager.root_node_id(store_id).unwrap(), root_id);
    assert!(!tfs::exists(&replica_dir.path().join("replica.pimble")), "no replica directory should have been created");
}

/// A replica starts with no documents, not even the root's: the peer's
/// root document is the root (see `LocalStore::create_replica`).
#[tokio::test]
async fn create_replica_makes_no_root_document() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let (id, root_id) = (StoreId::new(), NodeId::new());
    let store_id = manager.create_replica(dir.path().join("replica.pimble"), id, "Replica", root_id).await.unwrap();
    assert_eq!(store_id, id);
    assert!(manager.doc_ids(store_id).unwrap().is_empty());
    assert!(manager.list_node_ids(store_id).unwrap().is_empty());
    assert!(matches!(manager.get_node(store_id, root_id), Err(StoreError::NodeNotFound(_))));
    assert_eq!(manager.document_root(store_id).unwrap(), None);
    assert_eq!(manager.opened_since(), vec![store_id]);
}

/// Deleting a folder tombstones every descendant (docs/NODE_DOCUMENT_CONTRACT.md
/// section 2) — not just the folder itself — and deletes no file: the
/// documents stay, named for sync but no longer nodes.
#[tokio::test]
async fn delete_node_tombstones_the_whole_subtree() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();

    let folder_id = manager.create_node(store_id, Node::folder("Folder"), Some(root_id)).unwrap().0;
    let subfolder_id = manager.create_node(store_id, Node::folder("Subfolder"), Some(folder_id)).unwrap().0;
    let doc_id = manager.create_node(store_id, Node::document("Doc"), Some(subfolder_id)).unwrap().0;

    manager.update_node_content(store_id, doc_id, NodeDoc::from_plain_text("hi").unwrap().save()).unwrap();
    manager.flush(store_id).await.unwrap();

    let store_path = manager.get_store_info(store_id).unwrap().local_path().unwrap().clone();
    let content_path = store_path.join("nodes").join(format!("{}.yrs", doc_id));
    assert!(tfs::exists(&content_path), "the document's file should exist before delete");

    let (removal, edit) = manager.delete_node(store_id, folder_id).unwrap();

    assert_eq!(removal.parent_id, root_id);
    let removed: HashSet<_> = removal.removed.iter().copied().collect();
    assert_eq!(removed, [folder_id, subfolder_id, doc_id].into_iter().collect());
    let touched: HashSet<_> = edit.node_ids().into_iter().collect();
    assert_eq!(touched, [folder_id, subfolder_id, doc_id, root_id].into_iter().collect());

    let tree = manager.tree(store_id).unwrap();
    assert!(!tree.has_node(folder_id));
    assert!(!tree.has_node(subfolder_id));
    assert!(!tree.has_node(doc_id));
    assert!(!tree.get_children(root_id).unwrap().contains(&folder_id));
    assert!(manager.doc_ids(store_id).unwrap().contains(&doc_id), "a tombstone is still a document");
    assert!(!manager.list_node_ids(store_id).unwrap().contains(&doc_id));
    manager.flush(store_id).await.unwrap();
    assert!(tfs::exists(&content_path), "no file is deleted");
}

/// A subtree that hasn't been repaired yet can itself be malformed: a
/// concurrent merge (docs/history/HARDENING_CONTRACT.md decision 9) can leave a
/// cycle among nodes below the one being deleted (e.g. two folders that
/// concurrently moved under each other), and a child listed twice in the
/// same parent's children list (e.g. two replicas concurrently moving
/// the same node to the same destination). Reproduced directly here
/// with `insert_child` on the documents — the same primitive the tree's
/// edits use, so the resulting shape is exactly what such a merge
/// leaves behind — rather than via an actual two-replica merge, to
/// control precisely which lists end up malformed. `delete_node`'s walk
/// must terminate (not loop forever on the cycle) and the delete must
/// still succeed — without ever calling `repair_tree` first.
#[tokio::test]
async fn delete_node_survives_an_unrepaired_subtree_with_a_cycle_and_a_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();

    let top = manager.create_node(store_id, Node::folder("Top"), Some(root_id)).unwrap().0;
    let folder_a = manager.create_node(store_id, Node::folder("A"), Some(top)).unwrap().0;
    let folder_b = manager.create_node(store_id, Node::folder("B"), Some(top)).unwrap().0;
    let child = manager.create_node(store_id, Node::document("Child"), Some(folder_a)).unwrap().0;

    {
        let tree = manager.tree_mut(store_id).unwrap();
        // A cycle in the children-list graph below `top` (both A and B stay
        // listed under `top` too, so the subtree is still reachable from it):
        // A's list also names B, and B's list also names A.
        tree.doc_mut(folder_b).unwrap().insert_child(99, folder_a).unwrap();
        tree.doc_mut(folder_a).unwrap().insert_child(99, folder_b).unwrap();
        // A duplicate: `child` appears a second time in A's own list.
        tree.doc_mut(folder_a).unwrap().insert_child(99, child).unwrap();
    }

    let issues_before = manager.tree(store_id).unwrap().validate_tree();
    assert!(!issues_before.is_empty(), "expected the fabricated state to be malformed before any repair");

    let (removal, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async { manager.delete_node(store_id, top) })
        .await
        .expect("delete_node must terminate even with a cycle below the deleted node")
        .expect("delete_node must succeed even with a duplicated child below the deleted node");

    let removed: HashSet<_> = removal.removed.into_iter().collect();
    assert_eq!(removed, [top, folder_a, folder_b, child].into_iter().collect());

    let tree = manager.tree(store_id).unwrap();
    assert!(!tree.has_node(top));
    assert!(!tree.has_node(folder_a));
    assert!(!tree.has_node(folder_b));
    assert!(!tree.has_node(child));
}

/// An unrepaired list below the deleted node that names the root, or an id
/// with no document, must neither take the root (and with it the whole
/// store) down with the subtree nor fail the delete halfway.
#[tokio::test]
async fn delete_node_skips_the_root_and_dangling_entries_listed_below_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();

    let keep = manager.create_node(store_id, Node::document("Keep"), Some(root_id)).unwrap().0;
    let folder = manager.create_node(store_id, Node::folder("Folder"), Some(root_id)).unwrap().0;
    {
        let tree = manager.tree_mut(store_id).unwrap();
        tree.doc_mut(folder).unwrap().insert_child(99, root_id).unwrap();
        tree.doc_mut(folder).unwrap().insert_child(99, NodeId::new()).unwrap();
    }

    let (removal, _) = manager.delete_node(store_id, folder).expect("delete_node succeeds");
    assert_eq!(removal.removed, vec![folder]);

    let tree = manager.tree(store_id).unwrap();
    assert!(tree.has_node(root_id), "the root must survive");
    assert!(tree.has_node(keep), "the root's other children must survive");
    assert!(!tree.has_node(folder));
}

/// A node's stored `parent_id` can name a different node than the
/// children-list edge it is reachable by — the same not-yet-repaired
/// merge shape as the test above, in the `parent_id` field rather than
/// the list. The tree counts a node as a child only where list and
/// `parent_id` agree, so such a node is not in the deleted subtree: the
/// delete succeeds around it, and the next repair places it under the
/// root (its stored parent is a tombstone) rather than losing it.
#[tokio::test]
async fn delete_node_leaves_a_node_whose_parent_id_disagrees_with_the_list_to_repair() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();

    let top = manager.create_node(store_id, Node::folder("Top"), Some(root_id)).unwrap().0;
    let sibling = manager.create_node(store_id, Node::folder("Sibling"), Some(top)).unwrap().0;
    let mismatched = manager.create_node(store_id, Node::document("Mismatched"), Some(top)).unwrap().0;

    // `mismatched` stays listed under `top` (untouched), but its own
    // `parent_id` field is corrupted to name `sibling` instead.
    manager.node_doc(store_id, mismatched).unwrap().set_parent_id(Some(sibling), T).unwrap();

    let (removal, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async { manager.delete_node(store_id, top) })
        .await
        .unwrap()
        .expect("delete_node must succeed around a node whose parent_id disagrees with the list");

    let removed: HashSet<_> = removal.removed.into_iter().collect();
    assert_eq!(removed, [top, sibling].into_iter().collect());

    let tree = manager.tree(store_id).unwrap();
    assert!(!tree.has_node(top));
    assert!(!tree.has_node(sibling));
    assert!(tree.has_node(mismatched), "not reached by the deletion: still a node");

    let repair = manager.repair_tree(store_id).unwrap().expect("the orphan needs a repair");
    assert!(repair.node_ids().contains(&mismatched));
    assert_eq!(manager.get_node(store_id, mismatched).unwrap().parent_id, Some(root_id));
    assert!(manager.tree(store_id).unwrap().validate_tree().is_empty());
}

/// The root node can never be deleted, subtree or not.
#[tokio::test]
async fn delete_node_refuses_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();

    let err = manager.delete_node(store_id, root_id).expect_err("deleting the root must fail");
    assert!(matches!(err, StoreError::InvalidOperation(_)), "expected InvalidOperation, got {:?}", err);
    assert!(manager.tree(store_id).unwrap().has_node(root_id));
}

/// `repair_tree` on an already well-formed tree changes nothing
/// (docs/history/HARDENING_CONTRACT.md decision 9): in particular it must not mark
/// anything dirty, which would force a needless flush on every reconcile.
#[tokio::test]
async fn repair_tree_is_a_no_op_on_a_well_formed_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    manager.flush(store_id).await.unwrap();

    let repair = manager.repair_tree(store_id).unwrap();
    assert!(repair.is_none());
}

/// A plain node's deletion, once flushed, stays deleted across a
/// reopen: the store-layer half of the contract the RPC handler's
/// `deleteNode` depends on (from Joe's bug report: a node deleted in
/// the app reappeared after restarting; the root cause was `deleteNode`
/// never flushing, fixed in `pimble-server`, not here). With
/// tombstones the document comes back from disk, and comes back deleted.
#[tokio::test]
async fn deleted_node_stays_deleted_after_flush_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.pimble");

    let (store_id, root_id, doc_id) = {
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(&path, "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();
        let doc_id = manager.create_node(store_id, Node::document("Doc"), Some(root_id)).unwrap().0;
        manager.flush(store_id).await.unwrap();

        manager.delete_node(store_id, doc_id).unwrap();
        manager.flush(store_id).await.unwrap();

        (store_id, root_id, doc_id)
    };

    let mut manager = StoreManager::new();
    let reopened_id = manager.open_local_store(&path).await.unwrap();
    assert_eq!(reopened_id, store_id);
    let tree = manager.tree(store_id).unwrap();
    assert!(!tree.has_node(doc_id));
    assert!(!tree.get_children(root_id).unwrap().contains(&doc_id));
    assert!(manager.doc_ids(store_id).unwrap().contains(&doc_id), "the tombstone is held");
    assert!(manager.repair_tree(store_id).unwrap().is_none());
}

/// The manager's edits report what they wrote, per document, and a peer
/// applying exactly those updates ends up with the same tree.
#[tokio::test]
async fn edits_report_updates_a_peer_can_apply() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();
    let peer_id = manager.create_replica(dir.path().join("peer.pimble"), StoreId::new(), "Peer", root_id).await.unwrap();
    let relay = |manager: &mut StoreManager, edit: &TreeEdit| {
        for (id, update) in &edit.touched {
            manager.apply_node_update(peer_id, *id, update).unwrap();
        }
    };
    let root_bytes = manager.node_doc(store_id, root_id).unwrap().save();
    manager.apply_node_update(peer_id, root_id, &root_bytes).unwrap();

    let mut node = Node::document("Doc");
    node.metadata.tags = vec!["t".into()];
    node.metadata.custom.insert("icon".into(), serde_json::json!("star"));
    let (doc_id, edit) = manager.create_node(store_id, node, Some(root_id)).unwrap();
    relay(&mut manager, &edit);
    let (folder_id, edit) = manager.create_node(store_id, Node::folder("Folder"), None).unwrap();
    relay(&mut manager, &edit);
    let (_, _, edit) = manager.move_node(store_id, doc_id, folder_id, Place::First).unwrap();
    relay(&mut manager, &edit);
    let mut metadata = manager.get_node(store_id, doc_id).unwrap().metadata;
    metadata.title = "Renamed".into();
    metadata.custom.remove("icon");
    let edit = manager.update_node_metadata(store_id, doc_id, metadata).unwrap();
    relay(&mut manager, &edit);
    let (_, edit) = manager.delete_node(store_id, folder_id).unwrap();
    relay(&mut manager, &edit);
    let edit = manager.undelete_node(store_id, folder_id).unwrap();
    relay(&mut manager, &edit);
    assert!(manager.repair_tree(peer_id).unwrap().is_none(), "the relayed edits leave nothing to repair");

    for id in [root_id, folder_id, doc_id] {
        let (a, b) = (manager.get_node(store_id, id).unwrap(), manager.get_node(peer_id, id).unwrap());
        assert_eq!(a.parent_id, b.parent_id);
        assert_eq!(a.children, b.children);
        assert_eq!(a.metadata.title, b.metadata.title);
        assert_eq!(a.metadata.tags, b.metadata.tags);
        assert_eq!(a.metadata.custom, b.metadata.custom);
    }
    let doc = manager.get_node(peer_id, doc_id).unwrap();
    assert_eq!(doc.metadata.title, "Renamed");
    assert!(!doc.metadata.custom.contains_key("icon"));
    assert_eq!(manager.get_node(peer_id, root_id).unwrap().children, vec![folder_id]);
    assert_eq!(manager.get_node(peer_id, folder_id).unwrap().children, vec![doc_id]);
}

/// Creating under a mount node is refused: its children live in the
/// source store.
#[tokio::test]
async fn create_node_refuses_a_mount_as_parent() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();
    let mount = Node::new(pimble_core::node_types::MOUNT);
    let (mount_id, _) = manager.create_node(store_id, mount, Some(root_id)).unwrap();
    let err = manager.create_node(store_id, Node::document("Under a mount"), Some(mount_id)).unwrap_err();
    assert!(matches!(err, StoreError::MountHasNoChildren { node_id } if node_id == mount_id), "{err:?}");
    let err = manager.create_node(store_id, Node::document("Nowhere"), Some(NodeId::new())).unwrap_err();
    assert!(matches!(err, StoreError::NodeNotFound(_)), "{err:?}");
}

/// A node made a theme while a document names it as its parent is
/// refused even when its list does not name that document: repair would
/// list the document under the theme.
#[tokio::test]
async fn judge_placement_refuses_a_theme_over_a_child_named_only_by_its_parent_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = StoreManager::new();
    let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
    let root_id = manager.root_node_id(store_id).unwrap();
    let (folder, _) = manager.create_node(store_id, Node::folder("Folder"), Some(root_id)).unwrap();
    let (child, _) = manager.create_node(store_id, Node::document("Child"), Some(folder)).unwrap();
    assert!(manager.node_doc(store_id, folder).unwrap().remove_child(child).unwrap());

    let doc = manager.node_doc(store_id, folder).unwrap();
    let (mut peer, before) = (NodeDoc::load(&doc.save()).unwrap(), doc.state_vector());
    peer.set_node_type(pimble_core::node_types::THEME, T).unwrap();
    let update = peer.diff_since(&before).unwrap();
    let err = manager.judge_placement(store_id, folder, &update).unwrap_err();
    assert!(matches!(err, StoreError::InsideInternal { .. }), "{err:?}");
}
