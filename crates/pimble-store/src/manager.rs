//! Store manager - handles multiple open stores

use std::collections::{HashMap, HashSet};
use std::path::Path;

use pimble_core::{MountRef, MountState, Node, NodeId, NodeMetadata, Store, StoreId, StoreLocation, SyncState};
use pimble_crdt::{ContentDoc, StoreDocument, TreeRepair};
use tracing::info;

/// Maximum depth for transitive mount resolution
const MAX_MOUNT_DEPTH: usize = 16;

use crate::error::{Result, StoreError};
use crate::local::{LocalStore, SyncConfig};
use crate::registry::{StoreEndpoint, StoreRegistry};

/// What [`StoreManager::delete_node`] removed.
#[derive(Debug, Clone)]
pub struct NodeRemoval {
    /// The parent the deleted node was removed from.
    pub parent_id: NodeId,
    /// The deleted node and every descendant of it.
    pub removed: Vec<NodeId>,
}

/// Manages multiple open stores
pub struct StoreManager {
    /// Open local stores
    local_stores: HashMap<StoreId, LocalStore>,
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
            registry: StoreRegistry::new(),
            newly_opened: Vec::new(),
        }
    }

    /// Create a new local store
    pub async fn create_local_store(&mut self, path: impl AsRef<Path>, name: impl Into<String>) -> Result<StoreId> {
        let path = path.as_ref();
        let store = LocalStore::create(path, name).await?;
        let id = store.id;
        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });
        self.local_stores.insert(id, store);
        self.newly_opened.push(id);
        Ok(id)
    }

    /// Create a local replica of a store held by a remote server, from
    /// nothing (docs/SYNC_CONTRACT.md decision 8): same `id` and
    /// `root_node_id` as the remote, an empty store document.
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

    /// Open an existing local store
    pub async fn open_local_store(&mut self, path: impl AsRef<Path>) -> Result<StoreId> {
        let path = path.as_ref();
        let store = LocalStore::open(path).await?;
        let id = store.id;

        // Always register (updates path if it changed)
        self.registry.register(id, StoreEndpoint::Local { path: path.to_path_buf() });

        if self.local_stores.contains_key(&id) {
            info!("Store {} is already open", id);
            return Ok(id);
        }

        self.local_stores.insert(id, store);
        self.newly_opened.push(id);
        Ok(id)
    }

    /// Drain and return the list of stores opened since the last call to
    /// this method. See the [`StoreManager::newly_opened`] field doc for why
    /// this exists.
    pub fn opened_since(&mut self) -> Vec<StoreId> {
        std::mem::take(&mut self.newly_opened)
    }

    /// Close a store
    pub async fn close_store(&mut self, store_id: StoreId) -> Result<()> {
        if let Some(mut store) = self.local_stores.remove(&store_id) {
            store.flush().await?;
            info!("Closed store {}", store_id);
        }
        self.newly_opened.retain(|id| *id != store_id);
        Ok(())
    }

    /// Get store info
    pub fn get_store_info(&self, store_id: StoreId) -> Result<Store> {
        if let Some(store) = self.local_stores.get(&store_id) {
            let manifest = store.manifest();
            Ok(Store {
                id: store_id,
                name: manifest.name.clone(),
                location: StoreLocation::Local {
                    path: store.path.clone(),
                },
                root_node_id: manifest.root_node_id,
                sync_state: SyncState::Offline,
                is_replica: false,
            })
        } else {
            Err(StoreError::StoreNotFound(store_id))
        }
    }

    /// List all open stores
    pub fn list_stores(&self) -> Vec<StoreId> {
        self.local_stores.keys().copied().collect()
    }

    /// Check if a store is open
    pub fn is_open(&self, store_id: StoreId) -> bool {
        self.local_stores.contains_key(&store_id)
    }

    /// Get a node from a store
    pub async fn get_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<Node> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.get_node(node_id).await
    }

    /// Update a node's metadata in-place and mark it dirty
    pub async fn update_node_metadata(&mut self, store_id: StoreId, node_id: NodeId, metadata: NodeMetadata) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.update_node_metadata(node_id, &metadata).await
    }

    /// Create a node in a store. A mount node has no children of its own
    /// (its subtree lives entirely in the source store), so creating under
    /// one is rejected; create under the mount's `mount_ref` instead.
    pub async fn create_node(&mut self, store_id: StoreId, node: Node, parent_id: Option<NodeId>) -> Result<NodeId> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;

        if let Some(parent_id) = parent_id {
            let parent = store.store_document().get_node_info(parent_id)
                .map_err(StoreError::Crdt)?;
            if parent.node_type == pimble_core::node_types::MOUNT {
                return Err(StoreError::MountHasNoChildren { node_id: parent_id });
            }
        }

        store.create_node(node, parent_id).await
    }

    /// Move a node to a new parent in a store. Returns the parent it left.
    pub async fn move_node(&mut self, store_id: StoreId, node_id: NodeId, new_parent_id: NodeId, position: Option<usize>) -> Result<NodeId> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        let old_parent_id = store.store_document().get_node_info(node_id)
            .map_err(StoreError::Crdt)?
            .parent_id
            .ok_or_else(|| StoreError::InvalidOperation("Cannot move the root node".into()))?;
        store.move_node(node_id, new_parent_id, position).await?;
        Ok(old_parent_id)
    }

    /// Delete a node and its subtree from a store. Returns the parent it was
    /// removed from and every node id removed — the node itself and every
    /// descendant, entries and content files alike
    /// (docs/history/HARDENING_CONTRACT.md decision 10). Refuses to delete the root.
    ///
    /// The subtree is walked *before* anything is removed: `LocalStore::delete_node`
    /// deletes one node's own entry (and drops it from its parent's children
    /// list), so a child's parent must still be reachable when its turn comes.
    pub async fn delete_node(&mut self, store_id: StoreId, node_id: NodeId) -> Result<NodeRemoval> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        let parent_id = store.store_document().get_node_info(node_id)
            .map_err(StoreError::Crdt)?
            .parent_id
            .ok_or_else(|| StoreError::InvalidOperation("Cannot delete the root node".into()))?;

        // A merge that hasn't been repaired yet (repair runs *after* a changing
        // `applyStoreUpdate` — one can always be caught mid-flight, e.g. right
        // between two servers reconciling) may leave a cycle or a child listed
        // under two parents below `node_id`. `visited` makes both harmless: a
        // cycle stops the walk from looping forever, and a node reachable two
        // ways is still only queued for deletion once (queuing it twice would
        // make the second `LocalStore::delete_node` fail outright — its entry
        // would already be gone).
        // The same unrepaired state can also list the root, or an id with no
        // entry, below `node_id`: the root is never deleted along with a subtree
        // (that would take the whole store), and an entry that doesn't exist has
        // nothing to delete.
        let root_id = store.root_node_id();
        let mut visited = HashSet::new();
        let mut removed = Vec::new();
        let mut stack = vec![node_id];
        while let Some(id) = stack.pop() {
            if id == root_id || !store.store_document().has_node(id) || !visited.insert(id) {
                continue;
            }
            let children = store.store_document().get_children(id).map_err(StoreError::Crdt)?;
            stack.extend(children);
            removed.push(id);
        }

        // `removed` is top-down (a node's parent-by-list always appears before it —
        // see the walk above), so deleting bottom-up (reverse order) usually lets
        // `LocalStore::delete_node` clean the parent's children list too, not just
        // drop the node's own entry. It is not depended on for correctness: the
        // same malformed-merge case above can leave a node's stored `parent_id`
        // pointing somewhere other than the list edge this walk followed to reach
        // it, so `StoreDocument::remove_node` tolerates a parent that's already
        // gone (from earlier in this same loop, or otherwise) rather than erroring.
        for &id in removed.iter().rev() {
            store.delete_node(id).await?;
        }

        Ok(NodeRemoval { parent_id, removed })
    }

    /// Repair a store's tree after a merge (see `StoreDocument::repair`),
    /// marking the store document dirty only when it actually changed
    /// something (docs/history/HARDENING_CONTRACT.md decision 9).
    pub fn repair_tree(&mut self, store_id: StoreId) -> Result<Option<TreeRepair>> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.repair_tree()
    }

    /// Replace a node's content with a full yrs snapshot.
    pub async fn update_node_content(&mut self, store_id: StoreId, node_id: NodeId, content: Vec<u8>) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.update_node_content(node_id, content).await
    }

    /// Merge a yrs update (delta, reconciliation diff, or whole snapshot)
    /// into a node's content document. Returns whether it changed anything
    /// (docs/history/HARDENING_CONTRACT.md decision 8).
    pub async fn apply_content_update(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<bool> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.apply_content_update(node_id, update).await
    }

    /// Get a node's persistent CRDT content document (mutable reference)
    pub async fn get_node_document(&mut self, store_id: StoreId, node_id: NodeId) -> Result<&mut ContentDoc> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.get_node_document(node_id).await
    }

    /// Save a node's CRDT document
    pub async fn save_node_document(&mut self, store_id: StoreId, node_id: NodeId, doc: &mut ContentDoc) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.save_node_document(node_id, doc).await
    }

    /// Mark a node's content as dirty
    pub fn mark_content_dirty(&mut self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.mark_content_dirty(node_id);
        Ok(())
    }

    /// Get a reference to a store's StoreDocument
    pub fn store_document(&self, store_id: StoreId) -> Result<&StoreDocument> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.store_document())
    }

    /// Get a mutable reference to a store's StoreDocument
    pub fn store_document_mut(&mut self, store_id: StoreId) -> Result<&mut StoreDocument> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.store_document_mut())
    }

    /// v1-encoded state vector for a store's store document.
    pub fn store_doc_state_vector(&self, store_id: StoreId) -> Result<Vec<u8>> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.store_doc_state_vector())
    }

    /// Everything a store's store document has that a peer at `state_vector`
    /// (v1-encoded) lacks.
    pub fn store_doc_diff_since(&self, store_id: StoreId, state_vector: &[u8]) -> Result<Vec<u8>> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.store_doc_diff_since(state_vector)
    }

    /// Merge a peer's yrs update into a store's store document. Returns
    /// whether it changed anything and the ids of the node entries it
    /// touched (see `LocalStore::apply_store_doc_update`).
    pub fn apply_store_doc_update(&mut self, store_id: StoreId, update: &[u8]) -> Result<pimble_crdt::StoreUpdateEffect> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.apply_store_doc_update(update)
    }

    /// Read a store's replica sync link (`<store>/sync.json`), if any.
    pub async fn read_sync_config(&self, store_id: StoreId) -> Result<Option<SyncConfig>> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.read_sync_config().await
    }

    /// Write a store's replica sync link, replacing any existing one.
    pub async fn write_sync_config(&self, store_id: StoreId, config: &SyncConfig) -> Result<()> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.write_sync_config(config).await
    }

    /// Delete a store's replica sync link, if any (unlinking it).
    pub async fn clear_sync_config(&self, store_id: StoreId) -> Result<()> {
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.clear_sync_config().await
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
        let node = self.get_node(store_id, node_id).await?;
        if node.is_mount() {
            let mount_ref = node.mount_ref().ok_or_else(|| {
                StoreError::InvalidOperation(format!("mount node {} has no mount_ref", node_id))
            })?;
            let source_store = self.ensure_store_open(&mount_ref).await?;
            let store = self.local_stores.get_mut(&source_store)
                .ok_or(StoreError::NotOpen(source_store))?;
            let children = store.get_children(mount_ref.source_node).await?;
            return Ok((source_store, children));
        }

        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        let children = store.get_children(node_id).await?;
        Ok((store_id, children))
    }

    /// Flush a store to disk
    pub async fn flush(&mut self, store_id: StoreId) -> Result<()> {
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        store.flush().await
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
        let store = self.local_stores.get(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.root_node_id())
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

    /// The current state of a mount point: `Live` if its source store is
    /// open or could be opened (see [`StoreManager::ensure_store_open`]),
    /// `Unavailable` otherwise. Actually attempts resolution rather than
    /// checking the registry alone, so a `Live` result means the source can
    /// really be reached right now.
    pub async fn mount_state(&mut self, mount_ref: &MountRef) -> MountState {
        match self.ensure_store_open(mount_ref).await {
            Ok(_) => MountState::Live,
            Err(e) => MountState::Unavailable { reason: Some(e.to_string()) },
        }
    }

    /// Check if creating a mount would create a cycle.
    pub async fn validate_mount_creation(
        &mut self,
        mounting_store: StoreId,
        mounting_node: NodeId,
        mount_ref: &MountRef,
    ) -> Result<()> {
        let ancestors = self.collect_ancestors(mounting_store, mounting_node).await?;

        self.ensure_store_open(mount_ref).await?;

        let mut stack: Vec<(StoreId, NodeId, usize)> = vec![
            (mount_ref.source_store, mount_ref.source_node, 0),
        ];

        while let Some((current_store, current_node, depth)) = stack.pop() {
            if depth > MAX_MOUNT_DEPTH {
                return Err(StoreError::MountDepthExceeded { depth });
            }

            self.open_registered_store(current_store).await?;

            let node = {
                let store = self.local_stores.get_mut(&current_store)
                    .ok_or(StoreError::NotOpen(current_store))?;
                store.get_node(current_node).await?
            };

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
    async fn collect_ancestors(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
    ) -> Result<Vec<NodeId>> {
        let mut ancestors = vec![node_id];
        let store = self.local_stores.get_mut(&store_id)
            .ok_or(StoreError::NotOpen(store_id))?;

        let mut current = node_id;
        loop {
            let node = store.get_node(current).await?;
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

    /// Deleting a folder deletes every descendant's entry from the store document and
    /// every descendant's content file from disk (docs/history/HARDENING_CONTRACT.md
    /// decision 10) — not just the folder itself.
    #[tokio::test]
    async fn delete_node_removes_the_whole_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let folder_id = manager.create_node(store_id, Node::folder("Folder"), Some(root_id)).await.unwrap();
        let subfolder_id = manager.create_node(store_id, Node::folder("Subfolder"), Some(folder_id)).await.unwrap();
        let doc_id = manager.create_node(store_id, Node::document("Doc"), Some(subfolder_id)).await.unwrap();

        manager.update_node_content(store_id, doc_id, ContentDoc::from_plain_text("hi").unwrap().save()).await.unwrap();
        manager.flush(store_id).await.unwrap();

        let store_path = manager.get_store_info(store_id).unwrap().local_path().unwrap().clone();
        let content_path = store_path.join("nodes").join(format!("{}.yrs", doc_id));
        assert!(content_path.exists(), "content file should exist before delete");

        let removal = manager.delete_node(store_id, folder_id).await.unwrap();

        assert_eq!(removal.parent_id, root_id);
        let removed: std::collections::HashSet<_> = removal.removed.into_iter().collect();
        assert_eq!(removed, [folder_id, subfolder_id, doc_id].into_iter().collect());

        assert!(!manager.store_document(store_id).unwrap().has_node(folder_id));
        assert!(!manager.store_document(store_id).unwrap().has_node(subfolder_id));
        assert!(!manager.store_document(store_id).unwrap().has_node(doc_id));
        assert!(!manager.store_document(store_id).unwrap().get_children(root_id).unwrap().contains(&folder_id));
        assert!(!content_path.exists(), "content file should be gone after delete");
    }

    /// A subtree that hasn't been repaired yet can itself be malformed: a
    /// concurrent merge (docs/history/HARDENING_CONTRACT.md decision 9) can leave a
    /// cycle among nodes below the one being deleted (e.g. two folders that
    /// concurrently moved under each other), and a child listed twice in the
    /// same parent's children list (e.g. two replicas concurrently moving
    /// the same node to the same destination). Reproduced directly here with
    /// `append_child` — the same primitive `add_node`/`move_node` use
    /// internally, so the resulting shape is exactly what such a merge
    /// leaves behind — rather than via an actual two-replica merge, to
    /// control precisely which lists end up malformed. `delete_node`'s walk
    /// must terminate (not loop forever on the cycle) and the delete must
    /// still succeed (not fail partway through when the duplicate's second
    /// occurrence points at an entry the first one already removed) —
    /// without ever calling `repair_tree` first.
    #[tokio::test]
    async fn delete_node_survives_an_unrepaired_subtree_with_a_cycle_and_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let top = manager.create_node(store_id, Node::folder("Top"), Some(root_id)).await.unwrap();
        let folder_a = manager.create_node(store_id, Node::folder("A"), Some(top)).await.unwrap();
        let folder_b = manager.create_node(store_id, Node::folder("B"), Some(top)).await.unwrap();
        let child = manager.create_node(store_id, Node::document("Child"), Some(folder_a)).await.unwrap();

        {
            let doc = manager.store_document_mut(store_id).unwrap();
            // A cycle in the children-list graph below `top` (both A and B stay
            // listed under `top` too, so the subtree is still reachable from it):
            // A's list also names B, and B's list also names A.
            doc.append_child(folder_b, folder_a).unwrap();
            doc.append_child(folder_a, folder_b).unwrap();
            // A duplicate: `child` appears a second time in A's own list.
            doc.append_child(folder_a, child).unwrap();
        }

        let issues_before = manager.store_document(store_id).unwrap().validate_tree().unwrap();
        assert!(!issues_before.is_empty(), "expected the fabricated state to be malformed before any repair");

        let removal = tokio::time::timeout(std::time::Duration::from_secs(5), manager.delete_node(store_id, top))
            .await
            .expect("delete_node must terminate even with a cycle below the deleted node")
            .expect("delete_node must succeed even with a duplicated child below the deleted node");

        let removed: HashSet<_> = removal.removed.into_iter().collect();
        assert_eq!(removed, [top, folder_a, folder_b, child].into_iter().collect());

        let doc = manager.store_document(store_id).unwrap();
        assert!(!doc.has_node(top));
        assert!(!doc.has_node(folder_a));
        assert!(!doc.has_node(folder_b));
        assert!(!doc.has_node(child));
    }

    /// An unrepaired list below the deleted node that names the root, or an id
    /// with no entry, must neither take the root (and with it the whole store)
    /// down with the subtree nor fail the delete halfway.
    #[tokio::test]
    async fn delete_node_skips_the_root_and_dangling_entries_listed_below_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let keep = manager.create_node(store_id, Node::document("Keep"), Some(root_id)).await.unwrap();
        let folder = manager.create_node(store_id, Node::folder("Folder"), Some(root_id)).await.unwrap();
        {
            let doc = manager.store_document_mut(store_id).unwrap();
            doc.append_child(folder, root_id).unwrap();
            doc.append_child(folder, NodeId::new()).unwrap();
        }

        let removal = manager.delete_node(store_id, folder).await.expect("delete_node succeeds");
        assert_eq!(removal.removed, vec![folder]);

        let doc = manager.store_document(store_id).unwrap();
        assert!(doc.has_node(root_id), "the root must survive");
        assert!(doc.has_node(keep), "the root's other children must survive");
        assert!(!doc.has_node(folder));
    }

    /// A node's stored `parent_id` field can name a different node than the
    /// children-list edge the subtree walk actually followed to reach it —
    /// the same not-yet-repaired-merge shape as the test above, just in the
    /// parent_id field rather than the list. If that other node is also
    /// inside the subtree and ends up deleted earlier in the batch (as
    /// `top`'s children here are ordered to force), `LocalStore::delete_node`
    /// looking the node up in *that* parent's children list must not fail
    /// just because the parent's entry is already gone.
    #[tokio::test]
    async fn delete_node_survives_a_node_whose_parent_id_points_at_an_already_deleted_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let top = manager.create_node(store_id, Node::folder("Top"), Some(root_id)).await.unwrap();
        // Created in this order so `top`'s children list is [sibling,
        // mismatched]: the walk is a LIFO stack, so of these two leaves,
        // whichever is pushed *last* (the second one created) is popped —
        // and therefore deleted — first.
        let sibling = manager.create_node(store_id, Node::folder("Sibling"), Some(top)).await.unwrap();
        let mismatched = manager.create_node(store_id, Node::document("Mismatched"), Some(top)).await.unwrap();

        // `mismatched` stays listed under `top` (untouched), but its own
        // `parent_id` field is corrupted to name `sibling` instead.
        manager.store_document_mut(store_id).unwrap().set_parent_id(mismatched, Some(sibling)).unwrap();

        // `sibling` is deleted first, per the ordering above; `mismatched`
        // — whose stray `parent_id` names `sibling` — is deleted next, by
        // which point `sibling`'s own entry is already gone.
        let removal = tokio::time::timeout(std::time::Duration::from_secs(5), manager.delete_node(store_id, top))
            .await
            .unwrap()
            .expect("delete_node must succeed even when a node's parent_id points at an already-deleted sibling");

        let removed: HashSet<_> = removal.removed.into_iter().collect();
        assert_eq!(removed, [top, sibling, mismatched].into_iter().collect());

        let doc = manager.store_document(store_id).unwrap();
        assert!(!doc.has_node(top));
        assert!(!doc.has_node(sibling));
        assert!(!doc.has_node(mismatched));
    }

    /// The root node can never be deleted, subtree or not.
    #[tokio::test]
    async fn delete_node_refuses_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        let root_id = manager.root_node_id(store_id).unwrap();

        let err = manager.delete_node(store_id, root_id).await.expect_err("deleting the root must fail");
        assert!(matches!(err, StoreError::InvalidOperation(_)), "expected InvalidOperation, got {:?}", err);
        assert!(manager.store_document(store_id).unwrap().has_node(root_id));
    }

    /// `repair_tree` on an already well-formed tree changes nothing
    /// (docs/history/HARDENING_CONTRACT.md decision 9): in particular it must not mark the
    /// store dirty, which would force a needless flush on every reconcile.
    #[tokio::test]
    async fn repair_tree_is_a_no_op_on_a_well_formed_tree() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = StoreManager::new();
        let store_id = manager.create_local_store(dir.path().join("store.pimble"), "Store").await.unwrap();
        manager.flush(store_id).await.unwrap();

        let repair = manager.repair_tree(store_id).unwrap();
        assert!(repair.is_none());
    }
}
