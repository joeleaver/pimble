//! End-to-end tests for replica sync (docs/SYNC_CONTRACT.md). Unlike
//! `mounts.rs`/`store_sync.rs`/`content_sync.rs`, which call `RpcHandler`
//! directly, these run two real `PimbleServer`s on `127.0.0.1:0` in one
//! process, driven entirely through `PimbleClient` over real WebSocket
//! connections — the sync link itself is a `PimbleClient` under the hood, so
//! this is the only way to exercise it honestly.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, RemoteEndpoint, StoreId, SyncState};
use pimble_crdt::ContentDoc;
use pimble_rpc::{EditOperation, StoreChangeKind};
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

/// Start a `PimbleServer` bound to an OS-assigned loopback port and return
/// it plus a `PimbleClient` already connected to it.
async fn start_server() -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig { addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() });
    server.start().await.expect("server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

/// A `RemoteEndpoint` pointing at `client`'s server, unauthenticated.
fn remote_endpoint(addr: std::net::SocketAddr) -> RemoteEndpoint {
    RemoteEndpoint { url: format!("http://{}", addr).parse().unwrap(), auth: AuthMethod::None }
}

/// The plain-text projection of a node's current content, or an empty
/// string if the node can't be fetched (e.g. it hasn't replicated yet).
async fn node_text(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    match client.get_node(store_id, node_id).await {
        Ok(node) => ContentDoc::text_of(&node.content),
        Err(_) => String::new(),
    }
}

/// Send `text` as a full-document `applyEdit` delta from empty (a fresh
/// node's content is empty, so `ContentDoc::from_plain_text(text).save()`
/// — the whole document encoded as an update from an empty state vector —
/// is exactly the right "incremental" payload).
async fn seed_content(client: &PimbleClient, store_id: StoreId, node_id: NodeId, client_id: &str, text: &str) {
    let doc = ContentDoc::from_plain_text(text).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.save());
    client
        .apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes })
        .await
        .expect("seed content applies");
}

/// Diverge a node's content from whatever `client` currently sees to
/// `new_text`, sent as an `applyEdit` diff against the current state vector
/// (same "diff two independently built `from_plain_text` documents"
/// technique `content_sync.rs` uses: the result reliably contains both the
/// old and new text once merged, though not as a single clean edit).
async fn diverge_content(client: &PimbleClient, store_id: StoreId, node_id: NodeId, client_id: &str, new_text: &str) {
    let current = client.get_node(store_id, node_id).await.unwrap();
    let base = ContentDoc::load(&current.content).unwrap();
    let richer = ContentDoc::from_plain_text(new_text).unwrap();
    let diff = richer.diff_since(&base.state_vector()).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(&diff);
    client
        .apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes })
        .await
        .expect("diverging edit applies");
}

/// Set up B with a store containing one document node whose text is
/// "hello", and A with a replica of it (via `addRemoteStore`) at `a_path`.
/// Returns the two servers/clients, the shared store id, root id, the
/// document's id, B's on-disk store path, and B's `TempDir` guard (keep it
/// bound for as long as B's directory must survive).
async fn setup_synced_pair(a_path: &Path) -> (
    PimbleServer, PimbleClient,
    PimbleServer, PimbleClient,
    StoreId, NodeId, NodeId,
    PathBuf, tempfile::TempDir,
) {
    let (server_b, client_b) = start_server().await;
    let b_dir = tempfile::tempdir().unwrap();
    let b_path = b_dir.path().join("b.pimble");
    let (store_id, root_id) = client_b.create_store(&b_path, "B Store").await.unwrap();
    let doc_id = client_b.create_node(store_id, Some(root_id), "document", "Doc").await.unwrap();
    seed_content(&client_b, store_id, doc_id, "seed", "hello").await;

    let (server_a, client_a) = start_server().await;
    let remote = remote_endpoint(server_b.addr());
    let store = client_a
        .add_remote_store(remote, store_id, Some(a_path.to_path_buf()))
        .await
        .expect("addRemoteStore succeeds");

    (server_a, client_a, server_b, client_b, store.id, root_id, doc_id, b_path, b_dir)
}

// ── 1. Add remote store ──────────────────────────────────────────────

#[tokio::test]
async fn add_remote_store_replicates_an_existing_store() {
    let a_dir = tempfile::tempdir().unwrap();
    let (_server_a, client_a, _server_b, _client_b, store_id, root_id, doc_id, _b_path, _b_dir) =
        setup_synced_pair(&a_dir.path().join("a.pimble")).await;

    let (_, children) = client_a.get_children(store_id, root_id).await.unwrap();
    assert!(children.iter().any(|c| c.id == doc_id), "A's replica should have B's document node");
    assert_eq!(node_text(&client_a, store_id, doc_id).await, "hello");

    let (_, state) = client_a.get_store_sync(store_id).await.unwrap();
    assert!(matches!(state, SyncState::Synced { .. }), "expected Synced, got {:?}", state);
}

