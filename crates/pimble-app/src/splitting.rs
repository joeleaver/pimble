//! Edit > "Split Note at Caret" and "Join Note Below" (docs/SPLIT_CONTRACT.md),
//! on the note in the focused pane. The writes are the API's
//! (`Pimble::split_node`, `Pimble::join_next`): this names the place, from
//! the editor's caret, and says why when there is nothing to do.

use rinch::prelude::untracked;

use crate::protocol::BackendCommand;
use crate::state::AppStore;

const NO_NOTE: &str = "Open a text note to split or join it.";

/// The note the focused pane holds, when this device may change it.
fn writable_note(store: AppStore) -> Option<(crate::panes::PaneId, pimble_core::StoreId, pimble_core::NodeId)> {
    let pane = store.focused();
    let Some(active) = untracked(|| store.pane(pane).active_edit.get()) else {
        crate::events::show_notice(store, NO_NOTE.to_string());
        return None;
    };
    if !store.node_access(active.store_id, active.node_id).allows_write() {
        crate::events::show_notice(store, pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string());
        return None;
    }
    Some((pane, active.store_id, active.node_id))
}

/// Split the focused note at its caret: the text from there on becomes a new
/// note below it. With words selected, the split is where they start and they
/// are the new note's title (Scrivener's "Split with Selection as Title").
pub fn split_here(store: AppStore) {
    let Some((pane, store_id, node_id)) = writable_note(store) else { return };
    let handle = crate::editor::editor(pane);
    let selection = handle.selection();
    let Some(at) = handle.collab_sticky_index(selection.from()) else {
        crate::events::show_notice(store, "Put the caret in the text where the new note should begin.".to_string());
        return;
    };
    let title = if selection.is_empty() { None } else { handle.selection_clipboard().map(|(_, text)| title_of(&text)).filter(|t| !t.is_empty()) };
    store.send(BackendCommand::SplitNode { store_id, node_id, at, title });
}

/// Join the note below the focused one into it.
pub fn join_below(store: AppStore) {
    let Some((_, store_id, node_id)) = writable_note(store) else { return };
    store.send(BackendCommand::JoinBelow { store_id, node_id });
}

/// A selection as a title: its first line, trimmed, at most 120 characters.
fn title_of(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    line.chars().take(120).collect::<String>().trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::title_of;

    #[test]
    fn a_selection_titles_by_its_first_line() {
        assert_eq!(title_of("  Chapter Two \nand the rest"), "Chapter Two");
        assert_eq!(title_of("\n\n"), "");
        assert_eq!(title_of(&"x".repeat(200)).len(), 120);
    }
}
