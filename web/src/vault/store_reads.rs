//! What one encrypted store says about its documents: access, the tree, nodes.

use std::collections::HashMap;

use pimble_core::{node_types, DeletedNode, Node, NodeId, NodeMetadata, Store, StoreAccess};
use pimble_crdt::{NodeDoc, TreeEdit};

use super::{AccountStore, VaultStore};

impl VaultStore {
    /// Whether this account may write `node_id`'s document, by the rule
    /// [`VaultStore::node_for`] puts on every node it hands the UI.
    pub(super) fn may_write(&self, node_id: NodeId, row: Option<&AccountStore>) -> bool {
        self.access_of(node_id, row).allows_write()
    }

    /// What this account may change of `node_id`'s document: the roles of
    /// the roots above it, and a discussion is a commenter's to write
    /// (docs/DISCUSSIONS_CONTRACT.md section 7), as the server's
    /// `judge_access` answers.
    pub(super) fn access_of(&self, node_id: NodeId, row: Option<&AccountStore>) -> StoreAccess {
        let Some(row) = row else { return StoreAccess::Full };
        let grant_roots: Vec<NodeId> = row.grants.iter().filter_map(|g| g.root).collect();
        row.node_access(&self.tree.roots_above(&grant_roots, node_id)).on_document(self.is_discussion(node_id))
    }

    /// Whether `node_id` is a discussion node.
    fn is_discussion(&self, node_id: NodeId) -> bool {
        self.tree.doc(node_id).and_then(|doc| doc.fields().ok()).is_some_and(|f| f.node_type == node_types::DISCUSSION)
    }

    /// How far `node_id`'s log has been read without a gap.
    fn read_through(&self, node_id: NodeId) -> u64 {
        self.docs.get(&node_id).map(|doc| doc.cursor.applied_through()).unwrap_or(0)
    }

    /// The documents the server is known to hold more of than this page has
    /// read, with where to read each from: listed and never opened, or held
    /// with a blob nothing here opened.
    pub(super) fn behind(&self) -> Vec<(NodeId, u64)> {
        let mut behind: Vec<(NodeId, u64)> = self
            .heads
            .iter()
            .map(|(id, head)| (*id, *head, self.read_through(*id)))
            .filter(|(_, head, from)| head > from)
            .map(|(id, _, from)| (id, from))
            .collect();
        behind.sort_by_key(|(id, _)| id.to_string());
        behind
    }

    /// What a look reads again: everything [`behind`](Self::behind) when a
    /// scope key has just arrived, and otherwise the documents whose wraps
    /// this page never had. Either way those are now asked about.
    pub(super) fn reread_list(&mut self, new_key: bool) -> Vec<(NodeId, u64)> {
        let unread = std::mem::take(&mut self.look.unread);
        self.look.asked.extend(unread.iter().map(|(id, key_id)| (*id, *key_id)));
        if new_key {
            return self.behind();
        }
        let mut wanted: Vec<(NodeId, u64)> = unread.into_keys().map(|id| (id, self.read_through(id))).collect();
        wanted.sort_by_key(|(id, _)| id.to_string());
        wanted
    }

    /// Repair the tree if it needs it (`Tree::repair`): what it wrote, to be
    /// appended like any edit.
    ///
    /// A scoped store is repaired one scope at a time (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5): each scope root is a root, a document that leads to no
    /// scope root is left alone, and a scope that lists a document not held
    /// yet is not judged yet. The repair a desktop's partial replica makes,
    /// so the two never undo each other's fixes.
    pub(super) fn repair_now(&mut self, now: &str) -> Option<TreeEdit> {
        match self.tree.repair_held(&self.scope_roots, now) {
            Ok(edit) => edit,
            Err(e) => {
                tracing::warn!("Repairing the tree of {} failed: {}", self.listed.id, e);
                None
            }
        }
    }

    /// Re-root the tree when the documents say the root is another node
    /// than the one it started from: the manifest's root is a placeholder
    /// nothing has written, and exactly one node has no parent. The store
    /// as the UI should now see it, to be announced again so the tree is
    /// fetched under the real root.
    ///
    /// A store opened before its documents arrived — hosted from a desktop
    /// whose link had not pushed yet — starts from the placeholder; as the
    /// desktop's root document lands this is what moves the tree under it.
    pub(super) fn adopt_root(&mut self) -> Option<Store> {
        if self.is_partial() {
            // A scope's root is the grant's, not the documents'. Nothing this
            // page holds has no parent, and the first node whose parent is
            // simply not here is not a root.
            return None;
        }
        let current = self.tree.root();
        let root = self.tree.document_root(current).unwrap_or(current);
        if root == current {
            return None;
        }
        tracing::info!("Store {}: root {} replaced by the documents' root {}", self.listed.id, current, root);
        self.tree.reroot(root);
        Some(self.view())
    }

    /// The store as the UI should see it: what the server listed, with the
    /// tree's root.
    pub(super) fn view(&self) -> Store {
        let mut store = self.listed.clone();
        store.root_node_id = self.tree.root();
        store
    }

