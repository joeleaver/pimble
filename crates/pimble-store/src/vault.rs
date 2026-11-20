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
//! <store>/vault/{doc_id}/log       # append-only: seq u64 | len u32 | [part] | blob
//! <store>/vault/{doc_id}/snapshot  # seq u64 | blob, or seq u64 | "PP" | count u32 | group (16) | (len u32 | blob)*
//! <store>/vault/{doc_id}/keys.json # the document's wrapped data keys, opaque here
//! <store>/vault/{doc_id}/reports   # each device's sealed state report, opaque here
//! <store>/scopes.json              # the store's published scope sets
//! ```
//!
//! An in-memory index of each log entry's byte offset and length is built at
//! open (and kept current on every append/snapshot), so fetching a range of
//! updates reads exactly those bytes rather than rescanning the file.
//!
//! **Parts** (docs/SYNC_LIMITS_CONTRACT.md): a snapshot or an append too
//! large for one blob arrives as parts, each a blob of its own. They are held
//! aside in memory as they come ([`VaultStore::append_part`],
//! [`VaultStore::snapshot_part`]) and land only when the last one does: an
//! append's parts as consecutive log entries, each record carrying its part
//! (the high bit of `len` says a 24-byte `index u32 | count u32 | group` header
//! follows), written in one write; a snapshot's as one `snapshot` file that
//! replaces the old one atomically, so a half-uploaded snapshot never replaces
//! a whole one. Every limit applies per part. A log whose tail is a group cut
//! short (a crash mid-write) loses that tail at open, as a truncated record
//! does: its sender was never answered and sends it again.
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
//!
//! **State reports** (docs/CONVERGENCE_CONTRACT.md section 1): beside a
//! document's log, one slot per device (`reports`, a JSON map of device id to
//! the sealed blob, base64, and when it was stored), overwritten by each new
//! report from that device. What a report says (the state vector the device
//! holds of the document) is sealed under the document's data key like any
//! blob; the server keeps the latest per device and hands them back. Reports
//! live in the document's directory, so they go when the document does.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use pimble_core::{BlobId, NodeId, StoreId, StoreKind, StoreManifest};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::{Result, StoreError};

/// One blob's maximum size, and the maximum a document's log may reach
/// before a snapshot is required (docs/CRYPTO_CONTRACT.md "RPCs"). Guards
/// against runaway growth only: a document that outgrows them must still
/// sync (Joe, 2026-11-18, after a 4.9 MB canvas filled its 64 MiB log and
/// its snapshot was refused at the old 4 MiB, stopping the store's sync).
const MAX_BLOB_BYTES: usize = 256 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 1024 * 1024 * 1024;

static BLOB_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(MAX_BLOB_BYTES);
static LOG_LIMIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(MAX_LOG_BYTES);

/// Lower the limits for this process, for tests that reach them.
#[doc(hidden)]
pub fn set_limits_for_tests(blob: usize, log: u64) {
    BLOB_LIMIT.store(blob, std::sync::atomic::Ordering::SeqCst);
    LOG_LIMIT.store(log, std::sync::atomic::Ordering::SeqCst);
}

/// The most a state report's blob may be (docs/CONVERGENCE_CONTRACT.md
/// section 1): a report is a state vector, about 20 bytes per client that
/// ever wrote the document, so this is a runaway guard only.
pub const MAX_REPORT_BYTES: usize = 64 * 1024;
/// The most devices a document keeps a report from; past it, the oldest
/// report goes. A device that stopped reporting long ago (a replica removed,
/// a browser cleared) is the one dropped.
pub const MAX_REPORTS_PER_DOC: usize = 32;
/// The longest device id a report may name (a UUID is 36).
pub const MAX_DEVICE_ID_BYTES: usize = 64;

fn blob_limit() -> usize {
    BLOB_LIMIT.load(std::sync::atomic::Ordering::SeqCst)
}

fn log_limit() -> u64 {
    LOG_LIMIT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Write a file atomically: write to a `.tmp` sibling, then rename into
/// place. Mirrors `local::atomic_write` (not reused directly: that function
/// is private to this crate's `local` module).
async fn atomic_write(path: &Path, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, data).await?;
    fs::rename(&tmp_path, path).await?;
    Ok(())
}

/// Which part of a blob sealed in parts an entry or a snapshot blob is
/// (`pimble_rpc::VaultPart`, which this crate does not depend on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PartMeta {
    pub index: u32,
    pub count: u32,
    pub group: Uuid,
}

impl PartMeta {
    const LEN: usize = 4 + 4 + 16;

    fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..4].copy_from_slice(&self.index.to_le_bytes());
        out[4..8].copy_from_slice(&self.count.to_le_bytes());
        out[8..24].copy_from_slice(self.group.as_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let index = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?);
        let count = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
        let group = Uuid::from_slice(bytes.get(8..24)?).ok()?;
        Some(Self { index, count, group })
    }
}

/// A log record's `len` with this bit set is followed by a [`PartMeta`].
const PART_FLAG: u32 = 1 << 31;

/// How long the parts of a group may sit unfinished before they are let go.
const PENDING_FOR: Duration = Duration::from_secs(15 * 60);

/// One log entry's location within `log`, without its bytes.
#[derive(Debug, Clone, Copy)]
struct LogEntryMeta {
    seq: u64,
    /// Byte offset of this entry's header (`seq | len`) within the log file.
    offset: u64,
    len: u32,
    part: Option<PartMeta>,
}

impl LogEntryMeta {
    /// The record's header length: `seq | len`, and the part when it has one.
    fn header_len(&self) -> u64 {
        12 + if self.part.is_some() { PartMeta::LEN as u64 } else { 0 }
    }

