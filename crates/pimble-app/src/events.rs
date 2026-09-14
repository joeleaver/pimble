//! Backend event processing.
//!
//! Each event handler uses per-entity signal mutations. Only structural events
//! (store open/close, children loaded, node moved) bump `tree_structure_version`.

use std::cell::RefCell;

use pimble_core::NodeId;
use rinch::prelude::*;

use crate::backend::{BackendCommand, BackendEvent};
use crate::editor::{apply_remote, start_editing};
use crate::persistence::{load_app_state_file, save_app_state_file};
use crate::state::{parse_tree_value, AppStore, ConnectionState, MountInfo, SearchState};

thread_local! {
    pub(crate) static EVENT_PROCESSOR: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
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

                // Auto-open previously loaded stores
                let saved_paths = load_app_state_file();
                store.backend.with(|b| {
                    if let Some(backend) = b {
                        for path in saved_paths {
                            tracing::info!("Auto-opening saved store: {}", path);
                            backend.send(BackendCommand::OpenStore { path });
                        }
                    }
                });
            }

            BackendEvent::Disconnected => {
                tracing::info!("Disconnected from backend");
                store.connection.set(ConnectionState::Disconnected);
                store.connection_status.set("Disconnected".to_string());
            }

            BackendEvent::Error { message } => {
                tracing::error!("Backend error: {}", message);
                store.connection.set(ConnectionState::Error(message.clone()));
                store.connection_status.set(format!("Error: {}", message));
            }

            BackendEvent::StoreOpened { store: opened_store } => {
                tracing::info!("Store opened: {}", opened_store.name);
                let store_id = opened_store.id;
                let root_id = opened_store.root_node_id;

                // Structural: new store appears in tree
                store.upsert_store(opened_store.clone());
                store.expanded.update(|e| { e.insert((store_id, root_id)); });

                store.backend.with(|b| {
                    if let Some(backend) = b {
                        backend.send(BackendCommand::GetChildren {
                            store_id,
                            node_id: root_id,
                        });
                    }
                });

                // Subscribe to store changes for real-time updates
                store.send(BackendCommand::SubscribeStoreChanges { store_id });

                // Auto-expand the store node in the tree
                tree_state.controller.expand(&format!("store_{}", store_id));

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

                // Persist open store paths
                save_app_state_file(&store.all_store_local_paths());

                // Structural change
                store.bump_tree_structure();
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

            BackendEvent::ChildrenLoaded { store_id, parent_id, children_store_id, children } => {
                tracing::info!("Children loaded for {:?}: {} nodes (in store {:?})", parent_id, children.len(), children_store_id);

                let child_ids: Vec<NodeId> = children.iter().map(|n| n.id).collect();

                // Structural: children list changed
                store.set_children(*store_id, *parent_id, child_ids);

                // Collect mount node IDs so we can request their state
                let mount_node_ids: Vec<NodeId> = children.iter()
                    .filter(|child| child.is_mount())
                    .map(|child| child.id)
                    .collect();

                // Upsert each child node (per-entity signal)
                for child in children {
                    let key = (*store_id, child.id);
                    let should_update = untracked(|| {
                        store.node_data.with(|map| {
                            map.get(&key)
                                .map_or(true, |sig| sig.with(|cached| child.metadata.modified_at >= cached.metadata.modified_at))
                        })
                    });
                    if should_update {
                        store.track_mount_info(*store_id, child);
                        store.upsert_node(*store_id, child.clone());
                    }
                }

                // Request mount state for any mount nodes
                for mount_node_id in mount_node_ids {
                    store.send(BackendCommand::GetMountState {
                        store_id: *store_id,
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
                        children.retain(|&id| id != *node_id);
                    });
                }

                // Update children_of for new parent (avoid nested borrow)
                let new_key = (*store_id, *new_parent_id);
                let new_parent_sig = store.get_children_signal(*store_id, *new_parent_id);
                if let Some(sig) = new_parent_sig {
                    sig.update(|children| {
                        if !children.contains(node_id) {
                            children.push(*node_id);
                        }
                    });
                } else {
                    let new_sig = Signal::new(vec![*node_id]);
                    store.children_of.update(|map| {
                        map.insert(new_key, new_sig);
                    });
                }

                // Auto-expand the new parent so the moved node is visible
                store.expanded.update(|e| { e.insert((*store_id, *new_parent_id)); });
                let is_root = store.root_node_id(*store_id)
                    .map_or(false, |rid| rid == *new_parent_id);
                if !is_root {
                    tree_state.controller.expand(&format!("node_{}_{}", store_id, new_parent_id));
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
                        children.retain(|&id| id != *node_id);
                    });
                }

                // Clean up per-entity signals
                store.remove_node(*store_id, *node_id);

                // Clear selection if the deleted node was selected
                if let Some(selected_id) = store.selected_id.get() {
                    if let Some((sel_store_id, Some(sel_node_id))) = parse_tree_value(&selected_id) {
                        if sel_store_id == *store_id && sel_node_id == *node_id {
                            store.selected_id.set(None);
                            store.node_title.set(String::new());
                            store.show_editor.set(false);
                        }
                    }
                }

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

            BackendEvent::MountCreated { store_id, node_id, mount_ref } => {
                tracing::info!("Mount created: {:?}/{:?} -> {:?}/{:?}",
                    store_id, node_id, mount_ref.source_store, mount_ref.source_node);
                // Track mount info for the new node (avoid nested borrow)
                let existing_mount = store.get_mount_signal(*store_id, *node_id);
                if let Some(sig) = existing_mount {
                    sig.update(|m| { m.is_mount = true; m.mount_state = None; });
                } else {
                    let new_sig = Signal::new(MountInfo {
                        is_mount: true,
                        mount_state: None,
                    });
                    store.mount_data.update(|map| {
                        map.insert((*store_id, *node_id), new_sig);
                    });
                }
                // Re-fetch node and mount state
                store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
                store.send(BackendCommand::GetMountState { store_id: *store_id, node_id: *node_id });
            }

            BackendEvent::MountStateChanged { store_id, node_id, state } => {
                tracing::info!("Mount state changed: {:?}/{:?} -> {:?}", store_id, node_id, state);
                // Data-only: updates per-mount signal, NO tree rebuild.
                // The mount Effect on that node fires and updates icon opacity + label suffix.
                store.set_mount_state(*store_id, *node_id, state.clone());
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
                    StoreChangeKind::NodeCreated { node_id }
                    | StoreChangeKind::NodeDeleted { node_id }
                    | StoreChangeKind::NodeMoved { node_id } => {
                        // Re-fetch the tree from root
                        if let Some(root_id) = store.root_node_id(*store_id) {
                            store.send(BackendCommand::GetChildren {
                                store_id: *store_id,
                                node_id: root_id,
                            });
                        }
                        // If deleted node was selected, clear selection
                        if matches!(change_kind, StoreChangeKind::NodeDeleted { .. }) {
                            if let Some(selected_id) = store.selected_id.get() {
                                if let Some((sel_sid, Some(sel_nid))) = parse_tree_value(&selected_id) {
                                    if sel_sid == *store_id && sel_nid == *node_id {
                                        store.selected_id.set(None);
                                        store.node_title.set(String::new());
                                        store.show_editor.set(false);
                                    }
                                }
                            }
                        }
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
                    StoreChangeKind::TreeStructure => {
                        if let Some(root_id) = store.root_node_id(*store_id) {
                            store.send(BackendCommand::GetChildren {
                                store_id: *store_id,
                                node_id: root_id,
                            });
                        }
                    }
                }
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
        }
    }
}
