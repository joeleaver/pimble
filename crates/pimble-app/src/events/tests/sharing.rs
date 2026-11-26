//! Sharing: the share modal, sharing from this computer, sharing errors and refusals.

use super::*;

/// The answer to every sharing request but "Stop sharing" moves the modal
/// to its shared face, with the members and the link's state in words, and
/// asks for the node again so the tree's badge appears.
#[test]
fn a_share_update_fills_the_modal() {
    let (store, events, commands) = store_with_events();
    let (store_id, node_id) = (StoreId::new(), NodeId::new());
    store.share_modal_node.set(Some((store_id, node_id)));
    store.share_modal_pending.set(Some(CloudOp::Share));

    events
        .send(BackendEvent::CloudShareUpdated {
            store_id,
            node_id,
            share: share_info(store_id, node_id, "Recipes"),
            members: vec![
                ShareMember { email: "me@example.com".into(), role: MemberRole::Owner, status: ShareMemberStatus::Active, name: String::new() },
                ShareMember { email: "ann@example.com".into(), role: MemberRole::Editor, status: ShareMemberStatus::Invited, name: String::new() },
            ],
        })
        .unwrap();
    pump(store);

    assert!(store.share_modal_shared.get());
    assert_eq!(store.share_modal_name.get(), "Recipes");
    assert_eq!(store.share_modal_state.get(), "Handing the key and the scope over...");
    assert_eq!(store.share_modal_members.with(|m| m.len()), 2);
    assert_eq!(store.share_modal_pending.get(), None);
    assert!(store.share_modal_error.get().is_empty());
    // The marker the server wrote arrives with the node's own change, as
    // `Shown`: nothing is fetched for it.
    assert!(!commands.try_iter().any(|cmd| matches!(cmd, BackendCommand::GetNode { .. })));
}

/// An update for another node leaves a modal open on this one alone.
#[test]
fn a_share_update_for_another_node_is_ignored() {
    let (store, events, _commands) = store_with_events();
    let (store_id, node_id) = (StoreId::new(), NodeId::new());
    store.share_modal_node.set(Some((store_id, node_id)));

    let other = NodeId::new();
    events
        .send(BackendEvent::CloudShareUpdated {
            store_id,
            node_id: other,
            share: share_info(store_id, other, "Somebody else's"),
            members: Vec::new(),
        })
        .unwrap();
    pump(store);

    assert!(!store.share_modal_shared.get());
    assert!(store.share_modal_name.get().is_empty());
}

/// A live `ShareStateChanged` is what the modal shows while it is open.
#[test]
fn a_share_state_change_shows_in_the_modal() {
    let (store, events, _commands) = store_with_events();
    let (store_id, node_id) = (StoreId::new(), NodeId::new());
    store.share_modal_node.set(Some((store_id, node_id)));

    events
        .send(BackendEvent::RemoteStoreChange {
            store_id,
            change_kind: pimble_rpc::StoreChangeKind::ShareStateChanged { node_id, state: SyncState::Offline },
            source_client_id: None,
        })
        .unwrap();
    pump(store);

    assert_eq!(
        store.share_modal_state.get(),
        "Offline. The rest goes up when this device reconnects."
    );
}

/// "Stop sharing" closes the modal and asks for the node again, so the
/// row's badge goes with the marker.
#[test]
fn stopping_a_share_closes_the_modal() {
    let (store, events, commands) = store_with_events();
    let (store_id, node_id) = (StoreId::new(), NodeId::new());
    store.share_modal_node.set(Some((store_id, node_id)));
    store.share_modal_shared.set(true);
    store.share_modal_confirm_stop.set(true);
    store.share_modal_pending.set(Some(CloudOp::StopSharing));

    events.send(BackendEvent::CloudSharingStopped { store_id, node_id }).unwrap();
    pump(store);

    assert_eq!(store.share_modal_node.get(), None);
    assert!(!store.share_modal_shared.get());
    assert!(!store.share_modal_confirm_stop.get());
    assert_eq!(store.share_modal_pending.get(), None);
    assert!(!commands.try_iter().any(|cmd| matches!(cmd, BackendCommand::GetNode { .. })));
}

