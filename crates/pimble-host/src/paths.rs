//! Where a server keeps what is its own (docs/ONE_CODEBASE_CONTRACT.md seam
//! E): a value handed to it, never looked up by the server's core. The
//! desktop's comes from the platform's directories ([`Paths::native`]); the
//! browser's worker names OPFS folders, a phone app its sandbox, a test a
//! temp directory ([`Paths::in_dir`]).

use std::path::{Path, PathBuf};

/// The roots a server keeps its files under, each already `.../pimble`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Data that is the server's to keep: replicas, relay twins
    /// (`<data dir>/pimble` on the desktop).
    pub data: PathBuf,
    /// Settings and secrets: the keystore, saved credentials, the agents file
    /// (`<config dir>/pimble` on the desktop).
    pub config: PathBuf,
    /// Each store's per-computer state (`<data dir>/pimble/stores`:
    /// `<state>/<store id>/sync-state.json`, its index, its people).
    pub state: PathBuf,
}

impl Paths {
    /// A host's own roots: `data` and `config`, with the state root under
    /// `data`.
    pub fn new(data: impl Into<PathBuf>, config: impl Into<PathBuf>) -> Self {
        let data = data.into();
        Self { state: data.join("stores"), data, config: config.into() }
    }

    /// Everything under one directory, as `ServerConfig::in_dir` lays a
    /// test's server out: `dir/replicas`, `dir/stores`, `dir/relay`,
    /// `dir/keys.json`, `dir/credentials.json`, `dir/agents.json`.
    pub fn in_dir(dir: &Path) -> Self {
        Self::new(dir, dir)
    }

    /// The desktop's: `<data local dir>/pimble` and `<config dir>/pimble`
    /// (`dirs`; the working directory when the platform names none).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn native() -> Self {
        let data = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble");
        let config = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble");
        Self::new(data, config)
    }

    /// Where replicas are created (`addRemoteStore`, `cloudAddHostedStore`).
    pub fn replicas(&self) -> PathBuf {
        self.data.join("replicas")
    }

    /// Where relayed stores' twins live (docs/RELAY_CONTRACT.md).
    pub fn relay(&self) -> PathBuf {
        self.data.join("relay")
    }

    /// Where diagnostic records wait to be sent, and this install's number
    /// (docs/DIAGNOSTICS_CONTRACT.md): the desktop's programs and a browser's
    /// server alike.
    pub fn diagnostics(&self) -> PathBuf {
        self.data.join("diagnostics")
    }
    pub fn keystore(&self) -> PathBuf {
        self.config.join("keys.json")
    }

    pub fn credentials(&self) -> PathBuf {
        self.config.join("credentials.json")
    }

    pub fn agents(&self) -> PathBuf {
        self.config.join("agents.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layouts() {
        let p = Paths::in_dir(Path::new("/t"));
        assert_eq!(p.replicas(), Path::new("/t/replicas"));
        assert_eq!(p.state, Path::new("/t/stores"));
        assert_eq!(p.relay(), Path::new("/t/relay"));
        assert_eq!(p.keystore(), Path::new("/t/keys.json"));
        assert_eq!(p.credentials(), Path::new("/t/credentials.json"));
        assert_eq!(p.agents(), Path::new("/t/agents.json"));
    }
}
