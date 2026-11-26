//! Server-boundary tests for the search index (Step 5: search and graph
//! index on rhypedb). These call `RpcHandler` directly, exercising the
//! in-process `IndexFeed` -> debounced per-node upsert -> `search` /
//! `rebuildIndex` RPC path exactly as a real client would drive it.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use pimble_crdt::NodeDoc;
use pimble_rpc::{
    CloseStoreRequest, CreateNodeRequest, CreateStoreRequest, DeleteNodeRequest,
    ApplyEditRequest, EditOperation, OpenStoreRequest, PimbleApiServer, RebuildIndexRequest, SearchRequest,
};
use pimble_server::RpcHandler;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

/// How long a test waits for the index to catch up with what it was told.
/// Indexing runs behind the store's index opening and a 2 s per-node debounce,
/// so on a loaded machine it arrives late, never not at all (events queue
/// while the index opens, and a debounce only ever delays): the tests wait for
/// what they look for, under a deadline far past any delay but a lost edit.
/// Before 2026-11-26 they slept 2.5 s and searched once, and a full suite at
/// load average 90 found nothing.
const INDEX_CATCH_UP: Duration = Duration::from_secs(60);

/// Set up an `RpcHandler` backed by a fresh temporary store. Returns the
/// handler plus the store/root ids, and the `TempDir` guard (keep it alive
/// for the test's duration so the directory isn't cleaned up early).
async fn new_handler_with_store() -> (RpcHandler, pimble_core::StoreId, pimble_core::NodeId, tempfile::TempDir) {
    let store_manager = Arc::new(RwLock::new(StoreManager::new()));
    let dir = tempfile::tempdir().unwrap();
    let handler = RpcHandler::in_dir(store_manager, dir.path());

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

/// Replace a node's content with a full snapshot containing `text`, via
/// `updateNodeContent` (the same path a client uses to seed a document).
async fn set_text(
    handler: &RpcHandler,
    store_id: pimble_core::StoreId,
    node_id: pimble_core::NodeId,
    text: &str,
) {
    let doc = NodeDoc::from_plain_text(text).unwrap();
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(doc.save());
    handler
        .apply_edit(&pimble_server::service_extensions(), ApplyEditRequest { store_id, node_id, client_id: "seed".into(), operation: EditOperation::IncrementalChanges { changes: content_b64 } })
        .await
        .unwrap();
}

/// What `store_id`'s index is doing, as `getStoreSync` says.
async fn index_state(handler: &RpcHandler, store_id: pimble_core::StoreId) -> pimble_core::IndexState {
    handler
        .get_store_sync(&pimble_server::service_extensions(), pimble_rpc::GetStoreSyncRequest { store_id })
        .await
        .unwrap()
        .index
}

/// Wait (up to `within`) until `store_id`'s index is ready; whether it got
/// there.
async fn wait_index_ready(handler: &RpcHandler, store_id: pimble_core::StoreId, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if index_state(handler, store_id).await.is_ready() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// Search until `done` says the answer is the one looked for, or
/// [`INDEX_CATCH_UP`] has passed; the last answer either way, for the caller
/// to assert on.
async fn search_until(
    handler: &RpcHandler,
    query: &str,
    stores: Vec<pimble_core::StoreId>,
    done: impl Fn(&pimble_rpc::SearchResponse) -> bool,
) -> pimble_rpc::SearchResponse {
    let deadline = tokio::time::Instant::now() + INDEX_CATCH_UP;
    loop {
        let response = search(handler, query, stores.clone()).await;
        if done(&response) || tokio::time::Instant::now() >= deadline {
            return response;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// [`search_until`] some hit comes.
async fn search_until_found(handler: &RpcHandler, query: &str, stores: Vec<pimble_core::StoreId>) -> pimble_rpc::SearchResponse {
    search_until(handler, query, stores, |r| !r.results.is_empty()).await
}

async fn search(handler: &RpcHandler, query: &str, stores: Vec<pimble_core::StoreId>) -> pimble_rpc::SearchResponse {
    handler
        .search(&pimble_server::service_extensions(), SearchRequest {
            query: query.into(),
            stores,
            semantic: false,
            limit: 20,
        })
        .await
        .unwrap()
}

/// Create a store, create two documents, set distinct content on each; once
/// the per-node content-index debounce has passed and the index has caught
/// up, `search` finds the one whose text contains the query term.
#[tokio::test]
async fn search_finds_the_document_containing_the_query_term() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let doc_a = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Alpha".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;
    let doc_b = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Beta".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;

    set_text(&handler, store_id, doc_a, "the quick brown fox jumps").await;
    set_text(&handler, store_id, doc_b, "a completely different sentence").await;

    let response = search_until_found(&handler, "fox", Vec::new()).await;

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
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Gone Soon".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;

    set_text(&handler, store_id, doc_id, "ephemeral content here").await;
    let before = search_until_found(&handler, "ephemeral", Vec::new()).await;
    assert_eq!(before.results.len(), 1, "expected the node to be indexed before delete");

    handler
        .delete_node(&pimble_server::service_extensions(), DeleteNodeRequest { store_id, node_id: doc_id })
        .await
        .unwrap();

    // `Remove` is applied immediately (not debounced) but still goes through
    // the indexer's async channel/task: waited for.
    let after = search_until(&handler, "ephemeral", Vec::new(), |r| r.results.is_empty()).await;
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
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Rebuilt".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler, store_id, doc_id, "durable searchable text").await;
    // Indexed the ordinary way first, so the search after the rebuild finds
    // what the rebuild left (a later debounced upsert, such as the flush
    // stamp's, could add it again; `indexed` is what shows the rebuild ran).
    let indexed = search_until_found(&handler, "durable", vec![store_id]).await;
    assert_eq!(indexed.results.len(), 1, "indexed before the rebuild: {:?}", indexed.results);

    let rebuild_resp = handler
        .rebuild_index(&pimble_server::service_extensions(), RebuildIndexRequest { store_id })
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
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id: store_a,
            parent_id: Some(root_a),
            node_type: "document".into(),
            title: "In A".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler_a, store_a, doc_a, "unique marker zephyr").await;

    let doc_b = handler_b
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id: store_b,
            parent_id: Some(root_b),
            node_type: "document".into(),
            title: "In B".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler_b, store_b, doc_b, "unrelated other text").await;

    // Scoped to store_a explicitly, handler_a's own index (empty `stores`
    // also resolves to exactly its own open stores) finds only doc_a.
    let response = search_until_found(&handler_a, "zephyr", Vec::new()).await;
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].node_id, doc_a);

    // handler_b's store never indexed "zephyr": its own text is indexed
    // first, so the empty answer below is not merely an index behind.
    let indexed_b = search_until_found(&handler_b, "unrelated", Vec::new()).await;
    assert_eq!(indexed_b.results.len(), 1, "store b's own text is indexed: {:?}", indexed_b.results);
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
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Survives Restart".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler, store_id, doc_id, "persistent indexed content").await;
    let before = search_until_found(&handler, "persistent", vec![store_id]).await;
    assert_eq!(before.results.len(), 1, "expected the node to be indexed before closing");

    handler.close_store(&pimble_server::service_extensions(), CloseStoreRequest { store_id }).await.unwrap();
    handler.open_store(&pimble_server::service_extensions(), OpenStoreRequest { path: store_path }).await.unwrap();
    // The index opens in the background (docs/STARTUP_CONTRACT.md).
    assert!(wait_index_ready(&handler, store_id, INDEX_CATCH_UP).await, "the index opened again");

    let after = search(&handler, "persistent", vec![store_id]).await;
    assert_eq!(
        after.results.len(),
        1,
        "expected the reopened index to still find the node, got {:?}",
        after.results
    );
    assert_eq!(after.results[0].node_id, doc_id);
}

