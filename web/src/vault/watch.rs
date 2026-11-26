//! Listening through the API: watchers of a store and what they are told.

use pimble_app::protocol::BackendEvent;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeUpdateEffect;
use pimble_rpc::StoreChangeKind;

use super::{heard_kind, VaultClient};

impl VaultClient {
    // ── Listening through the API ───────────────────────────────────────────

    /// Hear `store_id`'s changes from now on, each with the update of the
    /// document it is about: what a plugin's `Pimble` keeps its copies current
    /// from (docs/PLUGINS_CONTRACT.md wave 3). Ends when the page lets go of
    /// the store.
    pub fn watch(&mut self, store_id: StoreId) -> futures::channel::mpsc::UnboundedReceiver<pimble_api::StoreChange> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        self.watchers.entry(store_id).or_default().push(tx);
        rx
    }

    /// Note a write this page made of one document (`update`, merged here
    /// already): heard by the API's listeners, and by the app's views when a
    /// plugin made it.
    pub(super) fn wrote(&mut self, store_id: StoreId, node_id: NodeId, effect: NodeUpdateEffect, update: &[u8]) {
        let source = Some(self.writer.clone().unwrap_or_else(|| self.client_id.clone()));
        let change = pimble_api::StoreChange { store: store_id, kind: heard_kind(node_id, effect), source, update: Some(update.to_vec()) };
        if self.writer.is_some() {
            self.by_plugins.push(change.clone());
        }
        if let Some(store) = self.stores.get_mut(&store_id) {
            store.heard.push(change);
        }
    }

    /// Hand every listener what its store heard since the last time. Called
    /// when a call through the API lets go of the vault and on every pass of
    /// the backend loop.
    pub fn tell_watchers(&mut self) {
        for (store_id, store) in &mut self.stores {
            if store.heard.is_empty() {
                continue;
            }
            let heard = std::mem::take(&mut store.heard);
            let Some(watchers) = self.watchers.get_mut(store_id) else { continue };
            watchers.retain(|tx| heard.iter().all(|change| tx.unbounded_send(change.clone()).is_ok()));
        }
        // A store let go of: its listeners hear the end, and subscribe again
        // when they read it next.
        let stores = &self.stores;
        self.watchers.retain(|store_id, _| stores.contains_key(store_id));
    }

    /// What plugins changed, as the app's views hear a peer's change of an
    /// encrypted store (`VaultStore::merge`): the kinds, which the backend
    /// loop shows through the API, and a content update for the editors
    /// holding the document.
    pub(super) fn plugin_events(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        for change in std::mem::take(&mut self.by_plugins) {
            match change.kind {
                StoreChangeKind::ContentUpdated { node_id } => {
                    if let Some(update) = change.update {
                        events.push(BackendEvent::RemoteChanges { store_id: change.store, node_id, update });
                    }
                    events.push(BackendEvent::NodeContentUpdated { store_id: change.store, node_id });
                }
                kind => events.push(BackendEvent::RemoteStoreChange { store_id: change.store, change_kind: kind, source_client_id: change.source }),
            }
        }
        events
    }
}
