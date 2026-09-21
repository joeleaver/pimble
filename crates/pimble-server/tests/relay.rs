//! End-to-end tests for the relay tier (docs/RELAY_CONTRACT.md): a store that
//! is **not** hosted on Pimble Cloud, shared from its owner's computer.
//!
//! The environment is `tests/share.rs`'s, with the accounts-service stub
//! grown by what the relay tier adds to the real service (`pimble-cloud`'s
//! `src/relay.rs` and `routes/stores.rs` are the authority on the wire):
//!
//! - `POST /stores` with `tier: "relay"` records a row and calls nobody;
//!   `GET /stores` rows carry `tier`; `DELETE /stores/{id}` removes the
//!   record and withdraws the store from the relay.
//! - `POST /token` lists every relay-tier store the account holds a grant on
//!   in `stores`, each with its own endpoint and **a token cut down to that
//!   one store**, which is the only token the relay takes for it.
//! - The relay itself, in memory: the owner's tunnel at `/api/v1/relay`
//!   (session auth, `{"serve": [...]}` / `{"serving": [...]}`, binary
//!   `[conn u32 BE][kind u8][payload]` frames) and members' connections at
//!   `/api/v1/relay/<store id>` (4404 `owner offline` with no tunnel).
//!
//! `H`, the hosted Pimble server, is there only to be looked at: nothing a
//! test here does may put anything on it.
//!
//! What is real: every "desktop" `PimbleServer`, the relay face each owner's
//! server starts inside itself, the tunnel client, the vault links, the
//! share upkeep, and the JWT verification between all of them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, Verifier};
use futures::{SinkExt, StreamExt};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, RelaySide, StoreAccess, StoreId, StoreKind, SyncState};
use pimble_crdt::NodeDoc;
use pimble_crypto::{derive_password_keys, wrap_account_keys, AccountKeyBlob, AccountKeys, KdfParams, KeyEnvelope};
use pimble_rpc::{EditOperation, MemberRole, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use uuid::Uuid;

// ── Small generic helpers (mirrors tests/share.rs) ────────────────────────

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
        Ok(node) => NodeDoc::text_of(&node.content),
        Err(_) => String::new(),
    }
}

async fn node_title(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    client.get_node(store_id, node_id).await.map(|node| node.metadata.title).unwrap_or_default()
}

async fn child_ids(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> Vec<NodeId> {
    client.get_children(store_id, node_id).await.map(|(_, children)| children.into_iter().map(|n| n.id).collect()).unwrap_or_default()
}

async fn rename(client: &PimbleClient, store_id: StoreId, node_id: NodeId, title: &str) {
    let mut metadata = client.get_node(store_id, node_id).await.expect("the node to rename").metadata;
    metadata.title = title.to_string();
    client.update_node_metadata(store_id, node_id, metadata).await.expect("rename");
}

/// Text typed into a node: a fresh document's state merged in, which reads
/// as another paragraph.
async fn write_text(client: &PimbleClient, store_id: StoreId, node_id: NodeId, client_id: &str, text: &str) {
    let doc = NodeDoc::from_plain_text(text).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.save());
    client.apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes }).await.expect("the text applies");
}

fn sorted(ids: Vec<NodeId>) -> Vec<String> {
    let mut out: Vec<String> = ids.into_iter().map(|id| id.to_string()).collect();
    out.sort();
    out
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

/// How often a test device's key sweep runs, in place of the minute.
const SWEEP_EVERY: Duration = Duration::from_secs(1);

/// Every path a device writes to is inside its own temp directory: the
/// keystore, the credentials, the replicas, and the twins of what it shares
/// from "this computer".
fn device_config(dir: &Path) -> ServerConfig {
    ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        keystore_path: Some(dir.join("keys.json")),
        credentials_path: Some(dir.join("credentials.json")),
        replicas_dir: Some(dir.join("replicas")),
        relay_dir: Some(dir.join("relay")),
        share_sweep_interval: Some(SWEEP_EVERY),
        ..Default::default()
    }
}

async fn start_local_server() -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (server, client) = start_device_in(dir.path()).await;
    (server, client, dir)
}

