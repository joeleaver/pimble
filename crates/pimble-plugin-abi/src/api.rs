//! The Pimble API across the plugin boundary (docs/PLUGINS_CONTRACT.md section
//! 3.4, wave 2): a plugin's `pimble_api::Pimble` runs over a `Backend` whose
//! every method is one [`ApiCall`] to the host, answered later by one
//! [`Resolution`] naming the call (a subscription by one [`ApiReply::Subscribed`]
//! and then a [`ApiReply::Change`] per change until [`ApiReply::Ended`]).
//!
//! These messages are JSON, not postcard like the rest of the ABI: the types
//! they carry (`Node`, `Store`, `StoreChangeKind`, ...) are the server's own
//! JSON-RPC types, built for JSON (tagged enums, skipped empty fields, JSON
//! values in a node's `custom`), and postcard cannot read those back. Bytes
//! (yrs updates, blobs) travel as base64 text, as the server's own messages
//! carry them.
//!
//! An [`ApiCall`] names no client id: the host attributes every edit to the
//! plugin (`client_id = "plugin:<id>"`), whatever the plugin would have said.

use pimble_api::{Addr, ApiError, Moved, NodeInit, StoreChange};
use pimble_core::{BlobId, BlobUrl, DeletedNode, LeftShare, LinkResolution, MountRef, MountState, Node, NodeId, Place, Store, StoreId};
use pimble_rpc::{PresenceFrame, SearchResultItem, StoreChangeKind, StorePeople};
use serde::{Deserialize, Serialize};

/// The prefix of the client id every edit a plugin makes carries, before the
/// plugin's id: what the server's notifications and logs say made it.
pub const PLUGIN_CLIENT_PREFIX: &str = "plugin:";

/// The client id a plugin's edits carry.
pub fn plugin_client_id(plugin: &str) -> String {
    format!("{PLUGIN_CLIENT_PREFIX}{plugin}")
}

/// Bytes, as base64 text in JSON.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD.decode(text).map(Bytes).map_err(serde::de::Error::custom)
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(bytes: Vec<u8>) -> Self {
        Bytes(bytes)
    }
}

/// One `Backend` method, as a plugin calls it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum ApiCall {
    ListStores,
    FetchOne { store: StoreId, node: NodeId },
    Fetch { store: StoreId, nodes: Vec<NodeId> },
    Children { store: StoreId, node: NodeId },
    Sync { store: StoreId, have: Vec<(NodeId, Bytes)>, list_unknown: bool },
    Apply { store: StoreId, node: NodeId, update: Bytes },
    CreateNode { store: StoreId, parent: NodeId, init: Init, place: Place },
    CreateAttachment { store: StoreId, host: NodeId, init: Init },
    MoveNode { store: StoreId, node: NodeId, parent: NodeId, place: Place },
    MoveNodes { store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place },
    Transplant { from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place },
    Delete { store: StoreId, node: NodeId },
    Undelete { store: StoreId, node: NodeId },
    ResolveLink { store: StoreId, node: NodeId },
    ListDeleted { store: StoreId },
    ListAttachments { store: StoreId, host: NodeId },
    ListDiscussions { store: StoreId },
    Search { query: String, stores: Vec<StoreId>, semantic: bool, limit: usize },
    PutBlob { store: StoreId, node: NodeId, mime: String, bytes: Bytes },
    GetBlob { store: StoreId, blob: BlobId },
    PublishPresence { frame: PresenceFrame },
    WhoAmI,
    /// The store's changes from now on, each as a [`ApiReply::Change`] to this
    /// call, until [`ApiReply::Ended`] (or the plugin cancels the call).
    Subscribe { store: StoreId },
    People { store: StoreId },
    CreateMount { store: StoreId, parent: NodeId, source_store: StoreId, source_node: NodeId, title: Option<String> },
    MountState { store: StoreId, node: NodeId },
}

/// Whether a call reads or writes, for the approval to judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Read,
    Write,
}

