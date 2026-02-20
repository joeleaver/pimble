# Phase 2: Basic UI - Implementation Guide

This document provides detailed implementation guidance for Phase 2. A Claude session can use this to implement the features without needing additional context.

## Overview

Phase 2 connects the Makepad UI to the local server, displays the node tree, and shows node content. By the end, users can:
- Start the app and auto-connect to the server
- Create/open workspaces and stores
- Browse the node tree
- View document content (read-only)

---

## Prerequisites

Before starting Phase 2, ensure:
```powershell
# Server runs successfully
cargo run -p pimble-cli -- server

# App launches (shows skeleton UI)
cargo run -p pimble-app
```

---

## 2.1 Server Connection

### Challenge: Makepad + Async

Makepad has its own event loop and doesn't use tokio directly. We need to:
1. Spawn a background thread with a tokio runtime
2. Use channels to communicate between Makepad UI and async code
3. Signal Makepad to redraw when data arrives

### Implementation

#### Step 1: Add dependencies to `pimble-app/Cargo.toml`

```toml
[dependencies]
# ... existing deps ...
tokio = { workspace = true }
futures = { workspace = true }
crossbeam-channel = "0.5"  # Add to workspace if not present
```

#### Step 2: Create `crates/pimble-app/src/backend.rs`

```rust
//! Background thread for RPC communication

use std::sync::Arc;
use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_client::PimbleClient;
use pimble_core::{Node, NodeId, Store, StoreId, Workspace};
use tokio::runtime::Runtime;

/// Commands sent from UI to backend
#[derive(Debug)]
pub enum BackendCommand {
    Connect { url: String },
    Disconnect,

    // Store operations
    CreateStore { path: String, name: String },
    OpenStore { path: String },
    CloseStore { store_id: StoreId },
    ListStores,

    // Node operations
    GetNode { store_id: StoreId, node_id: NodeId },
    GetChildren { store_id: StoreId, node_id: NodeId },

    // Workspace operations
    CreateWorkspace { name: String, path: String },
    LoadWorkspace { path: String },
    SaveWorkspace { workspace: Workspace, path: String },
}

/// Events sent from backend to UI
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Connected,
    Disconnected,
    Error { message: String },

    // Store events
    StoreCreated { store_id: StoreId, root_node_id: NodeId },
    StoreOpened { store: Store },
    StoreClosed { store_id: StoreId },
    StoreList { stores: Vec<Store> },

    // Node events
    NodeLoaded { store_id: StoreId, node: Node },
    ChildrenLoaded { store_id: StoreId, parent_id: NodeId, children: Vec<Node> },

    // Workspace events
    WorkspaceLoaded { workspace: Workspace },
    WorkspaceSaved,
}

/// Handle to communicate with the backend
pub struct BackendHandle {
    pub cmd_tx: Sender<BackendCommand>,
    pub event_rx: Receiver<BackendEvent>,
}

impl BackendHandle {
    /// Spawn the backend thread and return a handle
    pub fn spawn(signal_ui: impl Fn() + Send + 'static) -> Self {
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(100);
        let (event_tx, event_rx) = bounded::<BackendEvent>(100);

        thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(backend_loop(cmd_rx, event_tx, signal_ui));
        });

        Self { cmd_tx, event_rx }
    }

    /// Send a command to the backend (non-blocking)
    pub fn send(&self, cmd: BackendCommand) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    /// Try to receive an event (non-blocking)
    pub fn try_recv(&self) -> Option<BackendEvent> {
        self.event_rx.try_recv().ok()
    }
}

async fn backend_loop(
    cmd_rx: Receiver<BackendCommand>,
    event_tx: Sender<BackendEvent>,
    signal_ui: impl Fn(),
) {
    let mut client: Option<PimbleClient> = None;

    loop {
        // Block waiting for commands
        let cmd = match cmd_rx.recv() {
            Ok(cmd) => cmd,
            Err(_) => break, // Channel closed, exit
        };

        let event = process_command(&mut client, cmd).await;

        if let Some(event) = event {
            let _ = event_tx.try_send(event);
            signal_ui(); // Tell Makepad to redraw
        }
    }
}

async fn process_command(
    client: &mut Option<PimbleClient>,
    cmd: BackendCommand,
) -> Option<BackendEvent> {
    match cmd {
        BackendCommand::Connect { url } => {
            match PimbleClient::connect(&url).await {
                Ok(c) => {
                    *client = Some(c);
                    Some(BackendEvent::Connected)
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::Disconnect => {
            *client = None;
            Some(BackendEvent::Disconnected)
        }

        BackendCommand::CreateStore { path, name } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_store(&path, &name).await {
                Ok((store_id, root_node_id)) => {
                    Some(BackendEvent::StoreCreated { store_id, root_node_id })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::OpenStore { path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.open_store(&path).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CloseStore { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.close_store(store_id).await {
                Ok(()) => Some(BackendEvent::StoreClosed { store_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ListStores => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.list_stores().await {
                Ok(stores) => Some(BackendEvent::StoreList { stores }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetNode { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_node(store_id, node_id).await {
                Ok(node) => Some(BackendEvent::NodeLoaded { store_id, node }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetChildren { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_children(store_id, node_id).await {
                Ok(children) => Some(BackendEvent::ChildrenLoaded {
                    store_id,
                    parent_id: node_id,
                    children
                }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CreateWorkspace { name, path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_workspace(&name, &path).await {
                Ok(workspace) => Some(BackendEvent::WorkspaceLoaded { workspace }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::LoadWorkspace { path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.load_workspace(&path).await {
                Ok(workspace) => Some(BackendEvent::WorkspaceLoaded { workspace }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::SaveWorkspace { workspace, path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.save_workspace(workspace, &path).await {
                Ok(()) => Some(BackendEvent::WorkspaceSaved),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }
    }
}
```

