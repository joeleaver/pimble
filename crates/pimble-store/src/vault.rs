//! On-disk storage for a store of kind `vault` (docs/CRYPTO_CONTRACT.md
//! "Pimble server, a store of kind `vault`"): a `VaultStore` never holds a
//! [`pimble_crdt::StoreDocument`] or [`pimble_crdt::ContentDoc`] — a vault
//! store's tree and node content are opaque, client-encrypted blobs the
//! server only appends, snapshots and hands back. Layout per document
//! (`doc_id` is [`pimble_rpc`]'s `VaultDocId::as_str()`, a node id or the
//! literal `tree`, though this crate never depends on `pimble-rpc` — it just
//! takes `doc_id` as a `&str`):
//!
//! ```text
//! <store>/vault/{doc_id}/log       # append-only: seq u64 | len u32 | blob
//! <store>/vault/{doc_id}/snapshot  # seq u64 | blob
//! <store>/vault/{doc_id}/keys.json # the document's wrapped data keys, opaque here
//! <store>/scopes.json              # the store's published scope sets
//! ```
//!
//! An in-memory index of each log entry's byte offset and length is built at
//! open (and kept current on every append/snapshot), so fetching a range of
//! updates reads exactly those bytes rather than rescanning the file.
//!
//! Sharing (docs/NODE_DOCUMENT_CONTRACT.md section 5) adds two things the
//! server reads but never interprets as data: a document's data keys
//! (`keys.json`, the `VaultDocKeys` JSON the RPC layer defines, kept as the
//! text it arrived as so this crate needs no crypto types; only its `dek_id`
//! is read out, for `vaultListDocs`) and the scope sets (`scopes.json`: each
//! share's root mapped to the ids of the documents under it, as the owner's
//! devices publish them and as the server extends them when a scoped member
//! creates a document). Both are authorization metadata for the server's own
//! use; neither is ciphertext, and neither says anything about a document's
//! contents.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use pimble_core::{NodeId, StoreId, StoreKind, StoreManifest};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::{Result, StoreError};

/// One blob's maximum size, and the maximum a document's log may reach
/// before a snapshot is required (docs/CRYPTO_CONTRACT.md "RPCs").
const MAX_BLOB_BYTES: usize = 4 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;

/// Write a file atomically: write to a `.tmp` sibling, then rename into
/// place. Mirrors `local::atomic_write` (not reused directly: that function
/// is private to this crate's `local` module).
async fn atomic_write(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, data).await?;
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

/// One log entry's location within `log`, without its bytes.
#[derive(Debug, Clone, Copy)]
struct LogEntryMeta {
    seq: u64,
    /// Byte offset of this entry's header (`seq | len`) within the log file.
    offset: u64,
    len: u32,
}

/// One document's in-memory state: every log entry currently on disk (in
/// ascending seq order) and the current snapshot, if any.
struct VaultDoc {
    dir: PathBuf,
    entries: Vec<LogEntryMeta>,
    snapshot: Option<(u64, Vec<u8>)>,
    /// The latest sequence number ever handed out (0 = nothing appended
    /// yet). Distinct from "the highest seq still in `entries`": a snapshot
    /// can drop every entry without lowering this.
    head: u64,
    /// The document's wrapped data keys as `keys.json` holds them (see the
    /// module doc), with the `dek_id` read out of them once at load.
    keys: Option<DocKeys>,
}

/// `keys.json` as held in memory: the text verbatim, and the one field this
/// crate reads from it.
#[derive(Debug, Clone)]
pub struct DocKeys {
    pub json: String,
    pub dek_id: Uuid,
}

impl DocKeys {
    /// `json` as a keys record, if it carries a `dek_id`; anything else is
    /// not a keys record this crate stores.
    fn parse(json: String) -> Option<Self> {
        #[derive(Deserialize)]
        struct DekIdOnly {
            dek_id: Uuid,
        }
        let DekIdOnly { dek_id } = serde_json::from_str(&json).ok()?;
        Some(Self { json, dek_id })
    }
}