/// A local store that is neither hosted nor shared from anywhere, with a
/// document the Share dialog is open on, offering the two ways.
fn sharing_from_an_unhosted_store(store: AppStore) -> (StoreId, NodeId) {
    let local = pimble_core::Store::new_local("Notes", "/tmp/notes.pimble".into());
    let pasta = pimble_core::Node::document("Pasta");
    let (store_id, node_id) = (local.id, pasta.id);
    store.upsert_store(local);
    store.upsert_node(store_id, pasta);
    store.ensure_sync_entry(store_id);
    store.cloud_signed_in.set(true);
    store.share_modal_node.set(Some((store_id, node_id)));
    store.share_modal_shared.set(false);
    assert_eq!(untracked(|| store.share_modal_face()), crate::state::ShareFace::Ways);
    (store_id, node_id)
}

/// "Share from this computer" answered: the store is shared from here, so
/// the dialog carries on into the share (the name form) by itself, and
/// the row says `shared from here` and offers the way back
/// (docs/RELAY_CONTRACT.md, "The apps").
#[test]
fn sharing_from_this_computer_continues_into_the_share() {
    let (store, events, commands) = store_with_events();
    let (store_id, node_id) = sharing_from_an_unhosted_store(store);
    store.share_modal_pending.set(Some(CloudOp::RelayStore));
    let before = store.tree_structure_version.get();

    events.send(BackendEvent::CloudStoreRelayed { store_id }).unwrap();
    pump(store);

    assert_eq!(store.share_modal_node.get(), Some((store_id, node_id)), "the dialog closed instead of carrying on");
    assert_eq!(untracked(|| store.share_modal_face()), crate::state::ShareFace::Name);
    assert_eq!(store.share_modal_pending.get(), None);
    assert!(store.share_modal_error.get().is_empty());
    assert_eq!(store.store_relay(store_id), pimble_core::RelaySide::Owner);
    assert!(store.tree_structure_version.get() > before, "the row's menu never re-renders");
    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetStoreSync { store_id: s } if *s == store_id)));
    // Nothing was shared yet and nothing was hosted: the person still
    // names the share and presses "Share".
    assert!(!asked.iter().any(|c| matches!(c, BackendCommand::CloudShareNode { .. } | BackendCommand::CloudHostStore { .. })));
}

/// The server's refusal of "Share from this computer" is shown where it
/// was pressed, as the server wrote it, and the two ways stay on offer.
#[test]
fn a_refused_share_from_this_computer_stays_on_the_two_ways() {
    let (store, events, _commands) = store_with_events();
    let (store_id, _) = sharing_from_an_unhosted_store(store);
    store.share_modal_pending.set(Some(CloudOp::RelayStore));
    let sentence = "This store is linked to another Pimble server. Unlink it first. Nothing was changed.";

    events.send(BackendEvent::CloudError { op: CloudOp::RelayStore, message: sentence.to_string() }).unwrap();
    pump(store);

    assert_eq!(store.share_modal_error.get(), sentence);
    assert_eq!(store.share_modal_pending.get(), None);
    assert_eq!(untracked(|| store.share_modal_face()), crate::state::ShareFace::Ways);
    assert_eq!(store.store_relay(store_id), pimble_core::RelaySide::None);
    assert_eq!(store.connection_status.get(), "Connecting...");
}

