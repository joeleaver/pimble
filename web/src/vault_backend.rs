//! The browser's encrypted stores as a [`pimble_api::Backend`]
//! (docs/API_CONTRACT.md wave 4).
//!
//! The vault client (`crate::vault`) already is what decision 3 asks of a
//! backend: a `pimble_crdt::Tree` of the documents this page holds, fed by
//! `vaultFetch` and the store's subscription, and a transport (`vaultAppend`)
//! that every write goes through encrypted. This puts it behind the same
//! trait the RPC backend implements, so a [`pimble_api::Router`] can choose
//! between the two per store and every caller gets the same answers and the
//! same [`ApiError`]s from either (the conformance suite, below, checks it).
//!
//! The backend loop (`crate::backend`) owns the vault's life (connecting,
//! opening, pulling, keys, repairs) and reaches it through
//! [`VaultBackend::shared`]; this answers the calls. Neither holds the vault
//! while the other is working: the lock is taken per call.
//!
//! What a vault store cannot do here is said as capabilities
//! ([`CAPABILITIES`]) and refused as `NotAvailable`, never faked.

use std::sync::Arc;

use async_trait::async_trait;
use futures::lock::Mutex;
use pimble_api::backend::{ChangeStream, SyncedDoc};
use pimble_api::{ApiError, Backend, MaybeSend, MaybeSync, Moved, NodeInit, StoreCapabilities};
use pimble_core::{BlobId, BlobUrl, DeletedNode, LinkResolution, Node, NodeId, Place, Store, StoreId};
use pimble_rpc::{PresenceFrame, SearchResultItem};

use crate::vault::{VaultClient, VaultWire};

/// What an encrypted store can do in the browser: its words are searched in
/// the page, so there is no index to rebuild and no meaning to search by;
/// pictures in ciphertext, mounts and sync links are not built for it.
pub const CAPABILITIES: StoreCapabilities = StoreCapabilities {
    encrypted: true,
    mounts: false,
    pictures: false,
    sync_links: false,
    semantic_search: false,
    index_rebuild: false,
    // The vault holds the store's decrypted documents and is the one holder
    // of them in this page: the API asks it every time and keeps no copy
    // that could disagree (docs/API_CONTRACT.md wave 5).
    local_documents: true,
};

/// Which connection serves a store, as the backend loop last saw them.
pub trait WireTable: MaybeSend + MaybeSync {
    /// The connected wire to the endpoint that serves `store`; `None` while
    /// it is down.
    fn wire(&self, store: StoreId) -> Option<Arc<dyn VaultWire>>;
}

pub struct VaultBackend {
    vault: Arc<Mutex<VaultClient>>,
    wires: Arc<dyn WireTable>,
    /// What the writes made through this backend are attributed to: `None`
    /// for the app's own (its commands show what they changed themselves),
    /// a plugin's for [`VaultBackend::for_plugins`].
    writer: Option<String>,
}

/// The vault, held for one call through the API, with who is writing set for
/// its length. Letting it go hands every listener what the call changed
/// (`VaultClient::tell_watchers`), so a plugin hears its own writes and the
/// app's at once rather than at the backend loop's next pass.
struct Held<'a> {
    vault: futures::lock::MutexGuard<'a, VaultClient>,
}

impl std::ops::Deref for Held<'_> {
    type Target = VaultClient;
    fn deref(&self) -> &VaultClient {
        &self.vault
    }
}

impl std::ops::DerefMut for Held<'_> {
    fn deref_mut(&mut self) -> &mut VaultClient {
        &mut self.vault
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.vault.writer = None;
        self.vault.tell_watchers();
    }
}

impl VaultBackend {
    pub fn new(vault: VaultClient, wires: Arc<dyn WireTable>) -> Self {
        Self { vault: Arc::new(Mutex::new(vault)), wires, writer: None }
    }

    /// The same vault, for plugins' calls (docs/PLUGINS_CONTRACT.md wave 3):
    /// what they write is attributed to them and shown to the app's views,
    /// which no command of the app's own told of it.
    pub fn for_plugins(&self) -> Self {
        Self { vault: self.vault.clone(), wires: self.wires.clone(), writer: Some(PLUGINS.to_string()) }
    }

    /// The vault itself, for the backend loop.
    pub fn shared(&self) -> Arc<Mutex<VaultClient>> {
        self.vault.clone()
    }

    /// The vault for one call, its writes attributed to `writer` when this
    /// backend is a plugin's.
    async fn lock_as(&self, writer: Option<&str>) -> Held<'_> {
        let mut vault = self.vault.lock().await;
        vault.writer = self.writer.as_ref().map(|plugins| writer.unwrap_or(plugins).to_string());
        Held { vault }
    }

    async fn lock(&self) -> Held<'_> {
        self.lock_as(None).await
    }

    fn wire(&self, store: StoreId) -> Result<Arc<dyn VaultWire>, ApiError> {
        self.wires.wire(store).ok_or_else(|| ApiError::Offline {
            sentence: "The server that holds this encrypted store is not connected; try again in a moment.".into(),
        })
    }
}

/// What a plugin's writes are attributed to when a call names no client id
/// (only `apply` carries the plugin's own, `plugin:<id>`).
const PLUGINS: &str = "plugin";

