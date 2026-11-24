//! The keys of one encrypted store: scope keys and documents' data keys.

use pimble_core::{NodeId, StoreId};
use pimble_crypto::{KeyId, SymmetricKey};
use pimble_rpc::vault::{self as protocol, DocKeys, Mark, SealKey};
use pimble_rpc::{VaultDocKeys, VaultFetchResponse, VaultPart};

use crate::keys::Keyring;

use super::StoreKeys;

// ── Keys ────────────────────────────────────────────────────────────────────

impl StoreKeys {
    pub(super) fn new(scope: Keyring) -> Self {
        Self { scope, docs: DocKeys::default() }
    }

    /// Take a `vaultFetch`'s answer about a document's data key
    /// ([`DocKeys::note_wraps`]).
    pub(super) fn note_wraps(&mut self, node_id: NodeId, keys: Option<VaultDocKeys>) {
        self.docs.note_wraps(node_id, keys);
    }

    /// Remember the key this page made for a document it created, so its own
    /// next blob resolves without a round trip.
    pub(super) fn note_created(&mut self, node_id: NodeId, keys: &VaultDocKeys) {
        self.docs.note_created(node_id, keys);
    }

    /// Every document's data key as it was is forgotten: the logs they were
    /// read from are gone (`VaultStore::forget_remote_logs`), and a twin
    /// built again gives each document a new one. The scope keys stay; they
    /// are the account's, not the twin's.
    pub(super) fn forget_documents(&mut self) {
        self.docs.forget_all();
    }

    /// Forget one document's keys, for a document being pulled again from the
    /// beginning ([`DocKeys::forget`]).
    pub(super) fn forget(&mut self, node_id: NodeId) {
        self.docs.forget(node_id);
    }

    /// The key a blob whose header names `key_id` was written under, in the
    /// order the contract gives (see the struct's own documentation).
    pub(super) fn blob_key(&mut self, store_id: StoreId, node_id: NodeId, key_id: KeyId) -> Option<SymmetricKey> {
        self.docs.blob_key(store_id, node_id, key_id, &self.scope)
    }

    /// The key an outgoing blob for `node_id` is written under: its data key
    /// when it has one this page can open, and otherwise the scope key a
    /// document from before data keys is still written under. `None` when the
    /// document has a data key nothing here opens: its wraps have not been
    /// fetched, or none is under a key this account holds.
    pub(super) fn encrypt_key(&mut self, store_id: StoreId, node_id: NodeId) -> Option<(KeyId, SymmetricKey)> {
        match self.docs.seal_key(store_id, node_id, self.scope.current, &self.scope) {
            SealKey::With(key_id, key) => Some((key_id, key)),
            SealKey::Unreadable | SealKey::NoScopeKey => None,
        }
    }

    /// A data key for a document nothing has seen, wrapped under every scope
    /// key in `scope_key_ids` this page holds; a create tried again gets the
    /// first try's.
    pub(super) fn create_document_key(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        scope_key_ids: &[KeyId],
    ) -> Option<(KeyId, SymmetricKey, VaultDocKeys)> {
        self.docs.create_key(store_id, node_id, scope_key_ids, &self.scope)
    }

    /// The key a blob names, when nothing here yields it: what told a blob
    /// that will not open for want of a key from one that is simply bad.
    pub(super) fn key_wanted(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str) -> Option<KeyId> {
        self.docs.key_wanted(store_id, node_id, encoded, &self.scope)
    }

    /// Open one blob of one document. The error is the sentence the
    /// document waits with (`pimble_rpc::vault::Unopened::reason`).
    pub(super) fn decrypt_blob(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str) -> Result<Vec<u8>, String> {
        self.docs.open(store_id, node_id, encoded, &self.scope).map_err(|e| e.reason())
    }

    /// Open another device's state report of one document.
    pub(super) fn open_report(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str) -> Result<protocol::Report, String> {
        self.docs.open_report(store_id, node_id, encoded, &self.scope).map_err(|e| e.reason())
    }

    /// Open every part of a group of one document, as the one update they
    /// were cut from.
    pub(super) fn decrypt_parts(&mut self, store_id: StoreId, node_id: NodeId, parts: &[(VaultPart, String)]) -> Result<Vec<u8>, String> {
        self.docs.open_parts(store_id, node_id, parts.iter().map(|(p, b)| (*p, b.as_str())), &self.scope).map_err(|e| e.reason())
    }

    /// Everything in a fetch, snapshot first, decrypted in order: a whole
    /// blob alone, a group's parts together (`pimble_rpc::vault::fetched_units`).
    /// A group not all here is an error with no mark of its own: nothing of
    /// it applies, and the cursor stops in front of it.
    pub(super) fn decrypt_entries(
        &mut self,
        store_id: StoreId,
        node_id: NodeId,
        fetched: &VaultFetchResponse,
    ) -> Vec<(Mark, Result<Vec<u8>, String>)> {
        protocol::fetched_units(fetched)
            .into_iter()
            .map(|unit| {
                let opened = self.docs.open_unit(store_id, node_id, &unit, &self.scope).map_err(|e| e.reason());
                (unit.mark().unwrap_or(Mark::One(0)), opened)
            })
            .collect()
    }
}
