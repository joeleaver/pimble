//! [`Router`]: one [`Backend`] over several, chosen per store
//! (docs/API_CONTRACT.md decision 11, wave 4).
//!
//! A client can reach stores in more than one way at once: the browser
//! reads plain stores through a Pimble server's JSON-RPC and encrypted ones
//! through the vault it decrypts in the page, and a store served from its
//! owner's computer has an endpoint of its own. The router is the one place
//! that knows which backend answers for which store, and it owns what no
//! single backend can decide:
//!
//! - **the write gate**: what this principal may change of a store, judged
//!   from what the account says of it before anything is asked of anyone,
//!   so a plain store and an encrypted one are refused the same way and in
//!   the same words ([`Router::gate`]);
//! - **a store whose owner's computer is off** (docs/RELAY_CONTRACT.md): it
//!   is read from what is held of it, which may be nothing, and takes no
//!   change but the edits an editor has already put on screen
//!   ([`Router::set_away`]);
//! - **operations that span two stores**: a transplant between stores two
//!   backends serve, which no backend can do alone (refused with a
//!   sentence), and a search, whose stores each go to their own backend.
//!
//! Every backend still judges for itself; the router's refusals are the
//! ones it can make without asking.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use pimble_core::{
    node_types, BlobId, BlobUrl, DeletedNode, LinkResolution, Node, NodeId, NodeMetadata, Place, Store, StoreAccess, StoreId,
};
use pimble_rpc::{PresenceFrame, SearchResultItem};

use crate::backend::{Backend, ChangeStream, Moved, NodeInit, StoreCapabilities, SyncedDoc};
use crate::ApiError;

/// Which of a router's backends: the order they were added in, the first
/// (the default for a store nobody routed) `0`.
pub type BackendId = usize;

/// What a change is answered with while the computer a store is served from
/// is off: nothing of such a store is on Pimble Cloud, and a change made
/// here would reach nobody.
pub const OWNER_OFFLINE_REFUSAL: &str =
    "This is shared from its owner's computer, which is offline. It can be changed again once that computer is back.";

/// A transplant between an encrypted store and a plain one
/// (docs/MOVE_CONTRACT.md "Between stores"): the two are not one operation
/// yet.
pub const MIXED_TRANSPLANT_REFUSAL: &str = "Moving a node between an encrypted store and a plain one is not supported yet.";

/// A transplant between two stores different backends serve (two servers):
/// a server plants only into a store it holds.
pub const SPLIT_TRANSPLANT_REFUSAL: &str = "Moving a node between stores on different servers is not supported yet.";

/// What kind of change a write is, for the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    /// The tree or a node's place, title, appearance; a picture; anything
    /// that is not one of the two below.
    Structure,
    /// A new discussion: a commenter's to make.
    Discussion,
    /// An edit of one document's content, which may already be on someone's
    /// screen: the backend judges whose document it is (a commenter writes
    /// discussions), and a store whose owner is away keeps it for later.
    Content,
}

#[derive(Debug, Clone, Default)]
struct Route {
    backend: Option<BackendId>,
    /// What the account may change of the store, when it says.
    access: Option<StoreAccess>,
    /// The store's owner's computer is off: the row it is listed as.
    away: Option<Store>,
}

pub struct Router {
    backends: Vec<Box<dyn Backend>>,
    routes: Mutex<HashMap<StoreId, Route>>,
}

impl Router {
    /// A router whose every store goes to `default` until routed elsewhere.
    pub fn new(default: impl Backend + 'static) -> Self {
        Self { backends: vec![Box::new(default)], routes: Mutex::new(HashMap::new()) }
    }

    /// Add a backend; stores reach it once [`Router::serve`] says so.
    pub fn add(&mut self, backend: impl Backend + 'static) -> BackendId {
        self.backends.push(Box::new(backend));
        self.backends.len() - 1
    }

    pub fn backend(&self, id: BackendId) -> &dyn Backend {
        self.backends[id].as_ref()
    }

    /// `store` is answered by backend `id` from now on.
    pub fn serve(&self, store: StoreId, id: BackendId) {
        assert!(id < self.backends.len(), "no backend {id}");
        self.routes.lock().unwrap().entry(store).or_default().backend = Some(id);
    }

    /// Which backend answers for `store`.
    pub fn served_by(&self, store: StoreId) -> BackendId {
        self.route(store).backend.unwrap_or(0)
    }

