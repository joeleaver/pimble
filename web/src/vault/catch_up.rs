//! After a reconnect: catching up on what each held document missed, and keys still owed.

use std::sync::Arc;

use pimble_app::protocol::BackendEvent;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_rpc::VaultDocId;

use super::{VaultClient, REPAIR_DEBOUNCE_MS};
use super::helpers::{fetch_many, now_ms};
use super::shares::fetch_share_keys;

impl VaultClient {
    // ── After a reconnect ───────────────────────────────────────────────────

    /// Bring every open store level with the server again, both ways.
    ///
    /// This client outlives its connection: the socket is replaced on every
    /// token refresh and after every drop, while the decrypted documents stay.
    /// Nothing else closes the gap in between. Appends the server took while
    /// the socket was down were never notified here, and an append of this
    /// client's that failed was, until 2026-09-17, simply lost, along with
    /// every later edit's chance of being shown anywhere else (each depends on
    /// the one before). Called on every connect, after the subscriptions are
    /// made again and before `open_listed`.
    ///
    /// `served` says which stores `client`'s endpoint serves: a store served
    /// from its owner's computer has an endpoint of its own, and is caught up
    /// when that one connects (docs/RELAY_CONTRACT.md).
    ///
    /// Such a store's twin is disposable, and one built again numbers its
    /// logs from 1 under a new epoch. Every cursor this page kept would skip
    /// what those logs hold, and what it believed the server held is about
    /// logs that are gone, so both are forgotten
    /// ([`VaultStore::take_epoch`]): everything is read again from the start,
    /// and every document this account may write is then offered again, as
    /// whatever it holds beyond what the new logs turned out to.
    pub async fn catch_up(&mut self, client: &Arc<PimbleClient>, served: impl Fn(StoreId) -> bool) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        let store_ids: Vec<StoreId> = self.stores.keys().copied().filter(|id| served(*id)).collect();
        for store_id in store_ids {
            // The shares in a whole store first, so what is read below opens.
            // A new connection may also ask again about every document it
            // could not open, and the pull below reads each of them anyway.
            if let Some(store) = self.stores.get_mut(&store_id) {
                store.look.unread.clear();
                store.look.asked.clear();
                fetch_share_keys(store_id, store).await;
            }

            // Down: every document the server lists, from where this page
            // stopped reading it, or from the start for one it never held (a
            // node created elsewhere while the socket was down) or could not
            // open.
            let listing = match client.vault_list_docs_response(store_id).await {
                Ok(listing) => listing,
                Err(e) => {
                    tracing::warn!("Listing the documents of {} failed: {}", store_id, e);
                    continue;
                }
            };
            let wanted: Vec<(NodeId, u64)> = {
                let row = self.rows.get(&store_id);
                let Some(store) = self.stores.get_mut(&store_id) else { continue };
                let rebuilt = store.take_epoch(listing.epoch.as_deref(), row);
                store.parts_ok = listing.parts;
                // Read again from the cursors: every group comes whole.
                store.parts.clear();
                if rebuilt {
                    tracing::info!("Store {}: the server's logs are not the ones this page read (its twin was built again); reading them from the start and offering every document again", store_id);
                }
                let mut wanted = Vec::new();
                for info in listing.docs {
                    let VaultDocId::Node(id) = info.doc_id else { continue };
                    if rebuilt {
                        // Known to be there, whether or not the fetch below
                        // succeeds: an append to it must not try to create it.
                        store.heads.insert(id, info.head);
                    }
                    if let Some(dek_id) = info.dek_id {
                        store.keys.docs.note_listed(id, dek_id);
                    }
                    if let Some(doc) = store.docs.get_mut(&id) {
                        doc.snapshot_seq = doc.snapshot_seq.max(info.snapshot_seq);
                    }
                    let from = store.docs.get(&id).map(|d| d.cursor.applied_through()).unwrap_or(0);
                    // A document whose head has not moved is still fetched
                    // when its wraps are missing: a fetch answers with them
                    // whether or not it carries an entry, and without them
                    // nothing can be encrypted for that document.
                    let wants_keys = store.keys.docs.wants_wraps(id);
                    if info.head > from || wants_keys {
                        wanted.push((id, from));
                    }
                }
                wanted
            };
            let (pulled, structure) = self.pull(client, store_id, wanted).await;
            events.extend(pulled);

            // A pull is applied whole, then judged once: as at open, and as
            // the desktop's vault link does after its reconnect pull.
            if structure {
                if let Some(store) = self.stores.get_mut(&store_id) {
                    store.repair_due = None;
                    events.extend(store.adopt_root().map(|store| BackendEvent::StoreOpened { store }));
                }
                events.extend(self.repair(client, store_id).await);
            }

            // Up: whatever an append failed to deliver.
            let unsent: Vec<NodeId> = self
                .stores
                .get(&store_id)
                .map(|s| s.docs.iter().filter(|(_, doc)| doc.unsent).map(|(id, _)| *id).collect())
                .unwrap_or_default();
            for node_id in unsent {
                if let Err(e) = self.resend(client, store_id, node_id).await {
                    tracing::warn!("Resending the document {} failed: {}", node_id, e);
                }
            }

            // And what every device holds, compared (docs/CONVERGENCE_CONTRACT.md).
            self.compare_reports(client, store_id).await;
        }
        events
    }

    /// Fetch documents from where this page stopped reading each, and merge
    /// what comes: the events the UI should see, and whether any of it
    /// touched structure.
    async fn pull(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        wanted: Vec<(NodeId, u64)>,
    ) -> (Vec<BackendEvent>, bool) {
        let mut events = Vec::new();
        let mut structure = false;
        let fetched = fetch_many(client, store_id, wanted).await;
        let now = now_ms();
        for (node_id, fetched) in fetched {
            let Ok(fetched) = fetched else { continue };
            let Some(store) = self.stores.get_mut(&store_id) else { continue };
            let merged = store.take_fetched(store_id, node_id, &fetched, now);
            structure |= merged.structure;
            events.extend(merged.events);
        }
        (events, structure)
    }

    // ── What an open store is still owed ────────────────────────────────────

    /// One look for an open store (see [`KeyLook`]): the keys of the shares in
    /// it this page has seen a marker of and holds no key for, then the
    /// documents worth reading again: every one not read through when a key
    /// came, and otherwise the ones whose wraps this page never had.
    ///
    /// What opens reaches the UI as it would from a notification: the kinds a
    /// merge derives, and a repair once the updates have stopped.
    pub(super) async fn look_for_keys(&mut self, client: &Arc<PimbleClient>, store_id: StoreId) -> Vec<BackendEvent> {
        let wanted = {
            let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
            store.look.due = None;
            let new_key = fetch_share_keys(store_id, store).await;
            store.reread_list(new_key)
        };
        if wanted.is_empty() {
            return Vec::new();
        }
        tracing::debug!("Store {}: reading {} documents again", store_id, wanted.len());
        let (mut events, structure) = self.pull(client, store_id, wanted).await;
        if structure {
            if let Some(store) = self.stores.get_mut(&store_id) {
                store.repair_due = Some(now_ms() + REPAIR_DEBOUNCE_MS);
                events.extend(store.adopt_root().map(|store| BackendEvent::StoreOpened { store }));
            }
        }
        events
    }
}
