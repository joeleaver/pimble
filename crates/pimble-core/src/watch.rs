//! Watches (docs/NOTIFICATIONS_CONTRACT.md section 3): appends to a watched
//! document, gathered until the writer has been quiet for five minutes.
//!
//! The one batching code: the accounts service gathers what the hosted
//! server and every owner's relay face report through it, and the sharing
//! test harness's stand-in service does too. It holds no clock: every call
//! names the time, so a test moves it.

use std::collections::HashMap;
use std::hash::Hash;

/// "one notice is queued after five quiet minutes".
pub const QUIET_MS: i64 = 5 * 60 * 1000;
/// Someone who never stops for five minutes is still told about, once an
/// hour: a batch is queued at the latest this long after its first append.
pub const LONGEST_MS: i64 = 60 * 60 * 1000;

/// What has been gathered under one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gathered {
    /// How many appends.
    pub count: u32,
    pub first_ms: i64,
    pub last_ms: i64,
}

/// Appends gathered per key (for watches: watcher, document, writer).
#[derive(Debug, Clone)]
pub struct Batcher<K> {
    pending: HashMap<K, Gathered>,
}

impl<K> Default for Batcher<K> {
    fn default() -> Self {
        Self { pending: HashMap::new() }
    }
}

impl<K: Hash + Eq + Clone> Batcher<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// `count` appends under `key` at `now_ms`.
    pub fn record(&mut self, key: K, count: u32, now_ms: i64) {
        let entry = self.pending.entry(key).or_insert(Gathered { count: 0, first_ms: now_ms, last_ms: now_ms });
        entry.count = entry.count.saturating_add(count.max(1));
        entry.last_ms = entry.last_ms.max(now_ms);
    }

    /// The batches due at `now_ms` (quiet for [`QUIET_MS`], or gathering for
    /// [`LONGEST_MS`]), taken out.
    pub fn due(&mut self, now_ms: i64) -> Vec<(K, Gathered)> {
        let keys: Vec<K> = self
            .pending
            .iter()
            .filter(|(_, g)| now_ms - g.last_ms >= QUIET_MS || now_ms - g.first_ms >= LONGEST_MS)
            .map(|(k, _)| k.clone())
            .collect();
        keys.into_iter().filter_map(|k| self.pending.remove(&k).map(|g| (k, g))).collect()
    }

    /// Forget every batch `keep` says no to (a watcher who turned it all off).
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.pending.retain(|k, _| keep(k));
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// The line a watch notice is told in where nothing of the note can be
/// read (a push the service worker shows, an email): "3 changes in a note
/// you watch".
pub fn changes_line(count: u32) -> String {
    if count == 1 {
        "1 change in a note you watch".to_string()
    } else {
        format!("{count} changes in a note you watch")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_is_due_after_five_quiet_minutes_and_once() {
        let mut b = Batcher::new();
        b.record("k", 1, 0);
        b.record("k", 1, 60_000);
        b.record("k", 2, 120_000);
        assert!(b.due(120_000 + QUIET_MS - 1).is_empty(), "not quiet for five minutes yet");
        let due = b.due(120_000 + QUIET_MS);
        assert_eq!(due, vec![("k", Gathered { count: 4, first_ms: 0, last_ms: 120_000 })]);
        assert!(b.due(i64::MAX / 2).is_empty(), "taken once");
    }

    #[test]
    fn keys_are_apart_and_a_writer_who_never_stops_is_told_hourly() {
        let mut b = Batcher::new();
        let mut t = 0;
        while t < LONGEST_MS {
            b.record(("w", "d", "joe"), 1, t);
            t += 60_000;
        }
        b.record(("w", "d", "ann"), 1, t - 60_000);
        let due = b.due(t);
        assert_eq!(due.len(), 1, "only Joe's has gathered an hour");
        assert_eq!(due[0].0, ("w", "d", "joe"));
        assert_eq!(due[0].1.count, 60);
        assert_eq!(b.len(), 1);
        b.retain(|k| k.2 != "ann");
        assert!(b.is_empty());
        assert_eq!(changes_line(3), "3 changes in a note you watch");
        assert_eq!(changes_line(1), "1 change in a note you watch");
    }
}
