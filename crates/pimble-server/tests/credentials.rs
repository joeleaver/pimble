//! Credential resolution and persistence for connecting to remote Pimble
//! servers (docs/history/HARDENING_CONTRACT.md decision 4), exercised end to end
//! with two real servers and the actual files `addRemoteStore` and the
//! credentials store write, using a temp `credentials_path` so this test
//! never touches the real config directory.

use std::path::PathBuf;
use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, RemoteEndpoint, SyncState};
use pimble_server::credentials::{origin_of, CredentialStore};
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

async fn start_server(auth_token: Option<&str>, credentials_path: Option<PathBuf>) -> PimbleServer {
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: auth_token.map(String::from),
        credentials_path,
        ..Default::default()
    });
    server.start().await.expect("server starts");
    server
}

#[tokio::test]
async fn add_remote_store_saves_the_credential_and_never_writes_it_to_disk() {
    let mut server_b = start_server(Some("b-secret"), None).await;
    let client_b = PimbleClient::connect_with_auth(
        &format!("http://{}", server_b.addr()),
        &AuthMethod::Bearer { token: "b-secret".into() },
    )
    .await
    .unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client_b.create_store(b_dir.path().join("b.pimble"), "B").await.unwrap();

    let creds_dir = tempfile::tempdir().unwrap();
    let credentials_path = creds_dir.path().join("credentials.json");
    let mut server_a = start_server(None, Some(credentials_path.clone())).await;
    let client_a = PimbleClient::connect(&format!("http://{}", server_a.addr())).await.unwrap();

    let a_replica_dir = tempfile::tempdir().unwrap();
    let replica_path = a_replica_dir.path().join("replica.pimble");

    let remote = RemoteEndpoint {
        url: format!("http://{}", server_b.addr()).parse().unwrap(),
        auth: AuthMethod::Bearer { token: "b-secret".into() },
    };
    let store = client_a
        .add_remote_store(remote.clone(), store_id, Some(replica_path.clone()))
        .await
        .expect("addRemoteStore with the right token succeeds");
    assert!(
        matches!(store.sync_state, SyncState::Synced { .. }),
        "should reach Synced within addRemoteStore's own wait, got {:?}",
        store.sync_state
    );

    // sync.json never stores the real credential (decision 4).
    let sync_json = std::fs::read_to_string(replica_path.join("sync.json")).unwrap();
    let sync_value: serde_json::Value = serde_json::from_str(&sync_json).unwrap();
    assert_eq!(
        sync_value["remote"]["auth"]["method"], "none",
        "sync.json must always record auth: none, got {sync_json}"
    );

    // The credentials file holds the token under B's origin.
    let saved = CredentialStore::new(credentials_path.clone());
    match saved.get(&remote.url).await {
        Some(AuthMethod::Bearer { token }) => assert_eq!(token, "b-secret"),
        other => panic!("expected the bearer token saved under B's origin, got {other:?}"),
    }
    assert_eq!(origin_of(&remote.url), format!("http://127.0.0.1:{}", server_b.addr().port()));

    // getStoreSync never returns the credential either.
    match client_a.get_store_sync(store.id).await.unwrap() {
        (Some(r), _state) => assert!(matches!(r.auth, AuthMethod::None), "getStoreSync must never return a credential"),
        (None, _) => panic!("expected a remote to be recorded"),
    }

    // A fresh server on the same replica directory and the same
    // credentials path reaches Synced with nothing passed by the caller.
    server_a.stop().await.unwrap();
    let mut server_a2 = start_server(None, Some(credentials_path.clone())).await;
    let client_a2 = PimbleClient::connect(&format!("http://{}", server_a2.addr())).await.unwrap();
    let reopened = client_a2.open_store(&replica_path).await.expect("reopening the replica succeeds");

    let synced = wait_until(Duration::from_secs(10), || async {
        let (_remote, state) = client_a2.get_store_sync(reopened.id).await.unwrap();
        matches!(state, SyncState::Synced { .. })
    })
    .await;
    assert!(synced, "a fresh server reopening the replica must resync using the saved credential alone");

    server_a2.stop().await.unwrap();
    server_b.stop().await.unwrap();
}

#[tokio::test]
async fn list_remote_stores_uses_an_explicit_token_then_the_saved_one_and_reports_a_wrong_one() {
    let mut server_b = start_server(Some("b-secret"), None).await;
    let client_b = PimbleClient::connect_with_auth(
        &format!("http://{}", server_b.addr()),
        &AuthMethod::Bearer { token: "b-secret".into() },
    )
    .await
    .unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    client_b.create_store(b_dir.path().join("b.pimble"), "B").await.unwrap();

    let creds_dir = tempfile::tempdir().unwrap();
    let credentials_path = creds_dir.path().join("credentials.json");
    let mut server_a = start_server(None, Some(credentials_path)).await;
    let client_a = PimbleClient::connect(&format!("http://{}", server_a.addr())).await.unwrap();

    let url: url::Url = format!("http://{}", server_b.addr()).parse().unwrap();

    let remote_with_token = RemoteEndpoint { url: url.clone(), auth: AuthMethod::Bearer { token: "b-secret".into() } };
    let stores = client_a
        .list_remote_stores(remote_with_token)
        .await
        .expect("listRemoteStores with the right explicit token succeeds");
    assert_eq!(stores.len(), 1);

    // No auth passed this time: must fall back to what the call above just saved.
    let remote_no_auth = RemoteEndpoint { url: url.clone(), auth: AuthMethod::None };
    let stores_again = client_a
        .list_remote_stores(remote_no_auth)
        .await
        .expect("listRemoteStores falls back to the saved credential");
    assert_eq!(stores_again.len(), 1);

    let remote_wrong = RemoteEndpoint { url: url.clone(), auth: AuthMethod::Bearer { token: "wrong".into() } };
    let err = client_a.list_remote_stores(remote_wrong).await.expect_err("a wrong token must be refused");
    assert!(
        err.to_string().contains("refused the credentials"),
        "expected 'refused the credentials' in: {}",
        err
    );

    server_a.stop().await.unwrap();
    server_b.stop().await.unwrap();
}
