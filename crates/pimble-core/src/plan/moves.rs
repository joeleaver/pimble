//! Moves, new nodes and deletes (docs/TREE_CONTRACT.md "The drop model",
//! docs/MOVE_CONTRACT.md): where they land, whether they may happen, what the
//! person is told before they do, and which requests make them.

use crate::{NodeId, Place, StoreAccess, StoreId};

use super::{access, ancestors, TreeView};

/// Refusals the tree's own shape cannot know.
pub const BESIDE_A_STORE: &str = "Nodes go inside a store, not beside it.";
pub const INTO_ITSELF: &str = "A node can't go inside itself.";
pub const MOUNT_NOT_HERE: &str = "This mount's store isn't open here.";
pub const NOT_A_NODE: &str = "Only nodes can be moved.";

/// What a move that changes nothing says.
pub const WHERE_IT_IS: &str = "Where it is";

/// Why something was refused before it was sent. Each has one sentence,
/// the one the person reads wherever it is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A place beside a store row: there is no parent there.
    BesideAStore,
    /// A store row (or nothing) where a node was needed.
    NotANode,
    /// The new parent is a moved node or under one.
    IntoItself,
    /// A mount whose store is not open here (or whose target is not known
    /// yet), so there is nowhere to put anything.
    MountNotHere,
    /// Something it would change may only be read here.
    ReadOnly,
}

impl Refusal {
    pub fn sentence(self) -> &'static str {
        match self {
            Refusal::BesideAStore => BESIDE_A_STORE,
            Refusal::NotANode => NOT_A_NODE,
            Refusal::IntoItself => INTO_ITSELF,
            Refusal::MountNotHere => MOUNT_NOT_HERE,
            Refusal::ReadOnly => StoreAccess::READ_ONLY_REFUSAL,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.sentence())
    }
}

impl std::error::Error for Refusal {}

/// One request a move is made with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveStep {
    /// One node within its store (`moveNode`).
    Move { store: StoreId, node: NodeId, parent: NodeId, place: Place },
    /// Several nodes of the target's store, judged by the server all or
    /// none (`moveNodes`).
    MoveMany { store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place },
    /// A node from another store: a new node where it lands, the old one
    /// deleted (`transplantNode`, docs/MOVE_CONTRACT.md).
    Transplant { from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place },
}

/// What a move does, said and planned before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovePlan {
    /// The new parent as a node: a store row's root, a mount's source.
    pub parent: (StoreId, NodeId),
    pub place: Place,
    /// The requests, in the order they are sent. Empty when the nodes are
    /// already there.
    pub steps: Vec<MoveStep>,
    /// Where it lands ("Into MEDICAL, first"), or [`WHERE_IT_IS`].
    pub line: String,
    /// What else it does, when it does: a new node in another store, or
    /// leaving a share.
    pub warning: Option<String>,
}

impl MovePlan {
    /// The nodes are already where the move puts them: nothing to send.
    pub fn stays(&self) -> bool {
        self.steps.is_empty()
    }
}

/// The node a row stands for as a parent: a store row its root, a mount the
/// node it mounts (whose children are the ones drawn under it), anything
/// else itself. Refused for a store row with no root and a mount whose store
/// is not open here.
pub fn parent_of_row<V: TreeView + ?Sized>(view: &V, store: StoreId, node: Option<NodeId>) -> Result<(StoreId, NodeId), Refusal> {
    match node {
        None => view.store(store).map(|facts| (store, facts.root)).ok_or(Refusal::NotANode),
        Some(node) if view.is_mount(store, node) => view
            .mount_source(store, node)
            .filter(|(source, _)| view.store(*source).is_some())
            .ok_or(Refusal::MountNotHere),
        Some(node) => Ok((store, node)),
    }
}

/// Where a new node made on a row goes ([`parent_of_row`]), when this device
/// may write there. Every creation, whatever it makes (a node, a sheet, a
/// canvas, a discussion, a mount, a picture), asks this: a mount takes no
/// children, so whatever is made on a mount's row goes into its source
/// (docs/API_CONTRACT.md open decision 5).
pub fn create_parent<V: TreeView + ?Sized>(view: &V, store: StoreId, node: Option<NodeId>) -> Result<(StoreId, NodeId), Refusal> {
    let (store, parent) = parent_of_row(view, store, node)?;
    if !access(view, store, parent).allows_write() {
        return Err(Refusal::ReadOnly);
    }
    Ok((store, parent))
}

/// Whether `nodes` may be deleted: all of them or none (docs/TREE_CONTRACT.md
/// wave 3). Refused when there are none, or when any may only be read here.
pub fn can_delete<V: TreeView + ?Sized>(view: &V, nodes: &[(StoreId, NodeId)]) -> Result<(), Refusal> {
    if nodes.is_empty() {
        return Err(Refusal::NotANode);
    }
    if nodes.iter().any(|&(store, node)| !access(view, store, node).allows_write()) {
        return Err(Refusal::ReadOnly);
    }
    Ok(())
}

/// What the strip says when a move takes nodes into another store.
pub fn transplant_warning(count: usize, to_store: &str, from_store: &str) -> String {
    let (what, they) = if count == 1 { ("A new node", "it is") } else { ("New nodes", "they are") };
    format!("{what} in {to_store}; {they} deleted from {from_store} (Put Back in Recently Deleted).")
}

