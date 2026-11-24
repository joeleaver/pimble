//! The sharing harness `tests/share.rs` and `tests/share_upkeep.rs` run on
//! (each test file is its own crate, so this is a `tests/common` module):
//! `H`, a real `PimbleServer` in JWT mode holding the hosted `Vault` twins;
//! an axum stub standing in for the accounts service with the scoped
//! members, invitations and keys endpoints (`pimble-cloud`'s README,
//! "Sharing"); a TCP relay in front of `H` a test can cut; real "desktop"
//! `PimbleServer`s, one per device, each with its own temp keystore and
//! replicas directory; and the fixture most tests start from (Alice's hosted
//! store with a folder to share). Three accounts to start with (the owner,
//! two to share with), and a fourth that appears later, for an invitation
//! sent before its address had an account.
//!
//! `tests/vault_link.rs` and `tests/relay.rs` still carry harnesses of
//! their own (an earlier stub without the sharing endpoints, and one with
//! the relay's routes).

// Not every test file uses every helper.
#![allow(dead_code)]

use pimble_host::disk::testfs as tfs;
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
use pimble_core::{AuthMethod, NodeId, StoreId, StoreKind};
use pimble_crdt::NodeDoc;
use pimble_crypto::{derive_password_keys, wrap_account_keys, AccountKeyBlob, AccountKeys, KdfParams, KeyEnvelope};
use pimble_rpc::{EditOperation, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;
use uuid::Uuid;

// ── Small generic helpers (mirrors tests/vault_link.rs) ───────────────────

pub async fn wait_until<F, Fut>(timeout: Duration, mut cond: F) -> bool
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

pub async fn node_text(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    match client.get_node(store_id, node_id).await {
        Ok(node) => NodeDoc::text_of(&node.content),
        Err(_) => String::new(),
    }
}

pub async fn node_title(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    client.get_node(store_id, node_id).await.map(|node| node.metadata.title).unwrap_or_default()
}

pub async fn child_ids(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> Vec<NodeId> {
    client.get_children(store_id, node_id).await.map(|(_, children)| children.into_iter().map(|n| n.id).collect()).unwrap_or_default()
}

pub async fn rename(client: &PimbleClient, store_id: StoreId, node_id: NodeId, title: &str) {
    client.edit_node(store_id, node_id, "rename", |doc, now| doc.set_title(title, now)).await.expect("rename");
}

pub async fn seed_content(client: &PimbleClient, store_id: StoreId, node_id: NodeId, client_id: &str, text: &str) {
    let doc = NodeDoc::from_plain_text(text).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.save());
    client
        .apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes })
        .await
        .expect("seed content applies");
}

/// Put the picture `url` on `node_id`'s canvas the way the app does (an
/// element of kind picture in `data.board`), so it names the blob as a
/// canvas does and not as text does.
#[allow(dead_code)]
pub async fn put_on_canvas(client: &PimbleClient, store_id: StoreId, node_id: NodeId, url: &pimble_core::BlobUrl) {
    use pimble_board::{Board, Geom, Kind, NewElement, PictureGeom, Rect, Stamp};
    let node = client.get_node(store_id, node_id).await.expect("the node is there");
    let doc = NodeDoc::load(&node.content).expect("its document loads");
    let before = doc.state_vector();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let board = Board::of_node(&doc);
    if !board.exists() {
        board.create(Stamp::new(None, &now)).expect("a canvas");
    }
    let geom = Geom::Picture(PictureGeom { rect: Rect { x: 0.0, y: 0.0, w: 100.0, h: 80.0 }, src: url.to_string(), px_w: 100, px_h: 80, crop: None });
    board.add_element(Stamp::new(None, &now), NewElement::new(Kind::Picture, geom)).expect("a picture on it");
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.diff_since(&before).unwrap());
    client.apply_edit(store_id, node_id, "canvas", EditOperation::IncrementalChanges { changes }).await.expect("the canvas edit applies");
}

/// How often a test device's key sweep runs, in place of the minute.
pub const SWEEP_EVERY: Duration = Duration::from_secs(1);

pub fn device_config(dir: &Path) -> ServerConfig {
    ServerConfig { share_sweep_interval: Some(SWEEP_EVERY), ..ServerConfig::in_dir(dir) }
}

