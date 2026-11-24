//! What one encrypted store sends: sealing an update, recording the append, dropping documents.

use std::collections::HashSet;

use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_crypto::KeyId;
use pimble_rpc::vault::{self as protocol, LogSizes, Mark, NewDocument, Sealed};
use pimble_rpc::VaultDocId;

use super::{Outgoing, Side, VaultStore};
use super::helpers::{advance, now_ms};

impl VaultStore {
    /// Encrypt what goes to the server for one document: `update`, already
    /// merged into the document here, or — after a failed append, when the
    /// server is behind by more than this one change — everything it lacks.
    ///
    /// The key is the document's data key: the one this page has already
    /// resolved, the one its wraps yield, or a fresh one made here for a
    /// document the server has never seen, wrapped under every scope key this
    /// page holds that covers the node. A document from before data keys,
    /// whose blobs name a scope key directly, keeps being written under that
    /// key: giving it one is its owner's business, not a member's.
    pub(super) fn prepare(&mut self, store_id: StoreId, node_id: NodeId, update: &[u8]) -> Result<Outgoing, String> {
        let Some(node_doc) = self.tree.doc(node_id) else {
            return Err("that node's document is not held here".to_string());
        };
        let sent_sv = node_doc.state_vector();
        self.note_report_due(node_id, now_ms());
        let resend = self.docs.entry(node_id).or_default().unsent;
        let payload = if resend {
            let known = self.docs[&node_id].known_sv.clone();
            self.tree.doc(node_id).expect("held above").diff_since(&known).map_err(|e| e.to_string())?
        } else {
            update.to_vec()
        };

        let creating = !self.heads.contains_key(&node_id);
        let sealed = if creating {
            let new = NewDocument::of(&self.tree, node_id);
            let scope_key_ids = self.scope_key_ids_for(node_id);
            self.keys
                .create_document_key(store_id, node_id, &scope_key_ids)
                .map(|(key_id, key, keys)| (key_id, key, Some((keys, new))))
        } else {
            self.keys.encrypt_key(store_id, node_id).map(|(key_id, key)| (key_id, key, None))
        };
        let Some((key_id, key, created)) = sealed else {
            // No key for this document yet: its wraps have not been fetched,
            // or none of them is under a scope key this page holds. The work
            // is already in the document, so it is marked unsent and the next
            // connection — which fetches the wraps — resends it.
            self.docs.entry(node_id).or_default().unsent = true;
            self.hold(node_id, Side::Send, protocol::WAITING_FOR_KEY);
            return Err(format!("no key for the document {node_id} on this device yet"));
        };

        // In parts when it is larger than a part and the server takes them
        // (docs/SYNC_LIMITS_CONTRACT.md).
        let blobs = protocol::seal_parts(store_id, node_id, key_id, &key, &payload, self.parts_ok);
        Ok(Outgoing { doc_id: VaultDocId::Node(node_id), blobs, payload, sent_sv, resend, created })
    }

    /// The scope keys that cover a node (`pimble_rpc::vault::scope_key_ids`):
    /// the store key, when this page holds the whole store, and the key of
    /// every share the node sits under; for a scoped member with no marker on
    /// the way up, the keys it was given, which are its shares'.
    pub(super) fn scope_key_ids_for(&self, node_id: NodeId) -> Vec<KeyId> {
        let store_key = if self.is_partial() { None } else { self.keys.scope.key_id_for(None) };
        let given = self.keys.scope.scopes.iter().map(|(_, id)| *id);
        let mut ids = protocol::scope_key_ids(&self.tree, node_id, store_key, given);
        ids.retain(|id| self.keys.scope.get(id).is_some());
        ids
    }

    /// Record how an append went. `Ok(Some(seq))` says a snapshot stamped
    /// `seq` is due; `Err` is the append's failure, recorded against the
    /// document so the next append (or the next connect) resends it.
    #[cfg(test)]
    pub(super) fn record(&mut self, node_id: NodeId, outgoing: &Outgoing, appended: Result<u64, String>) -> Result<Option<u64>, String> {
        self.record_span(node_id, outgoing, appended.map(|seq| (seq, seq, None)))
    }