    /// What the account may change of `store` (`None`: nothing is known,
    /// which is no reason to refuse anything).
    pub fn set_access(&self, store: StoreId, access: Option<StoreAccess>) {
        self.routes.lock().unwrap().entry(store).or_default().access = access;
    }

    /// The computer `store` is served from is off (`Some`, with the row it
    /// is listed as while it is: for a store never opened here, what the
    /// account's own list says of it) or back (`None`).
    pub fn set_away(&self, store: StoreId, row: Option<Store>) {
        self.routes.lock().unwrap().entry(store).or_default().away = row;
    }

    pub fn is_away(&self, store: StoreId) -> bool {
        self.route(store).away.is_some()
    }

    /// Whether a change of kind `write` to `store` is refused before it is
    /// sent: a reader's, a commenter's that is not a discussion's, anything
    /// but an on-screen edit while the store's owner is away. The same
    /// sentences whichever backend serves the store.
    pub fn gate(&self, store: StoreId, write: Write) -> Result<(), ApiError> {
        let route = self.route(store);
        match route.access {
            Some(StoreAccess::Read) => return Err(ApiError::ReadOnly),
            Some(StoreAccess::Comment) if write == Write::Structure => return Err(ApiError::CommentOnly),
            _ => {}
        }
        if route.away.is_some() && write != Write::Content {
            return Err(ApiError::OwnerOffline { sentence: OWNER_OFFLINE_REFUSAL.to_string() });
        }
        Ok(())
    }

    fn route(&self, store: StoreId) -> Route {
        self.routes.lock().unwrap().get(&store).cloned().unwrap_or_default()
    }

    fn of(&self, store: StoreId) -> &dyn Backend {
        self.backends[self.served_by(store)].as_ref()
    }

    /// The row of a store whose owner is away, and the placeholder root it
    /// is listed with, when `node` is that root.
    fn away_root(&self, store: StoreId, node: NodeId) -> Option<Store> {
        self.route(store).away.filter(|row| row.root_node_id == node)
    }

    /// A node of a store whose owner is away, as it is handed out: to be
    /// read, whatever the account may do there, since a change would reach
    /// nobody.
    fn while_away(&self, store: StoreId, mut node: Node) -> Node {
        if self.is_away(store) {
            node.access = StoreAccess::Read;
        }
        node
    }
}

