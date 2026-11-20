//! A `PimbleServer` that owns the temp directory its per-computer paths live
//! in (`ServerConfig::in_dir`: replicas, each store's state, relay twins,
//! keystore, credentials, agents), for tests whose helpers hand back a server
//! alone: the directory lives exactly as long as the server, and nothing the
//! server does reaches the real config or data directories.
//! `tests/test_isolation.rs` checks every test starts its servers this way.

// Not every test file uses every helper.
#![allow(dead_code)]

use std::ops::{Deref, DerefMut};
use std::path::Path;

use pimble_server::{PimbleServer, ServerConfig};
use tempfile::TempDir;

pub struct TempServer {
    server: PimbleServer,
    // Dropped after `server` (fields drop in order).
    dir: TempDir,
}

impl TempServer {
    /// A server built from `config`, which is handed the fresh directory to
    /// build on: `|dir| ServerConfig { auth_token, ..ServerConfig::in_dir(dir) }`.
    pub fn with_config(config: impl FnOnce(&Path) -> ServerConfig) -> Self {
        let dir = tempfile::tempdir().expect("a temp directory");
        let server = PimbleServer::with_config(config(dir.path()));
        Self { server, dir }
    }

    /// A plain loopback server on a free port, in its own directory.
    pub fn isolated() -> Self {
        Self::with_config(ServerConfig::in_dir)
    }

    /// The directory this server keeps its per-computer state in.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }
}

impl Deref for TempServer {
    type Target = PimbleServer;
    fn deref(&self) -> &PimbleServer {
        &self.server
    }
}

impl DerefMut for TempServer {
    fn deref_mut(&mut self) -> &mut PimbleServer {
        &mut self.server
    }
}
