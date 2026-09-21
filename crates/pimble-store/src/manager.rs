//! Store manager - handles multiple open stores

use std::collections::{HashMap, HashSet};
use std::path::Path;

use pimble_core::{MountRef, Node, NodeId, NodeMetadata, Store, StoreId, StoreKind, StoreLocation, SyncState};
use pimble_crdt::{NodeDoc, NodeUpdateEffect, Tree, TreeEdit};
use tracing::info;

/// Maximum depth for transitive mount resolution
const MAX_MOUNT_DEPTH: usize = 16;

use crate::error::{Result, StoreError};
use crate::local::{peek_manifest_kind, LocalStore, NodeRemoval, SyncConfig};
use crate::registry::{StoreEndpoint, StoreRegistry};
use crate::vault::{DocKeys, VaultDocSummary, VaultStore};

/// Manages multiple open stores
pub struct StoreManager {
    /// Open local (`Plain`) stores.
    local_stores: HashMap<StoreId, LocalStore>,
    /// Open vault stores (docs/CRYPTO_CONTRACT.md): disjoint from
    /// `local_stores` — a store id is open in at most one of the two maps,
    /// whichever matches its manifest's `kind`.
    vault_stores: HashMap<StoreId, VaultStore>,
    /// Registry of known stores and how to reach them
    registry: StoreRegistry,
    /// Stores opened since the last call to [`StoreManager::opened_since`],
    /// which drains it. Populated by `create_local_store` and
    /// `open_local_store` (including when called transitively while
    /// resolving or validating a mount), so a caller that may have triggered
    /// an implicit open — e.g. `get_children` or `mount_state` on a mount
    /// node — can learn which stores it must now treat as ordinary open
    /// stores (search index, `listStores`, subscriptions) without diffing
    /// `list_stores()` before and after.
    newly_opened: Vec<StoreId>,
}

impl StoreManager {
    /// Create a new store manager
    pub fn new() -> Self {
        Self {
            local_stores: HashMap::new(),
            vault_stores: HashMap::new(),
            registry: StoreRegistry::new(),
            newly_opened: Vec::new(),
        }
    }

    /// Create a new `Plain` local store with a freshly generated id. See
    /// [`StoreManager::create_local_store_with`] for a chosen `kind`/`id`
    /// (docs/CRYPTO_CONTRACT.md).
    pub async fn create_local_store(&mut self, path: impl AsRef<Path>, name: impl Into<String>) -> Result<StoreId> {
        self.create_local_store_with(path, name, StoreKind::Plain, None).await
    }

    /// Create a new local store of `kind`, under `store_id` when given
    /// (refused if that id is already open here) or a freshly generated one
    /// otherwise (docs/CRYPTO_CONTRACT.md: `createStore`'s `kind`/`store_id`,
    /// the latter letting the accounts service create the hosted twin of a
    /// local store under the local store's own id).
    pub async fn create_local_store_with(
        &mut self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        kind: StoreKind,
        store_id: Option<StoreId>,
    ) -> Result<StoreId> {
        let path = path.as_ref();
        if let Some(id) = store_id {
            if self.is_open(id) {
                return Err(StoreError::InvalidOperation(format!("store {} is already open", id)));
            }
        }

        let id = match kind {
            StoreKind::Plain => {
                let store = match store_id {
                    Some(id) => LocalStore::create_with_id(path, name, id).await?,
                    None => LocalStore::create(path, name).await?,
                };
                let id = store.id;
                self.local_stores.insert(id, store);
                id
            }
            StoreKind::Vault => {
                let store = VaultStore::create(path, name, store_id).await?;
                let id = store.id;
                self.vault_stores.insert(id, store);
                id
            }
        };

        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
        self.newly_opened.push(id);
        Ok(id)
    }

    /// Create a local replica of a store held by a remote server, from
    /// nothing (docs/SYNC_CONTRACT.md decision 8): same `id` and
    /// `root_node_id` as the remote, and no documents until the first
    /// reconcile brings them (see [`LocalStore::create_replica`] for why
    /// not even the root's).
    pub async fn create_replica(
        &mut self,
        path: impl AsRef<Path>,
        id: StoreId,
        name: impl Into<String>,
        root_node_id: NodeId,
    ) -> Result<StoreId> {
        let path = path.as_ref();
        // Inserting a second `LocalStore` under an open id would replace the
        // open one in `local_stores` (dropping its unflushed state), so a
        // replica of a store this server already holds is refused here, not
        // only at the RPC boundary.
        if let Some(open) = self.local_stores.get(&id) {
            return Err(StoreError::InvalidOperation(format!(
                "store {} is already open locally at {}; link it with setStoreSync instead of adding a replica",
                id, open.path.display()
            )));
        }
        let store = LocalStore::create_replica(path, id, name, root_node_id).await?;
        let id = store.id;
        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
        self.local_stores.insert(id, store);
        self.newly_opened.push(id);
        Ok(id)
    }

    /// Open an existing local store, `Plain` or `Vault` (its manifest's
    /// `kind` decides which; docs/CRYPTO_CONTRACT.md).
    pub async fn open_local_store(&mut self, path: impl AsRef<Path>) -> Result<StoreId> {
        let path = path.as_ref();
        let kind = peek_manifest_kind(path).await?;

        let id = match kind {
            StoreKind::Plain => {
                let store = LocalStore::open(path).await?;
                let id = store.id;
                self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
                if self.is_open(id) {
                    info!("Store {} is already open", id);
                    return Ok(id);
                }
                self.local_stores.insert(id, store);
                id
            }
            StoreKind::Vault => {
                let store = VaultStore::open(path).await?;
                let id = store.id;
                self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
                if self.is_open(id) {
                    info!("Store {} is already open", id);
                    return Ok(id);
                }
                self.vault_stores.insert(id, store);
                id
            }
        };

        self.newly_opened.push(id);
        Ok(id)
    }

