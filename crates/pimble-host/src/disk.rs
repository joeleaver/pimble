//! A disk (docs/ONE_CODEBASE_CONTRACT.md seam B): everything the server keeps
//! between runs goes through one, the store folders (`nodes/`, the manifest,
//! `blobs/`, `sync.json`, `computers/`), each store's state directory
//! (`sync-state.json`, `people.json`), the hosted server's vault logs and
//! snapshots, the keystore, the saved credentials and the agents file.
//!
//! [`DiskBackend`] is what a host implements: small file operations, every one
//! async because the browser's (OPFS) are. [`Disk`] is the cheap handle the
//! server holds and clones, with the compositions every caller shares:
//! [`Disk::write_atomic`] (a temporary beside the file, renamed into place),
//! [`Disk::write_durable`] (the same, synced, with its directory) and
//! [`Disk::write_private_atomic`] (mode `0600`). A backend that writes a whole
//! file atomically on its own (OPFS's `createWritable` commits at `close`)
//! overrides those rather than imitating a rename.
//!
//! Three hosts are meant (the contract, question 8): the desktop
//! ([`NativeDisk`]: `tokio::fs`, the files exactly as they were before this
//! seam), the browser's worker (OPFS through synchronous access handles in a
//! dedicated worker, every file sealed with the disk key; wave 4), and a
//! phone app hosting the server on a background thread (a real filesystem:
//! [`NativeDisk`] as it is). [`MemoryDisk`] is the fourth: tests, and a
//! browser told not to remember itself.
//!
//! Paths stay `std::path::Path`s on every host: an OPFS backend walks their
//! components as directory handles. They are absolute on the desktop and
//! rooted at the host's own roots ([`crate::Paths`]) elsewhere.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(feature = "test-disk")]
pub mod testfs;

pub use memory::MemoryDisk;
#[cfg(not(target_arch = "wasm32"))]
pub use native::NativeDisk;

/// What [`Disk::metadata`] answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    /// The file's length in bytes (0 for a directory on a memory disk).
    pub len: u64,
    pub is_dir: bool,
    /// When the file was last written, where the host knows.
    pub modified: Option<SystemTime>,
}

/// One entry of a directory ([`Disk::list`]), in no particular order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The entry's whole path: the listed directory joined with its name.
    pub path: PathBuf,
    pub is_dir: bool,
}

impl Entry {
    /// The entry's name, when it is valid UTF-8.
    pub fn name(&self) -> Option<&str> {
        self.path.file_name().and_then(|n| n.to_str())
    }
}

/// Which kind of disk a [`Disk`] is: what decides the few things only a real
/// directory can do (a folder watch, an index on its own files).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskKind {
    /// The machine's own filesystem: paths are real paths.
    Native,
    /// Held in memory for the life of the process.
    Memory,
    /// Anything else a host brings (the browser's OPFS).
    Other,
}

/// How [`Disk::write_atomic`] names the temporary it writes before the
/// rename. Each caller keeps the name it has always used, so nothing a crash
/// leaves behind looks different from before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmpName {
    /// `<stem>.<12 random hex>.tmp` (`path.with_extension(...)`): new every
    /// time, so two writers of one folder never rename each other's half
    /// written bytes into place, and never ending in `.yrs`. The store
    /// folder's and the state directory's files.
    Unique,
    /// `path.with_extension("tmp")`: the hosted server's vault files.
    Extension,
}

impl TmpName {
    fn of(self, path: &Path) -> PathBuf {
        match self {
            TmpName::Unique => {
                let unique = uuid::Uuid::new_v4().simple().to_string();
                path.with_extension(format!("{}.tmp", &unique[..12]))
            }
            TmpName::Extension => path.with_extension("tmp"),
        }
    }
}

