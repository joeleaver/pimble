//! `GET /kdf`, `POST /signup`, `GET /verify`, `POST /resend-verification`,
//! `POST /login`, `POST /logout`, `GET /me`, `GET /me/keys`,
//! `GET /users/lookup`, `POST /recover/start`, `GET /recover/{token}`,
//! `POST /recover/{token}/complete`, `POST /recover/{token}/delete-account`,
//! `POST /me/password`, `POST /me/recovery-code`, `POST /token`.
//!
//! Phase 1b (docs/CLOUD_CONTRACT.md, "Phase 1b: email verification"): an
//! account is unusable until its email is verified by clicking a link.
//! `signup` no longer starts a session; `login` refuses an unverified
//! account with `email_unverified`.
//!
//! Phase 2a (docs/CRYPTO_CONTRACT.md): the server never sees a password —
//! `signup`/`login` carry `auth_key` (what the client's KDF derived), plus
//! the account's public keys and wrapped private keys. `password_hash` is
//! now `Argon2id(auth_key)`; the hashing itself (`crate::auth`) is
//! unchanged. The web app now owns the account pages, so verification
//! redirects target `/app/login`, not the static site's `/login.html`.
//!
//! Phase 2a-2 (docs/CRYPTO_CONTRACT.md "account recovery, password change,
//! new recovery code"): the server holds neither password nor recovery
//! KEK, so recovery can only replace what the recovery code unwrapped
//! client-side; a request naming an unfamiliar account is never
//! distinguishable from one naming a real, unverified, or keyless account
//! (all "202 always" or "404 recovery_invalid", never a different code).

use axum::extract::{Path, Query, State};
use axum::http::header::{LOCATION, SET_COOKIE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use pimble_crypto::{AccountKeyBlob, AccountPublicKeys, KdfParams};

use crate::auth::{hash_password, hash_recovery_token, hash_verify_token, new_recovery_token, new_session_token, new_verify_token, verify_password_constant_time};
use crate::claims::claims_for_user;
use crate::db::{NewUserKeyMaterial, UserRow};
use crate::error::{CloudError, CloudResult};
use crate::kdf_decoy::decoy_kdf_params;
use crate::mail::{recovery_email, verification_email};
use crate::session::{build_clear_cookie, build_set_cookie, AuthedUser};
use crate::state::AppState;

const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// "24 h expiry" (docs/CLOUD_CONTRACT.md, "Phase 1b").
const VERIFY_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// "1 hour" (docs/CRYPTO_CONTRACT.md, "Phase 2a-2").
const RECOVERY_TTL_MS: i64 = 60 * 60 * 1000;

/// Non-empty and, for a blob, a matching version and non-empty nonce and
/// ciphertext — a shallow sanity check on the client's own crypto, not a
/// cryptographic validation (the server never has the keys to check more).
/// Shared by every endpoint that accepts a `KdfParams`/`AccountKeyBlob`.
fn validate_kdf(kdf: &KdfParams) -> CloudResult<()> {
    if kdf.salt.trim().is_empty() || kdf.m_cost == 0 || kdf.t_cost == 0 || kdf.p_cost == 0 {
        return Err(CloudError::BadRequest("kdf is invalid".to_string()));
    }
    Ok(())
}

fn validate_blob(field: &'static str, blob: &AccountKeyBlob) -> CloudResult<()> {
    if blob.v != pimble_crypto::VERSION || blob.nonce.trim().is_empty() || blob.ciphertext.trim().is_empty() {
        return Err(CloudError::BadRequest(format!("{field} is invalid")));
    }
    Ok(())
}

// ── GET /kdf ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct KdfQuery {
    pub email: String,
}

/// `GET /api/v1/kdf?email=` — a real user's stored KDF parameters, or a
/// deterministic decoy for an unknown email (docs/CRYPTO_CONTRACT.md
/// "Client-derived login"). Same response shape either way, so a client
/// (and a passive observer) cannot tell which one it got.
pub async fn kdf(State(state): State<AppState>, Query(query): Query<KdfQuery>) -> CloudResult<Json<KdfParams>> {
    let email_lower = query.email.trim().to_lowercase();
    if let Some(params) = state.db.find_user_by_email(&email_lower).await?.and_then(|u| u.kdf_params()) {
        return Ok(Json(params));
    }
    // No user, or a legacy pre-Phase-2a account with no key material at
    // all: both get the same decoy (docs/CRYPTO_CONTRACT.md's migration
    // note — this only matters for the moment before a restart's startup
    // migration clears the row out; `kdf_params()` returning `None` is what
    // makes it fall through to here).
    Ok(Json(decoy_kdf_params(&state.kdf_decoy_secret, &email_lower)))
}

