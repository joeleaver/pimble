//! Application state management

use std::collections::{HashMap, HashSet};

use pimble_core::{Node, NodeId, Store, StoreId, Workspace};
use pimble_crdt::DocumentContent;
use rinch::components::TreeNodeData;

use crate::backend::BackendHandle;

/// Extract text content from CRDT node content bytes
pub fn get_node_content_text(content: &[u8]) -> String {
    if content.is_empty() {
        return String::new();
    }

    match DocumentContent::load(content) {
        Ok(doc) => doc.get_text().unwrap_or_else(|_| {
            String::from_utf8_lossy(content).to_string()
        }),
        Err(_) => String::from_utf8_lossy(content).to_string(),
    }
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
        }
    }

    /// Build Rinch TreeNodeData hierarchy from current state.
    /// Root children are returned as top-level items (stores appear as headings, not tree nodes).
    pub fn build_tree_data(&self) -> Vec<TreeNodeData> {
        let mut result = Vec::new();
        for store in self.stores.values() {
            result.extend(self.build_children_data(store.id, store.root_node_id));
        }
        result
    }

    /// Heading text for the sidebar (store name or fallback)
    pub fn sidebar_heading(&self) -> String {
        match self.stores.len() {
            0 => String::new(),
            1 => self.stores.values().next().unwrap().name.clone(),
            n => format!("{} Stores", n),
        }
    }

    /// Compute the display label for a node in the tree.
    ///
    /// - If `explicit_title` custom flag is set and title is non-empty → use title
    /// - Else if node content has text → first line, up to 25 chars (+ "…" if truncated)
    /// - Else if title is non-empty → use title (legacy nodes)
    /// - Else → "Untitled"
    pub fn display_label(&self, store_id: StoreId, node_id: NodeId) -> String {
        let Some(node) = self.nodes.get(&(store_id, node_id)) else {
            return "Untitled".to_string();
        };

        let has_explicit_title = node.metadata.custom
            .get("explicit_title")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if has_explicit_title && !node.metadata.title.is_empty() {
            return node.metadata.title.clone();
        }

        let content = get_node_content_text(&node.content);
        // Use only the first non-empty line to avoid newlines in tree labels
        let first_line = content
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        if !first_line.is_empty() {
            let char_count = first_line.chars().count();
            if char_count > 25 {
                let truncated: String = first_line.chars().take(25).collect();
                return format!("{truncated}…");
            } else {
                return first_line.to_string();
            }
        }

        if !node.metadata.title.is_empty() {
            return node.metadata.title.clone();
        }

        "Untitled".to_string()
    }

    /// Build TreeNodeData for children of the given parent node.
    fn build_children_data(&self, store_id: StoreId, parent_id: NodeId) -> Vec<TreeNodeData> {
        let Some(child_ids) = self.children.get(&(store_id, parent_id)) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for &child_id in child_ids {
            let label = self.display_label(store_id, child_id);
            let tree_node = TreeNodeData::new(
                format!("node_{}_{}", store_id, child_id),
                &label,
            );

            // Always check for children — any node can have children via drag-and-drop
            let children_data = self.build_children_data(store_id, child_id);
            let has_children = !children_data.is_empty();
            if has_children {
                result.push(tree_node.with_children(children_data));
            } else {
                result.push(tree_node);
            }
        }
        result
    }

    /// Get the selected node (if any)
    pub fn selected_node(&self) -> Option<&Node> {
        let selected_id = self.selected_id.as_ref()?;
        let (store_id, node_id_opt) = self.parse_tree_value(selected_id)?;
        let node_id = node_id_opt?;
        self.nodes.get(&(store_id, node_id))
    }

    /// Get the store_id and node_id of the selected node (if any)
    pub fn selected_store_and_node(&self) -> Option<(StoreId, NodeId)> {
        let selected_id = self.selected_id.as_ref()?;
        let (store_id, node_id) = self.parse_tree_value(selected_id)?;
        let node_id = node_id?;
        Some((store_id, node_id))
    }

    /// Parse a tree value ID like "store_{uuid}" or "node_{store_uuid}_{node_uuid}"
    pub fn parse_tree_value(&self, value: &str) -> Option<(StoreId, Option<NodeId>)> {
        if let Some(rest) = value.strip_prefix("store_") {
            let uuid: uuid::Uuid = rest.parse().ok()?;
            Some((StoreId(uuid), None))
        } else if let Some(rest) = value.strip_prefix("node_") {
            // Format: node_{store_uuid}_{node_uuid}
            // UUIDs are 36 chars each
            if rest.len() >= 73 {
                let store_str = &rest[..36];
                let node_str = &rest[37..];
                let store_uuid: uuid::Uuid = store_str.parse().ok()?;
                let node_uuid: uuid::Uuid = node_str.parse().ok()?;
                Some((StoreId(store_uuid), Some(NodeId(node_uuid))))
            } else {
                None
            }
        } else {
            None
        }
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
