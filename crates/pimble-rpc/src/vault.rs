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
//! - **Reading a fetch** ([`fetched_units`], [`Mark`]): snapshot first, then
//!   updates, and what each one vouches for in the [`VaultCursor`].
//! - **Sizes and parts** (docs/SYNC_LIMITS_CONTRACT.md): a document is
//!   compacted when its log outgrows its snapshot ([`snapshot_due_by`]), and a
//!   snapshot or an append larger than [`PART_BYTES`] goes as parts
//!   ([`seal_parts`]), each its own blob, applied only once every part of its
//!   group is here ([`fetched_units`], [`PartBuffer`]).
//!
//! The scope keys themselves come from wherever the caller keeps them (the
//! desktop's keystore, the page's keyring) through [`ScopeKeys`].

use std::collections::HashMap;

use uuid::Uuid;

use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use pimble_core::{custom_keys, node_types, BlobId, NodeId, ShareMarker, StoreId};
use pimble_crdt::{NodeFields, Tree};
use pimble_crypto::{blob_aad, blob_part_aad, dek_aad, unwrap_dek, wrap_dek, Blob, CryptoError, KeyId, SymmetricKey, WrappedDek};

use crate::{VaultAppendResponse, VaultCursor, VaultDocId, VaultDocKeys, VaultFetchResponse, VaultPart};

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
    snapshot_due_by(appended, snapshot_seq, cursor, None)
}

/// The least a log grows past its snapshot before its size alone makes a
/// snapshot due (docs/SYNC_LIMITS_CONTRACT.md, "Compaction by size").
pub const COMPACT_FLOOR: u64 = 1024 * 1024;

/// How large a document's log and its snapshot are on the remote, as its
/// `vaultAppend` answered (`log_bytes`, `snapshot_bytes`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogSizes {
    pub log_bytes: u64,
    pub snapshot_bytes: u64,
}

impl LogSizes {
    /// What an append's answer says, when it says it: an older server does
    /// not, and the count rule applies alone.
    pub fn of(answer: &VaultAppendResponse) -> Option<Self> {
        Some(Self { log_bytes: answer.log_bytes?, snapshot_bytes: answer.snapshot_bytes.unwrap_or(0) })
    }

    /// The log has grown, since the last snapshot (a snapshot empties the log
    /// of what it covers), past twice that snapshot's size and past
    /// [`COMPACT_FLOOR`]: a few hundred large appends (paint batches) fill a
    /// log long before two hundred of them.
    pub fn compaction_due(&self) -> bool {
        self.log_bytes > (2 * self.snapshot_bytes).max(COMPACT_FLOOR)
    }
}

/// [`snapshot_due`] with the remote's sizes when it gave them: due by the
/// count rule or by size ([`LogSizes::compaction_due`]), and in either case
/// only when `cursor` has read through `appended` (the same `applied_through`
/// rule: a snapshot deletes every entry at or below its number).
pub fn snapshot_due_by(appended: u64, snapshot_seq: u64, cursor: &VaultCursor, sizes: Option<LogSizes>) -> bool {
    let by_count = appended.saturating_sub(snapshot_seq) >= SNAPSHOT_EVERY;
    let by_size = sizes.is_some_and(|sizes| sizes.compaction_due());
    (by_count || by_size) && cursor.applied_through() == appended
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
    /// The consecutive entries of one append sent in parts, first to last:
    /// applied together, so marked together.
    Span(u64, u64),
}

impl Mark {
    /// Called only once the entry has been applied; an entry that would not
    /// decrypt or merge is never marked, so the cursor stops in front of it.
    pub fn apply(self, cursor: &mut VaultCursor) {
        match self {
            Mark::Through(seq) => cursor.mark_through(seq),
            Mark::One(seq) => cursor.mark(seq),
            Mark::Span(first, last) => {
                for seq in first..=last {
                    cursor.mark(seq);
                }
            }
        }
    }

    /// The snapshot number this entry is, when it is a snapshot.
    pub fn snapshot(self) -> Option<u64> {
        match self {
            Mark::Through(seq) => Some(seq),
            Mark::One(_) | Mark::Span(..) => None,
        }
    }
}

// ── Parts (docs/SYNC_LIMITS_CONTRACT.md, "Snapshots and appends in parts") ───

/// The most plaintext one blob carries. A snapshot or an append larger than
/// this goes as parts of at most this much each, every part a blob of its
/// own: no vault limit on one blob is ever what stops a document syncing.
pub const PART_BYTES: usize = 8 * 1024 * 1024;

/// One blob ready to go: whole, or one part of a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub blob: String,
    pub part: Option<VaultPart>,
}

/// Encrypt `plaintext` of `node_id` under `key` as the wire carries it: one
/// whole blob when it fits in [`PART_BYTES`] or `in_parts` is false (the
/// remote does not take parts, `VaultListDocsResponse::parts`), and
/// otherwise parts of one new group, each sealed with associated data naming
/// it ([`pimble_crypto::blob_part_aad`]).
pub fn seal_parts(store_id: StoreId, node_id: NodeId, key_id: KeyId, key: &SymmetricKey, plaintext: &[u8], in_parts: bool) -> Vec<Sealed> {
    if !in_parts || plaintext.len() <= PART_BYTES {
        return vec![Sealed { blob: seal(store_id, node_id, key_id, key, plaintext), part: None }];
    }
    let group = Uuid::new_v4();
    let chunks: Vec<&[u8]> = plaintext.chunks(PART_BYTES).collect();
    let count = chunks.len() as u32;
    let doc = VaultDocId::Node(node_id).as_str();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let part = VaultPart { index: index as u32, count, group };
            let aad = blob_part_aad(&store_id.to_string(), &doc, part.index, part.count, &group.to_string());
            Sealed { blob: URL_SAFE_NO_PAD.encode(Blob::encrypt(key, key_id, &aad, chunk)), part: Some(part) }
        })
        .collect()
}

/// What a fetched document's log is to a reader, in the order it applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit<'a> {
    /// A whole blob.
    Whole { mark: Mark, blob: &'a str },
    /// Every part of one group, in order: applied as one.
    Parts { mark: Mark, parts: Vec<(VaultPart, &'a str)> },
    /// Parts of a group that is not all here (or not in order): nothing of it
    /// is applied, and the cursor stops in front of it.
    Incomplete,
}

/// Every part of one group, in order and consecutive, from `entries`; `None`
/// when they are not exactly that.
fn whole_group<'a>(entries: &[&'a crate::VaultEntry]) -> Option<Vec<(VaultPart, &'a str)>> {
    let first = entries.first()?.part?;
    if !first.is_valid() || entries.len() != first.count as usize {
        return None;
    }
    let mut out = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let part = entry.part?;
        if part.group != first.group || part.count != first.count || part.index as usize != index {
            return None;
        }
        out.push((part, entry.blob.as_str()));
    }
    Some(out)
}