/// `scopes.json`: each share's root mapped to the documents under it. The
/// root itself is always in scope whether or not the set lists it.
///
/// `unconfirmed` holds what the server added by itself (a scoped member's
/// create) and no publish has named yet. A publish replaces a root's set
/// with the owner's view of the subtree, and an owner's device that has not
/// pulled the new document yet publishes a view without it: were that to
/// drop the id, the member would be refused their own new document until
/// the next publish. So a server-added id stays in scope until a publish
/// has included it once; from then on the owner's publishes alone say
/// whether it is in (leaving it out is the owner moving it out of the share).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ScopesFile {
    #[serde(default)]
    v: u8,
    #[serde(default)]
    scopes: HashMap<NodeId, HashSet<NodeId>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    unconfirmed: HashMap<NodeId, HashSet<NodeId>>,
}

impl ScopesFile {
    const VERSION: u8 = 1;
}

impl VaultDoc {
    fn empty(dir: PathBuf) -> Self {
        Self { dir, entries: Vec::new(), snapshot: None, head: 0, keys: None }
    }

    /// Load a document's directory from disk, tolerating a truncated final
    /// log record (a crash mid-append): it is dropped, with a warning, and
    /// the log file is rewritten without it so the next append's offset
    /// arithmetic (based on the file's length) stays correct.
    async fn load(dir: PathBuf) -> Result<Self> {
        let snapshot = {
            let snapshot_path = dir.join("snapshot");
            if snapshot_path.exists() {
                let bytes = fs::read(&snapshot_path).await?;
                if bytes.len() >= 8 {
                    let seq = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
                    Some((seq, bytes[8..].to_vec()))
                } else {
                    warn!("Vault snapshot {:?} is too short to contain a seq; ignoring it", snapshot_path);
                    None
                }
            } else {
                None
            }
        };

        let mut head = snapshot.as_ref().map(|(seq, _)| *seq).unwrap_or(0);
        let mut entries = Vec::new();
        let log_path = dir.join("log");
        if log_path.exists() {
            let bytes = fs::read(&log_path).await?;
            let mut offset = 0usize;
            let mut truncated = false;
            loop {
                if offset == bytes.len() {
                    break;
                }
                if offset + 12 > bytes.len() {
                    truncated = true;
                    break;
                }
                let seq = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
                let len = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
                let body_start = offset + 12;
                let body_end = body_start + len as usize;
                if body_end > bytes.len() {
                    truncated = true;
                    break;
                }
                entries.push(LogEntryMeta { seq, offset: offset as u64, len });
                head = head.max(seq);
                offset = body_end;
            }
            if truncated {
                warn!(
                    "Vault log {:?} has a truncated trailing record ({} of {} bytes read); dropping it",
                    log_path, offset, bytes.len()
                );
                atomic_write(&log_path, &bytes[..offset]).await?;
            }
        }

        let keys_path = dir.join(Self::KEYS_FILE);
        let keys = match fs::read_to_string(&keys_path).await {
            Ok(json) => {
                let parsed = DocKeys::parse(json);
                if parsed.is_none() {
                    warn!("Vault keys file {:?} is not a keys record; ignoring it", keys_path);
                }
                parsed
            }
            Err(_) => None,
        };

        Ok(Self { dir, entries, snapshot, head, keys })
    }

    const KEYS_FILE: &'static str = "keys.json";

    /// The bytes of one already-recorded log entry, read fresh from disk.
    async fn read_entry(&self, entry: &LogEntryMeta) -> Result<Vec<u8>> {
        let mut file = fs::File::open(self.dir.join("log")).await?;
        file.seek(std::io::SeekFrom::Start(entry.offset + 12)).await?;
        let mut buf = vec![0u8; entry.len as usize];
        file.read_exact(&mut buf).await?;
        Ok(buf)
    }

    /// Every log entry's total on-disk size (header plus blob).
    fn log_size(&self) -> u64 {
        self.entries.iter().map(|e| 12 + e.len as u64).sum()
    }
}