/// A share refused because the store is not hosted after all (the app's
/// idea of its link was stale) asks the server what the link is, which is
/// what moves the dialog to the two ways.
#[test]
fn a_share_refused_for_want_of_a_host_asks_what_the_link_is() {
    let (store, events, commands) = store_with_events();
    let (store_id, _) = sharing_from_an_unhosted_store(store);
    store.set_store_sync_mode(store_id, pimble_core::StoreKind::Vault);
    store.share_modal_pending.set(Some(CloudOp::Share));
    let sentence = "Sharing needs this store hosted on Pimble Cloud or shared from this computer.";

    events.send(BackendEvent::CloudError { op: CloudOp::Share, message: sentence.to_string() }).unwrap();
    pump(store);
    assert_eq!(store.share_modal_error.get(), sentence);
    assert!(commands.try_iter().any(|c| matches!(c, BackendCommand::GetStoreSync { store_id: s } if s == store_id)));

    let mut answer = sync_changed(store_id, pimble_core::StoreAccess::Full, Vec::new());
    if let BackendEvent::StoreSyncChanged { sync_mode, state, .. } = &mut answer {
        *sync_mode = pimble_core::StoreKind::Plain;
        *state = SyncState::Offline;
    }
    events.send(answer).unwrap();
    pump(store);
    assert_eq!(untracked(|| store.share_modal_face()), crate::state::ShareFace::Ways);
}

/// Hosting chosen from the Share dialog carries on into the share once
/// the store is hosted; hosting asked for on its own opens nothing.
#[test]
fn hosting_chosen_in_the_share_dialog_continues_into_the_share() {
    let (store, events, _commands) = store_with_events();
    let (store_id, node_id) = sharing_from_an_unhosted_store(store);
    crate::app::host_to_share(store);
    assert_eq!(store.share_modal_node.get(), None);
    store.host_modal_busy.set(true);

    events.send(BackendEvent::CloudStoreHosted { store_id }).unwrap();
    pump(store);

    assert_eq!(store.host_modal_store.get(), None);
    assert_eq!(store.share_after_host.get(), None);
    assert_eq!(store.share_modal_node.get(), Some((store_id, node_id)));
    assert_eq!(untracked(|| store.share_modal_face()), crate::state::ShareFace::Name);

    // Hosted from the store row's menu: no dialog follows.
    store.share_modal_node.set(None);
    events.send(BackendEvent::CloudStoreHosted { store_id }).unwrap();
    pump(store);
    assert_eq!(store.share_modal_node.get(), None);
}

/// "Stop Sharing from This Computer...": the server's refusal while the
/// store still has shares is shown in the confirmation as it is, and a
/// success closes it, takes `shared from here` off the row and rebuilds
/// its menu.
#[test]
fn stopping_sharing_from_this_computer_answers_in_its_own_confirmation() {
    let (store, events, commands) = store_with_events();
    let (store_id, _) = sharing_from_an_unhosted_store(store);
    store.share_modal_node.set(None);
    store.set_store_sync_mode(store_id, pimble_core::StoreKind::Vault);
    store.set_store_relay(store_id, pimble_core::RelaySide::Owner);
    crate::app::open_stop_relay_modal(store, store_id);
    store.stop_relay_modal_busy.set(true);
    let sentence = "This store still has shares. Stop sharing each of them first. Nothing was changed.";

    events.send(BackendEvent::CloudError { op: CloudOp::StopRelaying, message: sentence.to_string() }).unwrap();
    pump(store);
    assert_eq!(store.stop_relay_modal_error.get(), sentence);
    assert!(!store.stop_relay_modal_busy.get());
    assert_eq!(store.stop_relay_modal_store.get(), Some(store_id), "a refusal closed the confirmation");
    assert_eq!(store.store_relay(store_id), pimble_core::RelaySide::Owner);
    assert_eq!(store.connection_status.get(), "Connecting...");

    store.stop_relay_modal_busy.set(true);
    let before = store.tree_structure_version.get();
    events.send(BackendEvent::CloudRelayingStopped { store_id }).unwrap();
    pump(store);
    assert_eq!(store.stop_relay_modal_store.get(), None);
    assert!(!store.stop_relay_modal_busy.get());
    assert_eq!(store.store_relay(store_id), pimble_core::RelaySide::None);
    assert!(store.tree_structure_version.get() > before);
    assert!(commands.try_iter().any(|c| matches!(c, BackendCommand::GetStoreSync { store_id: s } if s == store_id)));
}

