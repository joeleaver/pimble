//! `POST /recover/start`, `GET /recover/{token}`, `POST
//! /recover/{token}/complete`, `POST /recover/{token}/delete-account`,
//! `POST /me/password`, `POST /me/recovery-code`: the password and the
//! recovery code, replaced with what the client wrapped (docs/CRYPTO_CONTRACT.md
//! "Phase 2a-2").
//!
//! The server holds neither password nor recovery KEK, so recovery can only
//! replace what the recovery code unwrapped client-side; a request naming an
//! unfamiliar account is never distinguishable from one naming a real,
//! unverified, or keyless account (all "202 always" or "404
//! recovery_invalid", never a different code).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pimble_crypto::{AccountKeyBlob, AccountPublicKeys, KdfParams};

use super::accounts::{to_json, validate_blob, validate_kdf, UserView};
use crate::auth::{hash_password, hash_recovery_token, new_recovery_token, verify_password_constant_time};
use crate::db::UserRow;
use crate::error::{CloudError, CloudResult};
use crate::mail::recovery_email;
use crate::session::AuthedUser;
use crate::state::AppState;

/// "1 hour" (docs/CRYPTO_CONTRACT.md, "Phase 2a-2").
const RECOVERY_TTL_MS: i64 = 60 * 60 * 1000;

/// The `session_epoch` a password change or a recovery raises `user`'s to:
/// above every session made so far. The clock, so two changes at once pick
/// different epochs; one above the stored epoch should the clock be behind.
fn next_session_epoch(user: &UserRow) -> u64 {
    let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
    now.max(user.session_epoch.saturating_add(1))
}

/// Delete the rows of `user_rid`'s sessions ended by raising its epoch to
/// `epoch`, all but `keep`. Housekeeping only: they are refused already, so
/// a failure here is logged and the change still answers that it was made.
async fn delete_ended_sessions(state: &AppState, user_rid: u64, epoch: u64, keep: Option<u64>) {
    let sessions = match state.db.sessions_for_user(user_rid).await {
        Ok(sessions) => sessions,
        Err(e) => {
            tracing::warn!(error = %e, "listing the sessions a password change ended failed; they are refused, and stay as rows");
            return;
        }
    };
    for session in sessions.into_iter().filter(|s| Some(s.rid) != keep && s.epoch < epoch) {
        if let Err(e) = state.db.delete_session(session.rid).await {
            tracing::warn!(error = %e, "deleting a session a password change ended failed; it is refused, and stays as a row");
        }
    }
}

// ── Recovery (docs/CRYPTO_CONTRACT.md "Phase 2a-2") ─────────────────────

#[derive(Deserialize)]
pub struct RecoverStartRequest {
    pub email: String,
}

/// `POST /api/v1/recover/start` — 202 always (no enumeration); mails a
/// recovery link only for a real, verified account that has key material,
/// through the same mailer `start_verification` uses and a limiter of the
/// same one-per-minute-per-address shape (docs/CRYPTO_CONTRACT.md: "the
/// same mailer and the same one-per-minute limit as verification") — its
/// own instance (`recovery_rate_limit`), not literally `resend_rate_limit`:
/// sharing that bucket would leave a brand-new signup's address
/// rate-limited here for a minute purely because signup itself just sent a
/// verification email.
pub async fn recover_start(State(state): State<AppState>, Json(req): Json<RecoverStartRequest>) -> CloudResult<Response> {
    // Every outcome answers 202 so the response never says whether an
    // account exists; the log does, because a request that sends nothing is
    // otherwise invisible (production, 2026-09-16: "the recovery mail never
    // reached Resend").
    let Some(user) = state.db.find_user_by_email(&req.email).await? else {
        tracing::info!(email = %req.email.trim(), "recovery requested for an unknown address; nothing sent");
        return Ok((StatusCode::ACCEPTED, Json(json!({ "status": "recovery_sent" }))).into_response());
    };
    if !user.verified {
        tracing::info!(email = %user.email, "recovery requested for an unverified account; nothing sent");
    } else if !user.has_key_material() {
        tracing::info!(email = %user.email, "recovery requested for a legacy account with no key material; nothing sent");
    } else if !state.recovery_rate_limit.try_acquire(&user.email.to_lowercase()) {
        tracing::info!(email = %user.email, "recovery requested again within a minute; nothing sent");
    } else {
        let (token, token_hash) = new_recovery_token();
        let expires_at_ms = chrono::Utc::now().timestamp_millis() + RECOVERY_TTL_MS;
        state.db.set_recovery_token(user.rid, &token_hash, expires_at_ms).await?;

        let link = format!("{}/app/recover?token={token}", state.config.public_url.trim_end_matches('/'));
        let (subject, text, html) = recovery_email(&link);
        state.mailer.send(&user.email, subject, &text, &html).await.map_err(|e| {
            tracing::warn!(email = %user.email, error = %e, "sending the recovery email failed");
            CloudError::MailFailed
        })?;
        tracing::info!(email = %user.email, "recovery email sent");
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "status": "recovery_sent" }))).into_response())
}

