//! What only a Pimble server's operator does, on the host connection:
//! stores, replicas and sync links, agents and the server itself. The Pimble
//! Cloud account, notifications and sharing are `cloud`'s.

use pimble_core::{AuthMethod, RemoteEndpoint};

use super::{api_command, Context};
use crate::protocol::{BackendCommand, BackendEvent};

/// Answer what only a Pimble server's operator does, on the host
/// connection; the Pimble Cloud commands go on to [`super::cloud`].
pub async fn host_command(ctx: &Context<'_>, cmd: BackendCommand) -> Option<BackendEvent> {
    match cmd {
        BackendCommand::CreateStore { path, name } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_store(&path, &name).await {
                Ok((store_id, root_node_id)) => {
                    Some(BackendEvent::StoreCreated { store_id, root_node_id })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        // Each backend's loop answers an import itself, as a run of the
        // commands here (`crate::import`); it never reaches this match.
        BackendCommand::Import { .. } => Some(BackendEvent::ImportFailed {
            message: "This backend cannot import.".into(),
        }),

        BackendCommand::OpenStore { path } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.open_store(&path).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::StoreOpenFailed { path, message: error_message_client(&e) }),
            }
        }

        BackendCommand::CloseStore { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.close_store(store_id).await {
                Ok(()) => {
                    ctx.api.forget(store_id);
                    Some(BackendEvent::StoreClosed { store_id })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ListStores => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.list_stores().await {
                Ok(stores) => Some(BackendEvent::StoresListed { stores }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::SyncPersonal { state, enabled, turn_on } => {
            let c = ctx.host.as_ref()?;
            match c.cloud_personal_sync(&state, enabled, turn_on).await {
                Ok((state, off)) => Some(BackendEvent::PersonalSynced { state, off }),
                Err(e) => {
                    // Nobody signed in, or the service unreachable: this
                    // device keeps its copy and tries again later.
                    tracing::debug!("personal document not synced: {e}");
                    None
                }
            }
        }

        BackendCommand::RebuildIndex { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
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
            let Some(c) = ctx.host.as_ref() else {
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
            let Some(c) = ctx.host.as_ref() else {
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
            let Some(c) = ctx.host.as_ref() else {
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
                    let _ = ctx.event_tx.try_send(BackendEvent::StoreOpened { store: added.clone() });
                    (ctx.signal_ui)();
                    added
                }
            };

            // Step 2: mount the source store's root under the target.
            let mount = BackendCommand::CreateMount {
                store_id: target_store_id,
                parent_id: target_parent_id,
                source_store_id: source.id,
                source_node_id: source.root_node_id,
                title: Some(source.name.clone()),
            };
            // The API's mount, its failure answered as this request's.
            api_command(ctx, mount).await.unwrap_or(None)
        }

        BackendCommand::SetStoreSync { store_id, remote } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.set_store_sync_response(store_id, remote).await {
                Ok(answer) => Some(store_sync_changed(store_id, answer)),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetStoreSync { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_store_sync_response(store_id).await {
                Ok(answer) => {
                    ctx.emit(BackendEvent::IndexStateChanged { store_id, state: answer.index.clone() });
                    Some(store_sync_changed(store_id, answer))
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ListAgents | BackendCommand::CreateAgent { .. } | BackendCommand::UpdateAgent { .. } | BackendCommand::RemoveAgent { .. } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::AgentsError { message: "Not connected to Pimble's server.".into() });
            };
            let done = match cmd {
                BackendCommand::CreateAgent { name, grants } => {
                    return Some(match c.create_agent(&name, grants).await {
                        Ok(made) => BackendEvent::AgentCreated { agent: made.agent, key: made.key },
                        Err(e) => BackendEvent::AgentsError { message: agent_error(e) },
                    });
                }
                BackendCommand::UpdateAgent { id, name, grants } => c.update_agent(&id, Some(name), Some(grants)).await.map(|_| ()),
                BackendCommand::RemoveAgent { id } => c.remove_agent(&id).await,
                _ => Ok(()),
            };
            if let Err(e) = done {
                return Some(BackendEvent::AgentsError { message: agent_error(e) });
            }
            match c.list_agents().await {
                Ok(agents) => Some(BackendEvent::AgentsListed { agents }),
                Err(e) => Some(BackendEvent::AgentsError { message: agent_error(e) }),
            }
        }

        BackendCommand::ServerInfo => {
            let c = ctx.host.as_ref()?;
            Some(BackendEvent::ServerInfo { version: c.server_info().await.ok().map(|i| i.version) })
        }

        // The accounts service owns hosted stores, and only a signed-in client
        // can mint an encrypted store's key and seal it to itself. The browser
        // backend answers this before a command ever reaches here; a build that
        // makes its stores as files on disk has nothing to do with it.
        BackendCommand::CreateHostedStore { .. } => Some(BackendEvent::Error {
            message: "This build cannot create a hosted store".into(),
        }),

        BackendCommand::RemoveReplica { store_id, force } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.remove_replica(store_id, force).await {
                Ok(()) => {
                    ctx.api.forget(store_id);
                    Some(BackendEvent::ReplicaRemoved { store_id })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        other => super::cloud::cloud_command(ctx, other).await,
    }
}

/// A store's sync answer as the event the UI takes it as: all of it, what
/// this device may change included. Dropping `access` here is how a role
/// changed while the app ran used to reach nothing until a restart.
fn store_sync_changed(store_id: pimble_core::StoreId, answer: pimble_rpc::GetStoreSyncResponse) -> BackendEvent {
    BackendEvent::StoreSyncChanged {
        store_id,
        remote: answer.remote,
        state: answer.state,
        sync_mode: answer.sync_mode,
        access: answer.access,
        read_only_roots: answer.read_only_roots,
        ended_roots: answer.ended_roots,
        relay: answer.relay,
        // Only when the server knows: the relay said the owner's computer
        // is not there. This device having no network reads as plain
        // `offline`, so nobody is told somebody else's computer is off
        // when it is their own connection.
        owner_offline: answer.owner_offline,
        waiting: answer.waiting,
        out_of_step: answer.out_of_step,
    }
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

/// An agent command's failure as the person reads it: the server's sentence,
/// without the error code around it.
fn agent_error(e: impl std::fmt::Display) -> String {
    let message = e.to_string();
    match message.split_once(": ") {
        Some((head, rest)) if head.contains("RPC") || head.contains("error") => rest.to_string(),
        _ => message,
    }
}

/// A server's own error as a person reads it: its message, without the
/// JSON-RPC code around it.
fn error_message_client(e: &pimble_client::ClientError) -> String {
    match e {
        pimble_client::ClientError::Server { message, .. } => message.clone(),
        other => other.to_string(),
    }
}
