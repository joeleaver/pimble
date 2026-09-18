//! Server-boundary tests for the node document as the unit of everything
//! (docs/NODE_DOCUMENT_CONTRACT.md): the kinds a merged update earns, a
//! tombstone and its undelete, and a replica finding its root in the
//! documents that arrive. These call `RpcHandler` directly, subscribing
//! through `RpcHandler::into_rpc()` on a clone so the original handle stays
//! usable for the direct calls that drive the scenario.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::{NodeDoc, Tree, TreeEdit};
use pimble_rpc::{
    ApplyEditRequest, CreateNodeRequest, CreateStoreRequest, DeleteNodeRequest, EditOperation, GetChildrenRequest,
    GetNodeRequest, NodeStateVector, PimbleApiServer, StoreChangeKind, StoreChangedNotification, SyncNodesRequest,
    UndeleteNodeRequest, UpdateNodeMetadataRequest,
};
use pimble_server::RpcHandler;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

const T0: &str = "2026-09-18T10:00:00Z";
const T1: &str = "2026-09-18T10:00:01Z";

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}

async fn new_handler_with_store() -> (RpcHandler, StoreId, NodeId, tempfile::TempDir) {
    let (handler, store_id, root_id, dir, _) = new_handler_with_store_and_manager().await;
    (handler, store_id, root_id, dir)
}

/// The same, keeping the `StoreManager` the handler was built over, for a
/// test that reads a tree or makes a replica directly.
async fn new_handler_with_store_and_manager() -> (RpcHandler, StoreId, NodeId, tempfile::TempDir, Arc<RwLock<StoreManager>>) {
    let store_manager = Arc::new(RwLock::new(StoreManager::new()));
    let handler = RpcHandler::new(Arc::clone(&store_manager));
    let dir = tempfile::tempdir().unwrap();
    let create_resp = handler
        .create_store(&pimble_server::service_extensions(), CreateStoreRequest {
            kind: Default::default(),
            store_id: None,
            path: dir.path().join("test.pimble"),
            name: "Test Store".into(),
        })
        .await
        .unwrap();
    (handler, create_resp.store_id, create_resp.root_node_id, dir, store_manager)
}

async fn create(handler: &RpcHandler, store_id: StoreId, parent_id: NodeId, node_type: &str, title: &str) -> NodeId {
    handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(parent_id),
            node_type: node_type.into(),
            title: title.into(),
        })
        .await
        .unwrap()
        .node_id
}

async fn children_of(handler: &RpcHandler, store_id: StoreId, node_id: NodeId) -> Vec<NodeId> {
    handler
        .get_children(&pimble_server::service_extensions(), GetChildrenRequest { store_id, node_id })
        .await
        .unwrap()
        .children
        .into_iter()
        .map(|n| n.id)
        .collect()
}

/// Every document the server holds, as a `Tree` a peer would build from a
/// fresh `syncNodes`.
async fn peer_tree(handler: &RpcHandler, store_id: StoreId, root_id: NodeId) -> Tree {
    let listing = handler
        .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest { store_id, nodes: Vec::new(), list_unknown: true })
        .await
        .unwrap();
    let empty_sv = b64(&pimble_crdt::empty_state_vector());
    let fetched = handler
        .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest {
            store_id,
            nodes: listing.unknown_ids.iter().map(|id| NodeStateVector { node_id: *id, state_vector: empty_sv.clone() }).collect(),
            list_unknown: false,
        })
        .await
        .unwrap();
    let docs: HashMap<NodeId, NodeDoc> = fetched.nodes.into_iter().map(|n| (n.node_id, NodeDoc::load(&unb64(&n.diff)).unwrap())).collect();
    Tree::from_docs(root_id, docs)
}

/// Send a peer's `TreeEdit` to the server the way a link does: one
/// `applyEdit` per touched document, in order.
async fn send_edit(handler: &RpcHandler, store_id: StoreId, client_id: &str, edit: &TreeEdit) {
    for (node_id, update) in &edit.touched {
        handler
            .apply_edit(&pimble_server::service_extensions(), ApplyEditRequest {
                store_id,
                node_id: *node_id,
                client_id: client_id.into(),
                operation: EditOperation::IncrementalChanges { changes: b64(update) },
            })
            .await
            .unwrap();
    }
}

