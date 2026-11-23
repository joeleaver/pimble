//! The test suites' disk switch (docs/ONE_CODEBASE_CONTRACT.md section 8:
//! "the server's test suite runs on the memory disk too").
//!
//! `PIMBLE_TEST_DISK=memory cargo test ...` makes [`crate::Disk::default`]
//! answer one [`MemoryDisk`] shared by the whole test process, so every store,
//! state directory, keystore and vault log a test makes lives in memory, and
//! the functions here (a `std::fs`-like set, synchronous like the calls they
//! replace) let the test's own setup and checks read and write the same disk.
//! Unset, or `native`, everything is the machine's filesystem as before.
//!
//! The memory disk takes every real directory directly under the system's
//! temp directory (a test's `TempDir`) and every directory above it as
//! existing: those are the roots a test hands its servers.
//!
//! Only built with the `test-disk` feature, which only test builds turn on.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use super::{Disk, MemoryDisk};

/// The environment variable that picks the disk.
pub const ENV: &str = "PIMBLE_TEST_DISK";

fn memory() -> Option<&'static MemoryDisk> {
    static MEMORY: OnceLock<Option<MemoryDisk>> = OnceLock::new();
    MEMORY
        .get_or_init(|| match std::env::var(ENV).as_deref() {
            Ok("memory") => {
                let temp = std::env::temp_dir();
                Some(MemoryDisk::with_roots(move |p: &Path| temp.starts_with(p) || (p.parent() == Some(temp.as_path()) && p.is_dir())))
            }
            Ok("native") | Ok("") | Err(_) => None,
            Ok(other) => panic!("{ENV}={other}: expected `native` or `memory`"),
        })
        .as_ref()
}

/// The disk `PIMBLE_TEST_DISK` names, when it names the memory one.
pub(crate) fn disk_from_env() -> Option<Disk> {
    memory().map(|m| Disk::new(m.clone()))
}

/// Whether this test process runs on the memory disk: what a test of a
/// native-only part (the folder watch, which a memory disk has nothing for)
/// checks to say so and stop.
pub fn is_memory() -> bool {
    memory().is_some()
}

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    match memory() {
        Some(m) => m.read_now(path.as_ref()),
        None => std::fs::read(path),
    }
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    match memory() {
        Some(m) => m.write_now(path.as_ref(), bytes.as_ref()),
        None => std::fs::write(path, bytes),
    }
}

pub fn exists(path: impl AsRef<Path>) -> bool {
    match memory() {
        Some(m) => m.metadata_now(path.as_ref()).is_ok(),
        None => path.as_ref().exists(),
    }
}

pub fn is_dir(path: impl AsRef<Path>) -> bool {
    match memory() {
        Some(m) => m.metadata_now(path.as_ref()).is_ok_and(|m| m.is_dir),
        None => path.as_ref().is_dir(),
    }
}

/// A file's length.
pub fn len(path: impl AsRef<Path>) -> io::Result<u64> {
    match memory() {
        Some(m) => m.metadata_now(path.as_ref()).map(|m| m.len),
        None => std::fs::metadata(path).map(|m| m.len()),
    }
}

pub fn modified(path: impl AsRef<Path>) -> io::Result<SystemTime> {
    match memory() {
        Some(m) => m.metadata_now(path.as_ref())?.modified.ok_or_else(|| io::Error::other("no modification time")),
        None => std::fs::metadata(path)?.modified(),
    }
}

pub fn set_modified(path: impl AsRef<Path>, at: SystemTime) -> io::Result<()> {
    match memory() {
        Some(m) => m.set_modified(path.as_ref(), at),
        None => std::fs::File::options().write(true).open(path)?.set_modified(at),
    }
}

pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    match memory() {
        Some(m) => m.create_dir_now(path.as_ref()),
        None => std::fs::create_dir(path),
    }
}

pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    match memory() {
        Some(m) => m.create_dir_all_now(path.as_ref()),
        None => std::fs::create_dir_all(path),
    }
}

pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    match memory() {
        Some(m) => m.remove_file_now(path.as_ref()),
        None => std::fs::remove_file(path),
    }
}

pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    match memory() {
        Some(m) => m.remove_dir_all_now(path.as_ref()),
        None => std::fs::remove_dir_all(path),
    }
}

pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    match memory() {
        Some(m) => m.rename_now(from.as_ref(), to.as_ref()),
        None => std::fs::rename(from, to),
    }
}

pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let bytes = read(&from)?;
    write(to, &bytes)?;
    Ok(bytes.len() as u64)
}

/// The paths directly in `dir`, sorted.
pub fn read_dir(dir: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = match memory() {
        Some(m) => m.list_now(dir.as_ref())?.into_iter().map(|e| e.path).collect(),
        None => std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<io::Result<_>>()?,
    };
    paths.sort();
    Ok(paths)
}

/// The names directly in `dir`, sorted.
pub fn names(dir: impl AsRef<Path>) -> io::Result<Vec<String>> {
    Ok(read_dir(dir)?.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect())
}

/// Copy the directory `from` to `to` with everything under it.
pub fn copy_dir(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (from, to) = (from.as_ref(), to.as_ref());
    create_dir_all(to)?;
    for path in read_dir(from)? {
        let target = to.join(path.file_name().expect("an entry has a name"));
        if is_dir(&path) {
            copy_dir(&path, &target)?;
        } else {
            copy(&path, &target)?;
        }
    }
    Ok(())
}
