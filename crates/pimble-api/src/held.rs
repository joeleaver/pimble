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
    /// Counts every change to what is held here (a merge, a document
    /// taken, the subscription lost), so what was derived from the held
    /// documents can be kept until it moves (`crate::references`).
    pub generation: u64,
}

/// A generation no held store has had: a store forgotten and held again
/// never repeats one, so nothing derived before is taken for current.
fn next_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl HeldStore {
    fn bump(&mut self) {
        self.generation = next_generation();
    }

    fn new(root: NodeId, store_access: StoreAccess) -> Self {
        Self {
            tree: Tree::from_docs(root, HashMap::new()),
            judged: HashMap::new(),
            absent: HashSet::new(),
            complete: false,
            live: false,
            store_access,
            generation: next_generation(),
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
            Some(doc) => match doc.apply_update(&node.content) {
                Ok(effect) => {
                    if effect.changed {
                        self.bump();
                    }
                    true
                }
                Err(_) => false,
            },
            None => match NodeDoc::load(&node.content) {
                Ok(doc) => {
                    self.bump();
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
        match self.tree.apply_update(id, update) {
            Ok(effect) => {
                if effect.changed {
                    self.bump();
                }
                true
            }
            Err(_) => {
                self.bump();
                false
            }
        }
    }

    /// Hold `id` from `update` when the update is its whole document: a node
    /// made elsewhere is announced with the update that brought it into
    /// being, and a store's root reaches a new replica the same way. Only a
    /// document that reads whole (its fields there, nothing waiting on state
    /// this side never had) is taken; an update that continues a document
    /// not held is left, and the node is fetched when it is asked for.
    fn adopt_whole(&mut self, id: NodeId, update: &[u8]) {
        if self.tree.doc(id).is_some() {
            return;
        }
        let Ok(doc) = NodeDoc::load(update) else { return };
        let whole = {
            use yrs::{ReadTxn, Transact};
            !doc.ydoc().transact().has_missing_updates()
        };
        if whole && doc.is_initialised() && doc.fields().is_ok() {
            self.bump();
            self.tree.insert_doc(id, doc);
        }
    }

    /// Derive again what this principal may do with `id` and the held nodes
    /// under it from where `id` now is: a node takes its parent's access,
    /// which is how every holder judges it (a share's role reaches what is
    /// under its root; a replica's read-only roots make what is under them
    /// read-only). A share's root keeps its own judgement, an attachment is
    /// not in the tree, and nothing is derived from a parent not judged.
    /// Answers the nodes whose access changed.
    fn inherit(&mut self, id: NodeId) -> Vec<NodeId> {
        let mut changed = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            let Some(fields) = self.tree.doc(n).and_then(|d| d.fields().ok()) else { continue };
            if fields.attached_to.is_some() {
                continue;
            }
            stack.extend(self.listed(n));
            if fields.custom.contains_key(pimble_core::custom_keys::SHARE) && self.judged.contains_key(&n) {
                continue;
            }
            let Some(parent) = fields.parent_id.and_then(|p| self.judged.get(&p)).map(|j| j.access) else { continue };
            let held = self.judged.entry(n).or_insert(Judged { access: self.store_access, attachments: 0, stale: false });
            if held.access != parent {
                held.access = parent;
                changed.push(n);
            }
        }
        changed
    }

    /// Merge an update into a held document, answering whether it changed
    /// anything there (a catch-up's diff is never empty, even when it brings
    /// nothing new).
    pub fn merge_changed(&mut self, id: NodeId, update: &[u8]) -> bool {
        if self.tree.doc(id).is_none() {
            return false;
        }
        self.absent.remove(&id);
        let changed = self.tree.apply_update(id, update).is_ok_and(|effect| effect.changed);
        if changed {
            self.bump();
        }
        changed
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
    /// asked again or derived again, the store row's link state followed.
    /// Answers the held nodes whose judgement was derived again with another
    /// access than they had (a folder moved under a root this principal only
    /// reads), which nothing in their own documents says.
    pub fn apply(&mut self, change: &StoreChange) -> Vec<NodeId> {
        if let StoreChangeKind::SyncStateChanged { state } = &change.kind {
            if let Some(row) = self.stores.iter_mut().find(|s| s.id == change.store) {
                row.sync_state = state.clone();
            }
        }
        if matches!(change.kind, StoreChangeKind::SharesEnded { .. }) {
            // The roots a store shows changed: the rows are listed again.
            self.listed = false;
        }
        let Some(held) = self.held.get_mut(&change.store) else { return Vec::new() };
        if let StoreChangeKind::AttachmentChanged { host_id, .. } = &change.kind {
            if let Some(j) = held.judged.get_mut(host_id) {
                j.stale = true;
            }
        }
        let Some(id) = document_of(&change.kind) else { return Vec::new() };
        // Asked for before and not there; it may be now (put back, or newly
        // in scope).
        held.absent.remove(&id);
        if let Some(update) = change.update.as_deref() {
            let complete = held.complete;
            if !held.merge(id, update, complete) {
                held.adopt_whole(id, update);
            }
        }
        match &change.kind {
            StoreChangeKind::NodeCreated { node_id, .. } | StoreChangeKind::NodeMoved { node_id, .. } => held.inherit(*node_id),
            _ => Vec::new(),
        }
    }

    /// The subscription of `store` ended (the connection went, or it fell
    /// behind): nothing held there is known to be current.
    pub fn lost(&mut self, store: StoreId) {
        if let Some(held) = self.held.get_mut(&store) {
            held.live = false;
            held.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::Place;

    const T: &str = "2026-11-02T00:00:00.000Z";

    /// A store as a server holds it: the root, a folder this principal only
    /// reads (`Reading`), one it edits (`Mine`), and `Mine/Note/Child`.
    struct Fixture {
        server: Tree,
        held: Held,
        store: StoreId,
        reading: NodeId,
        mine: NodeId,
        note: NodeId,
        child: NodeId,
    }

    fn fixture() -> Fixture {
        let root = NodeId::new();
        let mut server = Tree::new(root, "Work", T).unwrap();
        let [reading, mine, note, child] = [NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new()];
        server.add_node(reading, Some(root), Place::Last, "folder", "Reading", T).unwrap();
        server.add_node(mine, Some(root), Place::Last, "folder", "Mine", T).unwrap();
        server.add_node(note, Some(mine), Place::Last, "document", "Note", T).unwrap();
        server.add_node(child, Some(note), Place::Last, "document", "Child", T).unwrap();
        let store = StoreId::new();
        let mut held = Held::default();
        let h = held.store_mut(store);
        for (id, access) in [(root, StoreAccess::Full), (reading, StoreAccess::Read), (mine, StoreAccess::Full), (note, StoreAccess::Full), (child, StoreAccess::Full)] {
            let mut node = server.node(id).unwrap();
            node.access = access;
            h.take(&node);
        }
        h.complete = false;
        Fixture { server, held, store, reading, mine, note, child }
    }

    fn change(store: StoreId, kind: StoreChangeKind, update: Vec<u8>) -> StoreChange {
        StoreChange { store, kind, source: Some("elsewhere".into()), update: Some(update) }
    }

    /// A node moved under a root this principal only reads is read-only
    /// here at once, and so is everything held under it: their documents
    /// say nothing of it, so the judgement is derived from where they are.
    #[test]
    fn a_move_derives_access_again_for_the_held_subtree() {
        let mut f = fixture();
        let edit = f.server.move_node(f.note, f.reading, Place::Last, T).unwrap();
        let mut rejudged = Vec::new();
        for (id, update) in edit.touched {
            let kind = if id == f.note {
                StoreChangeKind::NodeMoved { node_id: id, old_parent_id: f.mine, new_parent_id: f.reading }
            } else {
                StoreChangeKind::TreeStructure { node_ids: vec![id] }
            };
            rejudged.extend(f.held.apply(&change(f.store, kind, update)));
        }
        let rejudged: HashSet<NodeId> = rejudged.into_iter().collect();
        assert_eq!(rejudged, HashSet::from([f.note, f.child]));
        let h = f.held.store(f.store).unwrap();
        assert_eq!(h.node(f.note).unwrap().access, StoreAccess::Read);
        assert_eq!(h.node(f.child).unwrap().access, StoreAccess::Read);
        assert_eq!(h.node(f.mine).unwrap().access, StoreAccess::Full);
        assert_eq!(h.listed(f.reading), vec![f.note]);
    }

    /// A node made elsewhere is announced with the update that brought it
    /// into being: it is held from that, judged as its parent is, and its
    /// parent lists it, with nothing fetched. An update that continues a
    /// document not held is not mistaken for one.
    #[test]
    fn a_node_made_elsewhere_is_held_from_its_announcement() {
        let mut f = fixture();
        let new = NodeId::new();
        let edit = f.server.add_node(new, Some(f.reading), Place::Last, "document", "New", T).unwrap();
        for (id, update) in edit.touched {
            let kind = if id == new {
                StoreChangeKind::NodeCreated { node_id: id, parent_id: f.reading }
            } else {
                StoreChangeKind::TreeStructure { node_ids: vec![id] }
            };
            f.held.apply(&change(f.store, kind, update));
        }
        let h = f.held.store(f.store).unwrap();
        let node = h.node(new).expect("held from its announcement");
        assert_eq!(node.metadata.title, "New");
        assert_eq!(node.access, StoreAccess::Read, "judged as its parent is");
        assert_eq!(h.listed(f.reading), vec![new]);

        // A rename of a node never held: only the rename's bytes, which do
        // not make a document.
        let elsewhere = NodeId::new();
        f.server.add_node(elsewhere, Some(f.mine), Place::Last, "document", "Old", T).unwrap();
        let before = f.server.doc(elsewhere).unwrap().state_vector();
        f.server.set_title(elsewhere, "Renamed", T).unwrap();
        let delta = f.server.doc(elsewhere).unwrap().diff_since(&before).unwrap();
        f.held.apply(&change(f.store, StoreChangeKind::MetadataUpdated { node_id: elsewhere }, delta));
        assert!(f.held.store(f.store).unwrap().tree.doc(elsewhere).is_none());
    }
}
