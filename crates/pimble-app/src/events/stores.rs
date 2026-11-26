//! Store events: a store opened, created, listed, closed or removed as a
//! replica, mounts created and their states, a hosted store made, an import
//! finished.

use pimble_core::{MountRef, MountState, NodeId, Store, StoreId};
use rinch::prelude::*;

use crate::persistence::save_app_state_file;
use crate::protocol::BackendCommand;
use crate::state::{AppStore, MountInfo};

use super::show_notice;
use super::links::restore_pane_documents;
use super::view::{apply_mount_state_followups, apply_roots_change, rejudge};

/// Register a newly-known store in the tree: upsert its signal, fetch each of
/// its roots and their children, subscribe to its changes, auto-expand its
/// row, and persist the open-store list. Used both when the app explicitly
/// opens a store (`StoreOpened`) and when it discovers one implicitly — a mount's
/// source store the server opened to resolve a mount (`StoresListed`,
/// decision 4) — minus the pending-mount finalization, which only applies to
/// the explicit "Mount Store..." folder-picker flow.
pub(super) fn register_opened_store(store: AppStore, tree_state: UseTreeReturn, opened_store: &Store) {
    let store_id = opened_store.id;
    // A whole store has one root; a partial replica of someone else's store
    // has one per share of it, each a row of its own under the store row
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "The recipient's replica").
    // Adding a second share of a store already open here extends the replica,
    // and the `StoreOpened` that answers carries the longer list — `upsert_store`
    // replaces the whole `Store`, so the new root is in the tree at once.
    let roots = opened_store.shown_roots();

    // A store already in the tree is told again when it is opened again (a
    // reconnect, a second share of it). If what this device may change in it
    // is not what was held, every node held of it carries a stale judgement
    // (`Node::access`), and only the roots are fetched below.
    let access_changed = store.set_store_access(store_id, opened_store.access, &opened_store.read_only_roots);
    // And if a root it was showing is not one it shows now (a share that
    // ended), that root leaves as it does on any other day: said, and
    // dropped. The roots that arrived are fetched below with the rest. A
    // store opened for the first time shows nothing yet, so an ended share
    // is simply not shown and nothing is said.
    let mut roots_change = store.set_store_roots(store_id, Some(&opened_store.roots), &opened_store.ended_roots);
    roots_change.arrived.clear();

    // Structural: new store appears in tree
    store.upsert_store(opened_store.clone());
    apply_roots_change(store, tree_state, store_id, roots_change);
    if access_changed {
        rejudge(store, store_id);
    }

    // The roots' children and every row of this store this device left
    // expanded, asked for together and answered as each is in
    // (docs/STARTUP_CONTRACT.md "Built": one level per round trip, in line
    // behind everything else, took seconds a level on a busy machine).
    for &root_id in &roots {
        store.expanded.update(|e| { e.insert((store_id, root_id)); });
    }
    let mut load: Vec<NodeId> = roots.clone();
    load.extend(untracked(|| store.expanded.with(|e| e.iter().filter(|(s, n)| *s == store_id && !roots.contains(n)).map(|(_, n)| *n).collect::<Vec<_>>())));
    for &node_id in &load {
        store.children_asked(store_id, node_id);
    }
    store.send(BackendCommand::LoadChildren { store_id, nodes: load });
    for &root_id in &roots {
        // The root node itself: a whole store's row takes its icon and colour
        // from it, and a share's root is a row that needs its own label.
        store.send(BackendCommand::GetNode { store_id, node_id: root_id });
    }

    // Subscribe to store changes for real-time updates
    store.send(BackendCommand::SubscribeStoreChanges { store_id });
    // And to who else is in its documents (docs/PRESENCE_CONTRACT.md).
    store.send(BackendCommand::SubscribePresence { store_id });
    // Who is who in its shares: the names its people go by there
    // (docs/PRESENCE_CONTRACT.md section 3). Only an encrypted store reaches
    // Pimble Cloud; any other has nobody to name.
    if opened_store.sync_mode == pimble_core::StoreKind::Vault || opened_store.shared_by.is_some() || opened_store.relay != pimble_core::RelaySide::None {
        store.send(BackendCommand::GetPeople { store_id });
    }

    // A store made for an import is open: the import goes into it.
    crate::import::store_opened(store, store_id, roots.first().copied());

    restore_pane_documents(store, store_id);

    // Sync status (docs/SYNC_CONTRACT.md "B: app side"): a placeholder
    // unlinked/offline entry exists immediately so the store row's badge
    // signal is always available by the time the tree renders it;
    // `GetStoreSync`'s answer (`StoreSyncChanged`) fills in the real state.
    store.ensure_sync_entry(store_id);
    store.send(BackendCommand::GetStoreSync { store_id });
    // Its themes are listed once something needs them
    // (`themes::want_listed`), not here: listing holds the whole store.

    // Auto-expand the store node in the tree — and, for a partial replica,
    // each shared root under it, so the store row does not open onto a list of
    // folder names with nothing in them.
    // A store this device has shown before opens as it was left
    // (`views.rs`).
    if !crate::views::seen_store(store_id) {
        tree_state.controller.expand(&format!("store_{}", store_id));
        if !opened_store.roots.is_empty() {
            for &root_id in &roots {
                tree_state.controller.expand(&format!("node_{}_{}", store_id, root_id));
            }
        }
    }

    // Persist open store paths (and the saved ones still opening).
    save_app_state_file(&store.store_paths_to_save());

    // Structural change
    store.bump_tree_structure();
}

