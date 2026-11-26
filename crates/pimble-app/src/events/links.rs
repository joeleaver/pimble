//! Opening what a link or a remembered layout names: revealing a node in the
//! tree, restoring each pane's document when its store opens, and following
//! a link's resolution (or saying why it stops).

use pimble_core::{LinkResolution, NodeId, PimbleUrl, StoreId, MAX_LINK_HOPS};
use rinch::prelude::*;

use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use super::show_notice;
use crate::state::{AppStore, LinkOpen};

/// Open the tree down to `node_id` and select it (a followed link's target,
/// docs/LINKS_CONTRACT.md): the store's row and every ancestor below the
/// root expanded, their children fetched where they are not loaded. The
/// root's own children are the store row's. Only if the node is still the
/// one selected: a later click has the last word.
pub(crate) fn reveal_in_tree(store: AppStore, tree_state: UseTreeReturn, store_id: StoreId, node_id: NodeId, ancestors: &[NodeId]) {
    let value = format!("node_{store_id}_{node_id}");
    if untracked(|| store.selected_id.get()).as_deref() != Some(value.as_str()) {
        return;
    }
    let root = store.root_node_id(store_id);
    tree_state.controller.expand(&format!("store_{store_id}"));
    for &ancestor in ancestors {
        store.expanded.update(|e| {
            e.insert((store_id, ancestor));
        });
        if Some(ancestor) != root {
            tree_state.controller.expand(&format!("node_{store_id}_{ancestor}"));
        }
        if !store.has_children_loaded(store_id, ancestor) {
            store.send(BackendCommand::GetChildren { store_id, node_id: ancestor });
        }
    }
    tree_state.controller.set_selection([value.clone()]);
    store.bump_tree_structure();
    crate::app::reveal_row(value);
}

/// The store a remembered layout names documents of is open: ask where each
/// of them is now (docs/SPLIT_VIEW_CONTRACT.md "Persistence"). `resolveLink`
/// is the question, because it says in one answer whether the note is there,
/// deleted or gone, and nothing is said to the person either way;
/// `restore_resolved` takes the answer.
pub(super) fn restore_pane_documents(store: AppStore, store_id: StoreId) {
    let mut asked: Vec<NodeId> = Vec::new();
    for pane in PaneId::ALL {
        if let Some((s, node_id)) = untracked(|| store.pane(pane).restore.get()) {
            if s == store_id && !asked.contains(&node_id) {
                asked.push(node_id);
                store.send(BackendCommand::ResolveLink { url: PimbleUrl::node(store_id, node_id), hops: 0 });
            }
        }
    }
}

/// An answer to `restore_pane_documents`, when `url` is a document a pane is
/// waiting to open: a note that is there opens in every pane that held it,
/// the way a search hit the tree has not loaded opens (selected, then
/// fetched, and `NodeLoaded` starts the session); one that is gone leaves
/// its pane empty. Answers whether the resolution was one of these.
pub(super) fn restore_resolved(store: AppStore, url: &PimbleUrl, resolution: &LinkResolution) -> bool {
    if url.anchor.is_some() {
        return false;
    }
    let waiting: Vec<PaneId> = PaneId::ALL
        .into_iter()
        .filter(|pane| untracked(|| store.pane(*pane).restore.get()) == Some((url.store, url.node)))
        .collect();
    if waiting.is_empty() {
        return false;
    }
    let live = matches!(resolution, LinkResolution::Live { store_id, node_id } if (*store_id, *node_id) == (url.store, url.node));
    for &pane in &waiting {
        store.pane(pane).restore.set(None);
        if live {
            let value = format!("node_{}_{}", url.store, url.node);
            // Where the pane's history (crate::nav) starts this session.
            store.pane(pane).history.update(|h| h.visit(&value));
            store.select_in_pane(pane, Some(value));
        }
    }
    if live {
        store.send(BackendCommand::GetNode { store_id: url.store, node_id: url.node });
        // Where the focused pane's document is in the tree, which has not
        // been opened that far, unless this device remembers how the
        // store's rows were left (`views.rs`).
        if waiting.contains(&store.focused()) && !crate::views::rows_remembered(url.store) {
            store.send(BackendCommand::GetAncestors { store_id: url.store, node_id: url.node });
        }
    }
    true
}

/// What following a link does with where `resolveLink` says it leads
/// (docs/LINKS_CONTRACT.md "Following a link"): a live node opens, with the
/// link's anchor for the editor; a chain that stopped in another store this
/// app holds is asked again of whoever holds that one (the server or, in the
/// browser, the vault client), at most `MAX_LINK_HOPS` times; anything else
/// is its one sentence in the status bar (`stop_sentence`).
pub(crate) fn follow_resolution(store: AppStore, url: &PimbleUrl, hops: usize, resolution: &LinkResolution) {
    match resolution {
        LinkResolution::Live { store_id, node_id } => {
            store.link_open.set(Some(LinkOpen {
                value: format!("node_{store_id}_{node_id}"),
                store_id: *store_id,
                node_id: *node_id,
                anchor: url.anchor.clone(),
                pane: untracked(|| store.link_from.get()),
            }));
            store.link_from.set(None);
        }
        LinkResolution::StoreNotHere { store_id, node_id }
            if *store_id != url.store && hops < MAX_LINK_HOPS && store.get_store_signal(*store_id).is_some() =>
        {
            let next = PimbleUrl { store: *store_id, node: *node_id, anchor: url.anchor.clone() };
            store.send(BackendCommand::ResolveLink { url: next, hops: hops + 1 });
        }
        other => {
            store.link_from.set(None);
            show_notice(store, stop_sentence(url, hops, other).to_string())
        }
    }
}

/// The sentence for a link that does not open. The last hop decides it: a
/// chain that moved on (a `became`) and stopped in a store this app does not
/// hold reads as no access, since the reader cannot reach where the note went;
/// only a link whose own store is not here says so. A chain still going after
/// `MAX_LINK_HOPS` is one that no longer leads anywhere.
pub(super) fn stop_sentence(url: &PimbleUrl, hops: usize, resolution: &LinkResolution) -> &'static str {
    let said = match resolution {
        LinkResolution::StoreNotHere { .. } if hops >= MAX_LINK_HOPS => LinkResolution::Missing.sentence(),
        LinkResolution::StoreNotHere { store_id, node_id } if (*store_id, *node_id) != (url.store, url.node) => {
            LinkResolution::NoAccess.sentence()
        }
        other => other.sentence(),
    };
    said.unwrap_or_default()
}
