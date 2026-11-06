//! `GET /health`, `GET /.well-known/jwks.json`, `GET /releases`, `GET /plugins`.

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::IntoResponse;
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

/// The plugin library's index (docs/SETTINGS_CONTRACT.md section 4). Public:
/// plugins are code, not anyone's data.
pub async fn plugins(State(state): State<AppState>) -> CloudResult<impl IntoResponse> {
    let index = state.releases.plugin_index().await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], index))
}

/// One file the library's index names: a plugin, its signature, a screenshot.
pub async fn plugin_file(State(state): State<AppState>, Path(file): Path<String>) -> CloudResult<impl IntoResponse> {
    let bytes = state.releases.plugin_file(&file).await?;
    let kind = if file.ends_with(".wasm") {
        "application/wasm"
    } else if file.ends_with(".png") {
        "image/png"
    } else {
        "application/octet-stream"
    };
    Ok(([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "public, max-age=86400")], bytes))
}