    /// Drain and return the list of stores opened since the last call to
    /// this method. See the [`StoreManager::newly_opened`] field doc for why
    /// this exists.
    pub fn opened_since(&mut self) -> Vec<StoreId> {
        std::mem::take(&mut self.newly_opened)
    }

    /// Close a store, `Plain` or `Vault`.
    pub async fn close_store(&mut self, store_id: StoreId) -> Result<()> {
        if let Some(mut store) = self.local_stores.remove(&store_id) {
            store.flush().await?;
            info!("Closed store {}", store_id);
        } else if self.vault_stores.remove(&store_id).is_some() {
            // A vault store has no in-memory dirty state to flush: every
            // append/snapshot already wrote to disk synchronously.
            info!("Closed vault store {}", store_id);
        }
        self.newly_opened.retain(|id| *id != store_id);
        Ok(())
    }

    /// Get store info
    pub fn get_store_info(&self, store_id: StoreId) -> Result<Store> {
        if let Some(store) = self.local_stores.get(&store_id) {
            let manifest = store.manifest();
            return Ok(Store {
                id: store_id,
                name: manifest.name.clone(),
                location: StoreLocation::Local {
                    path: store.path.clone(),
                },
                root_node_id: manifest.root_node_id,
                sync_state: SyncState::Offline,
                is_replica: false,
                kind: manifest.kind,
                // Placeholder, like `sync_state` above: callers that care
                // about the vault-link state overwrite this (see
                // `RpcHandler::sync_mode_of`).
                sync_mode: StoreKind::Plain,
                // From `sync.json`, as the store read it at open
                // (docs/NODE_DOCUMENT_CONTRACT.md section 5); the roots are the
                // manifest's scope roots, or empty for the store's own root.
                access: store.access(),
                shared_by: store.shared_by(),
                roots: manifest.scope_roots.clone(),
            });
        }
        if let Some(store) = self.vault_stores.get(&store_id) {
            let manifest = store.manifest();
            return Ok(Store {
                id: store_id,
                name: manifest.name.clone(),
                location: StoreLocation::Local {
                    path: store.path.clone(),
                },
                root_node_id: manifest.root_node_id,
                sync_state: SyncState::Offline,
                is_replica: false,
                kind: manifest.kind,
                sync_mode: StoreKind::Plain,
                access: pimble_core::StoreAccess::Full,
                shared_by: None,
                roots: Vec::new(),
            });
        }
        Err(StoreError::StoreNotFound(store_id))
    }

    /// List all open stores (`Plain` and `Vault` alike).
    pub fn list_stores(&self) -> Vec<StoreId> {
        self.local_stores.keys().chain(self.vault_stores.keys()).copied().collect()
    }

    /// Check if a store is open, `Plain` or `Vault`.
    pub fn is_open(&self, store_id: StoreId) -> bool {
        self.local_stores.contains_key(&store_id) || self.vault_stores.contains_key(&store_id)
    }

    /// The kind of store open under `store_id`, or `None` if it isn't open
    /// at all (docs/CRYPTO_CONTRACT.md). Every store-scoped RPC that isn't
    /// one of the four vault RPCs checks this and refuses a `Vault` store
    /// with `encrypted_store_error`.
    pub fn store_kind(&self, store_id: StoreId) -> Option<StoreKind> {
        if self.local_stores.contains_key(&store_id) {
            return Some(StoreKind::Plain);
        }
        if self.vault_stores.contains_key(&store_id) {
            return Some(StoreKind::Vault);
        }
        None
    }

