//! What the tree shows: taking a node, a children list or a `Shown` view
//! into the store's signals (bumping `tree_structure_version` only when the
//! structure changed), a store's roots changing, a store's access judged
//! again, and the follow-ups of a mount's state.

use pimble_core::{Node, NodeId, StoreId};
use rinch::prelude::*;

use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use super::show_notice;
use crate::state::{parse_tree_value, AppStore};

/// What this device may do in `store_id` changed (a role, a read-only root,
/// as the store's row or its sync answer says now), and each node carries
/// the server's judgement of itself (`Node::access`), which no document
/// says: the API asks it again for every node the window shows of the
/// store, in one call, and the answer is `Shown` like any other.
pub(super) fn rejudge(store: AppStore, store_id: StoreId) {
    let node_ids = store.shown_node_ids(store_id);
    if !node_ids.is_empty() {
        store.send(BackendCommand::Rejudge { store_id, node_ids });
    }
}

/// Whether `node`'s row would draw differently from the cached copy: the
/// row snapshots its icon, colours, share marker and access at render time
/// (the access as its menu's disabled items), and takes its chevron from
/// whether the node lists children, so (unlike a title or content change)
/// a change to any of them needs the tree rebuilt to show. A node never
/// cached before counts as changed when it carries any of them: a store's
/// root node arrives after its row was first built.
pub(super) fn row_data_changed(store: AppStore, store_id: StoreId, node: &Node) -> bool {
    let shared = node.metadata.share().is_some();
    let has_row_data = node.metadata.icon().is_some()
        || node.metadata.metanode.is_some()
        || node.metadata.color().is_some()
        || node.metadata.background().is_some()
        || shared
        || !node.access.allows_write();
    untracked(|| {
        store.node_data.with(|map| {
            map.get(&(store_id, node.id)).map_or(has_row_data, |sig| {
                sig.with(|cached| {
                    cached.metadata.icon() != node.metadata.icon()
                        || cached.metadata.color() != node.metadata.color()
                        || cached.metadata.background() != node.metadata.background()
                        || cached.metadata.share().is_some() != shared
                        || cached.metadata.metanode.as_ref().map(|m| &m.viewer) != node.metadata.metanode.as_ref().map(|m| &m.viewer)
                        || cached.access != node.access
                        || cached.children.is_empty() != node.children.is_empty()
                })
            })
        })
    })
}

/// Take one parent's children as the API holds them: `(list_store,
/// parent_id)`'s list (a mount's own address for a mount's children) is
/// set to them, each child cached under its own store, `children_store_id`.
/// What the rows need next is asked for: a store not known yet (the server
/// opened it to resolve a mount), the state of a mount not heard of yet,
/// and the children of a row left expanded on this device. Answers whether
/// the tree has to be rebuilt to show it (the list or a row's snapshot
/// changed).
pub(super) fn take_children(store: AppStore, list_store: StoreId, parent_id: NodeId, children_store_id: StoreId, children: &[Node]) -> bool {
    let child_pairs: Vec<(StoreId, NodeId)> = children.iter().map(|n| (children_store_id, n.id)).collect();
    let mut changed = untracked(|| store.get_children_signal(list_store, parent_id).map_or(true, |sig| sig.with(|held| *held != child_pairs)));
    if changed {
        store.set_children(list_store, parent_id, child_pairs);
    }

    let children_store_known = untracked(|| store.store_ids.with(|ids| ids.contains(&children_store_id)));
    if !children_store_known {
        tracing::info!("Unknown store {:?} in a children list; requesting store list", children_store_id);
        store.send(BackendCommand::ListStores);
    }

    for child in children {
        changed |= row_data_changed(store, children_store_id, child);
        store.track_mount_info(children_store_id, child);
        store.upsert_node(children_store_id, child.clone());
        // A row left expanded on this device opens again: its children are
        // fetched as soon as it is here.
        if store.is_expanded(children_store_id, child.id) && !store.has_children_loaded(children_store_id, child.id) && !store.children_on_their_way(children_store_id, child.id) {
            store.send(BackendCommand::GetChildren { store_id: children_store_id, node_id: child.id });
            store.children_asked(children_store_id, child.id);
        }
        // A mount's state is the server's (derived from its source's link),
        // asked once; its changes arrive as `MountStateChanged`.
        if child.is_mount() && store.get_mount_signal(children_store_id, child.id).is_none_or(|m| untracked(|| m.with(|m| m.mount_state.is_none()))) {
            store.send(BackendCommand::GetMountState { store_id: children_store_id, node_id: child.id });
        }
    }
    // A link-to-a-node cell being typed into offers these.
    crate::sheets::children_loaded(store, children_store_id, parent_id);
    changed
}

