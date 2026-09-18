//! End-to-end tests for tree repair (docs/history/HARDENING_CONTRACT.md decision 9):
//! two real `PimbleServer`s, linked as replicas, diverge while unlinked and
//! reconverge on a well-formed tree once relinked. Like `sync.rs`, driven
//! entirely through `PimbleClient` over real WebSocket connections, with
//! `PimbleServer::store_manager()` used only to read `Tree::validate_tree`
//! directly (there is no RPC for it — it is a repair-time diagnostic, not part
//! of the client-facing protocol).

use std::time::Duration;

use pimble_core::{AuthMethod, NodeId, RemoteEndpoint, StoreId, SyncState};
use pimble_server::{PimbleServer, ServerConfig};

/// Poll `cond` every 50ms until it returns `true` or `timeout` elapses.
/// Returns whether it converged in time.
async fn wait_until<F, Fut>(timeout: Duration, mut cond: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Start a `PimbleServer` bound to an OS-assigned loopback port and return it
/// plus a `PimbleClient` already connected to it.
async fn start_server() -> (PimbleServer, pimble_client::PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig { addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() });
    server.start().await.expect("server starts");
    let client = pimble_client::PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

/// A `RemoteEndpoint` pointing at `client`'s server, unauthenticated.
fn remote_endpoint(addr: std::net::SocketAddr) -> RemoteEndpoint {
    RemoteEndpoint { url: format!("http://{}", addr).parse().unwrap(), auth: AuthMethod::None }
}

/// `Tree::validate_tree()` issues for `store_id` on `server`, read directly
/// off its `StoreManager` (there is no RPC for this — see the module doc
/// comment).
async fn validate_tree(server: &PimbleServer, store_id: StoreId) -> Vec<pimble_crdt::TreeIssue> {
    let manager = server.store_manager();
    let manager = manager.read().await;
    manager.tree(store_id).expect("store is open").validate_tree()
}

/// Set up B with a store containing two folders under its root, and A with a
/// replica of it. Returns the two servers/clients, the shared store, root,
/// and the two folder ids.
async fn setup_synced_pair_with_two_folders(a_path: &std::path::Path) -> (
    PimbleServer, pimble_client::PimbleClient,
    PimbleServer, pimble_client::PimbleClient,
    StoreId, NodeId, NodeId, NodeId,
    tempfile::TempDir,
) {
    let (server_b, client_b) = start_server().await;
    let b_dir = tempfile::tempdir().unwrap();
    let b_path = b_dir.path().join("b.pimble");
    let (store_id, root_id) = client_b.create_store(&b_path, "B Store").await.unwrap();
    let folder_a = client_b.create_node(store_id, Some(root_id), "folder", "A").await.unwrap();
    let folder_b = client_b.create_node(store_id, Some(root_id), "folder", "B").await.unwrap();

    let (server_a, client_a) = start_server().await;
    let remote = remote_endpoint(server_b.addr());
    let store = client_a
        .add_remote_store(remote, store_id, Some(a_path.to_path_buf()))
        .await
        .expect("addRemoteStore succeeds");

    (server_a, client_a, server_b, client_b, store.id, root_id, folder_a, folder_b, b_dir)
}

/// A node moved to two different parents on two unlinked replicas converges,
/// once relinked, on a single well-formed placement under one of them.
#[tokio::test]
async fn concurrent_moves_to_different_parents_repair_to_one_winner() {
    let a_dir = tempfile::tempdir().unwrap();
    let (server_a, client_a, server_b, client_b, store_id, root_id, folder_a, folder_b, _b_dir) =
        setup_synced_pair_with_two_folders(&a_dir.path().join("a.pimble")).await;

    let child = client_a.create_node(store_id, Some(folder_a), "document", "Child").await.unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || async {
            client_b.get_node(store_id, child).await.is_ok()
        }).await,
        "the new child should replicate to B before it's unlinked"
    );

    client_a.set_store_sync(store_id, None).await.unwrap();

    // Diverge: A moves the child to folder B; B (unaware) moves it to the root.
    client_a.move_node(store_id, child, folder_b, None).await.unwrap();
    client_b.move_node(store_id, child, root_id, None).await.unwrap();

    let remote = remote_endpoint(server_b.addr());
    client_a.set_store_sync(store_id, Some(remote)).await.unwrap();

    // Well-formed on both sides is necessary but not sufficient: each side
    // repairs from whatever it currently knows, so the condition must also
    // wait for the two replicas to actually agree — otherwise "both trees
    // are locally well formed" can be true well before either has seen the
    // other's diff.
    let converged = wait_until(Duration::from_secs(5), || async {
        if !validate_tree(&server_a, store_id).await.is_empty() || !validate_tree(&server_b, store_id).await.is_empty() {
            return false;
        }
        let a_parent = client_a.get_node(store_id, child).await.map(|n| n.parent_id).ok();
        let b_parent = client_b.get_node(store_id, child).await.map(|n| n.parent_id).ok();
        a_parent.is_some() && a_parent == b_parent
    }).await;
    assert!(
        converged,
        "expected both trees well formed and in agreement within 5s; A: {:?}, B: {:?}",
        validate_tree(&server_a, store_id).await, validate_tree(&server_b, store_id).await
    );

    let a_node = client_a.get_node(store_id, child).await.unwrap();
    let b_node = client_b.get_node(store_id, child).await.unwrap();
    assert_eq!(a_node.parent_id, b_node.parent_id, "both replicas must agree on the child's parent");
    let winner = a_node.parent_id.unwrap();
    assert!(winner == folder_b || winner == root_id, "unexpected winner {}", winner);

    let (_, winner_children) = client_a.get_children(store_id, winner).await.unwrap();
    assert_eq!(
        winner_children.iter().filter(|n| n.id == child).count(), 1,
        "child should appear exactly once under its winning parent"
    );
}

