//! Pimble Desktop Application
//!
//! Built with Rinch UI framework

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use pimble_core::{Node, NodeId, Store, StoreId};
use rinch::prelude::*;
use rinch::core::{request_focus, set_keyboard_interceptor, clear_keyboard_interceptor};
use rinch::menu::{Menu, MenuItem};
use rinch_editor::prelude::*;
use rinch_editor_components::{render_toolbar, ToolbarConfig};
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::backend::{BackendCommand, BackendEvent, BackendHandle};
use crate::state::{get_node_content_text, AppState, ConnectionState};

fn state_file_path() -> std::path::PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    config_dir.join("pimble").join("state.json")
}

fn load_app_state_file() -> Vec<String> {
    let path = state_file_path();
    let Ok(json) = std::fs::read_to_string(&path) else { return Vec::new() };
    serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|v| v["open_stores"].as_array().cloned())
        .map(|arr| arr.into_iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn save_app_state_file(paths: &[String]) {
    let path = state_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::json!({ "open_stores": paths });
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap_or_default());
}

thread_local! {
    static EVENT_PROCESSOR: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
}

/// Deferred signal updates collected while state is borrowed
#[derive(Default)]
struct DeferredUpdates {
    connection_status: Option<String>,
    sidebar_heading: Option<String>,
    tree_data: Option<Vec<TreeNodeData>>,
    expand_nodes: Vec<String>,
    node_title: Option<String>,
    editor_content: Option<String>,
}