#### Step 3: Update `crates/pimble-app/src/main.rs`

```rust
//! Pimble - Personal Information Manager

mod app;
mod backend;
mod state;
mod ui;

fn main() {
    app::app_main();
}
```

#### Step 4: Update `crates/pimble-app/src/state.rs`

```rust
//! Application state management

use std::collections::HashMap;

use pimble_core::{Node, NodeId, Store, StoreId, Workspace};

use crate::backend::BackendHandle;

/// Global application state
pub struct AppState {
    /// Backend communication handle
    pub backend: Option<BackendHandle>,

    /// Connection status
    pub connection: ConnectionState,

    /// Current workspace
    pub workspace: Option<Workspace>,

    /// Open stores (loaded from workspace or opened manually)
    pub stores: HashMap<StoreId, Store>,

    /// Cached nodes by (store_id, node_id)
    pub nodes: HashMap<(StoreId, NodeId), Node>,

    /// Children cache: parent -> children ids
    pub children: HashMap<(StoreId, NodeId), Vec<NodeId>>,

    /// Currently selected node
    pub selected: Option<(StoreId, NodeId)>,

    /// Expanded nodes in tree view
    pub expanded: std::collections::HashSet<(StoreId, NodeId)>,

    /// Pending operations (for loading indicators)
    pub loading: LoadingState,

    /// Error message to display
    pub error: Option<String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            backend: None,
            connection: ConnectionState::Disconnected,
            workspace: None,
            stores: HashMap::new(),
            nodes: HashMap::new(),
            children: HashMap::new(),
            selected: None,
            expanded: std::collections::HashSet::new(),
            loading: LoadingState::default(),
            error: None,
        }
    }

    /// Check if a node's children are loaded
    pub fn has_children_loaded(&self, store_id: StoreId, node_id: NodeId) -> bool {
        self.children.contains_key(&(store_id, node_id))
    }

    /// Get children of a node (if loaded)
    pub fn get_children(&self, store_id: StoreId, node_id: NodeId) -> Vec<&Node> {
        self.children
            .get(&(store_id, node_id))
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.nodes.get(&(store_id, *id)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the selected node (if any)
    pub fn selected_node(&self) -> Option<&Node> {
        self.selected
            .and_then(|(store_id, node_id)| self.nodes.get(&(store_id, node_id)))
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

#[derive(Debug, Clone, Default)]
pub struct LoadingState {
    pub connecting: bool,
    pub loading_stores: bool,
    pub loading_nodes: std::collections::HashSet<(StoreId, NodeId)>,
}
```