/// A store of kind `vault`: no [`pimble_crdt::StoreDocument`], no
/// [`pimble_crdt::ContentDoc`], no search index — just append-only encrypted
/// blobs per document, addressed by sequence number.
pub struct VaultStore {
    pub id: StoreId,
    pub path: PathBuf,
    manifest: StoreManifest,
    docs: HashMap<String, VaultDoc>,
    /// The published scope sets (docs/NODE_DOCUMENT_CONTRACT.md section 5),
    /// `scopes.json`, loaded at open and rewritten on every change.
    scopes: ScopesFile,
}

impl VaultStore {
    const MANIFEST_FILE: &'static str = "manifest.json";
    const VAULT_DIR: &'static str = "vault";
    const SCOPES_FILE: &'static str = "scopes.json";

    /// Create a new vault store at `path`, with a freshly generated id
    /// unless `store_id` names one (docs/CRYPTO_CONTRACT.md: `createStore`'s
    /// `store_id`, for the accounts service creating a hosted twin under the
    /// same id as an existing local store). Refusing an id already open is
    /// the caller's job (`StoreManager`).
    pub async fn create(path: impl AsRef<Path>, name: impl Into<String>, store_id: Option<StoreId>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let name = name.into();

        if path.exists() {
            return Err(StoreError::StoreExists(path.display().to_string()));
        }

        fs::create_dir_all(&path).await?;
        fs::create_dir(path.join(Self::VAULT_DIR)).await?;

        // `root_node_id` is meaningless for a vault (there is no tree here),
        // but `StoreManifest` always carries one; a fresh id is as good as
        // any other placeholder.
        let mut manifest = StoreManifest::new_with_kind(&name, pimble_core::NodeId::new(), StoreKind::Vault);
        if let Some(id) = store_id {
            manifest.id = id;
        }

        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        fs::write(path.join(Self::MANIFEST_FILE), manifest_json).await?;

        info!("Created vault store '{}' ({}) at {:?}", name, manifest.id, path);
        Ok(Self { id: manifest.id, path, manifest, docs: HashMap::new(), scopes: ScopesFile::default() })
    }

    /// Open an existing vault store, loading every document under
    /// `<path>/vault/`.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let manifest_path = path.join(Self::MANIFEST_FILE);
        if !manifest_path.exists() {
            return Err(StoreError::InvalidPath(format!("No manifest found at {}", manifest_path.display())));
        }
        let manifest_json = fs::read_to_string(&manifest_path).await?;
        let manifest: StoreManifest = serde_json::from_str(&manifest_json)?;
        if manifest.kind != StoreKind::Vault {
            return Err(StoreError::InvalidOperation(format!(
                "store {} at {:?} is not a vault store", manifest.id, path
            )));
        }

        let mut docs = HashMap::new();
        let vault_dir = path.join(Self::VAULT_DIR);
        if vault_dir.exists() {
            let mut entries = fs::read_dir(&vault_dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                if entry.file_type().await?.is_dir() {
                    let doc_id = entry.file_name().to_string_lossy().into_owned();
                    let doc = VaultDoc::load(entry.path()).await?;
                    docs.insert(doc_id, doc);
                }
            }
        }

