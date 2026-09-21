//! End-to-end tests for the owner's half of sharing
//! (docs/NODE_DOCUMENT_CONTRACT.md section 5; `pimble_server::share`): the
//! `cloudShare*` RPCs and the share upkeep an owner's device runs (scope
//! sets, data-key wraps, the key sweep).
//!
//! The environment is `tests/vault_link.rs`'s: `H`, a real `PimbleServer` in
//! JWT mode holding the hosted `Vault` twins; an axum stub standing in for
//! the accounts service, extended here with the scoped members, invitations
//! and keys endpoints (`pimble-cloud`'s README, "Sharing"); and real
//! "desktop" `PimbleServer`s, one per device, each with its own temp
//! keystore and replicas directory. Three accounts to start with (the
//! owner, two to share with), and a fourth that appears later, for an
//! invitation sent before its address had an account.
//!
//! What the tests hold the owner's side to, above all: it hands over keys,
//! publishes scopes and hosts, and is never in the path of anyone's edit.
//! Two members edit the same shared folder's tree with the owner's server
//! stopped and see each other.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreAccess, StoreId, StoreKind, SyncState};
use pimble_crdt::NodeDoc;
use pimble_crypto::{derive_password_keys, wrap_account_keys, AccountKeyBlob, AccountKeys, KdfParams, KeyEnvelope};
use pimble_rpc::{EditOperation, MemberRole, ShareMemberStatus, StoreChangeKind, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;
use uuid::Uuid;

// ── Small generic helpers (mirrors tests/vault_link.rs) ───────────────────

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

async fn seed_content(client: &PimbleClient, store_id: StoreId, node_id: NodeId, client_id: &str, text: &str) {
    let doc = NodeDoc::from_plain_text(text).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.save());
    client
        .apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes })
        .await
        .expect("seed content applies");
}

/// How often a test device's key sweep runs, in place of the minute.
const SWEEP_EVERY: Duration = Duration::from_secs(1);

fn device_config(dir: &Path) -> ServerConfig {
    ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        keystore_path: Some(dir.join("keys.json")),
        credentials_path: Some(dir.join("credentials.json")),
        replicas_dir: Some(dir.join("replicas")),
        share_sweep_interval: Some(SWEEP_EVERY),
        ..Default::default()
    }
}

/// A fresh desktop-side `PimbleServer` (plain, tokenless, loopback) with
/// its own temp keystore/credentials/replicas paths.
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

// ── JWT signing ───────────────────────────────────────────────────────────

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
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());
    format!("{signing_input}.{sig_b64}")
}

// ── The accounts-service stub ──────────────────────────────────────────────
//
// `tests/vault_link.rs`'s stub, plus what sharing calls: members and
// invitations of a scope, and a scope's key envelopes. It keeps the rules of
// the real service that the code under test leans on: a scope's listing
// names the grants *of that scope* (never the owner's whole-store grant),
// an address without an account is invited and claims its invitation when
// the account appears, removing a member takes their envelopes for that
// scope with them, and an envelope is only taken for an account that holds a
// grant covering its scope.

/// One grant: `GET /stores` answers one row per grant of the caller's.
struct StubStoreRow {
    user_id: String,
    store_id: String,
    name: String,
    kind: String,
    created_at: String,
    role: String,
    /// The shared node, for a share.
    root: Option<String>,
    shared_by: Option<String>,
}

struct StubInvitation {
    store_id: String,
    email: String,
    role: String,
    root: Option<String>,
    name: String,
    invited_by: String,
}

struct KeyGrantRow {
    user_id: String,
    key_id: String,
    envelope: KeyEnvelope,
    /// The share the key is for; `None` for the store key.
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
    invitations: Vec<StubInvitation>,
    key_grants: HashMap<String, Vec<KeyGrantRow>>,
    signing_key: SigningKey,
    kid: String,
    issuer: String,
    h_client: Option<Arc<PimbleClient>>,
    h_rpc_url: String,
    h_stores_dir: PathBuf,
    /// Every call the sharing endpoints took, for a test to assert none was made.
    sharing_calls: usize,
}

struct Stub {
    inner: Mutex<StubInner>,
    /// The members endpoints answer 503 while set: "the service is unreachable".
    members_down: AtomicBool,
}

type StubState = Arc<Stub>;

fn stub_router(state: StubState) -> Router {
    Router::new()
        .route("/api/v1/kdf", get(stub_kdf))
        .route("/api/v1/login", post(stub_login))
        .route("/api/v1/me/keys", get(stub_me_keys))
        .route("/api/v1/token", post(stub_mint_token))
        .route("/api/v1/stores", get(stub_list_stores).post(stub_create_store))
        .route("/api/v1/stores/{store_id}/keys", get(stub_get_keys).put(stub_put_keys))
        .route("/api/v1/stores/{store_id}/members", get(stub_list_members).put(stub_put_member))
        .route("/api/v1/stores/{store_id}/members/{user_id}", delete(stub_delete_member))
        .route("/api/v1/stores/{store_id}/invitations/{email}", delete(stub_delete_invitation))
        .route("/jwks.json", get(stub_jwks))
        .with_state(state)
}

