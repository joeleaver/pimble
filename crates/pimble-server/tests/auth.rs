//! HTTP-edge auth (docs/history/HARDENING_CONTRACT.md decisions 1-2, extended by
//! docs/CLOUD_CONTRACT.md "B: pimble-server" items 1-5), exercised against a
//! real `PimbleServer` on `127.0.0.1:0` — unlike `src/auth.rs`'s own unit
//! tests, which check the header/JWT rules with no network at all (or, for
//! JWKS fetching, against a local axum stub but not a `PimbleServer`).

use std::collections::HashMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use pimble_client::PimbleClient;
use pimble_core::AuthMethod;
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;

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

// ── JWT test support: a local JWKS stub, plus signing tokens by hand ────

fn signing_key() -> SigningKey {
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

/// Serve `signing_key`'s public key as a JWKS under `kid`, on an
/// OS-assigned loopback port, for as long as the test process runs
/// (a `#[tokio::test]`'s runtime shuts down with the test, taking the
/// spawned task with it). Returns the endpoint's URL.
async fn spawn_jwks(signing_key: &SigningKey, kid: &str) -> String {
    let x = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let jwks = json!({ "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": kid, "x": x } ] });

    let app = axum::Router::new().route(
        "/jwks.json",
        axum::routing::get(move || {
            let jwks = jwks.clone();
            async move { axum::Json(jwks) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    format!("http://{}/jwks.json", addr)
}

/// A signed compact EdDSA JWT: `iss`/`aud: "pimble"`/`exp` (`exp_offset_secs`
/// from now — negative for an already-expired token) plus the
/// `claims.email`/`claims.stores` custom claims docs/CLOUD_CONTRACT.md
/// specifies.
fn make_jwt(
    signing_key: &SigningKey,
    kid: &str,
    issuer: &str,
    sub: &str,
    email: &str,
    stores: &HashMap<pimble_core::StoreId, &str>,
    exp_offset_secs: i64,
) -> String {
    let header = json!({ "alg": "EdDSA", "kid": kid });
    let stores_claim: serde_json::Map<String, serde_json::Value> =
        stores.iter().map(|(id, role)| (id.to_string(), json!(role))).collect();
    let payload = json!({
        "iss": issuer,
        "sub": sub,
        "aud": "pimble",
        "exp": chrono::Utc::now().timestamp() + exp_offset_secs,
        "claims": { "email": email, "stores": stores_claim },
    });

    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature = signing_key.sign(signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());
    format!("{signing_input}.{sig_b64}")
}

/// Start a server with both a static token (the accounts-service "Service"
/// credential used to set up stores in these tests, per "static token still
/// works alongside a JWT verifier") and a JWT verifier against `jwks_url`.
async fn start_jwt_server(auth_token: &str, jwks_url: &str, issuer: &str, allowed_origins: Vec<String>) -> PimbleServer {
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(auth_token.to_string()),
        jwks_url: Some(jwks_url.parse().unwrap()),
        jwt_issuer: Some(issuer.to_string()),
        allowed_origins,
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

// ── JWT auth and authorization (docs/CLOUD_CONTRACT.md "B: pimble-server") ──

#[tokio::test]
async fn a_reader_can_read_and_not_write_and_an_editor_can_write() {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;
    let url = format!("http://{}", server.addr());

    // The static token is still "Service": it sets up the store a JWT
    // principal could never create itself.
    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() })
        .await
        .expect("the static token still works alongside a JWT verifier");
    let dir = tempfile::tempdir().unwrap();
    let (store_id, root_id) = admin.create_store(dir.path().join("s.pimble"), "S").await.unwrap();

    let mut reader_grant = HashMap::new();
    reader_grant.insert(store_id, "reader");
    let reader_jwt = make_jwt(&sk, "kid-1", issuer, "reader-user", "reader@example.com", &reader_grant, 3600);
    let reader = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: reader_jwt }).await.expect("a valid JWT connects");
    reader.get_node(store_id, root_id).await.expect("a reader may read");
    let write_err = reader
        .create_node(store_id, Some(root_id), "document", "should be refused")
        .await
        .expect_err("a reader may not write");
    assert!(write_err.to_string().contains("Forbidden"), "expected a Forbidden error, got: {}", write_err);

    let mut editor_grant = HashMap::new();
    editor_grant.insert(store_id, "editor");
    let editor_jwt = make_jwt(&sk, "kid-1", issuer, "editor-user", "editor@example.com", &editor_grant, 3600);
    let editor = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: editor_jwt }).await.unwrap();
    editor.create_node(store_id, Some(root_id), "document", "ok").await.expect("an editor may write");
}

#[tokio::test]
async fn a_jwt_with_no_grant_on_a_store_is_forbidden() {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;
    let url = format!("http://{}", server.addr());

    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (store_id, root_id) = admin.create_store(dir.path().join("s.pimble"), "S").await.unwrap();

    // A grant on some other store, not this one.
    let mut grants = HashMap::new();
    grants.insert(pimble_core::StoreId::new(), "owner");
    let stranger_jwt = make_jwt(&sk, "kid-1", issuer, "stranger", "stranger@example.com", &grants, 3600);
    let stranger = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: stranger_jwt }).await.unwrap();

    let err = stranger.get_node(store_id, root_id).await.expect_err("no grant on this store must be forbidden");
    assert!(err.to_string().contains("Forbidden"), "expected a Forbidden error, got: {}", err);
}

