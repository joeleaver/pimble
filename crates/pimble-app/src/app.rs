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
use crate::editor::{save_content_via_ce_api, load_content_into_ce};
use crate::events::{EVENT_PROCESSOR, process_backend_events};
use crate::state::{parse_tree_value, display_label_from_node, AppStore, PendingMount};
use crate::styles::{APP_CSS, EDITOR_CSS};

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

    // Shared CE div handle for content loading from backend events
    let ce_div_cell: Rc<RefCell<Option<NodeHandle>>> = Rc::new(RefCell::new(None));

    // Persistent tree state — created once, preserves expanded/selected across data changes
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());

    // Drag-and-drop state for tree node rearrangement
    let drag_ctx: DragContext<String> = DragContext::new();

    // Set up event processing via thread-local so run_on_main_thread
    // can trigger it without capturing non-Send types.
    let ce_div_for_events = ce_div_cell.clone();
    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            process_backend_events(
                store,
                tree_state,
                &ce_div_for_events,
            );
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
        .item(MenuItem::new("Paste").shortcut("Ctrl+V").enabled(false).on_click(|| {}));

    let view_menu = Menu::new()
        .item(MenuItem::new("Toggle Sidebar").shortcut("Ctrl+\\").on_click(|| {
            tracing::info!("Toggle sidebar");
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
        dark_mode: true,
        default_radius: Some("sm".into()),
        ..Default::default()
    };

    // Save-on-close: register a thread-local that the close callback invokes.
    // WindowProps requires Send+Sync but our state is Rc-based (main thread only),
    // so we use the same thread-local pattern as EVENT_PROCESSOR.
    thread_local! {
        static CLOSE_HANDLER: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
    }
    let ce_div_for_close = ce_div_cell.clone();
    CLOSE_HANDLER.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            if let Some(ce_div) = ce_div_for_close.borrow().as_ref() {
                if let Some(bytes) = save_content_via_ce_api(ce_div) {
                    if let Some(selected_id) = store.selected_id.get() {
                        if let Some((store_id, Some(node_id))) = parse_tree_value(&selected_id) {
                            store.send(BackendCommand::SetNodeContent {
                                store_id,
                                node_id,
                                content: bytes,
                            });
                        }
                    }
                }
            }
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
    let ce_div_for_app = ce_div_cell.clone();
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
        let select_ce_div = ce_div_for_app.clone();
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
                    if let Some(input_node_id) = store.rename_input_ids.with(|ids| ids.get(&value).copied()) {
                        request_focus(input_node_id);
                    }
                    return;
                }

                // If we were renaming a node, commit it before switching
                cancel_rename_for_select(true);
            }

            // Save current editor content to previously selected node
            if let Some(ce_div) = select_ce_div.borrow().as_ref() {
                if let Some(bytes) = save_content_via_ce_api(ce_div) {
                    if let Some(prev_id) = store.selected_id.get() {
                        if let Some((s_id, Some(n_id))) = parse_tree_value(&prev_id) {
                            store.send(BackendCommand::SetNodeContent {
                                store_id: s_id,
                                node_id: n_id,
                                content: bytes,
                            });
                        }
                    }
                }
            }

            // Update selection and load new node
            store.selected_id.set(Some(value.clone()));

            let (title_opt, content_bytes_opt, is_document) = if let Some((s_id, node_id_opt)) = parse_tree_value(&value) {
                if let Some(n_id) = node_id_opt {
                    let result = store.get_node_signal(s_id, n_id).map(|sig| {
                        sig.with(|node| {
                            let label = display_label_from_node(node);
                            let is_doc = node.node_type == pimble_core::node_types::DOCUMENT;
                            (label, node.content.clone(), is_doc)
                        })
                    });
                    match result {
                        Some((label, content, is_doc)) => (Some(label), Some(content), is_doc),
                        None => {
                            store.send(BackendCommand::GetNode { store_id: s_id, node_id: n_id });
                            (None, None, false)
                        }
                    }
                } else {
                    let title = store.get_store_signal(s_id)
                        .map(|sig| sig.with(|s| s.name.clone()));
                    match title {
                        Some(t) => (Some(t), Some(Vec::new()), false),
                        None => (None, None, false),
                    }
                }
            } else {
                (None, None, false)
            };

            store.show_editor.set(is_document);

            if let Some(title) = title_opt {
                store.node_title.set(title);
            }

            // Load content into editor
            if is_document {
                let content_bytes = content_bytes_opt.unwrap_or_default();
                if let Some(ce_div) = select_ce_div.borrow().as_ref() {
                    load_content_into_ce(&content_bytes, ce_div);
                }
            }
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

            // Choose icon (static — changes only on structural rebuild)
            let icon = if is_store_root {
                TablerIcon::Database
            } else if is_mount {
                TablerIcon::Link
            } else if has_children {
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
                        if drag_store_id != target_store_id { return; }
                        match target_node_id_opt {
                            Some(nid) => nid,
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
                ids.insert(node_value.clone(), rename_input.node_id().0);
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
                            // Reactive icon opacity for mount state
                            move || {
                                if let Some(ms) = mount_sig {
                                    let unavailable = ms.with(|m| {
                                        m.mount_state.as_ref().map_or(false, |s| {
                                            matches!(s, pimble_core::MountState::Unavailable)
                                        })
                                    });
                                    if unavailable {
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
                            // Reactive label style — hides during rename, dims for unavailable mounts
                            move || {
                                if is_renaming.get() {
                                    "display: none;"
                                } else if let Some(ms) = mount_sig {
                                    let unavailable = ms.with(|m| {
                                        m.mount_state.as_ref().map_or(false, |s| {
                                            matches!(s, pimble_core::MountState::Unavailable)
                                        })
                                    });
                                    if unavailable {
                                        "cursor: default; opacity: 0.4;"
                                    } else {
                                        base_label_style
                                    }
                                } else {
                                    base_label_style
                                }
                            }
                        },

                        // Reactive label text — subscribes to per-node/per-store signal only
                        {move || {
                            let label = if is_store_root {
                                store_sig.map(|s| s.with(|st| st.name.clone()))
                                    .unwrap_or_default()
                            } else {
                                node_sig.map(|s| s.with(|n| display_label_from_node(n)))
                                    .unwrap_or_else(|| "Untitled".to_string())
                            };

                            if let Some(ms) = mount_sig {
                                let unavailable = ms.with(|m| {
                                    m.mount_state.as_ref().map_or(false, |s| {
                                        matches!(s, pimble_core::MountState::Unavailable)
                                    })
                                });
                                if unavailable {
                                    format!("{} (unavailable)", label)
                                } else {
                                    label
                                }
                            } else {
                                label
                            }
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
                                onclick: on_new_child,
                                "New Node"
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
                        request_focus(rename_input_for_focus.node_id().0);
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
                                left_section: TablerIcon::Edit,
                                onclick: on_rename,
                                "Rename"
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

        // Rich text editor setup
        let ce_div = rsx! {
            div {
                contenteditable: "true",
                class: "editor-content",
                style: "outline: none; flex: 1; padding: 24px 32px; cursor: text;",
            }
        };
        // Share ce_div handle for content loading from backend events
        *ce_div_for_app.borrow_mut() = Some(ce_div.clone());

        // Subscribe to CE events so toolbar active state updates on every
        // cursor move, formatting change, etc.
        rinch::core::ce::subscribe_ce_events(Rc::new(|_event| {
            crate::toolbar::bump_toolbar();
        }));

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

                            {tree_scroll}
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
                                {ce_div}
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

    rinch::run_with_window_props_and_menu(
        app_component,
        props,
        Some(theme),
        Some(menus),
    );

    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = None;
    });
    CLOSE_HANDLER.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
