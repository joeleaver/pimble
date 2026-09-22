//! HTTP-edge auth (docs/history/HARDENING_CONTRACT.md decisions 1-2, extended by
//! docs/CLOUD_CONTRACT.md "B: pimble-server" items 1-5), exercised against a
//! real `PimbleServer` on `127.0.0.1:0` — unlike `src/auth.rs`'s own unit
//! tests, which check the header/JWT rules with no network at all (or, for
//! JWKS fetching, against a local axum stub but not a `PimbleServer`).

use std::collections::HashMap;
use std::time::Duration;

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
    let stores_claim = stores.iter().map(|(id, role)| (id.to_string(), json!(role))).collect();
    make_jwt_with_claims(signing_key, kid, issuer, sub, email, stores_claim, exp_offset_secs)
}

/// One store's claim for a share's recipient: a role per shared root
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5), where a whole-store grant is
/// the role's string alone.
fn shares_claim(roots: &[(pimble_core::NodeId, &str)]) -> serde_json::Value {
    let roots: serde_json::Map<String, serde_json::Value> = roots.iter().map(|(root, role)| (root.to_string(), json!(role))).collect();
    json!({ "roots": roots })
}

/// A JWT for a member of one store's shares.
fn make_scoped_jwt(signing_key: &SigningKey, kid: &str, issuer: &str, sub: &str, store_id: pimble_core::StoreId, roots: &[(pimble_core::NodeId, &str)]) -> String {
    let mut stores_claim = serde_json::Map::new();
    stores_claim.insert(store_id.to_string(), shares_claim(roots));
    make_jwt_with_claims(signing_key, kid, issuer, sub, &format!("{sub}@example.com"), stores_claim, 3600)
}

