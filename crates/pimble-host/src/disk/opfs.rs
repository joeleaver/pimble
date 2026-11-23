//! The browser's disk: the origin private file system (OPFS), from the
//! dedicated worker the server runs in (docs/ONE_CODEBASE_CONTRACT.md seam B,
//! section 10). Plain bytes in, plain bytes out: the worker puts a
//! [`super::SealedDisk`] over this, so what OPFS holds is sealed with the disk
//! key.
//!
//! A path's components are walked as directory handles from the OPFS root.
//! A whole file is written with `createWritable()` and `close()`, which
//! commits it at once at `close` (a tab killed mid-write leaves the old file,
//! never half of the new one), so the atomic compositions need no temporary
//! and no rename. Where an engine has no `createWritable` in a worker (Safari
//! before 26), the file is written through a synchronous access handle
//! (truncate, write, flush), the contract's fallback: not atomic against a
//! kill mid-write, which the seal then reports as a file that does not open.
//! What OPFS has no call for (an append, a write at an offset, a rename, a
//! copy) is a read and a whole write.
//!
//! Operations on one file are taken one at a time in this worker (a sync
//! access handle is exclusive, and a read-then-write must not interleave with
//! another).

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Blob, File, FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions, FileSystemHandle, FileSystemHandleKind,
    FileSystemRemoveOptions, FileSystemSyncAccessHandle, FileSystemWritableFileStream, StorageManager,
};

use super::{DiskBackend, DiskKind, Entry, Metadata, TmpName};

/// See the module documentation. Holds no JavaScript value (the handles live
/// in this worker's thread-locals), so it is `Send` and `Sync` as the trait
/// asks.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpfsDisk;

thread_local! {
    static ROOT: RefCell<Option<FileSystemDirectoryHandle>> = const { RefCell::new(None) };
    static LOCKS: RefCell<HashMap<String, Rc<futures::lock::Mutex<()>>>> = RefCell::new(HashMap::new());
    /// Directory handles already walked to, by path: a store's open reads
    /// hundreds of files from one directory, and walking each path's
    /// components again from the root costs an await per component. Emptied
    /// whenever a directory is removed or moved.
    static DIRS: RefCell<HashMap<String, FileSystemDirectoryHandle>> = RefCell::new(HashMap::new());
    static WRITABLE: RefCell<Option<bool>> = const { RefCell::new(None) };
}

impl OpfsDisk {
    /// Whether this engine gives OPFS to this context at all (it does in
    /// every current engine's dedicated workers; not in Firefox's private
    /// windows before 115, for one).
    pub async fn available() -> bool {
        root().await.is_ok()
    }

    /// Remove everything this origin keeps in OPFS ("Forget This Browser",
    /// "Sign Out": docs/ONE_CODEBASE_CONTRACT.md section 7).
    pub async fn clear() -> io::Result<()> {
        let root = root().await?;
        for (name, _) in entries(&root).await? {
            remove_entry(&root, &name, true).await?;
        }
        Ok(())
    }

    /// The names at the root, for a check that it is empty.
    pub async fn root_names() -> io::Result<Vec<String>> {
        let root = root().await?;
        Ok(entries(&root).await?.into_iter().map(|(name, _)| name).collect())
    }
}

fn js_error(what: &str, path: &Path, e: JsValue) -> io::Error {
    let name = Reflect::get(&e, &JsValue::from_str("name")).ok().and_then(|n| n.as_string()).unwrap_or_default();
    let message = Reflect::get(&e, &JsValue::from_str("message")).ok().and_then(|m| m.as_string()).unwrap_or_else(|| format!("{e:?}"));
    let kind = match name.as_str() {
        "NotFoundError" => io::ErrorKind::NotFound,
        "TypeMismatchError" => io::ErrorKind::InvalidInput,
        "InvalidModificationError" => io::ErrorKind::DirectoryNotEmpty,
        "NoModificationAllowedError" => io::ErrorKind::WouldBlock,
        "QuotaExceededError" => io::ErrorKind::StorageFull,
        "NotAllowedError" | "SecurityError" => io::ErrorKind::PermissionDenied,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("OPFS {what} {}: {name} {message}", path.display()))
}

