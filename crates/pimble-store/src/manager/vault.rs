//! The store manager's encrypted stores (docs/CRYPTO_CONTRACT.md): each
//! vault document's log, snapshots, keys and reports, and the scope sets
//! a vault store keeps for its shares (docs/NODE_DOCUMENT_CONTRACT.md
//! section 5).

use super::*;

impl StoreManager {
    // ── Vault (encrypted store) storage, docs/CRYPTO_CONTRACT.md ────────

    /// Append `blob` to `doc_id`'s log in vault store `store_id`, returning
    /// its sequence number.
    pub async fn vault_append(&mut self, store_id: StoreId, doc_id: &str, blob: Vec<u8>) -> Result<u64> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.append(doc_id, blob).await
    }

    /// Take one part of an append sent in parts (`VaultStore::append_part`).
    pub async fn vault_append_part(&mut self, store_id: StoreId, doc_id: &str, part: crate::vault::PartMeta, blob: Vec<u8>) -> Result<crate::vault::PartAppended> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.append_part(doc_id, part, blob).await
    }

    /// Whether `part` would land its group (`VaultStore::completes_group`).
    pub fn vault_completes_group(&self, store_id: StoreId, doc_id: &str, part: crate::vault::PartMeta) -> Result<bool> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.completes_group(doc_id, part))
    }

    /// Take one part of a snapshot sent in parts (`VaultStore::snapshot_part`):
    /// whether the snapshot was stored with it.
    pub async fn vault_snapshot_part(&mut self, store_id: StoreId, doc_id: &str, upto_seq: u64, part: crate::vault::PartMeta, blob: Vec<u8>) -> Result<bool> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.snapshot_part(doc_id, upto_seq, part, blob).await
    }

    /// A document's log and snapshot sizes in bytes.
    pub fn vault_sizes(&self, store_id: StoreId, doc_id: &str) -> Result<(u64, u64)> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.sizes(doc_id))
    }

    /// The snapshot (if newer than `after_seq`), every update after
    /// `max(after_seq, snapshot.seq)`, and the document's head.
    pub async fn vault_fetch(&self, store_id: StoreId, doc_id: &str, after_seq: u64) -> Result<crate::vault::Fetched> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.fetch(doc_id, after_seq).await
    }

    /// Store a snapshot for `doc_id` covering every update up to and
    /// including `upto_seq`, dropping log entries at or below it.
    pub async fn vault_snapshot(&mut self, store_id: StoreId, doc_id: &str, upto_seq: u64, blob: Vec<u8>) -> Result<()> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.snapshot(doc_id, upto_seq, blob).await
    }

    /// Which log this is (docs/RELAY_CONTRACT.md: a relayed store's twin is
    /// derived and disposable, and one that was deleted is built again from
    /// nothing under the same store id). The vault's creation time: a twin
    /// built again has another, which is how a link that read the earlier
    /// one's logs knows its sequence numbers say nothing about these.
    pub fn vault_epoch(&self, store_id: StoreId) -> Result<String> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.manifest().created_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
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

    /// Store `device`'s state report of `doc_id` (see [`VaultStore::set_report`]).
    pub async fn vault_set_report(&mut self, store_id: StoreId, doc_id: &str, device: &str, blob: Vec<u8>, at: chrono::DateTime<chrono::Utc>, account: Option<String>) -> Result<()> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.set_report(doc_id, device, blob, at, account).await
    }

    /// Every device's latest state report of `doc_id` in vault store `store_id`.
    pub fn vault_reports(&self, store_id: StoreId, doc_id: &str) -> Result<Vec<crate::vault::StoredReport>> {
        let store = self.vault_stores.get(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        Ok(store.reports(doc_id))
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

    /// Whether a vault store's document is a discussion (its discussion set).
    pub fn vault_is_discussion(&self, store_id: StoreId, doc_id: NodeId) -> bool {
        self.vault_stores.get(&store_id).is_some_and(|s| s.is_discussion(doc_id))
    }

    /// Record discussions of a vault store (see [`VaultStore::add_discussions`]).
    pub async fn vault_add_discussions(&mut self, store_id: StoreId, discussions: Vec<(NodeId, NodeId)>) -> Result<bool> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.add_discussions(discussions).await
    }

    /// A scoped member created `doc_id` under `parent_id` (see
    /// [`VaultStore::extend_scopes`]).
    pub async fn vault_extend_scopes(&mut self, store_id: StoreId, roots: &[NodeId], parent_id: NodeId, doc_id: NodeId) -> Result<bool> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.extend_scopes(roots, parent_id, doc_id).await
    }

    /// The pictures a principal scoped to `roots` may read (see
    /// [`VaultStore::scope_blobs`]). Empty for a store that is not a vault.
    pub fn vault_scope_blobs(&self, store_id: StoreId, roots: &[NodeId]) -> HashSet<pimble_core::BlobId> {
        self.vault_stores.get(&store_id).map(|s| s.scope_blobs(roots)).unwrap_or_default()
    }

    /// Add pictures to a scope (see [`VaultStore::add_scope_blobs`]).
    pub async fn vault_add_scope_blobs(&mut self, store_id: StoreId, root: NodeId, blobs: Vec<pimble_core::BlobId>) -> Result<bool> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.add_scope_blobs(root, blobs).await
    }

    /// A scoped member uploaded a picture for a node (see
    /// [`VaultStore::extend_scope_blobs`]).
    pub async fn vault_extend_scope_blobs(&mut self, store_id: StoreId, roots: &[NodeId], node_id: NodeId, blob: pimble_core::BlobId) -> Result<bool> {
        let store = self.vault_stores.get_mut(&store_id).ok_or(StoreError::NotOpen(store_id))?;
        store.extend_scope_blobs(roots, node_id, blob).await
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
        self.disk.remove_dir_all(&store.path).await?;
        info!("Deleted vault store {} at {:?}", store_id, store.path);
        Ok(())
    }
}
