//! The Theme picker's list (docs/THEMES_CONTRACT.md sections 3.1, 6 and 9):
//! what each row is, the groups of this store's own themes and of other open
//! stores' themes, and what choosing, previewing, editing and customizing a
//! row does. The built-ins are drawn by the picker itself, since they never
//! change.

use rinch::prelude::*;

use pimble_core::{NodeId, StoreAccess, StoreId};
use pimble_themes::{Drawn, Theme};

use crate::protocol::{BackendCommand, ThemeCommand};
use crate::state::AppStore;
use crate::themes;

/// A row of the list.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Choice {
    /// "Use the parent's": no theme of the node's own.
    Parent,
    /// A built-in, or a theme node of the node's own store, by id.
    Theme(String),
    /// A theme node of another open store: copied in when chosen.
    Other { store_id: StoreId, node_id: NodeId },
}

/// One theme row of a group that changes as themes do.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Row {
    /// What keys the row in its list: everything it shows, as a row whose
    /// key is unchanged is not drawn again (a rename, or a copy made of an
    /// other store's theme, must draw it anew).
    pub key: String,
    pub choice: Choice,
    pub name: String,
    /// A second line ("in Family", "copied here").
    pub hint: Option<String>,
    /// The theme node "Edit Theme..." opens, for a theme of this store.
    pub node: Option<(StoreId, NodeId)>,
}

/// `store_id`'s own themes, tracked.
pub(super) fn store_rows(store: AppStore, store_id: StoreId) -> Vec<Row> {
    store.store_themes.with(|all| {
        all.get(&store_id)
            .map(|listed| {
                listed
                    .iter()
                    .map(|t| Row {
                        key: format!("{}/{}", t.node, t.name),
                        choice: Choice::Theme(t.node.to_string()),
                        name: t.name.clone(),
                        hint: None,
                        node: Some((store_id, t.node)),
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The themes of every other open store, tracked, each saying which store it
/// is in and whether a copy of it is here already (choosing it then applies
/// that copy, section 6).
pub(super) fn other_rows(store: AppStore, store_id: StoreId) -> Vec<Row> {
    let mut rows = Vec::new();
    let all = store.store_themes.get();
    let here = all.get(&store_id).cloned().unwrap_or_default();
    let mut stores: Vec<(StoreId, String)> = all
        .keys()
        .filter(|id| **id != store_id)
        .filter_map(|id| Some((*id, store.get_store_signal(*id)?.with(|s| s.name.clone()))))
        .collect();
    stores.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    for (other, store_name) in stores {
        for t in all.get(&other).into_iter().flatten() {
            let copied = here.iter().any(|mine| crate::commands::copy_of(&mine.stored, other, t.node));
            let hint = if copied { format!("in {store_name}, copied here") } else { format!("in {store_name}") };
            rows.push(Row {
                key: format!("{other}/{}/{}/{hint}", t.node, t.name),
                choice: Choice::Other { store_id: other, node_id: t.node },
                name: t.name.clone(),
                hint: Some(hint),
                node: None,
            });
        }
    }
    rows
}

/// The themes `choice` resolves through and its id among them, tracked: a
/// theme of another store resolves in that store.
fn source_of(store: AppStore, store_id: StoreId, choice: &Choice) -> (Vec<Theme>, Option<String>) {
    match choice {
        Choice::Parent => (Vec::new(), None),
        Choice::Theme(id) => (themes::of_store(store, store_id), Some(id.clone())),
        Choice::Other { store_id: other, node_id } => (themes::of_store(store, *other), Some(node_id.to_string())),
    }
}

/// What `choice` draws in the app's scheme, tracked.
pub(super) fn drawn_of(store: AppStore, store_id: StoreId, choice: &Choice, dark: bool) -> Drawn {
    let (list, id) = source_of(store, store_id, choice);
    id.map(|id| themes::drawn_for(&list, &id, dark)).unwrap_or_default()
}

/// `choice`'s name, tracked.
pub(super) fn name_of(store: AppStore, store_id: StoreId, choice: &Choice) -> String {
    let (list, id) = source_of(store, store_id, choice);
    id.map(|id| themes::theme_name(&list, &id)).unwrap_or_default()
}

/// Whether `node` may be changed here; says why not when it may not.
fn may_write(store: AppStore, (store_id, node_id): (StoreId, NodeId)) -> bool {
    // A native control cannot always be greyed out in time, so the check the
    // rows draw with is made again here.
    let writable = untracked(|| store.node_access(store_id, node_id).allows_write());
    if !writable {
        crate::events::show_notice(store, StoreAccess::READ_ONLY_REFUSAL.to_string());
    }
    writable
}

/// Apply `choice` to `target`: a theme of this store or a built-in by its
/// id, "Use the parent's" by clearing the key, and another store's theme by
/// copying it in first (or reusing the copy made before).
pub(super) fn choose(store: AppStore, target: (StoreId, NodeId), choice: &Choice) {
    if !may_write(store, target) {
        return;
    }
    let (store_id, node_id) = target;
    let theme = match choice {
        Choice::Parent => None,
        Choice::Theme(id) => Some(id.clone()),
        Choice::Other { store_id: from_store, node_id: from_node } => {
            let copy = ThemeCommand::Copy { from_store: *from_store, from_node: *from_node, to_store: store_id, apply_to: Some(node_id), customize: false };
            store.send(BackendCommand::Themes(copy));
            return;
        }
    };
    store.send(BackendCommand::SetNodeAppearance { store_id, node_id, icon: None, color: None, background: None, tags: None, theme: Some(theme) });
}

/// "Customize...": a new theme of `store_id` based on `choice` (another
/// store's theme copied in first) that sets nothing, applied to `apply_to`
/// and opened in the editor (section 3.1).
pub(super) fn customize(store: AppStore, store_id: StoreId, apply_to: NodeId, choice: &Choice) {
    if !may_write(store, (store_id, apply_to)) {
        return;
    }
    let command = match choice {
        Choice::Parent => return,
        Choice::Theme(id) => {
            let name = crate::commands::customized_name(&name_of(store, store_id, choice));
            ThemeCommand::New { store_id, name, base: Some(id.clone()), apply_to: Some(apply_to) }
        }
        Choice::Other { store_id: from_store, node_id: from_node } => {
            ThemeCommand::Copy { from_store: *from_store, from_node: *from_node, to_store: store_id, apply_to: Some(apply_to), customize: true }
        }
    };
    store.send(BackendCommand::Themes(command));
    store.theme_picker_node.set(None);
}

/// "Edit Theme...": the theme node in the focused pane, the picker closed.
pub(super) fn edit(store: AppStore, (store_id, node_id): (StoreId, NodeId)) {
    store.theme_picker_node.set(None);
    themes::open_theme_node(store, store_id, node_id);
}