// ── Signup / login / logout / me ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct SignupRequest {
    pub email: String,
    /// Base64url; the server argon2id-hashes this at rest exactly as it
    /// hashed a raw password before Phase 2a — see schema.rhype's
    /// `User.password_hash` comment.
    pub auth_key: String,
    pub kdf: KdfParams,
    pub public_keys: AccountPublicKeys,
    pub account_key_blob: AccountKeyBlob,
    pub recovery_salt: String,
    pub recovery_key_blob: AccountKeyBlob,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub auth_key: String,
}

#[derive(Serialize)]
pub struct UserView {
    id: String,
    email: String,
}

#[derive(Serialize)]
struct AuthResponse {
    user: UserView,
    session: String,
    token: String,
    exp: i64,
}

/// Create a session for `user`, mint a token from their current grants, and
/// build the `Set-Cookie` + JSON body `login` returns. Only ever called for
/// a verified user.
async fn issue_session(state: &AppState, user: &UserRow) -> CloudResult<Response> {
    let (session_token, token_hash) = new_session_token();
    let expires_at_ms = chrono::Utc::now().timestamp_millis() + SESSION_TTL_MS;
    state.db.create_session(user.rid, &token_hash, expires_at_ms).await?;

    let claims = claims_for_user(state, user).await?;
    let minted = state.signer.mint(&user.user_uuid, claims).await?;

    let body = AuthResponse {
        user: UserView { id: user.user_uuid.clone(), email: user.email.clone() },
        session: session_token.clone(),
        token: minted.token,
        exp: minted.exp,
    };
    let cookie = build_set_cookie(&session_token, state.config.cookie_secure());
    Ok((StatusCode::OK, [(SET_COOKIE, cookie)], Json(body)).into_response())
}

/// (Re)issues a verify token for `user_rid`/`email` and sends the mail:
/// a fresh signup, a signup retry against an existing unverified address,
/// and `POST /resend-verification` all funnel through here. Every call
/// records the send against `resend_rate_limit`, keyed on the lowercased
/// address — signup and a signup retry aren't themselves rate-limited (they
/// always send), but they count as a send, so a `resend-verification`
/// moments later still sees one and stays silent instead of finding an
/// empty map.
async fn start_verification(state: &AppState, user_rid: u64, email: &str) -> CloudResult<()> {
    let (token, token_hash) = new_verify_token();
    let expires_at_ms = chrono::Utc::now().timestamp_millis() + VERIFY_TTL_MS;
    state.db.set_verify_token(user_rid, &token_hash, expires_at_ms).await?;
    state.resend_rate_limit.record(&email.to_lowercase());

    let link = format!("{}/api/v1/verify?token={token}", state.config.public_url.trim_end_matches('/'));
    let (subject, text, html) = verification_email(&link);
    // The user already exists at this point (created or found above); the
    // provider's raw response (which may echo back the rejected address or
    // other detail) never reaches the caller — only this log line sees it.
    state.mailer.send(email, subject, &text, &html).await.map_err(|e| {
        tracing::warn!(%email, error = %e, "sending the verification email failed");
        CloudError::MailFailed
    })?;
    Ok(())
}

#[derive(Serialize)]
struct VerificationSentResponse {
    status: &'static str,
    email: String,
}

/// Non-empty and, for a blob, a matching version and non-empty nonce and
/// ciphertext — a shallow sanity check on the client's own crypto, not a
/// cryptographic validation (the server never has the keys to check more).
fn validate_signup_request(req: &SignupRequest) -> CloudResult<()> {
    if req.auth_key.trim().is_empty() {
        return Err(CloudError::BadRequest("auth_key must not be empty".to_string()));
    }
    if req.kdf.salt.trim().is_empty() || req.kdf.m_cost == 0 || req.kdf.t_cost == 0 || req.kdf.p_cost == 0 {
        return Err(CloudError::BadRequest("kdf is invalid".to_string()));
    }
    if req.public_keys.encryption.trim().is_empty() || req.public_keys.signing.trim().is_empty() {
        return Err(CloudError::BadRequest("public_keys must not be empty".to_string()));
    }
    let valid_blob = |b: &AccountKeyBlob| b.v == pimble_crypto::VERSION && !b.nonce.trim().is_empty() && !b.ciphertext.trim().is_empty();
    if !valid_blob(&req.account_key_blob) {
        return Err(CloudError::BadRequest("account_key_blob is invalid".to_string()));
    }
    if req.recovery_salt.trim().is_empty() {
        return Err(CloudError::BadRequest("recovery_salt must not be empty".to_string()));
    }
    if !valid_blob(&req.recovery_key_blob) {
        return Err(CloudError::BadRequest("recovery_key_blob is invalid".to_string()));
    }
    Ok(())
}