async fn wait(promise: js_sys::Promise) -> Result<JsValue, JsValue> {
    JsFuture::from(promise).await
}

async fn root() -> io::Result<FileSystemDirectoryHandle> {
    if let Some(root) = ROOT.with(|r| r.borrow().clone()) {
        return Ok(root);
    }
    let unavailable = |e: JsValue| js_error("root", Path::new("/"), e);
    let navigator = Reflect::get(&js_sys::global(), &JsValue::from_str("navigator")).map_err(unavailable)?;
    let storage = Reflect::get(&navigator, &JsValue::from_str("storage")).map_err(unavailable)?;
    if storage.is_undefined() {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "this context has no navigator.storage (OPFS)"));
    }
    let storage: StorageManager = storage.unchecked_into();
    let root: FileSystemDirectoryHandle = wait(storage.get_directory()).await.map_err(unavailable)?.unchecked_into();
    ROOT.with(|r| *r.borrow_mut() = Some(root.clone()));
    Ok(root)
}

/// A path's names from the root.
fn names(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn key(path: &Path) -> String {
    names(path).join("/")
}

/// One file's lock in this worker.
fn lock_of(path: &Path) -> Rc<futures::lock::Mutex<()>> {
    LOCKS.with(|locks| locks.borrow_mut().entry(key(path)).or_insert_with(|| Rc::new(futures::lock::Mutex::new(()))).clone())
}

async fn dir_handle(path: &Path, create: bool) -> io::Result<FileSystemDirectoryHandle> {
    let whole = key(path);
    if let Some(dir) = DIRS.with(|d| d.borrow().get(&whole).cloned()) {
        return Ok(dir);
    }
    let mut dir = root().await?;
    let mut walked = String::new();
    for name in names(path) {
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(&name);
        if let Some(known) = DIRS.with(|d| d.borrow().get(&walked).cloned()) {
            dir = known;
            continue;
        }
        let options = FileSystemGetDirectoryOptions::new();
        options.set_create(create);
        dir = wait(dir.get_directory_handle_with_options(&name, &options)).await.map_err(|e| js_error("directory", path, e))?.unchecked_into();
        DIRS.with(|d| d.borrow_mut().insert(walked.clone(), dir.clone()));
    }
    Ok(dir)
}

/// Forget every directory handle held (a directory went or moved).
fn forget_dirs() {
    DIRS.with(|d| d.borrow_mut().clear());
}

/// The directory a path is in, and its last name; `None` for the root.
async fn parent_of(path: &Path, create: bool) -> io::Result<Option<(FileSystemDirectoryHandle, String)>> {
    let mut all = names(path);
    let Some(last) = all.pop() else { return Ok(None) };
    let parent: PathBuf = all.iter().collect();
    Ok(Some((dir_handle(&parent, create).await?, last)))
}

async fn file_handle(path: &Path, create: bool) -> io::Result<FileSystemFileHandle> {
    let (dir, name) = parent_of(path, false).await?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the root is not a file"))?;
    let options = FileSystemGetFileOptions::new();
    options.set_create(create);
    Ok(wait(dir.get_file_handle_with_options(&name, &options)).await.map_err(|e| js_error("file", path, e))?.unchecked_into())
}

async fn entries(dir: &FileSystemDirectoryHandle) -> io::Result<Vec<(String, bool)>> {
    let iter = dir.values();
    let mut out = Vec::new();
    loop {
        let step = wait(iter.next().map_err(|e| js_error("list", Path::new(""), e))?).await.map_err(|e| js_error("list", Path::new(""), e))?;
        if Reflect::get(&step, &JsValue::from_str("done")).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
            break;
        }
        let handle: FileSystemHandle = Reflect::get(&step, &JsValue::from_str("value")).map_err(|e| js_error("list", Path::new(""), e))?.unchecked_into();
        out.push((handle.name(), handle.kind() == FileSystemHandleKind::Directory));
    }
    Ok(out)
}

async fn remove_entry(dir: &FileSystemDirectoryHandle, name: &str, recursive: bool) -> io::Result<()> {
    if recursive {
        forget_dirs();
    }
    let options = FileSystemRemoveOptions::new();
    options.set_recursive(recursive);
    wait(dir.remove_entry_with_options(name, &options)).await.map(|_| ()).map_err(|e| js_error("remove", Path::new(name), e))
}

