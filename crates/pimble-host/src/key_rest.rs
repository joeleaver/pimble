//! Where the server's keystore rests between runs (docs/ONE_CODEBASE_CONTRACT.md
//! sections 6 and 7, "KeyRest"): the signed-in account's unwrapped keys and
//! the store keys it has unwrapped, as the keystore's own bytes.
//!
//! - The desktop: [`FileKeyRest`], `keys.json` mode `0600` in the config
//!   directory, written atomically (as before this seam).
//! - The browser's worker (wave 4): the bytes sealed under the remembered
//!   device key (a non-extractable WebCrypto key in IndexedDB), or, for "don't
//!   remember this browser", [`MemoryKeyRest`].
//! - A phone app: the platform's keychain, or a file in its sandbox.

use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::Disk;

/// See the module documentation.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait KeyRest: Send + Sync + std::fmt::Debug {
    /// What rests here; `Ok(None)` when nothing does yet.
    async fn load(&self) -> io::Result<Option<Vec<u8>>>;
    /// Replace what rests here with `bytes`, whole or not at all.
    async fn save(&self, bytes: &[u8]) -> io::Result<()>;
    /// Where it rests, for a log line.
    fn describe(&self) -> String;
    /// The signed-in account's notice key (docs/NOTIFICATIONS_CONTRACT.md,
    /// wave 2), `(user id, X25519 secret)`, or `None` once nobody is signed
    /// in: for a rest that hands it to something outside the server that
    /// opens notices on its own, the browser's service worker showing a push
    /// while no page is open. Nothing by default: the desktop shows its own.
    async fn notice_key(&self, _account: Option<(&str, &[u8; 32])>) -> io::Result<()> {
        Ok(())
    }
}

/// A file on a [`Disk`], readable by this user alone: the desktop's
/// `keys.json`.
#[derive(Debug, Clone)]
pub struct FileKeyRest {
    disk: Disk,
    path: PathBuf,
}

impl FileKeyRest {
    pub fn new(disk: Disk, path: PathBuf) -> Self {
        Self { disk, path }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl KeyRest for FileKeyRest {
    async fn load(&self) -> io::Result<Option<Vec<u8>>> {
        match self.disk.read(&self.path).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn save(&self, bytes: &[u8]) -> io::Result<()> {
        self.disk.write_private_atomic(&self.path, bytes).await
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// Nothing rests: the keys last as long as the process.
#[derive(Debug, Default)]
pub struct MemoryKeyRest(Mutex<Option<Vec<u8>>>);

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl KeyRest for MemoryKeyRest {
    async fn load(&self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap_or_else(|p| p.into_inner()).clone())
    }

    async fn save(&self, bytes: &[u8]) -> io::Result<()> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(bytes.to_vec());
        Ok(())
    }

    fn describe(&self) -> String {
        "memory".to_string()
    }
}
