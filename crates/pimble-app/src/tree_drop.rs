//! Drops in the explorer (docs/TREE_CONTRACT.md "The drop model"): what a
//! drop rinch's Tree offers means in Pimble's terms, whether it may happen,
//! and the sentence at the foot of the explorer that says where it lands.
//!
//! The Tree knows rows; this knows stores. `plan_drop` is the one place a
//! drop is judged: the Tree asks it while dragging (`judge_drop`), the strip
//! shows its sentence (`ondropchange`), and a release sends what it planned
//! (`ondrop`), so what was shown is what happens. A drop carries one row or
//! a selection (wave 3); every node of it is judged, and one refusal refuses
//! them all (Joe, 2026-10-19).

use pimble_core::{NodeId, Place, StoreAccess, StoreId};
use rinch::prelude::{untracked, DropVerdict, TreeDrop, TreeSlot};

use crate::protocol::BackendCommand;
use crate::state::{parse_tree_value, AppStore};

/// What the strip at the foot of the explorer says during a drag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropHint {
    /// Where it lands, or why it cannot.
    pub line: String,
    /// What else the drop does (a new node in another store, leaving a
    /// share), when it does.
    pub warning: Option<String>,
    pub refused: bool,
}

/// What a drop does.
#[derive(Debug)]
pub(crate) enum DropPlan {
    /// Send `cmds`, in order; `hint` says where it lands. `expand` is the
    /// tree value of the new parent, opened once the move is answered so the
    /// nodes are seen.
    Send { cmds: Vec<BackendCommand>, hint: DropHint, expand: Option<String> },
    /// It is already there: nothing to send.
    Stays(DropHint),
    Refused(DropHint),
}

impl DropPlan {
    pub(crate) fn verdict(&self) -> DropVerdict {
        match self {
            DropPlan::Refused(_) => DropVerdict::Refused,
            _ => DropVerdict::Allowed,
        }
    }

    pub(crate) fn hint(&self) -> &DropHint {
        match self {
            DropPlan::Send { hint, .. } | DropPlan::Stays(hint) | DropPlan::Refused(hint) => hint,
        }
    }
}

/// Refusals the Tree's own rules cannot know.
pub(crate) const BESIDE_A_STORE: &str = "Nodes go inside a store, not beside it.";
pub(crate) const INTO_ITSELF: &str = "A node can't go inside itself.";
pub(crate) const MOUNT_NOT_HERE: &str = "This mount's store isn't open here.";
pub(crate) const NOT_A_NODE: &str = "Only nodes can be moved.";

/// One dragged node, as `judge` needs it.
#[derive(Clone, Debug)]
pub(crate) struct NodeFacts {
    pub node: (StoreId, NodeId),
    /// The node itself, and the list it leaves.
    pub node_access: StoreAccess,
    pub leaves_access: StoreAccess,
    /// The new parent is this node or under it.
    pub into_itself: bool,
    /// The drop puts it exactly where it is.
    pub stays: bool,
    /// The nearest share the move would take it out of.
    pub leaves_share: Option<String>,
    pub store_name: String,
}

/// Everything `judge` needs, read from the app's state by `facts_of`.
#[derive(Clone, Debug)]
pub(crate) struct DropFacts {
    /// The dragged nodes, top to bottom.
    pub nodes: Vec<NodeFacts>,
    /// The new parent as a node (through a mount: its source), or why
    /// there is none.
    pub target: Result<(StoreId, NodeId), &'static str>,
    pub place: Place,
    /// The list the nodes join.
    pub target_access: StoreAccess,
    /// Names for the sentence.
    pub parent_name: String,
    pub anchor_name: Option<String>,
    pub to_store_name: String,
    /// The new parent's tree value, to open after the move.
    pub parent_value: Option<String>,
}

