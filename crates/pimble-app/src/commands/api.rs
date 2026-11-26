//! The commands that read and edit a store's documents through the API:
//! nodes, lists, content, metanodes, links, search, presence and pictures.
//! What changes the tree's shape is `structure`'s.

use std::sync::Arc;

use pimble_api::{Addr, ApiError, Pimble};
use pimble_core::{custom_keys, NodeId, StoreId};

use super::changes::shown;
use super::{error_message, failed, spawn_task, Context};
use crate::protocol::{BackendCommand, BackendEvent};

/// Answer `cmd` through the API when it reads or writes a store; any other
/// command comes back untouched.
pub async fn api_command(ctx: &Context<'_>, cmd: BackendCommand) -> Result<Option<BackendEvent>, BackendCommand> {
    let api = ctx.api;
    use BackendCommand as C;
    let cmd = match super::structure::structure_command(ctx, cmd).await {
        Ok(answer) => return Ok(answer),
        Err(cmd) => *cmd,
    };
    Ok(match cmd {
        C::GetNode { store_id, node_id } => match api.node(Addr::new(store_id, node_id)).await {
            Ok(node) => Some(BackendEvent::NodeLoaded { store_id, node }),
            Err(e) => Some(BackendEvent::NodeNotLoaded { store_id, node_id, message: error_message(&e) }),
        },

        C::NameNodes { nodes } => {
            let mut names = Vec::with_capacity(nodes.len());
            for (store_id, node_id) in nodes {
                let name = match api.node(Addr::new(store_id, node_id)).await {
                    Ok(node) => Some(crate::protocol::NoteName {
                        label: crate::state::display_label_from_node(&node),
                        follows_text: pimble_core::plan::label_follows_text(&node),
                    }),
                    Err(e) => {
                        tracing::debug!("The note {node_id} a notice names is not read here: {}", error_message(&e));
                        None
                    }
                };
                names.push(((store_id, node_id), name));
            }
            Some(BackendEvent::NotesNamed { names })
        }

        C::LoadChildren { store_id, nodes } => {
            use futures::StreamExt;
            let listings = nodes.into_iter().map(|node_id| async move { (node_id, api.listing(Addr::new(store_id, node_id)).await) });
            let mut answers = futures::stream::iter(listings).buffer_unordered(8);
            while let Some((node_id, answer)) = answers.next().await {
                match answer {
                    Ok((children_store_id, children)) => ctx.emit(BackendEvent::ChildrenLoaded { store_id, parent_id: node_id, children_store_id, children }),
                    Err(e) => tracing::info!("The children of {node_id} in store {store_id} could not be read: {}", error_message(&e)),
                }
            }
            None
        }

        C::GetChildren { store_id, node_id } => match api.listing(Addr::new(store_id, node_id)).await {
            Ok((children_store_id, children)) => Some(BackendEvent::ChildrenLoaded { store_id, parent_id: node_id, children_store_id, children }),
            Err(e) => failed(e),
        },

        C::ListAttachments { store_id, node_id } => match api.attachments(Addr::new(store_id, node_id)).await {
            Ok(nodes) => Some(BackendEvent::AttachmentsListed { store_id, node_id, nodes }),
            Err(e) => failed(e),
        },

        C::ListDiscussions { store_id } => match api.discussions(store_id).await {
            Ok(nodes) => Some(BackendEvent::DiscussionsListed { store_id, nodes }),
            Err(e) => failed(e),
        },

        C::ApplyUpdate { store_id, node_id, update } => match api.apply(Addr::new(store_id, node_id), update).await {
            Ok(()) => {
                if let Some(event) = shown(api, store_id, &pimble_api::Affected::rows(vec![node_id])).await {
                    ctx.emit(event);
                }
                Some(BackendEvent::NodeContentUpdated { store_id, node_id })
            }
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
            renamed(ctx, store_id, node_id, edited).await
        }

        C::SetNodeAppearance { store_id, node_id, icon, color, background, tags, theme } => {
            let edited = api
                .edit(Addr::new(store_id, node_id), move |doc, now| -> Result<(), ApiError> {
                    // `None` leaves a key alone; `Some(None)`, or an empty
                    // string, removes it, as `NodeMetadata::set_icon` does.
                    for (key, value) in [(custom_keys::ICON, icon), (custom_keys::COLOR, color), (custom_keys::BACKGROUND, background), (custom_keys::THEME, theme)] {
                        let Some(value) = value else { continue };
                        match value.filter(|s| !s.is_empty()) {
                            Some(value) => doc.set_custom(key, &serde_json::Value::String(value), now)?,
                            None => {
                                doc.remove_custom(key, now)?;
                            }
                        }
                    }
                    // Made on the tags as the document has them now, so a
                    // tag someone else added meanwhile stays. They are the
                    // entered tags and the #hashtags in the text: taking off
                    // one in the text removes it from the text, and adding a
                    // tag adds an entered one (docs/HASHTAGS_CONTRACT.md).
                    if let Some(change) = tags {
                        let before = doc.fields()?.all_tags();
                        let after = change.apply(&before);
                        if after != before {
                            doc.set_all_tags(&after, now)?;
                        }
                    }
                    Ok(())
                })
                .await;
            renamed(ctx, store_id, node_id, edited).await
        }

        C::GetMetanode { store_id, node_id } => {
            let forest = api.metanode_source(Addr::new(store_id, node_id)).await.map_err(|e| error_message(&e));
            // Where each found node lives, for its row's tooltip and the
            // sentence a refused action on it says.
            let mut lives = Vec::new();
            if let Ok(forest) = &forest {
                for entry in forest.found() {
                    lives.push((entry.addr, api.lives_in(entry.addr).await));
                }
            }
            Some(BackendEvent::MetanodeRead { store_id, node_id, forest, lives })
        }

        C::SetMetanode { store_id, node_id, viewer, source } => {
            let edited = api.set_metanode(Addr::new(store_id, node_id), viewer.as_deref(), source.as_ref()).await;
            renamed(ctx, store_id, node_id, edited).await
        }

        C::GetAncestors { store_id, node_id } => {
            // Parent by parent, as far as this device may read: a member's
            // replica stops at the share's root, whose parent it does not
            // hold, and a parent it cannot read is no row to open. A chain
            // longer than any real tree is broken.
            let mut ancestors = Vec::new();
            let mut at = api.node(Addr::new(store_id, node_id)).await.ok();
            let kind = at.as_ref().and_then(|node| pimble_core::internal::internal_kind(&node.node_type));
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
            Some(BackendEvent::AncestorsLoaded { store_id, node_id, ancestors, kind })
        }

        C::GetThemeChain { store_id, node_id } => {
            // Parent by parent, as `GetAncestors` reads them, but the nodes
            // themselves: the app takes each as a `NodeLoaded`.
            let mut nodes: Vec<pimble_core::Node> = Vec::new();
            let mut at = Some(node_id);
            while let Some(id) = at.take() {
                if nodes.iter().any(|n| n.id == id) || nodes.len() >= 256 {
                    break;
                }
                let Ok(node) = api.node(Addr::new(store_id, id)).await else { break };
                at = node.parent_id;
                nodes.push(node);
            }
            Some(BackendEvent::ThemeChainLoaded { store_id, nodes })
        }

        C::Themes(cmd) => super::themes::theme_command(ctx, cmd).await,

        C::GetProperties { store_id, node_id } => {
            // Where it lives: parent by parent, as `GetAncestors` reads them.
            let mut lives_in = Vec::new();
            if let Ok(above) = api.ancestors(Addr::new(store_id, node_id)).await {
                for addr in above.into_iter().rev() {
                    let Ok(node) = api.node(addr).await else { break };
                    lives_in.push((addr.node, crate::state::display_label_from_node(&node)));
                }
            }
            let store_tags = api.tags_in(store_id).await.unwrap_or_default();
            Some(BackendEvent::PropertiesLoaded { store_id, node_id, lives_in, store_tags })
        }

        C::GetReferences { store_id, node_id } => {
            use crate::properties::{Mention, References, RowsMention};
            let addr = Addr::new(store_id, node_id);
            let mut references = References::default();
            let mention = |addr: Addr, node: &pimble_core::Node| Mention { store_id: addr.store, node_id: addr.node, label: crate::state::display_label_from_node(node) };
            match api.shown_in(addr).await {
                Ok(found) => references.shown_in = found.iter().map(|s| mention(s.metanode, &s.node)).collect(),
                Err(e) => references.not_known.push(error_message(&e)),
            }
            match api.backlinks(addr).await {
                Ok(links) => references.links_here = links.iter().map(|l| mention(l.addr, &l.node)).collect(),
                Err(e) => references.not_known.push(error_message(&e)),
            }
            match api.rows_pointing_here(addr).await {
                Ok(groups) => {
                    references.rows_here = groups
                        .iter()
                        .map(|g| RowsMention {
                            at: mention(g.at, &g.node),
                            table_name: g.table_name.clone(),
                            rows: g.rows.iter().map(|(_, label)| label.clone()).collect(),
                        })
                        .collect()
                }
                Err(e) => references.not_known.push(error_message(&e)),
            }
            Some(BackendEvent::ReferencesLoaded { store_id, node_id, references })
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

        C::Search { query, stores, limit, shown, purpose } => {
            // Hybrid (keyword + semantic) where a store's backend can; a
            // server built without an ONNX link mode, or an encrypted store
            // searched in the page, answers by words.
            let results = api.search_shown(&query, stores, true, limit, &shown).await.map_err(|e| error_message(&e));
            Some(BackendEvent::SearchResults { purpose, results })
        }

        // Presence (docs/PRESENCE_CONTRACT.md): the frame is the API's
        // (`pimble_api::presence`), its session named after the pane. A
        // frame that is not taken (an older server, a store not readable)
        // is no one's business.
        C::PublishPresence { store_id, node_id, session, focused, left, text, board } => {
            let frame = presence_frame(api, store_id, node_id, &session, focused, left, text, board);
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

        C::StorePicture { store_id, node_id, bytes } => {
            Some(BackendEvent::BlobStored { request_id: 0, result: store_picture(&api, Addr::new(store_id, node_id), bytes).await })
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

        // What this device may do in the store changed (a role, a
        // read-only root): the server's judgement of each node the window
        // shows is asked again, once, and shown.
        C::Rejudge { store_id, node_ids } => {
            let nodes = api.rejudge(store_id, node_ids).await;
            (!nodes.is_empty()).then_some(BackendEvent::Shown { view: pimble_api::View { store: store_id, nodes, lists: Vec::new(), gone: Vec::new() } })
        }

        C::GetMountState { store_id, node_id } => match api.backend().mount_state(store_id, node_id).await {
            Ok((state, mount_ref)) => Some(BackendEvent::MountStateChanged { store_id, node_id, state, mount_ref }),
            Err(e) => failed(e),
        },

        // Who else is in the store's documents (docs/PRESENCE_CONTRACT.md):
        // the frames held now, then each as it comes, for as long as the
        // subscription lives. A store that has none (an older server, a
        // store not readable) is no one's business.
        C::SubscribePresence { store_id } => {
            let (now, mut frames) = match api.backend().presence(store_id, ctx.client_id).await {
                Ok(subscribed) => subscribed,
                Err(e) => {
                    tracing::debug!("no presence for store {store_id}: {e}");
                    return Ok(None);
                }
            };
            for frame in now {
                let _ = ctx.event_tx.try_send(BackendEvent::PresenceFrame { frame });
            }
            (ctx.signal_ui)();
            let (tx, signal) = (ctx.event_tx.clone(), ctx.signal_ui.clone());
            spawn_task(async move {
                use futures::StreamExt;
                while let Some(frame) = frames.next().await {
                    if let Err(e) = tx.try_send(BackendEvent::PresenceFrame { frame }) {
                        tracing::debug!("a presence frame was dropped: {e}");
                    }
                    signal();
                }
            });
            None
        }

        // Names (docs/PRESENCE_CONTRACT.md section 3).
        C::GetPeople { store_id } => match api.backend().people(store_id).await {
            Ok(people) => Some(BackendEvent::PeopleLoaded { store_id, people }),
            Err(e) => {
                tracing::debug!("people of store {store_id} not loaded: {e}");
                None
            }
        },

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
    session: &str,
    focused: bool,
    left: bool,
    text: Option<pimble_rpc::TextPresence>,
    board: Option<pimble_rpc::BoardPresence>,
) -> pimble_rpc::PresenceFrame {
    let body = (!left).then(|| pimble_rpc::PresenceBody { device: crate::presence::device_label(), focused, text, board });
    api.frames().frame(store_id, node_id, session, body)
}

/// What a rename or an appearance change answers: the views of this window
/// holding the node take the update first, then the tree shows the node as
/// the edit left it (`Shown`, read from the document the edit was merged
/// into).
async fn renamed(ctx: &Context<'_>, store_id: StoreId, node_id: NodeId, edited: Result<pimble_api::Edited<()>, ApiError>) -> Option<BackendEvent> {
    match edited {
        Ok(edited) => {
            if let Some(update) = edited.delta {
                ctx.emit(BackendEvent::RemoteChanges { store_id, node_id, update });
            }
            shown(ctx.api, store_id, &pimble_api::Affected::rows(vec![node_id])).await
        }
        Err(e) => failed(e),
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
