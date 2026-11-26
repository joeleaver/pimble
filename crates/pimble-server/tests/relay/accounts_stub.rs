//! The accounts-service stub, grown by what the relay tier adds to the real
//! service.

use super::*;

// ── The accounts-service stub ──────────────────────────────────────────────

/// One grant: `GET /stores` answers one row per grant of the caller's.
pub(crate) struct StubStoreRow {
    pub(crate) user_id: String,
    pub(crate) store_id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) tier: String,
    pub(crate) created_at: String,
    pub(crate) role: String,
    /// The shared node, for a share.
    pub(crate) root: Option<String>,
    pub(crate) shared_by: Option<String>,
}

pub(crate) struct KeyGrantRow {
    pub(crate) user_id: String,
    pub(crate) key_id: String,
    pub(crate) envelope: KeyEnvelope,
    pub(crate) root: Option<String>,
}

pub(crate) struct StubAccount {
    pub(crate) kdf: KdfParams,
    pub(crate) user_id: String,
    pub(crate) email: String,
    pub(crate) session: String,
    pub(crate) account_key_blob: AccountKeyBlob,
    pub(crate) account_public_keys: pimble_crypto::AccountPublicKeys,
}

pub(crate) struct StubInner {
    /// The owner first; a request with no session the stub knows is theirs.
    pub(crate) accounts: Vec<StubAccount>,
    pub(crate) stores: Vec<StubStoreRow>,
    pub(crate) key_grants: HashMap<String, Vec<KeyGrantRow>>,
    pub(crate) signing_key: SigningKey,
    pub(crate) kid: String,
    pub(crate) issuer: String,
    pub(crate) h_client: Option<Arc<PimbleClient>>,
    pub(crate) h_rpc_url: String,
    pub(crate) h_stores_dir: PathBuf,
    /// This stub's own address, which is where its relay is.
    pub(crate) own_addr: String,
    /// Every `POST /stores` body, as it arrived: what a test reads to say
    /// what did and did not leave the owner's machine.
    pub(crate) create_store_bodies: Vec<serde_json::Value>,
}

pub(crate) struct Stub {
    pub(crate) inner: Mutex<StubInner>,
    pub(crate) relay: RelayStub,
}

pub(crate) type StubState = Arc<Stub>;

pub(crate) fn stub_router(state: StubState) -> Router {
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

pub(crate) fn session_of(headers: &HeaderMap) -> Option<&str> {
    headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "))
}

impl StubInner {
    pub(crate) fn account_of(&self, headers: &HeaderMap) -> &StubAccount {
        let session = session_of(headers);
        self.accounts.iter().find(|a| Some(a.session.as_str()) == session).unwrap_or(&self.accounts[0])
    }

    pub(crate) fn account_by_email(&self, email: Option<&str>) -> &StubAccount {
        self.accounts.iter().find(|a| Some(a.email.as_str()) == email).unwrap_or(&self.accounts[0])
    }

    pub(crate) fn is_owner(&self, user_id: &str, store_id: &str) -> bool {
        self.stores.iter().any(|row| row.user_id == user_id && row.store_id == store_id && row.root.is_none() && row.role == "owner")
    }

    pub(crate) fn has_key(&self, user_id: &str, store_id: &str, root: &Option<String>) -> bool {
        self.key_grants.get(store_id).is_some_and(|rows| rows.iter().any(|g| g.user_id == user_id && &g.root == root))
    }

    pub(crate) fn tier_of(&self, store_id: &str) -> String {
        self.stores.iter().find(|row| row.store_id == store_id).map(|row| row.tier.clone()).unwrap_or_else(|| "hosted".into())
    }

    /// The `stores` claim as the accounts service mints it: a role for a
    /// store held whole, a role per shared root for one held by shares, and
    /// the whole store winning where an account holds both.
    pub(crate) fn stores_claim(&self, user_id: &str) -> serde_json::Map<String, serde_json::Value> {
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
    pub(crate) fn store_token(&self, account: &StubAccount, store_id: &str) -> Option<String> {
        let mut claim = self.stores_claim(&account.user_id);
        claim.retain(|id, _| id == store_id);
        (!claim.is_empty()).then(|| make_jwt(&self.signing_key, &self.kid, &self.issuer, &account.user_id, &account.email, &claim))
    }
}

pub(crate) async fn stub_kdf(State(state): State<StubState>, Query(query): Query<HashMap<String, String>>) -> Json<KdfParams> {
    let s = state.inner.lock().unwrap();
    Json(s.account_by_email(query.get("email").map(String::as_str)).kdf.clone())
}

pub(crate) async fn stub_login(State(state): State<StubState>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_by_email(req["email"].as_str());
    Json(json!({ "user": { "id": account.user_id, "email": account.email }, "session": account.session, "token": "", "exp": 0 }))
}

pub(crate) async fn stub_me_keys(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let account = s.account_of(&headers);
    Json(json!({ "public_keys": account.account_public_keys, "kdf": account.kdf, "account_key_blob": account.account_key_blob }))
}

/// `stores`: every relay-tier store the account holds a grant on, its own
/// included, each with the relay's endpoint for it and its own token.
pub(crate) async fn stub_mint_token(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
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

pub(crate) async fn stub_list_stores(State(state): State<StubState>, headers: HeaderMap) -> Json<serde_json::Value> {
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
pub(crate) async fn stub_create_store(State(state): State<StubState>, headers: HeaderMap, Json(req): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
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
pub(crate) async fn stub_delete_store(State(state): State<StubState>, AxPath(store_id): AxPath<String>, headers: HeaderMap) -> Result<Json<serde_json::Value>, StatusCode> {
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

pub(crate) async fn stub_get_keys(
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
pub(crate) async fn stub_put_keys(State(state): State<StubState>, AxPath(store_id): AxPath<String>, Json(body): Json<serde_json::Value>) -> Result<Json<serde_json::Value>, StatusCode> {
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
pub(crate) async fn stub_list_members(
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
pub(crate) async fn stub_put_member(
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

pub(crate) async fn stub_delete_member(
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

pub(crate) async fn stub_delete_invitation() -> Json<serde_json::Value> {
    Json(json!({}))
}

pub(crate) async fn stub_jwks(State(state): State<StubState>) -> Json<serde_json::Value> {
    let s = state.inner.lock().unwrap();
    let x = URL_SAFE_NO_PAD.encode(s.signing_key.verifying_key().to_bytes());
    Json(json!({ "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": s.kid, "x": x } ] }))
}
