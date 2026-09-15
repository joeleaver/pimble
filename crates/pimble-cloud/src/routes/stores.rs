//! `GET/POST /stores`, `DELETE /stores/{id}`, and the members sub-resource.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::HostedStoreRow;
use crate::error::{CloudError, CloudResult};
use crate::session::AuthedUser;
use crate::state::AppState;

fn rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default().to_rfc3339()
}

const ROLES: [&str; 3] = ["owner", "editor", "reader"];

#[derive(Serialize)]
pub struct StoreView {
    store_id: String,
    name: String,
    role: String,
    created_at: String,
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
                out.push(StoreView { store_id: store.store_id, name: store.name, role: grant.role, created_at: rfc3339(store.created_at_ms) });
            }
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateStoreRequest {
    pub name: String,
}

pub async fn create_store(State(state): State<AppState>, authed: AuthedUser, Json(req): Json<CreateStoreRequest>) -> CloudResult<Json<StoreView>> {
    if req.name.trim().is_empty() {
        return Err(CloudError::BadRequest("name must not be empty".to_string()));
    }
    let (store_id, dir_name) = state.pimble.create_store(req.name.trim()).await?;
    let store_id_str = store_id.as_uuid().to_string();
    let hosted = state.db.create_hosted_store(&store_id_str, req.name.trim(), &dir_name).await?;
    state.db.create_grant(authed.user.rid, hosted.rid, &store_id_str, "owner").await?;
    Ok(Json(StoreView { store_id: store_id_str, name: hosted.name, role: "owner".to_string(), created_at: rfc3339(hosted.created_at_ms) }))
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