impl ApiCall {
    /// The stores a call touches, each with whether it reads or writes there.
    /// A call naming none (listing stores, who am I, a search across every
    /// store) is judged by what it answers instead.
    pub fn stores(&self) -> Vec<(StoreId, Reach)> {
        use ApiCall::*;
        use Reach::*;
        match self {
            ListStores | WhoAmI => Vec::new(),
            Search { stores, .. } => stores.iter().map(|s| (*s, Read)).collect(),
            FetchOne { store, .. }
            | Fetch { store, .. }
            | Children { store, .. }
            | Sync { store, .. }
            | ResolveLink { store, .. }
            | ListDeleted { store }
            | ListAttachments { store, .. }
            | ListDiscussions { store }
            | GetBlob { store, .. }
            | Subscribe { store }
            | People { store }
            | MountState { store, .. } => vec![(*store, Read)],
            Apply { store, .. }
            | CreateNode { store, .. }
            | CreateAttachment { store, .. }
            | MoveNode { store, .. }
            | MoveNodes { store, .. }
            | Delete { store, .. }
            | Undelete { store, .. }
            | PutBlob { store, .. } => vec![(*store, Write)],
            Transplant { from, to, .. } => vec![(*from, Write), (*to, Write)],
            CreateMount { store, source_store, .. } => vec![(*store, Write), (*source_store, Read)],
            // A presence frame says where the plugin is: it changes nothing,
            // but it is about a store, which must be one the plugin may read.
            PublishPresence { frame } => vec![(frame.store_id, Read)],
        }
    }
}

/// A new node's first state (`pimble_api::NodeInit`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Init {
    pub node_type: String,
    pub title: String,
    pub content: Option<Bytes>,
    pub custom: Vec<(String, serde_json::Value)>,
}

impl From<NodeInit> for Init {
    fn from(i: NodeInit) -> Self {
        Init { node_type: i.node_type, title: i.title, content: i.content.map(Bytes), custom: i.custom }
    }
}

impl From<Init> for NodeInit {
    fn from(i: Init) -> Self {
        NodeInit { node_type: i.node_type, title: i.title, content: i.content.map(|b| b.0), custom: i.custom }
    }
}

/// One document's answer to a reconcile (`pimble_api::SyncedDoc`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Synced {
    pub node: NodeId,
    pub diff: Bytes,
    pub state_vector: Bytes,
}

/// What a move answers (`pimble_api::Moved`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MovedWire {
    pub node: NodeId,
    pub left_shares: Vec<LeftShare>,
}

impl From<Moved> for MovedWire {
    fn from(m: Moved) -> Self {
        MovedWire { node: m.node, left_shares: m.left_shares }
    }
}

impl From<MovedWire> for Moved {
    fn from(m: MovedWire) -> Self {
        Moved { node: m.node, left_shares: m.left_shares }
    }
}

/// One change to a store (`pimble_api::StoreChange`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeWire {
    pub store: StoreId,
    pub kind: StoreChangeKind,
    pub source: Option<String>,
    pub update: Option<Bytes>,
}

impl From<StoreChange> for ChangeWire {
    fn from(c: StoreChange) -> Self {
        ChangeWire { store: c.store, kind: c.kind, source: c.source, update: c.update.map(Bytes) }
    }
}

impl From<ChangeWire> for StoreChange {
    fn from(c: ChangeWire) -> Self {
        StoreChange { store: c.store, kind: c.kind, source: c.source, update: c.update.map(|b| b.0) }
    }
}

/// A node by its store and id (`pimble_api::Addr`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddrWire {
    pub store: StoreId,
    pub node: NodeId,
}

/// What went wrong (`pimble_api::ApiError`), variant for variant, so the
/// plugin reads the same error the app would have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum ErrorWire {
    ReadOnly,
    CommentOnly,
    NoGrant { sentence: String },
    NotFound { sentence: String },
    Ambiguous { sentence: String, candidates: Vec<AddrWire> },
    Deleted { addr: AddrWire },
    Locked { sentence: String },
    Refused { sentence: String },
    EncryptedStore { sentence: String },
    NotAvailable { sentence: String },
    Offline { sentence: String },
    OwnerOffline { sentence: String },
    Server { code: i32, message: String },
    Failed { sentence: String },
}

