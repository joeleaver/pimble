//! Drops in the explorer (docs/TREE_CONTRACT.md "The drop model"): what a
//! drop rinch's Tree offers means in Pimble's terms, whether it may happen,
//! and the sentence at the foot of the explorer that says where it lands.
//!
//! The Tree knows rows; this knows stores. `plan_drop` is the one place a
//! drop is judged: the Tree asks it while dragging (`judge_drop`), the strip
//! shows its sentence (`ondropchange`), and a release sends what it planned
//! (`ondrop`), so what was shown is what happens.

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
    /// Send `cmd`; `hint` says where it lands. `expand` is the tree value of
    /// the new parent, opened once the move is answered so the node is seen.
    Send { cmd: BackendCommand, hint: DropHint, expand: Option<String> },
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

/// Everything `judge` needs, read from the app's state by `facts_of`.
#[derive(Clone, Debug)]
pub(crate) struct DropFacts {
    pub dragged: (StoreId, NodeId),
    /// The new parent as a node (through a mount: its source), or why
    /// there is none.
    pub target: Result<(StoreId, NodeId), &'static str>,
    pub place: Place,
    /// The node itself, the list it leaves, the list it joins.
    pub node_access: StoreAccess,
    pub leaves_access: StoreAccess,
    pub target_access: StoreAccess,
    /// The new parent is the dragged node or under it.
    pub into_itself: bool,
    /// The drop puts it exactly where it is.
    pub stays: bool,
    /// Names for the sentence.
    pub parent_name: String,
    pub anchor_name: Option<String>,
    pub from_store_name: String,
    pub to_store_name: String,
    /// The nearest share the move would take it out of.
    pub leaves_share: Option<String>,
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
    if facts.into_itself {
        return refuse(INTO_ITSELF);
    }
    if !facts.node_access.allows_write() || !facts.leaves_access.allows_write() || !facts.target_access.allows_write() {
        return refuse(StoreAccess::READ_ONLY_REFUSAL);
    }
    let line = match (&facts.place, &facts.anchor_name) {
        (Place::After(_), Some(anchor)) => format!("In {}, after {}", facts.parent_name, anchor),
        (Place::First, _) => format!("Into {}, first", facts.parent_name),
        _ => format!("Into {}", facts.parent_name),
    };
    if facts.stays {
        return DropPlan::Stays(DropHint { line: "Where it is".into(), warning: None, refused: false });
    }
    let (dragged_store, dragged_node) = facts.dragged;
    let (cmd, warning) = if dragged_store == target.0 {
        let warning = facts.leaves_share.as_ref().map(|share| {
            format!("Leaves the share {share}: its members will see it deleted and can put it back.")
        });
        (
            BackendCommand::MoveNode { store_id: dragged_store, node_id: dragged_node, new_parent_id: target.1, place: facts.place },
            warning,
        )
    } else {
        (
            BackendCommand::TransplantNode {
                from_store_id: dragged_store,
                node_id: dragged_node,
                to_store_id: target.0,
                new_parent_id: target.1,
                place: facts.place,
            },
            Some(format!(
                "A new node in {}; it is deleted from {} (Put Back in Recently Deleted).",
                facts.to_store_name, facts.from_store_name
            )),
        )
    };
    DropPlan::Send { cmd, hint: DropHint { line, warning, refused: false }, expand: facts.parent_value.clone() }
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
    // One row at a time until the explorer has a selection (wave 3).
    let dragged_value = drop.dragged.first().ok_or(NOT_A_NODE)?;
    let Some((drag_store, Some(drag_node))) = parse_tree_value(dragged_value) else { return Err(NOT_A_NODE) };

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

