use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::db::RhypeDb;
use crate::jwt::JwtSigner;
use crate::mail::Mailer;
use crate::pimble::PimbleService;
use crate::ratelimit::{QuotaLimiter, RateLimiter};
use crate::relay::Relay;
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
    /// The relay's registry (docs/RELAY_CONTRACT.md): which store is behind
    /// which owner's tunnel, in this process's memory and nowhere else. It
    /// dies with the process, and nothing is lost by that: every tunnel and
    /// member reconnects and it fills again.
    pub relay: Relay,
    /// One personal-document write at a time, so a version check and its
    /// write are one step (docs/DISCUSSIONS_CONTRACT.md section 8).
    pub personal_lock: tokio::sync::Mutex<()>,
    /// One write of watches at a time, so a version check and its write are
    /// one step (docs/NOTIFICATIONS_CONTRACT.md section 3).
    pub watches_lock: tokio::sync::Mutex<()>,
    /// One relay report taken at a time, so a report sent twice at once is
    /// gathered once (`POST /api/v1/watches/writes`).
    pub relay_reports_lock: tokio::sync::Mutex<()>,
    /// Per store: every change that could leave it with no owner (a demotion,
    /// a removal, an account deleted) counts the owners and writes under its
    /// store's lock, so two owners dropping each other at once cannot both
    /// count two (`crate::routes::members`).
    pub store_owner_locks: crate::locks::KeyedLocks,
    /// Per account: a password change, a recovery and a new recovery code
    /// read the account again and write under its lock, so neither writes
    /// over the other (`crate::routes::password`).
    pub account_locks: crate::locks::KeyedLocks,
    /// Where a test may hold a request to make a race overlap; nothing can
    /// be held outside builds with the `test-hooks` feature
    /// (`crate::checkpoints`).
    pub checkpoints: crate::checkpoints::Checkpoints,
    /// The hosted stores whose watched documents (or their watchers' shares)
    /// changed, for the hosted server to be told (`crate::watches`).
    pub watch_interest: crate::watches::InterestChanges,
    /// Notices one sender may queue (docs/NOTIFICATIONS_CONTRACT.md section 2).
    pub inbox_quota: QuotaLimiter,
    /// Wakes a recipient's waiting drain when a notice is queued for them.
    pub inbox_wakers: crate::routes::InboxWakers,
    /// This service's Web Push key, when one is configured
    /// (`PIMBLE_VAPID_PRIVATE_KEY`).
    pub vapid: Option<crate::webpush::VapidKey>,
    /// What pushes go out on: short timeouts, no redirects.
    pub push_http: reqwest::Client,
}

impl Inner {
    /// The waker of `recipient`'s drains.
    pub fn inbox_waker(&self, recipient: &str) -> Arc<tokio::sync::Notify> {
        let mut wakers = self.inbox_wakers.lock().expect("inbox wakers poisoned");
        Arc::clone(wakers.entry(recipient.to_string()).or_default())
    }

    /// A notice was queued for `recipient`: every drain of theirs that waits answers.
    pub fn wake_inbox(&self, recipient: &str) {
        if let Some(wake) = self.inbox_wakers.lock().expect("inbox wakers poisoned").get(recipient) {
            wake.notify_waiters();
        }
    }
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
        let relay = Relay::new(config.relay.clone());
        let vapid = match config.vapid_private_key.as_deref().map(crate::webpush::VapidKey::from_private) {
            Some(Ok(key)) => Some(key),
            Some(Err(e)) => {
                tracing::error!("PIMBLE_VAPID_PRIVATE_KEY is not a VAPID key ({e}); web push is off");
                None
            }
            None => {
                tracing::warn!("PIMBLE_VAPID_PRIVATE_KEY is not set; web push is off");
                None
            }
        };
        let push_http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
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
            relay,
            personal_lock: tokio::sync::Mutex::new(()),
            watches_lock: tokio::sync::Mutex::new(()),
            relay_reports_lock: tokio::sync::Mutex::new(()),
            store_owner_locks: Default::default(),
            account_locks: Default::default(),
            checkpoints: Default::default(),
            watch_interest: crate::watches::InterestChanges::default(),
            inbox_quota: QuotaLimiter::new(crate::routes::inbox::SEND_WINDOW, crate::routes::inbox::SENDS_PER_WINDOW),
            inbox_wakers: Default::default(),
            vapid,
            push_http,
        }))
    }
}
