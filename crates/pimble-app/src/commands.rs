//! Every `BackendCommand`, answered with `BackendEvent`s: the app's thin
//! adapter onto the one API (docs/API_CONTRACT.md wave 5).
//!
//! A command that reads or writes a store is a call on [`Pimble`]
//! ([`api_command`]): its documents are held there, kept current by the
//! store's subscription, and every write goes through it. What only a
//! Pimble server's operator does (open, close and create stores, replicas
//! and sync links, the cloud account, sharing, agents) is the server's own
//! RPCs on the connection to this client's own server ([`host_command`]),
//! the `Host` side of decision 2. The desktop's `backend` module and the web
//! app's loop own the API and that connection, and both call
//! [`process_command`]; a store's changes reach the UI through
//! [`change_events`], from whoever listens to the API.

use std::future::Future;
use std::sync::Arc;

use crossbeam_channel::Sender;
use pimble_api::{Addr, ApiError, Change, Pimble};
use pimble_client::PimbleClient;
use pimble_core::{custom_keys, node_types, AuthMethod, NodeId, RemoteEndpoint, StoreId};

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

/// What a command runs against.
pub struct Context<'a> {
    /// The API: every command that reads or writes a store.
    pub api: &'a Arc<Pimble>,
    /// The connection to this client's own server, for what only its
    /// operator does; `None` while there is none.
    pub host: Option<Arc<PimbleClient>>,
    pub event_tx: &'a Sender<BackendEvent>,
    pub signal_ui: &'a Arc<dyn Fn() + Send + Sync>,
    /// This window's id, what its views' writes are sent as.
    pub client_id: &'a str,
}

impl Context<'_> {
    fn emit(&self, event: BackendEvent) {
        let _ = self.event_tx.try_send(event);
        (self.signal_ui)();
    }
}

/// An API error as the app shows it: the sentence, and a store whose owner
/// is away says so as a notice, the way the app recognises a sentence meant
/// to be read as it is (`events::show_notice`, reached by `Forbidden: `; the
/// server's own refusals already read that way).
pub fn error_message(e: &ApiError) -> String {
    match e {
        ApiError::OwnerOffline { sentence } => format!("Forbidden: {sentence}"),
        other => other.sentence(),
    }
}

fn failed(e: ApiError) -> Option<BackendEvent> {
    Some(BackendEvent::Error { message: error_message(&e) })
}

/// What one change the API heard is for the UI, in order: the update for the
/// views holding the node (unless this window's own views made it, which
/// hand it to each other themselves), then the change for the tree. `last`
/// is the update handed over last: the server announces one update once per
/// kind of change it made (a content edit that also renamed), and a view
/// takes it once.
pub fn change_events(change: Change, client_id: &str, last: &mut Option<(StoreId, NodeId, Vec<u8>)>) -> Vec<BackendEvent> {
    match change {
        Change::Resynced { store } => {
            *last = None;
            vec![BackendEvent::Resynced { store_id: store }]
        }
        Change::Store(c) => {
            let mut events = Vec::new();
            let mine = c.source.as_deref() == Some(client_id);
            if let (Some(node), Some(update), false) = (pimble_api::backend::document_of(&c.kind), c.update, mine) {
                let key = (c.store, node, update);
                if last.as_ref() != Some(&key) {
                    events.push(BackendEvent::RemoteChanges { store_id: c.store, node_id: node, update: key.2.clone() });
                    *last = Some(key);
                }
            }
            events.push(BackendEvent::RemoteStoreChange { store_id: c.store, change_kind: c.kind, source_client_id: c.source });
            events
        }
    }
}

/// Answer `cmd`: through the API when it reads or writes a store, otherwise
/// on the host connection.
/// A failure of a request a modal waits on is answered as `Failed`, naming
/// it (`crate::protocol::Asked`).
pub async fn process_command(ctx: &Context<'_>, cmd: BackendCommand) -> Option<BackendEvent> {
    let asked = crate::protocol::Asked::of(&cmd);
    let answer = match api_command(ctx, cmd).await {
        Ok(event) => event,
        Err(cmd) => host_command(ctx, cmd).await,
    };
    crate::protocol::Asked::answer(asked, answer)
}

