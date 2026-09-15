//! Pimble Cloud: the accounts service (docs/CLOUD_CONTRACT.md, section C;
//! docs/CRYPTO_CONTRACT.md's "Accounts service endpoints" and "Data model
//! additions" for Phase 2a's end-to-end encryption).
//!
//! A library crate as well as the `pimble-cloud` binary so integration
//! tests (`tests/`) can build the axum [`Router`] directly, bind it to an
//! ephemeral port themselves, and drive it with a real HTTP client alongside
//! an in-process [`pimble_server::PimbleServer`] and a spawned `rhypedb`
//! server — see `tests/common/mod.rs`.

pub mod auth;
pub mod claims;
pub mod config;
pub mod db;
pub mod error;
pub mod jwt;
pub mod kdf_decoy;
pub mod mail;
pub mod pimble;
pub mod ratelimit;
pub mod releases;
pub mod routes;
pub mod session;
pub mod state;

use axum::Router;

use config::Config;
use db::RhypeDb;
use jwt::JwtSigner;
use pimble::PimbleService;
use releases::ReleasesCache;
use state::AppState;

/// Build the whole service's state (RhypeDB connection, Pimble server
/// connection, JWT signer, releases cache, mailer) without binding a
/// listener or building the router. Split out from [`build_router`] so
/// tests can hold onto the [`AppState`] (e.g. to reach the [`mail::LogMailer`]
/// through `AppState::mailer` — see `tests/integration.rs`) alongside the
/// router built from it.
pub async fn build_state(config: Config) -> anyhow::Result<AppState> {
    let mailer = mail::build_mailer(&config);
    build_state_with_mailer(config, mailer).await
}

/// Like [`build_state`], but with the mailer supplied rather than chosen
/// from `config` — lets a test substitute one that always fails, to cover
/// how a mail-provider failure is handled (`tests/integration.rs`).
pub async fn build_state_with_mailer(config: Config, mailer: std::sync::Arc<dyn mail::Mailer>) -> anyhow::Result<AppState> {
    let db = RhypeDb::connect(&config.rhypedb_addr).await?;
    let pimble = PimbleService::connect(&config).await?;
    let signer = JwtSigner::from_config(&config);
    let releases = ReleasesCache::new(config.github_repo.clone(), config.releases_base_url.clone());
    Ok(AppState::new(config, db, pimble, signer, releases, mailer))
}

pub fn router_from_state(state: AppState) -> Router {
    routes::router(state)
}

/// Build the whole service and its router, without binding a listener.
/// `main` binds `config.port`; tests bind their own ephemeral port.
pub async fn build_router(config: Config) -> anyhow::Result<Router> {
    let state = build_state(config).await?;
    Ok(router_from_state(state))
}
