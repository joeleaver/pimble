mod accounts;
mod misc;
mod stores;

use axum::routing::{delete, get, post};
use axum::Router;

use crate::state::AppState;

/// Everything under `/api/v1` (docs/CLOUD_CONTRACT.md: "serving under
/// `/api/v1`", including `/health` and `/.well-known/jwks.json` — see
/// `src/jwt.rs`'s module doc for why those two are mounted here rather than
/// at the site root).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/signup", post(accounts::signup))
        .route("/api/v1/verify", get(accounts::verify))
        .route("/api/v1/resend-verification", post(accounts::resend_verification))
        .route("/api/v1/login", post(accounts::login))
        .route("/api/v1/logout", post(accounts::logout))
        .route("/api/v1/me", get(accounts::me))
        .route("/api/v1/token", post(accounts::mint_token))
        .route("/api/v1/stores", get(stores::list_stores).post(stores::create_store))
        .route("/api/v1/stores/:id", delete(stores::delete_store))
        .route("/api/v1/stores/:id/members", get(stores::list_members).put(stores::put_member))
        .route("/api/v1/stores/:id/members/:user_id", delete(stores::delete_member))
        .route("/api/v1/releases", get(misc::releases))
        .route("/api/v1/.well-known/jwks.json", get(misc::jwks))
        .route("/api/v1/health", get(misc::health))
        .with_state(state)
}
