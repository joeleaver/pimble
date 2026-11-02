//! The encrypted-store protocol, once: what the desktop's vault link
//! (`pimble-server/src/vault_link.rs`) and the browser's vault client
//! (`web/src/vault.rs`) both do to a vault store's logs, as pure functions
//! over what each of them holds (docs/CRYPTO_CONTRACT.md,
//! docs/NODE_DOCUMENT_CONTRACT.md section 5 "Keys", docs/API_CONTRACT.md
//! wave 1). Transport, files, tasks and the documents themselves stay with
//! the callers.
//!
//! - **Keys** ([`DocKeys`]): every document has a data key, stored on the
//!   remote wrapped under each scope key that may read it (the store key,
//!   and each share's). A blob's header names the key it is under, and it is
//!   resolved in this order: the document's data key already unwrapped here;
//!   the document's wraps, opened with a scope key this client holds; a scope
//!   key of that id held directly, which is how a blob from before data keys,
//!   under the store key itself, still reads. A document the remote has never
//!   seen gets a data key from the client that creates it, wrapped under every
//!   scope key it holds that covers the node ([`scope_key_ids`]); those wraps
//!   ride the document's first append and the remote stores the two
//!   together, so no blob is ever there under a key it has no wrap of.
//! - **Snapshots** ([`snapshot_due`]): one rule, one number.
//! - **Echoes** ([`should_apply`]): authorship decides, never a number.
//! - **Reading a fetch** ([`fetched_entries`], [`Mark`]): snapshot first, then
//!   updates, and what each one vouches for in the [`VaultCursor`].
//!
//! The scope keys themselves come from wherever the caller keeps them (the
//! desktop's keystore, the page's keyring) through [`ScopeKeys`].

use std::collections::HashMap;

use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use pimble_core::{custom_keys, node_types, NodeId, ShareMarker, StoreId};
use pimble_crdt::{NodeFields, Tree};
use pimble_crypto::{blob_aad, dek_aad, unwrap_dek, wrap_dek, Blob, KeyId, SymmetricKey, WrappedDek};

use crate::{VaultCursor, VaultDocId, VaultDocKeys, VaultFetchResponse};

// ── Snapshots ───────────────────────────────────────────────────────────────

/// Upload a snapshot of a document once its log has grown this many entries
/// past the last snapshot (docs/CRYPTO_CONTRACT.md): a reader then replays at
/// most about this many updates, whoever appended them.
pub const SNAPSHOT_EVERY: u64 = 200;

/// Whether this client's append numbered `appended` should be followed by a
/// snapshot stamped `appended`. `snapshot_seq` is the document's snapshot as
/// last known here (from `vaultListDocs`, a fetch, or this client's own
/// upload; 0 for none).
///
/// Due when the log has grown [`SNAPSHOT_EVERY`] entries past the last
/// snapshot, counting every client's appends: the log is shared, and so is
/// what a reader has to replay and what the remote's per-log limit counts.
///
/// And only when `cursor` has read *through* `appended`. A snapshot deletes
/// every log entry at or below its number, so it may only be stamped with a
/// number this client has applied everything up to
/// ([`VaultCursor::applied_through`]). Our own append being `appended` says
/// nothing about another client's `appended - 1`, which may still be on its
/// way here; until it lands the snapshot waits for a later append.
pub fn snapshot_due(appended: u64, snapshot_seq: u64, cursor: &VaultCursor) -> bool {
    appended.saturating_sub(snapshot_seq) >= SNAPSHOT_EVERY && cursor.applied_through() == appended
}

// ── Echoes ──────────────────────────────────────────────────────────────────

/// Whether an incoming `VaultAppended` should be applied by the client whose
/// id is `my_client_id` (the `client_id` it names on its own appends).
///
/// Authorship is the only thing that can decide this, and the notification
/// carries it: a blob this client sent comes back with its own id on it, and
/// everything else is somebody else's work. A sequence number cannot stand in
/// for authorship. Several clients append to one document and the server hands
/// out its numbers in arrival order, so one client's appends are interleaved
/// with another's, and "not newer than the last number I was given" throws away
/// exactly the updates that arrived while this client was typing.
///
/// When in doubt this applies. A yrs merge is idempotent, so a genuine echo
/// applied twice changes nothing, while a dropped update is gone for good.
pub fn should_apply(source_client_id: Option<&str>, my_client_id: &str) -> bool {
    match source_client_id {
        Some(source) => source != my_client_id,
        // Unattributed: an append made by something that did not name itself,
        // or by a server that does not stamp one. Applying is the safe
        // reading: a merge repeated is nothing, a merge missed is a lost
        // edit.
        None => true,
    }
}

