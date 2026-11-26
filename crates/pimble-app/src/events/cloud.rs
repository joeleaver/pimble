//! Pimble Cloud account events (docs/DESKTOP_ACCOUNT_CONTRACT.md "Events"):
//! sign-in status, the errors every cloud operation names, hosted and relayed
//! stores, and shares.

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;

use crate::protocol::{BackendCommand, CloudOp};
use super::show_notice;
use crate::state::{share_state_text, AppStore};

/// `BackendEvent::CloudStatusChanged`.
pub(super) fn status_changed(
    store: AppStore,
    signed_in: &bool,
    email: &Option<String>,
    user_id: &Option<String>,
    url: &Option<String>,
) {
    // Who board edits are stamped as (docs/IMAGE_MARKUP_CONTRACT.md
    // section 6): the account, or nobody when signed out.
    if *signed_in {
        crate::boards::set_author(user_id.clone(), email.clone());
        // Signing in is the asking (docs/DISCUSSIONS_CONTRACT.md
        // section 8): the personal document follows the account.
        crate::personal::sync_now(store);
        // The name the account goes by, which stamps replace the
        // email's local part with (docs/PRESENCE_CONTRACT.md
        // section 3).
        store.send(BackendCommand::CloudListShares);
        // The notices this device keeps (docs/NOTIFICATIONS_CONTRACT.md).
        store.send(BackendCommand::ListNotices);
        store.send(BackendCommand::ListWatches);
    } else {
        crate::boards::set_author(None, None);
        store.watches.set(Vec::new());
    }
    tracing::info!("Cloud account: signed_in={} email={:?}", signed_in, email);
    store.cloud_answer.set(crate::state::CloudAnswer::Answered);
    crate::startup::mark("account answered");
    store.cloud_signed_in.set(*signed_in);
    if !*signed_in {
        store.cloud_connection.set(None);
    }
    store.cloud_email.set(email.clone().unwrap_or_default());
    store.cloud_url.set(url.clone().unwrap_or_default());
    // Seed the sign-in form with the account this server knows,
    // so a sign-out (or a restart) leaves the URL and email to be
    // typed once, not every time.
    if let Some(email) = email {
        store.account_modal_email.set(email.clone());
    }
    if let Some(url) = url {
        store.account_modal_url.set(url.clone());
    }
    // A sign-in or sign-out from the Account modal just answered:
    // the modal stays open and shows its other face, so the
    // person sees it worked.
    if untracked(|| store.account_modal_busy.get()) {
        store.account_modal_busy.set(false);
        store.account_modal_error.set(String::new());
        store.account_modal_password.set(String::new());
        store.account_modal_hint.set(String::new());
    }
}

