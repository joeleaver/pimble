//! Server-boundary tests for `syncNodes` (docs/NODE_DOCUMENT_CONTRACT.md
//! section 4: the tree is the node documents, and one RPC reconciles them).
//! These call `RpcHandler` directly, bypassing the network transport, to
//! exercise exactly the code paths a real client hits over JSON-RPC.

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::{NodeDoc, Tree};
use pimble_rpc::{
    ApplyEditRequest, CreateNodeRequest, CreateStoreRequest, DeleteNodeRequest, EditOperation, GetChildrenRequest,
    NodeStateVector, PimbleApiServer, SyncNodesRequest, MAX_SYNC_NODE_CONTENTS,
};
use pimble_server::RpcHandler;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

const T0: &str = "2026-09-18T10:00:00Z";

/// Set up an `RpcHandler` backed by a fresh temporary store. Returns the
/// handler plus the store/root ids, and the `TempDir` guard (keep it alive
/// for the test's duration).
async fn new_handler_with_store() -> (RpcHandler, StoreId, NodeId, tempfile::TempDir) {
    let store_manager = Arc::new(RwLock::new(StoreManager::new()));
    let handler = RpcHandler::new(store_manager);

    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("test.pimble");

    let create_resp = handler
        .create_store(&pimble_server::service_extensions(), CreateStoreRequest { kind: Default::default(), store_id: None,
            path: store_path,
            name: "Test Store".into(),
        })
        .await
        .unwrap();

    (handler, create_resp.store_id, create_resp.root_node_id, dir)
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}

/// `syncNodes` as a fresh replica calls it: nothing named, everything
/// asked for, then the unknown ids fetched with empty state vectors.
async fn pull_everything(handler: &RpcHandler, store_id: StoreId) -> Vec<(NodeId, Vec<u8>)> {
    let listing = handler
        .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest { store_id, nodes: Vec::new(), list_unknown: true })
        .await
        .unwrap();
    assert!(listing.nodes.is_empty(), "nothing was named, so nothing is answered");

    let empty_sv = b64(&pimble_crdt::empty_state_vector());
    let mut out = Vec::new();
    for chunk in listing.unknown_ids.chunks(MAX_SYNC_NODE_CONTENTS) {
        let fetched = handler
            .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest {
                store_id,
                nodes: chunk.iter().map(|id| NodeStateVector { node_id: *id, state_vector: empty_sv.clone() }).collect(),
                list_unknown: false,
            })
            .await
            .unwrap();
        assert_eq!(fetched.nodes.len(), chunk.len(), "every named document is answered");
        assert!(fetched.unknown_ids.is_empty(), "not asked for");
        out.extend(fetched.nodes.into_iter().map(|n| (n.node_id, unb64(&n.diff))));
    }
    out
}

/// A fresh client that names nothing and asks what is unknown learns every
/// document id, fetches each whole, and holds a tree equal to the server's,
/// tombstones included.
#[tokio::test]
async fn sync_nodes_hands_a_fresh_replica_the_whole_tree() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let folder_id = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "folder".into(),
            title: "Folder".into(),
        })
        .await
        .unwrap()
        .node_id;
    let doc_id = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(folder_id),
            node_type: "document".into(),
            title: "Doc".into(),
        })
        .await
        .unwrap()
        .node_id;
    let doomed = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Doomed".into(),
        })
        .await
        .unwrap()
        .node_id;
    handler.delete_node(&pimble_server::service_extensions(), DeleteNodeRequest { store_id, node_id: doomed }).await.unwrap();

    let pulled = pull_everything(&handler, store_id).await;
    let ids: HashSet<NodeId> = pulled.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, HashSet::from([root_id, folder_id, doc_id, doomed]), "a deletion is part of the document, so the tombstone is listed");

    let docs = pulled.into_iter().map(|(id, bytes)| (id, NodeDoc::load(&bytes).unwrap())).collect();
    let replica = Tree::from_docs(root_id, docs);
    assert!(replica.validate_tree().is_empty());
    assert_eq!(replica.get_children(root_id).unwrap(), vec![folder_id]);
    assert_eq!(replica.get_children(folder_id).unwrap(), vec![doc_id]);
    assert_eq!(replica.get_node_info(doc_id).unwrap().title, "Doc");
    assert!(!replica.has_node(doomed), "the tombstone arrived as one");
    assert!(replica.doc(doomed).unwrap().fields().unwrap().deleted_at.is_some());
}

