//! The Pimble API in a plugin (docs/PLUGINS_CONTRACT.md section 3.4, wave 2):
//! [`GuestBackend`] is a `pimble_api::Backend` whose every method is a call to the
//! host, so a plugin runs `pimble_api::Pimble` over it and uses the API exactly as
//! the app does: held documents, the one write, `plan`, `metanode_source`, the
//! change stream.
//!
//! Each call goes out as an [`ApiCall`] under a call id the guest picks
//! ([`ApiLink::call`]); the host checks it against what the person approved for
//! the plugin, forwards it to the app's own backend (so the server judges it with
//! the person's role, and nothing more), and answers later through the guest's
//! `pimble_resolve` export, which lands in [`Bridge::resolve`] and wakes the
//! future that waited. A subscription is one call answered many times: once to
//! say it was made, then once per change.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::channel::{mpsc, oneshot};
use futures::Stream;
use pimble_api::backend::{ChangeStream, SyncedDoc};
use pimble_api::{ApiError, Backend, MaybeSend, MaybeSync, Moved, NodeInit, StoreChange};
use pimble_core::{BlobId, BlobUrl, DeletedNode, LinkResolution, MountRef, MountState, Node, NodeId, Place, Store, StoreId};
use pimble_plugin_abi::api::{encode_json, ApiCall, ApiReply, Bytes, Resolution};
use pimble_rpc::{PresenceFrame, SearchResultItem, StorePeople};

/// How a guest's API calls reach the host. In a plugin this is the
/// `pimble_api_call` and `pimble_api_cancel` imports; a test answers them itself.
pub trait ApiLink: MaybeSend + MaybeSync {
    /// Send `call` (an [`ApiCall`] as JSON) under id `id`. Answers nothing now:
    /// the answer comes back through [`Bridge::resolve`].
    fn call(&self, id: u64, call: Vec<u8>);
    /// The guest wants no more answers to call `id`.
    fn cancel(&self, id: u64);
}

/// Who waits for a call's answer.
enum Waiting {
    /// One answer.
    Once(oneshot::Sender<ApiReply>),
    /// A subscription: the first answer says whether it was made, and every
    /// change after it goes to the stream.
    Stream { made: Option<oneshot::Sender<ApiReply>>, changes: mpsc::UnboundedSender<StoreChange> },
}

/// The calls a guest has made and not had answered.
pub struct Bridge {
    link: Box<dyn ApiLink>,
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, Waiting>>,
}

impl Bridge {
    pub fn new(link: impl ApiLink + 'static) -> Arc<Bridge> {
        Arc::new(Bridge { link: Box::new(link), next: AtomicU64::new(1), waiting: Mutex::new(HashMap::new()) })
    }

    /// The calls not answered yet (tests).
    pub fn outstanding(&self) -> usize {
        self.waiting.lock().unwrap().len()
    }

    /// The host answered call `resolution.call`: wake whoever waits for it. An
    /// answer to a call nobody waits for any more (a subscription dropped, a
    /// future given up) is dropped.
    pub fn resolve(&self, resolution: Resolution) {
        let Resolution { call, reply } = resolution;
        let mut waiting = self.waiting.lock().unwrap();
        match waiting.remove(&call) {
            None => {}
            Some(Waiting::Once(tx)) => {
                let _ = tx.send(reply);
            }
            Some(Waiting::Stream { made, changes }) => match reply {
                ApiReply::Change { change } => {
                    if changes.unbounded_send(change.into()).is_ok() {
                        waiting.insert(call, Waiting::Stream { made, changes });
                    }
                }
                // The end of the subscription: dropping the sender ends the
                // stream, and the API makes it again.
                ApiReply::Ended => {
                    if let Some(made) = made {
                        let _ = made.send(ApiReply::Ended);
                    }
                }
                ApiReply::Error { error } => {
                    if let Some(made) = made {
                        let _ = made.send(ApiReply::Error { error });
                    }
                }
                other => {
                    if let Some(made) = made {
                        let _ = made.send(other);
                    }
                    waiting.insert(call, Waiting::Stream { made: None, changes });
                }
            },
        }
    }

    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Make `call` and wait for its one answer.
    async fn ask(&self, call: ApiCall) -> Result<ApiReply, ApiError> {
        let id = self.id();
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(id, Waiting::Once(tx));
        // The lock is let go before the call goes out: a host (or a test) may
        // answer from inside it.
        self.link.call(id, encode_json(&call));
        match rx.await {
            Ok(ApiReply::Error { error }) => Err(error.into()),
            Ok(reply) => Ok(reply),
            Err(_) => Err(ApiError::Offline { sentence: "The plugin's host went away before it answered.".into() }),
        }
    }