    /// The open `Plain` store under `store_id`, or `NotOpen`.
    fn local(&self, store_id: StoreId) -> Result<&LocalStore> {
        self.local_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))
    }

    fn local_mut(&mut self, store_id: StoreId) -> Result<&mut LocalStore> {
        self.local_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))
    }

    // ── Vault (encrypted store) storage, docs/CRYPTO_CONTRACT.md ────────

    /// Append `blob` to `doc_id`'s log in vault store `store_id`, returning
    /// its sequence number.
    pub async fn vault_append(&mut self, store_id: StoreId, doc_id: &str, blob: Vec<u8>) -> Result<u64> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.append(doc_id, blob).await
    }

    /// The snapshot (if newer than `after_seq`), every update after
    /// `max(after_seq, snapshot.seq)`, and the document's head.
    pub async fn vault_fetch(&self, store_id: StoreId, doc_id: &str, after_seq: u64) -> Result<(Option<(u64, Vec<u8>)>, Vec<(u64, Vec<u8>)>, u64)> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.fetch(doc_id, after_seq).await
    }

    /// Store a snapshot for `doc_id` covering every update up to and
    /// including `upto_seq`, dropping log entries at or below it.
    pub async fn vault_snapshot(&mut self, store_id: StoreId, doc_id: &str, upto_seq: u64, blob: Vec<u8>) -> Result<()> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.snapshot(doc_id, upto_seq, blob).await
    }

    /// Every document in vault store `store_id`, with its head, snapshot seq
    /// and data key id.
    pub fn vault_list_docs(&self, store_id: StoreId) -> Result<Vec<VaultDocSummary>> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.list_docs())
    }

    /// Whether vault store `store_id` holds `doc_id` at all.
    pub fn vault_has_doc(&self, store_id: StoreId, doc_id: &str) -> Result<bool> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.has_doc(doc_id))
    }

    /// Whether `doc_id` in vault store `store_id` has ever been given a blob
    /// (see [`VaultStore::has_blobs`]).
    pub fn vault_has_blobs(&self, store_id: StoreId, doc_id: &str) -> Result<bool> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.has_blobs(doc_id))
    }

    /// `doc_id`'s wrapped data keys in vault store `store_id`, as stored.
    pub fn vault_doc_keys(&self, store_id: StoreId, doc_id: &str) -> Result<Option<DocKeys>> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.doc_keys(doc_id).cloned())
    }

    /// Store `json` as `doc_id`'s keys record (see [`VaultStore::set_doc_keys`]).
    pub async fn vault_set_doc_keys(&mut self, store_id: StoreId, doc_id: &str, json: String) -> Result<()> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.set_doc_keys(doc_id, json).await
    }

    // ── Scope sets (docs/NODE_DOCUMENT_CONTRACT.md section 5) ────────────

    /// A vault store's published scopes.
    pub fn vault_scopes(&self, store_id: StoreId) -> Result<Vec<(NodeId, Vec<NodeId>)>> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.scopes())
    }

    /// The documents a principal scoped to `roots` reaches in vault store
    /// `store_id` (see [`VaultStore::scope_union`]).
    pub fn vault_scope_union(&self, store_id: StoreId, roots: &[NodeId]) -> Result<HashSet<NodeId>> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.scope_union(roots))
    }

    /// Replace a scope's set, or remove the scope.
    pub async fn vault_set_scope(&mut self, store_id: StoreId, root: NodeId, docs: Option<Vec<NodeId>>) -> Result<()> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        match docs {
            Some(docs) => store.set_scope(root, docs).await,
            None => store.remove_scope(root).await,
        }
    }

    /// A scoped member created `doc_id` under `parent_id` (see
    /// [`VaultStore::extend_scopes`]).
    pub async fn vault_extend_scopes(&mut self, store_id: StoreId, roots: &[NodeId], parent_id: NodeId, doc_id: NodeId) -> Result<bool> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.extend_scopes(roots, parent_id, doc_id).await
    }

    /// Close vault store `store_id` and delete its directory
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5: `deleteVaultStore`, the
    /// accounts service deleting a hosted store). Only a store of kind
    /// `vault` that is open here: a plain store is refused, whatever its
    /// path, because this is the one RPC that removes data from disk and
    /// the hosted server holds nothing but vaults.
    pub async fn delete_vault_store(&mut self, store_id: StoreId) -> Result<()> {
        if self.local_stores.contains_key(&store_id) {
            return Err(StoreError::InvalidOperation(format!("store {} is not a vault store", store_id)));
        }
        let store = self.vault_stores.remove(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        self.registry.unregister(&store_id);
        self.newly_opened.retain(|id| *id != store_id);
        tokio::fs::remove_dir_all(&store.path).await?;
        info!("Deleted vault store {} at {:?}", store_id, store.path);
        Ok(())
    }

    // ── Partial replicas (docs/NODE_DOCUMENT_CONTRACT.md section 5) ──────

    /// What this device may change in plain store `store_id` (`sync.json`'s
    /// `access`; `Full` when unlinked or not a plain store).
    pub fn store_access(&self, store_id: StoreId) -> pimble_core::StoreAccess {
        self.local_stores.get(&store_id).map(|s| s.access()).unwrap_or_default()
    }

    /// Whether a write touching `ids` is refused in plain store `store_id`
    /// because of how this device holds it (see [`LocalStore::write_refused`]).
    pub fn write_refused(&self, store_id: StoreId, ids: &[NodeId]) -> bool {
        self.local_stores.get(&store_id).is_some_and(|s| s.write_refused(ids))
    }

    /// A partial replica's scope roots (empty for a whole store, or a store
    /// that is not open as a plain one).
    pub fn scope_roots(&self, store_id: StoreId) -> Vec<NodeId> {
        self.local_stores.get(&store_id).map(|s| s.scope_roots().to_vec()).unwrap_or_default()
    }

    /// What a partial replica's lists name and it does not hold yet (see
    /// [`LocalStore::awaited_docs`]); empty for anything else.
    pub fn awaited_docs(&self, store_id: StoreId) -> Vec<NodeId> {
        self.local_stores.get(&store_id).map(|s| s.awaited_docs()).unwrap_or_default()
    }

    /// Add a scope root to a partial replica (see [`LocalStore::add_scope_root`]).
    pub async fn add_scope_root(&mut self, store_id: StoreId, root: NodeId) -> Result<()> {
        self.local_mut(store_id)?.add_scope_root(root).await
    }

    /// Rename a partial replica (see [`LocalStore::set_partial_replica_name`]).
    pub async fn set_partial_replica_name(&mut self, store_id: StoreId, name: &str) -> Result<()> {
        self.local_mut(store_id)?.set_partial_replica_name(name).await
    }

    /// Create a partial replica: [`StoreManager::create_replica`] with the
    /// scope roots the share grants (see [`LocalStore::create_replica_with_scope`]).
    pub async fn create_partial_replica(
        &mut self,
        path: impl AsRef<Path>,
        id: StoreId,
        name: impl Into<String>,
        scope_roots: Vec<NodeId>,
    ) -> Result<StoreId> {
        let path = path.as_ref();
        if let Some(open) = self.local_stores.get(&id) {
            return Err(StoreError::InvalidOperation(format!(
                "store {} is already open locally at {}; add the root to it instead of creating another replica",
                id, open.path.display()
            )));
        }
        let root = scope_roots.first().copied().unwrap_or_else(NodeId::new);
        let store = LocalStore::create_replica_with_scope(path, id, name, root, scope_roots).await?;
        let id = store.id;
        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
        self.local_stores.insert(id, store);
        self.newly_opened.push(id);
        Ok(id)
    }

    // ── Nodes ────────────────────────────────────────────────────────────

    /// Get a node from a store. `NodeNotFound` for a tombstone or a
    /// document whose `node` root has not arrived.
    pub fn get_node(&self, store_id: StoreId, node_id: NodeId) -> Result<Node> {
        self.local(store_id)?.get_node(node_id)
    }

    /// Bring a node's title, tags and custom fields to `metadata` (the
    /// whole metadata; a custom key absent from it is removed). See
    /// [`LocalStore::update_node_metadata`].
    pub fn update_node_metadata(&mut self, store_id: StoreId, node_id: NodeId, metadata: NodeMetadata) -> Result<TreeEdit> {
        self.local_mut(store_id)?.update_node_metadata(node_id, &metadata)
    }

    /// Create a node in a store, returning its id and the edit to persist
    /// and broadcast per document. A mount node has no children of its own
    /// (its subtree lives entirely in the source store), so creating under
    /// one is rejected; create under the mount's `mount_ref` instead.
    pub fn create_node(&mut self, store_id: StoreId, node: Node, parent_id: Option<NodeId>) -> Result<(NodeId, TreeEdit)> {
        let store = self.local_mut(store_id)?;

        if let Some(parent_id) = parent_id {
            let parent = store.tree().get_node_info(parent_id).map_err(|_| StoreError::NodeNotFound(parent_id))?;
            if parent.node_type == pimble_core::node_types::MOUNT {
                return Err(StoreError::MountHasNoChildren { node_id: parent_id });
            }
        }

        store.create_node(node, parent_id)
    }

    /// Move a node to a new parent in a store. The edit names the old
    /// parent's list, the new parent's and the node itself; a caller that
    /// wants the old parent for a notification reads it off the tree first.
    pub fn move_node(&mut self, store_id: StoreId, node_id: NodeId, new_parent_id: NodeId, position: Option<usize>) -> Result<TreeEdit> {
        self.local_mut(store_id)?.move_node(node_id, new_parent_id, position)
    }

    /// Delete a node and its subtree from a store: every member becomes a
    /// tombstone (docs/NODE_DOCUMENT_CONTRACT.md section 2; no file is
    /// deleted). Returns the parent it was removed from with every id
    /// tombstoned, and the edit. Refuses to delete the root.
    pub fn delete_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<(NodeRemoval, TreeEdit)> {
        self.local_mut(store_id)?.delete_node(node_id)
    }

    /// Bring a deleted node and its subtree back (see
    /// [`LocalStore::undelete_node`]).
    pub fn undelete_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<TreeEdit> {
        self.local_mut(store_id)?.undelete_node(node_id)
    }

    /// Repair a store's tree after a merge (see `Tree::repair`), marking
    /// only the documents it changed dirty (docs/history/HARDENING_CONTRACT.md
    /// decision 9). `None` when nothing needed fixing.
    pub fn repair_tree(&mut self, store_id: StoreId) -> Result<Option<TreeEdit>> {
        self.local_mut(store_id)?.repair_tree()
    }

    /// Merge a node document snapshot into a node's document (never a
    /// replacement; see [`LocalStore::update_node_content`]).
    pub fn update_node_content(&mut self, store_id: StoreId, node_id: NodeId, content: Vec<u8>) -> Result<NodeUpdateEffect> {
        self.local_mut(store_id)?.update_node_content(node_id, content)
    }

    /// Get children of a node, and the id of the store they canonically live
    /// in.
    ///
    /// For an ordinary node this is `(store_id, ...)`: the request's own
    /// store. For a mount point, the mount's source store is opened if
    /// necessary (see [`StoreManager::ensure_store_open`]) and the source
    /// node's children are returned instead, addressed by the source store's
    /// id — a mount resolves exactly one level; a nested mount inside the
    /// returned children is itself a source-store node, resolved the same
    /// way when the caller expands it in turn.
    pub async fn get_children(&mut self, store_id: StoreId, node_id: NodeId) -> Result<(StoreId, Vec<Node>)> {
        let node = self.get_node(store_id, node_id)?;
        if node.is_mount() {
            let mount_ref = node.mount_ref().ok_or_else(|| {
                StoreError::InvalidOperation(format!("mount node {} has no mount_ref", node_id))
            })?;
            let source_store = self.ensure_store_open(&mount_ref).await?;
            let children = self.local(source_store)?.get_children(mount_ref.source_node)?;
            return Ok((source_store, children));
        }

        let children = self.local(store_id)?.get_children(node_id)?;
        Ok((store_id, children))
    }

    // ── Documents ────────────────────────────────────────────────────────

    /// The tree over a store's documents.
    pub fn tree(&self, store_id: StoreId) -> Result<&Tree> {
        Ok(self.local(store_id)?.tree())
    }

    /// The tree, to edit. Nothing edited through it is marked dirty: call
    /// [`StoreManager::mark_dirty`] for every id the resulting `TreeEdit`
    /// names.
    pub fn tree_mut(&mut self, store_id: StoreId) -> Result<&mut Tree> {
        Ok(self.local_mut(store_id)?.tree_mut())
    }

    /// A held document, to edit; marked dirty at once. `NodeNotFound` when
    /// no document is held under `node_id`.
    pub fn node_doc(&mut self, store_id: StoreId, node_id: NodeId) -> Result<&mut NodeDoc> {
        self.local_mut(store_id)?.node_doc(node_id)
    }

    /// Merge a peer's update into a node's document, creating the document
    /// when unknown (authorise first). Marks it dirty only when the merge
    /// changed something (docs/history/HARDENING_CONTRACT.md decision 8);
    /// repairs nothing, so a caller applies a batch and then runs
    /// `repair_tree` once when any effect reports `structure`.
    pub fn apply_node_update(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<NodeUpdateEffect> {
        self.local_mut(store_id)?.apply_node_update(node_id, update)
    }

    /// v1-encoded state vector of a node's document (`NodeNotFound` when no
    /// document is held under the id).
    pub fn node_state_vector(&self, store_id: StoreId, node_id: NodeId) -> Result<Vec<u8>> {
        self.local(store_id)?.node_state_vector(node_id)
    }

    /// Everything a node's document has that a peer at `state_vector`
    /// (v1-encoded) lacks.
    pub fn node_diff_since(&self, store_id: StoreId, node_id: NodeId, state_vector: &[u8]) -> Result<Vec<u8>> {
        self.local(store_id)?.node_diff_since(node_id, state_vector)
    }

    /// Every held document's id, tombstones included: what sync names.
    pub fn doc_ids(&self, store_id: StoreId) -> Result<Vec<NodeId>> {
        Ok(self.local(store_id)?.doc_ids())
    }

    /// Every undeleted node's id.
    pub fn list_node_ids(&self, store_id: StoreId) -> Result<Vec<NodeId>> {
        Ok(self.local(store_id)?.list_node_ids())
    }

    /// Mark a node's document as needing a flush (an edit made through
    /// `tree_mut`).
    pub fn mark_dirty(&mut self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        self.local_mut(store_id)?.mark_dirty(node_id);
        Ok(())
    }

    /// The root a store's documents say it has (see
    /// [`LocalStore::document_root`]), for `adopt_document_root`.
    pub fn document_root(&self, store_id: StoreId) -> Result<Option<NodeId>> {
        Ok(self.local(store_id)?.document_root())
    }

    // ── Sync configuration ───────────────────────────────────────────────

    /// Read a store's replica sync link (`<store>/sync.json`), if any.
    pub async fn read_sync_config(&self, store_id: StoreId) -> Result<Option<SyncConfig>> {
        self.local(store_id)?.read_sync_config().await
    }

    /// Write a store's replica sync link, replacing any existing one.
    pub async fn write_sync_config(&self, store_id: StoreId, config: &SyncConfig) -> Result<()> {
        self.local(store_id)?.write_sync_config(config).await
    }

    /// Delete a store's replica sync link, if any (unlinking it).
    pub async fn clear_sync_config(&self, store_id: StoreId) -> Result<()> {
        self.local(store_id)?.clear_sync_config().await
    }

    /// Flush a store to disk
    pub async fn flush(&mut self, store_id: StoreId) -> Result<()> {
        self.local_mut(store_id)?.flush().await
    }

    /// Flush all stores to disk
    pub async fn flush_all(&mut self) -> Result<()> {
        for store in self.local_stores.values_mut() {
            store.flush().await?;
        }
        Ok(())
    }

    /// Get the root node ID for a store
    pub fn root_node_id(&self, store_id: StoreId) -> Result<NodeId> {
        Ok(self.local(store_id)?.root_node_id())
    }

    /// Rewrite a local store's manifest root (see `LocalStore::set_root_node_id`).
    pub async fn set_root_node_id(&mut self, store_id: StoreId, root_node_id: NodeId) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id).ok_or(StoreError::StoreNotFound(store_id))?;
        store.set_root_node_id(root_node_id).await
    }

    /// Get a reference to the store registry
    pub fn registry(&self) -> &StoreRegistry {
        &self.registry
    }

    /// Get a mutable reference to the store registry
    pub fn registry_mut(&mut self) -> &mut StoreRegistry {
        &mut self.registry
    }

    // ── Mount resolution ────────────────────────────────────────────

    /// Ensure a mount's source store is open, returning its `StoreId`
    /// (always `mount_ref.source_store` on success). Resolution order:
    /// already open; a registry entry (works for a store opened earlier in
    /// this process, local or previously-registered); `mount_ref`'s
    /// `source_path` hint, opened directly and thereby registered (this is
    /// what lets a mount resolve after a restart even when its source was
    /// not itself in the app's open-store list). `Err` only when none of
    /// those apply, or the registry's entry is `Remote` (out of scope).
    pub async fn ensure_store_open(&mut self, mount_ref: &MountRef) -> Result<StoreId> {
        let store_id = mount_ref.source_store;

        match self.open_registered_store(store_id).await {
            Ok(()) => Ok(store_id),
            Err(StoreError::MountSourceUnavailable { .. }) => {
                let Some(path) = &mount_ref.source_path else {
                    return Err(StoreError::MountSourceUnavailable { store_id });
                };
                // The hint is only a hint: the directory may now hold a
                // different store. Open it, and if it is not the store the
                // mount names, close it again and report the source missing.
                let opened = self.open_local_store(path).await
                    .map_err(|_| StoreError::MountSourceUnavailable { store_id })?;
                if opened != store_id {
                    self.close_store(opened).await?;
                    return Err(StoreError::MountSourceUnavailable { store_id });
                }
                Ok(store_id)
            }
            Err(e) => Err(e),
        }
    }

    /// Open `store_id` if it is already open or known to the registry;
    /// otherwise `MountSourceUnavailable`. Does not consult a `MountRef`'s
    /// `source_path` hint — see [`StoreManager::ensure_store_open`] for the
    /// full resolution order used when resolving a specific mount.
    async fn open_registered_store(&mut self, store_id: StoreId) -> Result<()> {
        if self.local_stores.contains_key(&store_id) {
            return Ok(());
        }

        match self.registry.lookup(&store_id).cloned() {
            Some(StoreEndpoint::Local { path }) => {
                self.open_local_store(&path).await?;
                Ok(())
            }
            Some(StoreEndpoint::Remote { .. }) => {
                Err(StoreError::MountSourceUnavailable { store_id })
            }
            None => Err(StoreError::MountSourceUnavailable { store_id }),
        }
    }

    /// Check if creating a mount would create a cycle.
    pub async fn validate_mount_creation(
        &mut self,
        mounting_store: StoreId,
        mounting_node: NodeId,
        mount_ref: &MountRef,
    ) -> Result<()> {
        let ancestors = self.collect_ancestors(mounting_store, mounting_node)?;

        self.ensure_store_open(mount_ref).await?;

        let mut stack: Vec<(StoreId, NodeId, usize)> = vec![
            (mount_ref.source_store, mount_ref.source_node, 0),
        ];

        while let Some((current_store, current_node, depth)) = stack.pop() {
            if depth > MAX_MOUNT_DEPTH {
                return Err(StoreError::MountDepthExceeded { depth });
            }

            self.open_registered_store(current_store).await?;

            let node = self.local(current_store)?.get_node(current_node)?;

            if node.is_mount() {
                if let Some(nested_ref) = node.mount_ref() {
                    if nested_ref.source_store == mounting_store
                        && ancestors.contains(&nested_ref.source_node)
                    {
                        return Err(StoreError::MountCycle {
                            chain: vec![
                                (mounting_store, mounting_node),
                                (mount_ref.source_store, mount_ref.source_node),
                                (nested_ref.source_store, nested_ref.source_node),
                            ],
                        });
                    }

                    if self.registry.lookup(&nested_ref.source_store).is_some()
                        || self.local_stores.contains_key(&nested_ref.source_store)
                    {
                        stack.push((nested_ref.source_store, nested_ref.source_node, depth + 1));
                    }
                }
            }

            let children_ids = node.children.clone();
            for child_id in children_ids {
                stack.push((current_store, child_id, depth + 1));
            }
        }

        Ok(())
    }

    /// Collect the set of ancestor node IDs for a given node in a store.
    fn collect_ancestors(&self, store_id: StoreId, node_id: NodeId) -> Result<Vec<NodeId>> {
        let mut ancestors = vec![node_id];
        let store = self.local(store_id)?;

        let mut current = node_id;
        loop {
            let node = store.get_node(current)?;
            match node.parent_id {
                Some(parent_id) => {
                    ancestors.push(parent_id);
                    current = parent_id;
                }
                None => break,
            }
        }

        Ok(ancestors)
    }
}

