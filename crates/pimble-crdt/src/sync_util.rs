//! Shared helper behind `ContentDoc`/`StoreDocument::diff_if_peer_lacks_it`
//! (docs/history/HARDENING_CONTRACT.md decision 7).
//!
//! The bug this exists to fix: a yrs v1 diff is never actually empty. Even when a peer
//! is fully up to date, `encode_diff_v1` returns `[0, 0]` (an empty struct section) plus
//! the *whole* delete set of the document that produced it — not just what changed since
//! the peer's state vector. A reconcile that pushes back whenever `!diff.is_empty()` is
//! therefore pushing on every single reconcile, for every node, forever. Telling "the
//! peer still lacks something" apart from "this diff merely restates what it already
//! has" needs the two pieces of information this module compares: our diff's own
//! inserted structs (real, because a diff never includes structs the peer already has),
//! and whether our whole delete set is a subset of the delete set the peer's own diff to
//! us just revealed (also always their whole delete set, by the same property).

use std::collections::HashMap;
use std::ops::Range;

use yrs::updates::decoder::Decode;
use yrs::{ClientID, IdSet, Update};

use crate::error::{CrdtError, Result};

/// Whether `local_diff` (our diff since the peer's last-known state vector) tells the
/// peer anything it doesn't already have, given `remote_diff` (the diff the peer just
/// sent us — decoded here only for its delete set, never applied).
pub(crate) fn peer_lacks_something(local_diff: &[u8], remote_diff: &[u8]) -> Result<bool> {
    let local = Update::decode_v1(local_diff).map_err(|e| CrdtError::Yrs(e.to_string()))?;
    let remote = Update::decode_v1(remote_diff).map_err(|e| CrdtError::Yrs(e.to_string()))?;

    // `local_diff` was encoded as "everything beyond the peer's state vector", so any
    // inserted block in it (deleted or not — `include_deleted: true`; a tombstoned block
    // still needs to reach the peer for the delete-set check below to make sense to it)
    // is, by construction, content the peer doesn't have yet.
    if !local.insertions(true).is_empty() {
        return Ok(true);
    }

    // No new structs; the only thing left that could be missing is a deletion the peer
    // hasn't recorded yet.
    Ok(!covered_by(local.delete_set(), remote.delete_set()))
}

/// Whether every id in `subset` also falls within a range in `superset`. Both sides are
/// internally sorted, non-overlapping per-client ranges (`yrs::IdSet`'s own invariant),
/// but the public API exposes no cross-`IdSet` subset check (only
/// `IdRanges::subset_of`, `pub(crate)` inside yrs), so this reimplements it over the
/// ranges each side does expose (`IdSet::iter`, `Ranges::iter`).
fn covered_by(subset: &IdSet, superset: &IdSet) -> bool {
    let by_client: HashMap<ClientID, Vec<Range<u32>>> = superset
        .iter()
        .map(|(client, ranges)| (*client, ranges.iter().cloned().collect()))
        .collect();

    for (client, ranges) in subset.iter() {
        let covering = by_client.get(client);
        for r in ranges.iter() {
            let covered = covering
                .map(|sup_ranges| sup_ranges.iter().any(|sup| sup.start <= r.start && r.end <= sup.end))
                .unwrap_or(false);
            if !covered {
                return false;
            }
        }
    }
    true
}

// ── State vectors for a peer that cannot answer with one ────────────────
//
// A vault twin (docs/CRYPTO_CONTRACT.md) stores ciphertext and has no state
// vector to offer, so the vault link keeps its own record of what the remote
// is known to hold: a state vector it advances as updates go out and come in,
// and diffs against when it reconnects.

/// The v1 encoding of an empty state vector: "the peer has nothing".
pub fn empty_state_vector() -> Vec<u8> {
    use yrs::updates::encoder::Encode;
    yrs::StateVector::default().encode_v1()
}

/// `known` advanced by `update`, both v1-encoded.
///
/// A client's clock moves only when the update continues from what is already
/// known for that client (its lowest clock is not beyond the known one). An
/// update that starts past a gap proves nothing about the clocks in between,
/// and claiming them would make a later diff skip structs the peer never got;
/// leaving the vector behind only makes a later diff carry a little extra.
pub fn advance_state_vector(known: &[u8], update: &[u8]) -> Result<Vec<u8>> {
    use yrs::updates::encoder::Encode;
    let mut known = yrs::StateVector::decode_v1(known).map_err(|e| CrdtError::Yrs(e.to_string()))?;
    let update = Update::decode_v1(update).map_err(|e| CrdtError::Yrs(e.to_string()))?;
    let lower = update.state_vector_lower();
    let upper = update.state_vector();
    for (client, end) in upper.iter() {
        if lower.get(client) <= known.get(client) {
            known.set_max(*client, *end);
        }
    }
    Ok(known.encode_v1())
}

/// Whether `local` holds a struct that `known` does not, both v1-encoded.
/// Deletions do not move a state vector, so `false` is not proof that the peer
/// lacks nothing; callers that may have deleted while disconnected track that
/// separately.
pub fn state_vector_exceeds(local: &[u8], known: &[u8]) -> Result<bool> {
    let local = yrs::StateVector::decode_v1(local).map_err(|e| CrdtError::Yrs(e.to_string()))?;
    let known = yrs::StateVector::decode_v1(known).map_err(|e| CrdtError::Yrs(e.to_string()))?;
    Ok(local.iter().any(|(client, clock)| *clock > known.get(client)))
}

#[cfg(test)]
mod state_vector_tests {
    use super::*;
    use crate::ContentDoc;

    #[test]
    fn a_pushed_update_advances_the_known_vector_until_nothing_exceeds_it() {
        let doc = ContentDoc::from_plain_text("hello").unwrap();
        let known = empty_state_vector();
        assert!(state_vector_exceeds(&doc.state_vector(), &known).unwrap());
        let known = advance_state_vector(&known, &doc.save()).unwrap();
        assert!(!state_vector_exceeds(&doc.state_vector(), &known).unwrap());
    }

    #[test]
    fn an_update_past_a_gap_does_not_claim_the_gap() {
        let mut doc = ContentDoc::from_plain_text("one").unwrap();
        let first_sv = doc.state_vector();
        doc.replace_plain_text("one two").unwrap();
        let second_only = doc.diff_since(&first_sv).unwrap();
        // The peer never got the first update; the second alone proves nothing.
        let known = advance_state_vector(&empty_state_vector(), &second_only).unwrap();
        assert!(state_vector_exceeds(&doc.state_vector(), &known).unwrap());
    }
}