/// Collect notifications until `window` passes with none.
async fn drain(sub: &mut jsonrpsee::core::server::Subscription, window: Duration) -> Vec<StoreChangedNotification> {
    let mut out = Vec::new();
    while let Ok(Some(Ok((notif, _)))) = tokio::time::timeout(window, sub.next::<StoreChangedNotification>()).await {
        out.push(notif);
    }
    out
}

/// A peer's structural edits arriving as `applyEdit`s earn the kinds the
/// tree RPCs would have emitted for them: a create is `NodeCreated` for the
/// node and `TreeStructure` for the parent's list, a move is `NodeMoved`
/// plus the two lists, a rename is `MetadataUpdated`, a deletion is
/// `NodeDeleted`, an undelete `NodeCreated` again; every one carries the
/// update's bytes and the peer's id.
#[tokio::test]
async fn a_peers_structure_edits_earn_the_derived_kinds() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;
    let folder = create(&handler, store_id, root_id, "folder", "Folder").await;
    let mut peer = peer_tree(&handler, store_id, root_id).await;

    let mut module = handler.clone().into_rpc();
    module.extensions_mut().insert(pimble_server::Principal::Service);
    let mut sub = module.subscribe_unbounded("pimble_subscribeStoreChanges", (store_id,)).await.unwrap();

    // Create.
    let x = NodeId::new();
    let edit = peer.add_node(x, Some(root_id), None, "document", "X", T0).unwrap();
    send_edit(&handler, store_id, "peer", &edit).await;
    let notifs = drain(&mut sub, Duration::from_millis(300)).await;
    let kinds: Vec<&StoreChangeKind> = notifs.iter().map(|n| &n.change_kind).collect();
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeCreated { node_id, parent_id } if *node_id == x && *parent_id == root_id)), "{kinds:?}");
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::TreeStructure { node_ids } if node_ids == &vec![root_id])), "{kinds:?}");
    assert!(notifs.iter().all(|n| n.update.is_some() && n.source_client_id.as_deref() == Some("peer")), "{notifs:?}");
    assert_eq!(children_of(&handler, store_id, root_id).await, vec![folder, x]);

    // Move.
    let edit = peer.move_node(x, folder, None, T1).unwrap();
    send_edit(&handler, store_id, "peer", &edit).await;
    let notifs = drain(&mut sub, Duration::from_millis(300)).await;
    let kinds: Vec<&StoreChangeKind> = notifs.iter().map(|n| &n.change_kind).collect();
    assert!(
        kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeMoved { node_id, old_parent_id, new_parent_id } if *node_id == x && *old_parent_id == root_id && *new_parent_id == folder)),
        "{kinds:?}"
    );
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::TreeStructure { node_ids } if node_ids == &vec![root_id])), "{kinds:?}");
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::TreeStructure { node_ids } if node_ids == &vec![folder])), "{kinds:?}");
    assert_eq!(children_of(&handler, store_id, folder).await, vec![x]);
    assert_eq!(children_of(&handler, store_id, root_id).await, vec![folder]);

    // Rename.
    let edit = peer.set_title(x, "Renamed", T1).unwrap();
    send_edit(&handler, store_id, "peer", &edit).await;
    let notifs = drain(&mut sub, Duration::from_millis(300)).await;
    let kinds: Vec<&StoreChangeKind> = notifs.iter().map(|n| &n.change_kind).collect();
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert!(matches!(kinds[0], StoreChangeKind::MetadataUpdated { node_id } if *node_id == x), "{kinds:?}");
    let node = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: x }).await.unwrap().node;
    assert_eq!(node.metadata.title, "Renamed");

    // Content.
    let content = NodeDoc::from_plain_text("hello").unwrap();
    let edit = TreeEdit { touched: vec![(x, content.save())] };
    send_edit(&handler, store_id, "peer", &edit).await;
    let notifs = drain(&mut sub, Duration::from_millis(300)).await;
    let kinds: Vec<&StoreChangeKind> = notifs.iter().map(|n| &n.change_kind).collect();
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    assert!(matches!(kinds[0], StoreChangeKind::ContentUpdated { node_id } if *node_id == x), "{kinds:?}");
    peer.apply_update(x, &content.save()).unwrap();

    // Delete.
    let edit = peer.remove_node(x, T1).unwrap();
    send_edit(&handler, store_id, "peer", &edit).await;
    let notifs = drain(&mut sub, Duration::from_millis(300)).await;
    let kinds: Vec<&StoreChangeKind> = notifs.iter().map(|n| &n.change_kind).collect();
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeDeleted { node_id, parent_id } if *node_id == x && *parent_id == folder)), "{kinds:?}");
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::TreeStructure { node_ids } if node_ids == &vec![folder])), "{kinds:?}");
    assert!(handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: x }).await.is_err());
    assert!(children_of(&handler, store_id, folder).await.is_empty());

    // Undelete.
    let edit = peer.undelete_node(x, T1).unwrap();
    send_edit(&handler, store_id, "peer", &edit).await;
    let notifs = drain(&mut sub, Duration::from_millis(300)).await;
    let kinds: Vec<&StoreChangeKind> = notifs.iter().map(|n| &n.change_kind).collect();
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeCreated { node_id, parent_id } if *node_id == x && *parent_id == folder)), "{kinds:?}");
    assert_eq!(children_of(&handler, store_id, folder).await, vec![x]);
    let node = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: x }).await.unwrap().node;
    assert_eq!(NodeDoc::text_of(&node.content), "hello", "the content survived the round trip through the tombstone");

    // A peer's relayed content edit stamps nothing: `modified_at` is the
    // peer's to stamp, and the server would otherwise write one per relay.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let late = drain(&mut sub, Duration::from_millis(100)).await;
    assert!(
        late.iter().all(|n| !matches!(n.change_kind, StoreChangeKind::MetadataUpdated { .. })),
        "no modified_at stamp for a link's relay, got {late:?}"
    );
}

