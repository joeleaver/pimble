//! The accounts-service stub: its state, its routes, and its accounts,
//! stores, keys, members and shares.

use super::*;

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
    /// A role a minted token carries for (user id, store id) in place of the
    /// one the rows list: a hosted server that would take a write the
    /// device believes it may not send, so a test can prove it sent none.
    pub token_role: HashMap<(String, String), String>,
}

pub struct Stub {
    pub inner: Mutex<StubInner>,
    /// The members endpoints answer 503 while set: "the service is unreachable".
    pub members_down: AtomicBool,
    /// `GET /me/shares` takes a minute to answer, as a hung service would.
    pub shares_stalled: AtomicBool,
    /// The next `GET /me/shares` takes its answer, says so on
    /// `share_list_held`, and sends it when `share_list_release` says.
    pub hold_next_share_list: AtomicBool,
    pub share_list_held: tokio::sync::Notify,
    pub share_list_release: tokio::sync::Notify,
    /// How many times `GET /stores` was asked.
    pub store_list_calls: AtomicUsize,
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
            let role = s.token_role.get(&(row.user_id.clone(), row.store_id.clone())).unwrap_or(&row.role);
            stores_claim.insert(row.store_id.clone(), json!(role));
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
    state.store_list_calls.fetch_add(1, Ordering::SeqCst);
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
    if state.shares_stalled.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    let out = shares_of(&state, &headers);
    if state.hold_next_share_list.swap(false, Ordering::SeqCst) {
        state.share_list_held.notify_one();
        state.share_list_release.notified().await;
    }
    Json(json!(out))
}

/// What `GET /me/shares` answers `headers`' account now.
fn shares_of(state: &StubState, headers: &HeaderMap) -> Vec<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let me = s.account_of(headers).user_id.clone();
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
    out
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
