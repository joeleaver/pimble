//! Backend event processing.
//!
//! Each event handler uses per-entity signal mutations. Only structural events
//! (store open/close, children loaded, node moved) bump `tree_structure_version`.

use std::cell::RefCell;
use std::collections::HashSet;

use pimble_core::{NodeId, Store, StoreId};
use rinch::prelude::*;

use crate::protocol::{BackendCommand, BackendEvent, CloudOp};
use crate::editor::{apply_remote, start_editing};
use crate::persistence::{load_app_state_file, save_app_state_file};
use crate::state::{
    parse_tree_value, share_state_text, take_last_drop_target_value, AppStore, ConnectionState,
    MountInfo, SearchState,
};

thread_local! {
    pub(crate) static EVENT_PROCESSOR: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
    /// The timer that clears the status bar's notice again (see [`show_notice`]).
    static NOTICE_TIMEOUT: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
}

/// How long a refusal stays in the status bar before it clears itself.
const NOTICE_MS: u32 = 8000;

/// Show a sentence in the status bar for a few seconds.
///
/// A refused command (`Forbidden`, e.g. a store shared read-only) is not a
/// broken connection and must not read as one: the UI disables what a store's
/// access forbids, so this is for the rare command that got through anyway —
/// and it says what the server said (docs/NODE_DOCUMENT_CONTRACT.md section 5,
/// "Roles").
pub(crate) fn show_notice(store: AppStore, message: String) {
    if let Some(handle) = NOTICE_TIMEOUT.with(|slot| slot.borrow_mut().take()) {
        clear_timeout(handle);
    }
    store.notice.set(message);
    let timeout = set_timeout(NOTICE_MS, move || {
        NOTICE_TIMEOUT.with(|slot| {
            slot.borrow_mut().take();
        });
        store.notice.set(String::new());
    });
    NOTICE_TIMEOUT.with(|slot| {
        *slot.borrow_mut() = Some(timeout);
    });
}

/// Whether an error is a refused write rather than a failure: one of the two
/// sentences of `StoreAccess`, which the desktop's server sends as the whole
/// `-32004` message and the browser backend answers itself, or any other
/// `Forbidden: ` refusal, whose text is also meant for the person.
fn refusal_sentence(message: &str) -> Option<&str> {
    pimble_core::StoreAccess::refusal_in(message).or_else(|| message.strip_prefix("Forbidden: "))
}

/// Drain whatever the backend has posted into the UI, from a backend that has
/// just put something there.
///
/// `app::build_view` registers the processor; a backend calls this (through
/// `run_on_main_thread`, so it never runs while the backend's own borrow is
/// live) every time it emits an event. Doing nothing is correct before the UI
/// has been built and after it is gone.
pub fn pump_backend_events() {
    EVENT_PROCESSOR.with(|cell| {
        if let Some(f) = cell.borrow().as_ref() {
            f();
        }
    });
}

/// Register a newly-known store in the tree: upsert its signal, fetch each of
/// its roots and their children, subscribe to its changes, auto-expand its
/// row, and persist the open-store list. Used both when the app explicitly
/// opens a store (`StoreOpened`) and when it discovers one implicitly — a mount's
/// source store the server opened to resolve a mount (`StoresListed`,
/// decision 4) — minus the pending-mount finalization, which only applies to
/// the explicit "Mount Store..." folder-picker flow.
fn register_opened_store(store: AppStore, tree_state: UseTreeReturn, opened_store: &Store) {
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
        refetch_held_nodes(store, store_id);
    }

    for &root_id in &roots {
        store.expanded.update(|e| { e.insert((store_id, root_id)); });
        store.send(BackendCommand::GetChildren { store_id, node_id: root_id });
        // The root node itself: a whole store's row takes its icon and colour
        // from it, and a share's root is a row that needs its own label.
        store.send(BackendCommand::GetNode { store_id, node_id: root_id });
    }

    // Subscribe to store changes for real-time updates
    store.send(BackendCommand::SubscribeStoreChanges { store_id });

    // Sync status (docs/SYNC_CONTRACT.md "B: app side"): a placeholder
    // unlinked/offline entry exists immediately so the store row's badge
    // signal is always available by the time the tree renders it;
    // `GetStoreSync`'s answer (`StoreSyncChanged`) fills in the real state.
    store.ensure_sync_entry(store_id);
    store.send(BackendCommand::GetStoreSync { store_id });

    // Auto-expand the store node in the tree — and, for a partial replica,
    // each shared root under it, so the store row does not open onto a list of
    // folder names with nothing in them.
    tree_state.controller.expand(&format!("store_{}", store_id));
    if !opened_store.roots.is_empty() {
        for &root_id in &roots {
            tree_state.controller.expand(&format!("node_{}_{}", store_id, root_id));
        }
    }

    // Persist open store paths
    save_app_state_file(&store.all_store_local_paths());

    // Structural change
    store.bump_tree_structure();
}

/// Exact refetch (docs/history/HARDENING_CONTRACT.md "B: app"): refetch
/// `(changed_store, parent_id)`'s children list if it is loaded, plus every
/// mount (in any store) whose `mount_ref` source is exactly that pair and
/// whose own children are loaded (decision 6 of SYNC_CONTRACT: no new
/// server-side fan-out for mounts — the client does this itself). Each
/// answer arrives as `ChildrenLoaded`. Used for every structural
/// notification kind, since each one now names every parent it touched.
fn refetch_parent_children(store: AppStore, changed_store: StoreId, parent_id: NodeId) {
    if store.has_children_loaded(changed_store, parent_id) {
        store.send(BackendCommand::GetChildren { store_id: changed_store, node_id: parent_id });
    } else if store.get_node_signal(changed_store, parent_id).is_some() {
        // A row that was never opened still says whether it can be: its
        // node's own `children`. When the first child of such a folder turns
        // up after the folder's own change did (a document made elsewhere,
        // readable here only once its key wraps had been fetched), nothing
        // else refreshes the row, and it stays without a chevron, its child
        // out of reach (found in the browser, 2026-09-21).
        store.send(BackendCommand::GetNode { store_id: changed_store, node_id: parent_id });
    }
    for (mount_store, mount_node) in store.mounts_sourced_from_node(changed_store, parent_id) {
        if store.has_children_loaded(mount_store, mount_node) {
            store.send(BackendCommand::GetChildren { store_id: mount_store, node_id: mount_node });
        }
    }
}

/// Fetch again everything the app holds of `store_id`, because what this
/// device may change there has changed and each node carries the server's
/// judgement of itself (`Node::access`): every loaded children list with
/// nodes of the store in it (its own, and a mount's that shows them from
/// another store), each root the store row shows (a share's root is in no
/// list this device holds), and the document in the editor, which may have
/// been opened from a search hit and be in no loaded list at all. The
/// answers arrive as `ChildrenLoaded` and `NodeLoaded` like any other; a
/// `NodeLoaded` for the document being edited updates the node and leaves
/// the session alone.
fn refetch_held_nodes(store: AppStore, store_id: StoreId) {
    for (list_store, parent_id) in store.loaded_lists_holding(store_id) {
        store.send(BackendCommand::GetChildren { store_id: list_store, node_id: parent_id });
    }
    let mut singles = store.shown_roots(store_id);
    if let Some(active) = untracked(|| store.active_edit.get()).filter(|active| active.store_id == store_id) {
        if !singles.contains(&active.node_id) {
            singles.push(active.node_id);
        }
    }
    for node_id in singles {
        store.send(BackendCommand::GetNode { store_id, node_id });
    }
}

