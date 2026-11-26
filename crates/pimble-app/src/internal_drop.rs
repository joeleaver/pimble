//! What a drop, a paste or a key means on the explorer's grouping rows
//! (docs/THEMES_CONTRACT.md section 3.1a "Grouped by kind";
//! docs/TREE_CONTRACT.md "Internal groups"). A group is no node: it shows
//! one parent's internal nodes of a kind. So a drop into Themes is a drop
//! into that parent, of themes only, and a drop beside a group row is the
//! parent's last place. Nothing is made in a group.

use pimble_core::internal::InternalKind;
use pimble_core::plan::TreeView;
use pimble_core::{NodeId, Place, StoreId};
use rinch::prelude::TreeSlot;

use crate::state::{internal_group, internal_group_value, parse_tree_value, AppStore, InternalGroup};

/// A drop whose parent row is a grouping row, in a node's terms: the row of
/// the parent it lands in, its place there, and the group row to open once
/// it has moved.
#[derive(Debug, PartialEq)]
pub(crate) struct GroupDrop {
    pub parent_row: String,
    pub place: Place,
    pub expand: String,
}

/// What a drop of `nodes` into the grouping row `parent` at `slot` means,
/// or the sentence refusing it; `None` when `parent` is no grouping row.
pub(crate) fn group_drop(store: AppStore, parent: Option<&str>, slot: &TreeSlot, nodes: &[(StoreId, NodeId)]) -> Option<Result<GroupDrop, String>> {
    let value = parent?;
    let group = internal_group(value)?;
    let view = store.view();
    if nodes.iter().any(|&(s, n)| view.internal_kind(s, n) != Some(group.kind)) {
        return Some(Err(group.kind.only_here().to_string()));
    }
    // Among the group's rows a place is a place among the parent's
    // children (the group shows them in the parent's order).
    let place = match slot {
        TreeSlot::First => Place::First,
        TreeSlot::After(value) => match parse_tree_value(value) {
            Some((_, Some(anchor))) => Place::After(anchor),
            _ => Place::Last,
        },
        TreeSlot::Last => Place::Last,
    };
    Some(Ok(GroupDrop { parent_row: group.parent_row, place, expand: value.to_string() }))
}

/// Whether a drop of `nodes` right after the grouping row in `slot` puts
/// them where they already show: each of the group's kind and a child of
/// the parent it groups (an Outdent out of Themes, or a theme dragged to
/// just below its own group). Beside the group means the parent's last
/// place, so sending it would reorder the parent's list unseen.
pub(crate) fn beside_own_group(store: AppStore, slot: &TreeSlot, nodes: &[(StoreId, NodeId)]) -> bool {
    let TreeSlot::After(value) = slot else { return false };
    let Some(group) = internal_group(value) else { return false };
    let view = store.view();
    nodes.iter().all(|&(s, n)| {
        view.internal_kind(s, n) == Some(group.kind)
            && view.parent(s, n).is_some_and(|p| lands_under(store, (group.store_id, group.parent_id), s, p))
    })
}

/// What refuses making a node in the grouping row `group` (a new node, a
/// paste, pictures).
pub(crate) fn nothing_made_in(group: &InternalGroup) -> String {
    format!("Nothing is made in {}. {}", group.kind.name(), group.kind.how_made())
}

/// The grouping row to open so the internal node of `kind` just moved
/// under `(store_id, parent_id)` is seen, at the mount level its parent's
/// children carry (`children_suffix`).
pub(crate) fn row_to_open(store_id: StoreId, parent_id: NodeId, children_suffix: &str, kind: InternalKind) -> String {
    internal_group_value(store_id, parent_id, kind, children_suffix)
}

/// Whether a node that moved into `(store_id, parent_id)` landed under the
/// row of `row`: that node's own row, or a mount of it.
pub(crate) fn lands_under(store: AppStore, row: (StoreId, NodeId), store_id: StoreId, parent_id: NodeId) -> bool {
    row == (store_id, parent_id) || store.view().mount_source(row.0, row.1) == Some((store_id, parent_id))
}

/// The grouping row of `kind` under the node row `row` (`node_...`, mount
/// path and all): the row's own node, and the mount path its children
/// carry (one level more when the row is a mount's).
pub(crate) fn group_under_row(store: AppStore, row: &str, kind: InternalKind) -> Option<String> {
    let (s, n) = match parse_tree_value(row)? {
        (s, Some(n)) => (s, n),
        (_, None) => return None,
    };
    let path = row.find('/').map_or("", |at| &row[at..]);
    let suffix = if store.view().is_mount(s, n) { format!("{path}/{s}_{n}") } else { path.to_string() };
    Some(internal_group_value(s, n, kind, &suffix))
}

#[cfg(test)]
mod tests;