/// [`make_jwt`] with `claims.stores` as given: each store's value is a role
/// (the whole store) or [`shares_claim`]'s object.
fn make_jwt_with_claims(
    signing_key: &SigningKey,
    kid: &str,
    issuer: &str,
    sub: &str,
    email: &str,
    stores_claim: serde_json::Map<String, serde_json::Value>,
    exp_offset_secs: i64,
) -> String {
    let header = json!({ "alg": "EdDSA", "kid": kid });
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
    assert_eq!(write_err.to_string(), pimble_core::StoreAccess::READ_ONLY_REFUSAL, "a reader's refusal is the sentence, whole");

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

// ── Scoped grants: a share's member (docs/NODE_DOCUMENT_CONTRACT.md section 5) ──

const NO_GRANT: &str = "no grant for this document";

fn refused_as_no_grant<T: std::fmt::Debug>(result: Result<T, pimble_client::ClientError>, what: &str) {
    let err = result.expect_err(what).to_string();
    assert!(err.contains(NO_GRANT), "{what}: expected the document refusal, got: {err}");
}

fn refused_as_read_only<T: std::fmt::Debug>(result: Result<T, pimble_client::ClientError>, what: &str) {
    let err = result.expect_err(what).to_string();
    assert_eq!(err, pimble_core::StoreAccess::READ_ONLY_REFUSAL, "{what}: the sentence is the whole message");
}

fn edit_of(text: &str) -> pimble_rpc::EditOperation {
    use base64::engine::general_purpose::STANDARD;
    let doc = pimble_crdt::NodeDoc::from_plain_text(text).unwrap();
    pimble_rpc::EditOperation::IncrementalChanges { changes: STANDARD.encode(doc.save()) }
}

/// A new node's document as a client that makes its own tree edits would
/// send it: initialised, under `parent`.
fn create_edit(title: &str, parent: pimble_core::NodeId) -> pimble_rpc::EditOperation {
    use base64::engine::general_purpose::STANDARD;
    let mut doc = pimble_crdt::NodeDoc::new();
    doc.init("document", title, Some(parent), &chrono::Utc::now().to_rfc3339()).unwrap();
    pimble_rpc::EditOperation::IncrementalChanges { changes: STANDARD.encode(doc.save()) }
}

/// A plain store with a shared folder and something outside it, a server in
/// JWT mode in front of it, and the service client that set it up.
struct SharedStore {
    _server: PimbleServer,
    _dir: tempfile::TempDir,
    url: String,
    sk: SigningKey,
    issuer: &'static str,
    admin: PimbleClient,
    store_id: pimble_core::StoreId,
    root: pimble_core::NodeId,
    shared: pimble_core::NodeId,
    inside: pimble_core::NodeId,
    outside: pimble_core::NodeId,
}

impl SharedStore {
    async fn start() -> Self {
        let sk = signing_key();
        let jwks_url = spawn_jwks(&sk, "kid-1").await;
        let issuer = "https://issuer.example/v1";
        let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;
        let url = format!("http://{}", server.addr());
        let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (store_id, root) = admin.create_store(dir.path().join("s.pimble"), "S").await.unwrap();
        let shared = admin.create_node(store_id, Some(root), "folder", "Shared").await.unwrap();
        let inside = admin.create_node(store_id, Some(shared), "document", "Inside").await.unwrap();
        let outside = admin.create_node(store_id, Some(root), "document", "Outside").await.unwrap();
        Self { _server: server, _dir: dir, url, sk, issuer, admin, store_id, root, shared, inside, outside }
    }

    async fn member(&self, sub: &str, roots: &[(pimble_core::NodeId, &str)]) -> PimbleClient {
        let jwt = make_scoped_jwt(&self.sk, "kid-1", self.issuer, sub, self.store_id, roots);
        PimbleClient::connect_with_auth(&self.url, &AuthMethod::Bearer { token: jwt }).await.expect("a scoped JWT connects")
    }
}

#[tokio::test]
async fn a_scoped_editor_reads_and_writes_exactly_its_scope() {
    let s = SharedStore::start().await;
    let (store_id, root, shared, inside, outside) = (s.store_id, s.root, s.shared, s.inside, s.outside);
    let member = s.member("bob", &[(shared, "editor")]).await;

    // Reading: the share's root and what is under it; anything else,
    // whether it exists or not, is the same refusal.
    assert_eq!(member.get_node(store_id, shared).await.unwrap().metadata.title, "Shared");
    member.get_node(store_id, inside).await.expect("a document in scope");
    refused_as_no_grant(member.get_node(store_id, outside).await, "a document outside the scope");
    refused_as_no_grant(member.get_node(store_id, root).await, "the store's root");
    refused_as_no_grant(member.get_node(store_id, pimble_core::NodeId::new()).await, "a document that does not exist");
    let (_, children) = member.get_children(store_id, shared).await.unwrap();
    assert_eq!(children.iter().map(|n| n.id).collect::<Vec<_>>(), vec![inside]);
    refused_as_no_grant(member.get_children(store_id, root).await, "the root's children");
    let some = member.get_nodes(store_id, vec![inside, outside]).await.unwrap();
    assert_eq!(some.iter().map(|n| n.id).collect::<Vec<_>>(), vec![inside], "getNodes leaves out what is not the member's");

    // The store row a member is shown starts at their share.
    let listed = member.list_stores().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].roots, vec![shared]);
    assert_eq!(listed[0].root_node_id, shared, "never the store's own root, a document the member has no grant on");
    assert_eq!(listed[0].access, pimble_core::StoreAccess::Full);

    // Writing: everything in scope, structure included; a create joins it.
    let created = member.create_node(store_id, Some(shared), "folder", "Made by Bob").await.expect("a create under the share");
    member.get_node(store_id, created).await.expect("the new node is in the scope at once");
    let mut renamed = member.get_node(store_id, inside).await.unwrap().metadata;
    renamed.title = "Renamed by Bob".into();
    member.update_node_metadata(store_id, inside, renamed).await.expect("metadata in scope");
    member.move_node(store_id, inside, created, None).await.expect("a move inside the scope");
    member.apply_edit(store_id, inside, "bob-editor", edit_of("typed by bob")).await.expect("an edit in scope");
    let by_edit = pimble_core::NodeId::new();
    member.apply_edit(store_id, by_edit, "bob-editor", create_edit("By edit", shared)).await.expect("a document created by applyEdit under a parent in scope");
    member.get_node(store_id, by_edit).await.expect("in scope by its parent, before any list names it");
    member.delete_node(store_id, by_edit).await.expect("a delete in scope");
    member.undelete_node(store_id, by_edit).await.expect("a tombstone stays in scope, so it can come back");

    refused_as_no_grant(member.create_node(store_id, Some(root), "document", "x").await, "a create under the root");
    refused_as_no_grant(member.create_node(store_id, None, "document", "x").await, "a create with no parent is one under the root");
    refused_as_no_grant(member.create_node(store_id, Some(outside), "document", "x").await, "a create under a node outside");
    let outside_meta = s.admin.get_node(store_id, outside).await.unwrap().metadata;
    refused_as_no_grant(member.update_node_metadata(store_id, outside, outside_meta).await, "metadata outside");
    refused_as_no_grant(member.move_node(store_id, inside, root, None).await, "a move out of the scope");
    refused_as_no_grant(member.move_node(store_id, outside, shared, None).await, "a move into the scope");
    refused_as_no_grant(member.delete_node(store_id, outside).await, "a delete outside");
    refused_as_no_grant(member.delete_node(store_id, shared).await, "deleting the share's own root edits its parent's list");
    refused_as_no_grant(member.undelete_node(store_id, outside).await, "an undelete outside");
    refused_as_no_grant(member.apply_edit(store_id, outside, "bob-editor", edit_of("no")).await, "an edit outside");
    refused_as_no_grant(
        member.apply_edit(store_id, pimble_core::NodeId::new(), "bob-editor", create_edit("x", outside)).await,
        "a document created under a parent outside",
    );
    refused_as_no_grant(member.apply_edit(store_id, pimble_core::NodeId::new(), "bob-editor", edit_of("no parent named")).await, "a new document naming no parent");
    refused_as_no_grant(member.create_mount(store_id, root, store_id, inside, None).await, "a mount under the root");
    assert_eq!(s.admin.get_node(store_id, outside).await.unwrap().metadata.title, "Outside", "nothing refused changed anything");

    // syncNodes: the scope, and nothing of the rest, named or not.
    let (answered, unknown_ids) = member
        .sync_nodes(store_id, &[(outside, pimble_crdt::empty_state_vector()), (inside, pimble_crdt::empty_state_vector())], true)
        .await
        .unwrap();
    assert_eq!(answered.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(), vec![inside], "a named document outside the scope is left out like one not held");
    let unknown: std::collections::HashSet<_> = unknown_ids.into_iter().collect();
    assert_eq!(unknown, [shared, created, by_edit].into_iter().collect(), "unknown ids are the scope's and no one else's");
}

