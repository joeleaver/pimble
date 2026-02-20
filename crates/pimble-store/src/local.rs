//! Local file-based store implementation

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pimble_core::{Node, NodeId, StoreId, StoreManifest};
use pimble_crdt::CrdtDocument;
use tokio::fs;
use tracing::{debug, info};

use crate::error::{Result, StoreError};

/// A local store backed by the filesystem
///
/// Directory structure:
/// ```text
/// store.pimble/
/// ├── manifest.json           # Store metadata
/// ├── nodes/
/// │   ├── {node-id}.automerge # One Automerge doc per node
/// │   └── ...
/// ├── assets/                 # Binary files
/// │   └── {hash}.{ext}
/// └── index/                  # Search indexes (future)
/// ```
pub struct LocalStore {
    /// Store ID
    pub id: StoreId,

    /// Path to the store directory
    pub path: PathBuf,

    /// Store manifest
    manifest: StoreManifest,

    /// Cached nodes (loaded on demand)
    nodes: HashMap<NodeId, Node>,

    /// Dirty nodes that need saving
    dirty: std::collections::HashSet<NodeId>,
}

impl LocalStore {
    /// Subdirectory names
    const NODES_DIR: &'static str = "nodes";
    const ASSETS_DIR: &'static str = "assets";
    const INDEX_DIR: &'static str = "index";
    const MANIFEST_FILE: &'static str = "manifest.json";

    /// Create a new local store at the given path
    pub async fn create(path: impl AsRef<Path>, name: impl Into<String>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let name = name.into();

        // Check if store already exists
        if path.exists() {
            return Err(StoreError::StoreExists(path.display().to_string()));
        }

        // Create directory structure
        fs::create_dir_all(&path).await?;
        fs::create_dir(path.join(Self::NODES_DIR)).await?;
        fs::create_dir(path.join(Self::ASSETS_DIR)).await?;
        fs::create_dir(path.join(Self::INDEX_DIR)).await?;

        // Create root node
        let root_node = Node::folder(&name);
        let root_node_id = root_node.id;

        // Create manifest
        let manifest = StoreManifest::new(&name, root_node_id);

        // Write manifest
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(path.join(Self::MANIFEST_FILE), manifest_json).await?;

        let mut store = Self {
            id: manifest.id,
            path,
            manifest,
            nodes: HashMap::new(),
            dirty: std::collections::HashSet::new(),
        };

        // Save root node
        store.nodes.insert(root_node_id, root_node);
        store.dirty.insert(root_node_id);
        store.flush().await?;

        info!("Created local store '{}' at {:?}", name, store.path);
        Ok(store)
    }

    /// Open an existing local store
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        // Read manifest
        let manifest_path = path.join(Self::MANIFEST_FILE);
        if !manifest_path.exists() {
            return Err(StoreError::InvalidPath(format!(
                "No manifest found at {}",
                manifest_path.display()
            )));
        }

        let manifest_json = fs::read_to_string(&manifest_path).await?;
        let manifest: StoreManifest = serde_json::from_str(&manifest_json)?;

        info!("Opened local store '{}' from {:?}", manifest.name, path);