/// A board node (docs/IMAGE_MARKUP_CONTRACT.md): created with its board in
/// the same edit, the way the app makes one, it holds that board and its
/// custom keys at once, and the words written on the board are what search
/// finds in it.
#[tokio::test]
async fn a_board_node_starts_with_its_board_and_its_words_are_found() {
    use pimble_board::{Board, Geom, Kind, NewElement, Rect, Stamp};

    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let board = Board::standalone();
    let (_, update) = board
        .edit(Stamp::new(None, "2026-10-09T10:00:00.000Z"), |t| {
            t.create()?;
            t.add_element(NewElement::new(Kind::Sticky, Geom::Rect(Rect::new(0.0, 0.0, 200.0, 120.0))).with_text("periwinkle retro notes"))
        })
        .into_result()
        .unwrap();
    let mut custom = std::collections::HashMap::new();
    custom.insert("icon".to_string(), serde_json::json!("chalkboard"));
    let node_id = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: pimble_core::node_types::BOARD.into(),
            title: "Whiteboard".into(),
            content: Some(base64::engine::general_purpose::STANDARD.encode(update.unwrap())),
            custom,
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;

    let node = handler
        .get_node(&pimble_server::service_extensions(), pimble_rpc::GetNodeRequest { store_id, node_id })
        .await
        .unwrap()
        .node;
    assert_eq!(node.node_type, pimble_core::node_types::BOARD);
    assert_eq!(node.metadata.icon(), Some("chalkboard"));
    let doc = NodeDoc::load(&node.content).unwrap();
    let held = Board::of_node(&doc).snapshot();
    assert!(held.exists, "the board came with the node");
    assert_eq!(held.texts().len(), 1);

    let response = search_until_found(&handler, "periwinkle", Vec::new()).await;
    assert_eq!(response.results.len(), 1, "got {:?}", response.results);
    assert_eq!(response.results[0].node_id, node_id);
    assert_eq!(response.results[0].node_type, pimble_core::node_types::BOARD);
}


