//! Watches (docs/NOTIFICATIONS_CONTRACT.md section 3): writes to a watched
//! document, gathered until the writer has been quiet for a while.
//!
//! Two places gather writes, with the one rule here: a Pimble server that
//! verifies tokens (the hosted server, an owner's relay face) gathers each
//! (store, document, writer) for a couple of seconds before it reports the
//! write ([`Window::REPORT`]), so someone typing is a handful of reports and
//! not hundreds; and the accounts service gathers what it is told per
//! (watcher, document, writer) until five quiet minutes have passed
//! ([`Window::NOTICE`]), and then queues one notice. The service counts
//! what it gathers in spells of editing, not in appends ([`Spells`]):
//! someone typing for ten minutes edited the note once, not 412 times. Nothing here holds a clock: every call names the
//! time, so a test moves it.

use std::collections::HashMap;
use std::hash::Hash;

/// "one notice is queued after five quiet minutes".
pub const QUIET_MS: i64 = 5 * 60 * 1000;
/// Someone who never stops for five minutes is still told about, once an
/// hour: a batch is due at the latest this long after its first write.
pub const LONGEST_MS: i64 = 60 * 60 * 1000;

/// Writes this far apart are two spells of editing; closer, one
/// ([`Spells`]).
pub const SPELL_GAP_MS: i64 = 30 * 1000;

/// How long a batch waits: until it has been quiet for `quiet_ms`, or at
/// the latest `longest_ms` after it began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub quiet_ms: i64,
    pub longest_ms: i64,
}

impl Window {
    /// The accounts service's: one notice per five quiet minutes.
    pub const NOTICE: Window = Window { quiet_ms: QUIET_MS, longest_ms: LONGEST_MS };
    /// A server's, before it reports writes it saw.
    pub const REPORT: Window = Window { quiet_ms: 2_000, longest_ms: 10_000 };
}

/// What has been gathered under one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gathered {
    /// How many writes ([`Gathered::add`]: a server's report) or spells of
    /// editing ([`Spells::count`]: the service's batch).
    pub count: u32,
    pub first_ms: i64,
    pub last_ms: i64,
}

impl Gathered {
    /// `count` writes at `now_ms`, the first of a batch.
    pub fn start(count: u32, now_ms: i64) -> Self {
        Self { count: count.max(1), first_ms: now_ms, last_ms: now_ms }
    }

    /// `count` more writes at `now_ms`.
    pub fn add(&mut self, count: u32, now_ms: i64) {
        self.count = self.count.saturating_add(count.max(1));
        self.last_ms = self.last_ms.max(now_ms);
    }

    /// Whether the batch is due at `now_ms` under `window`.
    pub fn is_due(&self, window: Window, now_ms: i64) -> bool {
        now_ms - self.last_ms >= window.quiet_ms || now_ms - self.first_ms >= window.longest_ms
    }
}

/// The spells of editing a batch gathered: each a span of writes, no two
/// writes next to each other in time [`SPELL_GAP_MS`] or more apart, and
/// every span that far or more from the next. Writes are told in any order
/// (a relay's report can arrive late, its writes before or among those
/// gathered already), so the spans are kept, not only the first and last
/// write: a write is judged against the writes it falls between, wherever
/// it lands, and one that closes the gap between two spans makes them one.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Spells(Vec<(i64, i64)>);

impl Spells {
    /// A write at `at_ms`.
    pub fn add(&mut self, at_ms: i64) {
        let (mut first, mut last) = (at_ms, at_ms);
        // Every span within the gap of the write is one spell with it: one
        // it falls inside, one it follows or precedes closely, or two it
        // falls between.
        self.0.retain(|&(start, end)| {
            let joins = at_ms - end < SPELL_GAP_MS && start - at_ms < SPELL_GAP_MS;
            if joins {
                first = first.min(start);
                last = last.max(end);
            }
            !joins
        });
        let at = self.0.partition_point(|&(start, _)| start < first);
        self.0.insert(at, (first, last));
    }

    /// How many spells of editing (at least one once anything is added).
    pub fn count(&self) -> u32 {
        u32::try_from(self.0.len()).unwrap_or(u32::MAX)
    }

    /// The first write and the last, once anything is added.
    pub fn span(&self) -> Option<(i64, i64)> {
        Some((self.0.first()?.0, self.0.last()?.1))
    }

    /// The spans, in time order: `(first write, last write)` each.
    pub fn spans(&self) -> &[(i64, i64)] {
        &self.0
    }

    /// These spans as a batch is judged by ([`Gathered::is_due`]), `None`
    /// while nothing is added.
    pub fn gathered(&self) -> Option<Gathered> {
        let (first_ms, last_ms) = self.span()?;
        Some(Gathered { count: self.count(), first_ms, last_ms })
    }
}

/// Writes gathered in memory per key, under one [`Window`].
#[derive(Debug, Clone)]
pub struct Batcher<K> {
    window: Window,
    pending: HashMap<K, Gathered>,
}

impl<K: Hash + Eq + Clone> Batcher<K> {
    pub fn new(window: Window) -> Self {
        Self { window, pending: HashMap::new() }
    }