/// A `vaultFetch` answer as what applies: the snapshot first (whole, or every
/// one of its parts), then every update, the parts of an append sent in
/// parts together as one unit.
pub fn fetched_units(fetched: &VaultFetchResponse) -> Vec<Unit<'_>> {
    let mut out = Vec::new();
    if let Some(snapshot) = &fetched.snapshot {
        let mark = Mark::Through(snapshot.seq);
        if snapshot.part.is_none() && fetched.snapshot_parts.is_empty() {
            out.push(Unit::Whole { mark, blob: snapshot.blob.as_str() });
        } else {
            let all: Vec<&crate::VaultEntry> = std::iter::once(snapshot).chain(fetched.snapshot_parts.iter()).collect();
            out.push(match whole_group(&all) {
                Some(parts) => Unit::Parts { mark, parts },
                None => Unit::Incomplete,
            });
        }
    }
    let mut i = 0;
    let updates = &fetched.updates;
    while i < updates.len() {
        let entry = &updates[i];
        let Some(part) = entry.part else {
            out.push(Unit::Whole { mark: Mark::One(entry.seq), blob: entry.blob.as_str() });
            i += 1;
            continue;
        };
        // The run of this group's entries.
        let mut j = i;
        while j < updates.len() && updates[j].part.is_some_and(|p| p.group == part.group) {
            j += 1;
        }
        let run: Vec<&crate::VaultEntry> = updates[i..j].iter().collect();
        let consecutive = run.windows(2).all(|w| w[1].seq == w[0].seq + 1);
        out.push(match whole_group(&run).filter(|_| consecutive) {
            Some(parts) => Unit::Parts { mark: Mark::Span(run[0].seq, run[run.len() - 1].seq), parts },
            None => Unit::Incomplete,
        });
        i = j;
    }
    out
}

/// The parts of appends sent in parts, as they arrive one notification at a
/// time, kept until every part of a group is here.
#[derive(Debug, Default)]
pub struct PartBuffer {
    groups: HashMap<(String, Uuid), Vec<Option<(u64, String)>>>,
}

impl PartBuffer {
    /// Take one part of `doc`. When it completes its group: what the group
    /// vouches for and its parts in order. A part whose numbers make no sense
    /// is dropped (the document is read again by its next fetch).
    pub fn take(&mut self, doc: &str, seq: u64, part: VaultPart, blob: String) -> Option<(Mark, Vec<(VaultPart, String)>)> {
        if !part.is_valid() {
            return None;
        }
        let slots = self.groups.entry((doc.to_string(), part.group)).or_insert_with(|| vec![None; part.count as usize]);
        if slots.len() != part.count as usize {
            self.groups.remove(&(doc.to_string(), part.group));
            return None;
        }
        slots[part.index as usize] = Some((seq, blob));
        if slots.iter().any(Option::is_none) {
            return None;
        }
        let slots = self.groups.remove(&(doc.to_string(), part.group))?;
        let seqs: Vec<u64> = slots.iter().flatten().map(|(seq, _)| *seq).collect();
        let (first, last) = (*seqs.iter().min()?, *seqs.iter().max()?);
        if last - first + 1 != seqs.len() as u64 {
            return None;
        }
        let parts = slots.into_iter().flatten().enumerate().map(|(index, (_, blob))| (VaultPart { index: index as u32, ..part }, blob)).collect();
        Some((Mark::Span(first, last), parts))
    }

    /// Whether some group of `doc` is still waiting for a part.
    pub fn holds(&self, doc: &str) -> bool {
        self.groups.keys().any(|(d, _)| d == doc)
    }

    /// Forget the parts held of one document (it is being read again from
    /// its cursor, which brings every group whole).
    pub fn forget(&mut self, doc: &str) {
        self.groups.retain(|(d, _), _| d != doc);
    }

    /// Forget every part held (a connection ended: what it was in the middle
    /// of is read again by the next fetch).
    pub fn clear(&mut self) {
        self.groups.clear();
    }
}

/// A `vaultFetch` answer's blobs in the order they apply: the snapshot first,
/// then every update, each blob alone. For a picture's document, whose
/// chunks are never sent in parts; a node document is read with
/// [`fetched_units`].
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
    /// Written in a format newer than this client reads (a blob version it
    /// does not know): made by a newer Pimble.
    Newer,
    /// Parts of a group that is not all here.
    Incomplete,
}

impl Unopened {
    /// Why the document waits, as the sentence the app and the MCP show
    /// (docs/SYNC_LIMITS_CONTRACT.md, "Saying what waits").
    pub fn reason(&self) -> String {
        match self {
            Unopened::NoKey(_) => WAITING_FOR_KEY.to_string(),
            Unopened::Newer => MADE_BY_NEWER.to_string(),
            Unopened::Incomplete => "Part of a large change to it has not arrived yet.".to_string(),
            Unopened::Malformed(message) | Unopened::Failed(message) => format!("A change to it would not open here ({message})."),
        }
    }
}

/// A document waiting for a key this device does not hold.
pub const WAITING_FOR_KEY: &str = "Waiting for its key: no key on this device opens it yet.";
/// A document written in a format this client does not read.
pub const MADE_BY_NEWER: &str = "Made by a newer Pimble: update Pimble on this device to read it.";
/// An update the remote would not take for its size, from a remote that
/// takes nothing in parts.
pub const TOO_LARGE_FOR_REMOTE: &str = "Too large for the server it syncs with, which needs a newer Pimble to take it in parts.";

/// A blob's header, as an error that tells a newer format from damage.
fn header_key_id(blob: &[u8]) -> Result<KeyId, Unopened> {
    Blob::key_id(blob).map_err(|e| match e {
        CryptoError::UnsupportedVersion(_) => Unopened::Newer,
        other => Unopened::Malformed(other.to_string()),
    })
}

impl std::fmt::Display for Unopened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unopened::Malformed(message) => write!(f, "malformed blob: {message}"),
            Unopened::NoKey(key_id) => write!(f, "no key {key_id} held here"),
            Unopened::Failed(message) => write!(f, "decryption failed: {message}"),
            Unopened::Newer => f.write_str("made by a newer Pimble"),
            Unopened::Incomplete => f.write_str("parts of it have not all arrived"),
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
        let aad = blob_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str());
        self.open_with(store_id, node_id, encoded, &aad, scope)
    }

    /// Open one part of a group of one document.
    pub fn open_part(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str, part: VaultPart, scope: &impl ScopeKeys) -> Result<Vec<u8>, Unopened> {
        let aad = blob_part_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str(), part.index, part.count, &part.group.to_string());
        self.open_with(store_id, node_id, encoded, &aad, scope)
    }

    /// Open every part of a group, in order, as the one plaintext they were
    /// cut from.
    pub fn open_parts<'a>(&mut self, store_id: StoreId, node_id: NodeId, parts: impl IntoIterator<Item = (VaultPart, &'a str)>, scope: &impl ScopeKeys) -> Result<Vec<u8>, Unopened> {
        let mut out = Vec::new();
        for (part, encoded) in parts {
            out.extend(self.open_part(store_id, node_id, encoded, part, scope)?);
        }
        Ok(out)
    }

    /// Open one unit of a fetch ([`fetched_units`]).
    pub fn open_unit(&mut self, store_id: StoreId, node_id: NodeId, unit: &Unit<'_>, scope: &impl ScopeKeys) -> Result<Vec<u8>, Unopened> {
        match unit {
            Unit::Whole { blob, .. } => self.open(store_id, node_id, blob, scope),
            Unit::Parts { parts, .. } => self.open_parts(store_id, node_id, parts.iter().map(|(p, b)| (*p, *b)), scope),
            Unit::Incomplete => Err(Unopened::Incomplete),
        }
    }

    fn open_with(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str, aad: &[u8], scope: &impl ScopeKeys) -> Result<Vec<u8>, Unopened> {
        let blob = decode_blob(encoded).map_err(Unopened::Malformed)?;
        let key_id = header_key_id(&blob)?;
        let key = self.blob_key(store_id, node_id, key_id, scope).ok_or(Unopened::NoKey(key_id))?;
        Blob::decrypt(&key, aad, &blob).map_err(|e| Unopened::Failed(e.to_string()))
    }
}