/// A device started (or started again) over `dir`.
async fn start_device_in(dir: &Path) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(device_config(dir));
    server.start().await.expect("local server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

fn twin_dir(device_dir: &Path, store_id: StoreId) -> PathBuf {
    device_dir.join("relay").join(format!("{store_id}.pimble"))
}

/// The twin's epoch as it is on disk: its manifest's creation time.
fn twin_created_at(device_dir: &Path, store_id: StoreId) -> Option<String> {
    let manifest = std::fs::read(twin_dir(device_dir, store_id).join("manifest.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest).ok()?;
    manifest["created_at"].as_str().map(str::to_string)
}

/// How many documents the twin holds a log of.
fn twin_doc_count(device_dir: &Path, store_id: StoreId) -> usize {
    std::fs::read_dir(twin_dir(device_dir, store_id).join("vault")).map(|entries| entries.flatten().filter(|e| e.path().is_dir()).count()).unwrap_or(0)
}

// ── JWTs ──────────────────────────────────────────────────────────────────

fn fresh_signing_key() -> SigningKey {
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

fn make_jwt(signing_key: &SigningKey, kid: &str, issuer: &str, sub: &str, email: &str, stores_claim: &serde_json::Map<String, serde_json::Value>) -> String {
    let header = json!({ "alg": "EdDSA", "kid": kid });
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
    format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

/// The store ids a token's `stores` claim names, if its signature is the
/// stub's own.
fn verified_stores(signing_key: &SigningKey, token: &str) -> Option<Vec<String>> {
    let mut parts = token.split('.');
    let (header, payload, signature) = (parts.next()?, parts.next()?, parts.next()?);
    let signature = ed25519_dalek::Signature::from_slice(&URL_SAFE_NO_PAD.decode(signature).ok()?).ok()?;
    signing_key.verifying_key().verify(format!("{header}.{payload}").as_bytes(), &signature).ok()?;
    let payload: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    Some(payload["claims"]["stores"].as_object()?.keys().cloned().collect())
}

// ── The accounts-service stub ──────────────────────────────────────────────

/// One grant: `GET /stores` answers one row per grant of the caller's.
struct StubStoreRow {
    user_id: String,
    store_id: String,
    name: String,
    kind: String,
    tier: String,
    created_at: String,
    role: String,
    /// The shared node, for a share.
    root: Option<String>,
    shared_by: Option<String>,
}

struct KeyGrantRow {
    user_id: String,
    key_id: String,
    envelope: KeyEnvelope,
    root: Option<String>,
}

struct StubAccount {
    kdf: KdfParams,
    user_id: String,
    email: String,
    session: String,
    account_key_blob: AccountKeyBlob,
    account_public_keys: pimble_crypto::AccountPublicKeys,
}

struct StubInner {
    /// The owner first; a request with no session the stub knows is theirs.
    accounts: Vec<StubAccount>,
    stores: Vec<StubStoreRow>,
    key_grants: HashMap<String, Vec<KeyGrantRow>>,
    signing_key: SigningKey,
    kid: String,
    issuer: String,
    h_client: Option<Arc<PimbleClient>>,
    h_rpc_url: String,
    h_stores_dir: PathBuf,
    /// This stub's own address, which is where its relay is.
    own_addr: String,
    /// Every `POST /stores` body, as it arrived: what a test reads to say
    /// what did and did not leave the owner's machine.
    create_store_bodies: Vec<serde_json::Value>,
}

struct Stub {
    inner: Mutex<StubInner>,
    relay: RelayStub,
}

type StubState = Arc<Stub>;

fn stub_router(state: StubState) -> Router {
    Router::new()
        .route("/api/v1/kdf", get(stub_kdf))
        .route("/api/v1/login", post(stub_login))
        .route("/api/v1/me/keys", get(stub_me_keys))
        .route("/api/v1/token", post(stub_mint_token))
        .route("/api/v1/stores", get(stub_list_stores).post(stub_create_store))
        .route("/api/v1/stores/{store_id}", delete(stub_delete_store))
        .route("/api/v1/stores/{store_id}/keys", get(stub_get_keys).put(stub_put_keys))
        .route("/api/v1/stores/{store_id}/members", get(stub_list_members).put(stub_put_member))
        .route("/api/v1/stores/{store_id}/members/{user_id}", delete(stub_delete_member))
        .route("/api/v1/stores/{store_id}/invitations/{email}", delete(stub_delete_invitation))
        .route("/api/v1/relay", get(stub_owner_tunnel))
        .route("/api/v1/relay/{store_id}", get(stub_member_connection))
        // The hosted server is given the first; a relay face works the
        // second out from the URL its account signed in at.
        .route("/jwks.json", get(stub_jwks))
        .route("/api/v1/.well-known/jwks.json", get(stub_jwks))
        .with_state(state)
}

fn session_of(headers: &HeaderMap) -> Option<&str> {
    headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "))
}

impl StubInner {
    fn account_of(&self, headers: &HeaderMap) -> &StubAccount {
        let session = session_of(headers);
        self.accounts.iter().find(|a| Some(a.session.as_str()) == session).unwrap_or(&self.accounts[0])
    }

    fn account_by_email(&self, email: Option<&str>) -> &StubAccount {
        self.accounts.iter().find(|a| Some(a.email.as_str()) == email).unwrap_or(&self.accounts[0])
    }

    fn is_owner(&self, user_id: &str, store_id: &str) -> bool {
        self.stores.iter().any(|row| row.user_id == user_id && row.store_id == store_id && row.root.is_none() && row.role == "owner")
    }

    fn has_key(&self, user_id: &str, store_id: &str, root: &Option<String>) -> bool {
        self.key_grants.get(store_id).is_some_and(|rows| rows.iter().any(|g| g.user_id == user_id && &g.root == root))
    }

    fn tier_of(&self, store_id: &str) -> String {
        self.stores.iter().find(|row| row.store_id == store_id).map(|row| row.tier.clone()).unwrap_or_else(|| "hosted".into())
    }

    /// The `stores` claim as the accounts service mints it: a role for a
    /// store held whole, a role per shared root for one held by shares, and
    /// the whole store winning where an account holds both.
    fn stores_claim(&self, user_id: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut claim = serde_json::Map::new();
        for row in self.stores.iter().filter(|row| row.user_id == user_id && row.root.is_none()) {
            claim.insert(row.store_id.clone(), json!(row.role));
        }
        for row in self.stores.iter().filter(|row| row.user_id == user_id) {
            let Some(root) = &row.root else { continue };
            let entry = claim.entry(row.store_id.clone()).or_insert_with(|| json!({ "roots": {} }));
            if let Some(roots) = entry.get_mut("roots") {
                roots[root] = json!(row.role);
            }
        }
        claim
    }

    /// The account's token for one relayed store: the same claim, cut down
    /// to that store, word for word.
    fn store_token(&self, account: &StubAccount, store_id: &str) -> Option<String> {
        let mut claim = self.stores_claim(&account.user_id);
        claim.retain(|id, _| id == store_id);
        (!claim.is_empty()).then(|| make_jwt(&self.signing_key, &self.kid, &self.issuer, &account.user_id, &account.email, &claim))
    }
}

async fn stub_kdf(State(state): State<StubState>, Query(query): Query<HashMap<String, String>>) -> Json<KdfParams> {
    let s = state.inner.lock().unwrap();
    Json(s.account_by_email(query.get("email").map(String::as_str)).kdf.clone())
}

async fn stub_login(State(state): State<StubState>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_by_email(req["email"].as_str());
    Json(json!({ "user": { "id": account.user_id, "email": account.email }, "session": account.session, "token": "", "exp": 0 }))
}

async fn stub_me_keys(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_of(&headers);
    Json(json!({ "public_keys": account.account_public_keys, "kdf": account.kdf, "account_key_blob": account.account_key_blob }))
}

/// `stores`: every relay-tier store the account holds a grant on, its own
/// included, each with the relay's endpoint for it and its own token.
async fn stub_mint_token(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_of(&headers);
    let claim = s.stores_claim(&account.user_id);
    let token = make_jwt(&s.signing_key, &s.kid, &s.issuer, &account.user_id, &account.email, &claim);
    let exp = chrono::Utc::now().timestamp() + 300;
    let stores: Vec<_> = claim
        .keys()
        .filter(|store_id| s.tier_of(store_id) == "relay")
        .map(|store_id| json!({ "store_id": store_id, "rpc_url": format!("ws://{}/api/v1/relay/{}", s.own_addr, store_id), "token": s.store_token(account, store_id), "exp": exp }))
        .collect();
    Json(json!({ "token": token, "exp": exp, "rpc_url": s.h_rpc_url, "stores": stores }))
}

async fn stub_list_stores(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_of(&headers);
    let arr: Vec<_> = s
        .stores
        .iter()
        .filter(|st| st.user_id == account.user_id)
        .map(|st| {
            json!({ "store_id": st.store_id, "name": st.name, "role": st.role, "kind": st.kind, "tier": st.tier, "created_at": st.created_at, "root": st.root, "shared_by": st.shared_by })
        })
        .collect();
    Json(json!(arr))
}

/// A hosted store is created on `H`; a relayed one is recorded and nothing
/// else (it needs its id, it is a vault, and nobody is called).
async fn stub_create_store(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
    state.inner.lock().unwrap().create_store_bodies.push(req.clone());
    let name = req["name"].as_str().unwrap_or_default().to_string();
    let kind_str = req["kind"].as_str().unwrap_or("plain").to_string();
    let tier = req["tier"].as_str().unwrap_or("hosted").to_string();
    let requested_id = req["store_id"].as_str().map(|s| StoreId::parse(s).expect("valid store id"));
    if let Some(id) = requested_id {
        if state.inner.lock().unwrap().stores.iter().any(|row| row.store_id == id.to_string()) {
            return Err(StatusCode::CONFLICT);
        }
    }

    let created_id = if tier == "relay" {
        if kind_str != "vault" {
            return Err(StatusCode::BAD_REQUEST);
        }
        requested_id.ok_or(StatusCode::BAD_REQUEST)?
    } else {
        if name.trim().is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        let kind = if kind_str == "vault" { StoreKind::Vault } else { StoreKind::Plain };
        let (h_client, stores_dir) = {
            let s = state.inner.lock().unwrap();
            (Arc::clone(s.h_client.as_ref().expect("H client wired before any /stores call")), s.h_stores_dir.clone())
        };
        let dir_name = requested_id.map(|id| id.to_string()).unwrap_or_else(|| Uuid::new_v4().to_string());
        let (created_id, _root) = h_client.create_store_with(stores_dir.join(format!("{dir_name}.pimble")), &name, kind, requested_id).await.expect("H creates the hosted store");
        created_id
    };

    let created_at = chrono::Utc::now().to_rfc3339();
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    s.stores.push(StubStoreRow {
        user_id,
        store_id: created_id.to_string(),
        name: name.clone(),
        kind: kind_str.clone(),
        tier: tier.clone(),
        created_at: created_at.clone(),
        role: "owner".into(),
        root: None,
        shared_by: None,
    });
    Ok(Json(json!({ "store_id": created_id.to_string(), "name": name, "role": "owner", "kind": kind_str, "tier": tier, "created_at": created_at })))
}

/// Everything that names the store goes with it, and the relay stops piping
/// to it at once.
async fn stub_delete_store(State(state): State<StubState>, AxPath(store_id): AxPath<String>, headers: HeaderMap) -> Result<Json<serde_json::Value>, StatusCode> {
    {
        let mut s = state.inner.lock().unwrap();
        let caller = s.account_of(&headers).user_id.clone();
        if !s.stores.iter().any(|row| row.store_id == store_id) {
            return Err(StatusCode::NOT_FOUND);
        }
        if !s.is_owner(&caller, &store_id) {
            return Err(StatusCode::FORBIDDEN);
        }
        s.stores.retain(|row| row.store_id != store_id);
        s.key_grants.remove(&store_id);
    }
    state.relay.withdraw(&store_id);
    Ok(Json(json!({})))
}

async fn stub_get_keys(
    State(state): State<StubState>,
    AxPath(store_id): AxPath<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_of(&headers);
    let root = query.get("root");
    let envelopes: Vec<_> = s
        .key_grants
        .get(&store_id)
        .map(|rows| rows.iter().filter(|g| g.user_id == account.user_id && g.root.as_ref() == root).map(|g| json!({ "key_id": g.key_id, "envelope": g.envelope })).collect())
        .unwrap_or_default();
    let signers: Vec<_> = s
        .stores
        .iter()
        .filter(|row| row.store_id == store_id && row.root.is_none() && row.role == "owner")
        .filter_map(|row| s.accounts.iter().find(|a| a.user_id == row.user_id))
        .map(|owner| json!({ "user_id": owner.user_id, "email": owner.email, "public_signing_key": owner.account_public_keys.signing }))
        .collect();
    Json(json!({ "envelopes": envelopes, "signers": signers }))
}

/// An envelope is taken only for an account that holds a grant covering its
/// scope: the whole store, or that root (400 otherwise, as the real service).
async fn stub_put_keys(State(state): State<StubState>, AxPath(store_id): AxPath<String>, Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    for env in body["envelopes"].as_array().cloned().unwrap_or_default() {
        let user_id = env["user_id"].as_str().unwrap().to_string();
        let key_id = env["key_id"].as_str().unwrap().to_string();
        let root = env["root"].as_str().map(String::from);
        let covered = s.stores.iter().any(|row| row.user_id == user_id && row.store_id == store_id && (row.root.is_none() || row.root == root));
        if !covered {
            return Err(StatusCode::BAD_REQUEST);
        }
        let envelope: KeyEnvelope = serde_json::from_value(env["envelope"].clone()).unwrap();
        let entries = s.key_grants.entry(store_id.clone()).or_default();
        entries.retain(|g| !(g.user_id == user_id && g.key_id == key_id && g.root == root));
        entries.push(KeyGrantRow { user_id, key_id, envelope, root });
    }
    Ok(Json(json!({})))
}

/// The members of one scope. The owner's grant is on the whole store, so no
/// share's listing names it.
async fn stub_list_members(
    State(state): State<StubState>,
    AxPath(store_id): AxPath<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let caller_is_owner = s.is_owner(&s.account_of(&headers).user_id.clone(), &store_id);
    let root = query.get("root").cloned();
    let mut share_name: Option<String> = None;
    let mut members = Vec::new();
    for row in s.stores.iter().filter(|row| row.store_id == store_id && row.root == root) {
        let Some(account) = s.accounts.iter().find(|a| a.user_id == row.user_id) else { continue };
        if root.is_some() {
            share_name.get_or_insert_with(|| row.name.clone());
        }
        members.push(json!({
            "user_id": account.user_id,
            "email": account.email,
            "role": row.role,
            "root": root,
            "status": "active",
            "has_key": s.has_key(&account.user_id, &store_id, &root),
            "public_keys": if caller_is_owner { json!(account.account_public_keys) } else { json!(null) },
        }));
    }
    let share_name = root.as_ref().map(|_| share_name.unwrap_or_else(|| "Shared folder".into()));
    Json(json!({ "members": members, "share_name": share_name }))
}

/// Every address in these tests has an account, so a member is a grant.
async fn stub_put_member(
    State(state): State<StubState>,
    AxPath(store_id): AxPath<String>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let caller = s.account_of(&headers);
    let (caller_id, caller_email) = (caller.user_id.clone(), caller.email.clone());
    if !s.is_owner(&caller_id, &store_id) {
        return Err(StatusCode::FORBIDDEN);
    }
    let email = req["email"].as_str().unwrap_or_default().trim().to_string();
    let role = req["role"].as_str().unwrap_or_default().to_string();
    let root = req["root"].as_str().map(String::from);
    let name = req["name"].as_str().unwrap_or_default().to_string();
    if root.is_some() && (role == "owner" || name.trim().is_empty()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (user_id, public_keys) = s.accounts.iter().find(|a| a.email == email).map(|a| (a.user_id.clone(), a.account_public_keys.clone())).ok_or(StatusCode::NOT_FOUND)?;
    match s.stores.iter_mut().find(|row| row.user_id == user_id && row.store_id == store_id && row.root == root) {
        Some(row) => row.role = role.clone(),
        None => {
            let (kind, tier) = s.stores.iter().find(|row| row.store_id == store_id).map(|row| (row.kind.clone(), row.tier.clone())).unwrap_or_else(|| ("vault".into(), "hosted".into()));
            s.stores.push(StubStoreRow {
                user_id: user_id.clone(),
                store_id: store_id.clone(),
                name,
                kind,
                tier,
                created_at: chrono::Utc::now().to_rfc3339(),
                role: role.clone(),
                root: root.clone(),
                shared_by: Some(caller_email),
            });
        }
    }
    let has_key = s.has_key(&user_id, &store_id, &root);
    Ok(Json(json!({ "user_id": user_id, "email": email, "role": role, "root": root, "status": "active", "has_key": has_key, "public_keys": public_keys })))
}

async fn stub_delete_member(
    State(state): State<StubState>,
    AxPath((store_id, user_id)): AxPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let root = query.get("root").cloned();
    let before = s.stores.len();
    s.stores.retain(|row| !(row.user_id == user_id && row.store_id == store_id && row.root == root));
    if s.stores.len() == before {
        return Err(StatusCode::NOT_FOUND);
    }
    if let Some(rows) = s.key_grants.get_mut(&store_id) {
        rows.retain(|g| !(g.user_id == user_id && g.root == root));
    }
    Ok(Json(json!({})))
}

async fn stub_delete_invitation() -> Json<serde_json::Value> {
    Json(json!({}))
}

async fn stub_jwks(State(state): State<StubState>) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let x = URL_SAFE_NO_PAD.encode(s.signing_key.verifying_key().to_bytes());
    Json(json!({ "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": s.kid, "x": x } ] }))
}

// ── The relay stub ────────────────────────────────────────────────────────
//
// `pimble-cloud`'s `src/relay.rs`, as small as it can be and still be the
// same wire: session auth and no `Origin` for the tunnel, `serve`/`serving`,
// `[conn u32 BE][kind u8][payload]` frames, a member's token that must name
// this store and no other, 4404 `owner offline`. Nothing is kept: a message
// is in a channel or it is nowhere.

const KIND_OPEN: u8 = 1;
const KIND_TEXT: u8 = 2;
const KIND_CLOSE: u8 = 3;
const CLOSE_OWNER_OFFLINE: u16 = 4404;

type ServerSocket = WebSocketStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>;

enum ToMember {
    Text(String),
    Close(u16, &'static str),
}

struct TunnelStub {
    out: mpsc::Sender<Message>,
    conns: Mutex<HashMap<u32, mpsc::Sender<ToMember>>>,
}

impl TunnelStub {
    fn close_all(&self, code: u16, reason: &'static str) {
        for (_, member) in self.conns.lock().unwrap().drain() {
            let _ = member.try_send(ToMember::Close(code, reason));
        }
    }
}

#[derive(Default)]
struct RelayStub {
    /// Store id to the tunnel it is behind.
    by_store: Mutex<HashMap<String, Arc<TunnelStub>>>,
    next_conn: AtomicU32,
    /// Members' connections that were piped to an owner.
    piped: AtomicUsize,
    /// Members' connections answered `owner offline`.
    owner_offline: AtomicUsize,
    /// Members' connections refused before the upgrade for a token that
    /// names another store too (the account's general token).
    wrong_token: AtomicUsize,
}

impl RelayStub {
    fn withdraw(&self, store_id: &str) {
        if let Some(tunnel) = self.by_store.lock().unwrap().remove(store_id) {
            tunnel.close_all(CLOSE_OWNER_OFFLINE, "owner offline");
        }
    }

    fn serves(&self, store_id: StoreId) -> bool {
        self.by_store.lock().unwrap().contains_key(&store_id.to_string())
    }

    /// The virtual connections in flight, over every tunnel.
    fn open_conns(&self) -> usize {
        let tunnels: Vec<Arc<TunnelStub>> = self.by_store.lock().unwrap().values().cloned().collect();
        let mut seen: Vec<*const TunnelStub> = Vec::new();
        let mut count = 0;
        for tunnel in tunnels {
            if !seen.contains(&Arc::as_ptr(&tunnel)) {
                seen.push(Arc::as_ptr(&tunnel));
                count += tunnel.conns.lock().unwrap().len();
            }
        }
        count
    }
}

fn frame(conn: u32, kind: u8, payload: &[u8]) -> Message {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&conn.to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Message::Binary(out)
}

fn close_message(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() }))
}

/// Answer `101` and hand the upgraded socket to `run`.
fn upgrade<F, Fut>(mut request: Request, run: F) -> Response
where
    F: FnOnce(ServerSocket) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let Some(key) = request.headers().get("sec-websocket-key").cloned() else { return StatusCode::BAD_REQUEST.into_response() };
    let Some(on_upgrade) = request.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() else { return StatusCode::BAD_REQUEST.into_response() };
    tokio::spawn(async move {
        if let Ok(upgraded) = on_upgrade.await {
            run(WebSocketStream::from_raw_socket(hyper_util::rt::TokioIo::new(upgraded), Role::Server, None).await).await;
        }
    });
    let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes());
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_ACCEPT, accept)
        .body(axum::body::Body::empty())
        .unwrap()
}

