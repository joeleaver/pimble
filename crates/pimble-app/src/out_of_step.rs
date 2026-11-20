//! A note out of step, said on the note (docs/CONVERGENCE_CONTRACT.md
//! section 4; Joe, 2026-11-19: "it should go on the specific note, not the
//! store"): a mark on its row in the explorer with the reason as its tooltip,
//! a line at the top of the pane that shows it, and a line in its
//! Properties. The words, once, for all three.

use pimble_core::OutOfStep;

/// When the difference began, in this device's local time.
pub fn since_text(entry: &OutOfStep) -> String {
    crate::properties::when(entry.since)
}

/// The pane's line and the Properties panel's: "Out of step since 19 Nov
/// 2026, 14:05: <reason>".
pub fn notice(entry: &OutOfStep) -> String {
    entry.sentence(&since_text(entry))
}

/// The row mark's tooltip: the same, so a person reads one wording.
pub fn row_tooltip(entry: &OutOfStep) -> String {
    notice(entry)
}

/// What a row's `TreeNodeData` label carries of it, so the row re-renders
/// when a note goes out of step, comes back in step, or its reason changes
/// (rows snapshot at render time, CLAUDE.md "Tree appearance").
pub fn row_snapshot(entry: Option<&OutOfStep>) -> String {
    entry.map(|e| format!("|out of step: {}", e.reason)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::NodeId;

    fn entry(reason: &str) -> OutOfStep {
        let since = chrono::DateTime::parse_from_rfc3339("2026-11-19T14:05:00Z").unwrap().with_timezone(&chrono::Utc);
        OutOfStep { node_id: NodeId::new(), since, reason: reason.to_string() }
    }

    #[test]
    fn the_pane_and_the_row_say_the_reason_and_since_when() {
        let e = entry("Will's device holds changes this one lacks.");
        let local = crate::properties::when(e.since);
        assert_eq!(notice(&e), format!("Out of step since {local}: Will's device holds changes this one lacks."));
        assert_eq!(row_tooltip(&e), notice(&e));
        assert!(notice(&e).contains("2026"));
    }

    #[test]
    fn the_row_label_changes_with_the_mark_and_its_reason() {
        assert_eq!(row_snapshot(None), "", "a note in step adds nothing to its row");
        let a = row_snapshot(Some(&entry("Waits for earlier changes another device has not sent.")));
        let b = row_snapshot(Some(&entry("Another device holds changes this one lacks.")));
        assert!(a.contains("out of step") && a.contains("Waits for earlier changes"));
        assert_ne!(a, b, "a new reason re-renders the row");
    }
}