    fn record(seq: u64, part: Option<PartMeta>, blob: &[u8]) -> Vec<u8> {
        let mut record = Vec::with_capacity(12 + PartMeta::LEN + blob.len());
        record.extend_from_slice(&seq.to_le_bytes());
        let len = blob.len() as u32 | if part.is_some() { PART_FLAG } else { 0 };
        record.extend_from_slice(&len.to_le_bytes());
        if let Some(part) = part {
            record.extend_from_slice(&part.encode());
        }
        record.extend_from_slice(blob);
        record
    }
}

/// A document's snapshot: one blob, or every part of one group in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshot {
    pub seq: u64,
    pub blobs: Vec<Vec<u8>>,
    /// The group of a snapshot stored in parts; `None` for one blob.
    pub group: Option<Uuid>,
}

impl StoredSnapshot {
    const PARTS_MAGIC: &'static [u8; 2] = b"PP";

    fn bytes(&self) -> u64 {
        self.blobs.iter().map(|b| b.len() as u64).sum()
    }

    /// Each blob with the part it is (none for a whole snapshot).
    pub fn parts(&self) -> Vec<(Option<PartMeta>, &[u8])> {
        let count = self.blobs.len() as u32;
        self.blobs
            .iter()
            .enumerate()
            .map(|(index, blob)| (self.group.map(|group| PartMeta { index: index as u32, count, group }), blob.as_slice()))
            .collect()
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.bytes() as usize + 24 + 4 * self.blobs.len());
        out.extend_from_slice(&self.seq.to_le_bytes());
        match self.group {
            None => out.extend_from_slice(&self.blobs[0]),
            Some(group) => {
                out.extend_from_slice(Self::PARTS_MAGIC);
                out.extend_from_slice(&(self.blobs.len() as u32).to_le_bytes());
                out.extend_from_slice(group.as_bytes());
                for blob in &self.blobs {
                    out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
                    out.extend_from_slice(blob);
                }
            }
        }
        out
    }

    /// A whole snapshot's blob starts `PB` (every `pimble_crypto::Blob`
    /// does); one in parts starts `PP`.
    fn decode(bytes: &[u8]) -> Option<Self> {
        let seq = u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?);
        let body = &bytes[8..];
        if body.get(0..2) != Some(Self::PARTS_MAGIC.as_slice()) {
            return Some(Self { seq, blobs: vec![body.to_vec()], group: None });
        }
        let count = u32::from_le_bytes(body.get(2..6)?.try_into().ok()?) as usize;
        let group = Uuid::from_slice(body.get(6..22)?).ok()?;
        let mut at = 22;
        let mut blobs = Vec::with_capacity(count);
        for _ in 0..count {
            let len = u32::from_le_bytes(body.get(at..at + 4)?.try_into().ok()?) as usize;
            blobs.push(body.get(at + 4..at + 4 + len)?.to_vec());
            at += 4 + len;
        }
        Some(Self { seq, blobs, group: Some(group) })
    }
}

/// One log entry as a fetch hands it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEntry {
    pub seq: u64,
    pub blob: Vec<u8>,
    pub part: Option<PartMeta>,
}

/// What [`VaultStore::fetch`] answers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fetched {
    pub snapshot: Option<StoredSnapshot>,
    pub updates: Vec<StoredEntry>,
    pub head: u64,
}

/// What a part being held aside belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    Append,
    /// A snapshot through `upto_seq`; `ignored` when it was not newer than
    /// the snapshot held when its first part came (its parts are taken and
    /// dropped, as a whole snapshot that is not newer is).
    Snapshot { upto_seq: u64, ignored: bool },
}

/// The parts of one group received so far.
struct Pending {
    kind: PendingKind,
    count: u32,
    blobs: Vec<Vec<u8>>,
    bytes: u64,
    touched: Instant,
}

/// What a part of an append came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartAppended {
    /// Held aside until the rest of its group comes.
    Held,
    /// The group's last part: every part is in the log now, numbered
    /// consecutively, each with its blob and part.
    Landed(Vec<StoredEntry>),
}

/// One document's in-memory state: every log entry currently on disk (in
/// ascending seq order) and the current snapshot, if any.
struct VaultDoc {
    dir: PathBuf,
    entries: Vec<LogEntryMeta>,
    snapshot: Option<StoredSnapshot>,
    /// The latest sequence number ever handed out (0 = nothing appended
    /// yet). Distinct from "the highest seq still in `entries`": a snapshot
    /// can drop every entry without lowering this.
    head: u64,
    /// The document's wrapped data keys as `keys.json` holds them (see the
    /// module doc), with the `dek_id` read out of them once at load.
    keys: Option<DocKeys>,
    /// Each device's latest state report (see the module doc), as the
    /// `reports` file holds them.
    reports: HashMap<String, StoredReport>,
}

/// One device's state report of a document, as stored: the sealed blob and
/// when it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReport {
    pub device: String,
    pub blob: Vec<u8>,
    pub at: DateTime<Utc>,
}

/// One slot of the `reports` file.
#[derive(Serialize, Deserialize)]
struct ReportRecord {
    blob: String,
    at: DateTime<Utc>,
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
    /// The discussions among the store's documents, each with the node it
    /// is on (docs/DISCUSSIONS_CONTRACT.md section 7): which documents a
    /// commenter may write. Published by the owner's devices and added to by
    /// the server when a member starts one; it only grows.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    discussions: HashMap<NodeId, NodeId>,
    /// The pictures each scope's documents name (docs/IMAGES_CONTRACT.md
    /// "Replication"), published by the owner's devices and added to by the
    /// server when a member uploads one for a node they may write. Only
    /// grows, until the scope is removed.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    blobs: HashMap<NodeId, HashSet<BlobId>>,
}

impl ScopesFile {
    const VERSION: u8 = 1;
}