/// Where the node and its title were before a move: after a transplant the
/// node is a tombstone under its old id.
async fn before_move(api: &Pimble, addr: Addr) -> Result<(NodeId, String), ApiError> {
    let node = api.node(addr).await?;
    Ok((node.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())), node.metadata.title))
}

fn moved_event(store_id: StoreId, node_id: NodeId, old_parent_id: NodeId, title: String, new_parent_id: NodeId, moved: pimble_api::Moved) -> BackendEvent {
    if moved.node == node_id {
        BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id }
    } else {
        BackendEvent::NodeTransplanted {
            from_store_id: store_id,
            old_node_id: node_id,
            old_parent_id,
            to_store_id: store_id,
            node_id: moved.node,
            new_parent_id,
            title,
            left_shares: moved.left_shares,
        }
    }
}

/// A new node's first state as the API takes it.
fn node_init(title: String, init: Option<crate::protocol::NodeInit>) -> pimble_api::NodeInit {
    match init {
        Some(init) => pimble_api::NodeInit {
            node_type: init.node_type,
            title,
            content: (!init.content.is_empty()).then_some(init.content),
            custom: init.custom,
        },
        None => pimble_api::NodeInit { node_type: node_types::DOCUMENT.to_string(), title, content: None, custom: Vec::new() },
    }
}

