//! `GET/POST /stores`, `DELETE /stores/{id}`, the members sub-resource, and
//! the key-grants sub-resource (docs/CRYPTO_CONTRACT.md, Phase 2a).

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pimble_core::{StoreId, StoreKind};
use pimble_crypto::KeyEnvelope;

use crate::db::HostedStoreRow;
use crate::envelope::verify_envelope_signature;
use crate::error::{CloudError, CloudResult};
use crate::session::AuthedUser;
use crate::state::AppState;

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default().to_rfc3339()
}

const ROLES: [&str; 3] = ["owner", "editor", "reader"];

fn store_kind_str(kind: StoreKind) -> &'static str {
    match kind {
        StoreKind::Plain => "plain",
        StoreKind::Vault => "vault",
    }
}

fn to_json(field: &'static str, value: &impl Serialize) -> CloudResult<String> {
    serde_json::to_string(value).map_err(|e| CloudError::Internal(format!("serializing {field}: {e}")))
}

#[derive(Serialize)]
pub struct StoreView {
    store_id: String,
    name: String,
    role: String,
    kind: String,
    created_at: String,
}

fn store_view(store: &HostedStoreRow, role: &str) -> StoreView {
    StoreView { store_id: store.store_id.clone(), name: store.name.clone(), role: role.to_string(), kind: store.kind.clone(), created_at: rfc3339(store.created_at_ms) }
}

/// Look a store up by its external id, refusing (as 404) one that doesn't
/// exist or has been soft-deleted — a deleted store is gone as far as every
/// endpoint in this file is concerned.
async fn require_live_store(state: &AppState, store_id: &str) -> CloudResult<HostedStoreRow> {
    let store = state
        .db
        .find_hosted_store(store_id)
        .await?
        .ok_or_else(|| CloudError::NotFound("no such store".to_string()))?;
    if store.deleted {
        return Err(CloudError::NotFound("no such store".to_string()));
    }
    Ok(store)
}

/// The caller's role on `store`, or `Forbidden` if they have none.
async fn require_any_grant(state: &AppState, store: &HostedStoreRow, user_rid: u64) -> CloudResult<String> {
    state
        .db
        .find_grant(user_rid, store.rid)
        .await?
        .map(|g| g.role)
        .ok_or_else(|| CloudError::Forbidden("not a member of this store".to_string()))
}

/// `Forbidden` unless the caller is an owner of `store`.
async fn require_owner(state: &AppState, store: &HostedStoreRow, user_rid: u64) -> CloudResult<()> {
    match require_any_grant(state, store, user_rid).await? {
        role if role == "owner" => Ok(()),
        _ => Err(CloudError::Forbidden("only an owner can do this".to_string())),
    }
}

/// A store must always have at least one owner. Both the endpoint that
/// removes a grant and the one that changes its role reach this: a store
/// cannot end up ownerless either by removing its last owner outright or by
/// demoting them to `editor`/`reader`.
const LAST_OWNER_ERROR: &str = "cannot remove the last owner of a store";

async fn owner_count(state: &AppState, store_rid: u64) -> CloudResult<usize> {
    Ok(state.db.grants_for_store(store_rid).await?.into_iter().filter(|g| g.role == "owner").count())
}

