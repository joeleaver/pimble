//! What an implementation of the API must do: transport only
//! (docs/API_CONTRACT.md section 3b). Everything that judges, caches or
//! builds lives above it in [`crate::Pimble`], so the RPC implementation
//! here and the browser's vault (wave 4) give every caller the same answers.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use pimble_core::{BlobId, BlobUrl, DeletedNode, LeftShare, LinkResolution, Node, NodeId, Place, Store, StoreId};
use pimble_rpc::{PresenceFrame, SearchResultItem, StoreChangeKind};

use crate::ApiError;

/// `Send` where futures cross threads (native), nothing in the browser,
/// where nothing does and the JavaScript-backed types are not `Send`.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// `Sync` natively, nothing in the browser (see [`MaybeSend`]).
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync + ?Sized> MaybeSync for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSync for T {}

/// One change to a store, as its subscription says it: what changed, who
/// made it, and the update to the one node document it is about (yrs v1,
/// decoded), which is what keeps held documents current without refetching.
#[derive(Debug, Clone)]
pub struct StoreChange {
    pub store: StoreId,
    pub kind: StoreChangeKind,
    pub source: Option<String>,
    pub update: Option<Vec<u8>>,
}

/// A store's changes, for as long as the subscription lives. It ends when
/// the connection does; the API then subscribes again and brings what it
/// holds up to date.
#[cfg(not(target_arch = "wasm32"))]
pub type ChangeStream = Pin<Box<dyn Stream<Item = StoreChange> + Send>>;
#[cfg(target_arch = "wasm32")]
pub type ChangeStream = Pin<Box<dyn Stream<Item = StoreChange>>>;

/// What a move answers: the id the node has now (a new one when it left a
/// share or its store, docs/MOVE_CONTRACT.md) and the shares it left.
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub node: NodeId,
    pub left_shares: Vec<LeftShare>,
}

/// One document's answer to a reconcile: what this side lacks, and the
/// other side's state vector.
#[derive(Debug, Clone)]
pub struct SyncedDoc {
    pub node: NodeId,
    pub diff: Vec<u8>,
    pub state_vector: Vec<u8>,
}

/// A new node's first state, written in the one edit that creates it:
/// its type, a yrs update merged into its document, and `custom` metadata
/// (icon, `explicit_title`). [`crate::recipes`] makes these.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NodeInit {
    pub node_type: String,
    pub title: String,
    pub content: Option<Vec<u8>>,
    pub custom: Vec<(String, serde_json::Value)>,
}

/// What a store can do where a backend reaches it (docs/API_CONTRACT.md
/// decision 10). Said rather than faked: a caller offers what works and
/// explains what does not, instead of reading an answer made up to look
/// like success (the browser's vault once answered "index rebuilt, 0
/// nodes" for an index that does not exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreCapabilities {
    /// The store's content is ciphertext wherever it is not decrypted by
    /// this client; nothing moves between it and a plain store.
    pub encrypted: bool,
    /// A mount can be made in it.
    pub mounts: bool,
    /// Pictures can be stored in it and shown from it (blobs).
    pub pictures: bool,
    /// It can be linked to a store on another server and kept in step.
    pub sync_links: bool,
    /// Search may be asked to find by meaning as well as by words.
    pub semantic_search: bool,
    /// It has a search index that can be rebuilt.
    pub index_rebuild: bool,
    /// The backend holds the store's documents itself, in this process,
    /// and answers every read from them (the browser's vault holds the
    /// decrypted documents of an encrypted store). [`crate::Pimble`] then
    /// holds no second copy: it asks the backend each time, so there is one
    /// holder and nothing to disagree with it (docs/API_CONTRACT.md wave 5).
    pub local_documents: bool,
}

