//! The app's commands through the router (docs/API_CONTRACT.md wave 4): a
//! thin adapter from `BackendCommand` to [`pimble_api::Backend`] calls on a
//! [`Router`], and from their answers to the `BackendEvent`s the app waits
//! for, in the shapes `pimble_app::commands::process_command` gives them on
//! the desktop.
//!
//! What goes through it: every command an encrypted store answers that the
//! `Backend` trait covers (the vault is a backend, `crate::vault_backend`),
//! every transplant (it spans two stores, which is the router's to judge),
//! and a search that names an encrypted store (each store to its own
//! backend). A plain store's other commands still go to
//! `process_command`, shared with the desktop, until the app moves onto the
//! API in wave 5; they pass the router's write gate first ([`gate`]).

use base64::Engine as _;
use pimble_api::router::{BackendId, Write};
use pimble_api::{ApiError, Backend, Router};
use pimble_app::protocol::{BackendCommand, BackendEvent};
use pimble_core::{node_types, NodeId, StoreId};

/// What the router made of a command.
pub enum Routed {
    /// Answered: the events, in order (none for a write the app hears of
    /// another way).
    Answered(Vec<BackendEvent>),
    /// Not the router's (yet): the command comes back untouched.
    Not(BackendCommand),
}

/// What kind of write a command is, for the router's gate; `None` for a
/// command that changes nothing.
///
/// Listed by hand rather than by exclusion: a command added later should
/// have to be thought about once rather than quietly become a write a
/// reader may make.
pub fn write_of(cmd: &BackendCommand) -> Option<Write> {
    use BackendCommand::*;
    match cmd {
        CreateAttachment { init, .. } if init.node_type == node_types::DISCUSSION => Some(Write::Discussion),
        BroadcastChanges { .. } | ApplyUpdate { .. } => Some(Write::Content),
        CreateNode { .. }
        | CreateAttachment { .. }
        | RenameNode { .. }
        | SetNodeAppearance { .. }
        | DeleteNode { .. }
        | UndeleteNode { .. }
        | MoveNode { .. }
        | MoveNodes { .. }
        | TransplantNode { .. }
        | PutBlob { .. }
        | CreateMount { .. }
        | MountRemoteStore { .. }
        | SetStoreSync { .. }
        | RemoveReplica { .. } => Some(Write::Structure),
        _ => None,
    }
}

/// The router's refusal of a writing command on `store`, as the event the
/// app reads it from; `None` when it may go on.
pub fn gate(router: &Router, store: StoreId, cmd: &BackendCommand) -> Option<BackendEvent> {
    let write = write_of(cmd)?;
    let refusal = router.gate(store, write).err()?;
    Some(match cmd {
        BackendCommand::PutBlob { request_id, .. } => BackendEvent::BlobStored { request_id: *request_id, result: Err(refusal.sentence()) },
        _ => error_event(refusal),
    })
}

/// An error as the app shows it. A store whose owner is away says so as a
/// notice, the way the app recognises a sentence meant to be read as it is
/// (`pimble_app::events::show_notice`, reached by `Forbidden: `).
pub fn error_event(e: ApiError) -> BackendEvent {
    match e {
        ApiError::OwnerOffline { sentence } => BackendEvent::Error { message: format!("Forbidden: {sentence}") },
        other => BackendEvent::Error { message: other.sentence() },
    }
}

fn one(event: BackendEvent) -> Routed {
    Routed::Answered(vec![event])
}

fn failed(e: ApiError) -> Routed {
    one(error_event(e))
}

/// The old parent and the title of a node about to move, read before the
/// move: after a transplant the node is a tombstone under its old id.
async fn before_move(router: &Router, store: StoreId, node: NodeId) -> Result<(NodeId, String), ApiError> {
    let n = router.fetch_one(store, node).await?;
    Ok((n.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())), n.metadata.title))
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