        // A scopes file that does not parse is treated as absent, with a
        // warning: the owner's devices republish the sets at every connect,
        // and failing the open over authorization metadata would take the
        // whole store away from its owner too.
        let scopes_path = path.join(Self::SCOPES_FILE);
        let scopes = match fs::read_to_string(&scopes_path).await {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                warn!("Vault scopes file {:?} could not be parsed ({}); starting with no scopes", scopes_path, e);
                ScopesFile::default()
            }),
            Err(_) => ScopesFile::default(),
        };

        info!("Opened vault store '{}' ({}) from {:?}", manifest.name, manifest.id, path);
        Ok(Self { id: manifest.id, path, manifest, docs, scopes })
    }

    pub fn manifest(&self) -> &StoreManifest {
        &self.manifest
    }

    fn doc_dir(&self, doc_id: &str) -> PathBuf {
        self.path.join(Self::VAULT_DIR).join(doc_id)
    }

    // ── Scope sets (docs/NODE_DOCUMENT_CONTRACT.md section 5) ────────────

    /// Every scope: its root and the document ids under it, the published
    /// set and what the server added since (see [`ScopesFile`]) together,
    /// which is what a member scoped to the root reaches.
    pub fn scopes(&self) -> Vec<(NodeId, Vec<NodeId>)> {
        let roots: HashSet<NodeId> = self.scopes.scopes.keys().chain(self.scopes.unconfirmed.keys()).copied().collect();
        roots
            .into_iter()
            .map(|root| {
                let mut docs = self.scope_union(&[root]);
                // Listed as published: the root is implied, not an entry,
                // unless the owner's publish names it.
                if !self.scopes.scopes.get(&root).is_some_and(|set| set.contains(&root)) {
                    docs.remove(&root);
                }
                (root, docs.into_iter().collect())
            })
            .collect()
    }

    /// The documents a principal scoped to `roots` reaches: the union of
    /// those roots' sets, each root counted as in its own scope whether or
    /// not its set lists it. A root with no published set reaches only
    /// itself (and what its members created under it since).
    pub fn scope_union(&self, roots: &[NodeId]) -> HashSet<NodeId> {
        let mut out = HashSet::new();
        for root in roots {
            out.insert(*root);
            for sets in [&self.scopes.scopes, &self.scopes.unconfirmed] {
                if let Some(docs) = sets.get(root) {
                    out.extend(docs.iter().copied());
                }
            }
        }
        out
    }

    /// Replace `root`'s set (the owner's publish), and persist. What the
    /// server added and this publish names is confirmed; what it does not
    /// name yet stays in scope (see [`ScopesFile`]).
    pub async fn set_scope(&mut self, root: NodeId, docs: impl IntoIterator<Item = NodeId>) -> Result<()> {
        let docs: HashSet<NodeId> = docs.into_iter().collect();
        if let Some(unconfirmed) = self.scopes.unconfirmed.get_mut(&root) {
            unconfirmed.retain(|id| !docs.contains(id));
            if unconfirmed.is_empty() {
                self.scopes.unconfirmed.remove(&root);
            }
        }
        self.scopes.scopes.insert(root, docs);
        self.write_scopes().await
    }

    /// Remove `root`'s scope (the share is gone), and persist. Removing a
    /// scope that is not there changes nothing.
    pub async fn remove_scope(&mut self, root: NodeId) -> Result<()> {
        let published = self.scopes.scopes.remove(&root).is_some();
        let unconfirmed = self.scopes.unconfirmed.remove(&root).is_some();
        if !published && !unconfirmed {
            return Ok(());
        }
        self.write_scopes().await
    }

    /// A scoped member created `doc_id` under `parent_id`: the new id joins
    /// every one of the member's scopes (`roots`) that holds the parent, so
    /// their later reads and writes of it, and every other member's, are
    /// in scope before the owner's devices ever see it. Returns whether any
    /// scope changed.
    pub async fn extend_scopes(&mut self, roots: &[NodeId], parent_id: NodeId, doc_id: NodeId) -> Result<bool> {
        let mut changed = false;
        for root in roots {
            let scope = self.scope_union(&[*root]);
            if scope.contains(&parent_id) && !scope.contains(&doc_id) {
                self.scopes.unconfirmed.entry(*root).or_default().insert(doc_id);
                changed = true;
            }
        }
        if changed {
            self.write_scopes().await?;
        }
        Ok(changed)
    }

    async fn write_scopes(&mut self) -> Result<()> {
        self.scopes.v = ScopesFile::VERSION;
        let json = serde_json::to_string_pretty(&self.scopes)?;
        atomic_write(&self.path.join(Self::SCOPES_FILE), json).await?;
        Ok(())
    }

    // ── Data keys (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Keys") ─────

    /// `doc_id`'s wrapped data keys as stored, if it has any.
    pub fn doc_keys(&self, doc_id: &str) -> Option<&DocKeys> {
        self.docs.get(doc_id).and_then(|doc| doc.keys.as_ref())
    }

    /// Store `json` as `doc_id`'s keys record (the caller has merged it;
    /// this crate only checks it carries a `dek_id`). A document that has
    /// no log yet gets its directory: keys can precede the first blob.
    pub async fn set_doc_keys(&mut self, doc_id: &str, json: String) -> Result<()> {
        let keys = DocKeys::parse(json).ok_or_else(|| StoreError::InvalidOperation(format!("document {}: a keys record needs a dek_id", doc_id)))?;
        let dir = self.doc_dir(doc_id);
        let doc = self.docs.entry(doc_id.to_string()).or_insert_with(|| VaultDoc::empty(dir));
        fs::create_dir_all(&doc.dir).await?;
        atomic_write(&doc.dir.join(VaultDoc::KEYS_FILE), &keys.json).await?;
        doc.keys = Some(keys);
        Ok(())
    }

    /// Append `blob` to `doc_id`'s log, returning its sequence number.
    /// Refused when `blob` exceeds [`MAX_BLOB_BYTES`], or when appending it
    /// would push the log past [`MAX_LOG_BYTES`] (`VaultSnapshotRequired`:
    /// the caller must upload a snapshot first).
    pub async fn append(&mut self, doc_id: &str, blob: Vec<u8>) -> Result<u64> {
        if blob.len() > MAX_BLOB_BYTES {
            return Err(StoreError::VaultBlobTooLarge { size: blob.len() });
        }

        let dir = self.doc_dir(doc_id);
        let doc = self.docs.entry(doc_id.to_string()).or_insert_with(|| VaultDoc::empty(dir));

        let entry_size = 12u64 + blob.len() as u64;
        if doc.log_size() + entry_size > MAX_LOG_BYTES {
            return Err(StoreError::VaultSnapshotRequired);
        }

        fs::create_dir_all(&doc.dir).await?;
        let log_path = doc.dir.join("log");
        let offset = fs::metadata(&log_path).await.map(|m| m.len()).unwrap_or(0);

        let seq = doc.head + 1;
        let mut record = Vec::with_capacity(entry_size as usize);
        record.extend_from_slice(&seq.to_le_bytes());
        record.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        record.extend_from_slice(&blob);

        let mut file = fs::OpenOptions::new().create(true).append(true).open(&log_path).await?;
        file.write_all(&record).await?;
        file.flush().await?;

        doc.entries.push(LogEntryMeta { seq, offset, len: blob.len() as u32 });
        doc.head = seq;
        Ok(seq)
    }

    /// The snapshot (if its seq is greater than `after_seq`), every update
    /// after `max(after_seq, snapshot.seq)` in ascending order, and the
    /// document's head (0 if it doesn't exist or is empty).
    pub async fn fetch(&self, doc_id: &str, after_seq: u64) -> Result<(Option<(u64, Vec<u8>)>, Vec<(u64, Vec<u8>)>, u64)> {
        let Some(doc) = self.docs.get(doc_id) else {
            return Ok((None, Vec::new(), 0));
        };

        let snapshot = doc.snapshot.clone().filter(|(seq, _)| *seq > after_seq);
        let baseline = snapshot.as_ref().map(|(seq, _)| *seq).unwrap_or(after_seq).max(after_seq);

        let mut updates = Vec::new();
        for entry in &doc.entries {
            if entry.seq > baseline {
                updates.push((entry.seq, doc.read_entry(entry).await?));
            }
        }

        Ok((snapshot, updates, doc.head))
    }

    /// Store a snapshot covering every update up to and including
    /// `upto_seq`, dropping log entries at or below it (rewriting the log
    /// atomically). Refused if `upto_seq` is beyond the document's head, or
    /// `blob` exceeds [`MAX_BLOB_BYTES`].
    pub async fn snapshot(&mut self, doc_id: &str, upto_seq: u64, blob: Vec<u8>) -> Result<()> {
        if blob.len() > MAX_BLOB_BYTES {
            return Err(StoreError::VaultBlobTooLarge { size: blob.len() });
        }

        let dir = self.doc_dir(doc_id);
        let doc = self.docs.entry(doc_id.to_string()).or_insert_with(|| VaultDoc::empty(dir));

        if upto_seq > doc.head {
            return Err(StoreError::InvalidOperation(format!(
                "upto_seq {} is beyond document {}'s head {}", upto_seq, doc_id, doc.head
            )));
        }
        // Not newer than the snapshot already held: keep that one. The log at
        // or below it is gone, so replacing it with an older device's state
        // would put a snapshot covering less in front of a log that no longer
        // has the difference. Not an error: two devices racing to snapshot is
        // ordinary, and the slower one has nothing to do.
        if let Some((held, _)) = &doc.snapshot {
            if *held >= upto_seq {
                return Ok(());
            }
        }

        fs::create_dir_all(&doc.dir).await?;
        let mut snapshot_bytes = Vec::with_capacity(8 + blob.len());
        snapshot_bytes.extend_from_slice(&upto_seq.to_le_bytes());
        snapshot_bytes.extend_from_slice(&blob);
        atomic_write(&doc.dir.join("snapshot"), &snapshot_bytes).await?;
        doc.snapshot = Some((upto_seq, blob));

        // Drop every entry at or below the new snapshot, rewriting the log
        // atomically with only what's left (reading each kept entry's bytes
        // from the *old* log before it's replaced).
        if doc.entries.iter().any(|e| e.seq <= upto_seq) {
            let keep: Vec<LogEntryMeta> = doc.entries.iter().copied().filter(|e| e.seq > upto_seq).collect();
            let mut new_log = Vec::new();
            let mut new_entries = Vec::with_capacity(keep.len());
            for entry in &keep {
                let bytes = doc.read_entry(entry).await?;
                let offset = new_log.len() as u64;
                new_log.extend_from_slice(&entry.seq.to_le_bytes());
                new_log.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                new_log.extend_from_slice(&bytes);
                new_entries.push(LogEntryMeta { seq: entry.seq, offset, len: bytes.len() as u32 });
            }
            atomic_write(&doc.dir.join("log"), &new_log).await?;
            doc.entries = new_entries;
        }

        Ok(())
    }

    /// Every document this store has, with its head, snapshot seq (0 = no
    /// snapshot) and data key id (when it has keys). Order is unspecified.
    pub fn list_docs(&self) -> Vec<VaultDocSummary> {
        self.docs
            .iter()
            .map(|(doc_id, doc)| VaultDocSummary {
                doc_id: doc_id.clone(),
                head: doc.head,
                snapshot_seq: doc.snapshot.as_ref().map(|(seq, _)| *seq).unwrap_or(0),
                dek_id: doc.keys.as_ref().map(|k| k.dek_id),
            })
            .collect()
    }

    /// Whether this store holds `doc_id` at all (a log, a snapshot or keys).
    pub fn has_doc(&self, doc_id: &str) -> bool {
        self.docs.contains_key(doc_id)
    }
}

