//! Two minimal per-key limiters: [`RateLimiter`], a cooldown ("one send per
//! address per minute" — `POST /resend-verification`, docs/CLOUD_CONTRACT.md
//! "Phase 1b"), and [`QuotaLimiter`], a count within a window ("thirty
//! invitations per inviter per hour" — docs/SHARING_CONTRACT.md).
//!
//! In-memory only — fine for a single-process phase 1 deployment, same as
//! every other piece of in-memory state this service holds (the releases
//! and JWKS caches). A restart resets it, which only ever makes the limit
//! more permissive, never less.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    interval: Duration,
    last: Mutex<HashMap<String, Instant>>,
}

impl RateLimiter {
    pub fn new(interval: Duration) -> Self {
        Self { interval, last: Mutex::new(HashMap::new()) }
    }

    /// `true` and records `key` as used just now, unless `key` was already
    /// used within the last `interval`, in which case `false` and nothing is
    /// recorded (so the original attempt's cooldown keeps counting down).
    pub fn try_acquire(&self, key: &str) -> bool {
        let mut last = self.last.lock().expect("RateLimiter mutex poisoned");
        let now = Instant::now();
        match last.get(key) {
            Some(prev) if now.duration_since(*prev) < self.interval => false,
            _ => {
                last.insert(key.to_string(), now);
                true
            }
        }
    }

    /// Unconditionally marks `key` as used just now, regardless of whether
    /// it was already within its cooldown. Used by every code path that
    /// actually sends mail but isn't itself gated by this limiter (signup,
    /// and a signup retry against an existing unverified address both
    /// always send) — so a `resend-verification` moments later still sees a
    /// recent send and stays silent, instead of finding an empty map because
    /// only `try_acquire` ever wrote to it.
    pub fn record(&self, key: &str) {
        self.last.lock().expect("RateLimiter mutex poisoned").insert(key.to_string(), std::time::Instant::now());
    }
}

/// A quota rather than a cooldown: at most `limit` uses of a key within any
/// `window` (docs/SHARING_CONTRACT.md's "thirty invitations per inviter per
/// hour"). [`RateLimiter`] above can't express this — it allows exactly one
/// use per interval, and thirty invitations in a row is the ordinary way
/// somebody shares a folder with their family.
///
/// In-memory and per-process, same tradeoff as [`RateLimiter`]: a restart
/// only ever makes the limit more permissive.
pub struct QuotaLimiter {
    window: Duration,
    limit: usize,
    /// Per key, the instant of each use still inside the window. Bounded by
    /// `limit` per key, since a key at its limit records nothing further
    /// until an older use falls out of the window.
    uses: Mutex<HashMap<String, Vec<Instant>>>,
}

impl QuotaLimiter {
    pub fn new(window: Duration, limit: usize) -> Self {
        Self { window, limit, uses: Mutex::new(HashMap::new()) }
    }

    /// `true` and records a use, unless `key` has already been used `limit`
    /// times within the last `window`, in which case `false` and nothing is
    /// recorded (so a refused attempt never extends the block).
    pub fn try_acquire(&self, key: &str) -> bool {
        let mut uses = self.uses.lock().expect("QuotaLimiter mutex poisoned");
        let now = Instant::now();
        let entry = uses.entry(key.to_string()).or_default();
        entry.retain(|used| now.duration_since(*used) < self.window);
        if entry.len() >= self.limit {
            return false;
        }
        entry.push(now);
        true
    }
}