/// The file operations a host provides. Errors are `std::io::Error`s with the
/// kinds the standard library gives (`NotFound` above all: callers branch on
/// it), whatever the host.
///
/// The provided compositions ([`DiskBackend::write_atomic`],
/// [`DiskBackend::write_durable`], [`DiskBackend::write_private_atomic`]) are
/// exactly the sequences of calls the desktop made before this seam; a host
/// with a better primitive overrides them.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait DiskBackend: Send + Sync + fmt::Debug {
    fn kind(&self) -> DiskKind;

    /// The whole file.
    async fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// `len` bytes from `offset` (an error when the file is shorter).
    async fn read_range(&self, path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>>;
    /// The file's or directory's metadata; `NotFound` when there is none.
    async fn metadata(&self, path: &Path) -> io::Result<Metadata>;
    /// The entries of a directory.
    async fn list(&self, dir: &Path) -> io::Result<Vec<Entry>>;

    /// Make a directory whose parent exists (`AlreadyExists` when it does).
    async fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// Make a directory and every missing parent.
    async fn create_dir_all(&self, path: &Path) -> io::Result<()>;

    /// Write a whole file in place, creating or truncating it
    /// (`std::fs::write`). Not atomic: see [`DiskBackend::write_atomic`].
    async fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// [`DiskBackend::write`], readable by this user alone (mode `0600` where
    /// the host has modes, set when the file is created and again after).
    async fn write_private(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Add `bytes` at the end of a file, creating it when absent: a vault
    /// log's records, and the browser's per-document log of updates taken
    /// (the contract, section 4).
    async fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    /// Write `bytes` over an existing file from `offset` (a picture upload's
    /// header, finished last).
    async fn write_at(&self, path: &Path, offset: u64, bytes: &[u8]) -> io::Result<()>;

    /// Put a file's contents on the disk (`fsync`). `Ok(false)` when the file
    /// could not be opened to sync (absent, most likely); an error only when
    /// the sync itself failed.
    async fn sync_file(&self, path: &Path) -> io::Result<bool>;
    /// Put a directory's entries (a rename in it) on the disk. Best effort:
    /// not every platform opens a directory as a file.
    async fn sync_dir(&self, path: &Path);

    /// Rename a file or directory, replacing a file at `to`.
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Copy a file's bytes to `to`, answering how many.
    async fn copy(&self, from: &Path, to: &Path) -> io::Result<u64>;
    async fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Remove a directory and everything under it.
    async fn remove_dir_all(&self, path: &Path) -> io::Result<()>;

    /// Write a whole file so that a crash leaves the old one or the new one:
    /// a temporary beside it (named by `tmp`), renamed into place; the
    /// temporary removed when the rename fails. Not synced.
    async fn write_atomic(&self, path: &Path, bytes: &[u8], tmp: TmpName) -> io::Result<()> {
        let tmp_path = tmp.of(path);
        self.write(&tmp_path, bytes).await?;
        if let Err(e) = self.rename(&tmp_path, path).await {
            let _ = self.remove_file(&tmp_path).await;
            return Err(e);
        }
        Ok(())
    }

    /// [`DiskBackend::write_atomic`], and on the disk before it answers: the
    /// temporary synced before the rename, the directory synced after.
    async fn write_durable(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let tmp_path = TmpName::Unique.of(path);
        let written = async {
            self.write(&tmp_path, bytes).await?;
            self.sync_file(&tmp_path).await?;
            self.rename(&tmp_path, path).await
        }
        .await;
        if let Err(e) = written {
            let _ = self.remove_file(&tmp_path).await;
            return Err(e);
        }
        if let Some(dir) = path.parent() {
            self.sync_dir(dir).await;
        }
        Ok(())
    }

    /// A small, sensitive, single-writer file (the keystore, the saved
    /// credentials, the agents file): its directory made first, written
    /// readable by this user alone to `<path>.tmp`, renamed over `path`.
    async fn write_private_atomic(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            self.create_dir_all(parent).await?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp_path = PathBuf::from(tmp);
        self.write_private(&tmp_path, bytes).await?;
        self.rename(&tmp_path, path).await
    }
}

/// A disk: a cheap handle over a [`DiskBackend`], cloned by everything that
/// reads or writes. See the module documentation.
#[derive(Clone)]
pub struct Disk(Arc<dyn DiskBackend>);

impl fmt::Debug for Disk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Disk").field(&self.0.kind()).finish()
    }
}

impl Default for Disk {
    /// The desktop's own disk ([`NativeDisk`]); a memory disk where there is
    /// none (wasm32). In a test build (`test-disk`), the disk
    /// `PIMBLE_TEST_DISK` names: see [`testfs`].
    fn default() -> Self {
        #[cfg(feature = "test-disk")]
        if let Some(disk) = testfs::disk_from_env() {
            return disk;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Disk::native()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Disk::memory()
        }
    }
}