/// Answer `cmd` through the router when it is the router's: see the module
/// comment for which. `vault` is the backend encrypted stores are served
/// by; `root_of` names a store's root, for a node made with no parent.
pub async fn answer(router: &Router, vault: BackendId, cmd: BackendCommand, root_of: impl Fn(StoreId) -> Option<NodeId>) -> Routed {
    let on_vault = |store: StoreId| router.served_by(store) == vault;
    use BackendCommand as C;
    match cmd {
        C::TransplantNode { from_store_id, node_id, to_store_id, new_parent_id, place } => {
            let (old_parent_id, title) = match before_move(router, from_store_id, node_id).await {
                Ok(v) => v,
                Err(e) => return failed(e),
            };
            match router.transplant(from_store_id, node_id, to_store_id, new_parent_id, place).await {
                Ok(moved) => one(BackendEvent::NodeTransplanted {
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

        C::Search { query, stores, limit, purpose } if stores.iter().any(|id| on_vault(*id)) => {
            // Hybrid where a backend can; an encrypted store's are words.
            let results = router.search(&query, stores, true, limit).await.map_err(|e| e.sentence());
            one(BackendEvent::SearchResults { purpose, results })
        }

        C::GetChildren { store_id, node_id } if on_vault(store_id) => match router.children(store_id, node_id).await {
            Ok((children_store_id, children)) => one(BackendEvent::ChildrenLoaded { store_id, parent_id: node_id, children_store_id, children }),
            Err(e) => failed(e),
        },

        C::GetNode { store_id, node_id } if on_vault(store_id) => match router.fetch_one(store_id, node_id).await {
            Ok(node) => one(BackendEvent::NodeLoaded { store_id, node }),
            Err(e) => failed(e),
        },

        C::CreateNode { store_id, parent_id, title, init, place } if on_vault(store_id) => {
            let Some(parent) = parent_id.or_else(|| root_of(store_id)) else {
                return failed(ApiError::not_found("That encrypted store is not open in this page."));
            };
            let request = init.as_ref().and_then(|init| init.request);
            let init = match init {
                Some(init) => pimble_api::NodeInit {
                    node_type: init.node_type,
                    title,
                    content: (!init.content.is_empty()).then_some(init.content),
                    custom: init.custom,
                },
                None => pimble_api::NodeInit { node_type: node_types::DOCUMENT.to_string(), title, content: None, custom: Vec::new() },
            };
            match router.create_node(store_id, parent, init, place).await {
                Ok(node_id) => one(BackendEvent::NodeCreated { store_id, parent_id: Some(parent), node_id, request }),
                Err(e) => failed(e),
            }
        }

        C::CreateAttachment { store_id, host_id, title, init } if on_vault(store_id) => {
            let request = init.request;
            let init = pimble_api::NodeInit {
                node_type: init.node_type,
                title,
                content: (!init.content.is_empty()).then_some(init.content),
                custom: init.custom,
            };
            match router.create_attachment(store_id, host_id, init).await {
                Ok(node_id) => one(BackendEvent::AttachmentCreated { store_id, host_id, node_id, request }),
                Err(e) => failed(e),
            }
        }

        C::ListAttachments { store_id, node_id } if on_vault(store_id) => match router.list_attachments(store_id, node_id).await {
            Ok(nodes) => one(BackendEvent::AttachmentsListed { store_id, node_id, nodes }),
            Err(e) => failed(e),
        },

        C::DeleteNode { store_id, node_id } if on_vault(store_id) => {
            let parent_id = match router.fetch_one(store_id, node_id).await {
                Ok(node) => node.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())),
                Err(e) => return failed(e),
            };
            match router.delete(store_id, node_id).await {
                Ok(()) => one(BackendEvent::NodeDeleted { store_id, node_id, parent_id }),
                Err(e) => failed(e),
            }
        }

        C::MoveNode { store_id, node_id, new_parent_id, place } if on_vault(store_id) => {
            let (old_parent_id, title) = match before_move(router, store_id, node_id).await {
                Ok(v) => v,
                Err(e) => return failed(e),
            };
            match router.move_node(store_id, node_id, new_parent_id, place).await {
                Ok(moved) => one(moved_event(store_id, node_id, old_parent_id, title, new_parent_id, moved)),
                Err(e) => failed(e),
            }
        }

        C::MoveNodes { store_id, node_ids, new_parent_id, place } if on_vault(store_id) => {
            // Each node once, as the answer names them.
            let (once, _) = pimble_crdt::move_order(&node_ids, place);
            let mut before = Vec::with_capacity(once.len());
            for &node_id in &once {
                match before_move(router, store_id, node_id).await {
                    Ok((old_parent_id, title)) => before.push((node_id, old_parent_id, title)),
                    Err(e) => return failed(e),
                }
            }
            match router.move_nodes(store_id, node_ids, new_parent_id, place).await {
                Ok(answer) => Routed::Answered(
                    before
                        .into_iter()
                        .zip(answer)
                        .map(|((node_id, old_parent_id, title), moved)| moved_event(store_id, node_id, old_parent_id, title, new_parent_id, moved))
                        .collect(),
                ),
                Err(e) => failed(e),
            }
        }

        C::UndeleteNode { store_id, node_id } if on_vault(store_id) => {
            if let Err(e) = router.undelete(store_id, node_id).await {
                return failed(e);
            }
            // Back under its parent: the list it went back into is what the
            // tree refetches on `NodeCreated`.
            match router.fetch_one(store_id, node_id).await {
                Ok(node) => one(BackendEvent::NodeCreated { store_id, parent_id: node.parent_id, node_id, request: None }),
                Err(e) => failed(e),
            }
        }

        C::ListDeleted { store_id } if on_vault(store_id) => match router.list_deleted(store_id).await {
            Ok(nodes) => one(BackendEvent::DeletedListed { store_id, nodes }),
            Err(e) => failed(e),
        },

        C::ResolveLink { url, hops } if on_vault(url.store) => match router.resolve_link(url.store, url.node).await {
            Ok(resolution) => one(BackendEvent::LinkResolved { url, hops, resolution }),
            Err(e) => failed(e),
        },

        C::BroadcastChanges { store_id, node_id, changes } if on_vault(store_id) => {
            // The editor's outbound closure encodes with the standard
            // alphabet, exactly as the plain path's `applyEdit` carries it.
            let update = match base64::engine::general_purpose::STANDARD.decode(&changes) {
                Ok(update) => update,
                Err(e) => {
                    tracing::warn!("The editor's delta would not decode: {e}");
                    return Routed::Answered(Vec::new());
                }
            };
            match router.apply(store_id, node_id, update, "").await {
                Ok(()) => Routed::Answered(Vec::new()),
                // A refusal is the server (or the account) saying no, and no
                // later attempt will change its mind, so the person is told.
                // Any other failure is recorded against the document and
                // resent on the next connection, which needs no announcement.
                Err(e) if e.is_refusal() || matches!(e, ApiError::OwnerOffline { .. }) => failed(e),
                Err(e) => {
                    tracing::warn!("Appending an encrypted edit failed: {e}");
                    Routed::Answered(Vec::new())
                }
            }
        }

        C::ApplyUpdate { store_id, node_id, update } if on_vault(store_id) => match router.apply(store_id, node_id, update, "").await {
            Ok(()) => one(BackendEvent::NodeContentUpdated { store_id, node_id }),
            Err(e) => failed(e),
        },

        // Capabilities, said (decision 10): an encrypted store in the
        // browser keeps no picture and has no index on any server.
        C::PutBlob { store_id, request_id, .. } if on_vault(store_id) && !router.capabilities(store_id).pictures => {
            one(BackendEvent::BlobStored { request_id, result: Err(pimble_app::pictures::ENCRYPTED_IN_BROWSER.to_string()) })
        }
        C::GetBlob { url } if on_vault(url.store) && !router.capabilities(url.store).pictures => {
            one(BackendEvent::BlobLoaded { url, result: Err(pimble_app::pictures::ENCRYPTED_IN_BROWSER.to_string()) })
        }
        C::RebuildIndex { store_id } if on_vault(store_id) && !router.capabilities(store_id).index_rebuild => {
            one(error_event(ApiError::not_available("An encrypted store has no search index on any server to rebuild: it is searched in this page.")))
        }

        other => Routed::Not(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::Place;

    #[test]
    fn only_the_commands_that_change_something_are_writes() {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        assert_eq!(
            write_of(&BackendCommand::CreateNode { store_id, parent_id: None, title: String::new(), init: None, place: Default::default() }),
            Some(Write::Structure)
        );
        assert_eq!(write_of(&BackendCommand::MoveNode { store_id, node_id, new_parent_id: node_id, place: Place::Last }), Some(Write::Structure));
        assert_eq!(write_of(&BackendCommand::BroadcastChanges { store_id, node_id, changes: String::new() }), Some(Write::Content));
        let discussion = pimble_app::protocol::NodeInit { node_type: node_types::DISCUSSION.into(), content: Vec::new(), custom: Vec::new(), request: None };
        assert_eq!(
            write_of(&BackendCommand::CreateAttachment { store_id, host_id: node_id, title: String::new(), init: discussion }),
            Some(Write::Discussion)
        );
        assert_eq!(write_of(&BackendCommand::GetChildren { store_id, node_id }), None);
        assert_eq!(write_of(&BackendCommand::SubscribeStoreChanges { store_id }), None);
        assert_eq!(write_of(&BackendCommand::ListStores), None);
    }

    #[test]
    fn a_store_whose_owner_is_away_says_so_as_a_notice() {
        let refused = error_event(ApiError::OwnerOffline { sentence: pimble_api::router::OWNER_OFFLINE_REFUSAL.into() });
        assert!(matches!(&refused, BackendEvent::Error { message } if message == &format!("Forbidden: {}", pimble_api::router::OWNER_OFFLINE_REFUSAL)));
        let read_only = error_event(ApiError::ReadOnly);
        assert!(matches!(&read_only, BackendEvent::Error { message } if message == pimble_core::StoreAccess::READ_ONLY_REFUSAL));
    }
}
