//! Local file-based store: a directory of node documents
//! (docs/NODE_DOCUMENT_CONTRACT.md section 3). The tree is not a document of
//! its own any more; it is read out of the node documents by
//! [`pimble_crdt::Tree`], and every edit to it is an edit of one or more of
//! them, which is what lets a shared subtree be the same documents for
//! everyone who holds it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use pimble_core::{Node, NodeId, NodeMetadata, RemoteEndpoint, StoreId, StoreManifest};
use pimble_crdt::{NodeDoc, NodeUpdateEffect, StoreDocument, Tree, TreeEdit};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, info, warn};

use crate::error::{Result, StoreError};

/// Which kind of link `<store>/sync.json` describes (docs/CRYPTO_CONTRACT.md
/// "Pimble server, the desktop side"): `Sync` is the plain replica link
/// (`crate` re-exports nothing here — the running link is
/// `pimble_server::sync_link::SyncLink`); `Vault` links a `Plain` local
/// store to a hosted twin of kind `Vault` under the same id, kept in sync by
/// `pimble_server::vault_link::VaultLink` instead. Missing in an older
/// `sync.json`: `Sync`, so an existing replica link still parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    #[default]
    Sync,
    Vault,
}

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
    /// `Sync` (default, so an older `sync.json` still parses) or `Vault`
    /// (docs/CRYPTO_CONTRACT.md).
    #[serde(default)]
    pub mode: SyncMode,
    /// Per-document last-applied vault sequence number
    /// (`pimble_rpc::VaultDocId::as_str()` -> seq), meaningful only when
    /// `mode` is `Vault`: lets a restarted `VaultLink` resume `vaultFetch`
    /// from where it left off instead of re-fetching (and re-decrypting)
    /// the whole log. Always empty for `Sync` mode.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub last_seq: HashMap<String, u64>,
    /// The store key id a `Vault`-mode link encrypts outgoing blobs with
    /// (looked up in the keystore fresh on every connect, never cached here
    /// — only the id is persisted). `None` for `Sync` mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_key_id: Option<uuid::Uuid>,
}

/// What a deletion tombstoned (docs/NODE_DOCUMENT_CONTRACT.md section 2).
#[derive(Debug, Clone)]
pub struct NodeRemoval {
    /// The parent the deleted node was removed from.
    pub parent_id: NodeId,
    /// The deleted node and every descendant of it, in preorder. Each is a
    /// tombstone now: its document stays on disk and in memory, is no longer
    /// a node, and can be brought back by `undelete_node`.
    pub removed: Vec<NodeId>,
}

/// Write a file atomically: write to a `.tmp` sibling, then rename into place.
/// This prevents empty/corrupt files if the process is killed mid-write.
async fn atomic_write(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, data).await?;
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

