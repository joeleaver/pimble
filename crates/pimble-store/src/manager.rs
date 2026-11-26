//! Store manager - handles multiple open stores

use std::collections::{HashMap, HashSet};
use std::path::Path;

use pimble_core::{LeftShare, MountRef, Node, NodeId, NodeMetadata, Place, Store, StoreId, StoreKind, StoreLocation, SyncState};
use pimble_crdt::{NodeDoc, NodeUpdateEffect, Tree, TreeEdit};
use pimble_host::{Disk, Runtime};
use tracing::info;

/// Maximum depth for transitive mount resolution
const MAX_MOUNT_DEPTH: usize = 16;

use crate::blobs::BlobStore;
use crate::error::{Result, StoreError};
use crate::local::{peek_manifest_on, FolderMerge, LocalStore, NodeRemoval, SyncConfig};
use crate::registry::{StoreEndpoint, StoreRegistry};
use crate::vault::{DocKeys, VaultDocSummary, VaultStore};

mod mounts;
mod nodes;
mod placement;
#[cfg(test)]
mod tests;
mod vault;

/// What [`StoreManager::transplant_node`] did (docs/MOVE_CONTRACT.md
/// "Between stores"): the new root's id in the target store, the shares the
/// node left in the source, and one [`TreeEdit`] per store, since a
/// transplant between stores is two documents' worth of writes, never one.
pub struct TransplantOutcome {
    pub new_node_id: NodeId,
    pub left_shares: Vec<LeftShare>,
    /// What the source's `delete_node` tombstoned: the parent the node was
    /// removed from and its whole subtree, preorder.
    pub source_removal: NodeRemoval,
    pub source_edit: TreeEdit,
    pub target_edit: TreeEdit,
}

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
    /// Where each local store keeps this computer's own state, under
    /// `<state root>/<store id>/` (docs/STORE_FOLDERS_CONTRACT.md rule 1):
    /// `<data dir>/pimble/stores` for a server, a temp directory in a test.
    /// `None` (a manager made with [`StoreManager::new`]) keeps the old
    /// single-file layout in the store's folder.
    state_root: Option<std::path::PathBuf>,
    /// The disk every store is created and opened on, and the state root is
    /// on (docs/ONE_CODEBASE_CONTRACT.md seam B).
    disk: Disk,
    /// The runtime every store's tasks, heavy jobs and clocks go through
    /// (seam C), and the server's with it.
    runtime: Runtime,
}

impl StoreManager {
    /// Create a new store manager with no state root (see
    /// [`StoreManager::with_state_root`]), on the default disk.
    pub fn new() -> Self {
        Self {
            local_stores: HashMap::new(),
            vault_stores: HashMap::new(),
            registry: StoreRegistry::new(),
            newly_opened: Vec::new(),
            state_root: None,
            disk: Disk::default(),
            runtime: Runtime::default(),
        }
    }

    /// A store manager whose local stores keep this computer's state under
    /// `state_root` (docs/STORE_FOLDERS_CONTRACT.md rule 1).
    pub fn with_state_root(state_root: impl Into<std::path::PathBuf>) -> Self {
        Self { state_root: Some(state_root.into()), ..Self::new() }
    }

    /// The same manager on `disk`, before any store is open.
    pub fn on_disk(mut self, disk: Disk) -> Self {
        self.disk = disk;
        self
    }

    /// The disk this manager's stores are on.
    pub fn disk(&self) -> &Disk {
        &self.disk
    }

    /// The same manager on `runtime`, before any store is open.
    pub fn on_runtime(mut self, runtime: Runtime) -> Self {
        self.runtime = runtime;
        self
    }

    /// The runtime this manager's stores (and its server) run on.
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The state root, when there is one.
    pub fn state_root(&self) -> Option<&Path> {
        self.state_root.as_deref()
    }

    /// Local store `store_id`'s state directory (`<state root>/<store id>/`):
    /// where its search index, its link's state (`sync-state.json`, one
    /// record: `crate::sync_state`) and `people.json` live. `None` for a store not open as a local one, or
    /// a manager with no state root.
    pub fn state_dir(&self, store_id: StoreId) -> Option<std::path::PathBuf> {
        self.local(store_id).ok()?.state_dir().map(Path::to_path_buf)
    }

    /// Merge what changed in local store `store_id`'s folder at `paths`
    /// (`LocalStore::absorb_folder_paths`, docs/STORE_FOLDERS_CONTRACT.md
    /// rules 3 and 4).
    pub async fn absorb_folder_paths(&mut self, store_id: StoreId, paths: &[std::path::PathBuf]) -> Result<()> {
        self.local_mut(store_id)?.absorb_folder_paths(paths).await
    }

    /// What merges from local store `store_id`'s folder changed, not yet
    /// announced (`LocalStore::take_folder_merges`). Empty for any other.
    pub fn take_folder_merges(&mut self, store_id: StoreId) -> Vec<FolderMerge> {
        self.local_mut(store_id).map(|s| s.take_folder_merges()).unwrap_or_default()
    }

    /// Whether local store `store_id` has merges from its folder to announce.
    pub fn has_folder_merges(&self, store_id: StoreId) -> bool {
        self.local(store_id).is_ok_and(|s| s.has_folder_merges())
    }

    /// Whether local store `store_id` has anything not yet written.
    pub fn has_unwritten(&self, store_id: StoreId) -> bool {
        self.local(store_id).is_ok_and(|s| s.has_unwritten())
    }

