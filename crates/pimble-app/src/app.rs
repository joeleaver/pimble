//! Pimble Desktop Application
//!
//! Built with Slint UI framework

use std::rc::Rc;
use std::cell::RefCell;
use std::time::{Duration, Instant};

use slint::{ModelRc, SharedString, VecModel};
use pimble_core::NodeId;
use pimble_crdt::DocumentContent;

// Import winit window accessor trait for window dragging
#[cfg(feature = "backend-winit")]
use i_slint_backend_winit::WinitWindowAccessor;

use crate::backend::{BackendCommand, BackendEvent, BackendHandle};
use crate::state::{AppState, ConnectionState, TreeItem};

// Include the generated Slint code
slint::include_modules!();

/// Convert our TreeItem to Slint's TreeItemData
fn tree_item_to_slint(item: &TreeItem) -> TreeItemData {
    TreeItemData {
        id: SharedString::from(&item.id),
        label: SharedString::from(&item.label),
        icon: SharedString::from(&item.icon),
        depth: item.depth,
        expandable: item.expandable,
        expanded: item.expanded,
        is_store: item.is_store,
    }
}

/// Update editor views with new content
fn update_editor_content(window: &AppWindow, content: &str) {
    window.set_node_content(SharedString::from(content));
    window.set_cosmic_editor_text(SharedString::from(content));
}

/// Create the default file menu items
fn create_file_menu() -> Vec<MenuItemData> {
    vec![
        MenuItemData {
            label: SharedString::from("New Store..."),
            shortcut: SharedString::from("Ctrl+N"),
            action_id: SharedString::from("file_new_store"),
            enabled: true,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::from("Open Store..."),
            shortcut: SharedString::from("Ctrl+O"),
            action_id: SharedString::from("file_open_store"),
            enabled: true,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::new(),
            shortcut: SharedString::new(),
            action_id: SharedString::new(),
            enabled: false,
            is_separator: true,
        },
        MenuItemData {
            label: SharedString::from("Close Store"),
            shortcut: SharedString::new(),
            action_id: SharedString::from("file_close_store"),
            enabled: true,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::new(),
            shortcut: SharedString::new(),
            action_id: SharedString::new(),
            enabled: false,
            is_separator: true,
        },
        MenuItemData {
            label: SharedString::from("Exit"),
            shortcut: SharedString::from("Alt+F4"),
            action_id: SharedString::from("file_exit"),
            enabled: true,
            is_separator: false,
        },
    ]
}

/// Create the default edit menu items
fn create_edit_menu() -> Vec<MenuItemData> {
    vec![
        MenuItemData {
            label: SharedString::from("Undo"),
            shortcut: SharedString::from("Ctrl+Z"),
            action_id: SharedString::from("edit_undo"),
            enabled: false,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::from("Redo"),
            shortcut: SharedString::from("Ctrl+Y"),
            action_id: SharedString::from("edit_redo"),
            enabled: false,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::new(),
            shortcut: SharedString::new(),
            action_id: SharedString::new(),
            enabled: false,
            is_separator: true,
        },
        MenuItemData {
            label: SharedString::from("Cut"),
            shortcut: SharedString::from("Ctrl+X"),
            action_id: SharedString::from("edit_cut"),
            enabled: false,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::from("Copy"),
            shortcut: SharedString::from("Ctrl+C"),
            action_id: SharedString::from("edit_copy"),
            enabled: false,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::from("Paste"),
            shortcut: SharedString::from("Ctrl+V"),
            action_id: SharedString::from("edit_paste"),
            enabled: false,
            is_separator: false,
        },
    ]
}

/// Create the default view menu items
fn create_view_menu() -> Vec<MenuItemData> {
    vec![
        MenuItemData {
            label: SharedString::from("Toggle Sidebar"),
            shortcut: SharedString::from("Ctrl+B"),
            action_id: SharedString::from("view_toggle_sidebar"),
            enabled: true,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::new(),
            shortcut: SharedString::new(),
            action_id: SharedString::new(),
            enabled: false,
            is_separator: true,
        },
        MenuItemData {
            label: SharedString::from("Zoom In"),
            shortcut: SharedString::from("Ctrl+="),
            action_id: SharedString::from("view_zoom_in"),
            enabled: true,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::from("Zoom Out"),
            shortcut: SharedString::from("Ctrl+-"),
            action_id: SharedString::from("view_zoom_out"),
            enabled: true,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::from("Reset Zoom"),
            shortcut: SharedString::from("Ctrl+0"),
            action_id: SharedString::from("view_zoom_reset"),
            enabled: true,
            is_separator: false,
        },
    ]
}

/// Create the default help menu items
fn create_help_menu() -> Vec<MenuItemData> {
    vec![
        MenuItemData {
            label: SharedString::from("Documentation"),
            shortcut: SharedString::from("F1"),
            action_id: SharedString::from("help_docs"),
            enabled: true,
            is_separator: false,
        },
        MenuItemData {
            label: SharedString::new(),
            shortcut: SharedString::new(),
            action_id: SharedString::new(),
            enabled: false,
            is_separator: true,
        },
        MenuItemData {
            label: SharedString::from("About Pimble"),
            shortcut: SharedString::new(),
            action_id: SharedString::from("help_about"),
            enabled: true,
            is_separator: false,
        },
    ]
}