#[tokio::test]
async fn a_scoped_subscriber_hears_of_its_scope_and_of_nothing_else() {
    let s = SharedStore::start().await;
    let (store_id, root, shared, inside, outside) = (s.store_id, s.root, s.shared, s.inside, s.outside);
    let member = s.member("bob", &[(shared, "editor")]).await;
    let mut member_sub = member.subscribe_store_changes(store_id).await.unwrap();
    let mut whole_sub = s.admin.subscribe_store_changes(store_id).await.unwrap();
    refused_as_no_grant(member.subscribe_node_changes(store_id, outside).await.map(|_| ()), "a node subscription outside the scope");
    let mut inside_sub = member.subscribe_node_changes(store_id, inside).await.expect("a node subscription in scope");

    // Outside first: whatever reaches the member first is then, by order,
    // proof that the change outside did not.
    s.admin.apply_edit(store_id, outside, "admin", edit_of("outside text")).await.unwrap();
    s.admin.create_node(store_id, Some(root), "document", "Another outside").await.unwrap();
    s.admin.apply_edit(store_id, inside, "admin", edit_of("inside text")).await.unwrap();
    // The owner moves the share itself: the member's own root changed, and
    // the parents it moved between are not the member's to hear of.
    let elsewhere = s.admin.create_node(store_id, Some(root), "folder", "Elsewhere").await.unwrap();
    s.admin.move_node(store_id, shared, elsewhere, None).await.unwrap();
    let sentinel = s.admin.create_node(store_id, Some(shared), "document", "Sentinel").await.unwrap();

    let mut heard = Vec::new();
    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), member_sub.next()).await.expect("the sentinel arrives").unwrap().unwrap();
        let done = matches!(&notif.change_kind, pimble_rpc::StoreChangeKind::NodeCreated { node_id, .. } if *node_id == sentinel);
        heard.push(notif);
        if done {
            break;
        }
    }
    let foreign = [root, outside, elsewhere];
    for notif in &heard {
        let text = serde_json::to_string(&notif.change_kind).unwrap();
        for id in foreign {
            assert!(!text.contains(&id.to_string()), "a scoped subscriber must never be told another document's id: {text}");
        }
    }
    assert!(
        heard.iter().any(|n| matches!(&n.change_kind, pimble_rpc::StoreChangeKind::ContentUpdated { node_id } if *node_id == inside)),
        "the edit in scope arrives as it is: {heard:?}"
    );
    let moved = heard
        .iter()
        .find(|n| matches!(&n.change_kind, pimble_rpc::StoreChangeKind::TreeStructure { node_ids } if node_ids == &vec![shared]))
        .expect("the share's own move arrives as a change to its document, naming no parent");
    assert!(moved.update.is_some(), "with the bytes, so the member's copy of the root keeps up");

    // An unscoped subscriber heard all of it, the move as a move.
    let mut whole_heard = Vec::new();
    loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), whole_sub.next()).await.expect("the sentinel arrives").unwrap().unwrap();
        let done = matches!(&notif.change_kind, pimble_rpc::StoreChangeKind::NodeCreated { node_id, .. } if *node_id == sentinel);
        whole_heard.push(notif);
        if done {
            break;
        }
    }
    assert!(whole_heard.iter().any(|n| matches!(&n.change_kind, pimble_rpc::StoreChangeKind::NodeMoved { node_id, .. } if *node_id == shared)));
    assert!(whole_heard.iter().any(|n| matches!(&n.change_kind, pimble_rpc::StoreChangeKind::ContentUpdated { node_id } if *node_id == outside)));

    let node_notif = tokio::time::timeout(Duration::from_secs(5), inside_sub.next()).await.expect("the node's own subscriber hears the edit").unwrap().unwrap();
    assert_eq!(node_notif.node_id, inside);
}

