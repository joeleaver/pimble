//! Application state management
//!
//! Uses per-entity reactive signals so that data changes (rename, mount state)
//! only trigger effects on the affected node, while structural changes (children
//! loaded, node moved, store opened/closed) bump `tree_structure_version` to
//! trigger a full tree rebuild.

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
#[derive(Debug, Clone)]
pub struct MountInfo {
    pub is_mount: bool,
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

/// Compute display label from a Node reference (no signal dependency).
pub fn display_label_from_node(node: &Node) -> String {
    let has_explicit_title = node.metadata.custom
        .get("explicit_title")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if has_explicit_title && !node.metadata.title.is_empty() {
        return node.metadata.title.clone();
    }

    let content = get_node_content_text(&node.content);
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

/// Global application state with per-entity reactive signals.
///
/// Structural changes bump `tree_structure_version` to trigger tree rebuilds.
/// Data changes (rename, mount state) only update per-entity signals, so only
/// the affected node's render Effects fire.
#[derive(Clone, Copy)]
pub struct AppStore {
    // Backend
    pub backend: Signal<Option<BackendHandle>>,
    pub connection: Signal<ConnectionState>,
    pub pending_create_path: Signal<Option<String>>,

    // Per-entity signal registries
    pub store_ids: Signal<Vec<StoreId>>,
    pub store_data: Signal<HashMap<StoreId, Signal<Store>>>,
    pub node_data: Signal<HashMap<(StoreId, NodeId), Signal<Node>>>,
    pub children_of: Signal<HashMap<(StoreId, NodeId), Signal<Vec<NodeId>>>>,
    pub mount_data: Signal<HashMap<(StoreId, NodeId), Signal<MountInfo>>>,

    /// Bumped only on structural changes (store open/close, children loaded, node moved).
    /// The data_source closure subscribes to this to trigger tree rebuilds.
    pub tree_structure_version: Signal<u64>,

    pub expanded: Signal<HashSet<(StoreId, NodeId)>>,

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
            store_ids: Signal::new(Vec::new()),
            store_data: Signal::new(HashMap::new()),
            node_data: Signal::new(HashMap::new()),
            children_of: Signal::new(HashMap::new()),
            mount_data: Signal::new(HashMap::new()),
            tree_structure_version: Signal::new(0),
            expanded: Signal::new(HashSet::new()),
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

    /// Bump the tree structure version to trigger a tree rebuild.
    pub fn bump_tree_structure(&self) {
        self.tree_structure_version.update(|v| *v += 1);
    }

    /// Insert or update a store's signal and add to store_ids if new.
    ///
    /// Looks up the inner signal first, then sets it *outside* the outer
    /// HashMap borrow to avoid RefCell re-entrancy if subscribers read back.
    pub fn upsert_store(&self, store: Store) {
        let sid = store.id;
        let existing = self.store_data.with(|map| map.get(&sid).copied());
        if let Some(sig) = existing {
            sig.set(store);
        } else {
            // Pre-create signal outside .update() — Signal::new() and .update()
            // both borrow SIGNAL_STORE mutably, so nesting them panics.
            let new_sig = Signal::new(store);
            self.store_data.update(|map| {
                map.insert(sid, new_sig);
            });
        }
        self.store_ids.update(|ids| {
            if !ids.contains(&sid) {
                ids.push(sid);
            }
        });
    }

    /// Remove a store and all its associated per-entity signals.
    pub fn remove_store(&self, store_id: StoreId) {
        self.store_ids.update(|ids| ids.retain(|&id| id != store_id));
        self.store_data.update(|map| { map.remove(&store_id); });
        self.node_data.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.children_of.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
        self.mount_data.update(|map| { map.retain(|(sid, _), _| *sid != store_id); });
    }

    /// Insert or update a node's per-entity signal.
    ///
    /// Sets the inner signal outside the outer borrow to avoid re-entrancy.
    pub fn upsert_node(&self, store_id: StoreId, node: Node) {
        let key = (store_id, node.id);
        let existing = self.node_data.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.set(node);
        } else {
            let new_sig = Signal::new(node);
            self.node_data.update(|map| {
                map.insert(key, new_sig);
            });
        }
    }

    /// Set children for a parent node's per-entity signal.
    ///
    /// Sets the inner signal outside the outer borrow to avoid re-entrancy.
    pub fn set_children(&self, store_id: StoreId, parent_id: NodeId, child_ids: Vec<NodeId>) {
        let key = (store_id, parent_id);
        let existing = self.children_of.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.set(child_ids);
        } else {
            let new_sig = Signal::new(child_ids);
            self.children_of.update(|map| {
                map.insert(key, new_sig);
            });
        }
    }