impl StubInner {
    /// The account a request's `Authorization: Bearer <session>` names.
    fn account_of(&self, headers: &HeaderMap) -> &StubAccount {
        let session = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
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

    /// An invitation becomes a grant on the scope it named the moment its
    /// address has an account.
    fn claim_invitations(&mut self, email: &str) {
        let Some(user_id) = self.accounts.iter().find(|a| a.email == email).map(|a| a.user_id.clone()) else { return };
        let (claimed, kept): (Vec<StubInvitation>, Vec<StubInvitation>) = std::mem::take(&mut self.invitations).into_iter().partition(|i| i.email == email);
        self.invitations = kept;
        for invitation in claimed {
            let kind = self.stores.iter().find(|row| row.store_id == invitation.store_id).map(|row| row.kind.clone()).unwrap_or_else(|| "vault".into());
            self.stores.push(StubStoreRow {
                user_id: user_id.clone(),
                store_id: invitation.store_id,
                name: invitation.name,
                kind,
                created_at: chrono::Utc::now().to_rfc3339(),
                role: invitation.role,
                root: invitation.root,
                shared_by: Some(invitation.invited_by),
            });
        }
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

/// The token as the accounts service mints it: a role for a store held
/// whole, a role per shared root for one held by shares, and the whole
/// store winning where an account holds both.
async fn stub_mint_token(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let (token, rpc_url) = {
        let s = state.inner.lock().unwrap();
        let account = s.account_of(&headers);
        let mut stores_claim = serde_json::Map::new();
        for row in s.stores.iter().filter(|row| row.user_id == account.user_id && row.root.is_none()) {
            stores_claim.insert(row.store_id.clone(), json!(row.role));
        }
        for row in s.stores.iter().filter(|row| row.user_id == account.user_id) {
            let Some(root) = &row.root else { continue };
            let claim = stores_claim.entry(row.store_id.clone()).or_insert_with(|| json!({ "roots": {} }));
            if let Some(roots) = claim.get_mut("roots") {
                roots[root] = json!(row.role);
            }
        }
        let token = make_jwt(&s.signing_key, &s.kid, &s.issuer, &account.user_id, &account.email, &stores_claim);
        (token, s.h_rpc_url.clone())
    };
    Json(json!({ "token": token, "exp": chrono::Utc::now().timestamp() + 300, "rpc_url": rpc_url }))
}

async fn stub_list_stores(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_of(&headers);
    let arr: Vec<_> = s
        .stores
        .iter()
        .filter(|st| st.user_id == account.user_id)
        .map(|st| {
            json!({ "store_id": st.store_id, "name": st.name, "role": st.role, "kind": st.kind, "created_at": st.created_at, "root": st.root, "shared_by": st.shared_by })
        })
        .collect();
    Json(json!(arr))
}

async fn stub_create_store(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let name = req["name"].as_str().unwrap_or("store").to_string();
    let kind_str = req["kind"].as_str().unwrap_or("plain").to_string();
    let kind = if kind_str == "vault" { StoreKind::Vault } else { StoreKind::Plain };
    let requested_id = req["store_id"].as_str().map(|s| StoreId::parse(s).expect("valid store id"));

    let (h_client, stores_dir) = {
        let s = state.inner.lock().unwrap();
        (Arc::clone(s.h_client.as_ref().expect("H client wired before any /stores call")), s.h_stores_dir.clone())
    };
    let dir_name = requested_id.map(|id| id.to_string()).unwrap_or_else(|| Uuid::new_v4().to_string());
    let path = stores_dir.join(format!("{dir_name}.pimble"));
    let (created_id, _root) = h_client.create_store_with(&path, &name, kind, requested_id).await.expect("H creates the hosted store");

    let created_at = chrono::Utc::now().to_rfc3339();
    {
        let mut s = state.inner.lock().unwrap();
        let user_id = s.account_of(&headers).user_id.clone();
        s.stores.push(StubStoreRow {
            user_id,
            store_id: created_id.to_string(),
            name: name.clone(),
            kind: kind_str.clone(),
            created_at: created_at.clone(),
            role: "owner".into(),
            root: None,
            shared_by: None,
        });
    }

    Json(json!({ "store_id": created_id.to_string(), "name": name, "role": "owner", "kind": kind_str, "created_at": created_at }))
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
        .map(|rows| {
            rows.iter()
                .filter(|g| g.user_id == account.user_id && g.root.as_ref() == root)
                .map(|g| json!({ "key_id": g.key_id, "envelope": g.envelope }))
                .collect()
        })
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
        if root.is_some() {
            s.sharing_calls += 1;
        }
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

/// The members and invitations of one scope. The owner's grant is on the
/// whole store, so no share's listing names it.
async fn stub_list_members(
    State(state): State<StubState>,
    AxPath(store_id): AxPath<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.members_down.load(Ordering::SeqCst) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let mut s = state.inner.lock().unwrap();
    s.sharing_calls += 1;
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
    if caller_is_owner {
        for invitation in s.invitations.iter().filter(|i| i.store_id == store_id && i.root == root) {
            share_name.get_or_insert_with(|| invitation.name.clone());
            members.push(json!({ "user_id": null, "email": invitation.email, "role": invitation.role, "root": root, "status": "invited", "has_key": false, "public_keys": null }));
        }
    }
    let share_name = root.as_ref().map(|_| share_name.unwrap_or_else(|| "Shared folder".into()));
    Ok(Json(json!({ "members": members, "share_name": share_name })))
}

async fn stub_put_member(
    State(state): State<StubState>,
    AxPath(store_id): AxPath<String>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.members_down.load(Ordering::SeqCst) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let mut s = state.inner.lock().unwrap();
    s.sharing_calls += 1;
    let caller = s.account_of(&headers);
    let (caller_id, caller_email) = (caller.user_id.clone(), caller.email.clone());
    if !s.is_owner(&caller_id, &store_id) {
        return Err(StatusCode::FORBIDDEN);
    }
    let email = req["email"].as_str().unwrap_or_default().trim().to_string();
    let role = req["role"].as_str().unwrap_or_default().to_string();
    let root = req["root"].as_str().map(String::from);
    let name = req["name"].as_str().unwrap_or_default().to_string();
    // A share's members are editors or readers, and a share has a name.
    if root.is_some() && (role == "owner" || name.trim().is_empty()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let target = s.accounts.iter().find(|a| a.email == email).map(|a| (a.user_id.clone(), a.account_public_keys.clone()));
    match target {
        Some((user_id, public_keys)) => {
            match s.stores.iter_mut().find(|row| row.user_id == user_id && row.store_id == store_id && row.root == root) {
                Some(row) => row.role = role.clone(),
                None => {
                    let kind = s.stores.iter().find(|row| row.store_id == store_id).map(|row| row.kind.clone()).unwrap_or_else(|| "vault".into());
                    s.stores.push(StubStoreRow {
                        user_id: user_id.clone(),
                        store_id: store_id.clone(),
                        name: name.clone(),
                        kind,
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
        None => {
            match s.invitations.iter_mut().find(|i| i.store_id == store_id && i.email == email && i.root == root) {
                Some(invitation) => invitation.role = role.clone(),
                None => s.invitations.push(StubInvitation { store_id: store_id.clone(), email: email.clone(), role: role.clone(), root: root.clone(), name, invited_by: caller_email }),
            }
            Ok(Json(json!({ "user_id": null, "email": email, "role": role, "root": root, "status": "invited", "has_key": false, "public_keys": null })))
        }
    }
}

/// One membership goes, and that account's envelopes for that scope with it.
async fn stub_delete_member(
    State(state): State<StubState>,
    AxPath((store_id, user_id)): AxPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.members_down.load(Ordering::SeqCst) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let mut s = state.inner.lock().unwrap();
    s.sharing_calls += 1;
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

async fn stub_delete_invitation(
    State(state): State<StubState>,
    AxPath((store_id, email)): AxPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.members_down.load(Ordering::SeqCst) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let mut s = state.inner.lock().unwrap();
    s.sharing_calls += 1;
    let root = query.get("root").cloned();
    s.invitations.retain(|i| !(i.store_id == store_id && i.email == email && i.root == root));
    Ok(Json(json!({})))
}

async fn stub_jwks(State(state): State<StubState>) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let x = URL_SAFE_NO_PAD.encode(s.signing_key.verifying_key().to_bytes());
    Json(json!({ "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": s.kid, "x": x } ] }))
}

fn cheap_kdf_params() -> KdfParams {
    let mut params = KdfParams::generate();
    params.m_cost = 8 * 1024;
    params.t_cost = 1;
    params
}

/// A stub account with real key material.
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
/// Has no account until a test says so.
const DAVE: (&str, &str) = ("dave@example.com", "late to the stable");

struct Env {
    stub_url: String,
    stub: StubState,
    // Kept alive for the test's duration (dropping it would stop H).
    _h: PimbleServer,
    /// A client to `H` with the service token, for out-of-band verification.
    h_admin: PimbleClient,
    h_stores_dir: PathBuf,
    _h_dir: tempfile::TempDir,
    /// Every device's vault link reaches `H` through this, so a test can
    /// take the network away and give it back.
    relay: Relay,
}

/// A TCP relay in front of `H` that a test can cut.
#[derive(Clone)]
struct Relay {
    addr: std::net::SocketAddr,
    blocked: Arc<AtomicBool>,
    conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl Relay {
    async fn start(target: std::net::SocketAddr) -> Relay {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = Relay { addr: listener.local_addr().unwrap(), blocked: Arc::new(AtomicBool::new(false)), conns: Arc::new(Mutex::new(Vec::new())) };
        let this = relay.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else { break };
                if this.blocked.load(Ordering::SeqCst) {
                    drop(inbound);
                    continue;
                }
                let task = tokio::spawn(async move {
                    if let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                this.conns.lock().unwrap().push(task.abort_handle());
            }
        });
        relay
    }

    fn cut(&self) {
        self.blocked.store(true, Ordering::SeqCst);
        for conn in self.conns.lock().unwrap().drain(..) {
            conn.abort();
        }
    }

    fn restore(&self) {
        self.blocked.store(false, Ordering::SeqCst);
    }
}

impl Env {
    fn h_store_path(&self, store_id: StoreId) -> PathBuf {
        self.h_stores_dir.join(format!("{store_id}.pimble"))
    }

    fn hosted_store_contains_plaintext(&self, store_id: StoreId, needle: &str) -> bool {
        contains_bytes_recursive(&self.h_store_path(store_id).join("vault"), needle.as_bytes())
    }

    async fn sign_in(&self, client: &PimbleClient, who: (&str, &str)) {
        client.cloud_sign_in(&self.stub_url, who.0, who.1).await.expect("sign in");
    }

    /// The address gets an account (verified, with keys), and claims the
    /// invitations that were waiting for it.
    fn account_appears(&self, who: (&str, &str)) {
        let account = stub_account(who.0, who.1);
        let mut s = self.stub.inner.lock().unwrap();
        s.accounts.push(account);
        s.claim_invitations(who.0);
    }

    /// Whether `email`'s account holds an envelope for the share rooted at `root`.
    fn has_share_key(&self, store_id: StoreId, root: NodeId, email: &str) -> bool {
        let s = self.stub.inner.lock().unwrap();
        let Some(user_id) = s.accounts.iter().find(|a| a.email == email).map(|a| a.user_id.clone()) else { return false };
        s.has_key(&user_id, &store_id.to_string(), &Some(root.to_string()))
    }

    /// The scoped grants and invitations the accounts service holds for a share.
    fn scoped_rows(&self, store_id: StoreId, root: NodeId) -> usize {
        let s = self.stub.inner.lock().unwrap();
        let (store, root) = (store_id.to_string(), Some(root.to_string()));
        s.stores.iter().filter(|row| row.store_id == store && row.root == root).count() + s.invitations.iter().filter(|i| i.store_id == store && i.root == root).count()
    }

    /// The published scope of `root` on the hosted server, `None` when it has none.
    async fn scope_on_h(&self, store_id: StoreId, root: NodeId) -> Option<Vec<NodeId>> {
        self.h_admin.get_scopes(store_id).await.ok()?.into_iter().find(|scope| scope.root == root).map(|scope| scope.doc_ids)
    }

    /// The scope keys a hosted document's data key is wrapped under.
    async fn wrap_key_ids(&self, store_id: StoreId, doc: NodeId) -> Vec<Uuid> {
        self.h_admin
            .vault_fetch(store_id, VaultDocId::Node(doc), u64::MAX)
            .await
            .ok()
            .and_then(|fetch| fetch.keys)
            .map(|keys| keys.wraps.iter().map(|w| w.scope_key_id).collect())
            .unwrap_or_default()
    }

    async fn head_on_h(&self, store_id: StoreId, doc: NodeId) -> u64 {
        self.h_admin.vault_fetch(store_id, VaultDocId::Node(doc), u64::MAX).await.map(|fetch| fetch.head).unwrap_or(0)
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
    let signing_key = fresh_signing_key();
    let kid = "test-kid".to_string();
    let issuer = "https://cloud.test".to_string();

    let h_dir = tempfile::tempdir().unwrap();
    let h_stores_dir = h_dir.path().join("stores");
    std::fs::create_dir_all(&h_stores_dir).unwrap();

    let state: StubState = Arc::new(Stub {
        inner: Mutex::new(StubInner {
            accounts: vec![stub_account(ALICE.0, ALICE.1), stub_account(BOB.0, BOB.1), stub_account(CAROL.0, CAROL.1)],
            stores: Vec::new(),
            invitations: Vec::new(),
            key_grants: HashMap::new(),
            signing_key,
            kid,
            issuer: issuer.clone(),
            h_client: None,
            h_rpc_url: String::new(),
            h_stores_dir: h_stores_dir.clone(),
            sharing_calls: 0,
        }),
        members_down: AtomicBool::new(false),
    });

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
    let h_client = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token.clone() }).await.expect("service client connects to H");
    let h_admin = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token }).await.expect("admin client connects to H");

    let relay = Relay::start(h.addr()).await;
    {
        let mut s = state.inner.lock().unwrap();
        s.h_client = Some(Arc::new(h_client));
        s.h_rpc_url = format!("ws://{}", relay.addr);
    }

    Env { stub_url, stub: state, _h: h, h_admin, h_stores_dir, _h_dir: h_dir, relay }
}

/// The store keys a device's keystore holds for `store_id`, off its `keys.json`.
fn scope_keys_in(server_dir: &Path, store_id: StoreId) -> Vec<(Uuid, pimble_crypto::SymmetricKey)> {
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(server_dir.join("keys.json")).expect("the keystore file")).unwrap();
    json["store_keys"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry["store_id"].as_str() == Some(store_id.to_string().as_str()))
        .map(|entry| {
            let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(entry["key"].as_str().unwrap()).unwrap().try_into().unwrap();
            (entry["key_id"].as_str().unwrap().parse().unwrap(), pimble_crypto::SymmetricKey(bytes))
        })
        .collect()
}

async fn wait_all_seeded(env: &Env, store_id: StoreId, docs: &[NodeId]) {
    let seeded = wait_until(Duration::from_secs(15), || async {
        let listed = env.h_admin.vault_list_docs(store_id).await.unwrap_or_default();
        docs.iter().all(|id| listed.iter().any(|d| d.doc_id == VaultDocId::Node(*id) && d.head > 0))
    })
    .await;
    assert!(seeded, "every document must reach the hosted twin");
}

fn sorted(mut ids: Vec<NodeId>) -> Vec<String> {
    let mut out: Vec<String> = ids.drain(..).map(|id| id.to_string()).collect();
    out.sort();
    out
}

/// Alice's hosted store, the shape most tests start from:
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

async fn hosted_fixture(env: &Env, alice: &PimbleClient, alice_dir: &Path) -> Fixture {
    env.sign_in(alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(alice_dir.join("a.pimble"), "Alice's Notes").await.unwrap();
    let shared = alice.create_node(store_id, Some(root_id), "folder", "Holiday").await.unwrap();
    let inside = alice.create_node(store_id, Some(shared), "document", "Packing").await.unwrap();
    let deeper = alice.create_node(store_id, Some(shared), "folder", "Tickets").await.unwrap();
    let leaf = alice.create_node(store_id, Some(deeper), "document", "Train").await.unwrap();
    let private = alice.create_node(store_id, Some(root_id), "document", "Diary").await.unwrap();
    seed_content(alice, store_id, inside, "seed", "socks and a map").await;
    seed_content(alice, store_id, private, "seed", "PRIVATE-DIARY-TEXT").await;
    // A person's content edit earns a `modified_at` stamp on the server's
    // flush debounce (750 ms). Let it land before hosting, so that every
    // append a test counts later is one the test made.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    alice.cloud_host_store(store_id).await.unwrap();
    wait_all_seeded(env, store_id, &[root_id, shared, inside, deeper, leaf, private]).await;
    Fixture { store_id, root_id, shared, inside, deeper, leaf, private }
}

/// A member's device: signed in, the share added as a partial replica, and
/// its first document pulled.
async fn member_device(env: &Env, who: (&str, &str), fx: &Fixture) -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let (server, client, dir) = start_local_server().await;
    env.sign_in(&client, who).await;
    client.cloud_add_hosted_store(fx.store_id).await.expect("a share is added like any hosted store");
    let pulled = wait_until(Duration::from_secs(15), || async { node_text(&client, fx.store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "{}'s replica pulls the scope and reads it with the share's key", who.0);
    (server, client, dir)
}

// ── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn sharing_a_folder_gives_a_member_its_subtree_and_both_sides_edit_the_tree() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().expect("hosting made a store key");
    let mut alice_changes = alice.subscribe_store_changes(store_id).await.unwrap();

    // Share the folder: the owner is the only member, the marker is on the
    // node, the scope is published, and every document under it (and none
    // outside it) is wrapped under the share's key beside the store's.
    let answer = alice.cloud_share_node(store_id, shared, "Holiday Plans").await.expect("sharing a folder of a hosted store");
    assert_eq!(answer.share.name, "Holiday Plans");
    assert_eq!((answer.share.store_id, answer.share.node_id), (store_id, shared));
    assert!(matches!(answer.share.state, SyncState::Synced { .. }), "the wraps and the scope are in place when the call answers: {:?}", answer.share.state);
    assert_eq!(answer.members.iter().map(|m| (m.email.as_str(), m.role, m.status)).collect::<Vec<_>>(), vec![(ALICE.0, MemberRole::Owner, ShareMemberStatus::Active)]);

    let marker = alice.get_node(store_id, shared).await.unwrap().metadata.share().expect("the shared node carries the marker");
    assert_eq!((marker.name.as_str(), marker.url.as_str()), ("Holiday Plans", env.stub_url.as_str()));
    assert!(scope_keys_in(a_dir.path(), store_id).iter().any(|(id, _)| *id == marker.key_id), "the share key is in the keystore");
    assert!(env.has_share_key(store_id, shared, ALICE.0), "and wrapped to the owner's own account, for their other devices");
    assert_eq!(sorted(env.scope_on_h(store_id, shared).await.expect("a published scope")), sorted(vec![fx.inside, fx.deeper, fx.leaf]));
    for doc in [shared, fx.inside, fx.deeper, fx.leaf] {
        assert_eq!(sorted_uuids(env.wrap_key_ids(store_id, doc).await), sorted_uuids(vec![store_key_id, marker.key_id]), "{doc}");
    }
    for doc in [fx.root_id, fx.private] {
        assert_eq!(env.wrap_key_ids(store_id, doc).await, vec![store_key_id], "a document outside the share is not wrapped under its key");
    }
    let told = tokio::time::timeout(Duration::from_secs(5), async {
        let mut states = Vec::new();
        while let Some(Ok(notif)) = alice_changes.next().await {
            let StoreChangeKind::ShareStateChanged { node_id, state } = notif.change_kind else { continue };
            assert_eq!(node_id, shared);
            let synced = matches!(state, SyncState::Synced { .. });
            states.push(state);
            if synced {
                break;
            }
        }
        states
    })
    .await
    .expect("the share's state reaches the store's subscribers");
    assert_eq!(told.len(), 1, "transitions only, and a share that was kept up at once was never anything else: {told:?}");
    assert_eq!(alice.cloud_share_node(store_id, shared, "Again").await.expect_err("shared already").to_string(), "This node is shared already.");

    // Invite Bob, who has an account: the grant, and the key at once.
    let answer = alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.expect("inviting an editor");
    assert_eq!(
        answer.members.iter().map(|m| (m.email.as_str(), m.role, m.status)).collect::<Vec<_>>(),
        vec![(ALICE.0, MemberRole::Owner, ShareMemberStatus::Active), (BOB.0, MemberRole::Editor, ShareMemberStatus::Active)]
    );
    assert!(env.has_share_key(store_id, shared, BOB.0));
    let refused = alice.cloud_share_invite(store_id, shared, CAROL.0, MemberRole::Owner).await.expect_err("owner is no role of a share");
    assert!(refused.to_string().contains("editors or readers"), "{refused}");
    assert_eq!(env.scoped_rows(store_id, shared), 1, "and it changed nothing");

    // Bob's replica: the folder's subtree and nothing else.
    let (mut b, bob, b_dir) = start_local_server().await;
    env.sign_in(&bob, BOB).await;
    let rows = bob.cloud_list_hosted_stores().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].root, rows[0].shared_by.as_deref(), rows[0].name.as_str(), rows[0].role.as_str()), (Some(shared), Some(ALICE.0), "Holiday Plans", "editor"));
    let store = bob.cloud_add_hosted_store(store_id).await.expect("a share is added like any hosted store");
    assert_eq!((store.roots.clone(), store.access, store.name.as_str()), (vec![shared], StoreAccess::Full, "Holiday Plans"));
    let pulled = wait_until(Duration::from_secs(15), || async { node_text(&bob, store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "the scope's documents are pulled and read with the share's key");
    assert_eq!(child_ids(&bob, store_id, shared).await, vec![fx.inside, fx.deeper]);
    assert_eq!(child_ids(&bob, store_id, fx.deeper).await, vec![fx.leaf]);
    for outside in [fx.private, fx.root_id] {
        assert!(bob.get_node(store_id, outside).await.is_err(), "a node outside the scope is not held");
    }
    assert!(!contains_bytes_recursive(b_dir.path(), b"PRIVATE-DIARY-TEXT"));
    assert!(!contains_bytes_recursive(b_dir.path(), b"Alice's Notes"), "the owner's store name never reaches a recipient");

    // The member edits the tree and the owner's store follows: create,
    // rename, move (inside the share), delete.
    let made = bob.create_node(store_id, Some(fx.deeper), "document", "Ferry").await.unwrap();
    seed_content(&bob, store_id, made, "bob", "ferry at nine").await;
    let created = wait_until(Duration::from_secs(20), || async {
        child_ids(&alice, store_id, fx.deeper).await == vec![fx.leaf, made] && node_text(&alice, store_id, made).await.contains("ferry at nine")
    })
    .await;
    assert!(created, "a member's create reaches the owner, readable");

    // A rename is one append, the member's: the owner's device receives it
    // and writes nothing to the document. Keys and scope are all its upkeep does.
    let head_before = env.head_on_h(store_id, fx.inside).await;
    rename(&bob, store_id, fx.inside, "Packing list").await;
    let renamed = wait_until(Duration::from_secs(10), || async { node_title(&alice, store_id, fx.inside).await == "Packing list" }).await;
    assert!(renamed, "a member's rename reaches the owner");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(env.head_on_h(store_id, fx.inside).await, head_before + 1, "the owner's device is not in the path of the member's edit");

    bob.move_node(store_id, made, shared, None).await.expect("a move inside the share");
    let moved = wait_until(Duration::from_secs(10), || async {
        child_ids(&alice, store_id, shared).await == vec![fx.inside, fx.deeper, made] && child_ids(&alice, store_id, fx.deeper).await == vec![fx.leaf]
    })
    .await;
    assert!(moved, "a member's move reaches the owner: {:?}", child_ids(&alice, store_id, shared).await);
    bob.delete_node(store_id, fx.leaf).await.expect("a delete inside the share");
    let deleted = wait_until(Duration::from_secs(10), || async { child_ids(&alice, store_id, fx.deeper).await.is_empty() && alice.get_node(store_id, fx.leaf).await.is_err() }).await;
    assert!(deleted, "a member's delete reaches the owner");
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private], "and nothing a member did touched the rest of the owner's tree");

    // What the owner's upkeep did about the member's document: the store
    // key's wrap (the member could only wrap under the share's), and a
    // scope that names it, and keeps naming the deleted one (a tombstone
    // has to reach every member).
    let kept_up = wait_until(Duration::from_secs(15), || async {
        sorted_uuids(env.wrap_key_ids(store_id, made).await) == sorted_uuids(vec![store_key_id, marker.key_id])
            && env.scope_on_h(store_id, shared).await.map(sorted) == Some(sorted(vec![fx.inside, fx.deeper, fx.leaf, made]))
    })
    .await;
    assert!(kept_up, "wraps {:?}, scope {:?}", env.wrap_key_ids(store_id, made).await, env.scope_on_h(store_id, shared).await);

    // And the reverse: the owner creates, renames, moves and deletes, and
    // the member's replica follows.
    let hotel = alice.create_node(store_id, Some(shared), "document", "Hotel").await.unwrap();
    seed_content(&alice, store_id, hotel, "alice", "two nights").await;
    let created = wait_until(Duration::from_secs(30), || async { node_text(&bob, store_id, hotel).await.contains("two nights") }).await;
    assert!(created, "the owner's create reaches the member once the scope names it");
    rename(&alice, store_id, fx.deeper, "Travel").await;
    alice.move_node(store_id, hotel, fx.deeper, None).await.unwrap();
    alice.delete_node(store_id, made).await.unwrap();
    let followed = wait_until(Duration::from_secs(15), || async {
        node_title(&bob, store_id, fx.deeper).await == "Travel"
            && child_ids(&bob, store_id, fx.deeper).await == vec![hotel]
            && child_ids(&bob, store_id, shared).await == vec![fx.inside, fx.deeper]
    })
    .await;
    assert!(followed, "shared: {:?}, deeper: {:?}", child_ids(&bob, store_id, shared).await, child_ids(&bob, store_id, fx.deeper).await);
    assert_eq!(child_ids(&alice, store_id, shared).await, vec![fx.inside, fx.deeper]);
    assert!(!env.hosted_store_contains_plaintext(store_id, "ferry at nine"));
    assert!(!contains_bytes_recursive(b_dir.path(), b"PRIVATE-DIARY-TEXT"));

    // `cloudShareInfo` and removing a member, by address.
    let info = alice.cloud_share_info(store_id, shared).await.unwrap();
    assert_eq!(info.members.len(), 2);
    let after = alice.cloud_share_remove_member(store_id, shared, BOB.0).await.expect("removing a member");
    assert_eq!(after.members.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(), vec![ALICE.0]);
    assert!(!env.has_share_key(store_id, shared, BOB.0), "the service stops handing them the key");
    let nobody = alice.cloud_share_remove_member(store_id, shared, BOB.0).await.expect_err("not a member any more").to_string();
    assert!(nobody.contains("is not a member of this share"), "{nobody}");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

fn sorted_uuids(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

#[tokio::test]
async fn two_members_edit_the_same_folder_with_the_owners_server_stopped() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, shared, CAROL.0, MemberRole::Editor).await.unwrap();
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;

    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (mut c, carol, _c_dir) = member_device(&env, CAROL, &fx).await;

    // Every device of the owner's is off from here on.
    drop(alice);
    a.stop().await.unwrap();
    let scope_while_off = env.scope_on_h(store_id, shared).await;

    // Create: Bob's new document reaches Carol, readable.
    let ferry = bob.create_node(store_id, Some(shared), "document", "Ferry").await.unwrap();
    seed_content(&bob, store_id, ferry, "bob", "ferry at nine").await;
    let seen = wait_until(Duration::from_secs(20), || async {
        child_ids(&carol, store_id, shared).await == vec![fx.inside, fx.deeper, ferry] && node_text(&carol, store_id, ferry).await.contains("ferry at nine")
    })
    .await;
    assert!(seen, "a member's create reaches another member with no owner device on: {:?}", child_ids(&carol, store_id, shared).await);

    // Rename, by the other one.
    rename(&carol, store_id, ferry, "Night ferry").await;
    assert!(wait_until(Duration::from_secs(10), || async { node_title(&bob, store_id, ferry).await == "Night ferry" }).await, "a rename");

    // Move inside the share, and an edit to what was moved.
    carol.move_node(store_id, ferry, fx.deeper, Some(0)).await.unwrap();
    seed_content(&carol, store_id, ferry, "carol", "cabin booked").await;
    let moved = wait_until(Duration::from_secs(10), || async {
        child_ids(&bob, store_id, fx.deeper).await == vec![ferry, fx.leaf]
            && child_ids(&bob, store_id, shared).await == vec![fx.inside, fx.deeper]
            && node_text(&bob, store_id, ferry).await.contains("cabin booked")
    })
    .await;
    assert!(moved, "a move: {:?}", child_ids(&bob, store_id, fx.deeper).await);

    // Delete.
    bob.delete_node(store_id, fx.leaf).await.unwrap();
    let deleted = wait_until(Duration::from_secs(10), || async { child_ids(&carol, store_id, fx.deeper).await == vec![ferry] && carol.get_node(store_id, fx.leaf).await.is_err() }).await;
    assert!(deleted, "a delete");

    // Both at once, in the same folder.
    let from_bob = bob.create_node(store_id, Some(fx.deeper), "document", "Bus").await.unwrap();
    let from_carol = carol.create_node(store_id, Some(fx.deeper), "document", "Tram").await.unwrap();
    let converged = wait_until(Duration::from_secs(20), || async {
        let (at_bob, at_carol) = (child_ids(&bob, store_id, fx.deeper).await, child_ids(&carol, store_id, fx.deeper).await);
        at_bob == at_carol && sorted(at_bob) == sorted(vec![ferry, from_bob, from_carol])
    })
    .await;
    assert!(converged, "bob: {:?}, carol: {:?}", child_ids(&bob, store_id, fx.deeper).await, child_ids(&carol, store_id, fx.deeper).await);
    assert_eq!(env.scope_on_h(store_id, shared).await.map(|docs| docs.len()), scope_while_off.map(|docs| docs.len() + 3), "the hosted server put the members' documents in the scope itself");
    assert_eq!(env.wrap_key_ids(store_id, ferry).await, vec![share_key_id], "wrapped under the one scope key its maker holds");

    // The owner's device comes back and converges to the same tree; its
    // upkeep wraps what the members made under the store key and confirms
    // the scope.
    let (mut a, alice) = start_device_in(a_dir.path()).await;
    alice.open_store(a_dir.path().join("a.pimble")).await.expect("the owner's store reopens");
    let caught_up = wait_until(Duration::from_secs(30), || async {
        child_ids(&alice, store_id, fx.deeper).await == child_ids(&bob, store_id, fx.deeper).await
            && child_ids(&alice, store_id, shared).await == vec![fx.inside, fx.deeper]
            && node_text(&alice, store_id, ferry).await.contains("cabin booked")
            && node_title(&alice, store_id, ferry).await == "Night ferry"
    })
    .await;
    assert!(caught_up, "the owner's store follows what the members did while it was off: {:?}", child_ids(&alice, store_id, fx.deeper).await);
    let kept_up = wait_until(Duration::from_secs(20), || async {
        let mut all = true;
        for doc in [ferry, from_bob, from_carol] {
            all &= sorted_uuids(env.wrap_key_ids(store_id, doc).await) == sorted_uuids(vec![store_key_id, share_key_id]);
        }
        all && env.scope_on_h(store_id, shared).await.map(sorted) == Some(sorted(vec![fx.inside, fx.deeper, fx.leaf, ferry, from_bob, from_carol]))
    })
    .await;
    assert!(kept_up, "scope {:?}", env.scope_on_h(store_id, shared).await);
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private]);

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn a_node_moved_into_a_share_reaches_its_member_and_one_moved_out_stops_arriving() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let (mut b, bob, b_dir) = member_device(&env, BOB, &fx).await;

