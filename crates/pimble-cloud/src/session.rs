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

fn extract_session_token(parts: &Parts) -> CloudResult<String> {
    if let Some(v) = parts.headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(token) = v.strip_prefix("Bearer ") {
            return Ok(token.to_string());
        }
    }
    if let Some(v) = parts.headers.get(COOKIE).and_then(|v| v.to_str().ok()) {
        for part in v.split(';') {
            let part = part.trim();
            if let Some(token) = part.strip_prefix(&format!("{COOKIE_NAME}=")) {
                return Ok(token.to_string());
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
}

#[async_trait::async_trait]
impl FromRequestParts<AppState> for AuthedUser {
    type Rejection = CloudError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = extract_session_token(parts)?;
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

        Ok(AuthedUser { user, session_rid: session.rid })
    }
}
