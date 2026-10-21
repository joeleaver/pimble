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
use rinch_tabler_icons::{render_tabler_icon, TablerIcon, TablerIconStyle};

use crate::appearance::{display_color, IconGlyph};
use crate::editor::stop_editing;
use crate::panes::{Direction, PaneId, MAX_PANES};
use crate::rinch_editor::Editor;
use crate::state::{display_label_from_node, parse_tree_value, AppStore};

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
}

/// A press inside something of the pane's with inputs of its own (a table in
/// the text, the node's fields): the pane it bubbles up to focuses but
/// leaves the keyboard where the press put it, or a press on one of those
/// inputs would hand the keyboard to the editor.
pub(crate) fn press_in_view() {
    PRESS_IN_VIEW.with(|p| p.set(true));
}

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

/// View > "Split Right" / "Split Down": the focused pane.
pub(crate) fn split_focused(store: AppStore, direction: Direction) {
    split_pane(store, store.focused(), direction);
}

/// View > "Close Pane": the focused pane.
pub(crate) fn close_focused(store: AppStore) {
    close_pane(store, store.focused());
}

/// The node pane `pane` shows as a document, tracked: its canonical pair.
fn pane_document(store: AppStore, pane: PaneId) -> Option<(pimble_core::StoreId, pimble_core::NodeId)> {
    let state = store.pane(pane);
    if !state.show_editor.get() && !state.show_board.get() && !state.show_sheet.get() && !state.show_discussion.get() {
        return None;
    }
    match parse_tree_value(&state.selected.get()?) {
        Some((store_id, Some(node_id))) => Some((store_id, node_id)),
        _ => None,
    }
}

/// The title in pane `pane`'s strip, tracked: the label its tree row shows,
/// typing included.
fn pane_title(store: AppStore, pane: PaneId) -> String {
    let Some((store_id, node_id)) = pane_document(store, pane) else { return String::new() };
    if let Some(live) = store.live_label_signal((store_id, node_id)).get() {
        return live;
    }
    // One signal read at a time: a tracked read inside another's `with`
    // panics (see `editor_read_only` below).
    let node = store.node_data.with(|map| map.get(&(store_id, node_id)).copied());
    node.map(|sig| sig.with(display_label_from_node)).unwrap_or_else(|| "Untitled".to_string())
}

/// The icon in pane `pane`'s strip and its colour, tracked: the node's own
/// when it has them (docs "Tree appearance"), a document's otherwise. Empty
/// for a pane with no document.
fn pane_icon(store: AppStore, pane: PaneId) -> Vec<(String, String)> {
    let Some((store_id, node_id)) = pane_document(store, pane) else { return Vec::new() };
    let node = store.node_data.with(|map| map.get(&(store_id, node_id)).copied());
    let (icon, color) = node
        .map(|sig| sig.with(|n| (n.metadata.icon().map(str::to_string), n.metadata.color().map(str::to_string))))
        .unwrap_or_default();
    let dark = store.dark_mode.get();
    let fallback = if store.pane(pane).show_discussion.get() {
        TablerIcon::Messages
    } else if store.pane(pane).show_sheet.get() {
        TablerIcon::Table
    } else if store.pane(pane).show_board.get() && !store.pane(pane).markup.get() {
        TablerIcon::Chalkboard
    } else {
        TablerIcon::FileText
    };
    vec![(
        icon.unwrap_or_else(|| fallback.name().to_string()),
        color.map(|hex| display_color(&hex, dark)).unwrap_or_default(),
    )]
}

/// Whether the document in pane `pane` is one this device may only read
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"): its store is held to
/// read, or the server judged this node so (a reader's root in a store where
/// other roots are edited) — `AppStore::node_access`, read here with
/// tracking. The pane's `active_edit` holds the node's CANONICAL pair — where
/// the edit would be written, even when the node was reached through a mount
/// — so that is what decides. Reactive on the pane's node, on its store's
/// own signal and on the node's, so the pane follows an access that arrives
/// or changes. The pane shows a line saying so, the toolbar greys out while
/// the pane is focused, and the editor is locked so typing does nothing, in
/// every pane that holds the document.
///
/// The store's own signal comes out of the registry first and is read after
/// that borrow is released: rinch keeps every signal in one `RefCell`, and a
/// *tracked* read takes it mutably to record the subscription — so reading
/// one signal inside another's `with` panics. (`AppStore`'s own helpers nest
/// freely because they are `untracked`.)
fn editor_read_only(store: AppStore, pane: PaneId) -> bool {
    !pane_access(store, pane).allows_write()
}

