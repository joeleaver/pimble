//! Backend event processing.
//!
//! Each event handler uses per-entity signal mutations. Only structural events
//! (store open/close, children loaded, node moved) bump `tree_structure_version`.

use std::cell::RefCell;
use std::collections::HashSet;

use pimble_core::{NodeId, Store, StoreId};
use rinch::prelude::*;

use crate::backend::{BackendCommand, BackendEvent};
use crate::editor::{apply_remote, start_editing};
use crate::persistence::{load_app_state_file, save_app_state_file};
use crate::state::{parse_tree_value, take_last_drop_target_value, AppStore, ConnectionState, MountInfo, SearchState};

thread_local! {
    pub(crate) static EVENT_PROCESSOR: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
}

/// Register a newly-known store in the tree: upsert its signal, fetch its
/// root's children, subscribe to its changes, auto-expand its row, and
/// persist the open-store list. Used both when the app explicitly opens a
/// store (`StoreOpened`) and when it discovers one implicitly — a mount's
/// source store the server opened to resolve a mount (`StoresListed`,
/// decision 4) — minus the pending-mount finalization, which only applies to
/// the explicit "Mount Store..." folder-picker flow.
fn register_opened_store(store: AppStore, tree_state: UseTreeReturn, opened_store: &Store) {
    let store_id = opened_store.id;
    let root_id = opened_store.root_node_id;

    // Structural: new store appears in tree
    store.upsert_store(opened_store.clone());
    store.expanded.update(|e| { e.insert((store_id, root_id)); });

    store.send(BackendCommand::GetChildren { store_id, node_id: root_id });

    // Subscribe to store changes for real-time updates
    store.send(BackendCommand::SubscribeStoreChanges { store_id });

    // Sync status (docs/SYNC_CONTRACT.md "B: app side"): a placeholder
    // unlinked/offline entry exists immediately so the store row's badge
    // signal is always available by the time the tree renders it;
    // `GetStoreSync`'s answer (`StoreSyncChanged`) fills in the real state.
    store.ensure_sync_entry(store_id);
    store.send(BackendCommand::GetStoreSync { store_id });

    // Auto-expand the store node in the tree
    tree_state.controller.expand(&format!("store_{}", store_id));

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
    }
    for (mount_store, mount_node) in store.mounts_sourced_from_node(changed_store, parent_id) {
        if store.has_children_loaded(mount_store, mount_node) {
            store.send(BackendCommand::GetChildren { store_id: mount_store, node_id: mount_node });
        }
    }
}

/// After a transition to `Synced`, refetch the root's children if none are
/// loaded yet — a freshly added remote store's document starts empty and only
/// gains a root once the first reconcile lands (docs/SYNC_CONTRACT.md "B: app
/// side").
fn refetch_root_if_empty(store: AppStore, store_id: StoreId) {
    let Some(root_id) = store.root_node_id(store_id) else { return };
    let children_missing = untracked(|| {
        store.get_children_signal(store_id, root_id)
            .map(|sig| sig.with(|c| c.is_empty()))
            .unwrap_or(true)
    });
    if children_missing {
        store.send(BackendCommand::GetChildren { store_id, node_id: root_id });
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
                for child in children {
                    let key = (*children_store_id, child.id);
                    let should_update = untracked(|| {
                        store.node_data.with(|map| {
                            map.get(&key)
                                .map_or(true, |sig| sig.with(|cached| child.metadata.modified_at >= cached.metadata.modified_at))
                        })
                    });
                    if should_update {
                        store.track_mount_info(*children_store_id, child);
                        store.upsert_node(*children_store_id, child.clone());
                    }
                }

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

                // Check if the content actually changed compared to what we have cached.
                let content_changed = untracked(|| {
                    store.node_data.with(|map| {
                        map.get(&(*store_id, node_id))
                            .map_or(true, |sig| sig.with(|cached| cached.content != node.content))
                    })
                });

                // Data-only: updates per-node signal, NO tree rebuild
                store.upsert_node(*store_id, node.clone());
                let _ = content_changed;

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

            BackendEvent::StoreSyncChanged { store_id, remote, state } => {
                tracing::info!("Sync state for store {:?}: {:?}", store_id, state);
                let was_linked = store.is_linked(*store_id);
                store.set_sync(*store_id, remote.clone(), state.clone());
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
        }
    }
}
