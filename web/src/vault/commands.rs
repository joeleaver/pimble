//! Commands from the UI, and a store whose owner's computer is off.

use std::sync::Arc;

use pimble_app::protocol::{BackendCommand, BackendEvent};
use pimble_client::PimbleClient;
use pimble_core::{NodeId, RelaySide, Store, StoreAccess, StoreId, StoreKind};

use super::{store_id_of, AccountStore, Handled, VaultClient, VaultStore};
use super::helpers::{name_of, notice, offline_root};

impl VaultClient {
    // ── Commands ────────────────────────────────────────────────────────────

    /// Answer what an encrypted store answers itself and the API does not:
    /// its subscription (this page pumps what it delivers, decrypted, as
    /// events: the vault is the one holder of the store's documents, so it
    /// is the one that says what changed in them), presence's (riding that
    /// subscription), its status and closing it. Any other command comes back
    /// untouched: a read or a write is the API's (`crate::vault_backend`
    /// answers it through the router), and anything else is refused by
    /// [`VaultClient::refuse`].
    pub async fn handle(
        &mut self,
        client: &Arc<PimbleClient>,
        cmd: BackendCommand,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Handled {
        let Some(store_id) = store_id_of(&cmd) else { return Handled::No(cmd) };
        if !self.owns(store_id) {
            return Handled::No(cmd);
        }
        Handled::Yes(match cmd {
            BackendCommand::SubscribeStoreChanges { store_id } => self.subscribe(client, store_id, signal_ui).await,
            // Presence rides the store's own subscription (`subscribe`), made
            // again on every new socket; a pane's frame goes out sealed
            // (`publish_presence`).
            BackendCommand::SubscribePresence { .. } => None,
            // Not a sync link (a vault store in the browser has none, and
            // its capabilities say so): the store's status, which the app
            // reads its access, its relay side and whether its owner is
            // away from. What the account may change is what the store was
            // described with (`describe`), from the same rows.
            BackendCommand::GetStoreSync { store_id } => Some(self.sync_event(store_id)),
            BackendCommand::CloseStore { store_id } => Some(self.close(store_id)),
            other => return Handled::No(other),
        })
    }

    /// Refuse what an encrypted store cannot do in the browser and the API
    /// did not answer (mounts, sync links, replicas): a sentence, never a
    /// plain RPC the server would refuse for such a store anyway.
    pub fn refuse(&self, cmd: BackendCommand) -> Handled {
        match store_id_of(&cmd) {
            Some(store_id) if self.owns(store_id) => Handled::Yes(Some(BackendEvent::Error {
                message: format!("{} is not available on an encrypted store", name_of(&cmd)),
            })),
            _ => Handled::No(cmd),
        }
    }

    /// What the account says this page may change of a store: the gate the
    /// router keeps (`pimble_api::Router::set_access`). `None` for a store
    /// the account's list does not name, which is no reason to refuse
    /// anything.
    pub fn row_access(&self, store_id: StoreId) -> Option<StoreAccess> {
        self.rows.get(&store_id).map(AccountStore::access)
    }

    /// Whether the store's owner's computer is off, as the backend loop last
    /// said ([`VaultClient::endpoint_down`]), and the row it is listed as
    /// meanwhile (`pimble_api::Router::set_away`).
    pub fn away_row(&self, store_id: StoreId) -> Option<Store> {
        if !self.owner_offline.contains(&store_id) {
            return None;
        }
        Some(match self.stores.get(&store_id) {
            Some(store) => {
                let mut row = store.view();
                self.describe(&mut row);
                row
            }
            None => self.offline_row(store_id),
        })
    }

    // ── A store whose owner's computer is off ───────────────────────────────

    /// The backend loop could not reach the endpoint that serves these stores
    /// (docs/RELAY_CONTRACT.md): the computer they are shared from is off, or
    /// offline. Pimble Cloud itself has just answered this page, so that is
    /// what it means, and it is an ordinary state, not an error.
    ///
    /// A store this page never opened is listed all the same, from what the
    /// account's own list says of it: the share's name and who shared it. It
    /// opens empty and to be read, and fills in when the endpoint is back
    /// ([`VaultClient::endpoint_up`]). Every one of them says `owner offline`
    /// on its row, and takes no change until then. Answers only what is news.
    pub fn endpoint_down(&mut self, store_ids: &[StoreId]) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        for &store_id in store_ids {
            // Nothing of it is this account's any more: its endpoint not
            // answering says nothing about its owner's computer.
            if self.ended.contains(&store_id) {
                continue;
            }
            if !self.owner_offline.insert(store_id) {
                continue;
            }
            tracing::info!("Store {}: its owner's computer is offline", store_id);
            if !self.owns(store_id) {
                events.push(BackendEvent::StoresListed { stores: vec![self.offline_row(store_id)] });
            }
            events.push(self.sync_event(store_id));
        }
        events
    }

