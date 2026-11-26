//! Backend event processing.
//!
//! Each event handler uses per-entity signal mutations. Only structural events
//! (store open/close, children loaded, node moved) bump `tree_structure_version`.
//!
//! `process_backend_events` is the dispatcher: one `match` on `BackendEvent`,
//! small handlers inline, the rest in a module per concern: `connection`
//! (connect, disconnect, failures), `stores` (stores, mounts, replicas,
//! imports), `nodes` (node changes and a store's change notifications),
//! `sync` (a store's link), `cloud` (the account, hosting, sharing), `view`
//! (what the tree shows) and `links` (following links, restoring panes).

use std::cell::RefCell;

use rinch::prelude::*;

use crate::protocol::{BackendCommand, BackendEvent};
use crate::state::{AppStore, SearchState};

mod cloud;
mod connection;
mod links;
mod nodes;
mod panes;
mod stores;
mod sync;
mod view;

pub(crate) use links::{follow_resolution, reveal_in_tree};
pub(crate) use panes::rejudge_panes;
use links::restore_resolved;
use view::{apply_view, take_node};

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
            BackendEvent::Connected { server_addr, client_id } => connection::connected(store, server_addr, client_id),

            BackendEvent::Disconnected => connection::disconnected(store),

            BackendEvent::StoreOpenFailed { path, message } => {
                tracing::warn!("Store {} could not be opened: {}", path, message);
                store.opening_failed(path, message);
            }

            BackendEvent::IndexStateChanged { store_id, state } => {
                store.set_index_state(*store_id, state.clone());
            }

            BackendEvent::Failed { asked, message } => connection::failed(store, asked, message),

            BackendEvent::Error { message } => connection::error(store, message),

            BackendEvent::StoreOpened { store: opened_store } => stores::store_opened(store, tree_state, opened_store),

            BackendEvent::StoreCreated { store_id, root_node_id } => stores::store_created(store, store_id, root_node_id),

            BackendEvent::StoresListed { stores } => stores::stores_listed(store, tree_state, stores),

            BackendEvent::ChildrenLoaded { store_id, parent_id, children_store_id, children } => {
                nodes::children_loaded(store, store_id, parent_id, children_store_id, children);
            }

            BackendEvent::NodeLoaded { store_id, node } => {
                tracing::debug!("Node loaded: {:?}", node.id);
                if take_node(store, *store_id, node) {
                    store.bump_tree_structure();
                }
            }

            BackendEvent::ThemeChainLoaded { store_id, nodes } => {
                crate::themes::chain_answered(*store_id, nodes);
                let mut changed = false;
                for node in nodes {
                    changed |= take_node(store, *store_id, node);
                }
                if changed {
                    store.bump_tree_structure();
                }
            }

            BackendEvent::ThemesNotListed { store_id } => crate::themes::listing_failed(*store_id),
            BackendEvent::ThemesListed { store_id, themes } => {
                crate::themes::listing_ended(*store_id);
                if store.get_store_signal(*store_id).is_some() {
                    let same = rinch::prelude::untracked(|| store.store_themes.with(|all| all.get(store_id) == Some(themes)));
                    if !same {
                        let (store_id, themes) = (*store_id, themes.clone());
                        store.store_themes.update(|all| {
                            all.insert(store_id, themes);
                        });
                    }
                }
            }

            BackendEvent::ThemeMade { store_id, node_id } => {
                crate::app::open_node(store, tree_state, format!("node_{store_id}_{node_id}"));
            }

            BackendEvent::NodeNotLoaded { store_id, node_id, message } => {
                crate::metanodes::parent_not_loaded(*store_id, *node_id);
                connection::error(store, message);
            }

            BackendEvent::Shown { view } => {
                tracing::debug!("Shown in {:?}: {} rows, {} lists, {} gone", view.store, view.nodes.len(), view.lists.len(), view.gone.len());
                apply_view(store, view);
                // The metanodes the panes show that it may have touched.
                crate::metanodes::view_shown(store, view);
            }

            BackendEvent::MetanodeRead { store_id, node_id, forest, lives } => {
                crate::metanodes::forest_read(store, *store_id, *node_id, forest.clone(), lives);
            }

            BackendEvent::PluginReply { instance, call_id, reply } => {
                crate::plugins::reply(*instance, *call_id, reply.clone());
            }

            BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id } => {
                nodes::node_moved(store, tree_state, store_id, node_id, old_parent_id, new_parent_id);
            }

            BackendEvent::NodeCreated { store_id, parent_id, node_id, request } => {
                nodes::node_created(store, tree_state, store_id, parent_id, node_id, request);
            }

            BackendEvent::AttachmentCreated { store_id, host_id, node_id, request } => {
                tracing::info!("Attachment created: {:?}/{:?} on {:?}", store_id, node_id, host_id);
                match request {
                    Some(request) => crate::discussions::created(store, *request, *store_id, *node_id, Some(*host_id)),
                    None => store.send(BackendCommand::ListAttachments { store_id: *store_id, node_id: *host_id }),
                }
            }

            BackendEvent::AttachmentsListed { store_id, node_id, nodes } => {
                crate::discussions::attachments_listed(store, *store_id, *node_id, nodes.clone());
            }

            BackendEvent::PersonalSynced { state, off } => {
                crate::personal::synced(store, state, *off);
            }

            BackendEvent::DiscussionsListed { store_id, nodes } => {
                crate::discussions::discussions_listed(store, *store_id, nodes.clone());
            }

            BackendEvent::NodeContentUpdated { store_id, node_id } => {
                // An answered write (`ApplyUpdate`), or an encrypted store's
                // vault saying a document changed: what it did to the node's
                // row arrives as `Shown`.
                tracing::debug!("Node content updated: {:?}/{:?}", store_id, node_id);
            }

            BackendEvent::NodeTransplanted { from_store_id, old_node_id, old_parent_id: _, to_store_id, node_id, new_parent_id, title, left_shares } => {
                let transplanted = nodes::Transplanted { from_store_id, old_node_id, to_store_id, node_id, new_parent_id, title, left_shares };
                nodes::node_transplanted(store, tree_state, transplanted)
            }
            BackendEvent::LinkResolved { url, hops, resolution } => {
                if crate::text_boards::link_resolved(store, url, *hops, resolution) {
                    // A board embedded in text, not a link followed.
                } else if crate::sheets::reads::link_resolved(store, url, resolution) {
                    // A note a sheet's formulas read, not a link followed.
                } else if !restore_resolved(store, url, resolution) {
                    follow_resolution(store, url, *hops, resolution);
                }
            }
            BackendEvent::AncestorsLoaded { store_id, node_id, ancestors } => {
                reveal_in_tree(store, tree_state, *store_id, *node_id, ancestors);
            }
            BackendEvent::PropertiesLoaded { store_id, node_id, lives_in, store_tags } => {
                store.properties_context.set(Some(crate::properties::Context {
                    store_id: *store_id,
                    node_id: *node_id,
                    lives_in: lives_in.clone(),
                    store_tags: store_tags.clone(),
                }));
            }
            BackendEvent::ReferencesLoaded { store_id, node_id, references } => {
                crate::properties::references_loaded(store, *store_id, *node_id, references.clone());
            }
            BackendEvent::DeletedListed { store_id, nodes } => nodes::deleted_listed(store, store_id, nodes),
            // The new note opens where the split was made, as Scrivener's does.
            BackendEvent::NodeSplit { store_id, new_id, .. } => {
                store.send(BackendCommand::ResolveLink { url: pimble_core::PimbleUrl::node(*store_id, *new_id), hops: 0 });
            }
            BackendEvent::NodeDeleted { store_id, node_id, parent_id } => {
                // The list it left and the subtree that went with it were
                // shown just before this answer (`Shown`).
                tracing::info!("Node deleted: {:?}/{:?} from {:?}", store_id, node_id, parent_id);
            }

            BackendEvent::StoreClosed { store_id } => stores::store_closed(store, store_id),

            BackendEvent::MountCreated { store_id, parent_id, node_id, mount_ref } => {
                stores::mount_created(store, store_id, parent_id, node_id, mount_ref);
            }

            BackendEvent::MountStateChanged { store_id, node_id, state, mount_ref } => {
                stores::mount_state_changed(store, store_id, node_id, state, mount_ref);
            }

            BackendEvent::RemoteStoreChange { store_id, change_kind, source_client_id } => {
                nodes::store_changed(store, tree_state, store_id, change_kind, source_client_id);
            }

            BackendEvent::NodeContentReconciled { store_id, node_id, diff, server_state_vector } => {
                crate::doc_session::reconciled(store, *store_id, *node_id, diff, server_state_vector);
            }

            BackendEvent::RemoteChanges { store_id, node_id, update } => {
                // An update this window's views did not make: every view
                // holding the node merges it (re-projecting, never sending
                // it on).
                crate::doc_session::received(store, *store_id, *node_id, update);
            }

            BackendEvent::Resynced { store_id } => {
                // Nothing of the store was heard for a while: every view of
                // one of its nodes reconciles. What the catch-up changed in
                // the tree follows as `Shown`.
                crate::doc_session::resynced(store, *store_id);
                // Anything may have changed while it was away, themes too.
                crate::themes::relist(store, *store_id);
            }

            BackendEvent::BlobStored { request_id, result } => {
                // An image node's picture, or a picture for a note's text.
                if !crate::boards::stored(store, *request_id, result) {
                    crate::pictures::stored(store, *request_id, result);
                }
            }

            BackendEvent::BlobLoaded { url, result } => {
                crate::pictures::loaded(*url, result);
                crate::boards::blob_loaded(url, result);
                // A canvas in text asks for its picture itself
                // (`text_boards::held_picture`), not through the canvas view's
                // asking, so `blob_loaded` does not tell it: told here, it draws
                // again with the bytes it was waiting for.
                if result.is_ok() {
                    crate::text_boards::picture_arrived();
                }
            }

            BackendEvent::SearchResults { purpose: crate::protocol::SearchPurpose::MetanodeSetup(pane), results } => {
                crate::metanode_setup::search_answered(store, *pane, results);
            }

            BackendEvent::SearchResults { purpose: crate::protocol::SearchPurpose::LinkPicker, results } => {
                crate::link_picker::search_answered(store, results);
            }

            BackendEvent::SearchResults { purpose: crate::protocol::SearchPurpose::Panel, results } => {
                match results {
                    Ok(items) => store.search_results.set(SearchState::Results(items.clone())),
                    Err(message) => store.search_results.set(SearchState::Error(message.clone())),
                }
            }

            BackendEvent::IndexRebuilt { store_id, indexed } => {
                tracing::info!("Search index rebuilt for store {:?}: {} node(s) indexed", store_id, indexed);
            }

            BackendEvent::RemoteStoresListed { url, result } => sync::remote_stores_listed(store, url, result),

            BackendEvent::StoreSyncChanged { store_id, remote, state, sync_mode, access, read_only_roots, ended_roots, relay, owner_offline, waiting, out_of_step } => {
                let changed = sync::SyncChanged { store_id, remote, state, sync_mode, access, read_only_roots, ended_roots, relay, owner_offline, waiting, out_of_step };
                sync::store_sync_changed(store, tree_state, changed)
            }

            BackendEvent::HostedStoreCreated { name, store_id } => stores::hosted_store_created(store, name, store_id),

            BackendEvent::Imported { store_id, parent_id, node_id, title, count } => {
                stores::imported(store, tree_state, store_id, parent_id, node_id, title, count);
            }

            BackendEvent::ImportFailed { message } => {
                tracing::warn!("Import failed: {message}");
                show_notice(store, message.clone());
            }

            BackendEvent::Notice { message } => {
                tracing::info!("Notice: {message}");
                show_notice(store, message.clone());
            }

            BackendEvent::ReplicaRemoved { store_id } => stores::replica_removed(store, store_id),

            // Pimble Cloud account (docs/DESKTOP_ACCOUNT_CONTRACT.md "Events").
            BackendEvent::AgentsListed { agents } => crate::agents::listed(store, agents),
            BackendEvent::AgentCreated { agent, key } => crate::agents::created(store, agent, key),
            BackendEvent::AgentsError { message } => crate::agents::failed(store, message),
            BackendEvent::DiagnosticsSent { result } => {
                store.diagnostics_sending.set(false);
                if result.is_ok() {
                    store.diagnostics_note.set(String::new());
                }
                store.diagnostics_sent.set(Some(result.clone()));
            }

            BackendEvent::ServerInfo { version } => {
                store.server_version.set(Some(match version {
                    Some(v) => crate::state::ServerVersion::Known(v.clone()),
                    None => crate::state::ServerVersion::Unknown,
                }));
            }
            BackendEvent::CloudConnectionChanged { connection } => {
                store.cloud_connection.set(connection.clone());
            }
            BackendEvent::NoticeReceived { notice } => {
                crate::inbox::received(store, notice.clone());
            }
            BackendEvent::NotesNamed { names } => {
                crate::inbox::named(store, names.clone());
            }
            BackendEvent::NoticesListed { notices, on, email, push, push_key, watches } => {
                crate::inbox::listed(store, notices.clone(), *on);
                crate::inbox::channels(store, *email, *push, push_key.clone());
                store.notify_watches.set(*watches);
            }
            BackendEvent::WatchesListed { watches, on } => {
                crate::watches::listed(store, watches.clone(), *on);
            }
            BackendEvent::AppLink { url, activation_token, ack } => {
                crate::app_links::handed(store, url, activation_token.as_deref(), *ack);
            }
            BackendEvent::CloudStatusChanged { signed_in, email, user_id, url } => {
                cloud::status_changed(store, signed_in, email, user_id, url);
            }

            BackendEvent::CloudError { op, message } => cloud::error(store, op, message),

            BackendEvent::PresenceFrame { frame } => crate::presence::received(store, frame),
            BackendEvent::CloudSharesListed { shares } => crate::shares::listed(store, shares),
            BackendEvent::PeopleLoaded { store_id, people } => crate::shares::people_loaded(store, *store_id, people),
            BackendEvent::CloudHostedStoresListed { stores, relayed } => cloud::hosted_stores_listed(store, stores, relayed),

            BackendEvent::CloudStoreHosted { store_id } => cloud::store_hosted(store, store_id),

            BackendEvent::CloudStoreRelayed { store_id } => cloud::store_relayed(store, store_id),

            BackendEvent::CloudRelayingStopped { store_id } => cloud::relaying_stopped(store, store_id),

            // The answer to every sharing request but "Stop sharing"
            // (docs/NODE_DOCUMENT_CONTRACT.md section 5): the share as the accounts
            // service has it now, and everyone on it.
            BackendEvent::CloudShareUpdated { store_id, node_id, share, members } => {
                cloud::share_updated(store, store_id, node_id, share, members);
            }

            BackendEvent::CloudSharingStopped { store_id, node_id } => cloud::sharing_stopped(store, store_id, node_id),
        }
    }
    crate::startup::check(store);
}

#[cfg(test)]
mod tests;
