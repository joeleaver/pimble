//! End-to-end tests for the encrypting vault link (docs/CRYPTO_CONTRACT.md
//! "Desktop (E, after B): sign-in and the encrypting link").
//!
//! Three real processes' worth of state, all in-process:
//! - `H`, a real `PimbleServer` in JWT mode, holding the hosted `Vault`
//!   twin(s) of whatever a test hosts.
//! - A small axum stub standing in for the Pimble Cloud accounts service
//!   (`crates/pimble-cloud`), serving exactly the endpoints
//!   `crate::cloud`/`crate::handler`'s `cloud*` RPCs call: `GET /kdf`,
//!   `POST /login`, `GET /me/keys`, `POST /token`, `GET`/`POST /stores`,
//!   `GET`/`PUT /stores/{id}/keys`, plus its own JWKS endpoint (the same
//!   Ed25519 key `POST /token` signs with) so `H`'s `JwtVerifier` can check
//!   the minted tokens. One fixed test account throughout: real
//!   `pimble-crypto` output (KDF params shrunk for test speed, a real
//!   account keypair, a real `AccountKeyBlob` wrapped under the password's
//!   real KEK), so every `cloudSignIn` on every local server in a test
//!   genuinely derives the same keys the real client-side flow would.
//! - One or more real `PimbleServer`s in plain, tokenless, loopback mode —
//!   the "desktop" side under test — each with its own temp keystore,
//!   credentials file and replicas directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreId, StoreKind};
use pimble_crdt::ContentDoc;
use pimble_crypto::{derive_password_keys, wrap_account_keys, AccountKeyBlob, AccountKeys, KdfParams, KeyEnvelope};
use pimble_rpc::{EditOperation, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;
use uuid::Uuid;

// ── Small generic helpers (mirrors tests/sync.rs) ─────────────────────────

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

async fn node_text(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    match client.get_node(store_id, node_id).await {
        Ok(node) => ContentDoc::text_of(&node.content),
        Err(_) => String::new(),
    }
}

async fn seed_content(client: &PimbleClient, store_id: StoreId, node_id: NodeId, client_id: &str, text: &str) {
    let doc = ContentDoc::from_plain_text(text).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.save());
    client
        .apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes })
        .await
        .expect("seed content applies");
}

/// A fresh local desktop-side `PimbleServer` (plain, tokenless, loopback),
/// each with its own temp keystore/credentials/replicas paths so no test
/// ever touches the real config or data directory.
async fn start_local_server() -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        keystore_path: Some(dir.path().join("keys.json")),
        credentials_path: Some(dir.path().join("credentials.json")),
        replicas_dir: Some(dir.path().join("replicas")),
        ..Default::default()
    });
    server.start().await.expect("local server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client, dir)
}

// ── JWT signing (mirrors tests/auth.rs's local hand-signed EdDSA JWTs) ────