impl Unit<'_> {
    /// What applying this unit vouches for; `None` for one that cannot apply.
    pub fn mark(&self) -> Option<Mark> {
        match self {
            Unit::Whole { mark, .. } | Unit::Parts { mark, .. } => Some(*mark),
            Unit::Incomplete => None,
        }
    }

    /// The key id the unit's (first) blob names, when nothing here yields
    /// it (see [`DocKeys::key_wanted`]).
    pub fn first_blob(&self) -> Option<&str> {
        match self {
            Unit::Whole { blob, .. } => Some(blob),
            Unit::Parts { parts, .. } => parts.first().map(|(_, b)| *b),
            Unit::Incomplete => None,
        }
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

// ── Pictures (docs/IMAGES_CONTRACT.md "Replication") ─────────────────────────

/// How much of a picture one entry of its document carries, before
/// encryption: under the vault's 4 MiB per blob and the links' 3 MiB catch-up
/// size, with room for the header and the cipher's overhead.
pub const PICTURE_CHUNK: usize = 2 * 1024 * 1024;

const PICTURE_MAGIC: &[u8; 4] = b"PBLC";
const PICTURE_VERSION: u8 = 1;

/// One entry of a picture's document, decrypted: which picture it is part
/// of (its type and whole length) and where its bytes go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureChunk {
    pub mime: String,
    pub total: u64,
    pub offset: u64,
    pub bytes: Vec<u8>,
}

impl PictureChunk {
    pub fn encode(&self) -> Vec<u8> {
        let mime = self.mime.as_bytes();
        let mut out = Vec::with_capacity(4 + 1 + 1 + mime.len() + 16 + self.bytes.len());
        out.extend_from_slice(PICTURE_MAGIC);
        out.push(PICTURE_VERSION);
        out.push(mime.len().min(255) as u8);
        out.extend_from_slice(&mime[..mime.len().min(255)]);
        out.extend_from_slice(&self.total.to_le_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.bytes);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 6 || &bytes[..4] != PICTURE_MAGIC || bytes[4] != PICTURE_VERSION {
            return None;
        }
        let mime_len = bytes[5] as usize;
        let rest = bytes.get(6..)?;
        let mime = std::str::from_utf8(rest.get(..mime_len)?).ok()?.to_string();
        let rest = rest.get(mime_len..)?;
        let total = u64::from_le_bytes(rest.get(..8)?.try_into().ok()?);
        let offset = u64::from_le_bytes(rest.get(8..16)?.try_into().ok()?);
        Some(Self { mime, total, offset, bytes: rest.get(16..)?.to_vec() })
    }
}

/// A picture as the entries of its document, in order.
pub fn picture_chunks(mime: &str, bytes: &[u8]) -> Vec<PictureChunk> {
    let total = bytes.len() as u64;
    if bytes.is_empty() {
        return vec![PictureChunk { mime: mime.to_string(), total, offset: 0, bytes: Vec::new() }];
    }
    bytes
        .chunks(PICTURE_CHUNK)
        .scan(0u64, |offset, chunk| {
            let at = *offset;
            *offset += chunk.len() as u64;
            Some(PictureChunk { mime: mime.to_string(), total, offset: at, bytes: chunk.to_vec() })
        })
        .collect()
}

fn picture_doc(blob: BlobId) -> String {
    VaultDocId::Blob(blob).as_str()
}

/// Encrypt one entry of a picture's document under its data key.
pub fn seal_picture(store_id: StoreId, blob: BlobId, dek_id: KeyId, dek: &SymmetricKey, chunk: &PictureChunk) -> String {
    let aad = blob_aad(&store_id.to_string(), &picture_doc(blob));
    URL_SAFE_NO_PAD.encode(Blob::encrypt(dek, dek_id, &aad, &chunk.encode()))
}

/// A fresh data key for a picture, wrapped under every scope key in
/// `scope_key_ids` that is held. `None` when none is.
pub fn new_picture_key(store_id: StoreId, blob: BlobId, scope_key_ids: &[KeyId], scope: &impl ScopeKeys) -> Option<(KeyId, SymmetricKey, VaultDocKeys)> {
    let dek = SymmetricKey::generate();
    let dek_id = KeyId::new_v4();
    let aad = dek_aad(&store_id.to_string(), &picture_doc(blob));
    let mut wraps: Vec<WrappedDek> = Vec::new();
    for scope_key_id in scope_key_ids {
        if wraps.iter().any(|w| w.scope_key_id == *scope_key_id) {
            continue;
        }
        if let Some(scope_key) = scope.scope_key(scope_key_id) {
            wraps.push(wrap_dek(&dek, &scope_key, *scope_key_id, &aad));
        }
    }
    (!wraps.is_empty()).then(|| (dek_id, dek.clone(), VaultDocKeys { dek_id, wraps }))
}

/// A picture's data key, from its wraps and whichever scope key held here
/// opens one of them.
pub fn picture_key(store_id: StoreId, blob: BlobId, keys: &VaultDocKeys, scope: &impl ScopeKeys) -> Option<SymmetricKey> {
    let aad = dek_aad(&store_id.to_string(), &picture_doc(blob));
    keys.wraps.iter().find_map(|wrap| unwrap_dek(wrap, &scope.scope_key(&wrap.scope_key_id)?, &aad).ok())
}

/// Wrap a picture's data key under one more scope key (a share it entered).
pub fn wrap_picture_key(store_id: StoreId, blob: BlobId, dek: &SymmetricKey, scope_key: &SymmetricKey, scope_key_id: KeyId) -> WrappedDek {
    wrap_dek(dek, scope_key, scope_key_id, &dek_aad(&store_id.to_string(), &picture_doc(blob)))
}

/// How many entries a picture of `len` bytes is sent as.
pub fn picture_chunk_count(len: usize) -> u64 {
    len.div_ceil(PICTURE_CHUNK).max(1) as u64
}

/// How far a picture's document has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PictureProgress {
    /// Every chunk from the first is here: its type and bytes.
    Whole(String, Vec<u8>),
    /// A run from the first chunk that stops short: the type and length it
    /// says the picture has, and how many bytes of it are here. Its sender
    /// finishes it by appending from `have` (`continue_picture`).
    Partial { mime: String, total: u64, have: u64 },
    /// No first chunk opens (nothing sent yet, or nothing readable).
    Nothing,
}

