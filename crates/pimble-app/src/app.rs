//! Pimble Desktop Application
//!
//! Built with Rinch UI framework

use std::collections::{HashMap, HashSet};

use pimble_core::{Node, NodeId, Store, StoreId};
use rinch::prelude::*;

use crate::backend::{BackendCommand, BackendEvent, BackendHandle};

/// A flattened tree item for display
#[derive(Debug, Clone, PartialEq)]
struct TreeItem {
    id: String,
    store_id: StoreId,
    node_id: Option<NodeId>,
    label: String,
    icon: String,
    depth: i32,
    expandable: bool,
    expanded: bool,
    is_store: bool,
}

pub fn run() {
    #[component]
    fn pimble_app() -> NodeHandle {
        // Backend handle
        let backend = use_signal(|| Option::<BackendHandle>::None);

        // Connection status
        let connection_status = use_signal(|| "Connecting...".to_string());

        // Store state
        let stores = use_signal(|| HashMap::<StoreId, Store>::new());
        let nodes = use_signal(|| HashMap::<(StoreId, NodeId), Node>::new());
        let children = use_signal(|| HashMap::<(StoreId, NodeId), Vec<NodeId>>::new());

        // Tree state
        let tree_items = use_signal(|| Vec::<TreeItem>::new());
        let expanded = use_signal(|| HashSet::<(StoreId, NodeId)>::new());

        // Selection state
        let selected_id = use_signal(|| Option::<String>::None);
        let selected_node_content = use_signal(|| String::new());

        // Click counter for testing
        let click_count = use_signal(|| 0i32);

        // Tick signal to trigger re-renders
        let tick = use_signal(|| 0u32);

        // Helper to poll backend events and update UI
        let poll_backend_events = |backend: &BackendHandle,
                                   connection_status: &rinch::Signal<String>,
                                   stores: &rinch::Signal<HashMap<StoreId, Store>>,
                                   nodes: &rinch::Signal<HashMap<(StoreId, NodeId), Node>>,
                                   children: &rinch::Signal<HashMap<(StoreId, NodeId), Vec<NodeId>>>,
                                   expanded: &rinch::Signal<HashSet<(StoreId, NodeId)>>,
                                   tree_items: &rinch::Signal<Vec<TreeItem>>| {
            while let Some(event) = backend.try_recv() {
                match event {
                    BackendEvent::Connected => {
                        connection_status.set("Connected".to_string());
                    }
                    BackendEvent::Disconnected => {
                        connection_status.set("Disconnected".to_string());
                    }
                    BackendEvent::Error { message } => {
                        connection_status.set(format!("Error: {}", message));
                    }
                    BackendEvent::StoreOpened { store } => {
                        let store_id = store.id;
                        let root_node_id = store.root_node_id;
                        stores.update(|s| { s.insert(store_id, store); });
                        let _ = backend.send_command(BackendCommand::GetChildren {
                            store_id,
                            node_id: root_node_id,
                        });
                    }
                    BackendEvent::ChildrenLoaded { store_id, parent_id, children: child_nodes } => {
                        let child_ids: Vec<NodeId> = child_nodes.iter().map(|n| n.id).collect();
                        children.update(|c| { c.insert((store_id, parent_id), child_ids); });
                        for node in child_nodes {
                            nodes.update(|n| { n.insert((store_id, node.id), node); });
                        }
                        // Rebuild tree display
                        let stores_val = stores.get();
                        let nodes_val = nodes.get();
                        let children_val = children.get();
                        let expanded_val = expanded.get();
                        let mut new_items = Vec::new();
                        rebuild_tree_items(&stores_val, &nodes_val, &children_val, &expanded_val, &mut new_items);
                        tree_items.set(new_items);
                    }
                    BackendEvent::StoreCreated { store_id, root_node_id } => {
                        // Open the newly created store
                        let path = format!("./{}.pimble", store_id);
                        let _ = backend.send_command(BackendCommand::OpenStore { path });
                    }
                    _ => {}
                }
            }
        };

        // Initialize backend on mount
        use_effect(
            move || {
                let handle = BackendHandle::spawn(|| {});
                backend.set(Some(handle));

                // Connect to backend
                if let Some(ref h) = backend.get() {
                    let _ = h.send_command(BackendCommand::Connect {
                        url: "http://127.0.0.1:9876".to_string(),
                    });
                    // Poll multiple times - backend is async so need to wait for it
                    for _ in 0..50 {
                        poll_backend_events(h, &connection_status, &stores, &nodes, &children, &expanded, &tree_items);
                        if connection_status.get() != "Connecting...".to_string() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }

                    // Auto-open existing store
                    let path = "./new-store.pimble".to_string();
                    let _ = h.send_command(BackendCommand::OpenStore { path: path.clone() });
                    // Poll for store and children
                    for _ in 0..100 {
                        poll_backend_events(h, &connection_status, &stores, &nodes, &children, &expanded, &tree_items);
                        if !stores.get().is_empty() && !children.get().is_empty() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            },
            (),
        );

        // New Store button handler
        let on_new_store = {
            let backend = backend.clone();
            let connection_status = connection_status.clone();
            let stores = stores.clone();
            let nodes = nodes.clone();
            let children = children.clone();
            let expanded = expanded.clone();
            let tree_items = tree_items.clone();
            move || {
                if let Some(ref h) = backend.get() {
                    let name = "New Store".to_string();
                    let path = format!("./{}.pimble", name.to_lowercase().replace(' ', "-"));
                    let _ = h.send_command(BackendCommand::CreateStore { path, name });
                    // Poll for response
                    for _ in 0..50 {
                        poll_backend_events(h, &connection_status, &stores, &nodes, &children, &expanded, &tree_items);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }
        };

        // Open Store button handler
        let on_open_store = {
            let backend = backend.clone();
            let connection_status = connection_status.clone();
            let stores = stores.clone();
            let nodes = nodes.clone();
            let children = children.clone();
            let expanded = expanded.clone();
            let tree_items = tree_items.clone();
            move || {
                if let Some(ref h) = backend.get() {
                    let path = "./new-store.pimble".to_string();
                    let _ = h.send_command(BackendCommand::OpenStore { path });
                    // Poll for response - more iterations
                    for _ in 0..50 {
                        poll_backend_events(h, &connection_status, &stores, &nodes, &children, &expanded, &tree_items);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }
        };

        // Tree item click handler
        let stores_clone = stores.clone();
        let nodes_clone = nodes.clone();
        let children_clone = children.clone();
        let expanded_clone = expanded.clone();
        let tree_items_clone = tree_items.clone();
        let backend_clone = backend.clone();
        let selected_id_clone = selected_id.clone();

        let on_tree_click = move |item_id: String| {
            let items = tree_items_clone.get();
            if let Some(item) = items.iter().find(|i| i.id == item_id) {
                if item.expandable {
                    let key = if let Some(node_id) = item.node_id {
                        (item.store_id, node_id)
                    } else if let Some(store) = stores_clone.get().get(&item.store_id) {
                        (item.store_id, store.root_node_id)
                    } else {
                        return;
                    };

                    expanded_clone.update(|e| {
                        if e.contains(&key) {
                            e.remove(&key);
                        } else {
                            e.insert(key);
                        }
                    });

                    if !expanded_clone.get().contains(&key) {
                        if let Some(ref h) = backend_clone.get() {
                            let (_, node_id) = key;
                            let _ = h.send_command(BackendCommand::GetChildren {
                                store_id: item.store_id,
                                node_id,
                            });
                        }
                    }

                    // Rebuild tree
                    let stores_val = stores_clone.get();
                    let nodes_val = nodes_clone.get();
                    let children_val = children_clone.get();
                    let expanded_val = expanded_clone.get();
                    let mut new_items = Vec::new();
                    rebuild_tree_items(&stores_val, &nodes_val, &children_val, &expanded_val, &mut new_items);
                    tree_items_clone.set(new_items);
                } else {
                    selected_id_clone.set(Some(item_id.clone()));

                    if let Some(node_id) = item.node_id {
                        if let Some(ref h) = backend_clone.get() {
                            let _ = h.send_command(BackendCommand::GetNode {
                                store_id: item.store_id,
                                node_id,
                            });
                        }
                    }
                }
            }
        };

        // Force reactive update by using tick
        let _ = tick.get();

        rsx! {
            div {
                style: "display: flex; flex-direction: column; height: 100vh; background: #1e1e1e; color: #cccccc; font-family: sans-serif;",

                // Toolbar
                div {
                    style: "display: flex; gap: 8px; padding: 8px 16px; background: #252526; border-bottom: 1px solid #3c3c3c;",
                    button {
                        style: "padding: 6px 16px; background: #0e639c; color: white; border: none; border-radius: 2px; cursor: pointer;",
                        onclick: move || {
                            click_count.update(|c| *c += 1);
                            on_new_store();
                        },
                        "New Store ("
                        { click_count.get().to_string() }
                        ")"
                    }
                    button {
                        style: "padding: 6px 16px; background: #0e639c; color: white; border: none; border-radius: 2px; cursor: pointer;",
                        onclick: move || on_open_store(),
                        "Open Store"
                    }
                }

                // Status
                div {
                    style: "padding: 4px 16px; background: #252526; border-bottom: 1px solid #3c3c3c; font-size: 12px;",
                    { connection_status.get() }
                }

                // Main Content
                div {
                    style: "display: flex; flex: 1; overflow: hidden;",

                    // Sidebar
                    div {
                        style: "width: 250px; background: #252526; border-right: 1px solid #3c3c3c; overflow-y: auto; padding: 8px;",

                        // Use a separate variable for is_empty check
                        if tree_items.get().is_empty() {
                            div {
                                style: "padding: 8px; color: #808080;",
                                "No stores open"
                            }
                        } else {
                            for item in tree_items.get() {
                                div {
                                    style: format!("padding: 4px 8px; cursor: pointer; padding-left: {}px;", item.depth * 16),
                                    onclick: move || on_tree_click(item.id.clone()),
                                    "{item.icon} {item.label}"
                                }
                            }
                        }
                    }

                    // Editor
                    div {
                        style: "flex: 1; padding: 16px; overflow: auto;",
                        div {
                            style: "font-size: 18px; font-weight: 600; padding: 8px 0; margin-bottom: 8px; border-bottom: 1px solid #3c3c3c;",
                            "Editor"
                        }

                        if selected_id.get().is_some() {
                            div {
                                style: "min-height: 200px; padding: 8px; border: 1px solid #3c3c3c; border-radius: 2px;",
                                { selected_node_content.get() }
                            }
                        } else {
                            div {
                                style: "color: #808080;",
                                "Select a node to edit"
                            }
                        }
                    }
                }
            }
        }
    }

    // Run the app
    rinch::run("Pimble", 1200, 800, pimble_app);
}

/// Rebuild tree items from current state
fn rebuild_tree_items(
    stores: &HashMap<StoreId, Store>,
    nodes: &HashMap<(StoreId, NodeId), Node>,
    children: &HashMap<(StoreId, NodeId), Vec<NodeId>>,
    expanded: &HashSet<(StoreId, NodeId)>,
    items: &mut Vec<TreeItem>,
) {
    for (store_id, store) in stores {
        let root_node_id = store.root_node_id;
        let store_expanded = expanded.contains(&(*store_id, root_node_id));

        items.push(TreeItem {
            id: format!("store_{}", store_id),
            store_id: *store_id,
            node_id: None,
            label: store.name.clone(),
            icon: "📁".to_string(),
            depth: 0,
            expandable: true,
            expanded: store_expanded,
            is_store: true,
        });

        if store_expanded {
            add_children_to_tree(store_id, &root_node_id, nodes, children, expanded, items, 1);
        }
    }
}

/// Recursively add children to tree
fn add_children_to_tree(
    store_id: &StoreId,
    parent_id: &NodeId,
    nodes: &HashMap<(StoreId, NodeId), Node>,
    children: &HashMap<(StoreId, NodeId), Vec<NodeId>>,
    expanded: &HashSet<(StoreId, NodeId)>,
    items: &mut Vec<TreeItem>,
    depth: i32,
) {
    if let Some(child_ids) = children.get(&(*store_id, *parent_id)) {
        for child_id in child_ids {
            if let Some(node) = nodes.get(&(*store_id, *child_id)) {
                let is_folder = node.node_type == "folder";
                let is_expanded = expanded.contains(&(*store_id, *child_id));

                let icon = if is_folder { "📂" } else { "📄" };

                items.push(TreeItem {
                    id: format!("node_{}_{}", store_id, child_id),
                    store_id: *store_id,
                    node_id: Some(*child_id),
                    label: node.metadata.title.clone(),
                    icon: icon.to_string(),
                    depth,
                    expandable: is_folder,
                    expanded: is_expanded,
                    is_store: false,
                });

                if is_expanded && is_folder {
                    add_children_to_tree(store_id, child_id, nodes, children, expanded, items, depth + 1);
                }
            }
        }
    }
}
