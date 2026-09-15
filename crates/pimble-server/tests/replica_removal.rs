//! `removeReplica` (docs/history/HARDENING_CONTRACT.md decision 6): refused for an
//! ordinary store, refused for an out-of-sync replica unless `force`, and on
//! success stops the link, closes the store, and deletes its directory.
//!
//! `removeReplica` only treats a store as a replica when its directory sits
//! under `<dirs::data_local_dir()>/pimble/replicas/` (`Store::is_replica`),
//! so this test needs `path: None` on `addRemoteStore` to land there, which
//! means overriding `XDG_DATA_HOME` the same way and for the same reason as
//! `sync_default_replica_path.rs`: a whole separate test binary (own file),
//! never sharing a process with a test that reads `dirs::data_local_dir()`
//! expecting the real one.

use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, RemoteEndpoint, SyncState};
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

async fn start_server() -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig { addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() });
    server.start().await.expect("server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

#[tokio::test]
async fn remove_replica_refuses_ordinary_stores_and_removes_replicas() {
    let fake_data_home = tempfile::tempdir().unwrap();
    // Safe: this test binary is its own process (see the module doc).
    unsafe {
        std::env::set_var("XDG_DATA_HOME", fake_data_home.path());
    }

    let (mut server_b, client_b) = start_server().await;
    let b_dir = tempfile::tempdir().unwrap();

    let (mut server_a, client_a) = start_server().await;

    // ── An ordinary (non-replica) store is refused ──────────────────
    let ordinary_dir = tempfile::tempdir().unwrap();
    let (ordinary_id, _root) = client_a.create_store(ordinary_dir.path().join("ordinary.pimble"), "Ordinary").await.unwrap();
    let err = client_a
        .remove_replica(ordinary_id, false)
        .await
        .expect_err("removeReplica must refuse a store that isn't a replica");
    assert!(err.to_string().contains("not a replica"), "expected 'not a replica' in: {}", err);

    // ── A synced replica is removed cleanly ──────────────────────────
    let (store_id, _root) = client_b.create_store(b_dir.path().join("b.pimble"), "B").await.unwrap();
    let remote = RemoteEndpoint { url: format!("http://{}", server_b.addr()).parse().unwrap(), auth: AuthMethod::None };
    let replica = client_a
        .add_remote_store(remote, store_id, None)
        .await
        .expect("addRemoteStore with path: None succeeds");
    assert!(replica.is_replica, "a replica addRemoteStore creates must report is_replica: true");
    assert!(matches!(replica.sync_state, SyncState::Synced { .. }), "should already be Synced: {:?}", replica.sync_state);
    let replica_path = replica.local_path().cloned().expect("a local store reports a local path");
    assert!(replica_path.exists());

    client_a.remove_replica(store_id, false).await.expect("a fully synced replica is removed without force");
    assert!(!replica_path.exists(), "the replica directory must be deleted");
    let remaining = client_a.list_stores().await.unwrap();
    assert!(!remaining.iter().any(|s| s.id == store_id), "the removed replica must not be in listStores any more");

    // ── An out-of-sync replica is refused without force, removed with it ──
    let (store_id2, _root2) = client_b.create_store(b_dir.path().join("b2.pimble"), "B2").await.unwrap();
    let remote2 = RemoteEndpoint { url: format!("http://{}", server_b.addr()).parse().unwrap(), auth: AuthMethod::None };
    let replica2 = client_a.add_remote_store(remote2, store_id2, None).await.expect("second addRemoteStore succeeds");
    let replica2_path = replica2.local_path().cloned().unwrap();

    // Take the remote down and wait for A's link to notice.
    server_b.stop().await.unwrap();
    let noticed = wait_until(Duration::from_secs(10), || async {
        let (_remote, state) = client_a.get_store_sync(store_id2).await.unwrap();
        !matches!(state, SyncState::Synced { .. })
    })
    .await;
    assert!(noticed, "the sync link should notice the remote went away");

    let err = client_a
        .remove_replica(store_id2, false)
        .await
        .expect_err("an out-of-sync replica must be refused without force");
    assert!(err.to_string().contains("force"), "expected the refusal to mention force, got: {}", err);
    assert!(replica2_path.exists(), "a refused removal must not touch the directory");

    client_a.remove_replica(store_id2, true).await.expect("force removes an out-of-sync replica");
    assert!(!replica2_path.exists(), "force must still delete the directory");

    server_a.stop().await.unwrap();
}