/// docs/STARTUP_CONTRACT.md: a store whose index another program holds
/// answers its open at once, says so (`Held`), is named by search as still
/// being indexed, and opens its index the moment it is let go: never
/// rebuilt over (the directory, and what it found, are the same after).
#[tokio::test]
async fn a_locked_index_is_waited_for_and_never_rebuilt() {
    use std::os::unix::fs::MetadataExt;
    let (handler, store_id, root_id, dir) = new_handler_with_store().await;
    let store_path = dir.path().join("test.pimble");
    let doc_id = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            title: "Lighthouse".into(),
            content: None,
            custom: Default::default(),
            place: Default::default(),
        })
        .await
        .unwrap()
        .node_id;
    set_text(&handler, store_id, doc_id, "keepers of the lamp").await;
    assert!(wait_index_ready(&handler, store_id, INDEX_CATCH_UP).await);
    assert_eq!(search_until_found(&handler, "lamp", vec![store_id]).await.results.len(), 1);
    handler.close_store(&pimble_server::service_extensions(), CloseStoreRequest { store_id }).await.unwrap();

    // Another program holds the index (as pimble-mcp's server did after a
    // hand-over, 2026-11-20).
    let index_dir = store_path.join("index").join("rhypedb");
    let inode = std::fs::metadata(index_dir.join("LOCK")).unwrap().ino();
    // The handler's own schema (semantic when this build has it).
    let holder = pimble_search::SearchIndex::open(&index_dir, true).unwrap();

    // The open answers without its index, and before the brief wait for a
    // lock is over (`Held` is said only once it is): by what it says, not by
    // a stopwatch a loaded machine stretches.
    let opened = handler.open_store(&pimble_server::service_extensions(), OpenStoreRequest { path: store_path.clone() }).await.unwrap();
    assert!(opened.store.index.is_indexing(), "the open answers without its index: {:?}", opened.store.index);
    let answer = search(&handler, "lamp", Vec::new()).await;
    assert_eq!(answer.indexing, vec![store_id], "search says the store is still being indexed");
    assert!(answer.results.is_empty());

    // Still held after the quiet wait: said, and not rebuilt over.
    let deadline = tokio::time::Instant::now() + INDEX_CATCH_UP;
    while index_state(&handler, store_id).await != pimble_core::IndexState::Held {
        assert!(tokio::time::Instant::now() < deadline, "the store says its index is held: {:?}", index_state(&handler, store_id).await);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(std::fs::metadata(index_dir.join("LOCK")).unwrap().ino(), inode, "nothing deleted the held directory");

    // Let go: it opens, as it was.
    drop(holder);
    assert!(wait_index_ready(&handler, store_id, INDEX_CATCH_UP).await, "opened once let go");
    assert_eq!(std::fs::metadata(index_dir.join("LOCK")).unwrap().ino(), inode, "the same index, not a rebuilt one");
    let found = search(&handler, "lamp", vec![store_id]).await;
    assert_eq!(found.results.len(), 1, "what it held is found");
    assert!(found.indexing.is_empty());
}