/// Answer `cmd` through the API when it reads or writes a store; any other
/// command comes back untouched.
pub async fn api_command(ctx: &Context<'_>, cmd: BackendCommand) -> Result<Option<BackendEvent>, BackendCommand> {
    let api = ctx.api;
    use BackendCommand as C;
    Ok(match cmd {
        C::GetNode { store_id, node_id } => match api.node(Addr::new(store_id, node_id)).await {
            Ok(node) => Some(BackendEvent::NodeLoaded { store_id, node }),
            Err(e) => failed(e),
        },

        C::GetChildren { store_id, node_id } => match api.listing(Addr::new(store_id, node_id)).await {
            Ok((children_store_id, children)) => Some(BackendEvent::ChildrenLoaded { store_id, parent_id: node_id, children_store_id, children }),
            Err(e) => failed(e),
        },

        C::CreateNode { store_id, parent_id, title, init, place } => {
            let request = init.as_ref().and_then(|init| init.request);
            // No parent is the store's root, as the server takes it.
            let parent = match parent_id {
                Some(parent) => parent,
                None => match api.store_row(store_id).await {
                    Some(row) => row.root_node_id,
                    None => return Ok(failed(ApiError::not_found("That store is not open here."))),
                },
            };
            match api.backend().create_node(store_id, parent, node_init(title, init), place).await {
                Ok(node_id) => Some(BackendEvent::NodeCreated { store_id, parent_id: Some(parent), node_id, request }),
                Err(e) => failed(e),
            }
        }

        C::CreateAttachment { store_id, host_id, title, init } => {
            let request = init.request;
            match api.backend().create_attachment(store_id, host_id, node_init(title, Some(init))).await {
                Ok(node_id) => Some(BackendEvent::AttachmentCreated { store_id, host_id, node_id, request }),
                Err(e) => failed(e),
            }
        }

        C::ListAttachments { store_id, node_id } => match api.attachments(Addr::new(store_id, node_id)).await {
            Ok(nodes) => Some(BackendEvent::AttachmentsListed { store_id, node_id, nodes }),
            Err(e) => failed(e),
        },

        C::ListDiscussions { store_id } => match api.discussions(store_id).await {
            Ok(nodes) => Some(BackendEvent::DiscussionsListed { store_id, nodes }),
            Err(e) => failed(e),
        },

        C::ApplyUpdate { store_id, node_id, update } => match api.apply(Addr::new(store_id, node_id), update).await {
            Ok(()) => Some(BackendEvent::NodeContentUpdated { store_id, node_id }),
            Err(e) => failed(e),
        },

        C::BroadcastChanges { store_id, node_id, update } => match api.apply(Addr::new(store_id, node_id), update).await {
            Ok(()) => None,
            // A refusal is the server (or the account) saying no, and no
            // later attempt will change its mind, so the person is told.
            // Anything else is the connection's, which the API remakes and
            // the views reconcile across.
            Err(e) if e.is_refusal() || matches!(e, ApiError::OwnerOffline { .. }) => failed(e),
            Err(e) => {
                tracing::warn!("an edit of {node_id} was not sent: {e}");
                None
            }
        },

        // Rename and appearance are targeted edits of the node's document
        // through the one write; the views of this window holding the node
        // take the same update (the server relays it to everyone else).
        C::RenameNode { store_id, node_id, title } => {
            let edited = api
                .edit(Addr::new(store_id, node_id), move |doc, now| -> Result<(), ApiError> {
                    doc.set_title(&title, now)?;
                    doc.set_custom(custom_keys::EXPLICIT_TITLE, &serde_json::Value::Bool(true), now)?;
                    Ok(())
                })
                .await;
            renamed(ctx, store_id, node_id, edited)
        }

        C::SetNodeAppearance { store_id, node_id, icon, color, background, tags } => {
            let edited = api
                .edit(Addr::new(store_id, node_id), move |doc, now| -> Result<(), ApiError> {
                    // `None` leaves a key alone; `Some(None)`, or an empty
                    // string, removes it, as `NodeMetadata::set_icon` does.
                    for (key, value) in [(custom_keys::ICON, icon), (custom_keys::COLOR, color), (custom_keys::BACKGROUND, background)] {
                        let Some(value) = value else { continue };
                        match value.filter(|s| !s.is_empty()) {
                            Some(value) => doc.set_custom(key, &serde_json::Value::String(value), now)?,
                            None => {
                                doc.remove_custom(key, now)?;
                            }
                        }
                    }
                    if let Some(tags) = tags {
                        doc.set_tags(&tags, now)?;
                    }
                    Ok(())
                })
                .await;
            renamed(ctx, store_id, node_id, edited)
        }

        C::DeleteNode { store_id, node_id } => {
            let addr = Addr::new(store_id, node_id);
            let parent_id = match api.node(addr).await {
                Ok(node) => node.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())),
                Err(e) => return Ok(failed(e)),
            };
            match api.delete(addr).await {
                Ok(()) => Some(BackendEvent::NodeDeleted { store_id, node_id, parent_id }),
                Err(e) => failed(e),
            }
        }

        // Moves were planned by the explorer (`pimble_core::plan` over the
        // app's view); the backend judges them again and is authoritative.
        C::MoveNode { store_id, node_id, new_parent_id, place } => {
            let (old_parent_id, title) = match before_move(api, Addr::new(store_id, node_id)).await {
                Ok(v) => v,
                Err(e) => return Ok(failed(e)),
            };
            match api.backend().move_node(store_id, node_id, new_parent_id, place).await {
                Ok(moved) => Some(moved_event(store_id, node_id, old_parent_id, title, new_parent_id, moved)),
                Err(e) => failed(e),
            }
        }

        C::MoveNodes { store_id, node_ids, new_parent_id, place } => {
            // Each node once, as the answer names them.
            let (once, _) = pimble_crdt::move_order(&node_ids, place);
            let mut before = Vec::with_capacity(once.len());
            for &node_id in &once {
                match before_move(api, Addr::new(store_id, node_id)).await {
                    Ok((old_parent_id, title)) => before.push((node_id, old_parent_id, title)),
                    Err(e) => return Ok(failed(e)),
                }
            }
            match api.backend().move_nodes(store_id, node_ids, new_parent_id, place).await {
                Ok(answer) => {
                    let mut events: Vec<BackendEvent> = before
                        .into_iter()
                        .zip(answer)
                        .map(|((node_id, old_parent_id, title), moved)| moved_event(store_id, node_id, old_parent_id, title, new_parent_id, moved))
                        .collect();
                    let last = events.pop();
                    for event in events {
                        ctx.emit(event);
                    }
                    last
                }
                Err(e) => failed(e),
            }
        }

        C::TransplantNode { from_store_id, node_id, to_store_id, new_parent_id, place } => {
            let (old_parent_id, title) = match before_move(api, Addr::new(from_store_id, node_id)).await {
                Ok(v) => v,
                Err(e) => return Ok(failed(e)),
            };
            match api.backend().transplant(from_store_id, node_id, to_store_id, new_parent_id, place).await {
                Ok(moved) => Some(BackendEvent::NodeTransplanted {
                    from_store_id,
                    old_node_id: node_id,
                    old_parent_id,
                    to_store_id,
                    node_id: moved.node,
                    new_parent_id,
                    title,
                    left_shares: moved.left_shares,
                }),
                Err(e) => failed(e),
            }
        }

        C::UndeleteNode { store_id, node_id } => {
            let addr = Addr::new(store_id, node_id);
            if let Err(e) = api.undelete(addr).await {
                return Ok(failed(e));
            }
            // Back under its parent: the list it went back into is what the
            // tree refetches on `NodeCreated`.
            match api.node(addr).await {
                Ok(node) => Some(BackendEvent::NodeCreated { store_id, parent_id: node.parent_id, node_id, request: None }),
                Err(e) => failed(e),
            }
        }

        C::ListDeleted { store_id } => match api.list_deleted(store_id).await {
            Ok(nodes) => Some(BackendEvent::DeletedListed { store_id, nodes }),
            Err(e) => failed(e),
        },

        C::GetAncestors { store_id, node_id } => {
            // Parent by parent, as far as this device may read: a member's
            // replica stops at the share's root, whose parent it does not
            // hold, and a parent it cannot read is no row to open. A chain
            // longer than any real tree is broken.
            let mut ancestors = Vec::new();
            let mut at = api.node(Addr::new(store_id, node_id)).await.ok();
            while let Some(node) = at.take() {
                let Some(parent) = node.parent_id else { break };
                if ancestors.contains(&parent) || ancestors.len() >= 256 {
                    break;
                }
                if let Ok(parent_node) = api.node(Addr::new(store_id, parent)).await {
                    ancestors.push(parent);
                    at = Some(parent_node);
                }
            }
            ancestors.reverse();
            Some(BackendEvent::AncestorsLoaded { store_id, node_id, ancestors })
        }

        C::ResolveLink { url, hops } => match api.backend().resolve_link(url.store, url.node).await {
            Ok(resolution) => Some(BackendEvent::LinkResolved { url, hops, resolution }),
            Err(e) => failed(e),
        },

        C::ReconcileNodeContent { store_id, node_id, state_vector } => match api.reconcile(Addr::new(store_id, node_id), state_vector).await {
            Ok((diff, server_state_vector)) => Some(BackendEvent::NodeContentReconciled { store_id, node_id, diff, server_state_vector }),
            Err(e) => Some(BackendEvent::Error { message: format!("Reconcile failed: {}", error_message(&e)) }),
        },

        C::SubscribeStoreChanges { store_id } => match api.watch(store_id).await {
            Ok(()) => None,
            Err(e) => Some(BackendEvent::Error { message: format!("Subscribe failed: {}", error_message(&e)) }),
        },

        C::Search { query, stores, limit, purpose } => {
            // Hybrid (keyword + semantic) where a store's backend can; a
            // server built without an ONNX link mode, or an encrypted store
            // searched in the page, answers by words.
            let results = api.search(&query, stores, true, limit).await.map_err(|e| error_message(&e));
            Some(BackendEvent::SearchResults { purpose, results })
        }

        // Presence (docs/PRESENCE_CONTRACT.md): the frame is the API's
        // (`pimble_api::presence`), its session named after the pane. A
        // frame that is not taken (an older server, a store not readable)
        // is no one's business.
        C::PublishPresence { store_id, node_id, pane, focused, left, text, board } => {
            let frame = presence_frame(api, store_id, node_id, pane, focused, left, text, board);
            if let Err(e) = api.backend().publish_presence(frame, ctx.client_id).await {
                tracing::debug!("presence in {store_id} not taken: {e}");
            }
            None
        }

        // Both picture commands run beside the command loop, not in it: a
        // 20 MiB picture takes a moment to send and a huge one seconds to
        // scale down, and typing must not wait behind either.
        C::PutBlob { store_id, node_id, bytes, request_id } => {
            let (api, event_tx, signal_ui) = (api.clone(), ctx.event_tx.clone(), ctx.signal_ui.clone());
            spawn_task(async move {
                let result = store_picture(&api, Addr::new(store_id, node_id), bytes).await;
                let _ = event_tx.try_send(BackendEvent::BlobStored { request_id, result });
                signal_ui();
            });
            None
        }

        C::GetBlob { url } => {
            let (api, event_tx, signal_ui) = (api.clone(), ctx.event_tx.clone(), ctx.signal_ui.clone());
            spawn_task(async move {
                let result = match api.get_blob(url.store, url.blob).await {
                    Ok((_mime, bytes)) => Ok(Arc::new(bytes)),
                    Err(e) => Err(e.sentence()),
                };
                let _ = event_tx.try_send(BackendEvent::BlobLoaded { url, result });
                signal_ui();
            });
            None
        }

        other => return Err(other),
    })
}

