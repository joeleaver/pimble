//! The page around the explorer and the panes: the sidebar, the link
//! picker's popup and the status bar.

use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::persistence::save_sidebar_width;
use crate::state::AppStore;
use crate::state::{SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH};
use super::account::{has_account_action, open_account_modal, run_account_action};
use super::{CAN_ADMINISTER_STORES, CAN_CREATE_HOSTED_STORES};
use super::navigation::step_history;
use super::status::{cloud_part_now, server_part, server_part_name};
use super::store_modals::open_new_store_modal;

/// "Restart to update" (docs/UPDATE_CONTRACT.md). The browser has nothing to
/// update; its status bar never shows the control.
fn restart_to_update(store: AppStore) {
    #[cfg(feature = "native")]
    crate::update::restart_to_update(store);
    #[cfg(not(feature = "native"))]
    let _ = store;
}

/// The link picker's lines (a bare call for the rsx `for`; see
/// `search_rows` in `search.rs`).
fn picker_lines(store: AppStore) -> Vec<crate::link_picker::PickerLine> {
    store.link_picker.with(|view| view.as_ref().map(crate::link_picker::lines).unwrap_or_default())
}

/// Whether the open picker shows its own query line (Ctrl+L: the query is
/// typed into the popup, not the note).
fn picker_is_field(store: AppStore) -> bool {
    store.link_picker.with(|view| {
        matches!(
            view.as_ref().map(|v| v.kind),
            Some(crate::link_picker::PickerKind::Selection { .. } | crate::link_picker::PickerKind::Sheet { .. })
        )
    })
}

/// The Ctrl+L picker's query line.
fn picker_query_text(store: AppStore) -> String {
    store.link_picker.with(|view| match view {
        Some(view) if view.query.is_empty() && matches!(view.kind, crate::link_picker::PickerKind::Sheet { .. }) => "Sheet…".to_string(),
        Some(view) if view.query.is_empty() => "Link to…".to_string(),
        Some(view) => view.query.clone(),
        None => String::new(),
    })
}

/// What the open picker says with no rows; empty when it has some.
fn picker_empty_text(store: AppStore) -> String {
    store.link_picker.with(|view| view.as_ref().map(|view| crate::link_picker::quiet_line(view).to_string()).unwrap_or_default())
}

/// What the page around the explorer is given: the pieces built before it.
pub(super) struct Parts {
    pub(super) tree_scroll: NodeHandle,
    pub(super) search_bar: NodeHandle,
    pub(super) search_panel: NodeHandle,
    pub(super) panes_view: NodeHandle,
}

