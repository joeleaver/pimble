//! Local file-based store implementation

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use pimble_core::{Node, NodeId, NodeMetadata, RemoteEndpoint, StoreId, StoreManifest};
use pimble_crdt::{ContentDoc, StoreDocument};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, info};

use crate::error::{Result, StoreError};

/// A store's replica sync link, persisted as `<store>/sync.json`
/// (docs/SYNC_CONTRACT.md decision 7). Present exactly when the store is
/// linked to a remote twin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub remote: RemoteEndpoint,
    /// When the link last reached `Synced` (docs/history/REMOTE_MOUNTS_CONTRACT.md
    /// decision 4), so a mount sourced from this store can report
    /// `Cached { last_sync }` after a restart with the remote down. Written
    /// by the sync link on category transitions; `None` until it first syncs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<DateTime<Utc>>,
}

/// Write a file atomically: write to a `.tmp` sibling, then rename into place.
/// This prevents empty/corrupt files if the process is killed mid-write.
async fn atomic_write(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, data).await?;
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

/// Peek a store directory's `manifest.json` to learn its
/// [`pimble_core::StoreKind`] without committing to opening it as a
/// [`LocalStore`] (only ever `Plain`) or a `Vault` store
/// (docs/CRYPTO_CONTRACT.md) — `StoreManager::open_local_store` reads this
/// first to decide which one to construct.
pub async fn peek_manifest_kind(path: impl AsRef<Path>) -> Result<pimble_core::StoreKind> {
    let manifest_path = path.as_ref().join(LocalStore::MANIFEST_FILE);
    let manifest_json = fs::read_to_string(&manifest_path)
        .await
        .map_err(|_| StoreError::InvalidPath(format!("No manifest found at {}", manifest_path.display())))?;
    let manifest: StoreManifest = serde_json::from_str(&manifest_json)?;
    Ok(manifest.kind)
}

