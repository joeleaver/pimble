//! What a change does to what a client shows of a store's tree
//! (docs/API_CONTRACT.md decision 3, wave 5): the rows and lists one change
//! touches, read from the documents held after the change was merged.
//!
//! A change already names what it touched (a node made under a parent, one
//! moved between two, a list that changed). [`affected`] says which rows and
//! which lists that is; [`Pimble::shown`] reads them from what is held, so a
//! client updates exactly those, with no round trip to ask the server what
//! it has just said. Whoever holds a store's documents answers: `Held` for a
//! store a server holds, the backend itself for one it holds in this process
//! (the browser's vault), so both end in the same [`View`].

use pimble_core::{node_types, Node, NodeId, StoreId};
use pimble_rpc::StoreChangeKind;

use crate::{Addr, Pimble};

/// The rows and lists one change touches, by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Affected {
    /// Nodes whose row may read differently (title, icon, access, whether it
    /// has children).
    pub nodes: Vec<NodeId>,
    /// Parents whose list of children may have changed.
    pub lists: Vec<NodeId>,
    /// Nodes whose parent's list may have changed, the parent being wherever
    /// the node is now (a list that changed names its own document too).
    pub parents_of: Vec<NodeId>,
    /// Nodes that left the tree with everything under them.
    pub gone: Vec<NodeId>,
}

impl Affected {
    /// Every document a change of `kind` touched, as rows and lists.
    pub fn of(kind: &StoreChangeKind) -> Self {
        match kind {
            StoreChangeKind::NodeCreated { node_id, parent_id } => {
                Self { nodes: vec![*node_id, *parent_id], lists: vec![*parent_id], ..Self::default() }
            }
            StoreChangeKind::NodeDeleted { node_id, parent_id } => {
                Self { nodes: vec![*parent_id], lists: vec![*parent_id], gone: vec![*node_id], ..Self::default() }
            }
            StoreChangeKind::NodeMoved { node_id, old_parent_id, new_parent_id } => Self {
                nodes: vec![*node_id, *old_parent_id, *new_parent_id],
                lists: vec![*old_parent_id, *new_parent_id],
                ..Self::default()
            },
            StoreChangeKind::TreeStructure { node_ids } => {
                Self { nodes: node_ids.clone(), lists: node_ids.clone(), parents_of: node_ids.clone(), ..Self::default() }
            }
            StoreChangeKind::MetadataUpdated { node_id } | StoreChangeKind::ContentUpdated { node_id } => {
                Self { nodes: vec![*node_id], ..Self::default() }
            }
            // An attachment is in no list; its host's count is the
            // discussions' to follow. The rest are not a document's.
            StoreChangeKind::AttachmentChanged { .. }
            | StoreChangeKind::SyncStateChanged { .. }
            | StoreChangeKind::MountStateChanged { .. }
            | StoreChangeKind::ShareStateChanged { .. }
            | StoreChangeKind::SharesEnded { .. }
            | StoreChangeKind::PeopleChanged
            | StoreChangeKind::VaultAppended { .. } => Self::default(),
        }
    }

    /// Rows only: what a write of these nodes' own documents changed.
    pub fn rows(nodes: Vec<NodeId>) -> Self {
        Self { nodes, ..Self::default() }
    }

    /// Everything a catch-up changed: each document's row, its own list and
    /// its parent's.
    pub fn documents(ids: Vec<NodeId>) -> Self {
        Self { nodes: ids.clone(), lists: ids.clone(), parents_of: ids, ..Self::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.lists.is_empty() && self.parents_of.is_empty() && self.gone.is_empty()
    }

    /// This and `other`, each id once.
    pub fn and(mut self, other: Affected) -> Self {
        fn join(into: &mut Vec<NodeId>, from: Vec<NodeId>) {
            for id in from {
                if !into.contains(&id) {
                    into.push(id);
                }
            }
        }
        join(&mut self.nodes, other.nodes);
        join(&mut self.lists, other.lists);
        join(&mut self.parents_of, other.parents_of);
        join(&mut self.gone, other.gone);
        self
    }

    /// Every id named, for bringing those documents up to date.
    fn ids(&self) -> Vec<NodeId> {
        let mut ids = Vec::new();
        for id in self.nodes.iter().chain(&self.lists).chain(&self.parents_of) {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        ids
    }
}

/// One parent's children as held now.
#[derive(Debug, Clone)]
pub struct Listed {
    pub parent: NodeId,
    /// The store the children live in (the parent's: a mount's list is its
    /// source's, which a client maps itself).
    pub children_store: StoreId,
    pub children: Vec<Node>,
}

/// What a client shows of `store` that one change touched, as held now: the
/// rows that are still live, the lists of parents still live, and the nodes
/// that left. A node or a parent this client does not hold is left out:
/// nothing here shows it.
#[derive(Debug, Clone)]
pub struct View {
    pub store: StoreId,
    pub nodes: Vec<Node>,
    pub lists: Vec<Listed>,
    pub gone: Vec<NodeId>,
}

impl View {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.lists.is_empty() && self.gone.is_empty()
    }
}

impl Pimble {
    /// A node as held, without asking anyone: `None` when it is not held or
    /// not live. For a store whose backend holds the documents, that
    /// backend's (in this process) answer.
    async fn held_now(&self, addr: Addr) -> Option<Node> {
        if self.backend().capabilities(addr.store).local_documents {
            return self.backend().fetch_one(addr.store, addr.node).await.ok();
        }
        self.drain();
        let held = self.held.lock().unwrap();
        held.store(addr.store)?.node(addr.node)
    }

