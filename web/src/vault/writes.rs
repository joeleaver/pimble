//! Tree writes and content: every edit goes out encrypted through `vaultAppend`.

use std::collections::HashMap;

use pimble_app::protocol::BackendEvent;
use pimble_api::ApiError;
use pimble_core::{NodeId, Place, Store, StoreId};
use pimble_crdt::{move_order, NodeUpdateEffect, TreeEdit};
use pimble_rpc::vault::LogSizes;
use pimble_rpc::{StoreChangeKind, VaultAppendResponse, VaultDocId};

use super::{not_open, VaultClient, VaultWire, REPAIR_DEBOUNCE_MS};
use super::helpers::{now_ms, now_rfc3339};
use super::shares::as_left_shares;

impl VaultClient {
    // ── Tree writes ─────────────────────────────────────────────────────────

    /// `createNode`, as the server makes one: the node and what it starts
    /// with (a board node's board, its icon) in the one edit that makes it.
    pub async fn create_node<W: VaultWire + ?Sized>(
        &mut self,
        client: &W,
        store_id: StoreId,
        parent: NodeId,
        init: pimble_api::NodeInit,
        place: Place,
    ) -> Result<NodeId, ApiError> {
        let node_id = NodeId::new();
        let now = now_rfc3339();
        let edit = {
            let store = self.stores.get_mut(&store_id).ok_or_else(not_open)?;
            let mut edit = store.seed_root(&now);
            edit.touched.extend(store.tree.add_node(node_id, Some(parent), place, &init.node_type, &init.title, &now)?.touched);
            if let Some(content) = init.content.as_ref().filter(|content| !content.is_empty()) {
                store.tree.apply_update(node_id, content)?;
                edit.touched.push((node_id, content.clone()));
            }
            for (key, value) in &init.custom {
                edit.touched.extend(store.tree.set_custom(node_id, key, value, &now)?.touched);
            }
            edit
        };
        self.append_edit(client, store_id, &edit).await?;
        Ok(node_id)
    }

    /// An attachment of `host_id` (a discussion on it,
    /// docs/DISCUSSIONS_CONTRACT.md section 1), as the server's
    /// `createAttachment` makes one: only the new document is written.
    pub async fn create_attachment<W: VaultWire + ?Sized>(
        &mut self,
        client: &W,
        store_id: StoreId,
        host_id: NodeId,
        init: pimble_api::NodeInit,
    ) -> Result<NodeId, ApiError> {
        let node_id = NodeId::new();
        let now = now_rfc3339();
        let edit = {
            let store = self.stores.get_mut(&store_id).ok_or_else(not_open)?;
            let mut edit = store.seed_root(&now);
            let content = init.content.as_deref().filter(|content| !content.is_empty());
            edit.touched.extend(store.tree.add_attachment(node_id, host_id, &init.node_type, &init.title, content, &now)?.touched);
            for (key, value) in &init.custom {
                edit.touched.extend(store.tree.set_custom(node_id, key, value, &now)?.touched);
            }
            edit
        };
        self.append_edit(client, store_id, &edit).await?;
        Ok(node_id)
    }

    /// The whole subtree becomes tombstones, as it does server-side; the
    /// documents stay, so an undelete elsewhere finds them. The root is
    /// refused in the server's words.
    pub async fn delete_node<W: VaultWire + ?Sized>(&mut self, client: &W, store_id: StoreId, node_id: NodeId) -> Result<(), ApiError> {
        let now = now_rfc3339();
        let edit = {
            let store = self.stores.get_mut(&store_id).ok_or_else(not_open)?;
            let info = store.tree.get_node_info(node_id).map_err(|_| ApiError::node_not_found(node_id))?;
            if info.parent_id.is_none() {
                return Err(ApiError::invalid_operation("Cannot delete the root node"));
            }
            store.tree.remove_node(node_id, &now)?
        };
        self.append_edit(client, store_id, &edit).await
    }