        Ok(Self {
            id: manifest.id,
            path,
            manifest,
            nodes: HashMap::new(),
            dirty: std::collections::HashSet::new(),
        })
    }

    /// Get the store manifest
    pub fn manifest(&self) -> &StoreManifest {
        &self.manifest
    }

    /// Get the root node ID
    pub fn root_node_id(&self) -> NodeId {
        self.manifest.root_node_id
    }

    /// Get a node by ID (loads from disk if not cached)
    pub async fn get_node(&mut self, node_id: NodeId) -> Result<&Node> {
        if !self.nodes.contains_key(&node_id) {
            let node = self.load_node(node_id).await?;
            self.nodes.insert(node_id, node);
        }
        self.nodes.get(&node_id).ok_or(StoreError::NodeNotFound(node_id))
    }

    /// Get a mutable node by ID
    pub async fn get_node_mut(&mut self, node_id: NodeId) -> Result<&mut Node> {
        if !self.nodes.contains_key(&node_id) {
            let node = self.load_node(node_id).await?;
            self.nodes.insert(node_id, node);
        }
        self.dirty.insert(node_id);
        self.nodes.get_mut(&node_id).ok_or(StoreError::NodeNotFound(node_id))
    }

    /// Create a new node
    pub async fn create_node(&mut self, mut node: Node, parent_id: Option<NodeId>) -> Result<NodeId> {
        let node_id = node.id;
        node.parent_id = parent_id;

        // Add to parent's children
        if let Some(pid) = parent_id {
            let parent = self.get_node_mut(pid).await?;
            parent.add_child(node_id);
        }

        self.nodes.insert(node_id, node);
        self.dirty.insert(node_id);

        debug!("Created node {} in store {}", node_id, self.id);
        Ok(node_id)
    }

    /// Delete a node
    pub async fn delete_node(&mut self, node_id: NodeId) -> Result<()> {
        // Get node to find parent
        let parent_id = {
            let node = self.get_node(node_id).await?;
            node.parent_id
        };

        // Remove from parent's children
        if let Some(pid) = parent_id {
            let parent = self.get_node_mut(pid).await?;
            parent.remove_child(&node_id);
        }

        // Remove node file
        let node_path = self.node_path(node_id);
        if node_path.exists() {
            fs::remove_file(&node_path).await?;
        }

        // Remove from cache
        self.nodes.remove(&node_id);
        self.dirty.remove(&node_id);

        debug!("Deleted node {} from store {}", node_id, self.id);
        Ok(())
    }

    /// Update a node's CRDT content
    pub async fn update_node_content(&mut self, node_id: NodeId, content: Vec<u8>) -> Result<()> {
        let node = self.get_node_mut(node_id).await?;
        node.content = content;
        node.touch();
        Ok(())
    }

    /// Get a node's CRDT document
    pub async fn get_node_document(&mut self, node_id: NodeId) -> Result<CrdtDocument> {
        let node = self.get_node(node_id).await?;
        CrdtDocument::load(&node.content).map_err(StoreError::from)
    }

    /// Save a node's CRDT document
    pub async fn save_node_document(&mut self, node_id: NodeId, doc: &mut CrdtDocument) -> Result<()> {
        let content = doc.save();
        self.update_node_content(node_id, content).await
    }

    /// Flush all dirty nodes to disk
    pub async fn flush(&mut self) -> Result<()> {
        let dirty: Vec<NodeId> = self.dirty.iter().copied().collect();

        for node_id in dirty {
            if let Some(node) = self.nodes.get(&node_id) {
                self.save_node_to_disk(node).await?;
            }
        }

        self.dirty.clear();

        // Update manifest modified time
        self.manifest.modified_at = chrono::Utc::now();
        let manifest_json = serde_json::to_string_pretty(&self.manifest)?;
        fs::write(self.path.join(Self::MANIFEST_FILE), manifest_json).await?;

        debug!("Flushed store {} to disk", self.id);
        Ok(())
    }

    /// List all node IDs in the store
    pub async fn list_node_ids(&self) -> Result<Vec<NodeId>> {
        let nodes_dir = self.path.join(Self::NODES_DIR);
        let mut entries = fs::read_dir(&nodes_dir).await?;
        let mut ids = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Some(stem) = path.file_stem() {
                    if let Ok(id) = NodeId::parse(&stem.to_string_lossy()) {
                        ids.push(id);
                    }
                }
            }
        }

        Ok(ids)
    }

    /// Get children of a node
    pub async fn get_children(&mut self, node_id: NodeId) -> Result<Vec<Node>> {
        let children_ids = {
            let node = self.get_node(node_id).await?;
            node.children.clone()
        };

        let mut children = Vec::with_capacity(children_ids.len());
        for child_id in children_ids {
            let child = self.get_node(child_id).await?;
            children.push(child.clone());
        }

        Ok(children)
    }

    // Private helpers

    fn node_path(&self, node_id: NodeId) -> PathBuf {
        self.path.join(Self::NODES_DIR).join(format!("{}.json", node_id))
    }

    fn node_content_path(&self, node_id: NodeId) -> PathBuf {
        self.path.join(Self::NODES_DIR).join(format!("{}.automerge", node_id))
    }

    async fn load_node(&self, node_id: NodeId) -> Result<Node> {
        let node_path = self.node_path(node_id);

        if !node_path.exists() {
            return Err(StoreError::NodeNotFound(node_id));
        }

        let json = fs::read_to_string(&node_path).await?;
        let mut node: Node = serde_json::from_str(&json)?;

        // Load content separately if it exists
        let content_path = self.node_content_path(node_id);
        if content_path.exists() {
            node.content = fs::read(&content_path).await?;
        }

        debug!("Loaded node {} from disk", node_id);
        Ok(node)
    }

    async fn save_node_to_disk(&self, node: &Node) -> Result<()> {
        let node_path = self.node_path(node.id);

        // Save node metadata (without content for cleaner JSON)
        let mut node_for_json = node.clone();
        let content = std::mem::take(&mut node_for_json.content);

        let json = serde_json::to_string_pretty(&node_for_json)?;
        fs::write(&node_path, json).await?;

        // Save content separately if not empty
        if !content.is_empty() {
            let content_path = self.node_content_path(node.id);
            fs::write(&content_path, &content).await?;
        }

        debug!("Saved node {} to disk", node.id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_create_store() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        assert!(store_path.exists());
        assert!(store_path.join("manifest.json").exists());
        assert!(store_path.join("nodes").exists());
    }

    #[tokio::test]
    async fn test_open_store() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let store_id = store.id;
        drop(store);

        let store = LocalStore::open(&store_path).await.unwrap();
        assert_eq!(store.id, store_id);
    }

    #[tokio::test]
    async fn test_create_node() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let root_id = store.root_node_id();

        let doc = Node::document("Test Document");
        let doc_id = store.create_node(doc, Some(root_id)).await.unwrap();

        let node = store.get_node(doc_id).await.unwrap();
        assert_eq!(node.metadata.title, "Test Document");

        let root = store.get_node(root_id).await.unwrap();
        assert!(root.children.contains(&doc_id));
    }
}