/// What the placeholder root of a store that cannot be reached answers as:
/// an empty folder named after the store, to be read.
fn empty_folder(node_id: NodeId, title: &str) -> Node {
    let now = chrono::Utc::now();
    Node {
        id: node_id,
        parent_id: None,
        node_type: node_types::FOLDER.to_string(),
        metadata: NodeMetadata {
            title: title.to_string(),
            created_at: now,
            modified_at: now,
            tags: Vec::new(),
            custom: HashMap::new(),
            fields: Vec::new(),
        },
        content: Vec::new(),
        children: Vec::new(),
        became: None,
        attached_to: None,
        attachments: 0,
        access: StoreAccess::Read,
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl Backend for Router {
    fn capabilities(&self, store: StoreId) -> StoreCapabilities {
        self.of(store).capabilities(store)
    }

    /// Every backend's stores, each once, as the backend that serves it
    /// lists it; a store whose owner is away and that nobody lists, as the
    /// account's row of it. The default backend not answering is an error;
    /// another not answering leaves its stores out.
    async fn list_stores(&self) -> Result<Vec<Store>, ApiError> {
        let mut listed: Vec<(BackendId, Store)> = Vec::new();
        for (id, backend) in self.backends.iter().enumerate() {
            match backend.list_stores().await {
                Ok(rows) => listed.extend(rows.into_iter().map(|row| (id, row))),
                Err(e) if id == 0 => return Err(e),
                Err(e) => tracing::debug!("backend {id} did not list its stores: {e}"),
            }
        }
        let mut out: Vec<Store> = Vec::new();
        for (id, row) in &listed {
            if out.iter().any(|s| s.id == row.id) {
                continue;
            }
            let serving = self.served_by(row.id);
            let chosen = if *id == serving { row } else { listed.iter().find(|(b, r)| *b == serving && r.id == row.id).map_or(row, |(_, r)| r) };
            out.push(chosen.clone());
        }
        let away: Vec<Store> = self.routes.lock().unwrap().values().filter_map(|r| r.away.clone()).collect();
        for row in away {
            if !out.iter().any(|s| s.id == row.id) {
                out.push(row);
            }
        }
        Ok(out)
    }

    async fn fetch_one(&self, store: StoreId, node: NodeId) -> Result<Node, ApiError> {
        match self.of(store).fetch_one(store, node).await {
            Ok(n) => Ok(self.while_away(store, n)),
            Err(e) if !e.is_refusal() => match self.away_root(store, node) {
                Some(row) => Ok(empty_folder(node, &row.name)),
                None => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    async fn fetch(&self, store: StoreId, nodes: Vec<NodeId>) -> Result<Vec<Node>, ApiError> {
        let got = self.of(store).fetch(store, nodes).await?;
        Ok(got.into_iter().map(|n| self.while_away(store, n)).collect())
    }

    async fn children(&self, store: StoreId, node: NodeId) -> Result<(StoreId, Vec<Node>), ApiError> {
        match self.of(store).children(store, node).await {
            Ok((at, nodes)) => Ok((at, nodes.into_iter().map(|n| self.while_away(at, n)).collect())),
            // A store never opened here, whose owner is away: its placeholder
            // root is an empty folder, and nothing has gone wrong.
            Err(e) if !e.is_refusal() && self.away_root(store, node).is_some() => Ok((store, Vec::new())),
            Err(e) => Err(e),
        }
    }

    async fn sync(&self, store: StoreId, have: Vec<(NodeId, Vec<u8>)>, list_unknown: bool) -> Result<(Vec<SyncedDoc>, Vec<NodeId>), ApiError> {
        self.of(store).sync(store, have, list_unknown).await
    }

    async fn apply(&self, store: StoreId, node: NodeId, update: Vec<u8>, client_id: &str) -> Result<(), ApiError> {
        self.gate(store, Write::Content)?;
        self.of(store).apply(store, node, update, client_id).await
    }

    async fn create_node(&self, store: StoreId, parent: NodeId, init: NodeInit, place: Place) -> Result<NodeId, ApiError> {
        self.gate(store, Write::Structure)?;
        self.of(store).create_node(store, parent, init, place).await
    }

    async fn create_attachment(&self, store: StoreId, host: NodeId, init: NodeInit) -> Result<NodeId, ApiError> {
        let write = if init.node_type == node_types::DISCUSSION { Write::Discussion } else { Write::Structure };
        self.gate(store, write)?;
        self.of(store).create_attachment(store, host, init).await
    }

    async fn move_node(&self, store: StoreId, node: NodeId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        self.gate(store, Write::Structure)?;
        self.of(store).move_node(store, node, parent, place).await
    }

    async fn move_nodes(&self, store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place) -> Result<Vec<Moved>, ApiError> {
        self.gate(store, Write::Structure)?;
        self.of(store).move_nodes(store, nodes, parent, place).await
    }

    /// Between two stores: both gated first, then refused with a sentence
    /// when the two are not one operation (an encrypted store and a plain
    /// one, or two backends), otherwise the one backend that serves both.
    async fn transplant(&self, from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        self.gate(from, Write::Structure)?;
        self.gate(to, Write::Structure)?;
        if self.capabilities(from).encrypted != self.capabilities(to).encrypted {
            return Err(ApiError::not_available(MIXED_TRANSPLANT_REFUSAL));
        }
        if self.served_by(from) != self.served_by(to) {
            return Err(ApiError::not_available(SPLIT_TRANSPLANT_REFUSAL));
        }
        self.of(from).transplant(from, node, to, parent, place).await
    }

    async fn delete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        self.gate(store, Write::Structure)?;
        self.of(store).delete(store, node).await
    }

    async fn undelete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        self.gate(store, Write::Structure)?;
        self.of(store).undelete(store, node).await
    }

    async fn resolve_link(&self, store: StoreId, node: NodeId) -> Result<LinkResolution, ApiError> {
        self.of(store).resolve_link(store, node).await
    }

    async fn list_deleted(&self, store: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
        match self.of(store).list_deleted(store).await {
            Err(e) if !e.is_refusal() && self.is_away(store) => Ok(Vec::new()),
            other => other,
        }
    }

    async fn list_attachments(&self, store: StoreId, host: NodeId) -> Result<Vec<Node>, ApiError> {
        let nodes = self.of(store).list_attachments(store, host).await?;
        Ok(nodes.into_iter().map(|n| self.while_away(store, n)).collect())
    }

    async fn list_discussions(&self, store: StoreId) -> Result<Vec<Node>, ApiError> {
        let nodes = self.of(store).list_discussions(store).await?;
        Ok(nodes.into_iter().map(|n| self.while_away(store, n)).collect())
    }

    /// Each store to the backend that serves it (so a plain store is never
    /// searched through an encrypted store's endpoint), the hits merged by
    /// score. A backend that fails leaves its stores out; every one failing
    /// is the first failure.
    async fn search(&self, query: &str, stores: Vec<StoreId>, semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError> {
        let mut groups: Vec<(BackendId, Vec<StoreId>)> = Vec::new();
        for store in stores {
            let id = self.served_by(store);
            match groups.iter_mut().find(|(b, _)| *b == id) {
                Some((_, group)) => group.push(store),
                None => groups.push((id, vec![store])),
            }
        }
        let mut results = Vec::new();
        let mut first_error = None;
        let mut answered = groups.is_empty();
        for (id, group) in groups {
            match self.backends[id].search(query, group, semantic, limit).await {
                Ok(mut hits) => {
                    answered = true;
                    results.append(&mut hits);
                }
                Err(e) => {
                    tracing::warn!("searching through backend {id} failed: {e}");
                    first_error.get_or_insert(e);
                }
            }
        }
        if !answered {
            return Err(first_error.expect("a group failed"));
        }
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results)
    }

    async fn put_blob(&self, store: StoreId, node: NodeId, mime: &str, bytes: &[u8]) -> Result<BlobUrl, ApiError> {
        self.gate(store, Write::Structure)?;
        self.of(store).put_blob(store, node, mime, bytes).await
    }

    async fn get_blob(&self, store: StoreId, blob: BlobId) -> Result<(String, Vec<u8>), ApiError> {
        self.of(store).get_blob(store, blob).await
    }

    /// Nobody is there to hear a frame while a store's owner is away, and
    /// nobody to say so to.
    async fn publish_presence(&self, frame: PresenceFrame, client_id: &str) -> Result<(), ApiError> {
        if self.is_away(frame.store_id) {
            return Ok(());
        }
        self.of(frame.store_id).publish_presence(frame, client_id).await
    }

    async fn who_am_i(&self) -> Result<(Option<String>, String), ApiError> {
        self.backends[0].who_am_i().await
    }

    async fn subscribe(&self, store: StoreId) -> Result<ChangeStream, ApiError> {
        self.of(store).subscribe(store).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;
    use futures::executor::block_on;

    /// A backend that answers for a few stores and remembers what it was
    /// asked; everything else is not there.
    struct Fake {
        caps: StoreCapabilities,
        stores: Vec<Store>,
        searched: StdMutex<Vec<Vec<StoreId>>>,
        transplanted: StdMutex<usize>,
    }

    fn fake(caps: StoreCapabilities, stores: &[StoreId]) -> Fake {
        let stores = stores
            .iter()
            .map(|id| {
                let mut s = Store::new_local("S", std::path::PathBuf::new());
                s.id = *id;
                s
            })
            .collect();
        Fake { caps, stores, searched: StdMutex::new(Vec::new()), transplanted: StdMutex::new(0) }
    }

    fn nothing<T>() -> Result<T, ApiError> {
        Err(ApiError::not_found("not in this fake"))
    }

    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    impl Backend for Fake {
        fn capabilities(&self, _store: StoreId) -> StoreCapabilities {
            self.caps
        }
        async fn list_stores(&self) -> Result<Vec<Store>, ApiError> {
            Ok(self.stores.clone())
        }
        async fn fetch_one(&self, _: StoreId, _: NodeId) -> Result<Node, ApiError> {
            nothing()
        }
        async fn fetch(&self, _: StoreId, _: Vec<NodeId>) -> Result<Vec<Node>, ApiError> {
            nothing()
        }
        async fn children(&self, _: StoreId, _: NodeId) -> Result<(StoreId, Vec<Node>), ApiError> {
            nothing()
        }
        async fn sync(&self, _: StoreId, _: Vec<(NodeId, Vec<u8>)>, _: bool) -> Result<(Vec<SyncedDoc>, Vec<NodeId>), ApiError> {
            nothing()
        }
        async fn apply(&self, _: StoreId, _: NodeId, _: Vec<u8>, _: &str) -> Result<(), ApiError> {
            Ok(())
        }
        async fn create_node(&self, _: StoreId, _: NodeId, _: NodeInit, _: Place) -> Result<NodeId, ApiError> {
            Ok(NodeId::new())
        }
        async fn create_attachment(&self, _: StoreId, _: NodeId, _: NodeInit) -> Result<NodeId, ApiError> {
            Ok(NodeId::new())
        }
        async fn move_node(&self, _: StoreId, node: NodeId, _: NodeId, _: Place) -> Result<Moved, ApiError> {
            Ok(Moved { node, left_shares: Vec::new() })
        }
        async fn move_nodes(&self, _: StoreId, _: Vec<NodeId>, _: NodeId, _: Place) -> Result<Vec<Moved>, ApiError> {
            Ok(Vec::new())
        }
        async fn transplant(&self, _: StoreId, _: NodeId, _: StoreId, _: NodeId, _: Place) -> Result<Moved, ApiError> {
            *self.transplanted.lock().unwrap() += 1;
            Ok(Moved { node: NodeId::new(), left_shares: Vec::new() })
        }
        async fn delete(&self, _: StoreId, _: NodeId) -> Result<(), ApiError> {
            Ok(())
        }
        async fn undelete(&self, _: StoreId, _: NodeId) -> Result<(), ApiError> {
            Ok(())
        }
        async fn resolve_link(&self, _: StoreId, _: NodeId) -> Result<LinkResolution, ApiError> {
            Ok(LinkResolution::Missing)
        }
        async fn list_deleted(&self, _: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
            nothing()
        }
        async fn list_attachments(&self, _: StoreId, _: NodeId) -> Result<Vec<Node>, ApiError> {
            nothing()
        }
        async fn search(&self, _: &str, stores: Vec<StoreId>, _: bool, _: usize) -> Result<Vec<SearchResultItem>, ApiError> {
            self.searched.lock().unwrap().push(stores.clone());
            Ok(stores
                .into_iter()
                .map(|store_id| SearchResultItem {
                    node_id: NodeId::new(),
                    store_id,
                    score: if self.caps.encrypted { 0.5 } else { 0.9 },
                    title: String::new(),
                    snippet: String::new(),
                    kind: "prose".into(),
                    node_type: "document".into(),
                    path: String::new(),
                })
                .collect())
        }
        async fn put_blob(&self, _: StoreId, _: NodeId, _: &str, _: &[u8]) -> Result<BlobUrl, ApiError> {
            nothing()
        }
        async fn get_blob(&self, _: StoreId, _: BlobId) -> Result<(String, Vec<u8>), ApiError> {
            nothing()
        }
        async fn publish_presence(&self, _: PresenceFrame, _: &str) -> Result<(), ApiError> {
            Ok(())
        }
        async fn who_am_i(&self) -> Result<(Option<String>, String), ApiError> {
            Ok((None, "fake".into()))
        }
        async fn subscribe(&self, _: StoreId) -> Result<ChangeStream, ApiError> {
            nothing()
        }
    }

    const ENCRYPTED: StoreCapabilities = StoreCapabilities {
        encrypted: true,
        mounts: false,
        pictures: false,
        sync_links: false,
        semantic_search: false,
        index_rebuild: false,
        local_documents: true,
    };

    /// A plain backend (the default) and an encrypted one, with two stores
    /// each.
    fn two() -> (Router, [StoreId; 4]) {
        let ids = [StoreId::new(), StoreId::new(), StoreId::new(), StoreId::new()];
        let mut router = Router::new(fake(StoreCapabilities::SERVER, &ids[..2]));
        let vault = router.add(fake(ENCRYPTED, &ids[2..]));
        router.serve(ids[2], vault);
        router.serve(ids[3], vault);
        (router, ids)
    }

    #[test]
    fn a_transplant_is_one_backend_s_or_refused_with_a_sentence() {
        let (router, [p1, p2, v1, v2]) = two();
        let (node, parent) = (NodeId::new(), NodeId::new());
        block_on(router.transplant(p1, node, p2, parent, Place::Last)).expect("two plain stores of one server");
        block_on(router.transplant(v1, node, v2, parent, Place::Last)).expect("two encrypted stores of one page");
        for (from, to) in [(p1, v1), (v2, p2)] {
            let refused = block_on(router.transplant(from, node, to, parent, Place::Last)).unwrap_err();
            assert_eq!(refused, ApiError::not_available(MIXED_TRANSPLANT_REFUSAL));
        }

        // Two plain stores two servers serve.
        let mut router = Router::new(fake(StoreCapabilities::SERVER, &[p1]));
        let other = router.add(fake(StoreCapabilities::SERVER, &[p2]));
        router.serve(p2, other);
        let refused = block_on(router.transplant(p1, node, p2, parent, Place::Last)).unwrap_err();
        assert_eq!(refused, ApiError::not_available(SPLIT_TRANSPLANT_REFUSAL));
    }

    #[test]
    fn a_transplant_is_gated_on_both_sides() {
        let (router, [p1, p2, ..]) = two();
        router.set_access(p2, Some(StoreAccess::Read));
        let refused = block_on(router.transplant(p1, NodeId::new(), p2, NodeId::new(), Place::Last)).unwrap_err();
        assert_eq!(refused, ApiError::ReadOnly);
    }

    /// Each store's half of a search goes to the backend that serves it,
    /// never the plain stores through the vault's endpoint; hits merge by
    /// score.
    #[test]
    fn a_search_asks_each_backend_for_its_own_stores() {
        let (router, [p1, p2, v1, v2]) = two();
        let hits = block_on(router.search("q", vec![v1, p1, v2, p2], true, 10)).unwrap();
        assert_eq!(hits.iter().map(|h| h.store_id).collect::<Vec<_>>(), vec![p1, p2, v1, v2], "by score, the plain stores' first here");
        let hits = block_on(router.search("q", vec![v1, p1], true, 1)).unwrap();
        assert_eq!(hits.len(), 1, "the limit holds across backends");
    }

    #[test]
    fn the_gate_refuses_what_the_account_may_not_change() {
        let (router, [p1, ..]) = two();
        assert!(router.gate(p1, Write::Structure).is_ok(), "nothing known refuses nothing");
        router.set_access(p1, Some(StoreAccess::Read));
        for write in [Write::Structure, Write::Discussion, Write::Content] {
            assert_eq!(router.gate(p1, write).unwrap_err(), ApiError::ReadOnly);
        }
        router.set_access(p1, Some(StoreAccess::Comment));
        assert_eq!(router.gate(p1, Write::Structure).unwrap_err(), ApiError::CommentOnly);
        assert!(router.gate(p1, Write::Discussion).is_ok());
        assert!(router.gate(p1, Write::Content).is_ok(), "the backend judges whose document an edit is of");
        let discussion = NodeInit { node_type: node_types::DISCUSSION.into(), ..Default::default() };
        block_on(router.create_attachment(p1, NodeId::new(), discussion)).expect("a commenter starts a discussion");
        assert_eq!(block_on(router.create_attachment(p1, NodeId::new(), NodeInit::default())).unwrap_err(), ApiError::CommentOnly);
    }

    #[test]
    fn a_store_whose_owner_is_away_takes_only_what_is_on_screen() {
        let (router, [_, _, v1, _]) = two();
        let mut row = Store::new_local("Recipes", std::path::PathBuf::new());
        row.id = v1;
        row.root_node_id = NodeId(v1.0);
        router.set_away(v1, Some(row));
        let away = ApiError::OwnerOffline { sentence: OWNER_OFFLINE_REFUSAL.into() };
        assert_eq!(block_on(router.create_node(v1, NodeId::new(), NodeInit::default(), Place::Last)).unwrap_err(), away);
        assert_eq!(block_on(router.delete(v1, NodeId::new())).unwrap_err(), away);
        block_on(router.apply(v1, NodeId::new(), Vec::new(), "c")).expect("an edit on screen goes to the backend to keep");

        // Not held anywhere: the placeholder root is an empty folder.
        let root = NodeId(v1.0);
        let (at, children) = block_on(router.children(v1, root)).unwrap();
        assert_eq!((at, children.len()), (v1, 0));
        let folder = block_on(router.fetch_one(v1, root)).unwrap();
        assert_eq!((folder.metadata.title.as_str(), folder.access), ("Recipes", StoreAccess::Read));
        assert!(block_on(router.list_deleted(v1)).unwrap().is_empty());
        assert!(block_on(router.fetch_one(v1, NodeId::new())).is_err(), "only the placeholder answers");

        router.set_away(v1, None);
        block_on(router.create_node(v1, NodeId::new(), NodeInit::default(), Place::Last)).expect("back");
    }

    #[test]
    fn the_stores_are_every_backend_s_once() {
        let (router, ids) = two();
        let listed = block_on(router.list_stores()).unwrap();
        assert_eq!(listed.iter().map(|s| s.id).collect::<Vec<_>>(), ids.to_vec());
        assert_eq!(router.capabilities(ids[2]), ENCRYPTED);
        assert_eq!(router.capabilities(ids[0]), StoreCapabilities::SERVER);
    }
}
