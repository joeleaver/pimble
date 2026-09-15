//! Pimble Desktop Application
//!
//! Built with Rinch UI framework

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use rinch::prelude::*;
use rinch::core::{request_focus, set_keyboard_interceptor, clear_keyboard_interceptor};
use rinch::menu::{Menu, MenuItem};
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::backend::{BackendCommand, BackendHandle};
use crate::editor::{start_editing, stop_editing};
use crate::events::{EVENT_PROCESSOR, process_backend_events};
use crate::state::{parse_tree_value, display_label_from_node, mount_is_dimmed, mount_label_suffix, AppStore, PendingMount, SearchState};
use crate::styles::{APP_CSS, EDITOR_CSS};

/// The app's color scheme. Feeds rinch's `ThemeProviderProps` and the editor's
/// built-in dark stylesheet (`EditorHandle::set_dark_mode`, applied in
/// `editor::start_editing`), so both follow one switch.
pub(crate) const DARK_MODE: bool = true;

thread_local! {
    /// The search box's `NodeHandle`, captured once when the toolbar is built
    /// so the View menu's "Focus Search" (Ctrl+K) action — constructed earlier,
    /// before the box exists — can reach it later. Same reach-across-closures
    /// need as `EVENT_PROCESSOR`/`CLOSE_HANDLER` below.
    static SEARCH_INPUT: RefCell<Option<NodeHandle>> = const { RefCell::new(None) };
    /// The debounced `Search` command pending from the last keystroke, if any.
    static SEARCH_DEBOUNCE: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
}

/// Select and open a node or store root identified by a tree value
/// (`"store_{uuid}"` / `"node_{store_uuid}_{node_uuid}"`) — the single
/// implementation behind both a tree click and a search-result click, per
/// CLAUDE.md's "one way to write node content": there is one way to open one.
fn open_node(store: AppStore, tree_state: UseTreeReturn, value: String) {
    store.selected_id.set(Some(value.clone()));
    tree_state.controller.select(&value);

    // Offline-first: a document opens from the cached content at once and
    // `start_editing` then reconciles the session with the server (state
    // vector in, diff out), so a cache that is behind the server (this
    // window's own earlier session, another window, a replica) is merged
    // in place rather than reloaded. A node the cache has never seen is
    // fetched; `NodeLoaded` starts the session then.
    let parsed = parse_tree_value(&value);
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
                    (label, node.content.clone(), is_doc)
                })
            });
            match result {
                Some((label, content, is_doc)) => (Some(label), Some(content), is_doc, Some((s_id, n_id))),
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

    store.show_editor.set(is_document);

    if let Some(title) = title_opt {
        store.node_title.set(title);
    }

    if is_document {
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            start_editing(store, s_id, n_id, &content_bytes);
        }
    } else {
        stop_editing(store);
    }
    store.editor_dirty.set(false);
}

/// "Copy as Mount Source": record the canonical pair and display title of the
/// node or store root identified by `value` into `store.mount_source`, for a
/// subsequent "Paste Mount Here" elsewhere in the tree.
fn copy_as_mount_source(store: AppStore, value: &str) {
    let Some((s_id, node_id_opt)) = parse_tree_value(value) else { return };
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
    bump_tree_after_menu_closes(store);
}

/// Re-render every row's context menu (so "Paste Mount Here" reads its fresh
/// `disabled` value: every row's `TreeNodeData` carries that flag) on the
/// next main-thread turn, not inside the menu item's own click. A row that
/// re-renders while its menu is still open orphans the menu's portal, which
/// then stays on screen for good (rinch #714); deferring lets the item close
/// its menu first.
fn bump_tree_after_menu_closes(store: AppStore) {
    rinch::run_on_main_thread(move || store.bump_tree_structure());
}

/// "Paste Mount Here": create a mount at the tree location identified by
/// `target_value`, sourced from whatever "Copy as Mount Source" last recorded
/// in `store.mount_source`. No-op if nothing was copied. Clears the copied
/// source on success so the menu item disappears again.
fn paste_mount_here(store: AppStore, target_value: &str) {
    let Some(source) = untracked(|| store.mount_source.get()) else { return };
    let (source_store, source_node, title) = source;
    let Some((target_store, target_node_opt)) = parse_tree_value(target_value) else { return };
    let Some(parent_id) = target_node_opt.or_else(|| store.root_node_id(target_store)) else { return };
    store.send(BackendCommand::CreateMount {
        store_id: target_store,
        parent_id,
        source_store_id: source_store,
        source_node_id: source_node,
        title: Some(title),
    });
    store.mount_source.set(None);
    // Re-render every row's context menu so "Paste Mount Here" reads as
    // disabled again (see the `no_mount_source` note in `run`).
    bump_tree_after_menu_closes(store);
}

