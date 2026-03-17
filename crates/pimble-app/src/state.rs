//! Application state management

use std::collections::{HashMap, HashSet};

use pimble_core::{MountState, Node, NodeId, Store, StoreId};
use pimble_crdt::DocumentContent;
use rinch::components::TreeNodeData;
use rinch::prelude::*;
use rinch_editor::EditorDocument;

use crate::backend::BackendHandle;

/// A pending mount operation waiting for a store to be opened.
#[derive(Debug, Clone)]
pub struct PendingMount {
    pub target_store_id: StoreId,
    pub target_parent_id: NodeId,
    pub source_path: String,
}

/// Mount metadata for a tree node.
///
/// Stored in `AppStore::mount_info` for nodes where `node.is_mount()` is true.
#[derive(Debug, Clone)]
pub struct MountInfo {
    /// Whether this node is a mount point
    pub is_mount: bool,
    /// Current state of the mount (Live, Unavailable, etc.)
    pub mount_state: Option<MountState>,
}

/// Extract text content from node content bytes.
///
/// Tries new format (EditorDocument) first, falls back to old format (DocumentContent).
pub fn get_node_content_text(content: &[u8]) -> String {
    if content.is_empty() {
        return String::new();
    }

    // Try new format first (EditorDocument with rich blocks)
    if let Ok(doc) = EditorDocument::from_bytes(content) {
        return doc.to_markdown();
    }

    // Fall back to old format (DocumentContent with flat text)
    match DocumentContent::load(content) {
        Ok(doc) => doc.get_text().unwrap_or_else(|_| {
            String::from_utf8_lossy(content).to_string()
        }),
        Err(_) => String::from_utf8_lossy(content).to_string(),
    }
}

/// Global application state as a Rinch store.
///
/// All fields are independent `Signal<T>` values, so there are no borrow
/// conflicts and no deferred updates needed. The struct is `Clone + Copy`
/// (Signal is a lightweight reactive reference).
#[derive(Clone, Copy)]
pub struct AppStore {
    // Backend
    pub backend: Signal<Option<BackendHandle>>,
    pub connection: Signal<ConnectionState>,
    pub pending_create_path: Signal<Option<String>>,

    // Caches
    pub stores: Signal<HashMap<StoreId, Store>>,
    pub nodes: Signal<HashMap<(StoreId, NodeId), Node>>,
    pub children: Signal<HashMap<(StoreId, NodeId), Vec<NodeId>>>,
    pub expanded: Signal<HashSet<(StoreId, NodeId)>>,

    // Mount metadata: tracks which nodes are mounts and their current state
    pub mount_info: Signal<HashMap<(StoreId, NodeId), MountInfo>>,

    // UI state
    pub connection_status: Signal<String>,
    pub server_addr: Signal<String>,
    pub selected_id: Signal<Option<String>>,
    pub node_title: Signal<String>,
    pub show_editor: Signal<bool>,

    // Inline rename
    pub renaming_node: Signal<Option<String>>,
    pub rename_text: Signal<String>,

    // Drag-and-drop
    pub drop_target: Signal<Option<String>>,

    // Rename input DOM node IDs (tree_value → input element node ID for request_focus)
    pub rename_input_ids: Signal<HashMap<String, usize>>,

    // Mount picker
    pub pending_mount: Signal<Option<PendingMount>>,
}

impl AppStore {
    pub fn new() -> Self {
        Self {
            backend: Signal::new(None),
            connection: Signal::new(ConnectionState::Disconnected),
            pending_create_path: Signal::new(None),
            stores: Signal::new(HashMap::new()),
            nodes: Signal::new(HashMap::new()),
            children: Signal::new(HashMap::new()),
            expanded: Signal::new(HashSet::new()),
            mount_info: Signal::new(HashMap::new()),
            connection_status: Signal::new("Connecting...".to_string()),
            server_addr: Signal::new(String::new()),
            selected_id: Signal::new(None),
            node_title: Signal::new(String::new()),
            show_editor: Signal::new(false),
            renaming_node: Signal::new(None),
            rename_text: Signal::new(String::new()),
            drop_target: Signal::new(None),
            rename_input_ids: Signal::new(HashMap::new()),
            pending_mount: Signal::new(None),
        }
    }

    /// Update mount info for a node. Call this whenever a node is inserted into the cache.
    pub fn track_mount_info(&self, store_id: StoreId, node: &pimble_core::Node) {
        if node.is_mount() {
            self.mount_info.update(|info| {
                info.insert((store_id, node.id), MountInfo {
                    is_mount: true,
                    mount_state: None, // Will be populated by GetMountState
                });
            });
        }
    }

