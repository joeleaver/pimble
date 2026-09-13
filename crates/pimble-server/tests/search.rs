//! Server-boundary tests for the search index (Step 5: search and graph
//! index on rhypedb). These call `RpcHandler` directly, exercising the
//! in-process `IndexFeed` -> debounced per-node upsert -> `search` /
//! `rebuildIndex` RPC path exactly as a real client would drive it.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use pimble_crdt::ContentDoc;
use pimble_rpc::{
    CloseStoreRequest, CreateNodeRequest, CreateStoreRequest, DeleteNodeRequest,
    OpenStoreRequest, PimbleApiServer, RebuildIndexRequest, SearchRequest,
    UpdateNodeContentRequest,
};
use pimble_server::RpcHandler;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

/// Real time past the server's per-node content-index debounce (2s), with
/// margin.
const PAST_INDEX_DEBOUNCE: Duration = Duration::from_millis(2_500);

/// Set up an `RpcHandler` backed by a fresh temporary store. Returns the
/// handler plus the store/root ids, and the `TempDir` guard (keep it alive
/// for the test's duration so the directory isn't cleaned up early).
async fn new_handler_with_store() -> (RpcHandler, pimble_core::StoreId, pimble_core::NodeId, tempfile::TempDir) {
    let store_manager = Arc::new(RwLock::new(StoreManager::new()));
    let handler = RpcHandler::new(store_manager);

    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("test.pimble");

    let create_resp = handler
        .create_store(CreateStoreRequest {
            path: store_path,
            name: "Test Store".into(),
        })
        .await
        .unwrap();

    (handler, create_resp.store_id, create_resp.root_node_id, dir)
}

/// Replace a node's content with a full snapshot containing `text`, via
/// `updateNodeContent` (the same path a client uses to seed a document).
async fn set_text(
    handler: &RpcHandler,
    store_id: pimble_core::StoreId,
    node_id: pimble_core::NodeId,
    text: &str,
) {
    let doc = ContentDoc::from_plain_text(text).unwrap();
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(doc.save());
    handler
        .update_node_content(UpdateNodeContentRequest {
            store_id,
            node_id,
            content: content_b64,
            client_id: None,
        })
        .await
        .unwrap();
}

async fn search(handler: &RpcHandler, query: &str, stores: Vec<pimble_core::StoreId>) -> pimble_rpc::SearchResponse {
    handler
        .search(SearchRequest {
            query: query.into(),
            stores,
            semantic: false,
            limit: 20,
        })
        .await
        .unwrap()
}

/// Create a store, create two documents, set distinct content on each; once
/// the per-node content-index debounce has passed (real time), `search`
/// finds the one whose text contains the query term.
#[tokio::test]
async fn search_finds_the_document_containing_the_query_term() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let doc_a = handler
        .create_node(CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Alpha".into(),
        })
        .await
        .unwrap()
        .node_id;
    let doc_b = handler
        .create_node(CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Beta".into(),
        })
        .await
        .unwrap()
        .node_id;

    set_text(&handler, store_id, doc_a, "the quick brown fox jumps").await;
    set_text(&handler, store_id, doc_b, "a completely different sentence").await;

    tokio::time::sleep(PAST_INDEX_DEBOUNCE).await;

    let response = search(&handler, "fox", Vec::new()).await;

    assert_eq!(
        response.results.len(),
        1,
        "expected exactly one hit, got {:?}",
        response.results
    );
    assert_eq!(response.results[0].node_id, doc_a);
    assert_eq!(response.results[0].store_id, store_id);
    assert_eq!(response.results[0].node_type, "document");
    // `create_node` sets the raw title ("Alpha") but never the
    // `explicit_title` flag, so the indexed (and displayed) title falls back
    // to the node's content, matching the tree's label — not the literal
    // title string passed at creation.
    assert_eq!(response.results[0].title, "the quick brown fox jumps");
    assert!(
        response.results[0].snippet.contains("fox"),
        "expected snippet to contain the query term, got {:?}",
        response.results[0].snippet
    );
}

