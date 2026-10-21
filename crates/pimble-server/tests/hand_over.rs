//! A newer local client takes the port over from an older server
//! (docs/MCP_CONTRACT.md "Which server it talks to"): `serverInfo` says what a
//! server is, `handOver` makes the app's own server write everything and stop,
//! and `pimble_server::local::ensure_connected` starts a current one in its
//! place. A server that is not this computer's own never hands over.

use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_core::AuthMethod;
use pimble_server::{local, PimbleServer, ServerConfig};
use tempfile::TempDir;

fn config(dir: &TempDir, addr: &str) -> ServerConfig {
    ServerConfig {
        addr: addr.parse().unwrap(),
        credentials_path: Some(dir.path().join("credentials.json")),
        keystore_path: Some(dir.path().join("keys.json")),
        replicas_dir: Some(dir.path().join("replicas")),
        agents_path: Some(dir.path().join("agents.json")),
        ..Default::default()
    }
}

/// The only test here that sets the environment (`PIMBLE_APP_ADDR` and the
/// config and data directories `ensure_connected`'s new server reads), so
/// nothing else in this binary sees it change.
#[tokio::test]
async fn a_newer_client_takes_the_port_over_from_an_older_server() {
    let dir = TempDir::new().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    // SAFETY: this binary's only test that touches the environment, set
    // before anything reads it.
    unsafe {
        std::env::set_var("PIMBLE_APP_ADDR", &addr);
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
    }

    // An "older" server holding a note typed into just now.
    let mut old = PimbleServer::with_config(ServerConfig { reported_version: Some("0.0.1".into()), ..config(&dir, &addr) });
    old.start().await.unwrap();
    let service = PimbleClient::connect(format!("http://{addr}")).await.unwrap();
    let info = service.server_info().await.unwrap();
    assert_eq!((info.version.as_str(), info.hands_over), ("0.0.1", true));
    let store_path = dir.path().join("work.pimble");
    let (store, root) = service.create_store(&store_path, "Work").await.unwrap();
    let note = service.create_node(store, Some(root), "document", "Plans").await.unwrap();

    let (client, owned) = local::ensure_connected().await.expect("the newer client gets a server");
    assert!(owned.is_some(), "it started the server it now uses");
    let info = client.server_info().await.unwrap();
    assert_eq!(info.version, local::VERSION, "the server on the port is this build's");
    assert!(
        tokio::time::timeout(Duration::from_secs(5), old.wait()).await.is_ok(),
        "the older server stopped"
    );
    assert!(store_path.join("nodes").join(format!("{note}.yrs")).exists(), "and wrote what it held before it went");

    let mut owned = owned;
    local::shut_down(&mut owned).await;
}

#[tokio::test]
async fn only_this_computers_own_server_hands_over() {
    let dir = TempDir::new().unwrap();
    let mut own = PimbleServer::with_config(config(&dir, "127.0.0.1:0"));
    own.start().await.unwrap();
    let client = PimbleClient::connect(format!("http://{}", own.addr())).await.unwrap();
    let info = client.server_info().await.unwrap();
    assert_eq!((info.version.as_str(), info.hands_over), (local::VERSION, true));
    own.stop().await.unwrap();

    // A server with a token (a hosted one, `pimble-cli server`) says it does
    // not, and refuses even its own service principal.
    let mut hosted = PimbleServer::with_config(ServerConfig { auth_token: Some("t0ken-for-tests".into()), ..config(&dir, "127.0.0.1:0") });
    hosted.start().await.unwrap();
    let url = format!("http://{}", hosted.addr());
    let client = PimbleClient::connect_with_auth(url, &AuthMethod::Bearer { token: "t0ken-for-tests".into() }).await.unwrap();
    assert!(!client.server_info().await.unwrap().hands_over);
    let refused = client.hand_over().await.expect_err("a hosted server never hands over");
    assert!(refused.to_string().contains("does not hand over"), "{refused}");
    assert!(client.list_stores().await.is_ok(), "and it is still serving");
    hosted.stop().await.unwrap();
}
