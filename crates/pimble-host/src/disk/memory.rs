//! A disk held in memory for the life of the process: the browser told not to
//! remember itself (docs/ONE_CODEBASE_CONTRACT.md section 7), and the server's
//! test suite run the way the browser will run it (section 8).
//!
//! It keeps a filesystem's rules, so code that works on it works on a real
//! one: a file's parent must be a directory, `create_dir` refuses what exists,
//! a directory is not read as a file, a rename replaces a file and moves a
//! directory with everything under it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use super::{DiskBackend, DiskKind, Entry, Metadata};

#[derive(Debug, Clone)]
enum Item {
    File { bytes: Vec<u8>, modified: Option<SystemTime> },
    Dir,
}

/// Which directories exist without having been made on this disk: the roots
/// it is handed (a test's temp directory, made on the real one).
type Implicit = Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// See the module documentation.
#[derive(Clone)]
pub struct MemoryDisk {
    items: Arc<Mutex<BTreeMap<PathBuf, Item>>>,
    implicit: Option<Implicit>,
}

impl std::fmt::Debug for MemoryDisk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryDisk").field("items", &self.lock().len()).finish()
    }
}

impl Default for MemoryDisk {
    fn default() -> Self {
        Self::new()
    }
}

fn not_found(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{}: no such file or directory", path.display()))
}

