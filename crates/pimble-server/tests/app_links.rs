//! `openInApp` (docs/SETTINGS_CONTRACT.md section 12): a `pimble:` link this
//! computer was asked to open goes to one Pimble window listening through
//! `subscribeAppLinks`, the one open longest, and a caller hears whether any
//! window took it.

use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_rpc::HandedLink;
use pimble_server::{PimbleServer, ServerConfig};
use tempfile::TempDir;

#[tokio::test]
async fn a_link_goes_to_the_oldest_window_listening() {
    let dir = TempDir::new().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: addr.parse().unwrap(),
        ..ServerConfig::in_dir(dir.path())
    });
    server.start().await.unwrap();
    let url = format!("http://{addr}");
    let link = "pimble:settings/plugins?install=kanban-viewer";

    let caller = PimbleClient::connect(&url).await.unwrap();
    assert!(!caller.open_in_app(link, None).await.unwrap(), "no window listens yet");

    let first = PimbleClient::connect(&url).await.unwrap();
    let mut first_links = first.subscribe_app_links().await.unwrap();
    let second = PimbleClient::connect(&url).await.unwrap();
    let mut second_links = second.subscribe_app_links().await.unwrap();

    assert!(caller.open_in_app(link, None).await.unwrap());
    let heard = tokio::time::timeout(Duration::from_secs(5), first_links.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(heard.url, link);
    assert_eq!(heard.activation_token, None);
    assert!(tokio::time::timeout(Duration::from_millis(300), second_links.next()).await.is_err(), "only one window takes it");

    // The first window closes: the next link goes to the second.
    drop(first_links);
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(caller.open_in_app("pimble:settings", None).await.unwrap());
    let heard = tokio::time::timeout(Duration::from_secs(5), second_links.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(heard.url, "pimble:settings");

    drop(second_links);
    drop(second);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!caller.open_in_app(link, None).await.unwrap(), "every window is gone");
}

/// The activation token a click gave the program asked to open a link
/// (Wayland's `XDG_ACTIVATION_TOKEN`) reaches the window with the link, so the
/// window can come to the front with it (rinch #2880).
#[tokio::test]
async fn a_link_carries_its_activation_token_to_the_window() {
    let dir = TempDir::new().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: addr.parse().unwrap(),
        ..ServerConfig::in_dir(dir.path())
    });
    server.start().await.unwrap();
    let url = format!("http://{addr}");

    let window = PimbleClient::connect(&url).await.unwrap();
    let mut links = window.subscribe_app_links().await.unwrap();
    let caller = PimbleClient::connect(&url).await.unwrap();
    assert!(caller.open_in_app("pimble:settings", Some("token-from-the-click".into())).await.unwrap());
    let heard = tokio::time::timeout(Duration::from_secs(5), links.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(heard, HandedLink { url: "pimble:settings".into(), activation_token: Some("token-from-the-click".into()) });
}

/// A link with no token is sent as the bare URL, as before the token, so a
/// window from before it still reads every link it could; one with a token
/// is an object, which the window reads either way.
#[test]
fn a_link_without_a_token_is_the_bare_url_on_the_wire() {
    let bare = HandedLink { url: "pimble:settings".into(), activation_token: None };
    assert_eq!(serde_json::to_value(&bare).unwrap(), serde_json::json!("pimble:settings"));
    assert_eq!(serde_json::from_value::<HandedLink>(serde_json::json!("pimble:settings")).unwrap(), bare);

    let with = HandedLink { url: "pimble:settings".into(), activation_token: Some("t".into()) };
    let wire = serde_json::to_value(&with).unwrap();
    assert_eq!(wire, serde_json::json!({ "url": "pimble:settings", "activation_token": "t" }));
    assert_eq!(serde_json::from_value::<HandedLink>(wire).unwrap(), with);
}