// ── Reading a fetch ─────────────────────────────────────────────────────────

/// What applying a fetched entry says about the log: a snapshot stands for
/// everything up to its number, an update for its own number only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Through(u64),
    One(u64),
}

impl Mark {
    /// Called only once the entry has been applied; an entry that would not
    /// decrypt or merge is never marked, so the cursor stops in front of it.
    pub fn apply(self, cursor: &mut VaultCursor) {
        match self {
            Mark::Through(seq) => cursor.mark_through(seq),
            Mark::One(seq) => cursor.mark(seq),
        }
    }

    /// The snapshot number this entry is, when it is a snapshot.
    pub fn snapshot(self) -> Option<u64> {
        match self {
            Mark::Through(seq) => Some(seq),
            Mark::One(_) => None,
        }
    }
}

/// A `vaultFetch` answer's blobs in the order they apply: the snapshot first,
/// then every update.
pub fn fetched_entries(fetched: &VaultFetchResponse) -> Vec<(Mark, &str)> {
    let mut out = Vec::with_capacity(fetched.updates.len() + 1);
    if let Some(entry) = &fetched.snapshot {
        out.push((Mark::Through(entry.seq), entry.blob.as_str()));
    }
    for entry in &fetched.updates {
        out.push((Mark::One(entry.seq), entry.blob.as_str()));
    }
    out
}

// ── Blobs ───────────────────────────────────────────────────────────────────

/// base64url without padding is what the contract specifies; the padded and
/// standard alphabets are accepted too, so a server that encodes either way
/// still reads.
pub fn decode_blob(encoded: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(encoded)
        .or_else(|_| URL_SAFE.decode(encoded))
        .or_else(|_| STANDARD.decode(encoded))
        .map_err(|e| format!("the blob would not decode: {e}"))
}

/// Encrypt one update (or a whole document, for a snapshot) of `node_id`
/// under `key`, as the wire carries it.
pub fn seal(store_id: StoreId, node_id: NodeId, key_id: KeyId, key: &SymmetricKey, plaintext: &[u8]) -> String {
    let aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
    URL_SAFE_NO_PAD.encode(Blob::encrypt(key, key_id, &aad, plaintext))
}

/// Why a blob did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unopened {
    /// Not base64, or no blob header.
    Malformed(String),
    /// No key this client holds or can unwrap is the one the header names.
    NoKey(KeyId),
    /// The named key is held and the blob does not decrypt under it.
    Failed(String),
}

impl std::fmt::Display for Unopened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unopened::Malformed(message) => write!(f, "malformed blob: {message}"),
            Unopened::NoKey(key_id) => write!(f, "no key {key_id} held here"),
            Unopened::Failed(message) => write!(f, "decryption failed: {message}"),
        }
    }
}

// ── Keys ────────────────────────────────────────────────────────────────────

/// Where the scope keys come from: the store key, each share's, by id.
pub trait ScopeKeys {
    fn scope_key(&self, key_id: &KeyId) -> Option<SymmetricKey>;
}

impl ScopeKeys for HashMap<KeyId, SymmetricKey> {
    fn scope_key(&self, key_id: &KeyId) -> Option<SymmetricKey> {
        self.get(key_id).cloned()
    }
}

/// How the next blob of a document goes out (see [`DocKeys::seal_key`]).
pub enum SealKey {
    /// Under this key: the document's data key, or, for a document from
    /// before data keys, the scope key it has always been written under.
    With(KeyId, SymmetricKey),
    /// The document has a data key and no wrap this client can open (its
    /// wraps have not been fetched, or none is under a key held here).
    Unreadable,
    /// A document from before data keys, and the scope key it would be
    /// written under is not held.
    NoScopeKey,
}