impl VaultDoc {
    fn empty(dir: PathBuf) -> Self {
        Self { dir, entries: Vec::new(), snapshot: None, head: 0, keys: None, reports: HashMap::new() }
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
                let decoded = if bytes.len() >= 8 { StoredSnapshot::decode(&bytes) } else { None };
                if decoded.is_none() {
                    warn!("Vault snapshot {:?} is too short or malformed; ignoring it", snapshot_path);
                }
                decoded
            } else {
                None
            }
        };

        let mut head = snapshot.as_ref().map(|snapshot| snapshot.seq).unwrap_or(0);
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
                let raw_len = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
                let (len, part) = if raw_len & PART_FLAG != 0 {
                    match bytes.get(offset + 12..offset + 12 + PartMeta::LEN).and_then(PartMeta::decode) {
                        Some(part) => (raw_len & !PART_FLAG, Some(part)),
                        None => {
                            truncated = true;
                            break;
                        }
                    }
                } else {
                    (raw_len, None)
                };
                let meta = LogEntryMeta { seq, offset: offset as u64, len, part };
                let body_start = offset + meta.header_len() as usize;
                let body_end = body_start + len as usize;
                if body_end > bytes.len() {
                    truncated = true;
                    break;
                }
                entries.push(meta);
                offset = body_end;
            }
            // A group whose last parts never reached the disk (a crash in the
            // middle of its one write) is cut off with the record that was:
            // nobody could ever apply it, and every reader's cursor would
            // stop in front of it for good. Its sender had no answer and
            // sends it again.
            if let Some(last) = entries.last().and_then(|e| e.part) {
                let run = entries.iter().rev().take_while(|e| e.part.is_some_and(|p| p.group == last.group)).count();
                if run != last.count as usize {
                    let cut = entries.len() - run;
                    offset = entries[cut].offset as usize;
                    entries.truncate(cut);
                    truncated = true;
                }
            }
            head = entries.iter().map(|e| e.seq).fold(head, u64::max);
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

        let reports = Self::load_reports(&dir.join(Self::REPORTS_FILE)).await;

        Ok(Self { dir, entries, snapshot, head, keys, reports })
    }

    const KEYS_FILE: &'static str = "keys.json";
    const REPORTS_FILE: &'static str = "reports";

    /// The `reports` file, or nothing when it is absent or does not parse
    /// (with a warning): every device reports again at its next connect, so
    /// a lost file costs one round of reports and never the document.
    async fn load_reports(path: &Path) -> HashMap<String, StoredReport> {
        let Ok(json) = fs::read_to_string(path).await else { return HashMap::new() };
        let records: HashMap<String, ReportRecord> = match serde_json::from_str(&json) {
            Ok(records) => records,
            Err(e) => {
                warn!("Vault reports file {:?} could not be parsed ({}); starting with none", path, e);
                return HashMap::new();
            }
        };
        records
            .into_iter()
            .filter_map(|(device, record)| {
                let blob = STANDARD.decode(&record.blob).ok()?;
                Some((device.clone(), StoredReport { device, blob, at: record.at }))
            })
            .collect()
    }

    async fn save_reports(&self) -> Result<()> {
        let records: HashMap<&str, ReportRecord> = self
            .reports
            .iter()
            .map(|(device, report)| (device.as_str(), ReportRecord { blob: STANDARD.encode(&report.blob), at: report.at }))
            .collect();
        atomic_write(&self.dir.join(Self::REPORTS_FILE), serde_json::to_vec(&records)?).await?;
        Ok(())
    }

    /// The bytes of one already-recorded log entry, read fresh from disk.
    async fn read_entry(&self, entry: &LogEntryMeta) -> Result<Vec<u8>> {
        let mut file = fs::File::open(self.dir.join("log")).await?;
        file.seek(std::io::SeekFrom::Start(entry.offset + entry.header_len())).await?;
        let mut buf = vec![0u8; entry.len as usize];
        file.read_exact(&mut buf).await?;
        Ok(buf)
    }

    /// Every log entry's total on-disk size (header plus blob).
    fn log_size(&self) -> u64 {
        self.entries.iter().map(|e| e.header_len() + e.len as u64).sum()
    }

    fn snapshot_size(&self) -> u64 {
        self.snapshot.as_ref().map(StoredSnapshot::bytes).unwrap_or(0)
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
    /// The parts of groups still arriving, by document and group (see the
    /// module doc, "Parts"). In memory only: a server that restarts in the
    /// middle of one answers its next part with an error, and the sender
    /// starts the group again.
    pending: HashMap<(String, Uuid), Pending>,
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
        Ok(Self { id: manifest.id, path, manifest, docs: HashMap::new(), scopes: ScopesFile::default(), pending: HashMap::new() })
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
        Ok(Self { id: manifest.id, path, manifest, docs, scopes, pending: HashMap::new() })
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
        let pictures = self.scopes.blobs.remove(&root).is_some();
        if !published && !unconfirmed && !pictures {
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

    /// The pictures a principal scoped to `roots` may read.
    pub fn scope_blobs(&self, roots: &[NodeId]) -> HashSet<BlobId> {
        roots.iter().filter_map(|root| self.scopes.blobs.get(root)).flatten().copied().collect()
    }

    /// Add pictures to `root`'s scope (the owner's publish), and persist
    /// when that adds anything. Returns whether it did.
    pub async fn add_scope_blobs(&mut self, root: NodeId, blobs: impl IntoIterator<Item = BlobId>) -> Result<bool> {
        let set = self.scopes.blobs.entry(root).or_default();
        let before = set.len();
        set.extend(blobs);
        let changed = set.len() != before;
        if set.is_empty() {
            self.scopes.blobs.remove(&root);
        }
        if changed {
            self.write_scopes().await?;
        }
        Ok(changed)
    }

    /// A scoped member uploaded `blob` for `node_id`, a node they may write:
    /// it joins every one of the member's scopes (`roots`) that holds the
    /// node. Returns whether any scope changed.
    pub async fn extend_scope_blobs(&mut self, roots: &[NodeId], node_id: NodeId, blob: BlobId) -> Result<bool> {
        let mut changed = false;
        for root in roots {
            if self.scope_union(&[*root]).contains(&node_id) {
                changed |= self.scopes.blobs.entry(*root).or_default().insert(blob);
            }
        }
        if changed {
            self.write_scopes().await?;
        }
        Ok(changed)
    }

    /// Whether `doc_id` is a discussion (see [`ScopesFile::discussions`]).
    pub fn is_discussion(&self, doc_id: NodeId) -> bool {
        self.scopes.discussions.contains_key(&doc_id)
    }

    /// Record discussions and the nodes they are on (the owner's publish, or
    /// a member's new one), and persist when that adds anything. A
    /// discussion's host never changes here: it only grows.
    pub async fn add_discussions(&mut self, discussions: impl IntoIterator<Item = (NodeId, NodeId)>) -> Result<bool> {
        let mut changed = false;
        for (doc, host) in discussions {
            if !self.scopes.discussions.contains_key(&doc) {
                self.scopes.discussions.insert(doc, host);
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
        let landed = self.append_records(doc_id, vec![(None, blob)]).await?;
        Ok(landed[0].seq)
    }

    /// Append records in one write, numbered consecutively from the head.
    /// Every limit applies per record; the log limit to them all together.
    async fn append_records(&mut self, doc_id: &str, blobs: Vec<(Option<PartMeta>, Vec<u8>)>) -> Result<Vec<StoredEntry>> {
        if let Some((_, blob)) = blobs.iter().find(|(_, blob)| blob.len() > blob_limit()) {
            return Err(StoreError::VaultBlobTooLarge { size: blob.len() });
        }

        let dir = self.doc_dir(doc_id);
        let doc = self.docs.entry(doc_id.to_string()).or_insert_with(|| VaultDoc::empty(dir));

        let size: u64 = blobs.iter().map(|(part, blob)| 12 + if part.is_some() { PartMeta::LEN as u64 } else { 0 } + blob.len() as u64).sum();
        if doc.log_size() + size > log_limit() {
            return Err(StoreError::VaultSnapshotRequired);
        }

        fs::create_dir_all(&doc.dir).await?;
        let log_path = doc.dir.join("log");
        let mut offset = fs::metadata(&log_path).await.map(|m| m.len()).unwrap_or(0);

        let mut records = Vec::with_capacity(size as usize);
        let mut metas = Vec::with_capacity(blobs.len());
        let mut landed = Vec::with_capacity(blobs.len());
        let mut seq = doc.head;
        for (part, blob) in blobs {
            seq += 1;
            let record = LogEntryMeta::record(seq, part, &blob);
            metas.push(LogEntryMeta { seq, offset, len: blob.len() as u32, part });
            offset += record.len() as u64;
            records.extend_from_slice(&record);
            landed.push(StoredEntry { seq, blob, part });
        }

        let mut file = fs::OpenOptions::new().create(true).append(true).open(&log_path).await?;
        file.write_all(&records).await?;
        file.flush().await?;

        doc.entries.extend(metas);
        doc.head = seq;
        Ok(landed)
    }

    /// Let go of groups nobody has sent a part of for [`PENDING_FOR`].
    fn prune_pending(&mut self) {
        self.pending.retain(|_, pending| pending.touched.elapsed() < PENDING_FOR);
    }

    /// Take one part of a group, in order: part 0 starts (or starts over) the
    /// group, and every later part must be the next one, of the same count
    /// and kind. Answers the group's blobs once its last part is in, taking
    /// the group out of `pending`.
    fn take_part(&mut self, doc_id: &str, part: PartMeta, kind: PendingKind, blob: Vec<u8>) -> Result<Option<(PendingKind, Vec<Vec<u8>>)>> {
        self.prune_pending();
        if part.count == 0 || part.index >= part.count {
            return Err(StoreError::InvalidOperation(format!("part {} of {} makes no sense", part.index, part.count)));
        }
        if blob.len() > blob_limit() {
            return Err(StoreError::VaultBlobTooLarge { size: blob.len() });
        }
        let key = (doc_id.to_string(), part.group);
        if part.index == 0 {
            self.pending.insert(key.clone(), Pending { kind, count: part.count, blobs: Vec::new(), bytes: 0, touched: Instant::now() });
        }
        let Some(pending) = self.pending.get_mut(&key) else {
            return Err(StoreError::InvalidOperation(format!(
                "part {} of {} of a group this server does not hold (the first part never came, or the server restarted): send the group again from its first part",
                part.index, part.count
            )));
        };
        let same_kind = match (pending.kind, kind) {
            (PendingKind::Append, PendingKind::Append) => true,
            (PendingKind::Snapshot { upto_seq: a, .. }, PendingKind::Snapshot { upto_seq: b, .. }) => a == b,
            _ => false,
        };
        if !same_kind || pending.count != part.count || pending.blobs.len() != part.index as usize {
            self.pending.remove(&key);
            return Err(StoreError::InvalidOperation(format!(
                "part {} of {} arrived out of order: send the group again from its first part",
                part.index, part.count
            )));
        }
        pending.touched = Instant::now();
        let ignored = matches!(pending.kind, PendingKind::Snapshot { ignored: true, .. });
        pending.bytes += blob.len() as u64;
        // A snapshot that will not be stored keeps nothing of what it sends.
        pending.blobs.push(if ignored { Vec::new() } else { blob });
        if (pending.blobs.len() as u32) < pending.count {
            return Ok(None);
        }
        let pending = self.pending.remove(&key).expect("held above");
        Ok(Some((pending.kind, pending.blobs)))
    }

    /// Take one part of an append sent in parts (see the module doc,
    /// "Parts"). The log limit is judged with the parts held so far, so a
    /// group that would not fit is refused at the part that would overflow
    /// it (`VaultSnapshotRequired`), and the group is let go.
    pub async fn append_part(&mut self, doc_id: &str, part: PartMeta, blob: Vec<u8>) -> Result<PartAppended> {
        let held: u64 = self.pending.get(&(doc_id.to_string(), part.group)).filter(|_| part.index > 0).map(|p| p.bytes + p.blobs.len() as u64 * (12 + PartMeta::LEN as u64)).unwrap_or(0);
        let log = self.docs.get(doc_id).map(VaultDoc::log_size).unwrap_or(0);
        if log + held + 12 + PartMeta::LEN as u64 + blob.len() as u64 > log_limit() {
            self.pending.remove(&(doc_id.to_string(), part.group));
            return Err(StoreError::VaultSnapshotRequired);
        }
        let Some((_, blobs)) = self.take_part(doc_id, part, PendingKind::Append, blob)? else {
            return Ok(PartAppended::Held);
        };
        let count = blobs.len() as u32;
        let records = blobs.into_iter().enumerate().map(|(index, blob)| (Some(PartMeta { index: index as u32, count, group: part.group }), blob)).collect();
        Ok(PartAppended::Landed(self.append_records(doc_id, records).await?))
    }

    /// Whether `part` would be the last of its group, every earlier part
    /// held: what the server checks before it stores the keys a new
    /// document's first append carries, which must land with its blobs.
    pub fn completes_group(&self, doc_id: &str, part: PartMeta) -> bool {
        part.index + 1 == part.count
            && (part.index == 0 || self.pending.get(&(doc_id.to_string(), part.group)).is_some_and(|p| p.count == part.count && p.blobs.len() as u32 == part.index))
    }

    /// The document's log and snapshot sizes, in bytes (what a client
    /// compacts by, docs/SYNC_LIMITS_CONTRACT.md "Compaction by size").
    pub fn sizes(&self, doc_id: &str) -> (u64, u64) {
        self.docs.get(doc_id).map(|doc| (doc.log_size(), doc.snapshot_size())).unwrap_or((0, 0))
    }

    /// The snapshot (if its seq is greater than `after_seq`), every update
    /// after `max(after_seq, snapshot.seq)` in ascending order, and the
    /// document's head (0 if it doesn't exist or is empty). A range that
    /// would start inside a group starts at its first part: a group is
    /// applied whole or not at all.
    pub async fn fetch(&self, doc_id: &str, after_seq: u64) -> Result<Fetched> {
        let Some(doc) = self.docs.get(doc_id) else {
            return Ok(Fetched::default());
        };

        let snapshot = doc.snapshot.clone().filter(|snapshot| snapshot.seq > after_seq);
        let baseline = snapshot.as_ref().map(|snapshot| snapshot.seq).unwrap_or(after_seq).max(after_seq);

        let mut start = doc.entries.iter().position(|e| e.seq > baseline).unwrap_or(doc.entries.len());
        if let Some(part) = doc.entries.get(start).and_then(|e| e.part) {
            while start > 0 && doc.entries[start - 1].part.is_some_and(|p| p.group == part.group) {
                start -= 1;
            }
        }
        let mut updates = Vec::new();
        for entry in &doc.entries[start..] {
            updates.push(StoredEntry { seq: entry.seq, blob: doc.read_entry(entry).await?, part: entry.part });
        }

        Ok(Fetched { snapshot, updates, head: doc.head })
    }

    /// Store a snapshot covering every update up to and including
    /// `upto_seq`, dropping log entries at or below it (rewriting the log
    /// atomically). Refused if `upto_seq` is beyond the document's head, or
    /// `blob` exceeds [`MAX_BLOB_BYTES`].
    pub async fn snapshot(&mut self, doc_id: &str, upto_seq: u64, blob: Vec<u8>) -> Result<()> {
        if blob.len() > blob_limit() {
            return Err(StoreError::VaultBlobTooLarge { size: blob.len() });
        }
        self.store_snapshot(doc_id, StoredSnapshot { seq: upto_seq, blobs: vec![blob], group: None }).await
    }

    /// Take one part of a snapshot sent in parts. The snapshot held is
    /// replaced only when the last part arrives (answering `true`), so a
    /// half-uploaded one never replaces a whole one. Its first part is
    /// judged as a whole snapshot is: beyond the head is refused, not newer
    /// than the one held is taken and dropped.
    pub async fn snapshot_part(&mut self, doc_id: &str, upto_seq: u64, part: PartMeta, blob: Vec<u8>) -> Result<bool> {
        let mut ignored = false;
        if part.index == 0 {
            let head = self.docs.get(doc_id).map(|doc| doc.head).unwrap_or(0);
            if upto_seq > head {
                return Err(StoreError::InvalidOperation(format!("upto_seq {} is beyond document {}'s head {}", upto_seq, doc_id, head)));
            }
            ignored = self.docs.get(doc_id).and_then(|doc| doc.snapshot.as_ref()).is_some_and(|held| held.seq >= upto_seq);
        }
        let Some((kind, blobs)) = self.take_part(doc_id, part, PendingKind::Snapshot { upto_seq, ignored }, blob)? else {
            return Ok(false);
        };
        if matches!(kind, PendingKind::Snapshot { ignored: true, .. }) {
            return Ok(false);
        }
        self.store_snapshot(doc_id, StoredSnapshot { seq: upto_seq, blobs, group: Some(part.group) }).await?;
        Ok(true)
    }

    async fn store_snapshot(&mut self, doc_id: &str, snapshot: StoredSnapshot) -> Result<()> {
        let upto_seq = snapshot.seq;
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
        if let Some(held) = &doc.snapshot {
            if held.seq >= upto_seq {
                return Ok(());
            }
        }

        fs::create_dir_all(&doc.dir).await?;
        atomic_write(&doc.dir.join("snapshot"), snapshot.encode()).await?;
        doc.snapshot = Some(snapshot);

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
                new_log.extend_from_slice(&LogEntryMeta::record(entry.seq, entry.part, &bytes));
                new_entries.push(LogEntryMeta { seq: entry.seq, offset, len: bytes.len() as u32, part: entry.part });
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
                snapshot_seq: doc.snapshot.as_ref().map(|snapshot| snapshot.seq).unwrap_or(0),
                dek_id: doc.keys.as_ref().map(|k| k.dek_id),
            })
            .collect()
    }

    /// Whether this store holds `doc_id` at all (a log, a snapshot or keys).
    pub fn has_doc(&self, doc_id: &str) -> bool {
        self.docs.contains_key(doc_id)
    }

    /// Whether `doc_id` has ever been given a blob. Keys alone (set ahead of
    /// a first blob that then never came) protect nothing yet.
    pub fn has_blobs(&self, doc_id: &str) -> bool {
        self.docs.get(doc_id).is_some_and(|doc| doc.head > 0 || doc.snapshot.is_some())
    }

    // ── State reports (docs/CONVERGENCE_CONTRACT.md section 1) ───────────

    /// Store `device`'s state report of `doc_id`, replacing the one it sent
    /// before. Only for a document that has a log: a report is about what a
    /// device holds of what the log carries, and a report must never be what
    /// brings a document into being. Past [`MAX_REPORTS_PER_DOC`] devices,
    /// the oldest report goes.
    pub async fn set_report(&mut self, doc_id: &str, device: &str, blob: Vec<u8>, at: DateTime<Utc>) -> Result<()> {
        if blob.len() > MAX_REPORT_BYTES {
            return Err(StoreError::InvalidOperation(format!(
                "a state report of {} bytes exceeds the limit of {} bytes",
                blob.len(),
                MAX_REPORT_BYTES
            )));
        }
        if device.is_empty() || device.len() > MAX_DEVICE_ID_BYTES {
            return Err(StoreError::InvalidOperation(format!("a device id must be 1 to {} bytes", MAX_DEVICE_ID_BYTES)));
        }
        if !self.has_blobs(doc_id) {
            return Err(StoreError::InvalidOperation(format!("document {} has no log to report on", doc_id)));
        }
        let doc = self.docs.get_mut(doc_id).expect("a document with blobs is held");
        doc.reports.insert(device.to_string(), StoredReport { device: device.to_string(), blob, at });
        while doc.reports.len() > MAX_REPORTS_PER_DOC {
            let Some(oldest) = doc.reports.values().min_by_key(|r| r.at).map(|r| r.device.clone()) else { break };
            doc.reports.remove(&oldest);
        }
        doc.save_reports().await
    }

    /// Every device's latest report of `doc_id` (none for a document this
    /// store does not hold). Order is unspecified.
    pub fn reports(&self, doc_id: &str) -> Vec<StoredReport> {
        self.docs.get(doc_id).map(|doc| doc.reports.values().cloned().collect()).unwrap_or_default()
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

    /// A fetch of whole blobs only, as tuples.
    fn flat(fetched: Fetched) -> (Option<(u64, Vec<u8>)>, Vec<(u64, Vec<u8>)>, u64) {
        (
            fetched.snapshot.map(|s| (s.seq, s.blobs.concat())),
            fetched.updates.into_iter().map(|e| (e.seq, e.blob)).collect(),
            fetched.head,
        )
    }

    fn part(index: u32, count: u32, group: Uuid) -> PartMeta {
        PartMeta { index, count, group }
    }

    /// An append in parts lands only with its last part, as consecutive
    /// entries marked as one group, survives a reopen, and a fetch that
    /// would start inside it starts at its first part.
    #[tokio::test]
    async fn an_append_in_parts_lands_whole_with_its_last_part() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        let group = Uuid::new_v4();
        {
            let mut store = VaultStore::create(&path, "V", None).await.unwrap();
            store.append("doc", b"before".to_vec()).await.unwrap();
            assert_eq!(store.append_part("doc", part(0, 3, group), b"p0".to_vec()).await.unwrap(), PartAppended::Held);
            assert!(!store.completes_group("doc", part(2, 3, group)), "part 1 has not come");
            assert_eq!(store.append_part("doc", part(1, 3, group), b"p1".to_vec()).await.unwrap(), PartAppended::Held);
            // Another client's append lands between the parts: the group still
            // goes in consecutively.
            assert_eq!(store.append("doc", b"between".to_vec()).await.unwrap(), 2);
            assert_eq!(flat(store.fetch("doc", 0).await.unwrap()).2, 2, "nothing of the group is in the log yet");
            assert!(store.completes_group("doc", part(2, 3, group)));
            let PartAppended::Landed(landed) = store.append_part("doc", part(2, 3, group), b"p2".to_vec()).await.unwrap() else { panic!("the last part lands the group") };
            assert_eq!(landed.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
        }
        let store = VaultStore::open(&path).await.unwrap();
        let fetched = store.fetch("doc", 0).await.unwrap();
        assert_eq!(fetched.head, 5);
        let parts: Vec<_> = fetched.updates.iter().filter_map(|e| e.part).collect();
        assert_eq!(parts, vec![part(0, 3, group), part(1, 3, group), part(2, 3, group)]);
        assert_eq!(fetched.updates[2].blob, b"p0");
        // Asked from inside the group: from its first part.
        let fetched = store.fetch("doc", 4).await.unwrap();
        assert_eq!(fetched.updates.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
        let (log, snapshot) = store.sizes("doc");
        assert!(log > 0 && snapshot == 0);
    }

    /// Parts out of order, or of a group never started, are refused; a part
    /// 0 starts a group over.
    #[tokio::test]
    async fn parts_out_of_order_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::create(dir.path().join("v.pimble"), "V", None).await.unwrap();
        let group = Uuid::new_v4();
        assert!(matches!(store.append_part("doc", part(1, 2, group), b"x".to_vec()).await, Err(StoreError::InvalidOperation(_))));
        store.append_part("doc", part(0, 3, group), b"a".to_vec()).await.unwrap();
        assert!(matches!(store.append_part("doc", part(2, 3, group), b"c".to_vec()).await, Err(StoreError::InvalidOperation(_))));
        // Started over.
        store.append_part("doc", part(0, 2, group), b"a".to_vec()).await.unwrap();
        assert!(matches!(store.append_part("doc", part(1, 2, group), b"b".to_vec()).await.unwrap(), PartAppended::Landed(_)));
    }

    /// A snapshot in parts replaces the held one only when its last part
    /// arrives: one cut short leaves the whole one standing.
    #[tokio::test]
    async fn a_half_uploaded_snapshot_never_replaces_a_whole_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        let mut store = VaultStore::create(&path, "V", None).await.unwrap();
        for i in 0..6u8 {
            store.append("doc", vec![i]).await.unwrap();
        }
        store.snapshot("doc", 2, b"whole".to_vec()).await.unwrap();
        let group = Uuid::new_v4();
        assert!(!store.snapshot_part("doc", 5, part(0, 2, group), b"half".to_vec()).await.unwrap());
        let fetched = store.fetch("doc", 0).await.unwrap();
        assert_eq!(fetched.snapshot.as_ref().map(|s| s.seq), Some(2), "the whole one stands");
        assert_eq!(fetched.updates.len(), 4);
        // Beyond the head is refused at the first part.
        assert!(store.snapshot_part("doc", 9, part(0, 2, Uuid::new_v4()), b"x".to_vec()).await.is_err());
        // Finished: replaces it, drops the log it covers, survives a reopen.
        assert!(store.snapshot_part("doc", 5, part(1, 2, group), b"-and-half".to_vec()).await.unwrap());
        drop(store);
        let store = VaultStore::open(&path).await.unwrap();
        let fetched = store.fetch("doc", 0).await.unwrap();
        let snapshot = fetched.snapshot.unwrap();
        assert_eq!((snapshot.seq, snapshot.group), (5, Some(group)));
        assert_eq!(snapshot.blobs, vec![b"half".to_vec(), b"-and-half".to_vec()]);
        assert_eq!(fetched.updates.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![6]);
        assert_eq!(store.sizes("doc").1, 13);
        // A snapshot in parts not newer than the held one is taken and dropped.
        let mut store = store;
        let older = Uuid::new_v4();
        assert!(!store.snapshot_part("doc", 4, part(0, 2, older), b"o".to_vec()).await.unwrap());
        assert!(!store.snapshot_part("doc", 4, part(1, 2, older), b"o".to_vec()).await.unwrap());
        assert_eq!(store.fetch("doc", 0).await.unwrap().snapshot.unwrap().seq, 5);
    }

    /// A log whose tail is a group cut short loses that tail at open.
    #[tokio::test]
    async fn a_group_cut_short_by_a_crash_is_dropped_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        let group = Uuid::new_v4();
        {
            let mut store = VaultStore::create(&path, "V", None).await.unwrap();
            store.append("doc", b"one".to_vec()).await.unwrap();
        }
        let log_path = path.join("vault").join("doc").join("log");
        let mut bytes = std::fs::read(&log_path).unwrap();
        bytes.extend_from_slice(&LogEntryMeta::record(2, Some(part(0, 2, group)), b"first half"));
        std::fs::write(&log_path, &bytes).unwrap();
        let mut store = VaultStore::open(&path).await.unwrap();
        assert_eq!(flat(store.fetch("doc", 0).await.unwrap()), (None, vec![(1, b"one".to_vec())], 1));
        assert_eq!(store.append("doc", b"two".to_vec()).await.unwrap(), 2, "the number is handed out again");
    }

    /// Every limit applies per part: a group larger than one blob's limit
    /// goes through in parts under it; the log limit stops a group at the
    /// part that would overflow it.
    #[tokio::test]
    async fn limits_apply_per_part() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::create(dir.path().join("v.pimble"), "V", None).await.unwrap();
        // The limits are process-wide: these are the ones
        // `append_refuses_an_oversized_blob` sets, which every test here
        // satisfies.
        set_limits_for_tests(4 * 1024 * 1024, 64 * 1024 * 1024);
        let mib = 1024 * 1024;
        let group = Uuid::new_v4();
        store.append_part("doc", part(0, 2, group), vec![1u8; 3 * mib]).await.unwrap();
        let landed = store.append_part("doc", part(1, 2, group), vec![2u8; 3 * mib]).await.unwrap();
        assert!(matches!(landed, PartAppended::Landed(entries) if entries.len() == 2), "6 MiB in parts under a 4 MiB blob limit");
        assert!(matches!(store.append_part("doc", part(0, 2, Uuid::new_v4()), vec![0u8; 5 * mib]).await, Err(StoreError::VaultBlobTooLarge { .. })), "but no part over it");
        let big = Uuid::new_v4();
        let mut refused = None;
        for index in 0..30u32 {
            if let Err(e) = store.append_part("doc", part(index, 30, big), vec![3u8; 3 * mib]).await {
                refused = Some((index, e));
                break;
            }
        }
        let (index, e) = refused.expect("a 90 MiB group does not fit a 64 MiB log");
        assert!(matches!(e, StoreError::VaultSnapshotRequired));
        assert!(index > 10 && index < 30, "refused at the part that overflows, not before ({index})");
        assert!(!store.completes_group("doc", part(29, 30, big)), "and the group is let go");
        assert_eq!(store.fetch("doc", 0).await.unwrap().updates.len(), 2);
    }

    #[tokio::test]
    async fn append_and_fetch_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = VaultStore::create(dir.path().join("v.pimble"), "V", None).await.unwrap();

        let seq1 = store.append("tree", b"one".to_vec()).await.unwrap();
        let seq2 = store.append("tree", b"two".to_vec()).await.unwrap();
        assert_eq!((seq1, seq2), (1, 2));

        let (snapshot, updates, head) = flat(store.fetch("tree", 0).await.unwrap());
        assert!(snapshot.is_none());
        assert_eq!(updates, vec![(1, b"one".to_vec()), (2, b"two".to_vec())]);
        assert_eq!(head, 2);

        let (_, updates, _) = flat(store.fetch("tree", 1).await.unwrap());
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

            let (snapshot, updates, head) = flat(store.fetch("tree", 0).await.unwrap());
            assert_eq!(snapshot, Some((2, b"snap-of-one-two".to_vec())));
            assert_eq!(updates, vec![(3, b"three".to_vec())]);
            assert_eq!(head, 3);
        }

        let store = VaultStore::open(&path).await.unwrap();
        assert_eq!(store.id, store_id);
        let (snapshot, updates, head) = flat(store.fetch("tree", 0).await.unwrap());
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

        let (snapshot, updates, head) = flat(store.fetch("doc", 0).await.unwrap());
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
        set_limits_for_tests(4 * 1024 * 1024, 64 * 1024 * 1024);
        let big = vec![0u8; 4 * 1024 * 1024 + 1];
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
        let (_, updates, head) = flat(store.fetch("tree", 0).await.unwrap());
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

    /// The discussion set (docs/DISCUSSIONS_CONTRACT.md section 7) only grows
    /// and survives a reopen.
    #[tokio::test]
    async fn the_discussion_set_grows_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("d.pimble");
        let (d, host, other_host) = (NodeId::new(), NodeId::new(), NodeId::new());
        {
            let mut store = VaultStore::create(&path, "D", None).await.unwrap();
            assert!(!store.is_discussion(d));
            assert!(store.add_discussions([(d, host)]).await.unwrap());
            assert!(!store.add_discussions([(d, other_host)]).await.unwrap(), "a host never changes");
            assert!(store.is_discussion(d));
        }
        let store = VaultStore::open(&path).await.unwrap();
        assert!(store.is_discussion(d));
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
            let (_, _, head) = flat(store.fetch("doc", 0).await.unwrap());
            assert_eq!(head, 0, "keys are not a log entry");
        }
        let store = VaultStore::open(&path).await.unwrap();
        let listed = store.list_docs();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].dek_id, Some(dek_id));
        assert_eq!(store.doc_keys("doc").unwrap().dek_id, dek_id);
        assert!(store.doc_keys("other").is_none());
    }

    /// A device's report is kept per document and overwritten by its next;
    /// it survives a reopen; a document with no log takes none; a report over
    /// the limit is refused; past the most devices, the oldest report goes.
    #[tokio::test]
    async fn state_reports_are_one_slot_per_device_beside_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.pimble");
        let at = |secs: i64| DateTime::<Utc>::from_timestamp(1_800_000_000 + secs, 0).unwrap();
        {
            let mut store = VaultStore::create(&path, "V", None).await.unwrap();
            assert!(store.set_report("doc", "dev-a", b"r".to_vec(), at(0)).await.is_err(), "no log, no report");
            assert!(!store.has_doc("doc"), "a refused report brings no document into being");
            store.append("doc", b"blob".to_vec()).await.unwrap();
            store.set_report("doc", "dev-a", b"first".to_vec(), at(1)).await.unwrap();
            store.set_report("doc", "dev-b", b"bee".to_vec(), at(2)).await.unwrap();
            store.set_report("doc", "dev-a", b"second".to_vec(), at(3)).await.unwrap();
            assert!(store.set_report("doc", "dev-c", vec![0u8; MAX_REPORT_BYTES + 1], at(4)).await.is_err(), "over the limit");
            assert!(store.set_report("doc", "", b"x".to_vec(), at(4)).await.is_err(), "a device id is required");
        }
        let mut store = VaultStore::open(&path).await.unwrap();
        let mut reports = store.reports("doc");
        reports.sort_by(|a, b| a.device.cmp(&b.device));
        assert_eq!(reports.iter().map(|r| (r.device.as_str(), r.blob.as_slice(), r.at)).collect::<Vec<_>>(), vec![("dev-a", &b"second"[..], at(3)), ("dev-b", &b"bee"[..], at(2))]);
        assert_eq!(store.list_docs().len(), 1);

        for i in 0..MAX_REPORTS_PER_DOC as i64 {
            store.set_report("doc", &format!("dev-{i:02}"), b"x".to_vec(), at(10 + i)).await.unwrap();
        }
        let devices: HashSet<String> = store.reports("doc").into_iter().map(|r| r.device).collect();
        assert_eq!(devices.len(), MAX_REPORTS_PER_DOC);
        assert!(!devices.contains("dev-b") && !devices.contains("dev-a"), "the oldest reports went first");
        assert!(devices.contains("dev-00"));
    }
}