    /// Write the root's document when the store has none: a store the
    /// accounts service has just created has an empty log and a root id in
    /// its manifest, and nothing else will ever write that document.
    ///
    /// Done at the first write under the root rather than at open, so that
    /// a store whose documents are on their way from elsewhere (a desktop's
    /// link pushing them) is not given a second root by a page that merely
    /// looked at it; two roots each repair the other under themselves and
    /// never agree again. The update is the whole document, since nothing
    /// else holds any of it.
    pub(super) fn seed_root(&mut self, now: &str) -> TreeEdit {
        let root = self.tree.root();
        let mut edit = TreeEdit::default();
        // Never for a share: the shared node's document exists in the owner's
        // store, and writing one here would make a second node of the same id
        // that no repair could reconcile.
        if self.is_partial() || self.tree.has_node(root) || self.tree.doc(root).is_some_and(NodeDoc::is_initialised) {
            return edit;
        }
        let mut doc = self.tree.take_doc(root).unwrap_or_default();
        match doc.init(node_types::FOLDER, &self.listed.name, None, now) {
            Ok(()) => edit.touched.push((root, doc.save())),
            Err(e) => tracing::warn!("Seeding the root of {} failed: {}", self.listed.id, e),
        }
        self.tree.insert_doc(root, doc);
        edit
    }

    /// The children of `node_id` from the tree. The root with no document
    /// yet (see [`VaultStore::seed_root`]) has none rather than being an
    /// error: the row is there, the store is simply empty.
    pub(super) fn children_of(&self, node_id: NodeId) -> Result<Vec<NodeId>, String> {
        if node_id == self.tree.root() && !self.tree.has_node(node_id) && !self.is_partial() {
            return Ok(Vec::new());
        }
        self.tree.get_children(node_id).map_err(|e| e.to_string())
    }

    /// [`VaultStore::node_of`] as the UI is handed it: with what this account
    /// may change of the node (`Node::access`), which a server puts on every
    /// node it returns and this page, holding the documents itself, judges
    /// from the account's grants (`row`) by the hosted server's own rule. It
    /// is the app's one source for what to offer: a store can hold a root the
    /// account reads beside one it edits, and the hosted server refuses the
    /// writes under the first one at a time. No row is a store nothing is
    /// known about, which is no reason to disable anything.
    pub(super) fn node_for(&self, node_id: NodeId, row: Option<&AccountStore>) -> Result<Node, String> {
        let mut node = self.node_of(node_id)?;
        let access = self.access_of(node_id, row);
        if !access.allows_write() {
            node.access = access;
        }
        Ok(node)
    }

    /// Assemble the `Node` the app expects from the tree's view of a node and
    /// its document's bytes, as the server assembles one: the content is the
    /// whole node document, which the editor joins as it joined a content
    /// document (Pimble's roots ride along). The root with no document yet
    /// is a folder named after the store.
    pub(super) fn node_of(&self, node_id: NodeId) -> Result<Node, String> {
        let root = self.tree.root();
        if node_id == root && !self.tree.has_node(root) && !self.is_partial() {
            let now = chrono::Utc::now();
            return Ok(Node {
                id: root,
                parent_id: None,
                node_type: node_types::FOLDER.to_string(),
                metadata: NodeMetadata {
                    title: self.listed.name.clone(),
                    created_at: now,
                    modified_at: now,
                    tags: Vec::new(),
                    text_tags: Vec::new(),
                    custom: HashMap::new(),
                    fields: Vec::new(),
                    metanode: None,
                },
                content: Vec::new(),
                children: Vec::new(),
                became: None,
                attached_to: None,
                attachments: 0,
                access: StoreAccess::Full,
            });
        }
        // `Tree::node`, the assembly `LocalStore` answers with too; `access`
        // is judged per account in `node_for`, never part of the node.
        let mut node = self.tree.node(node_id).map_err(|e| e.to_string())?;
        node.attachments = self.tree.attachments_of(node_id).len() as u32;
        Ok(node)
    }

    /// "Recently Deleted..." (docs/MOVE_CONTRACT.md "Seeing and undoing what
    /// was removed"): `Tree::deleted`, the server's own list, each node
    /// marked read-only where this account may not put it back. A share that
    /// has ended has already taken its documents with it
    /// ([`VaultStore::drop_roots`]), so nothing further is excluded for that
    /// here.
    pub(super) fn list_deleted(&self, row: Option<&AccountStore>) -> Vec<DeletedNode> {
        let roots: Vec<NodeId> = if self.is_partial() { self.scope_roots.clone() } else { Vec::new() };
        let mut out = Vec::new();
        for entry in self.tree.deleted(self.tree.root(), &roots, |_| true, |_| false) {
            let Some(mut node) = self.tree.node_any(entry.id) else { continue };
            if !self.may_write(entry.id, row) {
                node.access = StoreAccess::Read;
            }
            out.push(DeletedNode { node, parent_title: entry.parent_title, deleted_at: entry.deleted_at, put_back_under: entry.put_back_under });
        }
        out
    }
}