#[tokio::test]
async fn a_scoped_members_search_finds_its_scope_only() {
    let s = SharedStore::start().await;
    let (store_id, shared, inside, outside) = (s.store_id, s.shared, s.inside, s.outside);
    s.admin.apply_edit(store_id, inside, "admin", edit_of("quokka notes kept inside the share")).await.unwrap();
    s.admin.apply_edit(store_id, outside, "admin", edit_of("quokka notes kept outside the share")).await.unwrap();
    let member = s.member("bob", &[(shared, "reader")]).await;

    // Content is indexed on a debounce; wait until the unscoped search sees both.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let all = s.admin.search("quokka", vec![store_id], false, 10).await.unwrap_or_default();
        if all.len() >= 2 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "both documents should be indexed, got {all:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let found = member.search("quokka", vec![store_id], false, 10).await.unwrap();
    assert_eq!(found.iter().map(|r| r.node_id).collect::<Vec<_>>(), vec![inside], "hits outside the scope are dropped");
    let found = member.search("quokka", Vec::new(), false, 1).await.unwrap();
    assert_eq!(found.iter().map(|r| r.node_id).collect::<Vec<_>>(), vec![inside], "and a page of one still finds the one in scope");
}

/// `Role::Reader` refuses every write, and the refusal is the one sentence,
/// whole: a whole-store reader and a share's reader alike.
#[tokio::test]
async fn a_reader_is_refused_every_write_with_the_sentence_as_the_whole_message() {
    let s = SharedStore::start().await;
    let (store_id, root, shared, inside) = (s.store_id, s.root, s.shared, s.inside);
    let mut whole = HashMap::new();
    whole.insert(store_id, "reader");
    let whole_reader_jwt = make_jwt(&s.sk, "kid-1", s.issuer, "reader", "reader@example.com", &whole, 3600);
    let whole_reader = PimbleClient::connect_with_auth(&s.url, &AuthMethod::Bearer { token: whole_reader_jwt }).await.unwrap();
    let share_reader = s.member("carol", &[(shared, "reader")]).await;

    for (who, reader) in [("a whole-store reader", &whole_reader), ("a share's reader", &share_reader)] {
        reader.get_node(store_id, inside).await.unwrap_or_else(|e| panic!("{who} reads: {e}"));
        let metadata = reader.get_node(store_id, inside).await.unwrap().metadata;
        refused_as_read_only(reader.create_node(store_id, Some(shared), "document", "x").await, who);
        refused_as_read_only(reader.update_node_metadata(store_id, inside, metadata).await, who);
        refused_as_read_only(reader.set_node_content_bytes(store_id, inside, pimble_crdt::NodeDoc::from_plain_text("x").unwrap().save(), None).await, who);
        refused_as_read_only(reader.delete_node(store_id, inside).await, who);
        refused_as_read_only(reader.undelete_node(store_id, inside).await, who);
        refused_as_read_only(reader.move_node(store_id, inside, shared, None).await, who);
        refused_as_read_only(reader.apply_edit(store_id, inside, "reader-editor", edit_of("x")).await, who);
        refused_as_read_only(reader.create_mount(store_id, shared, store_id, inside, None).await, who);
        refused_as_read_only(reader.rebuild_index(store_id).await, who);
        let listed = reader.list_stores().await.unwrap();
        assert_eq!(listed[0].access, pimble_core::StoreAccess::Read, "{who} is shown a store it may read");
    }
    assert_eq!(s.admin.get_children(store_id, root).await.unwrap().1.len(), 2, "nothing was written");
}