#[derive(Serialize)]
pub struct RecoveryKdfCosts {
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}

#[derive(Serialize)]
pub struct RecoverTokenResponse {
    email: String,
    recovery_salt: String,
    recovery_kdf: RecoveryKdfCosts,
    recovery_key_blob: AccountKeyBlob,
    public_keys: AccountPublicKeys,
}

/// Looks up the (unexpired) user a recovery token names, or `Err(RecoveryInvalid)`
/// — shared by every `/recover/{token}...` endpoint below. An empty path
/// segment is never valid (axum still routes `/recover/` to this handler
/// with `token == ""`, and an empty token would otherwise match the
/// `recovery_token_hash` sentinel every consumed/legacy row carries).
async fn user_for_recovery_token(state: &AppState, token: &str) -> CloudResult<UserRow> {
    if token.is_empty() {
        return Err(CloudError::RecoveryInvalid);
    }
    let token_hash = hash_recovery_token(token);
    let user = state.db.find_user_by_recovery_token_hash(&token_hash).await?.ok_or(CloudError::RecoveryInvalid)?;
    if user.recovery_token_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
        return Err(CloudError::RecoveryInvalid);
    }
    Ok(user)
}

/// `GET /api/v1/recover/{token}` — the only place the recovery blob is ever
/// served, and only to the holder of the emailed token.
pub async fn recover_get(State(state): State<AppState>, Path(token): Path<String>) -> CloudResult<Json<RecoverTokenResponse>> {
    let user = user_for_recovery_token(&state, &token).await?;
    let recovery_salt = user.recovery_salt.clone().ok_or(CloudError::RecoveryInvalid)?;
    let recovery_key_blob_json = user.recovery_key_blob.as_deref().ok_or(CloudError::RecoveryInvalid)?;
    let recovery_key_blob: AccountKeyBlob =
        serde_json::from_str(recovery_key_blob_json).map_err(|e| CloudError::Internal(format!("stored recovery_key_blob is not valid JSON: {e}")))?;
    let public_keys = user.public_keys().ok_or(CloudError::RecoveryInvalid)?;
    Ok(Json(RecoverTokenResponse {
        email: user.email,
        recovery_salt,
        // The recovery KEK always uses the same fixed costs the password
        // KDF does (there are no separate stored recovery costs — only a
        // salt); see `tests/integration.rs`'s `build_signup_body`.
        recovery_kdf: RecoveryKdfCosts { m_cost: pimble_crypto::KDF_M_COST_KIB, t_cost: pimble_crypto::KDF_T_COST, p_cost: pimble_crypto::KDF_P_COST },
        recovery_key_blob,
        public_keys,
    }))
}

#[derive(Deserialize)]
pub struct RecoverCompleteRequest {
    pub auth_key: String,
    pub kdf: KdfParams,
    pub account_key_blob: AccountKeyBlob,
    pub recovery_salt: String,
    pub recovery_key_blob: AccountKeyBlob,
}

#[derive(Serialize)]
pub struct RecoverCompleteResponse {
    user: UserView,
}

/// `POST /api/v1/recover/{token}/complete` — replaces password material and
/// recovery material (the client rotates the code), consumes the token, and
/// ends every session of the account in the same update (by raising its
/// `session_epoch`; the rows are deleted after), and starts no new one. Public keys are unchanged,
/// so every store envelope already wrapped to this account stays valid.
pub async fn recover_complete(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<RecoverCompleteRequest>,
) -> CloudResult<Json<RecoverCompleteResponse>> {
    if req.auth_key.trim().is_empty() {
        return Err(CloudError::BadRequest("auth_key must not be empty".to_string()));
    }
    validate_kdf(&req.kdf)?;
    validate_blob("account_key_blob", &req.account_key_blob)?;
    if req.recovery_salt.trim().is_empty() {
        return Err(CloudError::BadRequest("recovery_salt must not be empty".to_string()));
    }
    validate_blob("recovery_key_blob", &req.recovery_key_blob)?;

    let user = user_for_recovery_token(&state, &token).await?;
    let password_hash = hash_password(&req.auth_key)?;
    let epoch = next_session_epoch(&user);
    state
        .db
        .complete_recovery(
            user.rid,
            &password_hash,
            &req.kdf.salt,
            req.kdf.m_cost,
            req.kdf.t_cost,
            req.kdf.p_cost,
            &to_json("account_key_blob", &req.account_key_blob)?,
            &req.recovery_salt,
            &to_json("recovery_key_blob", &req.recovery_key_blob)?,
            epoch,
        )
        .await?;
    delete_ended_sessions(&state, user.rid, epoch, None).await;

    Ok(Json(RecoverCompleteResponse { user: UserView::from(&user) }))
}

