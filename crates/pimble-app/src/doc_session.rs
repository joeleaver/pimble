//! The documents this window's views hold, and the one way they talk to the
//! API (docs/API_CONTRACT.md decision 12).
//!
//! Five kinds of view hold node documents: a pane's text editor
//! (`crate::editor`), a canvas pane (`crate::boards`), a sheet, in a pane or
//! embedded in text (`crate::sheets`), a discussion, in a pane or inline
//! (`crate::discussions`), and the canvases shown in a note's text
//! (`crate::text_boards`). Each keeps its own copy in the shape it draws
//! from (rinch's collaboration session, a `NodeDoc`). What they share is
//! here, once:
//!
//! - a local update leaves through [`send`] (`applyEdit`, unanswered);
//! - a view joining or rejoining asks for what it lacks with [`ask`], and
//!   the answer reaches every view of the node through [`reconciled`],
//!   which sends back what the backend lacks only where this device may
//!   write ([`writable`]);
//! - another device's update (and one the API wrote for this window, a
//!   rename) reaches every view of the node through [`received`];
//! - a connection that came back, or a store heard again after a gap, has
//!   every view of what it holds reconcile ([`reconnected`], [`resynced`]).
//!
//! There is no subscription per node: a store's subscription carries every
//! document's update, so a view holding a node hears it as long as the
//! node's store is open, however many views hold it. Handing a view's update
//! to the other views of this window that hold the node is still each
//! kind's own (`editor::hand_across`, `boards::send_update`,
//! `sheets::text_delta`, `text_boards::local_update`, ...): which views of
//! which kinds take a delta differs by kind, and is what wave 5 left.

use pimble_core::{NodeId, StoreId};

use crate::protocol::BackendCommand;
use crate::state::AppStore;

/// Send an update a view of this window made to node `node_id`'s document.
pub(crate) fn send(store: AppStore, store_id: StoreId, node_id: NodeId, update: Vec<u8>) {
    store.send(BackendCommand::BroadcastChanges { store_id, node_id, update });
}

/// Ask for what the backend holds of node `node_id` beyond `state_vector`
/// (a view's copy); the answer reaches every view through [`reconciled`].
pub(crate) fn ask(store: AppStore, store_id: StoreId, node_id: NodeId, state_vector: Vec<u8>) {
    store.send(BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector });
}

/// Whether a view of node `node_id` may send what it holds back: a document
/// this device may only read takes changes and sends none (the server would
/// refuse them, and the freshly hosted empty document a view opens with is
/// not the server's to take; docs/NODE_DOCUMENT_CONTRACT.md section 5,
/// "Roles").
pub(crate) fn writable(store: AppStore, store_id: StoreId, node_id: NodeId) -> bool {
    store.node_access(store_id, node_id).allows_write()
}

/// Send what a view holds that the backend lacks, after a reconcile.
pub(crate) fn send_back(store: AppStore, store_id: StoreId, node_id: NodeId, ours: Option<Vec<u8>>) {
    if let Some(ours) = ours.filter(|o| !o.is_empty()) {
        send(store, store_id, node_id, ours);
    }
}

/// An update this window's views did not make: every view holding the node
/// merges it.
pub(crate) fn received(store: AppStore, store_id: StoreId, node_id: NodeId, update: &[u8]) {
    crate::editor::apply_remote(store, store_id, node_id, update);
    crate::boards::apply_remote(store, store_id, node_id, update);
    crate::sheets::apply_remote(store, store_id, node_id, update);
    crate::discussions::apply_remote(store, store_id, node_id, update);
    crate::text_boards::apply_remote(store_id, node_id, update);
}

/// The answer to an [`ask`]: what the backend has beyond the asking view's
/// copy, and the backend's state vector. Every view of the node merges it
/// and sends back what it has that the backend lacks. (Each view asks with
/// its own state vector; an answer meant for one is a harmless merge in
/// another, since they hold the same document.)
pub(crate) fn reconciled(store: AppStore, store_id: StoreId, node_id: NodeId, diff: &[u8], server_state_vector: &[u8]) {
    crate::editor::apply_reconcile(store, store_id, node_id, diff, server_state_vector);
    crate::boards::apply_reconcile(store, store_id, node_id, diff, server_state_vector);
    crate::sheets::apply_reconcile(store, store_id, node_id, diff, server_state_vector);
    crate::discussions::apply_reconcile(store, store_id, node_id, diff, server_state_vector);
    crate::text_boards::apply_reconcile(store_id, node_id, diff);
}

/// Every view of every node this window holds reconciles: the connection
/// came back, and anything sent or written while it was down may be on one
/// side only.
pub(crate) fn reconnected(store: AppStore) {
    rejoin(store, None);
}

/// Every view of a node of `store_id` reconciles: the store was heard again
/// after a gap.
pub(crate) fn resynced(store: AppStore, store_id: StoreId) {
    rejoin(store, Some(store_id));
}

fn rejoin(store: AppStore, only: Option<StoreId>) {
    for active in store.edited_nodes() {
        if only.is_some_and(|s| s != active.store_id) {
            continue;
        }
        for pane in store.panes_editing(active.store_id, active.node_id) {
            crate::editor::request_reconcile(store, pane, active.store_id, active.node_id);
        }
        crate::boards::request_reconcile_all(store, active.store_id, active.node_id);
        crate::sheets::request_reconcile_all(store, active.store_id, active.node_id);
        crate::discussions::request_reconcile_all(store, active.store_id, active.node_id);
    }
    // The canvases embedded in text, and the notes sheets read or embed.
    crate::text_boards::reconnected(store, only);
    crate::sheets::reads::reconnected(store, only);
    crate::sheets::embeds_reconnected(store, only);
}