/// Which end of the relay a device is arrives with the store's sync
/// answer, as its access does: it is written to the store (the badge
/// reads it) and the tree is rebuilt when it changed (the menu snapshots
/// it). So is a backend's knowledge that the owner's computer is off.
#[test]
fn a_sync_answer_carries_the_relay_side() {
    let (store, events, _commands) = store_with_events();
    let (store_id, _) = sharing_from_an_unhosted_store(store);
    let answer = |relay: pimble_core::RelaySide, owner_offline: bool| {
        let mut event = sync_changed(store_id, pimble_core::StoreAccess::Full, Vec::new());
        if let BackendEvent::StoreSyncChanged { relay: r, owner_offline: o, .. } = &mut event {
            *r = relay;
            *o = owner_offline;
        }
        event
    };

    let before = store.tree_structure_version.get();
    events.send(answer(pimble_core::RelaySide::Owner, false)).unwrap();
    pump(store);
    assert_eq!(store.store_relay(store_id), pimble_core::RelaySide::Owner);
    let after = store.tree_structure_version.get();
    assert!(after > before);

    // The same answer again changes nothing and rebuilds nothing.
    events.send(answer(pimble_core::RelaySide::Owner, false)).unwrap();
    pump(store);
    assert_eq!(store.tree_structure_version.get(), after);

    events.send(answer(pimble_core::RelaySide::Member, true)).unwrap();
    pump(store);
    assert_eq!(store.store_relay(store_id), pimble_core::RelaySide::Member);
    assert!(store.owner_offline.with(|set| set.contains(&store_id)));
    events.send(answer(pimble_core::RelaySide::Member, false)).unwrap();
    pump(store);
    assert!(!store.owner_offline.with(|set| set.contains(&store_id)));
}

/// The browser's half of the relay (docs/RELAY_CONTRACT.md, "The apps"): a
/// store whose owner's computer is off is listed from the account's row
/// of it, to be read, with nothing under it; the backend says `owner
/// offline`; and when the owner is back the store is announced as the
/// store it is, which is what makes the tree fetch what is in it with no
/// reload.
#[test]
fn a_store_listed_while_its_owner_is_offline_fills_in_when_announced() {
    let (store, events, commands) = store_with_events();
    let mut offline = pimble_core::Store::new_local("Recipes", std::path::PathBuf::new());
    offline.kind = pimble_core::StoreKind::Vault;
    offline.access = pimble_core::StoreAccess::Read;
    offline.shared_by = Some("ann@example.com".to_string());
    offline.relay = pimble_core::RelaySide::Member;
    let (store_id, placeholder) = (offline.id, offline.root_node_id);

    events.send(BackendEvent::StoresListed { stores: vec![offline.clone()] }).unwrap();
    let mut down = sync_changed(store_id, pimble_core::StoreAccess::Read, Vec::new());
    if let BackendEvent::StoreSyncChanged { sync_mode, state, relay, owner_offline, .. } = &mut down {
        *sync_mode = pimble_core::StoreKind::Plain;
        *state = SyncState::Offline;
        *relay = pimble_core::RelaySide::Member;
        *owner_offline = true;
    }
    events.send(down).unwrap();
    pump(store);

    assert_eq!(store.shown_roots(store_id), vec![placeholder]);
    assert!(!store.store_access(store_id).allows_write());
    assert!(store.owner_offline.with(|set| set.contains(&store_id)));
    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    assert!(asked.iter().any(|c| asks_children(c, placeholder)));

    // The owner's computer is back: the share's root, and an editor's access.
    let recipes = NodeId::new();
    let mut back = offline.clone();
    back.access = pimble_core::StoreAccess::Full;
    back.root_node_id = recipes;
    back.roots = vec![recipes];
    events.send(BackendEvent::StoreOpened { store: back }).unwrap();
    let mut up = sync_changed(store_id, pimble_core::StoreAccess::Full, Vec::new());
    if let BackendEvent::StoreSyncChanged { relay, .. } = &mut up {
        *relay = pimble_core::RelaySide::Member;
    }
    events.send(up).unwrap();
    pump(store);

    assert_eq!(store.shown_roots(store_id), vec![recipes]);
    assert!(store.store_access(store_id).allows_write());
    assert!(!store.owner_offline.with(|set| set.contains(&store_id)));
    let asked: Vec<BackendCommand> = commands.try_iter().collect();
    assert!(
        asked.iter().any(|c| asks_children(c, recipes)),
        "the share's root was never fetched: the row would stay empty until a reload"
    );
}

