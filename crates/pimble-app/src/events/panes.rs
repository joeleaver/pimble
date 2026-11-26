//! What each pane shows for a node it holds: the first time a pane's
//! document arrives its session starts here (the one path a document is
//! opened through), and a pane is judged again when what decides it changed
//! elsewhere (a column's parent arrived or became a kanban, the plugins
//! changed).

use pimble_core::{Node, NodeId, StoreId};
use rinch::prelude::*;

use crate::editor::start_editing;
use crate::panes::PaneId;
use crate::state::{parse_tree_value, AppStore};

/// Every pane opened on `node` (of `store_id`) shows it as what it is now.
pub(crate) fn open_in_panes(store: AppStore, store_id: StoreId, node: &Node) {
    let node_id = node.id;
    let content_bytes = &node.content;
    crate::metanodes::ask_for_parent(store, store_id, node);
    for pane in store.panes_showing(store_id, node_id) {
        if pane == store.focused() {
            store.node_title.set(store.display_label(store_id, node_id));
        }
        // First load of the pane's document — start the editing/collab
        // session. Guard against restarting an already-active session.
        let state = store.pane(pane);
        let already_editing = untracked(|| state.active_edit.get())
            .map_or(false, |e| e.store_id == store_id && e.node_id == node_id);
        let already_boarding = untracked(|| state.board.get())
            .is_some_and(|b| b.store_id == store_id && b.node_id == node_id);
        let already_sheeting = untracked(|| state.sheet.get())
            .is_some_and(|b| b.store_id == store_id && b.node_id == node_id);
        let already_discussing = untracked(|| state.discussion.get())
            .is_some_and(|b| b.store_id == store_id && b.node_id == node_id);
        // A metanode shows its outline (docs/METANODES_CONTRACT.md), and so
        // does a node a child viewer shows (a kanban's column,
        // docs/PLUGINS_CONTRACT.md ABI 7); one that stopped being either
        // shows as its type again, below.
        let already_metanode = untracked(|| state.metanode.get())
            .is_some_and(|m| m.store_id == store_id && m.node_id == node_id);
        if crate::metanodes::shows_as_metanode(store, store_id, node) {
            if !already_metanode {
                crate::metanodes::start(store, pane, store_id, node_id);
            }
            continue;
        }
        if already_metanode {
            crate::metanodes::stop(store, pane);
        }
        if node.node_type == pimble_core::node_types::DISCUSSION && !already_discussing {
            // A discussion (docs/DISCUSSIONS_CONTRACT.md).
            state.show_editor.set(false);
            state.show_board.set(false);
            state.show_sheet.set(false);
            state.show_discussion.set(true);
            crate::discussions::start(store, pane, store_id, node_id, content_bytes);
        }
        if node.node_type == pimble_core::node_types::SHEET && !already_sheeting {
            // A sheet node (docs/SHEETS_CONTRACT.md section 1).
            state.show_editor.set(false);
            state.show_board.set(false);
            state.show_sheet.set(true);
            crate::sheets::start(store, pane, store_id, node_id, content_bytes);
        }
        if node.node_type == pimble_core::node_types::BOARD && !already_boarding {
            // A board node (docs/IMAGE_MARKUP_CONTRACT.md): the pane shows
            // its board, its editor hidden.
            state.show_editor.set(false);
            state.show_board.set(true);
            crate::boards::start(store, pane, store_id, node_id, content_bytes);
        }
        if node.node_type == pimble_core::node_types::DOCUMENT && !already_editing {
            // `open_node` could not tell this was a document (a search hit
            // the tree had not loaded), so the pane is still hidden: show
            // it with the session.
            state.show_editor.set(true);
            start_editing(store, pane, store_id, node_id, content_bytes);
        }
    }
}

/// The nodes the panes hold that the window has, once each.
fn held_in_panes(store: AppStore) -> Vec<(StoreId, Node)> {
    let mut held: Vec<(StoreId, Node)> = Vec::new();
    for pane in PaneId::ALL {
        let Some((store_id, Some(node_id))) = untracked(|| store.pane(pane).selected.get()).and_then(|v| parse_tree_value(&v)) else { continue };
        if held.iter().any(|(s, n)| *s == store_id && n.id == node_id) {
            continue;
        }
        if let Some(sig) = store.get_node_signal(store_id, node_id) {
            held.push((store_id, untracked(|| sig.get())));
        }
    }
    held
}

/// Node `parent` (of `store_id`) arrived or changed: each pane holding one of
/// its children is judged again, as a child viewer may show it now or no
/// longer.
pub(super) fn rejudge_children(store: AppStore, store_id: StoreId, parent: NodeId) {
    for (s, node) in held_in_panes(store) {
        if s == store_id && node.parent_id == Some(parent) {
            open_in_panes(store, s, &node);
        }
    }
}

/// The plugins changed: every pane is judged again, as a child viewer may
/// show its node now or no longer.
pub(crate) fn rejudge_panes(store: AppStore) {
    for (store_id, node) in held_in_panes(store) {
        open_in_panes(store, store_id, &node);
    }
}