/// One row of [`VaultStore::list_docs`].
#[derive(Debug, Clone)]
pub struct VaultDocSummary {
    pub doc_id: String,
    pub head: u64,
    pub snapshot_seq: u64,
    pub dek_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_and_fetch_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::create(dir.path().join("v.pimble"), "V", None).await.unwrap();

        let seq1 = store.append("tree", b"one".to_vec()).await.unwrap();
        let seq2 = store.append("tree", b"two".to_vec()).await.unwrap();
        assert_eq!((seq1, seq2), (1, 2));

        let (snapshot, updates, head) = store.fetch("tree", 0).await.unwrap();
        assert!(snapshot.is_none());
        assert_eq!(updates, vec![(1, b"one".to_vec()), (2, b"two".to_vec())]);
        assert_eq!(head, 2);

        let (_, updates, _) = store.fetch("tree", 1).await.unwrap();
        assert_eq!(updates, vec![(2, b"two".to_vec())]);
    }

    #[tokio::test]
    async fn snapshot_drops_old_entries_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        let store_id;
        {
            let mut store = VaultStore::create(&path, "V", None).await.unwrap();
            store_id = store.id;
            store.append("tree", b"one".to_vec()).await.unwrap();
            store.append("tree", b"two".to_vec()).await.unwrap();
            store.append("tree", b"three".to_vec()).await.unwrap();
            store.snapshot("tree", 2, b"snap-of-one-two".to_vec()).await.unwrap();

            let (snapshot, updates, head) = store.fetch("tree", 0).await.unwrap();
            assert_eq!(snapshot, Some((2, b"snap-of-one-two".to_vec())));
            assert_eq!(updates, vec![(3, b"three".to_vec())]);
            assert_eq!(head, 3);
        }

        let store = VaultStore::open(&path).await.unwrap();
        assert_eq!(store.id, store_id);
        let (snapshot, updates, head) = store.fetch("tree", 0).await.unwrap();
        assert_eq!(snapshot, Some((2, b"snap-of-one-two".to_vec())));
        assert_eq!(updates, vec![(3, b"three".to_vec())]);
        assert_eq!(head, 3);
    }

    #[tokio::test]
    async fn a_snapshot_not_newer_than_the_held_one_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::create(dir.path().join("v.pimble"), "V", None).await.unwrap();
        for i in 0..6u8 {
            store.append("doc", vec![i]).await.unwrap();
        }
        store.snapshot("doc", 4, b"newer".to_vec()).await.unwrap();
        // A slower device, whose state is older, arrives second.
        store.snapshot("doc", 2, b"older".to_vec()).await.unwrap();
        store.snapshot("doc", 4, b"same seq, other device".to_vec()).await.unwrap();

        let (snapshot, updates, head) = store.fetch("doc", 0).await.unwrap();
        assert_eq!(snapshot, Some((4, b"newer".to_vec())));
        assert_eq!(updates.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(), vec![5, 6]);
        assert_eq!(head, 6);
    }

    #[tokio::test]
    async fn snapshot_refuses_upto_seq_beyond_head() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::create(dir.path().join("v.pimble"), "V", None).await.unwrap();
        store.append("tree", b"one".to_vec()).await.unwrap();

        let err = store.snapshot("tree", 5, b"nope".to_vec()).await.unwrap_err();
        assert!(matches!(err, StoreError::InvalidOperation(_)));
    }

    #[tokio::test]
    async fn append_refuses_an_oversized_blob() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::create(dir.path().join("v.pimble"), "V", None).await.unwrap();
        let big = vec![0u8; MAX_BLOB_BYTES + 1];
        let err = store.append("tree", big).await.unwrap_err();
        assert!(matches!(err, StoreError::VaultBlobTooLarge { .. }));
    }

    #[tokio::test]
    async fn a_truncated_trailing_log_record_is_dropped_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        {
            let mut store = VaultStore::create(&path, "V", None).await.unwrap();
            store.append("tree", b"one".to_vec()).await.unwrap();
        }

        // Simulate a crash mid-write: append a truncated record's header (a
        // seq/len with no, or partial, blob bytes) directly to the log file.
        let log_path = path.join("vault").join("tree").join("log");
        let mut bytes = std::fs::read(&log_path).unwrap();
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(&100u32.to_le_bytes()); // claims 100 bytes of blob
        bytes.extend_from_slice(b"short"); // far fewer than declared
        std::fs::write(&log_path, &bytes).unwrap();

        let store = VaultStore::open(&path).await.unwrap();
        let (_, updates, head) = store.fetch("tree", 0).await.unwrap();
        assert_eq!(updates, vec![(1, b"one".to_vec())], "the truncated record must be dropped, not the good one before it");
        assert_eq!(head, 1);

        // And the log file on disk was rewritten without the truncated tail.
        let rewritten = std::fs::read(&log_path).unwrap();
        assert!(rewritten.len() < bytes.len());
    }

    #[tokio::test]
    async fn create_with_a_chosen_id_uses_it() {
        let dir = tempfile::tempdir().unwrap();
        let id = StoreId::new();
        let store = VaultStore::create(dir.path().join("v.pimble"), "V", Some(id)).await.unwrap();
        assert_eq!(store.id, id);
        assert_eq!(store.manifest().id, id);
    }

    /// Scope sets (docs/NODE_DOCUMENT_CONTRACT.md section 5) survive a reopen,
    /// a root is in its own scope whether listed or not, and a scoped member's
    /// create joins exactly the scopes whose set holds the parent.
    #[tokio::test]
    async fn scopes_persist_and_extend_where_the_parent_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        let (r1, r2, a, b, c) = (NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new());
        {
            let mut store = VaultStore::create(&path, "V", None).await.unwrap();
            store.set_scope(r1, [a]).await.unwrap();
            store.set_scope(r2, [b]).await.unwrap();
            assert_eq!(store.scope_union(&[r1]), [r1, a].into_iter().collect());
            assert_eq!(store.scope_union(&[r1, r2]), [r1, r2, a, b].into_iter().collect());
            assert_eq!(store.scope_union(&[NodeId::new()]).len(), 1, "an unpublished root reaches only itself");

            // `c` created under `a`: joins r1's scope, not r2's.
            assert!(store.extend_scopes(&[r1, r2], a, c).await.unwrap());
            assert!(store.scope_union(&[r1]).contains(&c));
            assert!(!store.scope_union(&[r2]).contains(&c));
            // A parent in no scope of the member's changes nothing.
            assert!(!store.extend_scopes(&[r2], a, NodeId::new()).await.unwrap());
        }
        let mut store = VaultStore::open(&path).await.unwrap();
        assert_eq!(store.scope_union(&[r1, r2]), [r1, r2, a, b, c].into_iter().collect());
        store.remove_scope(r2).await.unwrap();
        assert_eq!(store.scopes().len(), 1);
        let store = VaultStore::open(&path).await.unwrap();
        assert_eq!(store.scopes().len(), 1);
    }

    /// A member's create outlives a publish from an owner's device that has
    /// not seen the new document yet, and is the owner's to remove once a
    /// publish has named it; a root nobody has published for still takes
    /// its members' creates.
    #[tokio::test]
    async fn a_members_create_stays_in_scope_until_a_publish_has_named_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        let (root, a, created, unpublished_root, under_it) = (NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new());
        let mut store = VaultStore::create(&path, "V", None).await.unwrap();
        store.set_scope(root, [a]).await.unwrap();
        assert!(store.extend_scopes(&[root], a, created).await.unwrap());

        // A stale publish (the owner's device has not pulled `created`).
        store.set_scope(root, [a]).await.unwrap();
        assert!(store.scope_union(&[root]).contains(&created), "not dropped by a publish that never knew it");
        let store = VaultStore::open(&path).await.unwrap();
        assert!(store.scope_union(&[root]).contains(&created), "and it survives a restart");
        let listed: HashSet<NodeId> = store.scopes().into_iter().find(|(r, _)| *r == root).unwrap().1.into_iter().collect();
        assert_eq!(listed, [a, created].into_iter().collect());

        // A publish that names it confirms it; the next one without it is
        // the owner moving it out.
        let mut store = store;
        store.set_scope(root, [a, created]).await.unwrap();
        store.set_scope(root, [a]).await.unwrap();
        assert!(!store.scope_union(&[root]).contains(&created));

        assert!(store.extend_scopes(&[unpublished_root], unpublished_root, under_it).await.unwrap());
        assert_eq!(store.scope_union(&[unpublished_root]), [unpublished_root, under_it].into_iter().collect());
        store.remove_scope(unpublished_root).await.unwrap();
        assert_eq!(store.scope_union(&[unpublished_root]).len(), 1);
    }

    /// A document's keys record is kept as the text it arrived as, listed by
    /// its `dek_id`, and can precede the document's first blob.
    #[tokio::test]
    async fn doc_keys_round_trip_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        let dek_id = Uuid::new_v4();
        let json = format!(r#"{{"dek_id":"{dek_id}","wraps":[]}}"#);
        {
            let mut store = VaultStore::create(&path, "V", None).await.unwrap();
            assert!(matches!(store.set_doc_keys("doc", "{}".into()).await, Err(StoreError::InvalidOperation(_))));
            store.set_doc_keys("doc", json.clone()).await.unwrap();
            assert!(store.has_doc("doc"));
            assert_eq!(store.doc_keys("doc").unwrap().json, json);
            let (_, _, head) = store.fetch("doc", 0).await.unwrap();
            assert_eq!(head, 0, "keys are not a log entry");
        }
        let store = VaultStore::open(&path).await.unwrap();
        let listed = store.list_docs();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].dek_id, Some(dek_id));
        assert_eq!(store.doc_keys("doc").unwrap().dek_id, dek_id);
        assert!(store.doc_keys("other").is_none());
    }
}