    // In: a folder with a document in it, from outside the share. Wrap and
    // scope, and the member's replica asks for what its list now names.
    let budget = alice.create_node(store_id, Some(fx.root_id), "folder", "Budget").await.unwrap();
    let sums = alice.create_node(store_id, Some(budget), "document", "Sums").await.unwrap();
    seed_content(&alice, store_id, sums, "alice", "BUDGET-FIGURES").await;
    wait_all_seeded(&env, store_id, &[budget, sums]).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(env.wrap_key_ids(store_id, sums).await, vec![store_key_id]);
    assert!(bob.get_node(store_id, sums).await.is_err() && !contains_bytes_recursive(b_dir.path(), b"BUDGET-FIGURES"), "outside the share it is none of the member's");

    alice.move_node(store_id, budget, shared, None).await.unwrap();
    let wrapped = wait_until(Duration::from_secs(15), || async {
        sorted_uuids(env.wrap_key_ids(store_id, sums).await) == sorted_uuids(vec![store_key_id, share_key_id])
            && env.scope_on_h(store_id, shared).await.is_some_and(|docs| docs.contains(&budget) && docs.contains(&sums))
    })
    .await;
    assert!(wrapped, "a document entering the share is wrapped under its key and named by its scope");
    let arrived = wait_until(Duration::from_secs(40), || async {
        node_text(&bob, store_id, sums).await.contains("BUDGET-FIGURES") && child_ids(&bob, store_id, shared).await == vec![fx.inside, fx.deeper, budget]
    })
    .await;
    assert!(arrived, "and becomes readable to the member");
    seed_content(&bob, store_id, sums, "bob", "and a contingency").await;
    assert!(wait_until(Duration::from_secs(10), || async { node_text(&alice, store_id, sums).await.contains("and a contingency") }).await, "which they edit like the rest");

