//! Pimble's application UI.
//!
//! Built with the Rinch UI framework. Everything here is target-independent
//! except [`run`], which opens the desktop window; the browser entry point in
//! `web/` mounts the very same component through `rinch_web`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rinch::prelude::*;

use crate::protocol::BackendCommand;
#[cfg(feature = "native")]
use crate::protocol::BackendHandle;
use crate::events::{EVENT_PROCESSOR, process_backend_events};
use crate::persistence::{load_dark_mode, load_panes, load_sidebar_width, save_panes};
use crate::state::{parse_tree_value, AppStore};
use crate::state::{SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH};
use crate::styles::{APP_CSS, EDITOR_CSS};
use explorer::TREE_STATE;
use navigation::step_history;

mod account;
mod appearance_modal;
mod deleted_modal;
mod explorer;
mod explorer_menu;
mod hosting;
mod navigation;
mod row_menu;
mod rows;
mod search;
mod share_modal;
mod shell;
mod status;
mod store_modals;
mod theme;
mod tree_row;
#[cfg(test)]
mod tests;

pub use account::{open_account_modal, run_account_action, run_sign_out_action, set_account_action, set_sign_out_action};
pub(crate) use account::{account_hidden, account_showing, account_shown};
pub use deleted_modal::{open_deleted_modal, open_deleted_modal_for_selection};
pub use explorer::focus_explorer;
pub(crate) use explorer::start_rename;
pub use hosting::{host_to_share, open_host_modal, open_hosted_modal, open_stop_relay_modal, share_from_this_computer};
pub(crate) use navigation::{open_node, open_node_in};
pub(crate) use rows::{copy_rows, reveal_row};
pub use search::{focus_search, rebuild_search_indexes};
pub(crate) use search::search_for;
pub use share_modal::open_share_modal;
#[cfg(feature = "native")]
pub use store_modals::{pick_and_create_store, pick_and_open_store};
pub use store_modals::{open_connect_modal, open_mount_picker, open_new_store_modal};
pub use theme::{apply_theme, theme_css, theme_props, toggle_dark_mode};

/// How long after a click a second one on the same row still counts as a
/// double-click, and so starts an inline rename.
const DOUBLE_CLICK_MS: f64 = 500.0;
/// The explorer's indent per level, in pixels: wide enough that the depths a drag
/// offers at the end of a group are easy to aim at (docs/TREE_CONTRACT.md).
const TREE_INDENT: f32 = 12.0;

/// Whether this build may administer the stores on its server: open one from a
/// path, add a remote as a replica, link, unlink, remove a replica, close one,
/// create one.
///
/// Those are the service principal's RPCs. The desktop app owns the server it
/// talks to and connects as that principal, so it offers them all. A browser
/// connects as a signed-in user, whose token carries per-store grants and
/// nothing else, so the server refuses every one of them; the account pages
/// create and share stores instead. Offering a menu item that can only fail is
/// worse than not offering it, so the browser build hides them.
const CAN_ADMINISTER_STORES: bool = cfg!(feature = "native");

/// Whether this build can make a store on the signed-in account.
///
/// The exact complement of [`CAN_ADMINISTER_STORES`] for creating one: a
/// desktop build makes a store as a directory through a native dialog, a
/// browser build asks the accounts service for a hosted one and mints its key.
/// Both are "New Store...", and no build offers neither.
const CAN_CREATE_HOSTED_STORES: bool = !CAN_ADMINISTER_STORES;

/// Why "Share..." is disabled on a mount row: the node itself lives in
/// another store, and that is where a share of it belongs.
const MOUNT_NOTE: &str = "A mount stands in for a node in another store. Share it there.";

/// Milliseconds on a clock that only moves forward, for the double-click
/// window above. `std::time::Instant` has no implementation on
/// `wasm32-unknown-unknown` and panics the first time it is read, so the
/// browser build reads the page's own clock instead. Only differences between
/// two readings matter here, so the two origins need not agree.
#[cfg(feature = "native")]
fn now_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

