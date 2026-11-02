//! The rules every holder of a store's documents applies the same way
//! (docs/API_CONTRACT.md decision 8): the server over a plain store's
//! documents, `pimble-store`'s `LocalStore`, and the browser's vault client
//! over the documents it decrypted. Each used to be written two or three
//! times, once per side, because the browser cannot depend on
//! `pimble-store`; the copies had started to disagree. Everything here is
//! pure over a [`Tree`] and builds for wasm.
//!
//! - What a merged update earns as notifications ([`NodeChange`],
//!   [`derive_changes`]).
//! - Which scope a document is in, and repairing a partial replica one scope
//!   at a time ([`Tree::roots_above`], [`Tree::repair_held`]).
//! - Which root the documents say a store has ([`Tree::document_root`],
//!   [`Tree::reroot`]).
//! - "Recently Deleted..." ([`Tree::deleted`]).
//! - One hop of following a link, and the walk over hops ([`Tree::link_hop`],
//!   [`LinkWalk`]).
//! - A node as a `Node` ([`Tree::node`], [`Tree::node_any`]).
//! - The checks a move makes before it touches anything, and the order
//!   several nodes move in ([`Tree::check_move`], [`move_order`]).
//!
//! What a grant's role allows of a document is `pimble_core::StoreAccess`'s
//! (`of_role`, `widest`, `on_document`): it needs no documents.

use std::collections::{HashMap, HashSet};

use pimble_core::{LinkResolution, Node, NodeId, Place, StoreId, MAX_LINK_HOPS};

use crate::node_doc::{NodeDoc, NodeFields, NodeUpdateEffect};
use crate::tree::{Tree, TreeEdit};
use crate::{CrdtError, Result};

// ── Deriving notifications from a merged update ─────────────────────────

/// What a merged update changed about one node, as a subscriber hears it.
/// The node-document half of `pimble_rpc::StoreChangeKind`, which maps from
/// it one to one (`From<NodeChange>` there); the kinds that are about a
/// store's link, mounts or shares are not a document's to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeChange {
    NodeCreated { node_id: NodeId, parent_id: NodeId },
    /// `node_id` and its whole subtree were removed from `parent_id`.
    NodeDeleted { node_id: NodeId, parent_id: NodeId },
    NodeMoved { node_id: NodeId, old_parent_id: NodeId, new_parent_id: NodeId },
    MetadataUpdated { node_id: NodeId },
    ContentUpdated { node_id: NodeId },
    /// A children list changed (or a root arrived): refetch the lists of
    /// `node_ids` and their parents.
    TreeStructure { node_ids: Vec<NodeId> },
    /// An attachment of `host_id` was created, deleted or put back
    /// (docs/DISCUSSIONS_CONTRACT.md section 1).
    AttachmentChanged { node_id: NodeId, host_id: NodeId, live: bool },
}

/// What a node document says about the node before or after a merge: the
/// `node` root as fields (`None` while the document is not initialised) and
/// the children list as stored.
#[derive(Debug, Clone)]
pub struct DocShape {
    pub fields: Option<NodeFields>,
    pub children: Vec<NodeId>,
}

impl Tree {
    /// The shape of `id`'s document, `None` when the tree holds none: read
    /// before and after a merge for [`derive_changes`].
    pub fn shape(&self, id: NodeId) -> Option<DocShape> {
        self.doc(id).map(|doc| DocShape { fields: doc.fields().ok(), children: doc.children() })
    }
}

