//! Your place in every node (docs/DISCUSSIONS_CONTRACT.md section 8b): the
//! selection as two sticky positions in the personal document, written when
//! a pane leaves a note and after about five seconds of quiet, never per
//! caret move; put back when the note opens again and nothing else (a deep
//! link, a search hit) names a place.

use std::cell::Cell;

use pimble_core::{NodeId, StoreId};
use pimble_discussion::Place;
use rinch::prelude::*;
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
    let place = Place { anchor, head, top: None, at: crate::boards::now_stamp() };
    if crate::personal::with(|p| p.set_place(&store_id.to_string(), &node_id.to_string(), &place)) {
        crate::personal::changed(store);
    }
}

/// The caret moved in pane `pane`: its place is written once it rests.
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

/// Put pane `pane`'s caret where this person last was in the note, a turn
/// later (the view is mounted then), when the personal document knows.
pub(crate) fn restore(pane: PaneId, store_id: StoreId, node_id: NodeId) {
    let Some(place) = crate::personal::with(|p| p.place(&store_id.to_string(), &node_id.to_string())) else { return };
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
            handle.scroll_into_view_aligned(head.pos, head.pos, crate::rinch_editor::ScrollAlign::Fraction(1.0 / 3.0));
        })
    });
}
