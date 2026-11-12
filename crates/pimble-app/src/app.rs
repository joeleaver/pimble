//! Pimble's application UI.
//!
//! Built with the Rinch UI framework. Everything here is target-independent
//! except [`run`], which opens the desktop window; the browser entry point in
//! `web/` mounts the very same component through `rinch_web`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rinch::prelude::*;
use rinch::core::{set_keyboard_interceptor, clear_keyboard_interceptor};
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::protocol::{BackendCommand, CloudOp};
#[cfg(feature = "native")]
use crate::protocol::BackendHandle;
use crate::editor::{start_editing, stop_editing};
use crate::panes::PaneId;
use crate::events::{EVENT_PROCESSOR, process_backend_events};
use crate::appearance::{display_color, icon_by_name, icons_matching, row_background, IconGlyph, COLOR_CHOICES, ROW_COLOR_CHOICES};
use crate::persistence::{load_dark_mode, load_panes, load_sidebar_width, save_dark_mode, save_panes, save_sidebar_width};
use crate::state::{parse_tree_value, display_label_from_node, mount_is_dimmed, mount_label_suffix, AppStore, SearchState};
use crate::state::{deleted_row_meta, deleted_row_read_only};
use crate::state::{ShareFace, HOST_ON_CLOUD_LABEL, HOST_ON_CLOUD_SENTENCE, SHARE_FROM_HERE_LABEL, SHARE_FROM_HERE_SENTENCE};
// Only "Mount Store Here...", which picks a directory on this machine, sets one.
#[cfg(feature = "native")]
use crate::state::PendingMount;
use crate::state::{SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH};
use crate::styles::{APP_CSS, EDITOR_CSS};
use pimble_ui::presence::PersonDot;

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

/// rinch's theme for the app: one function so the startup theme and a runtime
/// switch (View > "Toggle Dark Mode", `rinch::update_theme`) agree on everything
/// but the scheme. The editor's own stylesheet follows the same flag through
/// `EditorHandle::set_dark_mode`, and colours the app derives follow
/// `AppStore::dark_mode` reactively.
pub fn theme_props(dark_mode: bool) -> ThemeProviderProps {
    ThemeProviderProps {
        primary_color: Some("blue".into()),
        dark_mode,
        default_radius: Some("sm".into()),
        ..Default::default()
    }
}

/// The theme stylesheet: rinch's own for the scheme, then Pimble's variables for
/// it (`styles::LIGHT_THEME_VARS`, `styles::DARK_THEME_VARS`). They ride in the
/// theme sheet because it is the one sheet rinch replaces when the scheme
/// changes; an app sheet is parsed once.
pub fn theme_css(dark_mode: bool) -> String {
    let mut css = rinch::generate_theme_css_string(&theme_props(dark_mode));
    css.push_str(if dark_mode { crate::styles::DARK_THEME_VARS } else { crate::styles::LIGHT_THEME_VARS });
    css
}

/// "Restart to update" (docs/UPDATE_CONTRACT.md). The browser has nothing to
/// update; its status bar never shows the control.
fn restart_to_update(store: AppStore) {
    #[cfg(feature = "native")]
    crate::update::restart_to_update(store);
    #[cfg(not(feature = "native"))]
    let _ = store;
}

/// Put [`theme_css`] in force, at start and on every switch.
pub fn apply_theme(dark_mode: bool) {
    rinch::core::set_current_theme_css(Some(theme_css(dark_mode)));
}

/// Switch between the dark and light themes: the theme CSS, the editor's
/// stylesheet, every colour the app derives (they read `dark_mode`), and the
/// saved preference.
pub fn toggle_dark_mode(store: AppStore) {
    let dark = !untracked(|| store.dark_mode.get());
    store.dark_mode.set(dark);
    apply_theme(dark);
    crate::editor::set_dark_mode(dark);
    save_dark_mode(dark);
}

thread_local! {
    /// Each tree row's label element by its tree value, the latest render's,
    /// so a revealed row can be scrolled to (`reveal_row`).
    static TREE_ROWS: RefCell<std::collections::HashMap<String, NodeHandle>> = RefCell::new(std::collections::HashMap::new());
    /// A row to scroll to as soon as it is rendered (its parents' children
    /// may still be loading when it is revealed).
    static REVEAL_ROW: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Scroll the tree to the row for `value` (a followed link's target, after
/// `events::reveal_in_tree` opened its parents): now if the row is drawn,
/// and again when it is next drawn, since rows appear only once their
/// parent's children arrive.
pub(crate) fn reveal_row(value: String) {
    if let Some(row) = TREE_ROWS.with(|rows| rows.borrow().get(&value).cloned()) {
        row.scroll_into_view();
    }
    REVEAL_ROW.with(|pending| *pending.borrow_mut() = Some(value));
}

thread_local! {
    /// What the sidebar's account button does, when there is one.
    ///
    /// The browser build has account pages of its own (`/app/account`) and has
    /// to be able to reach them without a page load, because a reload throws
    /// away the keys that make an encrypted store readable. It registers the
    /// move here; the desktop registers nothing, and the button is not drawn.
    /// A hook rather than a `cfg` so the shared UI stays free of target
    /// spelling, and rather than a `BackendCommand` because nothing about it
    /// involves the backend.
    static ACCOUNT_ACTION: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    /// What "Sign Out" does, for a build that has an account to sign out of.
    /// Registered by the browser entry point, like [`ACCOUNT_ACTION`].
    static SIGN_OUT_ACTION: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    /// The search box's `NodeHandle`, captured once when the toolbar is built
    /// so the View menu's "Focus Search" (Ctrl+Shift+F) action — constructed earlier,
    /// before the box exists — can reach it later. Same reach-across-closures
    /// need as `EVENT_PROCESSOR`/`CLOSE_HANDLER` below.
    static SEARCH_INPUT: RefCell<Option<NodeHandle>> = const { RefCell::new(None) };
    /// The explorer tree's state, for the View menu's "Focus Explorer",
    /// built before the tree is.
    static TREE_STATE: Cell<Option<UseTreeReturn>> = const { Cell::new(None) };
    /// Each tree row's rename box, by tree value, for `start_rename` to give
    /// the keyboard to. A handle and not an id: `request_focus` is the
    /// desktop runtime's alone, and a browser needs the element's own focus.
    static RENAME_INPUTS: RefCell<std::collections::HashMap<String, NodeHandle>> = RefCell::new(std::collections::HashMap::new());
    /// The debounced `Search` command pending from the last keystroke, if any.
    static SEARCH_DEBOUNCE: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
}

/// Give the sidebar an account button and say what it does. Called before
/// [`build_view`]; calling it again replaces the action.
pub fn set_account_action(action: impl Fn() + 'static) {
    ACCOUNT_ACTION.with(|slot| *slot.borrow_mut() = Some(Rc::new(action)));
}

/// Whether a build registered one. Read once, at render time: the browser
/// registers before the view is built and the desktop never does, so this never
/// changes while a view is on screen.
fn has_account_action() -> bool {
    ACCOUNT_ACTION.with(|slot| slot.borrow().is_some())
}

pub fn run_account_action() {
    // Cloned out of the slot before it runs: the action navigates, which
    // unmounts this view, and a borrow held across that would be live while
    // the thing that owns it goes away.
    let action = ACCOUNT_ACTION.with(|slot| slot.borrow().clone());
    if let Some(action) = action {
        action();
    }
}

/// Give "Sign Out" something to do. Called before [`build_view`].
pub fn set_sign_out_action(action: impl Fn() + 'static) {
    SIGN_OUT_ACTION.with(|slot| *slot.borrow_mut() = Some(Rc::new(action)));
}

pub fn run_sign_out_action() {
    let action = SIGN_OUT_ACTION.with(|slot| slot.borrow().clone());
    if let Some(action) = action {
        action();
    }
}

/// "New Store...", the desktop's way: pick a path, create, open.
#[cfg(feature = "native")]
pub fn pick_and_create_store(store: AppStore) {
    let dialog = rinch::dialogs::save_file()
        .set_title("Create New Store")
        .add_filter("Pimble Store", &["pimble"]);

    let Some(path) = dialog.save() else { return };
    let path_str = path.to_string_lossy().to_string();
    let path_str = if path_str.ends_with(".pimble") {
        path_str
    } else {
        format!("{}.pimble", path_str)
    };
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "New Store".to_string());
    tracing::info!("Creating new store at: {}", path_str);
    store.pending_create_path.set(Some(path_str.clone()));
    store.send(BackendCommand::CreateStore { path: path_str, name });
}

/// "Open Store...", the desktop's way: pick a directory and open it.
#[cfg(feature = "native")]
pub fn pick_and_open_store(store: AppStore) {
    let Some(path) = rinch::dialogs::pick_folder().set_title("Open Store").pick() else {
        return;
    };
    let path_str = path.to_string_lossy().to_string();
    tracing::info!("Opening store at: {}", path_str);
    store.send(BackendCommand::OpenStore { path: path_str });
}

/// Put the caret in the search box.
///
/// The desktop reaches this through the View menu's "Focus Search"; a browser
/// build, which has no menu bar to carry an accelerator, binds Ctrl+K to it
/// itself. The search box advertises the shortcut in its placeholder, so
/// exactly one of the two must always be in place.
pub fn focus_search() {
    SEARCH_INPUT.with(|cell| {
        if let Some(handle) = cell.borrow().as_ref() {
            handle.focus();
        }
    });
}

/// Give the explorer the keyboard (View > "Focus Explorer"): its cursor row,
/// or its first (docs/TREE_CONTRACT.md "The keyboard").
pub fn focus_explorer() {
    if let Some(tree_state) = TREE_STATE.with(Cell::get) {
        tree_state.controller.focus();
    }
}

/// Rebuild the search index of every open store that has one.
///
/// An encrypted store has no server-side index and cannot have one — the
/// server has never seen a word of it — so it is skipped rather than asked.
pub fn rebuild_search_indexes(store: AppStore) {
    let store_ids = untracked(|| store.store_ids.get());
    for store_id in store_ids {
        if store.is_vault(store_id) {
            continue;
        }
        store.send(BackendCommand::RebuildIndex { store_id });
    }
}

/// Open the "Mount Store..." picker for a target node.
///
/// The desktop's "Mount Store..." picks a directory; a browser has none to pick
/// from, so it picks one of the stores the account already grants. Either way
/// the RPC is the same `createMount`, which an editor is allowed to call.
pub fn open_mount_picker(store: AppStore, target: (pimble_core::StoreId, pimble_core::NodeId)) {
    // The target goes in first: the picker's own option list reads it to leave
    // out the store being mounted into, and a list built before it was set
    // would offer that store as a place to put itself.
    store.mount_picker_target.set(Some(target));
    let first = untracked(|| {
        store.store_ids.with(|ids| ids.iter().find(|&&id| id != target.0).copied())
    });
    store.mount_picker_selected.set(first.map(|id| id.to_string()).unwrap_or_default());
    store.mount_picker_error.set(String::new());
    store.mount_picker_pending.set(false);
}

/// Open the "New Store..." modal, empty.
pub fn open_new_store_modal(store: AppStore) {
    store.new_store_modal_name.set(String::new());
    store.new_store_modal_error.set(String::new());
    store.new_store_modal_pending.set(false);
    store.new_store_modal_open.set(true);
}

/// Select and open a node or store root identified by a tree value
/// (`"store_{uuid}"` / `"node_{store_uuid}_{node_uuid}"`) — the single
/// implementation behind both a tree click and a search-result click, per
/// CLAUDE.md's "one way to write node content": there is one way to open one.
///
/// It opens in the focused pane (docs/SPLIT_VIEW_CONTRACT.md "What the
/// person sees"); [`open_node_in`] names another.
pub(crate) fn open_node(store: AppStore, tree_state: UseTreeReturn, value: String) {
    open_node_in(store, tree_state, store.focused(), value);
}

/// Open a tree value in pane `pane`. The tree's selected row follows when
/// the pane has the focus, and the pane's history ([`crate::nav`]) records it.
pub(crate) fn open_node_in(store: AppStore, tree_state: UseTreeReturn, pane: PaneId, value: String) {
    store.pane(pane).history.update(|h| h.visit(&value));
    show_in_pane(store, tree_state, pane, value);
}

/// The explorer's back (or forward) arrow: the focused pane steps through
/// what it has shown, skipping anything in a store that is no longer open.
fn step_history(store: AppStore, tree_state: UseTreeReturn, back: bool) {
    let pane = store.focused();
    let history = store.pane(pane).history;
    loop {
        let mut to = None;
        history.update(|h| to = if back { h.back() } else { h.forward() });
        let Some(value) = to else { return };
        let opens = parse_tree_value(&value).is_some_and(|(s_id, _)| store.get_store_signal(s_id).is_some());
        if opens {
            show_in_pane(store, tree_state, pane, value);
            return;
        }
        history.update(|h| h.forget(&value));
    }
}

/// Open a tree value in pane `pane` without touching its history: what both
/// an ordinary open and a step back or forward end in.
fn show_in_pane(store: AppStore, tree_state: UseTreeReturn, pane: PaneId, value: String) {
    // A mount row is a placeholder: the pane shows the node it mounts (a
    // mounted kanban is the kanban), and the explorer keeps the mount's row
    // chosen.
    if let Some((s_id, Some(n_id))) = parse_tree_value(&value) {
        if store.is_mount(s_id, n_id) {
            let source = pimble_core::plan::TreeView::mount_source(&store.view(), s_id, n_id);
            if let Some((source_store, source_node)) = source {
                show_in_pane(store, tree_state, pane, format!("node_{source_store}_{source_node}"));
                if pane == store.focused() {
                    tree_state.controller.set_selection([value.clone()]);
                    tree_state.controller.set_anchor(Some(&value));
                }
                return;
            }
        }
    }
    let focused = pane == store.focused();
    let state = store.pane(pane);
    store.select_in_pane(pane, Some(value.clone()));
    // Whatever the saved layout meant this pane for, the person chose this.
    state.restore.set(None);
    if focused {
        tree_state.controller.set_selection([value.clone()]);
        // The open row is where the next Shift run starts.
        tree_state.controller.set_anchor(Some(&value));
    }

    // Offline-first: a document opens from the cached content at once and
    // `start_editing` then reconciles the session with the server (state
    // vector in, diff out), so a cache that is behind the server (this
    // window's own earlier session, another window, a replica) is merged
    // in place rather than reloaded. A node the cache has never seen is
    // fetched; `NodeLoaded` starts the session then.
    let parsed = parse_tree_value(&value);
    let mut is_board = false;
    let mut is_sheet = false;
    let mut is_discussion = false;
    let mut is_metanode = false;
    let (title_opt, content_bytes_opt, is_document, sel_ids) = if let Some((s_id, node_id_opt)) = parsed {
        if let Some(n_id) = node_id_opt {
            let result = store.get_node_signal(s_id, n_id).map(|sig| {
                sig.with(|node| {
                    let label = if !node.metadata.title.is_empty() {
                        node.metadata.title.clone()
                    } else {
                        "Untitled".to_string()
                    };
                    let is_doc = node.node_type == pimble_core::node_types::DOCUMENT;
                    let board = node.node_type == pimble_core::node_types::BOARD;
                    let sheet = node.node_type == pimble_core::node_types::SHEET;
                    let discussion = node.node_type == pimble_core::node_types::DISCUSSION;
                    let metanode = crate::metanodes::is_metanode(node);
                    (label, node.content.clone(), is_doc, board, sheet, discussion, metanode)
                })
            });
            match result {
                // A metanode shows its outline whatever its type
                // (docs/METANODES_CONTRACT.md section 1.4).
                Some((label, _, _, _, _, _, true)) => {
                    is_metanode = true;
                    (Some(label), None, false, Some((s_id, n_id)))
                }
                Some((label, content, is_doc, board, sheet, discussion, false)) => {
                    is_board = board;
                    is_sheet = sheet;
                    is_discussion = discussion;
                    (Some(label), Some(content), is_doc, Some((s_id, n_id)))
                }
                None => {
                    store.send(BackendCommand::GetNode { store_id: s_id, node_id: n_id });
                    (None, None, false, None)
                }
            }
        } else {
            let title = store.get_store_signal(s_id)
                .map(|sig| sig.with(|s| s.name.clone()));
            match title {
                Some(t) => (Some(t), Some(Vec::new()), false, None),
                None => (None, None, false, None),
            }
        }
    } else {
        (None, None, false, None)
    };

    state.show_editor.set(is_document);
    state.show_board.set(is_board);
    state.show_sheet.set(is_sheet);
    state.show_discussion.set(is_discussion);

    if let Some(title) = title_opt.filter(|_| focused) {
        store.node_title.set(title);
    }

    if is_metanode {
        if let Some((s_id, n_id)) = sel_ids {
            crate::metanodes::start(store, pane, s_id, n_id);
        }
        store.editor_dirty.set(false);
        return;
    }
    // Whatever metanode the pane showed, it shows this instead.
    crate::metanodes::stop(store, pane);

    if is_document {
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            start_editing(store, pane, s_id, n_id, &content_bytes);
        }
    } else if is_board {
        // A board node (docs/IMAGE_MARKUP_CONTRACT.md section 1): the pane
        // shows its board, the editor stays mounted and hidden.
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            crate::boards::start(store, pane, s_id, n_id, &content_bytes);
        }
    } else if is_sheet {
        // A sheet node (docs/SHEETS_CONTRACT.md section 1): the pane shows
        // its tables, the editor stays mounted and hidden.
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            crate::sheets::start(store, pane, s_id, n_id, &content_bytes);
        }
    } else if is_discussion {
        // A discussion (docs/DISCUSSIONS_CONTRACT.md): the pane shows its
        // thread, the editor stays mounted and hidden.
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            crate::discussions::start(store, pane, s_id, n_id, &content_bytes);
        }
    } else {
        stop_editing(store, pane);
    }
    store.editor_dirty.set(false);
}

/// Remember the node of row `value` (a store row: its root) as copied, for a
/// later Paste to mount (docs/MENUS_CONTRACT.md section 3.1). A row of an
/// encrypted store is not remembered: no mount reaches into one.
fn remember_copied(store: AppStore, value: &str) {
    let Some((s_id, node_id_opt)) = parse_tree_value(value) else { return };
    if store.is_vault(s_id) {
        return;
    }
    match node_id_opt {
        Some(n_id) => {
            let title = store.display_label(s_id, n_id);
            store.mount_source.set(Some((s_id, n_id, title)));
        }
        None => {
            if let Some(root_id) = store.root_node_id(s_id) {
                let name = store.get_store_signal(s_id)
                    .map(|sig| untracked(|| sig.with(|s| s.name.clone())))
                    .unwrap_or_default();
                store.mount_source.set(Some((s_id, root_id, name)));
            }
        }
    }
}

/// Copy (docs/MENUS_CONTRACT.md section 3.1; Joe: "'copy' copies
/// everything"): the rows' `pimble:` links on the system clipboard, and the
/// first row remembered for a later Paste to mount. A row menu's Copy and the
/// explorer's Ctrl+C.
pub(crate) fn copy_rows(store: AppStore, values: &[String]) {
    if let Some(first) = values.first() {
        remember_copied(store, first);
    }
    copy_links(store, values);
}

/// Start renaming the row of tree value `value` in place: a double-click, F2,
/// or a node the keyboard just made. Enter commits and Escape cancels, and
/// either gives the keyboard back to the tree. `false` when the row cannot
/// be renamed here (a mount, or a node this device may only read).
pub(crate) fn start_rename(store: AppStore, tree_state: UseTreeReturn, value: &str) -> bool {
    // Don't allow rename on mount nodes
    let is_mount_node = if let Some((sid, Some(nid))) = parse_tree_value(value) {
        store.is_mount(sid, nid)
    } else {
        false
    };
    if is_mount_node {
        return false;
    }

    // A node this device may only read changes in no way
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"):
    // the context menu's "Rename" is disabled there, so the
    // double-click shortcut is not a way around it. An editor
    // renames like anyone else. Judged per node: a reader's
    // root may sit beside an editor's in the same store.
    let renameable = match parse_tree_value(value) {
        Some((s_id, Some(n_id))) => store.node_access(s_id, n_id).allows_write(),
        Some((s_id, None)) => store.store_access(s_id).allows_write(),
        None => true,
    };
    if !renameable {
        return false;
    }

    let edit_text = if let Some((store_id, Some(node_id))) = parse_tree_value(value) {
        store.get_node_signal(store_id, node_id)
            .map(|sig| sig.with(|n| n.metadata.title.clone()))
            .unwrap_or_default()
    } else {
        String::new()
    };
    store.rename_text.set(edit_text);
    store.renaming_node.set(Some(value.to_string()));
    let nv_interceptor = value.to_string();
    set_keyboard_interceptor(move |data| {
        if data.key == "Escape" {
            run_on_main_thread(move || {
                store.renaming_node.set(None);
                clear_keyboard_interceptor();
                tree_state.controller.focus();
            });
            return true;
        }
        if data.key == "Enter" {
            let nv = nv_interceptor.clone();
            run_on_main_thread(move || {
                let new_title = rename_title(&nv, untracked(|| store.rename_text.get()));
                store.renaming_node.set(None);
                clear_keyboard_interceptor();
                tree_state.controller.focus();
                if let (Some((s_id, Some(n_id))), Some(new_title)) = (parse_tree_value(&nv), new_title) {
                    store.send(BackendCommand::RenameNode {
                        store_id: s_id, node_id: n_id, title: new_title,
                    });
                }
            });
            return true;
        }
        false
    });
    // A moment later: the box is shown by an effect of `renaming_node`, and
    // a browser will not focus it while it is still `display: none`.
    if let Some(input) = RENAME_INPUTS.with(|inputs| inputs.borrow().get(value).cloned()) {
        let _ = set_timeout(0, move || input.focus());
    }
    true
}

