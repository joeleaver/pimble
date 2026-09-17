use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::db::RhypeDb;
use crate::jwt::JwtSigner;
use crate::mail::Mailer;
use crate::pimble::PimbleService;
use crate::ratelimit::{QuotaLimiter, RateLimiter};
use crate::releases::ReleasesCache;

/// One send per address per minute (docs/CLOUD_CONTRACT.md, "Phase 1b":
/// `POST /resend-verification`'s rate limit).
const RESEND_VERIFICATION_INTERVAL: Duration = Duration::from_secs(60);
/// `POST /recover/start`'s rate limit (docs/CRYPTO_CONTRACT.md "Phase
/// 2a-2": "the same one-per-minute limit as verification") — read as "the
/// same shape of limit" (a separate limiter with the same interval), not
/// literally `resend_rate_limit`: a fresh signup's own verification send
/// already records against that bucket, which would otherwise leave
/// `/recover/start` for the same brand-new address rate-limited for the
/// next minute before anyone ever asked it to send anything.
const RECOVERY_START_INTERVAL: Duration = Duration::from_secs(60);
/// `GET /api/v1/users/lookup` isn't given a specific number by
/// docs/CRYPTO_CONTRACT.md ("rate limited"), only that it must be — chosen
/// to allow ordinary UI use (typing an email into a share dialog) while
/// still blocking a scripted enumeration loop: at most once every 200ms per
/// caller (5/s).
const USERS_LOOKUP_INTERVAL: Duration = Duration::from_millis(200);
/// "one mail per (store, address) per minute" (docs/SHARING_CONTRACT.md,
/// "Accounts service"). Its own instance again, for the same reason
/// `recovery_rate_limit` is: sharing a bucket with verification would make an
/// invitation's mail depend on whether that address happened to sign up a
/// moment ago. Keyed by `<store id>:<lowercased address>`, so inviting the
/// same person to a second store is never silenced by the first.
const SHARE_MAIL_INTERVAL: Duration = Duration::from_secs(60);
/// "thirty invitations per inviter per hour" (docs/SHARING_CONTRACT.md) —
/// what stops an account using `PUT members` as a mail cannon at addresses
/// that never asked for anything. Only the invitation path counts: granting
/// to an address that already has an account mails somebody who is already a
/// Pimble user.
const INVITES_PER_INVITER_WINDOW: Duration = Duration::from_secs(60 * 60);
const INVITES_PER_INVITER_LIMIT: usize = 30;

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
    pub recovery_rate_limit: RateLimiter,
    pub users_lookup_rate_limit: RateLimiter,
    /// One sharing mail per `<store id>:<address>` per minute
    /// (docs/SHARING_CONTRACT.md). A repeat inside the minute still grants or
    /// upserts the invitation — only the mail is skipped.
    pub share_mail_rate_limit: RateLimiter,
    /// Thirty new invitations per inviter per hour (docs/SHARING_CONTRACT.md);
    /// beyond it `PUT members` answers 429.
    pub invite_quota: QuotaLimiter,
    /// `GET /kdf`'s decoy-salt HMAC key (docs/CRYPTO_CONTRACT.md), resolved
    /// once at startup from `config.kdf_decoy_secret` — see
    /// [`resolve_kdf_decoy_secret`].
    pub kdf_decoy_secret: Vec<u8>,
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// `config.kdf_decoy_secret` as bytes (any hex or plain string works — it's
/// only ever used as an HMAC key, never decoded back), or a fresh random
/// secret logged at `warn` when unset (same pattern as `JwtSigner`'s local
/// dev-signing seed): the process still starts and `/kdf` still answers
/// deterministically within its lifetime, but a restart changes every
/// unknown email's decoy salt.
fn resolve_kdf_decoy_secret(config: &Config) -> Vec<u8> {
    match &config.kdf_decoy_secret {
        Some(secret) => secret.as_bytes().to_vec(),
        None => {
            tracing::warn!(
                "PIMBLE_CLOUD_KDF_DECOY_SECRET is not set; generating a random secret for this \
                 process only. GET /kdf's decoy salt for an unknown email will not be stable \
                 across a restart or agree between replicas. Set the env var before running \
                 more than one instance."
            );
            let mut secret = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rng(), &mut secret);
            secret.to_vec()
        }
    }
}

impl AppState {
    pub fn new(config: Config, db: RhypeDb, pimble: PimbleService, signer: JwtSigner, releases: ReleasesCache, mailer: Arc<dyn Mailer>) -> Self {
        let kdf_decoy_secret = resolve_kdf_decoy_secret(&config);
        AppState(Arc::new(Inner {
            config,
            db,
            pimble,
            signer,
            releases,
            mailer,
            resend_rate_limit: RateLimiter::new(RESEND_VERIFICATION_INTERVAL),
            recovery_rate_limit: RateLimiter::new(RECOVERY_START_INTERVAL),
            users_lookup_rate_limit: RateLimiter::new(USERS_LOOKUP_INTERVAL),
            share_mail_rate_limit: RateLimiter::new(SHARE_MAIL_INTERVAL),
            invite_quota: QuotaLimiter::new(INVITES_PER_INVITER_WINDOW, INVITES_PER_INVITER_LIMIT),
            kdf_decoy_secret,
        }))
    }
}