fn to_json(field: &'static str, value: &impl Serialize) -> CloudResult<String> {
    serde_json::to_string(value).map_err(|e| CloudError::Internal(format!("serializing {field}: {e}")))
}

pub async fn signup(State(state): State<AppState>, Json(req): Json<SignupRequest>) -> CloudResult<Response> {
    validate_signup_request(&req)?;
    let email = req.email.trim();
    if email.is_empty() || !email.contains('@') {
        return Err(CloudError::BadRequest("email is not valid".to_string()));
    }

    match state.db.find_user_by_email(email).await? {
        Some(existing) if existing.verified => {
            return Err(CloudError::Conflict("an account with this email already exists".to_string()));
        }
        Some(existing) => {
            // An unverified duplicate just gets the link re-sent — no
            // enumeration signal beyond "check your inbox" either way, and
            // the key material already on file stands (the caller hasn't
            // proven mailbox control, let alone that they hold the original
            // keys, so a resend never overwrites it).
            start_verification(&state, existing.rid, &existing.email).await?;
        }
        None => {
            let password_hash = hash_password(&req.auth_key)?;
            let keys = NewUserKeyMaterial {
                kdf_salt: req.kdf.salt.clone(),
                kdf_m_cost: req.kdf.m_cost,
                kdf_t_cost: req.kdf.t_cost,
                kdf_p_cost: req.kdf.p_cost,
                public_encryption_key: req.public_keys.encryption.clone(),
                public_signing_key: req.public_keys.signing.clone(),
                account_key_blob: to_json("account_key_blob", &req.account_key_blob)?,
                recovery_salt: req.recovery_salt.clone(),
                recovery_key_blob: to_json("recovery_key_blob", &req.recovery_key_blob)?,
            };
            let user = state
                .db
                .create_user(email, &password_hash, &keys)
                .await?
                .ok_or_else(|| CloudError::Conflict("an account with this email already exists".to_string()))?;
            start_verification(&state, user.rid, &user.email).await?;
        }
    }

    Ok((StatusCode::ACCEPTED, Json(VerificationSentResponse { status: "verification_sent", email: email.to_string() })).into_response())
}

#[derive(Deserialize)]
pub struct VerifyQuery {
    pub token: Option<String>,
}

fn redirect_to(path: &str) -> Response {
    (StatusCode::SEE_OTHER, [(LOCATION, path.to_string())]).into_response()
}

/// `GET /api/v1/verify?token=...` — a browser follows the link from the
/// mail. Never returns a JSON error for an invalid/expired token (those are
/// redirects the web app's `/app/login` renders as a banner); a real
/// backend failure still surfaces as the usual JSON error. Redirects to the
/// web app, not the static site (docs/CRYPTO_CONTRACT.md: "the web app owns
/// the account pages now").
pub async fn verify(State(state): State<AppState>, Query(query): Query<VerifyQuery>) -> CloudResult<Response> {
    let Some(token) = query.token.as_deref().filter(|t| !t.is_empty()) else {
        return Ok(redirect_to("/app/login?verify_error=invalid"));
    };
    let token_hash = hash_verify_token(token);
    let Some(user) = state.db.find_user_by_verify_token_hash(&token_hash).await? else {
        return Ok(redirect_to("/app/login?verify_error=invalid"));
    };
    if user.verify_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
        return Ok(redirect_to("/app/login?verify_error=expired"));
    }
    state.db.mark_user_verified(user.rid).await?;
    // Claiming: this address may have
    // been invited to shares before it had an account. Best effort — a share
    // that cannot be handed over right now must not make verification itself
    // fail; the next login claims it.
    super::stores::claim_invitations_best_effort(&state, &user).await;
    Ok(redirect_to("/app/login?verified=1"))
}

