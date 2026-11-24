//! The split view as it is drawn (docs/SPLIT_VIEW_CONTRACT.md "How it is
//! drawn"): four pane slots and three divider slots, rendered once and never
//! re-parented.
//!
//! A slot is absolutely positioned inside the area from the tiling and hidden
//! when its pane is not shown, so splitting, closing and dragging only change
//! styles. Nothing here moves an editor's DOM node: rinch's `Editor {}` is
//! mounted exactly four times in the app's life, and each keeps its caret
//! and scroll position through every change of layout.

use rinch::prelude::*;

use crate::panes::{PaneId, MAX_PANES};
use crate::state::AppStore;

mod actions;
mod divider;
mod document;
mod pane;

pub(crate) use actions::{close_focused, focus_pane, open_beside, split_focused, split_pane};
pub(crate) use document::{pane_document, pane_icon, pane_spaced, pane_title};
use document::{editor_read_only, pane_access};
use divider::render_divider;
use pane::render_pane;

/// Whether the editor keeps a press of the pointer inside it to itself. The
/// browser's does (rinch-web consumes an in-editor `pointerdown` before any
/// ancestor's `onmousedown` hears of it, `rinch-web/src/editor_input.rs`),
/// so there a pane also takes the focus when the pointer is released in it;
/// the desktop's does not, and a release means nothing (a selection dragged
/// from one pane and let go over another stays where it began).
const EDITOR_KEEPS_ITS_PRESSES: bool = cfg!(not(feature = "native"));

thread_local! {
    /// Whether the press now held was heard by a pane or a divider. The
    /// release that follows it then decides nothing: the press already did,
    /// and what it did may have moved the focus on (a split's button focuses
    /// the new pane, and the pointer is still over the old one when it is
    /// let go). Only a press nothing here heard, which is one the browser's
    /// editor kept, leaves the release to say which pane was clicked.
    static PRESS_HEARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The press now bubbling up was inside an app component drawn in the
    /// text (a sheet's table): its inputs take the keyboard themselves.
    static PRESS_IN_VIEW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The same, for the release: the desktop's editor keeps its presses
    /// (a press in the text never reaches the pane), but not its releases.
    static RELEASE_IN_VIEW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A press inside something of the pane's with inputs of its own (a table in
/// the text, the node's fields): the pane it bubbles up to focuses but
/// leaves the keyboard where the press put it, or a press on one of those
/// inputs would hand the keyboard to the editor.
pub(crate) fn press_in_view() {
    PRESS_IN_VIEW.with(|p| p.set(true));
    RELEASE_IN_VIEW.with(|p| p.set(true));
}

/// Edit > "Compact Paragraphs" and the focused pane's note, kept as one
/// (docs/PARAGRAPHS_CONTRACT.md): the check follows the note (a focus change,
/// another window's or person's change), and a choice of the item writes the
/// note, or, with no note there or one this device may only read, the check
/// goes back to what the note says. Both effects live as long as the app.
fn follow_paragraphs(store: AppStore) {
    let _ = rinch::Effect::new(move || {
        let compact = !pane_spaced(store, store.focused_pane.get());
        if untracked(|| store.compact_paragraphs.get()) != compact {
            store.compact_paragraphs.set(compact);
        }
    });
    let _ = rinch::Effect::new(move || {
        let wanted = store.compact_paragraphs.get();
        untracked(|| {
            let pane = store.focused_pane.get();
            let compact = !pane_spaced(store, pane);
            if wanted == compact {
                return;
            }
            match pane_document(store, pane) {
                Some((store_id, node_id)) if store.pane(pane).show_editor.get() && !editor_read_only(store, pane) => {
                    store.send(crate::protocol::BackendCommand::SetNodeAppearance {
                        store_id,
                        node_id,
                        icon: None,
                        color: None,
                        background: None,
                        tags: None,
                        paragraphs: Some(!wanted),
                    });
                }
                _ => store.compact_paragraphs.set(compact),
            }
        });
    });
}

/// The area beside the explorer: the one toolbar, then every pane slot and
/// every divider slot, once. `hints` are the lines under the empty state's
/// sentence.
pub(crate) fn render_panes(__scope: &mut RenderScope, store: AppStore, hints: &[&'static str]) -> NodeHandle {
    follow_paragraphs(store);
    // The toolbar acts on the focused pane (docs/SPLIT_VIEW_CONTRACT.md "One
    // toolbar"), and has nothing to act on while that pane holds no document
    // or one this device may only read.
    // Which toolbar shows is `toolbar::which` (docs/TOOLBAR_CONTRACT.md).
    let usable = move || {
        let pane = store.focused_pane.get();
        store.pane(pane).show_editor.get() && !store.pane(pane).show_board.get() && !editor_read_only(store, pane)
    };
    let commentable = move || {
        let pane = store.focused_pane.get();
        store.pane(pane).show_editor.get() && !store.pane(pane).show_board.get() && pane_access(store, pane).allows_comment()
    };
    let toolbar = crate::toolbar::render_pimble_toolbar(__scope, store, usable, commentable);
    // The main toolbar shows the toolbar of what is selected in the focused
    // pane: one rule, `toolbar::which` (docs/TOOLBAR_CONTRACT.md).
    let sheet_toolbar = crate::sheets::toolbar::render_sheet_toolbar(__scope, store);
    let canvas_toolbar = crate::boards::render_canvas_toolbar(__scope, store);
    let shows = move |which: crate::toolbar::Which| if crate::toolbar::which(store) == which { "" } else { "display: none;" };
    // The keyboard follows the pane focus, however it moved (a press in the
    // pane, a split, a close, the View menu): the focused pane's editor when
    // it holds a document, no editor when it holds none. Only a change of the
    // focus moves it, so opening a note from the tree leaves the keyboard on
    // the tree.
    let _ = rinch::Effect::new(move || {
        let pane = store.focused_pane.get();
        untracked(|| crate::editor::give_keyboard_to(store, pane));
    });
    let slots: Vec<NodeHandle> = PaneId::ALL.into_iter().map(|pane| render_pane(__scope, store, pane, hints)).collect();
    let dividers: Vec<NodeHandle> = (0..MAX_PANES - 1).map(|index| render_divider(__scope, store, index)).collect();
    // The Properties panel (docs/PROPERTIES_CONTRACT.md), right of the
    // panes under the toolbar: it takes its width from the area, and the
    // panes keep theirs in percent of what is left.
    let properties = crate::properties::render_panel(__scope, store);
    rsx! {
        div {
            class: "pimble-panes-area",
            div {
                class: "pimble-editor__toolbar-wrap",
                div {
                    style: {move || shows(crate::toolbar::Which::Text)},
                    {toolbar}
                }
                div {
                    style: {move || shows(crate::toolbar::Which::Canvas)},
                    {canvas_toolbar}
                }
                div {
                    style: {move || shows(crate::toolbar::Which::Sheet)},
                    {sheet_toolbar}
                }
            }
            div {
                class: "pimble-panes-row",
                div {
                    class: "pimble-panes",
                    {slots}
                    {dividers}
                }
                {properties}
            }
        }
    }
}