impl From<ApiError> for ErrorWire {
    fn from(e: ApiError) -> Self {
        let addr = |a: Addr| AddrWire { store: a.store, node: a.node };
        match e {
            ApiError::ReadOnly => ErrorWire::ReadOnly,
            ApiError::CommentOnly => ErrorWire::CommentOnly,
            ApiError::NoGrant { sentence } => ErrorWire::NoGrant { sentence },
            ApiError::NotFound { sentence } => ErrorWire::NotFound { sentence },
            ApiError::Ambiguous { sentence, candidates } => {
                ErrorWire::Ambiguous { sentence, candidates: candidates.into_iter().map(addr).collect() }
            }
            ApiError::Deleted { addr: a } => ErrorWire::Deleted { addr: addr(a) },
            ApiError::Locked { sentence } => ErrorWire::Locked { sentence },
            ApiError::Refused { sentence } => ErrorWire::Refused { sentence },
            ApiError::EncryptedStore { sentence } => ErrorWire::EncryptedStore { sentence },
            ApiError::NotAvailable { sentence } => ErrorWire::NotAvailable { sentence },
            ApiError::Offline { sentence } => ErrorWire::Offline { sentence },
            ApiError::OwnerOffline { sentence } => ErrorWire::OwnerOffline { sentence },
            ApiError::Server { code, message } => ErrorWire::Server { code, message },
            ApiError::Failed { sentence } => ErrorWire::Failed { sentence },
        }
    }
}

impl From<ErrorWire> for ApiError {
    fn from(e: ErrorWire) -> Self {
        let addr = |a: AddrWire| Addr::new(a.store, a.node);
        match e {
            ErrorWire::ReadOnly => ApiError::ReadOnly,
            ErrorWire::CommentOnly => ApiError::CommentOnly,
            ErrorWire::NoGrant { sentence } => ApiError::NoGrant { sentence },
            ErrorWire::NotFound { sentence } => ApiError::NotFound { sentence },
            ErrorWire::Ambiguous { sentence, candidates } => {
                ApiError::Ambiguous { sentence, candidates: candidates.into_iter().map(addr).collect() }
            }
            ErrorWire::Deleted { addr: a } => ApiError::Deleted { addr: addr(a) },
            ErrorWire::Locked { sentence } => ApiError::Locked { sentence },
            ErrorWire::Refused { sentence } => ApiError::Refused { sentence },
            ErrorWire::EncryptedStore { sentence } => ApiError::EncryptedStore { sentence },
            ErrorWire::NotAvailable { sentence } => ApiError::NotAvailable { sentence },
            ErrorWire::Offline { sentence } => ApiError::Offline { sentence },
            ErrorWire::OwnerOffline { sentence } => ApiError::OwnerOffline { sentence },
            ErrorWire::Server { code, message } => ApiError::Server { code, message },
            ErrorWire::Failed { sentence } => ApiError::Failed { sentence },
        }
    }
}

/// The host's answer to an [`ApiCall`], by the shape the `Backend` method
/// answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum ApiReply {
    Done,
    Stores { stores: Vec<Store> },
    Node { node: Node },
    Nodes { nodes: Vec<Node> },
    Children { store: StoreId, nodes: Vec<Node> },
    Synced { docs: Vec<Synced>, unknown: Vec<NodeId> },
    Id { node: NodeId },
    Moved { moved: MovedWire },
    MovedMany { moved: Vec<MovedWire> },
    Link { resolution: LinkResolution },
    Deleted { nodes: Vec<DeletedNode> },
    Results { items: Vec<SearchResultItem> },
    BlobUrl { url: BlobUrl },
    Blob { mime: String, bytes: Bytes },
    Me { sub: Option<String>, name: String },
    People { people: StorePeople },
    Mounted { node: NodeId, mount: MountRef },
    MountState { state: MountState, mount: MountRef },
    /// A subscription was made; its changes follow under the same call.
    Subscribed,
    Change { change: ChangeWire },
    /// The subscription ended (the connection did, or the plugin's approval
    /// no longer covers the store); the plugin's API makes it again.
    Ended,
    Error { error: ErrorWire },
}