async fn read_file(path: &Path) -> io::Result<(Vec<u8>, Option<SystemTime>)> {
    let handle = file_handle(path, false).await?;
    let file: File = wait(handle.get_file()).await.map_err(|e| js_error("read", path, e))?.unchecked_into();
    let modified = UNIX_EPOCH.checked_add(Duration::from_millis(file.last_modified().max(0.0) as u64));
    let blob: &Blob = file.as_ref();
    let buffer = wait(blob.array_buffer()).await.map_err(|e| js_error("read", path, e))?;
    Ok((Uint8Array::new(&buffer).to_vec(), modified))
}

/// Whether this engine has `createWritable` on a file handle here.
fn has_create_writable(handle: &FileSystemFileHandle) -> bool {
    if let Some(known) = WRITABLE.with(|w| *w.borrow()) {
        return known;
    }
    let has = Reflect::get(handle, &JsValue::from_str("createWritable")).is_ok_and(|f| f.is_function());
    if !has {
        tracing::info!("This engine has no createWritable in a worker; OPFS files are written through sync access handles");
    }
    WRITABLE.with(|w| *w.borrow_mut() = Some(has));
    has
}

/// Replace a file's contents whole: committed at `close` with
/// `createWritable`, else through a sync access handle.
async fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let handle = file_handle(path, true).await?;
    if has_create_writable(&handle) {
        let stream: FileSystemWritableFileStream = wait(handle.create_writable()).await.map_err(|e| js_error("write", path, e))?.unchecked_into();
        let written = async {
            wait(stream.write_with_u8_array(bytes).map_err(|e| js_error("write", path, e))?).await.map_err(|e| js_error("write", path, e))?;
            wait(stream.close()).await.map_err(|e| js_error("write", path, e))?;
            Ok::<(), io::Error>(())
        }
        .await;
        if written.is_err() {
            // A stream not closed is thrown away: the old file stays.
            let _ = Reflect::get(&stream, &JsValue::from_str("abort")).ok().and_then(|f| f.dyn_into::<js_sys::Function>().ok()).map(|f| f.call0(&stream));
        }
        return written;
    }
    // A sync access handle is exclusive: one left open by a worker that is
    // still going away (the leader's hand-over) is waited out.
    let mut tries = 0;
    let access: FileSystemSyncAccessHandle = loop {
        match wait(handle.create_sync_access_handle()).await {
            Ok(access) => break access.unchecked_into(),
            Err(e) => {
                let err = js_error("open", path, e);
                tries += 1;
                if err.kind() != io::ErrorKind::WouldBlock || tries > 50 {
                    return Err(err);
                }
                gloo_timers::future::TimeoutFuture::new(20).await;
            }
        }
    };
    let written = (|| {
        access.truncate_with_f64(0.0)?;
        let mut at = 0usize;
        while at < bytes.len() {
            let options = web_sys::FileSystemReadWriteOptions::new();
            options.set_at(at as f64);
            let n = access.write_with_u8_array_and_options(&bytes[at..], &options)? as usize;
            if n == 0 {
                return Err(JsValue::from_str("a write took no bytes"));
            }
            at += n;
        }
        access.flush()
    })()
    .map_err(|e| js_error("write", path, e));
    access.close();
    written
}

#[async_trait::async_trait(?Send)]
impl DiskBackend for OpfsDisk {
    fn kind(&self) -> DiskKind {
        DiskKind::Other
    }

