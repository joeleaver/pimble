//! Opening a node in a pane, and stepping through a pane's history.

use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::editor::{start_editing, stop_editing};
use crate::panes::PaneId;
use crate::state::{parse_tree_value, AppStore};

/// Select and open a node or store root identified by a tree value
/// (`"store_{uuid}"` / `"node_{store_uuid}_{node_uuid}"`) — the single
/// implementation behind both a tree click and a search-result click, per
/// CLAUDE.md's "one way to write node content": there is one way to open one.
///
/// It opens in the focused pane (docs/SPLIT_VIEW_CONTRACT.md "What the
/// person sees"); [`open_node_in`] names another.
pub(crate) fn open_node(store: AppStore, tree_state: UseTreeReturn, value: String) {
    open_node_in(store, tree_state, store.focused(), value);
}

/// Open a tree value in pane `pane`. The tree's selected row follows when
/// the pane has the focus, and the pane's history ([`crate::nav`]) records it.
pub(crate) fn open_node_in(store: AppStore, tree_state: UseTreeReturn, pane: PaneId, value: String) {
    store.pane(pane).history.update(|h| h.visit(&value));
    show_in_pane(store, tree_state, pane, value);
}

/// The explorer's back (or forward) arrow: the focused pane steps through
/// what it has shown, skipping anything in a store that is no longer open.
pub(super) fn step_history(store: AppStore, tree_state: UseTreeReturn, back: bool) {
    let pane = store.focused();
    let history = store.pane(pane).history;
    loop {
        let mut to = None;
        history.update(|h| to = if back { h.back() } else { h.forward() });
        let Some(value) = to else { return };
        let opens = parse_tree_value(&value).is_some_and(|(s_id, _)| store.get_store_signal(s_id).is_some());
        if opens {
            show_in_pane(store, tree_state, pane, value);
            return;
        }
        history.update(|h| h.forget(&value));
    }
}

/// Open a tree value in pane `pane` without touching its history: what both
/// an ordinary open and a step back or forward end in.
fn show_in_pane(store: AppStore, tree_state: UseTreeReturn, pane: PaneId, value: String) {
    // A mount row is a placeholder: the pane shows the node it mounts (a
    // mounted kanban is the kanban), and the explorer keeps the mount's row
    // chosen.
    if let Some((s_id, Some(n_id))) = parse_tree_value(&value) {
        if store.is_mount(s_id, n_id) {
            let source = pimble_core::plan::TreeView::mount_source(&store.view(), s_id, n_id);
            if let Some((source_store, source_node)) = source {
                show_in_pane(store, tree_state, pane, format!("node_{source_store}_{source_node}"));
                if pane == store.focused() {
                    tree_state.controller.set_selection([value.clone()]);
                    tree_state.controller.set_anchor(Some(&value));
                }
                return;
            }
        }
    }
    let focused = pane == store.focused();
    let state = store.pane(pane);
    store.select_in_pane(pane, Some(value.clone()));
    // Whatever the saved layout meant this pane for, the person chose this.
    state.restore.set(None);
    if focused {
        tree_state.controller.set_selection([value.clone()]);
        // The open row is where the next Shift run starts.
        tree_state.controller.set_anchor(Some(&value));
    }

    // Offline-first: a document opens from the cached content at once and
    // `start_editing` then reconciles the session with the server (state
    // vector in, diff out), so a cache that is behind the server (this
    // window's own earlier session, another window, a replica) is merged
    // in place rather than reloaded. A node the cache has never seen is
    // fetched; `NodeLoaded` starts the session then.
    let parsed = parse_tree_value(&value);
    let mut is_board = false;
    let mut is_sheet = false;
    let mut is_discussion = false;
    let mut is_metanode = false;
    let (title_opt, content_bytes_opt, is_document, sel_ids) = if let Some((s_id, node_id_opt)) = parsed {
        if let Some(n_id) = node_id_opt {
            let result = store.get_node_signal(s_id, n_id).map(|sig| {
                sig.with(|node| {
                    let label = if !node.metadata.title.is_empty() {
                        node.metadata.title.clone()
                    } else {
                        "Untitled".to_string()
                    };
                    let is_doc = node.node_type == pimble_core::node_types::DOCUMENT;
                    let board = node.node_type == pimble_core::node_types::BOARD;
                    let sheet = node.node_type == pimble_core::node_types::SHEET;
                    let discussion = node.node_type == pimble_core::node_types::DISCUSSION;
                    let metanode = crate::metanodes::is_metanode(node);
                    (label, node.content.clone(), is_doc, board, sheet, discussion, metanode)
                })
            });
            match result {
                // A metanode shows its outline whatever its type
                // (docs/METANODES_CONTRACT.md section 1.4).
                Some((label, _, _, _, _, _, true)) => {
                    is_metanode = true;
                    (Some(label), None, false, Some((s_id, n_id)))
                }
                Some((label, content, is_doc, board, sheet, discussion, false)) => {
                    is_board = board;
                    is_sheet = sheet;
                    is_discussion = discussion;
                    (Some(label), Some(content), is_doc, Some((s_id, n_id)))
                }
                None => {
                    store.send(BackendCommand::GetNode { store_id: s_id, node_id: n_id });
                    (None, None, false, None)
                }
            }
        } else {
            let title = store.get_store_signal(s_id)
                .map(|sig| sig.with(|s| s.name.clone()));
            match title {
                Some(t) => (Some(t), Some(Vec::new()), false, None),
                None => (None, None, false, None),
            }
        }
    } else {
        (None, None, false, None)
    };

    state.show_editor.set(is_document);
    state.show_board.set(is_board);
    state.show_sheet.set(is_sheet);
    state.show_discussion.set(is_discussion);

    if let Some(title) = title_opt.filter(|_| focused) {
        store.node_title.set(title);
    }

    if is_metanode {
        if let Some((s_id, n_id)) = sel_ids {
            crate::metanodes::start(store, pane, s_id, n_id);
        }
        store.editor_dirty.set(false);
        return;
    }
    // Whatever metanode the pane showed, it shows this instead.
    crate::metanodes::stop(store, pane);

    if is_document {
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            start_editing(store, pane, s_id, n_id, &content_bytes);
        }
    } else if is_board {
        // A board node (docs/IMAGE_MARKUP_CONTRACT.md section 1): the pane
        // shows its board, the editor stays mounted and hidden.
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            crate::boards::start(store, pane, s_id, n_id, &content_bytes);
        }
    } else if is_sheet {
        // A sheet node (docs/SHEETS_CONTRACT.md section 1): the pane shows
        // its tables, the editor stays mounted and hidden.
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            crate::sheets::start(store, pane, s_id, n_id, &content_bytes);
        }
    } else if is_discussion {
        // A discussion (docs/DISCUSSIONS_CONTRACT.md): the pane shows its
        // thread, the editor stays mounted and hidden.
        let content_bytes = content_bytes_opt.unwrap_or_default();
        if let Some((s_id, n_id)) = sel_ids {
            crate::discussions::start(store, pane, s_id, n_id, &content_bytes);
        }
    } else {
        stop_editing(store, pane);
    }
    store.editor_dirty.set(false);
}
