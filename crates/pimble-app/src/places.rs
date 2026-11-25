//! Your place in every text node (docs/DISCUSSIONS_CONTRACT.md section 8b):
//! the selection and the first line in view as sticky positions (or "the
//! start", for a note left scrolled to its top), kept on
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
    // Left at its top, it opens at its top, whatever was added above.
    let at_start = top.is_some() && crate::presence::text::shows_start(pane, &handle) == Some(true);
    crate::views::set_place(store_id, node_id, crate::views::TextPlace::new(anchor, head, top).at_start(at_start));
}

/// Remember the place of every pane holding a note (the window is closing).
#[cfg(feature = "native")]
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
            match view_top(&handle, &place) {
                Some(top) => handle.scroll_into_view_aligned(Pos(top), Pos(top), crate::rinch_editor::ScrollAlign::Fraction(0.0)),
                None => handle.scroll_into_view_aligned(head.pos, head.pos, crate::rinch_editor::ScrollAlign::Fraction(1.0 / 3.0)),
            }
        })
    });
}

/// The position a restored view puts at its top: the note's start when it
/// was left at its start (so what was added above the line first in view
/// then shows), else that line where it is now; `None` when neither can be
/// found (the caret is shown instead).
fn view_top(handle: &crate::rinch_editor::EditorHandle, place: &crate::views::TextPlace) -> Option<usize> {
    if place.at_start {
        if let Some(first) = crate::presence::text::first_textblock(handle) {
            return Some(first);
        }
    }
    place.top.as_deref().and_then(|top| handle.collab_locate_sticky(top)).map(|top| top.pos.0)
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::views::TextPlace;
    use pimble_crdt::NodeDoc;

    /// A note read newest first, left at its top: an agent puts a new
    /// section above the first heading while the note is closed, and the
    /// note opens on that section, not on the heading that was first.
    #[test]
    fn a_note_left_at_its_start_opens_at_its_start_after_a_section_is_put_above() {
        let pane = PaneId::ALL[3];
        let (store_id, node_id) = (StoreId::new(), NodeId::new());
        let mut agent = NodeDoc::new();
        agent.append_markdown("### Tuesday\n\nWhat happened on Tuesday.\n\n### Monday\n\nWhat happened on Monday.").unwrap();
        let store = AppStore::new();
        let (cmd_tx, _commands) = crossbeam_channel::bounded::<crate::protocol::BackendCommand>(256);
        let (_events, event_rx) = crossbeam_channel::bounded::<crate::protocol::BackendEvent>(1);
        store.backend.set(Some(crate::protocol::BackendHandle { cmd_tx, event_rx }));
        crate::editor::start_editing(store, pane, store_id, node_id, &agent.save());
        let handle = crate::editor::editor(pane);
        let first = crate::presence::text::first_textblock(&handle).unwrap();
        let top = handle.collab_sticky_index_with(Pos(first), StickyAssoc::After).unwrap();
        let left_at_start = TextPlace::new(top.clone(), top.clone(), Some(top.clone())).at_start(true);
        let left_scrolled = TextPlace::new(top.clone(), top.clone(), Some(top));

        let added = agent
            .edit_markdown(pimble_crdt::markdown::MarkdownEdit::ReplaceBlock {
                block: pimble_crdt::markdown::BlockTarget::Index { index: 0, expect: None },
                markdown: "### Wednesday\n\nWhat happened on Wednesday.\n\n### Tuesday",
            })
            .unwrap()
            .0;
        assert!(handle.collab_receive(&added));
        // The text of the textblock that starts at `at`.
        let text_at = |at: usize| {
            fn collect(n: &rinch_editor_core::Node, out: &mut String) {
                match n.text() {
                    Some(t) => out.push_str(t),
                    None => (0..n.child_count()).for_each(|i| collect(n.child(i), out)),
                }
            }
            let doc = handle.doc();
            let mut found = String::new();
            doc.nodes_between(0, doc.content_size(), &mut |node, pos, _| {
                if node.is_textblock() {
                    if pos + 1 == at {
                        collect(node, &mut found);
                    }
                    return false;
                }
                true
            });
            found
        };
        assert_eq!(text_at(view_top(&handle, &left_at_start).unwrap()), "Wednesday", "the new section is in view");
        assert_eq!(text_at(view_top(&handle, &left_scrolled).unwrap()), "Tuesday", "a line further down stays the line it was");
        crate::editor::stop_editing(store, pane);
    }
}
