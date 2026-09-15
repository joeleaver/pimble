//! Server-boundary tests for mounts (docs/MOUNTS_CONTRACT.md). These call
//! `RpcHandler` directly, bypassing the network transport, to exercise
//! exactly the code paths a real client hits over JSON-RPC — the same
//! pattern `store_sync.rs` and `content_sync.rs` use.

use std::sync::Arc;

use pimble_core::{MountState, NodeId, StoreId};
use pimble_rpc::{
    CreateMountRequest, CreateMountResponse, CreateNodeRequest, CreateStoreRequest,
    CreateStoreResponse, GetChildrenRequest, GetMountStateRequest, OpenStoreRequest,
    PimbleApiServer, StoreChangeKind, StoreChangedNotification,
};
use pimble_server::RpcHandler;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

/// A fresh `RpcHandler` plus the `Arc<RwLock<StoreManager>>` backing it, so
/// a test can flush stores directly (simulating a clean shutdown) or open
/// its own fresh manager over the same directories (simulating a restart).
fn new_handler() -> (RpcHandler, Arc<RwLock<StoreManager>>) {
    let store_manager = Arc::new(RwLock::new(StoreManager::new()));
    let handler = RpcHandler::new(Arc::clone(&store_manager));
    (handler, store_manager)
}

/// Create a local store at `path` and return its id and root node id.
async fn create_store(handler: &RpcHandler, path: &std::path::Path, name: &str) -> (StoreId, NodeId) {
    let resp = handler
        .create_store(CreateStoreRequest {
            path: path.to_path_buf(),
            name: name.into(),
        })
        .await
        .unwrap();
    (resp.store_id, resp.root_node_id)
}

/// Create a document node under `parent_id` in `store_id` and return its id.
async fn create_doc(handler: &RpcHandler, store_id: StoreId, parent_id: NodeId, title: &str) -> NodeId {
    handler
        .create_node(CreateNodeRequest {
            store_id,
            parent_id: Some(parent_id),
            node_type: "document".into(),
            title: title.into(),
        })
        .await
        .unwrap()
        .node_id
}

/// 1. `createMount` then `getChildren` on the mount returns the source
/// node's children, addressed by the source store's id.
#[tokio::test]
async fn get_children_on_a_mount_resolves_to_the_source_store() {
    let (handler, _store_manager) = new_handler();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let (a_store, a_root) = create_store(&handler, &dir_a.path().join("a.pimble"), "A").await;
    let (b_store, b_root) = create_store(&handler, &dir_b.path().join("b.pimble"), "B").await;
    let b_doc = create_doc(&handler, b_store, b_root, "B Doc").await;

    let mount_resp = handler
        .create_mount(CreateMountRequest {
            store_id: a_store,
            parent_id: a_root,
            source_store_id: b_store,
            source_node_id: b_root,
            title: Some("B Mount".into()),
        })
        .await
        .unwrap();

    let children_resp = handler
        .get_children(GetChildrenRequest {
            store_id: a_store,
            node_id: mount_resp.node_id,
        })
        .await
        .unwrap();

    assert_eq!(children_resp.store_id, b_store);
    assert_eq!(children_resp.children.len(), 1);
    assert_eq!(children_resp.children[0].id, b_doc);
    assert_eq!(children_resp.children[0].metadata.title, "B Doc");
}

