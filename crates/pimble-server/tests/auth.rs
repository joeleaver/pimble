//! HTTP-edge auth (docs/history/HARDENING_CONTRACT.md decisions 1-2), exercised
//! against a real `PimbleServer` on `127.0.0.1:0` — unlike `src/auth.rs`'s
//! own unit tests, which check the header rules with no network at all.

use pimble_client::PimbleClient;
use pimble_core::AuthMethod;
use pimble_server::{PimbleServer, ServerConfig};

/// Start a `PimbleServer` bound to an OS-assigned loopback port with
/// `auth_token`.
async fn start_server(auth_token: Option<&str>) -> PimbleServer {
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: auth_token.map(String::from),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    server
}

#[tokio::test]
async fn a_token_server_refuses_no_header_and_a_wrong_bearer() {
    let server = start_server(Some("secret")).await;
    let url = format!("http://{}", server.addr());

    match PimbleClient::connect(&url).await {
        Ok(_) => panic!("a connection with no credentials must be refused"),
        Err(e) => assert!(
            e.to_string().contains("401"),
            "refusing a missing credential should surface the 401 the edge returned, got: {}",
            e
        ),
    }

    match PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "wrong".into() }).await {
        Ok(_) => panic!("a wrong bearer token must be refused"),
        Err(e) => assert!(e.to_string().contains("401")),
    }

    match PimbleClient::connect_with_auth(&url, &AuthMethod::ApiKey { key: "wrong".into() }).await {
        Ok(_) => panic!("a wrong API key must be refused"),
        Err(_) => {}
    }
}

#[tokio::test]
async fn a_token_server_accepts_the_right_bearer_and_the_right_api_key() {
    let server = start_server(Some("secret")).await;
    let url = format!("http://{}", server.addr());

    let bearer = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "secret".into() })
        .await
        .expect("the right bearer token must be accepted");
    bearer.list_stores().await.expect("an authenticated connection can actually make RPC calls");

    let api_key = PimbleClient::connect_with_auth(&url, &AuthMethod::ApiKey { key: "secret".into() })
        .await
        .expect("the right API key must be accepted");
    api_key.list_stores().await.expect("an authenticated connection can actually make RPC calls");
}

/// Build a raw `WsClientBuilder` with an `Origin` header, the way a browser
/// (not `PimbleClient`, which never sends one) would on a WebSocket
/// handshake, and try to connect.
async fn try_connect_with_origin(url: &str, bearer: Option<&str>) -> Result<(), String> {
    use jsonrpsee::ws_client::{HeaderMap, HeaderValue, WsClientBuilder};

    let ws_url = url.replacen("http://", "ws://", 1);
    let mut headers = HeaderMap::new();
    headers.insert("origin", HeaderValue::from_static("http://evil.example"));
    if let Some(token) = bearer {
        headers.insert("authorization", HeaderValue::from_str(&format!("Bearer {}", token)).unwrap());
    }

    WsClientBuilder::default()
        .set_headers(headers)
        .build(&ws_url)
        .await
        .map(|_client| ())
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_request_with_an_origin_header_is_refused_without_a_token() {
    let server = start_server(None).await;
    let url = format!("http://{}", server.addr());

    let result = try_connect_with_origin(&url, None).await;
    assert!(result.is_err(), "Origin must be refused even with no token configured");
    assert!(result.unwrap_err().contains("403"));
}

#[tokio::test]
async fn a_request_with_an_origin_header_is_refused_even_with_the_right_token() {
    let server = start_server(Some("secret")).await;
    let url = format!("http://{}", server.addr());

    let result = try_connect_with_origin(&url, Some("secret")).await;
    assert!(result.is_err(), "Origin must be refused even when the bearer token is correct");
    assert!(result.unwrap_err().contains("403"));
}

#[tokio::test]
async fn start_refuses_a_non_loopback_address_without_a_token_but_succeeds_with_one() {
    let mut without_token =
        PimbleServer::with_config(ServerConfig { addr: "0.0.0.0:0".parse().unwrap(), ..Default::default() });
    let err = without_token.start().await.expect_err("binding 0.0.0.0 without a token must be refused");
    assert!(err.to_string().to_lowercase().contains("token"), "the refusal should explain why: {}", err);

    let mut with_token = PimbleServer::with_config(ServerConfig {
        addr: "0.0.0.0:0".parse().unwrap(),
        auth_token: Some("secret".into()),
        ..Default::default()
    });
    with_token.start().await.expect("binding 0.0.0.0 with a token must succeed");
    with_token.stop().await.expect("stops cleanly");
}

#[tokio::test]
async fn start_refuses_an_empty_or_whitespace_only_token_even_on_loopback() {
    let mut empty = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(String::new()),
        ..Default::default()
    });
    let err = empty.start().await.expect_err("an empty auth_token must be refused, even on loopback");
    assert!(err.to_string().to_lowercase().contains("empty"), "got: {}", err);

    let mut whitespace = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some("   ".into()),
        ..Default::default()
    });
    whitespace.start().await.expect_err("a whitespace-only auth_token must be refused too");
}