#### Step 5: Update `crates/pimble-app/src/app.rs`

Replace the entire file with:

```rust
//! Main application entry point and setup

use makepad_widgets::*;

use crate::backend::{BackendCommand, BackendEvent, BackendHandle};
use crate::state::{AppState, ConnectionState};

// Server URL (could be configurable later)
const SERVER_URL: &str = "http://127.0.0.1:9876";

live_design! {
    use link::theme::*;
    use link::widgets::*;

    App = {{App}} {
        ui: <Root> {
            main_window = <Window> {
                window: { title: "Pimble" },
                pass: { clear_color: #1e1e2e }

                body = <View> {
                    flow: Down,
                    spacing: 0,

                    // Toolbar
                    toolbar = <View> {
                        width: Fill,
                        height: 48,
                        padding: { left: 10, right: 10 },
                        align: { y: 0.5 },
                        show_bg: true,
                        draw_bg: { color: #181825 }

                        title_label = <Label> {
                            text: "Pimble",
                            draw_text: {
                                color: #cdd6f4,
                                text_style: { font_size: 14.0 }
                            }
                        }

                        <Filler> {}

                        // TODO: Add "Open Store" button
                        // TODO: Add "New Store" button

                        search_input = <TextInput> {
                            width: 300,
                            height: 32,
                            text: "",
                            empty_message: "Search...",
                        }
                    }

                    // Main content area
                    content = <View> {
                        width: Fill,
                        height: Fill,
                        flow: Right,

                        // Tree panel (left)
                        tree_panel = <View> {
                            width: 250,
                            height: Fill,
                            show_bg: true,
                            draw_bg: { color: #1e1e2e }

                            <View> {
                                width: Fill,
                                height: Fill,
                                padding: 10,
                                flow: Down,

                                stores_label = <Label> {
                                    text: "Stores",
                                    draw_text: {
                                        color: #a6adc8,
                                        text_style: { font_size: 11.0 }
                                    }
                                }

                                tree_content = <View> {
                                    width: Fill,
                                    height: Fill,
                                    flow: Down,

                                    tree_placeholder = <Label> {
                                        margin: { top: 10 },
                                        text: "No stores open",
                                        draw_text: {
                                            color: #6c7086,
                                            text_style: { font_size: 12.0 }
                                        }
                                    }

                                    // Tree nodes will be added here dynamically
                                }
                            }
                        }

                        // Splitter
                        <View> {
                            width: 1,
                            height: Fill,
                            show_bg: true,
                            draw_bg: { color: #313244 }
                        }

                        // Node viewer (right)
                        node_viewer = <View> {
                            width: Fill,
                            height: Fill,
                            show_bg: true,
                            draw_bg: { color: #1e1e2e }

                            <View> {
                                width: Fill,
                                height: Fill,
                                padding: 20,
                                flow: Down,

                                node_title = <Label> {
                                    text: "",
                                    draw_text: {
                                        color: #cdd6f4,
                                        text_style: { font_size: 18.0 }
                                    }
                                }

                                node_meta = <Label> {
                                    margin: { top: 5 },
                                    text: "",
                                    draw_text: {
                                        color: #6c7086,
                                        text_style: { font_size: 10.0 }
                                    }
                                }

                                <View> { height: 20 }

                                node_content = <Label> {
                                    width: Fill,
                                    text: "Select a node to view its content",
                                    draw_text: {
                                        color: #6c7086,
                                        text_style: { font_size: 12.0 }
                                    }
                                }
                            }
                        }
                    }

                    // Status bar
                    status_bar = <View> {
                        width: Fill,
                        height: 24,
                        padding: { left: 10, right: 10 },
                        align: { y: 0.5 },
                        show_bg: true,
                        draw_bg: { color: #181825 }

                        status_label = <Label> {
                            text: "Disconnected",
                            draw_text: {
                                color: #6c7086,
                                text_style: { font_size: 10.0 }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Live)]
pub struct App {
    #[live]
    ui: WidgetRef,

    #[rust]
    state: AppState,
}

impl LiveHook for App {
    fn after_new_from_doc(&mut self, cx: &mut Cx) {
        // Spawn backend and initiate connection
        self.init_backend(cx);
    }
}

impl LiveRegister for App {
    fn live_register(cx: &mut Cx) {
        makepad_widgets::live_design(cx);
    }
}

impl App {
    fn init_backend(&mut self, cx: &mut Cx) {
        // Create a signal that tells Makepad to redraw
        let cx_ref = cx.get_ref();
        let signal_ui = move || {
            Cx::post_signal(cx_ref, SignalEvent::default());
        };

        // Spawn the backend thread
        let backend = BackendHandle::spawn(signal_ui);

        // Initiate connection
        backend.send(BackendCommand::Connect {
            url: SERVER_URL.to_string()
        });
        self.state.connection = ConnectionState::Connecting;

        self.state.backend = Some(backend);
    }

    fn process_backend_events(&mut self, cx: &mut Cx) {
        let Some(backend) = &self.state.backend else { return };

        // Process all pending events
        while let Some(event) = backend.try_recv() {
            match event {
                BackendEvent::Connected => {
                    self.state.connection = ConnectionState::Connected;
                    self.update_status_bar(cx);
                    // Request store list
                    backend.send(BackendCommand::ListStores);
                }

                BackendEvent::Disconnected => {
                    self.state.connection = ConnectionState::Disconnected;
                    self.update_status_bar(cx);
                }

                BackendEvent::Error { message } => {
                    self.state.error = Some(message.clone());
                    if matches!(self.state.connection, ConnectionState::Connecting) {
                        self.state.connection = ConnectionState::Error(message);
                    }
                    self.update_status_bar(cx);
                }

                BackendEvent::StoreList { stores } => {
                    for store in stores {
                        self.state.stores.insert(store.id, store);
                    }
                    self.update_tree_panel(cx);
                }

                BackendEvent::StoreOpened { store } => {
                    let store_id = store.id;
                    let root_id = store.root_node_id;
                    self.state.stores.insert(store_id, store);
                    // Auto-expand root and fetch its children
                    self.state.expanded.insert((store_id, root_id));
                    if let Some(backend) = &self.state.backend {
                        backend.send(BackendCommand::GetNode { store_id, node_id: root_id });
                        backend.send(BackendCommand::GetChildren { store_id, node_id: root_id });
                    }
                    self.update_tree_panel(cx);
                }

                BackendEvent::StoreClosed { store_id } => {
                    self.state.stores.remove(&store_id);
                    // Clean up related state
                    self.state.nodes.retain(|(sid, _), _| *sid != store_id);
                    self.state.children.retain(|(sid, _), _| *sid != store_id);
                    self.state.expanded.retain(|(sid, _)| *sid != store_id);
                    if self.state.selected.map(|(sid, _)| sid) == Some(store_id) {
                        self.state.selected = None;
                    }
                    self.update_tree_panel(cx);
                    self.update_node_viewer(cx);
                }

                BackendEvent::StoreCreated { store_id, root_node_id } => {
                    // Fetch the full store info
                    if let Some(backend) = &self.state.backend {
                        backend.send(BackendCommand::ListStores);
                    }
                }

                BackendEvent::NodeLoaded { store_id, node } => {
                    let node_id = node.id;
                    self.state.nodes.insert((store_id, node_id), node);
                    self.update_tree_panel(cx);
                    if self.state.selected == Some((store_id, node_id)) {
                        self.update_node_viewer(cx);
                    }
                }

                BackendEvent::ChildrenLoaded { store_id, parent_id, children } => {
                    let child_ids: Vec<_> = children.iter().map(|n| n.id).collect();
                    for child in children {
                        self.state.nodes.insert((store_id, child.id), child);
                    }
                    self.state.children.insert((store_id, parent_id), child_ids);
                    self.update_tree_panel(cx);
                }

                BackendEvent::WorkspaceLoaded { workspace } => {
                    self.state.workspace = Some(workspace);
                    // TODO: Open all stores in the workspace
                }

                BackendEvent::WorkspaceSaved => {
                    // Could show a toast notification
                }
            }
        }
    }

    fn update_status_bar(&mut self, cx: &mut Cx) {
        let status_text = match &self.state.connection {
            ConnectionState::Disconnected => "Disconnected".to_string(),
            ConnectionState::Connecting => "Connecting...".to_string(),
            ConnectionState::Connected => {
                let store_count = self.state.stores.len();
                format!("Connected • {} store(s)", store_count)
            }
            ConnectionState::Error(msg) => format!("Error: {}", msg),
        };

        // Update the label
        self.ui.label(id!(status_label)).set_text(cx, &status_text);
    }

    fn update_tree_panel(&mut self, cx: &mut Cx) {
        // For now, just update the placeholder text
        // Full tree rendering will be implemented next
        let has_stores = !self.state.stores.is_empty();
        let placeholder_text = if has_stores {
            // Build a simple text representation for now
            let mut text = String::new();
            for store in self.state.stores.values() {
                text.push_str(&format!("📁 {}\n", store.name));
                // Show root children if expanded
                if self.state.expanded.contains(&(store.id, store.root_node_id)) {
                    if let Some(children) = self.state.children.get(&(store.id, store.root_node_id)) {
                        for child_id in children {
                            if let Some(child) = self.state.nodes.get(&(store.id, *child_id)) {
                                let icon = if child.node_type == "folder" { "📁" } else { "📄" };
                                text.push_str(&format!("  {} {}\n", icon, child.metadata.title));
                            }
                        }
                    }
                }
            }
            text
        } else {
            "No stores open".to_string()
        };

        self.ui.label(id!(tree_placeholder)).set_text(cx, &placeholder_text);
    }

    fn update_node_viewer(&mut self, cx: &mut Cx) {
        if let Some(node) = self.state.selected_node() {
            self.ui.label(id!(node_title)).set_text(cx, &node.metadata.title);

            let meta = format!(
                "Type: {} • Created: {} • Modified: {}",
                node.node_type,
                node.metadata.created_at.format("%Y-%m-%d %H:%M"),
                node.metadata.modified_at.format("%Y-%m-%d %H:%M"),
            );
            self.ui.label(id!(node_meta)).set_text(cx, &meta);

            // For now, show content placeholder
            // Real content rendering depends on node type and CRDT content
            let content = if node.content.is_empty() {
                "(Empty document)".to_string()
            } else {
                format!("Content: {} bytes", node.content.len())
            };
            self.ui.label(id!(node_content)).set_text(cx, &content);
        } else {
            self.ui.label(id!(node_title)).set_text(cx, "");
            self.ui.label(id!(node_meta)).set_text(cx, "");
            self.ui.label(id!(node_content)).set_text(cx, "Select a node to view its content");
        }
    }
}

impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // Process backend events on every frame
        self.process_backend_events(cx);

        // Handle UI events
        let scope = &mut Scope::empty();
        self.ui.handle_event(cx, event, scope);
    }
}

pub fn app_main() {
    app_main!(App);
}
```