fn fresh_signing_key() -> SigningKey {
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

fn make_jwt(signing_key: &SigningKey, kid: &str, issuer: &str, sub: &str, email: &str, stores: &HashMap<String, String>) -> String {
    let header = json!({ "alg": "EdDSA", "kid": kid });
    let stores_claim: serde_json::Map<String, serde_json::Value> = stores.iter().map(|(id, role)| (id.clone(), json!(role))).collect();
    let payload = json!({
        "iss": issuer,
        "sub": sub,
        "aud": "pimble",
        "exp": chrono::Utc::now().timestamp() + 300,
        "claims": { "email": email, "stores": stores_claim },
    });
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature = signing_key.sign(signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());
    format!("{signing_input}.{sig_b64}")
}

// ── The accounts-service stub ──────────────────────────────────────────────
//
// Covers exactly the surface `crate::cloud` calls. No auth enforcement on
// the stub's own endpoints (the code under test is the pimble-server side,
// not this stand-in); a single fixed account throughout.

struct StubStoreRow {
    store_id: String,
    name: String,
    kind: String,
    created_at: String,
}

struct KeyGrantRow {
    user_id: String,
    key_id: String,
    envelope: KeyEnvelope,
}

struct StubInner {
    kdf: KdfParams,
    user_id: String,
    email: String,
    session: String,
    account_key_blob: AccountKeyBlob,
    account_public_keys: pimble_crypto::AccountPublicKeys,
    stores: Vec<StubStoreRow>,
    key_grants: HashMap<String, Vec<KeyGrantRow>>,
    signing_key: SigningKey,
    kid: String,
    issuer: String,
    /// Wired in once `H` has started (its address is only known after this
    /// stub is already listening, since `H`'s `jwks_url` points back at it).
    h_client: Option<Arc<PimbleClient>>,
    h_rpc_url: String,
    h_stores_dir: PathBuf,
}

type StubState = Arc<Mutex<StubInner>>;

fn stub_router(state: StubState) -> Router {
    Router::new()
        .route("/api/v1/kdf", get(stub_kdf))
        .route("/api/v1/login", post(stub_login))
        .route("/api/v1/me/keys", get(stub_me_keys))
        .route("/api/v1/token", post(stub_mint_token))
        .route("/api/v1/stores", get(stub_list_stores).post(stub_create_store))
        .route("/api/v1/stores/{store_id}/keys", get(stub_get_keys).put(stub_put_keys))
        .route("/jwks.json", get(stub_jwks))
        .with_state(state)
}

async fn stub_kdf(State(state): State<StubState>) -> Json<KdfParams> {
    Json(state.lock().unwrap().kdf.clone())
}

async fn stub_login(State(state): State<StubState>) -> Json<serde_json::Value> {
    let s = state.lock().unwrap();
    Json(json!({ "user": { "id": s.user_id, "email": s.email }, "session": s.session, "token": "", "exp": 0 }))
}

async fn stub_me_keys(State(state): State<StubState>) -> Json<serde_json::Value> {
    let s = state.lock().unwrap();
    Json(json!({ "public_keys": s.account_public_keys, "kdf": s.kdf, "account_key_blob": s.account_key_blob }))
}

async fn stub_mint_token(State(state): State<StubState>) -> Json<serde_json::Value> {
    let (token, rpc_url) = {
        let s = state.lock().unwrap();
        let stores_claim: HashMap<String, String> = s.stores.iter().map(|st| (st.store_id.clone(), "owner".to_string())).collect();
        let token = make_jwt(&s.signing_key, &s.kid, &s.issuer, &s.user_id, &s.email, &stores_claim);
        (token, s.h_rpc_url.clone())
    };
    Json(json!({ "token": token, "exp": chrono::Utc::now().timestamp() + 300, "rpc_url": rpc_url }))
}

async fn stub_list_stores(State(state): State<StubState>) -> Json<serde_json::Value> {
    let s = state.lock().unwrap();
    let arr: Vec<_> = s
        .stores
        .iter()
        .map(|st| json!({ "store_id": st.store_id, "name": st.name, "role": "owner", "kind": st.kind, "created_at": st.created_at }))
        .collect();
    Json(json!(arr))
}

async fn stub_create_store(State(state): State<StubState>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let name = req["name"].as_str().unwrap_or("store").to_string();
    let kind_str = req["kind"].as_str().unwrap_or("plain").to_string();
    let kind = if kind_str == "vault" { StoreKind::Vault } else { StoreKind::Plain };
    let requested_id = req["store_id"].as_str().map(|s| StoreId::parse(s).expect("valid store id"));

    let (h_client, stores_dir) = {
        let s = state.lock().unwrap();
        (Arc::clone(s.h_client.as_ref().expect("H client wired before any /stores call")), s.h_stores_dir.clone())
    };
    let dir_name = requested_id.map(|id| id.to_string()).unwrap_or_else(|| Uuid::new_v4().to_string());
    let path = stores_dir.join(format!("{dir_name}.pimble"));
    let (created_id, _root) = h_client.create_store_with(&path, &name, kind, requested_id).await.expect("H creates the hosted store");

    let created_at = chrono::Utc::now().to_rfc3339();
    {
        let mut s = state.lock().unwrap();
        s.stores.push(StubStoreRow { store_id: created_id.to_string(), name: name.clone(), kind: kind_str.clone(), created_at: created_at.clone() });
    }

    Json(json!({ "store_id": created_id.to_string(), "name": name, "role": "owner", "kind": kind_str, "created_at": created_at }))
}

async fn stub_get_keys(State(state): State<StubState>, AxPath(store_id): AxPath<String>) -> Json<serde_json::Value> {
    let s = state.lock().unwrap();
    let envelopes: Vec<_> = s
        .key_grants
        .get(&store_id)
        .map(|rows| rows.iter().map(|g| json!({ "key_id": g.key_id, "envelope": g.envelope })).collect())
        .unwrap_or_default();
    Json(json!({ "envelopes": envelopes }))
}

async fn stub_put_keys(State(state): State<StubState>, AxPath(store_id): AxPath<String>, Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.lock().unwrap();
    let entries = s.key_grants.entry(store_id).or_default();
    for env in body["envelopes"].as_array().cloned().unwrap_or_default() {
        let user_id = env["user_id"].as_str().unwrap().to_string();
        let key_id = env["key_id"].as_str().unwrap().to_string();
        let envelope: KeyEnvelope = serde_json::from_value(env["envelope"].clone()).unwrap();
        entries.retain(|g| !(g.user_id == user_id && g.key_id == key_id));
        entries.push(KeyGrantRow { user_id, key_id, envelope });
    }
    Json(json!({}))
}

async fn stub_jwks(State(state): State<StubState>) -> Json<serde_json::Value> {
    let s = state.lock().unwrap();
    let x = URL_SAFE_NO_PAD.encode(s.signing_key.verifying_key().to_bytes());
    Json(json!({ "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": s.kid, "x": x } ] }))
}

/// A cheap-to-derive `KdfParams`: same shape the contract's real 32 MiB/t=3
/// costs use, shrunk so `derive_password_keys` (both in this test, and every
/// time `cloudSignIn` runs it against this stub) takes milliseconds instead
/// of hundreds of them.
fn cheap_kdf_params() -> KdfParams {
    let mut params = KdfParams::generate();
    params.m_cost = 8 * 1024;
    params.t_cost = 1;
    params
}