/// What a change did to the rows and lists of a store's tree, as the API
/// holds them now (`BackendEvent::Shown`, docs/API_CONTRACT.md wave 5): the
/// one place the tree takes another device's change, this window's own, or
/// an encrypted store's from its vault. Exactly what the change touched is
/// updated: the rows of nodes this window shows (or a pane is waiting for),
/// the lists this window has loaded (a store's shown roots always are: they
/// are its row's children) and every loaded mount of one, and the subtrees
/// that left. Nothing is fetched again.
pub(super) fn apply_view(store: AppStore, view: &pimble_api::View) {
    let store_id = view.store;
    let mut structural = false;
    let roots = store.shown_roots(store_id);
    for listed in &view.lists {
        let mut lists = Vec::new();
        if store.has_children_loaded(store_id, listed.parent) || roots.contains(&listed.parent) {
            lists.push((store_id, listed.parent));
        }
        for (mount_store, mount_node) in store.mounts_sourced_from_node(store_id, listed.parent) {
            if store.has_children_loaded(mount_store, mount_node) {
                lists.push((mount_store, mount_node));
            }
        }
        for (list_store, parent_id) in lists {
            structural |= take_children(store, list_store, parent_id, listed.children_store, &listed.children);
        }
    }
    for node in &view.nodes {
        let shown = store.get_node_signal(store_id, node.id).is_some() || !store.panes_showing(store_id, node.id).is_empty();
        if shown {
            structural |= take_node(store, store_id, node);
        }
    }
    for &gone in &view.gone {
        // Out of every loaded list it was in, whether or not that list was
        // read again (its parent may be one this window does not hold, or a
        // mount elsewhere shows it), and everything cached under it, a pane
        // showing any of it included.
        let holding: Vec<Signal<Vec<(StoreId, NodeId)>>> =
            untracked(|| store.children_of.with(|map| map.values().filter(|sig| sig.with(|c| c.contains(&(store_id, gone)))).copied().collect()));
        for sig in holding {
            sig.update(|children| children.retain(|&pair| pair != (store_id, gone)));
        }
        store.remove_subtree(store_id, gone);
        structural = true;
    }
    if structural {
        store.bump_tree_structure();
    }
}

/// Take one node as the API holds it: its row's signal, the formulas and
/// discussions waiting on it, and every pane opened on it (the first time
/// a pane's document arrives, its session starts here: the one path a
/// document is opened through). Answers whether its row's snapshot changed,
/// which needs the tree rebuilt to show.
pub(super) fn take_node(store: AppStore, store_id: StoreId, node: &Node) -> bool {
    let node_id = node.id;
    let content_bytes = &node.content;
    store.track_mount_info(store_id, node);
    let row_changed = row_data_changed(store, store_id, node);

    // Data-only: updates the per-node signal, no tree rebuild.
    store.upsert_node(store_id, node.clone());
    // A formula in a sheet may name this node (its tables, its title).
    crate::sheets::node_loaded(store);
    // An inline discussion may be waiting for it, and a notice for its name.
    crate::discussions::node_loaded(store, store_id, node_id, content_bytes);
    crate::notice_show::names_arrived(store, &[(store_id, node_id)]);

    // Every pane opened on this node, and every pane holding one of its
    // children (it may have just become a kanban, or arrived as a column's
    // parent).
    super::panes::open_in_panes(store, store_id, node);
    super::panes::rejudge_children(store, store_id, node_id);
    row_changed
}