    // Out: the list the member holds stops naming it, the scope stops
    // holding it, and nothing about it arrives any more.
    alice.move_node(store_id, fx.inside, fx.root_id, None).await.unwrap();
    let out = wait_until(Duration::from_secs(15), || async { env.scope_on_h(store_id, shared).await.is_some_and(|docs| !docs.contains(&fx.inside)) }).await;
    assert!(out, "a document leaving the share leaves its scope");
    assert!(wait_until(Duration::from_secs(10), || async { child_ids(&bob, store_id, shared).await == vec![fx.deeper, budget] }).await);
    seed_content(&alice, store_id, fx.inside, "alice", "AFTER-MOVING-OUT").await;
    rename(&alice, store_id, fx.inside, "Private packing").await;
    let pushed = wait_until(Duration::from_secs(10), || async { env.head_on_h(store_id, fx.inside).await >= 5 }).await;
    assert!(pushed, "the owner's later edits are hosted as ever");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!contains_bytes_recursive(b_dir.path(), b"AFTER-MOVING-OUT"), "and never reach the member");
    assert_ne!(node_title(&bob, store_id, fx.inside).await, "Private packing");
    assert_eq!(child_ids(&bob, store_id, shared).await, vec![fx.deeper, budget]);

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn a_reader_reads_the_share_and_cannot_write() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    let answer = alice.cloud_share_invite(store_id, shared, CAROL.0, MemberRole::Reader).await.unwrap();
    assert_eq!(answer.members.last().map(|m| (m.email.as_str(), m.role, m.status)), Some((CAROL.0, MemberRole::Reader, ShareMemberStatus::Active)));

    let (mut c, carol, _c_dir) = member_device(&env, CAROL, &fx).await;
    assert_eq!(carol.list_stores().await.unwrap().iter().find(|s| s.id == store_id).unwrap().access, StoreAccess::Read);

    let heads_before: HashMap<VaultDocId, u64> = env.h_admin.vault_list_docs(store_id).await.unwrap().into_iter().map(|d| (d.doc_id, d.head)).collect();
    let refused = |result: Result<(), pimble_client::ClientError>| assert_eq!(result.expect_err("a reader's replica refuses writes").to_string(), StoreAccess::READ_ONLY_REFUSAL);
    refused(carol.create_node(store_id, Some(shared), "document", "no").await.map(|_| ()));
    refused(carol.move_node(store_id, fx.leaf, shared, None).await);
    refused(carol.delete_node(store_id, fx.inside).await);
    let mut metadata = carol.get_node(store_id, fx.inside).await.unwrap().metadata;
    metadata.title = "no".into();
    refused(carol.update_node_metadata(store_id, fx.inside, metadata).await);
    let changes = base64::engine::general_purpose::STANDARD.encode(NodeDoc::from_plain_text("no").unwrap().save());
    refused(carol.apply_edit(store_id, fx.inside, "carol", EditOperation::IncrementalChanges { changes }).await);
    // Sharing on is an owner's to do, whoever asks.
    assert_eq!(carol.cloud_share_node(store_id, fx.deeper, "Mine now").await.expect_err("not the reader's to share").to_string(), StoreAccess::READ_ONLY_REFUSAL);

    // And still receives everything.
    seed_content(&alice, store_id, fx.inside, "alice", "and sunscreen").await;
    rename(&alice, store_id, fx.inside, "Packing list").await;
    let received = wait_until(Duration::from_secs(10), || async {
        node_text(&carol, store_id, fx.inside).await.contains("and sunscreen") && node_title(&carol, store_id, fx.inside).await == "Packing list"
    })
    .await;
    assert!(received);
    let heads_after: HashMap<VaultDocId, u64> = env.h_admin.vault_list_docs(store_id).await.unwrap().into_iter().map(|d| (d.doc_id, d.head)).collect();
    assert_eq!(heads_after.get(&VaultDocId::Node(shared)), heads_before.get(&VaultDocId::Node(shared)), "a reader's device pushes nothing");

    a.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn a_member_can_neither_share_from_a_share_nor_change_it() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    alice.cloud_share_node(fx.store_id, fx.shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(fx.store_id, fx.shared, BOB.0, MemberRole::Editor).await.unwrap();
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;

    let refused = bob.cloud_share_node(fx.store_id, fx.deeper, "Mine now").await.expect_err("a member shares nothing on");
    assert_eq!(refused.to_string(), "Only an owner of this store can share from it. Nothing was changed.");
    assert!(bob.get_node(fx.store_id, fx.deeper).await.unwrap().metadata.share().is_none());
    assert!(env.scope_on_h(fx.store_id, fx.deeper).await.is_none());

    // Nor changes the share they are in. They may look at it: the owner is
    // whoever shared it, not the account looking.
    let not_theirs = "Only an owner of this store can change its shares. Nothing was changed.";
    assert_eq!(bob.cloud_share_invite(fx.store_id, fx.shared, CAROL.0, MemberRole::Editor).await.expect_err("not theirs to invite to").to_string(), not_theirs);
    assert_eq!(bob.cloud_share_remove_member(fx.store_id, fx.shared, BOB.0).await.expect_err("not theirs to remove from").to_string(), not_theirs);
    assert_eq!(bob.cloud_stop_sharing(fx.store_id, fx.shared).await.expect_err("not theirs to stop").to_string(), not_theirs);
    assert_eq!(env.scoped_rows(fx.store_id, fx.shared), 1);
    assert!(env.scope_on_h(fx.store_id, fx.shared).await.is_some() && bob.get_node(fx.store_id, fx.shared).await.unwrap().metadata.share().is_some());
    let seen = bob.cloud_share_info(fx.store_id, fx.shared).await.expect("a member may look at the share they are in");
    assert_eq!(
        seen.members.iter().map(|m| (m.email.as_str(), m.role)).collect::<Vec<_>>(),
        vec![(ALICE.0, MemberRole::Owner), (BOB.0, MemberRole::Editor)]
    );
    assert_eq!(alice.cloud_share_invite(fx.store_id, fx.shared, ALICE.0, MemberRole::Editor).await.expect_err("one's own store").to_string(), "This is your own store; there is nothing to invite yourself to.");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn a_member_invited_before_they_have_an_account_gets_the_key_within_a_sweep() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();

    let answer = alice.cloud_share_invite(store_id, shared, DAVE.0, MemberRole::Editor).await.expect("inviting an address with no account");
    assert_eq!(answer.members.last().map(|m| (m.email.as_str(), m.role, m.status)), Some((DAVE.0, MemberRole::Editor, ShareMemberStatus::Invited)));
    assert!(!env.has_share_key(store_id, shared, DAVE.0));

    // The account appears (signs up, verifies) and claims its invitation.
    // Nobody tells the owner's device; its sweep finds the member without a key.
    env.account_appears(DAVE);
    let handed = wait_until(SWEEP_EVERY * 5 + Duration::from_secs(5), || async { env.has_share_key(store_id, shared, DAVE.0) }).await;
    assert!(handed, "the sweep hands the share key to a member who turned up");
    let info = alice.cloud_share_info(store_id, shared).await.unwrap();
    assert_eq!(info.members.last().map(|m| (m.email.as_str(), m.status)), Some((DAVE.0, ShareMemberStatus::Active)));

    let (mut d, dave, _d_dir) = member_device(&env, DAVE, &fx).await;
    assert_eq!(child_ids(&dave, store_id, shared).await, vec![fx.inside, fx.deeper]);

    // An invitation is withdrawn by address, like a member.
    alice.cloud_share_invite(store_id, shared, "erin@example.com", MemberRole::Reader).await.unwrap();
    assert_eq!(env.scoped_rows(store_id, shared), 2);
    let after = alice.cloud_share_remove_member(store_id, shared, "erin@example.com").await.unwrap();
    assert_eq!(after.members.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(), vec![ALICE.0, DAVE.0]);

    a.stop().await.unwrap();
    d.stop().await.unwrap();
}

