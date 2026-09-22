//! Tests for the "a published scope set only grows" rule
//! (docs/MOVE_CONTRACT.md "Repair", last paragraph; `pimble_server::share`,
//! [`grown_scope`]): nothing a share's upkeep publishes for a root is ever
//! taken away except by stopping the share. The harness below (the stub
//! accounts service, `H`, `Env`, `Fixture`, `hosted_fixture`,
//! `member_device`) is copied from `tests/share.rs`, which owns that file;
//! see it for the fuller sharing test suite this one does not repeat.
//!
//! What is proven here: a node deleted inside a share stays in its
//! published scope (a tombstone's `parent_id` is untouched, so this
//! device's own tree still reaches it — `plan` in `share.rs`), a member
//! still holds it and can undo the delete, and the union `grown_scope`
//! computes never drops a document either side already published.

// The copied harness carries more than this file uses; sharing it with `share.rs`
// through a `tests/common` module is a follow-up.
#![allow(dead_code, unused_imports)]

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

/// The scenario docs/MOVE_CONTRACT.md asks for directly: "a node deleted
/// (tombstoned) inside a share after the set was published is still in the
/// published set at the next upkeep, and a member can still fetch it and
/// `undeleteNode` it." `plan` in `share.rs` already reaches a tombstone
/// through its unchanged `parent_id` (the doc comment above it says so);
/// this proves it end to end, on the real sharing harness, rather than
/// trusting the comment, and proves the member's side of "undoable" too
/// (there is no other test of `undeleteNode` through a share on this
/// harness).
#[tokio::test]
async fn a_node_deleted_inside_a_share_stays_in_its_published_scope_and_a_member_can_undelete_it() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);

    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.expect("sharing a folder of a hosted store");
    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.expect("inviting an editor");
    let published = wait_until(Duration::from_secs(15), || async { env.scope_on_h(store_id, shared).await.map(sorted) == Some(sorted(vec![fx.inside, fx.deeper, fx.leaf])) }).await;
    assert!(published, "the scope is published before anyone deletes anything: {:?}", env.scope_on_h(store_id, shared).await);

    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let has_leaf = wait_until(Duration::from_secs(15), || async { bob.get_node(store_id, fx.leaf).await.is_ok() }).await;
    assert!(has_leaf, "the member's replica pulls the whole scope before anything is deleted from it");

    // A member's delete inside their share: a tombstone, not a removal from
    // the scope (docs/NODE_DOCUMENT_CONTRACT.md section 2, "Tombstones").
    bob.delete_node(store_id, fx.leaf).await.expect("a member can delete inside their share");
    let tombstoned = wait_until(Duration::from_secs(10), || async { alice.get_node(store_id, fx.leaf).await.is_err() && child_ids(&alice, store_id, fx.deeper).await.is_empty() }).await;
    assert!(tombstoned, "the delete reaches the owner");

    // The owner's next upkeep pass (the delete is a structural change, so
    // one is due a second after it) still names the tombstone: a scope
    // set only grows until its share is stopped.
    let still_published = wait_until(Duration::from_secs(15), || async { env.scope_on_h(store_id, shared).await.map(sorted) == Some(sorted(vec![fx.inside, fx.deeper, fx.leaf])) }).await;
    assert!(still_published, "a tombstone stays in the published scope: {:?}", env.scope_on_h(store_id, shared).await);

    // The member still reaches the tombstone (it was never taken off the
    // scope they were sent) and undoes the delete themselves.
    bob.undelete_node(store_id, fx.leaf).await.expect("a member can still fetch the tombstone and undo its delete");
    let restored = wait_until(Duration::from_secs(15), || async {
        child_ids(&bob, store_id, fx.deeper).await == vec![fx.leaf] && node_title(&bob, store_id, fx.leaf).await == "Train" && alice.get_node(store_id, fx.leaf).await.is_ok()
    })
    .await;
    assert!(restored, "the member's undelete reaches the owner too: {:?}", child_ids(&bob, store_id, fx.deeper).await);
    assert_eq!(env.scope_on_h(store_id, shared).await.map(sorted), Some(sorted(vec![fx.inside, fx.deeper, fx.leaf])), "the scope still names it, restored or not");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}