/// What a client knows of a store's documents' data keys. In memory only: a
/// data key is unwrapped on demand from the document's wraps with a scope
/// key, and is gone with the client.
#[derive(Default)]
pub struct DocKeys {
    /// Each document's wraps, as `vaultFetch` last gave them.
    wraps: HashMap<NodeId, VaultDocKeys>,
    /// The data key id `vaultListDocs` names for a document whose wraps have
    /// not been fetched yet.
    listed: HashMap<NodeId, KeyId>,
    /// Data keys unwrapped (or made) so far, with the id the blobs name.
    deks: HashMap<NodeId, (KeyId, SymmetricKey)>,
    /// The wraps this client made for a document whose create has not been
    /// confirmed: a create tried again goes out under the same key, which is
    /// the one the remote holds if the first try's answer was lost (the
    /// remote keeps the keys a document's first blob came with).
    creating: HashMap<NodeId, VaultDocKeys>,
}

impl DocKeys {
    /// `vaultListDocs` names `dek_id` as the document's data key.
    pub fn note_listed(&mut self, node_id: NodeId, dek_id: KeyId) {
        self.listed.insert(node_id, dek_id);
    }

    /// Take a `vaultFetch`'s answer about a document's data key. `None` is a
    /// document from before data keys, whose blobs name a scope key; a data
    /// key held under a different `dek_id` is a rotation and is forgotten.
    pub fn note_wraps(&mut self, node_id: NodeId, keys: Option<VaultDocKeys>) {
        match keys {
            Some(keys) => {
                if self.deks.get(&node_id).is_some_and(|(id, _)| *id != keys.dek_id) {
                    self.deks.remove(&node_id);
                }
                self.listed.insert(node_id, keys.dek_id);
                self.wraps.insert(node_id, keys);
            }
            None => {
                self.wraps.remove(&node_id);
                self.listed.remove(&node_id);
            }
        }
    }

    /// The document's create landed with `keys`: its own next blob resolves
    /// without a round trip.
    pub fn note_created(&mut self, node_id: NodeId, keys: &VaultDocKeys) {
        self.creating.remove(&node_id);
        self.note_wraps(node_id, Some(keys.clone()));
    }

    /// Hold `dek` as the document's data key (one this client just gave it).
    pub fn hold(&mut self, node_id: NodeId, dek_id: KeyId, dek: SymmetricKey) {
        self.deks.insert(node_id, (dek_id, dek));
    }

    /// The document's wraps, as the remote last gave them.
    pub fn wraps(&self, node_id: NodeId) -> Option<&VaultDocKeys> {
        self.wraps.get(&node_id)
    }

    /// The id of the document's data key, as far as the remote has said.
    pub fn dek_id(&self, node_id: NodeId) -> Option<KeyId> {
        self.wraps.get(&node_id).map(|keys| keys.dek_id).or_else(|| self.listed.get(&node_id).copied())
    }

    /// Whether a data key of the document is held unwrapped here.
    pub fn holds_data_key(&self, node_id: NodeId) -> bool {
        self.deks.contains_key(&node_id)
    }

    /// Whether the remote has named a data key for the document and its wraps
    /// are not here: a fetch is what brings them.
    pub fn wants_wraps(&self, node_id: NodeId) -> bool {
        self.dek_id(node_id).is_some() && !self.wraps.contains_key(&node_id)
    }

    /// Forget one document's keys, for a document read again from the
    /// beginning: whatever the remote says about it then is the truth,
    /// including a data key this client made for a create it refused.
    pub fn forget(&mut self, node_id: NodeId) {
        self.wraps.remove(&node_id);
        self.deks.remove(&node_id);
        self.listed.remove(&node_id);
        self.creating.remove(&node_id);
    }

    /// The remote's logs are other logs than the ones read (a twin built
    /// again gives each document a new data key): every document's key as it
    /// was is forgotten. The scope keys are the account's, not the twin's,
    /// and are not kept here.
    pub fn forget_all(&mut self) {
        self.wraps.clear();
        self.deks.clear();
        self.listed.clear();
        self.creating.clear();
    }

    /// Whether nothing is known of any document's keys.
    pub fn is_empty(&self) -> bool {
        self.wraps.is_empty() && self.deks.is_empty() && self.listed.is_empty() && self.creating.is_empty()
    }

    /// The key a blob of `node_id` whose header names `key_id` reads with, in
    /// the module's order: the document's data key when the id names it
    /// (unwrapped already, or now with whichever held scope key opens one of
    /// its wraps), else a scope key of that id held directly.
    pub fn blob_key(&mut self, store_id: StoreId, node_id: NodeId, key_id: KeyId, scope: &impl ScopeKeys) -> Option<SymmetricKey> {
        if let Some(key) = self.data_key(store_id, node_id, key_id, scope) {
            return Some(key);
        }
        // A blob from before data keys names a scope key itself.
        scope.scope_key(&key_id)
    }

