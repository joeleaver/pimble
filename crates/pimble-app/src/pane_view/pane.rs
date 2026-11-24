//! One pane slot as it is drawn: its title strip, the read-only sentence when
//! it applies, its editor and the empty state.

use rinch::prelude::*;
use rinch_tabler_icons::{render_tabler_icon, TablerIcon, TablerIconStyle};

use crate::appearance::IconGlyph;
use pimble_ui::presence::PersonChip;
use crate::panes::{Direction, PaneId};
use crate::rinch_editor::Editor;
use crate::state::AppStore;

use super::actions::{close_pane, focus_pane, split_pane};
use super::document::{editor_read_only, pane_access, pane_icon, pane_out_of_step, pane_people, pane_title};
use super::{press_in_view, EDITOR_KEEPS_ITS_PRESSES, PRESS_HEARD, PRESS_IN_VIEW, RELEASE_IN_VIEW};

/// One pane slot: its title strip, the read-only sentence when it applies,
/// its editor and the empty state. Rendered once per slot.
pub(super) fn render_pane(__scope: &mut RenderScope, store: AppStore, pane: PaneId, hints: &[&'static str]) -> NodeHandle {
    let state = store.pane(pane);

    // The rich text editor (the rinch `Editor {}` component over this pane's
    // `EditorHandle`). Collaboration is wired in `start_editing`: local edits
    // broadcast their deltas through the server relay and to the window's
    // other panes on the same note, and remote deltas arrive via
    // `BackendEvent::RemoteChanges`. There is no manual autosave — the collab
    // session persists edits live (the server applies and relays each delta).
    // Its children are the editor's overlay slot: the margin markers of the
    // discussions anchored in the text (docs/DISCUSSIONS_CONTRACT.md section
    // 6), placed where the editor says their lines are.
    let markers = crate::discussions::margin::watch(store, pane);
    let editor_view = rsx! {
        Editor {
            editor: crate::editor::editor(pane),
            for marker in markers() {
                div {
                    key: format!("{:.1} {:?} {} {:?}", marker.top, marker.ids, marker.count, marker.unread),
                    class: marker.class,
                    title: marker.title.clone(),
                    style: marker.style.clone(),
                    onmousedown: move || press_in_view(),
                    onclick: {
                        let (s, n) = (marker.store_id, marker.opens);
                        move || crate::discussions::open_beside(store, pane, s, n)
                    },
                    {marker.label.clone()}
                }
            }
        }
    };

    // The editor's own switch follows the read-only judgement
    // (`editor::set_read_only`): locked, it refuses every local change while
    // remote ones keep landing, so a reader's keystroke changes nothing on
    // screen. The effect lives as long as the app.
    let _ = rinch::Effect::new(move || crate::editor::set_read_only(pane, editor_read_only(store, pane)));
    // The people who can open the pane's note, for how its mentions look
    // (docs/NOTIFICATIONS_CONTRACT.md section 1). Lives as long as the app.
    let _ = rinch::Effect::new(move || crate::mentions::follow_members(store, pane));

    // A board node's view (docs/IMAGE_MARKUP_CONTRACT.md "Showing one"),
    // beside the editor and rendered once with it; shown instead of it.
    let board_view = crate::boards::render_board(__scope, store, pane);
    // A sheet node's tables (docs/SHEETS_CONTRACT.md section 1), likewise.
    let sheet_view = crate::sheets::render_sheet(__scope, store, pane);
    // A discussion node's thread (docs/DISCUSSIONS_CONTRACT.md), likewise.
    let discussion_view = crate::discussions::render_discussion(__scope, store, pane);
    // A metanode's outline (docs/METANODES_CONTRACT.md section 1.4), likewise.
    let metanode_view = crate::metanodes::render_metanode(__scope, store, pane);
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
    // The note's document theme (docs/THEMES_CONTRACT.md section 7): its
    // custom properties on the wrapper and its modifiers as the wrapper's
    // classes; `document_theme.css` does the rest. Default draws nothing.
    let drawn = rinch::core::reactive::Memo::new(move || crate::themes::pane_drawn(store, pane));
    let content_wrap = rsx! {
        div {
            class: {move || crate::themes::classes(crate::themes::WRAP_CLASS, &drawn.get())},
            // Hidden under a picture's board view while it is marked up.
            style: {move || {
                let shown = state.show_editor.get() && !state.show_board.get() && !state.show_sheet.get();
                if shown { drawn.get().style } else { "display: none;".to_string() }
            }},
            onscroll: move |_| {
                crate::text_boards::scrolled(store, pane);
                crate::presence::text::redraw_later(store, pane);
                // What this pane shows is said to whoever follows it, and a
                // scroll by hand stops it following anyone.
                crate::presence::moved(store, pane);
                crate::presence::follow::scrolled(pane);
                // Where it rests is where it opens next time.
                crate::places::moved(store, pane);
            },
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
    // The browser finds the text's box by this (rinch-web fills no bounds).
    content_wrap.set_attribute("data-pimble-text", &pane.index().to_string());

    // Other people's carets in the text (docs/PRESENCE_CONTRACT.md section
    // 4), in the window frame the editor answers `caret_rect` in.
    let carets = crate::presence::text::watch(pane);
    let caret_frame = crate::presence::text::frame(pane);
    let presence_carets = rsx! {
        div {
            class: "pimble-presence-carets",
            // The text's box, clipping the carets to it.
            style: {move || caret_frame.get()},
            for caret in carets.get() {
                div {
                    // A keyed row is drawn once: everything it shows is in its key.
                    key: format!("{} {} {} {} {}", caret.session, caret.class, caret.style, caret.label, caret.title),
                    class: caret.class,
                    style: caret.style.clone(),
                    span {
                        class: if caret.agent { "pimble-presence-caret__flag pimble-presence-caret__flag--agent" } else { "pimble-presence-caret__flag" },
                        style: caret.flag_style.clone(),
                        title: format!("{}{}", caret.title, crate::presence::follow_hint(caret.agent)),
                        onclick: {
                            let key = caret.key.clone();
                            let agent = caret.agent;
                            // An agent is never followed, so its flag offers nothing.
                            move || if !agent { crate::presence::follow::start(store, pane, &key) }
                        },
                        // An agent's flag carries a robot (docs/PRESENCE_CONTRACT.md section 8).
                        span {
                            class: "pimble-presence-robot",
                            style: if caret.agent { "" } else { "display: none;" },
                            IconGlyph { name: "robot".to_string() }
                        }
                        {caret.label.clone()}
                    }
                }
            }
        }
    };

    let empty_icon = render_tabler_icon(__scope, TablerIcon::FileText, TablerIconStyle::Outline);
    let hint_lines: Vec<NodeHandle> = hints
        .iter()
        .map(|hint| rsx! { div { class: "pimble-empty-state__hint", {hint.to_string()} } })
        .collect();

    let full = move || !store.tiling.with(|tiling| tiling.can_split());

    rsx! {
        div {
            class: {move || {
                // The focus strip says which pane the toolbar acts on, so it
                // is drawn only when there is more than one.
                let split = store.tiling.with(|tiling| tiling.len() > 1);
                if !split {
                    "pimble-pane pimble-pane--alone"
                } else if store.focused_pane.get() == pane {
                    "pimble-pane pimble-pane--focused"
                } else {
                    "pimble-pane"
                }
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
            } + &match crate::presence::follow::border_color(pane) {
                // Following someone: the pane's edge in their color.
                Some(color) => format!(" box-shadow: inset 0 0 0 2px {color};"),
                None => String::new(),
            }},
            // A press anywhere in the pane focuses it, and gives it the
            // keyboard even when it already had the focus (a press on its
            // title strip after the keyboard went to the tree).
            onmousedown: move || {
                PRESS_HEARD.with(|heard| heard.set(true));
                // A click in the pane takes it back from whoever it follows
                // (a click on someone's initials then follows them).
                crate::presence::follow::stop(pane);
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
                    crate::presence::follow::stop(pane);
                    focus_pane(store, pane);
                }
                // A click outside a table in the text ends its cell: the
                // toolbar follows the click (docs/TOOLBAR_CONTRACT.md).
                if !RELEASE_IN_VIEW.with(|p| p.replace(false)) {
                    crate::sheets::pressed_outside(store, pane);
                }
                // A click on a board embedded in the text opens it; a
                // double-click on a picture marks it up.
                crate::text_boards::pane_mouseup(store, pane);
                // Ctrl/Cmd+click on a hashtag searches for it.
                crate::hashtags::clicked(store, pane);
            },

            div {
                class: "pimble-pane__title",
                // The focus pad: a short accent mark centered on the focused
                // pane's top edge, drawn only when the view is split.
                div { class: "pimble-pane__focus" }
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
                // Who else has this note open (docs/PRESENCE_CONTRACT.md
                // section 6): up to four, then how many more.
                span {
                    class: "pimble-pane__people",
                    for person in pane_people(store, pane).0 {
                        PersonChip {
                            key: person.key.clone(),
                            person: person.clone(),
                            onfollow: move |key: String| crate::presence::follow::start(store, pane, &key),
                        }
                    }
                    span {
                        class: "pimble-pane__more",
                        style: {move || if pane_people(store, pane).1 > 0 { "" } else { "display: none;" }},
                        {move || pimble_ui::presence::more_label(pane_people(store, pane).1)}
                    }
                }
                // Following someone (docs/PRESENCE_CONTRACT.md section 7):
                // "Following Sam · Stop", or "Sam left" for a moment.
                span {
                    class: "pimble-pane__follow",
                    style: {move || if crate::presence::follow::strip_text(pane).is_some() { "" } else { "display: none;" }},
                    span { {move || crate::presence::follow::strip_text(pane).unwrap_or_default()} }
                    span {
                        class: "pimble-pane__follow-stop",
                        style: {move || if crate::presence::follow::border_color(pane).is_some() { "" } else { "display: none;" }},
                        onclick: move || crate::presence::follow::stop(pane),
                        " · Stop"
                    }
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

            // A note out of step on this device says so at the top of the
            // pane that shows it, text, canvas or sheet alike
            // (docs/CONVERGENCE_CONTRACT.md section 4).
            div {
                class: "pimble-pane__out-of-step",
                style: {move || if pane_out_of_step(store, pane).is_some() { "" } else { "display: none;" }},
                {move || pane_out_of_step(store, pane).map(|entry| crate::out_of_step::notice(&entry)).unwrap_or_default()}
            }
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
            {metanode_view}
            {discussions_panel}
            {picture_controls}
            {presence_carets}
            div {
                class: "pimble-empty-state",
                style: {move || if state.show_editor.get() || state.show_board.get() || state.show_sheet.get() || state.show_discussion.get() || state.show_metanode.get() { "display: none;" } else { "" }},
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
