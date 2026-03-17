//! Backend event processing.

use std::cell::RefCell;
use std::rc::Rc;

use pimble_core::NodeId;
use rinch::prelude::*;

use crate::backend::{BackendCommand, BackendEvent};
use crate::editor::load_content_into_ce;
use crate::persistence::{load_app_state_file, save_app_state_file};
use crate::state::{parse_tree_value, AppStore, ConnectionState, MountInfo};

thread_local! {
    pub(crate) static EVENT_PROCESSOR: RefCell<Option<Box<dyn Fn()>>> = RefCell::new(None);
}

/// Process backend events and update store signals directly.
///
/// No deferred updates needed — each signal is independent, so mutations
/// don't cause re-entrancy issues.
pub(crate) fn process_backend_events(
    store: AppStore,
    tree_state: UseTreeReturn,
    ce_div_cell: &Rc<RefCell<Option<NodeHandle>>>,
) {
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
            BackendEvent::Connected { server_addr } => {
                tracing::info!("Connected to backend at {}", server_addr);
                store.connection.set(ConnectionState::Connected);
                store.connection_status.set("Connected".to_string());
                store.server_addr.set(server_addr.clone());

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
                store.stores.update(|s| { s.insert(store_id, opened_store.clone()); });
                store.expanded.update(|e| { e.insert((store_id, root_id)); });

                store.backend.with(|b| {
                    if let Some(backend) = b {
                        backend.send(BackendCommand::GetChildren {
                            store_id,
                            node_id: root_id,
                        });
                    }
                });

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
                let paths: Vec<String> = store.stores.with(|s| {
                    s.values()
                        .filter_map(|s| s.local_path().map(|p| p.to_string_lossy().to_string()))
                        .collect()
                });
                save_app_state_file(&paths);
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

            BackendEvent::ChildrenLoaded { store_id, parent_id, children } => {
                tracing::info!("Children loaded for {:?}: {} nodes", parent_id, children.len());

                let child_ids: Vec<NodeId> = children.iter().map(|n| n.id).collect();
                store.children.update(|c| { c.insert((*store_id, *parent_id), child_ids); });

                // Collect mount node IDs so we can request their state after updating the nodes map
                let mount_node_ids: Vec<NodeId> = children.iter()
                    .filter(|child| child.is_mount())
                    .map(|child| child.id)
                    .collect();

                store.nodes.update(|nodes| {
                    for child in children {
                        let dominated = nodes.get(&(*store_id, child.id))
                            .map_or(true, |cached| child.metadata.modified_at >= cached.metadata.modified_at);
                        if dominated {
                            store.track_mount_info(*store_id, &child);
                            nodes.insert((*store_id, child.id), child.clone());
                        }
                    }
                });

                // Request mount state for any mount nodes so the UI can show Live/Unavailable
                for mount_node_id in mount_node_ids {
                    store.send(BackendCommand::GetMountState {
                        store_id: *store_id,
                        node_id: mount_node_id,
                    });
                }

            }

            BackendEvent::NodeLoaded { store_id, node } => {
                tracing::info!("Node loaded: {:?} - {}", node.id, node.metadata.title);
                let node_id = node.id;
                let content_bytes = node.content.clone();
                store.track_mount_info(*store_id, &node);
                store.nodes.update(|n| { n.insert((*store_id, node_id), node.clone()); });

                if let Some(selected_id) = store.selected_id.get() {
                    if let Some((sel_store_id, Some(sel_node_id))) = parse_tree_value(&selected_id) {
                        if sel_store_id == *store_id && sel_node_id == node_id {
                            store.node_title.set(store.display_label(*store_id, node_id));
                            if let Some(ce_div) = ce_div_cell.borrow().as_ref() {
                                load_content_into_ce(&content_bytes, ce_div);
                            }
                        }
                    }
                }
            }

            BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id } => {
                tracing::info!("Node moved: {:?} from {:?} to {:?}", node_id, old_parent_id, new_parent_id);

                // Optimistic update: directly modify the children caches
                store.children.update(|children| {
                    // Remove moved node from old parent's children list
                    if let Some(old_children) = children.get_mut(&(*store_id, *old_parent_id)) {
                        old_children.retain(|&id| id != *node_id);
                    }

                    // Add moved node to new parent's children list
                    if let Some(new_children) = children.get_mut(&(*store_id, *new_parent_id)) {
                        if !new_children.contains(node_id) {
                            new_children.push(*node_id);
                        }
                    } else {
                        children.insert((*store_id, *new_parent_id), vec![*node_id]);
                    }
                });

                // Auto-expand the new parent so the moved node is visible
                store.expanded.update(|e| { e.insert((*store_id, *new_parent_id)); });
                let is_root = store.stores.with(|s| {
                    s.get(store_id).map_or(false, |s| s.root_node_id == *new_parent_id)
                });
                if !is_root {
                    tree_state.controller.expand(&format!("node_{}_{}", store_id, new_parent_id));
                }

                // Also re-fetch from server for authoritative data
                store.backend.with(|b| {
                    if let Some(backend) = b {
                        backend.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *old_parent_id });
                        backend.send(BackendCommand::GetChildren { store_id: *store_id, node_id: *new_parent_id });
                    }
                });
            }

            BackendEvent::NodeCreated { store_id, parent_id, node_id } => {
                tracing::info!("Node created: {:?}/{:?} under {:?}", store_id, node_id, parent_id);
                let parent = parent_id.unwrap_or_else(|| {
                    store.stores.with(|s| s.get(store_id).map(|s| s.root_node_id)).unwrap()
                });
                // Re-fetch parent's children so the new node appears in the tree
                store.send(BackendCommand::GetChildren { store_id: *store_id, node_id: parent });
            }

            BackendEvent::NodeContentUpdated { store_id, node_id } => {
                tracing::info!("Node content updated: {:?}/{:?}", store_id, node_id);
                store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
            }

            BackendEvent::NodeRenamed { store_id, node_id } => {
                tracing::info!("Node renamed: {:?}/{:?}", store_id, node_id);
                store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
            }

            BackendEvent::StoreClosed { store_id } => {
                tracing::info!("Store closed: {:?}", store_id);
                store.stores.update(|s| { s.remove(store_id); });

                // Persist open store paths
                let paths: Vec<String> = store.stores.with(|s| {
                    s.values()
                        .filter_map(|s| s.local_path().map(|p| p.to_string_lossy().to_string()))
                        .collect()
                });
                save_app_state_file(&paths);
            }

            BackendEvent::MountCreated { store_id, node_id, mount_ref } => {
                tracing::info!("Mount created: {:?}/{:?} -> {:?}/{:?}",
                    store_id, node_id, mount_ref.source_store, mount_ref.source_node);
                // Track mount info for the new node
                store.mount_info.update(|info| {
                    info.insert((*store_id, *node_id), MountInfo {
                        is_mount: true,
                        mount_state: None,
                    });
                });
                // Re-fetch parent's children so the mount node appears in the tree
                // The parent is not directly available here, so re-fetch the node to get it
                store.send(BackendCommand::GetNode { store_id: *store_id, node_id: *node_id });
                // Request mount state so we can show Live/Unavailable
                store.send(BackendCommand::GetMountState { store_id: *store_id, node_id: *node_id });
            }

            BackendEvent::MountStateChanged { store_id, node_id, state } => {
                tracing::info!("Mount state changed: {:?}/{:?} -> {:?}", store_id, node_id, state);
                store.set_mount_state(*store_id, *node_id, state.clone());
            }

            _ => {}
        }
    }
}