#[tokio::test]
async fn list_stores_is_filtered_to_the_principals_grants() {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;
    let url = format!("http://{}", server.addr());

    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (visible_id, _) = admin.create_store(dir.path().join("visible.pimble"), "Visible").await.unwrap();
    let (_hidden_id, _) = admin.create_store(dir.path().join("hidden.pimble"), "Hidden").await.unwrap();

    let mut grants = HashMap::new();
    grants.insert(visible_id, "reader");
    let jwt = make_jwt(&sk, "kid-1", issuer, "user-1", "user@example.com", &grants, 3600);
    let user = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: jwt }).await.unwrap();

    let stores = user.list_stores().await.unwrap();
    assert_eq!(stores.len(), 1, "expected exactly the one granted store, got {:?}", stores.iter().map(|s| s.id).collect::<Vec<_>>());
    assert_eq!(stores[0].id, visible_id);

    // Service still sees everything.
    let all = admin.list_stores().await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn an_expired_jwt_is_refused_with_401() {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;
    let url = format!("http://{}", server.addr());

    let expired = make_jwt(&sk, "kid-1", issuer, "user-1", "user@example.com", &HashMap::new(), -3600);
    match PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: expired }).await {
        Ok(_) => panic!("an expired JWT must be refused"),
        Err(e) => assert!(e.to_string().contains("401"), "expected a 401, got: {}", e),
    }
}

#[tokio::test]
async fn access_token_query_parameter_authenticates_a_websocket_connection() {
    use jsonrpsee::ws_client::WsClientBuilder;

    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;

    // No grants needed: `listStores` with an empty grant set just answers
    // an empty list, which is enough to prove the connection is
    // authenticated as a `User`, not refused.
    let jwt = make_jwt(&sk, "kid-1", issuer, "user-1", "user@example.com", &HashMap::new(), 3600);

    let ws_url = format!("ws://{}/rpc?access_token={}", server.addr(), jwt);
    let raw_client = WsClientBuilder::default().build(&ws_url).await.expect("access_token query param must authenticate");

    use pimble_rpc::PimbleApiClient;
    raw_client.list_stores().await.expect("an authenticated call over the query-param connection succeeds");
}

#[tokio::test]
async fn origin_allowlist_admits_one_origin_and_refuses_another() {
    use jsonrpsee::ws_client::{HeaderMap, HeaderValue, WsClientBuilder};

    let mut with_allowlist = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        allowed_origins: vec!["https://good.example".to_string()],
        ..Default::default()
    });
    with_allowlist.start().await.expect("server starts");
    let ws_url = format!("ws://{}", with_allowlist.addr());

    let mut allowed_headers = HeaderMap::new();
    allowed_headers.insert("origin", HeaderValue::from_static("https://good.example"));
    WsClientBuilder::default()
        .set_headers(allowed_headers)
        .build(&ws_url)
        .await
        .expect("an allowlisted Origin must be admitted");

    let mut refused_headers = HeaderMap::new();
    refused_headers.insert("origin", HeaderValue::from_static("https://evil.example"));
    let result = WsClientBuilder::default().set_headers(refused_headers).build(&ws_url).await;
    assert!(result.is_err(), "an Origin not on the allowlist must be refused");
    assert!(result.unwrap_err().to_string().contains("403"));
}

// ── Mount source-store authorization (uniform with `getChildren`) ──────────

#[tokio::test]
async fn create_mount_is_forbidden_without_read_on_the_source_store() {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;
    let url = format!("http://{}", server.addr());

    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mounting_id, mounting_root) = admin.create_store(dir.path().join("mounting.pimble"), "Mounting").await.unwrap();
    let (source_id, source_root) = admin.create_store(dir.path().join("source.pimble"), "Source").await.unwrap();

    // Editor on the mounting store — enough to write there — but no grant
    // on the source at all.
    let mut grants = HashMap::new();
    grants.insert(mounting_id, "editor");
    let jwt = make_jwt(&sk, "kid-1", issuer, "user-1", "user@example.com", &grants, 3600);
    let user = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: jwt }).await.unwrap();

    let err = user
        .create_mount(mounting_id, mounting_root, source_id, source_root, None)
        .await
        .expect_err("createMount must be forbidden without Read on the source store");
    assert!(err.to_string().contains("Forbidden"), "expected a Forbidden error, got: {}", err);
}

#[tokio::test]
async fn get_mount_state_is_forbidden_without_read_on_the_source_store() {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;
    let url = format!("http://{}", server.addr());

    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mounting_id, mounting_root) = admin.create_store(dir.path().join("mounting.pimble"), "Mounting").await.unwrap();
    let (source_id, source_root) = admin.create_store(dir.path().join("source.pimble"), "Source").await.unwrap();
    let (mount_node_id, _mount_ref) =
        admin.create_mount(mounting_id, mounting_root, source_id, source_root, None).await.unwrap();

    // Reader on the mounting store, no grant on the source.
    let mut grants = HashMap::new();
    grants.insert(mounting_id, "reader");
    let jwt = make_jwt(&sk, "kid-1", issuer, "user-2", "user2@example.com", &grants, 3600);
    let user = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: jwt }).await.unwrap();

    let err = user
        .get_mount_state(mounting_id, mount_node_id)
        .await
        .expect_err("getMountState must be forbidden without Read on the source store");
    assert!(err.to_string().contains("Forbidden"), "expected a Forbidden error, got: {}", err);
}