/// Read a picture's document as far as it goes. Entries are taken in order:
/// a chunk that continues a run where it stopped extends it, a first chunk
/// starts the run again (a sender that could not finish started over), and
/// anything else, or anything that does not open, is passed over.
/// `Err(NoKey)` when no wrap opens with a key held here.
pub fn picture_progress(store_id: StoreId, blob: BlobId, fetched: &VaultFetchResponse, scope: &impl ScopeKeys) -> Result<PictureProgress, Unopened> {
    let keys = fetched.keys.as_ref().ok_or(Unopened::NoKey(KeyId::nil()))?;
    let dek = picture_key(store_id, blob, keys, scope).ok_or(Unopened::NoKey(keys.dek_id))?;
    let aad = blob_aad(&store_id.to_string(), &picture_doc(blob));
    let mut run: Option<(String, u64, Vec<u8>)> = None;
    for (_, encoded) in fetched_entries(fetched) {
        let Ok(sealed) = decode_blob(encoded) else { continue };
        let Ok(plain) = Blob::decrypt(&dek, &aad, &sealed) else { continue };
        let Some(chunk) = PictureChunk::decode(&plain) else { continue };
        if chunk.offset == 0 {
            run = Some((chunk.mime, chunk.total, chunk.bytes));
        } else if let Some((mime, total, bytes)) = &mut run {
            if *mime == chunk.mime && *total == chunk.total && bytes.len() as u64 == chunk.offset {
                bytes.extend_from_slice(&chunk.bytes);
            } else {
                continue;
            }
        } else {
            continue;
        }
        if let Some((mime, total, bytes)) = &run {
            if bytes.len() as u64 == *total {
                return Ok(PictureProgress::Whole(mime.clone(), bytes.clone()));
            }
        }
    }
    Ok(match run {
        Some((mime, total, bytes)) if (bytes.len() as u64) < total => PictureProgress::Partial { mime, total, have: bytes.len() as u64 },
        _ => PictureProgress::Nothing,
    })
}

/// A picture read out of its document's fetch: its type and bytes, once
/// every chunk from the first is here and opens. `Err(NoKey)` when no wrap
/// opens with a key held here; `Ok(None)` while the picture is not whole
/// (its uploader is still sending it, or stopped half way and has not come
/// back to finish it).
pub fn open_picture(store_id: StoreId, blob: BlobId, fetched: &VaultFetchResponse, scope: &impl ScopeKeys) -> Result<Option<(String, Vec<u8>)>, Unopened> {
    Ok(match picture_progress(store_id, blob, fetched, scope)? {
        PictureProgress::Whole(mime, bytes) => Some((mime, bytes)),
        _ => None,
    })
}

/// The chunks a sender holding the whole picture appends to a document that
/// got only part of it: from where its run stopped when the run is this
/// picture's, every chunk again from the first otherwise (which
/// `picture_progress` reads as a fresh start).
pub fn continue_picture(progress: &PictureProgress, mime: &str, bytes: &[u8]) -> Vec<PictureChunk> {
    let from = match progress {
        PictureProgress::Partial { mime: m, total, have } if m == mime && *total == bytes.len() as u64 => *have,
        _ => 0,
    };
    picture_chunks(mime, bytes).into_iter().filter(|chunk| chunk.offset + chunk.bytes.len() as u64 > from).map(|mut chunk| {
        // A chunk that straddles `from` is trimmed to start there.
        if chunk.offset < from {
            let skip = (from - chunk.offset) as usize;
            chunk.bytes.drain(..skip);
            chunk.offset = from;
        }
        chunk
    }).collect()
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

// ── State reports and comparing (docs/CONVERGENCE_CONTRACT.md, wave 1) ─────

/// The version of a report's plaintext this client writes.
pub const REPORT_VERSION: u8 = 1;
/// How long after the last change to a document its report goes: changes
/// come in bursts (typing), and a report per keystroke would be a write per
/// keystroke on every other device. Debounced per document, so two reports of
/// one document are always at least this far apart.
pub const REPORT_DEBOUNCE_MS: u64 = 5_000;
/// How many documents one `vaultReports` asks about: a store of hundreds of
/// notes is asked in a few calls, each answer well under a relay's message.
pub const REPORTS_BATCH: usize = 128;
/// How many reports are sent at once at a connect: one socket carries them,
/// and a round trip each, one after another, would make a store of hundreds
/// of notes wait half a minute.
pub const REPORTS_IN_FLIGHT: usize = 32;

/// What a device holds of one document (docs/CONVERGENCE_CONTRACT.md
/// section 1): its state vector, a digest of its delete set, and when an
/// update waits parked in it, the state vector of what that update waits
/// for. A state vector says exactly which structs a yrs document holds, and
/// nothing of what it deleted; two devices whose only difference is a delete
/// have one vector, and only the digest tells them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// `NodeDoc::state_vector`, yrs v1.
    pub state_vector: Vec<u8>,
    /// `NodeDoc::delete_set_digest`. `None` in a report that carries none.
    pub deletes: Option<[u8; 32]>,
    /// `NodeDoc::missing`, yrs v1: what a parked update waits for. Said in
    /// wave 1 and acted on in wave 2 (section 3).
    pub missing: Option<Vec<u8>>,
}

/// A report's plaintext: small, versioned JSON. A newer version may add
/// fields; one that carries a state vector still reads.
#[derive(serde::Serialize, serde::Deserialize)]
struct ReportWire {
    v: u8,
    sv: String,
    /// The delete set's digest, base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ds: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    missing: Option<String>,
}

impl Report {
    /// What `doc` holds now.
    pub fn of(doc: &pimble_crdt::NodeDoc) -> Self {
        Self { state_vector: doc.state_vector(), deletes: Some(doc.delete_set_digest()), missing: doc.missing() }
    }

    pub fn encode(&self) -> Vec<u8> {
        let wire = ReportWire {
            v: REPORT_VERSION,
            sv: STANDARD.encode(&self.state_vector),
            ds: self.deletes.map(|d| STANDARD.encode(d)),
            missing: self.missing.as_ref().map(|m| STANDARD.encode(m)),
        };
        serde_json::to_vec(&wire).expect("a report always encodes")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let wire: ReportWire = serde_json::from_slice(bytes).map_err(|e| format!("not a state report: {e}"))?;
        if wire.v == 0 {
            return Err("a state report of version 0".to_string());
        }
        let state_vector = STANDARD.decode(&wire.sv).map_err(|e| format!("a state report's vector would not decode: {e}"))?;
        let missing = match wire.missing {
            Some(m) => Some(STANDARD.decode(m).map_err(|e| format!("a state report's missing vector would not decode: {e}"))?),
            None => None,
        };
        let deletes = match wire.ds {
            Some(d) => Some(
                STANDARD
                    .decode(d)
                    .ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                    .ok_or_else(|| "a state report's delete digest is not 32 bytes".to_string())?,
            ),
            None => None,
        };
        Ok(Self { state_vector, deletes, missing })
    }
}

/// The associated data a report of `node_id` is sealed with: the document's
/// own blobs' with `/report` after it. Under the same key as the document's
/// blobs, but never mistaken for one: a report the server put in a log would
/// not open there, nor a log entry handed back as a report (decided
/// 2026-11-19, building wave 1: "sealed like an append blob", with this one
/// difference).
fn report_aad(store_id: StoreId, node_id: NodeId) -> Vec<u8> {
    blob_aad(&store_id.to_string(), &format!("{}/report", VaultDocId::Node(node_id).as_str()))
}

/// Seal `report` of `node_id` under `key` (the key the document's next blob
/// would go out under), as `vaultReport` carries it. Always one blob.
pub fn seal_report(store_id: StoreId, node_id: NodeId, key_id: KeyId, key: &SymmetricKey, report: &Report) -> String {
    URL_SAFE_NO_PAD.encode(Blob::encrypt(key, key_id, &report_aad(store_id, node_id), &report.encode()))
}