/// The timestamp every edit made here carries: the crdt crate never reads a
/// clock, so what this store writes is decided here alone.
fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Append `more`'s touched documents to `edit`, so an operation made of
/// several tree edits reports them as one.
fn extend(edit: &mut TreeEdit, more: TreeEdit) {
    edit.touched.extend(more.touched);
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
/// ├── manifest.json       # Store metadata (version: 4): name, id, root node id, kind
/// ├── nodes/
/// │   ├── {node-id}.yrs   # The node document: content, tree place, metadata (yrs)
/// │   └── ...
/// ├── store.yrs.migrated  # The previous layout's tree document, retired by
/// │                       # the migration and never read again (absent in a
/// │                       # store created under this layout)
/// ├── assets/             # Binary files
/// │   └── {hash}.{ext}
/// ├── index/              # Search index (derived, disposable)
/// ├── sync.json           # Replica sync link, when linked
/// └── vault-link.json     # What the hosted twin is known to hold, when vault-linked
/// ```
pub struct LocalStore {
    /// Store ID
    pub id: StoreId,

    /// Path to the store directory
    pub path: PathBuf,

    /// Store manifest
    manifest: StoreManifest,

    /// Every node document of the store, tombstones included, loaded at open
    /// and held for the life of the store. That is one yrs document per node
    /// ever created, resident at once, with its whole edit history (a yrs
    /// document keeps its tombstoned structs), and it grows with the store:
    /// the memory cost of this layout, where the previous one held the tree
    /// in one document and loaded content on demand. Nothing is loaded on
    /// demand here because no document says which nodes exist without
    /// reading them all, and because sync names every held document
    /// (`doc_ids`) and a peer's update can land on any of them. A purge of
    /// old tombstones (docs/NODE_DOCUMENT_CONTRACT.md section 2) is the
    /// housekeeping that bounds it later.
    tree: Tree,

    /// Documents with changes not yet written to `nodes/{id}.yrs`.
    dirty: HashSet<NodeId>,
}

impl LocalStore {
    /// Subdirectory names
    const NODES_DIR: &'static str = "nodes";
    const ASSETS_DIR: &'static str = "assets";
    const INDEX_DIR: &'static str = "index";
    const MANIFEST_FILE: &'static str = "manifest.json";
    const SYNC_CONFIG_FILE: &'static str = "sync.json";
    /// The previous layout's tree document. Present exactly in a store the
    /// migration has not run on yet; retired under the name below once it
    /// has (docs/NODE_DOCUMENT_CONTRACT.md section 3).
    const STORE_DOC_FILE: &'static str = "store.yrs";
    const MIGRATED_STORE_DOC_FILE: &'static str = "store.yrs.migrated";
    /// The manifest version of the previous layout, the one `open` migrates.
    const PREVIOUS_MANIFEST_VERSION: u32 = 3;

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
        if let Some(id) = id {
            manifest.id = id;
        }

        // Write manifest
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(path.join(Self::MANIFEST_FILE), manifest_json).await?;

        // The root folder is the store's first document.
        let tree = Tree::new(root_node_id, &name, &now()).map_err(StoreError::from)?;

        let mut store = Self {
            id: manifest.id,
            path,
            manifest,
            tree,
            dirty: HashSet::from([root_node_id]),
        };

        store.flush().await?;

        info!("Created local store '{}' at {:?}", name, store.path);
        Ok(store)
    }

    /// Create a local replica of a store held by a remote server
    /// (docs/SYNC_CONTRACT.md decision 8): same directory layout as
    /// [`LocalStore::create`], but with the given `id`/`root_node_id`
    /// (matching the remote's) and **no documents at all**. The first
    /// reconcile with the remote brings every document over, the root's
    /// among them.
    ///
    /// The root document must never be created here: the peer's root
    /// document has the same id, and two documents initialised for one id
    /// merge by last-writer-wins per field, which can leave the root with
    /// this side's empty `tags` and `custom` instead of the peer's. Until the
    /// peer's root arrives the tree is empty (`get_node` on the root is
    /// `NodeNotFound`), and a replica reopened before its first reconcile
    /// stays that way: `open` creates nothing either.
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

        let now = Utc::now();
        let manifest = StoreManifest {
            version: StoreManifest::CURRENT_VERSION,
            id,
            name: name.clone(),
            root_node_id,
            created_at: now,
            modified_at: now,
            kind: pimble_core::StoreKind::Plain,
        };

        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(path.join(Self::MANIFEST_FILE), manifest_json).await?;

        let store = Self {
            id: manifest.id,
            path,
            manifest,
            tree: Tree::from_docs(root_node_id, HashMap::new()),
            dirty: HashSet::new(),
        };

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

    /// Open an existing local store: every node document is loaded, a store
    /// of the previous layout is migrated (see [`LocalStore::migrate`]), the
    /// manifest root is adopted from the documents when it names none of
    /// them, and the tree is repaired (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 2: repair runs at open). Manifest versions other than the
    /// previous and the current one are refused: nothing older is migrated.
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
        let mut manifest: StoreManifest = serde_json::from_str(&manifest_json)?;

        let previous_layout = manifest.version == Self::PREVIOUS_MANIFEST_VERSION;
        if !previous_layout && manifest.version != StoreManifest::CURRENT_VERSION {
            return Err(StoreError::UnsupportedFormat {
                path,
                version: manifest.version,
            });
        }

        let nodes_dir = path.join(Self::NODES_DIR);
        fs::create_dir_all(&nodes_dir).await?;
        let mut docs = Self::load_node_docs(&nodes_dir).await?;

        // The previous layout's tree document, when it is still there, is
        // written into the node documents before anything else happens.
        let store_doc_path = path.join(Self::STORE_DOC_FILE);
        let mut dirty = HashSet::new();
        if store_doc_path.exists() {
            let bytes = fs::read(&store_doc_path).await?;
            let store_doc = StoreDocument::load(&bytes).map_err(StoreError::from)?;
            dirty = Self::migrate(&store_doc, manifest.root_node_id, &mut docs)?;
            info!(
                "Store {}: migrated {} node(s) from {} into their node documents",
                manifest.id,
                dirty.len(),
                Self::STORE_DOC_FILE
            );
        }
        if previous_layout {
            manifest.version = StoreManifest::CURRENT_VERSION;
        }

        let mut store = Self {
            id: manifest.id,
            path,
            tree: Tree::from_docs(manifest.root_node_id, docs),
            manifest,
            dirty,
        };

        if store_doc_path.exists() {
            // Every migrated document reaches disk before the tree document
            // is retired: a crash in between re-runs the migration on the
            // next open, which then finds every document initialised, writes
            // nothing, and only renames.
            store.flush().await?;
            fs::rename(&store_doc_path, store.path.join(Self::MIGRATED_STORE_DOC_FILE)).await?;
        }

        // A manifest whose root names no node (a replica created around a
        // placeholder root, filled by a sync that never got to adopt it)
        // takes the root the documents agree on, so the tree starts
        // somewhere; the server's `adopt_document_root` does the same as
        // documents arrive.
        if !store.tree.has_node(store.manifest.root_node_id) {
            if let Some(root) = store.document_root() {
                info!("Store {}: manifest root {} replaced by the documents' root {}", store.id, store.manifest.root_node_id, root);
                store.set_root_node_id(root).await?;
            }
        }

        if let Some(edit) = store.tree.repair(&now()).map_err(StoreError::from)? {
            warn!("Store {}: tree repaired at open, {} document(s) touched", store.id, edit.touched.len());
            store.mark_edit_dirty(&edit);
        }
        if !store.dirty.is_empty() || previous_layout {
            store.flush().await?;
        }

        info!("Opened local store '{}' from {:?}", store.manifest.name, store.path);
        Ok(store)
    }

    /// Load every `nodes/{id}.yrs`. A file that is not a node document (a
    /// name that is not a node id, an unfinished `.tmp`, bytes yrs cannot
    /// read) is skipped with a warning rather than failing the open: the
    /// store still opens, and a peer that holds the document sends it whole
    /// on the next reconcile.
    async fn load_node_docs(nodes_dir: &Path) -> Result<HashMap<NodeId, NodeDoc>> {
        let mut docs = HashMap::new();
        let mut entries = fs::read_dir(nodes_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file = entry.path();
            if file.extension().and_then(|e| e.to_str()) != Some("yrs") {
                continue;
            }
            let Some(id) = file.file_stem().and_then(|s| s.to_str()).and_then(|s| NodeId::parse(s).ok()) else {
                warn!("Skipping {:?}: not named by a node id", file);
                continue;
            };
            let bytes = fs::read(&file).await?;
            match NodeDoc::load(&bytes) {
                Ok(doc) => {
                    docs.insert(id, doc);
                }
                Err(e) => warn!("Skipping node document {:?}: {}", file, e),
            }
        }
        Ok(docs)
    }

    /// The migration of docs/NODE_DOCUMENT_CONTRACT.md section 3: for every
    /// entry of the previous layout's tree document, write the `node` and
    /// `children` roots into that node's document in `docs` (its content
    /// document as loaded from disk, or a new empty one for a node that
    /// never had content): type, title, parent (none for the root),
    /// timestamps, tags, custom, and the children in the entry's order.
    /// Returns the ids written.
    ///
    /// Idempotent: a document that is already initialised is left alone,
    /// whether by an earlier run here or by a peer's migration that arrived
    /// through sync. Two replicas migrating the same store independently
    /// write the same values under different yrs client ids and converge
    /// (fields last-writer-wins to the same value, children lists doubled
    /// and then deduplicated by the first repair); the contract says why a
    /// fixed client id is the wrong fix for that.
    fn migrate(store_doc: &StoreDocument, manifest_root: NodeId, docs: &mut HashMap<NodeId, NodeDoc>) -> Result<HashSet<NodeId>> {
        let root = store_doc.root_node_id().unwrap_or(manifest_root);
        let mut written = HashSet::new();
        for id in store_doc.list_node_ids().map_err(StoreError::from)? {
            let doc = docs.entry(id).or_default();
            if doc.is_initialised() {
                continue;
            }
            let info = store_doc.get_node_info(id).map_err(StoreError::from)?;
            let parent = if id == root { None } else { info.parent_id };
            doc.init(&info.node_type, &info.title, parent, &info.created_at).map_err(StoreError::from)?;
            if !info.tags.is_empty() {
                doc.set_tags(&info.tags, &info.modified_at).map_err(StoreError::from)?;
            }
            // In key order: the order of writes never matters to the merge,
            // but a deterministic document is easier to compare in a test.
            let mut custom: Vec<_> = info.custom.iter().collect();
            custom.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in custom {
                doc.set_custom(key, value, &info.modified_at).map_err(StoreError::from)?;
            }
            for (at, child) in store_doc.get_children(id).map_err(StoreError::from)?.into_iter().enumerate() {
                doc.insert_child(at, child).map_err(StoreError::from)?;
            }
            // Last: the setters above stamp `modified_at`, and the entry's
            // own timestamps are what the node keeps.
            doc.set_timestamps(&info.created_at, &info.modified_at).map_err(StoreError::from)?;
            written.insert(id);
        }
        Ok(written)
    }

    /// Get the store manifest
    pub fn manifest(&self) -> &StoreManifest {
        &self.manifest
    }

    /// Get the root node ID
    pub fn root_node_id(&self) -> NodeId {
        self.manifest.root_node_id
    }

    /// Rewrite the manifest's root node id and re-root the tree on it. A
    /// replica created empty for a vault twin (`cloudAddHostedStore`) starts
    /// with a placeholder root because the real one is only known once the
    /// encrypted documents have been pulled; the vault link calls this when
    /// [`LocalStore::document_root`] disagrees with the manifest. The tree
    /// is not repaired here; the caller's next `repair_tree` settles what
    /// the new root changes.
    pub async fn set_root_node_id(&mut self, root_node_id: NodeId) -> Result<()> {
        if self.manifest.root_node_id == root_node_id {
            return Ok(());
        }
        self.manifest.root_node_id = root_node_id;
        self.manifest.modified_at = Utc::now();
        let manifest_json = serde_json::to_string_pretty(&self.manifest)?;
        atomic_write(&self.path.join(Self::MANIFEST_FILE), manifest_json).await?;

        // `Tree` fixes its root at construction; re-rooting is rebuilding it
        // over the same documents.
        let ids = self.tree.ids();
        let mut docs = HashMap::with_capacity(ids.len());
        for id in ids {
            if let Some(doc) = self.tree.take_doc(id) {
                docs.insert(id, doc);
            }
        }
        self.tree = Tree::from_docs(root_node_id, docs);
        Ok(())
    }

    /// The root the documents say the store has: the manifest's root when
    /// it is a node with no `parent_id`; otherwise the one node with no
    /// `parent_id`, when there is exactly one; otherwise `None` (no root
    /// has arrived yet, or several nodes claim to be it, which only a
    /// repair from a known root settles). What `set_root_node_id`'s callers
    /// compare the manifest against.
    pub fn document_root(&self) -> Option<NodeId> {
        let is_root_like = |id: NodeId| self.tree.get_node_info(id).is_ok_and(|info| info.parent_id.is_none());
        let manifest_root = self.manifest.root_node_id;
        if is_root_like(manifest_root) {
            return Some(manifest_root);
        }
        let mut candidates = self.tree.list_node_ids().into_iter().filter(|&id| is_root_like(id));
        match (candidates.next(), candidates.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

    // ── Documents ────────────────────────────────────────────────────────

    /// The tree over this store's documents.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The tree, to edit. Nothing edited through it is marked dirty: call
    /// [`LocalStore::mark_dirty`] for every id the resulting `TreeEdit`
    /// names, or nothing of it reaches disk.
    pub fn tree_mut(&mut self) -> &mut Tree {
        &mut self.tree
    }

    /// A held document (a node, a tombstone or one whose content arrived
    /// before its place), to edit: marked dirty at once, since a caller
    /// asking for `&mut` is about to change it. `NodeNotFound` when no
    /// document is held under `node_id`.
    pub fn node_doc(&mut self, node_id: NodeId) -> Result<&mut NodeDoc> {
        let doc = self.tree.doc_mut(node_id).ok_or(StoreError::NodeNotFound(node_id))?;
        self.dirty.insert(node_id);
        Ok(doc)
    }

    /// Merge a peer's update (a delta, a reconciliation diff, or a whole
    /// snapshot) into `node_id`'s document, creating the document when
    /// nothing is held under that id (so authorise first: an update for any
    /// id makes a document). A no-op merge (decision 8 of
    /// docs/history/HARDENING_CONTRACT.md — every part of `update` already
    /// reflected here) marks nothing dirty. Nothing is repaired or stamped
    /// here: the caller applies a batch, then runs `repair_tree` once when
    /// any effect reports `structure`.
    pub fn apply_node_update(&mut self, node_id: NodeId, update: &[u8]) -> Result<NodeUpdateEffect> {
        let effect = self.tree.apply_update(node_id, update).map_err(StoreError::from)?;
        if effect.changed {
            self.dirty.insert(node_id);
        }
        Ok(effect)
    }

    /// v1-encoded state vector of `node_id`'s document. `NodeNotFound` when
    /// no document is held under the id (a tombstone is held).
    pub fn node_state_vector(&self, node_id: NodeId) -> Result<Vec<u8>> {
        let doc = self.tree.doc(node_id).ok_or(StoreError::NodeNotFound(node_id))?;
        Ok(doc.state_vector())
    }

    /// Everything `node_id`'s document has that a peer at `state_vector`
    /// (v1-encoded) lacks. `NodeNotFound` as for `node_state_vector`.
    pub fn node_diff_since(&self, node_id: NodeId, state_vector: &[u8]) -> Result<Vec<u8>> {
        let doc = self.tree.doc(node_id).ok_or(StoreError::NodeNotFound(node_id))?;
        doc.diff_since(state_vector).map_err(StoreError::from)
    }

    /// Every held document's id, tombstones and content-only documents
    /// included: what sync names.
    pub fn doc_ids(&self) -> Vec<NodeId> {
        self.tree.ids()
    }

    /// Every undeleted node's id.
    pub fn list_node_ids(&self) -> Vec<NodeId> {
        self.tree.list_node_ids()
    }

    /// Mark `node_id`'s document as needing a flush (an edit made through
    /// `tree_mut`).
    pub fn mark_dirty(&mut self, node_id: NodeId) {
        self.dirty.insert(node_id);
    }

    fn mark_edit_dirty(&mut self, edit: &TreeEdit) {
        self.dirty.extend(edit.node_ids());
    }

    // ── Nodes ────────────────────────────────────────────────────────────

    /// Assemble a Node from the tree's view of `node_id` and its document's
    /// bytes. The content is the whole node document (the editor joins it
    /// as it joined a content document; Pimble's roots ride along).
    fn assemble_node(&self, node_id: NodeId) -> Result<Node> {
        let info = self.tree.get_node_info(node_id).map_err(|_| StoreError::NodeNotFound(node_id))?;
        let children = self.tree.get_children(node_id).map_err(|_| StoreError::NodeNotFound(node_id))?;

        let created_at = DateTime::parse_from_rfc3339(&info.created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let modified_at = DateTime::parse_from_rfc3339(&info.modified_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        let content = self.tree.doc(node_id).map(NodeDoc::save).unwrap_or_default();

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

    /// Get a node by ID. `NodeNotFound` for an id with no document, a
    /// tombstone, or a document whose `node` root has not arrived yet.
    pub fn get_node(&self, node_id: NodeId) -> Result<Node> {
        self.assemble_node(node_id)
    }

    /// Create a node under `parent_id` (the root when `None`), returning its
    /// id and the edit: the node's own document (its `node` root, then its
    /// content, tags and custom if any) and the parent's children list.
    /// `node.content`, when given, is merged into the new document as an
    /// update, the one way content reaches a document.
    pub fn create_node(&mut self, node: Node, parent_id: Option<NodeId>) -> Result<(NodeId, TreeEdit)> {
        let node_id = node.id;
        let parent = parent_id.unwrap_or_else(|| self.root_node_id());
        if !self.tree.has_node(parent) {
            return Err(StoreError::NodeNotFound(parent));
        }

        let now = now();
        let mut edit = self
            .tree
            .add_node(node_id, Some(parent), None, &node.node_type, &node.metadata.title, &now)
            .map_err(StoreError::from)?;
        if !node.content.is_empty() {
            let effect = self.tree.apply_update(node_id, &node.content).map_err(StoreError::from)?;
            if effect.changed {
                edit.touched.push((node_id, node.content.clone()));
            }
        }
        if !node.metadata.tags.is_empty() {
            extend(&mut edit, self.tree.set_tags(node_id, &node.metadata.tags, &now).map_err(StoreError::from)?);
        }
        for (key, value) in &node.metadata.custom {
            extend(&mut edit, self.tree.set_custom(node_id, key, value, &now).map_err(StoreError::from)?);
        }
        self.mark_edit_dirty(&edit);

        debug!("Created node {} in store {}", node_id, self.id);
        Ok((node_id, edit))
    }

    /// Delete a node and its subtree: every member becomes a tombstone (its
    /// document stays, in memory and on disk) and the node leaves its
    /// parent's list. The root cannot be deleted. The subtree is what the
    /// tree reaches from `node_id` through children whose list entry and
    /// `parent_id` agree (an unrepaired half-move below it stays where its
    /// `parent_id` says, and the next repair places it).
    pub fn delete_node(&mut self, node_id: NodeId) -> Result<(NodeRemoval, TreeEdit)> {
        let info = self.tree.get_node_info(node_id).map_err(|_| StoreError::NodeNotFound(node_id))?;
        let parent_id = info
            .parent_id
            .ok_or_else(|| StoreError::InvalidOperation("Cannot delete the root node".into()))?;
        let removed = self.tree.subtree_ids(node_id).map_err(StoreError::from)?;
        let edit = self.tree.remove_node(node_id, &now()).map_err(StoreError::from)?;
        self.mark_edit_dirty(&edit);

        debug!("Deleted node {} ({} document(s) tombstoned) from store {}", node_id, removed.len(), self.id);
        Ok((NodeRemoval { parent_id, removed }, edit))
    }

    /// Bring a deleted node and what the same deletion took with it back,
    /// at the end of its parent's list (the root's when the parent is gone).
    pub fn undelete_node(&mut self, node_id: NodeId) -> Result<TreeEdit> {
        if self.tree.doc(node_id).is_none() {
            return Err(StoreError::NodeNotFound(node_id));
        }
        if self.tree.has_node(node_id) {
            return Err(StoreError::InvalidOperation(format!("node {} is not deleted", node_id)));
        }
        let edit = self.tree.undelete_node(node_id, &now()).map_err(StoreError::from)?;
        self.mark_edit_dirty(&edit);
        Ok(edit)
    }

    /// Move a node to a new parent, optionally at a specific position. The
    /// edit names the old parent's list, the new parent's, and the node's
    /// own `parent_id`.
    pub fn move_node(&mut self, node_id: NodeId, new_parent_id: NodeId, position: Option<usize>) -> Result<TreeEdit> {
        if node_id == self.root_node_id() {
            return Err(StoreError::InvalidOperation("Cannot move the root node".into()));
        }
        if node_id == new_parent_id {
            return Err(StoreError::InvalidOperation("Cannot move node into itself".into()));
        }
        if !self.tree.has_node(node_id) {
            return Err(StoreError::NodeNotFound(node_id));
        }
        if !self.tree.has_node(new_parent_id) {
            return Err(StoreError::NodeNotFound(new_parent_id));
        }
        // A move under one's own descendant: `Tree::move_node` refuses it
        // too, but as a crdt error; here it is the invalid operation it is.
        if self.tree.subtree_ids(node_id).map_err(StoreError::from)?.contains(&new_parent_id) {
            return Err(StoreError::InvalidOperation(
                "Cannot move a node into one of its own descendants".into(),
            ));
        }

        let edit = self.tree.move_node(node_id, new_parent_id, position, &now()).map_err(StoreError::from)?;
        self.mark_edit_dirty(&edit);
        Ok(edit)
    }

    /// Merge `content`, a node document snapshot (or any update to one),
    /// into `node_id`'s document. Never a replacement: a snapshot that
    /// shares no history with the copies replicas hold would merge in
    /// beside their paragraphs, so this is only right for a node whose
    /// content was never written (the importer), and then it is the same
    /// merge `apply_node_update` makes. `NodeNotFound` unless `node_id` is
    /// a node.
    pub fn update_node_content(&mut self, node_id: NodeId, content: Vec<u8>) -> Result<NodeUpdateEffect> {
        if !self.tree.has_node(node_id) {
            return Err(StoreError::NodeNotFound(node_id));
        }
        self.apply_node_update(node_id, &content)
    }

    /// Bring a node's title, tags and custom fields to `metadata`, which is
    /// the whole metadata (the app reads the node, changes what it wants
    /// and sends everything back): a custom key absent from it is removed.
    /// Only what differs is written, so re-sending an unchanged title is
    /// not a write that could win over a concurrent rename elsewhere. The
    /// timestamps in `metadata` are ignored; every write here stamps
    /// `modified_at` with now.
    pub fn update_node_metadata(&mut self, node_id: NodeId, metadata: &NodeMetadata) -> Result<TreeEdit> {
        let info = self.tree.get_node_info(node_id).map_err(|_| StoreError::NodeNotFound(node_id))?;
        let now = now();
        let mut edit = TreeEdit::default();
        if info.title != metadata.title {
            extend(&mut edit, self.tree.set_title(node_id, &metadata.title, &now).map_err(StoreError::from)?);
        }
        if info.tags != metadata.tags {
            extend(&mut edit, self.tree.set_tags(node_id, &metadata.tags, &now).map_err(StoreError::from)?);
        }
        for (key, value) in &metadata.custom {
            if info.custom.get(key) != Some(value) {
                extend(&mut edit, self.tree.set_custom(node_id, key, value, &now).map_err(StoreError::from)?);
            }
        }
        for key in info.custom.keys().filter(|key| !metadata.custom.contains_key(*key)) {
            extend(&mut edit, self.tree.remove_custom(node_id, key, &now).map_err(StoreError::from)?);
        }
        self.mark_edit_dirty(&edit);
        Ok(edit)
    }

    /// Repair the tree (see `Tree::repair`), marking only the documents it
    /// changed dirty (docs/history/HARDENING_CONTRACT.md decision 9): a
    /// repair usually finds nothing to fix, and that must never force a
    /// flush. `None` when nothing needed fixing.
    pub fn repair_tree(&mut self) -> Result<Option<TreeEdit>> {
        let edit = self.tree.repair(&now()).map_err(StoreError::from)?;
        if let Some(edit) = &edit {
            self.mark_edit_dirty(edit);
        }
        Ok(edit)
    }

    /// Flush all changes to disk: each dirty document's snapshot, written
    /// atomically, then the manifest.
    pub async fn flush(&mut self) -> Result<()> {
        let dirty: Vec<NodeId> = self.dirty.iter().copied().collect();
        for node_id in dirty {
            if let Some(doc) = self.tree.doc(node_id) {
                atomic_write(&self.node_doc_path(node_id), doc.save()).await?;
            }
        }
        self.dirty.clear();

        // Update manifest modified time
        self.manifest.modified_at = Utc::now();
        let manifest_json = serde_json::to_string_pretty(&self.manifest)?;
        atomic_write(&self.path.join(Self::MANIFEST_FILE), manifest_json).await?;

        debug!("Flushed store {} to disk", self.id);
        Ok(())
    }

    /// Get children of a node
    pub fn get_children(&self, node_id: NodeId) -> Result<Vec<Node>> {
        let children_ids = self.tree.get_children(node_id).map_err(|_| StoreError::NodeNotFound(node_id))?;
        children_ids.into_iter().map(|child_id| self.get_node(child_id)).collect()
    }

    // Private helpers

    fn node_doc_path(&self, node_id: NodeId) -> PathBuf {
        self.path.join(Self::NODES_DIR).join(format!("{}.yrs", node_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_crdt::ContentDoc;
    use tempfile::tempdir;

    fn node_file(store_path: &Path, id: NodeId) -> PathBuf {
        store_path.join("nodes").join(format!("{}.yrs", id))
    }

    fn text_of(node: &Node) -> String {
        NodeDoc::text_of(&node.content)
    }

    #[tokio::test]
    async fn test_create_store() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        assert!(store_path.exists());
        assert!(store_path.join("manifest.json").exists());
        assert!(store_path.join("nodes").exists());
        assert!(node_file(&store_path, store.root_node_id()).exists(), "the root is a document");
        assert!(!store_path.join("store.yrs").exists(), "no tree document");
        assert_eq!(store.manifest().version, StoreManifest::CURRENT_VERSION);
    }

    #[tokio::test]
    async fn test_open_store() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let store_id = store.id;
        let root_id = store.root_node_id();
        drop(store);

        let store = LocalStore::open(&store_path).await.unwrap();
        assert_eq!(store.id, store_id);
        assert_eq!(store.get_node(root_id).unwrap().metadata.title, "Test Store");
    }

    #[tokio::test]
    async fn test_open_rejects_unsupported_format() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");
        fs::create_dir_all(&store_path).await.unwrap();

        // A manifest declaring a version older than the one migrated.
        let manifest = StoreManifest {
            version: 2,
            id: StoreId::new(),
            name: "Old Store".into(),
            root_node_id: NodeId::new(),
            created_at: Utc::now(),
            modified_at: Utc::now(),
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
        let (doc_id, edit) = store.create_node(doc, Some(root_id)).unwrap();
        let touched: HashSet<NodeId> = edit.node_ids().into_iter().collect();
        assert_eq!(touched, HashSet::from([doc_id, root_id]), "the node and its parent's list");

        let node = store.get_node(doc_id).unwrap();
        assert_eq!(node.metadata.title, "Test Document");
        assert_eq!(node.parent_id, Some(root_id));

        let root = store.get_node(root_id).unwrap();
        assert!(root.children.contains(&doc_id));

        assert!(store.create_node(Node::document("Orphan"), Some(NodeId::new())).is_err(), "no such parent");
    }

    #[tokio::test]
    async fn test_delete_node() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let root_id = store.root_node_id();

        let doc = Node::document("To Delete");
        let (doc_id, _) = store.create_node(doc, Some(root_id)).unwrap();
        let (removal, edit) = store.delete_node(doc_id).unwrap();
        assert_eq!(removal.parent_id, root_id);
        assert_eq!(removal.removed, vec![doc_id]);
        let touched: HashSet<NodeId> = edit.node_ids().into_iter().collect();
        assert_eq!(touched, HashSet::from([doc_id, root_id]));

        assert!(matches!(store.get_node(doc_id), Err(StoreError::NodeNotFound(_))));
        let root = store.get_node(root_id).unwrap();
        assert!(!root.children.contains(&doc_id));
    }

    #[tokio::test]
    async fn test_move_node() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let root_id = store.root_node_id();

        let (folder_a_id, _) = store.create_node(Node::folder("A"), Some(root_id)).unwrap();
        let (folder_b_id, _) = store.create_node(Node::folder("B"), Some(root_id)).unwrap();
        let (doc_id, _) = store.create_node(Node::document("Doc"), Some(folder_a_id)).unwrap();

        let edit = store.move_node(doc_id, folder_b_id, None).unwrap();
        let touched: HashSet<NodeId> = edit.node_ids().into_iter().collect();
        assert_eq!(touched, HashSet::from([folder_a_id, folder_b_id, doc_id]));

        let a = store.get_node(folder_a_id).unwrap();
        assert!(!a.children.contains(&doc_id));
        let b = store.get_node(folder_b_id).unwrap();
        assert!(b.children.contains(&doc_id));
        let moved_doc = store.get_node(doc_id).unwrap();
        assert_eq!(moved_doc.parent_id, Some(folder_b_id));

        assert!(matches!(store.move_node(root_id, folder_a_id, None), Err(StoreError::InvalidOperation(_))));
        assert!(matches!(store.move_node(folder_b_id, doc_id, None), Err(StoreError::InvalidOperation(_))), "under its own descendant");
        assert!(matches!(store.move_node(doc_id, doc_id, None), Err(StoreError::InvalidOperation(_))));
        assert!(matches!(store.move_node(doc_id, NodeId::new(), None), Err(StoreError::NodeNotFound(_))));
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
            doc_id = store.create_node(doc, Some(root_id)).unwrap().0;
            store.flush().await.unwrap();
        }

        let store = LocalStore::open(&store_path).await.unwrap();
        assert_eq!(store.id, store_id);
        let node = store.get_node(doc_id).unwrap();
        assert_eq!(node.metadata.title, "Persistent Doc");
    }

    #[tokio::test]
    async fn test_list_node_ids() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");

        let mut store = LocalStore::create(&store_path, "Test Store").await.unwrap();
        let root_id = store.root_node_id();

        let (doc1_id, _) = store.create_node(Node::document("Doc 1"), Some(root_id)).unwrap();
        let (doc2_id, _) = store.create_node(Node::document("Doc 2"), Some(root_id)).unwrap();

        let ids = store.list_node_ids();
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
            doc_id = store.create_node(node, Some(root_id)).unwrap().0;

            // Seed the node with a content snapshot, the importer's way.
            let base = NodeDoc::from_plain_text("hi").unwrap();
            let effect = store.update_node_content(doc_id, base.save()).unwrap();
            assert!(effect.changed && effect.content && !effect.structure);

            // Merge in a diff produced by an independent second document that
            // has more text than the server has state for.
            let richer = NodeDoc::from_plain_text("hi\nthere").unwrap();
            let base_sv = base.state_vector();
            let diff = richer.diff_since(&base_sv).unwrap();
            let effect = store.apply_node_update(doc_id, &diff).unwrap();
            assert!(effect.changed && effect.content);

            // The same bytes again change nothing and dirty nothing.
            store.flush().await.unwrap();
            let effect = store.apply_node_update(doc_id, &diff).unwrap();
            assert!(!effect.changed);
            assert!(store.dirty.is_empty());

            assert!(matches!(store.update_node_content(NodeId::new(), base.save()), Err(StoreError::NodeNotFound(_))));
        }

        // Reopen and verify the merged content survived a round trip through
        // disk, and that the node's place survived beside it.
        let store = LocalStore::open(&store_path).await.unwrap();
        let node = store.get_node(doc_id).unwrap();
        let text = text_of(&node);
        assert!(text.contains("hi"), "expected merged text to contain \"hi\", got {:?}", text);
        assert!(text.contains("there"), "expected merged text to contain \"there\", got {:?}", text);
        assert_eq!(node.metadata.title, "Content Doc");
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

            folder_id = store.create_node(Node::folder("Folder"), Some(root_id)).unwrap().0;

            let mut doc = Node::document("Doc");
            doc.metadata.tags = vec!["x".into(), "y".into()];
            doc.metadata.custom.insert("explicit_title".into(), serde_json::json!(true));
            doc.content = NodeDoc::from_plain_text("body").unwrap().save();
            doc_id = store.create_node(doc, Some(folder_id)).unwrap().0;

            store.flush().await.unwrap();
        }

        let store = LocalStore::open(&store_path).await.unwrap();

        let ids = store.list_node_ids();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&root_id) && ids.contains(&folder_id) && ids.contains(&doc_id));

        let root = store.get_node(root_id).unwrap();
        assert_eq!(root.children, vec![folder_id]);

        let folder = store.get_node(folder_id).unwrap();
        assert_eq!(folder.children, vec![doc_id]);
        assert_eq!(folder.parent_id, Some(root_id));

        let doc = store.get_node(doc_id).unwrap();
        assert_eq!(doc.parent_id, Some(folder_id));
        assert_eq!(doc.metadata.title, "Doc");
        assert_eq!(doc.metadata.tags, vec!["x".to_string(), "y".to_string()]);
        assert_eq!(doc.metadata.custom.get("explicit_title"), Some(&serde_json::json!(true)));
        assert_eq!(text_of(&doc), "body");
    }

    #[tokio::test]
    async fn delete_tombstones_the_subtree_and_keeps_its_documents() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");
        let mut store = LocalStore::create(&store_path, "Store").await.unwrap();
        let root_id = store.root_node_id();
        let (folder_id, _) = store.create_node(Node::folder("Folder"), Some(root_id)).unwrap();
        let (sub_id, _) = store.create_node(Node::folder("Sub"), Some(folder_id)).unwrap();
        let (doc_id, _) = store.create_node(Node::document("Doc"), Some(sub_id)).unwrap();
        let (keep_id, _) = store.create_node(Node::document("Keep"), Some(root_id)).unwrap();
        store.update_node_content(doc_id, NodeDoc::from_plain_text("kept text").unwrap().save()).unwrap();
        store.flush().await.unwrap();

        let (removal, _) = store.delete_node(folder_id).unwrap();
        assert_eq!(removal.parent_id, root_id);
        assert_eq!(removal.removed, vec![folder_id, sub_id, doc_id], "preorder");

        // Not nodes any more: not listed, not fetched, not the root's child.
        let listed: HashSet<NodeId> = store.list_node_ids().into_iter().collect();
        assert_eq!(listed, HashSet::from([root_id, keep_id]));
        for id in [folder_id, sub_id, doc_id] {
            assert!(matches!(store.get_node(id), Err(StoreError::NodeNotFound(_))));
            assert!(store.tree().doc(id).unwrap().fields().unwrap().deleted_at.is_some());
        }
        assert_eq!(store.get_node(root_id).unwrap().children, vec![keep_id]);

        // Still documents: held, named for sync, and on disk after a flush.
        let held: HashSet<NodeId> = store.doc_ids().into_iter().collect();
        assert_eq!(held, HashSet::from([root_id, keep_id, folder_id, sub_id, doc_id]));
        assert!(store.node_state_vector(doc_id).is_ok());
        store.flush().await.unwrap();
        for id in [folder_id, sub_id, doc_id] {
            assert!(node_file(&store_path, id).exists(), "a tombstone's file stays");
        }
        assert!(matches!(store.delete_node(folder_id), Err(StoreError::NodeNotFound(_))), "already deleted");
        assert!(matches!(store.delete_node(root_id), Err(StoreError::InvalidOperation(_))));

        // And a reopen sees the same.
        let store = LocalStore::open(&store_path).await.unwrap();
        assert_eq!(store.list_node_ids().len(), 2);
        assert_eq!(store.doc_ids().len(), 5);
        assert!(store.get_node(doc_id).is_err());
    }

    #[tokio::test]
    async fn undelete_brings_the_subtree_back() {
        let dir = tempdir().unwrap();
        let mut store = LocalStore::create(dir.path().join("test.pimble"), "Store").await.unwrap();
        let root_id = store.root_node_id();
        let (folder_id, _) = store.create_node(Node::folder("Folder"), Some(root_id)).unwrap();
        let (doc_id, _) = store.create_node(Node::document("Doc"), Some(folder_id)).unwrap();
        let (other_id, _) = store.create_node(Node::document("Other"), Some(root_id)).unwrap();
        store.update_node_content(doc_id, NodeDoc::from_plain_text("text").unwrap().save()).unwrap();
        store.delete_node(folder_id).unwrap();

        assert!(matches!(store.undelete_node(other_id), Err(StoreError::InvalidOperation(_))), "not deleted");
        assert!(matches!(store.undelete_node(NodeId::new()), Err(StoreError::NodeNotFound(_))));

        let edit = store.undelete_node(folder_id).unwrap();
        let touched: HashSet<NodeId> = edit.node_ids().into_iter().collect();
        assert_eq!(touched, HashSet::from([folder_id, doc_id, root_id]));
        assert_eq!(store.get_node(root_id).unwrap().children, vec![other_id, folder_id]);
        assert_eq!(store.get_node(folder_id).unwrap().children, vec![doc_id]);
        let doc = store.get_node(doc_id).unwrap();
        assert_eq!(doc.parent_id, Some(folder_id));
        assert_eq!(text_of(&doc), "text");
        assert!(store.tree().validate_tree().is_empty());
    }

    #[tokio::test]
    async fn apply_node_update_creates_an_unknown_document() {
        let dir = tempdir().unwrap();
        let mut store = LocalStore::create(dir.path().join("test.pimble"), "Store").await.unwrap();
        let root_id = store.root_node_id();

        // A peer creates a node in its copy of the store; its edit arrives
        // here document by document, the child's before the root's list.
        let mut peer = Tree::from_docs(root_id, HashMap::new());
        peer.apply_update(root_id, &store.tree().doc(root_id).unwrap().save()).unwrap();
        let child_id = NodeId::new();
        let edit = peer.add_node(child_id, Some(root_id), None, "document", "From peer", &now()).unwrap();

        assert!(matches!(store.node_state_vector(child_id), Err(StoreError::NodeNotFound(_))));
        for (id, update) in &edit.touched {
            let effect = store.apply_node_update(*id, update).unwrap();
            assert!(effect.changed && effect.structure);
        }
        assert!(store.doc_ids().contains(&child_id));
        assert!(store.node_state_vector(child_id).is_ok());
        assert!(store.dirty.contains(&child_id) && store.dirty.contains(&root_id));
        let child = store.get_node(child_id).unwrap();
        assert_eq!(child.metadata.title, "From peer");
        assert_eq!(store.get_node(root_id).unwrap().children, vec![child_id]);

        // Content that arrives before its place makes a document that is
        // not a node yet: held, but neither listed nor fetched.
        let early_id = NodeId::new();
        let effect = store.apply_node_update(early_id, &NodeDoc::from_plain_text("early").unwrap().save()).unwrap();
        assert!(effect.changed && effect.content && !effect.structure);
        assert!(store.doc_ids().contains(&early_id));
        assert!(!store.list_node_ids().contains(&early_id));
        assert!(store.get_node(early_id).is_err());
        assert!(store.tree().validate_tree().is_empty(), "a content-only document is not a tree issue");
    }

    #[tokio::test]
    async fn flush_writes_only_dirty_documents() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("test.pimble");
        let mut store = LocalStore::create(&store_path, "Store").await.unwrap();
        let root_id = store.root_node_id();
        let (a_id, _) = store.create_node(Node::document("A"), Some(root_id)).unwrap();
        let (b_id, _) = store.create_node(Node::document("B"), Some(root_id)).unwrap();
        store.flush().await.unwrap();
        assert!(store.dirty.is_empty());

        let before = |id: NodeId| std::fs::read(node_file(&store_path, id)).unwrap();
        let (root_before, a_before, b_before) = (before(root_id), before(a_id), before(b_id));

        // A rename touches one document; the flush rewrites that one.
        let mut metadata = store.get_node(a_id).unwrap().metadata;
        metadata.title = "A renamed".into();
        let edit = store.update_node_metadata(a_id, &metadata).unwrap();
        assert_eq!(edit.node_ids(), vec![a_id]);
        assert_eq!(store.dirty, HashSet::from([a_id]));
        store.flush().await.unwrap();

        assert_ne!(before(a_id), a_before, "the renamed node's file was rewritten");
        assert_eq!(before(root_id), root_before, "the root's file was left alone");
        assert_eq!(before(b_id), b_before, "the other node's file was left alone");
        assert!(!node_file(&store_path, a_id).with_extension("tmp").exists(), "no temp file left behind");

        // An edit through `tree_mut` reaches disk only once marked dirty.
        store.tree_mut().set_title(b_id, "B renamed", &now()).unwrap();
        store.flush().await.unwrap();
        assert_eq!(before(b_id), b_before);
        store.mark_dirty(b_id);
        store.flush().await.unwrap();
        assert_ne!(before(b_id), b_before);
        assert_eq!(LocalStore::open(&store_path).await.unwrap().get_node(b_id).unwrap().metadata.title, "B renamed");
    }

    #[tokio::test]
    async fn update_node_metadata_writes_differences_and_removes_absent_custom_keys() {
        let dir = tempdir().unwrap();
        let mut store = LocalStore::create(dir.path().join("test.pimble"), "Store").await.unwrap();
        let root_id = store.root_node_id();
        let mut node = Node::document("Doc");
        node.metadata.tags = vec!["t".into()];
        node.metadata.custom.insert("icon".into(), serde_json::json!("star"));
        node.metadata.custom.insert("color".into(), serde_json::json!("#112233"));
        let (doc_id, _) = store.create_node(node, Some(root_id)).unwrap();
        let sv = store.node_state_vector(doc_id).unwrap();

        // Sending the metadata back unchanged writes nothing at all.
        let metadata = store.get_node(doc_id).unwrap().metadata;
        let edit = store.update_node_metadata(doc_id, &metadata).unwrap();
        assert!(edit.is_empty());
        assert_eq!(store.node_state_vector(doc_id).unwrap(), sv);

        // The app's "clear the icon": the key is gone from what it sends.
        let mut metadata = metadata;
        metadata.custom.remove("icon");
        metadata.custom.insert("explicit_title".into(), serde_json::json!(true));
        metadata.tags = vec!["t".into(), "u".into()];
        metadata.title = "Renamed".into();
        let edit = store.update_node_metadata(doc_id, &metadata).unwrap();
        assert!(edit.node_ids().iter().all(|&id| id == doc_id));
        let node = store.get_node(doc_id).unwrap();
        assert_eq!(node.metadata.title, "Renamed");
        assert_eq!(node.metadata.tags, vec!["t".to_string(), "u".to_string()]);
        assert!(!node.metadata.custom.contains_key("icon"), "an absent key is removed");
        assert_eq!(node.metadata.custom.get("color"), Some(&serde_json::json!("#112233")));
        assert_eq!(node.metadata.custom.get("explicit_title"), Some(&serde_json::json!(true)));

        assert!(matches!(store.update_node_metadata(NodeId::new(), &metadata), Err(StoreError::NodeNotFound(_))));
    }

    #[tokio::test]
    async fn a_replica_has_no_root_document_until_one_arrives() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("replica.pimble");
        let (store_id, root_id) = (StoreId::new(), NodeId::new());
        let store = LocalStore::create_replica(&store_path, store_id, "Replica", root_id).await.unwrap();
        assert_eq!(store.id, store_id);
        assert_eq!(store.root_node_id(), root_id);
        assert!(store.doc_ids().is_empty(), "no document at all");
        assert!(store.get_node(root_id).is_err());
        assert_eq!(store.document_root(), None);
        assert!(!node_file(&store_path, root_id).exists());
        drop(store);

        // A reopen creates nothing either.
        let mut store = LocalStore::open(&store_path).await.unwrap();
        assert!(store.doc_ids().is_empty());
        assert!(store.list_node_ids().is_empty());
        assert!(store.tree().validate_tree().is_empty());

        // The peer's root arrives with its tags and custom intact: nothing
        // here fought it.
        let mut origin = Tree::new(root_id, "Origin", &now()).unwrap();
        origin.set_tags(root_id, &["shared".into()], &now()).unwrap();
        origin.set_custom(root_id, "icon", &serde_json::json!("home"), &now()).unwrap();
        let child_id = NodeId::new();
        origin.add_node(child_id, Some(root_id), None, "document", "Doc", &now()).unwrap();
        for id in origin.ids() {
            store.apply_node_update(id, &origin.doc(id).unwrap().save()).unwrap();
        }
        assert!(store.repair_tree().unwrap().is_none());
        assert_eq!(store.document_root(), Some(root_id));
        let root = store.get_node(root_id).unwrap();
        assert_eq!(root.metadata.title, "Origin");
        assert_eq!(root.metadata.tags, vec!["shared".to_string()]);
        assert_eq!(root.metadata.custom.get("icon"), Some(&serde_json::json!("home")));
        assert_eq!(root.children, vec![child_id]);
    }

    /// A replica made around a placeholder root (a vault twin's) adopts the
    /// root its documents name, on `set_root_node_id` and on its own at open.
    #[tokio::test]
    async fn a_placeholder_root_is_replaced_by_the_documents_root() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("replica.pimble");
        let placeholder = NodeId::new();
        let mut store = LocalStore::create_replica(&store_path, StoreId::new(), "Replica", placeholder).await.unwrap();

        let real_root = NodeId::new();
        let mut origin = Tree::new(real_root, "Origin", &now()).unwrap();
        let child_id = NodeId::new();
        origin.add_node(child_id, Some(real_root), None, "document", "Doc", &now()).unwrap();
        for id in origin.ids() {
            store.apply_node_update(id, &origin.doc(id).unwrap().save()).unwrap();
        }
        assert_eq!(store.document_root(), Some(real_root));
        assert_ne!(store.root_node_id(), real_root, "the manifest still says the placeholder");
        store.flush().await.unwrap();

        // Explicitly, as the vault link does.
        store.set_root_node_id(real_root).await.unwrap();
        assert_eq!(store.root_node_id(), real_root);
        assert_eq!(store.get_node(real_root).unwrap().children, vec![child_id]);
        assert!(store.repair_tree().unwrap().is_none());

        // And at open, from a manifest that still says the placeholder.
        let mut manifest = store.manifest().clone();
        manifest.root_node_id = placeholder;
        std::fs::write(store_path.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        let store = LocalStore::open(&store_path).await.unwrap();
        assert_eq!(store.root_node_id(), real_root);
        assert_eq!(store.manifest().root_node_id, real_root);
        assert_eq!(store.get_node(real_root).unwrap().children, vec![child_id]);
    }

    // ── Migration from the previous layout ───────────────────────────────

    /// Everything a node had in the previous layout, to check it survived.
    struct OldNode {
        id: NodeId,
        parent: NodeId,
        node_type: &'static str,
        title: &'static str,
        tags: Vec<String>,
        custom: Vec<(&'static str, serde_json::Value)>,
        created_at: &'static str,
        modified_at: &'static str,
        text: Option<&'static str>,
    }

    /// A store directory as the previous layout wrote it: a version 3
    /// manifest, `store.yrs` built with `StoreDocument`, and a
    /// `nodes/{id}.yrs` content document for every node that had text.
    /// Returns the root id and the nodes in creation order.
    async fn write_old_layout_store(store_path: &Path, name: &str) -> (StoreId, NodeId, Vec<OldNode>) {
        fs::create_dir_all(store_path.join("nodes")).await.unwrap();
        let root_id = NodeId::new();
        let store_id = StoreId::new();
        let manifest = StoreManifest {
            version: 3,
            id: store_id,
            name: name.into(),
            root_node_id: root_id,
            created_at: Utc::now(),
            modified_at: Utc::now(),
            kind: pimble_core::StoreKind::Plain,
        };
        fs::write(store_path.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap()).await.unwrap();

        let folder = NodeId::new();
        let nodes = vec![
            OldNode {
                id: folder,
                parent: root_id,
                node_type: "folder",
                title: "Folder",
                tags: vec!["draft".into()],
                custom: vec![("color", serde_json::json!("#abcdef"))],
                created_at: "2025-01-01T00:00:00+00:00",
                modified_at: "2025-01-02T00:00:00+00:00",
                text: None,
            },
            OldNode {
                id: NodeId::new(),
                parent: folder,
                node_type: "document",
                title: "Second",
                tags: vec![],
                custom: vec![],
                created_at: "2025-02-01T00:00:00+00:00",
                modified_at: "2025-02-02T00:00:00+00:00",
                text: Some("second body\nsecond line"),
            },
            OldNode {
                id: NodeId::new(),
                parent: folder,
                node_type: "document",
                title: "First",
                tags: vec!["a".into(), "b".into()],
                custom: vec![("explicit_title", serde_json::json!(true)), ("icon", serde_json::json!("star"))],
                created_at: "2025-03-01T00:00:00+00:00",
                modified_at: "2025-03-02T00:00:00+00:00",
                text: Some("first body"),
            },
            OldNode {
                id: NodeId::new(),
                parent: root_id,
                node_type: "document",
                title: "Empty",
                tags: vec![],
                custom: vec![],
                created_at: "2025-04-01T00:00:00+00:00",
                modified_at: "2025-04-02T00:00:00+00:00",
                text: None,
            },
        ];

        let mut store_doc = StoreDocument::new(name, root_id).unwrap();
        for node in &nodes {
            store_doc.add_node(node.id, Some(node.parent), node.node_type, node.title).unwrap();
            if !node.tags.is_empty() {
                store_doc.set_tags(node.id, &node.tags).unwrap();
            }
            for (key, value) in &node.custom {
                store_doc.set_custom(node.id, key, value).unwrap();
            }
            if let Some(text) = node.text {
                let content = ContentDoc::from_plain_text(text).unwrap();
                fs::write(store_path.join("nodes").join(format!("{}.yrs", node.id)), content.save()).await.unwrap();
            }
        }
        // "Second" was created before "First" but sits after it: the order
        // of the list, not of creation, is what must survive.
        store_doc.move_node(nodes[2].id, folder, Some(0)).unwrap();
        // Last, since every old-layout edit above stamps `modified_at`.
        for node in &nodes {
            store_doc.set_timestamps(node.id, node.created_at, node.modified_at).unwrap();
        }
        fs::write(store_path.join("store.yrs"), store_doc.save()).await.unwrap();
        (store_id, root_id, nodes)
    }

    fn assert_migrated(store: &LocalStore, root_id: NodeId, nodes: &[OldNode], name: &str) {
        let root = store.get_node(root_id).unwrap();
        assert_eq!(root.metadata.title, name);
        assert_eq!(root.parent_id, None);
        assert_eq!(root.node_type, "folder");
        for old in nodes {
            let node = store.get_node(old.id).unwrap();
            assert_eq!(node.parent_id, Some(old.parent), "{}", old.title);
            assert_eq!(node.node_type, old.node_type);
            assert_eq!(node.metadata.title, old.title);
            assert_eq!(node.metadata.tags, old.tags);
            assert_eq!(node.metadata.custom.len(), old.custom.len());
            for (key, value) in &old.custom {
                assert_eq!(node.metadata.custom.get(*key), Some(value), "{}.{}", old.title, key);
            }
            assert_eq!(node.metadata.created_at, DateTime::parse_from_rfc3339(old.created_at).unwrap());
            assert_eq!(node.metadata.modified_at, DateTime::parse_from_rfc3339(old.modified_at).unwrap());
            assert_eq!(text_of(&node), old.text.unwrap_or(""));
        }
        // The lists, in the old order.
        assert_eq!(root.children, vec![nodes[0].id, nodes[3].id]);
        assert_eq!(store.get_node(nodes[0].id).unwrap().children, vec![nodes[2].id, nodes[1].id]);
        assert!(store.tree().validate_tree().is_empty());
        assert_eq!(store.list_node_ids().len(), nodes.len() + 1);
    }

    #[tokio::test]
    async fn an_old_layout_store_is_migrated_at_open_once() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("old.pimble");
        let (store_id, root_id, nodes) = write_old_layout_store(&store_path, "Old Store").await;

        let store = LocalStore::open(&store_path).await.unwrap();
        assert_eq!(store.id, store_id);
        assert_eq!(store.manifest().version, StoreManifest::CURRENT_VERSION);
        assert_migrated(&store, root_id, &nodes, "Old Store");
        assert!(!store_path.join("store.yrs").exists(), "the tree document is retired");
        assert!(store_path.join("store.yrs.migrated").exists());
        for id in std::iter::once(root_id).chain(nodes.iter().map(|n| n.id)) {
            assert!(node_file(&store_path, id).exists(), "every node has a document on disk, content or not");
        }
        drop(store);

        // A second open finds nothing to migrate and writes nothing.
        let snapshot = |path: &Path| -> HashMap<String, Vec<u8>> {
            std::fs::read_dir(path.join("nodes"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
                .collect()
        };
        let manifest_before = std::fs::read(store_path.join("manifest.json")).unwrap();
        let files_before = snapshot(&store_path);
        let store = LocalStore::open(&store_path).await.unwrap();
        assert_migrated(&store, root_id, &nodes, "Old Store");
        assert_eq!(snapshot(&store_path), files_before, "no document was rewritten");
        assert_eq!(std::fs::read(store_path.join("manifest.json")).unwrap(), manifest_before);
        let manifest_json = std::fs::read_to_string(store_path.join("manifest.json")).unwrap();
        assert!(manifest_json.contains("\"version\": 4"), "{manifest_json}");
    }

    /// A crash between writing the migrated documents and retiring
    /// `store.yrs`: the next open runs the migration again over documents
    /// that are already initialised, and leaves them exactly as they are.
    #[tokio::test]
    async fn migration_is_idempotent_over_initialised_documents() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("old.pimble");
        let (_, root_id, nodes) = write_old_layout_store(&store_path, "Old Store").await;
        let store_doc_bytes = std::fs::read(store_path.join("store.yrs")).unwrap();

        let store = LocalStore::open(&store_path).await.unwrap();
        let states: Vec<Vec<u8>> = store.doc_ids().into_iter().map(|id| store.tree().doc(id).unwrap().save()).collect();
        drop(store);

        // Put the tree document back, as if the rename never happened.
        std::fs::write(store_path.join("store.yrs"), &store_doc_bytes).unwrap();
        std::fs::remove_file(store_path.join("store.yrs.migrated")).unwrap();
        let store = LocalStore::open(&store_path).await.unwrap();
        assert_migrated(&store, root_id, &nodes, "Old Store");
        assert!(!store_path.join("store.yrs").exists());
        let mut after: Vec<Vec<u8>> = store.doc_ids().into_iter().map(|id| store.tree().doc(id).unwrap().save()).collect();
        let mut before = states;
        before.sort();
        after.sort();
        assert_eq!(before, after, "the second migration wrote nothing into any document");
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// Two replicas migrating the same old store independently, then
    /// reconciling document by document, converge on one tree with no
    /// duplicate children (docs/NODE_DOCUMENT_CONTRACT.md section 3).
    #[tokio::test]
    async fn two_independent_migrations_converge_without_duplicates() {
        let dir = tempdir().unwrap();
        let path_a = dir.path().join("a.pimble");
        let path_b = dir.path().join("b.pimble");
        let (_, root_id, nodes) = write_old_layout_store(&path_a, "Shared").await;
        copy_dir(&path_a, &path_b);

        let mut a = LocalStore::open(&path_a).await.unwrap();
        let mut b = LocalStore::open(&path_b).await.unwrap();
        assert_migrated(&a, root_id, &nodes, "Shared");
        assert_migrated(&b, root_id, &nodes, "Shared");

        // One reconcile round: every document's snapshot both ways, a repair
        // on each side, and each side's repair applied to the other.
        let ids: HashSet<NodeId> = a.doc_ids().into_iter().chain(b.doc_ids()).collect();
        for &id in &ids {
            let from_a = a.tree().doc(id).map(NodeDoc::save);
            let from_b = b.tree().doc(id).map(NodeDoc::save);
            if let Some(bytes) = from_a {
                b.apply_node_update(id, &bytes).unwrap();
            }
            if let Some(bytes) = from_b {
                a.apply_node_update(id, &bytes).unwrap();
            }
        }
        // Before the repair the lists hold both migrations' entries.
        assert_eq!(a.tree().doc(root_id).unwrap().children().len(), 4);
        let repair_a = a.repair_tree().unwrap().expect("doubled lists need a repair");
        let repair_b = b.repair_tree().unwrap().expect("doubled lists need a repair");
        for (id, update) in &repair_a.touched {
            b.apply_node_update(*id, update).unwrap();
        }
        for (id, update) in &repair_b.touched {
            a.apply_node_update(*id, update).unwrap();
        }
        assert!(a.repair_tree().unwrap().is_none(), "converged in one round");
        assert!(b.repair_tree().unwrap().is_none());

        for store in [&a, &b] {
            assert_migrated(store, root_id, &nodes, "Shared");
            for id in store.doc_ids() {
                let list = store.tree().doc(id).unwrap().children();
                let unique: HashSet<NodeId> = list.iter().copied().collect();
                assert_eq!(list.len(), unique.len(), "duplicate children in {}", id);
            }
        }
        for &id in &ids {
            assert_eq!(a.tree().doc(id).unwrap().children(), b.tree().doc(id).unwrap().children());
            assert_eq!(a.get_node(id).unwrap().metadata.title, b.get_node(id).unwrap().metadata.title);
        }
        a.flush().await.unwrap();
        b.flush().await.unwrap();
    }
}
