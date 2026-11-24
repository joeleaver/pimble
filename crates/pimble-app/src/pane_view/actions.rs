//! What a person does to the panes: focus one, split one, close one, open
//! something beside one (docs/SPLIT_VIEW_CONTRACT.md).

use rinch::prelude::*;

use crate::editor::stop_editing;
use crate::panes::{Direction, PaneId};
use crate::state::AppStore;

/// What a split refused for want of room says.
pub(crate) const FOUR_PANES_NOTICE: &str = "Four panes are open. Close one to split again.";

/// Give pane `pane` the focus because the person clicked in it or opened
/// something into it. The link picker belongs to the pane it was opened in.
pub(crate) fn focus_pane(store: AppStore, pane: PaneId) {
    if store.focus_pane(pane) {
        crate::link_picker::close(store);
    }
}

/// Halve pane `pane` and put an empty pane in the new half, focused. Answers
/// the new pane; with four open it says so in the status bar and answers
/// nothing.
pub(crate) fn split_pane(store: AppStore, pane: PaneId, direction: Direction) -> Option<PaneId> {
    let mut tiling = untracked(|| store.tiling.get());
    let Some(new) = tiling.split(pane, direction) else {
        crate::events::show_notice(store, FOUR_PANES_NOTICE.to_string());
        return None;
    };
    // The slot is empty already (closing a pane empties it), and is emptied
    // again here so a slot never brings anything into a new pane.
    stop_editing(store, new);
    store.clear_pane(new);
    store.tiling.set(tiling);
    focus_pane(store, new);
    Some(new)
}

/// Close pane `pane`: its document's session ends and the sibling it was
/// split from takes its space. The last pane is emptied instead.
pub(crate) fn close_pane(store: AppStore, pane: PaneId) {
    crate::presence::follow::stop(pane);
    store.pane(pane).beside.set(None);
    for other in PaneId::ALL {
        if untracked(|| store.pane(other).beside.get()) == Some(pane) {
            store.pane(other).beside.set(None);
        }
    }
    stop_editing(store, pane);
    store.clear_pane(pane);
    let mut tiling = untracked(|| store.tiling.get());
    let Some(neighbour) = tiling.close(pane) else { return };
    store.tiling.set(tiling);
    if store.focused() == pane {
        focus_pane(store, neighbour);
        // The keyboard goes where the focus went, when there is a document
        // there to take it.
        if untracked(|| store.pane(neighbour).show_editor.get()) {
            crate::editor::editor(neighbour).focus();
        }
    }
}

/// Open node `node_id` of `store_id` beside pane `from`, which keeps the
/// focus (a kanban's card clicked: plugin ABI 6, `HostRequest::OpenBeside`).
/// It goes to the pane `from` already opens things beside while that is still
/// shown, so one card after another replaces the last; otherwise `from` is
/// split to the right; with four panes, another pane shown takes it, and with
/// `from` the only one it opens there.
pub(crate) fn open_beside(store: AppStore, from: PaneId, store_id: pimble_core::StoreId, node_id: pimble_core::NodeId) {
    let shown = untracked(|| store.tiling.with(|tiling| tiling.panes()));
    let kept = untracked(|| store.pane(from).beside.get()).filter(|p| *p != from && shown.contains(p));
    let target = kept
        .or_else(|| {
            if !untracked(|| store.tiling.with(|tiling| tiling.can_split())) {
                return None;
            }
            let new = split_pane(store, from, Direction::Right)?;
            // The split focused the new pane; the focus stays where the person is.
            focus_pane(store, from);
            Some(new)
        })
        .or_else(|| shown.iter().copied().find(|p| *p != from));
    if let Some(target) = target {
        store.pane(from).beside.set(Some(target));
    }
    store.link_open.set(Some(crate::state::LinkOpen {
        value: format!("node_{store_id}_{node_id}"),
        store_id,
        node_id,
        anchor: None,
        pane: Some(target.unwrap_or(from)),
    }));
}

/// View > "Split Right" / "Split Down": the focused pane.
pub(crate) fn split_focused(store: AppStore, direction: Direction) {
    split_pane(store, store.focused(), direction);
}

/// View > "Close Pane": the focused pane.
pub(crate) fn close_focused(store: AppStore) {
    close_pane(store, store.focused());
}