/// `POST /api/v1/recover/{token}/delete-account` — for people without the
/// recovery code: deletes the user, its sessions, grants and key grants;
/// any hosted store it solely owned is marked `deleted` (the same
/// soft-delete `DELETE /stores/{id}` uses — nobody else could ever manage
/// it once this account is gone; wiping the underlying vault data is a
/// later phase).
pub async fn recover_delete_account(State(state): State<AppState>, Path(token): Path<String>) -> CloudResult<Json<Value>> {
    let user = user_for_recovery_token(&state, &token).await?;

    for grant in state.db.grants_for_user(user.rid).await? {
        // Only a whole-store owner is an owner: a scoped grant is a share of
        // somebody else's store, and losing it leaves that store alone.
        if grant.role == "owner" && grant.is_whole_store() {
            let owner_count =
                state.db.grants_for_store(grant.store_rid).await?.into_iter().filter(|g| g.role == "owner" && g.is_whole_store()).count();
            if owner_count <= 1 {
                state.db.mark_store_deleted(grant.store_rid).await?;
                // If it was a relayed store with a tunnel up, nobody is piped
                // to it from here on (a no-op for any other store).
                state.relay.withdraw_store(&grant.store_uuid);
            }
        }
        state.db.delete_grant(grant.rid).await?;
        // The account held the store through this grant: what its watchers
        // hold changed.
        state.watch_interest.changed([grant.store_uuid.clone()]);
    }
    for key_grant in state.db.key_grants_for_user(user.rid).await? {
        state.db.delete_key_grant(key_grant.rid).await?;
    }
    for session in state.db.sessions_for_user(user.rid).await? {
        state.db.delete_session(session.rid).await?;
    }
    state.db.delete_share_names_for_user(user.rid).await?;
    for item in state.db.inbox_of(&user.user_uuid).await? {
        state.db.delete_inbox_item(item.rid).await?;
    }
    for sub in state.db.push_subscriptions_of(&user.user_uuid).await? {
        state.db.delete_push_subscription(sub.rid).await?;
    }
    state.db.delete_user(user.rid).await?;

    Ok(Json(json!({})))
}

// ── Password / recovery-code changes (session) ──────────────────────────

#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub current_auth_key: String,
    pub auth_key: String,
    pub kdf: KdfParams,
    pub account_key_blob: AccountKeyBlob,
}

/// `POST /api/v1/me/password` — verifies `current_auth_key` like login,
/// then replaces the password material and signs out every other session of
/// the account; the session that asked stays signed in (Joe, 2026-11-25:
/// "Sign out the others"; docs/ONE_CODEBASE_CONTRACT.md "Built: wave 5,
/// part 4").
///
/// Atomic: this session's epoch is raised first, which ends nothing (a
/// session is valid while its epoch is not below the account's), and then
/// the one `.update()` that writes the new password raises the account's
/// epoch to the same value, which ends every other session at that moment.
/// A change that fails before that update changes neither; a session made
/// by a login that checked the old password carries the old epoch and is
/// ended too. JWTs minted before the change are not revoked: a Pimble server
/// keeps no session state and takes one until it expires, within the hour.
pub async fn change_password(State(state): State<AppState>, authed: AuthedUser, Json(req): Json<ChangePasswordRequest>) -> CloudResult<Json<Value>> {
    if !verify_password_constant_time(&req.current_auth_key, Some(authed.user.password_hash.as_str())) {
        return Err(CloudError::WrongCurrentPassword);
    }
    if req.auth_key.trim().is_empty() {
        return Err(CloudError::BadRequest("auth_key must not be empty".to_string()));
    }
    validate_kdf(&req.kdf)?;
    validate_blob("account_key_blob", &req.account_key_blob)?;

    let password_hash = hash_password(&req.auth_key)?;
    let account_key_blob = to_json("account_key_blob", &req.account_key_blob)?;
    let epoch = next_session_epoch(&authed.user);
    state.db.set_session_epoch(authed.session_rid, epoch).await?;
    state
        .db
        .update_password_material(authed.user.rid, &password_hash, &req.kdf.salt, req.kdf.m_cost, req.kdf.t_cost, req.kdf.p_cost, &account_key_blob, epoch)
        .await?;
    delete_ended_sessions(&state, authed.user.rid, epoch, Some(authed.session_rid)).await;
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
pub struct ChangeRecoveryCodeRequest {
    pub recovery_salt: String,
    pub recovery_key_blob: AccountKeyBlob,
}

/// `POST /api/v1/me/recovery-code` — replaces the recovery material with a
/// new code the client generated while already holding the account keys.
pub async fn change_recovery_code(State(state): State<AppState>, authed: AuthedUser, Json(req): Json<ChangeRecoveryCodeRequest>) -> CloudResult<Json<Value>> {
    if req.recovery_salt.trim().is_empty() {
        return Err(CloudError::BadRequest("recovery_salt must not be empty".to_string()));
    }
    validate_blob("recovery_key_blob", &req.recovery_key_blob)?;
    state
        .db
        .update_recovery_material(authed.user.rid, &req.recovery_salt, &to_json("recovery_key_blob", &req.recovery_key_blob)?)
        .await?;
    Ok(Json(json!({})))
}