    /// What `affected` touched in `store`, read from the documents held now
    /// (docs/API_CONTRACT.md wave 5). A list is read only for a parent held
    /// and not a mount (a mount's children are its source's list, named by
    /// the source's own changes); a child of it not held yet is fetched
    /// with the list, which is the one time this asks the server anything.
    pub async fn shown(&self, store: StoreId, affected: &Affected) -> View {
        let mut nodes: Vec<Node> = Vec::new();
        for &id in &affected.nodes {
            if let Some(node) = self.held_now(Addr::new(store, id)).await {
                nodes.push(node);
            }
        }
        let mut parents: Vec<NodeId> = affected.lists.clone();
        for &id in &affected.parents_of {
            let parent = match nodes.iter().find(|n| n.id == id) {
                Some(node) => node.parent_id,
                None => self.held_now(Addr::new(store, id)).await.and_then(|n| n.parent_id),
            };
            if let Some(parent) = parent.filter(|p| !parents.contains(p)) {
                parents.push(parent);
            }
        }
        let mut lists = Vec::new();
        for parent in parents {
            let shown = match nodes.iter().find(|n| n.id == parent) {
                Some(node) => Some(node.clone()),
                None => self.held_now(Addr::new(store, parent)).await,
            };
            let Some(shown) = shown else { continue };
            if shown.node_type == node_types::MOUNT {
                continue;
            }
            match self.listing(Addr::new(store, parent)).await {
                Ok((children_store, children)) => lists.push(Listed { parent, children_store, children }),
                Err(e) => tracing::debug!("the list of {parent} could not be read for a view: {e}"),
            }
        }
        View { store, nodes, lists, gone: affected.gone.clone() }
    }

    /// [`Pimble::shown`] after a structural call this client made: the
    /// documents it touched are brought up to date first (one reconcile),
    /// since the change that says what the call did may not have been heard
    /// yet. What the store's subscription says of it later is the same view
    /// again.
    pub async fn shown_after(&self, store: StoreId, affected: &Affected) -> View {
        if !self.backend().capabilities(store).local_documents {
            let have: Vec<(NodeId, Vec<u8>)> = {
                let held = self.held.lock().unwrap();
                match held.store(store) {
                    Some(h) => affected.ids().into_iter().filter_map(|id| Some((id, h.tree.doc(id)?.state_vector()))).collect(),
                    None => Vec::new(),
                }
            };
            if !have.is_empty() {
                match self.backend().sync(store, have, false).await {
                    Ok((docs, _)) => {
                        let mut held = self.held.lock().unwrap();
                        let h = held.store_mut(store);
                        for d in docs {
                            h.merge(d.node, &d.diff, false);
                        }
                    }
                    Err(e) => tracing::debug!("held documents of {store} not brought up to date: {e}"),
                }
            }
        }
        self.shown(store, affected).await
    }

    /// Ask the server again what this principal may do with `nodes` of
    /// `store` (what it may do in the store changed: a role, a read-only
    /// root). Its judgement is not in any document, so this is a fetch: one
    /// for all of them. Answers the nodes as judged now.
    pub async fn rejudge(&self, store: StoreId, nodes: Vec<NodeId>) -> Vec<Node> {
        if nodes.is_empty() {
            return Vec::new();
        }
        let fetched = match self.backend().fetch(store, nodes).await {
            Ok(fetched) => fetched,
            Err(e) => {
                tracing::debug!("the nodes of {store} could not be judged again: {e}");
                return Vec::new();
            }
        };
        if self.backend().capabilities(store).local_documents {
            return fetched;
        }
        let mut held = self.held.lock().unwrap();
        let h = held.store_mut(store);
        for node in &fetched {
            h.take(node);
        }
        fetched.into_iter().filter_map(|n| h.node(n.id).or(Some(n))).collect()
    }
}