thread_local! {
    /// The row of a node just made whose name is being typed: its box starts
    /// empty, and an empty Enter keeps the name it was made with ("Metanode").
    static FRESH_RENAME: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The title a rename of `value` commits, or `None` to keep the one it has: a
/// node just made, named nothing, keeps the name it was made with.
fn rename_title(value: &str, text: String) -> Option<String> {
    let fresh = FRESH_RENAME.with(|f| f.borrow_mut().take()).as_deref() == Some(value);
    (!(fresh && text.trim().is_empty())).then_some(text)
}

/// What a row's menu acts on: the selection when the row is one of several
/// selected rows, the row alone otherwise (docs/TREE_CONTRACT.md
/// "Multi-select"). A right-click does not change the selection.
fn rows_for_menu(tree_state: UseTreeReturn, value: &str) -> Vec<String> {
    let selected = untracked(|| tree_state.selected.get());
    if selected.len() > 1 && selected.contains(value) {
        let mut rows: Vec<String> = selected.into_iter().collect();
        rows.sort();
        rows
    } else {
        vec![value.to_string()]
    }
}

/// "Copy Link" over several rows: one link per line.
fn copy_links(store: AppStore, values: &[String]) {
    if let [one] = values {
        return copy_link(store, one);
    }
    let links: Vec<String> = values
        .iter()
        .filter_map(|v| {
            let (s_id, node_id_opt) = parse_tree_value(v)?;
            let node_id = node_id_opt.or_else(|| store.root_node_id(s_id))?;
            Some(pimble_core::PimbleUrl::node(s_id, node_id).to_string())
        })
        .collect();
    match crate::links::copy_text(&links.join("\n")) {
        Ok(()) => crate::events::show_notice(store, format!("{} links copied.", links.len())),
        Err(e) => crate::events::show_notice(store, format!("The links could not be copied: {e}")),
    }
}

/// "Copy Link" (docs/LINKS_CONTRACT.md "Making a link"): the row's node, or a
/// store row's root, as a `pimble:` link on the system clipboard. Through a
/// mount the value names the source store's node, as every address does.
pub(crate) fn copy_link(store: AppStore, value: &str) {
    let Some((s_id, node_id_opt)) = parse_tree_value(value) else { return };
    let Some(node_id) = node_id_opt.or_else(|| store.root_node_id(s_id)) else { return };
    let url = pimble_core::PimbleUrl::node(s_id, node_id).to_string();
    match crate::links::copy_text(&url) {
        Ok(()) => crate::events::show_notice(store, "Link copied.".to_string()),
        Err(e) => crate::events::show_notice(store, format!("The link could not be copied: {e}")),
    }
}

/// Open the "Add Remote Store..." modal, resetting everything a previous run
/// left behind. `target` picks the mode (docs/history/REMOTE_MOUNTS_CONTRACT.md
/// decision 9): `None` is the plain File-menu flow that only adds a replica,
/// `Some(canonical pair)` is "Mount Remote Store Here...", whose action adds
/// the replica when this server does not already have the store and then
/// mounts its root under that pair.
pub fn open_connect_modal(store: AppStore, target: Option<(pimble_core::StoreId, pimble_core::NodeId)>) {
    store.connect_modal_target.set(target);
    store.connect_modal_stores.set(Vec::new());
    store.connect_modal_selected.set(String::new());
    store.connect_modal_error.set(String::new());
    store.connect_modal_busy.set(false);
    store.connect_modal_pending_add.set(false);
    store.connect_modal_token.set(String::new());
    store.connect_modal_token_visible.set(false);
    if untracked(|| store.connect_modal_url.get()).is_empty() {
        store.connect_modal_url.set("http://".to_string());
    }
    store.connect_modal_open.set(true);
}

/// Open the "Account..." modal (docs/DESKTOP_ACCOUNT_CONTRACT.md decision 1):
/// the sign-in form while nothing is signed in, the signed-in view otherwise.
/// `hint` is why it opened when it did not open from the menu ("Sign in to
/// host a store"), or empty. The URL field starts at `https://pimble.app` and
/// then keeps whatever was last typed in this run.
pub fn open_account_modal(store: AppStore, hint: &str) {
    crate::settings::open_at(store, crate::settings::Section::Account);
    store.account_modal_hint.set(hint.to_string());
}

/// The Account page is shown (Settings, docs/SETTINGS_CONTRACT.md section 2):
/// it starts clean, with no hint unless whoever opened it gives one.
pub(crate) fn account_shown(store: AppStore) {
    store.account_modal_error.set(String::new());
    store.account_modal_busy.set(false);
    store.account_modal_password.set(String::new());
    store.account_modal_password_visible.set(false);
    store.account_modal_hint.set(String::new());
    if untracked(|| store.account_modal_url.get()).is_empty() {
        store.account_modal_url.set("https://pimble.app".to_string());
    }
}

/// Settings closed: a typed password does not outlive it.
pub(crate) fn account_hidden(store: AppStore) {
    store.account_modal_error.set(String::new());
    store.account_modal_password.set(String::new());
    store.account_modal_hint.set(String::new());
}

/// Whether the Account page is showing.
pub(crate) fn account_showing(store: AppStore) -> bool {
    crate::settings::showing(store, crate::settings::Section::Account)
}

/// "Host on Pimble Cloud..." for `store_id`: the confirmation when an account
/// is signed in, the Account modal with a hint otherwise (decision 5).
pub fn open_host_modal(store: AppStore, store_id: pimble_core::StoreId) {
    if !untracked(|| store.cloud_signed_in.get()) {
        open_account_modal(store, "Sign in to host a store");
        return;
    }
    store.host_modal_error.set(String::new());
    store.host_modal_busy.set(false);
    store.host_modal_store.set(Some(store_id));
}

/// "Stop Sharing from This Computer..." for `store_id`
/// (docs/RELAY_CONTRACT.md): the confirmation, which sends
/// `CloudStopRelaying`. The server refuses it while the store still has
/// shares, and its sentence lands on the confirmation's error line.
pub fn open_stop_relay_modal(store: AppStore, store_id: pimble_core::StoreId) {
    store.stop_relay_modal_error.set(String::new());
    store.stop_relay_modal_busy.set(false);
    store.stop_relay_modal_store.set(Some(store_id));
}

/// "Recently Deleted..." for `store_id` (docs/MOVE_CONTRACT.md "Seeing and
/// undoing what was removed"): the store row's context menu already knows
/// which store. Clears the last list before asking, so a reopen never shows
/// a stale one while the answer is on its way.
pub fn open_deleted_modal(store: AppStore, store_id: pimble_core::StoreId) {
    store.deleted_modal_store.set(Some(store_id));
    store.deleted_modal_nodes.set(Vec::new());
    store.deleted_modal_error.set(String::new());
    store.deleted_modal_pending.set(true);
    store.send(BackendCommand::ListDeleted { store_id });
}

/// View > "Recently Deleted...": the store of the current selection (a node
/// or a store root), else the first open store — there is always one to ask
/// while any store is open.
pub fn open_deleted_modal_for_selection(store: AppStore) {
    let selected_store = untracked(|| store.selected_id.get())
        .and_then(|value| parse_tree_value(&value))
        .map(|(store_id, _)| store_id);
    let target = selected_store.or_else(|| untracked(|| store.store_ids.get().first().copied()));
    if let Some(store_id) = target {
        open_deleted_modal(store, store_id);
    }
}

/// "Share from this computer", the first of the Share modal's two ways for a
/// store that is not hosted (docs/RELAY_CONTRACT.md, "The apps"): the person
/// chose it, so the store is shared from here (`CloudRelayStore`; nothing is
/// uploaded), and `CloudStoreRelayed` moves the modal on to the share's name.
/// A refusal is the server's sentence on the modal's error line.
pub fn share_from_this_computer(store: AppStore) {
    let Some((store_id, _)) = untracked(|| store.share_modal_node.get()) else { return };
    store.share_modal_error.set(String::new());
    store.share_modal_pending.set(Some(crate::protocol::CloudOp::RelayStore));
    store.send(BackendCommand::CloudRelayStore { store_id });
}

/// "Host on Pimble Cloud...", the second way: the hosting confirmation the
/// store row's menu opens, which says what hosting does and asks before
/// anything is uploaded. The Share modal makes way for it and remembers its
/// node, so that a store the person then hosts carries on into the share
/// (`CloudStoreHosted`); closing the confirmation forgets it.
pub fn host_to_share(store: AppStore) {
    let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
    store.share_modal_node.set(None);
    store.share_modal_error.set(String::new());
    store.share_modal_pending.set(None);
    // Nothing to carry on from when the confirmation does not open (nobody
    // is signed in, and the Account modal opened in its place).
    let signed_in = untracked(|| store.cloud_signed_in.get());
    store.share_after_host.set(signed_in.then_some((store_id, node_id)));
    open_host_modal(store, store_id);
}

/// Open the "Add Hosted Store..." modal and ask for the account's stores at
/// once (decision 6). Not signed in, the modal says so and offers the
/// Account modal instead of a list.
pub fn open_hosted_modal(store: AppStore) {
    store.hosted_modal_stores.set(Vec::new());
    store.hosted_modal_relayed.set(Vec::new());
    store.hosted_modal_selected.set(String::new());
    store.hosted_modal_error.set(String::new());
    let signed_in = untracked(|| store.cloud_signed_in.get());
    store.hosted_modal_busy.set(signed_in);
    store.hosted_modal_open.set(true);
    if signed_in {
        store.send(BackendCommand::CloudListHostedStores);
    }
}

/// "Share..." for `(store_id, node_id)` (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5): a share is a scoped grant on this store — the node and the
/// documents under it — so it needs an account, and the Account modal opens
/// with a hint when there is none, the same rule "Host on Pimble Cloud..."
/// follows.
///
/// The node's own share marker decides which face opens, so a node already
/// shared shows its share at once; `CloudShareInfo` then fills in the members
/// the accounts service has. A share carries a name of its own, seeded here
/// from the node's title because that is usually what the owner would type.
/// In a store that is neither hosted nor shared from this computer the modal
/// opens on the two ways to share from it instead
/// (`crate::state::ShareFace::Ways`, docs/RELAY_CONTRACT.md), and moves on to
/// the name once the person has chosen one and it is done.
///
/// In a store that reached this device as someone else's share nothing can be
/// shared from here (`share_item_disabled` keeps the menu item off every node
/// but a shared root), and a shared root opens on the member's face: whose
/// share it is and who is on it, read only (`AppStore::share_modal_manages`).
/// That face is drawn from what this device already holds, so it needs no
/// sign-in; the member list is asked for when there is an account to ask as.
pub fn open_share_modal(store: AppStore, store_id: pimble_core::StoreId, node_id: pimble_core::NodeId) {
    let marker = store
        .get_node_signal(store_id, node_id)
        .and_then(|sig| untracked(|| sig.with(|n| n.metadata.share())));
    let held_as_share = store.shared_by(store_id).is_some();
    if held_as_share && marker.is_none() {
        // The menu never offers this; if something else asks, it hears the
        // reason rather than a form the server would refuse.
        crate::events::show_notice(store, crate::state::ONLY_AN_OWNER_SHARES_NOTE.to_string());
        return;
    }
    let signed_in = untracked(|| store.cloud_signed_in.get());
    if !signed_in && !held_as_share {
        open_account_modal(store, "Sign in to share a node");
        return;
    }
    let ask_for_members = marker.is_some() && signed_in;
    let name = match &marker {
        Some(marker) => marker.name.clone(),
        None => store.display_label(store_id, node_id),
    };
    store.share_modal_shared.set(marker.is_some());
    store.share_modal_name.set(name);
    store.share_modal_members.set(Vec::new());
    store.share_modal_state.set(String::new());
    store.share_modal_invite_email.set(String::new());
    store.share_modal_invite_role.set("editor".to_string());
    store.share_modal_error.set(String::new());
    store.share_modal_confirm_stop.set(false);
    store.share_modal_pending.set(ask_for_members.then_some(crate::protocol::CloudOp::ShareInfo));
    store.share_modal_node.set(Some((store_id, node_id)));
    if ask_for_members {
        store.send(BackendCommand::CloudShareInfo { store_id, node_id });
    }
}

/// How a hosted store reads in the "Add Hosted Store..." list. A row with a
/// `root` is a share — a scope inside someone else's store — so it reads as
/// the share's own name and who shared it, never the owner's store name,
/// which no recipient ever sees (docs/NODE_DOCUMENT_CONTRACT.md section 5,
/// "A share has a name of its own").
///
/// `relayed` is the row's tier (docs/RELAY_CONTRACT.md): such a store is not
/// hosted anywhere, it is served from its owner's computer while that is on,
/// and the row says whose. Pimble Cloud holds no name for one, so a row that
/// is not a share (one's own store on another of one's computers, or a grant
/// on the whole of someone else's) has none to show and reads as where it is.
fn hosted_store_label(info: &pimble_rpc::CloudHostedStoreInfo, relayed: bool) -> String {
    let name = info.name.trim();
    match (relayed, &info.shared_by) {
        (false, Some(email)) if info.root.is_some() => format!("{}, shared by {}", info.name, email),
        (false, _) => info.name.clone(),
        (true, Some(email)) if name.is_empty() => format!("Shared from {email}'s computer"),
        (true, Some(email)) => format!("{name}, shared from {email}'s computer"),
        (true, None) if name.is_empty() => crate::state::RELAYED_PLACEHOLDER_NAME.to_string(),
        (true, None) => format!("{name}, shared from another computer"),
    }
}

/// How far a member is from being able to open the share, in words
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5). An invitation to an address
/// with no account yet, and a member whose key no device of ours has wrapped
/// yet, are both ordinary states rather than failures, so both say what is
/// happening. The key is handed over by an owner's Pimble, which is "your
/// Pimble" only to someone who manages the share (`manages`); a member
/// reading the list is told whose it is.
fn member_status_text(status: pimble_rpc::ShareMemberStatus, manages: bool) -> &'static str {
    match (status, manages) {
        (pimble_rpc::ShareMemberStatus::Invited, _) => "invited, no account yet",
        (pimble_rpc::ShareMemberStatus::WaitingForKey, true) => "waiting for your Pimble to hand over the key",
        (pimble_rpc::ShareMemberStatus::WaitingForKey, false) => "waiting for an owner's Pimble to hand over the key",
        (pimble_rpc::ShareMemberStatus::Active, _) => "active",
    }
}

/// A member's role in words, for the member list. An editor edits everything
/// in the share — the text, the titles, the structure — so "can edit" is the
/// whole of it (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
fn member_role_text(role: pimble_rpc::MemberRole) -> &'static str {
    match role {
        pimble_rpc::MemberRole::Owner => "owner",
        pimble_rpc::MemberRole::Editor => "can edit",
        pimble_rpc::MemberRole::Commenter => "can comment",
        pimble_rpc::MemberRole::Reader => "can read",
    }
}

/// Open the "Appearance..." picker for `(store_id, node_id)`, its icon
/// search cleared. Tags are the Properties panel's.
fn open_appearance_modal(store: AppStore, store_id: pimble_core::StoreId, node_id: pimble_core::NodeId) {
    store.appearance_icon_query.set(String::new());
    store.appearance_modal_node.set(Some((store_id, node_id)));
}

/// Cancel a pending debounce timer and reset the search box to empty/idle.
/// Also drops the search box's Escape interceptor, installed by `oninput`
/// only while the box is non-empty (see there) so it never fights the tree
/// rename feature's own interceptor, which owns the same single global slot.
fn clear_search(store: AppStore) {
    if let Some(handle) = SEARCH_DEBOUNCE.with(|slot| slot.borrow_mut().take()) {
        clear_timeout(handle);
    }
    store.search_query.set(String::new());
    store.search_results.set(SearchState::Idle);
    clear_keyboard_interceptor();
}

/// Debounce search-as-you-type by 250ms (per the step 5 contract) and dispatch
/// a `BackendCommand::Search` over every open store when the timer fires.
fn schedule_search(store: AppStore, query: String) {
    if let Some(handle) = SEARCH_DEBOUNCE.with(|slot| slot.borrow_mut().take()) {
        clear_timeout(handle);
    }
    if query.is_empty() {
        store.search_results.set(SearchState::Idle);
        return;
    }
    let timeout = set_timeout(250, move || {
        SEARCH_DEBOUNCE.with(|slot| { slot.borrow_mut().take(); });
        let stores = untracked(|| store.store_ids.get());
        store.send(BackendCommand::Search { query, stores, limit: 50, purpose: crate::protocol::SearchPurpose::Panel });
    });
    SEARCH_DEBOUNCE.with(|slot| { *slot.borrow_mut() = Some(timeout); });
}

/// One search-result row, pre-resolved to plain fields the results panel
/// renders directly — `SearchResultItem` itself isn't `PartialEq`, which the
/// rsx `for` loop requires (see `rinch:rinch` Rule 9), and pimble-rpc isn't
/// this crate's to change.
#[derive(Clone, PartialEq)]
struct SearchRow {
    key: String,
    value: String,
    title: String,
    store_name: String,
    snippet: String,
    kind: &'static str,
}

/// The link picker's lines (a bare call for the rsx `for`; see
/// `search_rows`).
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
    store.link_picker.with(|view| match view {
        Some(view) if view.rows.is_empty() => crate::link_picker::empty_line(view).to_string(),
        _ => String::new(),
    })
}

/// Resolve raw search hits into display-ready `SearchRow`s (looking up each
/// store's name from already-cached signals — cheap, no new fetch). A plain
/// function so the rsx `for`'s iterable expression is a bare call with no
/// turbofish or closure braces for the macro's brace-stopping parser to trip
/// on (see `parse_expr_before_brace` in rinch-macros).
fn search_rows(store: AppStore, items: Vec<pimble_rpc::SearchResultItem>) -> Vec<SearchRow> {
    items.into_iter().map(|item| SearchRow {
        key: format!("{}_{}", item.store_id, item.node_id),
        value: format!("node_{}_{}", item.store_id, item.node_id),
        title: if item.title.is_empty() { "Untitled".to_string() } else { item.title },
        store_name: store.get_store_signal(item.store_id)
            .map(|sig| untracked(|| sig.with(|s| s.name.clone())))
            .unwrap_or_default(),
        snippet: item.snippet,
        kind: kind_label(&item.kind),
    }).collect()
}

/// A subtle, human-readable label for a search hit's `kind` (the index unit
/// kind the hit's chunk carries: prose, a heading, code, a table row, or a
/// structured field). Empty or unrecognized kinds render nothing.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "prose" | "document" => "",
        "heading" => "heading",
        "code" => "code",
        "table_row" | "table-row" | "tablerow" => "table row",
        "field" => "field",
        "folder" => "folder",
        _ => "",
    }
}

/// How one part of the status bar is doing: its dot's color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Light {
    Good,
    Waiting,
    Bad,
    Quiet,
}

/// One part of the status bar: a dot, a name, a dimmer detail beside it and
/// a sentence on hover. Every part reads the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusPart {
    light: Light,
    detail: String,
    tooltip: String,
}

impl StatusPart {
    fn dot_class(&self) -> &'static str {
        match self.light {
            Light::Good => "pimble-status-bar__dot pimble-status-bar__dot--good",
            Light::Waiting => "pimble-status-bar__dot pimble-status-bar__dot--waiting",
            Light::Bad => "pimble-status-bar__dot pimble-status-bar__dot--bad",
            Light::Quiet => "pimble-status-bar__dot",
        }
    }
}

/// What the first part is: Pimble's server on this computer, or in the
/// browser Pimble Cloud itself.
fn server_part_name() -> &'static str {
    if CAN_ADMINISTER_STORES { "This computer" } else { "Pimble Cloud" }
}

/// The first part, from `AppStore::connection_status` ("Connected",
/// "Disconnected", "Error: ...") and the address connected to.
fn server_part(status: &str, addr: &str) -> StatusPart {
    let place = if CAN_ADMINISTER_STORES { "Pimble's server on this computer" } else { "Pimble Cloud" };
    let shown = url::Url::parse(addr).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| addr.to_string());
    if status == "Connected" {
        let at = if addr.is_empty() { String::new() } else { format!(" at {addr}") };
        StatusPart { light: Light::Good, detail: shown, tooltip: format!("Connected to {place}{at}.") }
    } else if let Some(message) = status.strip_prefix("Error: ") {
        StatusPart { light: Light::Bad, detail: message.to_string(), tooltip: format!("{place}: {message}") }
    } else {
        StatusPart { light: Light::Waiting, detail: "connecting".to_string(), tooltip: format!("Connecting to {place}.") }
    }
}