/// `BackendEvent::CloudError`.
pub(super) fn error(store: AppStore, op: &CloudOp, message: &String) {
    tracing::warn!("Cloud {:?} failed: {}", op, message);
    match op {
        CloudOp::SignIn | CloudOp::SignOut => {
            store.account_modal_busy.set(false);
            store.account_modal_error.set(message.clone());
        }
        CloudOp::Status => {
            // The status bar says "disconnected" now: asking
            // actually failed (docs/STARTUP_CONTRACT.md).
            store.cloud_answer.set(crate::state::CloudAnswer::Failed(message.clone()));
            // Asked on every connect; only worth a line in the
            // modal when the modal is there to show it.
            if untracked(|| crate::app::account_showing(store)) {
                store.account_modal_busy.set(false);
                store.account_modal_error.set(message.clone());
            }
        }
        CloudOp::Shares => crate::shares::failed(store, message),
        CloudOp::Notifications => {
            // The switch did not take: it shows what holds, and why.
            store.send(BackendCommand::ListNotices);
            show_notice(store, format!("Notifications could not be changed: {message}"));
        }
        CloudOp::Watches => show_notice(store, message.clone()),
        CloudOp::HostStore => {
            store.host_modal_busy.set(false);
            store.host_modal_error.set(message.clone());
        }
        CloudOp::ListHostedStores | CloudOp::AddHostedStore => {
            store.hosted_modal_busy.set(false);
            store.hosted_modal_error.set(message.clone());
        }
        CloudOp::RelayStore => {
            // "Share from this computer" in the Share modal: the
            // server's own sentence, where the person pressed it.
            // Nothing was shared, so the two ways stay on offer.
            store.share_modal_pending.set(None);
            store.share_modal_error.set(message.clone());
        }
        CloudOp::StopRelaying => {
            // The confirmation that asked shows it as it is:
            // "This store still has shares. Stop sharing each of
            // them first. Nothing was changed."
            store.stop_relay_modal_busy.set(false);
            store.stop_relay_modal_error.set(message.clone());
        }
        CloudOp::Share
        | CloudOp::ShareInvite
        | CloudOp::ShareRemoveMember
        | CloudOp::StopSharing => {
            // The modal that asked shows it, in the server's own
            // words (docs/NODE_DOCUMENT_CONTRACT.md section 5).
            store.share_modal_pending.set(None);
            store.share_modal_confirm_stop.set(false);
            store.share_modal_error.set(message.clone());
            // A refused share may be the server knowing something
            // about the store's link that this app does not (it
            // is not hosted after all): ask, so the modal offers
            // the two ways instead of a form that is refused.
            if *op == CloudOp::Share {
                if let Some((store_id, _)) = untracked(|| store.share_modal_node.get()) {
                    store.send(BackendCommand::GetStoreSync { store_id });
                }
            }
        }
        CloudOp::ShareInfo => {
            // Sent when the modal opens on a node that is already
            // shared; the marker still says it is, so the modal
            // keeps the shared face and says what went wrong.
            // Not to a member looking at the share they are in:
            // their face is who shared it, which this device
            // holds, and the member list under it is a courtesy
            // that is simply absent when it cannot be had.
            store.share_modal_pending.set(None);
            if untracked(|| store.share_modal_manages()) {
                store.share_modal_error.set(message.clone());
            }
        }
    }
}

/// `BackendEvent::CloudHostedStoresListed`.
pub(super) fn hosted_stores_listed(
    store: AppStore,
    stores: &[pimble_rpc::CloudHostedStoreInfo],
    relayed: &[String],
) {
    // Only a hosted store not already open here can be added as
    // a replica (the server refuses an open one anyway), so the
    // modal lists just those (decision 6): an encrypted one, or a
    // plain one, which `cloudAddHostedStore` links through the
    // account since docs/ONE_CODEBASE_CONTRACT.md wave 5. A share is the
    // exception: a second share of a store already open here as a
    // partial replica extends that replica with another root, so
    // it is listed as long as this device does not hold that root
    // already (docs/NODE_DOCUMENT_CONTRACT.md section 5).
    let open: Vec<StoreId> = untracked(|| store.store_ids.get());
    let mut listed: Vec<String> = Vec::new();
    let candidates: Vec<pimble_rpc::CloudHostedStoreInfo> = stores
        .iter()
        .filter(|s| s.kind == "vault" || s.kind == "plain")
        .filter(|s| {
            let Ok(id) = s.store_id.parse::<uuid::Uuid>() else { return false };
            let store_id = StoreId(id);
            if open.contains(&store_id) {
                // Already here in full, or the share's root is one
                // of the ones this replica already holds.
                match s.root {
                    Some(root) if !store.shown_roots(store_id).contains(&root) => {}
                    _ => return false,
                }
            }
            // `cloudAddHostedStore` names a store, not a scope, so
            // two pending shares of one store are one row: adding
            // it brings every root the account is granted, and the
            // `StoreOpened` that answers says which arrived.
            if listed.contains(&s.store_id) {
                return false;
            }
            listed.push(s.store_id.clone());
            true
        })
        .cloned()
        .collect();
    store.hosted_modal_selected.set(
        candidates.first().map(|s| s.store_id.clone()).unwrap_or_default(),
    );
    // Before the rows, which the list's labels are drawn from.
    store.hosted_modal_relayed.set(relayed.to_vec());
    store.hosted_modal_stores.set(candidates);
    store.hosted_modal_busy.set(false);
    store.hosted_modal_error.set(String::new());
}

