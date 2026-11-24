mod accounts;
mod diagnostics;
pub mod inbox;
mod misc;
mod people;
mod stores;
mod watches;

use axum::routing::{delete, get, post};
use axum::Router;

use crate::state::AppState;

pub use inbox::InboxWakers;

/// Everything under `/api/v1` (docs/CLOUD_CONTRACT.md: "serving under
/// `/api/v1`", including `/health` and `/.well-known/jwks.json` — see
/// `src/jwt.rs`'s module doc for why those two are mounted here rather than
/// at the site root).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/kdf", get(accounts::kdf))
        .route("/api/v1/signup", post(accounts::signup))
        .route("/api/v1/verify", get(accounts::verify))
        .route("/api/v1/resend-verification", post(accounts::resend_verification))
        .route("/api/v1/login", post(accounts::login))
        .route("/api/v1/logout", post(accounts::logout))
        .route("/api/v1/me", get(accounts::me).put(people::put_me))
        .route("/api/v1/me/shares", get(people::list_shares))
        .route("/api/v1/me/shares/:store_id", axum::routing::put(people::put_share_name))
        .route("/api/v1/me/keys", get(accounts::me_keys))
        .route("/api/v1/me/personal", get(accounts::get_personal).put(accounts::put_personal).delete(accounts::delete_personal))
        .route("/api/v1/me/notice-key", axum::routing::put(inbox::put_notice_key))
        .route("/api/v1/me/notifications", axum::routing::put(inbox::put_notifications))
        .route("/api/v1/me/watches", get(watches::get_watches))
        .route("/api/v1/me/watches/choices", axum::routing::put(watches::put_choices))
        .route("/api/v1/me/watches/stores/:store_id", axum::routing::put(watches::put_store_docs))
        .route("/api/v1/watches/writes", post(watches::post_relay_writes))
        .route("/api/v1/inbox", get(inbox::drain).delete(inbox::clear))
        .route("/api/v1/inbox/ack", post(inbox::ack))
        .route("/api/v1/inbox/seen/:item", post(inbox::seen))
        .route("/api/v1/me/push", post(inbox::post_push).delete(inbox::delete_push))
        .route("/api/v1/inbox/:recipient", post(inbox::post_notice))
        .route("/api/v1/me/password", post(accounts::change_password))
        .route("/api/v1/me/recovery-code", post(accounts::change_recovery_code))
        .route("/api/v1/users/lookup", get(accounts::users_lookup))
        .route("/api/v1/recover/start", post(accounts::recover_start))
        .route("/api/v1/recover/:token", get(accounts::recover_get))
        .route("/api/v1/recover/:token/complete", post(accounts::recover_complete))
        .route("/api/v1/recover/:token/delete-account", post(accounts::recover_delete_account))
        .route("/api/v1/token", post(accounts::mint_token))
        .route("/api/v1/stores", get(stores::list_stores).post(stores::create_store))
        .route("/api/v1/stores/:id", delete(stores::delete_store))
        .route("/api/v1/stores/:id/members", get(stores::list_members).put(stores::put_member))
        .route("/api/v1/stores/:id/members/:user_id", delete(stores::delete_member))
        .route("/api/v1/stores/:id/invitations/:email", delete(stores::delete_invitation))
        .route("/api/v1/stores/:id/keys", get(stores::get_store_keys).put(stores::put_store_keys))
        // The relay (docs/RELAY_CONTRACT.md), both WebSockets: the owner's
        // tunnel, and a member's connection to the store behind one.
        .route("/api/v1/relay", get(crate::relay::owner_tunnel))
        .route("/api/v1/relay/:store_id", get(crate::relay::member_connection))
        .route("/api/v1/releases", get(misc::releases))
        .route("/api/v1/plugins", get(misc::plugins))
        .route("/api/v1/plugins/:file", get(misc::plugin_file))
        .route("/api/v1/.well-known/jwks.json", get(misc::jwks))
        .route("/api/v1/health", get(misc::health))
        .route("/api/v1/diagnostics/records", post(diagnostics::records))
        .route("/api/v1/diagnostics/reports", post(diagnostics::reports))
        .with_state(state)
}