/// `GET /api/v1/relay`: the owner's tunnel. The account's session, and never
/// a browser.
async fn stub_owner_tunnel(State(state): State<StubState>, request: Request) -> Response {
    if request.headers().contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let user_id = {
        let s = state.inner.lock().unwrap();
        let session = session_of(request.headers());
        s.accounts.iter().find(|a| Some(a.session.as_str()) == session).map(|a| a.user_id.clone())
    };
    let Some(user_id) = user_id else { return StatusCode::UNAUTHORIZED.into_response() };
    upgrade(request, move |socket| run_tunnel(state, user_id, socket))
}

async fn run_tunnel(state: StubState, user_id: String, socket: ServerSocket) {
    let (mut sink, mut stream) = socket.split();
    let (out, mut out_rx) = mpsc::channel::<Message>(8);
    let tunnel = Arc::new(TunnelStub { out, conns: Mutex::new(HashMap::new()) });
    let writer = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(message)) = stream.next().await {
        match message {
            // Each `serve` is the whole set from now on.
            Message::Text(text) => {
                let Ok(serve) = serde_json::from_str::<serde_json::Value>(&text) else { break };
                let announced: Vec<String> = serve["serve"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                let serving: Vec<String> = {
                    let s = state.inner.lock().unwrap();
                    announced.into_iter().filter(|id| s.tier_of(id) == "relay" && s.is_owner(&user_id, id)).collect()
                };
                {
                    let mut by_store = state.relay.by_store.lock().unwrap();
                    by_store.retain(|id, held| !Arc::ptr_eq(held, &tunnel) || serving.contains(id));
                    for id in &serving {
                        if let Some(earlier) = by_store.insert(id.clone(), tunnel.clone()).filter(|earlier| !Arc::ptr_eq(earlier, &tunnel)) {
                            earlier.close_all(CLOSE_OWNER_OFFLINE, "owner offline");
                        }
                    }
                }
                if tunnel.out.send(Message::Text(json!({ "serving": serving }).to_string())).await.is_err() {
                    break;
                }
            }
            Message::Binary(bytes) if bytes.len() >= 5 => {
                let conn = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let member = tunnel.conns.lock().unwrap().get(&conn).cloned();
                match (bytes[4], member) {
                    (KIND_TEXT, Some(member)) => {
                        if let Ok(text) = String::from_utf8(bytes[5..].to_vec()) {
                            let _ = member.send(ToMember::Text(text)).await;
                        }
                    }
                    (KIND_CLOSE, Some(member)) => {
                        tunnel.conns.lock().unwrap().remove(&conn);
                        let _ = member.send(ToMember::Close(1000, "closed by the owner")).await;
                    }
                    _ => {}
                }
            }
            Message::Binary(_) | Message::Close(_) => break,
            _ => {}
        }
    }

    // The tunnel is gone: its stores are behind nothing, and every member
    // through it is told so.
    state.relay.by_store.lock().unwrap().retain(|_, held| !Arc::ptr_eq(held, &tunnel));
    tunnel.close_all(CLOSE_OWNER_OFFLINE, "owner offline");
    writer.abort();
}