/// A fresh desktop-side `PimbleServer` (plain, tokenless, loopback) with
/// its own temp keystore/credentials/replicas paths.
pub async fn start_local_server() -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (server, client) = start_device_in(dir.path()).await;
    (server, client, dir)
}

/// A device started (or started again) over `dir`.
pub async fn start_device_in(dir: &Path) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(device_config(dir));
    server.start().await.expect("local server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

// ── JWT signing ───────────────────────────────────────────────────────────

pub fn fresh_signing_key() -> SigningKey {
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

pub fn make_jwt(signing_key: &SigningKey, kid: &str, issuer: &str, sub: &str, email: &str, stores_claim: &serde_json::Map<String, serde_json::Value>) -> String {
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
pub struct StubStoreRow {
    pub user_id: String,
    pub store_id: String,
    pub name: String,
    pub kind: String,
    pub created_at: String,
    pub role: String,
    /// The shared node, for a share.
    pub root: Option<String>,
    pub shared_by: Option<String>,
}

pub struct StubInvitation {
    pub store_id: String,
    pub email: String,
    pub role: String,
    pub root: Option<String>,
    pub name: String,
    pub invited_by: String,
}

pub struct KeyGrantRow {
    pub user_id: String,
    pub key_id: String,
    pub envelope: KeyEnvelope,
    /// The share the key is for; `None` for the store key.
    pub root: Option<String>,
}

pub struct StubAccount {
    pub kdf: KdfParams,
    pub user_id: String,
    pub email: String,
    pub session: String,
    pub account_key_blob: AccountKeyBlob,
    pub account_public_keys: pimble_crypto::AccountPublicKeys,
    /// The default name chosen (docs/PRESENCE_CONTRACT.md section 3).
    pub display_name: String,
    /// The notice key's public half and envelope, once a device made it
    /// (docs/NOTIFICATIONS_CONTRACT.md section 2).
    pub notice_key: Option<(String, KeyEnvelope)>,
    pub notifications_off: bool,
    /// Wave 2's channels.
    pub email_off: bool,
    pub push_off: bool,
}

/// One sealed notice queued in the stub's inbox.
#[derive(Clone, Debug)]
pub struct StubInboxItem {
    pub seq: u64,
    pub id: String,
    pub recipient: String,
    pub from: String,
    pub store_id: String,
    pub root: Option<String>,
    pub sealed: String,
}

pub struct StubInner {
    /// The owner first; a request with no session the stub knows is theirs.
    pub accounts: Vec<StubAccount>,
    pub stores: Vec<StubStoreRow>,
    pub invitations: Vec<StubInvitation>,
    pub key_grants: HashMap<String, Vec<KeyGrantRow>>,
    pub signing_key: SigningKey,
    pub kid: String,
    pub issuer: String,
    pub h_client: Option<Arc<PimbleClient>>,
    pub h_rpc_url: String,
    pub h_stores_dir: PathBuf,
    /// Every call the sharing endpoints took, for a test to assert none was made.
    pub sharing_calls: usize,
    /// The names chosen per (user, store, root) (docs/PRESENCE_CONTRACT.md
    /// section 3).
    pub share_names: HashMap<(String, String, Option<String>), String>,
    /// Notices queued (docs/NOTIFICATIONS_CONTRACT.md section 2), and every
    /// one ever posted (sender, recipient), for a test to count.
    pub inbox: Vec<StubInboxItem>,
    pub posted: Vec<(String, String)>,
    pub next_seq: u64,
    /// The kind of every notice posted, in order (wave 2).
    pub kinds: Vec<String>,
    /// Push subscriptions (user id, endpoint) (wave 2).
    pub push_subs: Vec<(String, String)>,
    /// Wave 3: each account's sealed watch choices (version, sealed), and
    /// the documents registered per (user id, store id).
    pub watch_choices: HashMap<String, (u64, String)>,
    pub watched: HashMap<(String, String), Vec<String>>,
}

pub struct Stub {
    pub inner: Mutex<StubInner>,
    /// The members endpoints answer 503 while set: "the service is unreachable".
    pub members_down: AtomicBool,
    /// `/token` answers 401 "invalid session" while set: the session expired.
    pub refuse_sessions: AtomicBool,
    /// How many times `/token` was asked.
    pub token_calls: std::sync::atomic::AtomicUsize,
    /// Wakes a waiting inbox drain.
    pub inbox_wake: tokio::sync::Notify,
}

pub type StubState = Arc<Stub>;

pub fn stub_router(state: StubState) -> Router {
    Router::new()
        .route("/api/v1/kdf", get(stub_kdf))
        .route("/api/v1/login", post(stub_login))
        .route("/api/v1/me/keys", get(stub_me_keys))
        .route("/api/v1/me", get(stub_me).put(stub_put_me))
        .route("/api/v1/me/shares", get(stub_list_shares))
        .route("/api/v1/me/shares/{store_id}", axum::routing::put(stub_put_share_name))
        .route("/api/v1/token", post(stub_mint_token))
        .route("/api/v1/stores", get(stub_list_stores).post(stub_create_store))
        .route("/api/v1/stores/{store_id}/keys", get(stub_get_keys).put(stub_put_keys))
        .route("/api/v1/stores/{store_id}/members", get(stub_list_members).put(stub_put_member))
        .route("/api/v1/stores/{store_id}/members/{user_id}", delete(stub_delete_member))
        .route("/api/v1/stores/{store_id}/invitations/{email}", delete(stub_delete_invitation))
        .route("/api/v1/me/notice-key", axum::routing::put(stub_put_notice_key))
        .route("/api/v1/me/notifications", axum::routing::put(stub_put_notifications))
        .route("/api/v1/me/push", post(stub_post_push).delete(stub_delete_push))
        .route("/api/v1/me/watches", get(stub_get_watches))
        .route("/api/v1/me/watches/choices", axum::routing::put(stub_put_watch_choices))
        .route("/api/v1/me/watches/stores/{store_id}", axum::routing::put(stub_put_watched_docs))
        .route("/api/v1/inbox", get(stub_drain_inbox).delete(stub_clear_inbox))
        .route("/api/v1/inbox/ack", post(stub_ack_inbox))
        .route("/api/v1/inbox/{recipient}", post(stub_post_notice))
        .route("/jwks.json", get(stub_jwks))
        .with_state(state)
}

impl StubInner {
    /// The account a request's `Authorization: Bearer <session>` names.
    pub fn account_of(&self, headers: &HeaderMap) -> &StubAccount {
        let session = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
        self.accounts.iter().find(|a| Some(a.session.as_str()) == session).unwrap_or(&self.accounts[0])
    }

    pub fn account_by_email(&self, email: Option<&str>) -> &StubAccount {
        self.accounts.iter().find(|a| Some(a.email.as_str()) == email).unwrap_or(&self.accounts[0])
    }

    /// What `user_id` goes by in the scope `root` of `store_id`, as the
    /// accounts service answers it.
    pub fn name_in(&self, user_id: &str, store_id: &str, root: &Option<String>) -> String {
        if let Some(name) = self.share_names.get(&(user_id.to_string(), store_id.to_string(), root.clone())) {
            return name.clone();
        }
        let account = self.accounts.iter().find(|a| a.user_id == user_id);
        match account {
            Some(a) if !a.display_name.is_empty() => a.display_name.clone(),
            Some(a) => a.email.split('@').next().unwrap_or_default().to_string(),
            None => "someone".to_string(),
        }
    }

    pub fn is_owner(&self, user_id: &str, store_id: &str) -> bool {
        self.stores.iter().any(|row| row.user_id == user_id && row.store_id == store_id && row.root.is_none() && row.role == "owner")
    }

    pub fn has_key(&self, user_id: &str, store_id: &str, root: &Option<String>) -> bool {
        self.key_grants.get(store_id).is_some_and(|rows| rows.iter().any(|g| g.user_id == user_id && &g.root == root))
    }

    /// An invitation becomes a grant on the scope it named the moment its
    /// address has an account.
    pub fn claim_invitations(&mut self, email: &str) {
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

pub async fn stub_kdf(State(state): State<StubState>, Query(query): Query<HashMap<String, String>>) -> Json<KdfParams> {
    let s = state.inner.lock().unwrap();
    Json(s.account_by_email(query.get("email").map(String::as_str)).kdf.clone())
}

pub async fn stub_login(State(state): State<StubState>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_by_email(req["email"].as_str());
    Json(json!({ "user": { "id": account.user_id, "email": account.email }, "session": account.session, "token": "", "exp": 0 }))
}

pub async fn stub_me_keys(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_of(&headers);
    let notice_key = account.notice_key.as_ref().map(|(public, envelope)| json!({ "public_key": public, "envelope": envelope }));
    Json(json!({ "public_keys": account.account_public_keys, "kdf": account.kdf, "account_key_blob": account.account_key_blob, "notice_key": notice_key }))
}

// ── Notices (docs/NOTIFICATIONS_CONTRACT.md), as the real service keeps them ──

pub async fn stub_put_notice_key(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let public = req["public_key"].as_str().unwrap_or_default().to_string();
    let envelope: KeyEnvelope = serde_json::from_value(req["envelope"].clone()).map_err(|_| StatusCode::BAD_REQUEST)?;
    let account = s.accounts.iter_mut().find(|a| a.user_id == user_id).unwrap();
    if envelope.recipient != account.account_public_keys.encryption || pimble_crypto::verify_envelope(&envelope, &account.account_public_keys.signing).is_err() {
        return Err(StatusCode::BAD_REQUEST);
    }
    match &account.notice_key {
        Some((held, _)) if *held != public => Err(StatusCode::CONFLICT),
        _ => {
            account.notice_key = Some((public.clone(), envelope));
            Ok(Json(json!({ "public_key": public })))
        }
    }
}

pub async fn stub_put_notifications(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let on = req["on"].as_bool();
    if let Some(a) = s.accounts.iter_mut().find(|a| a.user_id == user_id) {
        if let Some(on) = on {
            a.notifications_off = !on;
        }
        if let Some(email) = req["email"].as_bool() {
            a.email_off = !email;
        }
        if let Some(push) = req["push"].as_bool() {
            a.push_off = !push;
        }
    }
    if on == Some(false) {
        s.inbox.retain(|i| i.recipient != user_id);
    }
    Json(json!({ "on": on }))
}

pub async fn stub_post_push(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let endpoint = req["endpoint"].as_str().unwrap_or_default().to_string();
    s.push_subs.retain(|(_, e)| *e != endpoint);
    s.push_subs.push((user_id, endpoint));
    Json(json!({ "subscribed": true }))
}

pub async fn stub_delete_push(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let endpoint = req["endpoint"].as_str().unwrap_or_default().to_string();
    s.push_subs.retain(|(u, e)| !(*u == user_id && *e == endpoint));
    Json(json!({}))
}

/// `GET /api/v1/me/watches` (wave 3).
pub async fn stub_get_watches(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let choices = match s.watch_choices.get(&user_id) {
        Some((version, sealed)) => json!({ "version": version, "sealed": sealed }),
        None => json!({ "version": 0 }),
    };
    let stores: Vec<serde_json::Value> = s.watched.iter().filter(|((u, _), _)| *u == user_id).map(|((_, store), docs)| json!({ "store_id": store, "docs": docs })).collect();
    Json(json!({ "choices": choices, "stores": stores }))
}

/// `PUT /api/v1/me/watches/choices`: compare-and-swap on the version.
pub async fn stub_put_watch_choices(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let held = s.watch_choices.get(&user_id).map(|(v, _)| *v).unwrap_or(0);
    if req["version"].as_u64() != Some(held) {
        return Err(StatusCode::CONFLICT);
    }
    let sealed = req["sealed"].as_str().unwrap_or_default().to_string();
    s.watch_choices.insert(user_id, (held + 1, sealed));
    Ok(Json(json!({ "version": held + 1 })))
}

/// `PUT /api/v1/me/watches/stores/{store}`: the store's documents, whole,
/// from the choices at `version`.
pub async fn stub_put_watched_docs(State(state): State<StubState>, headers: HeaderMap, AxPath(store_id): AxPath<String>, Json(req): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let held = s.watch_choices.get(&user_id).map(|(v, _)| *v).unwrap_or(0);
    if req["version"].as_u64() != Some(held) {
        return Err(StatusCode::CONFLICT);
    }
    let docs: Vec<String> = req["docs"].as_array().map(|d| d.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    s.watched.insert((user_id, store_id), docs);
    Ok(Json(json!({})))
}

impl StubInner {
    /// Whether `user_id` reaches the scope `root` of `store_id`.
    pub fn reaches(&self, user_id: &str, store_id: &str, root: &Option<String>) -> bool {
        self.stores.iter().any(|row| row.user_id == user_id && row.store_id == store_id && (row.root.is_none() || (root.is_some() && row.root == *root)))
    }
}

pub async fn stub_post_notice(
    State(state): State<StubState>,
    AxPath(recipient): AxPath<String>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let mut s = state.inner.lock().unwrap();
    let from = s.account_of(&headers).user_id.clone();
    let store_id = req["store_id"].as_str().unwrap_or_default().to_string();
    let root = req["root"].as_str().map(str::to_string);
    if !s.reaches(&from, &store_id, &root) || !s.reaches(&recipient, &store_id, &root) {
        return Err(StatusCode::FORBIDDEN);
    }
    s.posted.push((from.clone(), recipient.clone()));
    s.kinds.push(req["kind"].as_str().unwrap_or_default().to_string());
    if s.accounts.iter().any(|a| a.user_id == recipient && a.notifications_off) {
        return Ok(Json(json!({ "id": null, "queued": false })));
    }
    s.next_seq += 1;
    let item = StubInboxItem { seq: s.next_seq, id: Uuid::new_v4().to_string(), recipient, from, store_id, root, sealed: req["sealed"].as_str().unwrap_or_default().to_string() };
    let id = item.id.clone();
    s.inbox.push(item);
    drop(s);
    state.inbox_wake.notify_waiters();
    Ok(Json(json!({ "id": id, "queued": true })))
}

fn stub_items_after(s: &StubInner, me: &str, after: u64) -> Vec<StubInboxItem> {
    s.inbox.iter().filter(|i| i.recipient == me && i.seq > after).cloned().collect()
}

pub async fn stub_drain_inbox(State(state): State<StubState>, headers: HeaderMap, Query(query): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
    let after: u64 = query.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    let wait: u64 = query.get("wait").and_then(|a| a.parse().ok()).unwrap_or(0).min(25);
    let me = state.inner.lock().unwrap().account_of(&headers).user_id.clone();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    let items = loop {
        let notified = state.inbox_wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let items = stub_items_after(&state.inner.lock().unwrap(), &me, after);
        if !items.is_empty() || tokio::time::Instant::now() >= deadline {
            break items;
        }
        let _ = tokio::time::timeout_at(deadline, notified).await;
    };
    let cursor = items.iter().map(|i| i.seq).max().unwrap_or(after);
    let items: Vec<_> = items
        .iter()
        .map(|i| json!({ "id": i.id, "seq": i.seq, "from": i.from, "store_id": i.store_id, "root": i.root, "sealed": i.sealed, "at": chrono::Utc::now().to_rfc3339() }))
        .collect();
    Json(json!({ "items": items, "cursor": cursor }))
}

pub async fn stub_ack_inbox(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let me = s.account_of(&headers).user_id.clone();
    let ids: Vec<String> = req["ids"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    s.inbox.retain(|i| !(i.recipient == me && ids.contains(&i.id)));
    Json(json!({}))
}

pub async fn stub_clear_inbox(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let me = s.account_of(&headers).user_id.clone();
    s.inbox.retain(|i| i.recipient != me);
    Json(json!({}))
}

/// The token as the accounts service mints it: a role for a store held
/// whole, a role per shared root for one held by shares, and the whole
/// store winning where an account holds both.
pub async fn stub_mint_token(State(state): State<StubState>, headers: HeaderMap) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    state.token_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if state.refuse_sessions.load(std::sync::atomic::Ordering::SeqCst) {
        return Err((StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized", "message": "invalid session" }))));
    }
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
    Ok(Json(json!({ "token": token, "exp": chrono::Utc::now().timestamp() + 300, "rpc_url": rpc_url })))
}

pub async fn stub_list_stores(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_of(&headers);
    let arr: Vec<_> = s
        .stores
        .iter()
        .filter(|st| st.user_id == account.user_id)
        .map(|st| {
            // The owner by the name they go by in the share, never their
            // email (docs/PRESENCE_CONTRACT.md section 3).
            let shared_by = st.shared_by.as_deref().map(|email| {
                let owner = s.account_by_email(Some(email)).user_id.clone();
                s.name_in(&owner, &st.store_id, &st.root)
            });
            json!({ "store_id": st.store_id, "name": st.name, "role": st.role, "kind": st.kind, "created_at": st.created_at, "root": st.root, "shared_by": shared_by })
        })
        .collect();
    Json(json!(arr))
}

pub async fn stub_create_store(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
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

pub async fn stub_get_keys(
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
pub async fn stub_put_keys(State(state): State<StubState>, AxPath(store_id): AxPath<String>, Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
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
pub async fn stub_list_members(
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
            "name": s.name_in(&account.user_id, &store_id, &root),
            "email": if caller_is_owner { account.email.clone() } else { String::new() },
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
    // Everyone reaching the scope: the whole-store holders, then its members.
    let mut people: Vec<serde_json::Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in s.stores.iter().filter(|row| row.store_id == store_id && (row.root.is_none() || (root.is_some() && row.root == root))) {
        if seen.insert(row.user_id.clone()) {
            let notice_key = s.accounts.iter().find(|a| a.user_id == row.user_id).and_then(|a| a.notice_key.as_ref().map(|(public, _)| public.clone()));
            people.push(json!({ "user_id": row.user_id, "name": s.name_in(&row.user_id, &store_id, &root), "role": row.role, "notice_key": notice_key }));
        }
    }
    Ok(Json(json!({ "members": members, "share_name": share_name, "people": people })))
}

pub async fn stub_me(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let a = s.account_of(&headers);
    let name = if a.display_name.is_empty() { a.email.split('@').next().unwrap_or_default().to_string() } else { a.display_name.clone() };
    Json(json!({ "id": a.user_id, "email": a.email, "display_name": a.display_name, "name": name, "notifications": !a.notifications_off, "notification_email": !a.email_off, "notification_push": !a.push_off, "push_key": "BStubVapidPublicKey" }))
}

pub async fn stub_put_me(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let user_id = s.account_of(&headers).user_id.clone();
    let name = req["display_name"].as_str().unwrap_or_default().trim().to_string();
    if let Some(a) = s.accounts.iter_mut().find(|a| a.user_id == user_id) {
        a.display_name = name.clone();
    }
    Json(json!({ "display_name": name }))
}

pub async fn stub_list_shares(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let me = s.account_of(&headers).user_id.clone();
    let mut out = Vec::new();
    for row in s.stores.iter().filter(|row| row.user_id == me) {
        if row.role == "owner" && row.root.is_none() {
            let mut roots: Vec<(Option<String>, String)> = Vec::new();
            for other in s.stores.iter().filter(|o| o.store_id == row.store_id && o.user_id != me && !(o.role == "owner" && o.root.is_none())) {
                if !roots.iter().any(|(r, _)| *r == other.root) {
                    roots.push((other.root.clone(), other.name.clone()));
                }
            }
            for (root, name) in roots {
                let chosen = s.share_names.get(&(me.clone(), row.store_id.clone(), root.clone())).cloned().unwrap_or_default();
                out.push(json!({ "store_id": row.store_id, "root": root, "name": name, "role": "owner", "owned": true, "owner_name": null, "kind": row.kind, "tier": "hosted", "my_name": chosen, "appears_as": s.name_in(&me, &row.store_id, &root) }));
            }
        } else {
            let owner = s.stores.iter().find(|o| o.store_id == row.store_id && o.role == "owner" && o.root.is_none()).map(|o| s.name_in(&o.user_id, &row.store_id, &row.root));
            let chosen = s.share_names.get(&(me.clone(), row.store_id.clone(), row.root.clone())).cloned().unwrap_or_default();
            out.push(json!({ "store_id": row.store_id, "root": row.root, "name": row.name, "role": row.role, "owned": false, "owner_name": owner, "kind": row.kind, "tier": "hosted", "my_name": chosen, "appears_as": s.name_in(&me, &row.store_id, &row.root) }));
        }
    }
    Json(json!(out))
}

pub async fn stub_put_share_name(
    State(state): State<StubState>,
    AxPath(store_id): AxPath<String>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let mut s = state.inner.lock().unwrap();
    let me = s.account_of(&headers).user_id.clone();
    let root = req["root"].as_str().map(str::to_string);
    let name = req["name"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        s.share_names.remove(&(me, store_id, root));
    } else {
        s.share_names.insert((me, store_id, root), name);
    }
    Json(json!({}))
}

pub async fn stub_put_member(
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
    // A share's members are editors, commenters or readers, and a share has a name.
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
pub async fn stub_delete_member(
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

pub async fn stub_delete_invitation(
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

pub async fn stub_jwks(State(state): State<StubState>) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let x = URL_SAFE_NO_PAD.encode(s.signing_key.verifying_key().to_bytes());
    Json(json!({ "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": s.kid, "x": x } ] }))
}

pub fn cheap_kdf_params() -> KdfParams {
    let mut params = KdfParams::generate();
    params.m_cost = 8 * 1024;
    params.t_cost = 1;
    params
}

/// A stub account with real key material.
pub fn stub_account(email: &str, password: &str) -> StubAccount {
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
        display_name: String::new(),
        notice_key: None,
        notifications_off: false,
        email_off: false,
        push_off: false,
    }
}

pub const ALICE: (&str, &str) = ("alice@example.com", "correct horse battery staple");
pub const BOB: (&str, &str) = ("bob@example.com", "a different horse entirely");
pub const CAROL: (&str, &str) = ("carol@example.com", "a third horse altogether");
/// Has no account until a test says so.
pub const DAVE: (&str, &str) = ("dave@example.com", "late to the stable");

pub struct Env {
    pub stub_url: String,
    pub stub: StubState,
    // Kept alive for the test's duration (dropping it would stop H).
    pub _h: PimbleServer,
    /// A client to `H` with the service token, for out-of-band verification.
    pub h_admin: PimbleClient,
    pub h_stores_dir: PathBuf,
    pub _h_dir: tempfile::TempDir,
    /// Every device's vault link reaches `H` through this, so a test can
    /// take the network away and give it back.
    pub relay: Relay,
}

/// A TCP relay in front of `H` that a test can cut.
#[derive(Clone)]
pub struct Relay {
    pub addr: std::net::SocketAddr,
    pub blocked: Arc<AtomicBool>,
    pub conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl Relay {
    pub async fn start(target: std::net::SocketAddr) -> Relay {
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

    pub fn cut(&self) {
        self.blocked.store(true, Ordering::SeqCst);
        for conn in self.conns.lock().unwrap().drain(..) {
            conn.abort();
        }
    }

    pub fn restore(&self) {
        self.blocked.store(false, Ordering::SeqCst);
    }
}

impl Env {
    pub fn h_store_path(&self, store_id: StoreId) -> PathBuf {
        self.h_stores_dir.join(format!("{store_id}.pimble"))
    }

    pub fn hosted_store_contains_plaintext(&self, store_id: StoreId, needle: &str) -> bool {
        contains_bytes_recursive(&self.h_store_path(store_id).join("vault"), needle.as_bytes())
    }

    #[allow(dead_code)]
    pub fn hosted_store_contains_plaintext_bytes(&self, store_id: StoreId, needle: &[u8]) -> bool {
        contains_bytes_recursive(&self.h_store_path(store_id), needle)
    }

    pub async fn sign_in(&self, client: &PimbleClient, who: (&str, &str)) {
        client.cloud_sign_in(&self.stub_url, who.0, who.1).await.expect("sign in");
    }

    /// The address gets an account (verified, with keys), and claims the
    /// invitations that were waiting for it.
    pub fn account_appears(&self, who: (&str, &str)) {
        let account = stub_account(who.0, who.1);
        let mut s = self.stub.inner.lock().unwrap();
        s.accounts.push(account);
        s.claim_invitations(who.0);
    }

    /// Whether `email`'s account holds an envelope for the share rooted at `root`.
    /// The account id of `email`.
    pub fn user_id(&self, email: &str) -> String {
        self.stub.inner.lock().unwrap().account_by_email(Some(email)).user_id.clone()
    }

    pub fn has_share_key(&self, store_id: StoreId, root: NodeId, email: &str) -> bool {
        let s = self.stub.inner.lock().unwrap();
        let Some(user_id) = s.accounts.iter().find(|a| a.email == email).map(|a| a.user_id.clone()) else { return false };
        s.has_key(&user_id, &store_id.to_string(), &Some(root.to_string()))
    }

    /// The scoped grants and invitations the accounts service holds for a share.
    pub fn scoped_rows(&self, store_id: StoreId, root: NodeId) -> usize {
        let s = self.stub.inner.lock().unwrap();
        let (store, root) = (store_id.to_string(), Some(root.to_string()));
        s.stores.iter().filter(|row| row.store_id == store && row.root == root).count() + s.invitations.iter().filter(|i| i.store_id == store && i.root == root).count()
    }

    /// The published scope of `root` on the hosted server, `None` when it has none.
    pub async fn scope_on_h(&self, store_id: StoreId, root: NodeId) -> Option<Vec<NodeId>> {
        self.h_admin.get_scopes(store_id).await.ok()?.into_iter().find(|scope| scope.root == root).map(|scope| scope.doc_ids)
    }

    /// The scope keys a hosted document's data key is wrapped under.
    pub async fn wrap_key_ids(&self, store_id: StoreId, doc: NodeId) -> Vec<Uuid> {
        self.h_admin
            .vault_fetch(store_id, VaultDocId::Node(doc), u64::MAX)
            .await
            .ok()
            .and_then(|fetch| fetch.keys)
            .map(|keys| keys.wraps.iter().map(|w| w.scope_key_id).collect())
            .unwrap_or_default()
    }

    pub async fn head_on_h(&self, store_id: StoreId, doc: NodeId) -> u64 {
        self.h_admin.vault_fetch(store_id, VaultDocId::Node(doc), u64::MAX).await.map(|fetch| fetch.head).unwrap_or(0)
    }
}

pub fn contains_bytes_recursive(dir: &Path, needle: &[u8]) -> bool {
    let Ok(entries) = tfs::read_dir(dir) else { return false };
    for path in entries {
        if tfs::is_dir(&path) {
            if contains_bytes_recursive(&path, needle) {
                return true;
            }
        } else if let Ok(bytes) = tfs::read(&path) {
            if bytes.windows(needle.len()).any(|w| w == needle) {
                return true;
            }
        }
    }
    false
}

pub async fn spawn_env() -> Env {
    let signing_key = fresh_signing_key();
    let kid = "test-kid".to_string();
    let issuer = "https://cloud.test".to_string();

    let h_dir = tempfile::tempdir().unwrap();
    let h_stores_dir = h_dir.path().join("stores");
    tfs::create_dir_all(&h_stores_dir).unwrap();

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
            share_names: HashMap::new(),
            inbox: Vec::new(),
            posted: Vec::new(),
            next_seq: 0,
            kinds: Vec::new(),
            push_subs: Vec::new(),
            watch_choices: HashMap::new(),
            watched: HashMap::new(),
        }),
        members_down: AtomicBool::new(false),
        refuse_sessions: AtomicBool::new(false),
        token_calls: std::sync::atomic::AtomicUsize::new(0),
        inbox_wake: tokio::sync::Notify::new(),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_addr = listener.local_addr().unwrap();
    let router = stub_router(Arc::clone(&state));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let stub_url = format!("http://{stub_addr}");
    let jwks_url = format!("{stub_url}/jwks.json");

    let h_service_token = "h-service-token".to_string();
    let mut h = PimbleServer::with_config(ServerConfig {
        auth_token: Some(h_service_token.clone()),
        jwks_url: Some(jwks_url.parse().unwrap()),
        jwt_issuer: Some(issuer),
        ..ServerConfig::in_dir(h_dir.path())
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
pub fn scope_keys_in(server_dir: &Path, store_id: StoreId) -> Vec<(Uuid, pimble_crypto::SymmetricKey)> {
    let json: serde_json::Value = serde_json::from_slice(&tfs::read(server_dir.join("keys.json")).expect("the keystore file")).unwrap();
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

pub async fn wait_all_seeded(env: &Env, store_id: StoreId, docs: &[NodeId]) {
    let seeded = wait_until(Duration::from_secs(15), || async {
        let listed = env.h_admin.vault_list_docs(store_id).await.unwrap_or_default();
        docs.iter().all(|id| listed.iter().any(|d| d.doc_id == VaultDocId::Node(*id) && d.head > 0))
    })
    .await;
    assert!(seeded, "every document must reach the hosted twin");
}

pub fn sorted(mut ids: Vec<NodeId>) -> Vec<String> {
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
pub struct Fixture {
    pub store_id: StoreId,
    pub root_id: NodeId,
    pub shared: NodeId,
    pub inside: NodeId,
    pub deeper: NodeId,
    pub leaf: NodeId,
    pub private: NodeId,
}

pub async fn hosted_fixture(env: &Env, alice: &PimbleClient, alice_dir: &Path) -> Fixture {
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
pub async fn member_device(env: &Env, who: (&str, &str), fx: &Fixture) -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let (server, client, dir) = start_local_server().await;
    env.sign_in(&client, who).await;
    client.cloud_add_hosted_store(fx.store_id).await.expect("a share is added like any hosted store");
    let pulled = wait_until(Duration::from_secs(15), || async { node_text(&client, fx.store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "{}'s replica pulls the scope and reads it with the share's key", who.0);
    (server, client, dir)
}