#[cfg(not(feature = "native"))]
fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// The application: its reactive state, its backend-event pump, and the component
/// that renders the whole UI. Both entry points call this and then differ only
/// in the platform chrome around it — [`run`] wraps it in a desktop window with
/// a menu bar, the web app hands the component straight to `rinch_web::mount`.
///
/// The returned [`AppStore`] is the same one the component uses, so a caller can
/// install a backend into it (the web app does, before mounting) or read the
/// theme choice out of it (the desktop does, to build the theme props).
pub fn build_view() -> (AppStore, impl FnOnce(&mut RenderScope) -> NodeHandle) {
    let store = AppStore::new();
    store.dark_mode.set(load_dark_mode());
    // Pictures in text with a board ask for the board drawn over them, and
    // boards embedded in text are drawn by the app (docs/IMAGE_MARKUP_CONTRACT.md
    // section 2). Installed here, outside any render, so it lasts as long
    // as the app.
    crate::text_boards::install_image_source();
    // Image files dropped on a tree row, and a picture pasted with a row
    // selected, become image nodes (docs/IMAGE_MARKUP_CONTRACT.md section 1);
    // the desktop hears both through rinch, the browser through the page.
    #[cfg(not(feature = "native"))]
    crate::boards::install_browser_handlers(store);
    if let Some(width) = load_sidebar_width() {
        store.sidebar_width.set(width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH));
    }
    // The Properties panel as this device left it (closed the first time,
    // docs/PROPERTIES_CONTRACT.md decision 2), remembered whenever it opens,
    // closes or is resized.
    if let Some(panel) = crate::views::properties() {
        store.properties_open.set(panel.open);
        store.properties_width.set(panel.width.clamp(crate::properties::MIN_WIDTH, crate::properties::MAX_WIDTH));
    }
    // The first run only subscribes (no timer before the window exists).
    let panel_subscribed = Cell::new(false);
    let _ = rinch::Effect::new(move || {
        let panel = crate::views::PanelView { open: store.properties_open.get(), width: store.properties_width.get() };
        if panel_subscribed.replace(true) {
            untracked(|| crate::views::set_properties(panel));
        }
    });

    // The split view as it was left (docs/SPLIT_VIEW_CONTRACT.md
    // "Persistence"): the tiling at once, each pane's document when its
    // store is open. From then on the layout is saved whenever the tiling,
    // a pane's document or the focus changes; a divider being dragged is
    // saved when it is let go. Coalesced over a turn, because switching a
    // pane's document passes through holding none.
    let remembered = load_panes();
    if let Some(saved) = remembered.clone() {
        store.restore_panes(saved);
    }
    let last_saved = Rc::new(RefCell::new(remembered));
    let save_pending = Rc::new(Cell::new(false));
    // The first run only subscribes: nothing has changed yet, and there is
    // no event loop to run a timer before the window exists.
    let subscribed = Cell::new(false);
    let _ = rinch::Effect::new(move || {
        let _ = store.saved_panes();
        if store.pane_dragging.get() || !subscribed.replace(true) || save_pending.replace(true) {
            return;
        }
        let (last_saved, save_pending) = (last_saved.clone(), save_pending.clone());
        let _ = set_timeout(0, move || {
            save_pending.set(false);
            let now = untracked(|| store.saved_panes());
            if last_saved.borrow().as_ref() != Some(&now) {
                save_panes(&now);
                *last_saved.borrow_mut() = Some(now);
            }
        });
    });

    // Double-click detection for rename: (last_click_time, last_click_value)
    let last_click: Rc<Cell<(f64, String)>> = Rc::new(Cell::new((now_ms(), String::new())));

    // Persistent tree state — created once, preserves expanded/selected across data changes.
    // The rows open as this device left them (`views.rs`): rinch's set
    // holds their tree values, `AppStore::expanded` the nodes, whose children
    // are fetched as each row arrives (`ChildrenLoaded`).
    // The saved stores have their rows from the first frame, "opening...",
    // not from the moment the server answers (docs/STARTUP_CONTRACT.md).
    for path in crate::persistence::load_app_state_file() {
        store.begin_opening(&path);
    }
    let remembered = crate::views::expanded();
    store.expanded.update(|e| {
        e.extend(remembered.iter().filter_map(|value| match parse_tree_value(value) {
            Some((store_id, Some(node_id))) => Some((store_id, node_id)),
            _ => None,
        }));
    });
    let tree_state = UseTreeReturn::new(UseTreeOptions {
        multiple: true,
        initial_expanded: remembered.into_iter().collect(),
        ..Default::default()
    });
    TREE_STATE.with(|cell| cell.set(Some(tree_state)));
    let _ = rinch::Effect::new(move || {
        tree_state.expanded.with(crate::views::expanded_now);
    });

    // A followed link's target opens the way a search hit does, with
    // `open_node` (docs/LINKS_CONTRACT.md "Following a link"), deferred out
    // of the effect so opening, which writes signals other effects read,
    // never runs inside it. It opens in the pane it was followed from (the
    // focused one when that pane has closed since, or it was followed from
    // none). A deep link's anchor waits in that pane's `pending_anchor` for
    // the editor session of that node.
    let _ = rinch::Effect::new(move || {
        let Some(open) = store.link_open.get() else { return };
        let _ = set_timeout(0, move || {
            if untracked(|| store.link_open.get()).as_ref() != Some(&open) {
                return;
            }
            store.link_open.set(None);
            let shown = untracked(|| store.tiling.with(|tiling| tiling.panes()));
            let pane = open.pane.filter(|pane| shown.contains(pane)).unwrap_or_else(|| store.focused());
            store.pane(pane).pending_anchor.set(open.anchor.clone().map(|anchor| (open.store_id, open.node_id, anchor)));
            open_node_in(store, tree_state, pane, open.value);
            // Where it is in the tree, which may not have been opened that far.
            store.send(BackendCommand::GetAncestors { store_id: open.store_id, node_id: open.node_id });
        });
    });

    // View > Back / Forward (and their shortcuts): a step through the
    // focused pane's history, taken here where opening lives, deferred out of
    // the effect as a followed link is.
    let _ = rinch::Effect::new(move || {
        let Some(back) = store.nav_step.get() else { return };
        let _ = set_timeout(0, move || {
            store.nav_step.set(None);
            step_history(store, tree_state, back);
        });
    });

    // The tree row being dragged, for drop targets outside the tree
    let drag_ctx: DragContext<String> = DragContext::new();
    // A row dropped on a board becomes a node card there
    // (docs/IMAGE_MARKUP_CONTRACT.md section 4).
    crate::boards::set_tree_drag(drag_ctx);

    // Set up event processing via thread-local so run_on_main_thread
    // can trigger it without capturing non-Send types. (No autosave loop
    // needed — the collab session persists edits live.)
    crate::startup::mark("window built");
    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            process_backend_events(store, tree_state);
        }));
    });
    // Build app component - parameter must be named __scope for the rsx! macro
    let app_component = move |__scope: &mut RenderScope| -> NodeHandle {
        // The shell put rinch's theme sheet in force; this one adds Pimble's
        // variables for the scheme the app starts in.
        apply_theme(untracked(|| store.dark_mode.get()));
        let (tree_scroll, cancel_rename) = explorer::render_explorer(__scope, store, tree_state, last_click, drag_ctx);

        // The split view: one to four panes beside the explorer, each with
        // its own title strip, toolbar and editor
        // (docs/SPLIT_VIEW_CONTRACT.md; `pane_view/` draws it).
        let mut empty_hints: Vec<&'static str> = Vec::new();
        if CAN_ADMINISTER_STORES {
            empty_hints.push("Or press Ctrl+N to create a new store");
        }
        if CAN_CREATE_HOSTED_STORES {
            empty_hints.push("Or use + above the tree to make a store");
        }
        let panes_view = crate::pane_view::render_panes(__scope, store, &empty_hints);

        // The tree's selected row is the focused pane's document: it follows
        // when the focus moves to another pane, by a click in it or by its
        // editor taking the keyboard, and when that pane's document changes.
        // Untracked inside: the tree's own selection is written here, never
        // read.
        let _ = rinch::Effect::new(move || {
            let selected = store.selected_id.get();
            untracked(|| match &selected {
                Some(value) => {
                    tree_state.controller.set_selection([value.clone()]);
                    tree_state.controller.set_anchor(Some(value));
                }
                None => tree_state.controller.clear_selected(),
            });
        });


        let (search_bar, search_panel) = search::render_search(__scope, store, tree_state);
        let connect_modal = store_modals::render_connect_modal(__scope, store);
        let link_modal = store_modals::render_link_modal(__scope, store);
        let appearance_modal = appearance_modal::render(__scope, store);
        // Paste with several things it could do (`crate::pasting`).
        let paste_chooser = crate::pasting::render_chooser(__scope, store);
        let new_store_modal = store_modals::render_new_store_modal(__scope, store);
        let import_modal = store_modals::render_import_modal(__scope, store);
        let mount_picker_modal = store_modals::render_mount_picker(__scope, store);
        let remove_replica_modal = store_modals::render_remove_replica_modal(__scope, store);
        let discussions_modal = crate::discussions::render_unread_modal(__scope, store);
        let deleted_modal = deleted_modal::render(__scope, store);
        let about_modal = crate::about::render_about(__scope, store);
        let send_diagnostics = crate::diagnostics_ui::render_send(__scope, store);
        let account_page = account::render_account_page(__scope, store);
        let settings_modal = crate::settings::render(__scope, store, account_page);
        let host_modal = hosting::render_host_modal(__scope, store);
        let stop_relay_modal = hosting::render_stop_relay_modal(__scope, store);
        let hosted_modal = hosting::render_hosted_modal(__scope, store);
        let share_modal = share_modal::render(__scope, store);

        // Spawn the backend now that the UI and event loop are fully set up.
        // The EVENT_PROCESSOR thread-local is already registered, so signal_ui
        // callbacks will be processed correctly via run_on_main_thread. The web
        // build has no thread to spawn: its entry point puts a backend into the
        // store before it mounts, and this leaves that one alone.
        // The plugins load off the UI thread (the desktop's folder) or
        // asynchronously (the browser's own store of them); a metanode opened
        // before they are done shows its outline until then.
        crate::plugins::load(store);
        #[cfg(feature = "native")]
        if store.backend.with(|b| b.is_none()) {
            let backend = BackendHandle::spawn(move || {
                run_on_main_thread(crate::events::pump_backend_events);
            });
            store.backend.set(Some(backend));
        }

        let parts = shell::Parts { tree_scroll, search_bar, search_panel, panes_view };
        let body = shell::render_body(__scope, store, tree_state, cancel_rename, parts);

        // The title bar shows the pebble and the wordmark, not a text title
        // (BorderlessWindow draws a title in the inline menu row as letters in
        // rotating colors).
        #[cfg(feature = "native")]
        let brand: rinch::components::SectionRenderer =
            Rc::new(move |__scope| crate::brand::titlebar_lockup(__scope, store));
        // The first-open question (docs/PRESENCE_CONTRACT.md section 3), both
        // targets; "Shares..." is a page of Settings.
        // This window's panes, said to everyone else (docs/PRESENCE_CONTRACT.md).
        crate::presence::start(store);
        let share_name_question = crate::shares::render_question(__scope, store);
        #[cfg(feature = "native")]
        let root = rsx! {
            BorderlessWindow {
                title: "",
                left_section: Some(brand),
                show_minimize: true,
                show_maximize: true,
                show_close: true,
                // `BorderlessWindow` only wires a control's click handler when
                // its callback prop is set (rinch-components/src/borderless_window.rs:
                // `if let Some(ref cb) = self.on_maximize { ... register_handler ... }`)
                // — with no `on_maximize`/`on_minimize`, the buttons render but
                // nothing is registered for `data-rid`, so clicking them did
                // nothing. These two are the platform calls the component's
                // own doc comment says to wire.
                on_minimize: || minimize_current_window(),
                on_maximize: || toggle_maximize_current_window(),
                on_close: || crate::desktop::close_window(),

                style { {APP_CSS} }
                style { {pimble_ui::STYLES} }
                style { {EDITOR_CSS} }

                {connect_modal}
                {link_modal}
                {remove_replica_modal}
                {deleted_modal}
                {discussions_modal}
                {new_store_modal}
                {import_modal}
                {mount_picker_modal}
                {appearance_modal}
                {settings_modal}
                {about_modal}
                {send_diagnostics}
                {host_modal}
                {stop_relay_modal}
                {hosted_modal}
                {share_modal}
                {share_name_question}
                {paste_chooser}
                {body}
            }
        };

        // A browser tab has its own chrome; the app just fills it. It fills
        // what the menu bar leaves, not the viewport: rinch-web puts this
        // under a 28px bar inside a viewport-tall box, and `100vh` here put
        // the status bar that far below the fold.
        #[cfg(not(feature = "native"))]
        let root = rsx! {
            div {
                style: "height: 100%; display: flex; flex-direction: column;",

                style { {APP_CSS} }
                style { {pimble_ui::STYLES} }
                style { {EDITOR_CSS} }

                {connect_modal}
                {link_modal}
                {remove_replica_modal}
                {deleted_modal}
                {discussions_modal}
                {new_store_modal}
                {import_modal}
                {mount_picker_modal}
                {appearance_modal}
                {settings_modal}
                {about_modal}
                {send_diagnostics}
                {host_modal}
                {stop_relay_modal}
                {hosted_modal}
                {share_modal}
                {share_name_question}
                {paste_chooser}
                {body}
            }
        };

        root
    };

    (store, app_component)
}