impl DocKeys {
    /// Open another device's report of `node_id`.
    pub fn open_report(&mut self, store_id: StoreId, node_id: NodeId, encoded: &str, scope: &impl ScopeKeys) -> Result<Report, Unopened> {
        let plaintext = self.open_with(store_id, node_id, encoded, &report_aad(store_id, node_id), scope)?;
        Report::decode(&plaintext).map_err(Unopened::Malformed)
    }
}

/// The comparing half of the rule (docs/CONVERGENCE_CONTRACT.md section 2),
/// the same on the desktop's link and in the browser: which other device's
/// report this device answers with an append, and from what vector.
///
/// Where this device's vector has a clock beyond another's report, the other
/// lacks something this one holds, and this one appends the diff from that
/// report. Where neither vector is beyond the other and the delete digests
/// differ, the two hold different deletes of the same structs: each appends
/// the diff from the other's vector, which carries no structs and the whole
/// delete set, so both end with the union (one exchange each way). Where the
/// report is beyond this device, nothing is this device's to send: the log
/// brings it, or the other side sends. And so two devices cannot answer each
/// other for ever, a (document, device) pair answered once is not answered
/// again until that device reports a different vector or digest: a
/// report that did not move after an answer is a device that cannot take
/// what it was sent, or one that is not there (a replica removed, a browser
/// cleared), and sending it again changes nothing. An answer is an append the
/// remote acknowledged, so it is in the log for good (a snapshot holds it
/// too), which is why this record may be kept (the desktop's `devices.json`,
/// the browser's `localStorage`) and is forgotten only with the logs
/// themselves (a twin built again). Without that, every connect of every device would answer
/// a removed replica's last report again, for every document changed since
/// (decided 2026-11-19, building wave 1).
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Comparing {
    /// Document -> device -> the report last answered: its vector and its
    /// delete digest (base64, `vector:digest`).
    #[serde(default)]
    answered: HashMap<String, HashMap<String, String>>,
}

/// How a report is keyed in [`Comparing`]'s record.
fn answered_key(report: &Report) -> String {
    format!("{}:{}", STANDARD.encode(&report.state_vector), report.deletes.map(|d| STANDARD.encode(d)).unwrap_or_default())
}

impl Comparing {
    /// The vector to append the diff from, when `device`'s `report` of `doc`
    /// lacks something this device (`own`, holding what `local` says) holds,
    /// structs or deletes, and that same report has not been answered
    /// already. A device's own report, and one whose vector will not decode,
    /// ask nothing; a report without a digest is compared by vector alone.
    pub fn owed(&self, own: &str, doc: &str, device: &str, local: &Report, report: &Report) -> Option<Vec<u8>> {
        if device == own {
            return None;
        }
        if self.answered.get(doc).and_then(|devices| devices.get(device)) == Some(&answered_key(report)) {
            return None;
        }
        let ours_beyond = pimble_crdt::state_vector_exceeds(&local.state_vector, &report.state_vector).ok()?;
        let theirs_beyond = pimble_crdt::state_vector_exceeds(&report.state_vector, &local.state_vector).ok()?;
        let deletes_differ = matches!((local.deletes, report.deletes), (Some(ours), Some(theirs)) if ours != theirs);
        (ours_beyond || (!theirs_beyond && deletes_differ)).then(|| report.state_vector.clone())
    }

    /// `device`'s `reported` report of `doc` was answered with an append.
    pub fn answered(&mut self, doc: &str, device: &str, reported: &Report) {
        self.answered.entry(doc.to_string()).or_default().insert(device.to_string(), answered_key(reported));
    }

    /// The remote's logs are other logs (a twin built again): nothing
    /// answered there is in these.
    pub fn forget(&mut self) {
        self.answered.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.answered.is_empty()
    }
}

// ── Out of step, said (docs/CONVERGENCE_CONTRACT.md section 4) ──────────────

/// How long a document's difference must last, with the device connected,
/// before it is out of step.
pub const OUT_OF_STEP_AFTER_SECS: u64 = 5 * 60;
/// A report older than this says nothing about whether a document is in
/// step: its device has not been heard from in a month.
pub const REPORTS_COUNT_FOR_DAYS: i64 = 30;
/// A document holds an update parked behind a gap.
pub const PARKED_REASON: &str = "Waits for earlier changes another device has not sent.";

/// Whose device a report came from, as a person reads it: the name the
/// account goes by in the store's shares (`name_of`), this person's own other
/// device, or another device when nobody's name is known.
pub fn device_name(account: Option<&str>, own_account: Option<&str>, name_of: impl Fn(&str) -> Option<String>) -> String {
    match account {
        Some(account) if Some(account) == own_account => "Your other device".to_string(),
        Some(account) => name_of(account).filter(|name| !name.trim().is_empty()).map(|name| format!("{name}'s device")).unwrap_or_else(|| "Another device".to_string()),
        None => "Another device".to_string(),
    }
}

/// "Will's device holds changes this one lacks."
pub fn lacks_here(name: &str) -> String {
    format!("{name} holds changes this one lacks.")
}

/// "This device holds changes Will's device lacks and could not send them."
pub fn could_not_send(name: &str) -> String {
    let whose = match name {
        "Your other device" => "your other device".to_string(),
        "Another device" => "another device".to_string(),
        other => other.to_string(),
    };
    format!("This device holds changes {whose} lacks and could not send them.")
}