/// The frame pane `pane` says where it is with: what it holds and where it
/// is in it, or, `left`, that it let go.
#[allow(clippy::too_many_arguments)]
pub fn presence_frame(
    api: &Pimble,
    store_id: StoreId,
    node_id: NodeId,
    pane: usize,
    focused: bool,
    left: bool,
    text: Option<pimble_rpc::TextPresence>,
    board: Option<pimble_rpc::BoardPresence>,
) -> pimble_rpc::PresenceFrame {
    let body = (!left).then(|| pimble_rpc::PresenceBody { device: crate::presence::device_label(), focused, text, board });
    api.frames().frame(store_id, node_id, &pane.to_string(), body)
}

/// What a rename or an appearance change answers: the tree reads the node
/// again (`NodeRenamed`), and the views of this window holding it take the
/// update first.
fn renamed(ctx: &Context<'_>, store_id: StoreId, node_id: NodeId, edited: Result<pimble_api::Edited<()>, ApiError>) -> Option<BackendEvent> {
    match edited {
        Ok(edited) => {
            if let Some(update) = edited.delta {
                ctx.emit(BackendEvent::RemoteChanges { store_id, node_id, update });
            }
            Some(BackendEvent::NodeRenamed { store_id, node_id })
        }
        Err(e) => failed(e),
    }
}