/// Process backend events and update signals
///
/// Signal updates are deferred until after the state borrow is released,
/// to avoid re-entrancy panics when Rinch's reactive system triggers
/// effects that try to borrow the same RefCell.
fn process_backend_events(
    state: &Rc<RefCell<AppState>>,
    connection_status: Signal<String>,
    sidebar_heading: Signal<String>,
    tree_data: Signal<Vec<TreeNodeData>>,
    tree_state: UseTreeReturn,
    node_title: Signal<String>,
    editor_rc: Rc<RefCell<Editor>>,
    bridge_cell: Rc<RefCell<Option<EditorBridge>>>,
) {
    let (_events, deferred) = {
        let events: Vec<BackendEvent> = {
            let state = state.borrow();
            let Some(backend) = &state.backend else {
                return;
            };
            let mut events = Vec::new();
            while let Some(event) = backend.try_recv() {
                events.push(event);
            }
            events
        };

        if events.is_empty() {
            return;
        }

        let mut state = state.borrow_mut();
        let mut deferred = DeferredUpdates::default();

        for event in &events {
            match event {
                BackendEvent::Connected => {
                    tracing::info!("Connected to backend");
                    state.connection = ConnectionState::Connected;
                    deferred.connection_status = Some("Connected".to_string());

                    // Auto-open previously loaded stores
                    let saved_paths = load_app_state_file();
                    if let Some(backend) = &state.backend {
                        for path in saved_paths {
                            tracing::info!("Auto-opening saved store: {}", path);
                            backend.send(BackendCommand::OpenStore { path });
                        }
                    }
                }

                BackendEvent::Disconnected => {
                    tracing::info!("Disconnected from backend");
                    state.connection = ConnectionState::Disconnected;
                    deferred.connection_status = Some("Disconnected".to_string());
                }

                BackendEvent::Error { message } => {
                    tracing::error!("Backend error: {}", message);
                    state.connection = ConnectionState::Error(message.clone());
                    deferred.connection_status = Some(format!("Error: {}", message));
                }

                BackendEvent::StoreOpened { store } => {
                    tracing::info!("Store opened: {}", store.name);
                    let store_id = store.id;
                    let root_id = store.root_node_id;
                    state.stores.insert(store_id, store.clone());

                    state.expanded.insert((store_id, root_id));
                    if let Some(backend) = &state.backend {
                        let _ = backend.send(BackendCommand::GetChildren {
                            store_id,
                            node_id: root_id,
                        });
                    }

                    deferred.sidebar_heading = Some(state.sidebar_heading());
                    deferred.tree_data = Some(state.build_tree_data());

                    // Persist open store paths
                    let paths: Vec<String> = state.stores.values()
                        .filter_map(|s| s.local_path().map(|p| p.to_string_lossy().to_string()))
                        .collect();
                    save_app_state_file(&paths);
                }

                BackendEvent::StoreCreated { store_id, root_node_id } => {
                    tracing::info!("Store created: {:?} with root {:?}", store_id, root_node_id);
                    if let Some(path) = state.pending_create_path.take() {
                        tracing::info!("Auto-opening created store at: {}", path);
                        if let Some(backend) = &state.backend {
                            let _ = backend.send(BackendCommand::OpenStore { path });
                        }
                    }
                }

                BackendEvent::ChildrenLoaded { store_id, parent_id, children } => {
                    tracing::info!("Children loaded for {:?}: {} nodes", parent_id, children.len());

                    let child_ids: Vec<NodeId> = children.iter().map(|n| n.id).collect();
                    state.children.insert((*store_id, *parent_id), child_ids);

                    for child in children {
                        // Only overwrite if the incoming node is at least as recent
                        // as the cached one, to avoid stale ChildrenLoaded responses
                        // clobbering a freshly-renamed/updated node.
                        let dominated = state.nodes.get(&(*store_id, child.id))
                            .map_or(true, |cached| child.metadata.modified_at >= cached.metadata.modified_at);
                        if dominated {
                            state.nodes.insert((*store_id, child.id), child.clone());
                        }
                    }

                    deferred.tree_data = Some(state.build_tree_data());
                }

                BackendEvent::NodeLoaded { store_id, node } => {
                    tracing::info!("Node loaded: {:?} - {}", node.id, node.metadata.title);
                    let node_id = node.id;
                    let content_bytes = node.content.clone();
                    state.nodes.insert((*store_id, node_id), node.clone());

                    // Rebuild tree so display labels (auto-title excerpts) update
                    deferred.tree_data = Some(state.build_tree_data());

                    if let Some(selected_id) = &state.selected_id {
                        if let Some((sel_store_id, Some(sel_node_id))) = state.parse_tree_value(selected_id) {
                            if sel_store_id == *store_id && sel_node_id == node_id {
                                deferred.node_title = Some(state.display_label(*store_id, node_id));
                                deferred.editor_content = Some(get_node_content_text(&content_bytes));
                            }
                        }
                    }
                }

                BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id } => {
                    tracing::info!("Node moved: {:?} from {:?} to {:?}", node_id, old_parent_id, new_parent_id);

                    // Optimistic update: directly modify the children caches
                    // instead of clearing everything and waiting for async
                    // re-fetches (which created intermediate states with
                    // duplicate nodes).

                    // Remove moved node from old parent's children list
                    if let Some(old_children) = state.children.get_mut(&(*store_id, *old_parent_id)) {
                        old_children.retain(|&id| id != *node_id);
                    }

                    // Add moved node to new parent's children list
                    if let Some(new_children) = state.children.get_mut(&(*store_id, *new_parent_id)) {
                        if !new_children.contains(node_id) {
                            new_children.push(*node_id);
                        }
                    } else {
                        // New parent had no cached children — create entry
                        state.children.insert((*store_id, *new_parent_id), vec![*node_id]);
                    }

                    // The moved node's own children cache stays intact —
                    // its subtree structure hasn't changed.

                    // Auto-expand the new parent so the moved node is visible
                    state.expanded.insert((*store_id, *new_parent_id));
                    let is_root = state.stores.get(store_id)
                        .map_or(false, |s| s.root_node_id == *new_parent_id);
                    if !is_root {
                        deferred.expand_nodes.push(format!("node_{}_{}", store_id, new_parent_id));
                    }

                    // Rebuild tree immediately with correct data
                    deferred.tree_data = Some(state.build_tree_data());

                    // Also re-fetch from server for authoritative data
                    if let Some(backend) = &state.backend {
                        backend.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *old_parent_id });
                        backend.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *new_parent_id });
                    }
                }

                BackendEvent::NodeContentUpdated { store_id, node_id } => {
                    tracing::info!("Node content updated: {:?}/{:?}", store_id, node_id);
                    // Re-fetch the node so cached content updates;
                    // tree rebuild will pick up the new excerpt for auto-titled nodes.
                    if let Some(backend) = &state.backend {
                        backend.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
                    }
                }

                BackendEvent::NodeRenamed { store_id, node_id } => {
                    tracing::info!("Node renamed: {:?}/{:?}", store_id, node_id);
                    // Re-fetch the node to update cached metadata, then rebuild tree
                    if let Some(backend) = &state.backend {
                        backend.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
                    }
                }

                BackendEvent::StoreClosed { store_id } => {
                    tracing::info!("Store closed: {:?}", store_id);
                    state.stores.remove(store_id);
                    deferred.sidebar_heading = Some(state.sidebar_heading());
                    deferred.tree_data = Some(state.build_tree_data());

                    // Persist open store paths
                    let paths: Vec<String> = state.stores.values()
                        .filter_map(|s| s.local_path().map(|p| p.to_string_lossy().to_string()))
                        .collect();
                    save_app_state_file(&paths);
                }

                _ => {}
            }
        }

        (events, deferred)
    };
    // State borrow is now released — safe to set signals

    if let Some(v) = deferred.connection_status {
        connection_status.set(v);
    }
    if let Some(v) = deferred.sidebar_heading {
        sidebar_heading.set(v);
    }
    if let Some(v) = deferred.tree_data {
        tree_data.set(v);
    }
    for value in deferred.expand_nodes {
        tree_state.controller.expand(&value);
    }
    if let Some(v) = deferred.node_title {
        node_title.set(v);
    }
    if let Some(content) = deferred.editor_content {
        if let Ok(mut ed) = editor_rc.try_borrow_mut() {
            ed.set_markdown(&content).ok();
        }
        if let Some(bridge) = bridge_cell.borrow().as_ref() {
            bridge.reconcile();
        }
    }
}

