//! Judgements about a store's tree, made before anything is sent
//! (docs/API_CONTRACT.md decision 5, wave 3): where a move lands and whether
//! it may happen, where a new node goes, whether nodes may be deleted, what a
//! row's menu offers, what a node is called. The explorer, a kanban view, the
//! MCP and plugins all ask these, so they refuse the same things with the
//! same sentences. The server still judges every request and is
//! authoritative; these say what it would answer, early.
//!
//! Every function here is pure over a [`TreeView`], a small read-only view of
//! the tree that a client implements over whatever it holds: the app over its
//! signals, `pimble-api` over its held documents. It lives in `pimble-core`
//! rather than `pimble-api` so that the app can use it before it moves onto
//! the API (wave 5), and so that the rules sit beside the types they judge.

mod capabilities;
mod found;
mod moves;
#[cfg(test)]
mod tests;

pub use capabilities::*;
pub use found::*;
pub use moves::*;

use std::collections::HashSet;

use crate::{Node, NodeId, RelaySide, StoreAccess, StoreId, StoreKind};

/// What the judgements need to know of one store this client holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreFacts {
    pub name: String,
    /// The store's root node. A partial replica names the store's real
    /// root, which this device does not hold; see `partial`.
    pub root: NodeId,
    /// What this device may change in the store as a whole.
    pub access: StoreAccess,
    /// Linked to another Pimble server, to its hosted twin, or to its twin on
    /// this computer.
    pub linked: bool,
    /// A copy of a store that lives somewhere else (`Store::is_replica`).
    pub replica: bool,
    /// Encrypted storage (`Store::kind` is `Vault`).
    pub vault: bool,
    /// What the store's link is (`Store::sync_mode`).
    pub sync_mode: StoreKind,
    pub relay: RelaySide,
    /// The owner's email when the store reached this device as someone
    /// else's share.
    pub shared_by: Option<String>,
    /// This device holds some shared subtrees of the store rather than all
    /// of it (`Store::roots` is not empty).
    pub partial: bool,
    /// A share's replica whose every share has ended (`Store::every_share_ended`).
    pub every_share_ended: bool,
}

impl StoreFacts {
    /// The facts of `store`, with whether it is linked, which a `Store` does
    /// not carry.
    pub fn of(store: &crate::Store, linked: bool) -> Self {
        Self {
            name: store.name.clone(),
            root: store.root_node_id,
            access: store.access,
            linked,
            replica: store.is_replica,
            vault: store.kind == StoreKind::Vault,
            sync_mode: store.sync_mode,
            relay: store.relay,
            shared_by: store.shared_by.clone(),
            partial: !store.roots.is_empty(),
            every_share_ended: store.every_share_ended(),
        }
    }
}

/// A read-only view of the stores' trees, as far as a client holds them.
/// Nodes are addressed by their canonical `(StoreId, NodeId)`: a node reached
/// through a mount is asked about in the store it lives in.
///
/// Anything a client does not hold answers "not known" (`None`, `false`, an
/// empty walk), and every judgement here reads "not known" the way the app
/// always has: nothing is refused on a guess, because the server judges the
/// request anyway.
pub trait TreeView {
    /// The store, when it is open here.
    fn store(&self, store: StoreId) -> Option<StoreFacts>;

    /// What this device may change in the store as a whole, when it is open
    /// here. A cheaper question than [`TreeView::store`], asked per node.
    fn store_access(&self, store: StoreId) -> Option<StoreAccess> {
        self.store(store).map(|facts| facts.access)
    }

    /// The node's parent; `None` for a root or a node not held.
    fn parent(&self, store: StoreId, node: NodeId) -> Option<NodeId>;

    /// The node's children in order, when the list is held.
    fn children(&self, store: StoreId, node: NodeId) -> Option<Vec<NodeId>>;

    /// Whether the node is a mount.
    fn is_mount(&self, store: StoreId, node: NodeId) -> bool;

    /// The node a mount shows, when it is a mount whose target is known.
    fn mount_source(&self, store: StoreId, node: NodeId) -> Option<(StoreId, NodeId)>;

    /// Whether the node carries a share marker.
    fn is_shared(&self, store: StoreId, node: NodeId) -> bool;

    /// The access the held copy of the node carries (`Node::access`, the
    /// server's judgement of it), when the node is held.
    fn held_access(&self, store: StoreId, node: NodeId) -> Option<StoreAccess>;

    /// What the node is called ([`display_label`]); [`UNTITLED`] for a node
    /// not held.
    fn label(&self, store: StoreId, node: NodeId) -> String;
}

/// What a node is called when it is called nothing.
pub const UNTITLED: &str = "Untitled";

/// How far a walk up the tree goes before it gives up: a tree deeper than
/// this is not one a person made, and a walk must end on a tree that a
/// concurrent move has left with a cycle until repair runs.
pub const MAX_DEPTH: usize = 4096;

