//! The one error type every handler and repository function returns.
//!
//! Maps to `{ "error": "<code>", "message": "..." }` (docs/CLOUD_CONTRACT.md's
//! "Endpoints" table: "errors as `{ "error": "<code>", "message": "..." }`
//! with sensible statuses").

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub enum CloudError {
    /// 400 — malformed input (e.g. a password under 8 characters).
    BadRequest(String),
    /// 401 — missing, invalid, or expired credentials (a session, or a
    /// login attempt). Always a generic message on login so a wrong
    /// password and an unknown email are indistinguishable to the caller.
    Unauthorized(String),
    /// 403 — a real session that isn't allowed to do this (e.g. a non-owner
    /// touching a store's members).
    Forbidden(String),
    /// 404 — no such store, member, or (for the invitation-free phase 1
    /// `PUT` member endpoint) no such email.
    NotFound(String),
    /// 409 — a `@unique` violation the caller could plausibly hit (duplicate
    /// email at signup).
    Conflict(String),
    /// 500 — RhypeDB, the Pimble server, or an outbound HTTP call misbehaved
    /// in a way the caller didn't cause and can't fix by retrying with
    /// different input.
    Internal(String),
}

impl CloudError {
    fn parts(&self) -> (StatusCode, &'static str, &str) {
        match self {
            CloudError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.as_str()),
            CloudError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, "unauthorized", m.as_str()),
            CloudError::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m.as_str()),
            CloudError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m.as_str()),
            CloudError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m.as_str()),
            CloudError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", m.as_str()),
        }
    }
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (status, code, message) = self.parts();
        write!(f, "{status} {code}: {message}")
    }
}

impl std::error::Error for CloudError {}

impl IntoResponse for CloudError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        // Sessions and tokens are never logged (docs/CLOUD_CONTRACT.md /
        // HARDENING_CONTRACT.md precedent); nothing here ever carries one —
        // every internal error message is a fixed string or names a
        // RhypeDB/Pimble-server failure, never a credential.
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(%message, "internal error");
        }
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}

pub type CloudResult<T> = std::result::Result<T, CloudError>;

/// A RhypeDB client error, translated. A `@unique` violation on a field this
/// caller could plausibly have triggered is the only one turned into a typed
/// variant (`Conflict`) instead of `Internal` — see
/// [`crate::db::unique_violation_on`].
impl From<rhypedb_client::Error> for CloudError {
    fn from(e: rhypedb_client::Error) -> Self {
        CloudError::Internal(format!("rhypedb: {e}"))
    }
}

impl From<pimble_client::ClientError> for CloudError {
    fn from(e: pimble_client::ClientError) -> Self {
        CloudError::Internal(format!("pimble server: {e}"))
    }
}
