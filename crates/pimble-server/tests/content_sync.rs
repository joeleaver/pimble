//! Server-boundary tests for node content sync (Phase A: content on yrs,
//! now one document per node with its structure beside it).
//!
//! These call `RpcHandler` directly, bypassing the network transport, to
//! exercise exactly the code paths a real client hits over JSON-RPC: the
//! server is the authoritative peer holding one `NodeDoc` per node, and
//! relays/persists updates without ever re-encoding or interpreting them.

use std::sync::Arc;

use base64::Engine;
use pimble_crdt::NodeDoc;
use pimble_rpc::{
    ApplyEditRequest, CreateNodeRequest, CreateStoreRequest, EditOperation, GetNodeRequest,
    NodeStateVector, PimbleApiServer, SyncNodesRequest, UpdateNodeContentRequest,
};
use pimble_server::RpcHandler;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

/// Set up an `RpcHandler` backed by a fresh temporary store, with one
/// document node created under the root. Returns the handler plus the
/// store/node ids, and the `TempDir` guard (keep it alive for the test's
/// duration so the directory isn't cleaned up early).
async fn new_handler_with_store() -> (RpcHandler, pimble_core::StoreId, pimble_core::NodeId, tempfile::TempDir) {
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

    let node_resp = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id: create_resp.store_id,
            parent_id: Some(create_resp.root_node_id),
            node_type: "document".into(),
            title: "Doc".into(),
        })
        .await
        .unwrap();

    (handler, create_resp.store_id, node_resp.node_id, dir)
}

/// Client A pushes a snapshot via `updateNodeContent`; client B, syncing from
/// an empty state vector, receives a diff whose text is "Hello".
#[tokio::test]
async fn sync_node_content_hands_new_client_the_full_document() {
    let (handler, store_id, node_id, _dir) = new_handler_with_store().await;

    let a_doc = NodeDoc::from_plain_text("Hello").unwrap();
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(a_doc.save());

    handler
        .update_node_content(&pimble_server::service_extensions(), UpdateNodeContentRequest {
            store_id,
            node_id,
            content: content_b64,
            client_id: Some("client-a".into()),
        })
        .await
        .unwrap();

    // An "empty" state vector is the properly-encoded state vector of an
    // empty document, not literally zero bytes.
    let empty_sv_b64 = base64::engine::general_purpose::STANDARD
        .encode(NodeDoc::new().state_vector());
    let sync_resp = handler
        .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest {
            store_id,
            nodes: vec![NodeStateVector { node_id, state_vector: empty_sv_b64 }],
            list_unknown: false,
        })
        .await
        .unwrap();
    assert_eq!(sync_resp.nodes.len(), 1);
    let sync_resp = &sync_resp.nodes[0];
    assert_eq!(sync_resp.node_id, node_id);

    // The diff is the whole node document: its text, and its place.
    let diff_bytes = base64::engine::general_purpose::STANDARD
        .decode(&sync_resp.diff)
        .unwrap();
    let doc = NodeDoc::load(&diff_bytes).unwrap();
    assert_eq!(doc.text(), "Hello");
    assert_eq!(doc.fields().unwrap().title, "Doc");

    // The server's returned state vector should not be empty now that it
    // holds content.
    let server_sv_bytes = base64::engine::general_purpose::STANDARD
        .decode(&sync_resp.state_vector)
        .unwrap();
    assert!(!server_sv_bytes.is_empty());
}

/// `applyEdit` with `IncrementalChanges` built from a diff of a doc that has
/// more text results in the server's document containing that text.
#[tokio::test]
async fn apply_edit_incremental_changes_merges_into_server_document() {
    let (handler, store_id, node_id, _dir) = new_handler_with_store().await;

    let base = NodeDoc::from_plain_text("Hello").unwrap();
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(base.save());
    handler
        .update_node_content(&pimble_server::service_extensions(), UpdateNodeContentRequest {
            store_id,
            node_id,
            content: content_b64,
            client_id: Some("client-a".into()),
        })
        .await
        .unwrap();

    // A peer document that has diverged with more text than the server
    // currently holds state for.
    let richer = NodeDoc::from_plain_text("Hello\nWorld").unwrap();
    let base_sv = base.state_vector();
    let diff = richer.diff_since(&base_sv).unwrap();
    let changes_b64 = base64::engine::general_purpose::STANDARD.encode(&diff);

    handler
        .apply_edit(&pimble_server::service_extensions(), ApplyEditRequest {
            store_id,
            node_id,
            client_id: "client-a".into(),
            operation: EditOperation::IncrementalChanges { changes: changes_b64 },
        })
        .await
        .unwrap();

    let node = handler
        .get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id })
        .await
        .unwrap()
        .node;

    let server_text = NodeDoc::text_of(&node.content);
    assert!(
        server_text.contains("Hello") && server_text.contains("World"),
        "expected server text to contain both \"Hello\" and \"World\", got {:?}",
        server_text
    );
}

/// An `applyEdit(IncrementalChanges)` is never flushed synchronously (that
/// would mean a flush per keystroke), but it must still reach disk on its
/// own shortly after, via the debounced flush — a crash or force-quit should
/// never lose more than the debounce window's worth of edits.
#[tokio::test]
async fn apply_edit_incremental_changes_is_flushed_to_disk_after_debounce() {
    let (handler, store_id, node_id, dir) = new_handler_with_store().await;

    let base = NodeDoc::from_plain_text("Hello").unwrap();
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(base.save());
    handler
        .update_node_content(&pimble_server::service_extensions(), UpdateNodeContentRequest {
            store_id,
            node_id,
            content: content_b64,
            client_id: Some("client-a".into()),
        })
        .await
        .unwrap();

    let richer = NodeDoc::from_plain_text("Hello\nWorld").unwrap();
    let base_sv = base.state_vector();
    let diff = richer.diff_since(&base_sv).unwrap();
    let changes_b64 = base64::engine::general_purpose::STANDARD.encode(&diff);

    handler
        .apply_edit(&pimble_server::service_extensions(), ApplyEditRequest {
            store_id,
            node_id,
            client_id: "client-a".into(),
            operation: EditOperation::IncrementalChanges { changes: changes_b64 },
        })
        .await
        .unwrap();

    // Give the debounced flush (750ms) time to run, using real time.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // Reopen the store directory with a completely fresh StoreManager,
    // bypassing the handler's in-memory state entirely, to prove the edit
    // actually reached disk rather than just living in the server's cache.
    let store_path = dir.path().join("test.pimble");
    let mut fresh_manager = StoreManager::new();
    fresh_manager.open_local_store(&store_path).await.unwrap();
    let node = fresh_manager.get_node(store_id, node_id).unwrap();

    let text = NodeDoc::text_of(&node.content);
    assert!(
        text.contains("World"),
        "expected the debounced flush to have persisted the edit, got {:?}",
        text
    );
}