impl StoreCapabilities {
    /// A store a Pimble server holds: everything is there to ask for, and
    /// the server answers for itself (a search without its model falls
    /// back to words; a sync link is for a principal allowed to make one).
    pub const SERVER: StoreCapabilities = StoreCapabilities {
        encrypted: false,
        mounts: true,
        pictures: true,
        sync_links: true,
        semantic_search: true,
        index_rebuild: true,
        local_documents: false,
    };
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait Backend: MaybeSend + MaybeSync {
    /// What `store` can do through this backend. A Pimble server's stores
    /// can do everything it offers, which is the answer unless a backend
    /// says otherwise.
    fn capabilities(&self, _store: StoreId) -> StoreCapabilities {
        StoreCapabilities::SERVER
    }

    /// The stores this principal may read.
    async fn list_stores(&self) -> Result<Vec<Store>, ApiError>;

    /// One live node, whole (its document as the content), with the
    /// access the server judges and its attachment count.
    async fn fetch_one(&self, store: StoreId, node: NodeId) -> Result<Node, ApiError>;

    /// Several live nodes, whole; one that is not there (deleted, outside
    /// the principal's scope, never existed) is left out.
    async fn fetch(&self, store: StoreId, nodes: Vec<NodeId>) -> Result<Vec<Node>, ApiError>;

    /// A node's children, whole, through a mount to its source: the store
    /// they live in, and them.
    async fn children(&self, store: StoreId, node: NodeId) -> Result<(StoreId, Vec<Node>), ApiError>;

    /// The stateless reconcile: for each `(node, state vector)` what this
    /// side lacks; with `list_unknown`, also the ids not named (everything
    /// a principal may read that this side does not hold). A tombstone is a
    /// document like any other; a document not there is left out.
    async fn sync(&self, store: StoreId, have: Vec<(NodeId, Vec<u8>)>, list_unknown: bool) -> Result<(Vec<SyncedDoc>, Vec<NodeId>), ApiError>;

    /// Merge `update` into a node's document: the one write.
    async fn apply(&self, store: StoreId, node: NodeId, update: Vec<u8>, client_id: &str) -> Result<(), ApiError>;

    async fn create_node(&self, store: StoreId, parent: NodeId, init: NodeInit, place: Place) -> Result<NodeId, ApiError>;
    async fn create_attachment(&self, store: StoreId, host: NodeId, init: NodeInit) -> Result<NodeId, ApiError>;
    async fn move_node(&self, store: StoreId, node: NodeId, parent: NodeId, place: Place) -> Result<Moved, ApiError>;
    async fn move_nodes(&self, store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place) -> Result<Vec<Moved>, ApiError>;
    async fn transplant(&self, from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place) -> Result<Moved, ApiError>;
    async fn delete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError>;
    async fn undelete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError>;

    /// One resolution of a link by whoever holds its store: `became` hops
    /// inside it followed, `StoreNotHere` where the chain left it.
    async fn resolve_link(&self, store: StoreId, node: NodeId) -> Result<LinkResolution, ApiError>;
    async fn list_deleted(&self, store: StoreId) -> Result<Vec<DeletedNode>, ApiError>;
    async fn list_attachments(&self, store: StoreId, host: NodeId) -> Result<Vec<Node>, ApiError>;

    /// Every live discussion of a store whose host is live (or that has
    /// none), each whole. Said not to be there by a backend that cannot list
    /// them.
    async fn list_discussions(&self, _store: StoreId) -> Result<Vec<Node>, ApiError> {
        Err(ApiError::not_available("Listing a store's discussions is not available here."))
    }
    async fn search(&self, query: &str, stores: Vec<StoreId>, semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError>;
    async fn put_blob(&self, store: StoreId, node: NodeId, mime: &str, bytes: &[u8]) -> Result<BlobUrl, ApiError>;
    async fn get_blob(&self, store: StoreId, blob: BlobId) -> Result<(String, Vec<u8>), ApiError>;
    async fn publish_presence(&self, frame: PresenceFrame, client_id: &str) -> Result<(), ApiError>;

    /// Who this connection is: `(sub, name)`, the sub absent for the
    /// server's own operator.
    async fn who_am_i(&self) -> Result<(Option<String>, String), ApiError>;

    /// A store's changes from now on.
    async fn subscribe(&self, store: StoreId) -> Result<ChangeStream, ApiError>;
}

/// A backend shared with whoever else needs it (the browser's loop keeps
/// the [`crate::Router`] it routes through, to tell it which backend serves
/// a store and what the account may change there).
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl<B: Backend + ?Sized> Backend for std::sync::Arc<B> {
    fn capabilities(&self, store: StoreId) -> StoreCapabilities {
        (**self).capabilities(store)
    }

    async fn list_stores(&self) -> Result<Vec<Store>, ApiError> {
        (**self).list_stores().await
    }

    async fn fetch_one(&self, store: StoreId, node: NodeId) -> Result<Node, ApiError> {
        (**self).fetch_one(store, node).await
    }

    async fn fetch(&self, store: StoreId, nodes: Vec<NodeId>) -> Result<Vec<Node>, ApiError> {
        (**self).fetch(store, nodes).await
    }

    async fn children(&self, store: StoreId, node: NodeId) -> Result<(StoreId, Vec<Node>), ApiError> {
        (**self).children(store, node).await
    }

    async fn sync(&self, store: StoreId, have: Vec<(NodeId, Vec<u8>)>, list_unknown: bool) -> Result<(Vec<SyncedDoc>, Vec<NodeId>), ApiError> {
        (**self).sync(store, have, list_unknown).await
    }

    async fn apply(&self, store: StoreId, node: NodeId, update: Vec<u8>, client_id: &str) -> Result<(), ApiError> {
        (**self).apply(store, node, update, client_id).await
    }

    async fn create_node(&self, store: StoreId, parent: NodeId, init: NodeInit, place: Place) -> Result<NodeId, ApiError> {
        (**self).create_node(store, parent, init, place).await
    }

    async fn create_attachment(&self, store: StoreId, host: NodeId, init: NodeInit) -> Result<NodeId, ApiError> {
        (**self).create_attachment(store, host, init).await
    }

    async fn move_node(&self, store: StoreId, node: NodeId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        (**self).move_node(store, node, parent, place).await
    }

    async fn move_nodes(&self, store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place) -> Result<Vec<Moved>, ApiError> {
        (**self).move_nodes(store, nodes, parent, place).await
    }

    async fn transplant(&self, from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        (**self).transplant(from, node, to, parent, place).await
    }

    async fn delete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        (**self).delete(store, node).await
    }

    async fn undelete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        (**self).undelete(store, node).await
    }