/// Judge a drop and phrase it. Pure: every fact was read beforehand.
pub(crate) fn judge(facts: &DropFacts) -> DropPlan {
    let refuse = |line: &str| DropPlan::Refused(DropHint { line: line.to_string(), warning: None, refused: true });
    let target = match facts.target {
        Ok(target) => target,
        Err(reason) => return refuse(reason),
    };
    if facts.nodes.is_empty() {
        return refuse(NOT_A_NODE);
    }
    // One refusal refuses them all.
    if facts.nodes.iter().any(|n| n.into_itself) {
        return refuse(INTO_ITSELF);
    }
    let read_only = |n: &NodeFacts| !n.node_access.allows_write() || !n.leaves_access.allows_write();
    if !facts.target_access.allows_write() || facts.nodes.iter().any(read_only) {
        return refuse(StoreAccess::READ_ONLY_REFUSAL);
    }

    let count = facts.nodes.len();
    let what = format!("{count} items");
    let line = match (&facts.place, &facts.anchor_name) {
        (Place::After(_), Some(anchor)) if count == 1 => format!("In {}, after {}", facts.parent_name, anchor),
        (Place::After(_), Some(anchor)) => format!("{what} in {}, after {}", facts.parent_name, anchor),
        (Place::First, _) if count == 1 => format!("Into {}, first", facts.parent_name),
        (Place::First, _) => format!("{what} into {}, first", facts.parent_name),
        _ if count == 1 => format!("Into {}", facts.parent_name),
        _ => format!("{what} into {}", facts.parent_name),
    };
    if count == 1 && facts.nodes[0].stays {
        return DropPlan::Stays(DropHint { line: "Where it is".into(), warning: None, refused: false });
    }

    // What else it does: new nodes in another store, or leaving a share.
    let (it, they) = if count == 1 { ("it", "it is") } else { ("them", "they are") };
    let warning = match facts.nodes.iter().find(|n| n.node.0 != target.0) {
        Some(from) => Some(format!(
            "{} in {}; {they} deleted from {} (Put Back in Recently Deleted).",
            if count == 1 { "A new node" } else { "New nodes" },
            facts.to_store_name,
            from.store_name
        )),
        None => facts
            .nodes
            .iter()
            .find_map(|n| n.leaves_share.as_ref())
            .map(|share| format!("Leaves the share {share}: its members will see {it} deleted and can put {it} back.")),
    };

    DropPlan::Send { cmds: commands(facts, target), hint: DropHint { line, warning, refused: false }, expand: facts.parent_value.clone() }
}

/// The commands that make the drop. All in the target's store: one
/// `moveNodes` (or `moveNode` for one), judged by the server all or none.
/// Otherwise one per node, each at the same place, the last first (in order
/// when the place is the end), so they land in their order without waiting
/// on the ids a transplant gives.
fn commands(facts: &DropFacts, target: (StoreId, NodeId)) -> Vec<BackendCommand> {
    let (to_store, parent) = target;
    let place = facts.place;
    if facts.nodes.iter().all(|n| n.node.0 == to_store) {
        let ids: Vec<NodeId> = facts.nodes.iter().map(|n| n.node.1).collect();
        return match ids.as_slice() {
            [one] => vec![BackendCommand::MoveNode { store_id: to_store, node_id: *one, new_parent_id: parent, place }],
            _ => vec![BackendCommand::MoveNodes { store_id: to_store, node_ids: ids, new_parent_id: parent, place }],
        };
    }
    let one = |n: &NodeFacts| {
        if n.node.0 == to_store {
            BackendCommand::MoveNode { store_id: to_store, node_id: n.node.1, new_parent_id: parent, place }
        } else {
            BackendCommand::TransplantNode { from_store_id: n.node.0, node_id: n.node.1, to_store_id: to_store, new_parent_id: parent, place }
        }
    };
    match place {
        Place::Last => facts.nodes.iter().map(one).collect(),
        _ => facts.nodes.iter().rev().map(one).collect(),
    }
}

/// What a drag from the row `value` carries: a node, never a store row.
pub(crate) fn drag_values(value: &str) -> Vec<String> {
    match parse_tree_value(value) {
        Some((_, Some(_))) => vec![value.to_string()],
        _ => Vec::new(),
    }
}

/// The plan for a drop the Tree offers.
pub(crate) fn plan_drop(store: AppStore, drop: &TreeDrop) -> DropPlan {
    match facts_of(store, drop) {
        Ok(facts) => judge(&facts),
        Err(reason) => DropPlan::Refused(DropHint { line: reason.to_string(), warning: None, refused: true }),
    }
}

