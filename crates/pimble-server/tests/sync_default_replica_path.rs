//! `addRemoteStore` with `path: None` (docs/SYNC_CONTRACT.md decision 8:
//! "the user never chooses a location") places the replica at
//! `<dirs::data_local_dir()>/pimble/replicas/<store id>.pimble`.
//!
//! `dirs::data_local_dir()` reads `XDG_DATA_HOME` on Linux, so this is
//! testable hermetically by overriding that env var to a temp directory
//! before starting the server. Env vars are process-global, so this lives in
//! its own test binary (a separate file) rather than next to `sync.rs`'s
//! other `#[tokio::test]`s: cargo runs the functions within one integration
//! test file concurrently on threads of the same process, and this would
//! otherwise race any other test in that process reading
//! `dirs::data_local_dir()` (e.g. the embedding-model cache dir every
//! `PimbleServer::start()` resolves). A whole separate file is a separate
//! process, so no other test's env is affected and this one has the env to
//! itself.

use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, RemoteEndpoint};
use pimble_server::{PimbleServer, ServerConfig};

#[tokio::test]
async fn add_remote_store_with_no_path_uses_the_default_replica_directory() {
    let fake_data_home = tempfile::tempdir().unwrap();
    // Safe: this test binary is its own process (see the module doc), so no
    // other test observes this override.
    unsafe {
        std::env::set_var("XDG_DATA_HOME", fake_data_home.path());
    }

    let mut server_b = PimbleServer::with_config(ServerConfig { addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() });
    server_b.start().await.expect("server B starts");
    let client_b = PimbleClient::connect(format!("http://{}", server_b.addr())).await.unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let (store_id, _root_id) = client_b.create_store(b_dir.path().join("b.pimble"), "B").await.unwrap();

    let mut server_a = PimbleServer::with_config(ServerConfig { addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() });
    server_a.start().await.expect("server A starts");
    let client_a = PimbleClient::connect(format!("http://{}", server_a.addr())).await.unwrap();

    let remote = RemoteEndpoint { url: format!("http://{}", server_b.addr()).parse().unwrap(), auth: AuthMethod::None };
    let store = client_a
        .add_remote_store(remote, store_id, None)
        .await
        .expect("addRemoteStore with path: None succeeds");

    let expected_dir = fake_data_home.path().join("pimble").join("replicas").join(format!("{}.pimble", store_id));
    let actual_path = store.local_path().cloned().expect("a local store reports a local path");
    assert_eq!(
        actual_path, expected_dir,
        "with path: None, the replica should land under <XDG_DATA_HOME>/pimble/replicas/<store id>.pimble"
    );
    assert!(actual_path.exists(), "the replica directory should actually exist on disk");
}
