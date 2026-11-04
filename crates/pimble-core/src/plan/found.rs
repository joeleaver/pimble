//! What a found row allows (docs/QUERY_METANODES_CONTRACT.md section 4): a
//! node a query metanode found is the real node, shown where it does not
//! live. The node itself may change (open, edit, rename, appearance,
//! discuss, copy, a new child); its place may not, from there. One rule, so
//! the explorer, plugins and the MCP refuse the same things with the same
//! sentence.

/// What a person does to a row that a query metanode found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundRowAction {
    Open,
    Edit,
    Rename,
    Appearance,
    Discuss,
    Copy,
    /// A new node inside it: a real child of the real node.
    NewInside,
    Delete,
    Cut,
    /// Dragging it away.
    Drag,
    /// A drop into it or beside it.
    Drop,
    /// Alt+Shift arrows.
    KeyboardMove,
    PasteInto,
}

impl FoundRowAction {
    /// Whether a found row allows it: everything that changes the node, nothing
    /// that changes where it is or puts something at its place there.
    pub fn allowed(self) -> bool {
        use FoundRowAction::*;
        match self {
            Open | Edit | Rename | Appearance | Discuss | Copy | NewInside => true,
            Delete | Cut | Drag | Drop | KeyboardMove | PasteInto => false,
        }
    }
}

/// Where a found node lives, as the refusal says it: its store and the titles
/// above it, `"Personal / Projects"`.
pub fn lives_in<S: AsRef<str>>(path: &[S]) -> String {
    path.iter().map(|s| s.as_ref().trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" / ")
}

/// The sentence a refused action on a found row says (the menu's top line):
/// "Shown here because it is tagged #waiting. It lives in Personal /
/// Projects." `reason` is the metanode's [`crate::metanode::Source::found_reason`],
/// `lives_in` what [`lives_in`] makes of the node's real place.
pub fn found_refusal(reason: &str, lives_in: &str) -> String {
    let reason = reason.trim().trim_end_matches('.');
    if lives_in.trim().is_empty() {
        format!("Shown here because {reason}.")
    } else {
        format!("Shown here because {reason}. It lives in {}.", lives_in.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_found_row_changes_itself_not_its_place() {
        use FoundRowAction::*;
        for allowed in [Open, Edit, Rename, Appearance, Discuss, Copy, NewInside] {
            assert!(allowed.allowed(), "{allowed:?}");
        }
        for refused in [Delete, Cut, Drag, Drop, KeyboardMove, PasteInto] {
            assert!(!refused.allowed(), "{refused:?}");
        }
        assert_eq!(
            found_refusal("it is tagged #waiting", &lives_in(&["Personal", "Projects"])),
            "Shown here because it is tagged #waiting. It lives in Personal / Projects."
        );
        assert_eq!(found_refusal("it is a canvas", ""), "Shown here because it is a canvas.");
    }
}