    /// The endpoint that serves these stores is connected again, and what it
    /// lists has been opened (`listed`, already described). A store shown from
    /// the account's row while it was down is announced as the store it is, so
    /// the tree fetches what is in it with no reload; one this page held all
    /// along only has its row say so. Answers nothing for a store that was
    /// never down.
    pub fn endpoint_up(&mut self, store_ids: &[StoreId], listed: &[Store]) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        for &store_id in store_ids {
            if !self.owner_offline.remove(&store_id) {
                continue;
            }
            tracing::info!("Store {}: its owner's computer is back", store_id);
            // `listed` was described while this store still read as offline.
            if let Some(store) = listed.iter().find(|store| store.id == store_id) {
                let mut store = store.clone();
                self.describe(&mut store);
                events.push(BackendEvent::StoreOpened { store });
            }
            events.push(self.sync_event(store_id));
        }
        events
    }

    /// The row of a store this page has never opened and cannot reach: what
    /// the account's own list says of it, which for a share is the share's
    /// name and who shared it. No root of its own is known (the twin's is on
    /// the computer that is off), so the row stands alone, a placeholder under
    /// it that answers as an empty folder.
    fn offline_row(&self, store_id: StoreId) -> Store {
        let row = self.rows.get(&store_id);
        let name = row
            .and_then(|row| row.name().or_else(|| row.grants.iter().map(|g| g.name.trim()).find(|name| !name.is_empty()).map(str::to_string)))
            .unwrap_or_default();
        let mut store = Store::new_local(name, std::path::PathBuf::new());
        store.id = store_id;
        store.kind = StoreKind::Vault;
        store.root_node_id = offline_root(store_id);
        store.shared_by = row.and_then(|row| row.shared_by.clone());
        store.access = StoreAccess::Read;
        store.relay = RelaySide::Member;
        if store.name.trim().is_empty() {
            store.name = pimble_app::state::RELAYED_PLACEHOLDER_NAME.to_string();
        }
        store
    }

    /// What `GetStoreSync` is answered with for a store this client answers
    /// for. A vault store in the browser has no link, so there is no badge,
    /// except the one that says its owner's computer is off.
    fn sync_event(&self, store_id: StoreId) -> BackendEvent {
        BackendEvent::StoreSyncChanged {
            store_id,
            remote: None,
            state: pimble_core::SyncState::Offline,
            sync_mode: pimble_core::StoreKind::Plain,
            access: self.access(store_id),
            read_only_roots: self.read_only_roots(store_id),
            // This page holds no replica: a share that ended is dropped and
            // is no longer one of the store's roots at all (`take_rows`).
            ended_roots: Vec::new(),
            relay: if self.is_relayed(store_id) { RelaySide::Member } else { RelaySide::None },
            owner_offline: self.owner_offline.contains(&store_id),
            waiting: self.stores.get(&store_id).map(VaultStore::waiting_list).unwrap_or_default(),
            out_of_step: self.stores.get(&store_id).map(VaultStore::out_of_step_list).unwrap_or_default(),
        }
    }

    /// A store's sync answer again for every store whose held-back documents
    /// changed since the UI was last told: the row's badge and the waiting
    /// node's Properties panel follow it.
    pub(super) fn held_back_events(&mut self) -> Vec<BackendEvent> {
        let changed: Vec<StoreId> = self
            .stores
            .iter_mut()
            .filter_map(|(id, store)| std::mem::take(&mut store.held_changed).then_some(*id))
            .collect();
        changed.into_iter().map(|id| self.sync_event(id)).collect()
    }

    /// Answer `cmd` for a store whose endpoint is down because its owner's
    /// computer is, with no connection to answer through; any other command
    /// comes back untouched.
    ///
    /// What the API covers (reads, writes, edits, reconciles) is answered
    /// through the router, from what this page holds and with its refusals.
    /// What is left here is answered the same way: from what this page
    /// holds, with no error, because nothing has gone wrong. Subscriptions
    /// wait for the endpoint (the backend loop makes them again when it
    /// connects).
    pub fn handle_unreachable(&mut self, cmd: BackendCommand) -> Handled {
        let Some(_) = store_id_of(&cmd).filter(|id| self.owner_offline.contains(id)) else {
            return Handled::No(cmd);
        };
        Handled::Yes(match cmd {
            BackendCommand::SubscribeStoreChanges { .. } => None,
            // Nobody is there to hear it, and nobody to say so.
            BackendCommand::PublishPresence { .. } | BackendCommand::SubscribePresence { .. } => None,
            BackendCommand::GetStoreSync { store_id } => Some(self.sync_event(store_id)),
            BackendCommand::CloseStore { store_id } => Some(self.close(store_id)),
            other => return Handled::No(other),
        })
    }

    /// What anything else asked of a store whose owner is away answers,
    /// after the API: a sentence saying so.
    pub fn refuse_unreachable(&self, cmd: BackendCommand) -> Handled {
        match store_id_of(&cmd) {
            Some(store_id) if self.owner_offline.contains(&store_id) => Handled::Yes(Some(notice(pimble_api::router::OWNER_OFFLINE_REFUSAL.to_string()))),
            _ => Handled::No(cmd),
        }
    }

    /// Merge an edit the editor has already made into its document and mark
    /// the document unsent, with nothing to append it through: the next
    /// connection's catch-up sends everything the server lacks of it.
    pub(super) fn keep_unsent(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<(), String> {
        let Some(store) = self.stores.get_mut(&store_id) else {
            return Err("no such encrypted store".to_string());
        };
        let effect = store.tree.apply_update(node_id, update).map_err(|e| e.to_string())?;
        store.doc_entry(node_id).unsent = true;
        if effect.changed {
            self.wrote(store_id, node_id, effect, update);
        }
        Ok(())
    }
}