// ── 2. Remote to local, live ─────────────────────────────────────────

#[tokio::test]
async fn remote_edits_and_creations_reach_local_live() {
    let a_dir = tempfile::tempdir().unwrap();
    let (_server_a, client_a, _server_b, client_b, store_id, root_id, doc_id, _b_path, _b_dir) =
        setup_synced_pair(&a_dir.path().join("a.pimble")).await;

    diverge_content(&client_b, store_id, doc_id, "client-b", "hello\nworld").await;
    assert!(
        wait_until(Duration::from_secs(2), || async { node_text(&client_a, store_id, doc_id).await.contains("world") }).await,
        "B's content edit should reach A within 2s"
    );

    let new_node_id = client_b.create_node(store_id, Some(root_id), "document", "From B").await.unwrap();
    assert!(
        wait_until(Duration::from_secs(2), || async {
            client_a.get_children(store_id, root_id).await.map(|(_, c)| c.iter().any(|n| n.id == new_node_id)).unwrap_or(false)
        }).await,
        "B's new node should appear in A's getChildren within 2s"
    );
}

// ── 3. Local to remote, live ─────────────────────────────────────────

#[tokio::test]
async fn local_edits_and_creations_reach_remote_live() {
    let a_dir = tempfile::tempdir().unwrap();
    let (_server_a, client_a, _server_b, client_b, store_id, root_id, doc_id, _b_path, _b_dir) =
        setup_synced_pair(&a_dir.path().join("a.pimble")).await;

    diverge_content(&client_a, store_id, doc_id, "client-a", "hello\nmoon").await;
    assert!(
        wait_until(Duration::from_secs(2), || async { node_text(&client_b, store_id, doc_id).await.contains("moon") }).await,
        "A's content edit should reach B within 2s"
    );

    let new_node_id = client_a.create_node(store_id, Some(root_id), "document", "From A").await.unwrap();
    assert!(
        wait_until(Duration::from_secs(2), || async {
            client_b.get_children(store_id, root_id).await.map(|(_, c)| c.iter().any(|n| n.id == new_node_id)).unwrap_or(false)
        }).await,
        "A's new node should appear in B's getChildren within 2s"
    );
}

// ── 4. Offline and reconverge ────────────────────────────────────────

#[tokio::test]
async fn offline_edits_on_both_sides_reconverge_after_relinking() {
    let a_dir = tempfile::tempdir().unwrap();
    let (_server_a, client_a, server_b, client_b, store_id, root_id, doc_id, _b_path, _b_dir) =
        setup_synced_pair(&a_dir.path().join("a.pimble")).await;

    let (_, state) = client_a.set_store_sync(store_id, None).await.unwrap();
    assert!(matches!(state, SyncState::Offline));

    diverge_content(&client_a, store_id, doc_id, "client-a", "hello\nA-edit").await;
    diverge_content(&client_b, store_id, doc_id, "client-b", "hello\nB-edit").await;
    let a_node = client_a.create_node(store_id, Some(root_id), "document", "A-only").await.unwrap();
    let b_node = client_b.create_node(store_id, Some(root_id), "document", "B-only").await.unwrap();

    let remote = remote_endpoint(server_b.addr());
    client_a.set_store_sync(store_id, Some(remote)).await.unwrap();

    let converged = wait_until(Duration::from_secs(5), || async {
        let a_text = node_text(&client_a, store_id, doc_id).await;
        let b_text = node_text(&client_b, store_id, doc_id).await;
        let text_ok = a_text.contains("A-edit") && a_text.contains("B-edit") && a_text == b_text;
        let a_children = client_a.get_children(store_id, root_id).await.map(|(_, c)| c).unwrap_or_default();
        let b_children = client_b.get_children(store_id, root_id).await.map(|(_, c)| c).unwrap_or_default();
        let children_ok = a_children.iter().any(|n| n.id == a_node)
            && a_children.iter().any(|n| n.id == b_node)
            && b_children.iter().any(|n| n.id == a_node)
            && b_children.iter().any(|n| n.id == b_node);
        text_ok && children_ok
    }).await;
    assert!(converged, "both sides should converge on both edits and both new nodes within 5s");
}

// ── 5. Remote down and back ──────────────────────────────────────────