/// The notifications a merged update earns, from what it changed in the
/// document (docs/NODE_DOCUMENT_CONTRACT.md section 2): the `node` root
/// newly written is `NodeCreated` (or `NodeDeleted` when it arrived as a
/// tombstone, or `TreeStructure` for a root, which has no parent to name);
/// a tombstone set is `NodeDeleted`, cleared is `NodeCreated` again (the
/// node is back under its parent); a changed `parent_id` is `NodeMoved`;
/// any other change to the `node` root is `MetadataUpdated`; a changed
/// children list is `TreeStructure { [node] }`; a changed `content` root,
/// or the plugin's `data` root, is `ContentUpdated`. One update can earn
/// several; a structural change nothing above names (a rewrite of the same
/// values) still earns `TreeStructure`, so that its bytes travel down a
/// chain of links. `root` stands in for a parent no document names.
///
/// The server broadcasts these; the browser's vault client, holding the
/// documents itself, tells its UI exactly what the server would.
pub fn derive_changes(node_id: NodeId, root: NodeId, before: Option<&DocShape>, after: Option<&DocShape>, effect: NodeUpdateEffect) -> Vec<NodeChange> {
    let mut kinds = Vec::new();
    if effect.structure {
        let before_fields = before.and_then(|s| s.fields.as_ref());
        let after_fields = after.and_then(|s| s.fields.as_ref());
        // An attachment (docs/DISCUSSIONS_CONTRACT.md section 1) is in no
        // list: its arrival, deletion and return are news for its host.
        if let Some(host_id) = after_fields.and_then(|a| a.attached_to) {
            let was_live = before_fields.is_some_and(|b| b.deleted_at.is_none());
            let live = after_fields.is_some_and(|a| a.deleted_at.is_none());
            if was_live != live {
                kinds.push(NodeChange::AttachmentChanged { node_id, host_id, live });
            } else if before_fields != after_fields {
                kinds.push(NodeChange::MetadataUpdated { node_id });
            }
            if effect.content || effect.data {
                kinds.push(NodeChange::ContentUpdated { node_id });
            }
            if kinds.is_empty() {
                kinds.push(NodeChange::MetadataUpdated { node_id });
            }
            return kinds;
        }
        let node_kind = match (before_fields, after_fields) {
            (None, Some(a)) => Some(match (a.deleted_at.is_some(), a.parent_id) {
                (true, parent) => NodeChange::NodeDeleted { node_id, parent_id: parent.unwrap_or(root) },
                (false, Some(parent_id)) => NodeChange::NodeCreated { node_id, parent_id },
                (false, None) => NodeChange::TreeStructure { node_ids: vec![node_id] },
            }),
            (Some(b), Some(a)) => {
                if b.deleted_at.is_none() && a.deleted_at.is_some() {
                    Some(NodeChange::NodeDeleted { node_id, parent_id: b.parent_id.or(a.parent_id).unwrap_or(root) })
                } else if b.deleted_at.is_some() && a.deleted_at.is_none() {
                    Some(NodeChange::NodeCreated { node_id, parent_id: a.parent_id.unwrap_or(root) })
                } else if b.parent_id != a.parent_id {
                    Some(NodeChange::NodeMoved {
                        node_id,
                        old_parent_id: b.parent_id.unwrap_or(root),
                        new_parent_id: a.parent_id.unwrap_or(root),
                    })
                } else if b != a {
                    Some(NodeChange::MetadataUpdated { node_id })
                } else {
                    None
                }
            }
            // An initialised `node` root cannot become uninitialised, and
            // a change that touched neither side of it is a list change.
            (Some(_), None) | (None, None) => None,
        };
        let named_the_list = matches!(node_kind, Some(NodeChange::TreeStructure { .. }));
        kinds.extend(node_kind);
        let before_children = before.map(|s| s.children.as_slice()).unwrap_or(&[]);
        let after_children = after.map(|s| s.children.as_slice()).unwrap_or(&[]);
        if (before_children != after_children || kinds.is_empty()) && !named_the_list {
            kinds.push(NodeChange::TreeStructure { node_ids: vec![node_id] });
        }
    }
    if effect.content || effect.data {
        kinds.push(NodeChange::ContentUpdated { node_id });
    }
    if kinds.is_empty() {
        kinds.push(NodeChange::TreeStructure { node_ids: vec![node_id] });
    }
    kinds
}

// ── Scopes, and repairing a partial replica ─────────────────────────────

impl Tree {
    /// The `roots` on `id`'s stored parent chain, itself included, nearest
    /// first, through held documents (tombstones too: a deleted document
    /// stays with its scope). An attachment is where its host is. The
    /// scopes a document is in: what a partial replica's write guard and
    /// the browser's access judgement read, and where "Put Back" puts a
    /// node no list names. Empty when `roots` is (a whole store) or none is
    /// reached. Bounded, so an unrepaired cycle cannot loop.
    pub fn roots_above(&self, roots: &[NodeId], id: NodeId) -> Vec<NodeId> {
        let mut reached = Vec::new();
        if roots.is_empty() {
            return reached;
        }
        let mut cur = id;
        for _ in 0..=self.ids().len() {
            if roots.contains(&cur) && !reached.contains(&cur) {
                reached.push(cur);
            }
            match self.doc(cur).and_then(|d| d.fields().ok()).and_then(|f| f.parent_id.or(f.attached_to)) {
                Some(parent) if self.doc(parent).is_some() => cur = parent,
                _ => break,
            }
        }
        reached
    }