/// 2. `getMountState` returns `Live` and a `mount_ref` whose `source_path`
/// is the source store's directory.
#[tokio::test]
async fn get_mount_state_is_live_with_source_path_set() {
    let (handler, _store_manager) = new_handler();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let b_path = dir_b.path().join("b.pimble");

    let (a_store, a_root) = create_store(&handler, &dir_a.path().join("a.pimble"), "A").await;
    let (b_store, b_root) = create_store(&handler, &b_path, "B").await;

    let mount_resp = handler
        .create_mount(CreateMountRequest {
            store_id: a_store,
            parent_id: a_root,
            source_store_id: b_store,
            source_node_id: b_root,
            title: None,
        })
        .await
        .unwrap();

    // create_mount itself should already report the path hint it filled in.
    assert_eq!(mount_resp.mount_ref.source_path.as_deref(), Some(b_path.as_path()));

    let state_resp = handler
        .get_mount_state(GetMountStateRequest {
            store_id: a_store,
            node_id: mount_resp.node_id,
        })
        .await
        .unwrap();

    assert!(matches!(state_resp.state, MountState::Live));
    assert_eq!(state_resp.mount_ref.source_path.as_deref(), Some(b_path.as_path()));
}

/// 3. Restart: a fresh `StoreManager`/`RpcHandler` over the same
/// directories, opening only the mounting store, still resolves the mount
/// through `source_path`, and `listStores` then includes the source store.
#[tokio::test]
async fn restart_resolves_mount_via_source_path_hint() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a_path = dir_a.path().join("a.pimble");
    let b_path = dir_b.path().join("b.pimble");

    let (a_store, mount_node_id, b_store, b_doc) = {
        let (handler, store_manager) = new_handler();
        let (a_store, a_root) = create_store(&handler, &a_path, "A").await;
        let (b_store, b_root) = create_store(&handler, &b_path, "B").await;
        let b_doc = create_doc(&handler, b_store, b_root, "B Doc").await;

        let mount_resp = handler
            .create_mount(CreateMountRequest {
                store_id: a_store,
                parent_id: a_root,
                source_store_id: b_store,
                source_node_id: b_root,
                title: None,
            })
            .await
            .unwrap();

        // Flush everything to disk before "shutting down" — the same thing
        // `PimbleServer::stop` does on a real shutdown.
        store_manager.write().await.flush_all().await.unwrap();

        (a_store, mount_resp.node_id, b_store, b_doc)
    };

    // Fresh manager/handler over the same directories, as if the process
    // restarted. Only the mounting store is opened.
    let (handler, _store_manager) = new_handler();
    handler
        .open_store(OpenStoreRequest { path: a_path.clone() })
        .await
        .unwrap();

    let children_resp = handler
        .get_children(GetChildrenRequest {
            store_id: a_store,
            node_id: mount_node_id,
        })
        .await
        .unwrap();

    assert_eq!(children_resp.store_id, b_store);
    assert_eq!(children_resp.children.len(), 1);
    assert_eq!(children_resp.children[0].id, b_doc);

    let stores_resp = handler.list_stores().await.unwrap();
    let ids: Vec<StoreId> = stores_resp.stores.iter().map(|s| s.id).collect();
    assert!(ids.contains(&a_store), "expected list_stores to include the mounting store");
    assert!(
        ids.contains(&b_store),
        "expected list_stores to include the source store opened to resolve the mount"
    );
}

/// 4. Source gone: renaming the source store's directory away makes
/// `getMountState` report `Unavailable` and `getChildren` on the mount an
/// error.
#[tokio::test]
async fn mount_source_directory_gone_is_unavailable() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a_path = dir_a.path().join("a.pimble");
    let b_path = dir_b.path().join("b.pimble");
    let b_moved_path = dir_b.path().join("b-moved.pimble");

    let (a_store, mount_node_id) = {
        let (handler, store_manager) = new_handler();
        let (a_store, a_root) = create_store(&handler, &a_path, "A").await;
        let (b_store, b_root) = create_store(&handler, &b_path, "B").await;

        let mount_resp = handler
            .create_mount(CreateMountRequest {
                store_id: a_store,
                parent_id: a_root,
                source_store_id: b_store,
                source_node_id: b_root,
                title: None,
            })
            .await
            .unwrap();

        store_manager.write().await.flush_all().await.unwrap();

        (a_store, mount_resp.node_id)
    };

    // The source store's directory disappears before a fresh handler opens
    // the mounting store, so neither "already open" nor "registry entry"
    // resolves it, and the source_path hint points nowhere.
    std::fs::rename(&b_path, &b_moved_path).unwrap();

    let (handler, _store_manager) = new_handler();
    handler
        .open_store(OpenStoreRequest { path: a_path.clone() })
        .await
        .unwrap();

    let state_resp = handler
        .get_mount_state(GetMountStateRequest {
            store_id: a_store,
            node_id: mount_node_id,
        })
        .await
        .unwrap();
    assert!(matches!(state_resp.state, MountState::Unavailable { .. }));

    let children_result = handler
        .get_children(GetChildrenRequest {
            store_id: a_store,
            node_id: mount_node_id,
        })
        .await;
    assert!(children_result.is_err(), "expected getChildren on an unresolvable mount to be an error");
}