/// Everything a test needs: the running `H` server, its stub, one fixed
/// account's credentials, and the vault directory `H` creates hosted stores
/// under (for grepping for plaintext leaks).
struct Env {
    stub_url: String,
    email: String,
    password: String,
    // Kept alive for the test's duration (dropping it would stop H);
    // never otherwise read.
    _h: PimbleServer,
    /// A client to `H` with the service token, for out-of-band verification
    /// (`vaultListDocs`/`vaultFetch`) a real device would never call this way.
    h_admin: PimbleClient,
    h_stores_dir: PathBuf,
    _h_dir: tempfile::TempDir,
}

impl Env {
    fn h_store_path(&self, store_id: StoreId) -> PathBuf {
        self.h_stores_dir.join(format!("{store_id}.pimble"))
    }

    /// Whether any file under a hosted store's `vault/` directory contains
    /// `needle` — the check that the plaintext never reaches `H`'s disk.
    fn hosted_store_contains_plaintext(&self, store_id: StoreId, needle: &str) -> bool {
        let vault_dir = self.h_store_path(store_id).join("vault");
        contains_bytes_recursive(&vault_dir, needle.as_bytes())
    }
}

fn contains_bytes_recursive(dir: &Path, needle: &[u8]) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if contains_bytes_recursive(&path, needle) {
                return true;
            }
        } else if let Ok(bytes) = std::fs::read(&path) {
            if bytes.windows(needle.len()).any(|w| w == needle) {
                return true;
            }
        }
    }
    false
}

async fn spawn_env() -> Env {
    let email = "alice@example.com".to_string();
    let password = "correct horse battery staple".to_string();
    let user_id = Uuid::new_v4().to_string();
    let session = format!("sess-{}", Uuid::new_v4());
    let kdf = cheap_kdf_params();
    let account_keys = AccountKeys::generate();
    let password_keys = derive_password_keys(&password, &kdf).expect("derive password keys");
    let account_key_blob = wrap_account_keys(&account_keys, &password_keys.kek).expect("wrap account keys");
    let account_public_keys = account_keys.public_keys();

    let signing_key = fresh_signing_key();
    let kid = "test-kid".to_string();
    let issuer = "https://cloud.test".to_string();

    let h_dir = tempfile::tempdir().unwrap();
    let h_stores_dir = h_dir.path().join("stores");
    std::fs::create_dir_all(&h_stores_dir).unwrap();

    let state: StubState = Arc::new(Mutex::new(StubInner {
        kdf,
        user_id,
        email: email.clone(),
        session,
        account_key_blob,
        account_public_keys,
        stores: Vec::new(),
        key_grants: HashMap::new(),
        signing_key,
        kid,
        issuer: issuer.clone(),
        h_client: None,
        h_rpc_url: String::new(),
        h_stores_dir: h_stores_dir.clone(),
    }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_addr = listener.local_addr().unwrap();
    let router = stub_router(Arc::clone(&state));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let stub_url = format!("http://{stub_addr}");
    let jwks_url = format!("{stub_url}/jwks.json");

    let h_service_token = "h-service-token".to_string();
    let mut h = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(h_service_token.clone()),
        jwks_url: Some(jwks_url.parse().unwrap()),
        jwt_issuer: Some(issuer),
        keystore_path: Some(h_dir.path().join("keys.json")),
        credentials_path: Some(h_dir.path().join("credentials.json")),
        replicas_dir: Some(h_dir.path().join("replicas")),
        ..Default::default()
    });
    h.start().await.expect("H starts");

    let h_url = format!("http://{}", h.addr());
    let h_client = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token.clone() })
        .await
        .expect("service client connects to H");
    let h_admin = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token })
        .await
        .expect("admin client connects to H");

    {
        let mut s = state.lock().unwrap();
        s.h_client = Some(Arc::new(h_client));
        s.h_rpc_url = format!("ws://{}", h.addr());
    }

    Env { stub_url, email, password, _h: h, h_admin, h_stores_dir, _h_dir: h_dir }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn signing_in_persists_the_account_and_status_reflects_it() {
    let env = spawn_env().await;
    let (mut a, client_a, _a_dir) = start_local_server().await;

    let before = client_a.cloud_status().await.unwrap();
    assert!(!before.signed_in, "a fresh server must start signed out");

    client_a.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.expect("sign in succeeds");

    let after = client_a.cloud_status().await.unwrap();
    assert!(after.signed_in);
    assert_eq!(after.email.as_deref(), Some(env.email.as_str()));
    assert_eq!(after.url.as_deref(), Some(env.stub_url.as_str()));

    client_a.cloud_sign_out().await.unwrap();
    let signed_out = client_a.cloud_status().await.unwrap();
    assert!(!signed_out.signed_in);

    a.stop().await.unwrap();
}