// ── Walks ────────────────────────────────────────────────────────────────

/// `node` and its ancestors, nearest first, as far as the view holds them.
/// The one walk up the tree: bounded by [`MAX_DEPTH`] and stopped by a cycle.
pub fn ancestors<V: TreeView + ?Sized>(view: &V, store: StoreId, node: NodeId) -> Vec<NodeId> {
    let mut path = vec![node];
    let mut seen = HashSet::from([node]);
    let mut at = node;
    while path.len() < MAX_DEPTH {
        let Some(parent) = view.parent(store, at) else { break };
        if !seen.insert(parent) {
            break;
        }
        path.push(parent);
        at = parent;
    }
    path
}

/// The nearest of `node` and its ancestors for which `is` holds.
pub fn nearest<V: TreeView + ?Sized>(view: &V, store: StoreId, node: NodeId, is: impl Fn(NodeId) -> bool) -> Option<NodeId> {
    ancestors(view, store, node).into_iter().find(|&at| is(at))
}

/// Whether `node` is `root` or under it, as far as the view holds the chain
/// of parents. A chain that breaks before it reaches `root` (an ancestor not
/// held) answers no.
pub fn is_at_or_under<V: TreeView + ?Sized>(view: &V, store: StoreId, node: NodeId, root: NodeId) -> bool {
    nearest(view, store, node, |at| at == root).is_some()
}

// ── Access ───────────────────────────────────────────────────────────────

/// What this device may change of one node, from what its store says and
/// what the held copy of the node carries.
///
/// A store held to read answers for everything in it. Otherwise the node's
/// own judgement says (a reader's root beside an editor's in one store, a
/// commenter's discussion), and a node not held reads as its store does.
///
/// **Unknown access is `Full`**: a store not open here, or open but not yet
/// told what this device may do, reads as `Full`, and so does a node not held
/// in such a store. That is policy, not an accident: nothing is disabled or
/// refused on a guess, and the server judges every write anyway, answering
/// with the sentence this would have given.
pub fn node_access(store: Option<StoreAccess>, node: Option<StoreAccess>) -> StoreAccess {
    let store = store.unwrap_or(StoreAccess::Full);
    if store == StoreAccess::Read {
        return store;
    }
    node.unwrap_or(store)
}

/// [`node_access`] of a node, read from the view.
pub fn access<V: TreeView + ?Sized>(view: &V, store: StoreId, node: NodeId) -> StoreAccess {
    node_access(view.store_access(store), view.held_access(store, node))
}

/// What this device may change in a store as a whole (a store row's items):
/// `Full` when the store is not known, by the same policy as [`node_access`].
pub fn store_access<V: TreeView + ?Sized>(view: &V, store: StoreId) -> StoreAccess {
    view.store_access(store).unwrap_or(StoreAccess::Full)
}

// ── Labels ───────────────────────────────────────────────────────────────

/// How many characters of a node's first line its label takes before an
/// ellipsis.
pub const LABEL_CHARS: usize = 25;

/// A label from an explicit-title flag and title plus the node's text: an
/// explicit title wins; otherwise the first non-empty line of `content_text`
/// (cut at [`LABEL_CHARS`] with an ellipsis); otherwise `title`; otherwise
/// [`UNTITLED`].
pub fn label_from_title_and_content(has_explicit_title: bool, title: &str, content_text: &str) -> String {
    if has_explicit_title && !title.is_empty() {
        return title.to_string();
    }
    let first_line = content_text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    if !first_line.is_empty() {
        return if first_line.chars().count() > LABEL_CHARS {
            format!("{}…", first_line.chars().take(LABEL_CHARS).collect::<String>())
        } else {
            first_line.to_string()
        };
    }
    if !title.is_empty() {
        return title.to_string();
    }
    UNTITLED.to_string()
}

/// What a node is called wherever it is named: the explorer's row, a pane's
/// title, a drop's sentence. `text_of` turns the node's content into plain
/// text (`pimble_crdt::NodeDoc::text_of`, which this crate cannot call); it
/// is called only when the label needs the text, since that decode walks the
/// whole document (an explicitly titled node's label is its title).
pub fn display_label(node: &Node, text_of: impl FnOnce(&[u8]) -> String) -> String {
    let has_explicit_title =
        node.metadata.custom.get(crate::custom_keys::EXPLICIT_TITLE).and_then(|v| v.as_bool()).unwrap_or(false);
    if has_explicit_title && !node.metadata.title.is_empty() {
        return node.metadata.title.clone();
    }
    // A discussion's title is its topic; it has no text to take one from
    // (docs/DISCUSSIONS_CONTRACT.md section 2).
    if node.node_type == crate::node_types::DISCUSSION {
        return if node.metadata.title.trim().is_empty() { "Discussion".to_string() } else { node.metadata.title.clone() };
    }
    label_from_title_and_content(has_explicit_title, &node.metadata.title, &text_of(&node.content))
}