/// A role per shared root (docs/NODE_DOCUMENT_CONTRACT.md section 5): a
/// reader of one folder who edits another is refused writes under the first
/// with the reader's sentence and allowed them under the second, and a
/// document in both scopes takes the wider role.
#[tokio::test]
async fn a_reader_of_one_root_and_editor_of_another_writes_only_under_the_second() {
    let s = SharedStore::start().await;
    let (store_id, read_root, read_doc) = (s.store_id, s.shared, s.inside);
    let edit_root = s.admin.create_node(store_id, Some(s.root), "folder", "Edited").await.unwrap();
    let edit_doc = s.admin.create_node(store_id, Some(edit_root), "document", "E").await.unwrap();
    // Overlapping shares: a folder the member edits inside the one they read.
    let nested_edit_root = s.admin.create_node(store_id, Some(read_root), "folder", "Edited inside").await.unwrap();
    let member = s.member("dana", &[(read_root, "reader"), (edit_root, "editor"), (nested_edit_root, "editor")]).await;

    member.get_node(store_id, read_doc).await.expect("reads under the root it reads");
    refused_as_read_only(member.apply_edit(store_id, read_doc, "dana", edit_of("no")).await, "an edit under the read root");
    refused_as_read_only(member.create_node(store_id, Some(read_root), "document", "no").await, "a create under the read root");
    refused_as_read_only(member.delete_node(store_id, read_doc).await, "a delete under the read root");
    refused_as_read_only(member.move_node(store_id, edit_doc, read_root, None).await, "a move into the read root edits its list");
    refused_as_read_only(member.move_node(store_id, read_doc, edit_root, None).await, "a move out of the read root edits the node");

    member.apply_edit(store_id, edit_doc, "dana", edit_of("yes")).await.expect("an edit under the edited root");
    member.create_node(store_id, Some(edit_root), "document", "yes").await.expect("a create under the edited root");
    member.create_node(store_id, Some(nested_edit_root), "document", "wider").await.expect("in both scopes: the wider role");
    refused_as_no_grant(member.get_node(store_id, s.outside).await, "and what is in neither is still nobody's");

    let listed = member.list_stores().await.unwrap();
    assert_eq!(listed[0].access, pimble_core::StoreAccess::Full, "something here may be written");
    assert_eq!(listed[0].roots.len(), 3);
}