/// What this device may change of the pane's document: its store's access,
/// narrowed by the node's own.
fn pane_access(store: AppStore, pane: PaneId) -> pimble_core::StoreAccess {
    let state = store.pane(pane);
    let Some(active) = state.active_edit.get().or_else(|| state.board.get()).or_else(|| state.sheet.get()).or_else(|| state.discussion.get()) else {
        return pimble_core::StoreAccess::Full;
    };
    let sig = store.store_data.with(|map| map.get(&active.store_id).copied());
    let node_sig = store.node_data.with(|map| map.get(&(active.store_id, active.node_id)).copied());
    let node = node_sig.map_or(pimble_core::StoreAccess::Full, |sig| sig.with(|n| n.access));
    // A discussion node answers for itself: the store's `Comment` lets its
    // commenter write it.
    if !node.allows_write() || node_sig.is_some_and(|sig| sig.with(|n| n.node_type == pimble_core::node_types::DISCUSSION)) {
        return node;
    }
    sig.map_or(pimble_core::StoreAccess::Full, |sig| sig.with(|s| s.access))
}

/// One pane slot: its title strip, the read-only sentence when it applies,
/// its editor and the empty state. Rendered once per slot.
fn render_pane(__scope: &mut RenderScope, store: AppStore, pane: PaneId, hints: &[&'static str]) -> NodeHandle {
    let state = store.pane(pane);

    // The rich text editor (the rinch `Editor {}` component over this pane's
    // `EditorHandle`). Collaboration is wired in `start_editing`: local edits
    // broadcast their deltas through the server relay and to the window's
    // other panes on the same note, and remote deltas arrive via
    // `BackendEvent::RemoteChanges`. There is no manual autosave — the collab
    // session persists edits live (the server applies and relays each delta).
    let editor_view = rsx! {
        Editor { editor: crate::editor::editor(pane) }
    };

    // The editor's own switch follows the read-only judgement
    // (`editor::set_read_only`): locked, it refuses every local change while
    // remote ones keep landing, so a reader's keystroke changes nothing on
    // screen. The effect lives as long as the app.
    let _ = rinch::Effect::new(move || crate::editor::set_read_only(pane, editor_read_only(store, pane)));

    // A board node's view (docs/IMAGE_MARKUP_CONTRACT.md "Showing one"),
    // beside the editor and rendered once with it; shown instead of it.
    let board_view = crate::boards::render_board(__scope, store, pane);
    // A sheet node's tables (docs/SHEETS_CONTRACT.md section 1), likewise.
    let sheet_view = crate::sheets::render_sheet(__scope, store, pane);
    // A discussion node's thread (docs/DISCUSSIONS_CONTRACT.md), likewise.
    let discussion_view = crate::discussions::render_discussion(__scope, store, pane);
    // The discussions on the pane's node, under its content: in the text's
    // scroll for a note, under the view for a board or a sheet.
    let discussions_in_text = crate::discussions::render_host_panel(__scope, store, pane, true);
    let discussions_panel = crate::discussions::render_host_panel(__scope, store, pane, false);
    // The floating controls over the picture under the pointer in the text
    // (docs/IMAGE_MARKUP_CONTRACT.md section 5).
    let picture_controls = crate::text_boards::render_controls(__scope, store, pane);

    // The note's tables no atom shows any more (docs/SHEETS_CONTRACT.md
    // section 1), under its text.
    let lost_tables = crate::sheets::in_text::render_lost(__scope, store, pane);
    let fields_band = crate::fields::render_fields(__scope, store, pane);
    let content_wrap = rsx! {
        div {
            class: "pimble-editor__content-wrap",
            // Hidden under a picture's board view while it is marked up.
            style: {move || if state.show_editor.get() && !state.show_board.get() && !state.show_sheet.get() { "" } else { "display: none;" }},
            onscroll: move |_| crate::text_boards::scrolled(store, pane),
            // A board node's row dropped on the text goes in as a picture
            // of it (docs/IMAGE_MARKUP_CONTRACT.md section 2).
            ondrop: move || {
                crate::boards::take_tree_drag(|value| crate::text_boards::drop_tree_row(store, pane, value));
            },
            {editor_view}
            {lost_tables}
            {discussions_in_text}
        }
    };
    // Where the text is on screen: the controls over a picture stay inside
    // it, and a board in text is drawn no wider than it.
    crate::text_boards::set_text_bounds(pane, content_wrap.bounds_signal());

    let empty_icon = render_tabler_icon(__scope, TablerIcon::FileText, TablerIconStyle::Outline);
    let hint_lines: Vec<NodeHandle> = hints
        .iter()
        .map(|hint| rsx! { div { class: "pimble-empty-state__hint", {hint.to_string()} } })
        .collect();

    let full = move || !store.tiling.with(|tiling| tiling.can_split());

    rsx! {
        div {
            class: {move || {
                if store.focused_pane.get() == pane { "pimble-pane pimble-pane--focused" } else { "pimble-pane" }
            }},
            // Where the tiling puts it, in percent of the area; hidden when
            // the tiling does not show it. A pane right of or below another
            // draws the line between them.
            style: {move || match store.tiling.with(|tiling| tiling.rect_of(pane)) {
                Some(rect) => format!(
                    "left: {:.4}%; top: {:.4}%; width: {:.4}%; height: {:.4}%;{}{}",
                    rect.x * 100.0,
                    rect.y * 100.0,
                    rect.w * 100.0,
                    rect.h * 100.0,
                    if rect.x > 0.0 { " border-left: 1px solid var(--rinch-color-border);" } else { "" },
                    if rect.y > 0.0 { " border-top: 1px solid var(--rinch-color-border);" } else { "" },
                ),
                None => "display: none;".to_string(),
            }},
            // A press anywhere in the pane focuses it, and gives it the
            // keyboard even when it already had the focus (a press on its
            // title strip after the keyboard went to the tree).
            onmousedown: move || {
                PRESS_HEARD.with(|heard| heard.set(true));
                // A press outside a discussion's message boxes ends typing there.
                crate::discussions::pane_pressed();
                focus_pane(store, pane);
                if !PRESS_IN_VIEW.with(|p| p.replace(false)) {
                    crate::editor::give_keyboard_to(store, pane);
                }
            },
            onmouseup: move || {
                let heard = PRESS_HEARD.with(|heard| heard.replace(false));
                if EDITOR_KEEPS_ITS_PRESSES && !heard {
                    focus_pane(store, pane);
                }
                // A click on a board embedded in the text opens it; a
                // double-click on a picture marks it up.
                crate::text_boards::pane_mouseup(store, pane);
            },

            div {
                class: "pimble-pane__title",
                span {
                    class: "pimble-pane__icon",
                    for (name, color) in pane_icon(store, pane) {
                        span {
                            key: format!("{name} {color}"),
                            class: "pimble-pane__icon-glyph",
                            style: if color.is_empty() { String::new() } else { format!("color: {color};") },
                            IconGlyph { name: name.clone() }
                        }
                    }
                }
                span {
                    class: "pimble-pane__name",
                    {move || pane_title(store, pane)}
                }
                span {
                    title: "Split Right",
                    ActionIcon {
                        icon: TablerIcon::LayoutColumns,
                        variant: "subtle",
                        size: "xs",
                        disabled: {move || full()},
                        onclick: move || { split_pane(store, pane, Direction::Right); },
                    }
                }
                span {
                    title: "Split Down",
                    ActionIcon {
                        icon: TablerIcon::LayoutRows,
                        variant: "subtle",
                        size: "xs",
                        disabled: {move || full()},
                        onclick: move || { split_pane(store, pane, Direction::Down); },
                    }
                }
                span {
                    title: "Close Pane",
                    ActionIcon {
                        icon: TablerIcon::X,
                        variant: "subtle",
                        size: "xs",
                        onclick: move || close_pane(store, pane),
                    }
                }
            }

            // The node's own fields (docs/SHEETS_CONTRACT.md section 8).
            {fields_band}
            // The same sentence a refused write comes back with, so a
            // reader meets one wording everywhere.
            div {
                class: "pimble-editor__read-only",
                // A discussion says it in its own place, where its message box would be.
                style: {move || if (state.show_editor.get() || state.show_board.get() || state.show_sheet.get()) && !state.show_discussion.get() && editor_read_only(store, pane) { "" } else { "display: none;" }},
                {move || pane_access(store, pane).refusal().to_string()}
            }
            {content_wrap}
            {board_view}
            {sheet_view}
            {discussion_view}
            {discussions_panel}
            {picture_controls}
            div {
                class: "pimble-empty-state",
                style: {move || if state.show_editor.get() || state.show_board.get() || state.show_sheet.get() || state.show_discussion.get() { "display: none;" } else { "" }},
                div {
                    class: "pimble-empty-state__icon",
                    {empty_icon}
                }
                div {
                    class: "pimble-empty-state__text",
                    "Select a document to start editing"
                }
                {hint_lines}
            }
        }
    }
}

/// One divider slot: the line of split `index`, when the tiling has that
/// many splits.
///
/// The element that hears the press covers the whole space its split
/// divides and lets every press through but the ones on the line inside it,
/// so the press's element bounds are that space in pixels on both backends:
/// the drag reads its ratio straight off them, and the area's size (for the
/// least a pane may be) follows from the space's share of it.
fn render_divider(__scope: &mut RenderScope, store: AppStore, index: usize) -> NodeHandle {
    let divider = move || store.tiling.with(|tiling| tiling.dividers().into_iter().find(|d| d.index == index));
    let ratio = move |direction: Direction, space: crate::panes::Rect, line: crate::panes::Rect| match direction {
        Direction::Right => (line.x - space.x) / space.w,
        Direction::Down => (line.y - space.y) / space.h,
    };
    rsx! {
        div {
            class: "pimble-pane-split",
            style: {move || match divider() {
                Some(d) => format!(
                    "left: {:.4}%; top: {:.4}%; width: {:.4}%; height: {:.4}%;",
                    d.space.x * 100.0,
                    d.space.y * 100.0,
                    d.space.w * 100.0,
                    d.space.h * 100.0,
                ),
                None => "display: none;".to_string(),
            }},
            onmousedown: move || {
                PRESS_HEARD.with(|heard| heard.set(true));
                let Some(d) = untracked(divider) else { return };
                let ctx = rinch::core::get_click_context();
                if ctx.element_width <= 0.0 || ctx.element_height <= 0.0 {
                    return;
                }
                let (area_width, area_height) = (ctx.element_width / d.space.w, ctx.element_height / d.space.h);
                // Where on the line it was grabbed: the line does not jump
                // to the pointer.
                let start = ratio(d.direction, d.space, d.rect);
                let grabbed = match d.direction {
                    Direction::Right => (ctx.mouse_x - ctx.element_x) / ctx.element_width,
                    Direction::Down => (ctx.mouse_y - ctx.element_y) / ctx.element_height,
                } - start;
                let direction = d.direction;
                let drag_to = move |px: f32, py: f32| {
                    let at = match direction {
                        Direction::Right => px,
                        Direction::Down => py,
                    } - grabbed;
                    let mut tiling = untracked(|| store.tiling.get());
                    if tiling.drag_ratio(index, at, area_width, area_height) && tiling != untracked(|| store.tiling.get()) {
                        store.tiling.set(tiling);
                    }
                };
                // The layout is saved when the drag ends, not on every move
                // (docs/SPLIT_VIEW_CONTRACT.md "Persistence").
                store.pane_dragging.set(true);
                rinch::core::Drag::percent()
                    .on_move(drag_to)
                    // The last move left the line where it was let go; the
                    // release itself says nothing more (rinch hands `on_end`
                    // of a percent drag the raw pointer position, not a
                    // fraction: rinch-core/src/events/drag.rs `finish_drag`).
                    .on_end(move |_, _| store.pane_dragging.set(false))
                    .on_cancel(move |_, _| store.pane_dragging.set(false))
                    .start();
            },
            div {
                class: {move || match divider().map(|d| d.direction) {
                    Some(Direction::Down) => "pimble-pane-divider pimble-pane-divider--down",
                    _ => "pimble-pane-divider pimble-pane-divider--right",
                }},
                style: {move || match divider() {
                    Some(d) => {
                        let at = ratio(d.direction, d.space, d.rect) * 100.0;
                        match d.direction {
                            Direction::Right => format!("left: {at:.4}%;"),
                            Direction::Down => format!("top: {at:.4}%;"),
                        }
                    }
                    None => String::new(),
                }},
            }
        }
    }
}

/// The area beside the explorer: the one toolbar, then every pane slot and
/// every divider slot, once. `hints` are the lines under the empty state's
/// sentence.
pub(crate) fn render_panes(__scope: &mut RenderScope, store: AppStore, hints: &[&'static str]) -> NodeHandle {
    // The toolbar acts on the focused pane (docs/SPLIT_VIEW_CONTRACT.md "One
    // toolbar"), and has nothing to act on while that pane holds no document
    // or one this device may only read.
    let usable = move || {
        let pane = store.focused_pane.get();
        store.pane(pane).show_editor.get() && !editor_read_only(store, pane)
    };
    let toolbar = crate::toolbar::render_pimble_toolbar(__scope, store, usable);
    // The board tools take the toolbar's place while the focused pane shows a
    // board (docs/IMAGE_MARKUP_CONTRACT.md section 1).
    let board_toolbar = crate::boards::render_board_toolbar(__scope, store);
    let board_focused = move || store.pane(store.focused_pane.get()).show_board.get();
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
    rsx! {
        div {
            class: "pimble-panes-area",
            div {
                class: "pimble-editor__toolbar-wrap",
                div {
                    style: {move || if board_focused() { "display: none;" } else { "" }},
                    {toolbar}
                }
                div {
                    style: {move || if board_focused() { "" } else { "display: none;" }},
                    {board_toolbar}
                }
            }
            div {
                class: "pimble-panes",
                {slots}
                {dividers}
            }
        }
    }
}