/// The Pimble Cloud part: who is signed in and how this computer's
/// connections to Pimble Cloud are doing (`pimble_rpc::CloudConnection`).
fn cloud_part(signed_in: bool, email: &str, connection: Option<&pimble_rpc::CloudConnection>) -> StatusPart {
    use pimble_rpc::CloudReach;
    if !signed_in {
        return StatusPart { light: Light::Quiet, detail: "sign in".to_string(), tooltip: "Not signed in. Sign in to host stores on Pimble Cloud and to share.".to_string() };
    }
    let Some(c) = connection else {
        return StatusPart { light: Light::Quiet, detail: email.to_string(), tooltip: format!("Signed in as {email}.") };
    };
    let (light, word) = match c.reach {
        CloudReach::Idle => (Light::Quiet, None),
        CloudReach::Connected => (Light::Good, None),
        CloudReach::Connecting => (Light::Waiting, Some("connecting")),
        CloudReach::Offline => (Light::Bad, Some("offline")),
    };
    let detail = match word {
        Some(word) => format!("{email} · {word}"),
        None => email.to_string(),
    };
    StatusPart { light, detail, tooltip: format!("Signed in as {email}. {}", c.sentence) }
}

/// Text for a linked store's status badge (docs/SYNC_CONTRACT.md "B: app
/// side"). `Conflict` is out of scope for this contract (there are no
/// conflicts to show yet) but still needs a label rather than a match gap.
/// An encrypting vault link says so first (docs/DESKTOP_ACCOUNT_CONTRACT.md
/// decision 4): `encrypted · synced`.
///
/// A store shared from this computer (`relay` is `Owner`,
/// docs/RELAY_CONTRACT.md) has no badge: its link is to its own twin on this
/// machine, so its state says nothing about whether the people it is shared
/// with can reach it, and the nodes shared carry the share icon (Joe,
/// 2026-11-06). A member's row reads as
/// any encrypted store's does ("a member sees no difference"), except that a
/// backend that knows the owner's computer to be off says `owner offline`.
/// Only the browser's knows: a Pimble server reports a member's link as
/// `Offline` whether the owner's computer is off or this one has no network,
/// and the badge does not say of someone else what it cannot tell.
fn sync_badge_text(
    state: &pimble_core::SyncState,
    mode: pimble_core::StoreKind,
    relay: pimble_core::RelaySide,
    owner_offline: bool,
) -> &'static str {
    use pimble_core::{RelaySide, StoreKind, SyncState};
    if owner_offline {
        return "owner offline";
    }
    match (relay, mode, state) {
        (RelaySide::Owner, _, _) => "",
        (_, StoreKind::Plain, SyncState::Offline) => "offline",
        (_, StoreKind::Plain, SyncState::Syncing) => "syncing",
        (_, StoreKind::Plain, SyncState::Synced { .. }) => "synced",
        (_, StoreKind::Plain, SyncState::Conflict { .. }) => "conflict",
        (_, StoreKind::Vault, SyncState::Offline) => "encrypted · offline",
        (_, StoreKind::Vault, SyncState::Syncing) => "encrypted · syncing",
        (_, StoreKind::Vault, SyncState::Synced { .. }) => "encrypted · synced",
        (_, StoreKind::Vault, SyncState::Conflict { .. }) => "encrypted · conflict",
    }
}

/// Walk the DOM subtree to find a `data-oncontextmenu` handler and copy it
/// to `target`. Used to hoist the context menu handler from the invisible
/// ContextMenuTarget (display:contents) to a visible wrapper div.
/// The kind of explorer row a context menu is open on: each kind has its own
/// list of items (docs/MENUS_CONTRACT.md section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RowKind {
    #[default]
    None,
    Store,
    Mount,
    Node,
}

impl RowKind {
    fn of(store: AppStore, value: &str) -> RowKind {
        if value.starts_with("store_") {
            return RowKind::Store;
        }
        match parse_tree_value(value) {
            Some((s_id, Some(n_id))) if store.is_mount(s_id, n_id) => RowKind::Mount,
            Some((_, Some(_))) => RowKind::Node,
            _ => RowKind::None,
        }
    }
}

/// The explorer's one context menu: which row it is aimed at, that row's kind,
/// and what the menu's items need of the row, one signal each. A right press
/// on a row aims it; the menu, built once for the whole tree
/// (`explorer_menu`), reads only those signals. Each is written only when its
/// value changes, so aiming the menu at another row of the same kind touches
/// just the items whose state differs. Before 2026-11-12 every row built a
/// whole menu of its own, about 500 DOM nodes a row, and every change to the
/// tree's shape rebuilt them all.
#[derive(Clone, Copy)]
struct ExplorerMenu {
    row: Signal<Option<String>>,
    kind: Signal<RowKind>,
    note: Signal<String>,
    /// Which of a store row's items are off (`Capabilities::store_row_menu`).
    store_menu: Signal<Option<pimble_core::plan::StoreRowMenu>>,
    can_write_tree: Signal<bool>,
    can_write_mount_source: Signal<bool>,
    can_comment: Signal<bool>,
    share_disabled: Signal<bool>,
    mounts_apply: Signal<bool>,
    cut_off: Signal<bool>,
    paste_off: Signal<bool>,
    delete_off: Signal<bool>,
    /// How many rows the menu acts on: the selection when the row is one of
    /// several selected rows (docs/TREE_CONTRACT.md "Multi-select").
    acts_on: Signal<usize>,
}

/// Write `value` into `signal` only when it differs, so its readers hear only
/// of real changes.
fn put<T: PartialEq + Clone + 'static>(signal: Signal<T>, value: T) {
    if untracked(|| signal.get()) != value {
        signal.set(value);
    }
}

impl ExplorerMenu {
    fn new() -> Self {
        ExplorerMenu {
            row: Signal::new(None),
            kind: Signal::new(RowKind::None),
            note: Signal::new(String::new()),
            store_menu: Signal::new(None),
            can_write_tree: Signal::new(false),
            can_write_mount_source: Signal::new(false),
            can_comment: Signal::new(false),
            share_disabled: Signal::new(true),
            mounts_apply: Signal::new(true),
            cut_off: Signal::new(true),
            paste_off: Signal::new(true),
            delete_off: Signal::new(true),
            acts_on: Signal::new(1),
        }
    }

    /// Aim the menu at `row` (or at no row: a press on the tree's empty
    /// space), judging the row now, as each row's menu once judged it when the
    /// row rendered.
    fn aim(self, store: AppStore, tree_state: UseTreeReturn, row: Option<String>) {
        use pimble_core::plan::FoundRowAction as Fra;
        let value = row.clone().unwrap_or_default();
        let kind = row.as_deref().map_or(RowKind::None, |value| RowKind::of(store, value));
        let judged = untracked(|| RowMenu::of(store, &value));
        let acts_on = untracked(|| tree_state.selected.with(|s| if s.contains(&value) { s.len().max(1) } else { 1 }));
        put(self.kind, kind);
        put(self.row, row);
        put(self.note, if kind == RowKind::Mount { judged.mount_note.clone() } else { judged.note.clone() });
        put(self.store_menu, (kind == RowKind::Store).then_some(judged.store_menu));
        put(self.can_write_tree, judged.can_write_tree);
        put(self.can_write_mount_source, judged.can_write_mount_source);
        put(self.can_comment, judged.can_comment);
        put(self.share_disabled, judged.share_disabled || acts_on > 1);
        put(self.mounts_apply, judged.mounts_apply);
        put(self.cut_off, !judged.can_write_tree || !judged.allows(Fra::Cut));
        put(self.paste_off, !judged.can_write_tree || !judged.allows(Fra::PasteInto));
        put(self.delete_off, !judged.can_write_tree || !judged.allows(Fra::Delete));
        put(self.acts_on, acts_on);
    }

    /// The row the menu is aimed at, read without subscribing (for actions).
    fn target(self) -> String {
        untracked(|| self.row.get()).unwrap_or_default()
    }

    /// A store row's item switch, read reactively.
    fn store_off(self, pick: fn(&pimble_core::plan::StoreRowMenu) -> bool) -> bool {
        self.store_menu.with(|m| m.as_ref().map_or(true, pick))
    }
}

/// What may be done with the row a menu is open on
/// (`pimble_core::plan::capabilities`, one rule for every menu): what this
/// device may change of the row's node (docs/NODE_DOCUMENT_CONTRACT.md section
/// 5, "Roles"), judged per node because one store can hold a root this
/// account reads beside a root it edits; a store row asks about the store.
/// `parse_tree_value` returns the CANONICAL pair, so a node reached through a
/// mount is judged as the node it is in its source store.
#[derive(Clone)]
struct RowMenu {
    /// Which of a store row's items are off (`Capabilities::store_row_menu`).
    store_menu: pimble_core::plan::StoreRowMenu,
    /// The one dimmed line at the top saying why items are off, if any are.
    note: String,
    /// A mount's own top line.
    mount_note: String,
    can_write_tree: bool,
    /// A mount's "New" makes in the mount's SOURCE node, so that node's
    /// access governs that one item.
    can_write_mount_source: bool,
    can_comment: bool,
    share_disabled: bool,
    /// Mounts do not exist in an encrypted store (the server has no tree there).
    mounts_apply: bool,
    /// A row a query metanode found: its place is not changed from here.
    found: bool,
}

impl RowMenu {
    fn of(store: AppStore, value: &str) -> RowMenu {
        let parsed = parse_tree_value(value);
        let is_store_root = value.starts_with("store_");
        let is_mount = !is_store_root && matches!(parsed, Some((s_id, Some(n_id))) if store.is_mount(s_id, n_id));
        let found = crate::state::found_row(value);
        let caps = parsed
            .map(|(s_id, n_id)| pimble_core::plan::capabilities(&store.view(), s_id, n_id))
            .unwrap_or_default();
        let can_write_tree = caps.writable();
        let can_write_mount_source = match parsed {
            Some((s_id, Some(n_id))) if is_mount => store
                .get_mount_signal(s_id, n_id)
                .and_then(|sig| sig.with(|m| m.mount_ref.as_ref().map(|r| (r.source_store, r.source_node))))
                .map_or(can_write_tree, |(source_store, source_node)| store.node_access(source_store, source_node).allows_write()),
            _ => can_write_tree,
        };
        // A browser build has none of these items to explain.
        let relayed_note = if CAN_ADMINISTER_STORES && is_store_root { caps.relayed_note() } else { "" };
        let share_note = if CAN_ADMINISTER_STORES { caps.share_note() } else { "" };
        let access_note = crate::state::access_note(caps.access);
        let found_note = found.as_ref().map(|f| f.refusal.clone()).unwrap_or_default();
        let note = if is_store_root && caps.every_share_ended {
            crate::state::ENDED_REPLICA_NOTE.to_string()
        } else {
            crate::state::menu_note_text(&[found_note.as_str(), access_note, share_note, relayed_note])
        };
        RowMenu {
            store_menu: caps.store_row_menu(false),
            note,
            mount_note: crate::state::menu_note_text(&[access_note, MOUNT_NOTE]),
            can_write_tree,
            can_write_mount_source,
            can_comment: caps.access.allows_comment(),
            share_disabled: !caps.can_share(),
            mounts_apply: parsed.map_or(true, |(s_id, _)| !store.is_vault(s_id)),
            found: found.is_some(),
        }
    }

    /// What a found row allows (`pimble_core::plan::FoundRowAction`, one rule
    /// for the explorer, plugins and the MCP).
    fn allows(&self, action: pimble_core::plan::FoundRowAction) -> bool {
        !self.found || action.allowed()
    }
}

/// The explorer's one context menu around `tree` (`ExplorerMenu`): one list of
/// items per kind of row (docs/MENUS_CONTRACT.md section 3: grouped, what
/// cannot apply left out), each kind's built the first time the menu opens on
/// it. What varies from row to row is read reactively from the aim.
fn explorer_menu(
    __scope: &mut RenderScope,
    store: AppStore,
    tree_state: UseTreeReturn,
    menu: ExplorerMenu,
    tree: NodeHandle,
) -> NodeHandle {
    let row = move || menu.target();
    let parsed = move || parse_tree_value(&row());
    let store_of = move || parsed().map(|(s_id, _)| s_id);
    // The node a store row stands for is its root.
    let node_of = move || parsed().and_then(|(s_id, n_id)| n_id.or_else(|| store.root_node_id(s_id)).map(|n| (s_id, n)));

    // With several rows selected and this one among them, the menu acts on
    // all of them where that makes sense (Delete, Cut, Copy) and turns off
    // what is for one row alone (docs/TREE_CONTRACT.md "Multi-select").
    let several = move || menu.acts_on.get() > 1;

    let on_start_discussion = move || {
        if let Some((s_id, Some(n_id))) = parsed() {
            crate::discussions::start_on(store, store.focused(), s_id, n_id);
        }
    };
    let on_cut = move || {
        let rows: Vec<String> = rows_for_menu(tree_state, &row()).into_iter().filter(|v| matches!(parse_tree_value(v), Some((_, Some(_))))).collect();
        if !rows.is_empty() {
            crate::tree_keys::set_cut(store, rows);
        }
    };
    let on_copy = move || copy_rows(store, &rows_for_menu(tree_state, &row()));
    let on_paste = move || crate::pasting::paste(store, &row());
    let on_close_store = move || {
        if let Some(s_id) = store_of() {
            store.send(BackendCommand::CloseStore { store_id: s_id });
        }
    };
    let on_link_to_remote = move || {
        if let Some(s_id) = store_of() {
            store.link_modal_store.set(Some(s_id));
            store.link_modal_url.set(String::new());
            store.link_modal_token.set(String::new());
            store.link_modal_token_visible.set(false);
            store.link_modal_error.set(String::new());
        }
    };
    let on_unlink_from_remote = move || {
        if let Some(s_id) = store_of() {
            store.send(BackendCommand::SetStoreSync { store_id: s_id, remote: None });
        }
    };
    let on_host_on_cloud = move || {
        if let Some(s_id) = store_of() {
            // Hosting asked for on its own is not on the way to sharing anything.
            store.share_after_host.set(None);
            open_host_modal(store, s_id);
        }
    };
    let on_stop_relaying = move || {
        if let Some(s_id) = store_of() {
            open_stop_relay_modal(store, s_id);
        }
    };
    // "Share...": grant people a scope on this store rooted at this node (a
    // store row shares its root node), so they hold the same documents and
    // co-author them (docs/NODE_DOCUMENT_CONTRACT.md section 5). Never off for
    // a store that is not hosted: the modal opens on the two ways to give the
    // share somewhere to live (docs/RELAY_CONTRACT.md).
    let on_share = move || {
        if let Some((s_id, n_id)) = node_of() {
            open_share_modal(store, s_id, n_id);
        }
    };
    let on_remove_replica = move || {
        if let Some(s_id) = store_of() {
            store.remove_replica_modal_store.set(Some(s_id));
            store.remove_replica_modal_error.set(String::new());
            store.remove_replica_modal_pending.set(false);
        }
    };
    let on_recently_deleted = move || {
        if let Some(s_id) = store_of() {
            open_deleted_modal(store, s_id);
        }
    };
    let on_mount_store = move || {
        if let Some((s_id, pid)) = node_of() {
            #[cfg(feature = "native")]
            {
                let dialog = rinch::dialogs::pick_folder().set_title("Select Store to Mount");
                if let Some(path) = dialog.pick() {
                    let path_str = path.to_string_lossy().to_string();
                    tracing::info!("Mounting store from: {} into {:?}", path_str, s_id);
                    store.send(BackendCommand::OpenStore { path: path_str.clone() });
                    store.pending_mount.set(Some(PendingMount {
                        target_store_id: s_id,
                        target_parent_id: pid,
                        source_path: path_str,
                    }));
                }
            }
            // A browser has no directories to offer, so it picks from the
            // stores the account already grants. The RPC either way is
            // `createMount`, which an editor may call.
            #[cfg(not(feature = "native"))]
            open_mount_picker(store, (s_id, pid));
        }
    };
    // "Mount Remote Store Here...": the connect modal in mount mode with this
    // row's canonical target (a store root's target is its own root node).
    let on_mount_remote_store = move || {
        if let Some(target) = crate::tree_view::create_parent_of_row(store, &row()) {
            open_connect_modal(store, Some(target));
        }
    };
    // The rows the menu acts on, all or none (`pimble_core::plan::can_delete`).
    let on_delete = move || {
        let Some(nodes) = crate::tree_view::deletable(store, &rows_for_menu(tree_state, &row())) else { return };
        for (s_id, n_id) in nodes {
            store.send(BackendCommand::DeleteNode { store_id: s_id, node_id: n_id });
        }
    };
    // "Appearance...": the icon, color and tags picker for this node (a store
    // row edits its root node).
    let on_appearance = move || {
        if let Some((s_id, n_id)) = node_of() {
            open_appearance_modal(store, s_id, n_id);
        }
    };
    let on_rename = move || {
        start_rename(store, tree_state, &row());
    };
    // "Open in Split View" (docs/SPLIT_VIEW_CONTRACT.md decision 2): the
    // focused pane is split to the right and this document opens in the new
    // half. Off with four panes open.
    let on_open_in_split = move || {
        let focused = store.focused();
        if let Some(new) = crate::pane_view::split_pane(store, focused, crate::panes::Direction::Right) {
            open_node_in(store, tree_state, new, row());
        }
    };
    // New: one list, `crate::creating`, made in the row (a mount's source).
    let make = move |kind: &crate::creating::Kind| crate::creating::make_in_row(store, kind, &row());

    let sync_applies = move || !menu.store_off(|s| s.link_to_remote && s.host && s.stop_relaying && s.unlink && s.remove_replica);

    // Open only on a row: a right press on the tree's empty space aims the
    // menu at none, and the menu closes before it is drawn.
    let opened = Signal::new(false);
    let _ = rinch::Effect::new(move || {
        if opened.get() && menu.kind.get() == RowKind::None {
            opened.set(false);
        }
    });

    let target = tree.clone();
    let menu_view = rsx! {
        ContextMenu {
            opened: opened,
            ContextMenuTarget { {tree} }
            ContextMenuDropdown {
                div {
                    class: "pimble-menu-note",
                    style: {move || if menu.note.with(|n| n.is_empty()) { "display: none;" } else { "" }},
                    {move || menu.note.get()}
                }
                if menu.kind.get() == RowKind::Store {
                    DropdownMenuSub {
                        label: "New",
                        left_section: TablerIcon::Plus,
                        disabled: {move || menu.store_off(|s| s.new_node)},
                        for item in crate::creating::kinds(store) {
                            DropdownMenuItem {
                                key: item.label.clone(),
                                left_section: item.icon,
                                onclick: { let kind = item.kind.clone(); move || make(&kind) },
                                {item.label.clone()}
                            }
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Copy,
                        onclick: on_copy,
                        "Copy"
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::Clipboard,
                        disabled: {move || menu.store_off(|s| s.new_node)},
                        onclick: on_paste,
                        "Paste"
                    }
                    DropdownMenuDivider {}
                    if CAN_ADMINISTER_STORES {
                        DropdownMenuItem {
                            left_section: TablerIcon::Share,
                            disabled: {move || menu.store_off(|s| s.share)},
                            onclick: on_share,
                            "Share..."
                        }
                    }
                    // How the store reaches other places: only what can apply
                    // to it now (principle 2), in one submenu, left out when
                    // nothing applies.
                    if CAN_ADMINISTER_STORES && sync_applies() {
                        DropdownMenuSub {
                            label: "Sync",
                            left_section: TablerIcon::Cloud,
                            if !menu.store_off(|s| s.link_to_remote) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Cloud,
                                    onclick: on_link_to_remote,
                                    "Link to Remote..."
                                }
                            }
                            if !menu.store_off(|s| s.host) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudUpload,
                                    onclick: on_host_on_cloud,
                                    "Host on Pimble Cloud..."
                                }
                            }
                            if !menu.store_off(|s| s.stop_relaying) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::ShareOff,
                                    onclick: on_stop_relaying,
                                    "Stop Sharing from This Computer..."
                                }
                            }
                            if !menu.store_off(|s| s.unlink) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Unlink,
                                    onclick: on_unlink_from_remote,
                                    "Unlink from Remote"
                                }
                            }
                            if !menu.store_off(|s| s.remove_replica) {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Trash,
                                    onclick: on_remove_replica,
                                    "Remove Replica..."
                                }
                            }
                        }
                    }
                    if menu.mounts_apply.get() || CAN_ADMINISTER_STORES {
                        DropdownMenuSub {
                            label: "Mount",
                            left_section: TablerIcon::Link,
                            if menu.mounts_apply.get() {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Link,
                                    disabled: {move || menu.store_off(|s| s.mount_store)},
                                    onclick: on_mount_store,
                                    "Mount Store Here..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudDownload,
                                    disabled: {move || menu.store_off(|s| s.mount_remote_store)},
                                    onclick: on_mount_remote_store,
                                    "Mount Remote Store Here..."
                                }
                            }
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Palette,
                        disabled: {move || menu.store_off(|s| s.appearance)},
                        onclick: on_appearance,
                        "Appearance..."
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::History,
                        disabled: {move || menu.store_off(|s| s.recently_deleted)},
                        onclick: on_recently_deleted,
                        "Recently Deleted..."
                    }
                    if CAN_ADMINISTER_STORES {
                        DropdownMenuDivider {}
                        DropdownMenuItem {
                            left_section: TablerIcon::X,
                            onclick: on_close_store,
                            "Close Store"
                        }
                    }
                }
                // New makes in the mount's source, never under the mount node
                // itself (the server rejects a mount node as a parent).
                if menu.kind.get() == RowKind::Mount {
                    DropdownMenuSub {
                        label: "New",
                        left_section: TablerIcon::Plus,
                        disabled: {move || !menu.can_write_mount_source.get()},
                        for item in crate::creating::kinds(store) {
                            DropdownMenuItem {
                                key: item.label.clone(),
                                left_section: item.icon,
                                onclick: { let kind = item.kind.clone(); move || make(&kind) },
                                {item.label.clone()}
                            }
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Palette,
                        disabled: {move || !menu.can_write_tree.get()},
                        onclick: on_appearance,
                        "Appearance..."
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Trash,
                        disabled: {move || !menu.can_write_tree.get()},
                        onclick: on_delete,
                        "Delete"
                    }
                }
                if menu.kind.get() == RowKind::Node {
                    DropdownMenuItem {
                        left_section: TablerIcon::LayoutColumns,
                        disabled: {move || several() || !store.tiling.with(|tiling| tiling.can_split())},
                        onclick: on_open_in_split,
                        "Open in Split View"
                    }
                    DropdownMenuSub {
                        label: "New",
                        left_section: TablerIcon::Plus,
                        disabled: {move || !menu.can_write_tree.get()},
                        for item in crate::creating::kinds(store) {
                            DropdownMenuItem {
                                key: item.label.clone(),
                                left_section: item.icon,
                                onclick: { let kind = item.kind.clone(); move || make(&kind) },
                                {item.label.clone()}
                            }
                        }
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::MessagePlus,
                        // A commenter's too (docs/DISCUSSIONS_CONTRACT.md section 7).
                        disabled: {move || !menu.can_comment.get()},
                        onclick: on_start_discussion,
                        "Discuss"
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Edit,
                        disabled: {move || !menu.can_write_tree.get() || several()},
                        onclick: on_rename,
                        "Rename"
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::Palette,
                        disabled: {move || !menu.can_write_tree.get()},
                        onclick: on_appearance,
                        "Appearance..."
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Scissors,
                        disabled: {move || menu.cut_off.get()},
                        onclick: on_cut,
                        "Cut"
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::Copy,
                        onclick: on_copy,
                        "Copy"
                    }
                    DropdownMenuItem {
                        left_section: TablerIcon::Clipboard,
                        disabled: {move || menu.paste_off.get() || several()},
                        onclick: on_paste,
                        "Paste"
                    }
                    if menu.mounts_apply.get() || CAN_ADMINISTER_STORES {
                        DropdownMenuSub {
                            label: "Mount",
                            left_section: TablerIcon::Link,
                            disabled: {move || menu.paste_off.get()},
                            if menu.mounts_apply.get() {
                                DropdownMenuItem {
                                    left_section: TablerIcon::Link,
                                    onclick: on_mount_store,
                                    "Mount Store Here..."
                                }
                            }
                            if CAN_ADMINISTER_STORES {
                                DropdownMenuItem {
                                    left_section: TablerIcon::CloudDownload,
                                    onclick: on_mount_remote_store,
                                    "Mount Remote Store Here..."
                                }
                            }
                        }
                    }
                    if CAN_ADMINISTER_STORES {
                        DropdownMenuItem {
                            left_section: TablerIcon::Share,
                            disabled: {move || menu.share_disabled.get()},
                            onclick: on_share,
                            "Share..."
                        }
                    }
                    DropdownMenuDivider {}
                    DropdownMenuItem {
                        left_section: TablerIcon::Trash,
                        disabled: {move || menu.delete_off.get()},
                        onclick: on_delete,
                        {move || match menu.acts_on.get() {
                            1 => "Delete".to_string(),
                            n => format!("Delete {n} Nodes"),
                        }}
                    }
                }
            }
        }
    };
    // ContextMenu's own boxes are `display: contents`, invisible to the hit
    // test: the tree's box carries the handler (hoisted from the target).
    hoist_context_menu_handler(&menu_view, &target);
    menu_view
}