/// Why a document differs from what the other devices hold, when it does
/// (section 4), the same judgement on the desktop's link and in the browser.
/// `local` is what this device holds; `heard` the other devices' latest
/// reports, each with its account and how many days old it is; `answered`
/// this device's record; `unsent` whether the remote is not known to hold
/// all this device holds (or a send of it failed); `pushes` whether this
/// device sends changes to the document at all (a reader's never does).
///
/// In order: an update parked behind a gap; a report holding structs this
/// device lacks; something this device holds that a report lacks, has not
/// been able to send, and the remote is not known to hold. What this device
/// has sent and another has not taken yet is that device's to take (it may
/// be off), so it does not count.
pub fn out_of_step_reason<'a>(
    own: &str,
    doc: &str,
    local: &Report,
    parked: bool,
    heard: impl IntoIterator<Item = (&'a str, &'a Report, Option<&'a str>, i64)>,
    answered: &Comparing,
    unsent: bool,
    pushes: bool,
    name: impl Fn(Option<&str>) -> String,
) -> Option<String> {
    if parked {
        return Some(PARKED_REASON.to_string());
    }
    let mut fresh: Vec<(&str, &Report, Option<&str>)> = heard.into_iter().filter(|(_, _, _, days)| *days <= REPORTS_COUNT_FOR_DAYS).map(|(d, r, a, _)| (d, r, a)).collect();
    fresh.sort_by(|a, b| a.0.cmp(b.0));
    for (_, report, account) in &fresh {
        if pimble_crdt::state_vector_exceeds(&report.state_vector, &local.state_vector).unwrap_or(false) {
            return Some(lacks_here(&name(*account)));
        }
    }
    if pushes && unsent {
        for (device, report, account) in &fresh {
            if answered.owed(own, doc, device, local, report).is_some() {
                return Some(could_not_send(&name(*account)));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_picture_goes_out_in_chunks_and_comes_back_whole_only_to_a_key_holder() {
        use super::*;
        use crate::VaultEntry;
        let store_id = StoreId::new();
        let blob = BlobId::new();
        let scope_key_id = KeyId::new_v4();
        let mut scope: HashMap<KeyId, SymmetricKey> = HashMap::new();
        scope.insert(scope_key_id, SymmetricKey::generate());
        let bytes: Vec<u8> = (0..(PICTURE_CHUNK * 2 + 17)).map(|i| (i % 251) as u8).collect();
        let chunks = picture_chunks("image/png", &bytes);
        assert_eq!(chunks.len(), 3);
        let (dek_id, dek, keys) = new_picture_key(store_id, blob, &[scope_key_id], &scope).unwrap();
        let sealed: Vec<String> = chunks.iter().map(|c| seal_picture(store_id, blob, dek_id, &dek, c)).collect();
        let fetch = |upto: usize, keys: Option<VaultDocKeys>| VaultFetchResponse {
            snapshot: None,
            snapshot_parts: Vec::new(),
            updates: sealed[..upto].iter().enumerate().map(|(i, blob)| VaultEntry { seq: i as u64 + 1, blob: blob.clone(), part: None }).collect(),
            head: upto as u64,
            snapshot_seq: 0,
            keys,
            more: false,
        };
        assert_eq!(open_picture(store_id, blob, &fetch(2, Some(keys.clone())), &scope), Ok(None), "not whole yet");
        assert_eq!(open_picture(store_id, blob, &fetch(3, Some(keys.clone())), &scope), Ok(Some(("image/png".to_string(), bytes.clone()))));
        let stranger: HashMap<KeyId, SymmetricKey> = HashMap::new();
        assert!(matches!(open_picture(store_id, blob, &fetch(3, Some(keys.clone())), &stranger), Err(Unopened::NoKey(_))));
        // Cut short after the first chunk: partial, and the sender finishes it
        // from where it stopped.
        let partial = picture_progress(store_id, blob, &fetch(1, Some(keys.clone())), &scope).unwrap();
        assert_eq!(partial, PictureProgress::Partial { mime: "image/png".into(), total: bytes.len() as u64, have: PICTURE_CHUNK as u64 });
        let rest = continue_picture(&partial, "image/png", &bytes);
        assert_eq!(rest.iter().map(|c| c.offset).collect::<Vec<_>>(), vec![PICTURE_CHUNK as u64, 2 * PICTURE_CHUNK as u64]);
        let mut resumed = fetch(1, Some(keys.clone()));
        for (i, chunk) in rest.iter().enumerate() {
            resumed.updates.push(VaultEntry { seq: i as u64 + 2, blob: seal_picture(store_id, blob, dek_id, &dek, chunk), part: None });
        }
        assert_eq!(open_picture(store_id, blob, &resumed, &scope).unwrap().map(|(_, b)| b.len()), Some(bytes.len()));
        // A run that cannot be continued (a stray chunk after it) is started
        // over from the first chunk, and that reads whole too.
        let mut restarted = fetch(1, Some(keys.clone()));
        let stray = PictureChunk { mime: "image/png".into(), total: bytes.len() as u64, offset: 5, bytes: vec![1, 2, 3] };
        restarted.updates.push(VaultEntry { seq: 2, blob: seal_picture(store_id, blob, dek_id, &dek, &stray), part: None });
        for (i, chunk) in continue_picture(&PictureProgress::Nothing, "image/png", &bytes).iter().enumerate() {
            restarted.updates.push(VaultEntry { seq: i as u64 + 3, blob: seal_picture(store_id, blob, dek_id, &dek, chunk), part: None });
        }
        assert_eq!(open_picture(store_id, blob, &restarted, &scope).unwrap().map(|(_, b)| b), Some(bytes.clone()));
        assert_eq!(picture_chunk_count(bytes.len()), 3);
        // Another picture's entries do not open as this one's.
        assert_eq!(open_picture(store_id, BlobId::new(), &fetch(3, Some(keys)), &scope).ok().flatten(), None);
    }

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
            snapshot: Some(VaultEntry { seq: 5, blob: "s".into(), part: None }),
            snapshot_parts: Vec::new(),
            updates: vec![VaultEntry { seq: 7, blob: "u".into(), part: None }],
            head: 7,
            snapshot_seq: 0,
            keys: None,
            more: false,
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

    #[test]
    fn a_snapshot_is_due_by_size_once_the_log_outgrows_twice_the_snapshot() {
        let mut cursor = VaultCursor::starting_at(0);
        cursor.mark(1);
        cursor.mark(2);
        let small = LogSizes { log_bytes: COMPACT_FLOOR, snapshot_bytes: 0 };
        assert!(!snapshot_due_by(2, 0, &cursor, Some(small)), "not past the floor");
        let big = LogSizes { log_bytes: COMPACT_FLOOR + 1, snapshot_bytes: 0 };
        assert!(snapshot_due_by(2, 0, &cursor, Some(big)), "past the floor, two appends");
        let under_twice = LogSizes { log_bytes: 6 * COMPACT_FLOOR, snapshot_bytes: 3 * COMPACT_FLOOR };
        assert!(!snapshot_due_by(2, 0, &cursor, Some(under_twice)));
        let over_twice = LogSizes { log_bytes: 6 * COMPACT_FLOOR + 1, snapshot_bytes: 3 * COMPACT_FLOOR };
        assert!(snapshot_due_by(2, 0, &cursor, Some(over_twice)));
        // The same `applied_through` rule as by count.
        cursor.mark(4);
        assert!(!snapshot_due_by(4, 0, &cursor, Some(big)), "3 is still on its way");
        // An older server gives no sizes: the count rule alone.
        assert!(!snapshot_due_by(2, 0, &cursor, None));
        assert_eq!(LogSizes::of(&VaultAppendResponse { seq: 1, first_seq: None, log_bytes: None, snapshot_bytes: None }), None);
    }

    fn parts_fixture(len: usize) -> (StoreId, NodeId, KeyId, HashMap<KeyId, SymmetricKey>, SymmetricKey, Vec<u8>) {
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        let key_id = KeyId::new_v4();
        let (scope, key) = scope_with(key_id);
        let plain: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        (store_id, node_id, key_id, scope, key, plain)
    }

    #[test]
    fn a_large_blob_goes_in_parts_and_opens_only_whole() {
        let (store_id, node_id, key_id, scope, key, plain) = parts_fixture(PART_BYTES * 2 + 5);
        assert_eq!(seal_parts(store_id, node_id, key_id, &key, &plain[..10], true).len(), 1, "small: whole");
        assert_eq!(seal_parts(store_id, node_id, key_id, &key, &plain, false).len(), 1, "no parts at a remote that takes none");
        let sealed = seal_parts(store_id, node_id, key_id, &key, &plain, true);
        assert_eq!(sealed.len(), 3);
        let fetched = VaultFetchResponse {
            snapshot: None,
            snapshot_parts: Vec::new(),
            updates: sealed.iter().enumerate().map(|(i, s)| VaultEntry { seq: i as u64 + 4, blob: s.blob.clone(), part: s.part }).collect(),
            head: 6,
            snapshot_seq: 0,
            keys: None,
            more: false,
        };
        let units = fetched_units(&fetched);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].mark(), Some(Mark::Span(4, 6)));
        let mut keys = DocKeys::default();
        assert_eq!(keys.open_unit(store_id, node_id, &units[0], &scope).unwrap(), plain);
        // A reader that knows nothing of parts (the whole-blob associated
        // data) opens no part of it: it never applies a fragment.
        for s in &sealed {
            assert!(matches!(keys.open(store_id, node_id, &s.blob, &scope), Err(Unopened::Failed(_))));
        }
        // A part is not another index of its group, nor another group's.
        let p0 = sealed[0].part.unwrap();
        assert!(keys.open_part(store_id, node_id, &sealed[0].blob, VaultPart { index: 1, ..p0 }, &scope).is_err());
        assert!(keys.open_part(store_id, node_id, &sealed[0].blob, VaultPart { group: Uuid::new_v4(), ..p0 }, &scope).is_err());
        // Missing a part: incomplete, nothing applies.
        let mut short = fetched.clone();
        short.updates.remove(1);
        assert_eq!(fetched_units(&short), vec![Unit::Incomplete]);
    }

    #[test]
    fn a_snapshot_in_parts_reads_as_one_unit() {
        let (store_id, node_id, key_id, scope, key, plain) = parts_fixture(PART_BYTES + 1);
        let sealed = seal_parts(store_id, node_id, key_id, &key, &plain, true);
        let entries: Vec<VaultEntry> = sealed.iter().map(|s| VaultEntry { seq: 9, blob: s.blob.clone(), part: s.part }).collect();
        let fetched = VaultFetchResponse {
            snapshot: Some(entries[0].clone()),
            snapshot_parts: entries[1..].to_vec(),
            updates: vec![VaultEntry::whole(10, seal(store_id, node_id, key_id, &key, b"after"))],
            head: 10,
            snapshot_seq: 0,
            keys: None,
            more: false,
        };
        let units = fetched_units(&fetched);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].mark(), Some(Mark::Through(9)));
        let mut keys = DocKeys::default();
        assert_eq!(keys.open_unit(store_id, node_id, &units[0], &scope).unwrap(), plain);
        assert_eq!(keys.open_unit(store_id, node_id, &units[1], &scope).unwrap(), b"after");
        let mut cursor = VaultCursor::default();
        for unit in &units {
            unit.mark().unwrap().apply(&mut cursor);
        }
        assert_eq!(cursor.applied_through(), 10);
    }

    #[test]
    fn live_parts_wait_for_their_group() {
        let (store_id, node_id, key_id, scope, key, plain) = parts_fixture(PART_BYTES * 2 + 1);
        let sealed = seal_parts(store_id, node_id, key_id, &key, &plain, true);
        let mut buffer = PartBuffer::default();
        let doc = node_id.to_string();
        assert!(buffer.take(&doc, 7, sealed[0].part.unwrap(), sealed[0].blob.clone()).is_none());
        assert!(buffer.holds(&doc));
        assert!(buffer.take(&doc, 8, sealed[1].part.unwrap(), sealed[1].blob.clone()).is_none());
        let (mark, parts) = buffer.take(&doc, 9, sealed[2].part.unwrap(), sealed[2].blob.clone()).unwrap();
        assert_eq!(mark, Mark::Span(7, 9));
        assert!(!buffer.holds(&doc));
        let mut keys = DocKeys::default();
        assert_eq!(keys.open_parts(store_id, node_id, parts.iter().map(|(p, b)| (*p, b.as_str())), &scope).unwrap(), plain);
    }

    #[test]
    fn a_blob_of_a_newer_format_says_so() {
        let (store_id, node_id, key_id, scope, key, _) = parts_fixture(1);
        let mut raw = decode_blob(&seal(store_id, node_id, key_id, &key, b"x")).unwrap();
        raw[2] = 2;
        let encoded = URL_SAFE_NO_PAD.encode(raw);
        let mut keys = DocKeys::default();
        assert_eq!(keys.open(store_id, node_id, &encoded, &scope), Err(Unopened::Newer));
        assert_eq!(Unopened::Newer.reason(), MADE_BY_NEWER);
    }

    /// A report seals under the document's key and opens back to what it
    /// said; it never opens as one of the document's blobs, nor a blob as a
    /// report; a holder of no key opens nothing.
    #[test]
    fn a_report_opens_as_a_report_and_never_as_a_blob() {
        use super::*;
        let store_id = StoreId::new();
        let node_id = NodeId::new();
        let key_id = KeyId::new_v4();
        let key = SymmetricKey::generate();
        let mut scope: HashMap<KeyId, SymmetricKey> = HashMap::new();
        scope.insert(key_id, key.clone());
        let mut doc = pimble_crdt::NodeDoc::new();
        doc.replace_plain_text("some words").unwrap();
        let report = Report::of(&doc);
        assert_eq!(Report::decode(&report.encode()).unwrap(), report);
        assert!(report.deletes.is_some(), "a report says what its document deleted");
        let with_missing = Report { state_vector: report.state_vector.clone(), deletes: None, missing: Some(pimble_crdt::empty_state_vector()) };
        assert_eq!(Report::decode(&with_missing.encode()).unwrap(), with_missing);

        let sealed = seal_report(store_id, node_id, key_id, &key, &report);
        let mut keys = DocKeys::default();
        assert_eq!(keys.open_report(store_id, node_id, &sealed, &scope).unwrap(), report);
        assert!(keys.open(store_id, node_id, &sealed, &scope).is_err(), "a report is not one of the document's blobs");
        assert!(keys.open_report(store_id, NodeId::new(), &sealed, &scope).is_err(), "nor another document's report");
        let blob = seal(store_id, node_id, key_id, &key, &report.encode());
        assert!(keys.open_report(store_id, node_id, &blob, &scope).is_err(), "a blob is not a report");
        let stranger: HashMap<KeyId, SymmetricKey> = HashMap::new();
        assert!(matches!(DocKeys::default().open_report(store_id, node_id, &sealed, &stranger), Err(Unopened::NoKey(_))));
    }

    /// A device answers a report that lacks what it holds, from that report's
    /// vector; not its own report, not one ahead of it or equal, and not the
    /// same report twice, even after a restart; a report that moved is
    /// answered again.
    #[test]
    fn comparing_answers_what_a_report_lacks_once_per_report() {
        use super::*;
        let mut ahead_doc = pimble_crdt::NodeDoc::new();
        ahead_doc.replace_plain_text("one").unwrap();
        let ahead = Report::of(&ahead_doc);
        let behind = Report::of(&pimble_crdt::NodeDoc::new());
        let mut comparing = Comparing::default();
        assert_eq!(comparing.owed("me", "doc", "me", &ahead, &behind), None, "never its own");
        assert_eq!(comparing.owed("me", "doc", "other", &ahead, &behind), Some(behind.state_vector.clone()));
        assert_eq!(comparing.owed("me", "doc", "other", &ahead, &ahead), None, "equal: nothing owed");
        assert_eq!(comparing.owed("me", "doc", "other", &behind, &ahead), None, "the report is ahead: nothing of this device's to send");
        comparing.answered("doc", "other", &behind);
        assert_eq!(comparing.owed("me", "doc", "other", &ahead, &behind), None, "the same report is not answered twice");
        assert!(comparing.owed("me", "doc", "third", &ahead, &behind).is_some(), "another device is its own pair");
        let mut moved = pimble_crdt::NodeDoc::new();
        moved.replace_plain_text("x").unwrap();
        let moved = Report::of(&moved);
        assert!(comparing.owed("me", "doc", "other", &ahead, &moved).is_some(), "a report that moved is answered again");
        let kept: Comparing = serde_json::from_str(&serde_json::to_string(&comparing).unwrap()).unwrap();
        assert_eq!(kept, comparing, "what was answered is kept as it was");
        comparing.forget();
        assert!(comparing.owed("me", "doc", "other", &ahead, &behind).is_some(), "logs built again: compared from the start");
        let broken = Report { state_vector: vec![0xff, 0xff, 0xff], deletes: None, missing: None };
        assert_eq!(comparing.owed("me", "doc", "other", &ahead, &broken), None, "a vector that will not decode asks nothing");
    }

    /// A copy of `doc` holding one more delete and no more structs: what a
    /// device that deleted text while the other's copy missed it holds.
    fn with_a_delete(doc: &pimble_crdt::NodeDoc) -> (pimble_crdt::NodeDoc, Vec<u8>) {
        let mut scratch = pimble_crdt::NodeDoc::load(&doc.save()).unwrap();
        scratch.replace_plain_text("").unwrap();
        let deletes_only = scratch.diff_since(&scratch.state_vector()).unwrap();
        let mut copy = pimble_crdt::NodeDoc::load(&doc.save()).unwrap();
        copy.apply_update(&deletes_only).unwrap();
        (copy, deletes_only)
    }

    /// Two devices with one state vector and different deletes compare
    /// different, each answers the other once from the other's vector, and
    /// the answers leave both with the union and equal reports; nothing is
    /// answered again for the same (vector, digest).
    #[test]
    fn a_delete_one_device_lacks_is_compared_by_its_digest() {
        use super::*;
        let mut base = pimble_crdt::NodeDoc::new();
        base.replace_plain_text("socks and a map").unwrap();
        let (mut deleted, _) = with_a_delete(&base);
        let mut missed = pimble_crdt::NodeDoc::load(&base.save()).unwrap();
        assert_eq!(deleted.state_vector(), missed.state_vector(), "a delete moves no clock");
        assert_eq!(pimble_crdt::NodeDoc::text_of(&deleted.save()), "");
        let (d, m) = (Report::of(&deleted), Report::of(&missed));
        assert_ne!(d.deletes, m.deletes, "the digests tell them apart");
        assert_eq!(Report::of(&base).deletes, m.deletes, "the same delete set, the same digest");

        let mut on_deleted = Comparing::default();
        let mut on_missed = Comparing::default();
        let from = on_deleted.owed("del", "doc", "mis", &d, &m).expect("the device with the delete sends it");
        let back = on_missed.owed("mis", "doc", "del", &m, &d).expect("and the other way: neither knows whose set is larger");
        missed.apply_update(&deleted.diff_since(&from).unwrap()).unwrap();
        deleted.apply_update(&missed.diff_since(&back).unwrap()).unwrap();
        on_deleted.answered("doc", "mis", &m);
        on_missed.answered("doc", "del", &d);
        assert_eq!(pimble_crdt::NodeDoc::text_of(&missed.save()), "", "the delete arrived");
        let (d2, m2) = (Report::of(&deleted), Report::of(&missed));
        assert_eq!(d2, m2, "both hold the union");
        assert_eq!(on_deleted.owed("del", "doc", "mis", &d2, &m2), None);
        assert_eq!(on_missed.owed("mis", "doc", "del", &m2, &d2), None);
        assert_eq!(on_deleted.owed("del", "doc", "mis", &d, &m), None, "the same report is not answered twice");

        // A report with no digest is compared by vector alone.
        let undigested = Report { deletes: None, ..m.clone() };
        assert_eq!(Comparing::default().owed("del", "doc", "mis", &d, &undigested), None);
    }

    /// The digest is the delete set's and not the blocks': deletes that
    /// arrive again, through another copy, and a save and load, which writes
    /// the blocks out afresh, leave it as it was.
    #[test]
    fn the_delete_digest_does_not_depend_on_how_deletes_were_made() {
        let mut base = pimble_crdt::NodeDoc::new();
        base.replace_plain_text("abcdef").unwrap();
        let mut once = pimble_crdt::NodeDoc::load(&base.save()).unwrap();
        once.replace_plain_text("").unwrap();
        let mut twice = pimble_crdt::NodeDoc::load(&base.save()).unwrap();
        let all = once.diff_since(&once.state_vector()).unwrap();
        let mut halves = pimble_crdt::NodeDoc::load(&base.save()).unwrap();
        halves.apply_update(&all).unwrap();
        twice.apply_update(&halves.diff_since(&halves.state_vector()).unwrap()).unwrap();
        twice.apply_update(&all).unwrap();
        let base_sv = base.state_vector();
        assert_eq!(twice.state_vector(), base_sv);
        assert_eq!(twice.delete_set_digest(), halves.delete_set_digest());
        let reloaded = pimble_crdt::NodeDoc::load(&twice.save()).unwrap();
        assert_eq!(reloaded.delete_set_digest(), twice.delete_set_digest(), "saving and loading keeps the digest");
    }

    /// The reasons a document is out of step, in their order, and the names
    /// a person reads.
    #[test]
    fn out_of_step_reasons_name_whose_device_and_why() {
        use super::*;
        let mut base = pimble_crdt::NodeDoc::new();
        base.replace_plain_text("one").unwrap();
        let mut ahead = pimble_crdt::NodeDoc::load(&base.save()).unwrap();
        ahead.replace_plain_text("two").unwrap();
        let (b, a) = (Report::of(&base), Report::of(&ahead));
        let name_of = |id: &str| (id == "will").then(|| "Will".to_string());
        let name = |account: Option<&str>| device_name(account, Some("me"), name_of);
        let none = Comparing::default();
        assert_eq!(name(Some("will")), "Will's device");
        assert_eq!(name(Some("me")), "Your other device");
        assert_eq!(name(Some("stranger")), "Another device");
        assert_eq!(name(None), "Another device");

        let reason = |local: &Report, parked: bool, heard: Vec<(&'static str, &Report, Option<&'static str>, i64)>, unsent: bool, pushes: bool| {
            out_of_step_reason("dev-me", "doc", local, parked, heard, &none, unsent, pushes, name)
        };
        assert_eq!(reason(&a, true, vec![], false, true).as_deref(), Some(PARKED_REASON));
        assert_eq!(reason(&b, false, vec![("dev-will", &a, Some("will"), 0)], false, true).as_deref(), Some("Will's device holds changes this one lacks."));
        assert_eq!(reason(&b, false, vec![("dev-will", &a, Some("will"), 31)], false, true), None, "a report a month old says nothing");
        assert_eq!(reason(&a, false, vec![("dev-will", &b, Some("will"), 0)], true, true).as_deref(), Some("This device holds changes Will's device lacks and could not send them."));
        assert_eq!(reason(&a, false, vec![("dev-x", &b, None, 0)], true, true).as_deref(), Some("This device holds changes another device lacks and could not send them."));
        assert_eq!(reason(&a, false, vec![("dev-will", &b, Some("will"), 0)], false, true), None, "sent, and the remote holds it: Will's device's to take");
        assert_eq!(reason(&a, false, vec![("dev-will", &b, Some("will"), 0)], true, false), None, "a reader sends nothing");
        let mut answered = Comparing::default();
        answered.answered("doc", "dev-will", &b);
        assert_eq!(out_of_step_reason("dev-me", "doc", &a, false, vec![("dev-will", &b, Some("will"), 0)], &answered, true, true, name), None, "answered: sent");
        assert_eq!(reason(&a, false, vec![("dev-will", &a, Some("will"), 0)], true, true), None, "equal: in step");
    }
}