    /// Several nodes under one parent, in order, all or none: what
    /// `moveNodes` is for a plain store (docs/TREE_CONTRACT.md). Each node
    /// once (a repeat was moved twice until 2026-11-02, the server's
    /// `moveNodes` never did), every one judged first in the order named;
    /// then each goes to the same place in the order `move_order` gives, so
    /// they keep their order without waiting on the ids transplants give
    /// them. Answered in the order named, as the server answers.
    pub async fn move_nodes<W: VaultWire + ?Sized>(
        &mut self,
        client: &W,
        store_id: StoreId,
        node_ids: &[NodeId],
        new_parent_id: NodeId,
        place: Place,
    ) -> Result<Vec<pimble_api::Moved>, ApiError> {
        let (once, order) = move_order(node_ids, place);
        {
            let store = self.stores.get(&store_id).ok_or_else(not_open)?;
            for &id in &once {
                store.tree.check_move(id, new_parent_id).map_err(ApiError::of_move)?;
            }
        }
        let mut answers = HashMap::new();
        for id in order {
            answers.insert(id, self.move_node(client, store_id, id, new_parent_id, place).await?);
        }
        Ok(once.iter().filter_map(|id| answers.remove(id)).collect())
    }

    /// `Tree::move_or_transplant` decides, as every caller of a move does
    /// (docs/MOVE_CONTRACT.md): a plain move inside a share or outside all of
    /// them, or a transplant when it would take the node out of one, with a
    /// fresh document (its key riding its first append, exactly as
    /// [`VaultClient::create_node`] makes one) for every node of the subtree
    /// and the original tombstoned. `Tree::transplant` orders the touched
    /// documents new-first, tombstones-last, and one `append_edit` call sends
    /// them in that order; a document an append fails on is marked unsent and
    /// resent by the existing mechanism, whichever half of the move it was.
    pub async fn move_node<W: VaultWire + ?Sized>(
        &mut self,
        client: &W,
        store_id: StoreId,
        node_id: NodeId,
        new_parent_id: NodeId,
        place: Place,
    ) -> Result<pimble_api::Moved, ApiError> {
        let now = now_rfc3339();
        let (left_shares, new_node_id, edit) = {
            let store = self.stores.get_mut(&store_id).ok_or_else(not_open)?;
            // The checks the server's `moveNode` makes, in its words.
            store.tree.check_move(node_id, new_parent_id).map_err(ApiError::of_move)?;
            // What the move would leave, read before it happens: a
            // transplant's original is a tombstone afterwards and answers
            // nothing about the shares it used to be in.
            let left_shares = as_left_shares(&store.tree, store.tree.shares_left(node_id, new_parent_id));
            let mut edit = store.seed_root(&now);
            let (new_node_id, more) = store.tree.move_or_transplant(store_id, node_id, new_parent_id, place, &now, &mut NodeId::new)?;
            edit.touched.extend(more.touched);
            (left_shares, new_node_id, edit)
        };
        self.append_edit(client, store_id, &edit).await?;
        Ok(pimble_api::Moved { node: new_node_id, left_shares })
    }

    /// "Put Back": clear the tombstone and everything the same deletion took
    /// with it, back under the parent it was deleted from.
    pub async fn undelete_node<W: VaultWire + ?Sized>(&mut self, client: &W, store_id: StoreId, node_id: NodeId) -> Result<(), ApiError> {
        let now = now_rfc3339();
        let edit = {
            let store = self.stores.get_mut(&store_id).ok_or_else(not_open)?;
            store.tree.undelete_node(node_id, &now)?
        };
        self.append_edit(client, store_id, &edit).await
    }