    /// Which scope each held document belongs to: the last (top-most) of
    /// `scope_roots` on its stored parent chain through held documents
    /// (tombstones included: a deleted document stays with its scope), and
    /// nothing when it reaches none. Bounded, so an unrepaired cycle cannot
    /// loop. The grouping [`Tree::repair_held`] repairs one tree at a time.
    pub fn scope_groups(&self, scope_roots: &[NodeId]) -> HashMap<NodeId, Vec<NodeId>> {
        let roots: HashSet<NodeId> = scope_roots.iter().copied().collect();
        let ids = self.ids();
        let bound = ids.len();
        let mut groups: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for &id in &ids {
            let mut cur = id;
            let mut top = None;
            for _ in 0..=bound {
                if roots.contains(&cur) {
                    top = Some(cur);
                }
                match self.doc(cur).and_then(|d| d.fields().ok()).and_then(|f| f.parent_id) {
                    Some(parent) if self.doc(parent).is_some() => cur = parent,
                    _ => break,
                }
            }
            if let Some(top) = top {
                groups.entry(top).or_default().push(id);
            }
        }
        groups
    }

    /// Repair the documents held here (see [`Tree::repair`]): the whole tree
    /// when `scope_roots` is empty, and otherwise, on a partial replica, one
    /// scope at a time. `None` when nothing needed fixing.
    ///
    /// A partial replica's repair (docs/NODE_DOCUMENT_CONTRACT.md section 5)
    /// is an edit that travels to the owner and to every other member, so it
    /// must be one a device holding the whole store would make too; two
    /// devices that disagree about a fix undo each other's for ever. Three
    /// things keep it so, each scope being handed to `Tree::repair` as the
    /// honest tree it is (rooted at its scope root, holding exactly the
    /// documents that lead to it):
    ///
    /// - **A scope root is a root.** Its `parent_id` names a document this
    ///   replica never holds, which is no orphan here, and as the root of
    ///   its own tree it is never re-parented nor listed. A scope root held
    ///   under another (overlapping shares) is an ordinary node of that
    ///   one's tree.
    /// - **A document that leads to no scope root is left alone**: moved
    ///   out of the share by its owner, or not yet under it. A whole store
    ///   knows where it belongs; adopting it here would move it back.
    /// - **A scope whose lists name a document not held yet is not judged
    ///   yet.** The entry is a document on its way (created by someone
    ///   else a moment ago, or not in the published scope yet), and
    ///   removing it would delete a child from the owner's folder. The
    ///   scope is repaired once the document is here, or by any device
    ///   that holds the whole store.
    ///
    /// `Tree::repair` itself never reads "not held" as "missing" (since
    /// 2026-09-21: that was false of a whole store too, whenever a document
    /// was still on its way or its key was), so what the scope pass adds is
    /// the rooting, not the caution.
    pub fn repair_held(&mut self, scope_roots: &[NodeId], now: &str) -> Result<Option<TreeEdit>> {
        if scope_roots.is_empty() {
            return self.repair(now);
        }
        let groups = self.scope_groups(scope_roots);

        let mut edit = TreeEdit::default();
        let mut tops: Vec<NodeId> = groups.keys().copied().collect();
        tops.sort_by_key(|id| id.to_string());
        for top in tops {
            let group = &groups[&top];
            // Only a node's list is repaired (a tombstone keeps its own for
            // an undelete), so only a node's list can ask for a removal.
            let awaited = group.iter().filter(|id| self.has_node(**id)).filter_map(|id| self.doc(*id)).any(|doc| {
                doc.children().into_iter().any(|child| !self.doc(child).is_some_and(|held| held.fields().is_ok()))
            });
            if awaited {
                tracing::debug!("Scope {} lists a document not held yet; not repairing it now", top);
                continue;
            }

            let mut docs = HashMap::with_capacity(group.len());
            for id in group {
                if let Some(doc) = self.take_doc(*id) {
                    docs.insert(*id, doc);
                }
            }
            let mut scope_tree = Tree::from_docs(top, docs);
            let repaired = scope_tree.repair(now);
            for id in group {
                if let Some(doc) = scope_tree.take_doc(*id) {
                    self.insert_doc(*id, doc);
                }
            }
            if let Some(scope_edit) = repaired? {
                edit.touched.extend(scope_edit.touched);
            }
        }
        Ok((!edit.is_empty()).then_some(edit))
    }
}