/// 5. Cycle: B's root is mounted in A, then mounting A's root into B is
/// rejected.
#[tokio::test]
async fn mounting_back_into_the_mounting_store_is_rejected_as_a_cycle() {
    let (handler, _store_manager) = new_handler();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let (a_store, a_root) = create_store(&handler, &dir_a.path().join("a.pimble"), "A").await;
    let (b_store, b_root) = create_store(&handler, &dir_b.path().join("b.pimble"), "B").await;

    // Mount B's root under A's root.
    handler
        .create_mount(CreateMountRequest {
            store_id: a_store,
            parent_id: a_root,
            source_store_id: b_store,
            source_node_id: b_root,
            title: None,
        })
        .await
        .unwrap();

    // Mounting A's root into B would close the loop (A -> B -> A).
    let result = handler
        .create_mount(CreateMountRequest {
            store_id: b_store,
            parent_id: b_root,
            source_store_id: a_store,
            source_node_id: a_root,
            title: None,
        })
        .await;

    assert!(result.is_err(), "expected mounting A's root into B to be rejected as a cycle");
}

/// 6. Nested: A mounts B's root, B mounts C's root. `getChildren` through
/// A's mount lists B's mount node (canonical store B); `getChildren` on that
/// node, addressed in B, returns C's children with `store_id == C` — a
/// mount resolves one level at a time.
#[tokio::test]
async fn nested_mounts_resolve_one_level_at_a_time() {
    let (handler, _store_manager) = new_handler();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let dir_c = tempfile::tempdir().unwrap();

    let (a_store, a_root) = create_store(&handler, &dir_a.path().join("a.pimble"), "A").await;
    let (b_store, b_root) = create_store(&handler, &dir_b.path().join("b.pimble"), "B").await;
    let (c_store, c_root) = create_store(&handler, &dir_c.path().join("c.pimble"), "C").await;

    let c_doc = create_doc(&handler, c_store, c_root, "C Doc").await;

    let b_mount_resp = handler
        .create_mount(CreateMountRequest {
            store_id: b_store,
            parent_id: b_root,
            source_store_id: c_store,
            source_node_id: c_root,
            title: Some("C Mount".into()),
        })
        .await
        .unwrap();

    let a_mount_resp = handler
        .create_mount(CreateMountRequest {
            store_id: a_store,
            parent_id: a_root,
            source_store_id: b_store,
            source_node_id: b_root,
            title: Some("B Mount".into()),
        })
        .await
        .unwrap();

    let via_a = handler
        .get_children(GetChildrenRequest {
            store_id: a_store,
            node_id: a_mount_resp.node_id,
        })
        .await
        .unwrap();
    assert_eq!(via_a.store_id, b_store);
    assert_eq!(via_a.children.len(), 1);
    assert_eq!(via_a.children[0].id, b_mount_resp.node_id);
    assert!(via_a.children[0].is_mount());

    let via_b = handler
        .get_children(GetChildrenRequest {
            store_id: b_store,
            node_id: b_mount_resp.node_id,
        })
        .await
        .unwrap();
    assert_eq!(via_b.store_id, c_store);
    assert_eq!(via_b.children.len(), 1);
    assert_eq!(via_b.children[0].id, c_doc);
}