/// Send what a plan says to send; `true` when it sent anything.
pub(crate) fn send_plan(store: AppStore, plan: DropPlan) -> bool {
    match plan {
        DropPlan::Send { cmds, expand, .. } => {
            // The new parent's exact (possibly mount-path-qualified) tree
            // value, so `NodeMoved` opens precisely this place.
            if let Some(value) = expand {
                crate::state::set_last_drop_target_value(value);
            }
            for cmd in cmds {
                tracing::info!("Drop: {:?}", cmd);
                store.send(cmd);
            }
            true
        }
        _ => false,
    }
}

fn store_name(store: AppStore, store_id: StoreId) -> String {
    store
        .get_store_signal(store_id)
        .map(|sig| untracked(|| sig.with(|s| s.name.clone())))
        .unwrap_or_default()
}

/// `node` and its ancestors, nearest first, as far as this device holds them.
fn path_up(store: AppStore, store_id: StoreId, node: NodeId) -> Vec<NodeId> {
    let mut path = vec![node];
    let mut at = node;
    while let Some(parent) = store.cached_parent_id(store_id, at) {
        if path.contains(&parent) {
            break;
        }
        path.push(parent);
        at = parent;
    }
    path
}

/// Read what `judge` needs from the app's state.
fn facts_of(store: AppStore, drop: &TreeDrop) -> Result<DropFacts, &'static str> {
    // The new parent: a store row stands for its root, and a mount for the
    // node it mounts, whose children are the ones drawn under it.
    let target = match drop.parent.as_deref().map(parse_tree_value) {
        None => Err(BESIDE_A_STORE),
        Some(None) => Err(NOT_A_NODE),
        Some(Some((s, None))) => store.root_node_id(s).map(|root| (s, root)).ok_or(NOT_A_NODE),
        Some(Some((s, Some(n)))) if store.is_mount(s, n) => store
            .get_mount_signal(s, n)
            .and_then(|sig| untracked(|| sig.with(|m| m.mount_ref.as_ref().map(|r| (r.source_store, r.source_node)))))
            .filter(|(source, _)| store.get_store_signal(*source).is_some())
            .ok_or(MOUNT_NOT_HERE),
        Some(Some((s, Some(n)))) => Ok((s, n)),
    };

    let place = match &drop.slot {
        TreeSlot::First => Place::First,
        TreeSlot::Last => Place::Last,
        TreeSlot::After(value) => match parse_tree_value(value) {
            Some((_, Some(anchor))) => Place::After(anchor),
            _ => Place::Last,
        },
    };
    let anchor_name = match (&place, &target) {
        (Place::After(anchor), Ok((s, _))) => Some(store.display_label(*s, *anchor)),
        _ => None,
    };
    let (target_access, target_path, parent_name) = match target {
        Ok((ts, tn)) => {
            let parent_name = if store.root_node_id(ts) == Some(tn) { store_name(store, ts) } else { store.display_label(ts, tn) };
            (store.node_access(ts, tn), path_up(store, ts, tn), parent_name)
        }
        Err(_) => (StoreAccess::Full, Vec::new(), String::new()),
    };

    let mut nodes = Vec::with_capacity(drop.dragged.len());
    for value in &drop.dragged {
        let Some((s, Some(n))) = parse_tree_value(value) else { return Err(NOT_A_NODE) };
        let old_parent = store.cached_parent_id(s, n);
        let (into_itself, stays, leaves_share) = match target {
            Ok((ts, tn)) if ts == s => {
                let stays = old_parent == Some(tn) && stays_put(store, ts, tn, n, place);
                // docs/MOVE_CONTRACT.md "What 'out of a share' means": a
                // share above the node that is not above (or at) the new
                // parent.
                let leaves_share = path_up(store, s, n)
                    .into_iter()
                    .skip(1)
                    .find(|r| store.is_shared(s, *r) && !target_path.contains(r))
                    .map(|r| store.display_label(s, r));
                (target_path.contains(&n), stays, leaves_share)
            }
            _ => (false, false, None),
        };
        nodes.push(NodeFacts {
            node: (s, n),
            node_access: store.node_access(s, n),
            leaves_access: old_parent.map_or(StoreAccess::Full, |p| store.node_access(s, p)),
            into_itself,
            stays,
            leaves_share,
            store_name: store_name(store, s),
        });
    }
    let to_store = target.map(|(s, _)| s).ok().or(nodes.first().map(|n| n.node.0));
    Ok(DropFacts {
        nodes,
        target,
        place,
        target_access,
        parent_name,
        anchor_name,
        to_store_name: to_store.map(|s| store_name(store, s)).unwrap_or_default(),
        parent_value: drop.parent.clone().filter(|v| v.starts_with("node_")),
    })
}