// ── The root the documents say a store has ──────────────────────────────

impl Tree {
    /// The root the documents say the store has: `current` (the manifest's
    /// root, or what the tree starts from now) when it is a node with no
    /// `parent_id`; otherwise the one node with no `parent_id`, when there
    /// is exactly one; otherwise `None` (no root has arrived yet, or several
    /// nodes claim to be it, which only a repair from a known root settles).
    /// A replica created empty (a vault twin, a page that opened a store
    /// before its documents arrived) starts from a placeholder root, and
    /// this is what moves it under the real one as that document lands.
    ///
    /// Never asked of a partial replica: its root is its first scope root,
    /// a node with a parent the replica never holds, and the first node
    /// whose parent is simply not here is not a root.
    pub fn document_root(&self, current: NodeId) -> Option<NodeId> {
        let is_root_like = |id: NodeId| self.get_node_info(id).is_ok_and(|info| info.parent_id.is_none());
        if is_root_like(current) {
            return Some(current);
        }
        let mut candidates = self.list_node_ids().into_iter().filter(|&id| is_root_like(id));
        match (candidates.next(), candidates.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

    /// Start the tree from another root, over the same documents. `Tree`
    /// fixes its root at construction, so re-rooting is rebuilding it.
    /// Nothing is repaired here; the caller's next repair settles what the
    /// new root changes.
    pub fn reroot(&mut self, root: NodeId) {
        let ids = self.ids();
        let mut docs: HashMap<NodeId, NodeDoc> = HashMap::with_capacity(ids.len());
        for id in ids {
            if let Some(doc) = self.take_doc(id) {
                docs.insert(id, doc);
            }
        }
        *self = Tree::from_docs(root, docs);
    }
}

// ── A node as a `Node` ──────────────────────────────────────────────────

impl Tree {
    /// A live node as the `Node` an answer carries: its fields
    /// (`NodeFields::into_node`, the one mapping of stored fields to a
    /// node), its children, and the whole node document as the content (the
    /// editor joins it as it joined a content document; Pimble's roots ride
    /// along). `access` stays `Full`: a judgement whoever answers makes per
    /// caller, never a property of the stored node. `attachments` stays 0:
    /// counting them walks every document, so a caller answering a list
    /// counts them once for the list (`Tree::attachment_counts`). An error
    /// for an id with no document, a tombstone, or a document whose `node`
    /// root has not arrived yet.
    pub fn node(&self, id: NodeId) -> Result<Node> {
        let fields = self.live_fields(id)?;
        let children = self.get_children(id)?;
        let content = self.doc(id).map(NodeDoc::save).unwrap_or_default();
        let mut node = fields.into_node(id);
        node.content = content;
        node.children = children;
        Ok(node)
    }

    /// [`Tree::node`] without requiring the node to be live: for "Recently
    /// Deleted..." (docs/MOVE_CONTRACT.md "Seeing and undoing what was
    /// removed"), which needs a tombstone's fields (and a live node's, for
    /// the ones no held list names) even though `Tree::get_node_info`/
    /// `Tree::get_children` refuse anything not live. `None` only for an id
    /// with no document at all, or whose `node` root has not arrived.
    /// Content and children are left empty on purpose: the list shows a
    /// name and a place to put it back, not the note itself. Something
    /// that wants the note back reads it with [`Tree::node`] after an
    /// undelete.
    pub fn node_any(&self, id: NodeId) -> Option<Node> {
        Some(self.doc(id)?.fields().ok()?.into_node(id))
    }
}

// ── "Recently Deleted..." ───────────────────────────────────────────────

/// One entry of "Recently Deleted...", before a caller makes it a
/// `pimble_core::DeletedNode` with the node and the access it judges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedEntry {
    pub id: NodeId,
    /// The title of the folder it was in (an attachment's host), when that
    /// is held here; only for a tombstone.
    pub parent_title: Option<String>,
    /// When it was deleted; `None` for a live node no list names.
    pub deleted_at: Option<String>,
    /// For a live node no list names: where "Put Back" moves it (the
    /// nearest scope root that reaches it, or the store's root). `None` for
    /// a tombstone, which an undelete puts back where it was.
    pub put_back_under: Option<NodeId>,
}

impl Tree {
    /// What "Recently Deleted..." shows (docs/MOVE_CONTRACT.md "Seeing and
    /// undoing what was removed"): the top-most tombstones (a tombstoned
    /// node whose parent is not itself a tombstone, or is not held), most
    /// recently deleted first; then the live nodes no held list names, in
    /// id order, excluding `root`, the `scope_roots` of a partial replica
    /// (named by no list by design) and live attachments (in no list by
    /// design, docs/DISCUSSIONS_CONTRACT.md section 1).
    ///
    /// Every held document is read, tombstoned or not: exactly the entries
    /// named here are the ones ordinary reachability leaves out (a
    /// tombstone keeps its list for an undelete; a node no list names is
    /// unreachable by definition). Which lists name a node is each owner's
    /// raw stored list, a tombstone's included: it is what an undelete
    /// restores, so a node it names is not lost.
    ///
    /// `shown` leaves out what the caller may not see (a scoped member reads
    /// what its scopes reach); `ended` leaves out a live node under a share
    /// that has ended (the server keeps those documents on disk until the
    /// replica is removed; the browser has already dropped them).
    pub fn deleted(&self, root: NodeId, scope_roots: &[NodeId], shown: impl Fn(NodeId) -> bool, ended: impl Fn(NodeId) -> bool) -> Vec<DeletedEntry> {
        let mut live: Vec<NodeId> = Vec::new();
        let mut tombstones: Vec<(NodeId, NodeFields)> = Vec::new();
        for id in self.ids() {
            let Some(fields) = self.doc(id).and_then(|doc| doc.fields().ok()) else { continue };
            if fields.deleted_at.is_some() {
                tombstones.push((id, fields));
            } else if fields.attached_to.is_none() {
                live.push(id);
            }
        }

        let mut listed: HashSet<NodeId> = HashSet::new();
        for owner in live.iter().copied().chain(tombstones.iter().map(|(id, _)| *id)) {
            if let Some(doc) = self.doc(owner) {
                listed.extend(doc.children());
            }
        }

        let tombstone_ids: HashSet<NodeId> = tombstones.iter().map(|(id, _)| *id).collect();
        let mut top: Vec<(NodeId, NodeFields)> = tombstones
            .into_iter()
            .filter(|(id, _)| shown(*id))
            .filter(|(_, fields)| fields.parent_id.is_none_or(|parent| !tombstone_ids.contains(&parent)))
            .collect();
        // Most recent first.
        top.sort_by(|(_, a), (_, b)| b.deleted_at.cmp(&a.deleted_at));

        let title_of = |id: NodeId| self.doc(id).and_then(|d| d.fields().ok()).map(|f| f.title);
        let mut out: Vec<DeletedEntry> = top
            .into_iter()
            .map(|(id, fields)| DeletedEntry {
                id,
                // An attachment's place is its host.
                parent_title: fields.parent_id.or(fields.attached_to).and_then(title_of),
                deleted_at: fields.deleted_at,
                put_back_under: None,
            })
            .collect();

        let mut unlisted: Vec<NodeId> = live
            .into_iter()
            .filter(|id| *id != root && !scope_roots.contains(id) && !listed.contains(id))
            .filter(|id| !ended(*id) && shown(*id))
            .collect();
        unlisted.sort_by_key(|id| id.to_string());
        out.extend(unlisted.into_iter().map(|id| DeletedEntry {
            id,
            parent_title: None,
            deleted_at: None,
            put_back_under: Some(self.roots_above(scope_roots, id).first().copied().unwrap_or(root)),
        }));
        out
    }
}

// ── Following a link ────────────────────────────────────────────────────

/// What one hop of following a link found (docs/LINKS_CONTRACT.md
/// "Following a link"): an answer, or a tombstone's `became` to follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkHop {
    Done(LinkResolution),
    Became((StoreId, NodeId)),
}

