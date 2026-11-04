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
    if let Some(sentence) = found_refusal(drop) {
        return DropPlan::Refused(DropHint { line: sentence, warning: None, refused: true });
    }
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

/// Why a drop touches a found row's place, when it does
/// (docs/QUERY_METANODES_CONTRACT.md section 4): a found row dragged away, a
/// drop into one, or beside one (after it, or first among its metanode's
/// finds). Said as the row's own sentence: "Shown here because it is tagged
/// #waiting. It lives in Personal / Projects."
pub(crate) fn found_refusal(drop: &TreeDrop) -> Option<String> {
    use pimble_core::plan::FoundRowAction;
    if let Some(found) = crate::state::first_found(&drop.dragged).filter(|_| !FoundRowAction::Drag.allowed()) {
        return Some(found.refusal);
    }
    if FoundRowAction::Drop.allowed() {
        return None;
    }
    let into = drop.parent.as_deref().and_then(crate::state::found_row);
    let beside = match &drop.slot {
        TreeSlot::After(value) => crate::state::found_row(value),
        _ => None,
    };
    into.or(beside).map(|found| found.refusal)
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

    /// docs/QUERY_METANODES_CONTRACT.md section 4: a found row is not
    /// dragged away, and nothing is dropped into or beside it, each refused
    /// with its own sentence; a drop onto the metanode itself is ordinary.
    #[test]
    fn a_found_rows_place_is_not_changed_by_a_drop() {
        use crate::state::{AppStore, Found};
        use pimble_core::metanode::{Condition, Metanode, Source};
        use pimble_core::{Node, Store};
        use rinch::prelude::{untracked, DropZone};
        let app = AppStore::new();
        let mut st = Store::new_local("Notes", "/tmp/notes.pimble".into());
        let root = Node::folder("Notes");
        st.root_node_id = root.id;
        let s = st.id;
        let mut list = Node::document("Waiting");
        list.metadata.metanode = Some(Metanode { viewer: "list".into(), source: Source::matching(vec![Condition::Tag { tag: "x".into() }]) });
        let (plain, own) = (Node::document("Plain"), Node::document("Own"));
        let (root_id, list_id, plain_id, own_id) = (root.id, list.id, plain.id, own.id);
        app.upsert_store(st);
        for n in [root, list, plain, own] {
            app.upsert_node(s, n);
        }
        app.set_children(s, root_id, vec![(s, plain_id), (s, list_id)]);
        app.set_children(s, list_id, vec![(s, own_id)]);
        let sentence = "Shown here because it is tagged #x. It lives in Notes.";
        app.finds.set(std::collections::HashMap::from([((s, list_id), vec![Found { store_id: s, node_id: plain_id, lives_in: "Notes".into(), refusal: sentence.into() }])]));
        untracked(|| app.build_tree_data_structural());
        let (list_row, own_row, plain_row) = (format!("node_{s}_{list_id}"), format!("node_{s}_{own_id}"), format!("node_{s}_{plain_id}"));
        let found_row = format!("node_{s}_{plain_id}/{s}_{list_id}");
        let drop = |dragged: &str, parent: &str, slot: TreeSlot| TreeDrop {
            dragged: vec![dragged.to_string()],
            parent: Some(parent.to_string()),
            slot,
            over: parent.to_string(),
            zone: DropZone::Inside,
        };
        let refused = |d: &TreeDrop| matches!(plan_drop(app, d), DropPlan::Refused(h) if h.line == sentence);
        assert!(refused(&drop(&found_row, &format!("store_{s}"), TreeSlot::Last)), "dragged away");
        assert!(refused(&drop(&own_row, &found_row, TreeSlot::Last)), "into it");
        assert!(refused(&drop(&own_row, &list_row, TreeSlot::After(found_row.clone()))), "beside it");
        assert!(!refused(&drop(&own_row, &list_row, TreeSlot::Last)), "onto the metanode: an own child");
        assert!(found_refusal(&drop(&plain_row, &list_row, TreeSlot::First)).is_none(), "the node in its own place is ordinary");
    }

    #[test]
    fn store_rows_are_not_dragged() {
        let (s, n) = (StoreId::new(), NodeId::new());
        assert!(drag_values(&format!("store_{s}")).is_empty());
        assert_eq!(drag_values(&format!("node_{s}_{n}")), vec![format!("node_{s}_{n}")]);
    }
}