/// Open the "Add Remote Store..." modal, resetting everything a previous run
/// left behind. `target` picks the mode (docs/history/REMOTE_MOUNTS_CONTRACT.md
/// decision 9): `None` is the plain File-menu flow that only adds a replica,
/// `Some(canonical pair)` is "Mount Remote Store Here...", whose action adds
/// the replica when this server does not already have the store and then
/// mounts its root under that pair.
fn open_connect_modal(store: AppStore, target: Option<(pimble_core::StoreId, pimble_core::NodeId)>) {
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
        store.send(BackendCommand::Search { query, stores, limit: 50 });
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

/// Text for a linked store's status badge (docs/SYNC_CONTRACT.md "B: app
/// side"). `Conflict` is out of scope for this contract (there are no
/// conflicts to show yet) but still needs a label rather than a match gap.
fn sync_badge_text(state: &pimble_core::SyncState) -> &'static str {
    match state {
        pimble_core::SyncState::Offline => "offline",
        pimble_core::SyncState::Syncing => "syncing",
        pimble_core::SyncState::Synced { .. } => "synced",
        pimble_core::SyncState::Conflict { .. } => "conflict",
    }
}

/// Walk the DOM subtree to find a `data-oncontextmenu` handler and copy it
/// to `target`. Used to hoist the context menu handler from the invisible
/// ContextMenuTarget (display:contents) to a visible wrapper div.
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

