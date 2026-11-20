//! A note out of step, said on the note (docs/CONVERGENCE_CONTRACT.md
//! section 4; Joe, 2026-11-19: "it should go on the specific note, not the
//! store"): a mark on its row in the explorer with the reason as its tooltip,
//! a line at the top of the pane that shows it, and a line in its
//! Properties. The words, once, for all three.
//!
//! Joe, 2026-11-20: a mark on a note only when someone can act. Another
//! device holding changes this one lacks, and changes this device could not
//! send, are shown on the row, the pane and in Properties. Changes no device
//! has sent, and what was set aside of them, are said only in Properties, as
//! a quiet line (`OutOfStep::is_shown`).

use pimble_core::{OutOfStep, OutOfStepKind};

/// When the difference began, in this device's local time: to the minute
/// for what is shown, the day for the quiet lines ("waiting since 26 Oct
/// 2026").
pub fn since_text(entry: &OutOfStep) -> String {
    match entry.kind {
        OutOfStepKind::Waiting | OutOfStepKind::SetAside => entry.since.with_timezone(&chrono::Local).format("%-d %b %Y").to_string(),
        _ => crate::properties::when(entry.since),
    }
}

/// The pane's line and the Properties panel's: "Out of step since 19 Nov
/// 2026, 14:05: <reason>", or the quiet line of a kind not shown.
pub fn notice(entry: &OutOfStep) -> String {
    entry.sentence(&since_text(entry))
}

/// The row mark's tooltip: the same, so a person reads one wording.
pub fn row_tooltip(entry: &OutOfStep) -> String {
    notice(entry)
}

/// What a row's `TreeNodeData` label carries of it, so the row re-renders
/// when a note goes out of step, comes back in step, or its reason changes
/// (rows snapshot at render time, CLAUDE.md "Tree appearance"). Only what
/// the row shows: a quiet entry adds nothing.
pub fn row_snapshot(entry: Option<&OutOfStep>) -> String {
    entry.filter(|e| e.is_shown()).map(|e| format!("|out of step: {}", e.reason)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::NodeId;

    fn entry(reason: &str, kind: OutOfStepKind) -> OutOfStep {
        let since = chrono::DateTime::parse_from_rfc3339("2026-11-19T14:05:00Z").unwrap().with_timezone(&chrono::Utc);
        OutOfStep { node_id: NodeId::new(), since, reason: reason.to_string(), kind }
    }

    #[test]
    fn the_pane_and_the_row_say_the_reason_and_since_when() {
        let e = entry("Will's device holds changes this one doesn't have yet. They'll arrive when Will opens Pimble.", OutOfStepKind::Behind);
        let local = crate::properties::when(e.since);
        assert_eq!(notice(&e), format!("Out of step since {local}: Will's device holds changes this one doesn't have yet. They'll arrive when Will opens Pimble."));
        assert_eq!(row_tooltip(&e), notice(&e));
        assert!(notice(&e).contains("2026"));
        assert!(e.is_shown());
    }

    #[test]
    fn the_row_label_changes_with_the_mark_and_its_reason() {
        assert_eq!(row_snapshot(None), "", "a note in step adds nothing to its row");
        let a = row_snapshot(Some(&entry("This device holds changes Will's device doesn't have, and couldn't send them.", OutOfStepKind::Unsent)));
        let b = row_snapshot(Some(&entry("Another device holds changes this one doesn't have yet. They'll arrive when that device is online again.", OutOfStepKind::Behind)));
        assert!(a.contains("out of step") && a.contains("couldn't send"));
        assert_ne!(a, b, "a new reason re-renders the row");
    }

    /// Joe, 2026-11-20: what nobody can act on is not on the row nor the
    /// pane, and Properties says it quietly, with the day.
    #[test]
    fn changes_nobody_sent_are_said_quietly_and_only_in_properties() {
        let waiting = entry(pimble_rpc::vault::WAITING_REASON, OutOfStepKind::Waiting);
        assert!(!waiting.is_shown());
        assert_eq!(row_snapshot(Some(&waiting)), "", "no mark on the row");
        let day = waiting.since.with_timezone(&chrono::Local).format("%-d %b %Y").to_string();
        assert_eq!(notice(&waiting), format!("Some changes from another device never arrived (waiting since {day})."));
        let aside = entry(pimble_rpc::vault::SET_ASIDE_REASON, OutOfStepKind::SetAside);
        assert!(!aside.is_shown());
        assert_eq!(notice(&aside), format!("Some changes from another device never arrived and were set aside on {day}."));
        // An entry from a Pimble before wave 3 has no kind and is read as
        // shown, but its parked wording is quiet.
        let older: OutOfStep = serde_json::from_value(serde_json::json!({ "node_id": NodeId::new(), "since": waiting.since, "reason": "Waits for earlier changes another device has not sent." })).unwrap();
        assert!(!older.is_shown(), "an older server's parked note is quiet too");
        let older_behind: OutOfStep = serde_json::from_value(serde_json::json!({ "node_id": NodeId::new(), "since": waiting.since, "reason": "Will's device holds changes this one lacks." })).unwrap();
        assert!(older_behind.is_shown());
    }
}
