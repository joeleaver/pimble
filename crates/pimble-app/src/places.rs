//! Your place in every text node (docs/DISCUSSIONS_CONTRACT.md section 8b):
//! the selection and the first line in view as sticky positions, kept on
//! this device (`views.rs`), written when a pane leaves a note, when the
//! window closes and after about five seconds without a caret move or a
//! scroll; put back when the note opens again and nothing else (a deep link,
//! a search hit) names a place.

use std::cell::Cell;

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;
use rinch_editor_core::Pos;
use rinch_editor_collab::StickyAssoc;

use crate::panes::{PaneId, MAX_PANES};
use crate::state::AppStore;

/// How long the caret rests before its place is written.
const QUIET_MS: u32 = 5000;

thread_local! {
    /// Bumped by every caret move per pane; a quiet timer of an older one does nothing.
    static MOVES: [Cell<u64>; MAX_PANES] = const { [Cell::new(0), Cell::new(0), Cell::new(0), Cell::new(0)] };
}

/// Write where pane `pane`'s editor is in note `(store_id, node_id)`.
pub(crate) fn remember(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId) {
    let handle = crate::editor::editor(pane);
    let selection = handle.selection();
    let (Some(anchor), Some(head)) = (
        handle.collab_sticky_index_with(selection.anchor(), StickyAssoc::After),
        handle.collab_sticky_index_with(selection.head(), StickyAssoc::After),
    ) else {
        return;
    };
    // The first line in view, when the text is on screen to say.
    let shown = untracked(|| {
        let state = store.pane(pane);
        state.show_editor.get() && !state.show_board.get() && !state.show_sheet.get()
    });
    let top = shown
        .then(|| crate::presence::text::viewport_top(pane, &handle))
        .flatten()
        .and_then(|pos| handle.collab_sticky_index_with(Pos(pos), StickyAssoc::After));
    crate::views::set_place(store_id, node_id, crate::views::TextPlace::new(anchor, head, top));
}

/// Remember the place of every pane holding a note (the window is closing).
pub(crate) fn remember_all(store: AppStore) {
    for pane in PaneId::ALL {
        if let Some(active) = untracked(|| store.pane(pane).active_edit.get()) {
            remember(store, pane, active.store_id, active.node_id);
        }
    }
}

/// The caret moved or the text scrolled in pane `pane`: its place is written
/// once it rests.
pub(crate) fn moved(store: AppStore, pane: PaneId) {
    let generation = MOVES.with(|m| {
        let g = m[pane.index()].get() + 1;
        m[pane.index()].set(g);
        g
    });
    rinch::core::reactive::unowned(|| {
        set_timeout(QUIET_MS, move || {
            if MOVES.with(|m| m[pane.index()].get()) != generation {
                return;
            }
            if let Some(active) = untracked(|| store.pane(pane).active_edit.get()) {
                remember(store, pane, active.store_id, active.node_id);
            }
        })
    });
}

/// Put pane `pane`'s caret where this person last was in the note on this
/// device, with the same line at the top, a turn later (the view is mounted
/// then).
pub(crate) fn restore(pane: PaneId, store_id: StoreId, node_id: NodeId) {
    let Some(place) = crate::views::place(store_id, node_id) else { return };
    rinch::core::reactive::unowned(|| {
        set_timeout(0, move || {
            let handle = crate::editor::editor(pane);
            let (Some(anchor), Some(head)) = (handle.collab_locate_sticky(&place.anchor), handle.collab_locate_sticky(&place.head)) else { return };
            let selection = if anchor.pos == head.pos {
                rinch_editor_core::selection::Selection::cursor(head.pos)
            } else {
                rinch_editor_core::selection::Selection::text(anchor.pos, head.pos)
            };
            handle.set_selection(selection);
            match place.top.as_deref().and_then(|top| handle.collab_locate_sticky(top)) {
                Some(top) => handle.scroll_into_view_aligned(top.pos, top.pos, crate::rinch_editor::ScrollAlign::Fraction(0.0)),
                None => handle.scroll_into_view_aligned(head.pos, head.pos, crate::rinch_editor::ScrollAlign::Fraction(1.0 / 3.0)),
            }
        })
    });
}
