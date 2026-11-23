//! A disk whose every file is sealed with a key (docs/ONE_CODEBASE_CONTRACT.md
//! section 7, "The disk"): the browser's OPFS under the remembered device's
//! disk key. Everything above sees plain bytes; what the backend below holds
//! is ciphertext.
//!
//! Each file is one `pimble_crypto::Blob` (XChaCha20-Poly1305), its associated
//! data the file's path and a format version (`pimble_crypto::local_file_aad`),
//! so a file moved or renamed on the disk below does not open: a rename here
//! reads and seals the file again under its new path. File and directory names
//! pass through unchanged (store and node ids, never titles or text).
//!
//! The seal adds a constant number of bytes, so a file's plain length is known
//! from its metadata. What a whole-file seal cannot do in place is done by
//! reading, changing and writing the file again: an append, a write at an
//! offset, a read of a range. None of them is on a path the browser's server
//! takes often (a picture's upload, the hosted server's vault logs).

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use pimble_crypto::{Blob, SymmetricKey};

use super::{DiskBackend, DiskKind, Entry, Metadata, TmpName};

/// The bytes a seal adds: `PB`, version, key id (16), nonce (24), tag (16).
pub const SEAL_OVERHEAD: u64 = 2 + 1 + 16 + 24 + 16;

/// See the module documentation.
pub struct SealedDisk {
    inner: Arc<dyn DiskBackend>,
    key: SymmetricKey,
}

impl std::fmt::Debug for SealedDisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedDisk").field("inner", &self.inner).finish_non_exhaustive()
    }
}

impl SealedDisk {
    pub fn new(inner: Arc<dyn DiskBackend>, key: SymmetricKey) -> Self {
        Self { inner, key }
    }

    fn seal(&self, path: &Path, plain: &[u8]) -> Vec<u8> {
        Blob::encrypt(&self.key, uuid::Uuid::nil(), &aad(path), plain)
    }

    fn open(&self, path: &Path, sealed: &[u8]) -> io::Result<Vec<u8>> {
        Blob::decrypt(&self.key, &aad(path), sealed)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{} does not open with this browser's disk key: {e}", path.display())))
    }

    /// The plain bytes of a file, or nothing when it is absent.
    async fn read_or_empty(&self, path: &Path) -> io::Result<Vec<u8>> {
        match self.read(path).await {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Move a file or a directory's files, each sealed again under its new
    /// path.
    fn move_sealed<'a>(&'a self, from: &'a Path, to: &'a Path) -> MovedFuture<'a> {
        Box::pin(async move {
            let meta = self.inner.metadata(from).await?;
            if meta.is_dir {
                if self.inner.metadata(to).await.is_ok_and(|m| !m.is_dir) {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} is a file", to.display())));
                }
                self.inner.create_dir_all(to).await?;
                for entry in self.inner.list(from).await? {
                    let name = entry.path.file_name().map(PathBuf::from).unwrap_or_default();
                    self.move_sealed(&entry.path, &to.join(name)).await?;
                }
                self.inner.remove_dir_all(from).await
            } else {
                let plain = self.read(from).await?;
                self.write(to, &plain).await?;
                self.inner.remove_file(from).await
            }
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
type MovedFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<()>> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
type MovedFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = io::Result<()>> + 'a>>;

/// The associated data for `path`: its components joined by `/` from the root,
/// so `a//b`, `a/./b` and `/a/b` name one file.
fn aad(path: &Path) -> Vec<u8> {
    let mut normal = String::new();
    for component in path.components() {
        if let Component::Normal(part) = component {
            normal.push('/');
            normal.push_str(&part.to_string_lossy());
        }
    }
    pimble_crypto::local_file_aad(&normal)
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl DiskBackend for SealedDisk {
    fn kind(&self) -> DiskKind {
        self.inner.kind()
    }

    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let sealed = self.inner.read(path).await?;
        self.open(path, &sealed)
    }

    async fn read_range(&self, path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let plain = self.read(path).await?;
        let start = usize::try_from(offset).map_err(|_| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        plain.get(start..start.saturating_add(len)).map(<[u8]>::to_vec).ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let mut meta = self.inner.metadata(path).await?;
        if !meta.is_dir {
            meta.len = meta.len.saturating_sub(SEAL_OVERHEAD);
        }
        Ok(meta)
    }

    async fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        self.inner.list(dir).await
    }

    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path).await
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path).await
    }

