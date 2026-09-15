//! A minimal per-key rate limiter: `POST /resend-verification`'s "one send
//! per address per minute" (docs/CLOUD_CONTRACT.md, "Phase 1b").
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
}