    /// A transplant between two vault stores this page holds
    /// (docs/MOVE_CONTRACT.md "Between stores"): the subtree read out of the
    /// source as data ([`pimble_crdt::Tree::take_cutting`]), planted under
    /// `new_parent_id` in the destination with a fresh id and a fresh key per
    /// node ([`pimble_crdt::Tree::plant`]), appended there, and only once
    /// that whole append succeeds is the source tombstoned and its own
    /// tombstones appended: created first, deleted second, so a failure
    /// between the two leaves the original exactly where it was.
    ///
    /// `left_shares` is every share the node was in: leaving the store leaves
    /// all of them, since two stores never share an ancestor. Which two
    /// stores may be transplanted between at all is the router's to judge
    /// (`pimble_api::Router`'s `transplant`).
    #[allow(clippy::too_many_arguments)]
    pub async fn transplant_node<W: VaultWire + ?Sized>(
        &mut self,
        from_client: &W,
        to_client: &W,
        from_store_id: StoreId,
        node_id: NodeId,
        to_store_id: StoreId,
        new_parent_id: NodeId,
        place: Place,
    ) -> Result<pimble_api::Moved, ApiError> {
        if from_store_id == to_store_id {
            return self.move_node(from_client, from_store_id, node_id, new_parent_id, place).await;
        }
        let now = now_rfc3339();

        let (left_shares, cutting) = {
            let from_store = self.stores.get(&from_store_id).ok_or_else(not_open)?;
            let info = from_store.tree.get_node_info(node_id).map_err(|_| ApiError::node_not_found(node_id))?;
            if info.parent_id.is_none() {
                return Err(ApiError::invalid_operation("Cannot move the root node into another store"));
            }
            let left_shares = as_left_shares(&from_store.tree, from_store.tree.shares(node_id));
            (left_shares, from_store.tree.take_cutting(node_id)?)
        };

        let (planted, edit_to) = {
            let to_store = self.stores.get_mut(&to_store_id).ok_or_else(not_open)?;
            let mut edit = to_store.seed_root(&now);
            let (planted, more) = to_store.tree.plant(cutting, new_parent_id, place, &now, &mut NodeId::new)?;
            edit.touched.extend(more.touched);
            (planted, edit)
        };

        // Created first: a failure appending the new documents leaves the
        // original untouched, never tombstoned with nothing to show for it.
        self.append_edit(to_client, to_store_id, &edit_to).await?;

        // Deleted second, only now that the new home is on the server.
        let tombstone_edit = {
            let from_store = self.stores.get_mut(&from_store_id).ok_or_else(not_open)?;
            from_store.tree.remove_transplanted(node_id, to_store_id, &planted, &now)?
        };
        if let Err(e) = self.append_edit(from_client, from_store_id, &tombstone_edit).await {
            // The new node is already live and answered below; a failed
            // tombstone is unsent like any other failed append and resent by
            // the same mechanism, not a reason to tell the person their move
            // did not happen.
            tracing::warn!("Appending the tombstone of {} after a transplant failed: {}", node_id, e);
        }

        Ok(pimble_api::Moved { node: planted.root, left_shares })
    }