/// Two folders concurrently moved under each other (A under B on one
/// replica, B under A on the other) form a cycle once merged; repair breaks
/// it and both replicas converge on the same shape.
#[tokio::test]
async fn concurrent_a_under_b_and_b_under_a_repairs_the_cycle() {
    let a_dir = tempfile::tempdir().unwrap();
    let (server_a, client_a, server_b, client_b, store_id, root_id, folder_a, folder_b, _b_dir) =
        setup_synced_pair_with_two_folders(&a_dir.path().join("a.pimble")).await;

    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(client_a.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. })))
        }).await,
        "initial link should reach Synced"
    );

    client_a.set_store_sync(store_id, None).await.unwrap();

    // Diverge: A moves B under A; B (unaware) moves A under B — a 2-cycle once merged.
    client_a.move_node(store_id, folder_b, folder_a, None).await.unwrap();
    client_b.move_node(store_id, folder_a, folder_b, None).await.unwrap();

    let remote = remote_endpoint(server_b.addr());
    client_a.set_store_sync(store_id, Some(remote)).await.unwrap();

    // See the previous test's comment: well formed on both sides isn't
    // enough, the two replicas must also agree.
    let converged = wait_until(Duration::from_secs(5), || async {
        if !validate_tree(&server_a, store_id).await.is_empty() || !validate_tree(&server_b, store_id).await.is_empty() {
            return false;
        }
        let a_parent = client_a.get_node(store_id, folder_a).await.map(|n| n.parent_id).ok();
        let b_parent = client_b.get_node(store_id, folder_a).await.map(|n| n.parent_id).ok();
        a_parent.is_some() && a_parent == b_parent
    }).await;
    assert!(
        converged,
        "expected both trees well formed and in agreement within 5s; A: {:?}, B: {:?}",
        validate_tree(&server_a, store_id).await, validate_tree(&server_b, store_id).await
    );

    // Exactly one of the two now sits directly under the root, on both replicas.
    let a_folder_a = client_a.get_node(store_id, folder_a).await.unwrap();
    let a_folder_b = client_a.get_node(store_id, folder_b).await.unwrap();
    let b_folder_a = client_b.get_node(store_id, folder_a).await.unwrap();
    let b_folder_b = client_b.get_node(store_id, folder_b).await.unwrap();
    assert_eq!(a_folder_a.parent_id, b_folder_a.parent_id);
    assert_eq!(a_folder_b.parent_id, b_folder_b.parent_id);
    assert!(
        (a_folder_a.parent_id == Some(root_id)) ^ (a_folder_b.parent_id == Some(root_id)),
        "expected exactly one of the two under the root"
    );
}

/// Deleting a node tombstones its whole subtree (docs/NODE_DOCUMENT_CONTRACT.md
/// section 2): every member is gone from `getNode` and from its parent's
/// list, and every document stays on disk, a deletion being part of it.
#[tokio::test]
async fn deleting_a_folder_tombstones_its_subtree_and_keeps_the_documents() {
    let (server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, root_id) = client.create_store(&dir.path().join("x.pimble"), "X").await.unwrap();

    let folder_id = client.create_node(store_id, Some(root_id), "folder", "Folder").await.unwrap();
    let doc_id = client.create_node(store_id, Some(folder_id), "document", "Doc").await.unwrap();

    let store_path = {
        let manager = server.store_manager();
        let manager = manager.read().await;
        manager.get_store_info(store_id).unwrap().local_path().unwrap().clone()
    };
    let content_path = store_path.join("nodes").join(format!("{}.yrs", doc_id));

    client.delete_node(store_id, folder_id).await.unwrap();

    assert!(client.get_node(store_id, folder_id).await.is_err(), "folder should be gone");
    assert!(client.get_node(store_id, doc_id).await.is_err(), "descendant should be gone too");
    assert!(content_path.exists(), "the descendant's document stays on disk as a tombstone");

    let (_, root_children) = client.get_children(store_id, root_id).await.unwrap();
    assert!(root_children.iter().all(|n| n.id != folder_id));
}