/// Main application runner
pub fn run() -> Result<(), slint::PlatformError> {
    // Create the main window
    let window = AppWindow::new()?;

    // Create shared state
    let state = Rc::new(RefCell::new(AppState::new()));

    // Set up menu items
    window.set_file_menu_items(ModelRc::new(VecModel::from(create_file_menu())));
    window.set_edit_menu_items(ModelRc::new(VecModel::from(create_edit_menu())));
    window.set_view_menu_items(ModelRc::new(VecModel::from(create_view_menu())));
    window.set_help_menu_items(ModelRc::new(VecModel::from(create_help_menu())));

    // Start backend connection
    {
        let mut state = state.borrow_mut();
        // Spawn backend with a no-op signal function (Slint uses its own event loop)
        state.backend = Some(BackendHandle::spawn(|| {}));
        state.connection = ConnectionState::Connecting;

        // Send connect command
        if let Some(backend) = &state.backend {
            let _ = backend.send(BackendCommand::Connect {
                url: "http://127.0.0.1:9876".to_string(),
            });
        }
    }

    // Update connection status in UI
    window.set_connection_status(SharedString::from("Connecting..."));

    // Set up callbacks
    let window_weak = window.as_weak();
    let state_clone = state.clone();

    // Cosmic text editor state
    let cosmic_editor = Rc::new(RefCell::new(crate::cosmic_editor::SimpleCosmicEditor::new(
        crate::cosmic_editor::EditorConfig::default(),
    )));

    // Menu item clicked callback
    window.global::<AppCallbacks>().on_menu_item_clicked({
        let window_weak = window_weak.clone();
        let state = state_clone.clone();
        move |action_id| {
            let action = action_id.as_str();
            tracing::info!("Menu action: {}", action);

            match action {
                "file_exit" => {
                    if let Some(window) = window_weak.upgrade() {
                        window.hide().ok();
                    }
                }
                "file_new_store" => {
                    let state = state.borrow();
                    if let Some(backend) = &state.backend {
                        let _ = backend.send(BackendCommand::CreateStore {
                            path: "./new-store.pimble".to_string(),
                            name: "New Store".to_string(),
                        });
                    }
                }
                "file_open_store" => {
                    let state = state.borrow();
                    if let Some(backend) = &state.backend {
                        let _ = backend.send(BackendCommand::OpenStore {
                            path: "./test.pimble".to_string(),
                        });
                    }
                }
                "file_close_store" => {
                    tracing::info!("Close store");
                    // TODO: Implement close store
                }
                "help_about" => {
                    tracing::info!("About Pimble v0.1.0");
                }
                "help_docs" => {
                    tracing::info!("Opening documentation...");
                    // TODO: Open docs URL
                }
                _ => {
                    tracing::debug!("Unhandled menu action: {}", action);
                }
            }
        }
    });

    // Tree item clicked
    window.global::<AppCallbacks>().on_tree_item_clicked({
        let window_weak = window_weak.clone();
        let state = state_clone.clone();
        move |item_id| {
            let id = item_id.as_str();
            tracing::info!("Tree item clicked: {}", id);

            let mut state = state.borrow_mut();
            state.selected_id = Some(id.to_string());

            // Find the tree item to get store_id and node_id
            if let Some((store_id, node_id_opt)) = state.find_tree_item(id) {
                if let Some(node_id) = node_id_opt {
                    // Check if we have this node in cache
                    if let Some(node) = state.nodes.get(&(store_id, node_id)) {
                        tracing::info!("Node found in cache: {} (content size: {} bytes)",
                            node.metadata.title, node.content.len());
                        if let Some(window) = window_weak.upgrade() {
                            window.set_node_title(SharedString::from(&node.metadata.title));
                            let content = get_node_content_text(&node.content);
                            tracing::info!("Extracted content: {} chars", content.len());
                            update_editor_content(&window, &content);
                        }
                    } else {
                        // Node not in cache, request it from backend
                        tracing::info!("Node not in cache, requesting from backend");
                        if let Some(backend) = &state.backend {
                            let _ = backend.send(BackendCommand::GetNode { store_id, node_id });
                        }
                    }
                } else {
                    // Clicked on a store header, show store info
                    tracing::info!("Clicked on store header");
                    if let Some(store) = state.stores.get(&store_id) {
                        if let Some(window) = window_weak.upgrade() {
                            window.set_node_title(SharedString::from(&store.name));
                            let path_str = store.local_path()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|| "remote".to_string());
                            let content = format!(
                                "Store: {}\nPath: {}\nRoot Node: {:?}",
                                store.name,
                                path_str,
                                store.root_node_id
                            );
                            update_editor_content(&window, &content);
                        }
                    }
                }
            }
        }
    });

    // Tree item toggle (expand/collapse)
    window.global::<AppCallbacks>().on_tree_item_toggle({
        let window_weak = window_weak.clone();
        let state = state_clone.clone();
        move |item_id| {
            let id = item_id.as_str();
            tracing::debug!("Tree item toggle: {}", id);

            let mut state = state.borrow_mut();
            state.toggle_expansion(id);

            // Check if we need to load children
            if let Some((store_id, node_id_opt)) = state.find_tree_item(id) {
                // For store headers (node_id is None), use the store's root node
                let node_id = node_id_opt.or_else(|| {
                    state.stores.get(&store_id).map(|s| s.root_node_id)
                });

                if let Some(node_id) = node_id {
                    if !state.children.contains_key(&(store_id, node_id)) {
                        // Request children from backend
                        if let Some(backend) = &state.backend {
                            let _ = backend.send(BackendCommand::GetChildren {
                                store_id,
                                node_id,
                            });
                        }
                    }
                }
            }

            // Rebuild and update tree
            state.rebuild_tree_items();
            if let Some(window) = window_weak.upgrade() {
                update_tree_view(&window, &state);
            }
        }
    });

    // Window controls
    window.global::<AppCallbacks>().on_window_minimize({
        let window_weak = window_weak.clone();
        move || {
            if let Some(window) = window_weak.upgrade() {
                window.window().set_minimized(true);
            }
        }
    });

    window.global::<AppCallbacks>().on_window_maximize({
        let window_weak = window_weak.clone();
        move || {
            if let Some(window) = window_weak.upgrade() {
                let is_maximized = window.window().is_maximized();
                window.window().set_maximized(!is_maximized);
            }
        }
    });

    window.global::<AppCallbacks>().on_window_close({
        let window_weak = window_weak.clone();
        move || {
            if let Some(window) = window_weak.upgrade() {
                window.hide().ok();
            }
        }
    });

    // Window drag - initiate drag move
    #[cfg(feature = "backend-winit")]
    window.global::<AppCallbacks>().on_start_window_drag({
        let window_weak = window_weak.clone();
        move || {
            if let Some(window) = window_weak.upgrade() {
                // Use the winit window drag functionality via Slint's window handle
                // This initiates a native window drag operation
                window.window().with_winit_window(|winit_window: &winit::window::Window| {
                    let _ = winit_window.drag_window();
                });
            }
        }
    });

    #[cfg(not(feature = "backend-winit"))]
    window.global::<AppCallbacks>().on_start_window_drag({
        move || {
            tracing::warn!("Window dragging not supported without winit backend");
        }
    });

    // Window resize - initiate resize operation
    #[cfg(feature = "backend-winit")]
    window.global::<AppCallbacks>().on_start_window_resize({
        let window_weak = window_weak.clone();
        move |direction: SharedString| {
            if let Some(window) = window_weak.upgrade() {
                use winit::window::ResizeDirection;
                let resize_dir = match direction.as_str() {
                    "n" => ResizeDirection::North,
                    "s" => ResizeDirection::South,
                    "e" => ResizeDirection::East,
                    "w" => ResizeDirection::West,
                    "ne" => ResizeDirection::NorthEast,
                    "nw" => ResizeDirection::NorthWest,
                    "se" => ResizeDirection::SouthEast,
                    "sw" => ResizeDirection::SouthWest,
                    _ => return,
                };
                window.window().with_winit_window(|winit_window: &winit::window::Window| {
                    let _ = winit_window.drag_resize_window(resize_dir);
                });
            }
        }
    });

    #[cfg(not(feature = "backend-winit"))]
    window.global::<AppCallbacks>().on_start_window_resize({
        move |_direction: SharedString| {
            tracing::warn!("Window resizing not supported without winit backend");
        }
    });

    // Toolbar actions
    window.global::<AppCallbacks>().on_new_store({
        let state = state_clone.clone();
        move || {
            tracing::info!("New store button clicked");

            // Show save dialog to choose location for new store
            let dialog = rfd::FileDialog::new()
                .set_title("Create New Store")
                .add_filter("Pimble Store", &["pimble"]);

            if let Some(path) = dialog.save_file() {
                let path_str = path.to_string_lossy().to_string();
                // Ensure it ends with .pimble
                let path_str = if path_str.ends_with(".pimble") {
                    path_str
                } else {
                    format!("{}.pimble", path_str)
                };

                // Extract name from path
                let name = std::path::Path::new(&path_str)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "New Store".to_string());

                tracing::info!("Creating new store at: {}", path_str);
                let mut state = state.borrow_mut();
                // Store the path so we can auto-open after creation
                state.pending_create_path = Some(path_str.clone());
                if let Some(backend) = &state.backend {
                    let _ = backend.send(BackendCommand::CreateStore {
                        path: path_str,
                        name,
                    });
                }
            }
        }
    });

    window.global::<AppCallbacks>().on_open_store({
        let state = state_clone.clone();
        move || {
            tracing::info!("Open store button clicked");

            // Show folder picker to select existing store
            let dialog = rfd::FileDialog::new()
                .set_title("Open Store")
                .add_filter("Pimble Store", &["pimble"]);

            if let Some(path) = dialog.pick_folder() {
                let path_str = path.to_string_lossy().to_string();
                tracing::info!("Opening store at: {}", path_str);
                let state = state.borrow();
                if let Some(backend) = &state.backend {
                    let _ = backend.send(BackendCommand::OpenStore {
                        path: path_str,
                    });
                }
            }
        }
    });

    // New node callback
    window.global::<AppCallbacks>().on_new_node({
        let state = state_clone.clone();
        move || {
            let state = state.borrow();

            // Find the first open store to create a node in
            if let Some((&store_id, store)) = state.stores.iter().next() {
                tracing::debug!("Creating new node in store: {}", store.name);
                if let Some(backend) = &state.backend {
                    let _ = backend.send(BackendCommand::CreateNode {
                        store_id,
                        parent_id: Some(store.root_node_id),
                        title: "New Document".to_string(),
                    });
                }
            } else {
                tracing::warn!("No store open to create node in");
            }
        }
    });

    // Cosmic text editor callbacks
    // Helper function to render the cosmic editor and update the Slint image
    fn render_cosmic_editor(
        window: &AppWindow,
        editor: &Rc<RefCell<crate::cosmic_editor::SimpleCosmicEditor>>,
        width: f32,
        height: f32,
    ) {
        let mut editor = editor.borrow_mut();
        let mut font_system = crate::cosmic_editor::get_font_system().lock().unwrap();
        let mut swash_cache = crate::cosmic_editor::get_swash_cache().lock().unwrap();

        // Set size from provided dimensions
        editor.set_size(width, height);

        let pixel_buffer = editor.render(&mut font_system, &mut swash_cache);

        // Convert to Slint image
        let image = slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(
            &pixel_buffer.pixels,
            pixel_buffer.width,
            pixel_buffer.height,
        ));

        window.set_cosmic_editor_image(image);

        // Update table toolbar state
        let has_table_cell = editor.has_table_cell_selected();
        window.set_table_cell_selected(has_table_cell);

        if has_table_cell {
            if let Some((x, y)) = editor.get_table_toolbar_position(&mut font_system) {
                window.set_table_toolbar_x(x);
                window.set_table_toolbar_y(y);
            }
        }
    }

    // Track editor size for rendering (current size + last rendered size for dedup)
    let cosmic_editor_size = Rc::new(RefCell::new((400.0f32, 600.0f32)));
    let _cosmic_last_rendered_size = Rc::new(RefCell::new((0.0f32, 0.0f32)));

    // Debounce tracking for CRDT sync (500ms after last edit)
    let cosmic_last_edit_time: Rc<RefCell<Option<Instant>>> = Rc::new(RefCell::new(None));
    let cosmic_pending_sync_text: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Content changed callback (for source text editing)
    window.global::<AppCallbacks>().on_content_changed({
        let window_weak = window_weak.clone();
        let state = state_clone.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move |new_text| {
            // Update the cosmic editor with the new text
            if let Some(window) = window_weak.upgrade() {
                let current_cosmic_text = window.get_cosmic_editor_text().to_string();
                if current_cosmic_text != new_text.as_str() {
                    // Update cosmic editor
                    cosmic_editor.borrow_mut().set_text(new_text.as_str());
                    window.set_cosmic_editor_text(new_text.clone());
                    // Re-render the cosmic editor
                    let (w, h) = *cosmic_editor_size.borrow();
                    render_cosmic_editor(&window, &cosmic_editor, w, h);
                }
            }

            // Sync to backend
            let state = state.borrow();
            if let Some((store_id, node_id)) = state.selected_store_and_node() {
                tracing::debug!("Content changed for node {:?}", node_id);
                if let Some(backend) = &state.backend {
                    let _ = backend.send(BackendCommand::SetNodeContent {
                        store_id,
                        node_id,
                        text: new_text.to_string(),
                    });
                }
            }
        }
    });

    // Cosmic key pressed handler
    window.global::<AppCallbacks>().on_cosmic_key_pressed({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        let cosmic_last_edit_time = cosmic_last_edit_time.clone();
        let cosmic_pending_sync_text = cosmic_pending_sync_text.clone();
        move |key, shift, ctrl, alt| {
            let key_str = key.as_str();
            tracing::debug!("Cosmic key: '{}', shift={}, ctrl={}, alt={}", key_str, shift, ctrl, alt);

            {
                let mut editor = cosmic_editor.borrow_mut();

                // Arrow keys and Tab are special characters in Slint
                const LEFT_ARROW: char = '\u{F702}';
                const RIGHT_ARROW: char = '\u{F703}';
                const UP_ARROW: char = '\u{F700}';
                const DOWN_ARROW: char = '\u{F701}';
                const HOME: char = '\u{F729}';
                const END: char = '\u{F72B}';
                const TAB: char = '\t';

                let first_char = key_str.chars().next();

                // Check if we're editing a table cell
                if editor.has_table_cell_selected() {
                    // Table cell editing mode
                    if ctrl {
                        // Handle Ctrl shortcuts in cell
                        match first_char {
                            Some('c') | Some('C') => {
                                // Copy cell text
                                if let Some(text) = editor.get_selected_cell_text() {
                                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                        let _ = clipboard.set_text(&text);
                                        tracing::debug!("Copied cell text: {} chars", text.len());
                                    }
                                }
                            }
                            Some('v') | Some('V') => {
                                // Paste into cell (replace entire cell content for now)
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    if let Ok(text) = clipboard.get_text() {
                                        // Insert char by char to handle multi-char paste
                                        for c in text.chars() {
                                            if c != '\n' && c != '\r' {
                                                editor.insert_char_in_cell(c);
                                            }
                                        }
                                        tracing::debug!("Pasted into cell: {} chars", text.len());
                                    }
                                }
                            }
                            // Table row/column operations
                            Some('\r') | Some('\n') => {
                                // Ctrl+Enter: Add row below, Ctrl+Shift+Enter: Add row above
                                if shift {
                                    editor.add_row_above();
                                    tracing::debug!("Added row above");
                                } else {
                                    editor.add_row_below();
                                    tracing::debug!("Added row below");
                                }
                            }
                            Some('+') | Some('=') => {
                                // Ctrl+=: Add column right, Ctrl+Shift+=: Add column left
                                if shift {
                                    editor.add_column_left();
                                    tracing::debug!("Added column left");
                                } else {
                                    editor.add_column_right();
                                    tracing::debug!("Added column right");
                                }
                            }
                            Some('-') => {
                                // Ctrl+-: Delete row, Ctrl+Shift+-: Delete column
                                if shift {
                                    editor.delete_column();
                                    tracing::debug!("Deleted column");
                                } else {
                                    editor.delete_row();
                                    tracing::debug!("Deleted row");
                                }
                            }
                            _ => {}
                        }
                    } else if alt {
                        // Alt shortcuts for table manipulation
                        match first_char {
                            Some(UP_ARROW) => {
                                editor.add_row_above();
                                tracing::debug!("Alt+Up: Added row above");
                            }
                            Some(DOWN_ARROW) => {
                                editor.add_row_below();
                                tracing::debug!("Alt+Down: Added row below");
                            }
                            Some(LEFT_ARROW) => {
                                editor.add_column_left();
                                tracing::debug!("Alt+Left: Added column left");
                            }
                            Some(RIGHT_ARROW) => {
                                editor.add_column_right();
                                tracing::debug!("Alt+Right: Added column right");
                            }
                            _ => {}
                        }
                    } else {
                        match first_char {
                            Some('\u{8}') => editor.backspace_in_cell(), // Backspace
                            Some('\u{7f}') => editor.delete_in_cell(),   // Delete
                            Some('\r') | Some('\n') => {
                                // Enter moves to next row (or exits table if last row)
                                editor.move_to_cell_below();
                            }
                            Some('\u{1b}') => {
                                // Escape clears table selection
                                editor.clear_table_selection();
                            }
                            Some(TAB) => {
                                // Tab/Shift+Tab navigates cells
                                if shift {
                                    editor.move_to_prev_cell();
                                } else {
                                    editor.move_to_next_cell();
                                }
                            }
                            Some(LEFT_ARROW) => {
                                // Move cursor left in cell, or to previous cell at start
                                if let Some(sel) = editor.selected_table_cell() {
                                    if sel.cursor_in_cell == 0 {
                                        editor.move_to_cell_left();
                                    } else {
                                        editor.move_cell_cursor_left();
                                    }
                                }
                            }
                            Some(RIGHT_ARROW) => {
                                // Move cursor right in cell, or to next cell at end
                                let at_end = editor.get_selected_cell_text()
                                    .map(|t| {
                                        editor.selected_table_cell()
                                            .map(|s| s.cursor_in_cell >= t.len())
                                            .unwrap_or(false)
                                    })
                                    .unwrap_or(false);
                                if at_end {
                                    editor.move_to_cell_right();
                                } else {
                                    editor.move_cell_cursor_right();
                                }
                            }
                            Some(UP_ARROW) => editor.move_to_cell_above(),
                            Some(DOWN_ARROW) => editor.move_to_cell_below(),
                            // Regular character input in cell
                            Some(c) if !c.is_control() => {
                                editor.insert_char_in_cell(c);
                            }
                            _ => {}
                        }
                    }
                } else {
                    // Normal editing mode (not in table cell)
                    // Handle Ctrl+key shortcuts
                    if ctrl {
                        match first_char {
                            Some('c') | Some('C') => {
                                // Copy
                                if let Some(text) = editor.get_selected_text() {
                                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                        let _ = clipboard.set_text(&text);
                                        tracing::debug!("Copied {} chars to clipboard", text.len());
                                    }
                                }
                            }
                            Some('x') | Some('X') => {
                                // Cut
                                if let Some(text) = editor.get_selected_text() {
                                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                        let _ = clipboard.set_text(&text);
                                        editor.backspace(); // Delete selection
                                        tracing::debug!("Cut {} chars to clipboard", text.len());
                                    }
                                }
                            }
                            Some('v') | Some('V') => {
                                // Paste
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    if let Ok(text) = clipboard.get_text() {
                                        editor.paste(&text);
                                        tracing::debug!("Pasted {} chars from clipboard", text.len());
                                    }
                                }
                            }
                            Some('a') | Some('A') => {
                                // Select all
                                editor.select_all();
                                tracing::debug!("Selected all text");
                            }
                            _ => {}
                        }
                    } else {
                        match first_char {
                            Some('\u{8}') => editor.backspace(), // Backspace
                            Some('\u{7f}') => editor.delete(),   // Delete
                            Some('\r') | Some('\n') => editor.enter(), // Enter
                            Some('\u{1b}') => {},                // Escape - could clear selection
                            Some(LEFT_ARROW) => editor.move_left(shift),
                            Some(RIGHT_ARROW) => editor.move_right(shift),
                            Some(HOME) => editor.move_home(shift),
                            Some(END) => editor.move_end(shift),
                            Some(UP_ARROW) => editor.move_up(shift),
                            Some(DOWN_ARROW) => editor.move_down(shift),
                            // Regular character input
                            Some(c) if !c.is_control() => {
                                editor.insert_char(c);
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Update the display and track changes for sync
            if let Some(window) = window_weak.upgrade() {
                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
                // Sync text back to UI
                let text = cosmic_editor.borrow().text().to_string();
                let old_text = window.get_cosmic_editor_text().to_string();

                // Track if text changed for debounced CRDT sync
                if text != old_text {
                    *cosmic_last_edit_time.borrow_mut() = Some(Instant::now());
                    *cosmic_pending_sync_text.borrow_mut() = Some(text.clone());
                    // Also update the source editor (node_content)
                    window.set_node_content(SharedString::from(&text));
                }

                window.set_cosmic_editor_text(SharedString::from(text));
            }
        }
    });

    // Cosmic mouse clicked handler
    window.global::<AppCallbacks>().on_cosmic_mouse_clicked({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move |x, y| {
            tracing::info!("Cosmic click at ({}, {})", x, y);

            if let Some(window) = window_weak.upgrade() {
                // Sync scroll position from UI before processing click
                let scroll_y = window.get_cosmic_scroll_y();

                {
                    let mut editor = cosmic_editor.borrow_mut();
                    editor.set_scroll(scroll_y);
                    let mut font_system = crate::cosmic_editor::get_font_system().lock().unwrap();
                    editor.click(x, y, &mut font_system);
                    tracing::info!("Cursor now at position: {}", editor.cursor_position());
                }

                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
            }
        }
    });

    // Cosmic mouse dragged handler
    window.global::<AppCallbacks>().on_cosmic_mouse_dragged({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move |x, y| {
            if let Some(window) = window_weak.upgrade() {
                // Sync scroll position from UI before processing drag
                let scroll_y = window.get_cosmic_scroll_y();

                {
                    let mut editor = cosmic_editor.borrow_mut();
                    editor.set_scroll(scroll_y);
                    let mut font_system = crate::cosmic_editor::get_font_system().lock().unwrap();
                    editor.drag(x, y, &mut font_system);
                }

                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
            }
        }
    });

    // Cosmic request render handler (with scroll and zoom support)
    window.global::<AppCallbacks>().on_cosmic_request_render({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move |width, height, scroll_y, zoom| {
            // Skip rendering if size is too small (prevents crashes during layout)
            if width < 10.0 || height < 10.0 {
                return;
            }

            tracing::debug!("Cosmic request render: {}x{} scroll={:.1} zoom={:.2}", width, height, scroll_y, zoom);
            // Store the new size
            *cosmic_editor_size.borrow_mut() = (width, height);

            if let Some(window) = window_weak.upgrade() {
                // Update editor text from UI if needed
                let ui_text = window.get_cosmic_editor_text();
                {
                    let mut editor = cosmic_editor.borrow_mut();
                    editor.set_text(ui_text.as_str());
                    editor.set_scroll(scroll_y);
                    editor.set_zoom(zoom);
                }
                render_cosmic_editor(&window, &cosmic_editor, width, height);

                // Update content height and max scroll in UI
                let editor = cosmic_editor.borrow();
                let content_height = editor.content_height();
                let max_scroll = (content_height - height).max(0.0);
                window.set_cosmic_content_height(content_height);
                window.set_cosmic_max_scroll_y(max_scroll);
            }
        }
    });

    // Cosmic focus changed handler
    // Note: We don't render here because the Slint component calls request-render
    // with the actual widget size when focus is gained
    window.global::<AppCallbacks>().on_cosmic_focus_changed({
        move |focused| {
            tracing::debug!("Cosmic focus changed: {}", focused);
            // Rendering is handled by the request-render callback which has the correct size
        }
    });

    // Cosmic blink update handler
    window.global::<AppCallbacks>().on_cosmic_blink_update({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move || {
            let changed = cosmic_editor.borrow_mut().update_blink();
            if changed {
                if let Some(window) = window_weak.upgrade() {
                    let (w, h) = *cosmic_editor_size.borrow();
                    render_cosmic_editor(&window, &cosmic_editor, w, h);
                }
            }
        }
    });

    // Table toolbar callbacks
    window.global::<AppCallbacks>().on_table_add_row_above({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move || {
            cosmic_editor.borrow_mut().add_row_above();
            if let Some(window) = window_weak.upgrade() {
                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
                // Sync text
                let text = cosmic_editor.borrow().text().to_string();
                window.set_cosmic_editor_text(SharedString::from(&text));
                window.set_node_content(SharedString::from(&text));
            }
        }
    });

    window.global::<AppCallbacks>().on_table_add_row_below({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move || {
            cosmic_editor.borrow_mut().add_row_below();
            if let Some(window) = window_weak.upgrade() {
                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
                let text = cosmic_editor.borrow().text().to_string();
                window.set_cosmic_editor_text(SharedString::from(&text));
                window.set_node_content(SharedString::from(&text));
            }
        }
    });

    window.global::<AppCallbacks>().on_table_add_column_left({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move || {
            cosmic_editor.borrow_mut().add_column_left();
            if let Some(window) = window_weak.upgrade() {
                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
                let text = cosmic_editor.borrow().text().to_string();
                window.set_cosmic_editor_text(SharedString::from(&text));
                window.set_node_content(SharedString::from(&text));
            }
        }
    });

    window.global::<AppCallbacks>().on_table_add_column_right({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move || {
            cosmic_editor.borrow_mut().add_column_right();
            if let Some(window) = window_weak.upgrade() {
                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
                let text = cosmic_editor.borrow().text().to_string();
                window.set_cosmic_editor_text(SharedString::from(&text));
                window.set_node_content(SharedString::from(&text));
            }
        }
    });

    window.global::<AppCallbacks>().on_table_delete_row({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move || {
            cosmic_editor.borrow_mut().delete_row();
            if let Some(window) = window_weak.upgrade() {
                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
                let text = cosmic_editor.borrow().text().to_string();
                window.set_cosmic_editor_text(SharedString::from(&text));
                window.set_node_content(SharedString::from(&text));
            }
        }
    });

    window.global::<AppCallbacks>().on_table_delete_column({
        let window_weak = window_weak.clone();
        let cosmic_editor = cosmic_editor.clone();
        let cosmic_editor_size = cosmic_editor_size.clone();
        move || {
            cosmic_editor.borrow_mut().delete_column();
            if let Some(window) = window_weak.upgrade() {
                let (w, h) = *cosmic_editor_size.borrow();
                render_cosmic_editor(&window, &cosmic_editor, w, h);
                let text = cosmic_editor.borrow().text().to_string();
                window.set_cosmic_editor_text(SharedString::from(&text));
                window.set_node_content(SharedString::from(&text));
            }
        }
    });

    // Set up a timer to process backend events
    let timer = slint::Timer::default();
    let window_weak_timer = window.as_weak();
    let state_timer = state.clone();
    // Clone debounce tracking for timer closure
    let cosmic_last_edit_time_timer = cosmic_last_edit_time.clone();
    let cosmic_pending_sync_text_timer = cosmic_pending_sync_text.clone();

    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(16), // ~60fps
        move || {
            process_backend_events(&window_weak_timer, &state_timer);

            // Check for debounced cosmic editor sync (500ms after last edit)
            let should_sync = {
                let last_edit = cosmic_last_edit_time_timer.borrow();
                if let Some(last_time) = *last_edit {
                    last_time.elapsed() >= Duration::from_millis(500)
                } else {
                    false
                }
            };

            if should_sync {
                if let Some(text) = cosmic_pending_sync_text_timer.borrow_mut().take() {
                    *cosmic_last_edit_time_timer.borrow_mut() = None;

                    // Sync to backend
                    let state = state_timer.borrow();
                    if let Some((store_id, node_id)) = state.selected_store_and_node() {
                        if let Some(backend) = &state.backend {
                            tracing::debug!("Syncing cosmic editor content to backend ({} chars)", text.len());
                            let _ = backend.send(BackendCommand::SetNodeContent {
                                store_id,
                                node_id,
                                text,
                            });
                        }
                    }
                }
            }
        },
    );

    // Run the event loop
    window.run()
}