/// `GET /api/v1/relay/<store id>`: a member's connection. Its token is the
/// stub's own, and names this store and no other.
async fn stub_member_connection(State(state): State<StubState>, AxPath(store_id): AxPath<String>, request: Request) -> Response {
    let Some(token) = session_of(request.headers()).map(str::to_string) else { return StatusCode::UNAUTHORIZED.into_response() };
    let named = {
        let s = state.inner.lock().unwrap();
        verified_stores(&s.signing_key, &token)
    };
    let Some(named) = named else { return StatusCode::UNAUTHORIZED.into_response() };
    if named != vec![store_id.clone()] {
        state.relay.wrong_token.fetch_add(1, Ordering::SeqCst);
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade(request, move |socket| run_member(state, store_id, token, socket))
}

async fn run_member(state: StubState, store_id: String, token: String, mut socket: ServerSocket) {
    let tunnel = state.relay.by_store.lock().unwrap().get(&store_id).cloned();
    let Some(tunnel) = tunnel else {
        state.relay.owner_offline.fetch_add(1, Ordering::SeqCst);
        let _ = socket.send(close_message(CLOSE_OWNER_OFFLINE, "owner offline")).await;
        // Read until the peer has closed too, so the close frame is not
        // overtaken by a reset.
        let _ = tokio::time::timeout(Duration::from_secs(2), async { while let Some(Ok(_)) = socket.next().await {} }).await;
        return;
    };
    state.relay.piped.fetch_add(1, Ordering::SeqCst);
    let conn = state.relay.next_conn.fetch_add(1, Ordering::SeqCst) + 1;
    let (to_member, mut from_owner) = mpsc::channel::<ToMember>(2);
    tunnel.conns.lock().unwrap().insert(conn, to_member);
    let mut owner_knows = false;
    if tunnel.out.send(frame(conn, KIND_OPEN, token.as_bytes())).await.is_ok() {
        loop {
            tokio::select! {
                incoming = socket.next() => match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if tunnel.out.send(frame(conn, KIND_TEXT, text.as_bytes())).await.is_err() {
                            let _ = socket.send(close_message(CLOSE_OWNER_OFFLINE, "owner offline")).await;
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
                outgoing = from_owner.recv() => match outgoing {
                    Some(ToMember::Text(text)) => {
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Some(ToMember::Close(code, reason)) => {
                        owner_knows = true;
                        let _ = socket.send(close_message(code, reason)).await;
                        let _ = tokio::time::timeout(Duration::from_secs(2), async { while let Some(Ok(_)) = socket.next().await {} }).await;
                        break;
                    }
                    None => break,
                },
            }
        }
    }
    tunnel.conns.lock().unwrap().remove(&conn);
    if !owner_knows {
        let _ = tunnel.out.send(frame(conn, KIND_CLOSE, &[])).await;
    }
}

// ── The environment ───────────────────────────────────────────────────────

fn cheap_kdf_params() -> KdfParams {
    let mut params = KdfParams::generate();
    params.m_cost = 8 * 1024;
    params.t_cost = 1;
    params
}

fn stub_account(email: &str, password: &str) -> StubAccount {
    let kdf = cheap_kdf_params();
    let account_keys = AccountKeys::generate();
    let password_keys = derive_password_keys(password, &kdf).expect("derive password keys");
    let account_key_blob = wrap_account_keys(&account_keys, &password_keys.kek).expect("wrap account keys");
    StubAccount {
        kdf,
        user_id: Uuid::new_v4().to_string(),
        email: email.to_string(),
        session: format!("sess-{}", Uuid::new_v4()),
        account_key_blob,
        account_public_keys: account_keys.public_keys(),
    }
}

const ALICE: (&str, &str) = ("alice@example.com", "correct horse battery staple");
const BOB: (&str, &str) = ("bob@example.com", "a different horse entirely");
const CAROL: (&str, &str) = ("carol@example.com", "a third horse altogether");

struct Env {
    stub_url: String,
    stub_addr: std::net::SocketAddr,
    stub: StubState,
    _h: PimbleServer,
    /// A client to `H` with the service token, to see that it holds nothing.
    h_admin: PimbleClient,
    h_stores_dir: PathBuf,
    h_dir: tempfile::TempDir,
}

impl Env {
    async fn sign_in(&self, client: &PimbleClient, who: (&str, &str)) {
        client.cloud_sign_in(&self.stub_url, who.0, who.1).await.expect("sign in");
    }

    /// `who` holds another store as well (hosted, and nobody's business
    /// here), so the account's general token names two stores, as most
    /// people's does. The relay takes no such token for either.
    fn holds_another_store(&self, who: (&str, &str)) {
        let mut s = self.stub.inner.lock().unwrap();
        let user_id = s.account_by_email(Some(who.0)).user_id.clone();
        s.stores.push(StubStoreRow {
            user_id,
            store_id: Uuid::new_v4().to_string(),
            name: "Something else entirely".into(),
            kind: "vault".into(),
            tier: "hosted".into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            role: "owner".into(),
            root: None,
            shared_by: None,
        });
    }

    /// Where members reach `store_id`: the relay's endpoint for it.
    fn relay_url(&self, store_id: StoreId) -> String {
        format!("ws://{}/api/v1/relay/{}", self.stub_addr, store_id)
    }

    /// `who`'s token for one relayed store, and the account's general one.
    fn tokens(&self, who: (&str, &str), store_id: StoreId) -> (String, String) {
        let s = self.stub.inner.lock().unwrap();
        let account = s.account_by_email(Some(who.0));
        let general = make_jwt(&s.signing_key, &s.kid, &s.issuer, &account.user_id, &account.email, &s.stores_claim(&account.user_id));
        (s.store_token(account, &store_id.to_string()).expect("the account holds a grant on the store"), general)
    }

    /// Nothing of any store is on Pimble Cloud's disk: `H` has no store, its
    /// stores directory no entry, and none of `needles` is anywhere under it.
    async fn assert_nothing_hosted(&self, needles: &[&str]) {
        assert!(self.h_admin.list_stores().await.unwrap().is_empty(), "the hosted server holds no store");
        assert_eq!(std::fs::read_dir(&self.h_stores_dir).unwrap().count(), 0, "and its stores directory gained nothing");
        for needle in needles {
            assert!(!contains_bytes_recursive(self.h_dir.path(), needle.as_bytes()), "{needle:?} reached the hosted server's disk");
        }
    }
}

async fn spawn_env() -> Env {
    let signing_key = fresh_signing_key();
    let issuer = "https://cloud.test".to_string();

    let h_dir = tempfile::tempdir().unwrap();
    let h_stores_dir = h_dir.path().join("stores");
    std::fs::create_dir_all(&h_stores_dir).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_addr = listener.local_addr().unwrap();

    let state: StubState = Arc::new(Stub {
        inner: Mutex::new(StubInner {
            accounts: vec![stub_account(ALICE.0, ALICE.1), stub_account(BOB.0, BOB.1), stub_account(CAROL.0, CAROL.1)],
            stores: Vec::new(),
            key_grants: HashMap::new(),
            signing_key,
            kid: "test-kid".to_string(),
            issuer: issuer.clone(),
            h_client: None,
            h_rpc_url: String::new(),
            h_stores_dir: h_stores_dir.clone(),
            own_addr: stub_addr.to_string(),
            create_store_bodies: Vec::new(),
        }),
        relay: RelayStub::default(),
    });

    let router = stub_router(Arc::clone(&state));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let stub_url = format!("http://{stub_addr}");

    let h_service_token = "h-service-token".to_string();
    let mut h = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(h_service_token.clone()),
        jwks_url: Some(format!("{stub_url}/jwks.json").parse().unwrap()),
        jwt_issuer: Some(issuer),
        keystore_path: Some(h_dir.path().join("keys.json")),
        credentials_path: Some(h_dir.path().join("credentials.json")),
        replicas_dir: Some(h_dir.path().join("replicas")),
        ..Default::default()
    });
    h.start().await.expect("H starts");
    let h_url = format!("http://{}", h.addr());
    let h_client = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token.clone() }).await.expect("service client connects to H");
    let h_admin = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token }).await.expect("admin client connects to H");
    {
        let mut s = state.inner.lock().unwrap();
        s.h_client = Some(Arc::new(h_client));
        s.h_rpc_url = format!("ws://{}", h.addr());
    }

    Env { stub_url, stub_addr, stub: state, _h: h, h_admin, h_stores_dir, h_dir }
}