/// Answer what only a Pimble server's operator does, on the host
/// connection.
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
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
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

        BackendCommand::CreateMount { store_id, parent_id, source_store_id, source_node_id, title } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_mount(store_id, parent_id, source_store_id, source_node_id, title).await {
                Ok((node_id, mount_ref)) => Some(BackendEvent::MountCreated { store_id, parent_id, node_id, mount_ref }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetMountState { store_id, node_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_mount_state(store_id, node_id).await {
                Ok((state, mount_ref)) => Some(BackendEvent::MountStateChanged { store_id, node_id, state, mount_ref }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
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
                Ok(answer) => Some(store_sync_changed(store_id, answer)),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md). Every
        // failure is a `CloudError` naming its operation, never the generic
        // `Error`, so the modal that asked gets it (decision 3).
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

        BackendCommand::CloudStatus => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Status, message: "Not connected".into() });
            };
            match c.cloud_status().await {
                Ok(status) => Some(cloud_status_event(status)),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Status, message: e.to_string() }),
            }
        }

        BackendCommand::CloudSignIn { url, email, password } => {
            let Some(c) = ctx.host.as_ref() else {
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
                    user_id: None,
                    url: Some(url),
                }),
            }
        }

        BackendCommand::CloudSignOut => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::SignOut, message: "Not connected".into() });
            };
            match c.cloud_sign_out().await {
                Ok(()) => Some(BackendEvent::CloudStatusChanged { signed_in: false, email: None, user_id: None, url: None }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::SignOut, message: e.to_string() }),
            }
        }

        BackendCommand::CloudHostStore { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::HostStore, message: "Not connected".into() });
            };
            match c.cloud_host_store(store_id).await {
                Ok(store_id) => Some(BackendEvent::CloudStoreHosted { store_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::HostStore, message: e.to_string() }),
            }
        }

        BackendCommand::CloudListHostedStores => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ListHostedStores, message: "Not connected".into() });
            };
            // The whole answer: which rows are relay-tier rides beside them.
            match c.cloud_list_hosted_stores_response().await {
                Ok(answer) => Some(BackendEvent::CloudHostedStoresListed { stores: answer.stores, relayed: answer.relayed }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::ListHostedStores, message: e.to_string() }),
            }
        }

        BackendCommand::SubscribePresence { store_id } => {
            let c = ctx.host.as_ref()?;
            let sub = match c.subscribe_presence(store_id, ctx.client_id).await {
                Ok(sub) => sub,
                Err(e) => {
                    tracing::debug!("no presence for store {store_id}: {e}");
                    return None;
                }
            };
            let mine = ctx.client_id.to_string();
            let theirs = move |frame: &pimble_rpc::PresenceFrame| frame.client() != mine;
            for frame in c.get_presence(store_id).await.unwrap_or_default().into_iter().filter(|f| theirs(f)) {
                let _ = ctx.event_tx.try_send(BackendEvent::PresenceFrame { frame });
            }
            (ctx.signal_ui)();
            let tx = ctx.event_tx.clone();
            let signal = ctx.signal_ui.clone();
            let mut sub = sub;
            spawn_task(async move {
                while let Some(Ok(frame)) = sub.next().await {
                    if !theirs(&frame) {
                        continue;
                    }
                    if let Err(e) = tx.try_send(BackendEvent::PresenceFrame { frame }) {
                        tracing::debug!("a presence frame was dropped: {e}");
                    }
                    signal();
                }
            });
            None
        }

        // Names (docs/PRESENCE_CONTRACT.md section 3).
        BackendCommand::CloudListShares => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Shares, message: "Not connected".into() });
            };
            Some(shares_listed(c).await)
        }
        BackendCommand::CloudSetDisplayName { name } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Shares, message: "Not connected".into() });
            };
            match c.cloud_set_display_name(&name).await {
                Ok(()) => Some(shares_listed(c).await),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Shares, message: e.to_string() }),
            }
        }
        BackendCommand::CloudSetShareName { store_id, root, name } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Shares, message: "Not connected".into() });
            };
            match c.cloud_set_share_name(store_id, root, &name).await {
                Ok(()) => Some(shares_listed(c).await),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::Shares, message: e.to_string() }),
            }
        }
        BackendCommand::GetPeople { store_id } => {
            let c = ctx.host.as_ref()?;
            match c.cloud_people(store_id).await {
                Ok(people) => Some(BackendEvent::PeopleLoaded { store_id, people }),
                Err(e) => {
                    tracing::debug!("people of store {store_id} not loaded: {e}");
                    None
                }
            }
        }

        // Sharing from this computer (docs/RELAY_CONTRACT.md). The link's
        // state arrives afterwards as ordinary `SyncStateChanged`
        // notifications; a refusal is the server's own sentence.
        BackendCommand::CloudRelayStore { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::RelayStore, message: "Not connected".into() });
            };
            match c.cloud_relay_store(store_id).await {
                Ok(store_id) => Some(BackendEvent::CloudStoreRelayed { store_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::RelayStore, message: e.to_string() }),
            }
        }

        BackendCommand::CloudStopRelaying { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::StopRelaying, message: "Not connected".into() });
            };
            match c.cloud_stop_relaying(store_id).await {
                Ok(()) => Some(BackendEvent::CloudRelayingStopped { store_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::StopRelaying, message: e.to_string() }),
            }
        }

        BackendCommand::CloudAddHostedStore { store_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::AddHostedStore, message: "Not connected".into() });
            };
            // The replica arrives the way `AddRemoteStore`'s does.
            match c.cloud_add_hosted_store(store_id).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::AddHostedStore, message: e.to_string() }),
            }
        }

        BackendCommand::CloudShareNode { store_id, node_id, name } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::Share, message: "Not connected".into() });
            };
            Some(share_answer(CloudOp::Share, store_id, node_id, c.cloud_share_node(store_id, node_id, &name).await))
        }

        BackendCommand::CloudShareInfo { store_id, node_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ShareInfo, message: "Not connected".into() });
            };
            Some(share_answer(CloudOp::ShareInfo, store_id, node_id, c.cloud_share_info(store_id, node_id).await))
        }

        BackendCommand::CloudShareInvite { store_id, node_id, email, role } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ShareInvite, message: "Not connected".into() });
            };
            Some(share_answer(CloudOp::ShareInvite, store_id, node_id, c.cloud_share_invite(store_id, node_id, &email, role).await))
        }

        BackendCommand::CloudShareRemoveMember { store_id, node_id, email } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::ShareRemoveMember, message: "Not connected".into() });
            };
            Some(share_answer(CloudOp::ShareRemoveMember, store_id, node_id, c.cloud_share_remove_member(store_id, node_id, &email).await))
        }

        BackendCommand::CloudStopSharing { store_id, node_id } => {
            let Some(c) = ctx.host.as_ref() else {
                return Some(BackendEvent::CloudError { op: CloudOp::StopSharing, message: "Not connected".into() });
            };
            match c.cloud_stop_sharing(store_id, node_id).await {
                Ok(()) => Some(BackendEvent::CloudSharingStopped { store_id, node_id }),
                Err(e) => Some(BackendEvent::CloudError { op: CloudOp::StopSharing, message: e.to_string() }),
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

        other => {
            tracing::warn!("a command reached the host that is the API's: {other:?}");
            None
        }
    }
}

