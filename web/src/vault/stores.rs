//! Which stores this page holds, as the account lists them: access, rows, opening.

use std::collections::HashMap;
use std::sync::Arc;

use pimble_app::protocol::BackendEvent;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, RelaySide, Store, StoreAccess, StoreId, StoreKind};
use pimble_rpc::{StoreChangeKind, VaultDocId};

use crate::keys::{self, KeyError};

use super::{shares_ended, AccountGrant, AccountStore, StoreKeys, VaultClient, VaultStore, Waiting, KEY_RETRY_MS, WAITING_SUFFIX};
use super::helpers::{fetch_many, notice, now_ms, now_rfc3339};
use super::shares::fetch_share_keys;

impl VaultClient {
    /// Whether this store is one the vault client answers for.
    pub fn owns(&self, store_id: StoreId) -> bool {
        self.stores.contains_key(&store_id)
    }

    /// Whether a store's changes are this client's to subscribe to: an
    /// encrypted store, whether it is open here or only known to be encrypted
    /// from the account's store list. A relayed store is always one.
    pub fn is_encrypted(&self, store_id: StoreId) -> bool {
        self.owns(store_id) || self.is_relayed(store_id) || self.rows.get(&store_id).is_some_and(|row| row.kind == "vault")
    }

    /// The stores the freshly minted session says are served from their
    /// owners' computers, each at an endpoint of its own.
    pub fn set_relayed(&mut self, store_ids: Vec<StoreId>) {
        self.relayed = store_ids.into_iter().collect();
        // A store the session no longer names is not waited for.
        let relayed = &self.relayed;
        self.owner_offline.retain(|id| relayed.contains(id));
    }

    /// Whether a store is served from its owner's computer through Pimble
    /// Cloud's relay (docs/RELAY_CONTRACT.md), as the session or the
    /// account's store list says.
    pub fn is_relayed(&self, store_id: StoreId) -> bool {
        self.relayed.contains(&store_id) || self.rows.get(&store_id).is_some_and(|row| row.relayed)
    }

    /// What this account may change in a store, from the grants it holds;
    /// nothing while the computer it is served from is off.
    pub fn access(&self, store_id: StoreId) -> StoreAccess {
        if self.owner_offline.contains(&store_id) {
            return StoreAccess::Read;
        }
        self.rows.get(&store_id).map(AccountStore::access).unwrap_or(StoreAccess::Full)
    }

    /// The roots of a store this account only reads while it edits others
    /// (`Store::read_only_roots`); none while [`VaultClient::access`] answers
    /// for the whole store.
    pub(super) fn read_only_roots(&self, store_id: StoreId) -> Vec<NodeId> {
        if self.owner_offline.contains(&store_id) {
            return Vec::new();
        }
        self.rows.get(&store_id).map(AccountStore::read_only_roots).unwrap_or_default()
    }