impl Disk {
    pub fn new(backend: impl DiskBackend + 'static) -> Self {
        Self(Arc::new(backend))
    }

    pub fn from_arc(backend: Arc<dyn DiskBackend>) -> Self {
        Self(backend)
    }

    /// The machine's filesystem through `tokio::fs`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn native() -> Self {
        Self::new(NativeDisk)
    }

    /// A fresh, empty memory disk.
    pub fn memory() -> Self {
        Self::new(MemoryDisk::new())
    }

    pub fn kind(&self) -> DiskKind {
        self.0.kind()
    }

    /// Whether paths on this disk are the machine's own: what a folder watch
    /// or a search index writing its own files needs.
    pub fn is_native(&self) -> bool {
        self.kind() == DiskKind::Native
    }

    pub async fn read(&self, path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        self.0.read(path.as_ref()).await
    }

    pub async fn read_to_string(&self, path: impl AsRef<Path>) -> io::Result<String> {
        let bytes = self.0.read(path.as_ref()).await?;
        String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub async fn read_range(&self, path: impl AsRef<Path>, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        self.0.read_range(path.as_ref(), offset, len).await
    }

    pub async fn metadata(&self, path: impl AsRef<Path>) -> io::Result<Metadata> {
        self.0.metadata(path.as_ref()).await
    }

    /// Whether anything is at `path` (`Path::exists`).
    pub async fn exists(&self, path: impl AsRef<Path>) -> bool {
        self.0.metadata(path.as_ref()).await.is_ok()
    }

    /// Whether a directory is at `path` (`Path::is_dir`).
    pub async fn is_dir(&self, path: impl AsRef<Path>) -> bool {
        self.0.metadata(path.as_ref()).await.is_ok_and(|m| m.is_dir)
    }

    pub async fn list(&self, dir: impl AsRef<Path>) -> io::Result<Vec<Entry>> {
        self.0.list(dir.as_ref()).await
    }

    pub async fn create_dir(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.0.create_dir(path.as_ref()).await
    }

    pub async fn create_dir_all(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.0.create_dir_all(path.as_ref()).await
    }

    pub async fn write(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        self.0.write(path.as_ref(), bytes.as_ref()).await
    }

    pub async fn append(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        self.0.append(path.as_ref(), bytes.as_ref()).await
    }

    pub async fn write_at(&self, path: impl AsRef<Path>, offset: u64, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        self.0.write_at(path.as_ref(), offset, bytes.as_ref()).await
    }

    pub async fn sync_file(&self, path: impl AsRef<Path>) -> io::Result<bool> {
        self.0.sync_file(path.as_ref()).await
    }

    pub async fn sync_dir(&self, path: impl AsRef<Path>) {
        self.0.sync_dir(path.as_ref()).await
    }

    pub async fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
        self.0.rename(from.as_ref(), to.as_ref()).await
    }

    pub async fn copy(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
        self.0.copy(from.as_ref(), to.as_ref()).await
    }

    pub async fn remove_file(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.0.remove_file(path.as_ref()).await
    }

    pub async fn remove_dir_all(&self, path: impl AsRef<Path>) -> io::Result<()> {
        self.0.remove_dir_all(path.as_ref()).await
    }

    /// Remove a directory and what it holds, or a file (`remove_dir_all` or
    /// `remove_file` by what is there).
    pub async fn remove_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        if self.is_dir(path).await {
            self.0.remove_dir_all(path).await
        } else {
            self.0.remove_file(path).await
        }
    }

    /// See [`DiskBackend::write_atomic`].
    pub async fn write_atomic(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>, tmp: TmpName) -> io::Result<()> {
        self.0.write_atomic(path.as_ref(), bytes.as_ref(), tmp).await
    }

    /// See [`DiskBackend::write_durable`].
    pub async fn write_durable(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        self.0.write_durable(path.as_ref(), bytes.as_ref()).await
    }

    /// See [`DiskBackend::write_private_atomic`].
    pub async fn write_private_atomic(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        self.0.write_private_atomic(path.as_ref(), bytes.as_ref()).await
    }
}