/// Every node an RPC returns says what its caller may change of it
/// (`Node::access`), by the judgement a write of it would meet: a client
/// disables what would be refused instead of letting someone type into a
/// document the server will not take. The same store answers differently to
/// a reader of one root who edits another, to a whole-store reader and to
/// the operator, and `getStoreSync`/`listStores` name the roots only read.
#[tokio::test]
async fn a_returned_node_says_what_its_caller_may_change_of_it() {
    use pimble_core::StoreAccess::{Full, Read};
    let s = SharedStore::start().await;
    let (store_id, read_root, read_doc) = (s.store_id, s.shared, s.inside);
    let edit_root = s.admin.create_node(store_id, Some(s.root), "folder", "Edited").await.unwrap();
    let edit_doc = s.admin.create_node(store_id, Some(edit_root), "document", "E").await.unwrap();
    let nested_edit_root = s.admin.create_node(store_id, Some(read_root), "folder", "Edited inside").await.unwrap();
    let nested_doc = s.admin.create_node(store_id, Some(nested_edit_root), "document", "Wider").await.unwrap();
    let member = s.member("dana", &[(read_root, "reader"), (edit_root, "editor"), (nested_edit_root, "editor")]).await;

    // getNode
    for (id, expected, what) in [
        (read_root, Read, "the root it reads"),
        (read_doc, Read, "a document under the root it reads"),
        (edit_root, Full, "the root it edits"),
        (edit_doc, Full, "a document under the root it edits"),
        (nested_edit_root, Full, "an edited root inside the read one"),
        (nested_doc, Full, "in both scopes: the wider role"),
    ] {
        assert_eq!(member.get_node(store_id, id).await.unwrap().access, expected, "getNode: {what}");
    }
    // getNodes
    let nodes = member.get_nodes(store_id, vec![read_doc, edit_doc, nested_doc]).await.unwrap();
    assert_eq!(nodes.iter().map(|n| (n.id, n.access)).collect::<Vec<_>>(), vec![(read_doc, Read), (edit_doc, Full), (nested_doc, Full)]);
    // getChildren: each child judged for itself, in one list.
    let (_, under_read) = member.get_children(store_id, read_root).await.unwrap();
    let mut under_read: Vec<_> = under_read.iter().map(|n| (n.id, n.access)).collect();
    under_read.sort_by_key(|(id, _)| *id != read_doc);
    assert_eq!(under_read, vec![(read_doc, Read), (nested_edit_root, Full)]);
    let (_, under_edit) = member.get_children(store_id, edit_root).await.unwrap();
    assert_eq!(under_edit.iter().map(|n| (n.id, n.access)).collect::<Vec<_>>(), vec![(edit_doc, Full)]);

    // The judgement is the one a write meets.
    refused_as_read_only(member.apply_edit(store_id, read_doc, "dana", edit_of("no")).await, "what was answered `read` refuses the edit");
    member.apply_edit(store_id, nested_doc, "dana", edit_of("yes")).await.expect("what was answered `full` takes it");

    // Which roots are only read, for a client to notice a change by.
    let (_, _, _, access) = member.get_store_sync_with_access(store_id).await.unwrap();
    assert_eq!(access, Full);
    assert_eq!(member.get_store_sync_response(store_id).await.unwrap().read_only_roots, vec![read_root]);
    assert_eq!(member.list_stores().await.unwrap()[0].read_only_roots, vec![read_root]);

    // A whole-store reader: everything reads. The operator: everything is full.
    let mut whole = HashMap::new();
    whole.insert(store_id, "reader");
    let reader_jwt = make_jwt(&s.sk, "kid-1", s.issuer, "reader", "reader@example.com", &whole, 3600);
    let reader = PimbleClient::connect_with_auth(&s.url, &AuthMethod::Bearer { token: reader_jwt }).await.unwrap();
    assert_eq!(reader.get_node(store_id, edit_doc).await.unwrap().access, Read);
    assert!(reader.get_children(store_id, s.root).await.unwrap().1.iter().all(|n| n.access == Read));
    assert!(reader.get_store_sync_response(store_id).await.unwrap().read_only_roots.is_empty(), "one answer covers the store");
    assert_eq!(s.admin.get_node(store_id, read_doc).await.unwrap().access, Full);
    assert!(s.admin.get_children(store_id, read_root).await.unwrap().1.iter().all(|n| n.access == Full));

    // `full` is the absence of the field: an answer is what it was before.
    let wire = serde_json::to_value(s.admin.get_node(store_id, read_doc).await.unwrap()).unwrap();
    assert!(wire.get("access").is_none(), "{wire}");
}

// ── docs/MOVE_CONTRACT.md, wave 2: transplantNode and listDeleted ────────

/// Two plain stores on one server, and the service client used to set them
/// up: `transplantNode`'s "between stores" half needs two, unlike
/// [`SharedStore`]'s one.
struct TwoStores {
    _server: PimbleServer,
    _dir: tempfile::TempDir,
    admin: PimbleClient,
    sk: SigningKey,
    issuer: &'static str,
    url: String,
    store_a: pimble_core::StoreId,
    root_a: pimble_core::NodeId,
    store_b: pimble_core::StoreId,
    root_b: pimble_core::NodeId,
}

async fn two_stores() -> TwoStores {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer, Vec::new()).await;
    let url = format!("http://{}", server.addr());
    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (store_a, root_a) = admin.create_store(dir.path().join("a.pimble"), "A").await.unwrap();
    let (store_b, root_b) = admin.create_store(dir.path().join("b.pimble"), "B").await.unwrap();
    TwoStores { _server: server, _dir: dir, admin, sk, issuer, url, store_a, root_a, store_b, root_b }
}

/// A transplant between two plain stores lands with the text intact, the
/// source tombstoned and found through `listDeleted`
/// (docs/MOVE_CONTRACT.md "Between stores").
#[tokio::test]
async fn transplant_node_between_two_plain_stores_carries_the_text_and_tombstones_the_source() {
    let t = two_stores().await;
    let note = t.admin.create_node(t.store_a, Some(t.root_a), "document", "Note").await.unwrap();
    t.admin.apply_edit(t.store_a, note, "admin", edit_of("packed and ready")).await.unwrap();
    let inbox = t.admin.create_node(t.store_b, Some(t.root_b), "folder", "Inbox").await.unwrap();

    let answer = t.admin.transplant_node(t.store_a, note, t.store_b, inbox, None).await.expect("a transplant between two plain stores");
    assert_ne!(answer.node_id, note, "the note lands under a new id");
    assert!(answer.left_shares.is_empty(), "neither store had a share in play");
    let new_note = answer.node_id;

    let landed = t.admin.get_node(t.store_b, new_note).await.expect("the note is in the target store");
    assert_eq!(pimble_crdt::NodeDoc::text_of(&landed.content), "packed and ready", "the text is intact");
    assert_eq!(landed.parent_id, Some(inbox));

    assert!(t.admin.get_node(t.store_a, note).await.is_err(), "the source is gone");
    let deleted = t.admin.list_deleted(t.store_a).await.unwrap();
    assert!(deleted.iter().any(|d| d.node.id == note && d.deleted_at.is_some()), "and listed as a tombstone on the source");
}