/// The page itself: the sidebar (search, explorer, its "+"), the panes and
/// the status bar.
pub(super) fn render_body(
    __scope: &mut RenderScope,
    store: AppStore,
    tree_state: UseTreeReturn,
    cancel_rename: impl Fn(bool) + Clone + 'static,
    parts: Parts,
) -> NodeHandle {
    let Parts { tree_scroll, search_bar, search_panel, panes_view } = parts;
    // The explorer's "+" (docs/MENUS_CONTRACT.md section 3.2): the list of
    // what can be made (`crate::creating`), made beside the selected row.
    // With no store open there is nowhere to make one: on the desktop
    // "New Store..." is a menu item away; a browser has no menu bar, so
    // "+" offers the store instead of an empty list.
    let on_new_node = move || {
        let no_store = untracked(|| store.store_ids.with(|ids| ids.is_empty()));
        if no_store {
            if CAN_CREATE_HOSTED_STORES {
                open_new_store_modal(store);
            }
            return;
        }
        store.new_menu_open.update(|open| *open = !*open);
    };

    // The page itself, with no window chrome around it: identical on both
    // targets. Only the frame differs — a borderless native window that
    // draws its own titlebar, or the browser's viewport.
    rsx! {
        // Outer wrapper — flex column fills the content area, pushes
        // status bar to the very bottom.
        div {
                style: "display: flex; flex-direction: column; height: 100%;",

                // ── Main content ──────────────────────────────
                div {
                    style: "display: flex; flex: 1; overflow: hidden;",

                    // ── Sidebar ───────────────────────────────
                    div {
                        class: "pimble-sidebar",
                        style: {move || format!("width: {}px;", store.sidebar_width.get())},

                        // Search heads the explorer, whose column its
                        // results take over (Joe, 2026-10-20: across the
                        // whole window it cost the panes a row).
                        {search_bar}

                        // Header: store name + new-node button
                        div {
                            class: "pimble-sidebar__header",

                            span {
                                class: "pimble-sidebar__heading",
                                "EXPLORER"
                            }

                            // Back and forward through what the focused
                            // pane has shown (this device only, crate::nav).
                            div {
                                title: "Back (Alt+Left)",
                                style: {move || if store.pane(store.focused_pane.get()).history.with(|h| h.can_go_back()) {
                                    ""
                                } else {
                                    "opacity: 0.35; pointer-events: none;"
                                }},
                                ActionIcon {
                                    icon: TablerIcon::ArrowLeft,
                                    variant: "subtle",
                                    size: "xs",
                                    onclick: move || step_history(store, tree_state, true),
                                }
                            }
                            div {
                                title: "Forward (Alt+Right)",
                                style: {move || if store.pane(store.focused_pane.get()).history.with(|h| h.can_go_forward()) {
                                    ""
                                } else {
                                    "opacity: 0.35; pointer-events: none;"
                                }},
                                ActionIcon {
                                    icon: TablerIcon::ArrowRight,
                                    variant: "subtle",
                                    size: "xs",
                                    onclick: move || step_history(store, tree_state, false),
                                }
                            }

                            // Only the browser build registers this: it is
                            // the way back to the account pages, which the
                            // desktop reaches through its menu bar instead.
                            if has_account_action() {
                                div {
                                    title: "Account",
                                    style: "display: inline-flex;",
                                    ActionIcon {
                                        icon: TablerIcon::User,
                                        variant: "subtle",
                                        size: "xs",
                                        onclick: move || run_account_action(),
                                    }
                                }
                            }

                            DropdownMenu {
                                opened_fn: move || store.new_menu_open.get(),
                                on_close: move || store.new_menu_open.set(false),
                                close_on_click_outside: true,
                                close_on_item_click: true,
                                position: "bottom-end",
                                DropdownMenuTarget {
                                    div {
                                        title: "New",
                                        style: "display: inline-flex;",
                                        ActionIcon {
                                            icon: TablerIcon::Plus,
                                            variant: "subtle",
                                            size: "xs",
                                            onclick: on_new_node,
                                        }
                                    }
                                }
                                DropdownMenuDropdown {
                                    for item in crate::creating::kinds(store) {
                                        DropdownMenuItem {
                                            key: item.label.clone(),
                                            left_section: item.icon,
                                            onclick: {
                                                let kind = item.kind.clone();
                                                move || {
                                                    store.new_menu_open.set(false);
                                                    crate::creating::make_from_plus(store, &kind);
                                                }
                                            },
                                            {item.label.clone()}
                                        }
                                    }
                                }
                            }
                        }

                        div {
                            class: "pimble-sidebar__pane",
                            style: {|| if store.search_query.get().is_empty() { "" } else { "display: none;" }},
                            {tree_scroll}
                        }
                        div {
                            class: "pimble-sidebar__pane",
                            style: {|| if store.search_query.get().is_empty() { "display: none;" } else { "" }},
                            {search_panel}
                        }

                        // Where a dragged row lands, in words, for the
                        // whole drag (docs/TREE_CONTRACT.md "Feedback").
                        div {
                            class: {move || if store.drop_hint.with(|h| h.as_ref().is_some_and(|h| h.refused)) {
                                "pimble-drop-hint pimble-drop-hint--refused"
                            } else {
                                "pimble-drop-hint"
                            }},
                            style: {move || if store.drop_hint.with(|h| h.is_some()) { "" } else { "display: none;" }},
                            div {
                                class: "pimble-drop-hint__line",
                                {move || store.drop_hint.with(|h| h.as_ref().map(|h| h.line.clone()).unwrap_or_default())}
                            }
                            div {
                                class: "pimble-drop-hint__warning",
                                style: {move || if store.drop_hint.with(|h| h.as_ref().is_some_and(|h| h.warning.is_some())) { "" } else { "display: none;" }},
                                {move || store.drop_hint.with(|h| h.as_ref().and_then(|h| h.warning.clone()).unwrap_or_default())}
                            }
                        }
                    }

                    // The explorer's right edge, dragged to resize it.
                    div {
                        class: "pimble-sidebar__resizer",
                        onmousedown: move || {
                            let start_width = untracked(|| store.sidebar_width.get());
                            let start_x = rinch::core::get_click_context().mouse_x;
                            let width_at = move |x: f32| (start_width + x - start_x).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                            rinch::core::Drag::absolute()
                                .on_move(move |x, _| store.sidebar_width.set(width_at(x)))
                                .on_end(move |x, _| save_sidebar_width(width_at(x)))
                                .start();
                        },
                    }

                    // ── Panes ─────────────────────────────────
                    div {
                        class: "pimble-editor",
                        onclick: move || cancel_rename(true),
                        {panes_view}
                    }
                }

                // ── Status bar ────────────────────────────────
                div {
                    class: "pimble-status-bar",

                    // Pimble's server on this computer (in the browser,
                    // Pimble Cloud itself, which is what it connects to).
                    div {
                        class: "pimble-status-bar__part",
                        title: {move || server_part(&store.connection_status.get(), &store.server_addr.get()).tooltip},
                        span {
                            class: {move || server_part(&store.connection_status.get(), &store.server_addr.get()).dot_class()},
                            style: {move || if server_part(&store.connection_status.get(), &store.server_addr.get()).spinning() { "display: none;" } else { "" }},
                        }
                        span {
                            class: "pimble-status-bar__spinner",
                            style: {move || if server_part(&store.connection_status.get(), &store.server_addr.get()).spinning() { "" } else { "display: none;" }},
                            Loader { size: "xs" }
                        }
                        span { class: "pimble-status-bar__name", {server_part_name()} }
                        span { class: "pimble-status-bar__detail", {move || server_part(&store.connection_status.get(), &store.server_addr.get()).detail} }
                    }

                    // The signed-in account and how this computer's
                    // connections to Pimble Cloud are doing; a click opens
                    // the Account modal (docs/DESKTOP_ACCOUNT_CONTRACT.md
                    // decision 7). Only a build whose server keeps an
                    // account: the browser signs in through its own pages.
                    if CAN_ADMINISTER_STORES {
                        span { class: "pimble-status-bar__rule" }
                    }
                    if CAN_ADMINISTER_STORES {
                        div {
                            class: "pimble-status-bar__part pimble-status-bar__part--button",
                            title: {move || cloud_part_now(store).tooltip},
                            onclick: move || open_account_modal(store, ""),
                            span {
                                class: {move || cloud_part_now(store).dot_class()},
                                style: {move || if cloud_part_now(store).spinning() { "display: none;" } else { "" }},
                            }
                            span {
                                class: "pimble-status-bar__spinner",
                                style: {move || if cloud_part_now(store).spinning() { "" } else { "display: none;" }},
                                Loader { size: "xs" }
                            }
                            span { class: "pimble-status-bar__name", "Pimble Cloud" }
                            span { class: "pimble-status-bar__detail", {move || cloud_part_now(store).detail} }
                        }
                    }

                    // A refusal the server sent back, in its own words
                    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
                    // It is not a connection failure, so it does not touch
                    // the badge beside it.
                    span {
                        class: "pimble-status-bar__notice",
                        style: {|| if store.notice.get().is_empty() { "display: none;" } else { "" }},
                        {|| store.notice.get()}
                    }

                    // The canvas tool in hand and what it does, while the
                    // toolbar has nothing for it (docs/TOOLBAR_CONTRACT.md
                    // "No floating bar").
                    span {
                        class: "pimble-status-bar__hint",
                        style: {move || if crate::boards::status_hint(store).is_empty() { "display: none;" } else { "" }},
                        {move || crate::boards::status_hint(store)}
                    }

                    div { style: "flex: 1;", }

                    // Notices not opened yet (docs/NOTIFICATIONS_CONTRACT.md
                    // section 5); a click opens the Inbox.
                    span {
                        class: "pimble-status-bar__inbox",
                        title: "Notices you have not opened. Click to open the Inbox.",
                        style: {move || if crate::inbox::unread_count(store) == 0 { "display: none;" } else { "" }},
                        onclick: move || crate::inbox::open_inbox(store),
                        {move || crate::inbox::unread_status(store).unwrap_or_default()}
                    }

                    // The window's zoom while it is not actual size
                    // (docs/ZOOM_CONTRACT.md); a click goes back to it.
                    span {
                        class: "pimble-status-bar__zoom",
                        title: "Zoom. Click for actual size (Ctrl+0).",
                        style: {|| if crate::zoom::is_actual(crate::zoom::current()) { "display: none;" } else { "" }},
                        onclick: move || crate::zoom::actual_size(store),
                        {|| crate::zoom::label(crate::zoom::current())}
                    }

                    // A verified update is staged (docs/UPDATE_CONTRACT.md);
                    // nothing restarts until this is clicked.
                    if CAN_ADMINISTER_STORES {
                        span {
                            class: "pimble-status-bar__update",
                            style: {|| if store.update_ready.get().is_some() { "" } else { "display: none;" }},
                            onclick: move || restart_to_update(store),
                            {|| store.update_ready.get().map(|v| format!("Restart to update to {v}")).unwrap_or_default()}
                        }
                    }
                }

                // Where the hovered link leads (docs/LINKS_CONTRACT.md
                // "What a link looks like"), under the link.
                div {
                    class: "pimble-link-tooltip",
                    style: {|| match store.link_hover.get() {
                        Some(tip) => format!("left: {}px; top: {}px;", tip.x, tip.y),
                        None => "display: none;".to_string(),
                    }},
                    div {
                        class: "pimble-link-tooltip__target",
                        {|| store.link_hover.get().map(|tip| tip.target).unwrap_or_default()}
                    }
                    div {
                        class: "pimble-link-tooltip__hint",
                        style: {|| if store.link_hover.get().is_some_and(|tip| tip.hint.is_empty()) { "display: none;" } else { "" }},
                        {|| store.link_hover.get().map(|tip| tip.hint).unwrap_or_default()}
                    }
                }

                // The link picker (`[[` and Ctrl+L, docs/LINKS_CONTRACT.md
                // "Making a link"), under the caret. Keys reach it through
                // the editor (`link_picker::key`); a click picks a row.
                div {
                    class: "pimble-link-picker",
                    style: {|| match store.link_picker.get() {
                        Some(view) => format!("left: {}px; top: {}px;", view.x, view.y),
                        None => "display: none;".to_string(),
                    }},
                    div {
                        class: "pimble-link-picker__query",
                        style: {|| if picker_is_field(store) { "" } else { "display: none;" }},
                        {|| picker_query_text(store)}
                    }
                    for line in picker_lines(store) {
                        div {
                            key: format!("{}:{}", line.key, line.selected),
                            class: if line.selected { "pimble-link-picker__row pimble-link-picker__row--selected" } else { "pimble-link-picker__row" },
                            onclick: {
                                let index = line.index;
                                move || {
                                    crate::link_picker::accept(store, &crate::editor::focused_editor(store), index);
                                }
                            },
                            span { class: "pimble-link-picker__label", {line.label.clone()} }
                            span { class: "pimble-link-picker__detail", {line.detail.clone()} }
                        }
                    }
                    div {
                        class: "pimble-link-picker__empty",
                        style: {|| if picker_empty_text(store).is_empty() { "display: none;" } else { "" }},
                        {|| picker_empty_text(store)}
                    }
                }
            }
    }
}