    /// The document's data key `dek_id` only, never a scope key of that id:
    /// what wrapping and sealing under a data key ask for.
    pub fn data_key(&mut self, store_id: StoreId, node_id: NodeId, dek_id: KeyId, scope: &impl ScopeKeys) -> Option<SymmetricKey> {
        if let Some((id, key)) = self.deks.get(&node_id) {
            if *id == dek_id {
                return Some(key.clone());
            }
        }
        let keys = self.wraps.get(&node_id).filter(|keys| keys.dek_id == dek_id)?;
        let aad = dek_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
        for wrap in &keys.wraps {
            let Some(scope_key) = scope.scope_key(&wrap.scope_key_id) else { continue };
            match unwrap_dek(wrap, &scope_key, &aad) {
                Ok(dek) => {
                    self.deks.insert(node_id, (dek_id, dek.clone()));
                    return Some(dek);
                }
                Err(e) => tracing::warn!("Store {} document {}: the wrap under scope key {} did not open ({})", store_id, node_id, wrap.scope_key_id, e),
            }
        }
        None
    }

    /// The key an outgoing blob of a document the remote holds is written
    /// under: its data key when it has one, and otherwise `keyless`, the scope
    /// key a document from before data keys is still written under (giving it
    /// a data key is its owner's business, not a member's).
    pub fn seal_key(&mut self, store_id: StoreId, node_id: NodeId, keyless: Option<KeyId>, scope: &impl ScopeKeys) -> SealKey {
        match self.dek_id(node_id) {
            Some(dek_id) => match self.data_key(store_id, node_id, dek_id, scope) {
                Some(key) => SealKey::With(dek_id, key),
                None => SealKey::Unreadable,
            },
            None => match keyless.and_then(|id| scope.scope_key(&id).map(|key| (id, key))) {
                Some((id, key)) => SealKey::With(id, key),
                None => SealKey::NoScopeKey,
            },
        }
    }

    /// A data key for a document the remote has never seen, wrapped under
    /// every scope key in `scope_key_ids` that is held. A create being tried
    /// again gets the same key as the first try. `None` when no scope key in
    /// the list is held.
    pub fn create_key(&mut self, store_id: StoreId, node_id: NodeId, scope_key_ids: &[KeyId], scope: &impl ScopeKeys) -> Option<(KeyId, SymmetricKey, VaultDocKeys)> {
        if let (Some(keys), Some((dek_id, dek))) = (self.creating.get(&node_id), self.deks.get(&node_id)) {
            if keys.dek_id == *dek_id {
                return Some((*dek_id, dek.clone(), keys.clone()));
            }
        }
        let (dek_id, dek, keys) = new_data_key(store_id, node_id, scope_key_ids, scope)?;
        self.deks.insert(node_id, (dek_id, dek.clone()));
        self.creating.insert(node_id, keys.clone());
        Some((dek_id, dek, keys))
    }

    /// The key id a blob names, when nothing here yields that key: what tells
    /// a blob that will not open for want of a key from one that is simply
    /// bad.
    pub fn key_wanted(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str, scope: &impl ScopeKeys) -> Option<KeyId> {
        let blob = decode_blob(encoded).ok()?;
        let key_id = Blob::key_id(&blob).ok()?;
        self.blob_key(store_id, node_id, key_id, scope).is_none().then_some(key_id)
    }

    /// Open one blob of one document.
    pub fn open(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str, scope: &impl ScopeKeys) -> Result<Vec<u8>, Unopened> {
        let blob = decode_blob(encoded).map_err(Unopened::Malformed)?;
        let key_id = Blob::key_id(&blob).map_err(|e| Unopened::Malformed(e.to_string()))?;
        let key = self.blob_key(store_id, node_id, key_id, scope).ok_or(Unopened::NoKey(key_id))?;
        let aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
        Blob::decrypt(&key, &aad, &blob).map_err(|e| Unopened::Failed(e.to_string()))
    }
}