/// What the strip says when a move takes nodes out of a share
/// (docs/MOVE_CONTRACT.md: its members see them deleted).
pub fn leaves_share_warning(count: usize, share: &str) -> String {
    let it = if count == 1 { "it" } else { "them" };
    format!("Leaves the share {share}: its members will see {it} deleted and can put {it} back.")
}

/// Judge a move of `nodes` (top to bottom, each by its canonical pair) into
/// the row `target` (a store row is `(store, None)`) at `place`, and plan it.
///
/// One refusal refuses them all (Joe, 2026-10-19). A move within one store is
/// one `moveNode`, or one `moveNodes` for several. Otherwise every node goes
/// on its own at the same place, the last first (in order when the place is
/// the end), so they land in their order without waiting on the ids a
/// transplant gives.
pub fn plan_move<V: TreeView + ?Sized>(
    view: &V,
    nodes: &[(StoreId, NodeId)],
    target: (StoreId, Option<NodeId>),
    place: Place,
) -> Result<MovePlan, Refusal> {
    let (to_store, parent) = parent_of_row(view, target.0, target.1)?;
    // A node named twice moves once.
    let mut moved: Vec<(StoreId, NodeId)> = Vec::with_capacity(nodes.len());
    for node in nodes {
        if !moved.contains(node) {
            moved.push(*node);
        }
    }
    if moved.is_empty() {
        return Err(Refusal::NotANode);
    }

    let target_path = ancestors(view, to_store, parent);
    if moved.iter().any(|&(store, node)| store == to_store && target_path.contains(&node)) {
        return Err(Refusal::IntoItself);
    }
    // Any of the three documents a move touches being read only refuses it
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"): the node, the
    // list it leaves and the list it joins.
    let writable = |store: StoreId, node: NodeId| access(view, store, node).allows_write();
    let leaves_writable = |store: StoreId, node: NodeId| view.parent(store, node).is_none_or(|old| writable(store, old));
    if !writable(to_store, parent) || moved.iter().any(|&(s, n)| !writable(s, n) || !leaves_writable(s, n)) {
        return Err(Refusal::ReadOnly);
    }

    let store_name = |store: StoreId| view.store(store).map(|facts| facts.name).unwrap_or_default();
    let parent_name =
        if view.store(to_store).is_some_and(|facts| facts.root == parent) { store_name(to_store) } else { view.label(to_store, parent) };
    let count = moved.len();
    let what = format!("{count} items");
    let line = match place {
        Place::After(anchor) if count == 1 => format!("In {parent_name}, after {}", view.label(to_store, anchor)),
        Place::After(anchor) => format!("{what} in {parent_name}, after {}", view.label(to_store, anchor)),
        Place::First if count == 1 => format!("Into {parent_name}, first"),
        Place::First => format!("{what} into {parent_name}, first"),
        Place::Last if count == 1 => format!("Into {parent_name}"),
        Place::Last => format!("{what} into {parent_name}"),
    };

    if let [(store, node)] = moved[..] {
        if store == to_store && view.parent(store, node) == Some(parent) && stays_put(view, store, parent, node, place) {
            return Ok(MovePlan { parent: (to_store, parent), place, steps: Vec::new(), line: WHERE_IT_IS.to_string(), warning: None });
        }
    }

    // What else it does: new nodes in another store, or leaving a share.
    let warning = match moved.iter().find(|(store, _)| *store != to_store) {
        Some(&(from, _)) => Some(transplant_warning(count, &store_name(to_store), &store_name(from))),
        // docs/MOVE_CONTRACT.md "What 'out of a share' means": a share above
        // the node that is not above (or at) the new parent.
        None => moved
            .iter()
            .find_map(|&(store, node)| {
                ancestors(view, store, node).into_iter().skip(1).find(|at| view.is_shared(store, *at) && !target_path.contains(at))
            })
            .map(|share| leaves_share_warning(count, &view.label(to_store, share))),
    };

    Ok(MovePlan { parent: (to_store, parent), place, steps: steps(&moved, to_store, parent, place), line, warning })
}

fn steps(nodes: &[(StoreId, NodeId)], to_store: StoreId, parent: NodeId, place: Place) -> Vec<MoveStep> {
    if nodes.iter().all(|(store, _)| *store == to_store) {
        return match nodes {
            [(_, node)] => vec![MoveStep::Move { store: to_store, node: *node, parent, place }],
            _ => vec![MoveStep::MoveMany { store: to_store, nodes: nodes.iter().map(|(_, node)| *node).collect(), parent, place }],
        };
    }
    let one = |&(store, node): &(StoreId, NodeId)| {
        if store == to_store {
            MoveStep::Move { store, node, parent, place }
        } else {
            MoveStep::Transplant { from: store, node, to: to_store, parent, place }
        }
    };
    match place {
        Place::Last => nodes.iter().map(one).collect(),
        _ => nodes.iter().rev().map(one).collect(),
    }
}

/// Whether `place` under `parent` is where `node` already is.
fn stays_put<V: TreeView + ?Sized>(view: &V, store: StoreId, parent: NodeId, node: NodeId, place: Place) -> bool {
    let Some(ids) = view.children(store, parent) else { return false };
    let Some(at) = ids.iter().position(|&n| n == node) else { return false };
    match place {
        Place::First => at == 0,
        Place::Last => at + 1 == ids.len(),
        Place::After(anchor) => anchor == node || (at > 0 && ids[at - 1] == anchor),
    }
}