impl ApiReply {
    pub fn error(e: ApiError) -> Self {
        ApiReply::Error { error: e.into() }
    }
}

/// What `pimble_resolve` is handed: an answer to one of the plugin's calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub call: u64,
    pub reply: ApiReply,
}

/// An API message as bytes.
pub fn encode_json<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("every API message encodes as JSON")
}

/// An API message from bytes, or why they are not one.
pub fn decode_json<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::StoreAccess;

    fn ids() -> (StoreId, NodeId) {
        (StoreId::new(), NodeId::new())
    }

    #[test]
    fn calls_round_trip_with_their_bytes() {
        let (store, node) = ids();
        let calls = vec![
            ApiCall::ListStores,
            ApiCall::Sync { store, have: vec![(node, Bytes(vec![0, 1, 255]))], list_unknown: true },
            ApiCall::Apply { store, node, update: Bytes(vec![1, 2, 3, 4, 5]) },
            ApiCall::CreateNode {
                store,
                parent: node,
                init: Init { node_type: "document".into(), title: "A".into(), content: None, custom: vec![("icon".into(), serde_json::json!("star"))] },
                place: Place::After(node),
            },
            ApiCall::MoveNodes { store, nodes: vec![node], parent: node, place: Place::First },
            ApiCall::Subscribe { store },
        ];
        for call in calls {
            assert_eq!(decode_json::<ApiCall>(&encode_json(&call)).unwrap(), call);
        }
        // Bytes are base64 text, not a list of numbers.
        let text = String::from_utf8(encode_json(&ApiCall::Apply { store, node, update: Bytes(vec![1, 2, 3]) })).unwrap();
        assert!(text.contains("\"AQID\""), "{text}");
    }

    #[test]
    fn replies_round_trip_with_nodes_changes_and_errors() {
        let (store, id) = ids();
        let mut node = Node::new("document");
        node.id = id;
        node.metadata.set_icon(Some("star".into()));
        node.content = vec![9, 8, 7];
        node.access = StoreAccess::Read;
        let reply = ApiReply::Children { store, nodes: vec![node.clone()] };
        let ApiReply::Children { nodes, .. } = decode_json::<ApiReply>(&encode_json(&reply)).unwrap() else { panic!() };
        assert_eq!(nodes[0].content, node.content);
        assert_eq!(nodes[0].access, StoreAccess::Read);
        assert_eq!(nodes[0].metadata.icon(), Some("star"));

        let change = ChangeWire {
            store,
            kind: StoreChangeKind::NodeMoved { node_id: id, old_parent_id: id, new_parent_id: id },
            source: Some(plugin_client_id("hello-viewer")),
            update: Some(Bytes(vec![4, 5])),
        };
        let ApiReply::Change { change: back } = decode_json::<ApiReply>(&encode_json(&ApiReply::Change { change })).unwrap() else { panic!() };
        let back: StoreChange = back.into();
        assert_eq!(back.update, Some(vec![4, 5]));
        assert_eq!(back.source.as_deref(), Some("plugin:hello-viewer"));

        for e in [
            ApiError::ReadOnly,
            ApiError::refused("This can't move into itself."),
            ApiError::Deleted { addr: Addr::new(store, id) },
            ApiError::Server { code: -32000, message: "Node not found".into() },
        ] {
            let wire = encode_json(&Resolution { call: 7, reply: ApiReply::error(e.clone()) });
            let Resolution { call, reply: ApiReply::Error { error } } = decode_json(&wire).unwrap() else { panic!() };
            assert_eq!(call, 7);
            assert_eq!(ApiError::from(error), e);
        }
    }
}