#[tokio::test]
async fn remote_restarted_on_a_new_port_reconverges() {
    let a_dir = tempfile::tempdir().unwrap();
    let (_server_a, client_a, mut server_b, _client_b, store_id, _root_id, doc_id, b_path, _b_dir) =
        setup_synced_pair(&a_dir.path().join("a.pimble")).await;

    // Stop B outright (simulating the remote going down).
    server_b.stop().await.expect("server B stops");

    // Edit on A while the remote is unreachable; A keeps working offline.
    diverge_content(&client_a, store_id, doc_id, "client-a", "hello\nwhile-offline").await;

    // Bring B back as an entirely new server process/port, serving the same
    // on-disk store directory (so the same store id and content resume).
    let (server_b2, client_b2) = start_server().await;
    client_b2.open_store(&b_path).await.expect("reopening B's store directory succeeds");

    // Point A's link at B's new address (any port).
    let new_remote = remote_endpoint(server_b2.addr());
    client_a.set_store_sync(store_id, Some(new_remote)).await.unwrap();

    let converged = wait_until(Duration::from_secs(5), || async {
        let a_text = node_text(&client_a, store_id, doc_id).await;
        let b_text = node_text(&client_b2, store_id, doc_id).await;
        a_text.contains("while-offline") && a_text == b_text
    }).await;
    assert!(converged, "A's offline edit should reach the restarted B within 5s");
}

// ── 6. No echo storm ─────────────────────────────────────────────────

#[tokio::test]
async fn one_local_edit_reaches_remote_exactly_once() {
    let a_dir = tempfile::tempdir().unwrap();
    let (_server_a, client_a, _server_b, client_b, store_id, _root_id, doc_id, _b_path, _b_dir) =
        setup_synced_pair(&a_dir.path().join("a.pimble")).await;

    // Let the initial full reconcile completely settle before measuring.
    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(client_a.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. })))
        }).await,
        "link should reach Synced before the echo check"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut sub = client_b.subscribe_store_changes(store_id).await.unwrap();

    diverge_content(&client_a, store_id, doc_id, "client-a", "hello\nonce").await;

    let mut matching = 0u32;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, sub.next()).await {
            Ok(Some(Ok(notif))) => {
                if matches!(notif.change_kind, StoreChangeKind::ContentUpdated { node_id } if node_id == doc_id) {
                    matching += 1;
                }
            }
            Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
        }
    }

    assert_eq!(matching, 1, "expected exactly one ContentUpdated notification for B's doc, got {}", matching);
}

// ── 7. Index ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_node_created_and_edited_on_remote_is_searchable_locally_after_sync() {
    let a_dir = tempfile::tempdir().unwrap();
    let (_server_a, client_a, _server_b, client_b, store_id, root_id, _doc_id, _b_path, _b_dir) =
        setup_synced_pair(&a_dir.path().join("a.pimble")).await;

    let node_id = client_b.create_node(store_id, Some(root_id), "document", "Quokka Doc").await.unwrap();
    seed_content(&client_b, store_id, node_id, "client-b", "quokka").await;

    let found = wait_until(Duration::from_secs(5), || async {
        client_a
            .search("quokka", vec![store_id], false, 10)
            .await
            .map(|results| results.iter().any(|r| r.node_id == node_id))
            .unwrap_or(false)
    }).await;
    assert!(found, "a node created and edited on B should be searchable on A once synced");
}

// ── 8. Restart ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_replicas_link_survives_a_restart() {
    let a_path_dir = tempfile::tempdir().unwrap();
    let a_path = a_path_dir.path().join("a.pimble");
    let (mut server_a, client_a, server_b, _client_b, store_id, _root_id, _doc_id, _b_path, _b_dir) =
        setup_synced_pair(&a_path).await;

    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(client_a.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. })))
        }).await,
        "initial link should reach Synced"
    );

    server_a.stop().await.expect("server A stops");
    drop(client_a);

    let (mut server_a2, client_a2) = start_server().await;
    client_a2.open_store(&a_path).await.expect("reopening A's replica directory succeeds");

    let reconnected = wait_until(Duration::from_secs(5), || async {
        matches!(client_a2.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. })))
    }).await;
    assert!(reconnected, "the link should come back from sync.json and reach Synced");

    server_a2.stop().await.ok();
    let _ = server_b;
}


// ── 9. Guards ────────────────────────────────────────────────────────

/// A store this server already holds cannot be added again as a replica,
/// even when the "remote" is this very server; and a store cannot be linked
/// to the server it lives on.
#[tokio::test]
async fn adding_or_linking_a_store_to_its_own_server_is_refused() {
    let (server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root_id) = client.create_store(&dir.path().join("x.pimble"), "X").await.unwrap();

    let me = remote_endpoint(server.addr());

    let err = client
        .add_remote_store(me.clone(), store_id, Some(dir.path().join("replica.pimble")))
        .await
        .expect_err("adding an already-open store must be refused");
    assert!(err.to_string().contains("already open locally"), "got: {}", err);
    assert!(!dir.path().join("replica.pimble").exists(), "no replica directory may be created");

    let err = client
        .set_store_sync(store_id, Some(me))
        .await
        .expect_err("linking a store to its own server must be refused");
    assert!(err.to_string().contains("is this server"), "got: {}", err);

    let (remote, state) = client.get_store_sync(store_id).await.unwrap();
    assert!(remote.is_none(), "no link may have been written");
    assert!(matches!(state, SyncState::Offline));
}
