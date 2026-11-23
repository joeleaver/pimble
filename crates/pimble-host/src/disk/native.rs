//! The desktop's disk: the machine's filesystem through `tokio::fs`. Every
//! operation is the call the server made directly before the seam existed,
//! so the files, their names and what is synced when are unchanged.

use std::io;
use std::path::Path;

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::{DiskBackend, DiskKind, Entry, Metadata};

/// See the module documentation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NativeDisk;

#[async_trait::async_trait]
impl DiskBackend for NativeDisk {
    fn kind(&self) -> DiskKind {
        DiskKind::Native
    }

    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path).await
    }

    async fn read_range(&self, path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut file = fs::File::open(path).await?;
        file.seek(io::SeekFrom::Start(offset)).await?;
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf).await?;
        Ok(buf)
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let meta = fs::metadata(path).await?;
        Ok(Metadata { len: meta.len(), is_dir: meta.is_dir(), modified: meta.modified().ok() })
    }

    async fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut entries = fs::read_dir(dir).await?;
        let mut out = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            out.push(Entry { path: entry.path(), is_dir });
        }
        Ok(out)
    }

    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir(path).await
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path).await
    }

    async fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        fs::write(path, bytes).await
    }

    async fn write_private(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        // On unix, the mode the file is *created* with (honored only when
        // `O_CREAT` creates it), so it is never briefly readable by others.
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(path).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        drop(file);
        // And again after writing: `open()`'s mode does not apply to a file
        // it reuses (a stale one with wider permissions).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
        }
        Ok(())
    }

    async fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file = fs::OpenOptions::new().create(true).append(true).open(path).await?;
        file.write_all(bytes).await?;
        file.flush().await
    }

    async fn write_at(&self, path: &Path, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let mut file = fs::OpenOptions::new().write(true).open(path).await?;
        file.seek(io::SeekFrom::Start(offset)).await?;
        file.write_all(bytes).await?;
        file.flush().await
    }

    async fn sync_file(&self, path: &Path) -> io::Result<bool> {
        let Ok(file) = fs::File::open(path).await else { return Ok(false) };
        file.sync_all().await?;
        Ok(true)
    }

    async fn sync_dir(&self, path: &Path) {
        if let Ok(dir) = fs::File::open(path).await {
            let _ = dir.sync_all().await;
        }
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to).await
    }

    async fn copy(&self, from: &Path, to: &Path) -> io::Result<u64> {
        fs::copy(from, to).await
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path).await
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path).await
    }
}