impl Tree {
    /// One hop of following a link to `node_id` in `store_id`, this tree's
    /// store, over the documents held here: live, deleted, or a tombstone's
    /// `became` to follow. A document a partial replica does not hold is
    /// outside every share this device holds: no access, not missing. A
    /// document whose `node` root has not arrived is not a node yet. What
    /// the caller may reach is its own to judge first.
    pub fn link_hop(&self, store_id: StoreId, node_id: NodeId, partial: bool) -> LinkHop {
        match self.doc(node_id).map(|doc| doc.fields()) {
            None if !partial => LinkHop::Done(LinkResolution::Missing),
            None => LinkHop::Done(LinkResolution::NoAccess),
            Some(Err(_)) => LinkHop::Done(LinkResolution::Missing),
            Some(Ok(fields)) => match (fields.deleted_at, fields.became) {
                (Some(_), Some(became)) => LinkHop::Became((became.store, became.node)),
                (Some(_), None) => LinkHop::Done(LinkResolution::Deleted { store_id, node_id }),
                (None, _) => LinkHop::Done(LinkResolution::Live { store_id, node_id }),
            },
        }
    }
}

/// The walk over a link's hops, through tombstones' `became`: at most
/// `MAX_LINK_HOPS` past the first, and a chain that loops or runs past the
/// limit leads nowhere (`Missing`). The hops themselves are the caller's (a
/// server opens stores between them, so a hop may be async):
///
/// ```ignore
/// let mut walk = LinkWalk::new(store_id, node_id);
/// while let Some((store_id, node_id)) = walk.next() {
///     if let Some(answer) = walk.took(hop(store_id, node_id)) {
///         return answer;
///     }
/// }
/// walk.nowhere()
/// ```
pub struct LinkWalk {
    at: (StoreId, NodeId),
    seen: HashSet<(StoreId, NodeId)>,
    hops: usize,
}

