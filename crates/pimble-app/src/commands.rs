//! Every `BackendCommand`, turned into RPC calls on a connected
//! [`PimbleClient`] and answered with `BackendEvent`s.
//!
//! This is the whole of what the two backends have in common, and the one place
//! a command's meaning is written down. The desktop's `backend` module wraps it
//! in a tokio thread that owns an embedded server and reconnects to it; the web
//! app wraps it in a `spawn_local` loop against a hosted server. Adding a
//! command means adding an arm here and nowhere else.

use std::future::Future;

use crossbeam_channel::Sender;
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, RemoteEndpoint};

use crate::protocol::{BackendCommand, BackendEvent, CloudOp};

/// Run `fut` alongside the command loop, for a subscription that then feeds
/// events in for as long as it lives.
///
/// Native has a tokio runtime to put it on; the browser has the page's own task
/// queue, where nothing is `Send` and nothing needs to be.
#[cfg(feature = "native")]
fn spawn_task(fut: impl Future<Output = ()> + Send + 'static) {
    tokio::spawn(fut);
}

#[cfg(not(feature = "native"))]
fn spawn_task(fut: impl Future<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(fut);
}

pub async fn process_command(
    client: &mut Option<std::sync::Arc<PimbleClient>>,
    cmd: BackendCommand,
    event_tx: &Sender<BackendEvent>,
    signal_ui: &std::sync::Arc<dyn Fn() + Send + Sync>,
    client_id: &str,
) -> Option<BackendEvent> {
    match cmd {
        BackendCommand::CreateStore { path, name } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_store(&path, &name).await {
                Ok((store_id, root_node_id)) => {
                    Some(BackendEvent::StoreCreated { store_id, root_node_id })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::OpenStore { path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.open_store(&path).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CloseStore { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.close_store(store_id).await {
                Ok(()) => Some(BackendEvent::StoreClosed { store_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ListStores => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.list_stores().await {
                Ok(stores) => Some(BackendEvent::StoresListed { stores }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CreateNode { store_id, parent_id, title } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_node(store_id, parent_id, "document", &title).await {
                Ok(node_id) => Some(BackendEvent::NodeCreated { store_id, parent_id, node_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetNode { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_node(store_id, node_id).await {
                Ok(node) => Some(BackendEvent::NodeLoaded { store_id, node }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetChildren { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_children(store_id, node_id).await {
                Ok((children_store_id, children)) => Some(BackendEvent::ChildrenLoaded {
                    store_id,
                    parent_id: node_id,
                    children_store_id,
                    children
                }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::SetNodeContent { store_id, node_id, content } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.set_node_content_bytes(store_id, node_id, content, Some(client_id.to_string())).await {
                Ok(()) => Some(BackendEvent::NodeContentUpdated { store_id, node_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::BroadcastChanges { store_id, node_id, changes } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            use pimble_rpc::EditOperation;
            if let Err(e) = c.apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes }).await {
                tracing::warn!("BroadcastChanges failed: {}", e);
            }
            None
        }

        BackendCommand::SetNodeAppearance { store_id, node_id, icon, color, tags } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_node(store_id, node_id).await {
                Ok(mut node) => {
                    if let Some(icon) = icon {
                        node.metadata.set_icon(icon);
                    }
                    if let Some(color) = color {
                        node.metadata.set_color(color);
                    }
                    if let Some(tags) = tags {
                        node.metadata.tags = tags;
                    }
                    match c.update_node_metadata(store_id, node_id, node.metadata).await {
                        Ok(()) => Some(BackendEvent::NodeRenamed { store_id, node_id }),
                        Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
                    }
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::RenameNode { store_id, node_id, title } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_node(store_id, node_id).await {
                Ok(mut node) => {
                    node.metadata.title = title;
                    node.metadata.custom.insert(
                        pimble_core::custom_keys::EXPLICIT_TITLE.to_string(),
                        serde_json::Value::Bool(true),
                    );
                    match c.update_node_metadata(store_id, node_id, node.metadata).await {
                        Ok(()) => Some(BackendEvent::NodeRenamed { store_id, node_id }),
                        Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
                    }
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::DeleteNode { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            // Get parent before deleting
            let parent_id = match c.get_node(store_id, node_id).await {
                Ok(node) => node.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())),
                Err(e) => return Some(BackendEvent::Error { message: e.to_string() }),
            };
            match c.delete_node(store_id, node_id).await {
                Ok(()) => Some(BackendEvent::NodeDeleted { store_id, node_id, parent_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::MoveNode { store_id, node_id, new_parent_id, position } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            // Get old parent before moving
            let old_parent_id = match c.get_node(store_id, node_id).await {
                Ok(node) => node.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())),
                Err(e) => return Some(BackendEvent::Error { message: e.to_string() }),
            };
            match c.move_node(store_id, node_id, new_parent_id, position).await {
                Ok(()) => Some(BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CreateMount { store_id, parent_id, source_store_id, source_node_id, title } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_mount(store_id, parent_id, source_store_id, source_node_id, title).await {
                Ok((node_id, mount_ref)) => Some(BackendEvent::MountCreated { store_id, parent_id, node_id, mount_ref }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetMountState { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_mount_state(store_id, node_id).await {
                Ok((state, mount_ref)) => Some(BackendEvent::MountStateChanged { store_id, node_id, state, mount_ref }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.sync_node_content(store_id, node_id, &state_vector).await {
                Ok((diff, server_state_vector)) => Some(BackendEvent::NodeContentReconciled {
                    store_id,
                    node_id,
                    diff,
                    server_state_vector,
                }),
                Err(e) => Some(BackendEvent::Error { message: format!("Reconcile failed: {}", e) }),
            }
        }

        BackendCommand::SubscribeStoreChanges { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.subscribe_store_changes(store_id).await {
                Ok(mut sub) => {
                    let tx = event_tx.clone();
                    let signal = signal_ui.clone();
                    spawn_task(async move {
                        while let Some(Ok(notification)) = sub.next().await {
                            let event = BackendEvent::RemoteStoreChange {
                                store_id: notification.store_id,
                                change_kind: notification.change_kind,
                                source_client_id: notification.source_client_id,
                            };
                            if let Err(e) = tx.try_send(event) {
                                tracing::warn!("RemoteStoreChange channel full, dropped: {}", e);
                            }
                            signal();
                        }
                    });
                    None // No immediate event
                }
                Err(e) => Some(BackendEvent::Error { message: format!("Subscribe failed: {}", e) }),
            }
        }

        BackendCommand::SubscribeNodeChanges { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.subscribe_node_changes(store_id, node_id).await {
                Ok(mut sub) => {
                    let tx = event_tx.clone();
                    let signal = signal_ui.clone();
                    let my_client_id = client_id.to_string();
                    spawn_task(async move {
                        loop {
                            let notification = match sub.next().await {
                                Some(Ok(n)) => n,
                                Some(Err(e)) => {
                                    tracing::warn!("Node subscription error: {}", e);
                                    continue;
                                }
                                None => break,
                            };

                            tracing::info!("Node sub: received notification, source={:?}, has_op={}", notification.source_client_id, notification.operation.is_some());

                            // Skip our own echoes
                            if let Some(ref source) = notification.source_client_id {
                                if source == &my_client_id {
                                    continue;
                                }
                            }

                            // Forward incremental changes if present
                            if let Some(ref op) = notification.operation {
                                use pimble_rpc::EditOperation;
                                let EditOperation::IncrementalChanges { changes } = op;
                                if let Err(e) = tx.try_send(BackendEvent::RemoteChanges {
                                    changes: changes.clone(),
                                }) {
                                    tracing::warn!("RemoteChanges channel full, dropped: {}", e);
                                }
                            }
                            signal();
                        }
                    });
                    None
                }
                Err(e) => Some(BackendEvent::Error { message: format!("Subscribe failed: {}", e) }),
            }
        }

        // Handled in `backend_loop` before dispatch; never reaches here.
        BackendCommand::ConnectionLost { .. } => None,

        BackendCommand::Search { query, stores, limit } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::SearchResults { results: Err("Not connected".into()) });
            };
            // Ask for hybrid (keyword + semantic); a server built without an ONNX
            // link mode answers keyword-only.
            match c.search(query, stores, true, limit).await {
                Ok(results) => Some(BackendEvent::SearchResults { results: Ok(results) }),
                Err(e) => Some(BackendEvent::SearchResults { results: Err(e.to_string()) }),
            }
        }

        BackendCommand::RebuildIndex { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.rebuild_index(store_id).await {
                Ok(indexed) => Some(BackendEvent::IndexRebuilt { store_id, indexed }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ListRemoteStores { url, token } => {
            // Through this server's own `listRemoteStores` (decision 5): the
            // app never connects to a remote itself. `c` here is our
            // connection to the local embedded/shared server.
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::RemoteStoresListed { url, result: Err("Not connected".into()) });
            };
            let remote = match remote_endpoint(&url, &token) {
                Ok(r) => r,
                Err(message) => return Some(BackendEvent::RemoteStoresListed { url, result: Err(message) }),
            };
            match c.list_remote_stores(remote).await {
                Ok(stores) => Some(BackendEvent::RemoteStoresListed { url, result: Ok(stores) }),
                Err(e) => Some(BackendEvent::RemoteStoresListed { url, result: Err(e.to_string()) }),
            }
        }

        BackendCommand::AddRemoteStore { url, remote_store_id, token } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            let remote = match remote_endpoint(&url, &token) {
                Ok(r) => r,
                Err(message) => return Some(BackendEvent::Error { message }),
            };
            // No path: the server puts the replica in its own data directory.
            match c.add_remote_store(remote, remote_store_id, None).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::MountRemoteStore {
            url,
            remote_store_id,
            token,
            target_store_id,
            target_parent_id,
        } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };

            // Step 1: the source store may already be open here (an earlier
            // replica, or a store that lives on this server), in which case
            // adding it again is both wrong and refused. Ask the local server
            // what it holds before deciding.
            let already_open = match c.list_stores().await {
                Ok(stores) => stores.into_iter().find(|s| s.id == remote_store_id),
                Err(e) => return Some(BackendEvent::Error { message: e.to_string() }),
            };

            let source = match already_open {
                Some(existing) => existing,
                None => {
                    let remote = match remote_endpoint(&url, &token) {
                        Ok(r) => r,
                        Err(message) => return Some(BackendEvent::Error { message }),
                    };
                    // No path: the server puts the replica in its own data directory.
                    let added = match c.add_remote_store(remote, remote_store_id, None).await {
                        Ok(store) => store,
                        Err(e) => return Some(BackendEvent::Error { message: e.to_string() }),
                    };
                    // Report the new store before the mount, so the tree has
                    // it registered by the time `MountCreated` lands.
                    let _ = event_tx.try_send(BackendEvent::StoreOpened { store: added.clone() });
                    signal_ui();
                    added
                }
            };

            // Step 2: mount the source store's root under the target.
            match c
                .create_mount(
                    target_store_id,
                    target_parent_id,
                    source.id,
                    source.root_node_id,
                    Some(source.name.clone()),
                )
                .await
            {
                Ok((node_id, mount_ref)) => Some(BackendEvent::MountCreated {
                    store_id: target_store_id,
                    parent_id: target_parent_id,
                    node_id,
                    mount_ref,
                }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::SetStoreSync { store_id, remote } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.set_store_sync_with_mode(store_id, remote).await {
                Ok((remote, state, sync_mode)) => {
                    Some(BackendEvent::StoreSyncChanged { store_id, remote, state, sync_mode })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetStoreSync { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_store_sync_with_mode(store_id).await {
                Ok((remote, state, sync_mode)) => {
                    Some(BackendEvent::StoreSyncChanged { store_id, remote, state, sync_mode })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md). Every
        // failure is a `CloudError` naming its operation, never the generic
        // `Error`, so the modal that asked gets it (decision 3).
        BackendCommand::CloudStatus => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Status, message: "Not connected".into() });
            };
            match c.cloud_status().await {
                Ok(status) => Some(cloud_status_event(status)),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Status, message: e.to_string() }),
            }
        }

        BackendCommand::CloudSignIn { url, email, password } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::SignIn, message: "Not connected".into() });
            };
            if let Err(e) = c.cloud_sign_in(url.clone(), email.clone(), password).await {
                return Some(BackendEvent::CloudError { op: CloudOp::SignIn, message: e.to_string() });
            }
            // The keystore's own record of the account is the answer; the
            // typed values stand in only if reading it back fails.
            match c.cloud_status().await {
                Ok(status) => Some(cloud_status_event(status)),
                Err(_) => Some(BackendEvent::CloudStatusChanged {
                    signed_in: true,
                    email: Some(email),
                    url: Some(url),
                }),
            }
        }

        BackendCommand::CloudSignOut => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::SignOut, message: "Not connected".into() });
            };
            match c.cloud_sign_out().await {
                Ok(()) => Some(BackendEvent::CloudStatusChanged { signed_in: false, email: None, url: None }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::SignOut, message: e.to_string() }),
            }
        }

        BackendCommand::CloudHostStore { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::HostStore, message: "Not connected".into() });
            };
            match c.cloud_host_store(store_id).await {
                Ok(store_id) => Some(BackendEvent::CloudStoreHosted { store_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::HostStore, message: e.to_string() }),
            }
        }

        BackendCommand::CloudListHostedStores => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ListHostedStores, message: "Not connected".into() });
            };
            match c.cloud_list_hosted_stores().await {
                Ok(stores) => Some(BackendEvent::CloudHostedStoresListed { stores }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::ListHostedStores, message: e.to_string() }),
            }
        }

        BackendCommand::CloudAddHostedStore { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::AddHostedStore, message: "Not connected".into() });
            };
            // The replica arrives the way `AddRemoteStore`'s does.
            match c.cloud_add_hosted_store(store_id).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::AddHostedStore, message: e.to_string() }),
            }
        }

        // The accounts service owns hosted stores, and only a signed-in client
        // can mint an encrypted store's key and seal it to itself. The browser
        // backend answers this before a command ever reaches here; a build that
        // makes its stores as files on disk has nothing to do with it.
        BackendCommand::CreateHostedStore { .. } => Some(BackendEvent::Error {
            message: "This build cannot create a hosted store".into(),
        }),

        BackendCommand::RemoveReplica { store_id, force } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.remove_replica(store_id, force).await {
                Ok(()) => Some(BackendEvent::ReplicaRemoved { store_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }
    }
}

/// The `CloudStatusChanged` event for a `cloudStatus` answer.
fn cloud_status_event(status: pimble_rpc::CloudStatusResponse) -> BackendEvent {
    BackendEvent::CloudStatusChanged { signed_in: status.signed_in, email: status.email, url: status.url }
}

/// Build a `RemoteEndpoint` for a URL typed into a modal: an empty token
/// means "use whatever this server already saved for that origin"
/// (`AuthMethod::None`, docs/history/HARDENING_CONTRACT.md decision 4); a non-empty
/// one is sent as `AuthMethod::Bearer`.
fn remote_endpoint(url: &str, token: &str) -> Result<RemoteEndpoint, String> {
    let url: url::Url = url.parse().map_err(|e| format!("Invalid remote URL: {}", e))?;
    let auth = if token.is_empty() { AuthMethod::None } else { AuthMethod::Bearer { token: token.to_string() } };
    Ok(RemoteEndpoint { url, auth })
}
