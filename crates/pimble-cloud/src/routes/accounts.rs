//! `POST /signup`, `POST /login`, `POST /logout`, `GET /me`, `POST /token`.

use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{hash_password, new_session_token, verify_password_constant_time};
use crate::claims::claims_for_user;
use crate::db::UserRow;
use crate::error::{CloudError, CloudResult};
use crate::session::{build_clear_cookie, build_set_cookie, AuthedUser};
use crate::state::AppState;

const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

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
/// build the `Set-Cookie` + JSON body both `signup` and `login` return.
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

pub async fn signup(State(state): State<AppState>, Json(req): Json<SignupRequest>) -> CloudResult<Response> {
    if req.password.len() < 8 {
        return Err(CloudError::BadRequest("password must be at least 8 characters".to_string()));
    }
    if req.email.trim().is_empty() || !req.email.contains('@') {
        return Err(CloudError::BadRequest("email is not valid".to_string()));
    }
    let password_hash = hash_password(&req.password)?;
    let user = state
        .db
        .create_user(req.email.trim(), &password_hash)
        .await?
        .ok_or_else(|| CloudError::Conflict("an account with this email already exists".to_string()))?;
    issue_session(&state, &user).await
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> CloudResult<Response> {
    let user = state.db.find_user_by_email(&req.email).await?;
    let ok = verify_password_constant_time(&req.password, user.as_ref().map(|u| u.password_hash.as_str()));
    let Some(user) = user.filter(|_| ok) else {
        return Err(CloudError::Unauthorized("invalid email or password".to_string()));
    };
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