#[tokio::test]
async fn sharing_from_a_store_that_is_not_hosted_is_refused_and_nothing_is_uploaded() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    env.sign_in(&alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(a_dir.path().join("a.pimble"), "Alice's Notes").await.unwrap();
    let folder = alice.create_node(store_id, Some(root_id), "folder", "Holiday").await.unwrap();
    let doc = alice.create_node(store_id, Some(folder), "document", "Packing").await.unwrap();
    seed_content(&alice, store_id, doc, "seed", "NEVER-HOSTED-TEXT").await;

    let refused = alice.cloud_share_node(store_id, folder, "Holiday Plans").await.expect_err("sharing never hosts");
    assert_eq!(refused.to_string(), "Sharing needs this store hosted on Pimble Cloud or shared from this computer.");
    // Whoever is or is not signed in: the sentence is about the store.
    alice.cloud_sign_out().await.unwrap();
    assert_eq!(alice.cloud_share_node(store_id, folder, "Holiday Plans").await.expect_err("still").to_string(), pimble_server::share::NOT_HOSTED_REFUSAL);

    // Nothing anywhere: the hosted server holds no store and no file, the
    // accounts service was asked nothing about stores, members or keys, and
    // the node carries no marker.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(env.h_admin.list_stores().await.unwrap().is_empty(), "the hosted server's store list is unchanged");
    assert!(env.h_admin.vault_list_docs(store_id).await.is_err(), "and it has no vault of the store");
    assert_eq!(std::fs::read_dir(&env.h_stores_dir).unwrap().count(), 0);
    assert!(!contains_bytes_recursive(env._h_dir.path(), b"NEVER-HOSTED-TEXT"));
    {
        let s = env.stub.inner.lock().unwrap();
        assert!(s.stores.is_empty() && s.key_grants.is_empty() && s.invitations.is_empty());
        assert_eq!(s.sharing_calls, 0);
    }
    assert!(alice.get_node(store_id, folder).await.unwrap().metadata.share().is_none());
    assert!(scope_keys_in(a_dir.path(), store_id).is_empty(), "no share key was made");
    let (remote, _, mode) = alice.get_store_sync_with_mode(store_id).await.unwrap();
    assert!(remote.is_none() && mode == StoreKind::Plain, "and the store is as unhosted as it was");

    a.stop().await.unwrap();
}