/// Alice's store, never hosted:
///
/// ```text
/// root
/// ├── Holiday (folder)        <- what gets shared
/// │   ├── Packing (document, "socks and a map")
/// │   └── Tickets (folder)
/// │       └── Train (document)
/// └── Diary (document, "PRIVATE-DIARY-TEXT")
/// ```
struct Fixture {
    store_id: StoreId,
    root_id: NodeId,
    shared: NodeId,
    inside: NodeId,
    deeper: NodeId,
    leaf: NodeId,
    private: NodeId,
}

const STORE_NAME: &str = "Alice's Secret Notebook";

async fn plain_fixture(env: &Env, alice: &PimbleClient, alice_dir: &Path) -> Fixture {
    env.sign_in(alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(alice_dir.join("a.pimble"), STORE_NAME).await.unwrap();
    let shared = alice.create_node(store_id, Some(root_id), "folder", "Holiday").await.unwrap();
    let inside = alice.create_node(store_id, Some(shared), "document", "Packing").await.unwrap();
    let deeper = alice.create_node(store_id, Some(shared), "folder", "Tickets").await.unwrap();
    let leaf = alice.create_node(store_id, Some(deeper), "document", "Train").await.unwrap();
    let private = alice.create_node(store_id, Some(root_id), "document", "Diary").await.unwrap();
    write_text(alice, store_id, inside, "seed", "socks and a map").await;
    write_text(alice, store_id, private, "seed", "PRIVATE-DIARY-TEXT").await;
    // A person's content edit earns a `modified_at` stamp on the server's
    // flush debounce (750 ms). Let it land before the store is relayed.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    Fixture { store_id, root_id, shared, inside, deeper, leaf, private }
}

/// The store shared from Alice's computer, its link reconciled and the
/// tunnel announcing it.
async fn relayed_fixture(env: &Env, alice: &PimbleClient, alice_dir: &Path) -> Fixture {
    let fx = plain_fixture(env, alice, alice_dir).await;
    alice.cloud_relay_store(fx.store_id).await.expect("an unhosted store is shared from this computer");
    let up = wait_until(Duration::from_secs(20), || async {
        matches!(alice.get_store_sync_response(fx.store_id).await.map(|a| a.state), Ok(SyncState::Synced { .. })) && env.stub.relay.serves(fx.store_id)
    })
    .await;
    assert!(up, "the link to the twin syncs and the relay is told the store is served");
    fx
}

/// The folder shared, with `members` invited.
async fn share_with(alice: &PimbleClient, fx: &Fixture, members: &[((&str, &str), MemberRole)]) {
    let answer = alice.cloud_share_node(fx.store_id, fx.shared, "Holiday Plans").await.expect("a relayed store's folder is shared exactly as a hosted one's");
    assert!(matches!(answer.share.state, SyncState::Synced { .. }), "the wraps and the scope are on the twin when the call answers: {:?}", answer.share.state);
    for (who, role) in members {
        alice.cloud_share_invite(fx.store_id, fx.shared, who.0, *role).await.expect("inviting a member");
    }
}

/// A member's device: signed in, the share added, its first document pulled
/// through the relay.
async fn member_device(env: &Env, who: (&str, &str), fx: &Fixture) -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let (server, client, dir) = start_local_server().await;
    env.sign_in(&client, who).await;
    let store = client.cloud_add_hosted_store(fx.store_id).await.expect("a relayed share is added like any hosted store");
    assert_eq!(store.relay, RelaySide::Member, "{}'s replica says it is served from its owner's computer", who.0);
    assert_eq!(store.sync_mode, StoreKind::Vault);
    let pulled = wait_until(Duration::from_secs(20), || async { node_text(&client, fx.store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "{}'s replica pulls the scope through the relay and reads it with the share's key", who.0);
    (server, client, dir)
}

async fn link_state(client: &PimbleClient, store_id: StoreId) -> SyncState {
    client.get_store_sync_response(store_id).await.map(|answer| answer.state).unwrap_or(SyncState::Offline)
}

// ── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn relaying_a_store_uploads_nothing_and_keeps_its_twin_on_this_machine() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;

    // The accounts service holds a record, the owner's grant and the owner's
    // key envelope: account data. No name, tier relay, and it called nobody.
    {
        let s = env.stub.inner.lock().unwrap();
        assert_eq!(s.create_store_bodies.len(), 1);
        let body = &s.create_store_bodies[0];
        assert_eq!((body["name"].as_str(), body["kind"].as_str(), body["tier"].as_str(), body["store_id"].as_str()), (Some(""), Some("vault"), Some("relay"), Some(store_id.to_string().as_str())));
        assert!(!body.to_string().contains(STORE_NAME), "the owner's name for the store stays on this machine");
        let rows: Vec<_> = s.stores.iter().filter(|row| row.store_id == store_id.to_string()).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].name.as_str(), rows[0].tier.as_str(), rows[0].role.as_str()), ("", "relay", "owner"));
        assert_eq!(s.key_grants.get(&store_id.to_string()).map(Vec::len), Some(1), "the owner's own envelope of the store key");
    }
    env.assert_nothing_hosted(&[STORE_NAME, "PRIVATE-DIARY-TEXT", "socks and a map", &store_id.to_string()]).await;

    // The twin is on this machine, whole, and ciphertext.
    let twin = twin_dir(a_dir.path(), store_id);
    let whole = wait_until(Duration::from_secs(15), || async { twin_doc_count(a_dir.path(), store_id) == 6 }).await;
    assert!(whole, "every document of the store is on the twin: {}", twin_doc_count(a_dir.path(), store_id));
    for needle in [STORE_NAME, "PRIVATE-DIARY-TEXT", "socks and a map", "Holiday"] {
        assert!(!contains_bytes_recursive(&twin, needle.as_bytes()), "{needle:?} is readable in the twin");
    }

    // What the app is told: a vault link, relayed from here, and where
    // members reach the store (never where the link connects: that is this
    // process's own face, on a port that is new every run).
    let sync = alice.get_store_sync_response(store_id).await.unwrap();
    assert_eq!((sync.sync_mode, sync.relay), (StoreKind::Vault, RelaySide::Owner));
    assert_eq!(sync.remote.as_ref().map(|r| r.url.to_string()), Some(env.relay_url(store_id)));
    let listed = alice.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap();
    assert_eq!((listed.relay, listed.sync_mode, listed.name.as_str()), (RelaySide::Owner, StoreKind::Vault, STORE_NAME));
    let sync_json: serde_json::Value = serde_json::from_slice(&std::fs::read(a_dir.path().join("a.pimble").join("sync.json")).unwrap()).unwrap();
    assert_eq!(sync_json["mode"].as_str(), Some("relay"));
    assert_eq!(sync_json["remote"]["url"].as_str(), Some(env.relay_url(store_id).as_str()), "the face's address, new every run, is never written down");

    let rows = alice.cloud_list_hosted_stores_response().await.unwrap();
    assert_eq!(rows.relayed, vec![store_id.to_string()]);
    assert_eq!(rows.tier_of(&store_id.to_string()), "relay");

    // Shared from here already; and its link is not something to unlink or
    // point elsewhere.
    assert_eq!(alice.cloud_relay_store(store_id).await.expect_err("relayed already").to_string(), pimble_server::relay_face::ALREADY_RELAYED_REFUSAL);
    assert_eq!(alice.set_store_sync_response(store_id, None).await.expect_err("not unlinked like that").to_string(), pimble_server::relay_face::RELAYED_LINK_REFUSAL);

    // An edit reaches the twin like any vault link's, still as ciphertext.
    write_text(&alice, store_id, fx.inside, "alice", "AND-A-TORCH").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!contains_bytes_recursive(&twin, b"AND-A-TORCH"));
    env.assert_nothing_hosted(&["AND-A-TORCH"]).await;

    // Closing the store takes it off the relay; opening it puts it back, on
    // the same twin.
    let created_at = twin_created_at(a_dir.path(), store_id).expect("the twin's manifest");
    alice.close_store(store_id).await.unwrap();
    assert!(wait_until(Duration::from_secs(10), || async { !env.stub.relay.serves(store_id) }).await, "a closed store is withdrawn from the relay");
    let reopened = alice.open_store(a_dir.path().join("a.pimble")).await.unwrap();
    assert_eq!(reopened.relay, RelaySide::Owner);
    assert!(wait_until(Duration::from_secs(20), || async { env.stub.relay.serves(store_id) }).await, "and announced again once it is open and its link has reconciled");
    assert_eq!(twin_created_at(a_dir.path(), store_id), Some(created_at), "the twin that was there is the twin that is used");

    a.stop().await.unwrap();
    assert!(wait_until(Duration::from_secs(10), || async { !env.stub.relay.serves(store_id) }).await, "a stopped server's tunnel is gone");
}