/// Nodes of `store_id` the app already held came back with another access
/// than the one held (`changed`). What made it so reaches what is under them
/// too: a folder the owner moved from a root this account edits into one it
/// reads changes what may be done with every document in it, and the
/// notification for the move names only the two lists the folder moved
/// between. So each changed node's loaded children list is fetched again
/// (and those answers cascade the same way, stopping where nothing changed),
/// and so is the open document when it is of this store, since it may be in
/// no loaded list at all.
fn refetch_below_changed_access(store: AppStore, store_id: StoreId, changed: &[NodeId]) {
    if changed.is_empty() {
        return;
    }
    for &node_id in changed {
        if store.has_children_loaded(store_id, node_id) {
            store.send(BackendCommand::GetChildren { store_id, node_id });
        }
    }
    if let Some(active) = untracked(|| store.active_edit.get()) {
        if active.store_id == store_id && !changed.contains(&active.node_id) {
            store.send(BackendCommand::GetNode { store_id, node_id: active.node_id });
        }
    }
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
fn apply_roots_change(store: AppStore, tree_state: UseTreeReturn, store_id: StoreId, change: crate::state::RootsChange) {
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
        let selected = untracked(|| store.selected_id.get()).and_then(|value| parse_tree_value(&value));
        if let Some((selected_store, Some(selected_node))) = selected {
            if selected_store == store_id && has_left(selected_node) {
                store.selected_id.set(None);
                store.node_title.set(String::new());
                store.show_editor.set(false);
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

/// After a transition to `Synced`, refetch the children of every root the
/// store shows if none are loaded yet — a freshly added remote store's
/// documents arrive empty and only gain their children once the first
/// reconcile lands (docs/SYNC_CONTRACT.md "B: app side"). A partial replica
/// has one root per share, and each fills in the same way.
fn refetch_root_if_empty(store: AppStore, store_id: StoreId) {
    for root_id in store.shown_roots(store_id) {
        let children_missing = untracked(|| {
            store.get_children_signal(store_id, root_id)
                .map(|sig| sig.with(|c| c.is_empty()))
                .unwrap_or(true)
        });
        if children_missing {
            store.send(BackendCommand::GetChildren { store_id, node_id: root_id });
        }
    }
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
fn apply_mount_state_followups(
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

/// Process backend events and update store signals directly.
///
/// Per-entity signals are updated individually. `tree_structure_version` is
/// bumped only for structural changes so the tree rebuilds minimally.
pub(crate) fn process_backend_events(store: AppStore, tree_state: UseTreeReturn) {
    let events: Vec<BackendEvent> = store.backend.with(|b| {
        let Some(backend) = b else { return Vec::new() };
        let mut events = Vec::new();
        while let Some(event) = backend.try_recv() {
            events.push(event);
        }
        events
    });

    if events.is_empty() {
        return;
    }

    for event in &events {
        match event {
            BackendEvent::Connected { server_addr, client_id } => {
                tracing::info!("Connected to backend at {} (client_id: {})", server_addr, client_id);
                store.connection.set(ConnectionState::Connected);
                store.connection_status.set("Connected".to_string());
                store.server_addr.set(server_addr.clone());
                if !client_id.is_empty() {
                    store.client_id.set(client_id.clone());
                }

                // Auto-open previously loaded stores. On a reconnect this is
                // the same list (every open store is persisted there), so
                // the new server ends up holding what the old one held; the
                // tree keeps its store ids and refreshes from `StoreOpened`.
                let saved_paths = load_app_state_file();
                store.backend.with(|b| {
                    if let Some(backend) = b {
                        for path in saved_paths {
                            tracing::info!("Auto-opening saved store: {}", path);
                            backend.send(BackendCommand::OpenStore { path });
                        }
                    }
                });

                // A document was being edited across the reconnect: its
                // subscription died with the old connection and any delta
                // sent during the gap was lost. Commands run in order, so
                // the store is open again by the time these execute:
                // subscribe again and reconcile (state vector in, diff out,
                // then our diff back), so nothing typed offline is lost and
                // nothing the new server holds is missed.
                if let Some(active) = untracked(|| store.active_edit.get()) {
                    store.send(BackendCommand::SubscribeNodeChanges {
                        store_id: active.store_id,
                        node_id: active.node_id,
                    });
                    crate::editor::request_reconcile(store, active.store_id, active.node_id);
                }

                // Whether the server's keystore holds a signed-in account
                // (a sign-in survives an app restart), for the status bar
                // and the Account modal's face (docs/DESKTOP_ACCOUNT_CONTRACT.md
                // decision 7).
                store.send(BackendCommand::CloudStatus);
            }

            BackendEvent::Disconnected => {
                tracing::info!("Disconnected from backend");
                store.connection.set(ConnectionState::Disconnected);
                store.connection_status.set("Disconnected".to_string());
            }

            BackendEvent::Error { message } => {
                tracing::error!("Backend error: {}", message);
                // A pending sync-modal request claims this error for its own
                // error line instead of the global status bar — the wire
                // protocol has no per-request tag, but only one such request
                // is ever in flight at a time from these modals.
                if untracked(|| store.connect_modal_pending_add.get()) {
                    store.connect_modal_pending_add.set(false);
                    store.connect_modal_busy.set(false);
                    store.connect_modal_error.set(message.clone());
                } else if untracked(|| store.link_modal_pending.get()) {
                    store.link_modal_pending.set(false);
                    store.link_modal_error.set(message.clone());
                } else if untracked(|| store.remove_replica_modal_pending.get()) {
                    store.remove_replica_modal_pending.set(false);
                    store.remove_replica_modal_error.set(message.clone());
                } else if untracked(|| store.new_store_modal_pending.get()) {
                    store.new_store_modal_pending.set(false);
                    store.new_store_modal_error.set(message.clone());
                } else if untracked(|| store.mount_picker_pending.get()) {
                    store.mount_picker_pending.set(false);
                    store.mount_picker_error.set(message.clone());
                } else if untracked(|| store.deleted_modal_store.get()).is_some() {
                    // "Recently Deleted..." has no pending flag of its own —
                    // `ListDeleted` and a "Put Back" are the only requests it
                    // ever sends, and both land here while it is open.
                    store.deleted_modal_error.set(message.clone());
                } else if let Some(sentence) = refusal_sentence(message) {
                    // The server refused the command; the connection is fine.
                    show_notice(store, sentence.to_string());
                } else {
                    store.connection.set(ConnectionState::Error(message.clone()));
                    store.connection_status.set(format!("Error: {}", message));
                }
            }

            BackendEvent::StoreOpened { store: opened_store } => {
                tracing::info!("Store opened: {}", opened_store.name);
                let store_id = opened_store.id;
                let root_id = opened_store.root_node_id;

                register_opened_store(store, tree_state, opened_store);

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

            BackendEvent::StoreCreated { store_id, root_node_id } => {
                tracing::info!("Store created: {:?} with root {:?}", store_id, root_node_id);
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

            BackendEvent::StoresListed { stores } => {
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
                        refetch_held_nodes(store, s.id);
                        store.bump_tree_structure();
                    }
                }
            }

            BackendEvent::ChildrenLoaded { store_id, parent_id, children_store_id, children } => {
                tracing::info!("Children loaded for {:?}: {} nodes (in store {:?})", parent_id, children.len(), children_store_id);

                // Each child's own canonical store is `children_store_id`
                // (the mount's source store when `parent_id` is a mount
                // point, `store_id` otherwise — decision 1).
                let child_pairs: Vec<(StoreId, NodeId)> = children.iter().map(|n| (*children_store_id, n.id)).collect();

                // Structural: children list changed
                store.set_children(*store_id, *parent_id, child_pairs);

                // The children's store might not be one we know about yet —
                // the server may have opened it implicitly to resolve a mount
                // (decision 4). Discover it via `listStores`.
                let children_store_known = untracked(|| store.store_ids.with(|ids| ids.contains(children_store_id)));
                if !children_store_known {
                    tracing::info!("Unknown store {:?} in ChildrenLoaded; requesting store list", children_store_id);
                    store.send(BackendCommand::ListStores);
                }

                // Collect mount node IDs so we can request their state
                let mount_node_ids: Vec<NodeId> = children.iter()
                    .filter(|child| child.is_mount())
                    .map(|child| child.id)
                    .collect();

                // Upsert each child node (per-entity signal), keyed by its
                // OWN canonical store.
                let mut access_changed: Vec<NodeId> = Vec::new();
                for child in children {
                    let key = (*children_store_id, child.id);
                    if store.get_node_signal(key.0, key.1).is_some() && store.node_held_access(key.0, key.1) != child.access {
                        access_changed.push(child.id);
                    }
                    let should_update = untracked(|| {
                        store.node_data.with(|map| {
                            map.get(&key)
                                .map_or(true, |sig| sig.with(|cached| child.metadata.modified_at >= cached.metadata.modified_at))
                        })
                    });
                    if should_update {
                        store.track_mount_info(*children_store_id, child);
                        store.upsert_node(*children_store_id, child.clone());
                    } else if let Some(sig) = store.get_node_signal(*children_store_id, child.id) {
                        // The cached copy is the newer node, but what this
                        // device may change of it is the server's judgement
                        // at the moment it answered, and this answer is the
                        // latest one (`Node::access`).
                        if untracked(|| sig.with(|cached| cached.access != child.access)) {
                            sig.update(|cached| cached.access = child.access);
                        }
                    }
                }
                refetch_below_changed_access(store, *children_store_id, &access_changed);

                // Request mount state for any mount nodes (addressed in
                // their own store).
                for mount_node_id in mount_node_ids {
                    store.send(BackendCommand::GetMountState {
                        store_id: *children_store_id,
                        node_id: mount_node_id,
                    });
                }

                // Structural change
                store.bump_tree_structure();
            }

            BackendEvent::NodeLoaded { store_id, node } => {
                tracing::info!("Node loaded: {:?} - {}", node.id, node.metadata.title);
                let node_id = node.id;
                let content_bytes = node.content.clone();
                store.track_mount_info(*store_id, node);

                // Whether the node's icon, colour, share marker or access
                // changed against the cache: the row snapshots all four at
                // render time (the access as its menu's disabled items), so
                // that (unlike a title or content change) needs the tree
                // rebuilt to show. A node never cached before counts as changed
                // when it carries any of them — a store's root node arrives
                // after its row was first built.
                let shared = node.metadata.share().is_some();
                let has_row_data = node.metadata.icon().is_some()
                    || node.metadata.color().is_some()
                    || shared
                    || !node.access.allows_write();
                let row_data_changed = untracked(|| {
                    store.node_data.with(|map| {
                        map.get(&(*store_id, node_id)).map_or(has_row_data, |sig| {
                            sig.with(|cached| {
                                cached.metadata.icon() != node.metadata.icon()
                                    || cached.metadata.color() != node.metadata.color()
                                    || cached.metadata.share().is_some() != shared
                                    || cached.access != node.access
                                    // A row not opened yet takes its chevron
                                    // from whether the node lists children.
                                    || cached.children.is_empty() != node.children.is_empty()
                            })
                        })
                    })
                });

                let access_changed = store.get_node_signal(*store_id, node_id).is_some()
                    && store.node_held_access(*store_id, node_id) != node.access;

                // Data-only: updates per-node signal, NO tree rebuild (except for
                // a change to what the row snapshots, below).
                store.upsert_node(*store_id, node.clone());
                if access_changed {
                    refetch_below_changed_access(store, *store_id, &[node_id]);
                }
                if row_data_changed {
                    store.bump_tree_structure();
                }

                if let Some(selected_id) = store.selected_id.get() {
                    if let Some((sel_store_id, Some(sel_node_id))) = parse_tree_value(&selected_id) {
                        if sel_store_id == *store_id && sel_node_id == node_id {
                            store.node_title.set(store.display_label(*store_id, node_id));
                            // First load of the selected document (its content just
                            // arrived from GetNode) — start the editing/collab session.
                            // Guard against restarting an already-active session.
                            let already_editing = untracked(|| store.active_edit.get())
                                .map_or(false, |e| e.store_id == *store_id && e.node_id == node_id);
                            if node.node_type == pimble_core::node_types::DOCUMENT
                                && !already_editing
                            {
                                // `open_node` could not tell this was a document
                                // (a search hit the tree had not loaded), so the
                                // pane is still hidden: show it with the session.
                                store.show_editor.set(true);
                                start_editing(store, *store_id, node_id, &content_bytes);
                            }
                        }
                    }
                }
            }

            BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id } => {
                tracing::info!("Node moved: {:?} from {:?} to {:?}", node_id, old_parent_id, new_parent_id);

                // Update children_of for old parent
                if let Some(old_sig) = store.get_children_signal(*store_id, *old_parent_id) {
                    old_sig.update(|children| {
                        children.retain(|&pair| pair != (*store_id, *node_id));
                    });
                }

                // Update children_of for new parent (avoid nested borrow)
                let new_key = (*store_id, *new_parent_id);
                let new_parent_sig = store.get_children_signal(*store_id, *new_parent_id);
                let new_pair = (*store_id, *node_id);
                if let Some(sig) = new_parent_sig {
                    sig.update(|children| {
                        if !children.contains(&new_pair) {
                            children.push(new_pair);
                        }
                    });
                } else {
                    let new_sig = Signal::new(vec![new_pair]);
                    store.children_of.update(|map| {
                        map.insert(new_key, new_sig);
                    });
                }

                // Auto-expand the new parent so the moved node is visible.
                // `MoveNode` is only ever sent from a drag-and-drop, whose
                // `on_drop` records the exact (possibly mount-path-qualified)
                // tree value it dropped onto — use that instead of
                // reconstructing an unqualified value that would not match a
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

                // Re-fetch from server for authoritative data
                store.backend.with(|b| {
                    if let Some(backend) = b {
                        backend.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *old_parent_id });
                        backend.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *new_parent_id });
                    }
                });

                // Structural change
                store.bump_tree_structure();
            }

            BackendEvent::NodeCreated { store_id, parent_id, node_id } => {
                tracing::info!("Node created: {:?}/{:?} under {:?}", store_id, node_id, parent_id);
                let parent = parent_id.unwrap_or_else(|| {
                    store.root_node_id(*store_id).unwrap()
                });
                // Re-fetch parent's children so the new node appears (triggers ChildrenLoaded → bump)
                store.send(BackendCommand::GetChildren { store_id: *store_id, node_id: parent });
            }

            BackendEvent::NodeContentUpdated { store_id, node_id } => {
                tracing::info!("Node content updated: {:?}/{:?}", store_id, node_id);
                // Only re-fetch if this isn't the currently-selected node.
                let is_selected = store.selected_id.get()
                    .and_then(|sel| parse_tree_value(&sel))
                    .map_or(false, |(sid, nid)| sid == *store_id && nid == Some(*node_id));
                if !is_selected {
                    // Data-only: NodeLoaded will upsert_node without tree rebuild
                    store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
                }
            }

            BackendEvent::NodeRenamed { store_id, node_id } => {
                tracing::info!("Node renamed: {:?}/{:?}", store_id, node_id);
                // Data-only: NodeLoaded will upsert_node without tree rebuild.
                // The per-node signal update triggers only that node's label Effect.
                store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
            }

            BackendEvent::NodeTransplanted {
                from_store_id, old_node_id, old_parent_id, to_store_id, node_id, new_parent_id, title, left_shares,
            } => {
                tracing::info!(
                    "Node {:?}/{:?} transplanted to {:?}/{:?} under {:?}",
                    from_store_id, old_node_id, to_store_id, node_id, new_parent_id
                );

                // The store it left: exactly what `NodeDeleted` does — the
                // old id is a tombstone now, gone from its old parent's list
                // and everything cached under it.
                if let Some(parent_sig) = store.get_children_signal(*from_store_id, *old_parent_id) {
                    parent_sig.update(|children| {
                        children.retain(|&pair| pair != (*from_store_id, *old_node_id));
                    });
                }
                store.remove_subtree(*from_store_id, *old_node_id);

                // The store it landed in: exactly what `NodeCreated` does —
                // ask for the new parent's children again, whether or not
                // they were ever loaded, so the person sees where it landed.
                store.send(BackendCommand::GetChildren { store_id: *to_store_id, node_id: *new_parent_id });

                // The editor follows the node it had open to its new id: a
                // fresh collab session on the new document, the old one
                // closed. Selecting first and then asking for the node is
                // what makes `NodeLoaded`'s own "selected and not already
                // editing" check start that session once the answer lands —
                // the one path a document is ever opened through.
                let was_open = untracked(|| store.active_edit.get())
                    .map_or(false, |active| active.store_id == *from_store_id && active.node_id == *old_node_id);
                if was_open {
                    let value = format!("node_{}_{}", to_store_id, node_id);
                    store.selected_id.set(Some(value.clone()));
                    tree_state.controller.select(&value);
                    store.node_title.set(title.clone());
                    store.show_editor.set(true);
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
            BackendEvent::DeletedListed { store_id, nodes } => {
                tracing::info!("Store {:?}: {} recently deleted", store_id, nodes.len());
                // A late answer for a store the modal has moved on from (or
                // closed) names nothing to fill.
                if untracked(|| store.deleted_modal_store.get()) == Some(*store_id) {
                    store.deleted_modal_nodes.set(nodes.iter().map(crate::state::deleted_row).collect());
                }
            }
            BackendEvent::NodeDeleted { store_id, node_id, parent_id } => {
                tracing::info!("Node deleted: {:?}/{:?}", store_id, node_id);

                // Remove from parent's children signal
                if let Some(parent_sig) = store.get_children_signal(*store_id, *parent_id) {
                    parent_sig.update(|children| {
                        children.retain(|&pair| pair != (*store_id, *node_id));
                    });
                }

                // The server deletes the whole subtree (item 10) — drop every
                // cached descendant too, and clear the selection if it was
                // anywhere inside.
                store.remove_subtree(*store_id, *node_id);

                // Structural change
                store.bump_tree_structure();
            }

            BackendEvent::StoreClosed { store_id } => {
                tracing::info!("Store closed: {:?}", store_id);

                // Structural: remove store and all its signals
                store.remove_store(*store_id);

                // Persist open store paths
                save_app_state_file(&store.all_store_local_paths());

                // Structural change
                store.bump_tree_structure();
            }

            BackendEvent::MountCreated { store_id, parent_id, node_id, mount_ref } => {
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
                // Re-fetch node and mount state
                store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
                store.send(BackendCommand::GetMountState { store_id: *store_id, node_id: *node_id });

                // Load the parent's children so the new mount shows up under
                // it. Unconditional, like the `NodeCreated` answer: the
                // `NodeCreated` notification's own refetch skips a parent
                // whose children were never loaded, which is exactly the case
                // when a mount is created under a collapsed folder.
                store.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *parent_id });

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

            BackendEvent::MountStateChanged { store_id, node_id, state, mount_ref } => {
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

            BackendEvent::RemoteStoreChange { store_id, change_kind, source_client_id } => {
                // Skip our own echoes
                let my_id = untracked(|| store.client_id.get());
                if let Some(source) = source_client_id {
                    if !my_id.is_empty() && source == &my_id {
                        continue;
                    }
                }
                tracing::info!("Remote store change: {:?} - {:?}", store_id, change_kind);
                use pimble_rpc::StoreChangeKind;
                match change_kind {
                    StoreChangeKind::NodeCreated { parent_id, .. } => {
                        refetch_parent_children(store, *store_id, *parent_id);
                    }
                    StoreChangeKind::NodeDeleted { node_id, parent_id } => {
                        // Instant feedback ahead of the refetch below: drop
                        // the node from its parent's loaded list right away.
                        if let Some(parent_sig) = store.get_children_signal(*store_id, *parent_id) {
                            parent_sig.update(|children| {
                                children.retain(|&pair| pair != (*store_id, *node_id));
                            });
                        }
                        // The server deletes the whole subtree (item 10); a
                        // notification names only its root.
                        store.remove_subtree(*store_id, *node_id);
                        refetch_parent_children(store, *store_id, *parent_id);
                        store.bump_tree_structure();
                    }
                    StoreChangeKind::NodeMoved { old_parent_id, new_parent_id, .. } => {
                        refetch_parent_children(store, *store_id, *old_parent_id);
                        refetch_parent_children(store, *store_id, *new_parent_id);
                    }
                    StoreChangeKind::MetadataUpdated { node_id } => {
                        store.send(BackendCommand::GetNode {
                            store_id: *store_id,
                            node_id: *node_id,
                        });
                    }
                    StoreChangeKind::ContentUpdated { node_id } => {
                        // CRDT-edited nodes are synced by the subscription task directly.
                        let is_active_crdt = untracked(|| {
                            store.active_edit.with(|ae| {
                                ae.as_ref().map_or(false, |e| e.store_id == *store_id && e.node_id == *node_id)
                            })
                        });

                        if !is_active_crdt {
                            store.send(BackendCommand::GetNode {
                                store_id: *store_id,
                                node_id: *node_id,
                            });
                        }
                    }
                    StoreChangeKind::TreeStructure { node_ids } => {
                        // Every listed id's own loaded children (it may be a
                        // parent whose list changed) and its cached parent's
                        // loaded children (it may have moved in or out of a
                        // list) — `refetch_parent_children` covers mounts
                        // sourced from each pair too. Collect the distinct
                        // parent ids first: a full reconcile can list many
                        // children of the same folder, and each parent must
                        // be refetched once, not once per listed child.
                        let mut parents: HashSet<NodeId> = HashSet::new();
                        for &id in node_ids {
                            parents.insert(id);
                            if let Some(parent_id) = store.cached_parent_id(*store_id, id) {
                                parents.insert(parent_id);
                            }
                        }
                        for parent_id in parents {
                            refetch_parent_children(store, *store_id, parent_id);
                        }
                    }
                    StoreChangeKind::SyncStateChanged { state } => {
                        tracing::info!("Sync state of {:?}: {:?}", store_id, state);
                        // Carries only the state — keep whatever remote endpoint
                        // is already known (decision 6, docs/SYNC_CONTRACT.md).
                        store.update_sync_state(*store_id, state.clone());
                        if matches!(state, pimble_core::SyncState::Synced { .. }) {
                            refetch_root_if_empty(store, *store_id);
                        }
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
                    StoreChangeKind::VaultAppended { .. } => {
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

            BackendEvent::NodeContentReconciled { store_id, node_id, diff, server_state_vector } => {
                crate::editor::apply_reconcile(store, *store_id, *node_id, diff, server_state_vector);
            }

            BackendEvent::RemoteChanges { changes } => {
                use base64::Engine;
                // A peer's delta: integrate it into the editor's collab session (which
                // re-projects the view and does NOT re-broadcast).
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(changes) {
                    apply_remote(&bytes);
                }
            }

            BackendEvent::SearchResults { results } => {
                match results {
                    Ok(items) => store.search_results.set(SearchState::Results(items.clone())),
                    Err(message) => store.search_results.set(SearchState::Error(message.clone())),
                }
            }

            BackendEvent::IndexRebuilt { store_id, indexed } => {
                tracing::info!("Search index rebuilt for store {:?}: {} node(s) indexed", store_id, indexed);
            }

            BackendEvent::RemoteStoresListed { url, result } => {
                // Guard against a stale answer landing after the modal's URL
                // changed and "List stores" was clicked again.
                if untracked(|| store.connect_modal_url.get()) != *url {
                    continue;
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

            BackendEvent::StoreSyncChanged { store_id, remote, state, sync_mode, access, read_only_roots, ended_roots, relay, owner_offline } => {
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
                    refetch_held_nodes(store, *store_id);
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

                if matches!(state, pimble_core::SyncState::Synced { .. }) {
                    refetch_root_if_empty(store, *store_id);
                }
            }

            BackendEvent::HostedStoreCreated { name } => {
                tracing::info!("Hosted store created: {}", name);
                // The store itself arrives through `StoresListed`, once the
                // backend has a token carrying the new grant. Nothing to do
                // here but close the modal.
                store.new_store_modal_pending.set(false);
                store.new_store_modal_open.set(false);
                store.new_store_modal_name.set(String::new());
                store.new_store_modal_error.set(String::new());
            }

            BackendEvent::ReplicaRemoved { store_id } => {
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

            // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md "Events").
            BackendEvent::CloudStatusChanged { signed_in, email, url } => {
                tracing::info!("Cloud account: signed_in={} email={:?}", signed_in, email);
                store.cloud_signed_in.set(*signed_in);
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

            BackendEvent::CloudError { op, message } => {
                tracing::warn!("Cloud {:?} failed: {}", op, message);
                match op {
                    CloudOp::SignIn | CloudOp::SignOut => {
                        store.account_modal_busy.set(false);
                        store.account_modal_error.set(message.clone());
                    }
                    CloudOp::Status => {
                        // Asked on every connect; only worth a line in the
                        // modal when the modal is there to show it.
                        if untracked(|| store.account_modal_open.get()) {
                            store.account_modal_busy.set(false);
                            store.account_modal_error.set(message.clone());
                        }
                    }
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

            BackendEvent::CloudHostedStoresListed { stores, relayed } => {
                // Only an encrypted store not already open here can be added
                // as a replica (the server refuses an open one anyway), so
                // the modal lists just those (decision 6). A share is the
                // exception: a second share of a store already open here as a
                // partial replica extends that replica with another root, so
                // it is listed as long as this device does not hold that root
                // already (docs/NODE_DOCUMENT_CONTRACT.md section 5).
                let open: Vec<StoreId> = untracked(|| store.store_ids.get());
                let mut listed: Vec<String> = Vec::new();
                let candidates: Vec<pimble_rpc::CloudHostedStoreInfo> = stores
                    .iter()
                    .filter(|s| s.kind == "vault")
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
                store.hosted_modal_relayed.set(relayed.clone());
                store.hosted_modal_stores.set(candidates);
                store.hosted_modal_busy.set(false);
                store.hosted_modal_error.set(String::new());
            }

            BackendEvent::CloudStoreHosted { store_id } => {
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

            BackendEvent::CloudStoreRelayed { store_id } => {
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

            BackendEvent::CloudRelayingStopped { store_id } => {
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

            // The answer to every sharing request but "Stop sharing"
            // (docs/NODE_DOCUMENT_CONTRACT.md section 5): the share as the accounts
            // service has it now, and everyone on it.
            BackendEvent::CloudShareUpdated { store_id, node_id, share, members } => {
                tracing::info!("Share of {:?}/{:?}: {:?}, {} members", store_id, node_id, share.state, members.len());
                if untracked(|| store.share_modal_node.get()) == Some((*store_id, *node_id)) {
                    store.share_modal_shared.set(true);
                    store.share_modal_name.set(share.name.clone());
                    store.share_modal_state.set(share_state_text(&share.state).to_string());
                    store.share_modal_members.set(members.clone());
                    store.share_modal_invite_email.set(String::new());
                    store.share_modal_pending.set(None);
                    store.share_modal_error.set(String::new());
                }
                // The marker the server wrote is what the tree's badge reads.
                store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
            }

            BackendEvent::CloudSharingStopped { store_id, node_id } => {
                tracing::info!("Stopped sharing {:?}/{:?}", store_id, node_id);
                if untracked(|| store.share_modal_node.get()) == Some((*store_id, *node_id)) {
                    store.share_modal_node.set(None);
                    store.share_modal_shared.set(false);
                    store.share_modal_members.set(Vec::new());
                    store.share_modal_confirm_stop.set(false);
                    store.share_modal_pending.set(None);
                    store.share_modal_error.set(String::new());
                }
                // The marker is gone, so the badge goes with it.
                store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;
    use pimble_core::{NodeId, StoreId, SyncState};
    use pimble_rpc::{MemberRole, ShareInfo, ShareMember, ShareMemberStatus};

    use crate::protocol::BackendHandle;

    /// An `AppStore` with a backend whose events this test feeds by hand, and
    /// the command channel the handlers send into, so a test can see what a
    /// handler asked the server for.
    fn store_with_events() -> (AppStore, crossbeam_channel::Sender<BackendEvent>, crossbeam_channel::Receiver<BackendCommand>) {
        let store = AppStore::new();
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(64);
        let (event_tx, event_rx) = bounded::<BackendEvent>(64);
        store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
        (store, event_tx, cmd_rx)
    }

    fn pump(store: AppStore) {
        process_backend_events(store, UseTreeReturn::new(UseTreeOptions::default()));
    }

    fn share_info(store_id: StoreId, node_id: NodeId, name: &str) -> ShareInfo {
        ShareInfo {
            store_id,
            node_id,
            name: name.to_string(),
            state: SyncState::Syncing,
        }
    }

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
                    ShareMember { email: "me@example.com".into(), role: MemberRole::Owner, status: ShareMemberStatus::Active },
                    ShareMember { email: "ann@example.com".into(), role: MemberRole::Editor, status: ShareMemberStatus::Invited },
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
        assert!(commands
            .try_iter()
            .any(|cmd| matches!(cmd, BackendCommand::GetNode { node_id: n, .. } if n == node_id)));
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
        assert!(commands
            .try_iter()
            .any(|cmd| matches!(cmd, BackendCommand::GetNode { node_id: n, .. } if n == node_id)));
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
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == placeholder)));

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
            asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == recipes)),
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

    /// "Add Hosted Store..." lists a share of a store already open here when
    /// this device does not hold that share's root yet — the server extends
    /// the partial replica — and never one whose root it holds. Two pending
    /// shares of one store are one row, because the RPC names a store rather
    /// than a scope (docs/NODE_DOCUMENT_CONTRACT.md section 5).
    #[test]
    fn the_hosted_list_offers_a_second_share_of_an_open_store() {
        let (store, events, _commands) = store_with_events();
        let mut held = pimble_core::Store::new_local("Ann's notes", "/tmp/anns.pimble".into());
        let (recipes, trips) = (NodeId::new(), NodeId::new());
        held.root_node_id = recipes;
        held.roots = vec![recipes];
        let store_id = held.id;
        store.upsert_store(held);

        let other = uuid::Uuid::new_v4().to_string();
        let row = |id: &str, name: &str, root: Option<NodeId>| pimble_rpc::CloudHostedStoreInfo {
            store_id: id.to_string(),
            name: name.to_string(),
            role: "editor".to_string(),
            kind: "vault".to_string(),
            created_at: String::new(),
            root,
            shared_by: root.map(|_| "ann@example.com".to_string()),
        };
        events
            .send(BackendEvent::CloudHostedStoresListed {
                stores: vec![
                    // Held already: not offered again.
                    row(&store_id.to_string(), "Recipes", Some(recipes)),
                    // A second share of the same store: offered, once.
                    row(&store_id.to_string(), "Trips", Some(trips)),
                    row(&store_id.to_string(), "Walks", Some(NodeId::new())),
                    row(&other, "Ledger", None),
                ],
                // The rows served from their owner's computer ride beside
                // the list, and the modal's labels read them.
                relayed: vec![other.clone()],
            })
            .unwrap();
        pump(store);
        assert_eq!(store.hosted_modal_relayed.get(), vec![other.clone()]);

        let offered: Vec<(String, String)> =
            store.hosted_modal_stores.with(|v| v.iter().map(|s| (s.store_id.clone(), s.name.clone())).collect());
        assert_eq!(
            offered,
            vec![(store_id.to_string(), "Trips".to_string()), (other, "Ledger".to_string())]
        );
    }

    /// A store shared with this device arrives as a partial replica: the app
    /// asks for each shared root and its children, not just the first, and a
    /// second share of the same store extends the list in place
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "The recipient's replica").
    #[test]
    fn a_partial_replica_fetches_every_shared_root() {
        let (store, events, commands) = store_with_events();
        let mut shared = pimble_core::Store::new_local("Ann's notes", "/tmp/anns.pimble".into());
        let (recipes, trips) = (NodeId::new(), NodeId::new());
        shared.root_node_id = recipes;
        shared.roots = vec![recipes, trips];
        shared.shared_by = Some("ann@example.com".to_string());
        let store_id = shared.id;

        events.send(BackendEvent::StoreOpened { store: shared.clone() }).unwrap();
        pump(store);

        let asked: Vec<BackendCommand> = commands.try_iter().collect();
        for root in [recipes, trips] {
            assert!(
                asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == root)),
                "no children asked for {root}"
            );
            assert!(
                asked.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == root)),
                "the root node itself was never fetched for {root}"
            );
        }
        assert_eq!(store.shown_roots(store_id), vec![recipes, trips]);

        // A second share of the same store: the server extends the replica and
        // answers with the longer list, which has to replace the held one.
        let walks = NodeId::new();
        let mut extended = shared.clone();
        extended.roots = vec![recipes, trips, walks];
        events.send(BackendEvent::StoreOpened { store: extended }).unwrap();
        pump(store);
        assert_eq!(store.shown_roots(store_id), vec![recipes, trips, walks]);
        assert!(commands
            .try_iter()
            .any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if node_id == walks)));
    }

    /// A store with two shared roots and a document under each, all loaded,
    /// as the tree holds a partial replica.
    fn a_replica_with_two_roots(store: AppStore) -> (StoreId, [NodeId; 2], [NodeId; 2]) {
        let mut shared = pimble_core::Store::new_local("Ann's notes", "/tmp/anns.pimble".into());
        let (recipes, trips) = (pimble_core::Node::folder("Recipes"), pimble_core::Node::folder("Trips"));
        let (pasta, rome) = (pimble_core::Node::document("Pasta"), pimble_core::Node::document("Rome"));
        shared.root_node_id = recipes.id;
        shared.roots = vec![recipes.id, trips.id];
        shared.shared_by = Some("ann@example.com".to_string());
        let store_id = shared.id;
        store.upsert_store(shared);
        let ids = ([recipes.id, trips.id], [pasta.id, rome.id]);
        store.set_children(store_id, recipes.id, vec![(store_id, pasta.id)]);
        store.set_children(store_id, trips.id, vec![(store_id, rome.id)]);
        for node in [recipes, trips, pasta, rome] {
            store.upsert_node(store_id, node);
        }
        (store_id, ids.0, ids.1)
    }

    /// Built through serde: this crate has no chrono of its own.
    fn synced() -> SyncState {
        serde_json::from_str(r#"{"state":"synced","last_sync":"2026-09-21T00:00:00Z"}"#).unwrap()
    }

    fn sync_changed(store_id: StoreId, access: pimble_core::StoreAccess, read_only_roots: Vec<NodeId>) -> BackendEvent {
        BackendEvent::StoreSyncChanged {
            store_id,
            remote: None,
            state: synced(),
            sync_mode: pimble_core::StoreKind::Vault,
            access,
            read_only_roots,
            ended_roots: Vec::new(),
            relay: pimble_core::RelaySide::None,
            owner_offline: false,
        }
    }

    /// A role the owner changes while the app runs reaches it with the next
    /// `GetStoreSync` answer: the store's `access` and `read_only_roots` are
    /// written, and because each node carries the server's judgement of
    /// itself, everything held of the store is asked for again: the loaded
    /// lists, the roots, the open document. An answer that changes nothing
    /// asks for nothing.
    #[test]
    fn a_changed_access_is_kept_and_refetches_what_is_held() {
        use pimble_core::StoreAccess::{Full, Read};
        let (store, events, commands) = store_with_events();
        let (store_id, [recipes, trips], [pasta, rome]) = a_replica_with_two_roots(store);
        store.active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: rome }));

        events.send(sync_changed(store_id, Full, Vec::new())).unwrap();
        pump(store);
        assert!(
            !commands.try_iter().any(|c| matches!(c, BackendCommand::GetChildren { .. } | BackendCommand::GetNode { .. })),
            "nothing changed, so nothing is fetched"
        );

        // The owner made this account a reader of Trips.
        let before = store.tree_structure_version.get();
        events.send(sync_changed(store_id, Full, vec![trips])).unwrap();
        pump(store);
        let held = store.get_store_signal(store_id).unwrap();
        assert_eq!(held.with(|s| (s.access, s.read_only_roots.clone())), (Full, vec![trips]));
        let asked: Vec<BackendCommand> = commands.try_iter().collect();
        for parent in [recipes, trips] {
            assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == parent)), "the list under {parent} was not fetched again");
            assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == parent)), "the root {parent} was not fetched again");
        }
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == rome)), "nor the open document");
        assert!(store.tree_structure_version.get() > before);

        // The answers carry the judgement, and the app believes it: the
        // document under Trips takes no edits now, its neighbour still does.
        let mut rome_now = store.get_node_signal(store_id, rome).unwrap().with(|n| n.clone());
        rome_now.access = Read;
        let before = store.tree_structure_version.get();
        events.send(BackendEvent::NodeLoaded { store_id, node: rome_now }).unwrap();
        pump(store);
        assert_eq!(store.node_access(store_id, rome), Read);
        assert_eq!(store.node_access(store_id, pasta), Full);
        assert!(store.tree_structure_version.get() > before, "the row's menu never re-renders");

        // The whole replica to read: the store's own word covers every node.
        events.send(sync_changed(store_id, Read, Vec::new())).unwrap();
        pump(store);
        assert_eq!(held.with(|s| (s.access, s.read_only_roots.clone())), (Read, Vec::new()));
        assert_eq!(store.node_access(store_id, pasta), Read);
        assert!(commands.try_iter().any(|c| matches!(c, BackendCommand::GetChildren { .. })));
    }

    /// A children list brings each child's access even when the cached copy
    /// of the child is the newer node and is kept.
    #[test]
    fn a_children_list_brings_access_to_a_newer_cached_node() {
        use pimble_core::StoreAccess::{Full, Read};
        let (store, events, _commands) = store_with_events();
        let (store_id, [_, trips], [_, rome]) = a_replica_with_two_roots(store);

        // The list's copy of the node is a minute older than the held one.
        let held_now = store.get_node_signal(store_id, rome).unwrap().with(|n| n.clone());
        let mut listed = held_now.clone();
        let mut newer = held_now;
        newer.metadata.modified_at += std::time::Duration::from_secs(60);
        store.upsert_node(store_id, newer);
        listed.metadata.title = "An older title".to_string();
        listed.access = Read;
        events
            .send(BackendEvent::ChildrenLoaded { store_id, parent_id: trips, children_store_id: store_id, children: vec![listed] })
            .unwrap();
        pump(store);

        let held = store.get_node_signal(store_id, rome).unwrap();
        assert_eq!(held.with(|n| n.metadata.title.clone()), "Rome", "the newer node is kept");
        assert_eq!(store.node_access(store_id, rome), Read, "with the latest judgement of it");
        assert_eq!(store.node_access(store_id, trips), Full);
    }

    /// The owner moves a folder from a root this account edits into one it
    /// reads: the move's notifications name the two lists it moved between,
    /// and the folder comes back in one of them with another access. What is
    /// under it changed with it, so its own loaded list is fetched again, and
    /// the open document; the same answer a second time asks for nothing.
    #[test]
    fn a_node_whose_access_changed_refetches_what_is_under_it() {
        use pimble_core::StoreAccess::Read;
        let (store, events, commands) = store_with_events();
        let (store_id, [recipes, trips], [pasta, _]) = a_replica_with_two_roots(store);
        store.active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: pasta }));

        let mut moved = store.get_node_signal(store_id, trips).unwrap().with(|n| n.clone());
        moved.access = Read;
        let listing = BackendEvent::ChildrenLoaded { store_id, parent_id: recipes, children_store_id: store_id, children: vec![moved] };
        events.send(listing.clone()).unwrap();
        pump(store);
        let asked: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == trips)), "what is under the folder keeps its old access");
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == pasta)), "and so may the open document");

        events.send(listing).unwrap();
        pump(store);
        assert!(!commands.try_iter().any(|c| matches!(c, BackendCommand::GetChildren { .. } | BackendCommand::GetNode { .. })), "nothing changed the second time");
    }

    /// A folder that was never opened shows a chevron from its node's own
    /// `children`. When its first child turns up after the folder's own change
    /// was heard (a document made on another device, readable here only once
    /// its key wraps had been fetched), the child's arrival has to refresh
    /// the folder's node, and a node that starts listing children has to
    /// rebuild its row, or the child stays out of reach.
    #[test]
    fn a_child_arriving_late_gives_its_unopened_folder_a_chevron() {
        let (store, events, commands) = store_with_events();
        let (store_id, [recipes, _], [pasta, _]) = a_replica_with_two_roots(store);
        let walks = pimble_core::Node::folder("Walks");
        let walks_id = walks.id;
        store.set_children(store_id, recipes, vec![(store_id, pasta), (store_id, walks_id)]);
        store.upsert_node(store_id, walks.clone());
        assert!(!store.has_children_loaded(store_id, walks_id));

        let storr = NodeId::new();
        events
            .send(BackendEvent::RemoteStoreChange {
                store_id,
                change_kind: pimble_rpc::StoreChangeKind::NodeCreated { node_id: storr, parent_id: walks_id },
                source_client_id: None,
            })
            .unwrap();
        pump(store);
        let asked: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == walks_id)), "the folder's node is asked for again: {asked:?}");
        assert!(!asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == walks_id)), "its list was never loaded and is not loaded now");

        let before = untracked(|| store.tree_structure_version.get());
        let mut listing = walks;
        listing.children = vec![storr];
        events.send(BackendEvent::NodeLoaded { store_id, node: listing }).unwrap();
        pump(store);
        assert_ne!(untracked(|| store.tree_structure_version.get()), before, "a node that starts listing children rebuilds its row");
    }

    /// A link reads how the account holds the store each time it connects,
    /// and says only `SyncStateChanged` when it has: the app asks for the
    /// rest. Not on `Syncing`, which a link announces before it has read
    /// anything.
    #[test]
    fn a_links_transition_asks_how_the_store_is_held_now() {
        let (store, events, commands) = store_with_events();
        let (store_id, _, _) = a_replica_with_two_roots(store);
        let notify = |state: SyncState| BackendEvent::RemoteStoreChange {
            store_id,
            change_kind: pimble_rpc::StoreChangeKind::SyncStateChanged { state },
            source_client_id: None,
        };
        let asked = |commands: &crossbeam_channel::Receiver<BackendCommand>| {
            commands.try_iter().any(|c| matches!(c, BackendCommand::GetStoreSync { store_id: s } if s == store_id))
        };

        events.send(notify(SyncState::Syncing)).unwrap();
        pump(store);
        assert!(!asked(&commands));
        events.send(notify(synced())).unwrap();
        pump(store);
        assert!(asked(&commands), "connected again: the grant was read");
        events.send(notify(SyncState::Offline)).unwrap();
        pump(store);
        assert!(asked(&commands), "refused or unreachable: a removal from every share is recorded before the connect fails");
    }

    /// A node that has just been shared must re-render its row, and rinch
    /// re-renders one only when its data changes — so the marker arriving
    /// bumps the tree (docs/NODE_DOCUMENT_CONTRACT.md section 5).
    #[test]
    fn a_new_share_marker_rebuilds_the_row() {
        let (store, events, _commands) = store_with_events();
        let store_id = StoreId::new();
        let mut node = pimble_core::Node::document("Recipes");
        store.upsert_node(store_id, node.clone());
        let before = store.tree_structure_version.get();

        node.metadata.set_share(Some(&pimble_core::ShareMarker {
            v: pimble_core::ShareMarker::VERSION,
            key_id: uuid::Uuid::new_v4(),
            url: "https://pimble.app".to_string(),
            name: "Recipes".to_string(),
        }));
        events.send(BackendEvent::NodeLoaded { store_id, node: node.clone() }).unwrap();
        pump(store);
        assert!(store.tree_structure_version.get() > before, "the row never re-renders");

        // The same node again changes nothing the row draws.
        let after = store.tree_structure_version.get();
        events.send(BackendEvent::NodeLoaded { store_id, node }).unwrap();
        pump(store);
        assert_eq!(store.tree_structure_version.get(), after);
    }

    // ── A share that ended (Joe, 2026-09-21) ────────────────────────────

    /// A replica of two shares with a document under each, parents and lists
    /// as the server sends them, and the document under Trips open.
    fn a_replica_with_rome_open(store: AppStore) -> (StoreId, [NodeId; 2], [NodeId; 2]) {
        let mut shared = pimble_core::Store::new_local("Shared by ann@example.com", "/tmp/anns.pimble".into());
        let (mut recipes, mut trips) = (pimble_core::Node::folder("Recipes"), pimble_core::Node::folder("Trips"));
        let (mut pasta, mut rome) = (pimble_core::Node::document("Pasta"), pimble_core::Node::document("Rome"));
        pasta.parent_id = Some(recipes.id);
        rome.parent_id = Some(trips.id);
        recipes.children = vec![pasta.id];
        trips.children = vec![rome.id];
        shared.root_node_id = recipes.id;
        shared.roots = vec![recipes.id, trips.id];
        shared.shared_by = Some("ann@example.com".to_string());
        shared.is_replica = true;
        let store_id = shared.id;
        store.upsert_store(shared);
        store.set_children(store_id, recipes.id, vec![(store_id, pasta.id)]);
        store.set_children(store_id, trips.id, vec![(store_id, rome.id)]);
        let ids = ([recipes.id, trips.id], [pasta.id, rome.id]);
        for node in [recipes, trips, pasta, rome] {
            store.upsert_node(store_id, node);
        }
        store.selected_id.set(Some(format!("node_{store_id}_{}", ids.1[1])));
        store.node_title.set("Rome".to_string());
        store.show_editor.set(true);
        store.active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: ids.1[1] }));
        (store_id, ids.0, ids.1)
    }

    fn shares_ended(store_id: StoreId, node_ids: Vec<NodeId>) -> BackendEvent {
        BackendEvent::RemoteStoreChange { store_id, change_kind: pimble_rpc::StoreChangeKind::SharesEnded { node_ids }, source_client_id: None }
    }

    fn row_values(store: AppStore) -> Vec<String> {
        untracked(|| store.build_tree_data_structural())[0].children.iter().map(|row| row.value.clone()).collect()
    }

    /// The owner removed this account from Trips while the app was running:
    /// the folder leaves the explorer with one sentence that names it,
    /// everything held of it is dropped, the document open under it closes,
    /// and the share still held is untouched. Nothing is asked of the
    /// server but how the store is held now.
    #[test]
    fn a_share_that_ended_leaves_the_explorer_with_a_notice() {
        let (store, events, commands) = store_with_events();
        let (store_id, [recipes, trips], [pasta, rome]) = a_replica_with_rome_open(store);
        let before = store.tree_structure_version.get();

        events.send(shares_ended(store_id, vec![trips])).unwrap();
        pump(store);

        assert_eq!(store.notice.get(), "\"Trips\" is no longer shared with you.");
        assert_eq!(store.shown_roots(store_id), vec![recipes]);
        assert_eq!(row_values(store), vec![format!("node_{store_id}_{recipes}")]);
        assert!(store.tree_structure_version.get() > before, "the tree is rebuilt");
        for dropped in [trips, rome] {
            assert!(store.get_node_signal(store_id, dropped).is_none(), "{dropped} is still held");
            assert!(!store.has_children_loaded(store_id, dropped));
        }
        for kept in [recipes, pasta] {
            assert!(store.get_node_signal(store_id, kept).is_some());
        }
        // The open document was under it: closed as a deleted node's is.
        assert_eq!(store.selected_id.get(), None);
        assert!(!store.show_editor.get());
        assert_eq!(store.node_title.get(), "");

        let asked: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(asked.is_empty(), "nothing is asked of the server, and nothing deleted: the files stay until the person removes the replica ({} command(s))", asked.len());

        // Told again (the answer to `GetStoreSync` names it too): nothing more.
        let settled = store.tree_structure_version.get();
        store.notice.set(String::new());
        let mut answer = sync_changed(store_id, pimble_core::StoreAccess::Full, vec![trips]);
        if let BackendEvent::StoreSyncChanged { ended_roots, .. } = &mut answer {
            *ended_roots = vec![trips];
        }
        events.send(shares_ended(store_id, vec![trips])).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "", "one notice per folder");
        assert_eq!(store.tree_structure_version.get(), settled, "and the tree is left alone");
        events.send(answer).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "");
        assert_eq!(store.shown_roots(store_id), vec![recipes]);
    }

    /// A document open under the share that is still held stays open, and a
    /// folder whose title the app never loaded is not named.
    #[test]
    fn a_share_that_ended_closes_nothing_else_and_names_only_what_it_knows() {
        let (store, events, _commands) = store_with_events();
        let (store_id, [recipes, trips], [pasta, _]) = a_replica_with_rome_open(store);
        store.selected_id.set(Some(format!("node_{store_id}_{pasta}")));
        store.active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: pasta }));
        store.remove_node(store_id, trips);

        events.send(shares_ended(store_id, vec![trips])).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "A shared folder is no longer shared with you.");
        assert_eq!(store.shown_roots(store_id), vec![recipes]);
        assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{pasta}")));
        assert!(store.show_editor.get());
        assert!(store.active_edit.get().is_some_and(|active| active.node_id == pasta));
    }

    /// Removed from every share at once: a sentence for each, no folder
    /// left, and the store row stays with "no longer shared" and a menu that
    /// offers "Remove Replica..." and nothing that changes anything.
    #[test]
    fn the_last_share_to_end_leaves_the_row_and_its_removal() {
        let (store, events, _commands) = store_with_events();
        let (store_id, [recipes, trips], _) = a_replica_with_rome_open(store);

        events.send(shares_ended(store_id, vec![recipes, trips])).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "\"Recipes\" is no longer shared with you. \"Trips\" is no longer shared with you.");
        assert!(store.every_share_ended(store_id));
        let data = untracked(|| store.build_tree_data_structural());
        assert_eq!(data.len(), 1, "the store row stays");
        assert!(data[0].children.is_empty());
        assert!(store.selected_id.get().is_none() && !store.show_editor.get());

        let held = store.get_store_signal(store_id).unwrap();
        assert_eq!(held.with(crate::state::shared_by_words), "no longer shared");
        let menu = crate::state::store_row_menu(crate::state::StoreRowFacts {
            access: store.store_access(store_id),
            linked: true,
            replica: held.with(|s| s.is_replica),
            vault: false,
            relay: pimble_core::RelaySide::None,
            held_as_share: true,
            shared_root: false,
            mount_source_copied: false,
            every_share_ended: store.every_share_ended(store_id),
        });
        assert!(!menu.remove_replica);
        assert!(menu.new_node && menu.unlink && menu.link_to_remote && menu.share && menu.appearance && menu.copy_as_mount_source);
    }

    /// Overlapping shares: a folder shared inside a shared folder. When the
    /// inner share ends its row under the store row goes and the notice is
    /// shown, and everything in it is still there under the outer share,
    /// the open document included.
    #[test]
    fn a_share_inside_another_that_ended_is_still_shown_under_the_outer_one() {
        let (store, events, _commands) = store_with_events();
        let (store_id, [recipes, trips], [_, rome]) = a_replica_with_rome_open(store);
        // Trips is inside Recipes, and shared on its own as well.
        store.get_node_signal(store_id, trips).unwrap().update(|n| n.parent_id = Some(recipes));

        events.send(shares_ended(store_id, vec![trips])).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "\"Trips\" is no longer shared with you.");
        assert_eq!(store.shown_roots(store_id), vec![recipes]);
        assert!(store.get_node_signal(store_id, trips).is_some() && store.get_node_signal(store_id, rome).is_some(), "still held: they are under Recipes");
        assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{rome}")), "and the open document stays open");
        assert!(store.show_editor.get());
    }

    /// At start-up an ended share is simply not shown: no notice (the person
    /// was told when it happened, or was not there), and nothing of it is
    /// fetched.
    #[test]
    fn an_ended_share_is_not_shown_at_start_up() {
        let (store, events, commands) = store_with_events();
        let mut opened = pimble_core::Store::new_local("Shared by ann@example.com", "/tmp/anns.pimble".into());
        let (recipes, trips) = (NodeId::new(), NodeId::new());
        opened.root_node_id = recipes;
        opened.roots = vec![recipes, trips];
        opened.ended_roots = vec![trips];
        let store_id = opened.id;

        events.send(BackendEvent::StoreOpened { store: opened.clone() }).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "");
        assert_eq!(store.shown_roots(store_id), vec![recipes]);
        let asked: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == recipes)));
        assert!(!asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } | BackendCommand::GetNode { node_id, .. } if *node_id == trips)));

        // The same store told again with the root it was showing ended: that
        // one was on the screen, so it is said.
        let mut recipes_node = pimble_core::Node::folder("Recipes");
        recipes_node.id = recipes;
        store.upsert_node(store_id, recipes_node);
        opened.ended_roots = vec![trips, recipes];
        events.send(BackendEvent::StoreOpened { store: opened }).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "\"Recipes\" is no longer shared with you.");
        assert!(store.every_share_ended(store_id));
        assert!(store.get_node_signal(store_id, recipes).is_none());
    }

    /// How the store is held, asked of the server, names the ended shares
    /// too: one that ended while nothing was listening leaves the explorer
    /// on that answer, and one granted again comes back and is fetched.
    #[test]
    fn the_answer_to_how_a_store_is_held_ends_and_un_ends_shares() {
        let (store, events, commands) = store_with_events();
        let (store_id, [recipes, trips], _) = a_replica_with_rome_open(store);
        let answer = |ended: Vec<NodeId>| {
            let mut answer = sync_changed(store_id, pimble_core::StoreAccess::Full, ended.clone());
            if let BackendEvent::StoreSyncChanged { ended_roots, .. } = &mut answer {
                *ended_roots = ended;
            }
            answer
        };

        events.send(answer(vec![trips])).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "\"Trips\" is no longer shared with you.");
        assert_eq!(store.shown_roots(store_id), vec![recipes]);
        assert!(store.selected_id.get().is_none() && !store.show_editor.get(), "the document open under it closed");
        let _ = commands.try_iter().count();

        // A share granted again: the server says the set changed and names
        // nothing that ended, and the app asks how the store is held.
        events.send(shares_ended(store_id, Vec::new())).unwrap();
        pump(store);
        assert!(commands.try_iter().any(|c| matches!(c, BackendCommand::GetStoreSync { store_id: asked_for } if asked_for == store_id)));
        assert_eq!(store.shown_roots(store_id), vec![recipes], "nothing changes until the answer");

        events.send(answer(Vec::new())).unwrap();
        pump(store);
        assert_eq!(store.shown_roots(store_id), vec![recipes, trips]);
        let asked: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetNode { node_id, .. } if *node_id == trips)), "the root that came back is fetched");
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { node_id, .. } if *node_id == trips)));
    }

    /// The browser holds no replica: its backend says which shares ended and
    /// then lists the store with the roots that are left. The listing drops
    /// nothing twice and says nothing twice; a root that is simply gone from
    /// a listing leaves without a word.
    #[test]
    fn a_listing_without_a_root_drops_it_and_says_nothing_more() {
        let (store, events, _commands) = store_with_events();
        let (store_id, [recipes, trips], _) = a_replica_with_rome_open(store);
        let listed = |roots: Vec<NodeId>| {
            let mut listed = store.get_store_signal(store_id).unwrap().with(|s| s.clone());
            listed.roots = roots;
            listed.ended_roots = Vec::new();
            BackendEvent::StoresListed { stores: vec![listed] }
        };

        events.send(shares_ended(store_id, vec![trips])).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "\"Trips\" is no longer shared with you.");
        store.notice.set(String::new());
        events.send(listed(vec![recipes])).unwrap();
        pump(store);
        assert_eq!(store.notice.get(), "");
        assert_eq!(store.shown_roots(store_id), vec![recipes]);
        assert_eq!(row_values(store), vec![format!("node_{store_id}_{recipes}")]);

        // The same listing again changes nothing.
        let settled = store.tree_structure_version.get();
        events.send(listed(vec![recipes])).unwrap();
        pump(store);
        assert_eq!(store.tree_structure_version.get(), settled);
    }

    fn left_share(name: &str) -> pimble_core::LeftShare {
        pimble_core::LeftShare { root: NodeId::new(), name: name.to_string() }
    }

    fn transplanted(
        from_store_id: StoreId,
        old_node_id: NodeId,
        old_parent_id: NodeId,
        to_store_id: StoreId,
        node_id: NodeId,
        new_parent_id: NodeId,
        title: &str,
        left_shares: Vec<pimble_core::LeftShare>,
    ) -> BackendEvent {
        BackendEvent::NodeTransplanted {
            from_store_id, old_node_id, old_parent_id, to_store_id, node_id, new_parent_id,
            title: title.to_string(), left_shares,
        }
    }

    /// A move between two shares of one store, the way a member holding both
    /// drags a note from one into the other (docs/MOVE_CONTRACT.md
    /// "Verification (the bar)"): the old row leaves the list it came from,
    /// the new parent's list is asked for so the person sees where it
    /// landed, and the notice names the nearest share left.
    #[test]
    fn a_transplant_leaving_a_share_drops_the_old_row_refetches_both_lists_and_shows_the_notice() {
        let (store, events, commands) = store_with_events();
        let store_id = StoreId::new();
        let (old_parent, new_parent) = (NodeId::new(), NodeId::new());
        let (old_id, new_id) = (NodeId::new(), NodeId::new());
        store.set_children(store_id, old_parent, vec![(store_id, old_id)]);

        events
            .send(transplanted(
                store_id, old_id, old_parent, store_id, new_id, new_parent,
                "Grocery list", vec![left_share("Trips"), left_share("Recipes")],
            ))
            .unwrap();
        pump(store);

        assert_eq!(
            store.get_children_signal(store_id, old_parent).unwrap().with(|c| c.clone()),
            Vec::new(),
            "the old row leaves the list it was in"
        );
        assert!(
            commands.try_iter().any(|c| matches!(c, BackendCommand::GetChildren { store_id: s, node_id: n, .. } if s == store_id && n == new_parent)),
            "the new parent's list is asked for, so the person sees where it landed"
        );
        assert_eq!(
            store.notice.get(),
            "\"Grocery list\" was moved out of \"Trips\". The people it is shared with see it as deleted and can put it back.",
            "the nearest share left, first in the list"
        );
    }

    /// A plain move between stores, with no share left, is quiet: both lists
    /// still refetch, but there is nothing to tell anyone.
    #[test]
    fn a_transplant_between_stores_with_no_share_left_shows_no_notice() {
        let (store, events, commands) = store_with_events();
        let (from_store, to_store) = (StoreId::new(), StoreId::new());
        let (old_parent, new_parent) = (NodeId::new(), NodeId::new());
        let (old_id, new_id) = (NodeId::new(), NodeId::new());
        store.set_children(from_store, old_parent, vec![(from_store, old_id)]);

        events
            .send(transplanted(from_store, old_id, old_parent, to_store, new_id, new_parent, "Notes", Vec::new()))
            .unwrap();
        pump(store);

        assert_eq!(store.get_children_signal(from_store, old_parent).unwrap().with(|c| c.clone()), Vec::new());
        let asked: Vec<BackendCommand> = commands.try_iter().collect();
        assert!(asked.iter().any(|c| matches!(c, BackendCommand::GetChildren { store_id: s, node_id: n, .. } if *s == to_store && *n == new_parent)));
        assert_eq!(store.notice.get(), "");
    }

    /// The document open in the editor when it is transplanted follows to
    /// its new id: a document opens on the new pair as any newly-selected
    /// one does, and the old one is gone from the tree.
    #[test]
    fn a_transplant_of_the_open_document_moves_the_editor_to_the_new_id() {
        let (store, events, commands) = store_with_events();
        let store_id = StoreId::new();
        let (old_parent, new_parent) = (NodeId::new(), NodeId::new());
        let (old_id, new_id) = (NodeId::new(), NodeId::new());
        store.set_children(store_id, old_parent, vec![(store_id, old_id)]);
        store.selected_id.set(Some(format!("node_{store_id}_{old_id}")));
        store.active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: old_id }));
        store.node_title.set("Grocery list".to_string());
        store.show_editor.set(true);

        events
            .send(transplanted(store_id, old_id, old_parent, store_id, new_id, new_parent, "Grocery list", Vec::new()))
            .unwrap();
        pump(store);

        assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{new_id}")));
        assert!(store.show_editor.get());
        assert_eq!(store.node_title.get(), "Grocery list");
        assert!(
            commands.try_iter().any(|c| matches!(c, BackendCommand::GetNode { store_id: s, node_id: n } if s == store_id && n == new_id)),
            "the new document is fetched, which is what starts its session"
        );
    }

    /// A transplant of a node that was not open in the editor leaves the
    /// selection and the editor alone — only the tree changes.
    #[test]
    fn a_transplant_of_a_node_not_open_leaves_the_editor_alone() {
        let (store, events, commands) = store_with_events();
        let store_id = StoreId::new();
        let (old_parent, new_parent) = (NodeId::new(), NodeId::new());
        let (old_id, new_id) = (NodeId::new(), NodeId::new());
        let open_id = NodeId::new();
        store.set_children(store_id, old_parent, vec![(store_id, old_id)]);
        store.selected_id.set(Some(format!("node_{store_id}_{open_id}")));
        store.active_edit.set(Some(crate::state::ActiveEdit { store_id, node_id: open_id }));

        events
            .send(transplanted(store_id, old_id, old_parent, store_id, new_id, new_parent, "Untitled", Vec::new()))
            .unwrap();
        pump(store);

        assert_eq!(store.selected_id.get(), Some(format!("node_{store_id}_{open_id}")), "a document open elsewhere stays open");
        assert!(!commands.try_iter().any(|c| matches!(c, BackendCommand::GetNode { node_id: n, .. } if n == new_id)));
    }

    fn deleted_node(
        node: pimble_core::Node,
        parent_title: Option<&str>,
        deleted_at: Option<&str>,
        put_back_under: Option<NodeId>,
    ) -> pimble_core::DeletedNode {
        pimble_core::DeletedNode {
            node,
            parent_title: parent_title.map(str::to_string),
            deleted_at: deleted_at.map(str::to_string),
            put_back_under,
        }
    }

    /// `DeletedListed` fills the modal's rows exactly as the contract lists
    /// them: an empty title reads "Untitled", and each `DeletedNode` becomes
    /// one row. A late answer for a store the modal has moved on from names
    /// nothing.
    #[test]
    fn deleted_listed_fills_the_modals_rows() {
        let (store, events, _commands) = store_with_events();
        let store_id = StoreId::new();
        store.deleted_modal_store.set(Some(store_id));

        let named = pimble_core::Node::document("Pasta");
        let named_id = named.id;
        let mut untitled = pimble_core::Node::document("");
        untitled.access = pimble_core::StoreAccess::Read;
        let untitled_id = untitled.id;
        let put_back_under = NodeId::new();

        events
            .send(BackendEvent::DeletedListed {
                store_id,
                nodes: vec![
                    deleted_node(named, Some("Recipes"), Some("2026-09-21T14:32:10Z"), None),
                    deleted_node(untitled, None, None, Some(put_back_under)),
                ],
            })
            .unwrap();
        pump(store);

        let rows = store.deleted_modal_nodes.with(|rows| rows.clone());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].node_id, named_id);
        assert_eq!(rows[0].title, "Pasta");
        assert_eq!(rows[0].parent_title.as_deref(), Some("Recipes"));
        assert_eq!(rows[0].deleted_at.as_deref(), Some("2026-09-21T14:32:10Z"));
        assert_eq!(rows[0].access, pimble_core::StoreAccess::Full);
        assert_eq!(rows[1].node_id, untitled_id);
        assert_eq!(rows[1].title, "Untitled", "an empty title reads Untitled");
        assert_eq!(rows[1].put_back_under, Some(put_back_under));
        assert_eq!(rows[1].access, pimble_core::StoreAccess::Read);

        // The modal moved to another store meanwhile: a late answer for the
        // one it left is not shown.
        store.deleted_modal_store.set(Some(StoreId::new()));
        events.send(BackendEvent::DeletedListed { store_id, nodes: Vec::new() }).unwrap();
        pump(store);
        assert_eq!(store.deleted_modal_nodes.with(|rows| rows.len()), 2, "a late answer for a store the modal left changes nothing");
    }
}
