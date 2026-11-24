//! The shares in a whole store, and a twin built again.

use std::collections::HashSet;

use pimble_core::{LeftShare, NodeId, StoreId};
use pimble_crdt::{DocShape, Tree};
use pimble_crypto::KeyId;
use pimble_rpc::vault as protocol;

use crate::keys::{self, KeyError, Keyring};

use super::{AccountStore, VaultStore, KEY_RETRY_MS, SHARE_KEY_FLOOR_MS};
use super::helpers::{now_ms, save_answered};

impl VaultStore {
    // ── The shares in a whole store ─────────────────────────────────────────

    /// The shared nodes whose key this page does not hold: every document it
    /// holds of a whole store whose `node` root carries a share's marker
    /// naming a key that is not in the keyring. Tombstones too: a deleted
    /// folder's share may still be the only thing a member's document is
    /// wrapped under. Nothing for a member's page, which holds its grants'
    /// keys and is owed no others.
    pub(super) fn missing_share_roots(&self) -> Vec<NodeId> {
        if self.is_partial() {
            return Vec::new();
        }
        let mut roots: Vec<NodeId> = self
            .tree
            .ids()
            .into_iter()
            .filter(|id| {
                self.tree
                    .doc(*id)
                    .and_then(|doc| doc.fields().ok())
                    .and_then(|fields| protocol::share_marker(&fields))
                    .is_some_and(|marker| self.keys.scope.get(&marker.key_id).is_none())
            })
            .collect();
        roots.sort_by_key(|id| id.to_string());
        roots
    }

    /// The shared roots to ask the accounts service about now, or `None`:
    /// nothing is missing, or the last ask was under [`SHARE_KEY_FLOOR_MS`]
    /// ago, in which case the look is put off until the floor has passed.
    /// Answering `Some` records the ask.
    pub(super) fn share_keys_to_ask_for(&mut self, now: f64) -> Option<Vec<NodeId>> {
        let missing = self.missing_share_roots();
        if missing.is_empty() {
            return None;
        }
        if let Some(asked_at) = self.look.asked_keys_at {
            if now - asked_at < SHARE_KEY_FLOOR_MS {
                self.look.want_at(asked_at + SHARE_KEY_FLOOR_MS);
                return None;
            }
        }
        self.look.asked_keys_at = Some(now);
        Some(missing)
    }

    /// Take more scope keys into the keyring; whether any of them is new. A
    /// new key may open what has been asked about before, so that is
    /// forgotten.
    pub(super) fn take_keys(&mut self, more: Keyring) -> bool {
        let new = self.keys.scope.absorb(more);
        if new {
            self.look.asked.clear();
        }
        new
    }

    // ── A twin built again ──────────────────────────────────────────────────

    /// Take the `epoch` a `vaultListDocs` answered with: which logs the
    /// server's sequence numbers belong to. Whether they turned out to be
    /// other logs than the ones this page read, in which case everything
    /// about the old ones has just been forgotten
    /// ([`VaultStore::forget_remote_logs`]).
    ///
    /// A store served from its owner's computer keeps its twin there, derived
    /// and disposable (docs/RELAY_CONTRACT.md): deleted, the owner's link
    /// builds it again, and every log starts again from 1. A page that kept
    /// its cursors would read each new log from wherever it had read the old
    /// one to, and skip what lies before. The first epoch heard is simply
    /// remembered, and a server that names none concludes nothing, as on the
    /// desktop (`full_reconcile` in `crates/pimble-server/src/vault_link.rs`).
    pub(super) fn take_epoch(&mut self, epoch: Option<&str>, row: Option<&AccountStore>) -> bool {
        let Some(epoch) = epoch else { return false };
        let rebuilt = self.epoch.as_deref().is_some_and(|read| read != epoch);
        if rebuilt {
            self.forget_remote_logs(row);
        }
        self.epoch = Some(epoch.to_string());
        rebuilt
    }

