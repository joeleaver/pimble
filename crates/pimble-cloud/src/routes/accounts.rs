//! `POST /signup`, `GET /verify`, `POST /resend-verification`, `POST /login`,
//! `POST /logout`, `GET /me`, `POST /token`.
//!
//! Phase 1b (docs/CLOUD_CONTRACT.md, "Phase 1b: email verification"): an
//! account is unusable until its email is verified by clicking a link.
//! `signup` no longer starts a session; `login` refuses an unverified
//! account with `email_unverified`.

use axum::extract::{Query, State};
use axum::http::header::{LOCATION, SET_COOKIE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{hash_password, hash_verify_token, new_session_token, new_verify_token, verify_password_constant_time};
use crate::claims::claims_for_user;
use crate::db::UserRow;
use crate::error::{CloudError, CloudResult};
use crate::mail::verification_email;
use crate::session::{build_clear_cookie, build_set_cookie, AuthedUser};
use crate::state::AppState;

const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// "24 h expiry" (docs/CLOUD_CONTRACT.md, "Phase 1b").
const VERIFY_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Deserialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
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
/// and `POST /resend-verification` all funnel through here.
async fn start_verification(state: &AppState, user_rid: u64, email: &str) -> CloudResult<()> {
    let (token, token_hash) = new_verify_token();
    let expires_at_ms = chrono::Utc::now().timestamp_millis() + VERIFY_TTL_MS;
    state.db.set_verify_token(user_rid, &token_hash, expires_at_ms).await?;

    let link = format!("{}/api/v1/verify?token={token}", state.config.public_url.trim_end_matches('/'));
    let (subject, text, html) = verification_email(&link);
    state.mailer.send(email, subject, &text, &html).await?;
    Ok(())
}

#[derive(Serialize)]
struct VerificationSentResponse {
    status: &'static str,
    email: String,
}

pub async fn signup(State(state): State<AppState>, Json(req): Json<SignupRequest>) -> CloudResult<Response> {
    if req.password.len() < 8 {
        return Err(CloudError::BadRequest("password must be at least 8 characters".to_string()));
    }
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
            // no new password is recorded (the caller hasn't proven they
            // control the mailbox, let alone this is the same person).
            start_verification(&state, existing.rid, &existing.email).await?;
        }
        None => {
            let password_hash = hash_password(&req.password)?;
            let user = state
                .db
                .create_user(email, &password_hash)
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
/// redirects the login page renders as a banner); a real backend failure
/// still surfaces as the usual JSON error.
pub async fn verify(State(state): State<AppState>, Query(query): Query<VerifyQuery>) -> CloudResult<Response> {
    let Some(token) = query.token.as_deref().filter(|t| !t.is_empty()) else {
        return Ok(redirect_to("/login.html?verify_error=invalid"));
    };
    let token_hash = hash_verify_token(token);
    let Some(user) = state.db.find_user_by_verify_token_hash(&token_hash).await? else {
        return Ok(redirect_to("/login.html?verify_error=invalid"));
    };
    if user.verify_expires_at_ms <= chrono::Utc::now().timestamp_millis() {
        return Ok(redirect_to("/login.html?verify_error=expired"));
    }
    state.db.mark_user_verified(user.rid).await?;
    Ok(redirect_to("/login.html?verified=1"))
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
    let ok = verify_password_constant_time(&req.password, user.as_ref().map(|u| u.password_hash.as_str()));
    let Some(user) = user.filter(|_| ok) else {
        return Err(CloudError::Unauthorized("invalid email or password".to_string()));
    };
    if !user.verified {
        return Err(CloudError::EmailUnverified);
    }
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