#[tokio::test]
async fn relaying_is_refused_for_what_cannot_be_shared_from_here() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;

    // Nobody signed in: nothing is asked of anyone.
    let (plain_id, _) = alice.create_store(a_dir.path().join("plain.pimble"), "Plain").await.unwrap();
    let refused = alice.cloud_relay_store(plain_id).await.expect_err("no account").to_string();
    assert!(refused.contains("no Pimble Cloud account is signed in"), "{refused}");
    assert_eq!(alice.cloud_stop_relaying(plain_id).await.expect_err("not relayed").to_string(), pimble_server::relay_face::NOT_RELAYED_REFUSAL);

    // A hosted store is shared from Pimble Cloud.
    env.sign_in(&alice, ALICE).await;
    let (hosted_id, _) = alice.create_store(a_dir.path().join("hosted.pimble"), "Hosted").await.unwrap();
    alice.cloud_host_store(hosted_id).await.unwrap();
    assert_eq!(alice.cloud_relay_store(hosted_id).await.expect_err("hosted").to_string(), pimble_server::relay_face::ALREADY_HOSTED_REFUSAL);

    // A replica of it, on another of the owner's devices, lives elsewhere.
    let (mut a2, alice2, a2_dir) = start_local_server().await;
    env.sign_in(&alice2, ALICE).await;
    alice2.cloud_add_hosted_store(hosted_id).await.expect("the owner's other device adds the hosted store");
    assert_eq!(alice2.cloud_relay_store(hosted_id).await.expect_err("a replica").to_string(), pimble_server::relay_face::REPLICA_REFUSAL);

    // None of it recorded anything, or started a face.
    {
        let s = env.stub.inner.lock().unwrap();
        assert!(s.create_store_bodies.iter().all(|body| body["tier"].as_str() != Some("relay")));
    }
    assert!(!a_dir.path().join("relay").exists() && !a2_dir.path().join("relay").exists(), "no relay face was started for a refusal");

    a.stop().await.unwrap();
    a2.stop().await.unwrap();
}