    /// `count` writes under `key` at `now_ms`.
    pub fn record(&mut self, key: K, count: u32, now_ms: i64) {
        match self.pending.get_mut(&key) {
            Some(gathered) => gathered.add(count, now_ms),
            None => {
                self.pending.insert(key, Gathered::start(count, now_ms));
            }
        }
    }

    /// The batches due at `now_ms`, taken out.
    pub fn due(&mut self, now_ms: i64) -> Vec<(K, Gathered)> {
        let window = self.window;
        let keys: Vec<K> = self.pending.iter().filter(|(_, g)| g.is_due(window, now_ms)).map(|(k, _)| k.clone()).collect();
        keys.into_iter().filter_map(|k| self.pending.remove(&k).map(|g| (k, g))).collect()
    }

    /// Everything gathered, due or not, taken out (a server stopping).
    pub fn drain(&mut self) -> Vec<(K, Gathered)> {
        self.pending.drain().collect()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// What a watch notice says where nothing of the note can be read (a push
/// the service worker shows, a notice whose note this device does not
/// hold), `spells` the spells of editing it stands for: "Joe edited a note
/// you watch", "Joe edited a note you watch 3 times".
pub fn watch_title(who: &str, spells: u32) -> String {
    match spells {
        0 | 1 => format!("{who} edited a note you watch"),
        n => format!("{who} edited a note you watch {n} times"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_is_due_after_five_quiet_minutes_and_once() {
        let mut b = Batcher::new(Window::NOTICE);
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
        let mut b = Batcher::new(Window::NOTICE);
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
        assert_eq!(b.drain().len(), 1);
        assert!(b.is_empty());
    }

    #[test]
    fn a_report_waits_two_quiet_seconds_and_at_most_ten() {
        let mut g = Gathered::start(1, 0);
        g.add(1, 1_500);
        assert!(!g.is_due(Window::REPORT, 3_000));
        assert!(g.is_due(Window::REPORT, 3_500));
        let mut busy = Gathered::start(1, 0);
        for t in (0..10_000).step_by(500) {
            busy.add(1, t);
        }
        assert!(busy.is_due(Window::REPORT, 10_000), "typing without a pause is reported every ten seconds");
        assert_eq!(Gathered::start(0, 5).count, 1, "a write is at least one");
    }

    fn spells(at: &[i64]) -> Spells {
        let mut spells = Spells::default();
        for t in at {
            spells.add(*t);
        }
        spells
    }

    #[test]
    fn spells_of_editing_are_counted_not_writes() {
        let mut g = spells(&[0]);
        for t in (2_000..600_000).step_by(2_000) {
            g.add(t);
        }
        assert_eq!(g.count(), 1, "ten minutes of typing without a pause is one spell");
        g.add(600_000 + SPELL_GAP_MS - 2_000);
        assert_eq!(g.count(), 2, "a half-minute pause starts another");
        g.add(-SPELL_GAP_MS);
        assert_eq!((g.count(), g.span()), (3, Some((-SPELL_GAP_MS, 600_000 + SPELL_GAP_MS - 2_000))), "a write told late, well before the first");
        g.add(300_000);
        assert_eq!(g.count(), 3, "one inside a spell is part of it");
        assert_eq!(g.gathered(), Some(Gathered { count: 3, first_ms: -SPELL_GAP_MS, last_ms: 600_000 + SPELL_GAP_MS - 2_000 }));
        assert_eq!(Spells::default().gathered(), None);
    }

    #[test]
    fn a_late_report_inside_the_span_is_judged_by_the_writes_around_it() {
        // Two spells ten minutes apart; a late write lands between them, at
        // least half a minute from both: a third spell.
        let mut g = spells(&[0, 600_000]);
        g.add(300_000);
        assert_eq!((g.count(), g.spans()), (3, &[(0, 0), (300_000, 300_000), (600_000, 600_000)][..]));
        // Late writes that close a gap join the spells on both sides.
        g.add(300_000 - SPELL_GAP_MS + 1);
        assert_eq!(g.count(), 3, "close to the middle one: part of it");
        let mut g = spells(&[0, 40_000]);
        assert_eq!(g.count(), 2);
        g.add(20_000);
        assert_eq!((g.count(), g.spans()), (1, &[(0, 40_000)][..]), "twenty seconds from each: one spell after all");
        // Order does not matter.
        let at = [5_000, 220_000, 80_000, 0, 100_000, 200_000, 60_000];
        let mut sorted = at;
        sorted.sort();
        assert_eq!(spells(&at), spells(&sorted));
        assert_eq!(spells(&at).count(), 3, "0..5s, 60..100s, 200..220s");
        // The same write told twice changes nothing.
        let mut again = spells(&at);
        again.add(80_000);
        assert_eq!(again, spells(&at));
    }

    #[test]
    fn the_title_says_who_and_how_often() {
        assert_eq!(watch_title("Joe", 3), "Joe edited a note you watch 3 times");
        assert_eq!(watch_title("Joe", 1), "Joe edited a note you watch");
        assert_eq!(watch_title("Joe", 0), "Joe edited a note you watch");
    }
}
