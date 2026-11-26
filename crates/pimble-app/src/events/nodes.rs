//! Node events: children loaded, a node moved, created or transplanted, the
//! deleted list, and a store's change notifications (`RemoteStoreChange`).

use pimble_core::{DeletedNode, Node, NodeId, StoreId};
use rinch::prelude::*;

use crate::protocol::BackendCommand;
use crate::state::{parse_tree_value, share_state_text, take_last_drop_target_value, AppStore};

use super::show_notice;
use super::view::{apply_mount_state_followups, apply_roots_change, take_children};

/// `BackendEvent::ChildrenLoaded`.
pub(super) fn children_loaded(
    store: AppStore,
    store_id: &StoreId,
    parent_id: &NodeId,
    children_store_id: &StoreId,
    children: &[Node],
) {
    tracing::info!("Children loaded for {:?}: {} nodes (in store {:?})", parent_id, children.len(), children_store_id);
    // Read from the documents the API holds, so it is current:
    // each child is taken as it is, keyed by its own canonical
    // store (the mount's source store when `parent_id` is a
    // mount point, `store_id` otherwise, decision 1).
    take_children(store, *store_id, *parent_id, *children_store_id, children);
    store.children_arrived(*store_id, *parent_id);
    // A query metanode's row shows what it finds after these.
    crate::metanodes::children_loaded(store, *store_id, *parent_id);
    store.bump_tree_structure();
}

/// `BackendEvent::NodeMoved`.
pub(super) fn node_moved(
    store: AppStore,
    tree_state: UseTreeReturn,
    store_id: &StoreId,
    node_id: &NodeId,
    old_parent_id: &NodeId,
    new_parent_id: &NodeId,
) {
    tracing::info!("Node moved: {:?} from {:?} to {:?}", node_id, old_parent_id, new_parent_id);
    // The lists it moved between were shown just before this
    // answer (`Shown`). What is left is to open the new parent so
    // the moved node is visible. `MoveNode` is only ever sent from
    // a drag-and-drop or the keyboard, whose drop records the
    // exact (possibly mount-path-qualified) tree value it dropped
    // onto: that, not an unqualified value that would not match a
    // target reached only through a mount (decision 7).
    store.expanded.update(|e| { e.insert((*store_id, *new_parent_id)); });
    let is_root = store.root_node_id(*store_id)
        .map_or(false, |rid| rid == *new_parent_id);
    let dropped_value = take_last_drop_target_value();
    if !is_root {
        let expand_value = dropped_value
            .filter(|v| parse_tree_value(v).map_or(false, |(sid, nid)| sid == *store_id && nid == Some(*new_parent_id)))
            .unwrap_or_else(|| format!("node_{}_{}", store_id, new_parent_id));
        tree_state.controller.expand(&expand_value);
    }
    if !store.has_children_loaded(*store_id, *new_parent_id) {
        // Opened for the first time: its list is loaded once.
        store.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *new_parent_id });
    }
    store.bump_tree_structure();
}

/// `BackendEvent::NodeCreated`.
pub(super) fn node_created(
    store: AppStore,
    tree_state: UseTreeReturn,
    store_id: &StoreId,
    parent_id: &Option<NodeId>,
    node_id: &NodeId,
    request: &Option<u64>,
) {
    if let Some(request) = request {
        crate::text_boards::node_created(store, *request, *store_id, *node_id);
        crate::sheets::in_text::node_created(store, *request, *store_id, *node_id);
        crate::sheets::node_created(store, *request, *store_id, *node_id);
        crate::discussions::created(store, *request, *store_id, *node_id, None);
    }
    tracing::info!("Node created: {:?}/{:?} under {:?}", store_id, node_id, parent_id);
    let parent = parent_id.unwrap_or_else(|| {
        store.root_node_id(*store_id).unwrap()
    });
    // A node the explorer made (a text or a query metanode): the cursor
    // goes to it, it opens in the focused pane (a metanode shows its
    // setup), and its row starts renaming once drawn. Opening leaves the
    // keyboard where it is, so the rename box keeps it; Enter there hands
    // it to the note (`app::explorer::start_rename`).
    if let Some(value) = crate::state::take_pending_create(*store_id, parent, *node_id) {
        tree_state.controller.set_cursor(&value);
        tree_state.controller.set_selection([value.clone()]);
        crate::app::open_node(store, tree_state, value);
    }
    // The list it went into was shown just before this answer
    // (`Shown`) when it is loaded; one never loaded is loaded
    // now, so the new row is there to move the cursor to.
    if !store.has_children_loaded(*store_id, parent) && !store.shown_roots(*store_id).contains(&parent) {
        store.send(BackendCommand::GetChildren { store_id: *store_id, node_id: parent });
    }
}