    let old_parent = store.cached_parent_id(drag_store, drag_node);
    let node_access = store.node_access(drag_store, drag_node);
    let leaves_access = old_parent.map_or(StoreAccess::Full, |p| store.node_access(drag_store, p));
    let (target_access, into_itself, stays, parent_name, leaves_share) = match target {
        Ok((ts, tn)) => {
            let same_store = ts == drag_store;
            let target_path = path_up(store, ts, tn);
            let into_itself = same_store && target_path.contains(&drag_node);
            let stays = same_store && old_parent == Some(tn) && stays_put(store, ts, tn, drag_node, place);
            let parent_name = if store.root_node_id(ts) == Some(tn) { store_name(store, ts) } else { store.display_label(ts, tn) };
            // docs/MOVE_CONTRACT.md "What 'out of a share' means": a share
            // above the node that is not above (or at) the new parent. A
            // move to another store leaves every share, and says so anyway.
            let leaves_share = if same_store {
                path_up(store, drag_store, drag_node)
                    .into_iter()
                    .skip(1)
                    .find(|r| store.is_shared(drag_store, *r) && !target_path.contains(r))
                    .map(|r| store.display_label(drag_store, r))
            } else {
                None
            };
            (store.node_access(ts, tn), into_itself, stays, parent_name, leaves_share)
        }
        Err(_) => (StoreAccess::Full, false, false, String::new(), None),
    };
    let to_store = target.map(|(s, _)| s).unwrap_or(drag_store);
    Ok(DropFacts {
        dragged: (drag_store, drag_node),
        target,
        place,
        node_access,
        leaves_access,
        target_access,
        into_itself,
        stays,
        parent_name,
        anchor_name,
        from_store_name: store_name(store, drag_store),
        to_store_name: store_name(store, to_store),
        leaves_share,
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

    fn facts() -> DropFacts {
        let s = StoreId::new();
        DropFacts {
            dragged: (s, NodeId::new()),
            target: Ok((s, NodeId::new())),
            place: Place::Last,
            node_access: Full,
            leaves_access: Full,
            target_access: Full,
            into_itself: false,
            stays: false,
            parent_name: "MEDICAL".into(),
            anchor_name: None,
            from_store_name: "Family".into(),
            to_store_name: "Family".into(),
            leaves_share: None,
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
            DropPlan::Send { cmd: BackendCommand::MoveNode { store_id, node_id, new_parent_id, place }, hint, expand } => {
                assert_eq!((store_id, node_id), f.dragged);
                assert_eq!(new_parent_id, f.target.unwrap().1);
                assert_eq!(place, Place::After(anchor));
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

    /// docs/MOVE_CONTRACT.md: a move between stores is always a transplant,
    /// and the strip says what that means before the drop.
    #[test]
    fn a_drop_between_stores_is_a_transplant_and_says_so() {
        let mut f = facts();
        let other = StoreId::new();
        f.target = Ok((other, NodeId::new()));
        f.to_store_name = "Work".into();
        match judge(&f) {
            DropPlan::Send { cmd: BackendCommand::TransplantNode { to_store_id, place, .. }, hint, .. } => {
                assert_eq!((to_store_id, place), (other, Place::Last));
                assert_eq!(
                    hint.warning.as_deref(),
                    Some("A new node in Work; it is deleted from Family (Put Back in Recently Deleted).")
                );
            }
            other => panic!("expected a transplant, got {other:?}"),
        }
    }

    #[test]
    fn leaving_a_share_is_said_before_the_drop() {
        let mut f = facts();
        f.leaves_share = Some("Taxes 2025".into());
        assert_eq!(
            judge(&f).hint().warning.as_deref(),
            Some("Leaves the share Taxes 2025: its members will see it deleted and can put it back.")
        );
    }

    /// Any of the three documents a move touches being read only refuses
    /// the drop (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles").
    #[test]
    fn a_read_only_place_refuses_the_drop() {
        for (n, l, t) in [(Read, Full, Full), (Full, Read, Full), (Full, Full, Read)] {
            let mut f = facts();
            (f.node_access, f.leaves_access, f.target_access) = (n, l, t);
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
        f.into_itself = true;
        assert_eq!(line(&judge(&f)), INTO_ITSELF);
        let mut f = facts();
        f.target = Err(MOUNT_NOT_HERE);
        assert_eq!(judge(&f).verdict(), DropVerdict::Refused);
    }

    #[test]
    fn a_drop_where_it_already_is_sends_nothing() {
        let mut f = facts();
        f.stays = true;
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