/// 7. `createMount` delivers a `NodeCreated` notification to a
/// `subscribeStoreChanges` subscriber of the mounting store.
///
/// `store_sync.rs` and `content_sync.rs` don't exercise subscriptions at
/// all (every other test here calls `RpcHandler`'s trait methods directly,
/// bypassing JSON-RPC dispatch entirely, which has no notion of a
/// subscription sink). There's no existing pattern to follow, so this test
/// goes through the handler's own JSON-RPC dispatch in-process — jsonrpsee's
/// `RpcModule::call`/`subscribe_unbounded` ("a subscription on the RPC
/// module without having to spin up a server", per its own doc comment) —
/// rather than opening a real network socket. Method names are the
/// `#[rpc(namespace = "pimble")]` ones (`{namespace}_{method}`, e.g.
/// `pimble_createMount`; the subscription's is its `name`,
/// `pimble_subscribeStoreChanges`).
#[tokio::test]
async fn create_mount_notifies_a_store_changes_subscriber() {
    let store_manager = Arc::new(RwLock::new(StoreManager::new()));
    let handler = RpcHandler::new(store_manager);
    let module = handler.into_rpc();

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let a: CreateStoreResponse = module
        .call(
            "pimble_createStore",
            (CreateStoreRequest { path: dir_a.path().join("a.pimble"), name: "A".into() },),
        )
        .await
        .unwrap();
    let b: CreateStoreResponse = module
        .call(
            "pimble_createStore",
            (CreateStoreRequest { path: dir_b.path().join("b.pimble"), name: "B".into() },),
        )
        .await
        .unwrap();

    // Subscribe to the mounting store's changes before creating the mount.
    let mut sub = module
        .subscribe_unbounded("pimble_subscribeStoreChanges", (a.store_id,))
        .await
        .unwrap();

    let mount_resp: CreateMountResponse = module
        .call(
            "pimble_createMount",
            (CreateMountRequest {
                store_id: a.store_id,
                parent_id: a.root_node_id,
                source_store_id: b.store_id,
                source_node_id: b.root_node_id,
                title: None,
            },),
        )
        .await
        .unwrap();

    let (notification, _sub_id) = sub
        .next::<StoreChangedNotification>()
        .await
        .expect("expected a storeChanged notification for the new mount node")
        .expect("notification payload should decode as StoreChangedNotification");

    assert_eq!(notification.store_id, a.store_id);
    assert!(
        matches!(
            notification.change_kind,
            StoreChangeKind::NodeCreated { node_id, .. } if node_id == mount_resp.node_id
        ),
        "expected a NodeCreated notification for the mount node, got {:?}",
        notification.change_kind
    );
}

/// 8. `createNode` with a mount node as parent is an error.
#[tokio::test]
async fn create_node_under_a_mount_is_an_error() {
    let (handler, _store_manager) = new_handler();
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let (a_store, a_root) = create_store(&handler, &dir_a.path().join("a.pimble"), "A").await;
    let (b_store, b_root) = create_store(&handler, &dir_b.path().join("b.pimble"), "B").await;

    let mount_resp = handler
        .create_mount(CreateMountRequest {
            store_id: a_store,
            parent_id: a_root,
            source_store_id: b_store,
            source_node_id: b_root,
            title: None,
        })
        .await
        .unwrap();

    let result = handler
        .create_node(CreateNodeRequest {
            store_id: a_store,
            parent_id: Some(mount_resp.node_id),
            node_type: "document".into(),
            title: "Should fail".into(),
        })
        .await;

    let err = result.expect_err("expected createNode under a mount node to fail");
    assert!(
        err.message().contains("source"),
        "expected the error message to point at the mount's source, got {:?}",
        err.message()
    );
}
