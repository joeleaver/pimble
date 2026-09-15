//! Pimble Cloud: the accounts service (docs/CLOUD_CONTRACT.md, section C).
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
pub mod pimble;
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

/// Build the whole service (RhypeDB connection, Pimble server connection,
/// JWT signer, releases cache) and its router, without binding a listener.
/// `main` binds `config.port`; tests bind their own ephemeral port.
pub async fn build_router(config: Config) -> anyhow::Result<Router> {
    let db = RhypeDb::connect(&config.rhypedb_addr).await?;
    let pimble = PimbleService::connect(&config).await?;
    let signer = JwtSigner::from_config(&config);
    let releases = ReleasesCache::new(config.github_repo.clone(), config.releases_base_url.clone());
    let state = AppState::new(config, db, pimble, signer, releases);
    Ok(routes::router(state))
}