/// Whether `place` under `parent` is where `node` already is.
fn stays_put(store: AppStore, store_id: StoreId, parent: NodeId, node: NodeId, place: Place) -> bool {
    let Some(children) = store.get_children_signal(store_id, parent) else { return false };
    let ids: Vec<NodeId> = untracked(|| children.with(|c| c.iter().map(|(_, n)| *n).collect()));
    let Some(at) = ids.iter().position(|&n| n == node) else { return false };
    match place {
        Place::First => at == 0,
        Place::Last => at + 1 == ids.len(),
        Place::After(anchor) => anchor == node || (at > 0 && ids[at - 1] == anchor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::StoreAccess::{Full, Read};

    fn node(s: StoreId) -> NodeFacts {
        NodeFacts {
            node: (s, NodeId::new()),
            node_access: Full,
            leaves_access: Full,
            into_itself: false,
            stays: false,
            leaves_share: None,
            store_name: "Family".into(),
        }
    }

    fn facts() -> DropFacts {
        let s = StoreId::new();
        DropFacts {
            nodes: vec![node(s)],
            target: Ok((s, NodeId::new())),
            place: Place::Last,
            target_access: Full,
            parent_name: "MEDICAL".into(),
            anchor_name: None,
            to_store_name: "Family".into(),
            parent_value: Some("node_x_y".into()),
        }
    }

    fn line(plan: &DropPlan) -> &str {
        &plan.hint().line
    }

    #[test]
    fn a_drop_within_one_store_is_a_move_at_its_place() {
        let mut f = facts();
        let anchor = NodeId::new();
        f.place = Place::After(anchor);
        f.anchor_name = Some("Boo Kitty Medical".into());
        let plan = judge(&f);
        assert_eq!(line(&plan), "In MEDICAL, after Boo Kitty Medical");
        match plan {
            DropPlan::Send { cmds, hint, expand } => {
                assert!(matches!(
                    cmds.as_slice(),
                    [BackendCommand::MoveNode { store_id, node_id, new_parent_id, place }]
                        if (*store_id, *node_id) == f.nodes[0].node && *new_parent_id == f.target.unwrap().1 && *place == Place::After(anchor)
                ));
                assert_eq!(hint.warning, None);
                assert_eq!(expand.as_deref(), Some("node_x_y"));
            }
            other => panic!("expected a move, got {other:?}"),
        }
    }

    #[test]
    fn the_sentence_names_the_place() {
        let mut f = facts();
        assert_eq!(line(&judge(&f)), "Into MEDICAL");
        f.place = Place::First;
        assert_eq!(line(&judge(&f)), "Into MEDICAL, first");
    }

    /// Several nodes of the target's store go as one `moveNodes`, in order,
    /// and the sentence counts them.
    #[test]
    fn several_nodes_move_as_one_request_and_are_counted() {
        let mut f = facts();
        let s = f.nodes[0].node.0;
        f.nodes.push(node(s));
        f.nodes.push(node(s));
        let ids: Vec<NodeId> = f.nodes.iter().map(|n| n.node.1).collect();
        let plan = judge(&f);
        assert_eq!(line(&plan), "3 items into MEDICAL");
        match plan {
            DropPlan::Send { cmds, .. } => assert!(matches!(
                cmds.as_slice(),
                [BackendCommand::MoveNodes { node_ids, place: Place::Last, .. }] if *node_ids == ids
            )),
            other => panic!("expected a move, got {other:?}"),
        }
    }

    /// Nodes from another store go one command each, the last first, at the
    /// same place: they land in their order.
    #[test]
    fn nodes_from_another_store_go_last_first_at_one_place() {
        let mut f = facts();
        let other = StoreId::new();
        f.nodes = vec![node(other), node(other)];
        f.place = Place::First;
        f.to_store_name = "Work".into();
        let ids: Vec<NodeId> = f.nodes.iter().map(|n| n.node.1).collect();
        match judge(&f) {
            DropPlan::Send { cmds, hint, .. } => {
                let sent: Vec<NodeId> = cmds
                    .iter()
                    .map(|c| match c {
                        BackendCommand::TransplantNode { node_id, place: Place::First, .. } => *node_id,
                        other => panic!("expected a transplant, got {other:?}"),
                    })
                    .collect();
                assert_eq!(sent, vec![ids[1], ids[0]]);
                assert_eq!(hint.warning.as_deref(), Some("New nodes in Work; they are deleted from Family (Put Back in Recently Deleted)."));
            }
            other => panic!("expected transplants, got {other:?}"),
        }
    }

    /// docs/MOVE_CONTRACT.md: a move between stores is always a transplant,
    /// and the strip says what that means before the drop.
    #[test]
    fn a_drop_between_stores_is_a_transplant_and_says_so() {
        let mut f = facts();
        let other = StoreId::new();
        f.target = Ok((other, NodeId::new()));
        f.to_store_name = "Work".into();
        match judge(&f) {
            DropPlan::Send { cmds, hint, .. } => {
                assert!(matches!(cmds.as_slice(), [BackendCommand::TransplantNode { to_store_id, place: Place::Last, .. }] if *to_store_id == other));
                assert_eq!(hint.warning.as_deref(), Some("A new node in Work; it is deleted from Family (Put Back in Recently Deleted)."));
            }
            other => panic!("expected a transplant, got {other:?}"),
        }
    }

    #[test]
    fn leaving_a_share_is_said_before_the_drop() {
        let mut f = facts();
        f.nodes[0].leaves_share = Some("Taxes 2025".into());
        assert_eq!(
            judge(&f).hint().warning.as_deref(),
            Some("Leaves the share Taxes 2025: its members will see it deleted and can put it back.")
        );
    }

    /// Any of the three documents a move touches being read only refuses
    /// the drop (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"), and one
    /// node that may not move refuses all of them.
    #[test]
    fn a_read_only_place_refuses_the_drop() {
        for (n, l, t) in [(Read, Full, Full), (Full, Read, Full), (Full, Full, Read)] {
            let mut f = facts();
            let s = f.nodes[0].node.0;
            f.nodes.insert(0, node(s));
            (f.nodes[1].node_access, f.nodes[1].leaves_access, f.target_access) = (n, l, t);
            let plan = judge(&f);
            assert_eq!(plan.verdict(), DropVerdict::Refused);
            assert_eq!(line(&plan), StoreAccess::READ_ONLY_REFUSAL);
        }
    }

    #[test]
    fn refusals_say_why() {
        let mut f = facts();
        f.target = Err(BESIDE_A_STORE);
        assert_eq!(line(&judge(&f)), BESIDE_A_STORE);
        let mut f = facts();
        f.nodes[0].into_itself = true;
        assert_eq!(line(&judge(&f)), INTO_ITSELF);
        let mut f = facts();
        f.target = Err(MOUNT_NOT_HERE);
        assert_eq!(judge(&f).verdict(), DropVerdict::Refused);
    }

    #[test]
    fn a_drop_where_it_already_is_sends_nothing() {
        let mut f = facts();
        f.nodes[0].stays = true;
        let plan = judge(&f);
        assert!(matches!(plan, DropPlan::Stays(_)));
        assert_eq!(plan.verdict(), DropVerdict::Allowed);
    }

    #[test]
    fn store_rows_are_not_dragged() {
        let (s, n) = (StoreId::new(), NodeId::new());
        assert!(drag_values(&format!("store_{s}")).is_empty());
        assert_eq!(drag_values(&format!("node_{s}_{n}")), vec![format!("node_{s}_{n}")]);
    }
}
