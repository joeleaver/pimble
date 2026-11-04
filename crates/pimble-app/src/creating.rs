//! What a person can make, and making it (docs/MENUS_CONTRACT.md principle 1):
//! one list read by the explorer's "+" and by the row menus' New, so a kind a
//! plugin adds shows up everywhere at once. Every kind is made under a parent
//! judged by `plan::create_parent`, at a `Place`, through the code path that
//! kind always had.

use pimble_core::{NodeId, Place, StoreId};
use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::protocol::BackendCommand;
use crate::state::{parse_tree_value, AppStore};

/// A kind of node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    Text,
    Canvas,
    Sheet,
    Discussion,
    /// A plugin's recipe ("Kanban").
    Recipe(pimble_plugin_abi::RecipeDecl),
    /// A canvas per picture chosen from files.
    CanvasFromPicture,
}

/// One entry of the list: what it makes, what it is called, its icon.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NewItem {
    pub kind: Kind,
    pub label: String,
    pub icon: TablerIcon,
    /// A line above it (the plugins' kinds, then the pictures, are groups).
    pub divided: bool,
}

/// What can be made, in order, tracked on the plugins' recipes: Text, Canvas,
/// Sheet, Discussion, each plugin recipe, Canvas from Picture...
pub(crate) fn kinds(store: AppStore) -> Vec<NewItem> {
    let item = |kind, label: &str, icon, divided| NewItem { kind, label: label.to_string(), icon, divided };
    let mut items = vec![
        item(Kind::Text, "Text", TablerIcon::FileText, false),
        item(Kind::Canvas, "Canvas", TablerIcon::Chalkboard, false),
        item(Kind::Sheet, "Sheet", TablerIcon::Table, false),
        item(Kind::Discussion, "Discussion", TablerIcon::Messages, false),
    ];
    for (i, r) in store.recipes.get().into_iter().enumerate() {
        let icon = r.recipe.icon.as_deref().and_then(crate::appearance::icon_by_name).unwrap_or(TablerIcon::LayoutKanban);
        // A recipe's label is "New Kanban"; in a list headed New, "Kanban".
        let label = r.recipe.label.strip_prefix("New ").unwrap_or(&r.recipe.label).to_string();
        items.push(NewItem { kind: Kind::Recipe(r.recipe), label, icon, divided: i == 0 });
    }
    items.push(item(Kind::CanvasFromPicture, "Canvas from Picture...", TablerIcon::Photo, true));
    items
}

/// Make `kind` under `parent` of `store_id`, at `place`. A text node made at
/// `rename_row`'s level is named in place as soon as its row is drawn.
pub(crate) fn make(store: AppStore, kind: &Kind, store_id: StoreId, parent: NodeId, place: Place, rename_suffix: Option<String>) {
    match kind {
        Kind::Text => {
            if let Some(suffix) = rename_suffix {
                crate::state::set_pending_create(store_id, parent, suffix);
            }
            store.send(BackendCommand::CreateNode { store_id, parent_id: Some(parent), title: String::new(), init: None, place });
        }
        Kind::Canvas => crate::boards::new_whiteboard(store, store_id, parent, place),
        Kind::Sheet => crate::sheets::new_sheet(store, store_id, parent, place),
        Kind::Discussion => crate::discussions::new_discussion(store, store_id, parent, place),
        Kind::Recipe(recipe) => crate::plugins::make(store, store_id, parent, recipe.clone(), place),
        Kind::CanvasFromPicture => crate::boards::pick_images(store, crate::boards::Dest::Under(store_id, parent)),
    }
}

/// Make `kind` inside the row `row` (a store row: under its root; a mount:
/// in the node it mounts), last: a row menu's New.
pub(crate) fn make_in_row(store: AppStore, kind: &Kind, row: &str) {
    if let Some((s, parent)) = crate::tree_view::create_parent_of_row(store, row) {
        make(store, kind, s, parent, Place::Last, None);
    }
}

/// Where the explorer's "+" makes a node (decision 1): beside the selected
/// row, after it; under the root when a store row is selected; under the first
/// store's root when nothing is. `None` (after saying why, when there is a
/// reason) when there is nowhere to make one.
pub(crate) fn where_plus(store: AppStore) -> Option<(StoreId, NodeId, Place, String)> {
    let selected = untracked(|| store.selected_id.get());
    let view = store.view();
    if let Some(value) = selected {
        match parse_tree_value(&value) {
            // A node: its parent (a node seen through a mount: its parent in the
            // mounted store), after it.
            Some((s, Some(n))) => {
                let parent = pimble_core::plan::TreeView::parent(&view, s, n);
                let made = match parent {
                    Some(p) => pimble_core::plan::create_parent(&view, s, Some(p)),
                    None => pimble_core::plan::create_parent(&view, s, None),
                };
                return match made {
                    Ok((s, parent)) => Some((s, parent, Place::After(n), suffix(&value))),
                    Err(refusal) => {
                        crate::events::show_notice(store, refusal.sentence().to_string());
                        None
                    }
                };
            }
            Some((_, None)) => {
                return crate::tree_view::create_parent_of_row(store, &value).map(|(s, root)| (s, root, Place::Last, String::new()));
            }
            None => {}
        }
    }
    let first = untracked(|| store.store_ids.with(|ids| ids.first().copied()))?;
    pimble_core::plan::create_parent(&view, first, None).ok().map(|(s, root)| (s, root, Place::Last, String::new()))
}

/// The explorer's "+": make `kind` where [`where_plus`] says.
pub(crate) fn make_from_plus(store: AppStore, kind: &Kind) -> bool {
    match where_plus(store) {
        Some((s, parent, place, suffix)) => {
            make(store, kind, s, parent, place, Some(suffix));
            true
        }
        None => false,
    }
}

/// The mount-path suffix of a tree value (what follows the node's own
/// address), which a sibling at the same level shares.
fn suffix(value: &str) -> String {
    value.find('/').map(|at| value[at..].to_string()).unwrap_or_default()
}