#[derive(Deserialize)]
pub struct ResendVerificationRequest {
    pub email: String,
}

/// `POST /api/v1/resend-verification` — 202 unconditionally (no
/// enumeration); re-sends only for a real, still-unverified account that
/// hasn't been sent one in the last minute.
pub async fn resend_verification(State(state): State<AppState>, Json(req): Json<ResendVerificationRequest>) -> CloudResult<Response> {
    if let Some(user) = state.db.find_user_by_email(&req.email).await? {
        if !user.verified && state.resend_rate_limit.try_acquire(&user.email.to_lowercase()) {
            start_verification(&state, user.rid, &user.email).await?;
        }
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "status": "verification_sent" }))).into_response())
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> CloudResult<Response> {
    let user = state.db.find_user_by_email(&req.email).await?;
    let ok = verify_password_constant_time(&req.auth_key, user.as_ref().map(|u| u.password_hash.as_str()));
    let Some(user) = user.filter(|_| ok) else {
        return Err(CloudError::Unauthorized("invalid email or password".to_string()));
    };
    if !user.has_key_material() {
        // A legacy pre-Phase-2a account: no keys to unwrap, so no way in.
        // Same generic message as any other failed login — no hint that
        // the account exists (docs/CRYPTO_CONTRACT.md's migration note; the
        // startup migration deletes these, so this only matters for the
        // moment before the next restart clears the row out).
        return Err(CloudError::Unauthorized("invalid email or password".to_string()));
    }
    if !user.verified {
        return Err(CloudError::EmailUnverified);
    }
    // Claim any invitations for this address before the token is minted, so
    // the `stores` claim in it already carries a share invited moments ago
    // (claiming runs at every login). Verification
    // usually does this first; login covers an invitation sent afterwards,
    // and any claim that failed then.
    super::stores::claim_invitations_best_effort(&state, &user).await;
    issue_session(&state, &user).await
}

pub async fn logout(State(state): State<AppState>, authed: AuthedUser) -> CloudResult<Response> {
    state.db.delete_session(authed.session_rid).await?;
    let cookie = build_clear_cookie(state.config.cookie_secure());
    Ok((StatusCode::OK, [(SET_COOKIE, cookie)], Json(json!({}))).into_response())
}

pub async fn me(authed: AuthedUser) -> Json<UserView> {
    Json(UserView { id: authed.user.user_uuid, email: authed.user.email })
}

// ── Keys ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct MeKeysResponse {
    public_keys: AccountPublicKeys,
    kdf: KdfParams,
    account_key_blob: AccountKeyBlob,
}

/// `GET /api/v1/me/keys` — never the recovery blob (docs/CRYPTO_CONTRACT.md).
pub async fn me_keys(authed: AuthedUser) -> CloudResult<Json<MeKeysResponse>> {
    let user = &authed.user;
    // A session can only exist for a user `create_session` was called for,
    // which only ever happens after a real login, which now refuses a
    // legacy no-key-material account outright — so this is reachable only
    // for a real Phase 2a account. Treated as `Internal`, not a panic: a
    // stale session surviving the startup migration would be a genuine bug
    // worth a loud 500, not a silent wrong answer.
    let no_keys = || CloudError::Internal("session exists for a user with no key material".to_string());
    let public_keys = user.public_keys().ok_or_else(no_keys)?;
    let kdf = user.kdf_params().ok_or_else(no_keys)?;
    let account_key_blob_json = user.account_key_blob.as_deref().ok_or_else(no_keys)?;
    let account_key_blob: AccountKeyBlob =
        serde_json::from_str(account_key_blob_json).map_err(|e| CloudError::Internal(format!("stored account_key_blob is not valid JSON: {e}")))?;
    Ok(Json(MeKeysResponse { public_keys, kdf, account_key_blob }))
}

#[derive(Deserialize)]
pub struct UsersLookupQuery {
    pub email: String,
}

#[derive(Serialize)]
pub struct UserLookupResponse {
    id: String,
    public_keys: AccountPublicKeys,
}