    /// Subscribe to `store`'s changes.
    async fn subscribe(self: &Arc<Self>, store: StoreId) -> Result<ChangeStream, ApiError> {
        let id = self.id();
        let (made_tx, made_rx) = oneshot::channel();
        let (changes_tx, changes_rx) = mpsc::unbounded();
        self.waiting.lock().unwrap().insert(id, Waiting::Stream { made: Some(made_tx), changes: changes_tx });
        self.link.call(id, encode_json(&ApiCall::Subscribe { store }));
        let stream = Changes { id, rx: changes_rx, bridge: self.clone() };
        match made_rx.await {
            Ok(ApiReply::Subscribed) => Ok(Box::pin(stream)),
            Ok(ApiReply::Error { error }) => Err(error.into()),
            Ok(ApiReply::Ended) => Err(ApiError::Offline { sentence: "The subscription ended before it was made.".into() }),
            Ok(other) => Err(unexpected(&other)),
            Err(_) => Err(ApiError::Offline { sentence: "The plugin's host went away before it answered.".into() }),
        }
    }

    fn cancel(&self, id: u64) {
        if self.waiting.lock().unwrap().remove(&id).is_some() {
            self.link.cancel(id);
        }
    }
}

/// A subscription's changes; dropping it tells the host to stop sending them.
struct Changes {
    id: u64,
    rx: mpsc::UnboundedReceiver<StoreChange>,
    bridge: Arc<Bridge>,
}

impl Stream for Changes {
    type Item = StoreChange;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<StoreChange>> {
        Pin::new(&mut self.rx).poll_next(cx)
    }
}

impl Drop for Changes {
    fn drop(&mut self) {
        self.bridge.cancel(self.id);
    }
}

fn unexpected(reply: &ApiReply) -> ApiError {
    ApiError::Failed { sentence: format!("The host answered a plugin's call with something else ({reply:?}).") }
}

/// A `pimble_api::Backend` over the host: what a plugin's `Pimble` runs on.
#[derive(Clone)]
pub struct GuestBackend(pub Arc<Bridge>);

impl GuestBackend {
    pub fn new(bridge: Arc<Bridge>) -> Self {
        GuestBackend(bridge)
    }
}

