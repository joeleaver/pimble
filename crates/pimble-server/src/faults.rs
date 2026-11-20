//! Faults a test injects into this server's vault links
//! (docs/CONVERGENCE_CONTRACT.md, "Proving it"): the randomized fault test
//! drives three devices over a real hosted server and real links, and breaks
//! them between steps the ways real devices break. Nothing here is set
//! outside a test: `ServerConfig::link_faults` is `None`, and every hook is
//! then a branch not taken.
//!
//! Each fault is a counter or a switch the test sets and the link consumes:
//!
//! - [`LinkFaults::set_offline`]: this device's network is gone. A connected
//!   link drops its connection within a tick, and connects fail until it is
//!   back. Per device, unlike cutting the relay all devices share.
//! - [`LinkFaults::lose_answers`]: the next appends are stored by the remote
//!   and their answers lost on the way back: the link sees its connection
//!   fail after the remote took the append and before it recorded anything.
//! - [`LinkFaults::drop_notifications`]: the next notifications from the
//!   remote are lost (an append's or a report's).
//! - [`LinkFaults::set_snapshot_every`]: a snapshot is due every few appends
//!   rather than every 200, so snapshots land while another device reads.
//! - [`LinkFaults::set_page_bytes`]: the link reads logs in pages this small,
//!   so a read spans many pages and a snapshot can land between two.
//! - [`LinkFaults::set_report_debounce`]: reports go this soon after a change
//!   rather than 5 s, so a test settles in seconds.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Default)]
pub struct LinkFaults {
    offline: AtomicBool,
    lose_answers: AtomicU32,
    drop_notifications: AtomicU32,
    snapshot_every: AtomicU64,
    page_bytes: AtomicU64,
    report_debounce_ms: AtomicU64,
    /// How many of each fault actually happened, for a test to say what it
    /// exercised.
    lost: AtomicU32,
    dropped: AtomicU32,
}

impl LinkFaults {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_offline(&self, offline: bool) {
        self.offline.store(offline, Ordering::SeqCst);
    }

    pub fn is_offline(&self) -> bool {
        self.offline.load(Ordering::SeqCst)
    }

    /// The next `n` appends' answers are lost.
    pub fn lose_answers(&self, n: u32) {
        self.lose_answers.fetch_add(n, Ordering::SeqCst);
    }

    /// The next `n` notifications from the remote are lost.
    pub fn drop_notifications(&self, n: u32) {
        self.drop_notifications.fetch_add(n, Ordering::SeqCst);
    }

    /// A snapshot is due every `n` appends (0: the usual 200).
    pub fn set_snapshot_every(&self, n: u64) {
        self.snapshot_every.store(n, Ordering::SeqCst);
    }

    /// Logs are read in pages of at most `bytes` (0: the usual).
    pub fn set_page_bytes(&self, bytes: u64) {
        self.page_bytes.store(bytes, Ordering::SeqCst);
    }

    /// Reports go `after` a change (zero: the usual 5 s).
    pub fn set_report_debounce(&self, after: Duration) {
        self.report_debounce_ms.store(after.as_millis() as u64, Ordering::SeqCst);
    }

    /// How many answers were lost and notifications dropped so far.
    pub fn happened(&self) -> (u32, u32) {
        (self.lost.load(Ordering::SeqCst), self.dropped.load(Ordering::SeqCst))
    }

    pub(crate) fn take_lost_answer(&self) -> bool {
        let took = take_one(&self.lose_answers);
        if took {
            self.lost.fetch_add(1, Ordering::SeqCst);
        }
        took
    }

    pub(crate) fn take_dropped_notification(&self) -> bool {
        let took = take_one(&self.drop_notifications);
        if took {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
        took
    }

    pub(crate) fn snapshot_every(&self) -> Option<u64> {
        Some(self.snapshot_every.load(Ordering::SeqCst)).filter(|n| *n > 0)
    }

    pub(crate) fn page_bytes(&self) -> Option<u64> {
        Some(self.page_bytes.load(Ordering::SeqCst)).filter(|n| *n > 0)
    }

    pub(crate) fn report_debounce(&self) -> Option<Duration> {
        Some(self.report_debounce_ms.load(Ordering::SeqCst)).filter(|n| *n > 0).map(Duration::from_millis)
    }
}

fn take_one(counter: &AtomicU32) -> bool {
    counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok()
}
