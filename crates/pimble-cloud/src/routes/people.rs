//! The names people go by (docs/PRESENCE_CONTRACT.md section 3, Joe
//! 2026-10-22): `PUT /me` sets the default name, `GET /me/shares` lists every
//! share the caller takes part in (owned or joined) with the name they go by
//! in each, and `PUT /me/shares/{store}` sets one of those names. Members of
//! a share see these names and never each other's emails; `GET
//! /stores/{id}/members` answers them as `people`.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::stores::{
    is_store_owner, normalize_root, require_live_store, require_role_for_scope, root_view, row_name, shared_by, stored_name_or_empty, Names, UNNAMED_SHARE,
    WHOLE_STORE,
};
use crate::error::{CloudError, CloudResult};
use crate::session::AuthedUser;
use crate::state::AppState;

/// How long a person's name may be: what a pane, a caret's flag and a member
/// list show in one line.
const MAX_PERSON_NAME_CHARS: usize = 80;

fn person_name(raw: &str) -> CloudResult<String> {
    let name = stored_name_or_empty(raw, "name")?;
    if name.chars().count() > MAX_PERSON_NAME_CHARS {
        return Err(CloudError::BadRequest(format!("a name must be at most {MAX_PERSON_NAME_CHARS} characters")));
    }
    Ok(name)
}

#[derive(Deserialize)]
pub struct PutMeRequest {
    /// The default name; empty clears it (the email's local part stands in).
    pub display_name: String,
}

/// `PUT /api/v1/me` — the caller's default name.
pub async fn put_me(State(state): State<AppState>, authed: AuthedUser, Json(req): Json<PutMeRequest>) -> CloudResult<Json<Value>> {
    let name = person_name(&req.display_name)?;
    state.db.set_display_name(authed.user.rid, &name).await?;
    Ok(Json(json!({ "display_name": name })))
}

/// One share the caller takes part in, as the Shares dialog lists it.
#[derive(Serialize)]
pub struct ShareView {
    store_id: String,
    /// The shared node; `null` for a whole store held by somebody else's
    /// grant (or, among owned rows, others holding the whole store).
    root: Option<String>,
    /// The share's name: what its owner called it, or the store's name for
    /// a whole store.
    name: String,
    /// The caller's role in it (`owner` for a share they own).
    role: String,
    /// Whether the caller owns the store this share is of.
    owned: bool,
    /// The owner's name in this share, for a share somebody else owns.
    owner_name: Option<String>,
    kind: String,
    tier: String,
    /// The name the caller chose for this share, `""` for none.
    my_name: String,
    /// What the others in it see: `my_name`, else the default name.
    appears_as: String,
}

/// `GET /api/v1/me/shares` — the shares the caller takes part in: every
/// grant somebody else gave them, and every share of a store they own that
/// someone else holds or was invited to.
pub async fn list_shares(State(state): State<AppState>, authed: AuthedUser) -> CloudResult<Json<Vec<ShareView>>> {
    let me = &authed.user;
    let mut out = Vec::new();
    for grant in state.db.grants_for_user(me.rid).await? {
        let Some(store) = state.db.get_hosted_store(grant.store_rid).await? else { continue };
        if store.deleted {
            continue;
        }
        let names = Names::of_store(&state, store.rid).await?;
        if grant.role == "owner" && grant.is_whole_store() {
            // The owner's own shares: every scope others reach.
            let mut roots: Vec<(String, String)> = Vec::new();
            let mut add = |root: &str, share_name: &str| {
                if !roots.iter().any(|(r, _)| r == root) {
                    roots.push((root.to_string(), share_name.to_string()));
                }
            };
            for other in state.db.grants_for_store(store.rid).await? {
                if other.user_rid != me.rid && !(other.role == "owner" && other.is_whole_store()) {
                    add(&other.root, &other.share_name);
                }
            }
            for invitation in state.db.invitations_for_store(store.rid).await? {
                add(&invitation.root, &invitation.share_name);
            }
            for (root, share_name) in roots {
                let name = if root == WHOLE_STORE {
                    store.name.clone()
                } else if share_name.is_empty() {
                    UNNAMED_SHARE.to_string()
                } else {
                    share_name
                };
                out.push(ShareView {
                    store_id: store.store_id.clone(),
                    root: root_view(&root),
                    name,
                    role: "owner".to_string(),
                    owned: true,
                    owner_name: None,
                    kind: store.kind.clone(),
                    tier: store.tier.clone(),
                    my_name: names.chosen(me.rid, &root),
                    appears_as: names.name_of(me, &root),
                });
            }
        } else {
            out.push(ShareView {
                store_id: store.store_id.clone(),
                root: root_view(&grant.root),
                name: row_name(&store, &grant),
                role: grant.role.clone(),
                owned: false,
                owner_name: shared_by(&state, &store, &grant).await?,
                kind: store.kind.clone(),
                tier: store.tier.clone(),
                my_name: names.chosen(me.rid, &grant.root),
                appears_as: names.name_of(me, &grant.root),
            });
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct PutShareNameRequest {
    /// The share's node; absent for a whole store.
    #[serde(default)]
    pub root: Option<String>,
    /// The name to go by there; empty means the default name.
    pub name: String,
}

/// `PUT /api/v1/me/shares/{store_id}` — the name the caller goes by in one
/// share they reach (or own).
pub async fn put_share_name(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Json(req): Json<PutShareNameRequest>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    let root = normalize_root(req.root.as_deref().unwrap_or(""))?;
    if !is_store_owner(&state, &store, authed.user.rid).await? {
        require_role_for_scope(&state, &store, authed.user.rid, &root).await?;
    }
    let name = person_name(&req.name)?;
    state.db.put_share_name(authed.user.rid, store.rid, &store.store_id, &root, &name).await?;
    let appears_as = if name.is_empty() { authed.user.default_name() } else { name.clone() };
    Ok(Json(json!({ "store_id": store.store_id, "root": root_view(&root), "my_name": name, "appears_as": appears_as })))
}
