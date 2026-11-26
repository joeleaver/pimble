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

use pimble_core::plan::{plan_move, MovePlan, MoveStep, Refusal, WHERE_IT_IS};
use pimble_core::{Place, StoreId};
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
    /// Reorder the stores (docs/TREE_CONTRACT.md "Reordering stores"):
    /// `moved`, top to bottom, go right after `after` (the top when `None`).
    /// The window's own order, so nothing is sent to any store.
    Stores { moved: Vec<StoreId>, after: Option<StoreId>, hint: DropHint },
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
            DropPlan::Send { hint, .. } | DropPlan::Stores { hint, .. } | DropPlan::Stays(hint) | DropPlan::Refused(hint) => hint,
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

/// What a drag from the row `value` carries: the row, a node or a store
/// (stores are reordered among themselves, docs/TREE_CONTRACT.md
/// "Reordering stores").
pub(crate) fn drag_values(value: &str) -> Vec<String> {
    match parse_tree_value(value) {
        Some(_) => vec![value.to_string()],
        None => Vec::new(),
    }
}

/// A store dropped inside a store or a node.
pub const STORES_SIDE_BY_SIDE: &str = "Stores go beside other stores.";
/// A drag that carries stores and nodes together.
pub const STORES_OR_NODES: &str = "Drag stores and nodes separately.";

/// The plan for a drag that carries store rows: the stores' own order,
/// only among the store rows at the top of the explorer.
fn plan_store_drop(store: AppStore, drop: &TreeDrop) -> DropPlan {
    let refuse = |line: &str| DropPlan::Refused(DropHint { line: line.to_string(), warning: None, refused: true });
    let mut moved = Vec::with_capacity(drop.dragged.len());
    for value in &drop.dragged {
        match parse_tree_value(value) {
            Some((s, None)) if !moved.contains(&s) => moved.push(s),
            Some((_, None)) => {}
            _ => return refuse(STORES_OR_NODES),
        }
    }
    if drop.parent.is_some() {
        return refuse(STORES_SIDE_BY_SIDE);
    }
    let order = untracked(|| store.store_ids.get());
    let after = match &drop.slot {
        TreeSlot::First => None,
        TreeSlot::Last => order.iter().rev().find(|id| !moved.contains(id)).copied(),
        TreeSlot::After(value) => match parse_tree_value(value) {
            Some((s, None)) => Some(s),
            _ => return refuse(STORES_SIDE_BY_SIDE),
        },
    };
    // Where they would be, against where they are.
    let mut next: Vec<StoreId> = order.iter().copied().filter(|id| !moved.contains(id)).collect();
    let at = after.and_then(|a| next.iter().position(|&id| id == a)).map_or(0, |at| at + 1);
    next.splice(at..at, moved.iter().copied());
    if next == order || after.is_some_and(|a| moved.contains(&a)) {
        return DropPlan::Stays(DropHint { line: WHERE_IT_IS.to_string(), warning: None, refused: false });
    }
    let what = match moved.len() {
        1 => String::new(),
        n => format!("{n} stores "),
    };
    let line = match after {
        None if what.is_empty() => "First of the stores".to_string(),
        None => format!("{what}first"),
        Some(anchor) => {
            let name = store.get_store_signal(anchor).map(|sig| untracked(|| sig.with(|s| s.name.clone()))).unwrap_or_default();
            format!("{what}after {name}")
        }
    };
    let line = capitalize(&line);
    DropPlan::Stores { moved, after, hint: DropHint { line, warning: None, refused: false } }
}

