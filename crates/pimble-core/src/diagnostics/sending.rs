//! Who records are sent as, for every sender that sends as an account (the
//! browser's worker, over its queue's batches, and the page, over its own
//! queued lines; docs/DIAGNOSTICS_CONTRACT.md section 5). The desktop's
//! programs send with the build's send key instead and are not judged here.
//!
//! Nothing goes while no one is signed in: what was recorded waits for the
//! next sign-in. An account's records go only as that account. Records made
//! while no one was signed in go as whoever signs in next.

use serde_json::Value;

/// Whether records made while `owner` was signed in (`None`: no one was) may
/// be sent now that `signed_in` is (`None`: no one is).
pub fn may_send(owner: Option<&str>, signed_in: Option<&str>) -> bool {
    match (owner, signed_in) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(owner), Some(signed_in)) => owner == signed_in,
    }
}

/// The account a record line carries (`Envelope::record`'s `account`), if
/// any. A line that cannot be read says no one.
pub fn record_owner(line: &str) -> Option<String> {
    serde_json::from_str::<Value>(line).ok()?.get("account")?.as_str().map(str::to_string)
}

/// The record lines of `queue` that may be sent as `signed_in`, oldest
/// first: none while no one is signed in.
pub fn sendable<'a>(queue: &'a [String], signed_in: Option<&str>) -> Vec<&'a String> {
    if signed_in.is_none() {
        return Vec::new();
    }
    queue.iter().filter(|line| may_send(record_owner(line).as_deref(), signed_in)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{Envelope, Scrubber};
    use serde_json::json;

    fn line(seq: u64, account: Option<&str>) -> String {
        let e = Envelope { install: "i".into(), session: "s".into(), app: "1.0".into(), os: "web".into() };
        e.record(&Scrubber::new(None), seq, "2026-11-26T00:00:00.000Z", "log", json!({}), account)
    }

    #[test]
    fn nothing_is_sent_while_no_one_is_signed_in() {
        assert!(!may_send(None, None));
        assert!(!may_send(Some("alice"), None));
        let queue = vec![line(0, None), line(1, Some("alice"))];
        assert!(sendable(&queue, None).is_empty());
    }

    #[test]
    fn records_go_only_as_the_account_that_made_them_or_as_anyone_when_no_one_did() {
        assert!(may_send(None, Some("bob")));
        assert!(may_send(Some("bob"), Some("bob")));
        assert!(!may_send(Some("alice"), Some("bob")));
        let queue = vec![line(0, None), line(1, Some("alice")), line(2, Some("bob"))];
        let seqs: Vec<u64> = sendable(&queue, Some("bob")).iter().map(|l| serde_json::from_str::<Value>(l).unwrap()["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, [0, 2]);
        assert_eq!(record_owner(&queue[1]).as_deref(), Some("alice"));
        assert_eq!(record_owner("not json"), None);
    }
}