#### Step 6: Add `crossbeam-channel` to workspace `Cargo.toml`

```toml
# In [workspace.dependencies] section:
crossbeam-channel = "0.5"
```

And to `pimble-app/Cargo.toml`:
```toml
crossbeam-channel = { workspace = true }
```

---

## 2.2 Testing the Connection

At this point, you should be able to:

1. Start the server in one terminal:
```powershell
   cargo run -p pimble-cli -- server
```

2. Start the app in another terminal:
```powershell
   cargo run -p pimble-app
```

3. See "Connected • 0 store(s)" in the status bar

---

## 2.3 Store Management

### Add "Open Store" Button

In the live_design!, add a button to the toolbar:

```rust
// In toolbar, before search_input:
open_store_btn = <Button> {
    text: "Open Store",
    draw_text: { color: #cdd6f4 }
}

<View> { width: 10 }  // Spacer
```

### Handle Button Click

In `App`, add click handling. Makepad buttons emit actions:

```rust
impl App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // Check for button clicks
        if self.ui.button(id!(open_store_btn)).clicked(actions) {
            self.open_store_dialog(cx);
        }
    }

    fn open_store_dialog(&mut self, cx: &mut Cx) {
        // For now, use a hardcoded path
        // TODO: Implement file picker dialog
        if let Some(backend) = &self.state.backend {
            backend.send(BackendCommand::OpenStore {
                path: "./test.pimble".to_string(),
            });
        }
    }
}
```