/// Main application entry point
pub fn run() {
    // Shared state
    let state = Rc::new(RefCell::new(AppState::new()));

    // Reactive signals
    let connection_status: Signal<String> = Signal::new("Connecting...".to_string());
    let sidebar_heading: Signal<String> = Signal::new(String::new());
    let tree_data: Signal<Vec<TreeNodeData>> = Signal::new(Vec::new());
    let node_title: Signal<String> = Signal::new(String::new());

    // Inline rename state
    let renaming_node: Signal<Option<String>> = Signal::new(None);
    let rename_text: Signal<String> = Signal::new(String::new());
    // Double-click detection for rename: (last_click_time, last_click_value)
    let last_click: Rc<Cell<(Instant, String)>> = Rc::new(Cell::new((Instant::now(), String::new())));

    // Rich text editor (shared between component, event processing, and tree selection)
    let editor = Editor::new(Schema::starter_kit(), EditorConfig::default()).unwrap();
    let editor_rc: Rc<RefCell<Editor>> = Rc::new(RefCell::new(editor));
    let bridge_cell: Rc<RefCell<Option<EditorBridge>>> = Rc::new(RefCell::new(None));

    // Persistent tree state — created once, preserves expanded/selected across data changes
    let tree_state = UseTreeReturn::new(UseTreeOptions::default());

    // Drag-and-drop state for tree node rearrangement
    let drag_ctx: DragContext<String> = DragContext::new();
    let drop_target: Signal<Option<String>> = Signal::new(None);

    // Set up event processing via thread-local so run_on_main_thread
    // can trigger it without capturing non-Send types.
    let state_for_events = state.clone();
    let editor_for_events = editor_rc.clone();
    let bridge_for_events = bridge_cell.clone();
    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move || {
            process_backend_events(
                &state_for_events,
                connection_status,
                sidebar_heading,
                tree_data,
                tree_state,
                node_title,
                editor_for_events.clone(),
                bridge_for_events.clone(),
            );
        }));
    });

    let backend = BackendHandle::spawn(move || {
        // Schedule event processing on the main thread (outside any Effect context)
        rinch::run_on_main_thread(|| {
            EVENT_PROCESSOR.with(|cell| {
                if let Some(f) = cell.borrow().as_ref() {
                    f();
                }
            });
        });
    });

    state.borrow_mut().backend = Some(backend);

    // Build menus
    let state_for_new = state.clone();
    let state_for_open = state.clone();
    let state_for_close = state.clone();

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
                let mut st = state_for_new.borrow_mut();
                st.pending_create_path = Some(path_str.clone());
                if let Some(backend) = &st.backend {
                    backend.send(BackendCommand::CreateStore { path: path_str, name });
                }
            }
        }))
        .item(MenuItem::new("Open Store...").shortcut("Ctrl+O").on_click(move || {
            tracing::info!("Open store menu clicked");
            let dialog = rinch::dialogs::pick_folder()
                .set_title("Open Store");

            if let Some(path) = dialog.pick() {
                let path_str = path.to_string_lossy().to_string();
                tracing::info!("Opening store at: {}", path_str);
                let st = state_for_open.borrow();
                if let Some(backend) = &st.backend {
                    backend.send(BackendCommand::OpenStore { path: path_str });
                }
            }
        }))
        .separator()
        .item(MenuItem::new("Close Store").on_click(move || {
            tracing::info!("Close store");
            let st = state_for_close.borrow();
            if let Some((store_id, _)) = st.selected_store_and_node() {
                if let Some(backend) = &st.backend {
                    backend.send(BackendCommand::CloseStore { store_id });
                }
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
        .item(MenuItem::new("Toggle Sidebar").shortcut("Ctrl+B").on_click(|| {
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

    let props = WindowProps {
        title: "Pimble".into(),
        width: 1200,
        height: 800,
        borderless: true,
        resizable: true,
        menu_in_titlebar: true,
        ..Default::default()
    };

    // Build app component - parameter must be named __scope for the rsx! macro
    let app_state = state.clone();
    let editor_for_app = editor_rc.clone();
    let bridge_for_app = bridge_cell.clone();
    let app_component = move |__scope: &mut RenderScope| -> NodeHandle {
        // Tree callbacks
        let select_state = app_state.clone();
        let select_editor = editor_for_app.clone();
        let select_bridge = bridge_for_app.clone();
        let select_title = node_title;

        let select_last_click = last_click.clone();
        let on_tree_select = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node selected: {}", value);

            // Double-click on a node → enter rename mode
            {
                let now = Instant::now();
                let (prev_time, prev_value) = select_last_click.replace((now, value.clone()));
                let is_double_click = prev_value == value && now.duration_since(prev_time).as_millis() < 500;

                let st = select_state.borrow();
                if is_double_click && value.starts_with("node_") {
                    let edit_text = if let Some((store_id, Some(node_id))) = st.parse_tree_value(&value) {
                        st.nodes.get(&(store_id, node_id))
                            .map(|n| n.metadata.title.clone())
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    drop(st);
                    rename_text.set(edit_text);
                    renaming_node.set(Some(value.clone()));
                    return;
                }

                // If we were renaming a node, commit it before switching
                let was_renaming = renaming_node.get();
                if let Some(prev_value) = was_renaming {
                    let new_title = rename_text.get();
                    if let Some((sid, Some(nid))) = st.parse_tree_value(&prev_value) {
                        if let Some(backend) = &st.backend {
                            backend.send(BackendCommand::RenameNode {
                                store_id: sid, node_id: nid, title: new_title,
                            });
                        }
                    }
                }

                drop(st);
                renaming_node.set(None);
            }

            // Save current editor content to previously selected node
            {
                let md = select_editor.borrow().get_markdown();
                let st = select_state.borrow();
                if let Some(prev_id) = &st.selected_id {
                    if let Some((store_id, Some(node_id))) = st.parse_tree_value(prev_id) {
                        if let Some(backend) = &st.backend {
                            backend.send(BackendCommand::SetNodeContent {
                                store_id,
                                node_id,
                                text: md,
                            });
                        }
                    }
                }
            }

            // Update selection and collect title + content
            let (title_opt, content_opt) = {
                let mut st = select_state.borrow_mut();
                st.selected_id = Some(value.clone());

                if let Some((store_id, node_id_opt)) = st.parse_tree_value(&value) {
                    if let Some(node_id) = node_id_opt {
                        if let Some(node) = st.nodes.get(&(store_id, node_id)) {
                            let label = st.display_label(store_id, node_id);
                            (Some(label), Some(get_node_content_text(&node.content)))
                        } else {
                            if let Some(backend) = &st.backend {
                                backend.send(BackendCommand::GetNode { store_id, node_id });
                            }
                            (None, None)
                        }
                    } else if let Some(store) = st.stores.get(&store_id) {
                        let title = store.name.clone();
                        let path_str = store.local_path()
                            .map(|p| p.to_string_lossy().to_string())
                            .unwrap_or_else(|| "remote".to_string());
                        let content = format!(
                            "Store: {}\nPath: {}\nRoot Node: {:?}",
                            store.name, path_str, store.root_node_id
                        );
                        (Some(title), Some(content))
                    } else {
                        (None, None)
                    }
                } else {
                    (None, None)
                }
            };
            // State borrow released — safe to set signals and update editor
            if let Some(title) = title_opt {
                select_title.set(title);
            }

            // Load content into editor
            let content = content_opt.unwrap_or_default();
            if let Ok(mut ed) = select_editor.try_borrow_mut() {
                ed.set_markdown(&content).ok();
            }
            if let Some(bridge) = select_bridge.borrow().as_ref() {
                bridge.reconcile();
            }
        });

        let expand_state = app_state.clone();
        let on_tree_expand = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node expanded: {}", value);
            let mut st = expand_state.borrow_mut();
            if let Some((store_id, node_id_opt)) = st.parse_tree_value(&value) {
                let resolved = node_id_opt.or_else(|| {
                    st.stores.get(&store_id).map(|s| s.root_node_id)
                });
                if let Some(nid) = resolved {
                    st.expanded.insert((store_id, nid));
                    if !st.children.contains_key(&(store_id, nid)) {
                        if let Some(backend) = &st.backend {
                            backend.send(BackendCommand::GetChildren { store_id, node_id: nid });
                        }
                    }
                }
            }
            // No signal mutation — UseTreeReturn handles expanded state internally
        });

        let collapse_state = app_state.clone();
        let on_tree_collapse = ValueCallback::new(move |value: String| {
            tracing::info!("Tree node collapsed: {}", value);
            let mut st = collapse_state.borrow_mut();
            if let Some((store_id, node_id_opt)) = st.parse_tree_value(&value) {
                let resolved = node_id_opt.or_else(|| {
                    st.stores.get(&store_id).map(|s| s.root_node_id)
                });
                if let Some(nid) = resolved {
                    st.expanded.remove(&(store_id, nid));
                }
            }
        });

        // Build the tree with reactive data_source — the Tree diffs root nodes by key,
        // and the persistent UseTreeReturn preserves expanded/selected state across updates.
        let tree_scroll = __scope.create_element("div");
        tree_scroll.set_attribute("style", "flex: 1; overflow-y: auto; padding: 0 4px;");

        let data_source: Rc<dyn Fn() -> Vec<TreeNodeData>> =
            Rc::new(move || tree_data.get());

        // Custom render_node with drag-and-drop support
        let dnd_state = app_state.clone();
        let render_node_fn: RenderTreeNode = Rc::new(move |payload: &RenderTreeNodePayload, scope: &mut RenderScope| {
            let node_value = payload.node.value.clone();
            let label_text = payload.node.label.clone();
            let is_store_root = node_value.starts_with("store_");

            // Wrapper span — draggable for non-root nodes
            let wrapper = scope.create_element("span");
            wrapper.set_attribute("class", "rinch-tree__label");
            wrapper.set_attribute("style", "flex: 1; display: inline-flex; align-items: center; border-radius: 2px; transition: background 0.1s;");

            if !is_store_root {
                wrapper.set_attribute("draggable", "true");

                // ondragstart — set drag data
                let nv = node_value.clone();
                let ds_handler = scope.register_handler(move || {
                    drag_ctx.set(nv.clone());
                });
                wrapper.set_attribute("data-ondragstart", &ds_handler.0.to_string());

                // ondragend — clear state
                let de_handler = scope.register_handler(move || {
                    drag_ctx.clear();
                    drop_target.set(None);
                });
                wrapper.set_attribute("data-ondragend", &de_handler.0.to_string());
            }

            // ondrop — handle the drop
            let nv_drop = node_value.clone();
            let drop_state = dnd_state.clone();
            let drop_handler = scope.register_handler(move || {
                drop_target.set(None);
                if let Some(dragged_value) = drag_ctx.take() {
                    if dragged_value == nv_drop {
                        return; // Can't drop onto self
                    }
                    let st = drop_state.borrow();
                    // Parse dragged node
                    let Some((drag_store_id, Some(drag_node_id))) = st.parse_tree_value(&dragged_value) else {
                        return;
                    };
                    // Parse drop target — determine new parent
                    let new_parent_id = if let Some((target_store_id, target_node_id_opt)) = st.parse_tree_value(&nv_drop) {
                        if drag_store_id != target_store_id {
                            return; // Can't move across stores
                        }
                        match target_node_id_opt {
                            Some(nid) => nid, // Drop on node → make child of that node
                            None => {
                                // Drop on store root → use root_node_id
                                if let Some(store) = st.stores.get(&target_store_id) {
                                    store.root_node_id
                                } else {
                                    return;
                                }
                            }
                        }
                    } else {
                        return;
                    };

                    if let Some(backend) = &st.backend {
                        backend.send(BackendCommand::MoveNode {
                            store_id: drag_store_id,
                            node_id: drag_node_id,
                            new_parent_id,
                            position: None,
                        });
                    }
                }
            });
            wrapper.set_attribute("data-ondrop", &drop_handler.0.to_string());

            // ondragenter — highlight drop target
            let nv_enter = node_value.clone();
            let enter_handler = scope.register_handler(move || {
                drop_target.set(Some(nv_enter.clone()));
            });
            wrapper.set_attribute("data-ondragenter", &enter_handler.0.to_string());

            // ondragleave — clear highlight
            let nv_leave = node_value.clone();
            let leave_handler = scope.register_handler(move || {
                let current = drop_target.get();
                if current.as_deref() == Some(&nv_leave) {
                    drop_target.set(None);
                }
            });
            wrapper.set_attribute("data-ondragleave", &leave_handler.0.to_string());

            // Reactive styling: dim when dragged, highlight when drop target
            let wrapper_effect = wrapper.clone();
            let nv_effect = node_value.clone();
            let base_style = "flex: 1; display: inline-flex; align-items: center; border-radius: 2px; transition: background 0.1s, opacity 0.1s;";
            scope.create_effect(move || {
                let is_being_dragged = drag_ctx.get().as_deref() == Some(&nv_effect);
                let is_target = drop_target.get().as_deref() == Some(&nv_effect)
                    && drag_ctx.is_active();
                if is_being_dragged {
                    wrapper_effect.set_attribute("style",
                        &format!("{} opacity: 0.3;", base_style));
                } else if is_target {
                    wrapper_effect.set_attribute("style",
                        &format!("{} background: var(--rinch-color-blue-1); outline: 1px solid var(--rinch-color-blue-4);", base_style));
                } else {
                    wrapper_effect.set_attribute("style", base_style);
                }
            });

            // Icon inside wrapper so it's part of the draggable area
            if !is_store_root {
                let icon = if payload.has_children {
                    TablerIcon::Folder
                } else {
                    TablerIcon::File
                };
                let icon_wrapper = scope.create_element("span");
                icon_wrapper.set_attribute("class", "rinch-tree__icon");
                let icon_el = render_tabler_icon(scope, icon, TablerIconStyle::Outline);
                icon_wrapper.append_child(&icon_el);
                wrapper.append_child(&icon_wrapper);
            }

            // Label span (normal display) — hidden during rename
            let label_span = scope.create_element("span");
            label_span.set_attribute("style", "cursor: default;");
            let text = scope.create_text(&label_text);
            label_span.append_child(&text);

            // Rename input — hidden by default, shown during rename
            let rename_input = scope.create_element("input");
            rename_input.set_attribute("type", "text");
            rename_input.set_attribute("class", "rinch-text-input__input");
            rename_input.set_attribute("style", "display: none;");
            rename_input.set_attribute("value", &label_text);

            // Track input value changes
            let input_handler_id = scope.register_input_handler(move |value: String| {
                rename_text.set(value);
            });
            rename_input.set_attribute("data-oninput", &input_handler_id.to_string());

            // Commit rename on Enter
            let nv_submit = node_value.clone();
            let submit_state = dnd_state.clone();
            let submit_handler = scope.register_handler(move || {
                let new_title = rename_text.get();
                renaming_node.set(None);
                let st = submit_state.borrow();
                if let Some((store_id, Some(node_id))) = st.parse_tree_value(&nv_submit) {
                    if let Some(backend) = &st.backend {
                        backend.send(BackendCommand::RenameNode {
                            store_id,
                            node_id,
                            title: new_title,
                        });
                    }
                }
            });
            rename_input.set_attribute("data-onsubmit", &submit_handler.0.to_string());

            wrapper.append_child(&label_span);
            wrapper.append_child(&rename_input);

            // Effect: toggle between label and input based on rename state.
            // Uses untracked() to read rename_text without subscribing, so
            // typing doesn't re-trigger this Effect (which would reset the
            // input value and refocus on every keystroke).
            let nv_rename = node_value.clone();
            let label_effect = label_span.clone();
            let input_effect = rename_input.clone();
            scope.create_effect(move || {
                let is_renaming = renaming_node.get().as_deref() == Some(&nv_rename);
                if is_renaming {
                    label_effect.set_attribute("style", "display: none;");
                    input_effect.set_attribute("style", "width: 100%; font-size: inherit; padding: 0 2px; height: 22px; line-height: 22px;");
                    let initial_value = untracked(|| rename_text.get());
                    input_effect.set_attribute("value", &initial_value);
                    request_focus(input_effect.node_id().0);
                } else {
                    label_effect.set_attribute("style", "cursor: default;");
                    input_effect.set_attribute("style", "display: none;");
                }
            });

            wrapper
        });

        let tree = Tree {
            data: tree_data.get(),
            tree: Some(tree_state),
            data_source: Some(data_source),
            level_offset: "xs".to_string(),
            select_on_click: true,
            expand_on_click: false,
            onselect: Some(on_tree_select.clone()),
            onexpand: Some(on_tree_expand.clone()),
            oncollapse: Some(on_tree_collapse.clone()),
            render_node: Some(render_node_fn),
            ..Default::default()
        };
        let tree_handle = tree.render(__scope, &[]);
        tree_scroll.append_child(&tree_handle);

        // Keyboard interceptor: Escape cancels rename.
        // Uses run_on_main_thread to defer the signal update so that
        // the Effect's clear_keyboard_interceptor() doesn't execute
        // while still inside the interceptor callback (re-entrancy).
        Effect::new(move || {
            let is_renaming = renaming_node.get().is_some();
            if is_renaming {
                set_keyboard_interceptor(move |data| {
                    if data.key == "Escape" {
                        rinch::run_on_main_thread(move || {
                            renaming_node.set(None);
                        });
                        return true;
                    }
                    false
                });
            } else {
                clear_keyboard_interceptor();
            }
        });

        // Toolbar button handlers
        let new_node_state = app_state.clone();

        // Rich text editor setup
        let on_editor_change: Rc<dyn Fn()> = Rc::new(|| {
            // Content saved on node switch — no per-keystroke action needed
        });

        let ce_div = __scope.create_element("div");
        ce_div.set_attribute("contenteditable", "true");
        ce_div.set_attribute("style", "min-height: 200px; outline: none; flex: 1; font-family: inherit; line-height: 1.6;");

        let bridge = EditorBridge::mount(
            __scope,
            editor_for_app.clone(),
            ce_div.clone(),
            on_editor_change.clone(),
        );
        *bridge_for_app.borrow_mut() = Some(bridge);

        let toolbar_handle = render_toolbar(
            __scope,
            editor_for_app.clone(),
            &ToolbarConfig::default_markdown(),
            on_editor_change,
        );

        // Connection status color
        let status_color: Signal<String> = Signal::new("yellow".to_string());
        Effect::new(move || {
            let status = connection_status.get();
            let color = if status == "Connected" {
                "green"
            } else if status.starts_with("Error") {
                "red"
            } else {
                "yellow"
            };
            status_color.set(color.to_string());
        });

        rsx! {
            BorderlessWindow {
                title: "Pimble".to_string(),
                show_minimize: true,
                show_maximize: true,
                show_close: true,
                on_close: Callback::new(|| close_current_window()),

                // Compact tree chevrons/spacers + dark-mode overrides for tree and editor toolbar
                style { "\
                    .rinch-tree__chevron, .rinch-tree__spacer { width: 0.875rem; height: 0.875rem; margin-right: 2px; } \
                    .rinch-tree__chevron svg, .rinch-tree__icon svg { width: 0.8rem; height: 0.8rem; } \
                    .rinch-tree__icon { width: 1rem; height: 1rem; margin-right: 4px; } \
                    .rinch-tree__node-content:hover { background-color: var(--rinch-color-default-hover); } \
                    .rinch-tree__node-content--selected { background-color: var(--rinch-primary-color-8); color: var(--rinch-primary-color-2); } \
                    .rinch-tree__node-content--selected:hover { background-color: var(--rinch-primary-color-7); } \
                    .rinch-tree__node-content--selected .rinch-tree__icon { color: var(--rinch-primary-color-2); } \
                    .rinch-tree__node-content--selected .rinch-tree__chevron { color: var(--rinch-primary-color-2); } \
                    .editor-toolbar { background: var(--rinch-color-surface) !important; border-bottom-color: var(--rinch-color-border) !important; } \
                " }

                // Main Content
                div {
                    style: "display: flex; flex: 1; overflow: hidden;",

                    // Sidebar
                    div {
                        style: "width: 250px; min-width: 200px; border-right: 1px solid var(--rinch-color-default-border); display: flex; flex-direction: column; background: var(--rinch-color-body);",

                        // Sidebar toolbar
                        div {
                            style: "display: flex; align-items: center; gap: 2px; padding: 4px 8px; border-bottom: 1px solid var(--rinch-color-default-border); background: var(--rinch-color-body);",

                            ActionIcon {
                                icon: TablerIcon::FilePlus,
                                variant: "subtle",
                                size: "sm",
                                onclick: Callback::new(move || {
                                    let st = new_node_state.borrow();
                                    if let Some((&store_id, store)) = st.stores.iter().next() {
                                        if let Some(backend) = &st.backend {
                                            backend.send(BackendCommand::CreateNode {
                                                store_id,
                                                parent_id: Some(store.root_node_id),
                                                title: String::new(),
                                            });
                                        }
                                    }
                                }),
                            }

                            // Spacer
                            div { style: "flex: 1;", }

                            // Connection status badge
                            Badge {
                                variant: "dot",
                                color: status_color.get(),
                                {|| connection_status.get()}
                            }
                        }

                        // Store heading pill
                        div {
                            style: "padding: 8px 8px 4px;",
                            Badge {
                                variant: "filled",
                                color: "blue",
                                size: "sm",
                                full_width: true,
                                {|| sidebar_heading.get()}
                            }
                        }

                        {tree_scroll}
                    }

                    // Editor panel
                    div {
                        style: "flex: 1; display: flex; flex-direction: column; overflow: hidden;",

                        // Editor toolbar
                        {toolbar_handle}

                        // Editor content
                        div {
                            style: "flex: 1; overflow-y: auto; padding: 16px;",
                            {ce_div}
                        }
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

    // Drop the EditorBridge while thread-local storage is still alive,
    // to avoid panic in unmount() during TLS destruction.
    bridge_cell.borrow_mut().take();
    EVENT_PROCESSOR.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
