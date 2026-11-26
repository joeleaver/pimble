//! Drops, keys and makes on the explorer's grouping rows, judged as the
//! explorer judges them (`tree_drop::plan_drop`).

use pimble_core::internal::InternalKind;
use pimble_core::{NodeId, Place};
use rinch::prelude::{untracked, DropZone, TreeDrop, TreeSlot};

use super::*;
use crate::protocol::BackendCommand;
use crate::state::test_fixtures::{a_store_with_themes, WithThemes};
use crate::tree_drop::{drag_values, plan_drop, DropPlan};

fn shown() -> (AppStore, WithThemes) {
    let (app, t) = a_store_with_themes();
    app.internal_shown[InternalKind::Theme.index()].set(true);
    untracked(|| app.build_tree_data_structural());
    (app, t)
}

fn row(t: &WithThemes, node: NodeId) -> String {
    format!("node_{}_{node}", t.store_id)
}

fn drop(dragged: &[String], parent: &str, slot: TreeSlot) -> TreeDrop {
    TreeDrop { dragged: dragged.to_vec(), parent: Some(parent.to_string()), slot, over: parent.to_string(), zone: DropZone::Inside }
}

/// The one move a plan sends: its parent and place.
fn moved_to(plan: DropPlan) -> (NodeId, Place) {
    match plan {
        DropPlan::Send { cmds, .. } => match cmds.as_slice() {
            [BackendCommand::MoveNode { new_parent_id, place, .. }] => (*new_parent_id, place.clone()),
            other => panic!("expected one move, got {other:?}"),
        },
        other => panic!("expected a move, got {other:?}"),
    }
}

fn refusal(plan: DropPlan) -> String {
    match plan {
        DropPlan::Refused(hint) => hint.line,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// docs/TREE_CONTRACT.md "Internal groups": a drop into Themes is a drop
/// into the parent the group stands for, of themes only, at the place among
/// the group's rows; beside a group row, the parent's last place.
#[test]
fn a_drop_into_a_group_is_a_drop_into_its_parent() {
    let (app, t) = shown();
    let s = t.store_id;
    let themes = internal_group_value(s, t.root, InternalKind::Theme, "");
    let folder_themes = internal_group_value(s, t.folder, InternalKind::Theme, "");

    // Book after Code, among the root's themes.
    let plan = plan_drop(app, &drop(&[row(&t, t.book)], &themes, TreeSlot::After(row(&t, t.code))));
    assert_eq!(moved_to(plan), (t.root, Place::After(t.code)));
    // Inner from the folder's group into the root's, first.
    assert_eq!(moved_to(plan_drop(app, &drop(&[row(&t, t.inner)], &themes, TreeSlot::First))), (t.root, Place::First));
    // Book into the folder's Themes.
    assert_eq!(moved_to(plan_drop(app, &drop(&[row(&t, t.book)], &folder_themes, TreeSlot::Last))).0, t.folder);
    // A theme onto an ordinary folder goes in it (and shows in its group).
    assert_eq!(moved_to(plan_drop(app, &drop(&[row(&t, t.book)], &row(&t, t.folder), TreeSlot::Last))).0, t.folder);
    // A note beside the root's Themes row: the root's last place.
    let beside = plan_drop(app, &drop(&[row(&t, t.recipes)], &format!("store_{s}"), TreeSlot::After(themes.clone())));
    assert_eq!(moved_to(beside), (t.root, Place::Last));
    // A theme beside its own group (Alt+Shift+Left out of Themes) stays:
    // the parent's list is not reordered unseen.
    let outdent = plan_drop(app, &drop(&[row(&t, t.book)], &format!("store_{s}"), TreeSlot::After(themes.clone())));
    assert!(matches!(outdent, DropPlan::Stays(_)), "{outdent:?}");
    // Beside another parent's group it moves there.
    let elsewhere = plan_drop(app, &drop(&[row(&t, t.book)], &row(&t, t.folder), TreeSlot::After(folder_themes.clone())));
    assert_eq!(moved_to(elsewhere), (t.folder, Place::Last));
}

/// A drop that would put something else in a group is refused with the
/// group's sentence, a mixed drop too (one refusal refuses all).
#[test]
fn only_a_groups_kind_goes_in_it() {
    let (app, t) = shown();
    let themes = internal_group_value(t.store_id, t.root, InternalKind::Theme, "");
    let note = row(&t, t.recipes);
    assert_eq!(refusal(plan_drop(app, &drop(std::slice::from_ref(&note), &themes, TreeSlot::Last))), "Only themes go in Themes.");
    assert_eq!(refusal(plan_drop(app, &drop(&[row(&t, t.book), note], &themes, TreeSlot::Last))), "Only themes go in Themes.");
    // Nothing goes inside a theme, in a group as anywhere.
    let into_theme = plan_drop(app, &drop(&[row(&t, t.code)], &row(&t, t.book), TreeSlot::Last));
    assert_eq!(refusal(into_theme), "Nothing goes inside a theme.");
}

/// A grouping row is no node: it is not dragged, deleted or made in, and
/// what a drop into it opens afterwards is its own row.
#[test]
fn a_group_row_is_not_a_node() {
    let (app, t) = shown();
    let s = t.store_id;
    let themes = internal_group_value(s, t.root, InternalKind::Theme, "");
    assert!(drag_values(&themes).is_empty(), "not dragged");
    assert!(crate::tree_view::deletable(app, std::slice::from_ref(&themes)).is_none(), "not deleted");
    assert!(crate::tree_view::create_parent_of_row(app, &themes).is_none(), "nothing made in it");
    assert_eq!(app.notice.get(), "Nothing is made in Themes. Node > New Theme makes a theme.");
    match plan_drop(app, &drop(&[row(&t, t.inner)], &themes, TreeSlot::Last)) {
        DropPlan::Send { expand, .. } => assert_eq!(expand.as_deref(), Some(themes.as_str())),
        other => panic!("expected a move, got {other:?}"),
    }
    assert_eq!(row_to_open(s, t.folder, "", InternalKind::Theme), internal_group_value(s, t.folder, InternalKind::Theme, ""));
}
