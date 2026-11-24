//! Live updates and the store's subscription: notifications, repairs, presence.

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use pimble_app::protocol::BackendEvent;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_crypto::Blob;
use pimble_rpc::vault::should_apply;
use pimble_rpc::{StoreChangeKind, VaultDocId, VaultPart};

use super::{VaultClient, REPAIR_DEBOUNCE_MS};
use super::helpers::{now_ms, now_rfc3339};

impl VaultClient {
    // ── Live updates ────────────────────────────────────────────────────────

    /// Apply whatever the subscriptions have delivered since the last pass.
    ///
    /// Returns the events the UI should see. Called once per turn of the
    /// backend loop, which is what keeps `&mut self` out of the subscription
    /// task (it only forwards raw notifications down a channel).
    pub fn pump(&mut self) -> Vec<BackendEvent> {
        let mut events = std::mem::take(&mut self.pending);
        events.extend(self.plugin_events());
        events.extend(self.open_presence());
        while let Ok(notification) = self.notices_rx.try_recv() {
            // Another device's state report: answered by the backend loop,
            // which has the connection to append with (`exchange_reports`).
            if let StoreChangeKind::VaultReported { doc_id: VaultDocId::Node(node_id), device, blob, account } = &notification.change_kind {
                if let Some(store) = self.stores.get_mut(&notification.store_id).filter(|store| store.reports_ok) {
                    store.reports_in.push((*node_id, device.clone(), blob.clone(), account.clone()));
                }
                continue;
            }
            if matches!(notification.change_kind, StoreChangeKind::VaultReported { .. }) {
                continue;
            }
            let StoreChangeKind::VaultAppended { doc_id, seq, part } = &notification.change_kind else {
                // A vault store emits nothing else, but forward anything that
                // does arrive rather than swallowing it.
                events.push(BackendEvent::RemoteStoreChange {
                    store_id: notification.store_id,
                    change_kind: notification.change_kind.clone(),
                    source_client_id: notification.source_client_id.clone(),
                });
                continue;
            };
            let store_id = notification.store_id;
            let node_id = match doc_id {
                VaultDocId::Node(id) => *id,
                VaultDocId::Tree => {
                    tracing::debug!("An append to the tree document of {}; nothing reads it", store_id);
                    continue;
                }
                // Never announced; nothing to do if one ever is.
                VaultDocId::Blob(_) => continue,
            };
            let seq = *seq;
            let Some(blob) = notification.update.clone() else {
                tracing::warn!("A VaultAppended notification carried no blob; ignoring it");
                continue;
            };
            let source = notification.source_client_id.clone();
            events.extend(self.apply_notification(store_id, node_id, seq, *part, &blob, source));
        }
        self.tell_watchers();
        events.extend(self.held_back_events());
        events
    }

    fn apply_notification(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        seq: u64,
        part: Option<VaultPart>,
        blob: &str,
        source_client_id: Option<String>,
    ) -> Vec<BackendEvent> {
        let apply = should_apply(source_client_id.as_deref(), &self.client_id);
        let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };

        // Sequence numbers say how far this client has read, never who wrote
        // what; `should_apply` is the whole of that decision. This client's own
        // append is already in its document, so its number counts as read.
        if !apply {
            if let Some(doc) = store.docs.get_mut(&node_id) {
                doc.cursor.mark(seq);
            }
            return Vec::new();
        }

        let merged = match part {
            // One part of an append sent in parts: held until its group is
            // whole, then applied as one.
            Some(part) => match store.parts.take(&node_id.to_string(), seq, part, blob.to_string()) {
                Some((mark, parts)) => store.take_parts(store_id, node_id, mark, &parts, source_client_id, now_ms()),
                None => return Vec::new(),
            },
            None => store.take_blob(store_id, node_id, seq, blob, source_client_id, now_ms()),
        };