Then call `self.handle_actions(cx, &actions)` in `AppMain::handle_event` after getting actions from the UI.

---

## 2.4 TreePanel with Real Tree Widget

The simple label-based tree works for testing, but for a proper tree:

### Option A: Custom Tree Widget (Recommended for learning Makepad)

Create `crates/pimble-app/src/ui/tree.rs`:

```rust
//! Custom tree widget for displaying node hierarchy

use makepad_widgets::*;

live_design! {
    use link::theme::*;
    use link::widgets::*;

    TreeNode = {{TreeNode}} {
        width: Fill,
        height: Fit,
        flow: Down,

        row = <View> {
            width: Fill,
            height: 28,
            padding: { left: 5, right: 5 },
            align: { y: 0.5 },

            expand_btn = <View> {
                width: 20,
                height: 20,
                // Arrow indicator
            }

            icon = <Label> {
                width: 20,
                text: "📄",
                draw_text: { text_style: { font_size: 12.0 } }
            }

            label = <Label> {
                text: "Node",
                draw_text: {
                    color: #cdd6f4,
                    text_style: { font_size: 12.0 }
                }
            }
        }

        children = <View> {
            width: Fill,
            height: Fit,
            flow: Down,
            padding: { left: 20 },
            // Child TreeNodes go here
        }
    }
}

#[derive(Live, Widget)]
pub struct TreeNode {
    #[deref]
    view: View,

    #[rust]
    pub node_id: Option<(pimble_core::StoreId, pimble_core::NodeId)>,

    #[rust]
    pub is_expanded: bool,
}

// ... implement Widget trait
```