#[tokio::test]
async fn hosting_a_store_seeds_it_as_ciphertext_and_local_edits_propagate() {
    let env = spawn_env().await;
    let (mut a, client_a, a_dir) = start_local_server().await;
    client_a.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.unwrap();

    let (store_id, root_id) = client_a.create_store(a_dir.path().join("a.pimble"), "Notes").await.unwrap();
    let doc_id = client_a.create_node(store_id, Some(root_id), "document", "Doc").await.unwrap();
    let marker = "PLAINTEXT-MARKER-9f31a7";
    seed_content(&client_a, store_id, doc_id, "seed", marker).await;

    let hosted_id = client_a.cloud_host_store(store_id).await.expect("hosting succeeds");
    assert_eq!(hosted_id, store_id, "the hosted twin must live under the same store id");

    // The vault link seeds both documents (tree, and the one node) since H
    // starts out with nothing.
    let seeded = wait_until(Duration::from_secs(10), || async {
        env.h_admin.vault_list_docs(store_id).await.map(|docs| docs.iter().any(|d| d.head > 0)).unwrap_or(false)
    })
    .await;
    assert!(seeded, "hosting a store with existing content must upload it");

    assert!(
        !env.hosted_store_contains_plaintext(store_id, marker),
        "the plaintext must never reach the hosted server's disk"
    );

    // A live local edit reaches the vault too.
    let marker2 = "SECOND-MARKER-b81c2";
    seed_content(&client_a, store_id, doc_id, "editor", marker2).await;
    let doc_key = VaultDocId::Node(doc_id);
    let updated = wait_until(Duration::from_secs(10), || async {
        env.h_admin.vault_fetch(store_id, doc_key.clone(), 0).await.map(|f| f.head >= 2).unwrap_or(false)
    })
    .await;
    assert!(updated, "a live edit must be appended to the hosted document");
    assert!(!env.hosted_store_contains_plaintext(store_id, marker2), "the second edit's plaintext must never reach disk either");

    a.stop().await.unwrap();
}

