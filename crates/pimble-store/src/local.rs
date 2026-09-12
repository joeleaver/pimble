//! Local file-based store implementation

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use pimble_core::{Node, NodeId, NodeMetadata, StoreId, StoreManifest};
use pimble_crdt::{ContentDoc, StoreDocument};
use tokio::fs;
use tracing::{debug, info, warn};

use crate::error::{Result, StoreError};
use crate::legacy;

/// Write a file atomically: write to a `.tmp` sibling, then rename into place.
/// This prevents empty/corrupt files if the process is killed mid-write.
async fn atomic_write(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, data).await?;
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

/// A local store backed by the filesystem
///
/// Directory structure (v2):
/// ```text
/// store.pimble/
/// ├── manifest.json           # Store metadata (version: 2)
/// ├── store.automerge         # Tree structure + node metadata (CRDT)
/// ├── nodes/
/// │   ├── {node-id}.yrs       # Per-node content documents (yrs)
/// │   ├── {node-id}.automerge # Legacy per-node content (pre-restart); read
/// │   │                       # once to migrate, then left untouched
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

    /// CRDT-backed tree structure and node metadata
    store_doc: StoreDocument,

    /// Whether the store document has unsaved changes
    store_doc_dirty: bool,

    /// Persistent CRDT documents per node (loaded on demand, kept in memory)
    content_docs: HashMap<NodeId, ContentDoc>,

    /// Nodes whose content needs saving
    dirty_content: std::collections::HashSet<NodeId>,
}

impl LocalStore {
    /// Subdirectory names
    const NODES_DIR: &'static str = "nodes";
    const ASSETS_DIR: &'static str = "assets";
    const INDEX_DIR: &'static str = "index";
    const MANIFEST_FILE: &'static str = "manifest.json";
    const STORE_DOC_FILE: &'static str = "store.automerge";

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

        // Create root node ID
        let root_node_id = NodeId::new();

        // Create manifest (v2)
        let manifest = StoreManifest::new(&name, root_node_id);

        // Write manifest
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(path.join(Self::MANIFEST_FILE), manifest_json).await?;

        // Create store document with root folder
        let store_doc = StoreDocument::new(&name, root_node_id)
            .map_err(|e| StoreError::Crdt(e))?;

        let mut store = Self {
            id: manifest.id,
            path,
            manifest,
            store_doc,
            store_doc_dirty: true,
            content_docs: HashMap::new(),
            dirty_content: std::collections::HashSet::new(),
        };

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

        // Load or migrate store document
        let store_doc_path = path.join(Self::STORE_DOC_FILE);
        let (store_doc, needs_migration) = if store_doc_path.exists() {
            let bytes = fs::read(&store_doc_path).await?;
            let doc = StoreDocument::load(&bytes).map_err(|e| StoreError::Crdt(e))?;
            (doc, false)
        } else {
            // Legacy v1 store — needs migration
            info!("Legacy v1 store detected, migrating to v2...");
            let doc = Self::migrate_v1(&path, &manifest).await?;
            (doc, true)
        };

        let mut store = Self {
            id: manifest.id,
            path,
            manifest,
            store_doc,
            store_doc_dirty: needs_migration,
            content_docs: HashMap::new(),
            dirty_content: std::collections::HashSet::new(),
        };

        if needs_migration {
            // Update manifest version and save
            store.manifest.version = StoreManifest::CURRENT_VERSION;
            store.flush().await?;
            info!("Migration to v2 complete");
        }

