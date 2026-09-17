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
//! ```
//!
//! An in-memory index of each log entry's byte offset and length is built at
//! open (and kept current on every append/snapshot), so fetching a range of
//! updates reads exactly those bytes rather than rescanning the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pimble_core::{StoreId, StoreKind, StoreManifest};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::{info, warn};

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
}

impl VaultDoc {
    fn empty(dir: PathBuf) -> Self {
        Self { dir, entries: Vec::new(), snapshot: None, head: 0 }
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

        Ok(Self { dir, entries, snapshot, head })
    }

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
}

impl VaultStore {
    const MANIFEST_FILE: &'static str = "manifest.json";
    const VAULT_DIR: &'static str = "vault";

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
        Ok(Self { id: manifest.id, path, manifest, docs: HashMap::new() })
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

        info!("Opened vault store '{}' ({}) from {:?}", manifest.name, manifest.id, path);
        Ok(Self { id: manifest.id, path, manifest, docs })
    }

    pub fn manifest(&self) -> &StoreManifest {
        &self.manifest
    }

    fn doc_dir(&self, doc_id: &str) -> PathBuf {
        self.path.join(Self::VAULT_DIR).join(doc_id)
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

    /// Every document this store has, with its head and snapshot seq (0 = no
    /// snapshot). Order is unspecified.
    pub fn list_docs(&self) -> Vec<(String, u64, u64)> {
        self.docs
            .iter()
            .map(|(doc_id, doc)| (doc_id.clone(), doc.head, doc.snapshot.as_ref().map(|(seq, _)| *seq).unwrap_or(0)))
            .collect()
    }
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
}