#[tokio::test]
async fn two_members_co_edit_a_relayed_folders_tree_and_outlast_the_owner_going_away() {
    let env = spawn_env().await;
    env.holds_another_store(BOB);
    env.holds_another_store(CAROL);
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor), (CAROL, MemberRole::Editor)]).await;

    let (mut b, bob, b_dir) = member_device(&env, BOB, &fx).await;
    let (mut c, carol, c_dir) = member_device(&env, CAROL, &fx).await;
    for (client, dir) in [(&bob, &b_dir), (&carol, &c_dir)] {
        // A member's link's endpoint is the relay, and its replica holds the
        // share and nothing else.
        let sync = client.get_store_sync_response(store_id).await.unwrap();
        assert_eq!(sync.remote.as_ref().map(|r| r.url.to_string()), Some(env.relay_url(store_id)));
        assert_eq!((sync.relay, sync.sync_mode), (RelaySide::Member, StoreKind::Vault));
        assert!(matches!(sync.state, SyncState::Synced { .. }), "{:?}", sync.state);
        assert_eq!(child_ids(client, store_id, shared).await, vec![fx.inside, fx.deeper]);
        assert!(client.get_node(store_id, fx.private).await.is_err(), "a node outside the share is not held");
        assert!(!contains_bytes_recursive(dir.path(), b"PRIVATE-DIARY-TEXT"));
        assert!(!contains_bytes_recursive(dir.path(), STORE_NAME.as_bytes()), "the owner's store name never reaches a member");
    }
    assert!(env.stub.relay.piped.load(Ordering::SeqCst) >= 2, "both members came in through the relay");
    assert_eq!(env.stub.relay.wrong_token.load(Ordering::SeqCst), 0, "each with the token minted for this store alone");

    // The TREE, co-edited through the relay: create, text, rename, move,
    // delete, each by one member and seen by the other and by the owner.
    let ferry = bob.create_node(store_id, Some(fx.deeper), "document", "Ferry").await.unwrap();
    write_text(&bob, store_id, ferry, "bob", "ferry at nine").await;
    let created = wait_until(Duration::from_secs(20), || async {
        child_ids(&carol, store_id, fx.deeper).await == vec![fx.leaf, ferry]
            && node_text(&carol, store_id, ferry).await.contains("ferry at nine")
            && child_ids(&alice, store_id, fx.deeper).await == vec![fx.leaf, ferry]
            && node_text(&alice, store_id, ferry).await.contains("ferry at nine")
    })
    .await;
    assert!(created, "one member's create reaches the other member and the owner, readable");

    rename(&carol, store_id, ferry, "Night ferry").await;
    carol.move_node(store_id, ferry, shared, None).await.expect("a move inside the share");
    write_text(&carol, store_id, fx.inside, "carol", "and sunscreen").await;
    let followed = wait_until(Duration::from_secs(20), || async {
        node_title(&bob, store_id, ferry).await == "Night ferry"
            && child_ids(&bob, store_id, shared).await == vec![fx.inside, fx.deeper, ferry]
            && child_ids(&bob, store_id, fx.deeper).await == vec![fx.leaf]
            && node_text(&bob, store_id, fx.inside).await.contains("and sunscreen")
            && child_ids(&alice, store_id, shared).await == vec![fx.inside, fx.deeper, ferry]
            && node_title(&alice, store_id, ferry).await == "Night ferry"
    })
    .await;
    assert!(followed, "the other member's rename, move and text reach everyone: bob sees {:?}", child_ids(&bob, store_id, shared).await);

    bob.delete_node(store_id, fx.leaf).await.expect("a delete inside the share");
    let deleted = wait_until(Duration::from_secs(20), || async {
        child_ids(&carol, store_id, fx.deeper).await.is_empty() && carol.get_node(store_id, fx.leaf).await.is_err() && child_ids(&alice, store_id, fx.deeper).await.is_empty()
    })
    .await;
    assert!(deleted, "a member's delete reaches the other member and the owner");
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private], "and nothing a member did touched the rest of the owner's tree");

    // And the owner's own edits reach both.
    let hotel = alice.create_node(store_id, Some(shared), "document", "Hotel").await.unwrap();
    write_text(&alice, store_id, hotel, "alice", "two nights").await;
    let reached = wait_until(Duration::from_secs(30), || async {
        node_text(&bob, store_id, hotel).await.contains("two nights") && node_text(&carol, store_id, hotel).await.contains("two nights")
    })
    .await;
    assert!(reached, "the owner's create reaches the members once the scope names it");
    env.assert_nothing_hosted(&["ferry at nine", "two nights", "Night ferry"]).await;

    // The owner's computer goes off. Members' links go Offline, and what
    // their reconnects are told is `owner offline`.
    let created_at = twin_created_at(a_dir.path(), store_id).expect("the twin's manifest");
    drop(alice);
    a.stop().await.unwrap();
    let offline = wait_until(Duration::from_secs(15), || async {
        matches!(link_state(&bob, store_id).await, SyncState::Offline) && matches!(link_state(&carol, store_id).await, SyncState::Offline)
    })
    .await;
    assert!(offline, "with the owner gone, members' links are offline: {:?} {:?}", link_state(&bob, store_id).await, link_state(&carol, store_id).await);
    assert!(wait_until(Duration::from_secs(15), || async { env.stub.relay.owner_offline.load(Ordering::SeqCst) >= 2 }).await, "and their reconnects are answered `owner offline`");
    assert_eq!(env.stub.relay.open_conns(), 0, "the relay holds nothing once they are gone");

    // They keep reading and editing what they have; their edits wait on
    // their own devices, and they do not see each other.
    let taxi = bob.create_node(store_id, Some(shared), "document", "Taxi").await.expect("a member edits offline");
    write_text(&bob, store_id, taxi, "bob", "taxi at six").await;
    rename(&carol, store_id, fx.deeper, "Travel").await;
    write_text(&carol, store_id, hotel, "carol", "with breakfast").await;
    carol.delete_node(store_id, ferry).await.expect("a member deletes offline");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(carol.get_node(store_id, taxi).await.is_err(), "two members do not see each other while the owner is away");
    assert_ne!(node_title(&bob, store_id, fx.deeper).await, "Travel");
    // `Offline`, and staying there: a try that meets `owner offline` is not
    // a connection being made, so the state does not flap at every one.
    for _ in 0..40 {
        assert!(matches!(link_state(&bob, store_id).await, SyncState::Offline) && matches!(link_state(&carol, store_id).await, SyncState::Offline));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The owner is back: the same twin, and everything converges.
    let (mut a, alice) = start_device_in(a_dir.path()).await;
    let reopened = alice.open_store(a_dir.path().join("a.pimble")).await.expect("the store opens again");
    assert_eq!(reopened.relay, RelaySide::Owner);
    let converged = wait_until(Duration::from_secs(90), || async {
        let want = sorted(vec![fx.inside, fx.deeper, hotel, taxi]);
        sorted(child_ids(&alice, store_id, shared).await) == want
            && sorted(child_ids(&bob, store_id, shared).await) == want
            && sorted(child_ids(&carol, store_id, shared).await) == want
            && node_text(&alice, store_id, taxi).await.contains("taxi at six")
            && node_text(&carol, store_id, taxi).await.contains("taxi at six")
            && node_title(&alice, store_id, fx.deeper).await == "Travel"
            && node_title(&bob, store_id, fx.deeper).await == "Travel"
            && node_text(&alice, store_id, hotel).await.contains("with breakfast")
            && node_text(&bob, store_id, hotel).await.contains("with breakfast")
    })
    .await;
    assert!(
        converged,
        "alice {:?}, bob {:?}, carol {:?}",
        sorted(child_ids(&alice, store_id, shared).await),
        sorted(child_ids(&bob, store_id, shared).await),
        sorted(child_ids(&carol, store_id, shared).await)
    );
    assert_eq!(twin_created_at(a_dir.path(), store_id), Some(created_at), "the twin was reused, not built again");
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private]);
    env.assert_nothing_hosted(&["taxi at six", "with breakfast"]).await;

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn a_deleted_twin_is_built_again_and_members_converge_on_it() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor), (CAROL, MemberRole::Editor)]).await;
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (mut c, carol, c_dir) = member_device(&env, CAROL, &fx).await;

    // Enough history that the old logs' numbers are well past where the new
    // ones will start.
    for round in 0..4 {
        write_text(&bob, store_id, fx.inside, "bob", &format!("bob's line {round}")).await;
        write_text(&carol, store_id, fx.inside, "carol", &format!("carol's line {round}")).await;
    }
    let settled = wait_until(Duration::from_secs(20), || async {
        let text = node_text(&alice, store_id, fx.inside).await;
        text.contains("bob's line 3") && text.contains("carol's line 3") && node_text(&bob, store_id, fx.inside).await.contains("carol's line 3")
    })
    .await;
    assert!(settled);

    // The owner goes away; each member edits offline; and the twin is
    // deleted: it is derived and disposable.
    let created_at = twin_created_at(a_dir.path(), store_id).expect("the twin's manifest");
    drop(alice);
    a.stop().await.unwrap();
    assert!(wait_until(Duration::from_secs(15), || async { matches!(link_state(&bob, store_id).await, SyncState::Offline) && matches!(link_state(&carol, store_id).await, SyncState::Offline) }).await);
    let taxi = bob.create_node(store_id, Some(shared), "document", "Taxi").await.unwrap();
    write_text(&bob, store_id, taxi, "bob", "taxi at six").await;
    write_text(&carol, store_id, fx.inside, "carol", "OFFLINE-SUNHAT").await;
    rename(&carol, store_id, fx.deeper, "Travel").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    std::fs::remove_dir_all(twin_dir(a_dir.path(), store_id)).expect("the twin is deleted while its server is stopped");

    // Back: the link builds the twin again from the store, the members read
    // the new logs from the start and offer what they hold, and everyone
    // ends up with everything.
    let (mut a, alice) = start_device_in(a_dir.path()).await;
    alice.open_store(a_dir.path().join("a.pimble")).await.expect("the store opens again");
    let converged = wait_until(Duration::from_secs(120), || async {
        let want = sorted(vec![fx.inside, fx.deeper, taxi]);
        sorted(child_ids(&alice, store_id, shared).await) == want
            && sorted(child_ids(&carol, store_id, shared).await) == want
            && node_text(&alice, store_id, taxi).await.contains("taxi at six")
            && node_text(&carol, store_id, taxi).await.contains("taxi at six")
            && node_text(&alice, store_id, fx.inside).await.contains("OFFLINE-SUNHAT")
            && node_text(&bob, store_id, fx.inside).await.contains("OFFLINE-SUNHAT")
            && node_title(&alice, store_id, fx.deeper).await == "Travel"
            && node_title(&bob, store_id, fx.deeper).await == "Travel"
    })
    .await;
    assert!(
        converged,
        "alice: {:?} / {:?}; bob: {:?}; carol: {:?}",
        sorted(child_ids(&alice, store_id, shared).await),
        node_text(&alice, store_id, fx.inside).await,
        node_title(&bob, store_id, fx.deeper).await,
        sorted(child_ids(&carol, store_id, shared).await)
    );
    let rebuilt = twin_created_at(a_dir.path(), store_id).expect("a twin again");
    assert_ne!(rebuilt, created_at, "another twin, with logs of its own");
    assert!(twin_doc_count(a_dir.path(), store_id) >= 7, "holding every document again: {}", twin_doc_count(a_dir.path(), store_id));
    assert!(!contains_bytes_recursive(&twin_dir(a_dir.path(), store_id), b"OFFLINE-SUNHAT"), "as ciphertext");

    // And it goes on working: a live edit each way on the new logs.
    write_text(&alice, store_id, taxi, "alice", "make it half past").await;
    write_text(&bob, store_id, fx.inside, "bob", "AFTER-THE-REBUILD").await;
    let live = wait_until(Duration::from_secs(30), || async {
        node_text(&carol, store_id, taxi).await.contains("make it half past") && node_text(&carol, store_id, fx.inside).await.contains("AFTER-THE-REBUILD") && node_text(&alice, store_id, fx.inside).await.contains("AFTER-THE-REBUILD")
    })
    .await;
    assert!(live, "edits made after the rebuild reach everyone");

    // A member who is away while the new logs grow comes back to entries
    // whose numbers are all below where it had read the old logs to. It
    // reads them: what it kept of the old logs went when it met the new
    // twin's epoch.
    drop(carol);
    c.stop().await.unwrap();
    write_text(&bob, store_id, fx.inside, "bob", "WHILE-CAROL-WAS-AWAY").await;
    rename(&alice, store_id, taxi, "Taxi to the ferry").await;
    assert!(wait_until(Duration::from_secs(20), || async { node_text(&alice, store_id, fx.inside).await.contains("WHILE-CAROL-WAS-AWAY") && node_title(&bob, store_id, taxi).await == "Taxi to the ferry" }).await);
    let (mut c, carol) = start_device_in(c_dir.path()).await;
    carol.open_store(c_dir.path().join("replicas").join(format!("{store_id}.pimble"))).await.expect("the member's replica opens again");
    let caught_up = wait_until(Duration::from_secs(30), || async {
        node_text(&carol, store_id, fx.inside).await.contains("WHILE-CAROL-WAS-AWAY") && node_title(&carol, store_id, taxi).await == "Taxi to the ferry"
    })
    .await;
    assert!(caught_up, "what was appended to the new logs while a member was away reaches it when it is back");
    env.assert_nothing_hosted(&["OFFLINE-SUNHAT", "taxi at six"]).await;

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn through_the_relay_a_reader_cannot_write_and_nothing_outside_the_share_is_reached() {
    let env = spawn_env().await;
    env.holds_another_store(BOB);
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    share_with(&alice, &fx, &[(BOB, MemberRole::Reader), (CAROL, MemberRole::Editor)]).await;

    // Bob's replica: read, and refused every write here as it would be there.
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    assert_eq!(bob.list_stores().await.unwrap().iter().find(|s| s.id == store_id).unwrap().access, StoreAccess::Read);
    let refused = |result: Result<(), pimble_client::ClientError>| assert_eq!(result.expect_err("a reader's replica refuses writes").to_string(), StoreAccess::READ_ONLY_REFUSAL);
    refused(bob.create_node(store_id, Some(shared), "document", "no").await.map(|_| ()));
    refused(bob.delete_node(store_id, fx.inside).await);
    let changes = base64::engine::general_purpose::STANDARD.encode(NodeDoc::from_plain_text("no").unwrap().save());
    refused(bob.apply_edit(store_id, fx.inside, "bob", EditOperation::IncrementalChanges { changes }).await);
    write_text(&alice, store_id, fx.inside, "alice", "and sunscreen").await;
    assert!(wait_until(Duration::from_secs(15), || async { node_text(&bob, store_id, fx.inside).await.contains("and sunscreen") }).await, "and still receives everything");

    // The relay takes the token minted for this store and no other.
    let (bob_token, bob_general) = env.tokens(BOB, store_id);
    let refused_before = env.stub.relay.wrong_token.load(Ordering::SeqCst);
    assert!(PimbleClient::connect_with_auth(env.relay_url(store_id), &AuthMethod::Bearer { token: bob_general }).await.is_err());
    assert_eq!(env.stub.relay.wrong_token.load(Ordering::SeqCst), refused_before + 1, "the account's general token is refused before anything reaches the owner");

    // Straight at the owner's face, through the relay, as a reader: the
    // scope is listed and read, nothing is written, nothing else is reached.
    let reader = PimbleClient::connect_with_auth(env.relay_url(store_id), &AuthMethod::Bearer { token: bob_token }).await.expect("a member's connection through the relay");
    let listed: HashSet<VaultDocId> = reader.vault_list_docs(store_id).await.expect("the scope is listed").into_iter().map(|d| d.doc_id).collect();
    let scope: HashSet<VaultDocId> = [shared, fx.inside, fx.deeper, fx.leaf].into_iter().map(VaultDocId::Node).collect();
    assert_eq!(listed, scope, "a member is listed the share and nothing else");
    assert!(reader.vault_fetch(store_id, VaultDocId::Node(fx.inside), 0).await.is_ok());
    let blob = URL_SAFE_NO_PAD.encode(b"PB-not-really");
    assert_eq!(reader.vault_append(store_id, VaultDocId::Node(fx.inside), blob.clone()).await.expect_err("a reader's append").to_string(), StoreAccess::READ_ONLY_REFUSAL);
    for outside in [fx.private, fx.root_id] {
        let refusal = reader.vault_fetch(store_id, VaultDocId::Node(outside), 0).await.expect_err("outside the share").to_string();
        assert!(refusal.contains(pimble_server::NO_GRANT_FOR_DOCUMENT), "{refusal}");
    }

    // As an editor: the share is written, and what is outside it is no more
    // reachable than for a reader.
    let (carol_token, _) = env.tokens(CAROL, store_id);
    let editor = PimbleClient::connect_with_auth(env.relay_url(store_id), &AuthMethod::Bearer { token: carol_token }).await.expect("an editor's connection through the relay");
    for outside in [fx.private, fx.root_id] {
        let refusal = editor.vault_fetch(store_id, VaultDocId::Node(outside), 0).await.expect_err("outside the share").to_string();
        assert!(refusal.contains(pimble_server::NO_GRANT_FOR_DOCUMENT), "{refusal}");
        let refusal = editor.vault_append(store_id, VaultDocId::Node(outside), blob.clone()).await.expect_err("outside the share").to_string();
        assert!(refusal.contains(pimble_server::NO_GRANT_FOR_DOCUMENT), "{refusal}");
    }
    // The relay face is a vault and a JWT server like the hosted one: no
    // plain RPC answers, and no lifecycle RPC is a member's to call.
    assert!(editor.get_node(store_id, fx.inside).await.is_err());
    assert!(editor.close_store(store_id).await.is_err());
    assert!(editor.delete_vault_store(store_id).await.is_err());
    assert!(alice.get_node(store_id, fx.private).await.is_ok(), "and the owner's store is as it was");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn stopping_is_refused_while_a_share_stands_and_undoes_everything_after() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor)]).await;
    let store_dir = a_dir.path().join("a.pimble");

    // A share stands: refused, and nothing has changed.
    assert_eq!(alice.cloud_stop_relaying(store_id).await.expect_err("a share is in place").to_string(), pimble_server::relay_face::HAS_SHARES_REFUSAL);
    assert_eq!(alice.get_store_sync_response(store_id).await.unwrap().relay, RelaySide::Owner);
    assert!(twin_dir(a_dir.path(), store_id).exists() && env.stub.relay.serves(store_id));
    assert!(env.stub.inner.lock().unwrap().stores.iter().any(|row| row.store_id == store_id.to_string() && row.root.is_none()));

    // The share stopped, the way back is open: the record, the tunnel's
    // entry, the link, the twin and `sync.json` all go.
    alice.cloud_stop_sharing(store_id, shared).await.expect("the share is stopped first");
    alice.cloud_stop_relaying(store_id).await.expect("then the store is no longer shared from here");
    let sync = alice.get_store_sync_response(store_id).await.unwrap();
    assert!(sync.remote.is_none() && sync.relay == RelaySide::None && sync.sync_mode == StoreKind::Plain, "{sync:?}");
    assert!(!twin_dir(a_dir.path(), store_id).exists(), "the twin is deleted");
    assert!(!store_dir.join("sync.json").exists() && !store_dir.join("vault-link.json").exists());
    {
        let s = env.stub.inner.lock().unwrap();
        assert!(!s.stores.iter().any(|row| row.store_id == store_id.to_string()), "the accounts service's record is removed");
        assert!(!s.key_grants.contains_key(&store_id.to_string()));
    }
    assert!(wait_until(Duration::from_secs(10), || async { !env.stub.relay.serves(store_id) }).await, "and the relay serves it no more");
    assert_eq!(alice.cloud_stop_relaying(store_id).await.expect_err("already stopped").to_string(), pimble_server::relay_face::NOT_RELAYED_REFUSAL);

    // The store itself is untouched, and is an unshared, unhosted store
    // again: sharing says what it needs, and offers nothing by itself.
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private]);
    assert!(node_text(&alice, store_id, fx.private).await.contains("PRIVATE-DIARY-TEXT"));
    assert_eq!(
        alice.cloud_share_node(store_id, shared, "Again").await.expect_err("neither hosted nor relayed").to_string(),
        "Sharing needs this store hosted on Pimble Cloud or shared from this computer."
    );

    // And it can be shared from here again, on a twin of its own.
    alice.cloud_relay_store(store_id).await.expect("relayed again");
    let again = wait_until(Duration::from_secs(20), || async { env.stub.relay.serves(store_id) && twin_doc_count(a_dir.path(), store_id) == 6 }).await;
    assert!(again, "announced again, on a whole twin: {}", twin_doc_count(a_dir.path(), store_id));
    env.assert_nothing_hosted(&[STORE_NAME, "PRIVATE-DIARY-TEXT"]).await;

    a.stop().await.unwrap();
}
