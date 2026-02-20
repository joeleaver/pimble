//! Application state management

use std::collections::{HashMap, HashSet};

use pimble_core::{Node, NodeId, Store, StoreId, Workspace};

use crate::backend::BackendHandle;

/// A flattened tree item for display
#[derive(Debug, Clone, PartialEq)]
pub struct TreeItem {
    /// Unique identifier for this item (as string for Slint)
    pub id: String,
    /// The store this item belongs to
    pub store_id: StoreId,
    /// Node ID (None if this is a store header)
    pub node_id: Option<NodeId>,
    /// Display text
    pub label: String,
    /// Icon character
    pub icon: String,
    /// Indentation level
    pub depth: i32,
    /// Whether this item can be expanded (has children)
    pub expandable: bool,
    /// Whether this item is currently expanded
    pub expanded: bool,
    /// Whether this is a store header
    pub is_store: bool,
}

/// Global application state
pub struct AppState {
    /// Backend communication handle
    pub backend: Option<BackendHandle>,

    /// Connection status
    pub connection: ConnectionState,

    /// Pending store creation path (for auto-open after create)
    pub pending_create_path: Option<String>,

    /// Current workspace
    pub workspace: Option<Workspace>,

    /// Open stores (loaded from workspace or opened manually)
    pub stores: HashMap<StoreId, Store>,

    /// Cached nodes by (store_id, node_id)
    pub nodes: HashMap<(StoreId, NodeId), Node>,

    /// Children cache: parent -> children ids
    pub children: HashMap<(StoreId, NodeId), Vec<NodeId>>,

    /// Currently selected item ID (as string)
    pub selected_id: Option<String>,

    /// Expanded nodes in tree view
    pub expanded: HashSet<(StoreId, NodeId)>,

    /// Pending operations (for loading indicators)
    pub loading: LoadingState,

    /// Error message to display
    pub error: Option<String>,

    /// Flattened tree items for display
    pub tree_items: Vec<TreeItem>,

    /// Counter for generating unique tree item IDs
    tree_item_counter: u64,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            backend: None,
            connection: ConnectionState::Disconnected,
            pending_create_path: None,
            workspace: None,
            stores: HashMap::new(),
            nodes: HashMap::new(),
            children: HashMap::new(),
            selected_id: None,
            expanded: HashSet::new(),
            loading: LoadingState::default(),
            error: None,
            tree_items: Vec::new(),
            tree_item_counter: 0,
        }
    }

    /// Rebuild the flattened tree items from current state
    pub fn rebuild_tree_items(&mut self) {
        self.tree_items.clear();
        self.tree_item_counter = 0;

        // Collect store info first to avoid borrow issues
        let stores_info: Vec<_> = self.stores.values()
            .map(|s| (s.id, s.root_node_id, s.name.clone()))
            .collect();

        for (store_id, root_node_id, store_name) in stores_info {
            // Add store header
            let store_expanded = self.expanded.contains(&(store_id, root_node_id));

            let item_id = self.next_tree_item_id();
            self.tree_items.push(TreeItem {
                id: format!("store_{}", item_id),
                store_id,
                node_id: None,
                label: store_name,
                icon: "📁".to_string(),
                depth: 0,
                expandable: true,
                expanded: store_expanded,
                is_store: true,
            });

            // Add children if expanded
            if store_expanded {
                self.add_children_to_tree(store_id, root_node_id, 1);
            }
        }
    }

    fn add_children_to_tree(&mut self, store_id: StoreId, parent_id: NodeId, depth: i32) {
        if let Some(child_ids) = self.children.get(&(store_id, parent_id)).cloned() {
            for child_id in child_ids {
                if let Some(node) = self.nodes.get(&(store_id, child_id)).cloned() {
                    let is_folder = node.node_type == "folder";
                    let is_expanded = self.expanded.contains(&(store_id, child_id));

                    let icon = if is_folder { "📂" } else { "📄" };

                    let _item_id = self.next_tree_item_id();
                    self.tree_items.push(TreeItem {
                        id: format!("node_{}_{}", store_id, child_id),
                        store_id,
                        node_id: Some(child_id),
                        label: node.metadata.title.clone(),
                        icon: icon.to_string(),
                        depth,
                        expandable: is_folder,
                        expanded: is_expanded,
                        is_store: false,
                    });

                    // Recursively add children if expanded
                    if is_expanded && is_folder {
                        self.add_children_to_tree(store_id, child_id, depth + 1);
                    }
                }
            }
        }
    }

    fn next_tree_item_id(&mut self) -> u64 {
        self.tree_item_counter += 1;
        self.tree_item_counter
    }

    /// Find a tree item by ID and return its store_id and node_id
    pub fn find_tree_item(&self, id: &str) -> Option<(StoreId, Option<NodeId>)> {
        self.tree_items
            .iter()
            .find(|item| item.id == id)
            .map(|item| (item.store_id, item.node_id))
    }

    /// Toggle expansion of a tree item
    pub fn toggle_expansion(&mut self, id: &str) {
        if let Some(item) = self.tree_items.iter().find(|item| item.id == id) {
            let key = if let Some(node_id) = item.node_id {
                (item.store_id, node_id)
            } else {
                // Store header - use root node
                if let Some(store) = self.stores.get(&item.store_id) {
                    (item.store_id, store.root_node_id)
                } else {
                    return;
                }
            };

            if self.expanded.contains(&key) {
                self.expanded.remove(&key);
            } else {
                self.expanded.insert(key);
            }
        }
    }

    /// Get the selected node (if any)
    pub fn selected_node(&self) -> Option<&Node> {
        let selected_id = self.selected_id.as_ref()?;
        let item = self.tree_items.iter().find(|item| &item.id == selected_id)?;
        let node_id = item.node_id?;
        self.nodes.get(&(item.store_id, node_id))
    }

    /// Get the store_id and node_id of the selected node (if any)
    pub fn selected_store_and_node(&self) -> Option<(StoreId, NodeId)> {
        let selected_id = self.selected_id.as_ref()?;
        let item = self.tree_items.iter().find(|item| &item.id == selected_id)?;
        let node_id = item.node_id?;
        Some((item.store_id, node_id))
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

impl ConnectionState {
    pub fn as_str(&self) -> &str {
        match self {
            ConnectionState::Disconnected => "Disconnected",
            ConnectionState::Connecting => "Connecting...",
            ConnectionState::Connected => "Connected",
            ConnectionState::Error(msg) => msg.as_str(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoadingState {
    pub connecting: bool,
    pub loading_stores: bool,
    pub loading_nodes: HashSet<(StoreId, NodeId)>,
}