        info!("Opened local store '{}' from {:?}", store.manifest.name, store.path);
        Ok(store)
    }

    /// Migrate a v1 store (per-node .json files) to v2 (store.automerge)
    async fn migrate_v1(path: &Path, manifest: &StoreManifest) -> Result<StoreDocument> {
        let nodes_dir = path.join(Self::NODES_DIR);
        let mut entries = fs::read_dir(&nodes_dir).await?;
        let mut nodes: Vec<Node> = Vec::new();

        // Load all node JSON files
        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            if entry_path.extension().is_some_and(|e| e == "json") {
                let json = fs::read_to_string(&entry_path).await?;
                match serde_json::from_str::<Node>(&json) {
                    Ok(node) => nodes.push(node),
                    Err(e) => warn!("Skipping invalid node file {:?}: {}", entry_path, e),
                }
            }
        }

        // Create store document with root
        let mut store_doc = StoreDocument::new(&manifest.name, manifest.root_node_id)
            .map_err(|e| StoreError::Crdt(e))?;

        // Update root node timestamps from the actual node data if available
        if let Some(root_node) = nodes.iter().find(|n| n.id == manifest.root_node_id) {
            store_doc.set_timestamps(
                manifest.root_node_id,
                &root_node.metadata.created_at.to_rfc3339(),
                &root_node.metadata.modified_at.to_rfc3339(),
            ).map_err(|e| StoreError::Crdt(e))?;

            // Set tags and custom fields from root
            if !root_node.metadata.tags.is_empty() {
                store_doc.set_tags(manifest.root_node_id, &root_node.metadata.tags)
                    .map_err(|e| StoreError::Crdt(e))?;
            }
            for (key, val) in &root_node.metadata.custom {
                store_doc.set_custom(manifest.root_node_id, key, val)
                    .map_err(|e| StoreError::Crdt(e))?;
            }
        }

        // Add non-root nodes
        for node in &nodes {
            if node.id == manifest.root_node_id {
                continue;
            }
            store_doc.add_node_bare(
                node.id,
                &node.node_type,
                &node.metadata.title,
                &node.metadata.created_at.to_rfc3339(),
                &node.metadata.modified_at.to_rfc3339(),
            ).map_err(|e| StoreError::Crdt(e))?;

            // Set parent_id
            if let Some(parent_id) = node.parent_id {
                store_doc.set_parent_id(node.id, Some(parent_id))
                    .map_err(|e| StoreError::Crdt(e))?;
            }

            // Set tags and custom fields
            if !node.metadata.tags.is_empty() {
                store_doc.set_tags(node.id, &node.metadata.tags)
                    .map_err(|e| StoreError::Crdt(e))?;
            }
            for (key, val) in &node.metadata.custom {
                store_doc.set_custom(node.id, key, val)
                    .map_err(|e| StoreError::Crdt(e))?;
            }
        }

        // Rebuild children lists from parent_id relationships
        for node in &nodes {
            if let Some(parent_id) = node.parent_id {
                if store_doc.has_node(parent_id) {
                    store_doc.append_child(parent_id, node.id)
                        .map_err(|e| StoreError::Crdt(e))?;
                }
            }
        }

        // Remove old .json files after successful migration
        let mut entries = fs::read_dir(&nodes_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            if entry_path.extension().is_some_and(|e| e == "json") {
                fs::remove_file(&entry_path).await?;
            }
        }

        Ok(store_doc)
    }

    /// Get the store manifest
    pub fn manifest(&self) -> &StoreManifest {
        &self.manifest
    }

    /// Get the root node ID
    pub fn root_node_id(&self) -> NodeId {
        self.manifest.root_node_id
    }

    /// Get a reference to the store document
    pub fn store_document(&self) -> &StoreDocument {
        &self.store_doc
    }

    /// Get a mutable reference to the store document
    pub fn store_document_mut(&mut self) -> &mut StoreDocument {
        self.store_doc_dirty = true;
        &mut self.store_doc
    }

    /// Assemble a Node from store document metadata + content bytes
    fn assemble_node(&mut self, node_id: NodeId) -> Result<Node> {
        let info = self.store_doc.get_node_info(node_id)
            .map_err(|e| StoreError::Crdt(e))?;
        let children = self.store_doc.get_children(node_id)
            .map_err(|e| StoreError::Crdt(e))?;

        let created_at = DateTime::parse_from_rfc3339(&info.created_at)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());
        let modified_at = DateTime::parse_from_rfc3339(&info.modified_at)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::Utc::now());

        let content = self.content_docs.get(&node_id)
            .map(|doc| doc.save())
            .unwrap_or_default();

        Ok(Node {
            id: node_id,
            parent_id: info.parent_id,
            node_type: info.node_type,
            metadata: NodeMetadata {
                title: info.title,
                created_at,
                modified_at,
                tags: info.tags,
                custom: info.custom,
            },
            content,
            children,
            links: Vec::new(),
        })
    }

    /// Get a node by ID. Loads (and migrates, if needed) its content from
    /// disk if not already cached.
    pub async fn get_node(&mut self, node_id: NodeId) -> Result<Node> {
        // Ensures the content doc is loaded (migrating legacy content if
        // necessary) and that the node exists.
        self.get_node_document(node_id).await?;

        self.assemble_node(node_id)
    }

    /// Create a new node
    pub async fn create_node(&mut self, node: Node, parent_id: Option<NodeId>) -> Result<NodeId> {
        let node_id = node.id;

        // Add to store document
        self.store_doc.add_node(
            node_id,
            parent_id,
            &node.node_type,
            &node.metadata.title,
        ).map_err(|e| StoreError::Crdt(e))?;

        // Set tags and custom fields if present
        if !node.metadata.tags.is_empty() {
            self.store_doc.set_tags(node_id, &node.metadata.tags)
                .map_err(|e| StoreError::Crdt(e))?;
        }
        for (key, val) in &node.metadata.custom {
            self.store_doc.set_custom(node_id, key, val)
                .map_err(|e| StoreError::Crdt(e))?;
        }

        self.store_doc_dirty = true;

        // Store content if present
        if !node.content.is_empty() {
            let doc = ContentDoc::load(&node.content).map_err(StoreError::from)?;
            self.content_docs.insert(node_id, doc);
            self.dirty_content.insert(node_id);
        }

        debug!("Created node {} in store {}", node_id, self.id);
        Ok(node_id)
    }

    /// Delete a node and its content file
    pub async fn delete_node(&mut self, node_id: NodeId) -> Result<()> {
        // Remove from store document (handles parent's children list)
        self.store_doc.remove_node(node_id)
            .map_err(|e| StoreError::Crdt(e))?;
        self.store_doc_dirty = true;

        // Remove content file(s)
        let content_path = self.node_content_path(node_id);
        if content_path.exists() {
            fs::remove_file(&content_path).await?;
        }
        let legacy_path = self.legacy_content_path(node_id);
        if legacy_path.exists() {
            fs::remove_file(&legacy_path).await?;
        }

        // Remove from caches
        self.content_docs.remove(&node_id);
        self.dirty_content.remove(&node_id);

        debug!("Deleted node {} from store {}", node_id, self.id);
        Ok(())
    }

    /// Move a node to a new parent, optionally at a specific position
    pub async fn move_node(&mut self, node_id: NodeId, new_parent_id: NodeId, position: Option<usize>) -> Result<()> {
        // Prevent moving node into itself
        if node_id == new_parent_id {
            return Err(StoreError::InvalidOperation("Cannot move node into itself".into()));
        }

        // Prevent cycles: walk up from new_parent to root
        {
            let mut cursor = new_parent_id;
            loop {
                let info = self.store_doc.get_node_info(cursor)
                    .map_err(|e| StoreError::Crdt(e))?;
                match info.parent_id {
                    None => break,
                    Some(pid) => {
                        if pid == node_id {
                            return Err(StoreError::InvalidOperation(
                                "Cannot move a node into one of its own descendants".into(),
                            ));
                        }
                        cursor = pid;
                    }
                }
            }
        }

        self.store_doc.move_node(node_id, new_parent_id, position)
            .map_err(|e| StoreError::Crdt(e))?;
        self.store_doc_dirty = true;

        Ok(())
    }

    /// Replace a node's content with a full yrs snapshot.
    pub async fn update_node_content(&mut self, node_id: NodeId, content: Vec<u8>) -> Result<()> {
        if !self.store_doc.has_node(node_id) {
            return Err(StoreError::NodeNotFound(node_id));
        }
        let doc = ContentDoc::load(&content).map_err(StoreError::from)?;
        self.content_docs.insert(node_id, doc);
        self.dirty_content.insert(node_id);
        // Touch modified_at in store doc
        self.store_doc.touch_modified(node_id)
            .map_err(|e| StoreError::Crdt(e))?;
        self.store_doc_dirty = true;
        Ok(())
    }

    /// Merge a yrs update (delta, reconciliation diff, or whole snapshot)
    /// into a node's content document.
    pub async fn apply_content_update(&mut self, node_id: NodeId, update: &[u8]) -> Result<()> {
        {
            let doc = self.get_node_document(node_id).await?;
            doc.apply_update(update).map_err(StoreError::from)?;
        }
        self.dirty_content.insert(node_id);
        self.store_doc.touch_modified(node_id)
            .map_err(|e| StoreError::Crdt(e))?;
        self.store_doc_dirty = true;
        Ok(())
    }

    /// Get a node's persistent CRDT content document (loaded on demand, kept
    /// in memory). If no `.yrs` file exists yet but a legacy `.automerge`
    /// content file does, its text is extracted best-effort and used to seed
    /// a new yrs document, which is then written to `.yrs`. The legacy file
    /// is left untouched.
    pub async fn get_node_document(&mut self, node_id: NodeId) -> Result<&mut ContentDoc> {
        if !self.store_doc.has_node(node_id) {
            return Err(StoreError::NodeNotFound(node_id));
        }
        if !self.content_docs.contains_key(&node_id) {
            let content_path = self.node_content_path(node_id);
            let doc = if content_path.exists() {
                let bytes = fs::read(&content_path).await?;
                ContentDoc::load(&bytes).map_err(StoreError::from)?
            } else {
                let legacy_path = self.legacy_content_path(node_id);
                if legacy_path.exists() {
                    let legacy_bytes = fs::read(&legacy_path).await?;
                    let text = legacy::extract_text(&legacy_bytes);
                    let doc = ContentDoc::from_plain_text(&text).map_err(StoreError::from)?;
                    let yrs_bytes = doc.save();
                    atomic_write(&content_path, &yrs_bytes).await?;
                    info!(
                        "Migrated legacy Automerge content for node {} to yrs ({} chars)",
                        node_id,
                        text.len()
                    );
                    doc
                } else {
                    ContentDoc::new()
                }
            };
            self.content_docs.insert(node_id, doc);
        }
        Ok(self.content_docs.get_mut(&node_id).unwrap())
    }

    /// Save a node's CRDT document (replaces the persistent instance)
    pub async fn save_node_document(&mut self, node_id: NodeId, doc: &mut ContentDoc) -> Result<()> {
        let content = doc.save();
        self.update_node_content(node_id, content).await
    }

    /// Mark a node's content as dirty (needs flushing to disk)
    pub fn mark_content_dirty(&mut self, node_id: NodeId) {
        self.dirty_content.insert(node_id);
    }

    /// Update node metadata in the store document
    pub async fn update_node_metadata(&mut self, node_id: NodeId, metadata: &NodeMetadata) -> Result<()> {
        self.store_doc.set_title(node_id, &metadata.title)
            .map_err(|e| StoreError::Crdt(e))?;
        self.store_doc.set_tags(node_id, &metadata.tags)
            .map_err(|e| StoreError::Crdt(e))?;
        for (key, val) in &metadata.custom {
            self.store_doc.set_custom(node_id, key, val)
                .map_err(|e| StoreError::Crdt(e))?;
        }
        self.store_doc_dirty = true;
        Ok(())
    }

    /// Flush all changes to disk
    pub async fn flush(&mut self) -> Result<()> {
        // Save store document if dirty
        if self.store_doc_dirty {
            let bytes = self.store_doc.save();
            atomic_write(&self.path.join(Self::STORE_DOC_FILE), &bytes).await?;
            self.store_doc_dirty = false;
        }

        // Save dirty content docs
        let dirty: Vec<NodeId> = self.dirty_content.iter().copied().collect();
        for node_id in dirty {
            if let Some(doc) = self.content_docs.get(&node_id) {
                let bytes = doc.save();
                if !bytes.is_empty() {
                    let content_path = self.node_content_path(node_id);
                    atomic_write(&content_path, &bytes).await?;
                }
            }
        }
        self.dirty_content.clear();

        // Update manifest modified time
        self.manifest.modified_at = chrono::Utc::now();
        let manifest_json = serde_json::to_string_pretty(&self.manifest)?;
        atomic_write(&self.path.join(Self::MANIFEST_FILE), manifest_json).await?;

        debug!("Flushed store {} to disk", self.id);
        Ok(())
    }

    /// List all node IDs in the store
    pub fn list_node_ids(&self) -> Result<Vec<NodeId>> {
        self.store_doc.list_node_ids().map_err(|e| StoreError::Crdt(e))
    }

    /// Get children of a node
    pub async fn get_children(&mut self, node_id: NodeId) -> Result<Vec<Node>> {
        let children_ids = self.store_doc.get_children(node_id)
            .map_err(|e| StoreError::Crdt(e))?;

        let mut children = Vec::with_capacity(children_ids.len());
        for child_id in children_ids {
            let child = self.get_node(child_id).await?;
            children.push(child);
        }

        Ok(children)
    }

    // Private helpers

    fn node_content_path(&self, node_id: NodeId) -> PathBuf {
        self.path.join(Self::NODES_DIR).join(format!("{}.yrs", node_id))
    }

    /// Path to a node's legacy (pre-restart) Automerge content file, if any.
    fn legacy_content_path(&self, node_id: NodeId) -> PathBuf {
        self.path.join(Self::NODES_DIR).join(format!("{}.automerge", node_id))
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
        assert!(store_path.join("store.automerge").exists());
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

    #[tokio::test]
    async fn test_delete_node() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let root_id = store.root_node_id();

        let doc = Node::document("To Delete");
        let doc_id = store.create_node(doc, Some(root_id)).await.unwrap();
        store.delete_node(doc_id).await.unwrap();

        assert!(store.get_node(doc_id).await.is_err());
        let root = store.get_node(root_id).await.unwrap();
        assert!(!root.children.contains(&doc_id));
    }

    #[tokio::test]
    async fn test_move_node() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let root_id = store.root_node_id();

        let folder_a = Node::folder("A");
        let folder_a_id = store.create_node(folder_a, Some(root_id)).await.unwrap();
        let folder_b = Node::folder("B");
        let folder_b_id = store.create_node(folder_b, Some(root_id)).await.unwrap();
        let doc = Node::document("Doc");
        let doc_id = store.create_node(doc, Some(folder_a_id)).await.unwrap();

        store.move_node(doc_id, folder_b_id, None).await.unwrap();

        let a = store.get_node(folder_a_id).await.unwrap();
        assert!(!a.children.contains(&doc_id));
        let b = store.get_node(folder_b_id).await.unwrap();
        assert!(b.children.contains(&doc_id));
        let moved_doc = store.get_node(doc_id).await.unwrap();
        assert_eq!(moved_doc.parent_id, Some(folder_b_id));
    }

    #[tokio::test]
    async fn test_persist_and_reopen() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let doc_id;
        let store_id;
        {
            let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
            store_id = store.id;
            let root_id = store.root_node_id();

            let doc = Node::document("Persistent Doc");
            doc_id = store.create_node(doc, Some(root_id)).await.unwrap();
            store.flush().await.unwrap();
        }

        let mut store = LocalStore::open(&store_path).await.unwrap();
        assert_eq!(store.id, store_id);
        let node = store.get_node(doc_id).await.unwrap();
        assert_eq!(node.metadata.title, "Persistent Doc");
    }

    #[tokio::test]
    async fn test_v1_migration() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        // Create a v1-style store manually
        fs::create_dir_all(&store_path).await.unwrap();
        fs::create_dir(store_path.join("nodes")).await.unwrap();
        fs::create_dir(store_path.join("assets")).await.unwrap();
        fs::create_dir(store_path.join("index")).await.unwrap();

        let root_id = NodeId::new();
        let child_id = NodeId::new();

        // Write v1 manifest
        let manifest = StoreManifest {
            version: 1,
            id: StoreId::new(),
            name: "Legacy Store".into(),
            root_node_id: root_id,
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
        };
        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        fs::write(store_path.join("manifest.json"), manifest_json).await.unwrap();

        // Write v1 node JSON files
        let root_node = Node {
            id: root_id,
            parent_id: None,
            node_type: "folder".into(),
            metadata: NodeMetadata {
                title: "Legacy Store".into(),
                created_at: chrono::Utc::now(),
                modified_at: chrono::Utc::now(),
                tags: Vec::new(),
                custom: std::collections::HashMap::new(),
            },
            content: Vec::new(),
            children: vec![child_id],
            links: Vec::new(),
        };
        let child_node = Node {
            id: child_id,
            parent_id: Some(root_id),
            node_type: "document".into(),
            metadata: NodeMetadata {
                title: "Legacy Doc".into(),
                created_at: chrono::Utc::now(),
                modified_at: chrono::Utc::now(),
                tags: Vec::new(),
                custom: std::collections::HashMap::new(),
            },
            content: Vec::new(),
            children: Vec::new(),
            links: Vec::new(),
        };

        let root_json = serde_json::to_string_pretty(&root_node).unwrap();
        fs::write(store_path.join("nodes").join(format!("{}.json", root_id)), root_json).await.unwrap();
        let child_json = serde_json::to_string_pretty(&child_node).unwrap();
        fs::write(store_path.join("nodes").join(format!("{}.json", child_id)), child_json).await.unwrap();

        // Open should trigger migration
        let mut store = LocalStore::open(&store_path).await.unwrap();

        // Verify v2 format
        assert!(store_path.join("store.automerge").exists());

        // Verify tree is intact
        let root = store.get_node(root_id).await.unwrap();
        assert_eq!(root.metadata.title, "Legacy Store");
        assert!(root.children.contains(&child_id));

        let child = store.get_node(child_id).await.unwrap();
        assert_eq!(child.metadata.title, "Legacy Doc");
        assert_eq!(child.parent_id, Some(root_id));

        // Verify .json files are removed
        assert!(!store_path.join("nodes").join(format!("{}.json", root_id)).exists());
    }

    #[tokio::test]
    async fn test_list_node_ids() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let root_id = store.root_node_id();

        let doc1 = Node::document("Doc 1");
        let doc1_id = store.create_node(doc1, Some(root_id)).await.unwrap();
        let doc2 = Node::document("Doc 2");
        let doc2_id = store.create_node(doc2, Some(root_id)).await.unwrap();

        let ids = store.list_node_ids().unwrap();
        assert_eq!(ids.len(), 3); // root + 2 docs
        assert!(ids.contains(&root_id));
        assert!(ids.contains(&doc1_id));
        assert!(ids.contains(&doc2_id));
    }

    #[tokio::test]
    async fn test_content_update_apply_and_reopen() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let doc_id;
        {
            let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
            let root_id = store.root_node_id();

            let node = Node::document("Content Doc");
            doc_id = store.create_node(node, Some(root_id)).await.unwrap();

            // Seed the node with a full snapshot.
            let base = ContentDoc::from_plain_text("hi").unwrap();
            store.update_node_content(doc_id, base.save()).await.unwrap();

            // Merge in a diff produced by an independent second document that
            // has more text than the server has state for.
            let richer = ContentDoc::from_plain_text("hi\nthere").unwrap();
            let base_sv = base.state_vector();
            let diff = richer.diff_since(&base_sv).unwrap();
            store.apply_content_update(doc_id, &diff).await.unwrap();

            store.flush().await.unwrap();
        }

        // Reopen and verify the merged content survived a round trip through
        // disk.
        let mut store = LocalStore::open(&store_path).await.unwrap();
        let node = store.get_node(doc_id).await.unwrap();
        let doc = ContentDoc::load(&node.content).unwrap();
        let text = doc.text();
        assert!(text.contains("hi"), "expected merged text to contain \"hi\", got {:?}", text);
        assert!(text.contains("there"), "expected merged text to contain \"there\", got {:?}", text);
    }

    #[tokio::test]
    async fn test_legacy_automerge_content_migration() {
        use automerge::transaction::Transactable;
        use automerge::{AutoCommit, ObjType};

        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let root_id = store.root_node_id();

        let node = Node::document("Legacy Doc");
        let doc_id = store.create_node(node, Some(root_id)).await.unwrap();
        store.flush().await.unwrap();

        // Write a legacy Automerge content file directly, bypassing the yrs
        // path entirely (simulating a pre-restart store).
        let mut legacy_doc = AutoCommit::new();
        let text_id = legacy_doc
            .put_object(automerge::ROOT, "text", ObjType::Text)
            .unwrap();
        legacy_doc.splice_text(&text_id, 0, 0, "Legacy content here").unwrap();
        let legacy_bytes = legacy_doc.save();

        let legacy_path = store_path.join("nodes").join(format!("{}.automerge", doc_id));
        fs::write(&legacy_path, &legacy_bytes).await.unwrap();

        let content_doc = store.get_node_document(doc_id).await.unwrap();
        assert_eq!(content_doc.text(), "Legacy content here");

        let yrs_path = store_path.join("nodes").join(format!("{}.yrs", doc_id));
        assert!(yrs_path.exists(), "migration should have written a .yrs file");
        assert!(legacy_path.exists(), "legacy .automerge file should be left untouched");
    }
}
