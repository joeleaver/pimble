//! Drops in the explorer (docs/TREE_CONTRACT.md "The drop model"): what a
//! drop rinch's Tree offers means in Pimble's terms, whether it may happen,
//! and the sentence at the foot of the explorer that says where it lands.
//!
//! The Tree knows rows; `pimble_core::plan::plan_move` knows stores, and
//! judges and phrases every move (the MCP's and a kanban's too); this only
//! translates. `plan_drop` is the one place a drop is judged: the Tree asks
//! it while dragging (`judge_drop`), the strip shows its sentence
//! (`ondropchange`), and a release sends what it planned (`ondrop`), so what
//! was shown is what happens. A drop carries one row or
//! a selection (wave 3); every node of it is judged, and one refusal refuses
//! them all (Joe, 2026-10-19).

use pimble_core::plan::{plan_move, MovePlan, MoveStep, Refusal};
use pimble_core::Place;
use rinch::prelude::{DropVerdict, TreeDrop, TreeSlot};

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

/// The one judgement of a move (`pimble_core::plan::plan_move`) and the
/// commands that make what it planned. `expand` is the tree value of the new
/// parent, opened once the move is answered so the nodes are seen.
fn plan_of(plan: MovePlan, expand: Option<String>) -> DropPlan {
    let hint = DropHint { line: plan.line.clone(), warning: plan.warning.clone(), refused: false };
    if plan.stays() {
        return DropPlan::Stays(hint);
    }
    let cmds = plan.steps.into_iter().map(command).collect();
    DropPlan::Send { cmds, hint, expand }
}

fn command(step: MoveStep) -> BackendCommand {
    match step {
        MoveStep::Move { store, node, parent, place } => {
            BackendCommand::MoveNode { store_id: store, node_id: node, new_parent_id: parent, place }
        }
        MoveStep::MoveMany { store, nodes, parent, place } => {
            BackendCommand::MoveNodes { store_id: store, node_ids: nodes, new_parent_id: parent, place }
        }
        MoveStep::Transplant { from, node, to, parent, place } => {
            BackendCommand::TransplantNode { from_store_id: from, node_id: node, to_store_id: to, new_parent_id: parent, place }
        }
    }
}

fn refused(refusal: Refusal) -> DropPlan {
    DropPlan::Refused(DropHint { line: refusal.sentence().to_string(), warning: None, refused: true })
}

/// What a drag from the row `value` carries: a node, never a store row.
pub(crate) fn drag_values(value: &str) -> Vec<String> {
    match parse_tree_value(value) {
        Some((_, Some(_))) => vec![value.to_string()],
        _ => Vec::new(),
    }
}

/// The plan for a drop the Tree offers: the Tree's rows and slot in
/// Pimble's terms (the nodes, the row they go into, a `Place`), judged by
/// `pimble_core::plan::plan_move`.
pub(crate) fn plan_drop(store: AppStore, drop: &TreeDrop) -> DropPlan {
    let mut nodes = Vec::with_capacity(drop.dragged.len());
    for value in &drop.dragged {
        let Some((s, Some(n))) = parse_tree_value(value) else { return refused(Refusal::NotANode) };
        nodes.push((s, n));
    }
    let target = match drop.parent.as_deref().map(parse_tree_value) {
        None => return refused(Refusal::BesideAStore),
        Some(None) => return refused(Refusal::NotANode),
        Some(Some(row)) => row,
    };
    let place = match &drop.slot {
        TreeSlot::First => Place::First,
        TreeSlot::Last => Place::Last,
        TreeSlot::After(value) => match parse_tree_value(value) {
            Some((_, Some(anchor))) => Place::After(anchor),
            _ => Place::Last,
        },
    };
    match plan_move(&store.view(), &nodes, target, place) {
        Ok(plan) => plan_of(plan, drop.parent.clone().filter(|v| v.starts_with("node_"))),
        Err(refusal) => refused(refusal),
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

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::{NodeId, StoreId};

    fn plan(steps: Vec<MoveStep>) -> MovePlan {
        let s = StoreId::new();
        MovePlan { parent: (s, NodeId::new()), place: Place::Last, steps, line: "Into MEDICAL".into(), warning: Some("Leaves".into()) }
    }

    /// What `plan_move` planned is what is sent, in its order, and what the
    /// strip says (the judgements themselves are `pimble_core::plan`'s tests).
    #[test]
    fn a_plan_becomes_its_commands() {
        let (s, t, n, p) = (StoreId::new(), StoreId::new(), NodeId::new(), NodeId::new());
        let steps = vec![
            MoveStep::Transplant { from: s, node: n, to: t, parent: p, place: Place::First },
            MoveStep::Move { store: t, node: n, parent: p, place: Place::First },
        ];
        match plan_of(plan(steps), Some("node_x_y".into())) {
            DropPlan::Send { cmds, hint, expand } => {
                assert!(matches!(
                    cmds.as_slice(),
                    [BackendCommand::TransplantNode { from_store_id, to_store_id, .. }, BackendCommand::MoveNode { store_id, .. }]
                        if (*from_store_id, *to_store_id, *store_id) == (s, t, t)
                ));
                assert_eq!((hint.line.as_str(), hint.warning.as_deref(), hint.refused), ("Into MEDICAL", Some("Leaves"), false));
                assert_eq!(expand.as_deref(), Some("node_x_y"));
            }
            other => panic!("expected a move, got {other:?}"),
        }
        let many = MoveStep::MoveMany { store: s, nodes: vec![n, p], parent: p, place: Place::Last };
        assert!(matches!(plan_of(plan(vec![many]), None), DropPlan::Send { cmds, .. } if matches!(cmds.as_slice(), [BackendCommand::MoveNodes { .. }])));
    }

    #[test]
    fn a_plan_that_stays_sends_nothing_and_a_refusal_says_why() {
        let stays = plan_of(plan(Vec::new()), None);
        assert!(matches!(stays, DropPlan::Stays(_)));
        assert_eq!(stays.verdict(), DropVerdict::Allowed);
        let no = refused(Refusal::IntoItself);
        assert_eq!((no.verdict(), no.hint().line.as_str()), (DropVerdict::Refused, pimble_core::plan::INTO_ITSELF));
    }

    #[test]
    fn store_rows_are_not_dragged() {
        let (s, n) = (StoreId::new(), NodeId::new());
        assert!(drag_values(&format!("store_{s}")).is_empty());
        assert_eq!(drag_values(&format!("node_{s}_{n}")), vec![format!("node_{s}_{n}")]);
    }
}