    /// The folder of local store `store_id`.
    pub fn local_path(&self, store_id: StoreId) -> Option<std::path::PathBuf> {
        self.local(store_id).ok().map(|s| s.path.clone())
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
                let mut store = LocalStore::create_on(&self.disk, &self.runtime, path, name, store_id).await?;
                if let Some(root) = &self.state_root {
                    store.set_state_root(root);
                }
                let id = store.id;
                self.local_stores.insert(id, store);
                id
            }
            StoreKind::Vault => {
                let store = VaultStore::create_on(&self.disk, &self.runtime, path, name, store_id).await?;
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
        let mut store = LocalStore::create_replica_on(&self.disk, &self.runtime, path, id, name, root_node_id, Vec::new()).await?;
        if let Some(root) = &self.state_root {
            store.set_state_root(root);
        }
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
        let manifest = peek_manifest_on(&self.disk, path).await?;

        let id = match manifest.kind {
            StoreKind::Plain => {
                // Already open: not opened a second time. Opening merges and
                // removes conflict copies and may move state out of the
                // folder (docs/STORE_FOLDERS_CONTRACT.md), none of which a
                // throwaway second copy of an open store may do.
                if self.is_open(manifest.id) {
                    self.registry.register(manifest.id, StoreEndpoint::Local { path: path.to_path_buf() });
                    info!("Store {} is already open", manifest.id);
                    return Ok(manifest.id);
                }
                let store = LocalStore::open_on(&self.disk, &self.runtime, path, self.state_root.as_deref()).await?;
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
                let store = VaultStore::open_on(&self.disk, &self.runtime, path).await?;
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

    /// Take a plain store loaded outside the manager's lock
    /// (`LocalStore::open_in` with [`StoreManager::state_root`]) as open, as
    /// [`StoreManager::open_local_store`] would have: the server reads a
    /// store's documents without holding the manager, so several stores open
    /// side by side (docs/STARTUP_CONTRACT.md). A store already open keeps
    /// the copy it has, and the loaded one is dropped.
    pub fn adopt_local_store(&mut self, path: impl AsRef<Path>, store: LocalStore) -> StoreId {
        let id = store.id;
        self.registry.register(id, StoreEndpoint::Local { path: path.as_ref().to_path_buf() });
        if !self.is_open(id) {
            self.local_stores.insert(id, store);
            self.newly_opened.push(id);
        }
        id
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
                read_only_roots: store.read_only_roots(),
                ended_roots: manifest.ended_roots.clone(),
                // A placeholder like `sync_mode`: `RpcHandler::link_kind_of`.
                relay: pimble_core::RelaySide::None,
                waiting: Vec::new(),
                out_of_step: Vec::new(),
                also_open_on: Vec::new(),
                index: pimble_core::IndexState::Ready,
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
                read_only_roots: Vec::new(),
                ended_roots: Vec::new(),
                relay: pimble_core::RelaySide::None,
                waiting: Vec::new(),
                out_of_step: Vec::new(),
                also_open_on: Vec::new(),
                index: pimble_core::IndexState::Ready,
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

    /// The pictures of plain store `store_id` (docs/IMAGES_CONTRACT.md): a
    /// handle to put, get and check blobs with, used after the manager's
    /// lock is let go. `NotOpen` for a store that is not open as a plain one
    /// (a vault's pictures are vault documents, not files here).
    pub fn blobs(&self, store_id: StoreId) -> Result<BlobStore> {
        Ok(self.local(store_id)?.blobs())
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

    /// [`LocalStore::comment_refused`].
    pub fn comment_refused(&self, store_id: StoreId, ids: &[NodeId]) -> bool {
        self.local_stores.get(&store_id).is_some_and(|s| s.comment_refused(ids))
    }

    /// Whether `id` is a discussion: from the tree of a plain store, from the
    /// discussion set of a vault store (docs/DISCUSSIONS_CONTRACT.md section 7).
    pub fn is_discussion(&self, store_id: StoreId, id: NodeId) -> bool {
        match self.local_stores.get(&store_id) {
            Some(s) => s.is_discussion(id),
            None => self.vault_is_discussion(store_id, id),
        }
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

    /// The scope roots of a partial replica the account no longer holds
    /// (see [`LocalStore::ended_roots`]); empty for anything else.
    pub fn ended_roots(&self, store_id: StoreId) -> Vec<NodeId> {
        self.local_stores.get(&store_id).map(|s| s.ended_roots().to_vec()).unwrap_or_default()
    }

    /// Whether a node is under ended roots only (see
    /// [`LocalStore::under_ended_root`]).
    pub fn under_ended_root(&self, store_id: StoreId, node_id: NodeId) -> bool {
        self.local_stores.get(&store_id).is_some_and(|s| s.under_ended_root(node_id))
    }

    /// Record which of a partial replica's scope roots have ended, answering
    /// whether that changed anything (see [`LocalStore::set_ended_roots`]).
    pub async fn set_ended_roots(&mut self, store_id: StoreId, ended: Vec<NodeId>) -> Result<bool> {
        self.local_mut(store_id)?.set_ended_roots(ended).await
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
        let mut store = LocalStore::create_replica_on(&self.disk, &self.runtime, path, id, name, root, scope_roots).await?;
        if let Some(root) = &self.state_root {
            store.set_state_root(root);
        }
        let id = store.id;
        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
        self.local_stores.insert(id, store);
        self.newly_opened.push(id);
        Ok(id)
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

    /// Put documents `ids` of local store `store_id` on the disk now
    /// (`LocalStore::flush_nodes_durably`).
    pub async fn flush_nodes_durably(&mut self, store_id: StoreId, ids: &[NodeId]) -> Result<()> {
        self.local_mut(store_id)?.flush_nodes_durably(ids).await
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

}

impl Default for StoreManager {
    fn default() -> Self {
        Self::new()
    }
}