impl Default for StoreManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const T: &str = "2026-09-18T10:00:00+00:00";

    /// Reproduces the bug an app-level `addRemoteStore` self-reference
    /// exposed: `create_replica` for an id that's already open would
    /// `HashMap::insert` over the open `LocalStore`, silently dropping its
    /// in-memory tree (any unflushed edits with it) and leaving the store id
    /// pointing at a fresh, empty replica. Refused here, not only at the RPC
    /// boundary, since this is `StoreManager`'s own invariant to keep.
    #[tokio::test]
    async fn create_replica_refuses_an_id_that_is_already_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("existing.pimble"), "Existing").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let replica_dir = tempfile::tempdir().unwrap();
        let err = manager
            .create_replica(replica_dir.path().join("replica.pimble"), store_id, "Existing", root_id)
            .await
            .expect_err("create_replica should refuse an id that's already open");
        assert!(
            matches!(err, StoreError::InvalidOperation(_)),
            "expected InvalidOperation, got {:?}", err
        );

        // The original store must be untouched: still open, same root.
        assert!(manager.is_open(store_id));
        assert_eq!(manager.root_node_id(store_id).unwrap(), root_id);
        assert!(!replica_dir.path().join("replica.pimble").exists(), "no replica directory should have been created");
    }

    /// A replica starts with no documents, not even the root's: the peer's
    /// root document is the root (see `LocalStore::create_replica`).
    #[tokio::test]
    async fn create_replica_makes_no_root_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let (id, root_id) = (StoreId::new(), NodeId::new());
        let store_id = manager.create_replica(dir.path().join("replica.pimble"), id, "Replica", root_id).await.unwrap();
        assert_eq!(store_id, id);
        assert!(manager.doc_ids(store_id).unwrap().is_empty());
        assert!(manager.list_node_ids(store_id).unwrap().is_empty());
        assert!(matches!(manager.get_node(store_id, root_id), Err(StoreError::NodeNotFound(_))));
        assert_eq!(manager.document_root(store_id).unwrap(), None);
        assert_eq!(manager.opened_since(), vec![store_id]);
    }

    /// Deleting a folder tombstones every descendant (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 2) — not just the folder itself — and deletes no file: the
    /// documents stay, named for sync but no longer nodes.
    #[tokio::test]
    async fn delete_node_tombstones_the_whole_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let folder_id = manager.create_node(store_id, Node::folder("Folder"), Some(root_id)).unwrap().0;
        let subfolder_id = manager.create_node(store_id, Node::folder("Subfolder"), Some(folder_id)).unwrap().0;
        let doc_id = manager.create_node(store_id, Node::document("Doc"), Some(subfolder_id)).unwrap().0;

        manager.update_node_content(store_id, doc_id, NodeDoc::from_plain_text("hi").unwrap().save()).unwrap();
        manager.flush(store_id).await.unwrap();

        let store_path = manager.get_store_info(store_id).unwrap().local_path().unwrap().clone();
        let content_path = store_path.join("nodes").join(format!("{}.yrs", doc_id));
        assert!(content_path.exists(), "the document's file should exist before delete");

        let (removal, edit) = manager.delete_node(store_id, folder_id).unwrap();

        assert_eq!(removal.parent_id, root_id);
        let removed: HashSet<_> = removal.removed.iter().copied().collect();
        assert_eq!(removed, [folder_id, subfolder_id, doc_id].into_iter().collect());
        let touched: HashSet<_> = edit.node_ids().into_iter().collect();
        assert_eq!(touched, [folder_id, subfolder_id, doc_id, root_id].into_iter().collect());

        let tree = manager.tree(store_id).unwrap();
        assert!(!tree.has_node(folder_id));
        assert!(!tree.has_node(subfolder_id));
        assert!(!tree.has_node(doc_id));
        assert!(!tree.get_children(root_id).unwrap().contains(&folder_id));
        assert!(manager.doc_ids(store_id).unwrap().contains(&doc_id), "a tombstone is still a document");
        assert!(!manager.list_node_ids(store_id).unwrap().contains(&doc_id));
        manager.flush(store_id).await.unwrap();
        assert!(content_path.exists(), "no file is deleted");
    }

    /// A subtree that hasn't been repaired yet can itself be malformed: a
    /// concurrent merge (docs/history/HARDENING_CONTRACT.md decision 9) can leave a
    /// cycle among nodes below the one being deleted (e.g. two folders that
    /// concurrently moved under each other), and a child listed twice in the
    /// same parent's children list (e.g. two replicas concurrently moving
    /// the same node to the same destination). Reproduced directly here
    /// with `insert_child` on the documents — the same primitive the tree's
    /// edits use, so the resulting shape is exactly what such a merge
    /// leaves behind — rather than via an actual two-replica merge, to
    /// control precisely which lists end up malformed. `delete_node`'s walk
    /// must terminate (not loop forever on the cycle) and the delete must
    /// still succeed — without ever calling `repair_tree` first.
    #[tokio::test]
    async fn delete_node_survives_an_unrepaired_subtree_with_a_cycle_and_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let top = manager.create_node(store_id, Node::folder("Top"), Some(root_id)).unwrap().0;
        let folder_a = manager.create_node(store_id, Node::folder("A"), Some(top)).unwrap().0;
        let folder_b = manager.create_node(store_id, Node::folder("B"), Some(top)).unwrap().0;
        let child = manager.create_node(store_id, Node::document("Child"), Some(folder_a)).unwrap().0;

        {
            let tree = manager.tree_mut(store_id).unwrap();
            // A cycle in the children-list graph below `top` (both A and B stay
            // listed under `top` too, so the subtree is still reachable from it):
            // A's list also names B, and B's list also names A.
            tree.doc_mut(folder_b).unwrap().insert_child(99, folder_a).unwrap();
            tree.doc_mut(folder_a).unwrap().insert_child(99, folder_b).unwrap();
            // A duplicate: `child` appears a second time in A's own list.
            tree.doc_mut(folder_a).unwrap().insert_child(99, child).unwrap();
        }

        let issues_before = manager.tree(store_id).unwrap().validate_tree();
        assert!(!issues_before.is_empty(), "expected the fabricated state to be malformed before any repair");

        let (removal, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async { manager.delete_node(store_id, top) })
            .await
            .expect("delete_node must terminate even with a cycle below the deleted node")
            .expect("delete_node must succeed even with a duplicated child below the deleted node");

        let removed: HashSet<_> = removal.removed.into_iter().collect();
        assert_eq!(removed, [top, folder_a, folder_b, child].into_iter().collect());

        let tree = manager.tree(store_id).unwrap();
        assert!(!tree.has_node(top));
        assert!(!tree.has_node(folder_a));
        assert!(!tree.has_node(folder_b));
        assert!(!tree.has_node(child));
    }

    /// An unrepaired list below the deleted node that names the root, or an id
    /// with no document, must neither take the root (and with it the whole
    /// store) down with the subtree nor fail the delete halfway.
    #[tokio::test]
    async fn delete_node_skips_the_root_and_dangling_entries_listed_below_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let keep = manager.create_node(store_id, Node::document("Keep"), Some(root_id)).unwrap().0;
        let folder = manager.create_node(store_id, Node::folder("Folder"), Some(root_id)).unwrap().0;
        {
            let tree = manager.tree_mut(store_id).unwrap();
            tree.doc_mut(folder).unwrap().insert_child(99, root_id).unwrap();
            tree.doc_mut(folder).unwrap().insert_child(99, NodeId::new()).unwrap();
        }

        let (removal, _) = manager.delete_node(store_id, folder).expect("delete_node succeeds");
        assert_eq!(removal.removed, vec![folder]);

        let tree = manager.tree(store_id).unwrap();
        assert!(tree.has_node(root_id), "the root must survive");
        assert!(tree.has_node(keep), "the root's other children must survive");
        assert!(!tree.has_node(folder));
    }

    /// A node's stored `parent_id` can name a different node than the
    /// children-list edge it is reachable by — the same not-yet-repaired
    /// merge shape as the test above, in the `parent_id` field rather than
    /// the list. The tree counts a node as a child only where list and
    /// `parent_id` agree, so such a node is not in the deleted subtree: the
    /// delete succeeds around it, and the next repair places it under the
    /// root (its stored parent is a tombstone) rather than losing it.
    #[tokio::test]
    async fn delete_node_leaves_a_node_whose_parent_id_disagrees_with_the_list_to_repair() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let top = manager.create_node(store_id, Node::folder("Top"), Some(root_id)).unwrap().0;
        let sibling = manager.create_node(store_id, Node::folder("Sibling"), Some(top)).unwrap().0;
        let mismatched = manager.create_node(store_id, Node::document("Mismatched"), Some(top)).unwrap().0;

        // `mismatched` stays listed under `top` (untouched), but its own
        // `parent_id` field is corrupted to name `sibling` instead.
        manager.node_doc(store_id, mismatched).unwrap().set_parent_id(Some(sibling), T).unwrap();

        let (removal, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async { manager.delete_node(store_id, top) })
            .await
            .unwrap()
            .expect("delete_node must succeed around a node whose parent_id disagrees with the list");

        let removed: HashSet<_> = removal.removed.into_iter().collect();
        assert_eq!(removed, [top, sibling].into_iter().collect());

        let tree = manager.tree(store_id).unwrap();
        assert!(!tree.has_node(top));
        assert!(!tree.has_node(sibling));
        assert!(tree.has_node(mismatched), "not reached by the deletion: still a node");

        let repair = manager.repair_tree(store_id).unwrap().expect("the orphan needs a repair");
        assert!(repair.node_ids().contains(&mismatched));
        assert_eq!(manager.get_node(store_id, mismatched).unwrap().parent_id, Some(root_id));
        assert!(manager.tree(store_id).unwrap().validate_tree().is_empty());
    }

    /// The root node can never be deleted, subtree or not.
    #[tokio::test]
    async fn delete_node_refuses_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let err = manager.delete_node(store_id, root_id).expect_err("deleting the root must fail");
        assert!(matches!(err, StoreError::InvalidOperation(_)), "expected InvalidOperation, got {:?}", err);
        assert!(manager.tree(store_id).unwrap().has_node(root_id));
    }

    /// `repair_tree` on an already well-formed tree changes nothing
    /// (docs/history/HARDENING_CONTRACT.md decision 9): in particular it must not mark
    /// anything dirty, which would force a needless flush on every reconcile.
    #[tokio::test]
    async fn repair_tree_is_a_no_op_on_a_well_formed_tree() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        manager.flush(store_id).await.unwrap();

        let repair = manager.repair_tree(store_id).unwrap();
        assert!(repair.is_none());
    }

    /// A plain node's deletion, once flushed, stays deleted across a
    /// reopen: the store-layer half of the contract the RPC handler's
    /// `deleteNode` depends on (from Joe's bug report: a node deleted in
    /// the app reappeared after restarting; the root cause was `deleteNode`
    /// never flushing, fixed in `pimble-server`, not here). With
    /// tombstones the document comes back from disk, and comes back deleted.
    #[tokio::test]
    async fn deleted_node_stays_deleted_after_flush_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.pimble");

        let (store_id, root_id, doc_id) = {
            let mut manager = StoreManager::new();
            let store_id = manager.create_local_store(&path, "Store").await.unwrap();
            let root_id = manager.root_node_id(store_id).unwrap();
            let doc_id = manager.create_node(store_id, Node::document("Doc"), Some(root_id)).unwrap().0;
            manager.flush(store_id).await.unwrap();

            manager.delete_node(store_id, doc_id).unwrap();
            manager.flush(store_id).await.unwrap();

            (store_id, root_id, doc_id)
        };

        let mut manager = StoreManager::new();
        let reopened_id = manager.open_local_store(&path).await.unwrap();
        assert_eq!(reopened_id, store_id);
        let tree = manager.tree(store_id).unwrap();
        assert!(!tree.has_node(doc_id));
        assert!(!tree.get_children(root_id).unwrap().contains(&doc_id));
        assert!(manager.doc_ids(store_id).unwrap().contains(&doc_id), "the tombstone is held");
        assert!(manager.repair_tree(store_id).unwrap().is_none());
    }

    /// The manager's edits report what they wrote, per document, and a peer
    /// applying exactly those updates ends up with the same tree.
    #[tokio::test]
    async fn edits_report_updates_a_peer_can_apply() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();
        let peer_id = manager.create_replica(dir.path().join("peer.pimble"), StoreId::new(), "Peer", root_id).await.unwrap();
        let relay = |manager: &mut StoreManager, edit: &TreeEdit| {
            for (id, update) in &edit.touched {
                manager.apply_node_update(peer_id, *id, update).unwrap();
            }
        };
        let root_bytes = manager.node_doc(store_id, root_id).unwrap().save();
        manager.apply_node_update(peer_id, root_id, &root_bytes).unwrap();

        let mut node = Node::document("Doc");
        node.metadata.tags = vec!["t".into()];
        node.metadata.custom.insert("icon".into(), serde_json::json!("star"));
        let (doc_id, edit) = manager.create_node(store_id, node, Some(root_id)).unwrap();
        relay(&mut manager, &edit);
        let (folder_id, edit) = manager.create_node(store_id, Node::folder("Folder"), None).unwrap();
        relay(&mut manager, &edit);
        let edit = manager.move_node(store_id, doc_id, folder_id, Some(0)).unwrap();
        relay(&mut manager, &edit);
        let mut metadata = manager.get_node(store_id, doc_id).unwrap().metadata;
        metadata.title = "Renamed".into();
        metadata.custom.remove("icon");
        let edit = manager.update_node_metadata(store_id, doc_id, metadata).unwrap();
        relay(&mut manager, &edit);
        let (_, edit) = manager.delete_node(store_id, folder_id).unwrap();
        relay(&mut manager, &edit);
        let edit = manager.undelete_node(store_id, folder_id).unwrap();
        relay(&mut manager, &edit);
        assert!(manager.repair_tree(peer_id).unwrap().is_none(), "the relayed edits leave nothing to repair");

        for id in [root_id, folder_id, doc_id] {
            let (a, b) = (manager.get_node(store_id, id).unwrap(), manager.get_node(peer_id, id).unwrap());
            assert_eq!(a.parent_id, b.parent_id);
            assert_eq!(a.children, b.children);
            assert_eq!(a.metadata.title, b.metadata.title);
            assert_eq!(a.metadata.tags, b.metadata.tags);
            assert_eq!(a.metadata.custom, b.metadata.custom);
        }
        let doc = manager.get_node(peer_id, doc_id).unwrap();
        assert_eq!(doc.metadata.title, "Renamed");
        assert!(!doc.metadata.custom.contains_key("icon"));
        assert_eq!(manager.get_node(peer_id, root_id).unwrap().children, vec![folder_id]);
        assert_eq!(manager.get_node(peer_id, folder_id).unwrap().children, vec![doc_id]);
    }

    /// Creating under a mount node is refused: its children live in the
    /// source store.
    #[tokio::test]
    async fn create_node_refuses_a_mount_as_parent() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();
        let mount = Node::new(pimble_core::node_types::MOUNT);
        let (mount_id, _) = manager.create_node(store_id, mount, Some(root_id)).unwrap();
        let err = manager.create_node(store_id, Node::document("Under a mount"), Some(mount_id)).unwrap_err();
        assert!(matches!(err, StoreError::MountHasNoChildren { node_id } if node_id == mount_id), "{err:?}");
        let err = manager.create_node(store_id, Node::document("Nowhere"), Some(NodeId::new())).unwrap_err();
        assert!(matches!(err, StoreError::NodeNotFound(_)), "{err:?}");
    }
}