    /// The server's logs are other logs than the ones this page read: where
    /// each was read to, how many appends it has had, what the server was
    /// known to hold of each document and the data keys of the documents as
    /// they were are all forgotten, as the desktop's link forgets them
    /// (`Progress::forget_remote_logs`). The documents themselves stay: they
    /// are this page's copy of the notes, not of the logs.
    ///
    /// What follows (`VaultClient::catch_up`) is every document read from the
    /// start (a merge repeated is nothing), which is also what says what the
    /// new logs hold, and then every document this account may write offered
    /// again: a diff from there, so what the server already has merges to
    /// nothing, and what only this page held is not lost with the old twin.
    /// One it may only read is never offered: the server would refuse it, and
    /// a refused append throws the local copy away to read the server's.
    fn forget_remote_logs(&mut self, row: Option<&AccountStore>) {
        let held: Vec<NodeId> = self.tree.ids();
        let writable: HashSet<NodeId> = held.iter().copied().filter(|id| self.may_write(*id, row)).collect();
        self.docs.clear();
        for node_id in held {
            let doc = self.docs.entry(node_id).or_default();
            doc.unsent = writable.contains(&node_id);
        }
        self.heads.clear();
        self.keys.forget_documents();
        self.look.unread.clear();
        self.look.asked.clear();
        self.reported.clear();
        self.answered.forget();
        save_answered(self.listed.id, &self.answered);
    }
}

/// The key a document's share marker names, when it carries one.
pub(super) fn share_key_id(shape: &DocShape) -> Option<KeyId> {
    shape.fields.as_ref().and_then(protocol::share_marker).map(|marker| marker.key_id)
}

/// `roots` (nearest first, from [`pimble_crdt::Tree::shares`] or
/// [`pimble_crdt::Tree::shares_left`]) resolved to what the move notice and
/// "Recently Deleted..." show: each root's [`pimble_core::ShareMarker`] name,
/// or its title when the marker does not parse (docs/MOVE_CONTRACT.md
/// "Seeing and undoing what was removed").
pub(super) fn as_left_shares(tree: &Tree, roots: Vec<NodeId>) -> Vec<LeftShare> {
    roots
        .into_iter()
        .map(|root| {
            let name = tree
                .doc(root)
                .and_then(|doc| doc.fields().ok())
                .map(|fields| protocol::share_marker(&fields).map(|marker| marker.name).unwrap_or(fields.title))
                .unwrap_or_default();
            LeftShare { root, name }
        })
        .collect()
}

/// Ask the accounts service for the keys of the shares in a whole store that
/// this page has seen a marker of and holds no key for, and take what comes
/// into the keyring. Whether a key came that was not held.
///
/// `GET /stores/{id}/keys?root=<node>` answers the caller's own envelopes for
/// that root, and a whole-store grant may ask about any root. An owner has
/// one for every share: the device that made the share sealed its key to the
/// owner's own account before it wrote the marker. Nothing there
/// ([`KeyError::NoneYet`]) is normal for anyone else who holds the whole store
/// and is quiet; a request that failed is asked again like a waiting store's.
///
/// Never for a member's page, and never more often than
/// [`SHARE_KEY_FLOOR_MS`] per store: [`VaultStore::share_keys_to_ask_for`].
pub(super) async fn fetch_share_keys(store_id: StoreId, store: &mut VaultStore) -> bool {
    let now = now_ms();
    let Some(roots) = store.share_keys_to_ask_for(now) else { return false };
    match keys::fetch_scope_keyring(&store_id.to_string(), &roots).await {
        Ok(more) => {
            let new = store.take_keys(more);
            if new {
                tracing::info!("Store {}: holding the key of a share in it that was not held", store_id);
            }
            new
        }
        Err(KeyError::NoneYet) => false,
        Err(KeyError::Failed(message)) => {
            tracing::warn!("Asking for the keys of the shares in {} failed: {}", store_id, message);
            store.look.want_at(now + KEY_RETRY_MS);
            false
        }
    }
}
