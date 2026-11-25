//! What the API hears, as the UI's events: a store's change, and what a
//! change or this window's own structural call did to the tree.

use pimble_api::{Change, Pimble};
use pimble_core::{NodeId, StoreId};

use super::Context;
use crate::protocol::BackendEvent;

/// What one change the API heard is for the UI, in order: the update for the
/// views holding the node (unless this window's own views made it, which
/// hand it to each other themselves), the change itself (what is not a
/// document's: a link's state, a share that ended, an attachment), and what
/// it did to the rows and lists of the tree, read from the documents the API
/// holds now that it merged the change (`BackendEvent::Shown`; nothing is
/// fetched again). `last` is the update handed over last: the server
/// announces one update once per kind of change it made (a content edit that
/// also renamed), and a view takes it once.
pub async fn change_events(api: &Pimble, change: Change, client_id: &str, last: &mut Option<(StoreId, NodeId, Vec<u8>)>) -> Vec<BackendEvent> {
    match change {
        Change::Resynced { store, changed } => {
            *last = None;
            let mut events = vec![BackendEvent::Resynced { store_id: store }];
            events.extend(shown(api, store, &pimble_api::Affected::documents(changed)).await);
            // The app lists the themes again if it listed them
            // (`events::themes_resynced`): what is held may not be whole now.
            events
        }
        Change::Rejudged { store, nodes } => shown(api, store, &pimble_api::Affected::rows(nodes)).await.into_iter().collect(),
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
            let affected = pimble_api::Affected::of(&c.kind);
            let document = pimble_api::backend::document_of(&c.kind);
            events.push(BackendEvent::RemoteStoreChange { store_id: c.store, change_kind: c.kind, source_client_id: c.source });
            events.extend(shown(api, c.store, &affected).await);
            // A theme node changed (its tokens, its name, made, deleted):
            // the store's list of themes, read again from what is held, when
            // they were listed (and so the store is held whole).
            if let (Some(node), true) = (document, api.themes_at_hand(c.store)) {
                if api.is_theme(pimble_api::Addr::new(c.store, node)).await {
                    events.extend(super::themes::listed(api, c.store).await);
                }
            }
            events
        }
    }
}

/// What `affected` is in `store` as the API holds it now, as the event the
/// tree takes it as; nothing when it touches nothing held.
pub async fn shown(api: &Pimble, store: StoreId, affected: &pimble_api::Affected) -> Option<BackendEvent> {
    if affected.is_empty() {
        return None;
    }
    let view = api.shown(store, affected).await;
    (!view.is_empty()).then_some(BackendEvent::Shown { view })
}

/// [`shown`] after a structural call this window made, emitted before its
/// answer: the documents it touched are brought up to date first, so the
/// tree shows what the call did without waiting for the store's
/// subscription to say it (which then says the same again).
pub(super) async fn show_after(ctx: &Context<'_>, store: StoreId, affected: pimble_api::Affected) {
    let view = ctx.api.shown_after(store, &affected).await;
    if !view.is_empty() {
        ctx.emit(BackendEvent::Shown { view });
    }
}