/// A fresh data key for `node_id`, wrapped under every scope key in
/// `scope_key_ids` that is held (each once). `None` when none is.
pub fn new_data_key(store_id: StoreId, node_id: NodeId, scope_key_ids: &[KeyId], scope: &impl ScopeKeys) -> Option<(KeyId, SymmetricKey, VaultDocKeys)> {
    let dek = SymmetricKey::generate();
    let dek_id = KeyId::new_v4();
    let aad = dek_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
    let mut wraps: Vec<WrappedDek> = Vec::new();
    for scope_key_id in scope_key_ids {
        if wraps.iter().any(|w| w.scope_key_id == *scope_key_id) {
            continue;
        }
        if let Some(scope_key) = scope.scope_key(scope_key_id) {
            wraps.push(wrap_dek(&dek, &scope_key, *scope_key_id, &aad));
        }
    }
    if wraps.is_empty() {
        return None;
    }
    Some((dek_id, dek, VaultDocKeys { dek_id, wraps }))
}

/// The share's marker a node carries, when it is a share's root
/// (`pimble_core::NodeMetadata::share`, read from a document's fields).
pub fn share_marker(fields: &NodeFields) -> Option<ShareMarker> {
    fields.custom.get(custom_keys::SHARE).and_then(|value| serde_json::from_value::<ShareMarker>(value.clone()).ok())
}

/// The scope keys a new document's data key is wrapped under: `store_key`
/// (the store key, when this client holds the whole store), and the key of
/// every share the node sits under, which each share's root names in its own
/// marker. An attachment's place is its host (docs/DISCUSSIONS_CONTRACT.md
/// section 1): it takes the keys its host has.
///
/// With neither (a scoped member, and no marker on the way up: a share from
/// before markers, or a root whose document has not arrived), `fallback`: the
/// keys the member was given, which are its shares'.
pub fn scope_key_ids(tree: &Tree, node_id: NodeId, store_key: Option<KeyId>, fallback: impl IntoIterator<Item = KeyId>) -> Vec<KeyId> {
    let mut ids: Vec<KeyId> = store_key.into_iter().collect();
    let mut cur = Some(node_id);
    for _ in 0..=tree.ids().len() {
        let Some(fields) = cur.and_then(|id| tree.doc(id)).and_then(|doc| doc.fields().ok()) else { break };
        if let Some(marker) = share_marker(&fields) {
            ids.push(marker.key_id);
        }
        cur = fields.parent_id.or(fields.attached_to);
    }
    if ids.is_empty() {
        ids.extend(fallback);
    }
    let mut seen = Vec::with_capacity(ids.len());
    ids.retain(|id| {
        let new = !seen.contains(id);
        seen.push(*id);
        new
    });
    ids
}

/// What a document's first append names besides its blob and keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NewDocument {
    /// The node's place: its parent, or an attachment's host. What admits a
    /// scoped member's create (the new id joins every scope of the member's
    /// that holds it), and what is ignored for anyone else's.
    pub parent_id: Option<NodeId>,
    /// A discussion on a node says so, naming its host: what lets a commenter
    /// create it and puts it in the store's discussion set
    /// (docs/DISCUSSIONS_CONTRACT.md section 7). A standalone discussion node
    /// is a node like any other.
    pub discussion_host: Option<NodeId>,
}