    /// Read the account's store list: which stores are encrypted, and every
    /// grant this account holds on each.
    ///
    /// Asked separately from the RPC store list because only the accounts
    /// service knows a share's own name, who shared it and which roots the
    /// account holds — the hosted server answers with the *owner's* store
    /// name, which a recipient has no business learning. One row per grant, so
    /// the rows of one store are collected here.
    ///
    /// Answers what the UI should hear about shares that ended since the
    /// list was last read ([`VaultClient::take_rows`]). A list that cannot be
    /// read changes nothing and ends nothing.
    pub async fn learn_rows(&mut self) -> Vec<BackendEvent> {
        let list = match crate::accounts::list_stores().await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!("Could not read the account's store list: {}", e);
                return Vec::new();
            }
        };

        let mut rows: HashMap<StoreId, AccountStore> = HashMap::new();
        for view in list {
            let Ok(store_id) = StoreId::parse(&view.store_id) else {
                tracing::warn!("The account's store list named {}, which is not a store id", view.store_id);
                continue;
            };
            // A row whose `root` will not parse is a scoped grant this page
            // cannot address; dropping the row is safer than reading it as a
            // whole-store grant, which is what `None` would mean.
            let root = match view.root.as_deref() {
                Some(raw) => match NodeId::parse(raw) {
                    Ok(root) => Some(root),
                    Err(_) => {
                        tracing::warn!("A grant on {} named the root {}, which is not a node id", store_id, raw);
                        continue;
                    }
                },
                None => None,
            };
            let row = rows.entry(store_id).or_default();
            row.kind = view.kind;
            row.relayed |= view.tier == "relay";
            if row.shared_by.is_none() {
                row.shared_by = view.shared_by;
            }
            row.grants.push(AccountGrant { root, role: view.role, name: view.name });
        }
        self.take_rows(rows)
    }

    /// Take the account's store list as just read, and let go of every share
    /// it no longer names (Joe, 2026-09-21: the folder leaves the explorer
    /// with a notice). This page holds no replica, so there is nothing to
    /// keep: the documents under an ended root are dropped from memory, and
    /// a store none of whose shares is left is closed. The UI is told which
    /// roots ended (`StoreChangeKind::SharesEnded`, which it answers with the
    /// notice and by dropping them) and, for a store with nothing left, that
    /// it is closed, before the list that no longer names them reaches it.
    pub(super) fn take_rows(&mut self, rows: HashMap<StoreId, AccountStore>) -> Vec<BackendEvent> {
        let ended = shares_ended(&self.rows, &rows);
        self.ended.retain(|store_id| !rows.contains_key(store_id));
        self.rows = rows;
        let mut events = Vec::new();
        for share in ended {
            tracing::info!("Store {}: {} share(s) of it ended{}", share.store_id, share.roots.len(), if share.every { ", the last of them" } else { "" });
            events.push(BackendEvent::RemoteStoreChange {
                store_id: share.store_id,
                change_kind: StoreChangeKind::SharesEnded { node_ids: share.roots.clone() },
                source_client_id: None,
            });
            // A store left with no scope root would read as a whole store,
            // so one that would be is closed here and opened again from
            // the list as it is now.
            let still_open = !share.every && self.stores.get_mut(&share.store_id).is_none_or(|open| open.drop_roots(&share.roots));
            if !still_open {
                self.stores.remove(&share.store_id);
            }
            if share.every {
                self.waiting.remove(&share.store_id);
                self.subscribed.remove(&share.store_id);
                self.owner_offline.remove(&share.store_id);
                self.ended.insert(share.store_id);
                events.push(BackendEvent::StoreClosed { store_id: share.store_id });
            }
        }
        events
    }

    /// Fill in what this page knows about a store before the UI sees it: the
    /// root its documents name, and what the account's grants say about it.
    ///
    /// The root matters. A hosted store's manifest carries the root the
    /// server minted when the store was created, which is the real one only
    /// when this browser also made the tree. For a store hosted from a
    /// desktop the tree was made elsewhere and names a different root, and
    /// the hosted server, holding only ciphertext, never learns it. The
    /// documents are the authority and this is where they replace the
    /// placeholder. A store whose documents are not open here keeps the
    /// server's root: there is nothing better to say until they are.
    ///
    /// For a share the row decides the rest: the name (the share's own, since
    /// the server's is the owner's store's), who shared it, the roots this
    /// account holds and whether any of them may be written to.
    ///
    /// A store served from its owner's computer (docs/RELAY_CONTRACT.md) says
    /// so (`Store::relay`), and has no name anywhere but in its own documents:
    /// Pimble Cloud holds none for it and its twin is made without one. An
    /// account that holds the whole of it reads the root's title once that is
    /// open here, and until then it is called what the desktop calls it.
    /// While that computer is off nothing of the store may be changed from
    /// this page ([`VaultClient::access`]).
    pub fn describe(&self, store: &mut Store) {
        if let Some(row) = self.rows.get(&store.id) {
            store.access = row.access();
            store.read_only_roots = row.read_only_roots();
            store.shared_by = row.shared_by.clone();
            let roots = row.roots();
            if !roots.is_empty() {
                store.root_node_id = roots[0];
                store.roots = roots;
            }
            if let Some(name) = row.name() {
                store.name = name;
            }
        }
        if self.is_relayed(store.id) {
            store.relay = RelaySide::Member;
        }
        if self.owner_offline.contains(&store.id) {
            store.access = StoreAccess::Read;
            store.read_only_roots.clear();
        }
        if store.relay == RelaySide::Member && store.name.trim().is_empty() {
            store.name = self
                .stores
                .get(&store.id)
                .filter(|open| !open.is_partial())
                .and_then(|open| open.tree.get_node_info(open.tree.root()).ok())
                .map(|root| root.title)
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| pimble_app::state::RELAYED_PLACEHOLDER_NAME.to_string());
        }
        if let Some(open) = self.stores.get(&store.id) {
            if open.scope_roots.is_empty() {
                store.root_node_id = open.tree.root();
            }
        } else if self.waiting.contains_key(&store.id) && !store.name.ends_with(WAITING_SUFFIX) {
            // The grant is real; only the key is late. Listing the row says so
            // rather than leaving a store that silently never opens.
            store.name.push_str(WAITING_SUFFIX);
        }
    }

    /// [`describe`](Self::describe) over a whole list.
    pub fn describe_all(&self, stores: &mut [Store]) {
        for store in stores.iter_mut() {
            self.describe(store);
        }
    }

    /// Open every vault store in `stores` that is not open yet.
    ///
    /// Called before `StoresListed` reaches the UI, so the tree's first
    /// `GetChildren` already has documents to read. Answers what the UI should
    /// hear about the ones that did not open: an error for a real failure, and
    /// for a share still waiting on its key a sentence saying so, once per
    /// store per session.
    pub async fn open_listed(
        &mut self,
        client: &Arc<PimbleClient>,
        stores: &[Store],
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        for store in stores {
            // Either source saying "vault" is enough: the accounts service
            // knows a store's kind before any grant of it reaches a token, and
            // the RPC `Store` knows it for one the account list has not been
            // re-read for yet.
            if store.kind == StoreKind::Vault {
                self.rows.entry(store.id).or_default().kind = "vault".to_string();
            }
            if !self.is_encrypted(store.id) || self.stores.contains_key(&store.id) {
                continue;
            }
            match self.open_store(client, store, signal_ui).await {
                Ok(()) => {}
                Err(KeyError::NoneYet) => events.extend(self.now_waiting(store)),
                Err(KeyError::Failed(message)) => {
                    tracing::error!("Opening the encrypted store {}: {}", store.name, message);
                    events.push(BackendEvent::Error {
                        message: format!("Could not open an encrypted store ({}: {})", store.name, message),
                    });
                }
            }
        }
        events
    }

    /// Record that a store is waiting for its key, and say so the first time.
    ///
    /// A grant with no envelope yet is the ordinary state of a fresh share:
    /// the owner's Pimble has not been online since to hand the key over. It
    /// is a sentence for the person, not an error, so it goes through the
    /// notice channel (see [`notice`]).
    fn now_waiting(&mut self, listed: &Store) -> Option<BackendEvent> {
        let store_id = listed.id;
        self.waiting.insert(store_id, Waiting { listed: listed.clone(), due: now_ms() + KEY_RETRY_MS });
        if !self.announced.insert(store_id) {
            return None;
        }
        let who = self
            .rows
            .get(&store_id)
            .and_then(|row| row.shared_by.clone())
            .unwrap_or_else(|| "the owner".to_string());
        Some(notice(format!(
            "Waiting for {who}'s Pimble to come online to finish sharing"
        )))
    }

    /// The stores with keys worth asking for again: one still waiting for its
    /// own (see [`KEY_RETRY_MS`]), and an open one whose look for a share's
    /// key or a document's wraps has come due (see [`KeyLook`]).
    pub fn key_retries_due(&self) -> Vec<StoreId> {
        let now = now_ms();
        let waiting = self.waiting.iter().filter(|(_, waiting)| waiting.due <= now).map(|(id, _)| *id);
        let looking = self.stores.iter().filter(|(_, store)| store.look.is_due(now)).map(|(id, _)| *id);
        waiting.chain(looking).collect()
    }

    /// Ask again for a waiting store's key and open it if it has arrived; for
    /// a store that is open, look for what it is still owed
    /// ([`VaultClient::look_for_keys`]).
    ///
    /// The store is announced to the UI when it opens, since `StoresListed`
    /// has long since been and gone — and it is announced without the
    /// "waiting" suffix its name carried, which is what makes the row settle.
    pub async fn retry_key(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Vec<BackendEvent> {
        if self.stores.contains_key(&store_id) {
            return self.look_for_keys(client, store_id).await;
        }
        let Some(listed) = self.waiting.get(&store_id).map(|w| w.listed.clone()) else {
            return Vec::new();
        };
        match self.open_store(client, &listed, signal_ui).await {
            Ok(()) => {
                let mut store = listed;
                self.describe(&mut store);
                tracing::info!("Store {}: the key arrived; it is open", store_id);
                vec![BackendEvent::StoreOpened { store }]
            }
            Err(e) => {
                // Still worth retrying either way: a request that failed may
                // not fail next time, and a key that has not arrived may.
                if let Some(waiting) = self.waiting.get_mut(&store_id) {
                    waiting.due = now_ms() + KEY_RETRY_MS;
                }
                if let KeyError::Failed(message) = e {
                    tracing::warn!("Asking again for the key of {} failed: {}", store_id, message);
                }
                Vec::new()
            }
        }
    }

    /// Fetch this store's keys and every document's log, decrypt, build the
    /// tree, and hold it.
    ///
    /// Every document is pulled at open, not lazily: titles and children
    /// lists live in the documents, so nothing of the tree can be drawn until
    /// they are here. That costs one `vaultFetch` per node (in windows of
    /// [`FETCH_WINDOW`]); a store of thousands of nodes wants a fetch that
    /// answers for many documents at once, which the vault RPCs do not offer
    /// yet.
    ///
    /// The subscription is made first, so an append that lands during the
    /// pull is queued for `pump` rather than missed until the next connect;
    /// one the pull already applied merges again to nothing.
    async fn open_store(
        &mut self,
        client: &Arc<PimbleClient>,
        store: &Store,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), KeyError> {
        let store_id = store.id;
        // The roots to fetch keys for are the account's grants: a share's key
        // lives under its own root, the store key under none.
        let scope_roots = self.rows.get(&store_id).map(AccountStore::roots).unwrap_or_default();
        let keyring = keys::fetch_scope_keyring(&store_id.to_string(), &scope_roots).await?;

        if let Some(BackendEvent::Error { message }) = self.subscribe(client, store_id, signal_ui).await {
            return Err(KeyError::Failed(message));
        }

        let listing = client
            .vault_list_docs_response(store_id)
            .await
            .map_err(|e| KeyError::Failed(format!("listing the vault's documents failed: {e}")))?;
        // Which logs these are: everything read below is numbered in them.
        let epoch = listing.epoch;
        let mut keys = StoreKeys::new(keyring);
        let mut wanted: Vec<(NodeId, u64)> = Vec::new();
        for info in listing.docs {
            match info.doc_id {
                VaultDocId::Node(id) => {
                    if let Some(dek_id) = info.dek_id {
                        keys.docs.note_listed(id, dek_id);
                    }
                    if info.head > 0 {
                        wanted.push((id, 0));
                    }
                }
                // A hosted twin from before this layout keeps its tree
                // document, superseded by the node documents.
                VaultDocId::Tree => tracing::debug!("Store {} lists a tree document; skipping it", store_id),
                // Pictures are not read in the browser yet (docs/IMAGES_CONTRACT.md
                // wave 4), and are never listed unless asked for.
                VaultDocId::Blob(_) => {}
            }
        }

        let mut fetched_docs = Vec::with_capacity(wanted.len());
        for (node_id, fetched) in fetch_many(client, store_id, wanted).await {
            match fetched {
                Ok(fetched) => fetched_docs.push((node_id, fetched)),
                // Left unheld: the next catch-up sees it listed and fetches
                // it from the start.
                Err(message) => tracing::warn!("Fetching the document {} failed: {}", node_id, message),
            }
        }

        let (mut vault_store, shut) = VaultStore::from_fetched(store.clone(), keys, scope_roots, fetched_docs);
        vault_store.epoch = epoch;
        vault_store.parts_ok = listing.parts;
        // The shares in a whole store: what the first pass opened says which
        // nodes are shared, and their keys open what members made that no
        // desktop of the owner's has wrapped under the store key yet. Before
        // the repair, so it judges the tree with those documents in it.
        if fetch_share_keys(store_id, &mut vault_store).await {
            let now = now_ms();
            for (node_id, fetched) in &shut {
                vault_store.take_fetched(store_id, *node_id, fetched, now);
            }
        }
        // Once, after the whole pull, never between the updates of one edit.
        let repair = vault_store.repair_now(&now_rfc3339());
        self.stores.insert(store_id, vault_store);
        self.waiting.remove(&store_id);
        if let Some(edit) = repair {
            if let Err(message) = self.append_edit(client, store_id, &edit).await {
                tracing::warn!("Appending the repair of {} failed: {}", store_id, message);
            }
        }
        self.compare_reports(client, store_id).await;
        Ok(())
    }
}