pub async fn list_stores(State(state): State<AppState>, authed: AuthedUser) -> CloudResult<Json<Vec<StoreView>>> {
    let grants = state.db.grants_for_user(authed.user.rid).await?;
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants {
        if let Some(store) = state.db.get_hosted_store(grant.store_rid).await? {
            if !store.deleted {
                out.push(store_view(&store, &grant.role));
            }
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateStoreRequest {
    pub name: String,
    /// `"plain"` (default) or `"vault"` (docs/CRYPTO_CONTRACT.md).
    #[serde(default)]
    pub kind: StoreKind,
    /// A chosen store id (a desktop app hosting an existing local store
    /// under its own id); refused server-side if that id is already open.
    #[serde(default)]
    pub store_id: Option<String>,
}

pub async fn create_store(State(state): State<AppState>, authed: AuthedUser, Json(req): Json<CreateStoreRequest>) -> CloudResult<Json<StoreView>> {
    if req.name.trim().is_empty() {
        return Err(CloudError::BadRequest("name must not be empty".to_string()));
    }
    let store_id = req
        .store_id
        .as_deref()
        .map(StoreId::parse)
        .transpose()
        .map_err(|e| CloudError::BadRequest(format!("store_id is not a valid id: {e}")))?;

    let (created_store_id, dir_name) = state.pimble.create_store(req.name.trim(), req.kind, store_id).await?;
    let store_id_str = created_store_id.as_uuid().to_string();
    let kind_str = store_kind_str(req.kind);
    let hosted = state.db.create_hosted_store(&store_id_str, req.name.trim(), &dir_name, kind_str).await?;
    state.db.create_grant(authed.user.rid, hosted.rid, &store_id_str, "owner").await?;
    Ok(Json(store_view(&hosted, "owner")))
}

pub async fn delete_store(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    require_owner(&state, &store, authed.user.rid).await?;
    state.db.delete_grants_for_store(store.rid).await?;
    state.db.mark_store_deleted(store.rid).await?;
    Ok(Json(json!({})))
}

#[derive(Serialize)]
pub struct MemberView {
    user_id: String,
    email: String,
    role: String,
}

pub async fn list_members(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>) -> CloudResult<Json<Vec<MemberView>>> {
    let store = require_live_store(&state, &store_id).await?;
    require_any_grant(&state, &store, authed.user.rid).await?;

    let grants = state.db.grants_for_store(store.rid).await?;
    let mut out = Vec::with_capacity(grants.len());
    for grant in grants {
        if let Some(user) = state.db.get_user(grant.user_rid).await? {
            out.push(MemberView { user_id: user.user_uuid, email: user.email, role: grant.role });
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct PutMemberRequest {
    pub email: String,
    pub role: String,
}

pub async fn put_member(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Json(req): Json<PutMemberRequest>,
) -> CloudResult<Json<MemberView>> {
    let store = require_live_store(&state, &store_id).await?;
    require_owner(&state, &store, authed.user.rid).await?;

    if !ROLES.contains(&req.role.as_str()) {
        return Err(CloudError::BadRequest(format!("role must be one of {ROLES:?}")));
    }
    let target = state
        .db
        .find_user_by_email(&req.email)
        .await?
        .ok_or_else(|| CloudError::NotFound("no account with this email".to_string()))?;

    match state.db.find_grant(target.rid, store.rid).await? {
        Some(existing) => {
            // Demoting the store's last owner to editor/reader is exactly as
            // forbidden as removing them outright (`delete_member` below) —
            // both leave the store with zero owners.
            if existing.role == "owner" && req.role != "owner" && owner_count(&state, store.rid).await? <= 1 {
                return Err(CloudError::Conflict(LAST_OWNER_ERROR.to_string()));
            }
            state.db.update_grant_role(existing.rid, &req.role).await?;
        }
        None => {
            state.db.create_grant(target.rid, store.rid, &store.store_id, &req.role).await?;
        }
    }
    Ok(Json(MemberView { user_id: target.user_uuid, email: target.email, role: req.role }))
}

pub async fn delete_member(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path((store_id, user_id)): Path<(String, String)>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    require_owner(&state, &store, authed.user.rid).await?;

    let target = state
        .db
        .find_user_by_uuid(&user_id)
        .await?
        .ok_or_else(|| CloudError::NotFound("no such member".to_string()))?;
    let grant = state
        .db
        .find_grant(target.rid, store.rid)
        .await?
        .ok_or_else(|| CloudError::NotFound("no such member".to_string()))?;

    if grant.role == "owner" && owner_count(&state, store.rid).await? <= 1 {
        return Err(CloudError::Conflict(LAST_OWNER_ERROR.to_string()));
    }
    state.db.delete_grant(grant.rid).await?;
    Ok(Json(json!({})))
}

// ── Key grants (docs/CRYPTO_CONTRACT.md "Accounts service endpoints") ────

#[derive(Serialize)]
pub struct KeyGrantView {
    key_id: String,
    envelope: KeyEnvelope,
}

#[derive(Serialize)]
pub struct KeyGrantsResponse {
    envelopes: Vec<KeyGrantView>,
}

/// `GET /api/v1/stores/{id}/keys` — the caller's own envelopes for this
/// store, never another member's (any grant may read their own).
pub async fn get_store_keys(State(state): State<AppState>, authed: AuthedUser, Path(store_id): Path<String>) -> CloudResult<Json<KeyGrantsResponse>> {
    let store = require_live_store(&state, &store_id).await?;
    require_any_grant(&state, &store, authed.user.rid).await?;

    let grants = state.db.key_grants_for_user_and_store(authed.user.rid, store.rid).await?;
    let mut envelopes = Vec::with_capacity(grants.len());
    for grant in grants {
        let envelope: KeyEnvelope =
            serde_json::from_str(&grant.envelope).map_err(|e| CloudError::Internal(format!("stored envelope is not valid JSON: {e}")))?;
        envelopes.push(KeyGrantView { key_id: grant.key_id, envelope });
    }
    Ok(Json(KeyGrantsResponse { envelopes }))
}

#[derive(Deserialize)]
pub struct EnvelopeUpsert {
    pub user_id: String,
    pub key_id: String,
    pub envelope: KeyEnvelope,
}

#[derive(Deserialize)]
pub struct PutStoreKeysRequest {
    pub envelopes: Vec<EnvelopeUpsert>,
}

/// `PUT /api/v1/stores/{id}/keys` — upserts one or more (user, key id)
/// envelopes. Ownership rule (docs/CRYPTO_CONTRACT.md): an owner or editor
/// may always set their OWN envelopes; only an owner may set another
/// member's. Every envelope must be signed by the CALLER (whoever is
/// distributing the key), verified against the caller's own
/// `public_signing_key` — see `crate::envelope` for the caveat that this
/// verification is a stand-in for `pimble_crypto::unwrap_key`'s sibling,
/// not yet implemented.
pub async fn put_store_keys(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Json(req): Json<PutStoreKeysRequest>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    let caller_role = require_any_grant(&state, &store, authed.user.rid).await?;
    if caller_role != "owner" && caller_role != "editor" {
        return Err(CloudError::Forbidden("only an owner or editor can set store keys".to_string()));
    }

    for item in &req.envelopes {
        if item.user_id != authed.user.user_uuid && caller_role != "owner" {
            return Err(CloudError::Forbidden("only an owner can set another member's keys".to_string()));
        }
        verify_envelope_signature(&item.envelope, &authed.user.public_signing_key)?;

        let target = state
            .db
            .find_user_by_uuid(&item.user_id)
            .await?
            .ok_or_else(|| CloudError::NotFound(format!("no such user: {}", item.user_id)))?;
        state
            .db
            .find_grant(target.rid, store.rid)
            .await?
            .ok_or_else(|| CloudError::BadRequest(format!("{} is not a member of this store", item.user_id)))?;

        let envelope_json = to_json("envelope", &item.envelope)?;
        match state.db.find_key_grant(target.rid, store.rid, &item.key_id).await? {
            Some(existing) => state.db.update_key_grant_envelope(existing.rid, &envelope_json).await?,
            None => {
                state.db.create_key_grant(target.rid, store.rid, &store.store_id, &item.key_id, &envelope_json).await?;
            }
        }
    }
    Ok(Json(json!({})))
}
