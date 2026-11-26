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

    // The first window closes: once the server has heard it close, the next
    // link goes to the second. Until then a link may still be queued for the
    // closing one and go with it (a caller that must know asks with
    // `confirm`), so this asks until the second window hears one.
    drop(first_links);
    drop(first);
    let deadline = tokio::time::Instant::now() + WINDOW_GONE_DEADLINE;
    let heard = loop {
        assert!(caller.open_in_app("pimble:settings", None).await.unwrap(), "the second window still listens");
        if let Ok(heard) = tokio::time::timeout(Duration::from_millis(250), second_links.next()).await {
            break heard.unwrap().unwrap();
        }
        assert!(tokio::time::Instant::now() < deadline, "the next link reaches the second window once the first has closed");
    };
    assert_eq!(heard.url, "pimble:settings");

    drop(second_links);
    drop(second);
    until_no_window(&caller, link).await;
}

/// How long a test waits for the server to hear a window's connection close:
/// when the socket's close reaches it, which a loaded machine delays.
const WINDOW_GONE_DEADLINE: Duration = Duration::from_secs(60);

/// Ask to open `link` until no window takes it: the server has heard every
/// window close.
async fn until_no_window(caller: &PimbleClient, link: &str) {
    let deadline = tokio::time::Instant::now() + WINDOW_GONE_DEADLINE;
    while caller.open_in_app(link, None).await.unwrap() {
        assert!(tokio::time::Instant::now() < deadline, "every window is gone");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
    assert_eq!(heard, HandedLink { url: "pimble:settings".into(), activation_token: Some("token-from-the-click".into()), ack: None });
}

/// A link with no token is sent as the bare URL, as before the token, so a
/// window from before it still reads every link it could; one with a token
/// is an object, which the window reads either way.
#[test]
fn a_link_without_a_token_is_the_bare_url_on_the_wire() {
    let bare = HandedLink::bare("pimble:settings");
    assert_eq!(serde_json::to_value(&bare).unwrap(), serde_json::json!("pimble:settings"));
    assert_eq!(serde_json::from_value::<HandedLink>(serde_json::json!("pimble:settings")).unwrap(), bare);

    let with = HandedLink { url: "pimble:settings".into(), activation_token: Some("t".into()), ack: None };
    let wire = serde_json::to_value(&with).unwrap();
    assert_eq!(wire, serde_json::json!({ "url": "pimble:settings", "activation_token": "t" }));
    assert_eq!(serde_json::from_value::<HandedLink>(wire).unwrap(), with);
}

/// The number a window confirms with rides the object form, beside or
/// without a token.
#[test]
fn a_link_to_confirm_carries_its_ack() {
    let link = HandedLink { url: "pimble:".into(), activation_token: None, ack: Some(7) };
    let wire = serde_json::to_value(&link).unwrap();
    assert_eq!(wire, serde_json::json!({ "url": "pimble:", "ack": 7 }));
    assert_eq!(serde_json::from_value::<HandedLink>(wire).unwrap(), link);
}

async fn started_server(dir: &TempDir) -> (PimbleServer, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: addr.parse().unwrap(),
        ..ServerConfig::in_dir(dir.path())
    });
    server.start().await.unwrap();
    (server, format!("http://{addr}"))
}

/// A caller that asks for confirmation hears `delivered` only once the
/// window says it acted on the link (docs/NOTIFICATIONS_CONTRACT.md "Built
/// in wave 4": a hand-over is acknowledged).
#[tokio::test]
async fn a_confirmed_link_is_delivered_when_the_window_says_it_took_it() {
    let dir = TempDir::new().unwrap();
    let (_server, url) = started_server(&dir).await;
    let window = PimbleClient::connect(&url).await.unwrap();
    let mut links = window.subscribe_app_links().await.unwrap();
    let caller = PimbleClient::connect(&url).await.unwrap();

    let asked = tokio::spawn(async move { caller.open_in_app_confirmed("pimble:", Some("t".into())).await.unwrap() });
    let heard = tokio::time::timeout(Duration::from_secs(5), links.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(heard.url, "pimble:");
    assert_eq!(heard.activation_token.as_deref(), Some("t"));
    let ack = heard.ack.expect("a link to confirm carries its number");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!asked.is_finished(), "the caller waits for the window");
    window.app_link_taken(ack).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(5), asked).await.unwrap().unwrap());
}

/// A window that hears the link but never acts on it (hung, or partway
/// through quitting) does not swallow it: the caller hears `false` after
/// the wait, and opens a window of its own. A late confirmation finds
/// nobody and changes nothing.
#[tokio::test]
async fn a_confirmed_link_no_window_acts_on_is_not_delivered() {
    let dir = TempDir::new().unwrap();
    let (_server, url) = started_server(&dir).await;
    let window = PimbleClient::connect(&url).await.unwrap();
    let mut links = window.subscribe_app_links().await.unwrap();
    let caller = PimbleClient::connect(&url).await.unwrap();

    let began = std::time::Instant::now();
    assert!(!caller.open_in_app_confirmed("pimble:", None).await.unwrap());
    let waited = began.elapsed();
    let wait = Duration::from_millis(pimble_rpc::APP_LINK_CONFIRM_WAIT_MS);
    assert!(waited >= wait, "waited the whole {wait:?}, not {waited:?}");
    assert!(waited < wait + Duration::from_secs(2), "and no longer: {waited:?}");

    let heard = tokio::time::timeout(Duration::from_secs(5), links.next()).await.unwrap().unwrap().unwrap();
    window.app_link_taken(heard.ack.unwrap()).await.unwrap();

    // Nobody listening at all answers at once: without waiting out the
    // confirmation's time, which a window listening would have had.
    drop(links);
    drop(window);
    until_no_window(&caller, "pimble:").await;
    let began = std::time::Instant::now();
    assert!(!caller.open_in_app_confirmed("pimble:", None).await.unwrap());
    assert!(began.elapsed() < wait, "no window, no wait: {:?}", began.elapsed());
}