/// Deleting through the RPC tombstones: the node is gone from `getNode` and
/// `getChildren`, its document is still listed by `syncNodes` as a
/// tombstone, and `undeleteNode` brings it and its subtree back, at the end
/// of its parent's list, content intact.
#[tokio::test]
async fn a_tombstoned_node_is_gone_and_comes_back_on_undelete() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;
    let folder = create(&handler, store_id, root_id, "folder", "Folder").await;
    let inner = create(&handler, store_id, folder, "document", "Inner").await;
    let other = create(&handler, store_id, root_id, "document", "Other").await;
    let mut metadata = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: inner }).await.unwrap().node.metadata;
    metadata.tags = vec!["kept".into()];
    handler
        .update_node_metadata(&pimble_server::service_extensions(), UpdateNodeMetadataRequest { store_id, node_id: inner, metadata })
        .await
        .unwrap();

    let mut module = handler.clone().into_rpc();
    module.extensions_mut().insert(pimble_server::Principal::Service);
    let mut sub = module.subscribe_unbounded("pimble_subscribeStoreChanges", (store_id,)).await.unwrap();

    handler.delete_node(&pimble_server::service_extensions(), DeleteNodeRequest { store_id, node_id: folder }).await.unwrap();
    let notifs = drain(&mut sub, Duration::from_millis(300)).await;
    let kinds: Vec<&StoreChangeKind> = notifs.iter().map(|n| &n.change_kind).collect();
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeDeleted { node_id, parent_id } if *node_id == folder && *parent_id == root_id)), "{kinds:?}");
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::TreeStructure { node_ids } if node_ids == &vec![inner])), "the descendant's tombstone travels too: {kinds:?}");
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::TreeStructure { node_ids } if node_ids == &vec![root_id])), "{kinds:?}");
    assert!(notifs.iter().all(|n| n.update.is_some()));

    for id in [folder, inner] {
        assert!(handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: id }).await.is_err());
        assert!(handler.get_children(&pimble_server::service_extensions(), GetChildrenRequest { store_id, node_id: id }).await.is_err());
    }
    assert_eq!(children_of(&handler, store_id, root_id).await, vec![other]);
    let listing = handler
        .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest { store_id, nodes: Vec::new(), list_unknown: true })
        .await
        .unwrap();
    assert!(listing.unknown_ids.contains(&folder) && listing.unknown_ids.contains(&inner), "tombstones are documents sync names");

    let err = handler.undelete_node(&pimble_server::service_extensions(), UndeleteNodeRequest { store_id, node_id: other }).await.unwrap_err();
    assert!(err.message().contains("not deleted"), "{}", err.message());

    handler.undelete_node(&pimble_server::service_extensions(), UndeleteNodeRequest { store_id, node_id: folder }).await.unwrap();
    let notifs = drain(&mut sub, Duration::from_millis(300)).await;
    let kinds: Vec<&StoreChangeKind> = notifs.iter().map(|n| &n.change_kind).collect();
    assert!(kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeCreated { node_id, parent_id } if *node_id == folder && *parent_id == root_id)), "{kinds:?}");

    assert_eq!(children_of(&handler, store_id, root_id).await, vec![other, folder], "back at the end of its parent's list");
    assert_eq!(children_of(&handler, store_id, folder).await, vec![inner]);
    let inner_node = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: inner }).await.unwrap().node;
    assert_eq!(inner_node.metadata.tags, vec!["kept".to_string()]);
    assert_eq!(inner_node.parent_id, Some(folder));
}