/// The same store on both sides is a `moveNode`, answered as one
/// (docs/MOVE_CONTRACT.md "Between stores").
#[tokio::test]
async fn transplant_node_between_the_same_store_answers_like_move_node() {
    let t = two_stores().await;
    let note = t.admin.create_node(t.store_a, Some(t.root_a), "document", "Note").await.unwrap();
    let folder = t.admin.create_node(t.store_a, Some(t.root_a), "folder", "Folder").await.unwrap();

    let answer = t.admin.transplant_node(t.store_a, note, t.store_a, folder, None).await.expect("same store, answered as a move");
    assert_eq!(answer.node_id, note, "no share to leave here: a plain move keeps the id");
    assert_eq!(t.admin.get_node(t.store_a, note).await.unwrap().parent_id, Some(folder));
}

/// A reader on either side refuses `transplantNode` with the reader's
/// sentence, as `moveNode` is refused today.
#[tokio::test]
async fn transplant_node_is_refused_without_write_on_either_side() {
    let t = two_stores().await;
    let note = t.admin.create_node(t.store_a, Some(t.root_a), "document", "Note").await.unwrap();
    let inbox = t.admin.create_node(t.store_b, Some(t.root_b), "folder", "Inbox").await.unwrap();

    let mut roles = HashMap::new();
    roles.insert(t.store_a, "reader");
    roles.insert(t.store_b, "editor");
    let jwt = make_jwt(&t.sk, "kid-1", t.issuer, "r1", "r1@example.com", &roles, 3600);
    let reader_of_a = PimbleClient::connect_with_auth(&t.url, &AuthMethod::Bearer { token: jwt }).await.unwrap();
    refused_as_read_only(reader_of_a.transplant_node(t.store_a, note, t.store_b, inbox, None).await, "a reader of the source store");

    let mut roles = HashMap::new();
    roles.insert(t.store_a, "editor");
    roles.insert(t.store_b, "reader");
    let jwt = make_jwt(&t.sk, "kid-1", t.issuer, "r2", "r2@example.com", &roles, 3600);
    let reader_of_b = PimbleClient::connect_with_auth(&t.url, &AuthMethod::Bearer { token: jwt }).await.unwrap();
    refused_as_read_only(reader_of_b.transplant_node(t.store_a, note, t.store_b, inbox, None).await, "a reader of the target store");

    assert!(t.admin.get_node(t.store_a, note).await.is_ok(), "nothing refused changed the source");
    assert_eq!(t.admin.get_children(t.store_b, inbox).await.unwrap().1.len(), 0, "nor planted anything in the target");
}

/// `listDeleted` judges like `getChildren`: a scoped member sees only their
/// scope's tombstones (docs/MOVE_CONTRACT.md "Seeing and undoing what was
/// removed").
#[tokio::test]
async fn list_deleted_shows_a_scoped_members_scope_only() {
    let s = SharedStore::start().await;
    let (store_id, shared, inside, outside) = (s.store_id, s.shared, s.inside, s.outside);
    s.admin.delete_node(store_id, inside).await.unwrap();
    s.admin.delete_node(store_id, outside).await.unwrap();

    let member = s.member("eve", &[(shared, "editor")]).await;
    let seen = member.list_deleted(store_id).await.unwrap();
    assert_eq!(seen.iter().map(|d| d.node.id).collect::<Vec<_>>(), vec![inside], "only the scope's own tombstone");
    assert_eq!(seen[0].parent_title.as_deref(), Some("Shared"), "the folder it was in, when that is held here");

    let admin_seen = s.admin.list_deleted(store_id).await.unwrap();
    let ids: std::collections::HashSet<_> = admin_seen.iter().map(|d| d.node.id).collect();
    assert!(ids.contains(&inside) && ids.contains(&outside), "the owner sees both: {ids:?}");
}