fn not_here(what: &str) -> ApiError {
    ApiError::not_available(format!("{what} is not available on an encrypted store in the browser yet."))
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl Backend for VaultBackend {
    fn capabilities(&self, _store: StoreId) -> StoreCapabilities {
        CAPABILITIES
    }

    /// The encrypted stores this page has open, as the UI is told them.
    async fn list_stores(&self) -> Result<Vec<Store>, ApiError> {
        Ok(self.lock().await.held_stores())
    }

    async fn fetch_one(&self, store: StoreId, node: NodeId) -> Result<Node, ApiError> {
        self.lock().await.node(store, node)
    }

    async fn fetch(&self, store: StoreId, nodes: Vec<NodeId>) -> Result<Vec<Node>, ApiError> {
        self.lock().await.nodes(store, &nodes)
    }

    /// No mounts in an encrypted store: a node's children are in its own.
    async fn children(&self, store: StoreId, node: NodeId) -> Result<(StoreId, Vec<Node>), ApiError> {
        Ok((store, self.lock().await.children(store, node)?))
    }

    async fn sync(&self, store: StoreId, have: Vec<(NodeId, Vec<u8>)>, list_unknown: bool) -> Result<(Vec<SyncedDoc>, Vec<NodeId>), ApiError> {
        self.lock().await.sync_docs(store, &have, list_unknown)
    }

    /// Merged here, encrypted, appended; kept for later with no connection
    /// to send it through (`VaultClient::apply`).
    async fn apply(&self, store: StoreId, node: NodeId, update: Vec<u8>, client_id: &str) -> Result<(), ApiError> {
        let wire = self.wires.wire(store);
        self.lock_as(Some(client_id)).await.apply(wire.as_deref(), store, node, &update).await
    }

    async fn create_node(&self, store: StoreId, parent: NodeId, init: NodeInit, place: Place) -> Result<NodeId, ApiError> {
        let wire = self.wire(store)?;
        self.lock().await.create_node(&*wire, store, parent, init, place).await
    }

    async fn create_attachment(&self, store: StoreId, host: NodeId, init: NodeInit) -> Result<NodeId, ApiError> {
        let wire = self.wire(store)?;
        self.lock().await.create_attachment(&*wire, store, host, init).await
    }

    async fn move_node(&self, store: StoreId, node: NodeId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        let wire = self.wire(store)?;
        self.lock().await.move_node(&*wire, store, node, parent, place).await
    }

    async fn move_nodes(&self, store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place) -> Result<Vec<Moved>, ApiError> {
        let wire = self.wire(store)?;
        self.lock().await.move_nodes(&*wire, store, &nodes, parent, place).await
    }

    /// Between two encrypted stores this page holds, each through the
    /// endpoint that serves it.
    async fn transplant(&self, from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        let from_wire = self.wire(from)?;
        let to_wire = self.wire(to)?;
        self.lock().await.transplant_node(&*from_wire, &*to_wire, from, node, to, parent, place).await
    }

    async fn delete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        let wire = self.wire(store)?;
        self.lock().await.delete_node(&*wire, store, node).await
    }

    async fn undelete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        let wire = self.wire(store)?;
        self.lock().await.undelete_node(&*wire, store, node).await
    }

    async fn resolve_link(&self, store: StoreId, node: NodeId) -> Result<LinkResolution, ApiError> {
        Ok(self.lock().await.resolve_link(store, node))
    }

    async fn list_deleted(&self, store: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
        self.lock().await.list_deleted(store)
    }

    async fn list_attachments(&self, store: StoreId, host: NodeId) -> Result<Vec<Node>, ApiError> {
        self.lock().await.list_attachments(store, host)
    }

    async fn list_discussions(&self, store: StoreId) -> Result<Vec<Node>, ApiError> {
        self.lock().await.list_discussions(store)
    }

    /// Words, in the page: there is no index anywhere for an encrypted
    /// store, so `semantic` changes nothing (its capabilities say so).
    async fn search(&self, query: &str, stores: Vec<StoreId>, _semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError> {
        Ok(self.lock().await.search(query, &stores, limit))
    }

    /// From the documents this page holds: no index anywhere.
    async fn backlinks(&self, store: StoreId, node: NodeId) -> Result<Vec<NodeId>, ApiError> {
        Ok(self.lock().await.backlinks(store, node))
    }

    async fn put_blob(&self, _store: StoreId, _node: NodeId, _mime: &str, _bytes: &[u8]) -> Result<BlobUrl, ApiError> {
        Err(ApiError::not_available(pimble_app::pictures::ENCRYPTED_IN_BROWSER))
    }

    async fn get_blob(&self, _store: StoreId, _blob: BlobId) -> Result<(String, Vec<u8>), ApiError> {
        Err(ApiError::not_available(pimble_app::pictures::ENCRYPTED_IN_BROWSER))
    }

    // What follows is not this backend's to answer through the trait. A
    // presence frame is sealed under a document's key by the backend loop
    // (`VaultClient::publish_presence`), and who is signed in is the
    // accounts service's (`crate::accounts::me`).

    async fn publish_presence(&self, _frame: PresenceFrame, _client_id: &str) -> Result<(), ApiError> {
        Err(not_here("Presence through the API"))
    }

    async fn who_am_i(&self) -> Result<(Option<String>, String), ApiError> {
        Err(not_here("Asking who is signed in through the API"))
    }

    /// Every change of the store's documents, each with its update. The
    /// app's own `Pimble` never asks: the vault is the one holder of an
    /// encrypted store's documents in this page
    /// (`CAPABILITIES.local_documents`), so it keeps no copy, and the backend
    /// loop pumps what the vault decrypts as the app's events itself. A
    /// plugin's does: it is another process, which holds copies of its own
    /// (docs/PLUGINS_CONTRACT.md wave 3).
    async fn subscribe(&self, store: StoreId) -> Result<ChangeStream, ApiError> {
        Ok(Box::pin(self.lock().await.watch(store)))
    }
}

#[cfg(test)]
mod tests;
