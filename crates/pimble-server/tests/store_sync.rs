//! Server-boundary tests for store document sync (Phase B: store document on
//! yrs). These call `RpcHandler` directly, bypassing the network transport,
//! to exercise exactly the code paths a real client hits over JSON-RPC.

use std::sync::Arc;

use base64::Engine;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::StoreDocument;
use pimble_rpc::{
    ApplyStoreUpdateRequest, CreateNodeRequest, CreateStoreRequest, GetChildrenRequest,
    PimbleApiServer, SyncStoreDocumentRequest,
};
use pimble_server::RpcHandler;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

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

/// A fresh, unrelated `StoreDocument`'s state vector shares no actor with
/// the server's, so diffing the server against it yields the server's
/// complete current state — the standard "bootstrap a new replica" pattern.
fn fresh_disjoint_state_vector() -> Vec<u8> {
    StoreDocument::new("bootstrap", NodeId::new())
        .unwrap()
        .state_vector()
}

/// A fresh client with an empty (disjoint) state vector gets back a diff
/// that, loaded into a standalone `StoreDocument`, has a tree equal to the
/// server's.
#[tokio::test]
async fn sync_store_document_hands_new_client_the_full_tree() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let node_resp = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Doc".into(),
        })
        .await
        .unwrap();
    let doc_id = node_resp.node_id;

    let sv_b64 = base64::engine::general_purpose::STANDARD.encode(fresh_disjoint_state_vector());

    let sync_resp = handler
        .sync_store_document(&pimble_server::service_extensions(), SyncStoreDocumentRequest {
            store_id,
            state_vector: sv_b64,
        })
        .await
        .unwrap();

    let diff_bytes = base64::engine::general_purpose::STANDARD
        .decode(&sync_resp.diff)
        .unwrap();
    let loaded = StoreDocument::load(&diff_bytes).unwrap();

    assert_eq!(loaded.root_node_id().unwrap(), root_id);
    assert_eq!(loaded.get_children(root_id).unwrap(), vec![doc_id]);
    let info = loaded.get_node_info(doc_id).unwrap();
    assert_eq!(info.title, "Doc");
    assert_eq!(info.parent_id, Some(root_id));

    // The server's returned state vector should reflect that it holds
    // history now (non-empty).
    let server_sv_bytes = base64::engine::general_purpose::STANDARD
        .decode(&sync_resp.state_vector)
        .unwrap();
    assert!(!server_sv_bytes.is_empty());
}

/// `applyStoreUpdate` with a diff that adds a node under the shared root
/// makes `getChildren` on the server include it.
#[tokio::test]
async fn apply_store_update_with_a_new_node_reaches_get_children() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    // Bootstrap an independent, fully mergeable replica of the server's
    // current store document via the same "disjoint state vector" sync path
    // exercised above — this is how a real peer would first obtain the tree.
    let bootstrap_resp = handler
        .sync_store_document(&pimble_server::service_extensions(), SyncStoreDocumentRequest {
            store_id,
            state_vector: base64::engine::general_purpose::STANDARD
                .encode(fresh_disjoint_state_vector()),
        })
        .await
        .unwrap();
    let snapshot_bytes = base64::engine::general_purpose::STANDARD
        .decode(&bootstrap_resp.diff)
        .unwrap();
    let mut peer = StoreDocument::load(&snapshot_bytes).unwrap();
    let peer_sv_before = peer.state_vector();

    // The peer adds a node under the shared root...
    let new_node_id = NodeId::new();
    peer.add_node(new_node_id, Some(root_id), "document", "From Peer").unwrap();

    // ...and sends only the resulting diff to the server.
    let update = peer.diff_since(&peer_sv_before).unwrap();
    let update_b64 = base64::engine::general_purpose::STANDARD.encode(&update);

    handler
        .apply_store_update(&pimble_server::service_extensions(), ApplyStoreUpdateRequest {
            store_id,
            client_id: "peer".into(),
            update: update_b64,
        })
        .await
        .unwrap();

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
