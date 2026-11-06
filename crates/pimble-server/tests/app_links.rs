//! `openInApp` (docs/SETTINGS_CONTRACT.md section 12): a `pimble:` link this
//! computer was asked to open goes to one Pimble window listening through
//! `subscribeAppLinks`, the one open longest, and a caller hears whether any
//! window took it.

use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_server::{PimbleServer, ServerConfig};
use tempfile::TempDir;

#[tokio::test]
async fn a_link_goes_to_the_oldest_window_listening() {
    let dir = TempDir::new().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: addr.parse().unwrap(),
        credentials_path: Some(dir.path().join("credentials.json")),
        keystore_path: Some(dir.path().join("keys.json")),
        replicas_dir: Some(dir.path().join("replicas")),
        agents_path: Some(dir.path().join("agents.json")),
        ..Default::default()
    });
    server.start().await.unwrap();
    let url = format!("http://{addr}");
    let link = "pimble:settings/plugins?install=kanban-viewer";

    let caller = PimbleClient::connect(&url).await.unwrap();
    assert!(!caller.open_in_app(link).await.unwrap(), "no window listens yet");

    let first = PimbleClient::connect(&url).await.unwrap();
    let mut first_links = first.subscribe_app_links().await.unwrap();
    let second = PimbleClient::connect(&url).await.unwrap();
    let mut second_links = second.subscribe_app_links().await.unwrap();

    assert!(caller.open_in_app(link).await.unwrap());
    let heard = tokio::time::timeout(Duration::from_secs(5), first_links.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(heard, link);
    assert!(tokio::time::timeout(Duration::from_millis(300), second_links.next()).await.is_err(), "only one window takes it");

    // The first window closes: the next link goes to the second.
    drop(first_links);
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(caller.open_in_app("pimble:settings").await.unwrap());
    let heard = tokio::time::timeout(Duration::from_secs(5), second_links.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(heard, "pimble:settings");

    drop(second_links);
    drop(second);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!caller.open_in_app(link).await.unwrap(), "every window is gone");
}
