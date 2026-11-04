//! [`Pimble`]: the API every client uses (docs/API_CONTRACT.md decisions 3,
//! 4 and 11, section 3b). Reads are answered from held documents; the one
//! write runs a closure on a held document and sends what it changed; the
//! structural calls go to the backend, which merges and repairs them. The
//! store subscriptions that keep held documents current are drained before
//! every read, so no task has to be spawned to listen (the library runs in
//! the browser and in tokio alike); a caller that wants every change as it
//! comes awaits [`Pimble::next_change`].

use std::collections::{HashSet, VecDeque};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc;
use futures::stream::SelectAll;
use futures::{FutureExt, Stream, StreamExt};
use pimble_core::{node_types, BlobId, BlobUrl, DeletedNode, Node, NodeId, Place, Store, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::{BoardPresence, SearchResultItem};

use crate::backend::{Backend, MaybeSend, NodeInit, StoreChange};
use crate::held::Held;
use crate::identity::Me;
use crate::presence::{PresenceFrames, Whereabouts, AGENT_TTL_MS};
use crate::{Addr, ApiError};

/// What a held store's subscription brought, or that it ended.
enum Event {
    Change(StoreChange),
    Ended(StoreId),
}

#[cfg(not(target_arch = "wasm32"))]
type Events = Pin<Box<dyn Stream<Item = Event> + Send>>;
#[cfg(target_arch = "wasm32")]
type Events = Pin<Box<dyn Stream<Item = Event>>>;

/// A change, after [`Pimble`] merged it into what it holds.
#[derive(Debug, Clone)]
pub enum Change {
    /// A store's change as its subscription said it.
    Store(StoreChange),
    /// A store's subscription was made again (the connection came back); what
    /// is held there was brought up to date. `changed` names the held
    /// documents the catch-up changed, which is what anything shown of the
    /// store has to read again.
    Resynced { store: StoreId, changed: Vec<NodeId> },
    /// What this principal may do with these held nodes was derived again
    /// from where a change put them (`Held::apply`), and differs from what
    /// it was: nothing in their own documents changed to say so.
    Rejudged { store: StoreId, nodes: Vec<NodeId> },
}

/// What one write did. `changed` is whether merging it into the document
/// as it was changes anything: an edit that would not is not sent.
#[derive(Debug, Clone)]
pub struct Edited<T> {
    pub value: T,
    pub changed: bool,
    /// Where the edit left off in the text (a sticky index), for presence.
    pub caret: Option<Vec<u8>>,
    /// The node the edited document is attached to (a discussion's host).
    pub host: Option<NodeId>,
    /// What was sent, when anything was: the views this client holds of
    /// the node take the same bytes (the server relays a write to everyone
    /// but its sender).
    pub delta: Option<Vec<u8>>,
}

/// Where a move left the node.
#[derive(Debug, Clone)]
pub struct MovedTo {
    pub addr: Addr,
    /// It has a new id (it left a share, or its store).
    pub new_link: bool,
    pub left_shares: usize,
}

/// One node of a walk through a store (`Pimble::walk`): where it is in the
/// tree as the person sees it, mounts included.
#[derive(Debug, Clone)]
pub struct Walked {
    pub addr: Addr,
    pub node: Node,
    pub parent: Option<Addr>,
    /// Titles from the store's name down to it.
    pub path: String,
}

/// The time a write is stamped with: RFC 3339 UTC with milliseconds, the
/// one shape every writer uses (messages and steps sort by it as a string).
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub struct Pimble {
    backend: Box<dyn Backend>,
    client_id: String,
    pub(crate) held: Mutex<Held>,
    subs: futures::lock::Mutex<SelectAll<Events>>,
    /// Subscriptions made while someone awaited the others; joined to them
    /// at the next drain.
    new_subs: Mutex<Vec<Events>>,
    /// Rung when a subscription is added, so a listener waiting with none
    /// (or only quiet ones) takes it in.
    wake: mpsc::UnboundedSender<()>,
    woken: futures::lock::Mutex<mpsc::UnboundedReceiver<()>>,
    /// The stores a caller asked to hear ([`Pimble::watch`]): subscribed
    /// again whenever their subscription ends, for as long as they are.
    watched: Mutex<HashSet<StoreId>>,
    /// Stores whose subscription is being made, so two reads make one.
    subscribing: Mutex<std::collections::HashSet<StoreId>>,
    /// Changes kept for [`Pimble::next_change`] once someone listens.
    queue: Mutex<VecDeque<Change>>,
    listening: AtomicBool,
    me: futures::lock::Mutex<Option<Option<Me>>>,
    frames: PresenceFrames,
    whereabouts: Whereabouts,
}

impl Pimble {
    /// The API over `backend`, writing as `client_id` (`agent:<id>` for an
    /// agent, so notifications and logs say where an edit came from).
    pub fn new(backend: impl Backend + 'static, client_id: impl Into<String>) -> Self {
        Self::with_presence_ttl(backend, client_id, AGENT_TTL_MS)
    }

    /// As [`Pimble::new`], with presence frames that hold `ttl_ms`.
    pub fn with_presence_ttl(backend: impl Backend + 'static, client_id: impl Into<String>, ttl_ms: u64) -> Self {
        let client_id = client_id.into();
        let (wake, woken) = mpsc::unbounded();
        Self {
            backend: Box::new(backend),
            frames: PresenceFrames::new(client_id.clone(), ttl_ms),
            client_id,
            held: Mutex::new(Held::default()),
            subs: futures::lock::Mutex::new(SelectAll::new()),
            new_subs: Mutex::new(Vec::new()),
            wake,
            woken: futures::lock::Mutex::new(woken),
            watched: Mutex::new(HashSet::new()),
            subscribing: Mutex::new(Default::default()),
            queue: Mutex::new(VecDeque::new()),
            listening: AtomicBool::new(false),
            me: futures::lock::Mutex::new(None),
            whereabouts: Whereabouts::default(),
        }
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The transport, for what the API does not wrap yet.
    pub fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    // ── Changes ──────────────────────────────────────────────────────────

    /// Merge whatever the subscriptions have brought, without waiting.
    pub(crate) fn drain(&self) {
        let Some(mut subs) = self.subs.try_lock() else { return };
        for s in self.new_subs.lock().unwrap().drain(..) {
            subs.push(s);
        }
        while let Some(Some(event)) = subs.next().now_or_never() {
            self.process(event);
        }
    }

    fn process(&self, event: Event) {
        match event {
            Event::Change(change) => {
                let store = change.store;
                let rejudged = self.held.lock().unwrap().apply(&change);
                self.keep(Change::Store(change));
                if !rejudged.is_empty() {
                    self.keep(Change::Rejudged { store, nodes: rejudged });
                }
            }
            Event::Ended(store) => self.held.lock().unwrap().lost(store),
        }
    }

    fn keep(&self, change: Change) {
        if self.listening.load(Ordering::Relaxed) {
            self.queue.lock().unwrap().push_back(change);
        }
    }

    /// Keep every change from now on for [`Pimble::next_change`], including
    /// the ones reads merge on their way (a client that never asks for
    /// changes keeps none).
    pub fn listen(&self) {
        self.listening.store(true, Ordering::Relaxed);
    }

    /// The next change to a held store, merged into what is held before it
    /// is answered (and listening from the first call, [`Pimble::listen`]).
    /// With nothing subscribed it waits for a subscription to be made. A
    /// subscription that ends is made again when its store is watched
    /// ([`Pimble::watch`]) or held, and its store brought up to date, before
    /// the next change is answered (`Change::Resynced`); one that cannot be
    /// made now (the connection is down, the store not open there yet) is
    /// made at the next [`Pimble::watch`] or [`Pimble::rewatch`].
    pub async fn next_change(&self) -> Option<Change> {
        self.listen();
        loop {
            if let Some(change) = self.queue.lock().unwrap().pop_front() {
                return Some(change);
            }
            let event = {
                let mut subs = self.subs.lock().await;
                let mut woken = self.woken.lock().await;
                loop {
                    for s in self.new_subs.lock().unwrap().drain(..) {
                        subs.push(s);
                    }
                    if subs.is_empty() {
                        woken.next().await?;
                        continue;
                    }
                    match futures::future::select(subs.next(), woken.next()).await {
                        futures::future::Either::Left((Some(event), _)) => break event,
                        // Every subscription ended at once; wait for the next.
                        futures::future::Either::Left((None, _)) => continue,
                        futures::future::Either::Right((Some(()), _)) => continue,
                        futures::future::Either::Right((None, _)) => return None,
                    }
                }
            };
            match event {
                Event::Change(change) => {
                    let store = change.store;
                    let rejudged = self.held.lock().unwrap().apply(&change);
                    if !rejudged.is_empty() {
                        // After the change that caused it.
                        self.queue.lock().unwrap().push_back(Change::Rejudged { store, nodes: rejudged });
                    }
                    return Some(Change::Store(change));
                }
                Event::Ended(store) => {
                    self.held.lock().unwrap().lost(store);
                    if self.watched.lock().unwrap().contains(&store) {
                        let _ = self.ensure_live(store).await;
                    }
                }
            }
        }
    }

    /// Hear `store`'s changes from now on, through [`Pimble::next_change`],
    /// and keep hearing them across a lost connection: a subscription that
    /// ends is made again. Answers why it could not be made now, if it
    /// could not (it is made again at the next [`Pimble::rewatch`]). A store
    /// whose backend holds its documents itself is not subscribed to here:
    /// its backend says what changed in it.
    pub async fn watch(&self, store: StoreId) -> Result<(), ApiError> {
        self.watched.lock().unwrap().insert(store);
        self.subscribe_now(store).await
    }

    /// Stop hearing `store` (it was closed): what is held of it goes too.
    pub fn forget(&self, store: StoreId) {
        self.watched.lock().unwrap().remove(&store);
        self.held.lock().unwrap().held.remove(&store);
    }

    /// Make again every watched store's subscription that is not live (a new
    /// connection: the old one's subscriptions went with it).
    pub async fn rewatch(&self) {
        let watched: Vec<StoreId> = self.watched.lock().unwrap().iter().copied().collect();
        for store in watched {
            if let Err(e) = self.subscribe_now(store).await {
                tracing::debug!("store {store} could not be heard again yet: {e}");
            }
        }
    }

    /// Whether `store`'s backend holds its documents itself, so nothing of
    /// it is held here ([`crate::StoreCapabilities::local_documents`]).
    pub(crate) fn passes_through(&self, store: StoreId) -> bool {
        self.backend.capabilities(store).local_documents
    }

    /// Subscribe to `store` if what is held there is not kept current (the
    /// first read of it, or its subscription ended), and bring what is held
    /// up to date. A store that cannot be subscribed to is read afresh
    /// every time instead.
    pub(crate) async fn ensure_live(&self, store: StoreId) -> Result<(), ApiError> {
        if let Err(e) = self.subscribe_now(store).await {
            tracing::debug!("store {store} could not be subscribed to; reading it afresh: {e}");
        }
        Ok(())
    }

    /// [`Pimble::ensure_live`], answering why a subscription could not be
    /// made.
    async fn subscribe_now(&self, store: StoreId) -> Result<(), ApiError> {
        if self.passes_through(store) {
            return Ok(());
        }
        self.drain();
        let had = {
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            if h.live {
                return Ok(());
            }
            !h.tree.ids().is_empty()
        };
        if !self.subscribing.lock().unwrap().insert(store) {
            return Ok(());
        }
        let subscribed = self.backend.subscribe(store).await;
        self.subscribing.lock().unwrap().remove(&store);
        let stream = subscribed?;
        let tagged: Events = Box::pin(stream.map(Event::Change).chain(futures::stream::once(async move { Event::Ended(store) })));
        self.new_subs.lock().unwrap().push(tagged);
        let _ = self.wake.unbounded_send(());
        let have = {
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            h.live = true;
            // What the server judged may have changed while nothing was heard.
            for j in h.judged.values_mut() {
                j.stale = true;
            }
            if had {
                h.state_vectors()
            } else {
                Vec::new()
            }
        };
        if !have.is_empty() {
            let (docs, _) = self.backend.sync(store, have, false).await?;
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            let mut changed = Vec::new();
            for d in docs {
                if h.merge_changed(d.node, &d.diff) {
                    changed.push(d.node);
                }
            }
            drop(held);
            self.keep(Change::Resynced { store, changed });
        }
        Ok(())
    }

    // ── Stores ───────────────────────────────────────────────────────────

    /// The stores this principal may read, asked of the server now.
    pub async fn stores(&self) -> Result<Vec<Store>, ApiError> {
        let stores = self.backend.list_stores().await?;
        let mut held = self.held.lock().unwrap();
        for s in &stores {
            if let Some(h) = held.held.get_mut(&s.id) {
                h.store_access = s.access;
            }
        }
        held.stores = stores.clone();
        held.listed = true;
        Ok(stores)
    }

    /// The stores as last listed (listed now if they never were).
    pub async fn known_stores(&self) -> Result<Vec<Store>, ApiError> {
        let listed = {
            let held = self.held.lock().unwrap();
            held.listed.then(|| held.stores.clone())
        };
        match listed {
            Some(stores) => Ok(stores),
            None => self.stores().await,
        }
    }

    /// A store by its name or id, listed again when the last listing has
    /// none by that name (a store opened since).
    pub async fn find_store(&self, name: &str) -> Result<Store, ApiError> {
        let stores = self.known_stores().await?;
        match crate::names::find_store(&stores, name) {
            Ok(store) => Ok(store.clone()),
            Err(ApiError::NotFound { .. }) => crate::names::find_store(&self.stores().await?, name).cloned(),
            Err(e) => Err(e),
        }
    }

    /// One store's row: from the last listing, or listed again when it is
    /// not in it (a store opened since).
    pub async fn store_row(&self, id: StoreId) -> Option<Store> {
        if let Some(row) = self.held.lock().unwrap().store_row(id).cloned() {
            return Some(row);
        }
        self.stores().await.ok()?.into_iter().find(|s| s.id == id)
    }

    // ── Reading ──────────────────────────────────────────────────────────

    /// A node as the server judged it for this principal (access, how many
    /// discussions it has), its document held from now on.
    pub async fn node(&self, addr: Addr) -> Result<Node, ApiError> {
        self.node_fetched(addr).await.map(|(node, _)| node)
    }

    /// [`Pimble::node`], and whether it was fetched just now.
    async fn node_fetched(&self, addr: Addr) -> Result<(Node, bool), ApiError> {
        if self.passes_through(addr.store) {
            return Ok((self.backend.fetch_one(addr.store, addr.node).await?, true));
        }
        self.ensure_live(addr.store).await?;
        {
            let held = self.held.lock().unwrap();
            if let Some(h) = held.store(addr.store) {
                if h.fresh(addr.node) {
                    if let Some(node) = h.node(addr.node) {
                        return Ok((node, false));
                    }
                }
            }
        }
        let node = self.backend.fetch_one(addr.store, addr.node).await?;
        let mut held = self.held.lock().unwrap();
        let h = held.store_mut(addr.store);
        h.take(&node);
        Ok((h.node(addr.node).unwrap_or(node), true))
    }

    /// A node held (fetched when it is not), without asking the server for
    /// a fresh judgement: what walks and lists read.
    pub(crate) async fn held_node(&self, addr: Addr) -> Result<Node, ApiError> {
        if self.passes_through(addr.store) {
            return self.backend.fetch_one(addr.store, addr.node).await;
        }
        self.ensure_live(addr.store).await?;
        {
            let held = self.held.lock().unwrap();
            if let Some(h) = held.store(addr.store).filter(|h| h.live) {
                if let Some(node) = h.node(addr.node) {
                    return Ok(node);
                }
            }
        }
        self.node(addr).await
    }

    /// A copy of a node's document as held, to read from.
    pub async fn doc(&self, addr: Addr) -> Result<NodeDoc, ApiError> {
        if self.passes_through(addr.store) {
            let node = self.backend.fetch_one(addr.store, addr.node).await?;
            return NodeDoc::load(&node.content).map_err(ApiError::unreadable);
        }
        self.held_node(addr).await?;
        let held = self.held.lock().unwrap();
        let doc = held.store(addr.store).and_then(|h| h.tree.doc(addr.node)).ok_or_else(|| ApiError::not_found("That node is not here."))?;
        NodeDoc::load(&doc.save()).map_err(ApiError::unreadable)
    }

    /// A node's children, through a mount to its source's: each with the
    /// address it lives at (a mount's children live in the source store).
    pub async fn children(&self, addr: Addr) -> Result<Vec<(Addr, Node)>, ApiError> {
        let (store, nodes) = self.listing(addr).await?;
        Ok(nodes.into_iter().map(|n| (Addr { store, node: n.id }, n)).collect())
    }

    /// [`Pimble::children`] as a list answers it: the store the children
    /// live in (a mount's source store, for a mount, even when it has none
    /// yet), and them, in order.
    pub async fn listing(&self, addr: Addr) -> Result<(StoreId, Vec<Node>), ApiError> {
        if self.passes_through(addr.store) {
            return self.backend.children(addr.store, addr.node).await;
        }
        let parent = self.held_node(addr).await?;
        if parent.node_type == node_types::MOUNT && parent.mount_ref().is_some() {
            // The server resolves a mount (opening or replicating its
            // source), so its children are always asked of it.
            let (store, nodes) = self.backend.children(addr.store, addr.node).await?;
            self.ensure_live(store).await?;
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            for n in &nodes {
                h.take(n);
            }
            return Ok((store, nodes));
        }
        let missing = {
            let held = self.held.lock().unwrap();
            let Some(h) = held.store(addr.store) else { return Ok((addr.store, Vec::new())) };
            h.listed(addr.node).into_iter().filter(|c| (!h.live || h.tree.doc(*c).is_none()) && !h.absent.contains(c)).collect::<Vec<_>>()
        };
        if !missing.is_empty() {
            let got = self.backend.fetch(addr.store, missing.clone()).await?;
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(addr.store);
            for n in &got {
                h.take(n);
            }
            for id in missing {
                if !got.iter().any(|n| n.id == id) {
                    h.absent.insert(id);
                }
            }
        }
        let held = self.held.lock().unwrap();
        let Some(h) = held.store(addr.store) else { return Ok((addr.store, Vec::new())) };
        let ids = h.tree.get_children(addr.node).unwrap_or_default();
        Ok((addr.store, ids.into_iter().filter_map(|id| h.node(id)).collect()))
    }

    /// Hold every document of `store` this principal may read: two
    /// reconciles, not a walk. After it, names and lists in the store are
    /// lookups, and a node made there is heard of and held.
    pub async fn hold_store(&self, store: StoreId) -> Result<(), ApiError> {
        self.ensure_live(store).await?;
        let have = {
            let held = self.held.lock().unwrap();
            match held.store(store) {
                Some(h) if h.complete && h.live => return Ok(()),
                Some(h) => h.state_vectors(),
                None => Vec::new(),
            }
        };
        let (docs, unknown) = self.backend.sync(store, have, true).await?;
        {
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            for d in docs {
                h.merge(d.node, &d.diff, false);
            }
        }
        if !unknown.is_empty() {
            let empty = pimble_crdt::empty_state_vector();
            let ask = unknown.iter().map(|id| (*id, empty.clone())).collect();
            let (docs, _) = self.backend.sync(store, ask, false).await?;
            let mut held = self.held.lock().unwrap();
            let h = held.store_mut(store);
            for d in docs {
                h.merge(d.node, &d.diff, true);
            }
        }
        let mut held = self.held.lock().unwrap();
        let h = held.store_mut(store);
        h.complete = h.live;
        h.absent.clear();
        Ok(())
    }

    /// Every node of `store` as the person sees its tree: its top (the root,
    /// or a partial replica's shared roots) and everything under it, through
    /// mounts, each with its path of titles from the store's name.
    pub async fn walk(&self, store: &Store) -> Result<Vec<Walked>, ApiError> {
        self.hold_store(store.id).await?;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let tops = crate::names::store_tops(store);
        let mut queue: Vec<(Addr, String)> = Vec::new();
        for (i, top) in tops.iter().enumerate() {
            if i == 0 {
                if let Ok(node) = self.held_node(*top).await {
                    out.push(Walked { addr: *top, node, parent: None, path: store.name.clone() });
                }
            }
            queue.push((*top, store.name.clone()));
        }
        while let Some((at, path)) = queue.pop() {
            if !seen.insert(at) {
                continue;
            }
            let Ok(children) = self.children(at).await else { continue };
            for (addr, node) in children {
                let child_path = format!("{path}/{}", crate::names::label(&node));
                queue.push((addr, child_path.clone()));
                out.push(Walked { addr, node, parent: Some(at), path: child_path });
            }
        }
        Ok(out)
    }

    /// The fields of a held document, live or a tombstone.
    pub(crate) fn held_doc_fields(&self, addr: Addr) -> Option<pimble_crdt::NodeFields> {
        let held = self.held.lock().unwrap();
        held.store(addr.store)?.tree.doc(addr.node)?.fields().ok()
    }

    /// A node's document even when it is a tombstone (what "it was deleted"
    /// needs to say its title): asked of the server when not held.
    pub(crate) async fn any_fields(&self, addr: Addr) -> Option<pimble_crdt::NodeFields> {
        if let Some(fields) = self.held_doc_fields(addr) {
            return Some(fields);
        }
        let ask = vec![(addr.node, pimble_crdt::empty_state_vector())];
        let (docs, _) = self.backend.sync(addr.store, ask, false).await.ok()?;
        let d = docs.into_iter().next()?;
        NodeDoc::load(&d.diff).ok()?.fields().ok()
    }

    // ── The one write ────────────────────────────────────────────────────

    /// Change a node's document and send what changed as one `applyEdit`
    /// (docs/API_CONTRACT.md decision 4): the one way anything in a node is
    /// written. The document is brought up to date first, so `edit` judges
    /// what the server holds now (a quote, a table's name); a write this
    /// principal may not make is refused before anything is sent (the
    /// server judges again); `edit` gets a copy of the document and the time
    /// to stamp; what it changed is sent only when merging it into the
    /// document as it was changes anything (never the state vector: a
    /// deletion alone does not move it). A refusal from `edit` sends
    /// nothing.
    pub async fn edit<T, E, F>(&self, addr: Addr, edit: F) -> Result<Edited<T>, ApiError>
    where
        F: FnOnce(&mut NodeDoc, &str) -> Result<T, E> + MaybeSend,
        E: Into<ApiError>,
        T: MaybeSend,
    {
        let pass_through = self.passes_through(addr.store);
        let (node, fetched) = self.node_fetched(addr).await?;
        if let Some(refusal) = ApiError::of_access(node.access) {
            return Err(refusal);
        }
        if !fetched {
            self.refresh(addr).await?;
        }
        let before_bytes = if pass_through {
            // The backend's own copy, as it handed it over just now.
            node.content.clone()
        } else {
            let held = self.held.lock().unwrap();
            let doc = held.store(addr.store).and_then(|h| h.tree.doc(addr.node)).ok_or_else(|| ApiError::not_found("That node is not here."))?;
            doc.save()
        };
        // The documents are dropped before anything is awaited.
        let (value, caret, delta, changed) = {
            let mut doc = NodeDoc::load(&before_bytes).map_err(ApiError::unreadable)?;
            let before = doc.state_vector();
            let text_before = doc.textblock_texts();
            let value = edit(&mut doc, &now()).map_err(Into::into)?;
            let caret = doc.sticky_after_change(&text_before);
            let delta = doc.diff_since(&before)?;
            let mut untouched = NodeDoc::load(&before_bytes).map_err(ApiError::unreadable)?;
            let changed = untouched.apply_update(&delta)?.changed;
            (value, caret, delta, changed)
        };
        if changed {
            self.backend.apply(addr.store, addr.node, delta.clone(), &self.client_id).await?;
            if !pass_through {
                self.held.lock().unwrap().store_mut(addr.store).merge(addr.node, &delta, false);
            }
        }
        Ok(Edited { value, changed, caret: caret.filter(|_| changed), host: node.attached_to, delta: changed.then_some(delta) })
    }

    /// Bring one held document up to date with the server's.
    async fn refresh(&self, addr: Addr) -> Result<(), ApiError> {
        let sv = {
            let held = self.held.lock().unwrap();
            match held.store(addr.store).and_then(|h| h.tree.doc(addr.node)) {
                Some(doc) => doc.state_vector(),
                None => return Ok(()),
            }
        };
        let (docs, _) = self.backend.sync(addr.store, vec![(addr.node, sv)], false).await?;
        let mut held = self.held.lock().unwrap();
        let h = held.store_mut(addr.store);
        for d in docs {
            h.merge(d.node, &d.diff, false);
        }
        Ok(())
    }

    // ── Structure ────────────────────────────────────────────────────────

    /// Make a node from `init` (a [`crate::recipes`] recipe) under `parent`,
    /// at `place`, in the one `createNode` that makes it.
    pub async fn create(&self, parent: Addr, place: Place, init: NodeInit) -> Result<Addr, ApiError> {
        let parent = self.create_parent(parent).await?;
        let node = self.backend.create_node(parent.store, parent.node, init, place).await?;
        Ok(Addr { store: parent.store, node })
    }

    /// Make an attachment of `host` (a discussion on it) from `init`.
    pub async fn create_attached(&self, host: Addr, init: NodeInit) -> Result<Addr, ApiError> {
        let host = self.attachment_host(host).await?;
        let node = self.backend.create_attachment(host.store, host.node, init).await?;
        Ok(Addr { store: host.store, node })
    }

    /// Move a node under `parent` (a mount's source for a mount) at
    /// `place`: within its store a move, into another a transplant. A move
    /// out of a share or a store gives the node a new id
    /// (docs/MOVE_CONTRACT.md). Planned first ([`Pimble::plan_move`]), so it
    /// is refused as the app refuses it; a node already there stays.
    pub async fn move_node(&self, node: Addr, parent: Addr, place: Place) -> Result<MovedTo, ApiError> {
        let plan = self.plan_move(&[node], parent, place).await?;
        let moved = self.move_nodes(&plan).await?;
        Ok(moved.into_iter().next().unwrap_or(MovedTo { addr: node, new_link: false, left_shares: 0 }))
    }

    /// Delete a node and its subtree (a tombstone; [`Pimble::undelete`]).
    pub async fn delete(&self, addr: Addr) -> Result<(), ApiError> {
        self.backend.delete(addr.store, addr.node).await
    }

    pub async fn undelete(&self, addr: Addr) -> Result<(), ApiError> {
        self.backend.undelete(addr.store, addr.node).await
    }

    // ── Pass-throughs ────────────────────────────────────────────────────

    pub async fn search(&self, query: &str, stores: Vec<StoreId>, semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError> {
        self.backend.search(query, stores, semantic, limit).await
    }

    pub async fn list_deleted(&self, store: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
        self.backend.list_deleted(store).await
    }

    /// A node's attachments (its discussions).
    pub async fn attachments(&self, host: Addr) -> Result<Vec<Node>, ApiError> {
        let nodes = self.backend.list_attachments(host.store, host.node).await?;
        if self.passes_through(host.store) {
            return Ok(nodes);
        }
        let mut held = self.held.lock().unwrap();
        let h = held.store_mut(host.store);
        for n in &nodes {
            h.take(n);
        }
        Ok(nodes)
    }

    /// Every discussion of `store` (a host's list is [`Pimble::attachments`]).
    pub async fn discussions(&self, store: StoreId) -> Result<Vec<Node>, ApiError> {
        self.backend.list_discussions(store).await
    }

    /// The stateless reconcile of one document a view holds (an editor's
    /// session, a board's): everything the backend has beyond
    /// `state_vector`, and the backend's own state vector, so the view can
    /// send back what the backend lacks. What it brings is merged into
    /// what is held too.
    pub async fn reconcile(&self, addr: Addr, state_vector: Vec<u8>) -> Result<(Vec<u8>, Vec<u8>), ApiError> {
        let (docs, _) = self.backend.sync(addr.store, vec![(addr.node, state_vector)], false).await?;
        let Some(d) = docs.into_iter().find(|d| d.node == addr.node) else {
            return Err(ApiError::node_not_found(addr.node));
        };
        if !self.passes_through(addr.store) {
            let mut held = self.held.lock().unwrap();
            if let Some(h) = held.held.get_mut(&addr.store) {
                h.merge(addr.node, &d.diff, false);
            }
        }
        Ok((d.diff, d.state_vector))
    }

    /// Merge `update` into a node's document, as a view of this client made
    /// it (typing, a stroke on a canvas): the view already holds it, so this
    /// only sends it and keeps what is held in step. The access judgement
    /// is the view's (a locked editor makes no update) and the backend's.
    pub async fn apply(&self, addr: Addr, update: Vec<u8>) -> Result<(), ApiError> {
        self.backend.apply(addr.store, addr.node, update.clone(), &self.client_id).await?;
        if !self.passes_through(addr.store) {
            let mut held = self.held.lock().unwrap();
            if let Some(h) = held.held.get_mut(&addr.store) {
                h.merge(addr.node, &update, false);
            }
        }
        Ok(())
    }

    /// Store a picture as a blob of `addr`'s store, as a write of `addr`.
    pub async fn put_blob(&self, addr: Addr, mime: &str, bytes: &[u8]) -> Result<BlobUrl, ApiError> {
        self.backend.put_blob(addr.store, addr.node, mime, bytes).await
    }

    pub async fn get_blob(&self, store: StoreId, blob: BlobId) -> Result<(String, Vec<u8>), ApiError> {
        self.backend.get_blob(store, blob).await
    }

    // ── Who, and where ───────────────────────────────────────────────────

    /// Who this connection is, asked once: `None` for a principal with no
    /// account (the server's own operator, someone signed out). A failed
    /// ask is not kept, so the next write asks again.
    pub async fn me(&self) -> Option<Me> {
        let mut me = self.me.lock().await;
        if let Some(known) = me.as_ref() {
            return known.clone();
        }
        let asked = self.backend.who_am_i().await.ok()?;
        let known = match asked {
            (Some(id), name) => Some(Me { id, name }),
            (None, _) => None,
        };
        *me = Some(known.clone());
        known
    }

    /// The MCP client app (or other device) this client works through.
    pub fn set_device(&self, name: &str) {
        self.whereabouts.set_device(name);
    }

    /// Say this client is in `addr`'s text, its caret where it just wrote
    /// (`None` keeps the last). A frame that cannot be sent fails nothing.
    pub async fn say(&self, addr: Addr, caret: Option<Vec<u8>>) {
        let frame = self.whereabouts.frame(&self.frames, addr, caret, None);
        if let Err(e) = self.backend.publish_presence(frame, &self.client_id).await {
            tracing::debug!("presence in {} not sent: {e}", addr.link());
        }
    }

    /// Say this client is on the canvas in `addr`.
    pub async fn say_on_board(&self, addr: Addr, board: BoardPresence) {
        let frame = self.whereabouts.frame(&self.frames, addr, None, Some(board));
        if let Err(e) = self.backend.publish_presence(frame, &self.client_id).await {
            tracing::debug!("presence in {} not sent: {e}", addr.link());
        }
    }

    /// The frame builder, for a client that names its own sessions (the
    /// app's panes).
    pub fn frames(&self) -> &PresenceFrames {
        &self.frames
    }
}

/// A `Pimble` shared between tasks.
pub type SharedPimble = Arc<Pimble>;
