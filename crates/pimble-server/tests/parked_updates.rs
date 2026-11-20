//! An update that arrives before the changes it builds on is parked by yrs
//! and merged later, when they arrive. What the server sends on then (to
//! every window and every link) must be everything that merge let in, the
//! parked part included: before 2026-11-19 only the late-arriving bytes went
//! on, so every other device held the parked part's successors parked in
//! turn, for ever (a shared note showed a member's first edits and none of
//! what followed).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::{ApplyEditRequest, CreateNodeRequest, CreateStoreRequest, EditOperation, PimbleApiServer, StoreChangedNotification};
use pimble_server::RpcHandler;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

async fn new_handler_with_node() -> (RpcHandler, StoreId, NodeId, tempfile::TempDir) {
    let handler = RpcHandler::new(Arc::new(RwLock::new(StoreManager::new())));
    let dir = tempfile::tempdir().unwrap();
    let store = handler
        .create_store(&pimble_server::service_extensions(), CreateStoreRequest { kind: Default::default(), store_id: None, path: dir.path().join("test.pimble"), name: "Test".into() })
        .await
        .unwrap();
    let node = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id: store.store_id,
            parent_id: Some(store.root_node_id),
            node_type: "document".into(),
            title: "Doc".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap();
    (handler, store.store_id, node.node_id, dir)
}

async fn edit(handler: &RpcHandler, store_id: StoreId, node_id: NodeId, update: &[u8]) {
    let changes = base64::engine::general_purpose::STANDARD.encode(update);
    handler
        .apply_edit(&pimble_server::service_extensions(), ApplyEditRequest { store_id, node_id, client_id: "peer".into(), operation: EditOperation::IncrementalChanges { changes } })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_parked_update_goes_on_whole_once_what_it_waited_for_arrives() {
    let (handler, store_id, node_id, _dir) = new_handler_with_node().await;

    let base = NodeDoc::from_plain_text("Hello").unwrap();
    edit(&handler, store_id, node_id, &base.save()).await;

    // Another device types twice; the second change builds on the first.
    let mut peer = NodeDoc::load(&base.save()).unwrap();
    let first = peer.replace_plain_text("Hello there").unwrap();
    let second = peer.replace_plain_text("Hello there, Will").unwrap();

    let mut module = handler.clone().into_rpc();
    module.extensions_mut().insert(pimble_server::Principal::Service);
    let mut sub = module.subscribe_unbounded("pimble_subscribeStoreChanges", (store_id,)).await.unwrap();

    // The second arrives first: parked.
    edit(&handler, store_id, node_id, &second).await;
    // The first arrives: both merge.
    edit(&handler, store_id, node_id, &first).await;

    // A device that saw only what the server sent on, from the base.
    let mut observer = NodeDoc::load(&base.save()).unwrap();
    let mut heard = 0;
    while let Ok(Some(Ok((notif, _)))) = tokio::time::timeout(Duration::from_millis(300), sub.next::<StoreChangedNotification>()).await {
        if let Some(update) = notif.update {
            observer.apply_update(&base64::engine::general_purpose::STANDARD.decode(update).unwrap()).unwrap();
            heard += 1;
        }
    }
    assert!(heard > 0, "the merge was announced");
    assert_eq!(observer.missing(), None, "nothing the observer heard waits on anything");
    assert_eq!(observer.text(), peer.text(), "the observer has both changes, the parked one too");
}

#[test]
fn a_document_says_what_its_parked_update_waits_for() {
    let base = NodeDoc::from_plain_text("Hello").unwrap();
    let mut peer = NodeDoc::load(&base.save()).unwrap();
    let first = peer.replace_plain_text("Hello there").unwrap();
    let second = peer.replace_plain_text("Hello there, Will").unwrap();

    let mut doc = NodeDoc::load(&base.save()).unwrap();
    assert_eq!(doc.missing(), None);
    doc.apply_update(&second).unwrap();
    assert!(doc.missing().is_some(), "the second change waits for the first");
    assert_eq!(doc.text(), "Hello", "and shows nothing yet");
    doc.apply_update(&first).unwrap();
    assert_eq!(doc.missing(), None);
    assert_eq!(doc.text(), "Hello there, Will");
}

/// A device that lacks a child's tombstone, and predates the `left:` record
/// (docs/MOVE_CONTRACT.md "Repair"), puts the child back in its parent's
/// list after every repair here takes it out. The server cannot
/// settle that by repairing (each side is right by what it holds), so after
/// a number of rounds it repairs that store only every 30 s, and the storm
/// (three rounds a second, for as long as both stay connected) stops.
#[tokio::test]
async fn a_repair_another_device_keeps_undoing_slows_down() {
    let manager = Arc::new(RwLock::new(StoreManager::new()));
    let handler = RpcHandler::new(manager.clone());
    let dir = tempfile::tempdir().unwrap();
    let ext = pimble_server::service_extensions();
    let store = handler
        .create_store(&ext, CreateStoreRequest { kind: Default::default(), store_id: None, path: dir.path().join("t.pimble"), name: "T".into() })
        .await
        .unwrap();
    let new_node = |parent: NodeId, title: &str| CreateNodeRequest {
        store_id: store.store_id,
        parent_id: Some(parent),
        node_type: "document".into(),
        title: title.into(),
        content: None,
        custom: Default::default(),
        place: Default::default(),
    };
    let folder = handler.create_node(&ext, new_node(store.root_node_id, "Shared")).await.unwrap().node_id;
    let child = handler.create_node(&ext, new_node(folder, "Blinking")).await.unwrap().node_id;

    // The other device holds everything as it is now, and never hears of
    // the deletion that follows.
    let mut peer = {
        let m = manager.read().await;
        let tree = m.tree(store.store_id).unwrap();
        let docs = tree.ids().into_iter().map(|id| (id, NodeDoc::load(&tree.doc(id).unwrap().save()).unwrap())).collect();
        pimble_crdt::Tree::from_docs(store.root_node_id, docs)
    };
    handler.delete_node(&ext, pimble_rpc::DeleteNodeRequest { store_id: store.store_id, node_id: child }).await.unwrap();

    let listed_here = || async {
        let m = manager.read().await;
        m.tree(store.store_id).unwrap().doc(folder).unwrap().children().contains(&child)
    };
    let mut slowed = false;
    let mut rounds = 0;
    for _ in 0..30 {
        rounds += 1;
        // The peer hears this server's list and puts the note back, as a
        // device from before 0.28.5 did (it ignores the `left:` record this
        // version's repair heeds): an entry appended by hand.
        let list = manager.read().await.tree(store.store_id).unwrap().doc(folder).unwrap().save();
        peer.apply_update(folder, &list).unwrap();
        let doc = peer.doc(folder).unwrap();
        let mut copy = NodeDoc::load(&doc.save()).unwrap();
        let before = copy.state_vector();
        let at = copy.children().len();
        copy.insert_child(at, child).unwrap();
        let update = copy.diff_since(&before).unwrap();
        peer.apply_update(folder, &update).unwrap();
        edit(&handler, store.store_id, folder, &update).await;
        assert!(listed_here().await, "the peer put it back");
        tokio::time::sleep(Duration::from_millis(600)).await;
        if listed_here().await {
            slowed = true;
            break;
        }
    }
    assert!(slowed, "after enough rounds the server stops taking it out at once");
    assert!(rounds > 10, "until then it took it out each round (the loop the test stands for): {rounds} rounds");
}