/// "Put Back" (docs/MOVE_CONTRACT.md "Seeing and undoing what was
/// removed"): a live node no held list names but whose `parent_id` still
/// says a scope this principal may write is still theirs to move — there
/// is no list for the move to leave, so only the node and the new parent
/// are judged.
#[tokio::test]
async fn put_back_of_a_node_no_list_names_succeeds_for_an_editor_of_its_scope() {
    let s = SharedStore::start().await;
    let (store_id, shared, inside) = (s.store_id, s.shared, s.inside);

    // A tampered client that unlists a node without moving it
    // (docs/MOVE_CONTRACT.md "Repair", "a tampered client that also
    // unlists X"): `inside`'s `parent_id` still says `shared`, but no list
    // holds it any more.
    let shared_bytes = s.admin.get_node(store_id, shared).await.unwrap().content;
    let mut shared_doc = pimble_crdt::NodeDoc::load(&shared_bytes).unwrap();
    let before_sv = shared_doc.state_vector();
    shared_doc.remove_child(inside).unwrap();
    let diff = shared_doc.diff_since(&before_sv).unwrap();
    s.admin
        .apply_edit(
            store_id,
            shared,
            "tamperer",
            pimble_rpc::EditOperation::IncrementalChanges { changes: base64::engine::general_purpose::STANDARD.encode(diff) },
        )
        .await
        .unwrap();
    assert!(s.admin.get_children(store_id, shared).await.unwrap().1.is_empty(), "no list names it any more");

    let member = s.member("fay", &[(shared, "editor")]).await;
    let answer = member.move_node(store_id, inside, shared, None).await.expect("an editor puts an unlisted node of their scope back");
    assert_eq!(answer.node_id, inside, "putting it back under the same scope is a plain move, not a transplant");
    assert_eq!(member.get_children(store_id, shared).await.unwrap().1.iter().map(|n| n.id).collect::<Vec<_>>(), vec![inside]);
}

/// The list a move leaves is the list that names the node, not the node's
/// `parent_id` (docs/MOVE_CONTRACT.md "Repair", and the write-judgement
/// finding it left open): a tampered client points a node's `parent_id` at
/// a root the mover may write, with no list update to match, exactly what
/// would otherwise launder the node into a scope its real list never put
/// it in. A member who may only read the folder that still lists the node
/// is refused moving it, even though the tampered `parent_id` alone would
/// place the node in a root they may write.
#[tokio::test]
async fn a_move_is_refused_when_the_list_naming_the_node_is_a_root_the_member_only_reads() {
    let s = SharedStore::start().await;
    let (store_id, root, shared, inside) = (s.store_id, s.root, s.shared, s.inside);
    let edited = s.admin.create_node(store_id, Some(root), "folder", "Edited").await.unwrap();
    // Connected before the tamper, so the JWT/JWKS round trip a fresh
    // client needs is not part of the window below: without a share marker
    // on either folder (this store is scoped by JWT roots, not
    // `custom["share"]`), general repair treats a bare `parent_id` as the
    // truth and would relist the node itself once its 250ms debounce
    // fires, which is a real and separate behaviour this test is not
    // about — the write judgement below is.
    let member = s.member("gail", &[(shared, "reader"), (edited, "editor")]).await;

    // The tamper: `inside`'s own document gets a `parent_id` naming
    // `edited`, a root this principal may write, with `shared`'s list left
    // exactly as it was — what a client that does not keep
    // docs/MOVE_CONTRACT.md's "list wins" rule would write.
    let bytes = s.admin.get_node(store_id, inside).await.unwrap().content;
    let mut doc = pimble_crdt::NodeDoc::load(&bytes).unwrap();
    let before_sv = doc.state_vector();
    doc.set_parent_id(Some(edited), &chrono::Utc::now().to_rfc3339()).unwrap();
    let diff = doc.diff_since(&before_sv).unwrap();
    s.admin
        .apply_edit(
            store_id,
            inside,
            "tamperer",
            pimble_rpc::EditOperation::IncrementalChanges { changes: base64::engine::general_purpose::STANDARD.encode(diff) },
        )
        .await
        .expect("the merge itself is unconditional; only the write judgement below is what this test checks");

    // A member who reads `shared` and edits `edited`: the tampered
    // `parent_id` alone would place `inside` in `edited`'s write scope, but
    // `shared`'s list is the one the move actually leaves, and that is
    // theirs to read only. Called at once, with nothing else awaited in
    // between, to land inside the window above.
    refused_as_read_only(member.move_node(store_id, inside, edited, None).await, "shared's list, not the tampered parent_id, is judged");
    assert_eq!(
        s.admin.get_children(store_id, shared).await.unwrap().1.iter().map(|n| n.id).collect::<Vec<_>>(),
        vec![inside],
        "the refused move left shared's own list exactly as it was"
    );
}