macro_rules! expect {
    ($reply:expr, $pat:pat => $out:expr) => {
        match $reply {
            $pat => Ok($out),
            other => Err(unexpected(&other)),
        }
    };
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl Backend for GuestBackend {
    async fn list_stores(&self) -> Result<Vec<Store>, ApiError> {
        expect!(self.0.ask(ApiCall::ListStores).await?, ApiReply::Stores { stores } => stores)
    }

    async fn fetch_one(&self, store: StoreId, node: NodeId) -> Result<Node, ApiError> {
        expect!(self.0.ask(ApiCall::FetchOne { store, node }).await?, ApiReply::Node { node } => node)
    }

    async fn fetch(&self, store: StoreId, nodes: Vec<NodeId>) -> Result<Vec<Node>, ApiError> {
        expect!(self.0.ask(ApiCall::Fetch { store, nodes }).await?, ApiReply::Nodes { nodes } => nodes)
    }

    async fn children(&self, store: StoreId, node: NodeId) -> Result<(StoreId, Vec<Node>), ApiError> {
        expect!(self.0.ask(ApiCall::Children { store, node }).await?, ApiReply::Children { store, nodes } => (store, nodes))
    }

    async fn sync(&self, store: StoreId, have: Vec<(NodeId, Vec<u8>)>, list_unknown: bool) -> Result<(Vec<SyncedDoc>, Vec<NodeId>), ApiError> {
        let have = have.into_iter().map(|(n, sv)| (n, Bytes(sv))).collect();
        expect!(self.0.ask(ApiCall::Sync { store, have, list_unknown }).await?, ApiReply::Synced { docs, unknown } => (
            docs.into_iter().map(|d| SyncedDoc { node: d.node, diff: d.diff.0, state_vector: d.state_vector.0 }).collect(),
            unknown,
        ))
    }

    async fn apply(&self, store: StoreId, node: NodeId, update: Vec<u8>, _client_id: &str) -> Result<(), ApiError> {
        // No client id crosses: the host names the plugin on every edit.
        expect!(self.0.ask(ApiCall::Apply { store, node, update: Bytes(update) }).await?, ApiReply::Done => ())
    }

    async fn create_node(&self, store: StoreId, parent: NodeId, init: NodeInit, place: Place) -> Result<NodeId, ApiError> {
        expect!(self.0.ask(ApiCall::CreateNode { store, parent, init: init.into(), place }).await?, ApiReply::Id { node } => node)
    }

    async fn create_attachment(&self, store: StoreId, host: NodeId, init: NodeInit) -> Result<NodeId, ApiError> {
        expect!(self.0.ask(ApiCall::CreateAttachment { store, host, init: init.into() }).await?, ApiReply::Id { node } => node)
    }

    async fn move_node(&self, store: StoreId, node: NodeId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        expect!(self.0.ask(ApiCall::MoveNode { store, node, parent, place }).await?, ApiReply::Moved { moved } => moved.into())
    }

    async fn move_nodes(&self, store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place) -> Result<Vec<Moved>, ApiError> {
        expect!(self.0.ask(ApiCall::MoveNodes { store, nodes, parent, place }).await?, ApiReply::MovedMany { moved } => moved.into_iter().map(Into::into).collect())
    }

    async fn transplant(&self, from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        expect!(self.0.ask(ApiCall::Transplant { from, node, to, parent, place }).await?, ApiReply::Moved { moved } => moved.into())
    }

    async fn delete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        expect!(self.0.ask(ApiCall::Delete { store, node }).await?, ApiReply::Done => ())
    }

    async fn undelete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        expect!(self.0.ask(ApiCall::Undelete { store, node }).await?, ApiReply::Done => ())
    }

    async fn resolve_link(&self, store: StoreId, node: NodeId) -> Result<LinkResolution, ApiError> {
        expect!(self.0.ask(ApiCall::ResolveLink { store, node }).await?, ApiReply::Link { resolution } => resolution)
    }

    async fn list_deleted(&self, store: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
        expect!(self.0.ask(ApiCall::ListDeleted { store }).await?, ApiReply::Deleted { nodes } => nodes)
    }

    async fn list_attachments(&self, store: StoreId, host: NodeId) -> Result<Vec<Node>, ApiError> {
        expect!(self.0.ask(ApiCall::ListAttachments { store, host }).await?, ApiReply::Nodes { nodes } => nodes)
    }

    async fn list_discussions(&self, store: StoreId) -> Result<Vec<Node>, ApiError> {
        expect!(self.0.ask(ApiCall::ListDiscussions { store }).await?, ApiReply::Nodes { nodes } => nodes)
    }

    async fn search(&self, query: &str, stores: Vec<StoreId>, semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError> {
        let call = ApiCall::Search { query: query.to_string(), stores, semantic, limit };
        expect!(self.0.ask(call).await?, ApiReply::Results { items } => items)
    }

    async fn put_blob(&self, store: StoreId, node: NodeId, mime: &str, bytes: &[u8]) -> Result<BlobUrl, ApiError> {
        let call = ApiCall::PutBlob { store, node, mime: mime.to_string(), bytes: Bytes(bytes.to_vec()) };
        expect!(self.0.ask(call).await?, ApiReply::BlobUrl { url } => url)
    }

    async fn get_blob(&self, store: StoreId, blob: BlobId) -> Result<(String, Vec<u8>), ApiError> {
        expect!(self.0.ask(ApiCall::GetBlob { store, blob }).await?, ApiReply::Blob { mime, bytes } => (mime, bytes.0))
    }

    async fn publish_presence(&self, frame: PresenceFrame, _client_id: &str) -> Result<(), ApiError> {
        expect!(self.0.ask(ApiCall::PublishPresence { frame }).await?, ApiReply::Done => ())
    }

    async fn who_am_i(&self) -> Result<(Option<String>, String), ApiError> {
        expect!(self.0.ask(ApiCall::WhoAmI).await?, ApiReply::Me { sub, name } => (sub, name))
    }

    async fn subscribe(&self, store: StoreId) -> Result<ChangeStream, ApiError> {
        self.0.subscribe(store).await
    }

    async fn people(&self, store: StoreId) -> Result<StorePeople, ApiError> {
        expect!(self.0.ask(ApiCall::People { store }).await?, ApiReply::People { people } => people)
    }

    async fn create_mount(&self, store: StoreId, parent: NodeId, source_store: StoreId, source_node: NodeId, title: Option<String>) -> Result<(NodeId, MountRef), ApiError> {
        let call = ApiCall::CreateMount { store, parent, source_store, source_node, title };
        expect!(self.0.ask(call).await?, ApiReply::Mounted { node, mount } => (node, mount))
    }

    async fn mount_state(&self, store: StoreId, node: NodeId) -> Result<(MountState, MountRef), ApiError> {
        expect!(self.0.ask(ApiCall::MountState { store, node }).await?, ApiReply::MountState { state, mount } => (state, mount))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::future::Future;
    use pimble_plugin_abi::api::{decode_json, ChangeWire, ErrorWire};
    use pimble_rpc::StoreChangeKind;

    /// A host that records every call; a test answers them.
    #[derive(Clone, Default)]
    struct Recorder {
        calls: Arc<Mutex<Vec<(u64, ApiCall)>>>,
        cancelled: Arc<Mutex<Vec<u64>>>,
    }

    impl ApiLink for Recorder {
        fn call(&self, id: u64, call: Vec<u8>) {
            self.calls.lock().unwrap().push((id, decode_json(&call).unwrap()));
        }
        fn cancel(&self, id: u64) {
            self.cancelled.lock().unwrap().push(id);
        }
    }

    #[test]
    fn a_call_waits_for_its_resolution_and_reads_errors_as_the_apis() {
        let recorder = Recorder::default();
        let bridge = Bridge::new(recorder.clone());
        let backend = GuestBackend::new(bridge.clone());
        let (store, node) = (StoreId::new(), NodeId::new());

        let mut pending = Box::pin(backend.move_node(store, node, node, Place::First));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(pending.as_mut().poll(&mut cx).is_pending());
        let (id, call) = recorder.calls.lock().unwrap()[0].clone();
        assert_eq!(call, ApiCall::MoveNode { store, node, parent: node, place: Place::First });
        bridge.resolve(Resolution { call: id, reply: ApiReply::Error { error: ErrorWire::Refused { sentence: "No.".into() } } });
        let Poll::Ready(answer) = pending.as_mut().poll(&mut cx) else { panic!("resolved") };
        assert_eq!(answer.unwrap_err(), ApiError::refused("No."));
        assert_eq!(bridge.outstanding(), 0);
    }

    #[test]
    fn a_subscription_takes_every_change_until_it_ends_and_says_when_it_is_dropped() {
        let recorder = Recorder::default();
        let bridge = Bridge::new(recorder.clone());
        let backend = GuestBackend::new(bridge.clone());
        let store = StoreId::new();
        let node = NodeId::new();
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut making = Box::pin(backend.subscribe(store));
        assert!(making.as_mut().poll(&mut cx).is_pending());
        let id = recorder.calls.lock().unwrap()[0].0;
        bridge.resolve(Resolution { call: id, reply: ApiReply::Subscribed });
        let Poll::Ready(Ok(mut stream)) = making.as_mut().poll(&mut cx) else { panic!("made") };
        for _ in 0..2 {
            let change = ChangeWire { store, kind: StoreChangeKind::MetadataUpdated { node_id: node }, source: None, update: Some(Bytes(vec![1])) };
            bridge.resolve(Resolution { call: id, reply: ApiReply::Change { change } });
        }
        assert!(matches!(stream.poll_next_unpin(&mut cx), Poll::Ready(Some(StoreChange { update: Some(_), .. }))));
        assert!(matches!(stream.poll_next_unpin(&mut cx), Poll::Ready(Some(_))));
        assert!(stream.poll_next_unpin(&mut cx).is_pending());
        drop(stream);
        assert_eq!(*recorder.cancelled.lock().unwrap(), vec![id]);

        // A second one, ended by the host: the stream ends.
        let mut making = Box::pin(backend.subscribe(store));
        assert!(making.as_mut().poll(&mut cx).is_pending());
        let id = recorder.calls.lock().unwrap()[1].0;
        bridge.resolve(Resolution { call: id, reply: ApiReply::Subscribed });
        let Poll::Ready(Ok(mut stream)) = making.as_mut().poll(&mut cx) else { panic!("made") };
        bridge.resolve(Resolution { call: id, reply: ApiReply::Ended });
        assert!(matches!(stream.poll_next_unpin(&mut cx), Poll::Ready(None)));
    }
}