/// What the explorer does about a change in the roots a share's replica
/// shows (`AppStore::set_store_roots`, `AppStore::end_shares`).
///
/// A share that ended (the person was removed from it, or its owner stopped
/// sharing it; Joe, 2026-09-21) leaves the explorer with one sentence in the
/// status bar per folder, naming it when the app holds its title. Everything
/// held of it is dropped from the app's caches, and the open document closes
/// if it was under it, exactly as a deleted node's does (the selection and
/// the title are cleared and the pane is hidden: `AppStore::remove_subtree`;
/// the editor itself is left alone, and the next document opened ends its
/// session as it ends any other). What is also under a share still shown (a
/// folder shared inside a shared folder) stays. Nothing is asked of the
/// backend: the files stay where they are until the replica is removed. A
/// root that is no longer among the store's at all goes the same way with
/// nothing said (the browser holds no replica: its backend says what ended
/// before it lists the store again). A root that arrived (another share of
/// the store, or one granted again) is fetched like a newly opened store's.
pub(super) fn apply_roots_change(store: AppStore, tree_state: UseTreeReturn, store_id: StoreId, change: crate::state::RootsChange) {
    if change.is_empty() {
        return;
    }
    // Before anything is dropped: the titles are in the cache being emptied.
    let sentences: Vec<String> = change
        .ended
        .iter()
        .map(|root| crate::state::share_ended_sentence(store.cached_title(store_id, *root).as_deref()))
        .collect();

    let left: Vec<NodeId> = change.ended.iter().chain(change.gone.iter()).copied().collect();
    if !left.is_empty() {
        tracing::info!("Store {:?}: {} root(s) left the explorer ({} ended)", store_id, left.len(), change.ended.len());
        // What has left with them: a node under a root that left as far as
        // the cached parent chain says, or, when the store shows nothing at
        // all any more, anything of it (a document opened from a search hit
        // has no cached chain to follow). Never a node that is also under a
        // root still shown (overlapping shares: a folder shared inside a
        // shared folder is still there, under the outer one).
        let shown = store.shown_roots(store_id);
        let has_left = |node_id: NodeId| {
            !shown.iter().any(|root| store.is_at_or_under(store_id, node_id, *root))
                && (shown.is_empty() || left.iter().any(|root| store.is_at_or_under(store_id, node_id, *root)))
        };
        // In every pane that holds one of them.
        for pane in PaneId::ALL {
            let selected = untracked(|| store.pane(pane).selected.get()).and_then(|value| parse_tree_value(&value));
            if let Some((selected_store, Some(selected_node))) = selected {
                if selected_store == store_id && has_left(selected_node) {
                    store.clear_pane(pane);
                }
            }
        }
        for root in left.iter().filter(|root| has_left(**root)) {
            store.remove_subtree(store_id, *root);
        }
    }

    for &root_id in &change.arrived {
        store.expanded.update(|e| { e.insert((store_id, root_id)); });
        store.send(BackendCommand::GetChildren { store_id, node_id: root_id });
        store.send(BackendCommand::GetNode { store_id, node_id: root_id });
        if store.is_partial_replica(store_id) {
            tree_state.controller.expand(&format!("node_{}_{}", store_id, root_id));
        }
    }

    if !sentences.is_empty() {
        show_notice(store, sentences.join(" "));
    }
    // The rows under the store row changed, and its own menu and badge
    // snapshot whether every share has ended.
    store.bump_tree_structure();
}

/// What a `Live`/`Cached` mount state means for the tree, shared by the
/// `getMountState` answer and the `MountStateChanged` notification
/// (docs/history/REMOTE_MOUNTS_CONTRACT.md "B: app side"):
///
/// - the source store may be one the app has never heard of — the server
///   opens or replicates it to resolve the mount — so ask for the store list
///   and let `StoresListed` register it;
/// - the mount's children either failed to load while the source was out of
///   reach, or are stale, so refetch them whenever the row is expanded or has
///   no children loaded yet.
///
/// `Connecting` and `Unavailable` mean there is nothing to fetch yet.
pub(super) fn apply_mount_state_followups(
    store: AppStore,
    store_id: StoreId,
    node_id: NodeId,
    state: &pimble_core::MountState,
    source_store: Option<StoreId>,
) {
    use pimble_core::MountState;
    if !matches!(state, MountState::Live | MountState::Cached { .. }) {
        return;
    }

    if let Some(source_store) = source_store {
        let known = untracked(|| store.store_ids.with(|ids| ids.contains(&source_store)));
        if !known {
            tracing::info!("Unknown mount source store {:?}; requesting store list", source_store);
            store.send(BackendCommand::ListStores);
        }
    }

    let children_loaded = store.has_children_loaded(store_id, node_id);
    if !children_loaded || store.is_expanded(store_id, node_id) {
        store.send(BackendCommand::GetChildren { store_id, node_id });
    }
}