/// `BackendEvent::CloudStoreHosted`.
pub(super) fn store_hosted(store: AppStore, store_id: &StoreId) {
    tracing::info!("Store {:?} is hosted on Pimble Cloud", store_id);
    store.host_modal_busy.set(false);
    store.host_modal_store.set(None);
    store.host_modal_error.set(String::new());
    // The badge and mode come from the server's view of the new
    // link, and the row's menu re-renders with "Host on Pimble
    // Cloud..." disabled (decision 5). That the link is an
    // encrypting one is known already, and the Share modal below
    // opens on it.
    store.set_store_sync_mode(*store_id, pimble_core::StoreKind::Vault);
    store.send(BackendCommand::GetStoreSync { store_id: *store_id });
    store.bump_tree_structure();
    // Hosting was the way the person chose to share a node
    // (docs/RELAY_CONTRACT.md, "The apps"): carry on into the
    // share, as "Share from this computer" does.
    if let Some((share_store, share_node)) = untracked(|| store.share_after_host.get()) {
        store.share_after_host.set(None);
        if share_store == *store_id {
            crate::app::open_share_modal(store, share_store, share_node);
        }
    }
}

/// `BackendEvent::CloudStoreRelayed`.
pub(super) fn store_relayed(store: AppStore, store_id: &StoreId) {
    tracing::info!("Store {:?} is shared from this computer", store_id);
    // What the server has just made of the store's link: an
    // encrypting one to its twin on this machine. The Share modal
    // reads both from the store's signal and moves on to the
    // share's name by itself (`AppStore::share_modal_face`); the
    // link's state follows as ordinary `SyncStateChanged`.
    store.set_store_sync_mode(*store_id, pimble_core::StoreKind::Vault);
    store.set_store_relay(*store_id, pimble_core::RelaySide::Owner);
    if untracked(|| store.share_modal_node.get()).is_some_and(|(modal_store, _)| modal_store == *store_id) {
        store.share_modal_pending.set(None);
        store.share_modal_error.set(String::new());
    }
    store.send(BackendCommand::GetStoreSync { store_id: *store_id });
    store.bump_tree_structure();
}

/// `BackendEvent::CloudRelayingStopped`.
pub(super) fn relaying_stopped(store: AppStore, store_id: &StoreId) {
    tracing::info!("Store {:?} is no longer shared from this computer", store_id);
    if untracked(|| store.stop_relay_modal_store.get()) == Some(*store_id) {
        store.stop_relay_modal_busy.set(false);
        store.stop_relay_modal_store.set(None);
        store.stop_relay_modal_error.set(String::new());
    }
    // The store is unlinked again; the server's answer fills in
    // the rest, and the row's menu re-renders.
    store.set_store_sync_mode(*store_id, pimble_core::StoreKind::Plain);
    store.set_store_relay(*store_id, pimble_core::RelaySide::None);
    store.send(BackendCommand::GetStoreSync { store_id: *store_id });
    store.bump_tree_structure();
}

/// `BackendEvent::CloudShareUpdated`.
pub(super) fn share_updated(
    store: AppStore,
    store_id: &StoreId,
    node_id: &NodeId,
    share: &pimble_rpc::ShareInfo,
    members: &[pimble_rpc::ShareMember],
) {
    tracing::info!("Share of {:?}/{:?}: {:?}, {} members", store_id, node_id, share.state, members.len());
    if untracked(|| store.share_modal_node.get()) == Some((*store_id, *node_id)) {
        store.share_modal_shared.set(true);
        store.share_modal_name.set(share.name.clone());
        store.share_modal_state.set(share_state_text(&share.state).to_string());
        store.share_modal_members.set(members.to_vec());
        store.share_modal_invite_email.set(String::new());
        store.share_modal_pending.set(None);
        store.share_modal_error.set(String::new());
    }
    // The marker the server wrote, which the tree's badge reads,
    // arrives as `Shown` with the node's change.
}

/// `BackendEvent::CloudSharingStopped`.
pub(super) fn sharing_stopped(store: AppStore, store_id: &StoreId, node_id: &NodeId) {
    tracing::info!("Stopped sharing {:?}/{:?}", store_id, node_id);
    if untracked(|| store.share_modal_node.get()) == Some((*store_id, *node_id)) {
        store.share_modal_node.set(None);
        store.share_modal_shared.set(false);
        store.share_modal_members.set(Vec::new());
        store.share_modal_confirm_stop.set(false);
        store.share_modal_pending.set(None);
        store.share_modal_error.set(String::new());
    }
    // The marker is gone, so the badge goes with it, when the
    // node's change arrives as `Shown`.
}