    /// Update the mount state for a specific node.
    pub fn set_mount_state(&self, store_id: StoreId, node_id: NodeId, state: MountState) {
        self.mount_info.update(|info| {
            if let Some(mount) = info.get_mut(&(store_id, node_id)) {
                mount.mount_state = Some(state);
            }
        });
    }

    /// Check if a node is a mount point.
    pub fn is_mount(&self, store_id: StoreId, node_id: NodeId) -> bool {
        self.mount_info.with(|info| {
            info.get(&(store_id, node_id)).map_or(false, |m| m.is_mount)
        })
    }

    /// Build Rinch TreeNodeData hierarchy from current state.
    /// Each store appears as a top-level tree node with its children underneath.
    ///
    /// Uses `.get()` (clone) instead of `.with()` (borrow) so this can safely
    /// be called from reactive contexts without RefCell re-entrancy panics.
    pub fn build_tree_data(&self) -> Vec<TreeNodeData> {
        let stores = self.stores.get();
        let children = self.children.get();
        let nodes = self.nodes.get();
        let mount_info = self.mount_info.get();
        let mut result = Vec::new();
        for store in stores.values() {
            let store_node = TreeNodeData::new(
                format!("store_{}", store.id),
                &store.name,
            );
            let children_data = build_children_data(&children, &nodes, &mount_info, store.id, store.root_node_id);
            if children_data.is_empty() {
                result.push(store_node);
            } else {
                result.push(store_node.with_children(children_data));
            }
        }
        result
    }

    /// Compute the display label for a node in the tree.
    ///
    /// - If `explicit_title` custom flag is set and title is non-empty → use title
    /// - Else if node content has text → first line, up to 25 chars (+ "…" if truncated)
    /// - Else if title is non-empty → use title (legacy nodes)
    /// - Else → "Untitled"
    pub fn display_label(&self, store_id: StoreId, node_id: NodeId) -> String {
        let nodes = self.nodes.get();
        display_label_inner(&nodes, store_id, node_id)
    }

    /// Get the store_id and node_id of the selected node (if any)
    pub fn selected_store_and_node(&self) -> Option<(StoreId, NodeId)> {
        let selected_id = self.selected_id.get()?;
        let (store_id, node_id) = parse_tree_value(&selected_id)?;
        let node_id = node_id?;
        Some((store_id, node_id))
    }

    /// Send a command to the backend if connected.
    pub fn send(&self, cmd: crate::backend::BackendCommand) {
        self.backend.with(|b| {
            if let Some(backend) = b {
                backend.send(cmd);
            }
        });
    }
}

/// Parse a tree value ID like "store_{uuid}" or "node_{store_uuid}_{node_uuid}"
pub fn parse_tree_value(value: &str) -> Option<(StoreId, Option<NodeId>)> {
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

/// Compute display label from a nodes map (no signal dependency).
fn display_label_inner(nodes: &HashMap<(StoreId, NodeId), Node>, store_id: StoreId, node_id: NodeId) -> String {
    let Some(node) = nodes.get(&(store_id, node_id)) else {
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

/// Build TreeNodeData for children of the given parent node (no signal dependency).
fn build_children_data(
    children: &HashMap<(StoreId, NodeId), Vec<NodeId>>,
    nodes: &HashMap<(StoreId, NodeId), Node>,
    mount_info: &HashMap<(StoreId, NodeId), MountInfo>,
    store_id: StoreId,
    parent_id: NodeId,
) -> Vec<TreeNodeData> {
    let Some(child_ids) = children.get(&(store_id, parent_id)) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for &child_id in child_ids {
        let is_mount = mount_info.get(&(store_id, child_id)).map_or(false, |m| m.is_mount);
        let label = display_label_inner(nodes, store_id, child_id);
        let tree_node = TreeNodeData::new(
            format!("node_{}_{}", store_id, child_id),
            &label,
        );

        // Always check for children — any node can have children via drag-and-drop
        let children_data = build_children_data(children, nodes, mount_info, store_id, child_id);
        let has_children = !children_data.is_empty();
        if has_children {
            result.push(tree_node.with_children(children_data));
        } else if is_mount && !children.contains_key(&(store_id, child_id)) {
            // Mount nodes without loaded children get a placeholder so the
            // expand chevron is visible. The placeholder is replaced when
            // the user expands the node and children are fetched.
            let placeholder = TreeNodeData::new(
                format!("mount_loading_{}_{}", store_id, child_id),
                "Loading...",
            );
            result.push(tree_node.with_children(vec![placeholder]));
        } else {
            result.push(tree_node);
        }
    }
    result
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