/// Process backend events and update UI
fn process_backend_events(
    window_weak: &slint::Weak<AppWindow>,
    state: &Rc<RefCell<AppState>>,
) {
    let Some(window) = window_weak.upgrade() else {
        return;
    };

    // Collect events first to avoid borrow issues
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

    // Process all collected events
    for event in events {
        match event {
            BackendEvent::Connected => {
                tracing::info!("Connected to backend");
                state.connection = ConnectionState::Connected;
                window.set_connection_status(SharedString::from("Connected"));
            }

            BackendEvent::Disconnected => {
                tracing::info!("Disconnected from backend");
                state.connection = ConnectionState::Disconnected;
                window.set_connection_status(SharedString::from("Disconnected"));
            }

            BackendEvent::Error { message } => {
                tracing::error!("Backend error: {}", message);
                state.connection = ConnectionState::Error(message.clone());
                window.set_connection_status(SharedString::from(format!("Error: {}", message)));
            }

            BackendEvent::StoreOpened { store } => {
                tracing::info!("Store opened: {}", store.name);
                let store_id = store.id;
                let root_id = store.root_node_id;
                state.stores.insert(store_id, store);

                // Auto-expand the store and request children
                state.expanded.insert((store_id, root_id));
                if let Some(backend) = &state.backend {
                    let _ = backend.send(BackendCommand::GetChildren {
                        store_id,
                        node_id: root_id,
                    });
                }

                // Rebuild tree
                state.rebuild_tree_items();
                update_tree_view(&window, &state);
            }

            BackendEvent::StoreCreated { store_id, root_node_id } => {
                tracing::info!("Store created: {:?} with root {:?}", store_id, root_node_id);
                // Auto-open the newly created store using the pending path
                if let Some(path) = state.pending_create_path.take() {
                    tracing::info!("Auto-opening created store at: {}", path);
                    if let Some(backend) = &state.backend {
                        let _ = backend.send(BackendCommand::OpenStore { path });
                    }
                }
            }

            BackendEvent::ChildrenLoaded { store_id, parent_id, children } => {
                tracing::debug!("Children loaded for {:?}: {} nodes", parent_id, children.len());

                // Store children IDs and node data
                let child_ids: Vec<NodeId> = children.iter().map(|n| n.id).collect();
                state.children.insert((store_id, parent_id), child_ids);

                for child in children {
                    state.nodes.insert((store_id, child.id), child);
                }

                // Rebuild tree
                state.rebuild_tree_items();
                update_tree_view(&window, &state);
            }

            BackendEvent::NodeLoaded { store_id, node } => {
                tracing::info!("Node loaded: {:?} - {} (content: {} bytes)",
                    node.id, node.metadata.title, node.content.len());
                let node_id = node.id;
                let title = node.metadata.title.clone();
                let content_bytes = node.content.clone();
                state.nodes.insert((store_id, node_id), node);

                // Update viewer if this is the selected node
                if let Some(selected_id) = &state.selected_id {
                    if let Some((sel_store_id, Some(sel_node_id))) = state.find_tree_item(selected_id) {
                        if sel_store_id == store_id && sel_node_id == node_id {
                            tracing::info!("Updating viewer for loaded node");
                            window.set_node_title(SharedString::from(&title));
                            let content = get_node_content_text(&content_bytes);
                            update_editor_content(&window, &content);
                        }
                    }
                }
            }

            BackendEvent::NodeCreated { store_id, parent_id, node_id } => {
                tracing::info!("Node created: {:?}", node_id);

                // Get the actual parent node ID (either specified parent or store's root)
                let actual_parent_id = parent_id.or_else(|| {
                    state.stores.get(&store_id).map(|s| s.root_node_id)
                });

                if let Some(parent_id) = actual_parent_id {
                    // Ensure parent is expanded so the new node will be visible
                    if !state.expanded.contains(&(store_id, parent_id)) {
                        state.expanded.insert((store_id, parent_id));
                    }

                    // Refresh children of the parent to show the new node
                    if let Some(backend) = &state.backend {
                        let _ = backend.send(BackendCommand::GetChildren { store_id, node_id: parent_id });
                    }
                }

                // Also load the new node so we can select it
                if let Some(backend) = &state.backend {
                    let _ = backend.send(BackendCommand::GetNode { store_id, node_id });
                }
            }

            // Handle other events as they're added
            _ => {}
        }
    }
}

/// Update the tree view in the UI
fn update_tree_view(window: &AppWindow, state: &AppState) {
    let items: Vec<TreeItemData> = state
        .tree_items
        .iter()
        .map(tree_item_to_slint)
        .collect();

    window.set_tree_items(ModelRc::new(VecModel::from(items)));
}

/// Extract text content from CRDT node content bytes
fn get_node_content_text(content: &[u8]) -> String {
    if content.is_empty() {
        return String::new();
    }

    // Try to load as CRDT document and get text
    match DocumentContent::load(content) {
        Ok(doc) => {
            match doc.get_text() {
                Ok(text) => text,
                Err(_) => String::from_utf8_lossy(content).to_string(),
            }
        }
        Err(_) => {
            // Fallback: try to decode as plain UTF-8
            String::from_utf8_lossy(content).to_string()
        }
    }
}