/// `BackendEvent::StoreOpened`.
pub(super) fn store_opened(store: AppStore, tree_state: UseTreeReturn, opened_store: &Store) {
    tracing::info!("Store opened: {}", opened_store.name);
    crate::startup::mark(&format!("store {} open", opened_store.name));
    let store_id = opened_store.id;
    let root_id = opened_store.root_node_id;

    register_opened_store(store, tree_state, opened_store);
    if let Some(path) = opened_store.local_path() {
        store.finish_opening(&path.to_string_lossy());
    }
    // "Add Here" in Settings > Shares: the row becomes "Open".
    if untracked(|| crate::settings::showing(store, crate::settings::Section::Shares)) {
        store.send(BackendCommand::CloudListShares);
    }

    // An `AddRemoteStore` request just answered successfully —
    // close the connect modal (contract: "the modal closes on
    // success"). In the "Mount Remote Store Here..." flow the
    // same event only reports the replica half of the two-step
    // command: the modal stays open and busy until `MountCreated`
    // (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 9).
    if untracked(|| store.connect_modal_pending_add.get())
        && untracked(|| store.connect_modal_target.get()).is_none()
    {
        store.connect_modal_pending_add.set(false);
        store.connect_modal_busy.set(false);
        store.connect_modal_open.set(false);
        store.connect_modal_error.set(String::new());
    }

    // A `CloudAddHostedStore` request just answered: the replica
    // is open, so the "Add Hosted Store..." modal closes
    // (docs/DESKTOP_ACCOUNT_CONTRACT.md decision 6). The id
    // check keeps a store opening for another reason while the
    // list request is in flight (busy too) from closing it.
    let hosted_add_answered = untracked(|| store.hosted_modal_busy.get())
        && untracked(|| store.hosted_modal_selected.get()) == store_id.to_string();
    if hosted_add_answered {
        store.hosted_modal_busy.set(false);
        store.hosted_modal_open.set(false);
        store.hosted_modal_error.set(String::new());
    }

    // Check if this store was opened as part of a pending mount
    if let Some(pending) = store.pending_mount.get() {
        if let Some(opened_path) = opened_store.local_path() {
            if opened_path.to_string_lossy() == pending.source_path {
                tracing::info!("Completing pending mount: source store {} opened", store_id);
                store.pending_mount.set(None);
                store.send(BackendCommand::CreateMount {
                    store_id: pending.target_store_id,
                    parent_id: pending.target_parent_id,
                    source_store_id: store_id,
                    source_node_id: root_id,
                    title: Some(opened_store.name.clone()),
                });
            }
        }
    }
}