/// A replica created empty around a placeholder root (what
/// `cloudAddHostedStore` does, since a vault twin's root id is not known in
/// the clear) adopts its root from the documents as they arrive, whatever
/// their order, and then shows the whole tree.
#[tokio::test]
async fn an_empty_replica_adopts_its_root_from_the_documents_that_arrive() {
    let (handler, store_id, root_id, _dir, manager) = new_handler_with_store_and_manager().await;
    let folder = create(&handler, store_id, root_id, "folder", "Folder").await;
    let inner = create(&handler, store_id, folder, "document", "Inner").await;
    let source = peer_tree(&handler, store_id, root_id).await;

    // A second store on the same handler, made the way a hosted replica is.
    let replica_dir = tempfile::tempdir().unwrap();
    let replica_id = StoreId::new();
    let placeholder = NodeId::new();
    {
        let mut manager = manager.write().await;
        manager.create_replica(replica_dir.path().join("replica.pimble"), replica_id, "Replica", placeholder).await.unwrap();
        assert_eq!(manager.root_node_id(replica_id).unwrap(), placeholder);
    }

    // The documents arrive child first, root last, each as a whole.
    for id in [inner, folder, root_id] {
        let whole = source.doc(id).unwrap().save();
        handler
            .apply_edit(&pimble_server::service_extensions(), ApplyEditRequest {
                store_id: replica_id,
                node_id: id,
                client_id: "sync-link:test".into(),
                operation: EditOperation::IncrementalChanges { changes: b64(&whole) },
            })
            .await
            .unwrap();
    }
    // The debounced repair has run by now; nothing needed fixing, since
    // every document arrived whole and the parents were only ever missing,
    // never wrong.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let manager = manager.read().await;
    assert_eq!(manager.root_node_id(replica_id).unwrap(), root_id, "the manifest root is the documents' root");
    let tree = manager.tree(replica_id).unwrap();
    assert!(tree.validate_tree().is_empty(), "{:?}", tree.validate_tree());
    assert_eq!(tree.get_children(root_id).unwrap(), vec![folder]);
    assert_eq!(tree.get_children(folder).unwrap(), vec![inner]);
    assert_eq!(tree.get_node_info(inner).unwrap().parent_id, Some(folder), "not re-parented while its parent had not arrived");
}