fn now() -> Option<SystemTime> {
    // `SystemTime::now` panics on wasm32-unknown-unknown; the browser's
    // memory disk has no modification times until the runtime's clock
    // reaches it (wave 3).
    #[cfg(not(target_arch = "wasm32"))]
    {
        Some(SystemTime::now())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// `path` with `.` and trailing separators gone and `..` applied: one key per
/// place, however it was spelled.
fn key(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl MemoryDisk {
    /// An empty disk: only its root (`/`, or `""` for relative paths) exists.
    pub fn new() -> Self {
        Self { items: Arc::default(), implicit: None }
    }

    /// An empty disk on which every directory `exists` answers for is there
    /// without being made: the roots a host hands it.
    pub fn with_roots(exists: impl Fn(&Path) -> bool + Send + Sync + 'static) -> Self {
        Self { items: Arc::default(), implicit: Some(Arc::new(exists)) }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Item>> {
        self.items.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn is_dir_in(&self, items: &BTreeMap<PathBuf, Item>, path: &Path) -> bool {
        match items.get(path) {
            Some(Item::Dir) => true,
            Some(Item::File { .. }) => false,
            None => path.parent().is_none() || self.implicit.as_ref().is_some_and(|exists| exists(path)),
        }
    }

    /// The parent of `path` must be a directory for anything to be made there.
    fn check_parent(&self, items: &BTreeMap<PathBuf, Item>, path: &Path) -> io::Result<()> {
        match path.parent() {
            Some(parent) if !self.is_dir_in(items, parent) => Err(not_found(parent)),
            _ => Ok(()),
        }
    }

    fn file_mut<'a>(items: &'a mut BTreeMap<PathBuf, Item>, path: &Path) -> io::Result<&'a mut Vec<u8>> {
        match items.get_mut(path) {
            Some(Item::File { bytes, modified }) => {
                *modified = now();
                Ok(bytes)
            }
            Some(Item::Dir) => Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{} is a directory", path.display()))),
            None => Err(not_found(path)),
        }
    }

    /// Every path at or under `path`.
    fn subtree(items: &BTreeMap<PathBuf, Item>, path: &Path) -> Vec<PathBuf> {
        items.range(path.to_path_buf()..).map(|(p, _)| p).take_while(|p| p.starts_with(path)).cloned().collect()
    }

    // ── The operations, synchronous: what the async trait and the tests'
    //    `testfs` both call. ─────────────────────────────────────────────

    pub fn read_now(&self, path: &Path) -> io::Result<Vec<u8>> {
        let path = key(path);
        match self.lock().get(&path) {
            Some(Item::File { bytes, .. }) => Ok(bytes.clone()),
            Some(Item::Dir) => Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{} is a directory", path.display()))),
            None => Err(not_found(&path)),
        }
    }

    pub fn metadata_now(&self, path: &Path) -> io::Result<Metadata> {
        let path = key(path);
        let items = self.lock();
        match items.get(&path) {
            Some(Item::File { bytes, modified }) => Ok(Metadata { len: bytes.len() as u64, is_dir: false, modified: *modified }),
            Some(Item::Dir) => Ok(Metadata { len: 0, is_dir: true, modified: None }),
            None if self.is_dir_in(&items, &path) => Ok(Metadata { len: 0, is_dir: true, modified: None }),
            None => Err(not_found(&path)),
        }
    }

    pub fn list_now(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let dir = key(dir);
        let items = self.lock();
        if !self.is_dir_in(&items, &dir) {
            return Err(match items.get(&dir) {
                Some(_) => io::Error::new(io::ErrorKind::NotADirectory, format!("{} is not a directory", dir.display())),
                None => not_found(&dir),
            });
        }
        Ok(items
            .range(dir.clone()..)
            .take_while(|(p, _)| p.starts_with(&dir))
            .filter(|(p, _)| p.parent() == Some(dir.as_path()))
            .map(|(p, item)| Entry { path: p.clone(), is_dir: matches!(item, Item::Dir) })
            .collect())
    }

    pub fn create_dir_now(&self, path: &Path) -> io::Result<()> {
        let path = key(path);
        let mut items = self.lock();
        if items.contains_key(&path) || self.is_dir_in(&items, &path) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", path.display())));
        }
        self.check_parent(&items, &path)?;
        items.insert(path, Item::Dir);
        Ok(())
    }

    pub fn create_dir_all_now(&self, path: &Path) -> io::Result<()> {
        let path = key(path);
        let mut items = self.lock();
        let mut at = PathBuf::new();
        for component in path.components() {
            at.push(component.as_os_str());
            match items.get(&at) {
                Some(Item::Dir) => {}
                Some(Item::File { .. }) => {
                    return Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{} is a file", at.display())));
                }
                None if self.is_dir_in(&items, &at) => {}
                None => {
                    items.insert(at.clone(), Item::Dir);
                }
            }
        }
        Ok(())
    }

    pub fn write_now(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let path = key(path);
        let mut items = self.lock();
        self.check_parent(&items, &path)?;
        if let Some(Item::Dir) = items.get(&path) {
            return Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{} is a directory", path.display())));
        }
        items.insert(path, Item::File { bytes: bytes.to_vec(), modified: now() });
        Ok(())
    }

    pub fn append_now(&self, path: &Path, more: &[u8]) -> io::Result<()> {
        let path = key(path);
        let mut items = self.lock();
        if !items.contains_key(&path) {
            self.check_parent(&items, &path)?;
            items.insert(path.clone(), Item::File { bytes: Vec::new(), modified: now() });
        }
        Self::file_mut(&mut items, &path)?.extend_from_slice(more);
        Ok(())
    }

    pub fn write_at_now(&self, path: &Path, offset: u64, more: &[u8]) -> io::Result<()> {
        let path = key(path);
        let mut items = self.lock();
        let bytes = Self::file_mut(&mut items, &path)?;
        let start = offset as usize;
        let end = start + more.len();
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[start..end].copy_from_slice(more);
        Ok(())
    }

    pub fn rename_now(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (from, to) = (key(from), key(to));
        let mut items = self.lock();
        let Some(item) = items.get(&from).cloned() else { return Err(not_found(&from)) };
        if from == to {
            return Ok(());
        }
        self.check_parent(&items, &to)?;
        match item {
            Item::File { .. } => {
                if let Some(Item::Dir) = items.get(&to) {
                    return Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{} is a directory", to.display())));
                }
                items.remove(&from);
                items.insert(to, item);
            }
            Item::Dir => {
                if to.starts_with(&from) {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("cannot move {} into itself", from.display())));
                }
                match items.get(&to) {
                    Some(Item::File { .. }) => {
                        return Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{} is a file", to.display())));
                    }
                    Some(Item::Dir) if Self::subtree(&items, &to).len() > 1 => {
                        return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, format!("{} is not empty", to.display())));
                    }
                    _ => {}
                }
                for old in Self::subtree(&items, &from) {
                    let moved = items.remove(&old).expect("listed");
                    let rest = old.strip_prefix(&from).expect("under from");
                    items.insert(if rest.as_os_str().is_empty() { to.clone() } else { to.join(rest) }, moved);
                }
            }
        }
        Ok(())
    }

    pub fn remove_file_now(&self, path: &Path) -> io::Result<()> {
        let path = key(path);
        let mut items = self.lock();
        match items.get(&path) {
            Some(Item::File { .. }) => {
                items.remove(&path);
                Ok(())
            }
            Some(Item::Dir) => Err(io::Error::new(io::ErrorKind::IsADirectory, format!("{} is a directory", path.display()))),
            None => Err(not_found(&path)),
        }
    }

    pub fn remove_dir_all_now(&self, path: &Path) -> io::Result<()> {
        let path = key(path);
        let mut items = self.lock();
        match items.get(&path) {
            Some(Item::File { .. }) => {
                return Err(io::Error::new(io::ErrorKind::NotADirectory, format!("{} is a file", path.display())));
            }
            None if !self.is_dir_in(&items, &path) => return Err(not_found(&path)),
            _ => {}
        }
        for p in Self::subtree(&items, &path) {
            items.remove(&p);
        }
        Ok(())
    }

    /// Set a file's modification time (a test making a file look old).
    pub fn set_modified(&self, path: &Path, at: SystemTime) -> io::Result<()> {
        let path = key(path);
        match self.lock().get_mut(&path) {
            Some(Item::File { modified, .. }) => {
                *modified = Some(at);
                Ok(())
            }
            _ => Err(not_found(&path)),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl DiskBackend for MemoryDisk {
    fn kind(&self) -> DiskKind {
        DiskKind::Memory
    }

    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.read_now(path)
    }

    async fn read_range(&self, path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let bytes = self.read_now(path)?;
        let start = offset as usize;
        bytes
            .get(start..start + len)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "failed to fill whole buffer"))
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        self.metadata_now(path)
    }

    async fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        self.list_now(dir)
    }

    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.create_dir_now(path)
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.create_dir_all_now(path)
    }

    async fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.write_now(path, bytes)
    }

    async fn write_private(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.write_now(path, bytes)
    }

    async fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.append_now(path, bytes)
    }

    async fn write_at(&self, path: &Path, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.write_at_now(path, offset, bytes)
    }

    async fn sync_file(&self, path: &Path) -> io::Result<bool> {
        Ok(self.read_now(path).is_ok())
    }

    async fn sync_dir(&self, _path: &Path) {}

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.rename_now(from, to)
    }

    async fn copy(&self, from: &Path, to: &Path) -> io::Result<u64> {
        let bytes = self.read_now(from)?;
        self.write_now(to, &bytes)?;
        Ok(bytes.len() as u64)
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.remove_file_now(path)
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        self.remove_dir_all_now(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Disk, TmpName};

    #[tokio::test]
    async fn keeps_a_filesystems_rules() {
        let disk = Disk::memory();
        let root = Path::new("/a");
        assert_eq!(disk.write(root.join("f"), b"x").await.unwrap_err().kind(), io::ErrorKind::NotFound, "no parent");
        disk.create_dir_all(root.join("b/c")).await.unwrap();
        assert_eq!(disk.create_dir(root.join("b")).await.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        disk.write(root.join("b/c/f"), b"one").await.unwrap();
        assert_eq!(disk.read(root.join("b/c/f")).await.unwrap(), b"one");
        assert!(disk.read(root.join("b/c")).await.is_err(), "a directory is not a file");
        disk.append(root.join("b/c/f"), b" two").await.unwrap();
        disk.write_at(root.join("b/c/f"), 0, b"ONE").await.unwrap();
        assert_eq!(disk.read_to_string(root.join("b/c/f")).await.unwrap(), "ONE two");
        assert_eq!(disk.read_range(root.join("b/c/f"), 4, 3).await.unwrap(), b"two");
        assert!(disk.read_range(root.join("b/c/f"), 4, 30).await.is_err());

        disk.write_atomic(root.join("b/g"), b"atomic", TmpName::Unique).await.unwrap();
        disk.write_durable(root.join("b/h"), b"durable").await.unwrap();
        disk.write_private_atomic(root.join("new/private"), b"secret").await.unwrap();
        let mut names: Vec<String> = disk.list(root.join("b")).await.unwrap().iter().map(|e| e.name().unwrap().to_string()).collect();
        names.sort();
        assert_eq!(names, ["c", "g", "h"], "no temporaries left, and only direct children");

        disk.rename(root.join("b"), root.join("moved")).await.unwrap();
        assert_eq!(disk.read(root.join("moved/c/f")).await.unwrap(), b"ONE two");
        assert!(!disk.exists(root.join("b")).await);
        disk.remove_dir_all(root.join("moved")).await.unwrap();
        assert!(!disk.exists(root.join("moved/c/f")).await);
        assert!(disk.exists(root.join("new/private")).await);
    }

    #[tokio::test]
    async fn roots_it_is_handed_exist() {
        let disk = Disk::new(MemoryDisk::with_roots(|p| p == Path::new("/tmp/root")));
        disk.write("/tmp/root/f", b"x").await.unwrap();
        assert!(disk.is_dir("/tmp/root").await);
        assert!(disk.write("/tmp/other/f", b"x").await.is_err());
    }
}
