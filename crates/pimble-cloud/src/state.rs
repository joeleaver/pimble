use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::db::RhypeDb;
use crate::jwt::JwtSigner;
use crate::mail::Mailer;
use crate::pimble::PimbleService;
use crate::ratelimit::RateLimiter;
use crate::releases::ReleasesCache;

/// One send per address per minute (docs/CLOUD_CONTRACT.md, "Phase 1b":
/// `POST /resend-verification`'s rate limit).
const RESEND_VERIFICATION_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

pub struct Inner {
    pub config: Config,
    pub db: RhypeDb,
    pub pimble: PimbleService,
    pub signer: JwtSigner,
    pub releases: ReleasesCache,
    pub mailer: Arc<dyn Mailer>,
    pub resend_rate_limit: RateLimiter,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(config: Config, db: RhypeDb, pimble: PimbleService, signer: JwtSigner, releases: ReleasesCache, mailer: Arc<dyn Mailer>) -> Self {
        AppState(Arc::new(Inner { config, db, pimble, signer, releases, mailer, resend_rate_limit: RateLimiter::new(RESEND_VERIFICATION_INTERVAL) }))
    }
}