impl NewDocument {
    pub fn of(tree: &Tree, node_id: NodeId) -> Self {
        let Some(fields) = tree.doc(node_id).and_then(|doc| doc.fields().ok()) else { return Self::default() };
        let parent_id = fields.parent_id.or(fields.attached_to);
        let discussion = fields.node_type == node_types::DISCUSSION && fields.attached_to.is_some();
        Self { parent_id, discussion_host: parent_id.filter(|_| discussion) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VaultEntry;

    fn scope_with(id: KeyId) -> (HashMap<KeyId, SymmetricKey>, SymmetricKey) {
        let key = SymmetricKey::generate();
        (HashMap::from([(id, key.clone())]), key)
    }

    #[test]
    fn a_snapshot_is_due_by_log_length_and_only_once_read_through() {
        let mut cursor = VaultCursor::starting_at(0);
        for seq in 1..SNAPSHOT_EVERY {
            cursor.mark(seq);
            assert!(!snapshot_due(seq, 0, &cursor));
        }
        // Our own append, with somebody else's just below it still on its way.
        cursor.mark(SNAPSHOT_EVERY + 1);
        assert!(!snapshot_due(SNAPSHOT_EVERY + 1, 0, &cursor));
        cursor.mark(SNAPSHOT_EVERY);
        assert!(snapshot_due(SNAPSHOT_EVERY + 1, 0, &cursor));
        // Counted from the last snapshot, whoever made it.
        assert!(!snapshot_due(SNAPSHOT_EVERY + 1, 2, &cursor));
    }

    #[test]
    fn a_client_skips_only_its_own_work() {
        assert!(!should_apply(Some("me"), "me"));
        assert!(should_apply(Some("someone-else"), "me"));
        assert!(should_apply(None, "me"));
    }

    #[test]
    fn a_fetch_applies_snapshot_first_and_marks_what_each_entry_vouches_for() {
        let fetched = VaultFetchResponse {
            snapshot: Some(VaultEntry { seq: 5, blob: "s".into() }),
            updates: vec![VaultEntry { seq: 7, blob: "u".into() }],
            head: 7,
            keys: None,
        };
        let entries = fetched_entries(&fetched);
        assert_eq!(entries, vec![(Mark::Through(5), "s"), (Mark::One(7), "u")]);
        let mut cursor = VaultCursor::default();
        for (mark, _) in entries {
            mark.apply(&mut cursor);
        }
        assert_eq!(cursor.applied_through(), 5, "6 has not arrived");
    }

    #[test]
    fn a_blob_s_key_is_the_document_s_data_key_then_a_scope_key() {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        let scope_id = KeyId::new_v4();
        let (scope, scope_key) = scope_with(scope_id);
        let mut keys = DocKeys::default();

        // Before data keys: under the scope key itself.
        let old = seal(store_id, node_id, scope_id, &scope_key, b"old");
        assert_eq!(keys.open(store_id, node_id, &old, &scope).unwrap(), b"old");
        assert!(matches!(keys.seal_key(store_id, node_id, Some(scope_id), &scope), SealKey::With(id, _) if id == scope_id));

        // Given a data key: its wraps open with the scope key.
        let (dek_id, dek, wraps) = new_data_key(store_id, node_id, &[scope_id], &scope).unwrap();
        keys.note_wraps(node_id, Some(wraps));
        let new = seal(store_id, node_id, dek_id, &dek, b"new");
        assert_eq!(keys.open(store_id, node_id, &new, &scope).unwrap(), b"new");
        assert_eq!(keys.open(store_id, node_id, &old, &scope).unwrap(), b"old", "old blobs still read");
        assert!(matches!(keys.seal_key(store_id, node_id, Some(scope_id), &scope), SealKey::With(id, _) if id == dek_id));

        // Another document's blob does not open under this one's key.
        assert!(keys.open(store_id, NodeId::new(), &new, &scope).is_err());

        // Listed with a data key nothing here opens: shut, both ways.
        let shut = NodeId::new();
        keys.note_listed(shut, KeyId::new_v4());
        assert!(keys.wants_wraps(shut));
        assert!(matches!(keys.seal_key(store_id, shut, Some(scope_id), &scope), SealKey::Unreadable));
        let other = SymmetricKey::generate();
        let other_id = KeyId::new_v4();
        let blob = seal(store_id, shut, other_id, &other, b"x");
        assert_eq!(keys.open(store_id, shut, &blob, &scope), Err(Unopened::NoKey(other_id)));
        assert_eq!(keys.key_wanted(store_id, shut, &blob, &scope), Some(other_id));
        assert!(matches!(keys.open(store_id, shut, "not a blob!", &scope), Err(Unopened::Malformed(_))));
    }

    #[test]
    fn a_create_tried_again_goes_out_under_the_same_key() {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        let scope_id = KeyId::new_v4();
        let (scope, _) = scope_with(scope_id);
        let mut keys = DocKeys::default();
        let (first, _, wraps) = keys.create_key(store_id, node_id, &[scope_id, scope_id, KeyId::new_v4()], &scope).unwrap();
        assert_eq!(wraps.wraps.len(), 1, "once per held scope key");
        let (again, _, _) = keys.create_key(store_id, node_id, &[scope_id], &scope).unwrap();
        assert_eq!(first, again);
        keys.note_created(node_id, &wraps);
        assert!(matches!(keys.seal_key(store_id, node_id, None, &scope), SealKey::With(id, _) if id == first));
        assert!(keys.create_key(store_id, NodeId::new(), &[KeyId::new_v4()], &scope).is_none(), "nothing held, nothing made");
    }
}