        let mut events = merged.events;
        if merged.structure {
            store.repair_due = Some(now_ms() + REPAIR_DEBOUNCE_MS);
            events.extend(store.adopt_root().map(|store| BackendEvent::StoreOpened { store }));
        }
        events
    }

    /// The stores whose debounced repair has come due.
    pub fn repairs_due(&self) -> Vec<StoreId> {
        let now = now_ms();
        self.stores
            .iter()
            .filter(|(_, store)| store.repair_due.is_some_and(|due| due <= now))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Repair a store's tree if it needs it and append what the repair wrote,
    /// as any edit is appended. The UI hears `TreeStructure` for the
    /// documents it touched, as it would from the server.
    pub async fn repair(&mut self, client: &Arc<PimbleClient>, store_id: StoreId) -> Vec<BackendEvent> {
        let edit = {
            let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
            store.repair_due = None;
            store.repair_now(&now_rfc3339())
        };
        let Some(edit) = edit else { return Vec::new() };
        tracing::info!("Store {}: repaired the tree ({} documents)", store_id, edit.touched.len());
        if let Err(message) = self.append_edit(client, store_id, &edit).await {
            tracing::warn!("Appending the repair of {} failed: {}", store_id, message);
        }
        vec![BackendEvent::RemoteStoreChange {
            store_id,
            change_kind: StoreChangeKind::TreeStructure { node_ids: edit.node_ids() },
            source_client_id: None,
        }]
    }

    // ── Subscription ────────────────────────────────────────────────────────

    /// Subscribe to the store's changes and forward every notification into the
    /// channel `pump` drains.
    ///
    /// The task deliberately holds nothing but the sender: decryption needs
    /// `&mut self`, which a detached task cannot have and the backend loop
    /// already does.
    ///
    /// Three things ask for this — the client itself when it opens a store,
    /// the UI when it registers one, and the backend loop when it restores
    /// what a dead socket carried — and one per store per connection is what
    /// is wanted. `forget_subscriptions` is what makes a new socket start
    /// again.
    pub async fn subscribe(
        &mut self,
        client: &Arc<PimbleClient>,
        store_id: StoreId,
        signal_ui: &Arc<dyn Fn() + Send + Sync>,
    ) -> Option<BackendEvent> {
        if !self.subscribed.insert(store_id) {
            return None;
        }
        match client.subscribe_store_changes_with_reports(store_id).await {
            Ok(mut sub) => {
                let tx = self.notices_tx.clone();
                let signal = signal_ui.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    while let Some(Ok(notification)) = sub.next().await {
                        if tx.send(notification).is_err() {
                            break;
                        }
                        signal();
                    }
                });
                self.subscribe_presence(client, store_id, signal_ui).await;
                None
            }
            Err(e) => {
                self.subscribed.remove(&store_id);
                tracing::warn!("Subscribing to the changes of {} failed: {}", store_id, e);
                Some(BackendEvent::Error { message: format!("Subscribe failed: {e}") })
            }
        }
    }

    /// Hear who else is in the store's documents (docs/PRESENCE_CONTRACT.md):
    /// the frames already there, then each one as it comes, sealed, down the
    /// channel [`pump`] opens them from. A server without presence answers
    /// nothing, and that is no error.
    async fn subscribe_presence(&mut self, client: &Arc<PimbleClient>, store_id: StoreId, signal_ui: &Arc<dyn Fn() + Send + Sync>) {
        let mut sub = match client.subscribe_presence(store_id, &self.client_id).await {
            Ok(sub) => sub,
            Err(e) => {
                tracing::debug!("No presence for {}: {}", store_id, e);
                return;
            }
        };
        for frame in client.get_presence(store_id).await.unwrap_or_default() {
            let _ = self.presence_tx.send(frame);
        }
        let tx = self.presence_tx.clone();
        let signal = signal_ui.clone();
        wasm_bindgen_futures::spawn_local(async move {
            while let Some(Ok(frame)) = sub.next().await {
                if tx.send(frame).is_err() {
                    break;
                }
                signal();
            }
        });
        signal_ui();
    }

    /// Send a pane's presence frame (`pimble_app::commands::presence_frame`),
    /// its body sealed under the document's data key: the server holds which
    /// document and who, never what it says.
    pub async fn publish_presence(&mut self, client: &Arc<PimbleClient>, mut frame: pimble_rpc::PresenceFrame) {
        let (store_id, node_id) = (frame.store_id, frame.doc_id);
        if !frame.left {
            let Some(store) = self.stores.get_mut(&store_id) else { return };
            let Some((key_id, key)) = store.keys.encrypt_key(store_id, node_id) else { return };
            let Ok(plain) = URL_SAFE_NO_PAD.decode(&frame.body) else { return };
            let aad = pimble_crypto::presence_aad(&store_id.to_string(), &node_id.to_string());
            frame.body = URL_SAFE_NO_PAD.encode(Blob::encrypt(&key, key_id, &aad, &plain));
            frame.sealed = true;
        }
        if let Err(e) = client.publish_presence(frame, &self.client_id).await {
            tracing::debug!("Presence in {} not taken: {}", store_id, e);
        }
    }

    /// The presence frames delivered since the last pass, opened: anyone's
    /// but this page's own, and nothing that does not open.
    fn open_presence(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        while let Ok(mut frame) = self.presence_rx.try_recv() {
            if frame.client() == self.client_id {
                continue;
            }
            if frame.sealed {
                let Some(store) = self.stores.get_mut(&frame.store_id) else { continue };
                let Ok(blob) = URL_SAFE_NO_PAD.decode(&frame.body) else { continue };
                let Ok(key_id) = Blob::key_id(&blob) else { continue };
                let Some(key) = store.keys.blob_key(frame.store_id, frame.doc_id, key_id) else { continue };
                let aad = pimble_crypto::presence_aad(&frame.store_id.to_string(), &frame.doc_id.to_string());
                let Ok(plain) = Blob::decrypt(&key, &aad, &blob) else { continue };
                frame.body = URL_SAFE_NO_PAD.encode(plain);
                frame.sealed = false;
            }
            events.push(BackendEvent::PresenceFrame { frame });
        }
        events
    }

    /// Forget which of the stores an endpoint serves are subscribed, because
    /// the socket that carried those subscriptions is gone. Called once per
    /// new connection, before anything subscribes again on it. `served` says
    /// which stores are that endpoint's: another endpoint's socket is its own.
    pub fn forget_subscriptions(&mut self, served: impl Fn(StoreId) -> bool) {
        self.subscribed.retain(|id| !served(*id));
    }
}