/// What a `BackendEvent::NodeTransplanted` says, borrowed from it (its old
/// parent is not read here).
pub(super) struct Transplanted<'a> {
    pub from_store_id: &'a StoreId,
    pub old_node_id: &'a NodeId,
    pub to_store_id: &'a StoreId,
    pub node_id: &'a NodeId,
    pub new_parent_id: &'a NodeId,
    pub title: &'a String,
    pub left_shares: &'a Vec<pimble_core::LeftShare>,
}

/// `BackendEvent::NodeTransplanted`.
pub(super) fn node_transplanted(store: AppStore, tree_state: UseTreeReturn, transplanted: Transplanted<'_>) {
    let Transplanted { from_store_id, old_node_id, to_store_id, node_id, new_parent_id, title, left_shares } = transplanted;
    tracing::info!(
        "Node {:?}/{:?} transplanted to {:?}/{:?} under {:?}",
        from_store_id, old_node_id, to_store_id, node_id, new_parent_id
    );

    // Both stores' lists were shown just before this answer
    // (`Shown`): the old id gone with everything under it, the
    // new one where it landed. A list there never loaded is
    // loaded now, so the person sees where it landed.
    if !store.has_children_loaded(*to_store_id, *new_parent_id) && !store.shown_roots(*to_store_id).contains(new_parent_id) {
        store.send(BackendCommand::GetChildren { store_id: *to_store_id, node_id: *new_parent_id });
    }

    // Each pane's editor follows the node it had open to its new
    // id: a fresh collab session on the new document, the old one
    // closed. Selecting first and then asking for the node is
    // what makes `NodeLoaded`'s own "selected and not already
    // editing" check start that session once the answer lands —
    // the one path a document is ever opened through.
    let was_open = store.panes_editing(*from_store_id, *old_node_id);
    for &pane in &was_open {
        let value = format!("node_{}_{}", to_store_id, node_id);
        store.select_in_pane(pane, Some(value.clone()));
        store.pane(pane).show_editor.set(true);
        if pane == store.focused() {
            tree_state.controller.set_selection([value.clone()]);
            store.node_title.set(title.clone());
        }
    }
    if !was_open.is_empty() {
        store.send(BackendCommand::GetNode { store_id: *to_store_id, node_id: *node_id });
    }

    // The nearest share it left, if any: one line for the person
    // who moved it (docs/MOVE_CONTRACT.md "Seeing and undoing
    // what was removed"). A plain move between stores, with no
    // share left, says nothing.
    if let Some(nearest) = left_shares.first() {
        show_notice(
            store,
            format!(
                "\"{}\" was moved out of \"{}\". The people it is shared with see it as deleted and can put it back.",
                title, nearest.name
            ),
        );
    }

    store.bump_tree_structure();
}

/// `BackendEvent::DeletedListed`.
pub(super) fn deleted_listed(store: AppStore, store_id: &StoreId, nodes: &[DeletedNode]) {
    tracing::info!("Store {:?}: {} recently deleted", store_id, nodes.len());
    // A late answer for a store the modal has moved on from (or
    // closed) names nothing to fill.
    if untracked(|| store.deleted_modal_store.get()) == Some(*store_id) {
        store.deleted_modal_nodes.set(nodes.iter().map(crate::state::deleted_row).collect());
        store.deleted_modal_pending.set(false);
    }
}