    async fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let lock = lock_of(path);
        let _held = lock.lock().await;
        read_file(path).await.map(|(bytes, _)| bytes)
    }

    async fn read_range(&self, path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let bytes = self.read(path).await?;
        let start = offset as usize;
        bytes.get(start..start.saturating_add(len)).map(<[u8]>::to_vec).ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let Some((dir, name)) = parent_of(path, false).await? else {
            root().await?;
            return Ok(Metadata { len: 0, is_dir: true, modified: None });
        };
        let options = FileSystemGetFileOptions::new();
        match wait(dir.get_file_handle_with_options(&name, &options)).await {
            Ok(handle) => {
                let handle: FileSystemFileHandle = handle.unchecked_into();
                let file: File = wait(handle.get_file()).await.map_err(|e| js_error("metadata", path, e))?.unchecked_into();
                let blob: &Blob = file.as_ref();
                Ok(Metadata { len: blob.size() as u64, is_dir: false, modified: UNIX_EPOCH.checked_add(Duration::from_millis(file.last_modified().max(0.0) as u64)) })
            }
            Err(e) => {
                let err = js_error("metadata", path, e);
                if err.kind() != io::ErrorKind::InvalidInput {
                    return Err(err);
                }
                // A directory by that name.
                Ok(Metadata { len: 0, is_dir: true, modified: None })
            }
        }
    }

    async fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let handle = dir_handle(dir, false).await?;
        Ok(entries(&handle).await?.into_iter().map(|(name, is_dir)| Entry { path: dir.join(name), is_dir }).collect())
    }

    async fn create_dir(&self, path: &Path) -> io::Result<()> {
        if self.metadata(path).await.is_ok() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists", path.display())));
        }
        let Some((parent, name)) = parent_of(path, false).await? else { return Ok(()) };
        let options = FileSystemGetDirectoryOptions::new();
        options.set_create(true);
        wait(parent.get_directory_handle_with_options(&name, &options)).await.map(|_| ()).map_err(|e| js_error("create directory", path, e))
    }

    async fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        dir_handle(path, true).await.map(|_| ())
    }

    async fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let lock = lock_of(path);
        let _held = lock.lock().await;
        write_whole(path, bytes).await
    }

    async fn write_private(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.write(path, bytes).await
    }

    async fn append(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let lock = lock_of(path);
        let _held = lock.lock().await;
        let mut whole = match read_file(path).await {
            Ok((whole, _)) => whole,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        whole.extend_from_slice(bytes);
        write_whole(path, &whole).await
    }

    async fn write_at(&self, path: &Path, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let lock = lock_of(path);
        let _held = lock.lock().await;
        let (mut whole, _) = read_file(path).await?;
        let start = offset as usize;
        let end = start + bytes.len();
        if whole.len() < end {
            whole.resize(end, 0);
        }
        whole[start..end].copy_from_slice(bytes);
        write_whole(path, &whole).await
    }

    async fn sync_file(&self, path: &Path) -> io::Result<bool> {
        // Every write is committed when it answers.
        Ok(self.metadata(path).await.is_ok_and(|m| !m.is_dir))
    }

    async fn sync_dir(&self, _path: &Path) {}

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if key(from) == key(to) {
            return self.metadata(from).await.map(|_| ());
        }
        let meta = self.metadata(from).await?;
        if meta.is_dir {
            self.create_dir_all(to).await?;
            for entry in self.list(from).await? {
                let name = entry.path.file_name().map(PathBuf::from).unwrap_or_default();
                Box::pin(self.rename(&entry.path, &to.join(name))).await?;
            }
            return self.remove_dir_all(from).await;
        }
        let bytes = self.read(from).await?;
        self.write(to, &bytes).await?;
        self.remove_file(from).await
    }

    async fn copy(&self, from: &Path, to: &Path) -> io::Result<u64> {
        let bytes = self.read(from).await?;
        self.write(to, &bytes).await?;
        Ok(bytes.len() as u64)
    }

    async fn remove_file(&self, path: &Path) -> io::Result<()> {
        let lock = lock_of(path);
        let _held = lock.lock().await;
        let (dir, name) = parent_of(path, false).await?.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the root is not a file"))?;
        remove_entry(&dir, &name, false).await
    }

    async fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        match parent_of(path, false).await? {
            Some((dir, name)) => remove_entry(&dir, &name, true).await,
            None => OpfsDisk::clear().await,
        }
    }

    // A whole file is committed at once, so the compositions are one write.
    async fn write_atomic(&self, path: &Path, bytes: &[u8], _tmp: TmpName) -> io::Result<()> {
        self.write(path, bytes).await
    }

    async fn write_durable(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.write(path, bytes).await
    }

    async fn write_private_atomic(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            self.create_dir_all(parent).await?;
        }
        self.write(path, bytes).await
    }
}
