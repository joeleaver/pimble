//! What this client holds (docs/API_CONTRACT.md decision 3): per store, a
//! `pimble_crdt::Tree` of the node documents it has read, kept current by
//! the store's subscription, plus what the server judged of each node when
//! it handed it over (access, attachment count). Reads are answered from
//! here through the same rules every holder of documents applies
//! (`Tree::node`, `NodeFields::into_node`); nothing here does I/O, which
//! [`crate::Pimble`] does around it.
//!
//! What is held is bounded by what was asked for: a node, a list, or (for a
//! walk, a name looked up anywhere) a whole store.

use std::collections::{HashMap, HashSet};

use pimble_core::{Node, NodeId, Store, StoreAccess, StoreId};
use pimble_crdt::{NodeDoc, Tree};
use pimble_rpc::StoreChangeKind;

use crate::backend::{document_of, StoreChange};

/// What the server said of a node when it handed it over, which its
/// document cannot say: what this principal may do with it, and how many
/// attachments it has. `stale` once something may have changed it (an
/// attachment came or went, the subscription was remade).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Judged {
    pub access: StoreAccess,
    pub attachments: u32,
    pub stale: bool,
}

pub(crate) struct HeldStore {
    pub tree: Tree,
    pub judged: HashMap<NodeId, Judged>,
    /// Ids asked for and not handed over (deleted, outside the principal's
    /// scope): not asked for again until a change names them.
    pub absent: HashSet<NodeId>,
    /// Every document the principal may read is held (`Pimble::hold_store`):
    /// an update for one not held is then a new node's, and is taken.
    pub complete: bool,
    /// The store's subscription is live, so what is held is current. When it
    /// is not, a read fetches again.
    pub live: bool,
    /// What the store's row says this principal may do in it: a node's
    /// access until the server judges that node.
    pub store_access: StoreAccess,
}

impl HeldStore {
    fn new(root: NodeId, store_access: StoreAccess) -> Self {
        Self {
            tree: Tree::from_docs(root, HashMap::new()),
            judged: HashMap::new(),
            absent: HashSet::new(),
            complete: false,
            live: false,
            store_access,
        }
    }

    /// Take a node the server handed over whole: its document merged into
    /// what is held (or held from now), and its judgement.
    pub fn take(&mut self, node: &Node) {
        // An answer without the document says nothing to hold.
        if node.content.is_empty() {
            return;
        }
        let merged = match self.tree.doc_mut(node.id) {
            Some(doc) => doc.apply_update(&node.content).is_ok(),
            None => match NodeDoc::load(&node.content) {
                Ok(doc) => {
                    self.tree.insert_doc(node.id, doc);
                    true
                }
                Err(_) => false,
            },
        };
        if merged {
            self.judged.insert(node.id, Judged { access: node.access, attachments: node.attachments, stale: false });
            self.absent.remove(&node.id);
        }
    }

    /// Merge an update into a held document; with `or_new`, into a new one
    /// when it is not held (a store held whole hears of a new node only this
    /// way). Answers whether it was merged.
    pub fn merge(&mut self, id: NodeId, update: &[u8], or_new: bool) -> bool {
        if self.tree.doc(id).is_none() && !or_new {
            return false;
        }
        self.absent.remove(&id);
        self.tree.apply_update(id, update).is_ok()
    }

    /// The live node `id` as an answer carries it: its fields, its whole
    /// document as the content, its children, and the server's judgement.
    /// Children not held yet are named from the stored list, so a count is
    /// right before they are read. `None` for a node not held or not live.
    pub fn node(&self, id: NodeId) -> Option<Node> {
        let mut node = self.tree.node(id).ok()?;
        let listed = self.listed(id);
        if listed.iter().any(|c| self.tree.doc(*c).is_none() && !self.absent.contains(c)) {
            node.children = listed;
        }
        match self.judged.get(&id) {
            Some(j) => {
                node.access = j.access;
                node.attachments = j.attachments;
            }
            None => node.access = self.store_access,
        }
        Some(node)
    }

    /// The children `id`'s document lists, each once, itself left out.
    pub fn listed(&self, id: NodeId) -> Vec<NodeId> {
        let Some(doc) = self.tree.doc(id) else { return Vec::new() };
        let mut seen = HashSet::new();
        doc.children().into_iter().filter(|&c| c != id && seen.insert(c)).collect()
    }

    /// Whether `id` is held with a judgement that is still good.
    pub fn fresh(&self, id: NodeId) -> bool {
        self.live && self.tree.has_node(id) && self.judged.get(&id).is_some_and(|j| !j.stale)
    }

    /// Every held document's id and state vector, for a reconcile.
    pub fn state_vectors(&self) -> Vec<(NodeId, Vec<u8>)> {
        self.tree.ids().into_iter().filter_map(|id| Some((id, self.tree.doc(id)?.state_vector()))).collect()
    }
}

#[derive(Default)]
pub(crate) struct Held {
    /// The stores as last listed.
    pub stores: Vec<Store>,
    pub listed: bool,
    pub held: HashMap<StoreId, HeldStore>,
}

impl Held {
    pub fn store_row(&self, id: StoreId) -> Option<&Store> {
        self.stores.iter().find(|s| s.id == id)
    }

    /// The held part of `store`, made empty the first time.
    pub fn store_mut(&mut self, store: StoreId) -> &mut HeldStore {
        let row = self.store_row(store).map(|s| (s.root_node_id, s.access));
        let (root, access) = row.unwrap_or_else(|| (NodeId::new(), StoreAccess::Full));
        self.held.entry(store).or_insert_with(|| HeldStore::new(root, access))
    }

    pub fn store(&self, store: StoreId) -> Option<&HeldStore> {
        self.held.get(&store)
    }

    /// What a store's change does to what is held: the update merged into
    /// the document it is about, judgements it may have changed marked to be
    /// asked again, the store row's link state followed.
    pub fn apply(&mut self, change: &StoreChange) {
        if let StoreChangeKind::SyncStateChanged { state } = &change.kind {
            if let Some(row) = self.stores.iter_mut().find(|s| s.id == change.store) {
                row.sync_state = state.clone();
            }
        }
        if matches!(change.kind, StoreChangeKind::SharesEnded { .. }) {
            // The roots a store shows changed: the rows are listed again.
            self.listed = false;
        }
        let Some(held) = self.held.get_mut(&change.store) else { return };
        if let StoreChangeKind::AttachmentChanged { host_id, .. } = &change.kind {
            if let Some(j) = held.judged.get_mut(host_id) {
                j.stale = true;
            }
        }
        if let Some(id) = document_of(&change.kind) {
            // Asked for before and not there; it may be now (put back, or
            // newly in scope).
            held.absent.remove(&id);
            if let Some(update) = change.update.as_deref() {
                let complete = held.complete;
                held.merge(id, update, complete);
            }
        }
    }

    /// The subscription of `store` ended (the connection went, or it fell
    /// behind): nothing held there is known to be current.
    pub fn lost(&mut self, store: StoreId) {
        if let Some(held) = self.held.get_mut(&store) {
            held.live = false;
        }
    }
}
