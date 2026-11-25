//! The session cookie/header (docs/CLOUD_CONTRACT.md: "Session auth accepts
//! the cookie or `Authorization: Bearer <session>`") and the
//! [`AuthedUser`] extractor built on it.

use axum::extract::FromRequestParts;
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::request::Parts;

use crate::auth::hash_session_token;
use crate::db::UserRow;
use crate::error::{CloudError, CloudResult};
use crate::state::AppState;

pub const COOKIE_NAME: &str = "pimble_session";
const THIRTY_DAYS_SECS: i64 = 30 * 24 * 60 * 60;
/// How long a session lasts from its last use: every use that finds less
/// than [`RENEW_WHEN_LEFT_MS`] left pushes it back to this. Before 2026-11-19
/// a session lasted 30 days from sign-in whatever happened, and a desktop
/// that ran every day lost its sign-in on day 30 without a word.
pub const SESSION_TTL_MS: i64 = THIRTY_DAYS_SECS * 1000;
/// Renewed at most about once a day per session, so a busy session is not
/// written on every request.
const RENEW_WHEN_LEFT_MS: i64 = SESSION_TTL_MS - 24 * 60 * 60 * 1000;

/// The `Set-Cookie` value that sets the session cookie to `token`.
pub fn build_set_cookie(token: &str, secure: bool) -> String {
    let mut v = format!("{COOKIE_NAME}={token}; HttpOnly; Path=/; SameSite=Lax; Max-Age={THIRTY_DAYS_SECS}");
    if secure {
        v.push_str("; Secure");
    }
    v
}

/// The `Set-Cookie` value that clears the session cookie (`POST /logout`).
pub fn build_clear_cookie(secure: bool) -> String {
    let mut v = format!("{COOKIE_NAME}=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0");
    if secure {
        v.push_str("; Secure");
    }
    v
}

/// The session token, and whether it came in the cookie.
fn extract_session_token(parts: &Parts) -> CloudResult<(String, bool)> {
    if let Some(v) = parts.headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = v.strip_prefix("Bearer ") {
            return Ok((token.to_string(), false));
        }
    }
    if let Some(v) = parts.headers.get(COOKIE).and_then(|v| v.to_str().ok()) {
        for part in v.split(';') {
            let part = part.trim();
            if let Some(token) = part.strip_prefix(&format!("{COOKIE_NAME}=")) {
                return Ok((token.to_string(), true));
            }
        }
    }
    Err(CloudError::Unauthorized("no session cookie or Authorization: Bearer <session> header".to_string()))
}

/// An authenticated caller, resolved from the session cookie/header. Every
/// endpoint that requires "session" auth in docs/CLOUD_CONTRACT.md's
/// endpoint table takes this as an axum extractor argument.
pub struct AuthedUser {
    pub user: UserRow,
    pub session_rid: u64,
    /// The session was renewed by this request and came in the cookie: the
    /// answer sets the cookie again (with this token), so the browser keeps
    /// it as long as the server does.
    pub renewed_cookie: Option<String>,
}

#[async_trait::async_trait]
impl FromRequestParts<AppState> for AuthedUser {
    type Rejection = CloudError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let (token, via_cookie) = extract_session_token(parts)?;
        let token_hash = hash_session_token(&token);
        let session = state
            .db
            .find_session_by_token_hash(&token_hash)
            .await?
            .ok_or_else(|| CloudError::Unauthorized("invalid session".to_string()))?;

        let now_ms = chrono::Utc::now().timestamp_millis();
        if session.expires_at_ms <= now_ms {
            // An expired session behaves exactly like one that never
            // existed: nothing distinguishes "wrong token" from "old token"
            // to the caller.
            return Err(CloudError::Unauthorized("invalid session".to_string()));
        }

        let user = state
            .db
            .get_user(session.user_rid)
            .await?
            .ok_or_else(|| CloudError::Internal("session references a user that no longer exists".to_string()))?;
        if session.ended_for(&user) {
            // Ended by a password change on another session, or a recovery:
            // refused like one that never existed.
            return Err(CloudError::Unauthorized("invalid session".to_string()));
        }

        // In use: pushed back to 30 days from now (at most once a day). A
        // failed write leaves the session as it was; the request goes on.
        let mut renewed_cookie = None;
        if session.expires_at_ms - now_ms < RENEW_WHEN_LEFT_MS {
            match state.db.renew_session(session.rid, now_ms + SESSION_TTL_MS).await {
                Ok(()) if via_cookie => renewed_cookie = Some(token.clone()),
                Ok(()) => {}
                Err(e) => tracing::warn!(error = %e, "renewing a session failed; it keeps its expiry"),
            }
        }

        Ok(AuthedUser { user, session_rid: session.rid, renewed_cookie })
    }
}
