//! How far into a vault document's log a client has read, without gaps.
//!
//! A vault log hands out sequence numbers in arrival order to every device that
//! appends, so the numbers a device has dealt with are not a prefix: its own
//! append is numbered the moment the server takes it, while another device's
//! append with a lower number may still be on its way here. "The highest
//! number I have seen" therefore says nothing about the numbers below it, and
//! two decisions need exactly that:
//!
//! - **Where to fetch from after a reconnect.** Fetching after the highest
//!   number seen skips the entry that was still in flight when the connection
//!   dropped, for good.
//! - **What a snapshot covers.** The server deletes every log entry at or below
//!   a snapshot's number (docs/CRYPTO_CONTRACT.md), so a snapshot stamped with
//!   a number whose predecessors were not all applied destroys the ones it
//!   lacks. Nobody can get them back.
//!
//! [`VaultCursor::applied_through`] is the answer to both: the largest `n` such
//! that every entry `1..=n` is reflected in the local document, because it was
//! applied here or appended from here. A blob that could not be decrypted is
//! never marked, so the cursor stops in front of it: the device keeps asking
//! for it, and never vouches for it in a snapshot.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaultCursor {
    applied_through: u64,
    /// Entries dealt with beyond a gap.
    ahead: BTreeSet<u64>,
}

impl VaultCursor {
    /// A cursor for a document whose entries `1..=applied_through` are known to
    /// be reflected locally (0 for a document nothing is known about).
    pub fn starting_at(applied_through: u64) -> Self {
        Self { applied_through, ahead: BTreeSet::new() }
    }

    /// Every entry up to and including this number is reflected locally.
    pub fn applied_through(&self) -> u64 {
        self.applied_through
    }

    /// Entry `seq` is reflected locally: applied here, or appended from here.
    pub fn mark(&mut self, seq: u64) {
        if seq <= self.applied_through {
            return;
        }
        self.ahead.insert(seq);
        self.close_gaps();
    }

    /// Everything up to and including `seq` is reflected locally: a snapshot
    /// covering it was applied.
    pub fn mark_through(&mut self, seq: u64) {
        if seq > self.applied_through {
            self.applied_through = seq;
            self.ahead = self.ahead.split_off(&(seq + 1));
        }
        self.close_gaps();
    }

    fn close_gaps(&mut self) {
        while self.ahead.remove(&(self.applied_through + 1)) {
            self.applied_through += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_own_append_does_not_vouch_for_the_entry_still_in_flight_below_it() {
        let mut cursor = VaultCursor::starting_at(8);
        cursor.mark(10); // our append; somebody else's 9 has not arrived
        assert_eq!(cursor.applied_through(), 8);
        cursor.mark(9);
        assert_eq!(cursor.applied_through(), 10);
    }

    #[test]
    fn a_skipped_entry_holds_the_cursor_until_a_snapshot_covers_it() {
        let mut cursor = VaultCursor::default();
        cursor.mark(1);
        cursor.mark(3); // 2 would not decrypt
        cursor.mark(4);
        assert_eq!(cursor.applied_through(), 1);
        cursor.mark_through(3);
        assert_eq!(cursor.applied_through(), 4);
    }

    #[test]
    fn marking_is_idempotent_and_never_goes_back() {
        let mut cursor = VaultCursor::starting_at(5);
        cursor.mark(3);
        cursor.mark(6);
        cursor.mark(6);
        cursor.mark_through(2);
        assert_eq!(cursor.applied_through(), 6);
    }
}