    /// Get the per-node signal (untracked read of the registry).
    pub fn get_node_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<Node>> {
        untracked(|| self.node_data.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Get the per-store signal (untracked read of the registry).
    pub fn get_store_signal(&self, store_id: StoreId) -> Option<Signal<Store>> {
        untracked(|| self.store_data.with(|map| map.get(&store_id).copied()))
    }

    /// Get the per-mount signal (untracked read of the registry).
    pub fn get_mount_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<MountInfo>> {
        untracked(|| self.mount_data.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Get the children signal for a parent (untracked read of the registry).
    pub fn get_children_signal(&self, store_id: StoreId, node_id: NodeId) -> Option<Signal<Vec<NodeId>>> {
        untracked(|| self.children_of.with(|map| map.get(&(store_id, node_id)).copied()))
    }

    /// Check if children have been loaded for a parent.
    pub fn has_children_loaded(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| self.children_of.with(|map| map.contains_key(&(store_id, node_id))))
    }

    /// Get the root node id for a store (untracked).
    pub fn root_node_id(&self, store_id: StoreId) -> Option<NodeId> {
        self.get_store_signal(store_id).map(|sig| untracked(|| sig.with(|s| s.root_node_id)))
    }

    /// Get all local paths of open stores (untracked, for persistence).
    pub fn all_store_local_paths(&self) -> Vec<String> {
        untracked(|| {
            let ids = self.store_ids.get();
            ids.iter().filter_map(|&sid| {
                self.store_data.with(|map| {
                    map.get(&sid).and_then(|sig| {
                        sig.with(|s| s.local_path().map(|p| p.to_string_lossy().to_string()))
                    })
                })
            }).collect()
        })
    }

    /// Update mount info for a node. Call this whenever a node is inserted into the cache.
    pub fn track_mount_info(&self, store_id: StoreId, node: &pimble_core::Node) {
        if node.is_mount() {
            let key = (store_id, node.id);
            let existing = self.mount_data.with(|map| map.get(&key).copied());
            if let Some(sig) = existing {
                sig.update(|m| { m.is_mount = true; });
            } else {
                let new_sig = Signal::new(MountInfo {
                    is_mount: true,
                    mount_state: None,
                });
                self.mount_data.update(|map| {
                    map.insert(key, new_sig);
                });
            }
        }
    }

    /// Update the mount state for a specific node.
    pub fn set_mount_state(&self, store_id: StoreId, node_id: NodeId, state: MountState) {
        let key = (store_id, node_id);
        let existing = self.mount_data.with(|map| map.get(&key).copied());
        if let Some(sig) = existing {
            sig.update(|m| { m.mount_state = Some(state); });
        }
    }

    /// Check if a node is a mount point (untracked).
    pub fn is_mount(&self, store_id: StoreId, node_id: NodeId) -> bool {
        untracked(|| {
            self.mount_data.with(|map| {
                map.get(&(store_id, node_id)).map_or(false, |sig| sig.with(|m| m.is_mount))
            })
        })
    }

    /// Build structural tree data for the Tree component's data_source.
    ///
    /// Must be called from within an `untracked()` context. Builds TreeNodeData
    /// with empty labels for nodes (render_node Effects fill them reactively)
    /// and store names for store roots.
    pub fn build_tree_data_structural(&self) -> Vec<TreeNodeData> {
        let store_ids = self.store_ids.get();
        let mut result = Vec::new();
        for &sid in &store_ids {
            let store_info = self.store_data.with(|map| {
                map.get(&sid).map(|sig| sig.with(|s| (s.name.clone(), s.root_node_id)))
            });
            let Some((store_name, root_id)) = store_info else { continue };

            let store_node = TreeNodeData::new(format!("store_{}", sid), &store_name);
            let children = self.build_children_structural(sid, root_id);
            if children.is_empty() {
                result.push(store_node);
            } else {
                result.push(store_node.with_children(children));
            }
        }
        result
    }

    /// Build structural children recursively (called from untracked context).
    fn build_children_structural(&self, store_id: StoreId, parent_id: NodeId) -> Vec<TreeNodeData> {
        let child_ids = self.children_of.with(|map| {
            map.get(&(store_id, parent_id)).map(|sig| sig.get())
        });
        let Some(child_ids) = child_ids else { return Vec::new() };

        let mut result = Vec::new();
        for &child_id in &child_ids {
            let is_mount = self.mount_data.with(|map| {
                map.get(&(store_id, child_id)).map_or(false, |sig| sig.with(|m| m.is_mount))
            });

            // Empty label — render_node Effects will populate reactively
            let tree_node = TreeNodeData::new(
                format!("node_{}_{}", store_id, child_id),
                "",
            );

            let children_data = self.build_children_structural(store_id, child_id);
            let has_children = !children_data.is_empty();
            let has_loaded_children = self.children_of.with(|map| {
                map.contains_key(&(store_id, child_id))
            });

            if has_children {
                result.push(tree_node.with_children(children_data));
            } else if is_mount && !has_loaded_children {
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

    /// Compute the display label for a node (untracked, for use in callbacks).
    pub fn display_label(&self, store_id: StoreId, node_id: NodeId) -> String {
        untracked(|| {
            self.node_data.with(|map| {
                map.get(&(store_id, node_id))
                    .map(|sig| sig.with(|node| display_label_from_node(node)))
                    .unwrap_or_else(|| "Untitled".to_string())
            })
        })
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