    /// [`VaultStore::record`] for an append that may have gone in parts:
    /// numbered `first..=last`, with the log's sizes when the server gave
    /// them (what a snapshot is also due by, `snapshot_due_by`).
    pub(super) fn record_span(&mut self, node_id: NodeId, outgoing: &Outgoing, appended: Result<(u64, u64, Option<LogSizes>), String>) -> Result<Option<u64>, String> {
        let (first, seq, sizes) = match appended {
            Ok(span) => span,
            Err(message) => {
                self.docs.entry(node_id).or_default().unsent = true;
                self.hold(node_id, Side::Send, format!("Not sent yet: {message}"));
                return Err(message);
            }
        };
        self.release(node_id, Side::Send);
        let doc = self.docs.entry(node_id).or_default();
        if outgoing.resend {
            doc.known_sv = outgoing.sent_sv.clone();
            doc.unsent = false;
        } else {
            advance(&mut doc.known_sv, &outgoing.payload);
        }
        Mark::Span(first, seq).apply(&mut doc.cursor);
        // Stamped only with a number read *through* (`snapshot_due_by`): the
        // server deletes every log entry at or below a snapshot's number.
        let due = protocol::snapshot_due_by(seq, doc.snapshot_seq, &doc.cursor, sizes);
        let head = self.heads.entry(node_id).or_insert(0);
        *head = (*head).max(seq);
        Ok(due.then_some(seq))
    }

    /// Forget everything this page holds of one document, because the server
    /// refused an append to it.
    ///
    /// There is nothing to resend — the same bytes would be refused for ever,
    /// and a document left `unsent` would offer them again on every reconnect
    /// — and nothing in the local document is worth keeping either: a yrs
    /// document cannot un-merge the edit that was refused, so showing it would
    /// show work that exists nowhere and never will. Dropping it is what makes
    /// the next fetch start from the beginning and end at what the server
    /// holds.
    pub(super) fn refused(&mut self, node_id: NodeId) {
        self.drop_doc(node_id);
    }

    /// Let go of one document and everything kept about it.
    fn drop_doc(&mut self, node_id: NodeId) {
        self.tree.take_doc(node_id);
        self.docs.remove(&node_id);
        self.heads.remove(&node_id);
        self.keys.forget(node_id);
        self.look.unread.remove(&node_id);
        self.look.asked.retain(|(id, _)| *id != node_id);
    }

    /// Let go of the shares rooted at `ended`: they are no longer scope roots
    /// of this page, and every document under one of them and under no root
    /// still held is dropped (overlapping shares: what is also under a share
    /// that is left stays). Answers `false`, having changed nothing, when no
    /// scope root would be left: such a store is not this function's to
    /// keep open (with no scope root it would read as a whole store).
    pub(super) fn drop_roots(&mut self, ended: &[NodeId]) -> bool {
        let left: Vec<NodeId> = self.scope_roots.iter().copied().filter(|root| !ended.contains(root)).collect();
        if left.is_empty() {
            return false;
        }
        let kept: HashSet<NodeId> = left.iter().flat_map(|root| self.tree.subtree_ids(*root).unwrap_or_default()).collect();
        let dropped: Vec<NodeId> = ended
            .iter()
            .filter(|root| self.scope_roots.contains(root))
            .flat_map(|root| self.tree.subtree_ids(*root).unwrap_or_else(|_| vec![*root]))
            .filter(|id| !kept.contains(id))
            .collect();
        for id in dropped {
            self.drop_doc(id);
        }
        self.scope_roots = left;
        self.listed.roots.retain(|root| !ended.contains(root));
        // The tree starts from the first scope root for its life; when that
        // one went, it starts from the first that is left.
        if ended.contains(&self.tree.root()) {
            let root = self.scope_roots[0];
            self.listed.root_node_id = root;
            self.tree.reroot(root);
        }
        true
    }

    /// The document's whole state, encrypted, for a snapshot. Under the same
    /// key its updates go out under: a snapshot replaces them.
    pub(super) fn snapshot_blob(&mut self, store_id: StoreId, node_id: NodeId) -> Result<Vec<Sealed>, String> {
        let full = self.tree.doc(node_id).map(NodeDoc::save).unwrap_or_default();
        let (key_id, key) = self
            .keys
            .encrypt_key(store_id, node_id)
            .ok_or_else(|| format!("no key for the document {node_id} on this device"))?;
        Ok(protocol::seal_parts(store_id, node_id, key_id, &key, &full, self.parts_ok))
    }
}
