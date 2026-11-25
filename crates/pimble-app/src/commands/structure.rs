//! The commands that change the shape of a store's tree: creating, splitting
//! and joining nodes, deleting and putting back, moves inside a store and
//! between stores, and mounts.

use pimble_api::{Addr, ApiError, Pimble};
use pimble_core::{node_types, NodeId, StoreId};

use super::changes::show_after;
use super::{failed, Context};
use crate::protocol::{BackendCommand, BackendEvent};

/// Answer `cmd` when it changes the shape of a store's tree; any other
/// command comes back untouched (boxed: a command is large to carry back).
pub(super) async fn structure_command(ctx: &Context<'_>, cmd: BackendCommand) -> Result<Option<BackendEvent>, Box<BackendCommand>> {
    let api = ctx.api;
    use BackendCommand as C;
    Ok(match cmd {
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
                Ok(node_id) => {
                    show_after(ctx, store_id, created(node_id, parent)).await;
                    Some(BackendEvent::NodeCreated { store_id, parent_id: Some(parent), node_id, request })
                }
                Err(e) => failed(e),
            }
        }

        C::MakeFromRecipe { store_id, parent_id, recipe, place } => {
            let init = pimble_api::recipes::metanode(&recipe.title, recipe.icon.as_deref(), &recipe.viewer, recipe.depth);
            let node_id = match api.backend().create_node(store_id, parent_id, init, place).await {
                Ok(node_id) => node_id,
                Err(e) => return Ok(failed(e)),
            };
            show_after(ctx, store_id, created(node_id, parent_id)).await;
            for title in &recipe.children {
                let child = match pimble_api::recipes::document(title, None) {
                    Ok(init) => init,
                    Err(e) => return Ok(failed(e)),
                };
                match api.backend().create_node(store_id, node_id, child, pimble_core::Place::Last).await {
                    Ok(child_id) => show_after(ctx, store_id, created(child_id, node_id)).await,
                    Err(e) => return Ok(failed(e)),
                }
            }
            Some(BackendEvent::NodeCreated { store_id, parent_id: Some(parent_id), node_id, request: None })
        }

        C::CreateAttachment { store_id, host_id, title, init } => {
            let request = init.request;
            match api.backend().create_attachment(store_id, host_id, node_init(title, Some(init))).await {
                Ok(node_id) => Some(BackendEvent::AttachmentCreated { store_id, host_id, node_id, request }),
                Err(e) => failed(e),
            }
        }

        // docs/SPLIT_CONTRACT.md: the views of this window holding the node
        // take the update the API sent (the server relays it to everyone
        // else), and the tree shows the new node, or the one joined in gone.
        C::SplitNode { store_id, node_id, at, title } => {
            let addr = Addr::new(store_id, node_id);
            let parent = match api.node(addr).await {
                Ok(node) => node.parent_id,
                Err(e) => return Ok(failed(e)),
            };
            match api.split_node(addr, at, title).await {
                Ok(split) => {
                    if let Some(update) = split.kept_delta {
                        ctx.emit(BackendEvent::RemoteChanges { store_id, node_id, update });
                    }
                    let lists: Vec<NodeId> = parent.into_iter().collect();
                    let mut nodes = vec![node_id, split.new.node];
                    nodes.extend(lists.iter().copied());
                    show_after(ctx, store_id, pimble_api::Affected { nodes, lists, ..Default::default() }).await;
                    Some(BackendEvent::NodeSplit { store_id, node_id, new_id: split.new.node })
                }
                Err(e) => failed(e),
            }
        }

        C::JoinBelow { store_id, node_id } => {
            let addr = Addr::new(store_id, node_id);
            let parent = match api.node(addr).await {
                Ok(node) => node.parent_id,
                Err(e) => return Ok(failed(e)),
            };
            match api.join_next(addr).await {
                Ok(joined) => {
                    if let Some(update) = joined.delta {
                        ctx.emit(BackendEvent::RemoteChanges { store_id, node_id, update });
                    }
                    let mut lists = vec![node_id];
                    lists.extend(parent);
                    let nodes = lists.clone();
                    show_after(ctx, store_id, pimble_api::Affected { nodes, lists, gone: vec![joined.gone.node], ..Default::default() }).await;
                    None
                }
                Err(e) => failed(e),
            }
        }

        C::DeleteNode { store_id, node_id } => {
            let addr = Addr::new(store_id, node_id);
            let parent_id = match api.node(addr).await {
                Ok(node) => node.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())),
                Err(e) => return Ok(failed(e)),
            };
            match api.delete(addr).await {
                Ok(()) => {
                    show_after(ctx, store_id, pimble_api::Affected::of(&pimble_rpc::StoreChangeKind::NodeDeleted { node_id, parent_id })).await;
                    Some(BackendEvent::NodeDeleted { store_id, node_id, parent_id })
                }
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
                Ok(moved) => {
                    show_after(ctx, store_id, moved_affected(node_id, old_parent_id, new_parent_id, &moved)).await;
                    Some(moved_event(store_id, node_id, old_parent_id, title, new_parent_id, moved))
                }
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
                    let affected = before
                        .iter()
                        .zip(&answer)
                        .fold(pimble_api::Affected::default(), |all, ((node_id, old_parent_id, _), moved)| all.and(moved_affected(*node_id, *old_parent_id, new_parent_id, moved)));
                    show_after(ctx, store_id, affected).await;
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
                Ok(moved) => {
                    use pimble_rpc::StoreChangeKind as K;
                    show_after(ctx, from_store_id, pimble_api::Affected::of(&K::NodeDeleted { node_id, parent_id: old_parent_id })).await;
                    show_after(ctx, to_store_id, created(moved.node, new_parent_id)).await;
                    Some(BackendEvent::NodeTransplanted {
                    from_store_id,
                    old_node_id: node_id,
                    old_parent_id,
                    to_store_id,
                    node_id: moved.node,
                    new_parent_id,
                    title,
                    left_shares: moved.left_shares,
                    })
                }
                Err(e) => failed(e),
            }
        }

        C::UndeleteNode { store_id, node_id } => {
            let addr = Addr::new(store_id, node_id);
            if let Err(e) = api.undelete(addr).await {
                return Ok(failed(e));
            }
            // Back under its parent: the list it went back into is shown
            // again, read from the documents as they are now.
            match api.node(addr).await {
                Ok(node) => {
                    if let Some(parent) = node.parent_id {
                        show_after(ctx, store_id, created(node_id, parent)).await;
                    }
                    Some(BackendEvent::NodeCreated { store_id, parent_id: node.parent_id, node_id, request: None })
                }
                Err(e) => failed(e),
            }
        }

        C::ListDeleted { store_id } => match api.list_deleted(store_id).await {
            Ok(nodes) => Some(BackendEvent::DeletedListed { store_id, nodes }),
            Err(e) => failed(e),
        },

        C::CreateMount { store_id, parent_id, source_store_id, source_node_id, title } => {
            match api.backend().create_mount(store_id, parent_id, source_store_id, source_node_id, title).await {
                Ok((node_id, mount_ref)) => {
                    show_after(ctx, store_id, created(node_id, parent_id)).await;
                    Some(BackendEvent::MountCreated { store_id, parent_id, node_id, mount_ref })
                }
                Err(e) => failed(e),
            }
        }

        other => return Err(Box::new(other)),
    })
}

/// What a move did to the tree: a move inside the store, or, when the node
/// has a new id (it left a share), a deletion where it was and a creation
/// where it landed.
fn moved_affected(node_id: NodeId, old_parent_id: NodeId, new_parent_id: NodeId, moved: &pimble_api::Moved) -> pimble_api::Affected {
    use pimble_rpc::StoreChangeKind as K;
    if moved.node == node_id {
        pimble_api::Affected::of(&K::NodeMoved { node_id, old_parent_id, new_parent_id })
    } else {
        pimble_api::Affected::of(&K::NodeDeleted { node_id, parent_id: old_parent_id })
            .and(pimble_api::Affected::of(&K::NodeCreated { node_id: moved.node, parent_id: new_parent_id }))
    }
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

/// What a new node does to the tree: a row under `parent`, whose list
/// changed.
pub(super) fn created(node_id: NodeId, parent_id: NodeId) -> pimble_api::Affected {
    pimble_api::Affected::of(&pimble_rpc::StoreChangeKind::NodeCreated { node_id, parent_id })
}