/// Main application entry point
pub fn run() {
    let store = AppStore::new();

    // Double-click detection for rename: (last_click_time, last_click_value)
    let last_click: Rc<Cell<(Instant, String)>> = Rc::new(Cell::new((Instant::now(), String::new())));

    // Persistent tree state — created once, preserves expanded/selected across data changes
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());

    // Drag-and-drop state for tree node rearrangement
    let drag_ctx: DragContext<String> = DragContext::new();

    // Set up event processing via thread-local so run_on_main_thread
    // can trigger it without capturing non-Send types. (No autosave loop
    // needed — the collab session persists edits live.)
    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            process_backend_events(store, tree_state);
        }));
    });

    // Build menus
    let file_menu = Menu::new()
        .item(MenuItem::new("New Store...").shortcut("Ctrl+N").on_click(move || {
            tracing::info!("New store menu clicked");
            let dialog = rinch::dialogs::save_file()
                .set_title("Create New Store")
                .add_filter("Pimble Store", &["pimble"]);

            if let Some(path) = dialog.save() {
                let path_str = path.to_string_lossy().to_string();
                let path_str = if path_str.ends_with(".pimble") {
                    path_str
                } else {
                    format!("{}.pimble", path_str)
                };
                let name = path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "New Store".to_string());
                tracing::info!("Creating new store at: {}", path_str);
                store.pending_create_path.set(Some(path_str.clone()));
                store.send(BackendCommand::CreateStore { path: path_str, name });
            }
        }))
        .item(MenuItem::new("Open Store...").shortcut("Ctrl+O").on_click(move || {
            tracing::info!("Open store menu clicked");
            let dialog = rinch::dialogs::pick_folder()
                .set_title("Open Store");

            if let Some(path) = dialog.pick() {
                let path_str = path.to_string_lossy().to_string();
                tracing::info!("Opening store at: {}", path_str);
                store.send(BackendCommand::OpenStore { path: path_str });
            }
        }))
        .item(MenuItem::new("Add Remote Store...").on_click(move || {
            tracing::info!("Add remote store menu clicked");
            open_connect_modal(store, None);
        }))
        .separator()
        .item(MenuItem::new("Close Store").on_click(move || {
            tracing::info!("Close store");
            if let Some((store_id, _)) = store.selected_store_and_node() {
                store.send(BackendCommand::CloseStore { store_id });
            }
        }))
        .separator()
        .item(MenuItem::new("Exit").shortcut("Alt+F4").on_click(|| {
            close_current_window();
        }));

    let edit_menu = Menu::new()
        .item(MenuItem::new("Undo").shortcut("Ctrl+Z").enabled(false).on_click(|| {}))
        .item(MenuItem::new("Redo").shortcut("Ctrl+Y").enabled(false).on_click(|| {}))
        .separator()
        .item(MenuItem::new("Cut").shortcut("Ctrl+X").enabled(false).on_click(|| {}))
        .item(MenuItem::new("Copy").shortcut("Ctrl+C").enabled(false).on_click(|| {}))
        .item(MenuItem::new("Paste").shortcut("Ctrl+V").enabled(false).on_click(|| {}))
        .separator()
        .item(MenuItem::new("Delete").on_click(move || {
            if let Some((store_id, node_id)) = store.selected_store_and_node() {
                store.send(BackendCommand::DeleteNode { store_id, node_id });
            }
        }));

    let view_menu = Menu::new()
        .item(MenuItem::new("Toggle Sidebar").shortcut("Ctrl+\\").on_click(|| {
            tracing::info!("Toggle sidebar");
        }))
        .separator()
        .item(MenuItem::new("Focus Search").shortcut("Ctrl+K").on_click(|| {
            SEARCH_INPUT.with(|cell| {
                if let Some(handle) = cell.borrow().as_ref() {
                    handle.focus();
                }
            });
        }))
        .item(MenuItem::new("Rebuild Search Index").on_click(move || {
            let store_ids = untracked(|| store.store_ids.get());
            tracing::info!("Rebuilding search index for {} store(s)", store_ids.len());
            for store_id in store_ids {
                store.send(BackendCommand::RebuildIndex { store_id });
            }
        }))
        .separator()
        .item(MenuItem::new("Zoom In").shortcut("Ctrl+=").on_click(|| {}))
        .item(MenuItem::new("Zoom Out").shortcut("Ctrl+-").on_click(|| {}))
        .item(MenuItem::new("Reset Zoom").shortcut("Ctrl+0").on_click(|| {}));

    let help_menu = Menu::new()
        .item(MenuItem::new("Documentation").shortcut("F1").on_click(|| {
            tracing::info!("Opening documentation...");
        }))
        .separator()
        .item(MenuItem::new("About Pimble").on_click(|| {
            tracing::info!("About Pimble v0.1.0");
        }));

    let menus = vec![
        ("File", file_menu),
        ("Edit", edit_menu),
        ("View", view_menu),
        ("Help", help_menu),
    ];

    // Theme
    let theme = ThemeProviderProps {
        primary_color: Some("blue".into()),
        dark_mode: DARK_MODE,
        default_radius: Some("sm".into()),
        ..Default::default()
    };

    // Save-on-close: register a thread-local that the close callback invokes.
    // WindowProps requires Send+Sync but our state is Rc-based (main thread only),
    // so we use the same thread-local pattern as EVENT_PROCESSOR.
    thread_local! {
        static CLOSE_HANDLER: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
    }
    CLOSE_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            // Edits persist live through the collab relay; nothing to flush on close.
            let _ = store;
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

    let props = WindowProps {
        title: "Pimble".into(),
        width: 1200,
        height: 800,
        borderless: true,
        transparent: true,
        resizable: true,
        menu_in_titlebar: true,
        on_close_requested: Some(on_close),
        ..Default::default()
    };

    // Build app component - parameter must be named __scope for the rsx! macro
    let app_component = move |__scope: &mut RenderScope| -> NodeHandle {
        // Cancel any in-progress rename, optionally committing the change.
        let cancel_last_click = last_click.clone();
        let cancel_rename = move |commit: bool| {
            if let Some(prev_value) = untracked(|| store.renaming_node.get()) {
                if commit {
                    let new_title = untracked(|| store.rename_text.get());
                    if let Some((sid, Some(nid))) = parse_tree_value(&prev_value) {
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
            cancel_last_click.set((Instant::now(), String::new()));
        };

        // Tree callbacks
        let select_last_click = last_click.clone();
        let cancel_rename_for_select = cancel_rename.clone();
        let on_tree_select = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node selected: {}", value);

            // Double-click on a node → enter rename mode
            {
                let now = Instant::now();
                let (prev_time, prev_value) = select_last_click.replace((now, value.clone()));
                let is_double_click = prev_value == value && now.duration_since(prev_time).as_millis() < 500;

                if is_double_click && value.starts_with("node_") {
                    // Don't allow rename on mount nodes
                    let is_mount_node = if let Some((sid, Some(nid))) = parse_tree_value(&value) {
                        store.is_mount(sid, nid)
                    } else {
                        false
                    };
                    if is_mount_node {
                        return;
                    }

                    let edit_text = if let Some((store_id, Some(node_id))) = parse_tree_value(&value) {
                        store.get_node_signal(store_id, node_id)
                            .map(|sig| sig.with(|n| n.metadata.title.clone()))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    store.rename_text.set(edit_text);
                    store.renaming_node.set(Some(value.clone()));
                    let nv_interceptor = value.clone();
                    set_keyboard_interceptor(move |data| {
                        if data.key == "Escape" {
                            rinch::run_on_main_thread(move || {
                                store.renaming_node.set(None);
                                clear_keyboard_interceptor();
                            });
                            return true;
                        }
                        if data.key == "Enter" {
                            let nv = nv_interceptor.clone();
                            rinch::run_on_main_thread(move || {
                                let new_title = untracked(|| store.rename_text.get());
                                store.renaming_node.set(None);
                                clear_keyboard_interceptor();
                                if let Some((s_id, Some(n_id))) = parse_tree_value(&nv) {
                                    store.send(BackendCommand::RenameNode {
                                        store_id: s_id, node_id: n_id, title: new_title,
                                    });
                                }
                            });
                            return true;
                        }
                        false
                    });
                    if let Some((doc_key, input_node_id)) = store.rename_input_ids.with(|ids| ids.get(&value).copied()) {
                        request_focus(doc_key, input_node_id);
                    }
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

        // Custom render_node with per-node reactive Effects for label/icon/mount
        let render_node_fn: RenderTreeNode = Rc::new(move |payload: &RenderTreeNodePayload, __scope: &mut RenderScope| {
            let node_value = payload.node.value.clone();
            let is_store_root = node_value.starts_with("store_");
            let has_children = payload.has_children;

            // Parse once for reuse
            let parsed = parse_tree_value(&node_value);

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
            // read reactively below for the badge; `is_linked_now` is a
            // static (untracked) snapshot for the context menu's `disabled`
            // values, rebuilt only when the tree structurally re-renders this
            // row (rinch #714 — see the `no_mount_source` note further down).
            let sync_sig = if is_store_root {
                parsed.and_then(|(s_id, _)| store.get_sync_signal(s_id))
            } else {
                None
            };
            let is_linked_now = sync_sig
                .map(|sig| untracked(|| sig.with(|(remote, _)| remote.is_some())))
                .unwrap_or(false);

            // Same static-snapshot-rebuilt-on-structural-rebuild pattern as
            // `is_linked_now` (rinch #714 — see the `no_mount_source` note
            // further down): whether this store row is a replica, for
            // "Remove Replica..."'s `disabled` value.
            let is_replica_now = store_sig
                .map(|sig| untracked(|| sig.with(|s| s.is_replica)))
                .unwrap_or(false);

            // Choose icon (static — changes only on structural rebuild). Folders
            // are folders even when empty; documents are documents even with
            // children, so the icon follows node_type, not has_children.
            let is_folder = node_sig
                .map(|sig| sig.with(|n| n.node_type == pimble_core::node_types::FOLDER))
                .unwrap_or(has_children);
            let icon = if is_store_root {
                TablerIcon::Database
            } else if is_mount {
                TablerIcon::Link
            } else if is_folder {
                TablerIcon::Folder
            } else {
                TablerIcon::File
            };

            let icon_class = if is_mount {
                "rinch-tree__icon rinch-tree__icon--mount"
            } else {
                "rinch-tree__icon"
            };

            let base_label_style = if is_store_root {
                "cursor: default; font-weight: 600; text-transform: uppercase; font-size: 11px; letter-spacing: 0.03em;"
            } else {
                "cursor: default;"
            };

            // Drag-and-drop callbacks
            let nv_dragstart = node_value.clone();
            let nv_drop = node_value.clone();
            let nv_enter = node_value.clone();
            let nv_leave = node_value.clone();

            let on_dragstart = move || {
                tracing::info!("DragStart: {}", nv_dragstart);
                drag_ctx.set(nv_dragstart.clone());
            };
            let on_dragend = move || {
                tracing::info!("DragEnd");
                drag_ctx.clear();
                store.drop_target.set(None);
            };
            let on_drop = {
                let nv = nv_drop.clone();
                move || {
                    // Always clear drag state first, even if the drop is invalid
                    let dragged_value = drag_ctx.take();
                    store.drop_target.set(None);

                    let Some(dragged_value) = dragged_value else { return; };
                    if dragged_value == nv { return; }
                    let Some((drag_store_id, Some(drag_node_id))) = parse_tree_value(&dragged_value) else { return; };
                    let new_parent_id = if let Some((target_store_id, target_node_id_opt)) = parse_tree_value(&nv) {
                        if drag_store_id != target_store_id {
                            tracing::warn!(
                                "Ignoring drop: source store {:?} differs from target store {:?} (cross-store move not supported yet)",
                                drag_store_id, target_store_id
                            );
                            return;
                        }
                        match target_node_id_opt {
                            Some(nid) => {
                                if store.is_mount(target_store_id, nid) {
                                    tracing::warn!("Ignoring drop onto mount node {:?}/{:?}", target_store_id, nid);
                                    return;
                                }
                                nid
                            }
                            None => {
                                match store.root_node_id(target_store_id) {
                                    Some(id) => id,
                                    None => return,
                                }
                            }
                        }
                    } else {
                        return;
                    };
                    tracing::info!("Drop: moving {:?} into {:?}", drag_node_id, new_parent_id);
                    // Record the exact (possibly mount-path-qualified) tree
                    // value dropped onto, so the resulting `NodeMoved` event
                    // can auto-expand precisely this place (decision 7).
                    crate::state::set_last_drop_target_value(nv.clone());
                    store.send(BackendCommand::MoveNode {
                        store_id: drag_store_id, node_id: drag_node_id,
                        new_parent_id, position: None,
                    });
                }
            };
            let on_dragenter = move || { store.drop_target.set(Some(nv_enter.clone())); };
            let on_dragleave = {
                let nv = nv_leave.clone();
                move || {
                    if store.drop_target.get().as_deref() == Some(&nv) {
                        store.drop_target.set(None);
                    }
                }
            };

            // Context menu actions
            let nv_ctx = node_value.clone();
            let on_new_child = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, node_id_opt)) = parse_tree_value(&nv) {
                        let parent_id = node_id_opt.or_else(|| store.root_node_id(s_id));
                        if let Some(pid) = parent_id {
                            store.send(BackendCommand::CreateNode {
                                store_id: s_id, parent_id: Some(pid), title: String::new(),
                            });
                        }
                    }
                }
            };
            let on_close_store = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        store.send(BackendCommand::CloseStore { store_id: s_id });
                    }
                }
            };
            let on_link_to_remote = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        store.link_modal_store.set(Some(s_id));
                        store.link_modal_url.set(String::new());
                        store.link_modal_token.set(String::new());
                        store.link_modal_token_visible.set(false);
                        store.link_modal_error.set(String::new());
                    }
                }
            };
            let on_unlink_from_remote = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        store.send(BackendCommand::SetStoreSync { store_id: s_id, remote: None });
                    }
                }
            };
            let on_remove_replica = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, _)) = parse_tree_value(&nv) {
                        store.remove_replica_modal_store.set(Some(s_id));
                        store.remove_replica_modal_error.set(String::new());
                        store.remove_replica_modal_pending.set(false);
                    }
                }
            };
            let on_mount_store = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, node_id_opt)) = parse_tree_value(&nv) {
                        let parent_id = node_id_opt.or_else(|| store.root_node_id(s_id));
                        if let Some(pid) = parent_id {
                            let dialog = rinch::dialogs::pick_folder()
                                .set_title("Select Store to Mount");
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
                    }
                }
            };

            // "Mount Remote Store Here..." — store roots and ordinary nodes
            // only. Opens the connect modal in mount mode with this row's
            // canonical target (a store root's target is its own root node).
            let on_mount_remote_store = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, node_id_opt)) = parse_tree_value(&nv) {
                        let parent_id = node_id_opt.or_else(|| store.root_node_id(s_id));
                        match parent_id {
                            Some(pid) => open_connect_modal(store, Some((s_id, pid))),
                            None => tracing::warn!("No target node for a remote mount under {:?}", s_id),
                        }
                    }
                }
            };

            let on_delete = {
                let nv = nv_ctx.clone();
                move || {
                    if let Some((s_id, Some(n_id))) = parse_tree_value(&nv) {
                        store.send(BackendCommand::DeleteNode {
                            store_id: s_id, node_id: n_id,
                        });
                    }
                }
            };

            // Mount-node-only "New Node": creates under the mount's source
            // (mount_ref), never under the mount node itself (decision 2 —
            // the server rejects a mount node as a create parent).
            let on_new_child_mount = {
                let sig = mount_sig;
                move || {
                    let Some(sig) = sig else { return };
                    let mount_ref = untracked(|| sig.with(|m| m.mount_ref.clone()));
                    match mount_ref {
                        Some(mount_ref) => {
                            store.send(BackendCommand::CreateNode {
                                store_id: mount_ref.source_store,
                                parent_id: Some(mount_ref.source_node),
                                title: String::new(),
                            });
                        }
                        None => tracing::warn!("Mount node has no mount_ref yet; cannot create a child under it"),
                    }
                }
            };

            // "Copy as Mount Source" — store roots and ordinary nodes only
            // (not mount nodes, which are placeholders). Not reactive, so a
            // single `move` closure built once is fine (Rule 4/16).
            let on_copy_as_mount_source = {
                let nv = nv_ctx.clone();
                move || copy_as_mount_source(store, &nv)
            };
            // "Paste Mount Here" is always rendered, disabled while nothing is
            // copied. It must NOT sit inside a reactive `if` block: a rinch
            // DropdownMenuItem captures the menu's close signal from a
            // thread-local only during the ContextMenu's own render, so an
            // item rendered later by a reactive block never closes the menu,
            // and the tree rebuild that follows the paste then orphans the
            // still-open portal. `copy_as_mount_source`/`paste_mount_here`
            // bump the tree structure instead, so every row's menu re-renders
            // with a fresh `disabled` value.
            let no_mount_source = untracked(|| store.mount_source.get().is_none());
            let on_paste_mount = {
                let nv = nv_ctx.clone();
                move || paste_mount_here(store, &nv)
            };

            // Build the wrapper span with drag-and-drop via rsx
            let draggable = if is_store_root { "false" } else { "true" };

            let icon_el = render_tabler_icon(__scope, icon, TablerIconStyle::Outline);

            let nv_submit = node_value.clone();
            let nv_effect = node_value.clone();
            let nv_for_context_menu = node_value.clone();

            // Memo: only propagates when THIS node's rename state actually changes.
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
                    value_fn: move || store.rename_text.get(),
                    oninput: move |value: String| {
                        store.rename_text.set(value);
                    },
                    onsubmit: move || {
                        let new_title = store.rename_text.get();
                        store.renaming_node.set(None);
                        clear_keyboard_interceptor();
                        if let Some((s_id, Some(n_id))) = parse_tree_value(&nv_submit) {
                            store.send(BackendCommand::RenameNode {
                                store_id: s_id, node_id: n_id, title: new_title,
                            });
                        }
                    },
                }
            };

            // Register this input's DOM node ID so the double-click handler can focus it
            store.rename_input_ids.update(|ids| {
                ids.insert(node_value.clone(), (rename_input.doc_key(), rename_input.node_id().0));
            });

            // Clone before wrapper rsx moves rename_input
            let rename_input_for_focus = rename_input.clone();

            let wrapper = rsx! {
                span {
                    class: "rinch-tree__label",
                    style: "flex: 1; display: inline-flex; align-items: center;",

                    span {
                        class: icon_class,
                        style: {
                            // Reactive icon opacity: a mount whose source is
                            // out of reach or not reachable yet renders dimmed.
                            move || {
                                if let Some(ms) = mount_sig {
                                    let dimmed = ms.with(|m| {
                                        m.mount_state.as_ref().map_or(false, mount_is_dimmed)
                                    });
                                    if dimmed {
                                        "width: 1rem; height: 1rem; margin-right: 4px; opacity: 0.4;"
                                    } else {
                                        ""
                                    }
                                } else {
                                    ""
                                }
                            }
                        },
                        {icon_el}
                    }

                    span {
                        style: {
                            // Reactive label style — hides during rename, dims
                            // a mount whose source is out of reach or not
                            // reachable yet.
                            move || {
                                if is_renaming.get() {
                                    "display: none;"
                                } else if let Some(ms) = mount_sig {
                                    let dimmed = ms.with(|m| {
                                        m.mount_state.as_ref().map_or(false, mount_is_dimmed)
                                    });
                                    if dimmed {
                                        "cursor: default; opacity: 0.4;"
                                    } else {
                                        base_label_style
                                    }
                                } else {
                                    base_label_style
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
                                let live = parsed.and_then(|(s_id, n_id)| {
                                    let n_id = n_id?;
                                    store.live_label.with(|m| m.get(&(s_id, n_id)).cloned())
                                });
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

                    // Sync status badge (store roots only, linked stores
                    // only) — reactive per store, no tree rebuild
                    // (docs/SYNC_CONTRACT.md "B: app side").
                    span {
                        style: {
                            move || {
                                let linked = sync_sig
                                    .map(|sig| sig.with(|(remote, _)| remote.is_some()))
                                    .unwrap_or(false);
                                if linked {
                                    "margin-left: 6px; cursor: default; font-size: 10px; font-weight: 400; \
                                     text-transform: none; letter-spacing: normal; opacity: 0.6;"
                                } else {
                                    "display: none;"
                                }
                            }
                        },
                        {move || {
                            sync_sig
                                .map(|sig| sig.with(|(_, state)| sync_badge_text(state).to_string()))
                                .unwrap_or_default()
                        }}
                    }

                    {rename_input}
                }
            };

            // Wrap in ContextMenu — different items for store roots vs nodes
            let context_menu = if is_store_root {
                rsx! {
                    ContextMenu {
                        ContextMenuTarget { {wrapper} }
                        ContextMenuDropdown {
                            DropdownMenuItem {
                                left_section: TablerIcon::FilePlus,
                                onclick: on_new_child,
                                "New Node"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Link,
                                onclick: on_mount_store,
                                "Mount Store..."
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::CloudDownload,
                                onclick: on_mount_remote_store,
                                "Mount Remote Store Here..."
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Copy,
                                onclick: on_copy_as_mount_source,
                                "Copy as Mount Source"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::ClipboardCopy,
                                disabled: no_mount_source,
                                onclick: on_paste_mount,
                                "Paste Mount Here"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Cloud,
                                disabled: is_linked_now,
                                onclick: on_link_to_remote,
                                "Link to Remote..."
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Unlink,
                                disabled: !is_linked_now,
                                onclick: on_unlink_from_remote,
                                "Unlink from Remote"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Trash,
                                disabled: !is_replica_now,
                                onclick: on_remove_replica,
                                "Remove Replica..."
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::X,
                                onclick: on_close_store,
                                "Close Store"
                            }
                        }
                    }
                }
            } else if is_mount {
                rsx! {
                    ContextMenu {
                        ContextMenuTarget { {wrapper} }
                        ContextMenuDropdown {
                            DropdownMenuItem {
                                left_section: TablerIcon::FilePlus,
                                onclick: on_new_child_mount,
                                "New Node"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Trash,
                                onclick: on_delete,
                                "Delete"
                            }
                        }
                    }
                }
            } else {
                // Regular nodes: new child, mount store, rename
                let nv_rename_ctx = nv_for_context_menu.clone();
                let on_rename = move || {
                    if let Some((s_id, Some(n_id))) = parse_tree_value(&nv_rename_ctx) {
                        let edit_text = store.get_node_signal(s_id, n_id)
                            .map(|sig| sig.with(|n| n.metadata.title.clone()))
                            .unwrap_or_default();
                        store.rename_text.set(edit_text);
                        store.renaming_node.set(Some(nv_rename_ctx.clone()));
                        let nv_interceptor = nv_rename_ctx.clone();
                        set_keyboard_interceptor(move |data| {
                            if data.key == "Escape" {
                                rinch::run_on_main_thread(move || {
                                    store.renaming_node.set(None);
                                    clear_keyboard_interceptor();
                                });
                                return true;
                            }
                            if data.key == "Enter" {
                                let nv = nv_interceptor.clone();
                                rinch::run_on_main_thread(move || {
                                    let new_title = untracked(|| store.rename_text.get());
                                    store.renaming_node.set(None);
                                    clear_keyboard_interceptor();
                                    if let Some((s_id, Some(n_id))) = parse_tree_value(&nv) {
                                        store.send(BackendCommand::RenameNode {
                                            store_id: s_id, node_id: n_id, title: new_title,
                                        });
                                    }
                                });
                                return true;
                            }
                            false
                        });
                        rename_input_for_focus.focus();
                    }
                };

                rsx! {
                    ContextMenu {
                        ContextMenuTarget { {wrapper} }
                        ContextMenuDropdown {
                            DropdownMenuItem {
                                left_section: TablerIcon::FilePlus,
                                onclick: on_new_child,
                                "New Node"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Link,
                                onclick: on_mount_store,
                                "Mount Store..."
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::CloudDownload,
                                onclick: on_mount_remote_store,
                                "Mount Remote Store Here..."
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Copy,
                                onclick: on_copy_as_mount_source,
                                "Copy as Mount Source"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::ClipboardCopy,
                                disabled: no_mount_source,
                                onclick: on_paste_mount,
                                "Paste Mount Here"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Edit,
                                onclick: on_rename,
                                "Rename"
                            }
                            DropdownMenuItem {
                                left_section: TablerIcon::Trash,
                                onclick: on_delete,
                                "Delete"
                            }
                        }
                    }
                }
            };

            // The ContextMenu sets data-oncontextmenu on its ContextMenuTarget,
            // but both rinch-context-menu and rinch-context-menu__target have
            // display:contents — they're invisible to hit testing. Wrap in a
            // visible div that carries:
            //  - context menu handler (hoisted from ContextMenuTarget)
            //  - drag-and-drop attributes (draggable, ondragstart, etc.)
            //  - visual feedback for drag state (opacity, highlight)
            // This ensures right-click, drag, and drop all work on the full row.
            let hit_area = rsx! {
                div {
                    style: {
                        let nv = nv_effect.clone();
                        move || {
                            let base = "flex: 1; display: flex; align-items: center; border-radius: 2px; transition: background 0.1s, opacity 0.1s;";
                            let is_being_dragged = drag_ctx.get().as_deref() == Some(&nv);
                            let is_target = store.drop_target.get().as_deref() == Some(&nv)
                                && drag_ctx.is_active();
                            if is_being_dragged {
                                format!("{} opacity: 0.3;", base)
                            } else if is_target {
                                format!("{} background: var(--rinch-color-blue-1); outline: 1px solid var(--rinch-color-blue-4);", base)
                            } else {
                                base.to_string()
                            }
                        }
                    },
                    draggable: draggable,
                    ondragstart: on_dragstart,
                    ondragend: on_dragend,
                    ondrop: on_drop,
                    ondragenter: on_dragenter,
                    ondragleave: on_dragleave,
                    {context_menu}
                }
            };
            hoist_context_menu_handler(&hit_area, &hit_area);
            hit_area
        });

        let tree_scroll = rsx! {
            div {
                class: "pimble-sidebar__tree",
                Tree {
                    data: untracked(|| store.build_tree_data_structural()),
                    tree: tree_state,
                    data_source: data_source,
                    level_offset: "xs",
                    select_on_click: true,
                    expand_on_click: false,
                    onselect: on_tree_select,
                    onexpand: on_tree_expand,
                    oncollapse: on_tree_collapse,
                    render_node: render_node_fn,
                }
            }
        };

        // Toolbar button handlers — new node (in selected store, or first store)
        let on_new_node = move || {
            // Try to use the currently selected store
            let result = store.selected_id.get()
                .and_then(|sel| parse_tree_value(&sel))
                .and_then(|(s_id, _)| {
                    store.root_node_id(s_id).map(|rid| (s_id, rid))
                })
                // Fall back to first store
                .or_else(|| {
                    let first_sid = untracked(|| store.store_ids.with(|ids| ids.first().copied()));
                    first_sid.and_then(|sid| {
                        store.root_node_id(sid).map(|rid| (sid, rid))
                    })
                });
            if let Some((s_id, root_id)) = result {
                store.send(BackendCommand::CreateNode {
                    store_id: s_id,
                    parent_id: Some(root_id),
                    title: String::new(),
                });
            }
        };

        // Rich text editor (the rinch `Editor {}` component over the shared
        // thread-local `EditorHandle`). Collaboration is wired in `start_editing`:
        // local edits broadcast their deltas through the server relay, and remote
        // deltas arrive via `BackendEvent::RemoteChanges`. There is no manual
        // autosave — the collab session persists edits live (the server applies
        // and relays each delta).
        let editor_view = rsx! {
            Editor { editor: crate::editor::editor() }
        };

        let toolbar_handle = crate::toolbar::render_pimble_toolbar(__scope);

        // Editor empty state icon
        let empty_icon = render_tabler_icon(__scope, TablerIcon::FileText, TablerIconStyle::Outline);

        // Connection status color (pure derivation → Memo)
        let status_color = Memo::new(move || {
            let status = store.connection_status.get();
            if status == "Connected" {
                "green".to_string()
            } else if status.starts_with("Error") {
                "red".to_string()
            } else {
                "yellow".to_string()
            }
        });

        // ── Search box + results panel ────────────────────────────────
        // The results panel replaces the tree while `search_query` is
        // non-empty; Esc (via `clear_search`'s interceptor) restores it.
        let search_icon = render_tabler_icon(__scope, TablerIcon::Search, TablerIconStyle::Outline);
        let search_input = rsx! {
            input {
                r#type: "text",
                class: "pimble-search-bar__input",
                placeholder: "Search... (Ctrl+K)",
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
                                rinch::run_on_main_thread(move || clear_search(store));
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
                span { class: "pimble-search-bar__icon", {search_icon} }
                {search_input}
                if !store.search_query.get().is_empty() {
                    ActionIcon {
                        icon: TablerIcon::X,
                        variant: "subtle",
                        size: "xs",
                        class: "pimble-search-bar__clear",
                        onclick: move || clear_search(store),
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

        // Spawn the backend now that the UI and event loop are fully set up.
        // The EVENT_PROCESSOR thread-local is already registered, so signal_ui
        // callbacks will be processed correctly via run_on_main_thread.
        if store.backend.with(|b| b.is_none()) {
            let backend = BackendHandle::spawn(move || {
                rinch::run_on_main_thread(|| {
                    EVENT_PROCESSOR.with(|cell| {
                        if let Some(f) = cell.borrow().as_ref() {
                            f();
                        }
                    });
                });
            });
            store.backend.set(Some(backend));
        }

        rsx! {
            BorderlessWindow {
                title: "Pimble",
                show_minimize: true,
                show_maximize: true,
                show_close: true,
                on_close: || close_current_window(),

                style { {APP_CSS} }
                style { {EDITOR_CSS} }

                {connect_modal}
                {link_modal}
                {remove_replica_modal}

                // Outer wrapper — flex column fills the content area, pushes
                // status bar to the very bottom.
                div {
                    style: "display: flex; flex-direction: column; height: 100%;",

                    {search_bar}

                    // ── Main content ──────────────────────────────
                    div {
                        style: "display: flex; flex: 1; overflow: hidden;",

                        // ── Sidebar ───────────────────────────────
                        div {
                            class: "pimble-sidebar",

                            // Header: store name + new-node button
                            div {
                                class: "pimble-sidebar__header",

                                span {
                                    class: "pimble-sidebar__heading",
                                    "EXPLORER"
                                }

                                ActionIcon {
                                    icon: TablerIcon::Plus,
                                    variant: "subtle",
                                    size: "xs",
                                    onclick: on_new_node,
                                }
                            }

                            div {
                                style: {|| if store.search_query.get().is_empty() { "" } else { "display: none;" }},
                                {tree_scroll}
                            }
                            div {
                                style: {|| if store.search_query.get().is_empty() { "display: none;" } else { "" }},
                                {search_panel}
                            }
                        }

                        // ── Editor panel ──────────────────────────
                        div {
                            class: "pimble-editor",
                            onclick: move || cancel_rename(true),

                            div {
                                class: "pimble-editor__toolbar-wrap",
                                style: {|| if store.show_editor.get() { "" } else { "display: none;" }},
                                {toolbar_handle}
                            }
                            div {
                                class: "pimble-editor__content-wrap",
                                style: {|| if store.show_editor.get() { "" } else { "display: none;" }},
                                {editor_view}
                            }
                            div {
                                class: "pimble-empty-state",
                                style: {|| if store.show_editor.get() { "display: none;" } else { "" }},
                                div {
                                    class: "pimble-empty-state__icon",
                                    {empty_icon}
                                }
                                div {
                                    class: "pimble-empty-state__text",
                                    "Select a document to start editing"
                                }
                                div {
                                    class: "pimble-empty-state__hint",
                                    "Or press Ctrl+N to create a new store"
                                }
                            }
                        }
                    }

                    // ── Status bar ────────────────────────────────
                    div {
                        class: "pimble-status-bar",

                        Badge {
                            variant: "dot",
                            color: {|| status_color.get()},
                            size: "xs",
                            {|| store.connection_status.get()}
                        }

                        span {
                            class: "pimble-status-bar__addr",
                            {|| {
                                let addr = store.server_addr.get();
                                if addr.is_empty() { String::new() } else { addr }
                            }}
                        }

                        div { style: "flex: 1;", }
                    }
                }
            }
        }
    };

    App::new(app_component)
        .window_props(props)
        .theme(theme)
        .menu(menus)
        .run();

    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = None;
    });
    CLOSE_HANDLER.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