    async fn resolve_link(&self, store: StoreId, node: NodeId) -> Result<LinkResolution, ApiError> {
        (**self).resolve_link(store, node).await
    }

    async fn list_deleted(&self, store: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
        (**self).list_deleted(store).await
    }

    async fn list_attachments(&self, store: StoreId, host: NodeId) -> Result<Vec<Node>, ApiError> {
        (**self).list_attachments(store, host).await
    }

    async fn list_discussions(&self, store: StoreId) -> Result<Vec<Node>, ApiError> {
        (**self).list_discussions(store).await
    }

    async fn search(&self, query: &str, stores: Vec<StoreId>, semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError> {
        (**self).search(query, stores, semantic, limit).await
    }

    async fn put_blob(&self, store: StoreId, node: NodeId, mime: &str, bytes: &[u8]) -> Result<BlobUrl, ApiError> {
        (**self).put_blob(store, node, mime, bytes).await
    }

    async fn get_blob(&self, store: StoreId, blob: BlobId) -> Result<(String, Vec<u8>), ApiError> {
        (**self).get_blob(store, blob).await
    }

    async fn publish_presence(&self, frame: PresenceFrame, client_id: &str) -> Result<(), ApiError> {
        (**self).publish_presence(frame, client_id).await
    }

    async fn who_am_i(&self) -> Result<(Option<String>, String), ApiError> {
        (**self).who_am_i().await
    }

    async fn subscribe(&self, store: StoreId) -> Result<ChangeStream, ApiError> {
        (**self).subscribe(store).await
    }
}

/// The node document a change is about, when it is about one: the kinds a
/// store's link, mounts or shares cause are not a document's. A longer
/// `TreeStructure` list is about its first, like the bytes it carries.
pub fn document_of(kind: &StoreChangeKind) -> Option<NodeId> {
    match kind {
        StoreChangeKind::NodeCreated { node_id, .. }
        | StoreChangeKind::NodeDeleted { node_id, .. }
        | StoreChangeKind::NodeMoved { node_id, .. }
        | StoreChangeKind::MetadataUpdated { node_id }
        | StoreChangeKind::AttachmentChanged { node_id, .. }
        | StoreChangeKind::ContentUpdated { node_id } => Some(*node_id),
        StoreChangeKind::TreeStructure { node_ids } => node_ids.first().copied(),
        StoreChangeKind::SyncStateChanged { .. }
        | StoreChangeKind::MountStateChanged { .. }
        | StoreChangeKind::ShareStateChanged { .. }
        | StoreChangeKind::SharesEnded { .. }
        | StoreChangeKind::PeopleChanged
        | StoreChangeKind::VaultAppended { .. } => None,
    }
}