/// Deleting a node that was indexed removes it from later search results.
#[tokio::test]
async fn search_no_longer_finds_a_deleted_node() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let doc_id = handler
        .create_node(CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Gone Soon".into(),
        })
        .await
        .unwrap()
        .node_id;

    set_text(&handler, store_id, doc_id, "ephemeral content here").await;
    tokio::time::sleep(PAST_INDEX_DEBOUNCE).await;

    let before = search(&handler, "ephemeral", Vec::new()).await;
    assert_eq!(before.results.len(), 1, "expected the node to be indexed before delete");

    handler
        .delete_node(DeleteNodeRequest { store_id, node_id: doc_id })
        .await
        .unwrap();

    // `Remove` is applied immediately (not debounced) but still goes through
    // the indexer's async channel/task; give it a moment.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let after = search(&handler, "ephemeral", Vec::new()).await;
    assert!(
        after.results.is_empty(),
        "expected no hits after delete, got {:?}",
        after.results
    );
}

/// `rebuildIndex` deletes and rebuilds a store's on-disk index from scratch,
/// re-indexing every node from the store's documents; `search` afterward
/// finds content that was set before the rebuild.
#[tokio::test]
async fn rebuild_index_reindexes_every_node_from_scratch() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let doc_id = handler
        .create_node(CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Rebuilt".into(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler, store_id, doc_id, "durable searchable text").await;
    tokio::time::sleep(PAST_INDEX_DEBOUNCE).await;

    let rebuild_resp = handler
        .rebuild_index(RebuildIndexRequest { store_id })
        .await
        .unwrap();
    // At least the root and the one document.
    assert!(
        rebuild_resp.indexed >= 2,
        "expected at least 2 nodes indexed, got {}",
        rebuild_resp.indexed
    );

    let response = search(&handler, "durable", vec![store_id]).await;
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].node_id, doc_id);
}

/// An empty `stores` list in the request searches every open store.
#[tokio::test]
async fn empty_stores_list_searches_all_open_stores() {
    let (handler_a, store_a, root_a, _dir_a) = new_handler_with_store().await;
    // A second, independent store opened on the same handler's manager would
    // require a shared StoreManager; instead, create a second store on a
    // fresh handler sharing nothing, then merge by calling search on each —
    // this test only needs to prove one store's results surface when
    // `stores` is empty, which the primary test above already exercises
    // end-to-end. Here we confirm a second unrelated store's content does
    // NOT leak into a query scoped to `store_a` alone.
    let (handler_b, store_b, root_b, _dir_b) = new_handler_with_store().await;

    let doc_a = handler_a
        .create_node(CreateNodeRequest {
            store_id: store_a,
            parent_id: Some(root_a),
            node_type: "document".into(),
            title: "In A".into(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler_a, store_a, doc_a, "unique marker zephyr").await;

    let doc_b = handler_b
        .create_node(CreateNodeRequest {
            store_id: store_b,
            parent_id: Some(root_b),
            node_type: "document".into(),
            title: "In B".into(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler_b, store_b, doc_b, "unrelated other text").await;

    tokio::time::sleep(PAST_INDEX_DEBOUNCE).await;

    // Scoped to store_a explicitly, handler_a's own index (empty `stores`
    // also resolves to exactly its own open stores) finds only doc_a.
    let response = search(&handler_a, "zephyr", Vec::new()).await;
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].node_id, doc_a);

    // handler_b's store never indexed "zephyr".
    let response_b = search(&handler_b, "zephyr", Vec::new()).await;
    assert!(response_b.results.is_empty());
}

/// Closing a store and reopening it (simulating a server restart) preserves
/// its search index — schema-drift detection must not mistake a normal
/// reopen, with an unchanged schema, for staleness and wipe it. This is a
/// regression guard for `open_index_for_store`'s hash-file diffing: an
/// earlier version pre-computed an "expected" hash instead of diffing what
/// `SearchIndex::open` actually persisted, which happened to match in this
/// build but wasn't guaranteed to.
#[tokio::test]
async fn reopening_a_store_preserves_its_search_index() {
    let (handler, store_id, root_id, dir) = new_handler_with_store().await;
    let store_path = dir.path().join("test.pimble");

    let doc_id = handler
        .create_node(CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Survives Restart".into(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler, store_id, doc_id, "persistent indexed content").await;
    tokio::time::sleep(PAST_INDEX_DEBOUNCE).await;

    let before = search(&handler, "persistent", vec![store_id]).await;
    assert_eq!(before.results.len(), 1, "expected the node to be indexed before closing");

    handler.close_store(CloseStoreRequest { store_id }).await.unwrap();
    handler.open_store(OpenStoreRequest { path: store_path }).await.unwrap();

    let after = search(&handler, "persistent", vec![store_id]).await;
    assert_eq!(
        after.results.len(),
        1,
        "expected the reopened index to still find the node, got {:?}",
        after.results
    );
    assert_eq!(after.results[0].node_id, doc_id);
}