/// A failed sharing request lands in the modal that asked, in the server's
/// own words — including the stub's, until the server side is built.
#[test]
fn a_sharing_error_lands_in_the_modal() {
    let (store, events, _commands) = store_with_events();
    store.share_modal_pending.set(Some(CloudOp::ShareInvite));

    events
        .send(BackendEvent::CloudError {
            op: CloudOp::ShareInvite,
            message: "Sharing a node is not built yet".to_string(),
        })
        .unwrap();
    pump(store);

    assert_eq!(store.share_modal_error.get(), "Sharing a node is not built yet");
    assert_eq!(store.share_modal_pending.get(), None);
    // The connection is fine; nothing about it changed.
    assert_eq!(store.connection_status.get(), "Connecting...");
}

/// The member list is what the owner's face is for, so an owner whose
/// list cannot be loaded is told why. A member looking at the share they
/// are in is not: their face is who shared it, which this device holds,
/// and the list under it is a courtesy that is simply absent.
#[test]
fn a_member_list_that_cannot_be_loaded_is_an_error_only_to_an_owner() {
    let (store, events, _commands) = store_with_events();
    let mut own = pimble_core::Store::new_local("Notes", "/tmp/notes.pimble".into());
    own.root_node_id = NodeId::new();
    let mut theirs = pimble_core::Store::new_local("Recipes", "/tmp/recipes.pimble".into());
    theirs.root_node_id = NodeId::new();
    theirs.shared_by = Some("ann@example.com".to_string());
    let (own_id, theirs_id, node_id) = (own.id, theirs.id, NodeId::new());
    store.upsert_store(own);
    store.upsert_store(theirs);
    let failed = || BackendEvent::CloudError { op: CloudOp::ShareInfo, message: "Pimble Cloud cannot be reached".to_string() };

    store.share_modal_node.set(Some((own_id, node_id)));
    store.share_modal_shared.set(true);
    store.share_modal_pending.set(Some(CloudOp::ShareInfo));
    events.send(failed()).unwrap();
    pump(store);
    assert_eq!(store.share_modal_error.get(), "Pimble Cloud cannot be reached");
    assert_eq!(store.share_modal_pending.get(), None);

    store.share_modal_error.set(String::new());
    store.share_modal_node.set(Some((theirs_id, node_id)));
    store.share_modal_pending.set(Some(CloudOp::ShareInfo));
    events.send(failed()).unwrap();
    pump(store);
    assert!(store.share_modal_error.get().is_empty(), "a member is shown an error for a courtesy");
    assert_eq!(store.share_modal_pending.get(), None);
    assert!(store.share_modal_shared.get(), "the face stays, with the sentence and no list");
    assert_eq!(store.share_modal_node.get(), Some((theirs_id, node_id)));
}

/// A refusal is the server saying no, not a broken connection: it shows
/// its own sentence and leaves the connection badge alone.
#[test]
fn a_refusal_is_a_notice_not_an_error() {
    let (store, events, _commands) = store_with_events();

    events
        .send(BackendEvent::Error {
            message: format!("Forbidden: {}", pimble_core::StoreAccess::READ_ONLY_REFUSAL),
        })
        .unwrap();
    pump(store);

    assert_eq!(store.notice.get(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);
    assert_eq!(store.connection_status.get(), "Connecting...");
}

/// The desktop's server and the browser backend send the sentence alone.
#[test]
fn a_refusal_without_a_prefix_is_a_notice_too() {
    let (store, events, _commands) = store_with_events();

    events
        .send(BackendEvent::Error { message: pimble_core::StoreAccess::READ_ONLY_REFUSAL.to_string() })
        .unwrap();
    pump(store);

    assert_eq!(store.notice.get(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);
    assert_eq!(store.connection_status.get(), "Connecting...");
}
