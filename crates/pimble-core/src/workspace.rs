//! Workspace types - user's view into stores

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{NodeId, Store, StoreId};

/// A Workspace defines which stores are visible to the user
///
/// Workspaces are saved as `.pimble-workspace` files and define:
/// - Which stores to load
/// - UI state (expanded nodes, column widths, etc.)
/// - Display preferences
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    /// Schema version
    pub version: u32,

    /// Unique identifier
    pub id: Uuid,

    /// Display name
    pub name: String,

    /// Stores visible in this workspace
    pub stores: Vec<WorkspaceStore>,

    /// UI state
    pub ui_state: WorkspaceUiState,
}

impl Workspace {
    /// Current schema version
    pub const CURRENT_VERSION: u32 = 1;

    /// Create a new empty workspace
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            id: Uuid::new_v4(),
            name: name.into(),
            stores: Vec::new(),
            ui_state: WorkspaceUiState::default(),
        }
    }

    /// Add a store to the workspace
    pub fn add_store(&mut self, store: Store) {
        let position = self.stores.len();
        self.stores.push(WorkspaceStore {
            store,
            display_name: None,
            position,
            expanded_nodes: HashSet::new(),
        });
    }

    /// Remove a store from the workspace
    pub fn remove_store(&mut self, store_id: &StoreId) -> bool {
        if let Some(pos) = self.stores.iter().position(|s| s.store.id == *store_id) {
            self.stores.remove(pos);
            // Recompute positions
            for (i, store) in self.stores.iter_mut().enumerate() {
                store.position = i;
            }
            true
        } else {
            false
        }
    }

    /// Get a store by ID
    pub fn get_store(&self, store_id: &StoreId) -> Option<&WorkspaceStore> {
        self.stores.iter().find(|s| s.store.id == *store_id)
    }

    /// Get a mutable store by ID
    pub fn get_store_mut(&mut self, store_id: &StoreId) -> Option<&mut WorkspaceStore> {
        self.stores.iter_mut().find(|s| s.store.id == *store_id)
    }
}

/// A store as it appears in a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStore {
    /// The actual store
    pub store: Store,

    /// Override display name (None = use store name)
    pub display_name: Option<String>,

    /// Position in the store list
    pub position: usize,

    /// Which nodes are expanded in the tree view
    pub expanded_nodes: HashSet<NodeId>,
}

impl WorkspaceStore {
    /// Get the display name for this store
    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.store.name)
    }

    /// Check if a node is expanded
    pub fn is_expanded(&self, node_id: &NodeId) -> bool {
        self.expanded_nodes.contains(node_id)
    }

    /// Toggle a node's expanded state
    pub fn toggle_expanded(&mut self, node_id: NodeId) -> bool {
        if self.expanded_nodes.contains(&node_id) {
            self.expanded_nodes.remove(&node_id);
            false
        } else {
            self.expanded_nodes.insert(node_id);
            true
        }
    }

    /// Expand a node
    pub fn expand(&mut self, node_id: NodeId) {
        self.expanded_nodes.insert(node_id);
    }

    /// Collapse a node
    pub fn collapse(&mut self, node_id: &NodeId) {
        self.expanded_nodes.remove(node_id);
    }
}

/// UI state for a workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceUiState {
    /// Width of the tree panel in pixels
    pub tree_panel_width: f32,

    /// Currently selected node
    pub selected_node: Option<(StoreId, NodeId)>,

    /// Recently opened nodes (for history navigation)
    pub recent_nodes: Vec<(StoreId, NodeId)>,

    /// Maximum recent nodes to keep
    pub max_recent: usize,
}

impl Default for WorkspaceUiState {
    fn default() -> Self {
        Self {
            tree_panel_width: 250.0,
            selected_node: None,
            recent_nodes: Vec::new(),
            max_recent: 50,
        }
    }
}

impl WorkspaceUiState {
    /// Select a node and add it to history
    pub fn select_node(&mut self, store_id: StoreId, node_id: NodeId) {
        self.selected_node = Some((store_id, node_id));

        // Add to recent, removing duplicates
        self.recent_nodes.retain(|&(s, n)| s != store_id || n != node_id);
        self.recent_nodes.insert(0, (store_id, node_id));

        // Trim to max
        if self.recent_nodes.len() > self.max_recent {
            self.recent_nodes.truncate(self.max_recent);
        }
    }

    /// Go back in history
    pub fn go_back(&mut self) -> Option<(StoreId, NodeId)> {
        if self.recent_nodes.len() > 1 {
            self.recent_nodes.remove(0);
            self.selected_node = self.recent_nodes.first().copied();
            self.selected_node
        } else {
            None
        }
    }
}

/// Workspace file reference - used when loading workspaces
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRef {
    /// Path to the workspace file
    pub path: PathBuf,

    /// Workspace name (read from file)
    pub name: String,

    /// Last opened time
    pub last_opened: Option<chrono::DateTime<chrono::Utc>>,
}