fn capitalize(line: &str) -> String {
    let mut chars = line.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The plan for a drop the Tree offers: the Tree's rows and slot in
/// Pimble's terms (the nodes, the row they go into, a `Place`), judged by
/// `pimble_core::plan::plan_move`.
pub(crate) fn plan_drop(store: AppStore, drop: &TreeDrop) -> DropPlan {
    if let Some(sentence) = found_refusal(drop) {
        return DropPlan::Refused(DropHint { line: sentence, warning: None, refused: true });
    }
    if drop.dragged.iter().any(|v| matches!(parse_tree_value(v), Some((_, None)))) {
        return plan_store_drop(store, drop);
    }
    let mut nodes = Vec::with_capacity(drop.dragged.len());
    for value in &drop.dragged {
        let Some((s, Some(n))) = parse_tree_value(value) else { return refused(Refusal::NotANode) };
        nodes.push((s, n));
    }
    // Into a grouping row ("Themes"): into the parent it groups
    // (`internal_drop`). Beside one, the place falls to the parent's last,
    // as beside any row that is no node, except for the group's own nodes,
    // which already show there.
    if crate::internal_drop::beside_own_group(store, &drop.slot, &nodes) {
        return DropPlan::Stays(DropHint { line: WHERE_IT_IS.to_string(), warning: None, refused: false });
    }
    let (parent_row, place, expand) = match crate::internal_drop::group_drop(store, drop.parent.as_deref(), &drop.slot, &nodes) {
        Some(Err(line)) => return DropPlan::Refused(DropHint { line, warning: None, refused: true }),
        Some(Ok(group)) => (Some(group.parent_row), group.place, Some(group.expand)),
        None => {
            let place = match &drop.slot {
                TreeSlot::First => Place::First,
                TreeSlot::Last => Place::Last,
                TreeSlot::After(value) => match parse_tree_value(value) {
                    Some((_, Some(anchor))) => Place::After(anchor),
                    _ => Place::Last,
                },
            };
            (drop.parent.clone(), place, drop.parent.clone().filter(|v| v.starts_with("node_")))
        }
    };
    let target = match parent_row.as_deref().map(parse_tree_value) {
        None => return refused(Refusal::BesideAStore),
        Some(None) => return refused(Refusal::NotANode),
        Some(Some(row)) => row,
    };
    match plan_move(&store.view(), &nodes, target, place) {
        Ok(plan) => plan_of(plan, expand),
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
        DropPlan::Stores { moved, after, .. } => {
            store.move_stores(&moved, after);
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
    fn store_rows_and_node_rows_drag() {
        let (s, n) = (StoreId::new(), NodeId::new());
        assert_eq!(drag_values(&format!("store_{s}")), vec![format!("store_{s}")]);
        assert_eq!(drag_values(&format!("node_{s}_{n}")), vec![format!("node_{s}_{n}")]);
        assert!(drag_values("nothing").is_empty());
    }

    /// Three stores, A, B and C; A's root holds `folder` (with `x` and `y`)
    /// and `leaf`.
    struct Fixture {
        app: AppStore,
        stores: [StoreId; 3],
        folder: NodeId,
        x: NodeId,
        y: NodeId,
        leaf: NodeId,
    }

    fn fixture() -> Fixture {
        use pimble_core::{Node, Store};
        let app = AppStore::new();
        let mut stores = [StoreId::new(); 3];
        let mut a_root = None;
        for (i, name) in ["A", "B", "C"].into_iter().enumerate() {
            let mut st = Store::new_local(name, format!("/tmp/tree-drop-test/{name}.pimble").into());
            let root = Node::folder(name);
            st.root_node_id = root.id;
            stores[i] = st.id;
            app.upsert_store(st);
            if i == 0 {
                a_root = Some(root.id);
            }
            app.upsert_node(stores[i], root);
        }
        let a = stores[0];
        let (folder, x, y, leaf) = (Node::folder("Folder"), Node::document("X"), Node::document("Y"), Node::document("Leaf"));
        let ids = (folder.id, x.id, y.id, leaf.id);
        let root = a_root.unwrap();
        let (mut folder, mut x, mut y, mut leaf) = (folder, x, y, leaf);
        folder.parent_id = Some(root);
        leaf.parent_id = Some(root);
        x.parent_id = Some(ids.0);
        y.parent_id = Some(ids.0);
        for n in [folder, x, y, leaf] {
            app.upsert_node(a, n);
        }
        app.set_children(a, root, vec![(a, ids.0), (a, ids.3)]);
        app.set_children(a, ids.0, vec![(a, ids.1), (a, ids.2)]);
        Fixture { app, stores, folder: ids.0, x: ids.1, y: ids.2, leaf: ids.3 }
    }

    fn drop_of(dragged: &[String], parent: Option<String>, slot: TreeSlot) -> TreeDrop {
        use rinch::prelude::DropZone;
        let over = dragged[0].clone();
        TreeDrop { dragged: dragged.to_vec(), parent, slot, over, zone: DropZone::Before }
    }

    fn row(s: StoreId, n: NodeId) -> String {
        format!("node_{s}_{n}")
    }

    /// Joe, 2026-11-14: a node dropped just before or after a row at another
    /// level becomes that row's sibling there. The Tree offers the row's
    /// parent and the sibling above it (or `First`); the plan moves the node
    /// there, whatever level it came from.
    #[test]
    fn a_drop_beside_a_row_at_another_level_moves_in_beside_it() {
        let f = fixture();
        let a = f.stores[0];
        // `leaf` (level 1) before `y` (level 2): into Folder, after X.
        let deeper = drop_of(&[row(a, f.leaf)], Some(row(a, f.folder)), TreeSlot::After(row(a, f.x)));
        match plan_drop(f.app, &deeper) {
            DropPlan::Send { cmds, hint, .. } => {
                assert!(matches!(
                    cmds.as_slice(),
                    [BackendCommand::MoveNode { node_id, new_parent_id, place: Place::After(anchor), .. }]
                        if (*node_id, *new_parent_id, *anchor) == (f.leaf, f.folder, f.x)
                ));
                assert_eq!(hint.line, "In Folder, after X");
            }
            other => panic!("expected a move, got {other:?}"),
        }
        // `leaf` before `x`, the first of Folder: first in Folder.
        let first = drop_of(&[row(a, f.leaf)], Some(row(a, f.folder)), TreeSlot::First);
        assert!(matches!(
            plan_drop(f.app, &first),
            DropPlan::Send { cmds, .. } if matches!(cmds.as_slice(), [BackendCommand::MoveNode { new_parent_id, place: Place::First, .. }] if *new_parent_id == f.folder)
        ));
        // `y` (level 2) after `leaf` (level 1): out to the store's top level.
        let shallower = drop_of(&[row(a, f.y)], Some(format!("store_{a}")), TreeSlot::After(row(a, f.leaf)));
        match plan_drop(f.app, &shallower) {
            DropPlan::Send { cmds, hint, .. } => {
                assert!(matches!(
                    cmds.as_slice(),
                    [BackendCommand::MoveNode { node_id, place: Place::After(anchor), .. }] if (*node_id, *anchor) == (f.y, f.leaf)
                ));
                assert_eq!(hint.line, "In A, after Leaf");
            }
            other => panic!("expected a move, got {other:?}"),
        }
        // Into another store, beside a row there: a transplant at that place.
        let b = f.stores[1];
        let across = drop_of(&[row(a, f.x)], Some(format!("store_{b}")), TreeSlot::First);
        assert!(matches!(plan_drop(f.app, &across), DropPlan::Send { cmds, .. } if matches!(cmds.as_slice(), [BackendCommand::TransplantNode { place: Place::First, .. }])));
    }

    /// Joe, 2026-11-14: just below an expanded row there are two positions,
    /// picked by the pointer's column. Inside, the Tree offers the row as
    /// parent and `First`; outside, the row's parent and `After(row)`. Both
    /// plan exactly that, for node rows and store rows.
    #[test]
    fn below_an_expanded_row_inside_and_outside_are_two_places() {
        let f = fixture();
        let [a, b, _] = f.stores;
        let store_row = |s: StoreId| format!("store_{s}");
        // Inside Folder: its first child.
        let inside = drop_of(&[row(a, f.leaf)], Some(row(a, f.folder)), TreeSlot::First);
        match plan_drop(f.app, &inside) {
            DropPlan::Send { cmds, hint, .. } => {
                assert!(matches!(
                    cmds.as_slice(),
                    [BackendCommand::MoveNode { new_parent_id, place: Place::First, .. }] if *new_parent_id == f.folder
                ));
                assert_eq!(hint.line, "Into Folder, first");
            }
            other => panic!("expected a move, got {other:?}"),
        }
        // Outside Folder: its next sibling, after its whole subtree. `y` is
        // inside Folder now, so this takes it out.
        let outside = drop_of(&[row(a, f.y)], Some(store_row(a)), TreeSlot::After(row(a, f.folder)));
        match plan_drop(f.app, &outside) {
            DropPlan::Send { cmds, hint, .. } => {
                assert!(matches!(
                    cmds.as_slice(),
                    [BackendCommand::MoveNode { node_id, place: Place::After(anchor), .. }] if (*node_id, *anchor) == (f.y, f.folder)
                ));
                assert_eq!(hint.line, "In A, after Folder");
            }
            other => panic!("expected a move, got {other:?}"),
        }
        // `leaf` already follows Folder: outside is where it is.
        let stays = drop_of(&[row(a, f.leaf)], Some(store_row(a)), TreeSlot::After(row(a, f.folder)));
        assert!(matches!(plan_drop(f.app, &stays), DropPlan::Stays(h) if h.line == WHERE_IT_IS));

        // Below an expanded store row. A node: inside is the top of the
        // store; outside (beside the store) is refused.
        let node_inside = drop_of(&[row(a, f.x)], Some(store_row(b)), TreeSlot::First);
        assert!(matches!(plan_drop(f.app, &node_inside), DropPlan::Send { hint, .. } if hint.line == "Into B, first"));
        let node_outside = drop_of(&[row(a, f.x)], None, TreeSlot::After(store_row(b)));
        assert!(matches!(plan_drop(f.app, &node_outside), DropPlan::Refused(h) if h.line == pimble_core::plan::BESIDE_A_STORE));
        // A store: outside is after it; inside is refused.
        let store_outside = drop_of(&[store_row(a)], None, TreeSlot::After(store_row(b)));
        assert!(matches!(plan_drop(f.app, &store_outside), DropPlan::Stores { hint, .. } if hint.line == "After B"));
        let store_inside = drop_of(&[store_row(a)], Some(store_row(b)), TreeSlot::First);
        assert!(matches!(plan_drop(f.app, &store_inside), DropPlan::Refused(h) if h.line == STORES_SIDE_BY_SIDE));
    }

    /// docs/TREE_CONTRACT.md "Reordering stores": store rows move among the
    /// store rows only, and the order is the window's.
    #[test]
    fn stores_reorder_among_themselves() {
        let f = fixture();
        let [a, b, c] = f.stores;
        let store_row = |s: StoreId| format!("store_{s}");
        assert_eq!(untracked(|| f.app.store_ids.get()), vec![a, b, c]);

        // C before A: first.
        let top = drop_of(&[store_row(c)], None, TreeSlot::First);
        let plan = plan_drop(f.app, &top);
        assert_eq!((plan.verdict(), plan.hint().line.as_str()), (DropVerdict::Allowed, "First of the stores"));
        assert!(send_plan(f.app, plan));
        assert_eq!(untracked(|| f.app.store_ids.get()), vec![c, a, b]);

        // C after B: last again; A and B together after nothing changes them.
        let after_b = drop_of(&[store_row(c)], None, TreeSlot::After(store_row(b)));
        assert_eq!(plan_drop(f.app, &after_b).hint().line, "After B");
        assert!(send_plan(f.app, plan_drop(f.app, &after_b)));
        assert_eq!(untracked(|| f.app.store_ids.get()), vec![a, b, c]);
        let both = drop_of(&[store_row(a), store_row(b)], None, TreeSlot::First);
        assert!(matches!(plan_drop(f.app, &both), DropPlan::Stays(h) if h.line == WHERE_IT_IS));
        let both_last = drop_of(&[store_row(a), store_row(b)], None, TreeSlot::After(store_row(c)));
        assert_eq!(plan_drop(f.app, &both_last).hint().line, "2 stores after C");
        assert!(send_plan(f.app, plan_drop(f.app, &both_last)));
        assert_eq!(untracked(|| f.app.store_ids.get()), vec![c, a, b]);

        // Where it is (after itself, or after the one above it).
        assert!(matches!(plan_drop(f.app, &drop_of(&[store_row(a)], None, TreeSlot::After(store_row(c)))), DropPlan::Stays(_)));
        assert!(matches!(plan_drop(f.app, &drop_of(&[store_row(a)], None, TreeSlot::After(store_row(a)))), DropPlan::Stays(_)));

        // Never inside a store or a node, never with nodes.
        let refused = |d: TreeDrop, line: &str| matches!(plan_drop(f.app, &d), DropPlan::Refused(h) if h.line == line);
        assert!(refused(drop_of(&[store_row(a)], Some(store_row(b)), TreeSlot::Last), STORES_SIDE_BY_SIDE));
        assert!(refused(drop_of(&[store_row(a)], Some(row(b, NodeId::new())), TreeSlot::First), STORES_SIDE_BY_SIDE));
        assert!(refused(drop_of(&[store_row(b), row(a, f.leaf)], None, TreeSlot::First), STORES_OR_NODES));
        // A node beside the stores is still refused as before.
        assert!(refused(drop_of(&[row(a, f.leaf)], None, TreeSlot::After(store_row(a))), pimble_core::plan::BESIDE_A_STORE));
    }

    /// A store opened later takes its place in the saved order, and one the
    /// order does not name goes last.
    #[test]
    fn move_stores_puts_them_after_the_anchor() {
        let f = fixture();
        let [a, b, c] = f.stores;
        f.app.move_stores(&[a], Some(c));
        assert_eq!(untracked(|| f.app.store_ids.get()), vec![b, c, a]);
        f.app.move_stores(&[c, a], None);
        assert_eq!(untracked(|| f.app.store_ids.get()), vec![c, a, b]);
        // An anchor that is gone means the end.
        f.app.move_stores(&[c], Some(StoreId::new()));
        assert_eq!(untracked(|| f.app.store_ids.get()), vec![a, b, c]);
        // Reopened, a closed store comes back where it was saved.
        let removed = f.app.get_store_signal(a).map(|sig| untracked(|| sig.get())).unwrap();
        f.app.remove_store(a);
        f.app.upsert_store(removed);
        assert_eq!(untracked(|| f.app.store_ids.get()), vec![a, b, c]);
    }
}
