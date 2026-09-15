use std::sync::Arc;

use crate::config::Config;
use crate::db::RhypeDb;
use crate::jwt::JwtSigner;
use crate::pimble::PimbleService;
use crate::releases::ReleasesCache;

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

pub struct Inner {
    pub config: Config,
    pub db: RhypeDb,
    pub pimble: PimbleService,
    pub signer: JwtSigner,
    pub releases: ReleasesCache,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(config: Config, db: RhypeDb, pimble: PimbleService, signer: JwtSigner, releases: ReleasesCache) -> Self {
        AppState(Arc::new(Inner { config, db, pimble, signer, releases }))
    }
}