impl LinkWalk {
    pub fn new(store_id: StoreId, node_id: NodeId) -> Self {
        Self { at: (store_id, node_id), seen: HashSet::new(), hops: 0 }
    }

    /// Where to look next; `None` once the chain has looped or run past the
    /// limit.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<(StoreId, NodeId)> {
        if self.hops > MAX_LINK_HOPS || !self.seen.insert(self.at) {
            return None;
        }
        self.hops += 1;
        Some(self.at)
    }

    /// Take what the hop at [`LinkWalk::next`]'s place found: the answer, or
    /// `None` and the walk moves on to where the tombstone became.
    pub fn took(&mut self, hop: LinkHop) -> Option<LinkResolution> {
        match hop {
            LinkHop::Done(resolution) => Some(resolution),
            LinkHop::Became(next) => {
                self.at = next;
                None
            }
        }
    }

    /// The answer of a walk that ran out: it leads nowhere.
    pub fn nowhere(&self) -> LinkResolution {
        LinkResolution::Missing
    }
}

// ── Moves ───────────────────────────────────────────────────────────────

impl Tree {
    /// The checks a move of `node_id` under `new_parent_id` makes before it
    /// touches anything, as sentences: not the root, not into itself, both
    /// live, not under one of its own descendants
    /// (`Tree::move_or_transplant` refuses that last one too, but as a
    /// failure; here it is the refusal it is).
    pub fn check_move(&self, node_id: NodeId, new_parent_id: NodeId) -> std::result::Result<(), MoveRefused> {
        if node_id == self.root() {
            return Err(MoveRefused::Root);
        }
        if node_id == new_parent_id {
            return Err(MoveRefused::IntoItself);
        }
        if !self.has_node(node_id) {
            return Err(MoveRefused::NotFound(node_id));
        }
        if !self.has_node(new_parent_id) {
            return Err(MoveRefused::NotFound(new_parent_id));
        }
        if self.subtree_ids(node_id).is_ok_and(|ids| ids.contains(&new_parent_id)) {
            return Err(MoveRefused::IntoDescendant);
        }
        Ok(())
    }
}

/// Why a move was refused before anything was touched ([`Tree::check_move`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveRefused {
    Root,
    IntoItself,
    /// Not a live node here.
    NotFound(NodeId),
    IntoDescendant,
}