    /// Append what a tree operation wrote: one `vaultAppend` per touched
    /// document, in the order the edit names them (a new document before its
    /// parent's list, so a peer never lists a node it cannot read yet).
    ///
    /// Every document is attempted even after one fails, so each failure is
    /// recorded against its own document and resent from there; stopping at
    /// the first would leave the rest holding work nothing knows is unsent.
    pub(super) async fn append_edit<W: VaultWire + ?Sized>(&mut self, client: &W, store_id: StoreId, edit: &TreeEdit) -> Result<(), ApiError> {
        let structure = NodeUpdateEffect { changed: true, structure: true, ..Default::default() };
        for (node_id, update) in &edit.touched {
            self.wrote(store_id, *node_id, structure, update);
        }
        let mut first_error = None;
        for (node_id, update) in &edit.touched {
            if let Err(e) = self.append_prepared(client, store_id, *node_id, update).await {
                tracing::warn!("Appending to the document {} failed: {}", node_id, e);
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    // ── Content ─────────────────────────────────────────────────────────────

    /// The one write (`applyEdit`'s part, `pimble_api::Backend::apply`): an
    /// update of one document, merged here and appended. Judged per document
    /// first, as the server judges an `applyEdit` (a commenter writes
    /// discussions and nothing else). With no connection to send it through
    /// (`client` `None`: the store's owner's computer is off), what the
    /// editor has already put on screen is merged and kept as unsent, which
    /// the next connection's catch-up delivers; a store this page never
    /// opened takes nothing.
    pub async fn apply<W: VaultWire + ?Sized>(&mut self, client: Option<&W>, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<(), ApiError> {
        let Some(store) = self.stores.get(&store_id) else {
            if self.owner_offline.contains(&store_id) {
                return Err(ApiError::OwnerOffline { sentence: pimble_api::router::OWNER_OFFLINE_REFUSAL.to_string() });
            }
            return Err(not_open());
        };
        if let Some(refusal) = ApiError::of_access(store.access_of(node_id, self.rows.get(&store_id))) {
            return Err(refusal);
        }
        match client {
            Some(client) => self.append_update(client, store_id, node_id, update).await,
            None => self.keep_unsent(store_id, node_id, update).map_err(|sentence| ApiError::Failed { sentence }),
        }
    }

    /// Merge `update` into the node's document (the editor's delta, or a whole
    /// document from a session that started from nothing), then append it.
    async fn append_update<W: VaultWire + ?Sized>(&mut self, client: &W, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<(), ApiError> {
        let effect = {
            let store = self.stores.get_mut(&store_id).ok_or_else(not_open)?;
            store.tree.apply_update(node_id, update)?
        };
        if effect.changed {
            self.wrote(store_id, node_id, effect, update);
        }
        self.append_prepared(client, store_id, node_id, update).await
    }

    /// Send everything `node_id`'s document holds that the server does not,
    /// for a document an earlier append failed to deliver.
    pub(super) async fn resend<W: VaultWire + ?Sized>(&mut self, client: &W, store_id: StoreId, node_id: NodeId) -> Result<(), ApiError> {
        // An empty update: `prepare` sends the diff since `known_sv` for an
        // unsent document whatever it is handed.
        self.append_prepared(client, store_id, node_id, &[]).await
    }

    /// Append one document's update, already merged into the document here:
    /// encrypt, `vaultAppend`, record the outcome, snapshot when due.
    ///
    /// A document the server has never seen is created: its append names the
    /// parent (which is how a scoped member's new document joins their scope)
    /// and carries its wrapped data key, which the server stores together
    /// with the blob, so nothing is ever there under a key it has no wrap of.
    pub(super) async fn append_prepared<W: VaultWire + ?Sized>(&mut self, client: &W, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<(), ApiError> {
        let outgoing = {
            let store = self.stores.get_mut(&store_id).ok_or_else(not_open)?;
            store.prepare(store_id, node_id, update).map_err(|sentence| ApiError::Failed { sentence })?
        };
        let new = outgoing.created.as_ref().map(|(_, new)| *new).unwrap_or_default();
        let keys = outgoing.created.as_ref().map(|(keys, _)| keys.clone());
        // A new discussion on a node says so, which is what lets a commenter
        // create it and puts it in the store's discussion set
        // (docs/DISCUSSIONS_CONTRACT.md section 7).
        let mut appended: Result<VaultAppendResponse, ApiError> = Err(ApiError::Failed { sentence: "nothing was sent".into() });
        for one in &outgoing.blobs {
            appended = client
                .append(store_id, outgoing.doc_id.clone(), one.blob.clone(), Some(self.client_id.clone()), new.parent_id, new.discussion_host, keys.clone(), one.part)
                .await
                .map_err(ApiError::from);
            if appended.is_err() {
                break;
            }
        }
        let appended = appended.map(|answer| (answer.first_seq.unwrap_or(answer.seq), answer.seq, LogSizes::of(&answer)));

        // A refusal is the server saying no — a reader's root inside a store
        // this account edits elsewhere, or a document in nobody's scope. There
        // is nothing to resend, so the document is not left `unsent` (which
        // would push the same refused bytes on every reconnect); instead it is
        // pulled again, so what is on screen is what the server holds.
        if let Err(e) = &appended {
            if e.is_refusal() {
                tracing::warn!("Store {} refused an append to {}: {}", store_id, node_id, e);
                let events = self.repull(client, store_id, node_id).await;
                self.pending.extend(events);
                return Err(e.clone());
            }
        }

        let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
        let snapshot = match store.record_span(node_id, &outgoing, appended.clone().map_err(|e| e.sentence())) {
            Ok(snapshot) => snapshot,
            Err(sentence) => return Err(appended.err().unwrap_or(ApiError::Failed { sentence })),
        };

        // The document is on the server now, and its wraps with it.
        if let Some((keys, _)) = &outgoing.created {
            store.keys.note_created(node_id, keys);
        }

        if let Some(upto_seq) = snapshot {
            let Some(store) = self.stores.get_mut(&store_id) else { return Ok(()) };
            let blobs = store.snapshot_blob(store_id, node_id).map_err(|sentence| ApiError::Failed { sentence })?;
            // In parts when it is large: the server replaces the snapshot it
            // holds only when the last part lands.
            let mut sent = Ok(());
            for one in blobs {
                sent = client.snapshot(store_id, outgoing.doc_id.clone(), upto_seq, one.blob, one.part).await;
                if sent.is_err() {
                    break;
                }
            }
            match sent {
                Ok(()) => {
                    if let Some(doc) = self.stores.get_mut(&store_id).and_then(|s| s.docs.get_mut(&node_id)) {
                        doc.snapshot_seq = doc.snapshot_seq.max(upto_seq);
                    }
                }
                Err(e) => tracing::warn!("Uploading a snapshot of {} failed: {}", node_id, e),
            }
        }
        Ok(())
    }

    /// Throw one document away and fetch it again from the start, so what this
    /// page holds is what the server holds.
    ///
    /// The only caller is a refused append. A yrs document cannot un-merge the
    /// edit the server would not take, and leaving it would show work that
    /// exists nowhere else and never will; the honest answer is the server's
    /// copy. A document the server refuses to serve either is simply dropped:
    /// it is not this account's to see.
    async fn repull<W: VaultWire + ?Sized>(&mut self, client: &W, store_id: StoreId, node_id: NodeId) -> Vec<BackendEvent> {
        // The parent before the document goes: what the UI has drawn of that
        // list is what changes, whether the document comes back or not.
        let touched = {
            let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
            let mut touched = vec![node_id];
            touched.extend(
                store
                    .tree
                    .doc(node_id)
                    .and_then(|doc| doc.fields().ok())
                    .and_then(|fields| fields.parent_id),
            );
            store.refused(node_id);
            touched
        };
        let fetched = client.fetch(store_id, VaultDocId::Node(node_id), 0).await;
        let Some(store) = self.stores.get_mut(&store_id) else { return Vec::new() };
        let fetched = match fetched {
            Ok(fetched) => fetched,
            Err(e) => {
                tracing::warn!("Pulling {} again after a refusal failed: {}", node_id, e);
                return vec![BackendEvent::RemoteStoreChange {
                    store_id,
                    change_kind: StoreChangeKind::TreeStructure { node_ids: touched },
                    source_client_id: None,
                }];
            }
        };
        let merged = store.take_fetched(store_id, node_id, &fetched, now_ms());
        if merged.structure {
            store.repair_due = Some(now_ms() + REPAIR_DEBOUNCE_MS);
        }
        let mut events = merged.events;
        // The tree holds one document fewer or one different: what the UI has
        // drawn of it, and of the list it was in, is stale either way.
        events.push(BackendEvent::RemoteStoreChange {
            store_id,
            change_kind: StoreChangeKind::TreeStructure { node_ids: touched },
            source_client_id: None,
        });
        events
    }

    /// Close a store in this page: what it holds of it is let go, its
    /// subscription is no longer restored, and it stays out of the list
    /// until the page is loaded again (a browser opens what the account
    /// grants; there is no store to open by path). Until 2026-11-02 an
    /// encrypted store answered "Closing a store is not available".
    pub fn close(&mut self, store_id: StoreId) -> BackendEvent {
        self.stores.remove(&store_id);
        self.waiting.remove(&store_id);
        self.subscribed.remove(&store_id);
        self.closed.insert(store_id);
        BackendEvent::StoreClosed { store_id }
    }

    /// Whether this page closed the store ([`VaultClient::close`]).
    pub fn is_closed(&self, store_id: StoreId) -> bool {
        self.closed.contains(&store_id)
    }

    /// The encrypted stores open in this page, as the UI is told them.
    pub fn held_stores(&self) -> Vec<Store> {
        self.stores
            .values()
            .map(|store| {
                let mut row = store.view();
                self.describe(&mut row);
                row
            })
            .collect()
    }
}