    async fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.inner.write(path, &self.seal(path, bytes)).await
    }

    async fn write_private(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_private(path, &self.seal(path, bytes)).await
    }

    async fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut plain = self.read_or_empty(path).await?;
        plain.extend_from_slice(bytes);
        self.write(path, &plain).await
    }

    async fn write_at(&self, path: &Path, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let mut plain = self.read(path).await?;
        let start = usize::try_from(offset).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let end = start.saturating_add(bytes.len());
        if plain.len() < end {
            plain.resize(end, 0);
        }
        plain[start..end].copy_from_slice(bytes);
        self.write(path, &plain).await
    }

    async fn sync_file(&self, path: &Path) -> io::Result<bool> {
        self.inner.sync_file(path).await
    }

    async fn sync_dir(&self, path: &Path) {
        self.inner.sync_dir(path).await
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if from == to {
            return self.inner.metadata(from).await.map(|_| ());
        }
        self.move_sealed(from, to).await
    }

    async fn copy(&self, from: &Path, to: &Path) -> io::Result<u64> {
        let plain = self.read(from).await?;
        self.write(to, &plain).await?;
        Ok(plain.len() as u64)
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path).await
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir_all(path).await
    }

    // The compositions go to the backend below with the bytes sealed under
    // the final path: whatever temporary and rename it uses (none, on OPFS)
    // moves those bytes unchanged.
    async fn write_atomic(&self, path: &Path, bytes: &[u8], tmp: TmpName) -> io::Result<()> {
        self.inner.write_atomic(path, &self.seal(path, bytes), tmp).await
    }

    async fn write_durable(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_durable(path, &self.seal(path, bytes)).await
    }

    async fn write_private_atomic(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_private_atomic(path, &self.seal(path, bytes)).await
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::{Disk, MemoryDisk};

    fn disks() -> (Disk, Disk) {
        let below = Disk::memory();
        let sealed = Disk::new(SealedDisk::new(Arc::new(MemoryDiskHandle(below.clone())), SymmetricKey::generate()));
        (below, sealed)
    }

    /// A `Disk` as a backend, so the test can look below the seal.
    #[derive(Debug)]
    struct MemoryDiskHandle(Disk);

    #[async_trait::async_trait]
    impl DiskBackend for MemoryDiskHandle {
        fn kind(&self) -> DiskKind {
            DiskKind::Other
        }
        async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
            self.0.read(p).await
        }
        async fn read_range(&self, p: &Path, o: u64, l: usize) -> io::Result<Vec<u8>> {
            self.0.read_range(p, o, l).await
        }
        async fn metadata(&self, p: &Path) -> io::Result<Metadata> {
            self.0.metadata(p).await
        }
        async fn list(&self, p: &Path) -> io::Result<Vec<Entry>> {
            self.0.list(p).await
        }
        async fn create_dir(&self, p: &Path) -> io::Result<()> {
            self.0.create_dir(p).await
        }
        async fn create_dir_all(&self, p: &Path) -> io::Result<()> {
            self.0.create_dir_all(p).await
        }
        async fn write(&self, p: &Path, b: &[u8]) -> io::Result<()> {
            self.0.write(p, b).await
        }
        async fn write_private(&self, p: &Path, b: &[u8]) -> io::Result<()> {
            self.0.write(p, b).await
        }
        async fn append(&self, p: &Path, b: &[u8]) -> io::Result<()> {
            self.0.append(p, b).await
        }
        async fn write_at(&self, p: &Path, o: u64, b: &[u8]) -> io::Result<()> {
            self.0.write_at(p, o, b).await
        }
        async fn sync_file(&self, p: &Path) -> io::Result<bool> {
            self.0.sync_file(p).await
        }
        async fn sync_dir(&self, p: &Path) {
            self.0.sync_dir(p).await
        }
        async fn rename(&self, f: &Path, t: &Path) -> io::Result<()> {
            self.0.rename(f, t).await
        }
        async fn copy(&self, f: &Path, t: &Path) -> io::Result<u64> {
            self.0.copy(f, t).await
        }
        async fn remove_file(&self, p: &Path) -> io::Result<()> {
            self.0.remove_file(p).await
        }
        async fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
            self.0.remove_dir_all(p).await
        }
    }

    #[tokio::test]
    async fn what_is_written_is_read_and_only_ciphertext_is_below() {
        let (below, disk) = disks();
        disk.create_dir_all("/pimble/stores/s").await.unwrap();
        disk.write("/pimble/stores/s/a.yrs", b"pomegranates in the garden").await.unwrap();
        assert_eq!(disk.read("/pimble/stores/s/a.yrs").await.unwrap(), b"pomegranates in the garden");
        let raw = below.read("/pimble/stores/s/a.yrs").await.unwrap();
        assert!(!raw.windows(12).any(|w| w == b"pomegranates"), "the plain text is not below the seal");
        assert_eq!(raw.len() as u64, 26 + SEAL_OVERHEAD);
        assert_eq!(disk.metadata("/pimble/stores/s/a.yrs").await.unwrap().len, 26);
        // The same path spelled another way is the same file.
        assert_eq!(disk.read("/pimble//stores/./s/a.yrs").await.unwrap(), b"pomegranates in the garden");
    }

    #[tokio::test]
    async fn a_file_moved_below_the_seal_does_not_open() {
        let (below, disk) = disks();
        disk.create_dir_all("/d").await.unwrap();
        disk.write("/d/one", b"one").await.unwrap();
        below.rename("/d/one", "/d/two").await.unwrap();
        let err = disk.read("/d/two").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn appends_writes_at_ranges_renames_and_copies() {
        let (_below, disk) = disks();
        disk.create_dir_all("/d/sub").await.unwrap();
        disk.append("/d/log", b"abc").await.unwrap();
        disk.append("/d/log", b"def").await.unwrap();
        assert_eq!(disk.read("/d/log").await.unwrap(), b"abcdef");
        disk.write_at("/d/log", 1, b"XY").await.unwrap();
        assert_eq!(disk.read("/d/log").await.unwrap(), b"aXYdef");
        assert_eq!(disk.read_range("/d/log", 2, 3).await.unwrap(), b"Yde");
        assert!(disk.read_range("/d/log", 5, 3).await.is_err());

        disk.write("/d/sub/x", b"x").await.unwrap();
        disk.rename("/d/sub", "/d/moved").await.unwrap();
        assert_eq!(disk.read("/d/moved/x").await.unwrap(), b"x");
        assert!(!disk.exists("/d/sub").await);
        disk.rename("/d/log", "/d/log2").await.unwrap();
        assert_eq!(disk.read("/d/log2").await.unwrap(), b"aXYdef");
        assert_eq!(disk.copy("/d/log2", "/d/log3").await.unwrap(), 6);
        assert_eq!(disk.read("/d/log3").await.unwrap(), b"aXYdef");

        disk.write_atomic("/d/atomic", b"whole", TmpName::Unique).await.unwrap();
        disk.write_durable("/d/durable", b"kept").await.unwrap();
        disk.write_private_atomic("/d/new/private", b"secret").await.unwrap();
        assert_eq!(disk.read("/d/atomic").await.unwrap(), b"whole");
        assert_eq!(disk.read("/d/durable").await.unwrap(), b"kept");
        assert_eq!(disk.read("/d/new/private").await.unwrap(), b"secret");
        let names: Vec<String> = disk.list("/d").await.unwrap().iter().filter_map(|e| e.name().map(str::to_string)).collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    #[tokio::test]
    async fn another_key_does_not_open_it() {
        let below = Arc::new(MemoryDiskHandle(Disk::new(MemoryDisk::new())));
        let one = Disk::new(SealedDisk::new(below.clone(), SymmetricKey::generate()));
        let other = Disk::new(SealedDisk::new(below, SymmetricKey::generate()));
        one.write("/f", b"text").await.unwrap();
        assert_eq!(other.read("/f").await.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
