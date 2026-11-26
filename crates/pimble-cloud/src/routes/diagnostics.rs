//! The web app's alpha diagnostics (docs/DIAGNOSTICS_CONTRACT.md section 5):
//! records and reports from the browser, passed on to `pimble-diagnostics`
//! with the send key this service holds. A page never carries the key, and
//! what it sends is tied to the signed-in account here, whatever the page
//! says. A page names the account it means in `X-Pimble-Account`; one that
//! is not the session's is refused (`account_changed`), so records queued as
//! one account never go under another's cookie. A sender with no such header
//! (the browser's worker, whose bearer is the session it chose) is not asked.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::error::CloudError;
use crate::session::AuthedUser;
use crate::state::AppState;

/// `POST /api/v1/diagnostics/records`: NDJSON, gzip or not, as the
/// diagnostics server takes it.
pub async fn records(State(state): State<AppState>, authed: AuthedUser, headers: HeaderMap, body: Bytes) -> Response {
    forward(&state, &authed, &headers, body, "/v1/records").await
}

/// `POST /api/v1/diagnostics/reports`: one report (Help > Send Diagnostics...).
pub async fn reports(State(state): State<AppState>, authed: AuthedUser, headers: HeaderMap, body: Bytes) -> Response {
    forward(&state, &authed, &headers, body, "/v1/reports").await
}

/// The account a request names (`X-Pimble-Account`), if it names one.
const ACCOUNT_HEADER: &str = "x-pimble-account";

async fn forward(state: &AppState, authed: &AuthedUser, headers: &HeaderMap, body: Bytes, path: &str) -> Response {
    if let Some(named) = headers.get(ACCOUNT_HEADER) {
        if named.to_str().ok() != Some(authed.user.user_uuid.as_str()) {
            return CloudError::AccountChanged.into_response();
        }
    }
    let Some(key) = state.config.diagnostics_key.as_deref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "diagnostics are not set up on this service").into_response();
    };
    let url = format!("{}{}", state.config.diagnostics_url.trim_end_matches('/'), path);
    let mut request = reqwest::Client::new()
        .post(url)
        .bearer_auth(key)
        .header("Content-Type", headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/x-ndjson"))
        // Who sent it, for the record (the page's own `account` is not
        // trusted; the diagnostics server records what lines carry).
        .header("X-Pimble-Account", authed.user.user_uuid.as_str())
        .timeout(std::time::Duration::from_secs(30))
        .body(body);
    if let Some(encoding) = headers.get("content-encoding").and_then(|v| v.to_str().ok()) {
        request = request.header("Content-Encoding", encoding);
    }
    match request.send().await {
        Ok(answer) => {
            let status = StatusCode::from_u16(answer.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let text = answer.text().await.unwrap_or_default();
            (status, [("content-type", "application/json")], text).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "passing diagnostics on failed");
            (StatusCode::BAD_GATEWAY, "the diagnostics server could not be reached").into_response()
        }
    }
}