#[tokio::test]
async fn a_second_server_receives_content_and_a_reply_edit_reaches_the_first() {
    let env = spawn_env().await;

    let (mut a, client_a, a_dir) = start_local_server().await;
    client_a.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.unwrap();
    let (store_id, root_id) = client_a.create_store(a_dir.path().join("a.pimble"), "Shared").await.unwrap();
    let doc_id = client_a.create_node(store_id, Some(root_id), "document", "Doc").await.unwrap();
    seed_content(&client_a, store_id, doc_id, "seed", "from A").await;
    client_a.cloud_host_store(store_id).await.unwrap();

    let doc_key = VaultDocId::Node(doc_id);
    let seeded = wait_until(Duration::from_secs(10), || async {
        env.h_admin.vault_fetch(store_id, doc_key.clone(), 0).await.map(|f| f.head > 0).unwrap_or(false)
    })
    .await;
    assert!(seeded, "A's content must reach the vault before B can pull it");

    let (mut b, client_b, _b_dir) = start_local_server().await;
    client_b.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.unwrap();
    let store_on_b = client_b.cloud_add_hosted_store(store_id).await.expect("B adds the hosted store");
    assert_eq!(store_on_b.id, store_id);

    let b_root = store_on_b.root_node_id;
    let (_, b_children) = wait_children_nonempty(&client_b, store_id, b_root).await;
    assert_eq!(b_children.len(), 1, "B must see A's one document node");
    let b_doc_id = b_children[0].id;

    let pulled = wait_until(Duration::from_secs(10), || async { node_text(&client_b, store_id, b_doc_id).await.contains("from A") }).await;
    assert!(pulled, "B must receive A's content through the hosted vault");

    // B edits; A must see it.
    seed_content(&client_b, store_id, b_doc_id, "b-editor", "from B").await;
    let reached_a = wait_until(Duration::from_secs(10), || async { node_text(&client_a, store_id, doc_id).await.contains("from B") }).await;
    assert!(reached_a, "an edit on B must reach A through the hosted vault");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

/// Regression test for the bug found in the web vault client: echo
/// suppression there dropped any incoming `VaultAppended` whose seq wasn't
/// above the client's own last-appended seq, which is wrong whenever a peer
/// races ahead while this side keeps appending too (its own seq is not
/// reliably the highest). `crate::vault_link::EchoTracker` suppresses by
/// identity instead (the exact `(doc_id, seq)` pairs this link itself
/// appended, an `EchoTracker`/`HashSet`, not a `>=` comparison against
/// `last_seq`) — this fires many interleaved appends from two servers with
/// no waiting in between, so their vault appends race each other, and
/// checks both sides still converge to every edit.
#[tokio::test]
async fn interleaved_edits_from_two_servers_on_one_hosted_document_converge() {
    let env = spawn_env().await;

    let (mut a, client_a, a_dir) = start_local_server().await;
    client_a.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.unwrap();
    let (store_id, root_id) = client_a.create_store(a_dir.path().join("a.pimble"), "Interleave").await.unwrap();
    let doc_id = client_a.create_node(store_id, Some(root_id), "document", "Doc").await.unwrap();
    seed_content(&client_a, store_id, doc_id, "seed", "start").await;
    client_a.cloud_host_store(store_id).await.unwrap();

    let doc_key = VaultDocId::Node(doc_id);
    let seeded = wait_until(Duration::from_secs(10), || async {
        env.h_admin.vault_fetch(store_id, doc_key.clone(), 0).await.map(|f| f.head > 0).unwrap_or(false)
    })
    .await;
    assert!(seeded, "A's content must reach the vault before B can pull it");

    let (mut b, client_b, _b_dir) = start_local_server().await;
    client_b.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.unwrap();
    let store_on_b = client_b.cloud_add_hosted_store(store_id).await.expect("B adds the hosted store");
    let (_, b_children) = wait_children_nonempty(&client_b, store_id, store_on_b.root_node_id).await;
    assert_eq!(b_children.len(), 1);
    let b_doc_id = b_children[0].id;
    let pulled = wait_until(Duration::from_secs(10), || async { node_text(&client_b, store_id, b_doc_id).await.contains("start") }).await;
    assert!(pulled, "B must receive A's seed content before the interleaving starts");

    // Fire edits from both sides in strict alternation with no waiting for
    // propagation in between, so each side's `vaultAppend`s race the
    // other's live `VaultAppended` notifications arriving through its own
    // subscription — exactly the scenario a seq-threshold echo check gets
    // wrong (this side's own seq is not reliably the highest once the other
    // side is also appending concurrently).
    let markers: [(&str, bool); 8] = [
        ("alpha-from-a", true),
        ("bravo-from-b", false),
        ("charlie-from-a", true),
        ("delta-from-b", false),
        ("echo-from-a", true),
        ("foxtrot-from-b", false),
        ("golf-from-a", true),
        ("hotel-from-b", false),
    ];
    for (marker, from_a) in &markers {
        if *from_a {
            seed_content(&client_a, store_id, doc_id, "a-editor", marker).await;
        } else {
            seed_content(&client_b, store_id, b_doc_id, "b-editor", marker).await;
        }
    }

    let converged_a = wait_until(Duration::from_secs(15), || async {
        let text = node_text(&client_a, store_id, doc_id).await;
        markers.iter().all(|(m, _)| text.contains(m))
    })
    .await;
    assert!(converged_a, "A must end up with every edit from both sides, not just its own");

    let converged_b = wait_until(Duration::from_secs(15), || async {
        let text = node_text(&client_b, store_id, b_doc_id).await;
        markers.iter().all(|(m, _)| text.contains(m))
    })
    .await;
    assert!(converged_b, "B must end up with every edit from both sides, not just its own");

    assert_eq!(
        node_text(&client_a, store_id, doc_id).await,
        node_text(&client_b, store_id, b_doc_id).await,
        "both sides must converge to byte-identical text"
    );

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

/// Poll `getChildren` until it's non-empty (the tree pull may lag a beat
/// behind the vault link reaching `Synced`).
async fn wait_children_nonempty(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> (StoreId, Vec<pimble_core::Node>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok((sid, children)) = client.get_children(store_id, node_id).await {
            if !children.is_empty() {
                return (sid, children);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return (store_id, Vec::new());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_snapshot_is_uploaded_after_200_updates_and_a_fresh_server_reads_it() {
    let env = spawn_env().await;
    let (mut a, client_a, a_dir) = start_local_server().await;
    client_a.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.unwrap();
    let (store_id, root_id) = client_a.create_store(a_dir.path().join("a.pimble"), "Busy").await.unwrap();
    let doc_id = client_a.create_node(store_id, Some(root_id), "document", "Doc").await.unwrap();
    client_a.cloud_host_store(store_id).await.unwrap();

    let seeded = wait_until(Duration::from_secs(10), || async { env.h_admin.vault_list_docs(store_id).await.map(|d| !d.is_empty()).unwrap_or(false) }).await;
    assert!(seeded, "hosting must seed at least the tree document");

    // 200 more appends to the node document (the seed above was already
    // one). Small, distinct texts so a merge failure would be obvious.
    for i in 0..200 {
        seed_content(&client_a, store_id, doc_id, "editor", &format!("edit {i}")).await;
    }

    let doc_key = VaultDocId::Node(doc_id);
    let snapshotted = wait_until(Duration::from_secs(20), || async {
        env.h_admin
            .vault_list_docs(store_id)
            .await
            .map(|docs| docs.iter().any(|d| d.doc_id == doc_key && d.snapshot_seq > 0))
            .unwrap_or(false)
    })
    .await;
    assert!(snapshotted, "a snapshot must be uploaded once the document's head reaches a multiple of 200");

    // A fresh server (a third device on the same account) reads the
    // document straight from the snapshot rather than replaying the log.
    let (mut c, client_c, _c_dir) = start_local_server().await;
    client_c.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.unwrap();
    let store_on_c = client_c.cloud_add_hosted_store(store_id).await.expect("C adds the hosted store");
    let (_, c_children) = wait_children_nonempty(&client_c, store_id, store_on_c.root_node_id).await;
    assert_eq!(c_children.len(), 1);
    let c_doc_id = c_children[0].id;

    let has_last_edit = wait_until(Duration::from_secs(10), || async { node_text(&client_c, store_id, c_doc_id).await.contains("edit 199") }).await;
    assert!(has_last_edit, "a fresh server must recover the full content via the snapshot");

    a.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn restarting_a_server_resumes_the_vault_link_without_losing_or_duplicating_content() {
    let env = spawn_env().await;
    let (mut a, client_a, a_dir) = start_local_server().await;
    client_a.cloud_sign_in(&env.stub_url, &env.email, &env.password).await.unwrap();
    let a_path = a_dir.path().join("a.pimble");
    let (store_id, root_id) = client_a.create_store(&a_path, "Restart").await.unwrap();
    let doc_id = client_a.create_node(store_id, Some(root_id), "document", "Doc").await.unwrap();
    seed_content(&client_a, store_id, doc_id, "seed", "before restart").await;
    client_a.cloud_host_store(store_id).await.unwrap();

    let seeded = wait_until(Duration::from_secs(10), || async { node_text(&client_a, store_id, doc_id).await.contains("before restart") }).await;
    assert!(seeded);
    // Let the link fully settle so `last_seq` is persisted before we stop it.
    tokio::time::sleep(Duration::from_millis(300)).await;

    a.stop().await.unwrap();

    // A fresh `PimbleServer` instance, same store directory, same keystore
    // and credentials paths (as a restart of the same desktop app would
    // use) — `openStore` should restart the vault link from `sync.json`.
    let dir = a_dir.path();
    let mut a2 = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        keystore_path: Some(dir.join("keys.json")),
        credentials_path: Some(dir.join("credentials.json")),
        replicas_dir: Some(dir.join("replicas")),
        ..Default::default()
    });
    a2.start().await.expect("restarted server starts");
    let client_a2 = PimbleClient::connect(format!("http://{}", a2.addr())).await.expect("client connects to restarted server");

    let opened = client_a2.open_store(&a_path).await.expect("reopening the store succeeds");
    assert_eq!(opened.id, store_id);
    // Content survived the restart intact (not lost, and — since the CRDT
    // merge is idempotent either way — this alone doesn't prove `last_seq`
    // was honored, but a corrupted/duplicated merge would still fail it).
    assert_eq!(node_text(&client_a2, store_id, doc_id).await, "before restart");

    // The link resumed live: a further edit still reaches the hosted vault.
    seed_content(&client_a2, store_id, doc_id, "editor", "after restart").await;
    let propagated = wait_until(Duration::from_secs(10), || async { node_text(&client_a2, store_id, doc_id).await.contains("after restart") }).await;
    assert!(propagated);

    a2.stop().await.unwrap();
}

// ── Interop with the REAL accounts service (crates/pimble-cloud) ─────────
//
// Everything above drives an axum stub. This test instead spawns the real
// `rhypedb-server` and `pimble-cloud` binaries and walks the whole flow
// against them: a real signup (built with real `pimble-crypto` output),
// following the real verification link `pimble-cloud`'s `LogMailer` logs,
// `cloudSignIn`, `cloudHostStore`, a second device's `cloudAddHostedStore`,
// edits each way, and a check that the hosted server's disk never holds the
// plaintext. Skips itself cleanly (prints why on stderr) if either real
// binary isn't available, the same way `pimble-cloud`'s own
// `tests/integration.rs` skips without `rhypedb-server`.

use std::process::{Child, Command, Stdio};

/// Kills the wrapped child on drop, so a panicking assertion never leaves
/// `rhypedb-server`/`pimble-cloud` running after the test ends.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The minimal stand-in for jkbase's edge proxy (see the caller's comment):
/// every connection to `listen_port` is routed, by peeking its HTTP request
/// line, to `h_port` when the path starts with `/rpc` (a WebSocket upgrade,
/// for the vault RPCs) or to `cloud_port` otherwise (every plain HTTP call
/// this test or `pimble_server::cloud` makes). Once routed, the connection
/// is spliced byte-for-byte in both directions — this never parses HTTP
/// beyond that first line, so it works for both a WebSocket upgrade and an
/// ordinary request/response.
async fn run_edge_proxy(listen_port: u16, cloud_port: u16, h_port: u16) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", listen_port)).await.expect("proxy binds");
    loop {
        let Ok((mut inbound, _)) = listener.accept().await else { continue };
        tokio::spawn(async move {
            let mut peek_buf = [0u8; 4096];
            let n = match inbound.peek(&mut peek_buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let head = String::from_utf8_lossy(&peek_buf[..n]);
            let is_rpc = head.starts_with("GET /rpc") || head.starts_with("POST /rpc");
            let target_port = if is_rpc { h_port } else { cloud_port };
            let Ok(mut outbound) = tokio::net::TcpStream::connect(("127.0.0.1", target_port)).await else { return };
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

fn find_rhypedb_server() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RHYPEDB_SERVER_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let home = dirs::home_dir()?;
    for rel in ["dev/rhypedb/target/release/rhypedb-server", "dev/rhypedb/target/debug/rhypedb-server"] {
        let p = home.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// `pimble-cloud` is a workspace member, not an external checkout: look for
/// an already-built binary next to this crate's own target directory
/// (`cargo build -p pimble-cloud --release` builds it there) rather than
/// building it from inside this test.
fn find_pimble_cloud_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PIMBLE_CLOUD_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?.to_path_buf();
    for rel in ["target/release/pimble-cloud", "target/debug/pimble-cloud"] {
        let p = workspace_root.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// The verification link `pimble-cloud`'s `LogMailer` writes at `info`
/// (`RUST_LOG=pimble_cloud=info`, set on the child below) — polls the
/// captured log file until the link appears.
async fn wait_for_verify_url(log_path: &Path, timeout: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(contents) = std::fs::read_to_string(log_path) {
            if let Some(start) = contents.find("/api/v1/verify?token=") {
                let rest = &contents[start..];
                let end = rest.find(|c: char| c.is_whitespace() || c == '"' || c == '\\').unwrap_or(rest.len());
                return Some(rest[..end].to_string());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn interop_with_the_real_accounts_service() {
    let Some(rhypedb_bin) = find_rhypedb_server() else {
        eprintln!(
            "SKIPPING interop_with_the_real_accounts_service: no rhypedb-server binary found \
             (set RHYPEDB_SERVER_BIN, or build ~/dev/rhypedb in release or debug mode)"
        );
        return;
    };
    let Some(cloud_bin) = find_pimble_cloud_binary() else {
        eprintln!(
            "SKIPPING interop_with_the_real_accounts_service: no pimble-cloud binary found \
             (set PIMBLE_CLOUD_BIN, or `cargo build -p pimble-cloud --release`)"
        );
        return;
    };

    let rhypedb_data_dir = tempfile::tempdir().unwrap();
    let rhypedb_http_port = free_port();
    let rhypedb_tcp_port = free_port();
    let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("pimble-cloud").join("schema.rhype");

    let _rhypedb = ChildGuard(
        Command::new(&rhypedb_bin)
            .args([
                "--schema",
                schema_path.to_str().unwrap(),
                "--data-dir",
                rhypedb_data_dir.path().to_str().unwrap(),
                "--listen",
                &format!("127.0.0.1:{rhypedb_http_port}"),
                "--tcp-listen",
                &format!("127.0.0.1:{rhypedb_tcp_port}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("rhypedb-server spawns"),
    );

    let rhypedb_ready = wait_until(Duration::from_secs(15), || async {
        reqwest::get(format!("http://127.0.0.1:{rhypedb_http_port}/health")).await.map(|r| r.status().is_success()).unwrap_or(false)
    })
    .await;
    assert!(rhypedb_ready, "rhypedb-server must come up within 15s");

    // H, the hosted Pimble server: real, in-process, JWT-verified against
    // pimble-cloud's own dev-mode JWKS (its issuer is fixed by pimble-cloud
    // to `{public_url}/api/v1`).
    let h_dir = tempfile::tempdir().unwrap();
    let h_token = "h-service-token".to_string();

    // pimble-cloud computes `rpc_url` (`mint_token`) and its verification
    // link from `PIMBLE_CLOUD_PUBLIC_URL` alone, on the assumption that in
    // production jkbase's edge proxies that one public origin's `/rpc` to
    // the hosted Pimble server and everything else to this service (see the
    // crate's README, "Layout on jkbase"). Locally there is no such edge, so
    // this test stands up the minimal version of it: a raw TCP proxy in
    // front of one public port, routing `/rpc*` to `H` and everything else
    // to the real `pimble-cloud` process on its own internal port. Without
    // this, `rpc_url` would point at pimble-cloud's own HTTP port, which
    // serves no such route at all — exactly the gap the README calls out.
    let cloud_internal_port = free_port();
    let cloud_internal_url = format!("http://127.0.0.1:{cloud_internal_port}");
    let proxy_port = free_port();
    let public_url = format!("http://127.0.0.1:{proxy_port}");
    let issuer = format!("{public_url}/api/v1");
    let jwks_url = format!("{issuer}/.well-known/jwks.json");

    let mut h = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(h_token.clone()),
        jwks_url: Some(jwks_url.parse().unwrap()),
        jwt_issuer: Some(issuer),
        keystore_path: Some(h_dir.path().join("keys.json")),
        credentials_path: Some(h_dir.path().join("credentials.json")),
        replicas_dir: Some(h_dir.path().join("replicas")),
        ..Default::default()
    });
    h.start().await.expect("H starts");
    let h_addr = h.addr();
    let h_url = format!("http://{h_addr}");
    let h_admin = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_token.clone() }).await.expect("admin client connects to H");

    let cloud_stores_dir = tempfile::tempdir().unwrap();
    let cloud_log_path = h_dir.path().join("cloud.log");
    let cloud_log_file = std::fs::File::create(&cloud_log_path).unwrap();
    let cloud_log_file_err = cloud_log_file.try_clone().unwrap();
    let _cloud = ChildGuard(
        Command::new(&cloud_bin)
            .env("RHYPEDB_ADDR", format!("127.0.0.1:{rhypedb_tcp_port}"))
            .env("PIMBLE_SERVER_URL", &h_url)
            .env("PIMBLE_SERVER_TOKEN", &h_token)
            .env("PIMBLE_STORES_DIR", cloud_stores_dir.path())
            // 32 bytes hex (README): two v4 UUIDs' 16-byte simple forms concatenated.
            .env("PIMBLE_CLOUD_DEV_SIGNING_SEED", format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()))
            .env("PIMBLE_CLOUD_PUBLIC_URL", &public_url)
            .env("PORT", cloud_internal_port.to_string())
            .env("RUST_LOG", "pimble_cloud=info")
            .stdout(Stdio::from(cloud_log_file))
            .stderr(Stdio::from(cloud_log_file_err))
            .spawn()
            .expect("pimble-cloud spawns"),
    );

    let cloud_ready = wait_until(Duration::from_secs(15), || async {
        reqwest::get(format!("{cloud_internal_url}/api/v1/health")).await.map(|r| r.status().is_success()).unwrap_or(false)
    })
    .await;
    assert!(cloud_ready, "pimble-cloud must come up within 15s (see {:?})", cloud_log_path);

    tokio::spawn(run_edge_proxy(proxy_port, cloud_internal_port, h_addr.port()));
    let cloud_url = public_url;
    let proxy_ready = wait_until(Duration::from_secs(5), || async {
        reqwest::get(format!("{cloud_url}/api/v1/health")).await.map(|r| r.status().is_success()).unwrap_or(false)
    })
    .await;
    assert!(proxy_ready, "the local edge proxy must come up within 5s");

    // A real signup, with real pimble-crypto output.
    let email = format!("interop-{}@example.com", Uuid::new_v4());
    let password = "correct horse battery staple";
    let kdf = cheap_kdf_params();
    let password_keys = derive_password_keys(password, &kdf).unwrap();
    let auth_key = pimble_crypto::encode_auth_key(&password_keys.auth_key);
    let account_keys = AccountKeys::generate();
    let public_keys = account_keys.public_keys();
    let account_key_blob = wrap_account_keys(&account_keys, &password_keys.kek).unwrap();
    let recovery_kdf = cheap_kdf_params();
    let recovery_code = pimble_crypto::generate_recovery_code();
    let recovery_kek = pimble_crypto::derive_recovery_kek(&recovery_code, &recovery_kdf).unwrap();
    let recovery_key_blob = wrap_account_keys(&account_keys, &recovery_kek).unwrap();

    let http = reqwest::Client::new();
    let signup_resp = http
        .post(format!("{cloud_url}/api/v1/signup"))
        .json(&json!({
            "email": email,
            "auth_key": auth_key,
            "kdf": kdf,
            "public_keys": public_keys,
            "account_key_blob": account_key_blob,
            "recovery_salt": recovery_kdf.salt,
            "recovery_key_blob": recovery_key_blob,
        }))
        .send()
        .await
        .expect("signup request sends");
    assert_eq!(signup_resp.status(), 202, "signup must accept a well-formed real body");

    let verify_url = wait_for_verify_url(&cloud_log_path, Duration::from_secs(5))
        .await
        .expect("the verification link must appear in pimble-cloud's log (LogMailer, RUST_LOG=pimble_cloud=info)");
    let no_redirect = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let verify_resp = no_redirect.get(format!("{cloud_url}{verify_url}")).send().await.expect("verify link is reachable");
    assert_eq!(verify_resp.status(), 303);
    let location = verify_resp.headers().get("location").unwrap().to_str().unwrap().to_string();
    assert!(location.contains("verified=1"), "verification must succeed, got redirect to {location}");

    // The desktop side: real `cloudSignIn`/`cloudHostStore` against the real service.
    let (mut a, client_a, a_dir) = start_local_server().await;
    client_a.cloud_sign_in(&cloud_url, &email, password).await.expect("cloudSignIn against the real service succeeds");
    let status = client_a.cloud_status().await.unwrap();
    assert!(status.signed_in);
    assert_eq!(status.email.as_deref(), Some(email.as_str()));

    let (store_id, root_id) = client_a.create_store(a_dir.path().join("a.pimble"), "Interop Notes").await.unwrap();
    let doc_id = client_a.create_node(store_id, Some(root_id), "document", "Doc").await.unwrap();
    let marker = "REAL-SERVICE-PLAINTEXT-MARKER-4471";
    seed_content(&client_a, store_id, doc_id, "seed", marker).await;

    let hosted_id = client_a.cloud_host_store(store_id).await.expect("cloudHostStore against the real service succeeds");
    assert_eq!(hosted_id, store_id);

    let doc_key = VaultDocId::Node(doc_id);
    let seeded = wait_until(Duration::from_secs(10), || async {
        h_admin.vault_fetch(store_id, doc_key.clone(), 0).await.map(|f| f.head > 0).unwrap_or(false)
    })
    .await;
    assert!(seeded, "hosting must upload the store's existing content (see {:?} for pimble-cloud's log)", cloud_log_path);
    assert!(
        !contains_bytes_recursive(cloud_stores_dir.path(), marker.as_bytes()),
        "the plaintext must never reach the real hosted server's disk"
    );

    // A second device (same account) receives the content and can edit back.
    let (mut b, client_b, _b_dir) = start_local_server().await;
    client_b.cloud_sign_in(&cloud_url, &email, password).await.expect("a second device signs in to the same account");
    let store_on_b = client_b.cloud_add_hosted_store(store_id).await.expect("cloudAddHostedStore against the real service succeeds");
    assert_eq!(store_on_b.id, store_id);

    let (_, b_children) = wait_children_nonempty(&client_b, store_id, store_on_b.root_node_id).await;
    assert_eq!(b_children.len(), 1);
    let b_doc_id = b_children[0].id;
    let pulled = wait_until(Duration::from_secs(10), || async { node_text(&client_b, store_id, b_doc_id).await.contains(marker) }).await;
    assert!(pulled, "the second device must receive the first device's content");

    seed_content(&client_b, store_id, b_doc_id, "b-editor", "edited from the second device").await;
    let reached_a = wait_until(Duration::from_secs(10), || async {
        node_text(&client_a, store_id, doc_id).await.contains("edited from the second device")
    })
    .await;
    assert!(reached_a, "an edit on the second device must reach the first through the real hosted vault");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    h.stop().await.unwrap();
}