### Option B: Use Makepad's Built-in FoldList/Tree (if available in 1.0)

Check makepad-widgets documentation for tree/list widgets.

---

## 2.5 Node Selection

When a tree node is clicked:

1. Update `state.selected = Some((store_id, node_id))`
2. If node not in cache, fetch it: `backend.send(BackendCommand::GetNode { ... })`
3. Call `update_node_viewer(cx)`

---

## 2.6 Document Content Rendering

For document nodes, decode the CRDT content:

```rust
fn render_document_content(&self, node: &Node) -> String {
    if node.content.is_empty() {
        return "(Empty document)".to_string();
    }

    // Load as CRDT document
    match pimble_crdt::DocumentContent::load(&node.content) {
        Ok(doc) => {
            match doc.get_text() {
                Ok(text) => text,
                Err(e) => format!("Error reading content: {}", e),
            }
        }
        Err(e) => format!("Error loading document: {}", e),
    }
}
```

Add `pimble-crdt` to `pimble-app/Cargo.toml`:
```toml
pimble-crdt = { workspace = true }
```

---

## Verification Checklist

After completing Phase 2:

- [ ] App connects to server automatically on startup
- [ ] Status bar shows connection state
- [ ] Can open an existing store (hardcoded path for now)
- [ ] Tree panel shows store name and root children
- [ ] Clicking a node selects it
- [ ] Node viewer shows selected node's title, metadata, and content
- [ ] Tree nodes can be expanded/collapsed
- [ ] Children are fetched on expand

---

## Common Issues & Solutions

### Issue: "Cx::post_signal" not found
Makepad API may have changed. Check `makepad_widgets` docs for the current way to trigger redraws from background threads.

### Issue: Async runtime panics
Ensure only one tokio runtime is created. The backend thread should own the only runtime.

### Issue: UI not updating
Make sure `signal_ui()` is called after each event, and that `process_backend_events()` is called in `handle_event`.

### Issue: Makepad live_design! syntax errors
The DSL is sensitive. Check that:
- All widgets have closing braces
- Properties use correct types (e.g., `width: Fill` not `width: "Fill"`)
- Colors use `#hexcode` format

---

## Files Changed Summary

| File | Change |
| --- | --- |
| `pimble-app/Cargo.toml` | Add crossbeam-channel, pimble-crdt |
| `pimble-app/src/main.rs` | Add `mod backend` |
| `pimble-app/src/backend.rs` | NEW - Backend thread and commands |
| `pimble-app/src/state.rs` | Rewrite with full state management |
| `pimble-app/src/app.rs` | Rewrite with backend integration |
| `Cargo.toml` (workspace) | Add crossbeam-channel |