#[tokio::test]
async fn stopping_a_share_leaves_the_documents_hosted_and_removes_scope_marker_and_members() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, shared, DAVE.0, MemberRole::Reader).await.unwrap();
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let (mut b, bob, b_dir) = member_device(&env, BOB, &fx).await;
    let from_bob = bob.create_node(store_id, Some(shared), "document", "Ferry").await.unwrap();
    assert!(wait_until(Duration::from_secs(20), || async { alice.get_node(store_id, from_bob).await.is_ok() }).await);
    let hosted_before: HashMap<VaultDocId, u64> = env.h_admin.vault_list_docs(store_id).await.unwrap().into_iter().map(|d| (d.doc_id, d.head)).collect();

    // Unreachable: nothing is changed, and the answer says so.
    env.stub.members_down.store(true, Ordering::SeqCst);
    let unreachable = alice.cloud_stop_sharing(store_id, shared).await.expect_err("the accounts service is down").to_string();
    assert!(unreachable.starts_with("Pimble Cloud cannot be reached, so nothing was changed."), "{unreachable}");
    env.stub.members_down.store(false, Ordering::SeqCst);
    env.relay.cut();
    let offline = wait_until(Duration::from_secs(10), || async { !matches!(alice.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. }))) }).await;
    assert!(offline);
    let unreachable = alice.cloud_stop_sharing(store_id, shared).await.expect_err("the hosted server is unreachable").to_string();
    assert_eq!(unreachable, "Pimble Cloud cannot be reached, so nothing was changed. Stop sharing again once this store is back online.");
    assert!(matches!(alice.cloud_share_info(store_id, shared).await.unwrap().share.state, SyncState::Offline), "a share is offline with its store's link");
    env.relay.restore();
    assert_eq!(env.scoped_rows(store_id, shared), 2);
    assert!(env.scope_on_h(store_id, shared).await.is_some());
    assert!(alice.get_node(store_id, shared).await.unwrap().metadata.share().is_some());
    let back = wait_until(Duration::from_secs(20), || async { matches!(alice.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. }))) }).await;
    assert!(back, "the link reconnects");
    let kept_up_again = wait_until(Duration::from_secs(10), || async { matches!(alice.cloud_share_info(store_id, shared).await.map(|info| info.share.state), Ok(SyncState::Synced { .. })) }).await;
    assert!(kept_up_again, "and the share is kept up again");

    alice.cloud_stop_sharing(store_id, shared).await.expect("stopping the share");

    // Members, invitation, scope, marker and key are gone.
    assert_eq!(env.scoped_rows(store_id, shared), 0);
    assert!(!env.has_share_key(store_id, shared, BOB.0));
    assert!(env.scope_on_h(store_id, shared).await.is_none(), "{:?}", env.h_admin.get_scopes(store_id).await);
    assert!(alice.get_node(store_id, shared).await.unwrap().metadata.share().is_none());
    assert!(!scope_keys_in(a_dir.path(), store_id).iter().any(|(id, _)| *id == share_key_id), "the share key is dropped");
    assert!(alice.cloud_share_info(store_id, shared).await.expect_err("no share to ask about").to_string().contains("is not shared"));
    // The owner's own whole-store grant is nobody's to remove by stopping a share.
    assert!(alice.cloud_list_hosted_stores().await.unwrap().iter().any(|row| row.store_id == store_id.to_string() && row.root.is_none() && row.role == "owner"));

    // The documents stay exactly where they are, the member's among them,
    // which the owner reads without the share key: it has the store key's wrap.
    let hosted_after: HashMap<VaultDocId, u64> = env.h_admin.vault_list_docs(store_id).await.unwrap().into_iter().map(|d| (d.doc_id, d.head)).collect();
    for (doc, head) in &hosted_before {
        assert!(hosted_after.get(doc).is_some_and(|after| after >= head), "{doc:?} is still hosted");
    }
    assert!(env.wrap_key_ids(store_id, from_bob).await.contains(&store_key_id));
    let (_, state, mode) = alice.get_store_sync_with_mode(store_id).await.unwrap();
    assert!(matches!(state, SyncState::Synced { .. }) && mode == StoreKind::Vault, "the store is still hosted, because the person hosted it");
    let marker_gone_on_h = wait_until(Duration::from_secs(10), || async { env.head_on_h(store_id, shared).await > hosted_before[&VaultDocId::Node(shared)] }).await;
    assert!(marker_gone_on_h, "the marker's removal replicates like any metadata");

    // The member's replica keeps what it had and receives nothing more.
    seed_content(&alice, store_id, fx.inside, "alice", "AFTER-THE-SHARE-STOPPED").await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!contains_bytes_recursive(b_dir.path(), b"AFTER-THE-SHARE-STOPPED"));
    assert!(node_text(&bob, store_id, fx.inside).await.contains("socks and a map"));

    // Shared again, it is a new share with a new key.
    let again = alice.cloud_share_node(store_id, shared, "Holiday, again").await.expect("sharing the node again");
    assert_eq!(again.members.len(), 1);
    assert_ne!(alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id, share_key_id);

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn deleting_a_shared_folder_stops_its_share_first_and_goes_ahead_without_the_cloud() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);

    // A share rooted inside what is deleted goes with it.
    alice.cloud_share_node(store_id, fx.deeper, "Tickets").await.unwrap();
    alice.cloud_share_invite(store_id, fx.deeper, BOB.0, MemberRole::Editor).await.unwrap();
    assert_eq!(env.scoped_rows(store_id, fx.deeper), 1);
    alice.delete_node(store_id, shared).await.expect("deleting a folder that holds a share");
    assert!(alice.get_node(store_id, fx.deeper).await.is_err());
    assert_eq!(env.scoped_rows(store_id, fx.deeper), 0, "its members are removed");
    assert!(env.scope_on_h(store_id, fx.deeper).await.is_none(), "and its scope");

    // Signed out, the deletion goes ahead all the same; what is left on the
    // account is the log's to say.
    let other = alice.create_node(store_id, Some(fx.root_id), "folder", "Recipes").await.unwrap();
    wait_all_seeded(&env, store_id, &[other]).await;
    alice.cloud_share_node(store_id, other, "Recipes").await.unwrap();
    alice.cloud_share_invite(store_id, other, CAROL.0, MemberRole::Reader).await.unwrap();
    alice.cloud_sign_out().await.unwrap();
    alice.delete_node(store_id, other).await.expect("a deletion never waits for Pimble Cloud");
    assert!(alice.get_node(store_id, other).await.is_err());
    assert_eq!(env.scoped_rows(store_id, other), 1, "the grant is left on the account");

    a.stop().await.unwrap();
}

