//! A store's key grants: `GET/PUT /stores/{id}/keys`
//! (docs/CRYPTO_CONTRACT.md "Accounts service endpoints").

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pimble_crypto::KeyEnvelope;

use super::stores::{normalize_root, require_live_store, require_role_for_scope, to_json, verify_envelope_signature, ScopeQuery, WHOLE_STORE};
use crate::error::{CloudError, CloudResult};
use crate::session::AuthedUser;
use crate::state::AppState;

// ── Key grants (docs/CRYPTO_CONTRACT.md "Accounts service endpoints") ────

#[derive(Serialize)]
pub struct KeyGrantView {
    key_id: String,
    envelope: KeyEnvelope,
}

/// One account whose signature on an envelope for this store is to be
/// believed — the store's owners. A
/// recipient is handed the store key by whichever of the owner's devices
/// sweeps first, so "the envelope is signed by me" is not enough to check
/// against any more — it has to be "signed by me or by an owner of this
/// store".
#[derive(Serialize)]
pub struct SignerView {
    user_id: String,
    email: String,
    public_signing_key: String,
}

#[derive(Serialize)]
pub struct KeyGrantsResponse {
    envelopes: Vec<KeyGrantView>,
    signers: Vec<SignerView>,
}

/// `GET /api/v1/stores/{id}/keys[?root=<node id>]` — the caller's own
/// envelopes for one scope of this store (the store key without a `root`,
/// that share's key with one), never another member's, plus the owners whose
/// signatures on them are legitimate.
pub async fn get_store_keys(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Query(scope): Query<ScopeQuery>,
) -> CloudResult<Json<KeyGrantsResponse>> {
    let store = require_live_store(&state, &store_id).await?;
    let root = normalize_root(&scope.root)?;
    require_role_for_scope(&state, &store, authed.user.rid, &root).await?;

    let grants = state.db.key_grants_for_user_store_and_root(authed.user.rid, store.rid, &root).await?;
    let mut envelopes = Vec::with_capacity(grants.len());
    for grant in grants {
        let envelope: KeyEnvelope =
            serde_json::from_str(&grant.envelope).map_err(|e| CloudError::Internal(format!("stored envelope is not valid JSON: {e}")))?;
        envelopes.push(KeyGrantView { key_id: grant.key_id, envelope });
    }

    let mut signers = Vec::new();
    for grant in state.db.grants_for_store(store.rid).await? {
        if grant.role != "owner" || !grant.is_whole_store() {
            continue;
        }
        if let Some(owner) = state.db.get_user(grant.user_rid).await? {
            // A public signing key is all a verifier needs; an owner without
            // one (a legacy row that has not been cleaned up yet) simply
            // cannot have signed anything, so leaving them out is right.
            if let Some(public_signing_key) = owner.public_signing_key.clone() {
                signers.push(SignerView { user_id: owner.user_uuid, email: owner.email, public_signing_key });
            }
        }
    }
    Ok(Json(KeyGrantsResponse { envelopes, signers }))
}

#[derive(Deserialize)]
pub struct EnvelopeUpsert {
    pub user_id: String,
    pub key_id: String,
    pub envelope: KeyEnvelope,
    /// Which scope key this envelope carries: the shared node, or absent for
    /// the store key.
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Deserialize)]
pub struct PutStoreKeysRequest {
    pub envelopes: Vec<EnvelopeUpsert>,
}

/// `PUT /api/v1/stores/{id}/keys` — upserts one or more (user, key id, scope)
/// envelopes. Ownership rule: an owner may set anyone's; a member may set
/// their own. Every envelope must be signed by the CALLER (whoever is
/// distributing the key), verified against the caller's own
/// `public_signing_key`; the recipient checks it against the `signers` of
/// `GET .../keys`, which are the store's owners.
pub async fn put_store_keys(
    State(state): State<AppState>,
    authed: AuthedUser,
    Path(store_id): Path<String>,
    Json(req): Json<PutStoreKeysRequest>,
) -> CloudResult<Json<Value>> {
    let store = require_live_store(&state, &store_id).await?;
    let caller_grants = state.db.grants_for_user_and_store(authed.user.rid, store.rid).await?;
    if caller_grants.is_empty() {
        return Err(CloudError::Forbidden("not a member of this store".to_string()));
    }
    let caller_is_owner = caller_grants.iter().any(|g| g.role == "owner" && g.is_whole_store());
    // A session only ever exists for a real Phase 2a account now (`login`
    // refuses a legacy no-key-material one outright), so this is always
    // `Some` in practice; treated as `Internal`, not a panic, if it somehow
    // isn't.
    let caller_signing_key = authed
        .user
        .public_signing_key
        .as_deref()
        .ok_or_else(|| CloudError::Internal("session exists for a user with no key material".to_string()))?;

    for item in &req.envelopes {
        if item.user_id != authed.user.user_uuid && !caller_is_owner {
            return Err(CloudError::Forbidden("only an owner can set another member's keys".to_string()));
        }
        let root = normalize_root(item.root.as_deref().unwrap_or(WHOLE_STORE))?;
        verify_envelope_signature(&item.envelope, caller_signing_key)?;

        let target = state
            .db
            .find_user_by_uuid(&item.user_id)
            .await?
            .ok_or_else(|| CloudError::NotFound(format!("no such user: {}", item.user_id)))?;
        // The key only goes to somebody the scope is already shared with: a
        // whole-store grant reaches every scope, a scoped one only its own.
        let covered = state
            .db
            .grants_for_user_and_store(target.rid, store.rid)
            .await?
            .into_iter()
            .any(|g| g.is_whole_store() || g.root == root);
        if !covered {
            return Err(CloudError::BadRequest(format!("{} is not a member of this scope of the store", item.user_id)));
        }

        let envelope_json = to_json("envelope", &item.envelope)?;
        match state.db.find_key_grant(target.rid, store.rid, &item.key_id, &root).await? {
            Some(existing) => state.db.update_key_grant_envelope(existing.rid, &envelope_json).await?,
            None => {
                state.db.create_key_grant(target.rid, store.rid, &store.store_id, &item.key_id, &root, &envelope_json).await?;
            }
        }
    }
    Ok(Json(json!({})))
}