/// A local store backed by the filesystem
///
/// Directory structure:
/// ```text
/// store.pimble/
/// ├── manifest.json     # Store metadata (version: 3)
/// ├── store.yrs         # Tree structure + node metadata (CRDT, yrs)
/// ├── nodes/
/// │   ├── {node-id}.yrs # Per-node content documents (yrs)
/// │   └── ...
/// ├── assets/           # Binary files
/// │   └── {hash}.{ext}
/// └── index/            # Search indexes (future)
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
    const STORE_DOC_FILE: &'static str = "store.yrs";
    const SYNC_CONFIG_FILE: &'static str = "sync.json";
    /// The only supported store format version.
    const STORE_MANIFEST_VERSION: u32 = 3;

    /// Create a new local store at the given path, with a freshly generated id.
    pub async fn create(path: impl AsRef<Path>, name: impl Into<String>) -> Result<Self> {
        Self::create_impl(path, name, None).await
    }

    /// Like [`LocalStore::create`], but under a chosen `id` rather than a
    /// freshly generated one (docs/CRYPTO_CONTRACT.md: `createStore`'s
    /// `store_id`, letting the accounts service create the hosted twin of a
    /// local store under the local store's own id). Refusing an id already
    /// open is the caller's job (`StoreManager`), same as for a fresh id.
    pub async fn create_with_id(path: impl AsRef<Path>, name: impl Into<String>, id: StoreId) -> Result<Self> {
        Self::create_impl(path, name, Some(id)).await
    }

    async fn create_impl(path: impl AsRef<Path>, name: impl Into<String>, id: Option<StoreId>) -> Result<Self> {
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

        // Create manifest
        let mut manifest = StoreManifest::new(&name, root_node_id);
        manifest.version = Self::STORE_MANIFEST_VERSION;
        if let Some(id) = id {
            manifest.id = id;
        }

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

    /// Create a local replica of a store held by a remote server
    /// (docs/SYNC_CONTRACT.md decision 8): same directory layout as
    /// [`LocalStore::create`], but with the given `id`/`root_node_id`
    /// (matching the remote's) and an **empty** store document
    /// (`StoreDocument::load(&[])`) rather than one with its own freshly
    /// generated root. The first reconcile with the remote populates it.
    ///
    /// `StoreDocument::new` must never run for a replica: two independently
    /// created roots for the same store id would merge into duplicated
    /// children once the replica's tree and the remote's are reconciled.
    pub async fn create_replica(
        path: impl AsRef<Path>,
        id: StoreId,
        name: impl Into<String>,
        root_node_id: NodeId,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let name = name.into();

        if path.exists() {
            return Err(StoreError::StoreExists(path.display().to_string()));
        }

        fs::create_dir_all(&path).await?;
        fs::create_dir(path.join(Self::NODES_DIR)).await?;
        fs::create_dir(path.join(Self::ASSETS_DIR)).await?;
        fs::create_dir(path.join(Self::INDEX_DIR)).await?;

        let now = chrono::Utc::now();
        let manifest = StoreManifest {
            version: Self::STORE_MANIFEST_VERSION,
            id,
            name: name.clone(),
            root_node_id,
            created_at: now,
            modified_at: now,
            kind: pimble_core::StoreKind::Plain,
        };

        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(path.join(Self::MANIFEST_FILE), manifest_json).await?;

        let store_doc = StoreDocument::load(&[]).map_err(StoreError::from)?;

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

        info!("Created replica store '{}' ({}) at {:?}", name, id, store.path);
        Ok(store)
    }

    /// Read `<store>/sync.json`, if present.
    pub async fn read_sync_config(&self) -> Result<Option<SyncConfig>> {
        let path = self.path.join(Self::SYNC_CONFIG_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let json = fs::read_to_string(&path).await?;
        let config: SyncConfig = serde_json::from_str(&json)?;
        Ok(Some(config))
    }

    /// Write `<store>/sync.json`, replacing any existing content.
    pub async fn write_sync_config(&self, config: &SyncConfig) -> Result<()> {
        let json = serde_json::to_string_pretty(config)?;
        atomic_write(&self.path.join(Self::SYNC_CONFIG_FILE), json).await?;
        Ok(())
    }

    /// Delete `<store>/sync.json` if present (unlinking the store).
    pub async fn clear_sync_config(&self) -> Result<()> {
        let path = self.path.join(Self::SYNC_CONFIG_FILE);
        if path.exists() {
            fs::remove_file(&path).await?;
        }
        Ok(())
    }

    /// Open an existing local store. Only the current format (manifest
    /// version 3, `store.yrs` present) is supported; anything older must be
    /// re-imported.
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

        let store_doc_path = path.join(Self::STORE_DOC_FILE);
        if manifest.version != Self::STORE_MANIFEST_VERSION || !store_doc_path.exists() {
            return Err(StoreError::UnsupportedFormat {
                path,
                version: manifest.version,
            });
        }

        let bytes = fs::read(&store_doc_path).await?;
        let store_doc = StoreDocument::load(&bytes).map_err(StoreError::from)?;

        let store = Self {
            id: manifest.id,
            path,
            manifest,
            store_doc,
            store_doc_dirty: false,
            content_docs: HashMap::new(),
            dirty_content: std::collections::HashSet::new(),
        };

        info!("Opened local store '{}' from {:?}", store.manifest.name, store.path);
        Ok(store)
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

    /// v1-encoded state vector for the store document.
    pub fn store_doc_state_vector(&self) -> Vec<u8> {
        self.store_doc.state_vector()
    }

    /// Everything the store document has that a peer at `state_vector`
    /// (v1-encoded) lacks.
    pub fn store_doc_diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>> {
        self.store_doc.diff_since(state_vector).map_err(StoreError::from)
    }

    /// Merge a peer's yrs update (delta, reconciliation diff, or whole
    /// snapshot) into the store document. A no-op merge (decision 8 of
    /// docs/history/HARDENING_CONTRACT.md — every part of `update` already reflected
    /// here) leaves the store untouched: not marked dirty, not
    /// re-validated. A real change marks it dirty and re-validates the tree
    /// (concurrent moves can leave duplicates or stray entries; issues are
    /// logged, not repaired here — `StoreManager::repair_tree` does that).
    /// Returns whether it changed anything and the ids of the node entries
    /// it touched (created, deleted, or modified), for the caller to feed a
    /// search index (docs/SYNC_CONTRACT.md decision 9).
    pub fn apply_store_doc_update(&mut self, update: &[u8]) -> Result<pimble_crdt::StoreUpdateEffect> {
        let effect = self.store_doc.apply_update(update).map_err(StoreError::from)?;
        if !effect.changed {
            return Ok(effect);
        }
        self.store_doc_dirty = true;
        match self.store_doc.validate_tree() {
            Ok(issues) => {
                for issue in &issues {
                    tracing::warn!("Tree issue after applying store update in store {}: {:?}", self.id, issue);
                }
            }
            Err(e) => tracing::warn!("Failed to validate tree after applying store update: {}", e),
        }
        Ok(effect)
    }

    /// Repair the store document's tree (see `StoreDocument::repair`),
    /// marking the store dirty only when it actually changed something
    /// (docs/history/HARDENING_CONTRACT.md decision 9). Unlike `store_document_mut`
    /// — which every caller reaches for because it is *about* to mutate —
    /// a repair often finds nothing to fix, and that must never force an
    /// extra flush.
    pub fn repair_tree(&mut self) -> Result<Option<pimble_crdt::TreeRepair>> {
        let repair = self.store_doc.repair().map_err(StoreError::from)?;
        if repair.is_some() {
            self.store_doc_dirty = true;
        }
        Ok(repair)
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

    /// Get a node by ID. Loads its content from disk if not already cached.
    pub async fn get_node(&mut self, node_id: NodeId) -> Result<Node> {
        // Ensures the content doc is loaded and that the node exists.
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

        // Remove content file
        let content_path = self.node_content_path(node_id);
        if content_path.exists() {
            fs::remove_file(&content_path).await?;
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
    /// into a node's content document. A no-op merge (decision 8 of
    /// docs/history/HARDENING_CONTRACT.md) leaves `modified_at` and every dirty flag
    /// untouched; returns whether it changed anything.
    pub async fn apply_content_update(&mut self, node_id: NodeId, update: &[u8]) -> Result<bool> {
        let changed = {
            let doc = self.get_node_document(node_id).await?;
            doc.apply_update(update).map_err(StoreError::from)?
        };
        if !changed {
            return Ok(false);
        }
        self.dirty_content.insert(node_id);
        self.store_doc.touch_modified(node_id)
            .map_err(|e| StoreError::Crdt(e))?;
        self.store_doc_dirty = true;
        Ok(true)
    }

    /// Get a node's persistent CRDT content document (loaded on demand, kept
    /// in memory).
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
                ContentDoc::new()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_create_store() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let _store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        assert!(store_path.exists());
        assert!(store_path.join("manifest.json").exists());
        assert!(store_path.join("store.yrs").exists());
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
    async fn test_open_rejects_unsupported_format() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");
        fs::create_dir_all(&store_path).await.unwrap();

        // A manifest declaring an old version, with no store.yrs present.
        let manifest = StoreManifest {
            version: 2,
            id: StoreId::new(),
            name: "Old Store".into(),
            root_node_id: NodeId::new(),
            created_at: chrono::Utc::now(),
            modified_at: chrono::Utc::now(),
            kind: pimble_core::StoreKind::Plain,
        };
        fs::write(
            store_path.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        ).await.unwrap();

        match LocalStore::open(&store_path).await {
            Err(StoreError::UnsupportedFormat { version, .. }) => assert_eq!(version, 2),
            Err(other) => panic!("expected UnsupportedFormat, got {:?}", other),
            Ok(_) => panic!("expected an error opening an unsupported-format store"),
        }
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
    async fn test_reopen_preserves_full_tree() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let root_id;
        let folder_id;
        let doc_id;
        {
            let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
            root_id = store.root_node_id();

            let folder = Node::folder("Folder");
            folder_id = store.create_node(folder, Some(root_id)).await.unwrap();

            let mut doc = Node::document("Doc");
            doc.metadata.tags = vec!["x".into(), "y".into()];
            doc.metadata.custom.insert("explicit_title".into(), serde_json::json!(true));
            doc_id = store.create_node(doc, Some(folder_id)).await.unwrap();

            store.flush().await.unwrap();
        }

        let mut store = LocalStore::open(&store_path).await.unwrap();

        let ids = store.list_node_ids().unwrap();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&root_id) && ids.contains(&folder_id) && ids.contains(&doc_id));

        let root = store.get_node(root_id).await.unwrap();
        assert_eq!(root.children, vec![folder_id]);

        let folder = store.get_node(folder_id).await.unwrap();
        assert_eq!(folder.children, vec![doc_id]);
        assert_eq!(folder.parent_id, Some(root_id));

        let doc = store.get_node(doc_id).await.unwrap();
        assert_eq!(doc.parent_id, Some(folder_id));
        assert_eq!(doc.metadata.title, "Doc");
        assert_eq!(doc.metadata.tags, vec!["x".to_string(), "y".to_string()]);
        assert_eq!(doc.metadata.custom.get("explicit_title"), Some(&serde_json::json!(true)));
    }
}