#[tokio::test]
async fn a_second_device_of_the_owners_keeps_the_share_up_from_the_replicated_marker() {
    let env = spawn_env().await;
    let (mut a1, alice1, a1_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice1, a1_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a1_dir.path(), store_id).pop().unwrap();

    // The second device is up before the share exists: the marker reaches
    // it live, through replication, and with it the share's upkeep.
    let (mut a2, alice2, a2_dir) = start_local_server().await;
    env.sign_in(&alice2, ALICE).await;
    alice2.cloud_add_hosted_store(store_id).await.unwrap();
    assert!(wait_until(Duration::from_secs(15), || async { node_text(&alice2, store_id, fx.inside).await.contains("socks and a map") }).await);

    alice1.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    let share_key_id = alice1.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let has_key = wait_until(Duration::from_secs(20), || async { scope_keys_in(a2_dir.path(), store_id).iter().any(|(id, _)| *id == share_key_id) }).await;
    assert!(has_key, "the owner's other device fetches its own envelope of the share key");
    drop(alice1);
    a1.stop().await.unwrap();

    // Everything an owner's device does for a share, from the second one.
    let info = alice2.cloud_share_info(store_id, shared).await.expect("the share is this device's to keep up too");
    assert_eq!(info.share.name, "Holiday Plans");
    alice2.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    assert!(env.has_share_key(store_id, shared, BOB.0));
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;

    let made = bob.create_node(store_id, Some(shared), "document", "Ferry").await.unwrap();
    let kept_up = wait_until(Duration::from_secs(30), || async {
        sorted_uuids(env.wrap_key_ids(store_id, made).await) == sorted_uuids(vec![store_key_id, share_key_id]) && env.scope_on_h(store_id, shared).await.is_some_and(|docs| docs.contains(&made))
    })
    .await;
    assert!(kept_up, "wraps {:?}", env.wrap_key_ids(store_id, made).await);
    let hotel = alice2.create_node(store_id, Some(shared), "document", "Hotel").await.unwrap();
    assert!(wait_until(Duration::from_secs(30), || async { bob.get_node(store_id, hotel).await.is_ok() }).await, "and the member follows what it does");

    a2.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn a_document_from_before_data_keys_gets_one_when_it_comes_under_a_share() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, store_key) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();

    // A whole document from before data keys, under the folder: its blobs
    // are under the store key itself, made by hand as a phase 2a client
    // would have. The folder's list names it, so it is in the tree.
    let old = NodeId::new();
    let old_key = VaultDocId::Node(old);
    let mut old_doc = NodeDoc::from_plain_text("WRITTEN-BEFORE-DATA-KEYS").unwrap();
    old_doc.init("document", "Old", Some(shared), &chrono::Utc::now().to_rfc3339()).unwrap();
    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &old_key.as_str());
    let blob = pimble_crypto::Blob::encrypt(&store_key, store_key_id, &aad, &old_doc.save());
    env.h_admin.vault_append(store_id, old_key.clone(), URL_SAFE_NO_PAD.encode(blob)).await.unwrap();
    let here = wait_until(Duration::from_secs(15), || async { child_ids(&alice, store_id, shared).await.contains(&old) }).await;
    assert!(here, "the owner's device reads it with the store key and repair lists it");
    seed_content(&alice, store_id, old, "alice", "and edited since").await;
    assert!(wait_until(Duration::from_secs(10), || async { env.head_on_h(store_id, old).await >= 2 }).await);
    assert!(env.wrap_key_ids(store_id, old).await.is_empty(), "no device makes keys for a document that exists, until it comes under a share");

    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let rekeyed = wait_until(Duration::from_secs(20), || async {
        let Ok(fetch) = env.h_admin.vault_fetch(store_id, old_key.clone(), 0).await else { return false };
        let Some(keys) = &fetch.keys else { return false };
        let under_the_dek = |blob: &str| pimble_crypto::Blob::key_id(&URL_SAFE_NO_PAD.decode(blob).unwrap()).unwrap() == keys.dek_id;
        fetch.snapshot.as_ref().is_some_and(|s| under_the_dek(&s.blob)) && fetch.updates.iter().all(|u| under_the_dek(&u.blob))
    })
    .await;
    assert!(rekeyed, "a data key, and a snapshot under it in place of the blobs no member could read");
    assert_eq!(sorted_uuids(env.wrap_key_ids(store_id, old).await), sorted_uuids(vec![store_key_id, share_key_id]));

    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let read = wait_until(Duration::from_secs(15), || async {
        let text = node_text(&bob, store_id, old).await;
        text.contains("WRITTEN-BEFORE-DATA-KEYS") && text.contains("and edited since")
    })
    .await;
    assert!(read, "the member reads what was written before data keys: {:?}", node_text(&bob, store_id, old).await);
    // What the owner's device writes next goes out under the data key.
    seed_content(&alice, store_id, old, "alice", "after the share").await;
    assert!(wait_until(Duration::from_secs(10), || async { node_text(&bob, store_id, old).await.contains("after the share") }).await);

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn a_big_folder_is_wrapped_in_slices_and_its_scope_published_once_all_of_it_is_readable() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    env.sign_in(&alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(a_dir.path().join("a.pimble"), "Alice's Notes").await.unwrap();
    let shared = alice.create_node(store_id, Some(root_id), "folder", "Archive").await.unwrap();
    // More documents than one pass wraps before it lets the link's loop turn.
    let mut docs = Vec::new();
    for i in 0..150 {
        docs.push(alice.create_node(store_id, Some(shared), "document", format!("Note {i}")).await.unwrap());
    }
    alice.cloud_host_store(store_id).await.unwrap();
    let mut all = docs.clone();
    all.extend([root_id, shared]);
    wait_all_seeded(&env, store_id, &all).await;
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();

    alice.cloud_share_node(store_id, shared, "Archive").await.expect("sharing a big folder");
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let published = wait_until(Duration::from_secs(30), || async { env.scope_on_h(store_id, shared).await.is_some() }).await;
    assert!(published);
    // Wraps first, scope second: the moment the scope is there, everything
    // it names can be read with the share's key.
    assert_eq!(sorted(env.scope_on_h(store_id, shared).await.unwrap()), sorted(docs.clone()));
    for doc in &docs {
        assert_eq!(sorted_uuids(env.wrap_key_ids(store_id, *doc).await), sorted_uuids(vec![store_key_id, share_key_id]), "{doc}");
    }
    let settled = wait_until(Duration::from_secs(10), || async { matches!(alice.cloud_share_info(store_id, shared).await.map(|info| info.share.state), Ok(SyncState::Synced { .. })) }).await;
    assert!(settled);

    a.stop().await.unwrap();
}

// ── Against the REAL accounts service (crates/pimble-cloud) ──────────────
//
// Everything above drives a stub. This walks the owner's half of sharing
// against the real `rhypedb-server` and `pimble-cloud` binaries, the way
// `tests/vault_link.rs`'s interop test walks hosting: real signups, the
// verification links `LogMailer` logs, scoped members, invitations claimed
// at verification, scoped key envelopes, and the token's role per root as
// the real service mints it. Skips itself (saying why on stderr) when
// either binary is missing.

use std::process::{Child, Command, Stdio};

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

/// The minimal stand-in for jkbase's edge: `/rpc*` to `H`, the rest to
/// `pimble-cloud` (see `tests/vault_link.rs`).
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
    ["dev/rhypedb/target/release/rhypedb-server", "dev/rhypedb/target/debug/rhypedb-server"].iter().map(|rel| home.join(rel)).find(|p| p.is_file())
}

fn find_pimble_cloud_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PIMBLE_CLOUD_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?.to_path_buf();
    ["target/release/pimble-cloud", "target/debug/pimble-cloud"].iter().map(|rel| workspace_root.join(rel)).find(|p| p.is_file())
}

/// A real signup with real `pimble-crypto` output, then the verification
/// link `LogMailer` logged for it (the newest one in the log).
async fn sign_up_and_verify(cloud_url: &str, cloud_log: &Path, email: &str, password: &str) {
    let kdf = cheap_kdf_params();
    let password_keys = derive_password_keys(password, &kdf).unwrap();
    let account_keys = AccountKeys::generate();
    let recovery_kdf = cheap_kdf_params();
    let recovery_kek = pimble_crypto::derive_recovery_kek(&pimble_crypto::generate_recovery_code(), &recovery_kdf).unwrap();
    let logged_before = std::fs::read_to_string(cloud_log).map(|log| log.len()).unwrap_or(0);

    let signup = reqwest::Client::new()
        .post(format!("{cloud_url}/api/v1/signup"))
        .json(&json!({
            "email": email,
            "auth_key": pimble_crypto::encode_auth_key(&password_keys.auth_key),
            "kdf": kdf,
            "public_keys": account_keys.public_keys(),
            "account_key_blob": wrap_account_keys(&account_keys, &password_keys.kek).unwrap(),
            "recovery_salt": recovery_kdf.salt,
            "recovery_key_blob": wrap_account_keys(&account_keys, &recovery_kek).unwrap(),
        }))
        .send()
        .await
        .expect("signup request sends");
    assert_eq!(signup.status(), 202, "signup of {email}");

    let mut verify_url = None;
    let found = wait_until(Duration::from_secs(5), || {
        let log = std::fs::read_to_string(cloud_log).unwrap_or_default();
        let link = log.get(logged_before..).and_then(|fresh| {
            let start = fresh.find("/api/v1/verify?token=")?;
            let rest = &fresh[start..];
            let end = rest.find(|c: char| c.is_whitespace() || c == '"' || c == '\\').unwrap_or(rest.len());
            Some(rest[..end].to_string())
        });
        let found = link.is_some();
        if found {
            verify_url = link;
        }
        async move { found }
    })
    .await;
    assert!(found, "the verification link for {email} must appear in pimble-cloud's log");
    let no_redirect = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let verified = no_redirect.get(format!("{cloud_url}{}", verify_url.unwrap())).send().await.expect("verify link is reachable");
    assert_eq!(verified.status(), 303);
    assert!(verified.headers().get("location").unwrap().to_str().unwrap().contains("verified=1"), "verification of {email}");
}

