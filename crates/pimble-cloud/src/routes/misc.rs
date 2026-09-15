//! `GET /health`, `GET /.well-known/jwks.json`, `GET /releases`.

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::error::CloudResult;
use crate::releases::ReleaseInfo;
use crate::state::AppState;

pub async fn health() -> &'static str {
    "ok"
}

pub async fn jwks(State(state): State<AppState>) -> CloudResult<Json<Value>> {
    Ok(Json(state.signer.jwks().await?))
}

pub async fn releases(State(state): State<AppState>) -> CloudResult<Json<ReleaseInfo>> {
    Ok(Json(state.releases.get().await?))
}
