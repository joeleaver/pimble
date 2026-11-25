//! The document a pane holds and what is shown of it, read with tracking: its
//! canonical pair, its title, icon and people, and what this device may do with it.

use rinch_tabler_icons::TablerIcon;

use crate::appearance::display_color;
use crate::panes::PaneId;
use crate::state::{display_label_from_node, parse_tree_value, AppStore};

/// The node pane `pane` shows as a document, tracked: its canonical pair.
pub(crate) fn pane_document(store: AppStore, pane: PaneId) -> Option<(pimble_core::StoreId, pimble_core::NodeId)> {
    let state = store.pane(pane);
    if !state.show_editor.get() && !state.show_board.get() && !state.show_sheet.get() && !state.show_discussion.get() && !state.show_metanode.get() && !state.show_theme.get() {
        return None;
    }
    match parse_tree_value(&state.selected.get()?) {
        Some((store_id, Some(node_id))) => Some((store_id, node_id)),
        _ => None,
    }
}

/// Who else is in pane `pane`'s document, tracked: the first four, and how
/// many more there are.
pub(super) fn pane_people(store: AppStore, pane: PaneId) -> (Vec<crate::presence::Person>, usize) {
    let Some((store_id, node_id)) = pane_document(store, pane) else { return (Vec::new(), 0) };
    let mut people = crate::presence::people_in(store, store_id, node_id);
    let more = people.len().saturating_sub(4);
    people.truncate(4);
    (people, more)
}

/// The title in pane `pane`'s strip, tracked: the label its tree row shows,
/// typing included.
pub(crate) fn pane_title(store: AppStore, pane: PaneId) -> String {
    let Some((store_id, node_id)) = pane_document(store, pane) else { return String::new() };
    if let Some(live) = store.live_label_signal((store_id, node_id)).get() {
        return live;
    }
    // One signal read at a time: a tracked read inside another's `with`
    // panics (see `editor_read_only` below).
    let node = store.node_data.with(|map| map.get(&(store_id, node_id)).copied());
    node.map(|sig| sig.with(display_label_from_node)).unwrap_or_else(|| "Untitled".to_string())
}

/// The icon in pane `pane`'s strip and its colour, tracked: the node's own
/// when it has them (docs "Tree appearance"), a document's otherwise. Empty
/// for a pane with no document.
pub(crate) fn pane_icon(store: AppStore, pane: PaneId) -> Vec<(String, String)> {
    let Some((store_id, node_id)) = pane_document(store, pane) else { return Vec::new() };
    let node = store.node_data.with(|map| map.get(&(store_id, node_id)).copied());
    // The icon and color the node's explorer row shows: a metanode with a
    // query is violet unless the person gave it a color.
    let dark = store.dark_mode.get();
    let (icon, color) = node
        .map(|sig| {
            sig.with(|n| {
                let query = n.metadata.metanode.as_ref().is_some_and(|m| m.is_query());
                let shown_by_a_viewer = crate::metanodes::is_metanode(n) || store.child_viewer(store_id, n).is_some();
                let icon = (n.metadata.icon().is_some() || query || shown_by_a_viewer).then(|| store.node_icon(store_id, n));
                let color = n.metadata.color().map(|hex| display_color(hex, dark)).or_else(|| query.then(|| pimble_ui::appearance::gathered_violet(dark).to_string()));
                (icon, color)
            })
        })
        .unwrap_or_default();
    let fallback = if store.pane(pane).show_discussion.get() {
        TablerIcon::Messages
    } else if store.pane(pane).show_sheet.get() {
        TablerIcon::Table
    } else if store.pane(pane).show_theme.get() {
        TablerIcon::Palette
    } else if store.pane(pane).show_board.get() && !store.pane(pane).markup.get() {
        TablerIcon::Chalkboard
    } else {
        TablerIcon::FileText
    };
    vec![(
        icon.unwrap_or_else(|| fallback.name().to_string()),
        color.unwrap_or_default(),
    )]
}

/// Whether the document in pane `pane` is one this device may only read
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"): its store is held to
/// read, or the server judged this node so (a reader's root in a store where
/// other roots are edited) — `AppStore::node_access`, read here with
/// tracking. The pane's `active_edit` holds the node's CANONICAL pair — where
/// the edit would be written, even when the node was reached through a mount
/// — so that is what decides. Reactive on the pane's node, on its store's
/// own signal and on the node's, so the pane follows an access that arrives
/// or changes. The pane shows a line saying so, the toolbar greys out while
/// the pane is focused, and the editor is locked so typing does nothing, in
/// every pane that holds the document.
///
/// The store's own signal comes out of the registry first and is read after
/// that borrow is released: rinch keeps every signal in one `RefCell`, and a
/// *tracked* read takes it mutably to record the subscription — so reading
/// one signal inside another's `with` panics. (`AppStore`'s own helpers nest
/// freely because they are `untracked`.)
pub(super) fn editor_read_only(store: AppStore, pane: PaneId) -> bool {
    !pane_access(store, pane).allows_write()
}

/// Whether the note the pane shows is out of step on this device, and why.
pub(super) fn pane_out_of_step(store: AppStore, pane: PaneId) -> Option<pimble_core::OutOfStep> {
    let state = store.pane(pane);
    let shown = state.active_edit.get().or_else(|| state.board.get()).or_else(|| state.sheet.get()).or_else(|| state.discussion.get())?;
    if !(state.show_editor.get() || state.show_board.get() || state.show_sheet.get() || state.show_discussion.get()) {
        return None;
    }
    store.out_of_step_shown(shown.store_id, shown.node_id)
}

/// What this device may change of the pane's document: its store's access,
/// narrowed by the node's own.
pub(super) fn pane_access(store: AppStore, pane: PaneId) -> pimble_core::StoreAccess {
    let state = store.pane(pane);
    let Some(active) = state.active_edit.get().or_else(|| state.board.get()).or_else(|| state.sheet.get()).or_else(|| state.discussion.get()) else {
        return pimble_core::StoreAccess::Full;
    };
    let sig = store.store_data.with(|map| map.get(&active.store_id).copied());
    let node_sig = store.node_data.with(|map| map.get(&(active.store_id, active.node_id)).copied());
    let node = node_sig.map_or(pimble_core::StoreAccess::Full, |sig| sig.with(|n| n.access));
    // A discussion node answers for itself: the store's `Comment` lets its
    // commenter write it.
    if !node.allows_write() || node_sig.is_some_and(|sig| sig.with(|n| n.node_type == pimble_core::node_types::DISCUSSION)) {
        return node;
    }
    sig.map_or(pimble_core::StoreAccess::Full, |sig| sig.with(|s| s.access))
}