#[tokio::test]
async fn sharing_against_the_real_accounts_service() {
    let Some(rhypedb_bin) = find_rhypedb_server() else {
        eprintln!("SKIPPING sharing_against_the_real_accounts_service: no rhypedb-server binary found (set RHYPEDB_SERVER_BIN, or build ~/dev/rhypedb)");
        return;
    };
    let Some(cloud_bin) = find_pimble_cloud_binary() else {
        eprintln!("SKIPPING sharing_against_the_real_accounts_service: no pimble-cloud binary found (set PIMBLE_CLOUD_BIN, or `cargo build -p pimble-cloud --release`)");
        return;
    };

    let rhypedb_data_dir = tempfile::tempdir().unwrap();
    let (rhypedb_http_port, rhypedb_tcp_port) = (free_port(), free_port());
    let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("pimble-cloud").join("schema.rhype");
    let _rhypedb = ChildGuard(
        Command::new(&rhypedb_bin)
            .args(["--schema", schema_path.to_str().unwrap(), "--data-dir", rhypedb_data_dir.path().to_str().unwrap()])
            .args(["--listen", &format!("127.0.0.1:{rhypedb_http_port}"), "--tcp-listen", &format!("127.0.0.1:{rhypedb_tcp_port}")])
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

    let h_dir = tempfile::tempdir().unwrap();
    let h_token = "h-service-token".to_string();
    let (cloud_internal_port, proxy_port) = (free_port(), free_port());
    let cloud_url = format!("http://127.0.0.1:{proxy_port}");
    let issuer = format!("{cloud_url}/api/v1");
    let mut h = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(h_token.clone()),
        jwks_url: Some(format!("{issuer}/.well-known/jwks.json").parse().unwrap()),
        jwt_issuer: Some(issuer),
        keystore_path: Some(h_dir.path().join("keys.json")),
        credentials_path: Some(h_dir.path().join("credentials.json")),
        replicas_dir: Some(h_dir.path().join("replicas")),
        ..Default::default()
    });
    h.start().await.expect("H starts");
    let h_url = format!("http://{}", h.addr());
    let h_admin = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_token.clone() }).await.expect("admin client connects to H");

    let cloud_stores_dir = tempfile::tempdir().unwrap();
    let cloud_log = h_dir.path().join("cloud.log");
    let cloud_log_file = std::fs::File::create(&cloud_log).unwrap();
    let cloud_log_file_err = cloud_log_file.try_clone().unwrap();
    let _cloud = ChildGuard(
        Command::new(&cloud_bin)
            .env("RHYPEDB_ADDR", format!("127.0.0.1:{rhypedb_tcp_port}"))
            .env("PIMBLE_SERVER_URL", &h_url)
            .env("PIMBLE_SERVER_TOKEN", &h_token)
            .env("PIMBLE_STORES_DIR", cloud_stores_dir.path())
            .env("PIMBLE_CLOUD_DEV_SIGNING_SEED", format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()))
            .env("PIMBLE_CLOUD_PUBLIC_URL", &cloud_url)
            .env("PORT", cloud_internal_port.to_string())
            .env("RUST_LOG", "pimble_cloud=info")
            .stdout(Stdio::from(cloud_log_file))
            .stderr(Stdio::from(cloud_log_file_err))
            .spawn()
            .expect("pimble-cloud spawns"),
    );
    let cloud_ready = wait_until(Duration::from_secs(15), || async {
        reqwest::get(format!("http://127.0.0.1:{cloud_internal_port}/api/v1/health")).await.map(|r| r.status().is_success()).unwrap_or(false)
    })
    .await;
    assert!(cloud_ready, "pimble-cloud must come up within 15s (see {:?})", cloud_log);
    tokio::spawn(run_edge_proxy(proxy_port, cloud_internal_port, h.addr().port()));
    let proxy_ready = wait_until(Duration::from_secs(5), || async {
        reqwest::get(format!("{cloud_url}/api/v1/health")).await.map(|r| r.status().is_success()).unwrap_or(false)
    })
    .await;
    assert!(proxy_ready, "the local edge proxy must come up within 5s");

    let tag = Uuid::new_v4().simple().to_string();
    let (alice_email, bob_email, late_email) = (format!("alice-{tag}@example.com"), format!("bob-{tag}@example.com"), format!("late-{tag}@example.com"));
    let password = "correct horse battery staple";
    sign_up_and_verify(&cloud_url, &cloud_log, &alice_email, password).await;
    sign_up_and_verify(&cloud_url, &cloud_log, &bob_email, password).await;

    // The owner: a hosted store with a folder to share and a document not to.
    let (mut a, alice, a_dir) = start_local_server().await;
    alice.cloud_sign_in(&cloud_url, &alice_email, password).await.expect("the owner signs in");
    let (store_id, root_id) = alice.create_store(a_dir.path().join("a.pimble"), "Alice's Notes").await.unwrap();
    let shared = alice.create_node(store_id, Some(root_id), "folder", "Holiday").await.unwrap();
    let inside = alice.create_node(store_id, Some(shared), "document", "Packing").await.unwrap();
    let private = alice.create_node(store_id, Some(root_id), "document", "Diary").await.unwrap();
    seed_content(&alice, store_id, inside, "seed", "socks and a map").await;
    seed_content(&alice, store_id, private, "seed", "PRIVATE-DIARY-TEXT").await;
    alice.cloud_host_store(store_id).await.expect("hosting against the real service");
    let seeded = wait_until(Duration::from_secs(15), || async {
        let listed = h_admin.vault_list_docs(store_id).await.unwrap_or_default();
        [root_id, shared, inside, private].iter().all(|id| listed.iter().any(|d| d.doc_id == VaultDocId::Node(*id) && d.head > 0))
    })
    .await;
    assert!(seeded);

    // Share, and invite an account that exists: the grant, and the key at once.
    let answer = alice.cloud_share_node(store_id, shared, "Holiday Plans").await.expect("cloudShareNode against the real service");
    assert_eq!(answer.members.iter().map(|m| (m.email.as_str(), m.role)).collect::<Vec<_>>(), vec![(alice_email.as_str(), MemberRole::Owner)]);
    assert!(matches!(answer.share.state, SyncState::Synced { .. }), "{:?}", answer.share.state);
    let answer = alice.cloud_share_invite(store_id, shared, &bob_email, MemberRole::Editor).await.expect("cloudShareInvite against the real service");
    assert_eq!(
        answer.members.iter().map(|m| (m.email.as_str(), m.role, m.status)).collect::<Vec<_>>(),
        vec![(alice_email.as_str(), MemberRole::Owner, ShareMemberStatus::Active), (bob_email.as_str(), MemberRole::Editor, ShareMemberStatus::Active)]
    );

    // The member: the real service's rows and token (a role per root) make
    // a partial replica, and both sides edit the tree.
    let (mut b, bob, b_dir) = start_local_server().await;
    bob.cloud_sign_in(&cloud_url, &bob_email, password).await.expect("the member signs in");
    let rows = bob.cloud_list_hosted_stores().await.unwrap();
    assert_eq!(rows.iter().map(|r| (r.root, r.name.as_str(), r.role.as_str(), r.shared_by.as_deref())).collect::<Vec<_>>(), vec![(Some(shared), "Holiday Plans", "editor", Some(alice_email.as_str()))]);
    let store = bob.cloud_add_hosted_store(store_id).await.expect("the share is added as a partial replica");
    assert_eq!((store.roots.clone(), store.name.as_str()), (vec![shared], "Holiday Plans"));
    assert!(wait_until(Duration::from_secs(15), || async { node_text(&bob, store_id, inside).await.contains("socks and a map") }).await, "read with the share's key");
    assert!(bob.get_node(store_id, private).await.is_err() && !contains_bytes_recursive(b_dir.path(), b"PRIVATE-DIARY-TEXT"));
    let ferry = bob.create_node(store_id, Some(shared), "document", "Ferry").await.unwrap();
    assert!(wait_until(Duration::from_secs(20), || async { child_ids(&alice, store_id, shared).await == vec![inside, ferry] }).await, "the member's create reaches the owner");
    rename(&alice, store_id, ferry, "Night ferry").await;
    assert!(wait_until(Duration::from_secs(10), || async { node_title(&bob, store_id, ferry).await == "Night ferry" }).await, "the owner's rename reaches the member");

    // An address with no account: invited, claimed when it verifies, and
    // handed the key by the owner's sweep.
    let answer = alice.cloud_share_invite(store_id, shared, &late_email, MemberRole::Reader).await.expect("inviting an address with no account");
    assert_eq!(answer.members.last().map(|m| (m.email.as_str(), m.status)), Some((late_email.as_str(), ShareMemberStatus::Invited)));
    sign_up_and_verify(&cloud_url, &cloud_log, &late_email, password).await;
    let handed = wait_until(SWEEP_EVERY * 5 + Duration::from_secs(5), || async {
        alice.cloud_share_info(store_id, shared).await.is_ok_and(|info| info.members.iter().any(|m| m.email == late_email && m.status == ShareMemberStatus::Active))
    })
    .await;
    assert!(handed, "the sweep hands the key to the member who turned up: {:?}", alice.cloud_share_info(store_id, shared).await.map(|info| info.members));
    let (mut l, late, _l_dir) = start_local_server().await;
    late.cloud_sign_in(&cloud_url, &late_email, password).await.unwrap();
    assert_eq!(late.cloud_add_hosted_store(store_id).await.expect("the late member adds the share").access, StoreAccess::Read);
    assert!(wait_until(Duration::from_secs(15), || async { node_text(&late, store_id, inside).await.contains("socks and a map") }).await);

    // Remove one, stop the rest.
    let after = alice.cloud_share_remove_member(store_id, shared, &late_email).await.expect("cloudShareRemoveMember against the real service");
    assert_eq!(after.members.len(), 2);
    alice.cloud_stop_sharing(store_id, shared).await.expect("cloudStopSharing against the real service");
    assert!(bob.cloud_list_hosted_stores().await.unwrap().is_empty(), "the member's grant is gone");
    assert!(h_admin.get_scopes(store_id).await.unwrap().iter().all(|scope| scope.root != shared), "and the scope");
    assert!(alice.get_node(store_id, shared).await.unwrap().metadata.share().is_none(), "and the marker");
    assert!(h_admin.vault_list_docs(store_id).await.unwrap().iter().any(|d| d.doc_id == VaultDocId::Node(ferry)), "the documents stay hosted");
    assert!(alice.cloud_list_hosted_stores().await.unwrap().iter().any(|row| row.store_id == store_id.to_string() && row.role == "owner"));

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    l.stop().await.unwrap();
    h.stop().await.unwrap();
}
