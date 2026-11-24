//! A store's link to a remote: the remote's store list for the connect
//! modal, and the sync answer (state, mode, access, relay side, roots).

use pimble_core::Store;
use rinch::prelude::*;

use crate::state::AppStore;

use super::view::{apply_roots_change, rejudge};

/// `BackendEvent::RemoteStoresListed`.
pub(super) fn remote_stores_listed(store: AppStore, url: &String, result: &Result<Vec<Store>, String>) {
    // Guard against a stale answer landing after the modal's URL
    // changed and "List stores" was clicked again.
    if untracked(|| store.connect_modal_url.get()) != *url {
        return;
    }
    store.connect_modal_busy.set(false);
    match result {
        Ok(stores) => {
            store.connect_modal_stores.set(stores.clone());
            store.connect_modal_selected.set(
                stores.first().map(|s| s.id.to_string()).unwrap_or_default()
            );
            store.connect_modal_error.set(String::new());
        }
        Err(message) => {
            store.connect_modal_stores.set(Vec::new());
            store.connect_modal_selected.set(String::new());
            store.connect_modal_error.set(message.clone());
        }
    }
}

/// What a `BackendEvent::StoreSyncChanged` says, borrowed from it.
pub(super) struct SyncChanged<'a> {
    pub store_id: &'a pimble_core::StoreId,
    pub remote: &'a Option<pimble_core::RemoteEndpoint>,
    pub state: &'a pimble_core::SyncState,
    pub sync_mode: &'a pimble_core::StoreKind,
    pub access: &'a pimble_core::StoreAccess,
    pub read_only_roots: &'a Vec<pimble_core::NodeId>,
    pub ended_roots: &'a Vec<pimble_core::NodeId>,
    pub relay: &'a pimble_core::RelaySide,
    pub owner_offline: &'a bool,
    pub waiting: &'a Vec<pimble_core::WaitingDoc>,
    pub out_of_step: &'a Vec<pimble_core::OutOfStep>,
}

/// `BackendEvent::StoreSyncChanged`.
pub(super) fn store_sync_changed(store: AppStore, tree_state: UseTreeReturn, changed: SyncChanged<'_>) {
    let SyncChanged { store_id, remote, state, sync_mode, access, read_only_roots, ended_roots, relay, owner_offline, waiting, out_of_step } = changed;
    tracing::info!("Sync state for store {:?}: {:?} ({:?}, {:?}, relay {:?})", store_id, state, sync_mode, access, relay);
    let was_linked = store.is_linked(*store_id);
    store.set_sync(*store_id, remote.clone(), state.clone());
    // What the link is, for the badge's "encrypted" prefix
    // (docs/DESKTOP_ACCOUNT_CONTRACT.md decision 4).
    store.set_store_sync_mode(*store_id, *sync_mode);
    // Which end of the relay this device is, for the badge's
    // "shared from here" and for the row's menu, which snapshots
    // it (docs/RELAY_CONTRACT.md, "The apps").
    if store.set_store_relay(*store_id, *relay) {
        store.bump_tree_structure();
    }
    store.set_owner_offline(*store_id, *owner_offline);
    // What the link is holding back, for the badge's "· N notes
    // waiting" and each waiting node's Properties panel.
    store.set_waiting(*store_id, waiting.clone());
    // Which notes are out of step, for their rows (which snapshot
    // it), panes and Properties (docs/CONVERGENCE_CONTRACT.md
    // section 4).
    if store.set_out_of_step(*store_id, out_of_step.clone()) {
        store.bump_tree_structure();
    }
    // What this device may change here, as the server has it now
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"). When it
    // differs from what was held, so may the server's judgement
    // of any node of the store (`Node::access`), and nothing names
    // which: everything held of the store is fetched again.
    // The shares of it that have ended, as the server has them
    // now: one that ended while this app was not listening leaves
    // the explorer here, and one granted again comes back. Before
    // the access below, so that what is fetched again is what is
    // still shown.
    let roots_change = store.set_store_roots(*store_id, None, ended_roots);
    apply_roots_change(store, tree_state, *store_id, roots_change);
    if store.set_store_access(*store_id, *access, read_only_roots) {
        tracing::info!("Access to store {:?} changed: {:?}, read-only roots {:?}", store_id, access, read_only_roots);
        rejudge(store, *store_id);
        // The store row's menu and every node row's snapshot it.
        store.bump_tree_structure();
    }
    let now_linked = remote.is_some();
    if was_linked != now_linked {
        // The "Link to Remote..."/"Unlink from Remote" disabled
        // state flipped — rebuild the tree so the store row's
        // context menu re-renders with it (rinch #714: menu items
        // must not sit inside a reactive block).
        store.bump_tree_structure();
    }

    // A "Link to Remote..." modal request just answered.
    if untracked(|| store.link_modal_pending.get())
        && untracked(|| store.link_modal_store.get()) == Some(*store_id)
    {
        store.link_modal_pending.set(false);
        store.link_modal_store.set(None);
        store.link_modal_error.set(String::new());
    }
}