/// Naming a document with the state vector one already holds gets a diff
/// that carries nothing new, and the server's own state vector; naming an
/// id the server does not hold gets nothing for it.
#[tokio::test]
async fn sync_nodes_answers_only_what_the_server_holds() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;
    let pulled = pull_everything(&handler, store_id).await;
    let (_, root_bytes) = pulled.into_iter().find(|(id, _)| *id == root_id).unwrap();
    let held = NodeDoc::load(&root_bytes).unwrap();

    let unknown = NodeId::new();
    let resp = handler
        .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest {
            store_id,
            nodes: vec![
                NodeStateVector { node_id: root_id, state_vector: b64(&held.state_vector()) },
                NodeStateVector { node_id: unknown, state_vector: b64(&pimble_crdt::empty_state_vector()) },
            ],
            list_unknown: true,
        })
        .await
        .unwrap();
    assert_eq!(resp.nodes.len(), 1, "the unknown id is left out, not an error");
    assert_eq!(resp.nodes[0].node_id, root_id);
    assert!(resp.unknown_ids.is_empty(), "everything the server holds was named");

    let mut held = held;
    let effect = held.apply_update(&unb64(&resp.nodes[0].diff)).unwrap();
    assert!(!effect.changed, "a peer that is up to date is told nothing new");
    assert_eq!(unb64(&resp.nodes[0].state_vector), held.state_vector());
}

#[tokio::test]
async fn sync_nodes_refuses_more_than_the_batch_limit() {
    let (handler, store_id, _root_id, _dir) = new_handler_with_store().await;
    let nodes = (0..=MAX_SYNC_NODE_CONTENTS)
        .map(|_| NodeStateVector { node_id: NodeId::new(), state_vector: b64(&pimble_crdt::empty_state_vector()) })
        .collect();
    let err = handler
        .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest { store_id, nodes, list_unknown: false })
        .await
        .expect_err("over the limit");
    assert!(err.message().contains("at most"), "{}", err.message());
}

/// A peer that holds the tree adds a node in its own `Tree` and sends the
/// two updates (the node's document, the parent's list) as `applyEdit`s;
/// `getChildren` on the server then includes it. This is how a replica's
/// structural edit reaches a server: the same RPC as a keystroke.
#[tokio::test]
async fn apply_edit_with_a_peers_new_node_reaches_get_children() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let docs = pull_everything(&handler, store_id).await.into_iter().map(|(id, bytes)| (id, NodeDoc::load(&bytes).unwrap())).collect();
    let mut peer = Tree::from_docs(root_id, docs);

    let new_node_id = NodeId::new();
    let edit = peer.add_node(new_node_id, Some(root_id), None, "document", "From Peer", T0).unwrap();
    assert_eq!(edit.touched.len(), 2, "the node's init and the root's list");

    for (node_id, update) in &edit.touched {
        handler
            .apply_edit(&pimble_server::service_extensions(), ApplyEditRequest {
                store_id,
                node_id: *node_id,
                client_id: "peer".into(),
                operation: EditOperation::IncrementalChanges { changes: b64(update) },
            })
            .await
            .unwrap();
    }

    let children_resp = handler
        .get_children(&pimble_server::service_extensions(), GetChildrenRequest { store_id, node_id: root_id })
        .await
        .unwrap();

    assert!(
        children_resp
            .children
            .iter()
            .any(|n| n.id == new_node_id && n.metadata.title == "From Peer"),
        "expected the server's children of root to include the peer's new node"
    );
}