/// `BackendEvent::RemoteStoreChange`.
pub(super) fn store_changed(
    store: AppStore,
    tree_state: UseTreeReturn,
    store_id: &StoreId,
    change_kind: &pimble_rpc::StoreChangeKind,
    source_client_id: &Option<String>,
) {
    // Whoever made it, it may change what points at the node
    // the Properties panel shows.
    crate::properties::store_changed(store);
    // Skip our own echoes
    let my_id = untracked(|| store.client_id.get());
    if let Some(source) = source_client_id {
        if !my_id.is_empty() && source == &my_id {
            return;
        }
    }
    // A note a notice named may be named differently now.
    match change_kind {
        pimble_rpc::StoreChangeKind::MetadataUpdated { node_id } => {
            crate::inbox::note_changed(store, *store_id, *node_id, crate::inbox::NoteChange::Metadata);
        }
        pimble_rpc::StoreChangeKind::ContentUpdated { node_id } => {
            crate::inbox::note_changed(store, *store_id, *node_id, crate::inbox::NoteChange::Text);
        }
        _ => {}
    }
    // An index's progress is said several times a second while it
    // is built: not a line each.
    if !matches!(change_kind, pimble_rpc::StoreChangeKind::IndexStateChanged { .. }) {
        tracing::info!("Remote store change: {:?} - {:?}", store_id, change_kind);
    }
    use pimble_rpc::StoreChangeKind;
    match change_kind {
        // What these did to the tree arrives as `Shown`, read
        // from the documents the API merged them into.
        StoreChangeKind::NodeCreated { .. }
        | StoreChangeKind::NodeDeleted { .. }
        | StoreChangeKind::NodeMoved { .. }
        | StoreChangeKind::MetadataUpdated { .. }
        | StoreChangeKind::ContentUpdated { .. }
        | StoreChangeKind::TreeStructure { .. } => {}
        StoreChangeKind::AttachmentChanged { node_id, host_id, live } => {
            crate::discussions::attachment_changed(store, *store_id, *host_id, *node_id, *live);
        }
        StoreChangeKind::SyncStateChanged { state } => {
            tracing::info!("Sync state of {:?}: {:?}", store_id, state);
            // Carries only the state — keep whatever remote endpoint
            // is already known (decision 6, docs/SYNC_CONTRACT.md).
            store.update_sync_state(*store_id, state.clone());
            // A link reads how the account holds the store each
            // time it connects, and connects again when the grant
            // changes (a role the owner changed, a share stopped):
            // what this device may change here can differ on the
            // far side of any transition, and this notification
            // carries only the state. Ask for the rest; the answer
            // (`StoreSyncChanged`) refetches what it has to. Not
            // on `Syncing`, which a link announces before it has
            // read anything: the `Synced` or `Offline` that
            // follows is the one that knows.
            if !matches!(state, pimble_core::SyncState::Syncing) {
                store.send(BackendCommand::GetStoreSync { store_id: *store_id });
            }
        }
        StoreChangeKind::ShareStateChanged { node_id, state } => {
            // What the share's link is doing, derived by the
            // server and never forwarded by a link. Only the
            // Share modal shows it, and only for the node it is
            // open for (docs/NODE_DOCUMENT_CONTRACT.md section 5).
            tracing::info!("Share state of {:?}/{:?}: {:?}", store_id, node_id, state);
            if untracked(|| store.share_modal_node.get()) == Some((*store_id, *node_id)) {
                store.share_modal_state.set(share_state_text(state).to_string());
            }
        }
        StoreChangeKind::SharesEnded { node_ids } => {
            // The replica's link has just learned that the
            // account no longer holds these shares (a removal, a
            // share its owner stopped). Derived by the server,
            // never forwarded by a link. The folders leave the
            // explorer with a sentence saying so; the files stay
            // until the replica is removed.
            tracing::info!("Shares of {:?} ended: {:?}", store_id, node_ids);
            let roots_change = store.end_shares(*store_id, node_ids);
            apply_roots_change(store, tree_state, *store_id, roots_change);
            // The notification names what ended just now, and a
            // server sends it whenever the set of ended shares
            // changes: with nothing in it, the change was a share
            // granted again, and which one is in how the store is
            // held. (With something in it there is nothing to
            // ask: the link's next state brings the rest, and a
            // browser's store whose last share ended is closed
            // by the time a question about it could be answered.)
            if node_ids.is_empty() {
                store.send(BackendCommand::GetStoreSync { store_id: *store_id });
            }
        }
        StoreChangeKind::PeopleChanged => {
            store.send(BackendCommand::GetPeople { store_id: *store_id });
        }
        StoreChangeKind::IndexStateChanged { state } => {
            store.set_index_state(*store_id, state.clone());
        }
        StoreChangeKind::VaultAppended { .. } | StoreChangeKind::VaultReported { .. } => {
            // Encrypted-store blobs are handled by the vault client
            // (docs/CRYPTO_CONTRACT.md); the tree does not change here.
        }
        StoreChangeKind::MountStateChanged { node_id, state } => {
            tracing::info!("Mount state of {:?}/{:?}: {:?}", store_id, node_id, state);
            // Derived state, recomputed by this server whenever
            // the source's link moves (decision 5). It carries no
            // `MountRef`, so keep the one already known. The
            // follow-ups only run when this is news — see
            // `update_mount_state` for the two cases that aren't.
            if store.update_mount_state(*store_id, *node_id, state.clone()) {
                let source_store = store.mount_source_store(*store_id, *node_id);
                apply_mount_state_followups(store, *store_id, *node_id, state, source_store);
            }
        }
    }
}