/// `BackendEvent::StoreCreated`.
pub(super) fn store_created(store: AppStore, store_id: &StoreId, root_node_id: &NodeId) {
    tracing::info!("Store created: {:?} with root {:?}", store_id, root_node_id);
    crate::import::store_created(*store_id);
    let pending = store.pending_create_path.get();
    if let Some(path) = pending {
        store.pending_create_path.set(None);
        tracing::info!("Auto-opening created store at: {}", path);
        store.backend.with(|b| {
            if let Some(backend) = b {
                backend.send(BackendCommand::OpenStore { path });
            }
        });
    }
}

/// `BackendEvent::StoresListed`.
pub(super) fn stores_listed(store: AppStore, tree_state: UseTreeReturn, stores: &Vec<Store>) {
    // Discover any store not yet known — the server may have
    // opened it implicitly to resolve a mount (decision 4). Run
    // the same registration path a normal `StoreOpened` uses,
    // minus the pending-mount finalization (that's specific to
    // the explicit "Mount Store..." folder-picker flow).
    let known = untracked(|| store.store_ids.with(|ids| ids.clone()));
    for s in stores {
        if !known.contains(&s.id) {
            tracing::info!("Discovered implicitly-opened store: {} ({})", s.name, s.id);
            register_opened_store(store, tree_state, s);
            continue;
        }
        // A store the tree already has. The roots it shows are
        // the ones listed now: a share that ended since is gone
        // from them (the browser lists again at every token
        // refresh, and its backend has said which ended by then)
        // or named as ended (a desktop replica, which keeps it).
        let roots_change = store.set_store_roots(s.id, Some(&s.roots), &s.ended_roots);
        apply_roots_change(store, tree_state, s.id, roots_change);
        if store.set_store_access(s.id, s.access, &s.read_only_roots) {
            // Listed with another access than the one held (the
            // browser lists again at every token refresh, which
            // is when a role the owner changed arrives there): as
            // `StoreSyncChanged` does, fetch what is held of it
            // again, for each node's own `access`.
            tracing::info!("Access to store {:?} changed: {:?}", s.id, s.access);
            rejudge(store, s.id);
            store.bump_tree_structure();
        }
    }
}

/// `BackendEvent::StoreClosed`.
pub(super) fn store_closed(store: AppStore, store_id: &StoreId) {
    tracing::info!("Store closed: {:?}", store_id);

    // Structural: remove store and all its signals
    store.remove_store(*store_id);
    store.store_themes.update(|all| {
        all.remove(store_id);
    });
    crate::themes::listing_ended(*store_id);
    crate::presence::forget_store(store, *store_id);

    // Persist open store paths
    save_app_state_file(&store.all_store_local_paths());

    // Structural change
    store.bump_tree_structure();
}

/// `BackendEvent::MountCreated`.
pub(super) fn mount_created(
    store: AppStore,
    store_id: &StoreId,
    parent_id: &NodeId,
    node_id: &NodeId,
    mount_ref: &MountRef,
) {
    tracing::info!("Mount created: {:?}/{:?} -> {:?}/{:?}",
        store_id, node_id, mount_ref.source_store, mount_ref.source_node);
    // Track mount info for the new node, using the mount_ref the
    // RPC returned (avoid nested borrow)
    let existing_mount = store.get_mount_signal(*store_id, *node_id);
    if let Some(sig) = existing_mount {
        sig.update(|m| { m.is_mount = true; m.mount_state = None; m.mount_ref = Some(mount_ref.clone()); });
    } else {
        let new_sig = Signal::new(MountInfo {
            is_mount: true,
            mount_state: None,
            mount_ref: Some(mount_ref.clone()),
        });
        store.mount_data.update(|map| {
            map.insert((*store_id, *node_id), new_sig);
        });
    }
    // Its row and its parent's list were shown just before this
    // answer (`Shown`); its state is the server's to say.
    store.send(BackendCommand::GetMountState { store_id: *store_id, node_id: *node_id });
    if !store.has_children_loaded(*store_id, *parent_id) && !store.shown_roots(*store_id).contains(parent_id) {
        // Made under a folder never opened: its list is loaded
        // once, so the mount is there when it opens.
        store.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *parent_id });
    }

    // The "Mount Store..." picker's request just succeeded.
    if untracked(|| store.mount_picker_target.get()).is_some() {
        store.mount_picker_pending.set(false);
        store.mount_picker_target.set(None);
        store.mount_picker_error.set(String::new());
    }

    // The second and last step of "Mount Remote Store Here..."
    // just succeeded — close the connect modal and leave it in
    // its plain "Add Remote Store..." mode.
    if untracked(|| store.connect_modal_target.get()).is_some() {
        store.connect_modal_pending_add.set(false);
        store.connect_modal_busy.set(false);
        store.connect_modal_open.set(false);
        store.connect_modal_error.set(String::new());
        store.connect_modal_target.set(None);
    }
}