/// `GET /api/v1/users/lookup?email=` — a verified user's id and public keys
/// (so a caller can wrap a store key to them), 404 for anyone else (unknown
/// email, or a real but unverified account: it has no usable keys to share
/// yet as far as a sharer is concerned). Rate limited per caller
/// (docs/CRYPTO_CONTRACT.md doesn't pin a number; see
/// `state::USERS_LOOKUP_INTERVAL`'s doc comment for the one chosen here).
pub async fn users_lookup(State(state): State<AppState>, authed: AuthedUser, Query(query): Query<UsersLookupQuery>) -> CloudResult<Json<UserLookupResponse>> {
    if !state.users_lookup_rate_limit.try_acquire(&authed.user.user_uuid) {
        return Err(CloudError::RateLimited("too many lookups; slow down".to_string()));
    }
    let user = state
        .db
        .find_user_by_email(query.email.trim())
        .await?
        // A verified legacy account (no key material) has nothing a sharer
        // could wrap a key to; treated the same as "no such user" (404),
        // not surfaced differently — no enumeration signal either way.
        .filter(|u| u.verified && u.has_key_material())
        .ok_or_else(|| CloudError::NotFound("no such user".to_string()))?;
    let public_keys = user.public_keys().ok_or_else(|| CloudError::Internal("user has key material but public_keys() failed".to_string()))?;
    Ok(Json(UserLookupResponse { id: user.user_uuid, public_keys }))
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
/// recovery material (the client rotates the code), consumes the token,
/// deletes every session, and starts no new one. Public keys are unchanged,
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
        )
        .await?;

    for session in state.db.sessions_for_user(user.rid).await? {
        state.db.delete_session(session.rid).await?;
    }

    Ok(Json(RecoverCompleteResponse { user: UserView { id: user.user_uuid, email: user.email } }))
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
            }
        }
        state.db.delete_grant(grant.rid).await?;
    }
    for key_grant in state.db.key_grants_for_user(user.rid).await? {
        state.db.delete_key_grant(key_grant.rid).await?;
    }
    for session in state.db.sessions_for_user(user.rid).await? {
        state.db.delete_session(session.rid).await?;
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
/// then replaces the password material. Other sessions are kept (Joe may
/// change that later — docs/CRYPTO_CONTRACT.md).
pub async fn change_password(State(state): State<AppState>, authed: AuthedUser, Json(req): Json<ChangePasswordRequest>) -> CloudResult<Json<Value>> {
    if !verify_password_constant_time(&req.current_auth_key, Some(authed.user.password_hash.as_str())) {
        return Err(CloudError::Unauthorized("current password is incorrect".to_string()));
    }
    if req.auth_key.trim().is_empty() {
        return Err(CloudError::BadRequest("auth_key must not be empty".to_string()));
    }
    validate_kdf(&req.kdf)?;
    validate_blob("account_key_blob", &req.account_key_blob)?;

    let password_hash = hash_password(&req.auth_key)?;
    state
        .db
        .update_password_material(
            authed.user.rid,
            &password_hash,
            &req.kdf.salt,
            req.kdf.m_cost,
            req.kdf.t_cost,
            req.kdf.p_cost,
            &to_json("account_key_blob", &req.account_key_blob)?,
        )
        .await?;
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

// ── Tokens ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct TokenResponse {
    token: String,
    exp: i64,
    rpc_url: String,
}

pub async fn mint_token(State(state): State<AppState>, authed: AuthedUser) -> CloudResult<Json<TokenResponse>> {
    let claims = claims_for_user(&state, &authed.user).await?;
    let minted = state.signer.mint(&authed.user.user_uuid, claims).await?;
    Ok(Json(TokenResponse { token: minted.token, exp: minted.exp, rpc_url: rpc_url(&state) }))
}

/// `wss://<host>/rpc` (or `ws://` for a plain-http `PIMBLE_CLOUD_PUBLIC_URL`,
/// e.g. local development) — one origin, no CORS (docs/CLOUD_CONTRACT.md
/// "Layout on jkbase"): the edge proxy at this same host routes `/rpc` to the
/// hosted Pimble server, so this is derived from this service's own public
/// URL, not `PIMBLE_SERVER_URL` (which names the internal, same-VM address
/// this service itself connects to).
fn rpc_url(state: &AppState) -> String {
    let base = state.config.public_url.trim_end_matches('/');
    if let Some(host) = base.strip_prefix("https://") {
        format!("wss://{host}/rpc")
    } else if let Some(host) = base.strip_prefix("http://") {
        format!("ws://{host}/rpc")
    } else {
        format!("{base}/rpc")
    }
}