/// Fit a picture under the size limit and store it: the one way a picture
/// the person adds reaches a store (docs/IMAGES_CONTRACT.md "Too large").
/// Every error is a sentence for the person.
async fn store_picture(api: &Pimble, addr: Addr, bytes: Vec<u8>) -> Result<crate::protocol::StoredBlob, String> {
    let fitted = fit_picture(bytes).await?;
    let url = api.put_blob(addr, fitted.mime.as_str(), &fitted.bytes).await.map_err(|e| {
        let message = e.sentence();
        message.strip_prefix("Forbidden: ").map(str::to_string).unwrap_or(message)
    })?;
    let size = ::image::ImageReader::new(std::io::Cursor::new(&fitted.bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok());
    Ok(crate::protocol::StoredBlob { url, size, notice: fitted.notice })
}

/// `pimble_image::fit` off the runtime's own threads: scaling a huge picture
/// down decodes and re-encodes it, which takes seconds.
#[cfg(feature = "native")]
async fn fit_picture(bytes: Vec<u8>) -> Result<pimble_image::Fitted, String> {
    tokio::task::spawn_blocking(move || pimble_image::fit(bytes))
        .await
        .map_err(|e| format!("The picture could not be read: {e}"))?
        .map_err(|e| e.to_string())
}

/// The browser has one thread, so the page waits while an oversize picture
/// is scaled. One under the limit is only looked at, not decoded.
#[cfg(not(feature = "native"))]
async fn fit_picture(bytes: Vec<u8>) -> Result<pimble_image::Fitted, String> {
    pimble_image::fit(bytes).map_err(|e| e.to_string())
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
    }
}

/// The `CloudStatusChanged` event for a `cloudStatus` answer.
fn cloud_status_event(status: pimble_rpc::CloudStatusResponse) -> BackendEvent {
    BackendEvent::CloudStatusChanged { signed_in: status.signed_in, email: status.email, user_id: status.user_id, url: status.url }
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

/// The event for a sharing RPC's answer (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5): the share and its members, or the error under the operation
/// that asked.
fn share_answer(
    op: CloudOp,
    store_id: pimble_core::StoreId,
    node_id: pimble_core::NodeId,
    answer: pimble_client::Result<pimble_rpc::CloudShareInfoResponse>,
) -> BackendEvent {
    match answer {
        Ok(response) => BackendEvent::CloudShareUpdated { store_id, node_id, share: response.share, members: response.members },
        Err(e) => BackendEvent::CloudError { op, message: e.to_string() },
    }
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

/// `CloudSharesListed`, or the error that kept it from being listed.
async fn shares_listed(c: &PimbleClient) -> BackendEvent {
    match c.cloud_list_shares().await {
        Ok(shares) => BackendEvent::CloudSharesListed { shares },
        Err(e) => BackendEvent::CloudError { op: CloudOp::Shares, message: e.to_string() },
    }
}