/// `BackendEvent::MountStateChanged`.
pub(super) fn mount_state_changed(
    store: AppStore,
    store_id: &StoreId,
    node_id: &NodeId,
    state: &MountState,
    mount_ref: &MountRef,
) {
    tracing::info!("Mount state changed: {:?}/{:?} -> {:?}", store_id, node_id, state);
    // Data-only: updates per-mount signal, NO tree rebuild.
    // The mount Effect on that node fires and updates icon opacity + label suffix.
    store.set_mount_state(*store_id, *node_id, state.clone(), mount_ref.clone());

    // Same follow-ups the live notification gets: the source
    // store may be one only the server knows about (it opens or
    // replicates it to resolve the mount), and the mount's
    // children may never have loaded while it was out of reach.
    apply_mount_state_followups(
        store,
        *store_id,
        *node_id,
        state,
        Some(mount_ref.source_store),
    );
}

/// `BackendEvent::HostedStoreCreated`.
pub(super) fn hosted_store_created(store: AppStore, name: &String, store_id: &Option<StoreId>) {
    tracing::info!("Hosted store created: {}", name);
    if let Some(store_id) = store_id {
        crate::import::store_created(*store_id);
    }
    // The store itself arrives through `StoresListed`, once the
    // backend has a token carrying the new grant. Nothing to do
    // here but close the modal.
    store.new_store_modal_pending.set(false);
    store.new_store_modal_open.set(false);
    store.new_store_modal_name.set(String::new());
    store.new_store_modal_error.set(String::new());
}

/// `BackendEvent::Imported`.
pub(super) fn imported(
    store: AppStore,
    tree_state: UseTreeReturn,
    store_id: &StoreId,
    parent_id: &Option<NodeId>,
    node_id: &NodeId,
    title: &String,
    count: &usize,
) {
    tracing::info!("Imported {node_id:?} ({count} nodes)");
    let notes = if *count == 1 { "1 note".to_string() } else { format!("{count} notes") };
    show_notice(store, format!("Imported \"{title}\" ({notes})."));
    // The parent's list has a new entry: fetch it, and open the
    // parent so the import is in view.
    let root = store.root_node_id(*store_id);
    if let Some(parent) = parent_id.or(root) {
        store.expanded.update(|e| { e.insert((*store_id, parent)); });
        // A root's children are its store row's.
        if Some(parent) == root {
            tree_state.controller.expand(&format!("store_{store_id}"));
        } else {
            tree_state.controller.expand(&format!("node_{store_id}_{parent}"));
        }
        if !store.has_children_loaded(*store_id, parent) {
            store.send(BackendCommand::GetChildren { store_id: *store_id, node_id: parent });
        }
    }
}

/// `BackendEvent::ReplicaRemoved`.
pub(super) fn replica_removed(store: AppStore, store_id: &StoreId) {
    tracing::info!("Replica removed: {:?}", store_id);

    // Same tree/state-file cleanup as closing a store.
    store.remove_store(*store_id);
    save_app_state_file(&store.all_store_local_paths());
    store.bump_tree_structure();

    // The confirmation modal closes on success.
    if untracked(|| store.remove_replica_modal_store.get()) == Some(*store_id) {
        store.remove_replica_modal_pending.set(false);
        store.remove_replica_modal_store.set(None);
        store.remove_replica_modal_error.set(String::new());
    }
}