impl std::fmt::Display for MoveRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveRefused::Root => f.write_str("Cannot move the root node"),
            MoveRefused::IntoItself => f.write_str("Cannot move node into itself"),
            MoveRefused::NotFound(id) => write!(f, "Node not found: {}", id),
            MoveRefused::IntoDescendant => f.write_str("Cannot move a node into one of its own descendants"),
        }
    }
}

impl std::error::Error for MoveRefused {}

impl From<MoveRefused> for CrdtError {
    fn from(refused: MoveRefused) -> Self {
        CrdtError::Refused(refused.to_string())
    }
}

/// Several nodes moved to one place (`moveNodes`, docs/TREE_CONTRACT.md):
/// the ids once each, in the order given (what the answer lists), and the
/// order to move them in. Every node goes to the same place, the last first,
/// so they keep their order without waiting on the ids transplants give
/// them; at the end they simply go in order.
pub fn move_order(node_ids: &[NodeId], place: Place) -> (Vec<NodeId>, Vec<NodeId>) {
    let mut once: Vec<NodeId> = Vec::with_capacity(node_ids.len());
    for id in node_ids {
        if !once.contains(id) {
            once.push(*id);
        }
    }
    let order = match place {
        Place::Last => once.clone(),
        _ => once.iter().rev().copied().collect(),
    };
    (once, order)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "2026-11-02T00:00:00Z";

    fn tree() -> (Tree, NodeId) {
        let root = NodeId::new();
        (Tree::new(root, "Store", T).unwrap(), root)
    }

    fn add(tree: &mut Tree, parent: NodeId, title: &str) -> NodeId {
        let id = NodeId::new();
        tree.add_node(id, Some(parent), Place::Last, pimble_core::node_types::FOLDER, title, T).unwrap();
        id
    }

    const T1: &str = "2026-11-02T00:00:01Z";

    fn replica_of(tree: &Tree) -> Tree {
        let docs = tree.ids().into_iter().map(|id| (id, NodeDoc::load(&tree.doc(id).unwrap().save()).unwrap())).collect();
        Tree::from_docs(tree.root(), docs)
    }

    fn shapes(before: &Tree, after: &Tree, id: NodeId) -> (Option<DocShape>, Option<DocShape>) {
        (before.shape(id), after.shape(id))
    }

    fn structural() -> NodeUpdateEffect {
        NodeUpdateEffect { changed: true, structure: true, content: false, data: false }
    }

    #[test]
    fn derive_changes_names_what_a_merge_changed() {
        let (mut tree, root) = tree();
        let (p, q) = (add(&mut tree, root, "P"), add(&mut tree, root, "Q"));
        let x = NodeId::new();

        // Created: the node is new, the parent's list changed.
        let before = replica_of(&tree);
        tree.add_node(x, Some(p), Place::Last, "document", "X", T).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert_eq!(derive_changes(x, root, b.as_ref(), a.as_ref(), structural()), vec![NodeChange::NodeCreated { node_id: x, parent_id: p }]);
        let (b, a) = shapes(&before, &tree, p);
        assert_eq!(derive_changes(p, root, b.as_ref(), a.as_ref(), structural()), vec![NodeChange::TreeStructure { node_ids: vec![p] }]);

        // Moved.
        let before = replica_of(&tree);
        tree.move_node(x, q, Place::Last, T1).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert_eq!(
            derive_changes(x, root, b.as_ref(), a.as_ref(), structural()),
            vec![NodeChange::NodeMoved { node_id: x, old_parent_id: p, new_parent_id: q }]
        );

        // Renamed.
        let before = replica_of(&tree);
        tree.set_title(x, "Y", T1).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert_eq!(derive_changes(x, root, b.as_ref(), a.as_ref(), structural()), vec![NodeChange::MetadataUpdated { node_id: x }]);

        // Content, on top of nothing structural.
        let content = NodeUpdateEffect { changed: true, structure: false, content: true, data: false };
        let (b, a) = shapes(&tree, &tree, x);
        assert_eq!(derive_changes(x, root, b.as_ref(), a.as_ref(), content), vec![NodeChange::ContentUpdated { node_id: x }]);

        // Deleted, and back.
        let before = replica_of(&tree);
        tree.remove_node(x, T1).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert_eq!(derive_changes(x, root, b.as_ref(), a.as_ref(), structural()), vec![NodeChange::NodeDeleted { node_id: x, parent_id: q }]);
        let before = replica_of(&tree);
        tree.undelete_node(x, T1).unwrap();
        let (b, a) = shapes(&before, &tree, x);
        assert_eq!(derive_changes(x, root, b.as_ref(), a.as_ref(), structural()), vec![NodeChange::NodeCreated { node_id: x, parent_id: q }]);

        // A tombstone arriving whole on a replica that never had the node.
        let mut fresh = Tree::from_docs(root, HashMap::new());
        tree.remove_node(x, T1).unwrap();
        fresh.apply_update(x, &tree.doc(x).unwrap().save()).unwrap();
        let a = fresh.shape(x);
        assert_eq!(derive_changes(x, root, None, a.as_ref(), structural()), vec![NodeChange::NodeDeleted { node_id: x, parent_id: q }]);

        // The root arriving on an empty replica: no parent to name.
        fresh.apply_update(root, &tree.doc(root).unwrap().save()).unwrap();
        let a = fresh.shape(root);
        assert_eq!(derive_changes(root, root, None, a.as_ref(), structural()), vec![NodeChange::TreeStructure { node_ids: vec![root] }]);

        // A structural change that reads the same still earns bytes to forward.
        let (b, a) = shapes(&tree, &tree, root);
        assert!(matches!(derive_changes(root, root, b.as_ref(), a.as_ref(), structural())[..], [NodeChange::TreeStructure { .. }]));
    }

    #[test]
    fn a_move_is_checked_before_anything_is_touched() {
        let (mut tree, root) = tree();
        let a = add(&mut tree, root, "a");
        let b = add(&mut tree, a, "b");
        assert_eq!(tree.check_move(root, a), Err(MoveRefused::Root));
        assert_eq!(tree.check_move(a, a), Err(MoveRefused::IntoItself));
        assert_eq!(tree.check_move(a, b), Err(MoveRefused::IntoDescendant));
        let gone = NodeId::new();
        assert_eq!(tree.check_move(gone, a), Err(MoveRefused::NotFound(gone)));
        assert_eq!(tree.check_move(b, root), Ok(()));
    }

    #[test]
    fn several_nodes_move_once_each_the_last_first_unless_at_the_end() {
        let (a, b, c) = (NodeId::new(), NodeId::new(), NodeId::new());
        assert_eq!(move_order(&[a, b, a, c], Place::Last), (vec![a, b, c], vec![a, b, c]));
        assert_eq!(move_order(&[a, b, a, c], Place::First), (vec![a, b, c], vec![c, b, a]));
    }

    #[test]
    fn a_link_walk_follows_became_and_gives_up_on_a_loop() {
        let (s, x, y) = (StoreId::new(), NodeId::new(), NodeId::new());
        let mut walk = LinkWalk::new(s, x);
        assert_eq!(walk.next(), Some((s, x)));
        assert_eq!(walk.took(LinkHop::Became((s, y))), None);
        assert_eq!(walk.next(), Some((s, y)));
        assert_eq!(walk.took(LinkHop::Became((s, x))), None);
        assert_eq!(walk.next(), None, "back where it started");
        assert_eq!(walk.nowhere(), LinkResolution::Missing);
    }

    #[test]
    fn the_documents_root_is_the_one_node_with_no_parent() {
        let (tree, root) = tree();
        assert_eq!(tree.document_root(root), Some(root));
        assert_eq!(tree.document_root(NodeId::new()), Some(root), "a placeholder gives way to the documents' root");
        let empty = Tree::from_docs(NodeId::new(), HashMap::new());
        assert_eq!(empty.document_root(empty.root()), None);
    }

    #[test]
    fn deleted_lists_top_tombstones_then_nodes_no_list_names() {
        let (mut tree, root) = tree();
        let a = add(&mut tree, root, "a");
        let b = add(&mut tree, a, "b");
        tree.remove_node(a, T).unwrap();
        let entries = tree.deleted(root, &[], |_| true, |_| false);
        assert_eq!(entries.len(), 1, "b went with a: only the top-most tombstone is listed");
        assert_eq!(entries[0].id, a);
        assert_eq!(entries[0].parent_title.as_deref(), Some("Store"));
        assert!(entries[0].deleted_at.is_some());
        let _ = b;
    }
}