fn hoist_context_menu_handler(from: &NodeHandle, target: &NodeHandle) {
    if let Some(handler_id) = find_context_menu_handler(from) {
        target.set_attribute("data-oncontextmenu", &handler_id);
    }
}

fn find_context_menu_handler(node: &NodeHandle) -> Option<String> {
    if let Some(val) = node.get_attribute("data-oncontextmenu") {
        return Some(val);
    }
    for child in node.children() {
        if let Some(val) = find_context_menu_handler(&child) {
            return Some(val);
        }
    }
    None
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
        // Cancel any in-progress rename, optionally committing the change.
        let cancel_last_click = last_click.clone();
        let cancel_rename = move |commit: bool| {
            if let Some(prev_value) = untracked(|| store.renaming_node.get()) {
                if commit {
                    let new_title = rename_title(&prev_value, untracked(|| store.rename_text.get()));
                    if let (Some((sid, Some(nid))), Some(new_title)) = (parse_tree_value(&prev_value), new_title) {
                        store.send(BackendCommand::RenameNode {
                            store_id: sid, node_id: nid, title: new_title,
                        });
                    }
                }
                store.renaming_node.set(None);
                clear_keyboard_interceptor();
            }
            // Reset double-click timer so stale timestamps can't cause
            // a spurious rename on the next click.
            cancel_last_click.set((now_ms(), String::new()));
        };

        // Tree callbacks
        let select_last_click = last_click.clone();
        let cancel_rename_for_select = cancel_rename.clone();
        let on_tree_select = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node selected: {}", value);

            // Double-click on a node → enter rename mode
            {
                let now = now_ms();
                let (prev_time, prev_value) = select_last_click.replace((now, value.clone()));
                let is_double_click = prev_value == value && now - prev_time < DOUBLE_CLICK_MS;

                if is_double_click && value.starts_with("node_") {
                    start_rename(store, tree_state, &value);
                    return;
                }

                // If we were renaming a node, commit it before switching
                cancel_rename_for_select(true);
            }

            // (The previously-edited node's changes already persisted live through
            // the collab delta relay — no explicit save-on-switch is needed.)

            // Update selection and load new node — the same `open_node` a
            // search-result click uses, so there is exactly one way to open one.
            open_node(store, tree_state, value);
        });

        let on_tree_expand = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node expanded: {}", value);
            if let Some((s_id, node_id_opt)) = parse_tree_value(&value) {
                let resolved = node_id_opt.or_else(|| store.root_node_id(s_id));
                if let Some(nid) = resolved {
                    store.expanded.update(|e| { e.insert((s_id, nid)); });
                    let needs_fetch = !store.has_children_loaded(s_id, nid);
                    if needs_fetch {
                        store.send(BackendCommand::GetChildren { store_id: s_id, node_id: nid });
                    }
                }
            }
            // No signal mutation — UseTreeReturn handles expanded state internally
        });

        let on_tree_collapse = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node collapsed: {}", value);
            if let Some((s_id, node_id_opt)) = parse_tree_value(&value) {
                let resolved = node_id_opt.or_else(|| store.root_node_id(s_id));
                if let Some(nid) = resolved {
                    store.expanded.update(|e| { e.remove(&(s_id, nid)); });
                }
            }
        });

        // Build the tree with reactive data_source.
        // ONLY subscribes to tree_structure_version; everything else is untracked.
        let data_source: Rc<dyn Fn() -> Vec<TreeNodeData>> =
            Rc::new(move || {
                let _ = store.tree_structure_version.get(); // structural subscription
                untracked(|| store.build_tree_data_structural())
            });

        // The explorer's one context menu, aimed by a right press on a row.
        let menu = ExplorerMenu::new();

        // Custom render_node with per-node reactive Effects for label/icon/mount
        let render_node_fn: RenderTreeNode = Rc::new(move |payload: &RenderTreeNodePayload, __scope: &mut RenderScope| {
            let node_value = payload.node.value.clone();
            let is_store_root = node_value.starts_with("store_");
            let has_children = payload.has_children;

            // Parse once for reuse
            let parsed = parse_tree_value(&node_value);
            // A row a query metanode found (docs/QUERY_METANODES_CONTRACT.md
            // section 4): the real node, shown where it does not live. Its
            // place is not changed from here (`FoundRowAction`), and the
            // menu's top line says why. A snapshot: the row's data carries
            // it, so the row re-renders when it changes.
            let found = crate::state::found_row(&node_value);
            let found_on = found.is_some();
            let level = payload.level;

            // Determine if this node is a mount point (static — mount status is structural)
            let is_mount = if !is_store_root {
                if let Some((s_id, Some(n_id))) = parsed {
                    store.is_mount(s_id, n_id)
                } else {
                    false
                }
            } else {
                false
            };

            // Capture per-entity signals for reactive label/icon Effects.
            // These are looked up once (untracked) and the inner Signal is captured.
            let node_sig = if !is_store_root {
                if let Some((s_id, Some(n_id))) = parsed {
                    store.get_node_signal(s_id, n_id)
                } else {
                    None
                }
            } else {
                None
            };

            let store_sig = if is_store_root {
                parsed.and_then(|(s_id, _)| store.get_store_signal(s_id))
            } else {
                None
            };

            let mount_sig = if is_mount {
                if let Some((s_id, Some(n_id))) = parsed {
                    store.get_mount_signal(s_id, n_id)
                } else {
                    None
                }
            } else {
                None
            };

            // Sync status (store roots only, docs/SYNC_CONTRACT.md "B: app
            // side"): looked up once (an entry always exists by the time a
            // registered store's row renders — see `ensure_sync_entry`) and
            // read reactively below for the badge.
            let sync_sig = if is_store_root {
                parsed.and_then(|(s_id, _)| store.get_sync_signal(s_id))
            } else {
                None
            };

            // Whether the row's node carries a share marker (a store row takes
            // its root's), for the badge. What the menu may do with a row is
            // judged when the menu opens on it (`RowMenu::of`).
            let caps = parsed
                .map(|(s_id, n_id)| pimble_core::plan::capabilities(&store.view(), s_id, n_id))
                .unwrap_or_default();
            let is_shared_now = caps.shared_root;

            // Choose icon (static — changes only on structural rebuild). Folders
            // are folders even when empty; documents are documents even with
            // children, so the icon follows node_type, not has_children.
            let is_folder = node_sig
                .map(|sig| sig.with(|n| n.node_type == pimble_core::node_types::FOLDER))
                .unwrap_or(has_children);
            // A custom icon or colour from the node's metadata (the "Appearance..."
            // picker, or an import); a store row takes its root node's, and a
            // partial replica's row has none. Snapshotted here like the type
            // icon: the row's `TreeNodeData` carries both, so a change
            // re-renders the row.
            let appearance_sig = if is_store_root {
                parsed.and_then(|(s_id, _)| store.store_row_node(s_id).and_then(|root| store.get_node_signal(s_id, root)))
            } else {
                node_sig
            };
            // A query metanode, a query metanode: violet, its own icon unless
            // the person chose one (section 6).
            let is_query = !is_store_root && node_sig.is_some_and(|sig| sig.with(crate::metanodes::is_query));
            // A metanode with no icon of its own shows its viewer's.
            let viewer_icon = if is_store_root {
                None
            } else {
                node_sig
                    .and_then(|sig| sig.with(|n| n.metadata.metanode.as_ref().map(|m| m.viewer.clone())))
                    .and_then(|viewer| store.viewer_icon(&viewer))
                    .and_then(|name| icon_by_name(&name))
            };
            let (custom_icon, node_color, row_color) = appearance_sig
                .map(|sig| {
                    sig.with(|n| {
                        (
                            n.metadata.icon().and_then(icon_by_name),
                            n.metadata.color().map(String::from),
                            n.metadata.background().map(String::from),
                        )
                    })
                })
                .unwrap_or((None, None, None));
            let icon = if let Some(custom) = custom_icon {
                custom
            } else if is_store_root {
                TablerIcon::Database
            } else if is_mount {
                TablerIcon::Link
            } else if is_query {
                TablerIcon::FilterSearch
            } else if let Some(icon) = viewer_icon {
                icon
            } else if is_folder {
                TablerIcon::Folder
            } else {
                TablerIcon::File
            };
            // The CSS for the node's colour on the current theme; reactive on the
            // theme so a toggle recolours rows in place. One clone per closure
            // (rinch: two closures cannot share one moved String).
            let color_css = move |color: &Option<String>| -> String {
                color.as_ref().map(|c| format!(" color: {};", display_color(c, store.dark_mode.get()))).unwrap_or_default()
            };
            // A query metanode's icon is violet unless the person colored
            // the node; then a small violet mark on the icon says it still
            // gathers.
            let query_mark = is_query && node_color.is_some();
            let query_violet = is_query && node_color.is_none();
            let icon_color = node_color.clone();
            let label_color = node_color;

            let icon_class = if is_mount {
                "rinch-tree__icon rinch-tree__icon--mount"
            } else {
                "rinch-tree__icon"
            };

            let base_label_style = if is_store_root {
                "cursor: default; font-weight: 600; text-transform: uppercase; font-size: 11px; letter-spacing: 0.03em; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;"
            } else {
                // A long title ends in an ellipsis at the row's end, rather
                // than running on under the scrollbar and past the explorer.
                "cursor: default; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;"
            };

            let icon_el = render_tabler_icon(__scope, icon, TablerIconStyle::Outline);

            // The badge a shared node's row carries after its label. Drawn
            // from the render-time snapshot, like the type icon: the row's
            // data holds the marker, so it re-renders when one appears or goes
            // (docs/NODE_DOCUMENT_CONTRACT.md section 5).
            let share_badge: Option<NodeHandle> = if is_shared_now {
                let badge_icon = render_tabler_icon(__scope, TablerIcon::Share, TablerIconStyle::Outline);
                Some(rsx! {
                    span {
                        class: "pimble-tree__share",
                        title: "Shared with other people",
                        {badge_icon}
                    }
                })
            } else {
                None
            };

            // A found row's arrow at its end: it lives elsewhere, and the
            // tooltip says where (section 6).
            let found_arrow: Option<NodeHandle> = match &found {
                Some(f) => {
                    let arrow = render_tabler_icon(__scope, TablerIcon::ArrowUpRight, TablerIconStyle::Outline);
                    let title = if f.lives_in.is_empty() { "Lives elsewhere".to_string() } else { format!("Lives in {}", f.lives_in) };
                    Some(rsx! {
                        span {
                            class: "pimble-tree__found",
                            title: title,
                            style: {move || format!("color: {};", crate::appearance::gathered_violet(store.dark_mode.get()))},
                            {arrow}
                        }
                    })
                }
                None => None,
            };

            // A store someone else shared with this account says so on its
            // row, and what it lets this device do. Reactive on the store's
            // own signal so it appears as soon as the store arrives.
            let shared_by_note: Option<NodeHandle> = if is_store_root {
                // The sidebar is narrow enough to clip this, so the same
                // sentence is the row's tooltip.
                // Once every share of it has ended the words are "no longer
                // shared", in place of whose it was and what it allowed.
                let shared_by_text = move || -> String {
                    store_sig.map(|sig| sig.with(crate::state::shared_by_words)).unwrap_or_default()
                };
                Some(rsx! {
                    span {
                        class: "pimble-tree__shared-by",
                        style: {
                            move || {
                                if shared_by_text().is_empty() { "display: none;" } else { "" }
                            }
                        },
                        title: {move || shared_by_text()},
                        {move || shared_by_text()}
                    }
                })
            } else {
                None
            };

            let nv_submit = node_value.clone();

            // This row's own locally-computed label (see `AppStore::live_label`):
            // the label below subscribes to it and not to any other row's.
            let live_label = if is_store_root {
                None
            } else {
                parsed.and_then(|(s_id, n_id)| Some(store.live_label_signal((s_id, n_id?))))
            };

            // Whether THIS node is being renamed. (A rinch `Memo` re-notifies
            // its readers whenever `renaming_node` changes, equal result or
            // not, so every row's rename-dependent closures re-run on a
            // rename; renames are rare enough for that to be fine.)
            let is_renaming = {
                let nv = node_value.clone();
                Memo::new(move || store.renaming_node.get().as_deref() == Some(&nv))
            };

            let rename_input = rsx! {
                input {
                    r#type: "text",
                    class: "rinch-text-input__input",
                    style: {
                        move || {
                            if is_renaming.get() {
                                "width: 100%; font-size: inherit; padding: 0 2px; height: 22px; line-height: 22px;"
                            } else {
                                "display: none;"
                            }
                        }
                    },
                    value: {move || store.rename_text.get()},
                    oninput: move |value: String| {
                        store.rename_text.set(value);
                    },
                    onsubmit: move || {
                        let new_title = store.rename_text.get();
                        let new_title = rename_title(&nv_submit, new_title);
                        store.renaming_node.set(None);
                        clear_keyboard_interceptor();
                        if let (Some((s_id, Some(n_id))), Some(new_title)) = (parse_tree_value(&nv_submit), new_title) {
                            store.send(BackendCommand::RenameNode {
                                store_id: s_id, node_id: n_id, title: new_title,
                            });
                        }
                    },
                }
            };

            // Register this row's rename box so `start_rename` can focus it
            RENAME_INPUTS.with(|inputs| inputs.borrow_mut().insert(node_value.clone(), rename_input.clone()));
            // A node the keyboard just made is named as soon as its row is
            // drawn (docs/TREE_CONTRACT.md "The keyboard").
            if crate::state::take_pending_rename(&node_value) {
                let value = node_value.clone();
                run_on_main_thread(move || {
                    if start_rename(store, tree_state, &value) {
                        // Typed from nothing, not after the name it was made
                        // with.
                        FRESH_RENAME.with(|f| *f.borrow_mut() = Some(value.clone()));
                        store.rename_text.set(String::new());
                    }
                });
            }

            let row_value = node_value.clone();

            let wrapper = rsx! {
                span {
                    class: "rinch-tree__label",
                    style: "flex: 1; min-width: 0; display: inline-flex; align-items: center;",

                    // The row color: a band across the whole row, the
                    // indent and chevron included, behind the icon and title
                    // (the row is its stacking context, `APP_CSS`).
                    span {
                        class: "pimble-row-band",
                        style: {
                            move || match &row_color {
                                Some(hex) => format!("background: {};", row_background(hex, store.dark_mode.get())),
                                None => "display: none;".to_string(),
                            }
                        },
                    }

                    // Beside a found row, its metanode's guide line drawn
                        // violet: the finds read as one group after its own
                        // children (section 6). Over the Tree's own guide,
                        // which is one line for every child alike.
                    span {
                        class: "pimble-found-guide",
                        style: {
                            move || if found_on {
                                format!(
                                    "left: calc(var(--tree-level-offset) * {} + var(--tree-guide-offset)); background: {};",
                                    level.saturating_sub(1),
                                    crate::appearance::gathered_violet(store.dark_mode.get())
                                )
                            } else {
                                "display: none;".to_string()
                            }
                        },
                    }

                    span {
                        class: icon_class,
                        style: {
                            // Reactive icon opacity: a mount whose source is
                            // out of reach or not reachable yet renders dimmed.
                            || {
                                let dimmed = mount_sig.map_or(false, |ms| {
                                    ms.with(|m| m.mount_state.as_ref().map_or(false, mount_is_dimmed))
                                });
                                let color = if query_violet {
                                    format!(" color: {};", crate::appearance::gathered_violet(store.dark_mode.get()))
                                } else {
                                    color_css(&icon_color)
                                };
                                if dimmed {
                                    format!("width: 1rem; height: 1rem; margin-right: 4px; opacity: 0.4;{color}")
                                } else {
                                    color
                                }
                            }
                        },
                        {icon_el}
                        span {
                            class: "pimble-query-mark",
                            style: {move || if query_mark { format!("background: {};", crate::appearance::gathered_violet(store.dark_mode.get())) } else { "display: none;".to_string() }},
                        }
                    }

                    span {
                        style: {
                            // Reactive label style — hides during rename, dims
                            // a mount whose source is out of reach or not
                            // reachable yet.
                            || {
                                if is_renaming.get() {
                                    return "display: none;".to_string();
                                }
                                let dimmed = mount_sig.map_or(false, |ms| {
                                    ms.with(|m| m.mount_state.as_ref().map_or(false, mount_is_dimmed))
                                });
                                let color = color_css(&label_color);
                                if dimmed {
                                    format!("cursor: default; opacity: 0.4;{color}")
                                } else {
                                    format!("{base_label_style}{color}")
                                }
                            }
                        },

                        // Reactive label text — subscribes to per-node/per-store signal
                        // (and, for a node under active local edit, `live_label`).
                        {move || {
                            let label = if is_store_root {
                                store_sig.map(|s| s.with(|st| st.name.clone()))
                                    .unwrap_or_default()
                            } else {
                                let live = live_label.and_then(|sig| sig.get());
                                live.or_else(|| node_sig.map(|s| s.with(|n| display_label_from_node(n))))
                                    .unwrap_or_else(|| "Untitled".to_string())
                            };

                            if let Some(ms) = mount_sig {
                                let suffix = ms.with(|m| {
                                    m.mount_state.as_ref().map_or("", mount_label_suffix)
                                });
                                if suffix.is_empty() { label } else { format!("{label}{suffix}") }
                            } else {
                                label
                            }
                        }}
                    }

                    {share_badge}
                    {shared_by_note}
                    {found_arrow}

                    // Who else has the note open (docs/PRESENCE_CONTRACT.md
                    // section 6): a dot each, up to three, from the
                    // presence signal, so it follows without a tree rebuild.
                    span {
                        class: "pimble-tree-people",
                        for person in (match parsed {
                            Some((s_id, Some(n_id))) => crate::presence::people_in(store, s_id, n_id).into_iter().take(3).collect::<Vec<_>>(),
                            _ => Vec::new(),
                        }) {
                            PersonDot { key: person.key.clone(), person: person.clone() }
                        }
                    }

                    // How many discussions are on the node
                    // (docs/DISCUSSIONS_CONTRACT.md section 1): read from
                    // its signal, so it follows without a tree rebuild.
                    span {
                        class: "pimble-tree-discussions",
                        title: "Discussions on this node",
                        style: {move || if node_sig.map_or(0, |s| s.with(|n| n.attachments)) > 0 { "" } else { "display: none;" }},
                        span { class: "pimble-tree-discussions__icon", IconGlyph { name: "messages".to_string() } }
                        {move || node_sig.map_or(String::new(), |s| s.with(|n| n.attachments.to_string()))}
                        // Something unread in them (docs/DISCUSSIONS_CONTRACT.md section 8).
                        span {
                            class: "pimble-tree-discussions__unread",
                            style: {move || match parsed {
                                Some((s_id, Some(n_id))) if crate::discussions::host_unread(store, s_id, n_id) > 0 => "",
                                _ => "display: none;",
                            }},
                        }
                    }

                    // Sync status badge (store roots only, linked stores
                    // only) — reactive per store, no tree rebuild
                    // (docs/SYNC_CONTRACT.md "B: app side"). A store whose
                    // owner's computer is known to be off says so whether or
                    // not the backend calls what reaches it a link (the
                    // browser holds no links, docs/RELAY_CONTRACT.md).
                    span {
                        style: {
                            move || {
                                let linked = sync_sig
                                    .map(|sig| sig.with(|(remote, _)| remote.is_some()))
                                    .unwrap_or(false);
                                let owner_offline = parsed
                                    .map_or(false, |(s_id, _)| store.owner_offline.with(|set| set.contains(&s_id)));
                                // A replica whose every share has ended has a
                                // link that can reach nothing: "no longer
                                // shared" is said once, beside the name, and
                                // how that link is doing is not.
                                let ended = store_sig.map_or(false, |sig| sig.with(|s| s.every_share_ended()));
                                // A store shared from this computer has no
                                // badge: its link is to its own twin here, so
                                // it says nothing about the people it is
                                // shared with, and the shared nodes carry the
                                // share icon (Joe, 2026-11-06).
                                let owner = store_sig.map_or(false, |sig| sig.with(|s| s.relay == pimble_core::RelaySide::Owner));
                                if (linked || owner_offline) && !ended && !owner {
                                    "margin-left: 6px; cursor: default; font-size: 10px; font-weight: 400; \
                                     text-transform: none; letter-spacing: normal; opacity: 0.6;"
                                } else {
                                    "display: none;"
                                }
                            }
                        },
                        {move || {
                            // The mode is the store's own signal (decision 4),
                            // read here so the badge follows a new vault link;
                            // so is which end of the relay this device is.
                            let (mode, relay) = store_sig
                                .map(|sig| sig.with(|s| (s.sync_mode, s.relay)))
                                .unwrap_or_default();
                            let owner_offline = parsed
                                .map_or(false, |(s_id, _)| store.owner_offline.with(|set| set.contains(&s_id)));
                            sync_sig
                                .map(|sig| sig.with(|(_, state)| sync_badge_text(state, mode, relay, owner_offline).to_string()))
                                .unwrap_or_default()
                        }}
                    }

                    {rename_input}
                }
            };

            // Remember this row for `reveal_row`, and scroll to it now if it
            // is the one a followed link revealed.
            TREE_ROWS.with(|rows| rows.borrow_mut().insert(row_value.clone(), wrapper.clone()));
            if REVEAL_ROW.with(|pending| pending.borrow().as_deref() == Some(row_value.as_str())) {
                REVEAL_ROW.with(|pending| pending.borrow_mut().take());
                wrapper.scroll_into_view();
            }

            // The explorer has ONE context menu (`explorer_menu`), built once
            // for the whole tree: a right press on a row only names the row it
            // is for, so a row carries no menu of its own (2026-11-12: each
            // row built its full menu, about 500 DOM nodes, and every change
            // to the tree's shape rebuilt them all).
            let row_for_menu = node_value.clone();
            let hit_area = rsx! {
                div {
                    // `min-width: 0`: a flex item never narrower than its content
                    // pushed a long title out past the row (and the scrollbar).
                    style: "flex: 1; min-width: 0; display: flex; align-items: center; border-radius: 2px;",
                    onmousedown: move || {
                        if rinch::core::get_click_context().button == rinch::core::MouseButton::Right {
                            menu.aim(store, tree_state, Some(row_for_menu.clone()));
                        }
                    },
                    {wrapper}
                }
            };
            // Image files dropped on the row from the OS become image nodes
            // under it, one per file, in order (docs/IMAGE_MARKUP_CONTRACT.md
            // section 1). The browser finds the row by this attribute
            // (`boards::install_browser_handlers`).
            hit_area.set_attribute("data-pimble-row", &node_value);
            #[cfg(feature = "native")]
            {
                let nv = node_value.clone();
                let handler = __scope.register_file_drop_handler(move |paths: Vec<std::path::PathBuf>| {
                    if let Some((s_id, parent)) = crate::tree_view::create_parent_of_row(store, &nv) {
                        crate::boards::add_image_files(store, crate::boards::Dest::Under(s_id, parent), paths);
                    }
                });
                hit_area.set_attribute("data-onfiledrop", &handler.0.to_string());
            }
            hit_area
        });

        // Drops (docs/TREE_CONTRACT.md "The drop model"): `tree_drop` judges
        // each one the Tree offers, the strip at the foot of the explorer
        // says where it lands, and a release sends what was planned.
        let tree_drag_values: rinch::prelude::DragValues = Rc::new(crate::tree_drop::drag_values);
        let tree_judge_drop: rinch::prelude::JudgeDrop =
            Rc::new(move |drop: &TreeDrop| crate::tree_drop::plan_drop(store, drop).verdict());
        let on_tree_drop_change = ValueCallback::new(move |offer: Option<(TreeDrop, DropVerdict)>| {
            store.drop_hint.set(offer.map(|(drop, _)| crate::tree_drop::plan_drop(store, &drop).hint().clone()));
        });
        let on_tree_drop = ValueCallback::new(move |drop: TreeDrop| {
            crate::tree_drop::send_plan(store, crate::tree_drop::plan_drop(store, &drop));
        });
        // A row dragged out of the tree onto a board becomes a node card
        // there (docs/IMAGE_MARKUP_CONTRACT.md section 4).
        let on_tree_drag_start = ValueCallback::new(move |values: Vec<String>| {
            if let Some(value) = values.into_iter().next() {
                drag_ctx.set(value);
            }
        });
        let on_tree_drag_end = ValueCallback::new(move |()| {
            drag_ctx.clear();
            store.drop_hint.set(None);
        });

        // The keyboard (docs/TREE_CONTRACT.md "The keyboard"): the Tree moves
        // the cursor; `tree_keys` binds Pimble's keys; the document follows
        // the cursor once it rests (Joe, 2026-10-19).
        let tree_onkey: rinch::prelude::TreeKeyHandler =
            Rc::new(move |key: &rinch::prelude::TreeKey| crate::tree_keys::on_key(store, tree_state, key));
        let on_tree_cursor = ValueCallback::new(move |value: String| {
            // A cursor that rests on one of several selected rows (the end
            // of a Shift run) opens nothing: opening selects the row alone.
            // Off the selection it opens the row, as a click would.
            let selected = untracked(|| tree_state.selected.get());
            if selected.len() > 1 && selected.contains(&value) {
                return;
            }
            if untracked(|| store.selected_id.get()).as_deref() != Some(value.as_str()) {
                open_node(store, tree_state, value);
            }
        });
        let tree_label_of: rinch::prelude::LabelOf = Rc::new(move |value: &str| crate::tree_keys::label_of(store, value));

        let tree_scroll = rsx! {
            div {
                class: "pimble-sidebar__tree",
                // A right press on the tree's empty space aims the menu at no
                // row (a row's own press, nearer, aims it at the row).
                onmousedown: move || {
                    if rinch::core::get_click_context().button == rinch::core::MouseButton::Right {
                        menu.aim(store, tree_state, None);
                    }
                },
                Tree {
                    data: untracked(|| store.build_tree_data_structural()),
                    tree: tree_state,
                    data_source: data_source,
                    indent: TREE_INDENT,
                    guides: true,
                    select_on_click: true,
                    expand_on_click: false,
                    onselect: on_tree_select,
                    onexpand: on_tree_expand,
                    oncollapse: on_tree_collapse,
                    render_node: render_node_fn,
                    draggable: true,
                    drag_values: tree_drag_values,
                    judge_drop: tree_judge_drop,
                    ondropchange: on_tree_drop_change,
                    ondrop: on_tree_drop,
                    ondragstart: on_tree_drag_start,
                    ondragend: on_tree_drag_end,
                    keyboard: true,
                    handle_key: tree_onkey,
                    oncursor: on_tree_cursor,
                    label_of: tree_label_of,
                }
            }
        };
        let tree_scroll = explorer_menu(__scope, store, tree_state, menu, tree_scroll);

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

        // The split view: one to four panes beside the explorer, each with
        // its own title strip, toolbar and editor
        // (docs/SPLIT_VIEW_CONTRACT.md; `pane_view.rs` draws it).
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

        // ── Search box + results panel ────────────────────────────────
        // The results panel replaces the tree while `search_query` is
        // non-empty; Esc (via `clear_search`'s interceptor) restores it.
        let search_icon = render_tabler_icon(__scope, TablerIcon::Search, TablerIconStyle::Outline);
        let search_input = rsx! {
            input {
                r#type: "text",
                class: "pimble-search-bar__input",
                placeholder: format!("Search... ({})", crate::shortcuts::chord(crate::shortcuts::SEARCH)),
                // Raw elements have no special "value_fn" prop — that's a
                // `TextInput`-component-only field. On a raw `<input>` any
                // closure-valued prop just becomes a reactively-set attribute
                // named literally after the prop, so `value_fn` was writing a
                // phantom `value_fn` attribute the renderer never reads
                // (`paint_input_value` only reads `"value"`). Bind `value`
                // itself so clearing the signal actually clears the box.
                value: move || store.search_query.get(),
                oninput: move |val: String| {
                    let was_empty = untracked(|| store.search_query.get()).is_empty();
                    let now_empty = val.is_empty();
                    store.search_query.set(val.clone());
                    if was_empty && !now_empty {
                        // Only while the box is non-empty — see `clear_search`.
                        set_keyboard_interceptor(move |data| {
                            if data.key == "Escape" {
                                run_on_main_thread(move || clear_search(store));
                                return true;
                            }
                            false
                        });
                    } else if !was_empty && now_empty {
                        clear_keyboard_interceptor();
                    }
                    schedule_search(store, val);
                },
                onsubmit: move || {
                    let first = untracked(|| match store.search_results.get() {
                        SearchState::Results(items) => items.into_iter().next(),
                        _ => None,
                    });
                    if let Some(item) = first {
                        let value = format!("node_{}_{}", item.store_id, item.node_id);
                        clear_search(store);
                        open_node(store, tree_state, value);
                    }
                },
            }
        };
        SEARCH_INPUT.with(|cell| { *cell.borrow_mut() = Some(search_input.clone()); });

        let search_bar = rsx! {
            div {
                class: "pimble-search-bar",
                // The field itself, a box in the header band, so it reads as
                // something to type in.
                div {
                    class: "pimble-search-bar__field",
                    span { class: "pimble-search-bar__icon", {search_icon} }
                    {search_input}
                    if !store.search_query.get().is_empty() {
                        div {
                            title: "Clear the search",
                            style: "display: inline-flex;",
                            ActionIcon {
                                icon: TablerIcon::X,
                                variant: "subtle",
                                size: "xs",
                                class: "pimble-search-bar__clear",
                                onclick: move || clear_search(store),
                            }
                        }
                    }
                }
            }
        };

        let search_panel = rsx! {
            div {
                class: "pimble-search-results",
                match store.search_results.get() {
                    SearchState::Idle => "",
                    SearchState::Error(_message) => div {
                        class: "pimble-search-results__message", {_message}
                    },
                    SearchState::Results(items) if items.is_empty() => div {
                        class: "pimble-search-results__message", "No results"
                    },
                    SearchState::Results(_items) => div {
                        for row in search_rows(store, _items.clone()) {
                            div {
                                key: row.key.clone(),
                                class: "pimble-search-result",
                                onclick: {
                                    let value = row.value.clone();
                                    move || {
                                        clear_search(store);
                                        open_node(store, tree_state, value.clone());
                                    }
                                },
                                div {
                                    class: "pimble-search-result__title-row",
                                    span { class: "pimble-search-result__title", {row.title.clone()} }
                                    if !row.kind.is_empty() {
                                        span { class: "pimble-search-result__kind", {row.kind} }
                                    }
                                }
                                div { class: "pimble-search-result__store", {row.store_name.clone()} }
                                div { class: "pimble-search-result__snippet", {row.snippet.clone()} }
                            }
                        }
                    }
                }
            }
        };

        // ── "Add Remote Store..." modal (File menu) ─────────────────────
        // docs/SYNC_CONTRACT.md "B: app side". No folder/path field: the
        // server places the replica in its own data directory, so nothing
        // in this flow opens a native OS dialog (the rinch debug tools can
        // drive it end to end).
        //
        // The same modal serves "Mount Remote Store Here..." when
        // `connect_modal_target` names a target node: its action adds the
        // replica (unless the store is already open here) and then mounts it
        // there, in one command (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 9).
        let connect_modal = rsx! {
            Modal {
                opened_fn: move || store.connect_modal_open.get(),
                onclose: move || {
                    store.connect_modal_open.set(false);
                    store.connect_modal_error.set(String::new());
                    store.connect_modal_target.set(None);
                },
                title: {|| if store.connect_modal_target.get().is_some() {
                    "Mount Remote Store"
                } else {
                    "Add Remote Store"
                }},
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    TextInput {
                        label: "Server URL",
                        placeholder: "http://host:7462",
                        value_fn: move || store.connect_modal_url.get(),
                        oninput: move |val: String| store.connect_modal_url.set(val),
                    }

                    PasswordInput {
                        label: "Token",
                        placeholder: "Leave empty to use a saved token",
                        value_fn: move || store.connect_modal_token.get(),
                        oninput: move |val: String| store.connect_modal_token.set(val),
                        visible_fn: move || store.connect_modal_token_visible.get(),
                        ontoggle: move || store.connect_modal_token_visible.update(|v| *v = !*v),
                    }

                    Button {
                        variant: "light",
                        size: "sm",
                        disabled: {|| store.connect_modal_busy.get()},
                        onclick: move || {
                            let url = untracked(|| store.connect_modal_url.get());
                            let token = untracked(|| store.connect_modal_token.get());
                            store.connect_modal_busy.set(true);
                            store.connect_modal_error.set(String::new());
                            store.send(BackendCommand::ListRemoteStores { url, token });
                        },
                        "List stores"
                    }

                    Select {
                        label: "Store",
                        placeholder: "Select a store",
                        value_fn: move || store.connect_modal_selected.get(),
                        onchange: move |val: String| store.connect_modal_selected.set(val),
                        data: {|| {
                            store.connect_modal_stores.get().iter()
                                .map(|s| SelectOption::new(s.id.to_string(), s.name.clone()))
                                .collect::<Vec<_>>()
                        }},
                    }

                    Button {
                        variant: "filled",
                        size: "sm",
                        disabled: {|| store.connect_modal_busy.get() || store.connect_modal_selected.get().is_empty()},
                        onclick: move || {
                            let selected = untracked(|| store.connect_modal_selected.get());
                            let Ok(remote_uuid) = selected.parse::<uuid::Uuid>() else {
                                store.connect_modal_error.set("Select a store first".to_string());
                                return;
                            };
                            let remote_store_id = pimble_core::StoreId(remote_uuid);
                            let url = untracked(|| store.connect_modal_url.get());
                            let token = untracked(|| store.connect_modal_token.get());
                            // Both modes report their outcome through the same
                            // route: `connect_modal_pending_add` claims the
                            // generic `Error` event for this modal's error line.
                            store.connect_modal_pending_add.set(true);
                            store.connect_modal_busy.set(true);
                            store.connect_modal_error.set(String::new());
                            match untracked(|| store.connect_modal_target.get()) {
                                Some((target_store_id, target_parent_id)) => {
                                    store.send(BackendCommand::MountRemoteStore {
                                        url,
                                        remote_store_id,
                                        token,
                                        target_store_id,
                                        target_parent_id,
                                    });
                                }
                                None => {
                                    store.send(BackendCommand::AddRemoteStore { url, remote_store_id, token });
                                }
                            }
                        },
                        {|| if store.connect_modal_target.get().is_some() { "Mount" } else { "Add" }}
                    }

                    if !store.connect_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.connect_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Link to Remote..." modal (store root context menu) ─────────
        // docs/SYNC_CONTRACT.md "B: app side".
        let link_modal = rsx! {
            Modal {
                opened_fn: move || store.link_modal_store.get().is_some(),
                onclose: move || {
                    store.link_modal_store.set(None);
                    store.link_modal_error.set(String::new());
                },
                title: "Link to Remote",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    TextInput {
                        label: "Server URL",
                        placeholder: "http://host:7462",
                        value_fn: move || store.link_modal_url.get(),
                        oninput: move |val: String| store.link_modal_url.set(val),
                    }

                    PasswordInput {
                        label: "Token",
                        placeholder: "Leave empty to use a saved token",
                        value_fn: move || store.link_modal_token.get(),
                        oninput: move |val: String| store.link_modal_token.set(val),
                        visible_fn: move || store.link_modal_token_visible.get(),
                        ontoggle: move || store.link_modal_token_visible.update(|v| *v = !*v),
                    }

                    Button {
                        variant: "filled",
                        size: "sm",
                        disabled: {|| store.link_modal_pending.get()},
                        onclick: move || {
                            let Some(store_id) = untracked(|| store.link_modal_store.get()) else { return };
                            let url_str = untracked(|| store.link_modal_url.get());
                            let Ok(url) = url_str.parse::<url::Url>() else {
                                store.link_modal_error.set("Invalid URL".to_string());
                                return;
                            };
                            let token = untracked(|| store.link_modal_token.get());
                            let auth = if token.is_empty() {
                                pimble_core::AuthMethod::None
                            } else {
                                pimble_core::AuthMethod::Bearer { token }
                            };
                            store.link_modal_pending.set(true);
                            store.link_modal_error.set(String::new());
                            store.send(BackendCommand::SetStoreSync {
                                store_id,
                                remote: Some(pimble_core::RemoteEndpoint { url, auth }),
                            });
                        },
                        "Link"
                    }

                    if !store.link_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.link_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Remove Replica..." confirmation modal (store root context
        // menu) ──────────────────────────────────────────────────────────
        // docs/history/HARDENING_CONTRACT.md "B: app". Names the store and its
        // remote; when the link isn't `Synced`, warns before removing with
        // `force`.
        // ── "Appearance..." modal (node context menu) ───────────────────
        // Picks a colour and an icon for one node; every click applies at
        // once through `SetNodeAppearance`, and the selection shown follows
        // the node's own signal, so a change from another window shows too.
        let appearance_node_color = move || -> Option<String> {
            let (s_id, n_id) = store.appearance_modal_node.get()?;
            store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.color().map(String::from))
        };
        let appearance_node_icon = move || -> Option<String> {
            let (s_id, n_id) = store.appearance_modal_node.get()?;
            store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.icon().map(String::from))
        };
        let appearance_node_background = move || -> Option<String> {
            let (s_id, n_id) = store.appearance_modal_node.get()?;
            store.get_node_signal(s_id, n_id)?.with(|n| n.metadata.background().map(String::from))
        };
        let send_appearance = move |icon: Option<Option<String>>, color: Option<Option<String>>, background: Option<Option<String>>| {
            if let Some((store_id, node_id)) = untracked(|| store.appearance_modal_node.get()) {
                store.send(BackendCommand::SetNodeAppearance { store_id, node_id, icon, color, background, tags: None });
            }
        };
        let swatches: Vec<NodeHandle> = COLOR_CHOICES
            .iter()
            .map(|&(name, hex)| {
                rsx! {
                    div {
                        class: {move || {
                            if appearance_node_color().as_deref() == Some(hex) { "pimble-swatch pimble-swatch--active" } else { "pimble-swatch" }
                        }},
                        style: format!("background: {hex};"),
                        title: name,
                        onclick: move || send_appearance(None, Some(Some(hex.to_string())), None),
                    }
                }
            })
            .collect();
        let row_swatches: Vec<NodeHandle> = ROW_COLOR_CHOICES
            .iter()
            .map(|&(name, hex)| {
                rsx! {
                    div {
                        class: {move || {
                            if appearance_node_background().as_deref() == Some(hex) { "pimble-swatch pimble-swatch--active" } else { "pimble-swatch" }
                        }},
                        style: format!("background: {hex};"),
                        title: name,
                        onclick: move || send_appearance(None, None, Some(Some(hex.to_string()))),
                    }
                }
            })
            .collect();
        let appearance_modal = rsx! {
            Modal {
                opened_fn: move || store.appearance_modal_node.get().is_some(),
                onclose: move || store.appearance_modal_node.set(None),
                title: "Appearance",
                size: "md",

                div {
                    div { class: "pimble-appearance__label", "Text" }
                    div {
                        class: "pimble-appearance__row",
                        div {
                            class: {move || {
                                if appearance_node_color().is_none() { "pimble-swatch pimble-swatch--none pimble-swatch--active" } else { "pimble-swatch pimble-swatch--none" }
                            }},
                            onclick: move || send_appearance(None, Some(None), None),
                            "None"
                        }
                        {swatches}
                    }
                    // The whole row tinted, as Scrivener's full-width binder
                    // labels are; independent of the text color above.
                    div { class: "pimble-appearance__label", "Background" }
                    div {
                        class: "pimble-appearance__row",
                        div {
                            class: {move || {
                                if appearance_node_background().is_none() { "pimble-swatch pimble-swatch--none pimble-swatch--active" } else { "pimble-swatch pimble-swatch--none" }
                            }},
                            onclick: move || send_appearance(None, None, Some(None)),
                            "None"
                        }
                        {row_swatches}
                    }
                    div { class: "pimble-appearance__label", "Icon" }
                    TextInput {
                        placeholder: "Search all icons...",
                        value_fn: move || store.appearance_icon_query.get(),
                        oninput: move |val: String| store.appearance_icon_query.set(val),
                    }
                    div {
                        class: "pimble-appearance__row",
                        style: "margin-top: 6px;",
                        div {
                            class: {move || {
                                if appearance_node_icon().is_none() { "pimble-swatch pimble-swatch--none pimble-swatch--active" } else { "pimble-swatch pimble-swatch--none" }
                            }},
                            onclick: move || send_appearance(Some(None), None, None),
                            "Default"
                        }
                        // The curated set, or every Tabler icon matching the
                        // search: reactive on the query, one component per icon.
                        for icon in icons_matching(&store.appearance_icon_query.get()) {
                            div {
                                key: icon.name(),
                                class: {move || {
                                    if appearance_node_icon().as_deref() == Some(icon.name()) { "pimble-icon-choice pimble-icon-choice--active" } else { "pimble-icon-choice" }
                                }},
                                title: icon.name(),
                                onclick: move || send_appearance(Some(Some(icon.name().to_string())), None, None),
                                IconGlyph { name: icon.name() }
                            }
                        }
                    }
                    div {
                        style: "margin-top: 14px; display: flex; justify-content: flex-end;",
                        Button {
                            variant: "filled",
                            onclick: move || store.appearance_modal_node.set(None),
                            "Done"
                        }
                    }
                }
            }
        };

        let create_hosted_store = move || {
            let name = untracked(|| store.new_store_modal_name.get()).trim().to_string();
            if name.is_empty() {
                store.new_store_modal_error.set("Give the store a name.".to_string());
                return;
            }
            store.new_store_modal_error.set(String::new());
            store.new_store_modal_pending.set(true);
            store.send(BackendCommand::CreateHostedStore { name, kind: "vault".to_string() });
        };

        // ── "New Store..." modal ────────────────────────────────────────
        // A browser build's stores live on the account, not on disk, so there
        // is no path to pick: a name and a kind is the whole of it. The
        // backend makes the store, mints its key if it is encrypted, and asks
        // for a token that carries the new grant; the store then arrives
        // through `StoresListed` like any other.
        // Paste with several things it could do (`crate::pasting`).
        let paste_chooser = crate::pasting::render_chooser(__scope, store);

        let new_store_modal = rsx! {
            Modal {
                opened_fn: move || store.new_store_modal_open.get(),
                onclose: move || {
                    store.new_store_modal_open.set(false);
                    store.new_store_modal_error.set(String::new());
                },
                title: "New Store",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    TextInput {
                        label: "Name",
                        placeholder: "Notes",
                        value_fn: move || store.new_store_modal_name.get(),
                        oninput: move |val: String| store.new_store_modal_name.set(val),
                        onsubmit: create_hosted_store,
                    }

                    div {
                        style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                        "Encrypted. Its key is made here and sealed to your account; \
                         the server stores what it cannot read."
                    }

                    div {
                        style: {
                            move || if store.new_store_modal_error.get().is_empty() {
                                "display: none;"
                            } else {
                                "color: var(--rinch-color-red-6); font-size: 12px;"
                            }
                        },
                        {|| store.new_store_modal_error.get()}
                    }

                    Button {
                        variant: "filled",
                        size: "sm",
                        loading: {|| store.new_store_modal_pending.get()},
                        disabled: {|| store.new_store_modal_pending.get()},
                        onclick: create_hosted_store,
                        "Create"
                    }
                }
            }
        };

        // ── File > Import: where it goes ────────────────────────────────
        // docs/IMPORT_CONTRACT.md. The files are already read; this only
        // asks for the place: under the selected node (when there is one
        // this device may write), or a new store made for it.
        let import_modal = rsx! {
            Modal {
                opened_fn: move || store.import_modal_open.get(),
                onclose: move || crate::import::cancel(store),
                title: "Import",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    div {
                        style: "font-size: 13px;",
                        {|| store.import_modal_label.get()}
                    }

                    div {
                        style: {
                            move || if store.import_modal_under.get().is_some() { "display: flex; flex-direction: column; gap: 6px;" } else { "display: none;" }
                        },
                        Button {
                            variant: "filled",
                            size: "sm",
                            onclick: move || crate::import::under_selection(store),
                            {|| match store.import_modal_under.get() {
                                Some((_, _, title)) => format!("Import under \"{title}\""),
                                None => String::new(),
                            }}
                        }
                        div {
                            style: "font-size: 12px; color: var(--rinch-color-dimmed); text-align: center;",
                            "or"
                        }
                    }

                    TextInput {
                        label: "New store",
                        value_fn: move || store.import_modal_store_name.get(),
                        oninput: move |val: String| store.import_modal_store_name.set(val),
                        onsubmit: move || crate::import::into_new_store(store),
                    }

                    div {
                        style: {
                            move || if store.import_modal_error.get().is_empty() {
                                "display: none;"
                            } else {
                                "color: var(--rinch-color-red-6); font-size: 12px;"
                            }
                        },
                        {|| store.import_modal_error.get()}
                    }

                    Button {
                        variant: "default",
                        size: "sm",
                        onclick: move || crate::import::into_new_store(store),
                        "Import into a new store"
                    }
                }
            }
        };

        // ── "Mount Store..." picker ─────────────────────────────────────
        // The desktop picks a directory on this machine. A browser has none to
        // pick from: its stores live on the account, so it picks one of those.
        // Either way the RPC is `createMount`, which an editor may call — the
        // picker is a different way to name the same thing, not a lesser one.
        let do_mount_store = move || {
            let Some((target_store, target_parent)) = untracked(|| store.mount_picker_target.get())
            else {
                return;
            };
            let selected = untracked(|| store.mount_picker_selected.get());
            let Ok(uuid) = selected.parse::<uuid::Uuid>() else {
                store.mount_picker_error.set("Choose a store to mount.".to_string());
                return;
            };
            let source_store_id = pimble_core::StoreId(uuid);
            // A store's root is what gets mounted; the source keeps its own name.
            let Some(source_root) = store.root_node_id(source_store_id) else {
                store.mount_picker_error.set("That store has not finished opening.".to_string());
                return;
            };
            let title = store
                .get_store_signal(source_store_id)
                .map(|sig| untracked(|| sig.with(|s| s.name.clone())));

            store.mount_picker_error.set(String::new());
            store.mount_picker_pending.set(true);
            store.send(BackendCommand::CreateMount {
                store_id: target_store,
                parent_id: target_parent,
                source_store_id,
                source_node_id: source_root,
                title,
            });
        };

        let mount_picker_modal = rsx! {
            Modal {
                opened_fn: move || store.mount_picker_target.get().is_some(),
                onclose: move || {
                    store.mount_picker_target.set(None);
                    store.mount_picker_error.set(String::new());
                },
                title: "Mount Store",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    Select {
                        label: "Store",
                        placeholder: "Choose a store",
                        value_fn: move || store.mount_picker_selected.get(),
                        onchange: move |val: String| store.mount_picker_selected.set(val),
                        data: {|| {
                            // Everything open except the one being mounted into:
                            // a store cannot contain itself.
                            let target = store.mount_picker_target.get().map(|(sid, _)| sid);
                            store.store_ids.get().iter()
                                .filter(|sid| Some(**sid) != target)
                                .filter_map(|sid| {
                                    store.get_store_signal(*sid)
                                        .map(|sig| sig.with(|s| SelectOption::new(sid.to_string(), s.name.clone())))
                                })
                                .collect::<Vec<_>>()
                        }},
                    }

                    div {
                        style: {
                            move || if store.mount_picker_error.get().is_empty() {
                                "display: none;"
                            } else {
                                "color: var(--rinch-color-red-6); font-size: 12px;"
                            }
                        },
                        {|| store.mount_picker_error.get()}
                    }

                    Button {
                        variant: "filled",
                        size: "sm",
                        loading: {|| store.mount_picker_pending.get()},
                        disabled: {|| store.mount_picker_pending.get() || store.mount_picker_selected.get().is_empty()},
                        onclick: do_mount_store,
                        "Mount"
                    }
                }
            }
        };

        let remove_replica_modal = rsx! {
            Modal {
                opened_fn: move || store.remove_replica_modal_store.get().is_some(),
                onclose: move || {
                    store.remove_replica_modal_store.set(None);
                    store.remove_replica_modal_error.set(String::new());
                },
                title: "Remove Replica",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    div {
                        {|| {
                            let Some(store_id) = store.remove_replica_modal_store.get() else {
                                return String::new();
                            };
                            let name = store.get_store_signal(store_id)
                                .map(|sig| sig.with(|s| s.name.clone()))
                                .unwrap_or_default();
                            let remote_url = store.get_sync_signal(store_id)
                                .and_then(|sig| sig.with(|(remote, _)| {
                                    remote.as_ref().map(|r| r.url.as_str().trim_end_matches('/').to_string())
                                }));
                            match remote_url {
                                Some(url) => format!("Remove the local copy of \"{}\"? The store stays on {}.", name, url),
                                None => format!("Remove the local copy of \"{}\"?", name),
                            }
                        }}
                    }

                    div {
                        style: {
                            move || {
                                let unsynced = store.remove_replica_modal_store.get()
                                    .and_then(|sid| store.get_sync_signal(sid))
                                    .map(|sig| sig.with(|(_, state)| {
                                        !matches!(state, pimble_core::SyncState::Synced { .. })
                                    }))
                                    .unwrap_or(false);
                                if unsynced {
                                    "color: var(--rinch-color-yellow-6); font-size: 12px;"
                                } else {
                                    "display: none;"
                                }
                            }
                        },
                        "This replica is not synced right now. Anything changed here since it last synced will be lost."
                    }

                    Button {
                        variant: "filled",
                        color: "red",
                        size: "sm",
                        disabled: {|| store.remove_replica_modal_pending.get()},
                        onclick: move || {
                            let Some(store_id) = untracked(|| store.remove_replica_modal_store.get()) else { return };
                            let force = store.get_sync_signal(store_id)
                                .map(|sig| untracked(|| sig.with(|(_, state)| {
                                    !matches!(state, pimble_core::SyncState::Synced { .. })
                                })))
                                .unwrap_or(false);
                            store.remove_replica_modal_pending.set(true);
                            store.remove_replica_modal_error.set(String::new());
                            store.send(BackendCommand::RemoveReplica { store_id, force });
                        },
                        "Remove"
                    }

                    if !store.remove_replica_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.remove_replica_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Recently Deleted..." modal (View menu, store row context menu)
        // ─────────────────────────────────────────────────────────────────
        // docs/MOVE_CONTRACT.md "Seeing and undoing what was removed": the
        // top-most tombstones and the live nodes no list names, one store at
        // a time. `DeletedListed` fills `deleted_modal_nodes`; a "Put Back"
        // re-asks so the row it worked on leaves.
        let put_back_now = move |node_id: pimble_core::NodeId, deleted_at: Option<String>, put_back_under: Option<pimble_core::NodeId>| {
            let Some(store_id) = untracked(|| store.deleted_modal_store.get()) else { return };
            if deleted_at.is_some() {
                // A tombstone: `undeleteNode` brings back what its deletion took.
                store.deleted_modal_pending.set(true);
                store.send(BackendCommand::UndeleteNode { store_id, node_id });
            } else if let Some(new_parent_id) = put_back_under {
                // A live node no list names: put it back under the scope's root.
                store.deleted_modal_pending.set(true);
                store.send(BackendCommand::MoveNode { store_id, node_id, new_parent_id, place: pimble_core::Place::Last });
            } else {
                return;
            }
            store.deleted_modal_pending.set(true);
            store.send(BackendCommand::ListDeleted { store_id });
        };
        let discussions_modal = crate::discussions::render_unread_modal(__scope, store);
        let deleted_modal = rsx! {
            Modal {
                opened_fn: move || store.deleted_modal_store.get().is_some(),
                onclose: move || {
                    store.deleted_modal_store.set(None);
                    store.deleted_modal_error.set(String::new());
                },
                title: {move || {
                    let name = store.deleted_modal_store.get()
                        .and_then(|sid| store.get_store_signal(sid))
                        .map(|sig| sig.with(|s| s.name.clone()))
                        .unwrap_or_default();
                    format!("Recently Deleted in \"{}\"", name)
                }},
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    div {
                        style: {
                            move || if store.deleted_modal_nodes.with(|n| n.is_empty()) {
                                "font-size: 12px; color: var(--rinch-color-dimmed);"
                            } else {
                                "display: none;"
                            }
                        },
                        "Nothing deleted recently."
                    }

                    div {
                        class: "pimble-deleted__list",
                        style: {
                            move || if store.deleted_modal_nodes.with(|n| n.is_empty()) { "display: none;" } else { "" }
                        },
                        for row in store.deleted_modal_nodes.get() {
                            let read_only = store.deleted_modal_store.get()
                                .map(|sid| deleted_row_read_only(store.store_access(sid), &row))
                                .unwrap_or(true);
                            let node_id = row.node_id;
                            let deleted_at = row.deleted_at.clone();
                            let put_back_under = row.put_back_under;
                            let title_text = row.title.clone();
                            let meta_text = deleted_row_meta(&row);
                            div {
                                key: node_id.to_string(),
                                class: "pimble-deleted__row",
                                div {
                                    style: "flex: 1; min-width: 0;",
                                    div {
                                        style: "font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;",
                                        {title_text}
                                    }
                                    div {
                                        style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                                        {meta_text}
                                    }
                                }
                                span {
                                    title: if read_only { pimble_core::StoreAccess::READ_ONLY_REFUSAL } else { "" },
                                    Button {
                                        variant: "light",
                                        size: "xs",
                                        disabled: read_only,
                                        onclick: move || put_back_now(node_id, deleted_at.clone(), put_back_under),
                                        "Put Back"
                                    }
                                }
                            }
                        }
                    }

                    if !store.deleted_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.deleted_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Account..." modal ──────────────────────────────────────────
        // docs/DESKTOP_ACCOUNT_CONTRACT.md decision 1: one modal, two faces.
        // The sign-in form while nothing is signed in; the account and a
        // "Sign Out" button once something is. Both outcomes arrive as
        // `CloudStatusChanged`, which flips the face and leaves the modal
        // open so the person sees it worked.
        #[cfg_attr(not(feature = "native"), allow(unused_variables))]
        let sign_in = move || {
            let url = untracked(|| store.account_modal_url.get()).trim().to_string();
            let email = untracked(|| store.account_modal_email.get()).trim().to_string();
            let password = untracked(|| store.account_modal_password.get());
            if url.is_empty() || email.is_empty() || password.is_empty() {
                store.account_modal_error.set("Fill in the service URL, email and password.".to_string());
                return;
            }
            store.account_modal_error.set(String::new());
            store.account_modal_busy.set(true);
            store.send(BackendCommand::CloudSignIn { url, email, password });
        };
        let about_modal = crate::about::render_about(__scope, store);
        // Settings > Account (docs/SETTINGS_CONTRACT.md section 2): the
        // sign-in form, or who is signed in. The browser signs in through its
        // own pages, so there it is a way to them.
        #[cfg(not(feature = "native"))]
        let account_page = rsx! {
            div {
                class: "pimble-settings__section",
                style: "display: flex; flex-direction: column; gap: 10px; max-width: 420px;",
                div { {|| format!("Signed in as {}", store.cloud_email.get())} }
                Checkbox {
                    label: "Keep what I've read in step",
                    checked_fn: move || store.personal_sync.get(),
                    onchange: move || crate::personal::set_sync_enabled(store, !untracked(|| store.personal_sync.get())),
                }
                div {
                    style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                    "What you have read in discussions, and where you were in each note, sealed so only your devices can open it."
                }
                div {
                    style: "display: flex; gap: 8px;",
                    Button { variant: "default", size: "xs", onclick: run_account_action, "Your Account Page" }
                    Button { variant: "light", size: "xs", onclick: run_sign_out_action, "Sign Out" }
                }
            }
        };
        #[cfg(feature = "native")]
        let account_page = rsx! {
                div {
                    class: "pimble-settings__section",
                    style: "display: flex; flex-direction: column; gap: 10px; max-width: 420px;",

                    if !store.account_modal_hint.get().is_empty() {
                        div {
                            style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                            {|| store.account_modal_hint.get()}
                        }
                    }

                    // The sign-in face.
                    if !store.cloud_signed_in.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            TextInput {
                                label: "Service URL",
                                placeholder: "https://pimble.app",
                                value_fn: move || store.account_modal_url.get(),
                                oninput: move |val: String| store.account_modal_url.set(val),
                            }

                            TextInput {
                                label: "Email",
                                placeholder: "you@example.com",
                                value_fn: move || store.account_modal_email.get(),
                                oninput: move |val: String| store.account_modal_email.set(val),
                            }

                            PasswordInput {
                                label: "Password",
                                value_fn: move || store.account_modal_password.get(),
                                oninput: move |val: String| store.account_modal_password.set(val),
                                visible_fn: move || store.account_modal_password_visible.get(),
                                ontoggle: move || store.account_modal_password_visible.update(|v| *v = !*v),
                            }

                            Button {
                                variant: "filled",
                                size: "sm",
                                loading: {|| store.account_modal_busy.get()},
                                disabled: {|| store.account_modal_busy.get()},
                                onclick: sign_in,
                                "Sign In"
                            }
                        }
                    }

                    // The signed-in face.
                    if store.cloud_signed_in.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            div {
                                {|| format!("Signed in as {}", store.cloud_email.get())}
                            }
                            div {
                                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                                {|| store.cloud_url.get()}
                            }

                            // docs/DISCUSSIONS_CONTRACT.md section 8: signing in
                            // is the asking; this turns it off.
                            Checkbox {
                                label: "Keep what I've read in step",
                                checked_fn: move || store.personal_sync.get(),
                                onchange: move || crate::personal::set_sync_enabled(store, !untracked(|| store.personal_sync.get())),
                            }
                            div {
                                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                                "What you have read in discussions, and where you were in each note, sealed so only your devices can open it."
                            }

                            Button {
                                variant: "light",
                                size: "sm",
                                loading: {|| store.account_modal_busy.get()},
                                disabled: {|| store.account_modal_busy.get()},
                                onclick: move || {
                                    store.account_modal_error.set(String::new());
                                    store.account_modal_busy.set(true);
                                    store.send(BackendCommand::CloudSignOut);
                                },
                                "Sign Out"
                            }
                        }
                    }

                    if !store.account_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.account_modal_error.get()}
                        }
                    }
                }
        };
        let settings_modal = crate::settings::render(__scope, store, account_page);

        // ── "Host on Pimble Cloud..." confirmation (store root context
        // menu) ─────────────────────────────────────────────────────────
        // decision 5: names the store and the account, then `CloudHostStore`.
        // `CloudStoreHosted` closes it; a `CloudError { HostStore }` lands
        // on its error line.
        let host_modal = rsx! {
            Modal {
                opened_fn: move || store.host_modal_store.get().is_some(),
                onclose: move || {
                    store.host_modal_store.set(None);
                    store.host_modal_error.set(String::new());
                    // Not hosted, so there is no share to carry on into.
                    store.share_after_host.set(None);
                },
                title: "Host on Pimble Cloud",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    div {
                        {|| {
                            let Some(store_id) = store.host_modal_store.get() else {
                                return String::new();
                            };
                            let name = store.get_store_signal(store_id)
                                .map(|sig| sig.with(|s| s.name.clone()))
                                .unwrap_or_default();
                            format!("Host \"{}\" on Pimble Cloud as {}?", name, store.cloud_email.get())
                        }}
                    }

                    div {
                        style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                        "Encrypted before it leaves this machine. Its key is made here and \
                         sealed to your account; the server stores what it cannot read."
                    }

                    if !store.host_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.host_modal_error.get()}
                        }
                    }

                    div {
                        style: "display: flex; justify-content: flex-end; gap: 8px;",
                        Button {
                            variant: "light",
                            size: "sm",
                            disabled: {|| store.host_modal_busy.get()},
                            onclick: move || {
                                store.host_modal_store.set(None);
                                store.host_modal_error.set(String::new());
                                store.share_after_host.set(None);
                            },
                            "Cancel"
                        }
                        Button {
                            variant: "filled",
                            size: "sm",
                            loading: {|| store.host_modal_busy.get()},
                            disabled: {|| store.host_modal_busy.get()},
                            onclick: move || {
                                let Some(store_id) = untracked(|| store.host_modal_store.get()) else { return };
                                store.host_modal_error.set(String::new());
                                store.host_modal_busy.set(true);
                                store.send(BackendCommand::CloudHostStore { store_id });
                            },
                            "Host"
                        }
                    }
                }
            }
        };

        // ── "Stop Sharing from This Computer..." confirmation (store root
        // context menu) ─────────────────────────────────────────────────
        // docs/RELAY_CONTRACT.md: the way back from "Share from this
        // computer". Names the store and says what stopping does, then
        // `CloudStopRelaying`. `CloudRelayingStopped` closes it; the server's
        // refusal while the store still has shares (a
        // `CloudError { StopRelaying }`) lands on its error line as it is.
        let stop_relay_modal = rsx! {
            Modal {
                opened_fn: move || store.stop_relay_modal_store.get().is_some(),
                onclose: move || {
                    store.stop_relay_modal_store.set(None);
                    store.stop_relay_modal_error.set(String::new());
                },
                title: "Stop Sharing from This Computer",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    div {
                        {|| {
                            let Some(store_id) = store.stop_relay_modal_store.get() else {
                                return String::new();
                            };
                            let name = store.get_store_signal(store_id)
                                .map(|sig| sig.with(|s| s.name.clone()))
                                .unwrap_or_default();
                            format!("Stop sharing \"{}\" from this computer?", name)
                        }}
                    }

                    div {
                        style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                        "Nobody will reach this store through Pimble Cloud any more, and its \
                         encrypted copy for them on this computer is deleted. The store and \
                         everything in it stay as they are. Each share in it has to be stopped first."
                    }

                    if !store.stop_relay_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.stop_relay_modal_error.get()}
                        }
                    }

                    div {
                        style: "display: flex; justify-content: flex-end; gap: 8px;",
                        Button {
                            variant: "light",
                            size: "sm",
                            disabled: {|| store.stop_relay_modal_busy.get()},
                            onclick: move || {
                                store.stop_relay_modal_store.set(None);
                                store.stop_relay_modal_error.set(String::new());
                            },
                            "Cancel"
                        }
                        Button {
                            variant: "filled",
                            color: "red",
                            size: "sm",
                            loading: {|| store.stop_relay_modal_busy.get()},
                            disabled: {|| store.stop_relay_modal_busy.get()},
                            onclick: move || {
                                let Some(store_id) = untracked(|| store.stop_relay_modal_store.get()) else { return };
                                store.stop_relay_modal_error.set(String::new());
                                store.stop_relay_modal_busy.set(true);
                                store.send(BackendCommand::CloudStopRelaying { store_id });
                            },
                            "Stop Sharing"
                        }
                    }
                }
            }
        };

        // ── "Add Hosted Store..." modal (Account menu) ──────────────────
        // decision 6: the account's encrypted stores and shares not already
        // open here (the event handler filters `CloudHostedStoresListed`),
        // one of which becomes a local replica — a whole one for a store of
        // one's own, a partial one for a share. The store arrives as
        // `StoreOpened`, which closes the modal and carries the roots this
        // device now holds (docs/NODE_DOCUMENT_CONTRACT.md section 5).
        let add_hosted_store = move || {
            let selected = untracked(|| store.hosted_modal_selected.get());
            let Ok(uuid) = selected.parse::<uuid::Uuid>() else {
                store.hosted_modal_error.set("Select a store first".to_string());
                return;
            };
            store.hosted_modal_error.set(String::new());
            store.hosted_modal_busy.set(true);
            store.send(BackendCommand::CloudAddHostedStore { store_id: pimble_core::StoreId(uuid) });
        };
        let hosted_modal = rsx! {
            Modal {
                opened_fn: move || store.hosted_modal_open.get(),
                onclose: move || {
                    store.hosted_modal_open.set(false);
                    store.hosted_modal_error.set(String::new());
                },
                title: "Add Hosted Store",
                size: "sm",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    // Nothing to list without an account.
                    if !store.cloud_signed_in.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",
                            div { "Sign in first." }
                            Button {
                                variant: "filled",
                                size: "sm",
                                onclick: move || {
                                    store.hosted_modal_open.set(false);
                                    store.hosted_modal_error.set(String::new());
                                    open_account_modal(store, "");
                                },
                                "Account..."
                            }
                        }
                    }

                    if store.cloud_signed_in.get() {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            Select {
                                label: "Store",
                                placeholder: "Select a store",
                                value_fn: move || store.hosted_modal_selected.get(),
                                onchange: move |val: String| store.hosted_modal_selected.set(val),
                                data: {|| {
                                    // A row served from its owner's computer
                                    // says so (docs/RELAY_CONTRACT.md).
                                    let relayed = store.hosted_modal_relayed.get();
                                    store.hosted_modal_stores.get().iter()
                                        .map(|s| SelectOption::new(s.store_id.clone(), hosted_store_label(s, relayed.contains(&s.store_id))))
                                        .collect::<Vec<_>>()
                                }},
                            }

                            div {
                                style: {
                                    move || {
                                        let nothing = !store.hosted_modal_busy.get()
                                            && store.hosted_modal_error.get().is_empty()
                                            && store.hosted_modal_stores.with(|s| s.is_empty());
                                        if nothing {
                                            "font-size: 12px; color: var(--rinch-color-dimmed);"
                                        } else {
                                            "display: none;"
                                        }
                                    }
                                },
                                "Every encrypted store and share on this account is already open here."
                            }

                            Button {
                                variant: "filled",
                                size: "sm",
                                loading: {|| store.hosted_modal_busy.get()},
                                disabled: {|| store.hosted_modal_busy.get() || store.hosted_modal_selected.get().is_empty()},
                                onclick: add_hosted_store,
                                "Add"
                            }
                        }
                    }

                    if !store.hosted_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.hosted_modal_error.get()}
                        }
                    }
                }
            }
        };

        // ── "Share..." modal (node context menu) ────────────────────────
        // docs/NODE_DOCUMENT_CONTRACT.md section 5: a share is a scoped grant
        // on this store, so everyone on it edits the same documents. One
        // modal, two faces: a name and "Share" while the node is not shared;
        // the share, its members and "Stop sharing" once it is. A member
        // looking at the share they are in gets a third, read only
        // (`AppStore::share_modal_manages` decides). Every outcome
        // arrives as `CloudShareUpdated`, `CloudSharingStopped` or
        // `CloudError { op }`, so `events.rs` fills the busy and error lines
        // without guessing which request answered (the Account modal's rule,
        // decision 3).
        //
        // A store that is neither hosted nor shared from this computer has
        // nowhere for a share to live, and the server refuses one. So the
        // modal opens on a fourth face there (`ShareFace::Ways`,
        // docs/RELAY_CONTRACT.md "The apps"): the two ways, each with its
        // sentence. "Share from this computer" sends `CloudRelayStore`, and
        // when that answers the face moves on to the name by itself, because
        // it is read from the store's link (`AppStore::share_modal_face`);
        // "Host on Pimble Cloud..." hands over to the hosting confirmation,
        // and a store hosted there opens this modal again on the same node.
        // Nothing is chosen for the person: neither button is filled, neither
        // answers Enter, and closing the modal does neither.
        let share_node_now = move || {
            let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
            let name = untracked(|| store.share_modal_name.get()).trim().to_string();
            if name.is_empty() {
                store.share_modal_error.set("Give the share a name.".to_string());
                return;
            }
            store.share_modal_error.set(String::new());
            store.share_modal_pending.set(Some(CloudOp::Share));
            store.send(BackendCommand::CloudShareNode { store_id, node_id, name });
        };
        let invite_now = move || {
            let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
            let email = untracked(|| store.share_modal_invite_email.get()).trim().to_string();
            if email.is_empty() {
                store.share_modal_error.set("Type the address to invite.".to_string());
                return;
            }
            let role = match untracked(|| store.share_modal_invite_role.get()).as_str() {
                "reader" => pimble_rpc::MemberRole::Reader,
                "commenter" => pimble_rpc::MemberRole::Commenter,
                _ => pimble_rpc::MemberRole::Editor,
            };
            store.share_modal_error.set(String::new());
            store.share_modal_pending.set(Some(CloudOp::ShareInvite));
            store.send(BackendCommand::CloudShareInvite { store_id, node_id, email, role });
        };
        let remove_member = move |email: String| {
            let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
            store.share_modal_error.set(String::new());
            store.share_modal_pending.set(Some(CloudOp::ShareRemoveMember));
            store.send(BackendCommand::CloudShareRemoveMember { store_id, node_id, email });
        };
        let stop_sharing_now = move || {
            let Some((store_id, node_id)) = untracked(|| store.share_modal_node.get()) else { return };
            store.share_modal_error.set(String::new());
            store.share_modal_pending.set(Some(CloudOp::StopSharing));
            store.send(BackendCommand::CloudStopSharing { store_id, node_id });
        };
        let share_modal = rsx! {
            Modal {
                opened_fn: move || store.share_modal_node.get().is_some(),
                onclose: move || {
                    store.share_modal_node.set(None);
                    store.share_modal_error.set(String::new());
                    store.share_modal_confirm_stop.set(false);
                    store.share_modal_pending.set(None);
                },
                title: "Share",
                size: "md",

                div {
                    style: "display: flex; flex-direction: column; gap: 10px;",

                    // The store is neither hosted nor shared from this
                    // computer: the two ways, and the person picks.
                    if store.share_modal_face() == ShareFace::Ways {
                        div {
                            style: "display: flex; flex-direction: column; gap: 14px;",

                            div {
                                class: "pimble-share__note",
                                "This store is not hosted on Pimble Cloud or shared from this computer \
                                 yet. Choose how the people you invite will reach it."
                            }

                            // The same weight for both, and neither filled:
                            // there is no default here.
                            div {
                                class: "pimble-share__way",
                                Button {
                                    variant: "light",
                                    size: "sm",
                                    loading: {|| store.share_modal_pending.get() == Some(CloudOp::RelayStore)},
                                    disabled: {|| store.share_modal_pending.get().is_some() || !store.share_modal_ways_note().is_empty()},
                                    onclick: move || share_from_this_computer(store),
                                    {SHARE_FROM_HERE_LABEL}
                                }
                                div { class: "pimble-share__note", {SHARE_FROM_HERE_SENTENCE} }
                            }

                            div {
                                class: "pimble-share__way",
                                Button {
                                    variant: "light",
                                    size: "sm",
                                    disabled: {|| store.share_modal_pending.get().is_some() || !store.share_modal_ways_note().is_empty()},
                                    onclick: move || host_to_share(store),
                                    {HOST_ON_CLOUD_LABEL}
                                }
                                div { class: "pimble-share__note", {HOST_ON_CLOUD_SENTENCE} }
                            }

                            // A store neither way is open to says why, once.
                            if !store.share_modal_ways_note().is_empty() {
                                div {
                                    class: "pimble-share__note",
                                    {|| store.share_modal_ways_note()}
                                }
                            }
                        }
                    }

                    // The node is not shared yet: name it and share it.
                    if store.share_modal_face() == ShareFace::Name {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            TextInput {
                                label: "Name",
                                placeholder: "Recipes",
                                value_fn: move || store.share_modal_name.get(),
                                oninput: move |val: String| store.share_modal_name.set(val),
                                onsubmit: share_node_now,
                            }

                            div {
                                class: "pimble-share__note",
                                "This name is the share's own: it is what the people you invite \
                                 see, and the name of your store is not. Pimble Cloud sees it \
                                 too. The notes themselves stay encrypted."
                            }

                            div {
                                class: "pimble-share__note",
                                "Everyone you invite to edit gets this folder and everything in \
                                 it, and edits all of it: the writing, the titles, and where \
                                 things sit. Their changes and yours are the same notes."
                            }

                            div {
                                style: "display: flex; justify-content: flex-end;",
                                Button {
                                    variant: "filled",
                                    size: "sm",
                                    loading: {|| store.share_modal_pending.get() == Some(CloudOp::Share)},
                                    disabled: {|| store.share_modal_pending.get().is_some()},
                                    onclick: share_node_now,
                                    "Share"
                                }
                            }
                        }
                    }

                    // The node is shared and this person manages the share:
                    // who has it, and how to stop.
                    if store.share_modal_face() == ShareFace::Owner {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            div {
                                style: "font-weight: 600;",
                                {|| store.share_modal_name.get()}
                            }
                            div {
                                class: "pimble-share__note",
                                {|| store.share_modal_state.get()}
                            }

                            // The confirmation takes the modal over, so that
                            // "Stop sharing" is never one stray click.
                            if store.share_modal_confirm_stop.get() {
                                div {
                                    style: "display: flex; flex-direction: column; gap: 10px;",
                                    // Stopping a share ends the grant. It
                                    // deletes nothing: the notes are the
                                    // owner's own documents and stay where
                                    // they are (docs/NODE_DOCUMENT_CONTRACT.md
                                    // section 5).
                                    div {
                                        {|| format!(
                                            "Stop sharing \"{}\"? The people on it lose access and get nothing \
                                             new from it, and what they have already synced stays on their own \
                                             devices. The notes stay where they are, in your store and hosted \
                                             as before.",
                                            store.share_modal_name.get(),
                                        )}
                                    }
                                    div {
                                        style: "display: flex; justify-content: flex-end; gap: 8px;",
                                        Button {
                                            variant: "light",
                                            size: "sm",
                                            onclick: move || store.share_modal_confirm_stop.set(false),
                                            "Cancel"
                                        }
                                        Button {
                                            variant: "filled",
                                            color: "red",
                                            size: "sm",
                                            loading: {|| store.share_modal_pending.get() == Some(CloudOp::StopSharing)},
                                            disabled: {|| store.share_modal_pending.get().is_some()},
                                            onclick: stop_sharing_now,
                                            "Stop sharing"
                                        }
                                    }
                                }
                            }

                            if !store.share_modal_confirm_stop.get() {
                                div {
                                    style: "display: flex; flex-direction: column; gap: 10px;",

                                    div {
                                        class: "pimble-share__members",
                                        // An empty bordered box would read as
                                        // a list that failed to load; the line
                                        // below says it plainly instead.
                                        style: {
                                            move || if store.share_modal_members.with(|m| m.is_empty()) {
                                                "display: none;"
                                            } else {
                                                ""
                                            }
                                        },
                                        for member in store.share_modal_members.get() {
                                            div {
                                                key: member.email.clone(),
                                                class: "pimble-share__member",
                                                span {
                                                    class: "pimble-share__member-email",
                                                    {crate::state::member_label(&member, true)}
                                                }
                                                span {
                                                    class: "pimble-share__member-status",
                                                    {format!("{}, {}", member_role_text(member.role), member_status_text(member.status, true))}
                                                }
                                                // The owner is the account this
                                                // Pimble is signed in as; there
                                                // is no share without them.
                                                if member.role != pimble_rpc::MemberRole::Owner {
                                                    div {
                                                        title: "Remove from this share",
                                                        style: "display: inline-flex;",
                                                        ActionIcon {
                                                            icon: TablerIcon::X,
                                                            variant: "subtle",
                                                            size: "xs",
                                                            onclick: {
                                                                let email = member.email.clone();
                                                                move || remove_member(email.clone())
                                                            },
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    div {
                                        style: {
                                            move || if store.share_modal_members.with(|m| m.is_empty()) {
                                                "font-size: 12px; color: var(--rinch-color-dimmed);"
                                            } else {
                                                "display: none;"
                                            }
                                        },
                                        "Nobody else yet."
                                    }

                                    div {
                                        class: "pimble-share__invite",
                                        div {
                                            style: "flex: 1;",
                                            TextInput {
                                                label: "Invite",
                                                placeholder: "them@example.com",
                                                value_fn: move || store.share_modal_invite_email.get(),
                                                oninput: move |val: String| store.share_modal_invite_email.set(val),
                                                onsubmit: invite_now,
                                            }
                                        }
                                        Select {
                                            value_fn: move || store.share_modal_invite_role.get(),
                                            onchange: move |val: String| store.share_modal_invite_role.set(val),
                                            data: {|| vec![
                                                SelectOption::new("editor", "Editor"),
                                                // docs/DISCUSSIONS_CONTRACT.md section 7.
                                                SelectOption::new("commenter", "Commenter"),
                                                SelectOption::new("reader", "Reader"),
                                            ]},
                                        }
                                        Button {
                                            variant: "filled",
                                            size: "sm",
                                            loading: {|| store.share_modal_pending.get() == Some(CloudOp::ShareInvite)},
                                            disabled: {|| store.share_modal_pending.get().is_some()},
                                            onclick: invite_now,
                                            "Invite"
                                        }
                                    }

                                    div {
                                        class: "pimble-share__note",
                                        "Someone who can edit changes everything in this folder: the writing, \
                                         the titles, and where things sit. Someone who can read changes nothing."
                                    }

                                    div {
                                        class: "pimble-share__note",
                                        {|| store.share_modal_keeping_note()}
                                    }

                                    div {
                                        style: "display: flex; justify-content: flex-end;",
                                        Button {
                                            variant: "light",
                                            color: "red",
                                            size: "sm",
                                            disabled: {|| store.share_modal_pending.get().is_some()},
                                            onclick: move || store.share_modal_confirm_stop.set(true),
                                            "Stop sharing"
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // The node is shared and the share is someone else's (a
                    // member looking at the share they are in): whose it is
                    // and who is on it, as the accounts service answers a
                    // member. Inviting, removing and stopping are an owner's
                    // and the server refuses them from anyone else, so none
                    // of them is offered, and the owner's state line (what
                    // the owner's devices are handing over) is not this
                    // device's to report. The list is a courtesy: when it
                    // cannot be loaded the sentence stands alone, with no
                    // error line (`events.rs`, `CloudOp::ShareInfo`).
                    if store.share_modal_face() == ShareFace::Member {
                        div {
                            style: "display: flex; flex-direction: column; gap: 10px;",

                            div {
                                style: "font-weight: 600;",
                                {|| store.share_modal_name.get()}
                            }
                            div {
                                class: "pimble-share__note",
                                {|| store.share_modal_shared_by_sentence()}
                            }
                            div {
                                class: "pimble-share__members",
                                style: {
                                    move || if store.share_modal_members.with(|m| m.is_empty()) {
                                        "display: none;"
                                    } else {
                                        ""
                                    }
                                },
                                for member in store.share_modal_members.get() {
                                    div {
                                        key: format!("{}|{}", member.name, member.email),
                                        class: "pimble-share__member",
                                        span {
                                            class: "pimble-share__member-email",
                                            {crate::state::member_label(&member, false)}
                                        }
                                        span {
                                            class: "pimble-share__member-status",
                                            {format!("{}, {}", member_role_text(member.role), member_status_text(member.status, false))}
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if !store.share_modal_error.get().is_empty() {
                        div {
                            style: "color: var(--rinch-color-red-6); font-size: 12px;",
                            {|| store.share_modal_error.get()}
                        }
                    }
                }
            }
        };

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

        // The page itself, with no window chrome around it: identical on both
        // targets. Only the frame differs — a borderless native window that
        // draws its own titlebar, or the browser's viewport.
        let body = rsx! {
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
                            span { class: {move || server_part(&store.connection_status.get(), &store.server_addr.get()).dot_class()} }
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
                                title: {move || cloud_part(store.cloud_signed_in.get(), &store.cloud_email.get(), store.cloud_connection.get().as_ref()).tooltip},
                                onclick: move || open_account_modal(store, ""),
                                span { class: {move || cloud_part(store.cloud_signed_in.get(), &store.cloud_email.get(), store.cloud_connection.get().as_ref()).dot_class()} }
                                span { class: "pimble-status-bar__name", "Pimble Cloud" }
                                span { class: "pimble-status-bar__detail", {move || cloud_part(store.cloud_signed_in.get(), &store.cloud_email.get(), store.cloud_connection.get().as_ref()).detail} }
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

                        div { style: "flex: 1;", }

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
                        div { class: "pimble-link-tooltip__hint", {crate::links::OPEN_HINT} }
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
        };

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
                on_close: || close_window(),

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

/// A link from outside reached this window: show it. (rinch has no way yet
/// to raise and focus a window; until it does, the window is made visible
/// where it is.)
#[cfg(feature = "native")]
pub(crate) fn bring_to_front() {
    show_current_window();
}

/// Open the desktop window: the same UI [`build_view`] builds, wrapped in a
/// native menu bar, the borderless window and the saved theme. `renderer` is
/// the GPU with a fall back to software unless the command line chose
/// (`pimble --cpu`); `RINCH_RENDERER` overrides either.
#[cfg(feature = "native")]
pub fn run(renderer: Renderer) {
    use std::sync::Arc;

    let (store, app_component) = build_view();
    crate::update::start(store);

    // One spec, two shells: `crates/pimble-app/src/menus.rs` says what the
    // menus hold and which target each item belongs on, so the browser's menu
    // bar cannot drift from this one.
    let menus = crate::menus::build_menus(store);

    // Theme
    let theme = theme_props(untracked(|| store.dark_mode.get()));

    // Save-on-close (an OS close; the app's own close button and File >
    // Exit go through `close_window`): register a thread-local that the close callback invokes.
    // WindowProps requires Send+Sync but our state is Rc-based (main thread only),
    // so we use the same thread-local pattern as EVENT_PROCESSOR.
    thread_local! {
        static CLOSE_HANDLER: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
    }
    CLOSE_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            // Edits persist live through the collab relay. What this device
            // remembers of how things looked is written now, with every
            // pane's place and board camera as they are.
            remember_views(Some(store));
        }));
    });
    let on_close: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| {
        CLOSE_HANDLER.with(|cell| {
            if let Some(f) = cell.borrow().as_ref() {
                f();
            }
        });
        true // proceed with close
    });

    // The size and place it closed at on this device.
    let saved_window = crate::persistence::load_window();
    let (width, height, place) = saved_window.as_ref().and_then(|w| w.sane()).unwrap_or((1200, 800, None));
    let props = WindowProps {
        title: "Pimble".into(),
        width,
        height,
        x: place.map(|p| p.0),
        y: place.map(|p| p.1),
        maximized: saved_window.as_ref().is_some_and(|w| w.maximized),
        // The monitor the place is on, only with a place to put there.
        monitor: saved_window.and_then(|w| w.monitor).filter(|_| place.is_some()),
        borderless: true,
        transparent: true,
        resizable: true,
        menu_in_titlebar: true,
        icon: Some(include_bytes!("../../../assets/brand/png/pimble-256.png")),
        on_close_requested: Some(on_close),
        ..Default::default()
    };

    App::new(app_component)
        .window_props(props)
        .theme(theme)
        .menu(menus)
        .renderer(renderer)
        .run();

    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = None;
    });
    CLOSE_HANDLER.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
#[cfg(test)]
mod tests {
    use super::{
        hosted_store_label, host_to_share, member_role_text, member_status_text, open_share_modal, open_stop_relay_modal,
        account_showing, cloud_part, server_part, share_from_this_computer, sync_badge_text, Light, MOUNT_NOTE,
    };
    use crate::protocol::{BackendCommand, BackendHandle, CloudOp};
    use crate::state::{AppStore, ShareFace};
    use pimble_core::{Node, NodeId, RelaySide, Store, StoreId, StoreKind, SyncState};
    use pimble_rpc::{CloudHostedStoreInfo, MemberRole, ShareMemberStatus};
    use rinch::prelude::untracked;

    /// A hosted store with a shared folder and a plain document under its
    /// root, a backend whose commands the test can read, and an account
    /// signed in. `shared_by` is who shared the store with this account, if
    /// anyone did.
    fn store_with_a_share(shared_by: Option<&str>) -> (AppStore, crossbeam_channel::Receiver<BackendCommand>, StoreId, NodeId, NodeId) {
        let app = AppStore::new();
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<BackendCommand>(16);
        let (_event_tx, event_rx) = crossbeam_channel::bounded(16);
        app.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        app.cloud_signed_in.set(true);
        app.cloud_email.set("me@example.com".to_string());

        let mut store = Store::new_local("Notes", "/tmp/notes.pimble".into());
        let root = Node::folder("Notes");
        store.root_node_id = root.id;
        store.shared_by = shared_by.map(str::to_string);
        // Hosted: its link is an encrypting one, which is what a share needs.
        store.sync_mode = StoreKind::Vault;
        let mut recipes = Node::folder("Recipes");
        recipes.metadata.set_share(Some(&pimble_core::ShareMarker {
            v: pimble_core::ShareMarker::VERSION,
            key_id: uuid::Uuid::new_v4(),
            url: "https://pimble.app".to_string(),
            name: "Family recipes".to_string(),
        }));
        let plain = Node::document("Pasta");
        let (store_id, shared_id, plain_id) = (store.id, recipes.id, plain.id);
        app.upsert_store(store);
        app.upsert_node(store_id, root);
        app.upsert_node(store_id, recipes);
        app.upsert_node(store_id, plain);
        (app, cmd_rx, store_id, shared_id, plain_id)
    }

    fn asked_for_members(commands: &crossbeam_channel::Receiver<BackendCommand>, node_id: NodeId) -> bool {
        commands
            .try_iter()
            .any(|cmd| matches!(cmd, BackendCommand::CloudShareInfo { node_id: n, .. } if n == node_id))
    }

    /// One's own store keeps the dialog it always had: a shared node opens on
    /// the owner's face and asks for its members, any other node opens on the
    /// name field, and neither opens without an account.
    #[test]
    fn share_opens_the_owners_dialog_in_ones_own_store() {
        let (app, commands, store_id, shared_id, plain_id) = store_with_a_share(None);

        open_share_modal(app, store_id, shared_id);
        assert_eq!(app.share_modal_node.get(), Some((store_id, shared_id)));
        assert!(app.share_modal_shared.get());
        assert!(untracked(|| app.share_modal_manages()));
        assert_eq!(app.share_modal_name.get(), "Family recipes");
        assert_eq!(app.share_modal_pending.get(), Some(CloudOp::ShareInfo));
        assert!(asked_for_members(&commands, shared_id));

        open_share_modal(app, store_id, plain_id);
        assert_eq!(app.share_modal_node.get(), Some((store_id, plain_id)));
        assert!(!app.share_modal_shared.get());
        assert!(untracked(|| app.share_modal_manages()));
        assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Name);
        assert_eq!(app.share_modal_pending.get(), None);
        assert!(!asked_for_members(&commands, plain_id));

        app.share_modal_node.set(None);
        app.cloud_signed_in.set(false);
        open_share_modal(app, store_id, plain_id);
        assert_eq!(app.share_modal_node.get(), None);
        assert!(account_showing(app));
        assert_eq!(app.account_modal_hint.get(), "Sign in to share a node");
    }

    /// In a store that reached this device as someone else's share, a shared
    /// root opens on the member's face (the share's own name, no owner's
    /// controls) and the member list is asked for as a courtesy. Signed out,
    /// the face still opens from what this device holds, and asks nobody.
    #[test]
    fn share_opens_the_members_face_in_someone_elses_share() {
        let (app, commands, store_id, shared_id, _) = store_with_a_share(Some("ann@example.com"));

        open_share_modal(app, store_id, shared_id);
        assert_eq!(app.share_modal_node.get(), Some((store_id, shared_id)));
        assert!(app.share_modal_shared.get());
        assert!(!untracked(|| app.share_modal_manages()), "a member is shown the owner's dialog");
        assert_eq!(app.share_modal_name.get(), "Family recipes");
        assert_eq!(
            untracked(|| app.share_modal_shared_by_sentence()),
            "Shared by ann@example.com. Only an owner changes who it is shared with."
        );
        assert!(app.share_modal_state.get().is_empty());
        assert!(asked_for_members(&commands, shared_id));

        app.share_modal_node.set(None);
        app.cloud_signed_in.set(false);
        open_share_modal(app, store_id, shared_id);
        assert_eq!(app.share_modal_node.get(), Some((store_id, shared_id)));
        assert!(!account_showing(app), "a member is asked to sign in to read who shared it");
        assert_eq!(app.share_modal_pending.get(), None);
        assert!(!asked_for_members(&commands, shared_id));
    }

    /// The menu never offers "Share..." on any other node of someone else's
    /// store; if something asks anyway, no form the server would refuse opens
    /// and the status bar says why.
    #[test]
    fn share_does_not_open_on_a_node_that_is_not_ones_to_share() {
        let (app, commands, store_id, _, plain_id) = store_with_a_share(Some("ann@example.com"));

        open_share_modal(app, store_id, plain_id);
        assert_eq!(app.share_modal_node.get(), None);
        assert!(!account_showing(app));
        assert_eq!(app.notice.get(), "Only an owner of this store can share from it.");
        assert!(commands.try_iter().next().is_none());
    }

    /// The same store before anyone hosted it or shared it from anywhere: it
    /// is only on this computer.
    fn unhosted_store() -> (AppStore, crossbeam_channel::Receiver<BackendCommand>, StoreId, NodeId, NodeId) {
        let (app, commands, store_id, shared_id, plain_id) = store_with_a_share(None);
        app.get_store_signal(store_id).unwrap().update(|s| s.sync_mode = StoreKind::Plain);
        app.ensure_sync_entry(store_id);
        (app, commands, store_id, shared_id, plain_id)
    }

    /// In a store that is neither hosted nor shared from this computer the
    /// dialog opens on the two ways and does nothing else: nothing is asked
    /// of the server until the person picks one, and picking "Share from this
    /// computer" asks for exactly that, never for hosting
    /// (docs/RELAY_CONTRACT.md, "The apps"; `CLAUDE.md`, "Nothing is hosted
    /// unless the person asked for it").
    #[test]
    fn share_in_a_store_that_is_not_hosted_offers_the_two_ways() {
        let (app, commands, store_id, shared_id, plain_id) = unhosted_store();

        open_share_modal(app, store_id, plain_id);
        assert_eq!(app.share_modal_node.get(), Some((store_id, plain_id)));
        assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Ways);
        assert_eq!(untracked(|| app.share_modal_ways_note()), "");
        assert_eq!(app.share_modal_pending.get(), None);
        assert!(commands.try_iter().next().is_none(), "opening the dialog asked the server for something");

        share_from_this_computer(app);
        assert_eq!(app.share_modal_pending.get(), Some(CloudOp::RelayStore));
        let sent: Vec<BackendCommand> = commands.try_iter().collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(matches!(sent[0], BackendCommand::CloudRelayStore { store_id: s } if s == store_id), "{sent:?}");
        // Still the two ways until the server has answered.
        assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Ways);

        // The store's link is what moves the dialog on, whoever reports it.
        app.get_store_signal(store_id).unwrap().update(|s| {
            s.sync_mode = StoreKind::Vault;
            s.relay = RelaySide::Owner;
        });
        assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Name);

        // A node that carries a share shows it whatever the link says.
        app.get_store_signal(store_id).unwrap().update(|s| {
            s.sync_mode = StoreKind::Plain;
            s.relay = RelaySide::None;
        });
        open_share_modal(app, store_id, shared_id);
        assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Owner);
    }

    /// "Host on Pimble Cloud..." hands over to the hosting confirmation, which
    /// is where hosting is said in full and agreed to: the dialog itself
    /// uploads nothing. The node is remembered so that the share carries on
    /// once the store is hosted.
    #[test]
    fn hosting_from_the_share_dialog_goes_through_the_confirmation() {
        let (app, commands, store_id, _, plain_id) = unhosted_store();

        open_share_modal(app, store_id, plain_id);
        host_to_share(app);
        assert_eq!(app.share_modal_node.get(), None);
        assert_eq!(app.host_modal_store.get(), Some(store_id));
        assert_eq!(app.share_after_host.get(), Some((store_id, plain_id)));
        assert!(commands.try_iter().next().is_none(), "the store was hosted without the confirmation");
    }

    /// Neither way is open to a copy of a store that lives somewhere else, or
    /// to a store linked to another Pimble server: the dialog says which, and
    /// asks nothing of the server (hosting a linked store would replace its
    /// link, and the server does not refuse it).
    #[test]
    fn the_two_ways_say_why_they_are_off() {
        let (app, _commands, store_id, _, plain_id) = unhosted_store();
        open_share_modal(app, store_id, plain_id);

        app.set_sync(
            store_id,
            Some(pimble_core::RemoteEndpoint { url: "http://pimble.example:7462".parse().unwrap(), auth: pimble_core::AuthMethod::None }),
            SyncState::Offline,
        );
        assert_eq!(untracked(|| app.share_modal_face()), ShareFace::Ways);
        assert_eq!(
            untracked(|| app.share_modal_ways_note()),
            "This store is linked to another Pimble server. Unlink it first."
        );

        app.get_store_signal(store_id).unwrap().update(|s| s.is_replica = true);
        assert_eq!(
            untracked(|| app.share_modal_ways_note()),
            "This is a copy of a store that lives somewhere else, and can only be shared from there."
        );
    }

    /// "Stop Sharing from This Computer..." asks before it sends anything.
    #[test]
    fn stopping_sharing_from_this_computer_asks_first() {
        let (app, commands, store_id, _, _) = unhosted_store();
        app.stop_relay_modal_error.set("left over".to_string());

        open_stop_relay_modal(app, store_id);
        assert_eq!(app.stop_relay_modal_store.get(), Some(store_id));
        assert!(app.stop_relay_modal_error.get().is_empty());
        assert!(!app.stop_relay_modal_busy.get());
        assert!(commands.try_iter().next().is_none());
    }

    fn hosted(name: &str, share: bool, shared_by: Option<&str>) -> CloudHostedStoreInfo {
        CloudHostedStoreInfo {
            store_id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            role: "editor".to_string(),
            kind: "vault".to_string(),
            created_at: String::new(),
            root: share.then(pimble_core::NodeId::new),
            shared_by: shared_by.map(str::to_string),
        }
    }

    /// A row with a `root` is a share: its own name and who shared it, never
    /// the owner's store name (docs/NODE_DOCUMENT_CONTRACT.md section 5, "A
    /// share has a name of its own"). A store of one's own is just its name.
    #[test]
    fn a_shared_store_says_who_shared_it() {
        assert_eq!(hosted_store_label(&hosted("Notes", false, None), false), "Notes");
        assert_eq!(
            hosted_store_label(&hosted("Recipes", true, Some("ann@example.com")), false),
            "Recipes, shared by ann@example.com"
        );
        // A share whose owner the service did not name is still just a name.
        assert_eq!(hosted_store_label(&hosted("Recipes", true, None), false), "Recipes");
    }

    /// A relayed row is not hosted anywhere: it says whose computer it is
    /// served from (docs/RELAY_CONTRACT.md). Pimble Cloud holds no name for
    /// such a store, so a row that is not a share has words in place of one.
    #[test]
    fn a_relayed_store_says_whose_computer_it_is_on() {
        assert_eq!(
            hosted_store_label(&hosted("Recipes", true, Some("alice@example.com")), true),
            "Recipes, shared from alice@example.com's computer"
        );
        // A grant on the whole of someone's relayed store: no name to show.
        assert_eq!(
            hosted_store_label(&hosted("", false, Some("alice@example.com")), true),
            "Shared from alice@example.com's computer"
        );
        // One's own, seen from another of one's computers.
        assert_eq!(hosted_store_label(&hosted("", false, None), true), "Shared from another computer");
        assert_eq!(hosted_store_label(&hosted("Recipes", true, None), true), "Recipes, shared from another computer");
    }

    /// Every member state the service can report has words for it, and none of
    /// them is jargon: an invitation and a key that has not been handed over
    /// are ordinary things to be waiting for.
    #[test]
    fn a_member_reads_in_plain_words() {
        assert_eq!(member_status_text(ShareMemberStatus::Invited, true), "invited, no account yet");
        assert_eq!(
            member_status_text(ShareMemberStatus::WaitingForKey, true),
            "waiting for your Pimble to hand over the key"
        );
        assert_eq!(member_status_text(ShareMemberStatus::Active, true), "active");
        // A member reading the list is not the one whose Pimble hands keys over.
        assert_eq!(
            member_status_text(ShareMemberStatus::WaitingForKey, false),
            "waiting for an owner's Pimble to hand over the key"
        );
        assert_eq!(member_status_text(ShareMemberStatus::Active, false), "active");
        assert_eq!(member_role_text(MemberRole::Owner), "owner");
        assert_eq!(member_role_text(MemberRole::Editor), "can edit");
        assert_eq!(member_role_text(MemberRole::Reader), "can read");
        assert!(MOUNT_NOTE.contains("another store"));
    }

    /// The badge is the server's `sync_mode` (docs/DESKTOP_ACCOUNT_CONTRACT.md
    /// decision 4): a vault link says "encrypted" first, a plain link reads
    /// as it always has.
    #[test]
    fn badge_text_follows_state_and_mode() {
        // Built through serde: this crate has no chrono of its own.
        let synced: SyncState =
            serde_json::from_str(r#"{"state":"synced","last_sync":"2026-09-16T00:00:00Z"}"#).unwrap();
        let none = RelaySide::None;
        assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Plain, none, false), "offline");
        assert_eq!(sync_badge_text(&SyncState::Syncing, StoreKind::Plain, none, false), "syncing");
        assert_eq!(sync_badge_text(&synced, StoreKind::Plain, none, false), "synced");
        assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault, none, false), "encrypted · offline");
        assert_eq!(sync_badge_text(&SyncState::Syncing, StoreKind::Vault, none, false), "encrypted · syncing");
        assert_eq!(sync_badge_text(&synced, StoreKind::Vault, none, false), "encrypted · synced");
    }

    #[test]
    fn the_cloud_part_says_who_and_how_it_is_doing() {
        use pimble_rpc::{CloudConnection, CloudReach};
        assert_eq!(cloud_part(false, "", None).detail, "sign in");
        let connected = CloudConnection { reach: CloudReach::Connected, sentence: "Connected to pimble.app.".into() };
        let part = cloud_part(true, "joe@example.com", Some(&connected));
        assert_eq!((part.light, part.detail.as_str()), (Light::Good, "joe@example.com"));
        assert_eq!(part.tooltip, "Signed in as joe@example.com. Connected to pimble.app.");
        let offline = CloudConnection { reach: CloudReach::Offline, sentence: "Not everything here can reach pimble.app.".into() };
        let part = cloud_part(true, "joe@example.com", Some(&offline));
        assert_eq!((part.light, part.detail.as_str()), (Light::Bad, "joe@example.com · offline"));
        // A server too old to say: the account alone.
        assert_eq!(cloud_part(true, "joe@example.com", None).light, Light::Quiet);
    }

    #[test]
    fn the_server_part_reads_the_connection_status() {
        assert_eq!(server_part("Connected", "127.0.0.1:7462").light, Light::Good);
        assert_eq!(server_part("Disconnected", "127.0.0.1:7462").detail, "connecting");
        let part = server_part("Error: refused", "127.0.0.1:7462");
        assert_eq!((part.light, part.detail.as_str()), (Light::Bad, "refused"));
    }

    /// A store shared from this computer says nothing: its link is to its own
    /// twin here (docs/RELAY_CONTRACT.md, "The apps"). A member's
    /// row reads as any encrypted store's does, and says `owner offline` only
    /// when the backend knows that is why: a Pimble server reports a member's
    /// link as `Offline` and no more, which no network looks like too.
    #[test]
    fn badge_text_says_which_end_of_the_relay_this_is() {
        let synced: SyncState =
            serde_json::from_str(r#"{"state":"synced","last_sync":"2026-09-21T00:00:00Z"}"#).unwrap();
        assert_eq!(sync_badge_text(&synced, StoreKind::Vault, RelaySide::Owner, false), "");
        assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault, RelaySide::Owner, false), "");

        assert_eq!(sync_badge_text(&synced, StoreKind::Vault, RelaySide::Member, false), "encrypted · synced");
        assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault, RelaySide::Member, false), "encrypted · offline");
        assert_eq!(sync_badge_text(&SyncState::Offline, StoreKind::Vault, RelaySide::Member, true), "owner offline");
    }
}

/// What this device remembers of how things looked (`views.rs`), written now
/// with every pane's place and board camera as they are: the window is
/// closing.
#[cfg(feature = "native")]
fn remember_views(store: Option<AppStore>) {
    if let Some(store) = store.or_else(|| crate::links::with_app_store(|store| store)) {
        crate::places::remember_all(store);
    }
    crate::boards::remember_all();
    crate::views::save_now();
    // The window's own size and place (as it is when not maximized, where
    // the platform says), and whether it is maximized.
    if let Some(window) = rinch::prelude::current_window_restore() {
        crate::persistence::save_window(&crate::persistence::WindowGeometry {
            width: window.size.0,
            height: window.size.1,
            x: window.position.map(|p| p.0),
            y: window.position.map(|p| p.1),
            maximized: window.maximized,
            monitor: window.monitor,
        });
    }
}

/// Close the window (its close button, File > Exit), writing first what
/// this device remembers of how things looked. Rinch's own close control
/// exits without asking `on_close_requested`.
#[cfg(feature = "native")]
pub(crate) fn close_window() {
    remember_views(None);
    close_current_window();
}
