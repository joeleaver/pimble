//! Per-origin credentials for connecting to remote Pimble servers
//! (docs/history/HARDENING_CONTRACT.md decision 4): saved once a connection using an
//! explicitly given credential succeeds, and looked up whenever this server
//! connects to a remote and wasn't given one for that call. Never lives in a
//! store directory — `sync.json` always records `auth: none` — and never
//! comes back out through an RPC response (`GetStoreSyncResponse.remote.auth`
//! is always `None`).

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use pimble_core::AuthMethod;
use tokio::sync::RwLock;
use tracing::warn;
use url::Url;

/// `<dirs::config_dir()>/pimble/credentials.json`, used whenever
/// `ServerConfig::credentials_path` is `None`.
pub fn default_credentials_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("credentials.json")
}

/// `url`'s origin as the credentials file keys it: `scheme://host[:port]`,
/// `ws`/`wss` normalized to `http`/`https` (a `RemoteEndpoint` is stored
/// with whichever spelling the caller used), and the scheme's default port
/// omitted so `http://host` and `http://host:80` are the same origin.
pub fn origin_of(url: &Url) -> String {
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        other => other,
    };
    let host = url.host_str().unwrap_or("");
    let default_port = match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    match url.port() {
        Some(port) if Some(port) != default_port => format!("{scheme}://{host}:{port}"),
        _ => format!("{scheme}://{host}"),
    }
}

/// One server's saved credentials, keyed by origin. Loaded once at
/// construction (a missing file starts empty silently — nothing has ever
/// been saved yet; a file that exists but can't be read or parsed also
/// starts empty, logged, rather than failing server startup — the worst
/// case is re-prompting for a credential already saved once, and a `save`
/// only ever happens after that); every [`CredentialStore::save`] persists
/// the whole map with an atomic, mode-`0600` rewrite.
pub struct CredentialStore {
    path: PathBuf,
    entries: RwLock<HashMap<String, AuthMethod>>,
}

impl CredentialStore {
    pub fn new(path: PathBuf) -> Self {
        let entries = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<HashMap<String, AuthMethod>>(&bytes) {
                Ok(entries) => entries,
                Err(e) => {
                    warn!(
                        "Credentials file {} could not be parsed ({}); starting with no saved credentials \
                         (the next save overwrites it)",
                        path.display(), e
                    );
                    HashMap::new()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                warn!(
                    "Credentials file {} could not be read ({}); starting with no saved credentials \
                     (the next save overwrites it)",
                    path.display(), e
                );
                HashMap::new()
            }
        };
        Self { path, entries: RwLock::new(entries) }
    }

    /// The credential to use for a connection to `url`: `requested` if it
    /// is not `AuthMethod::None`, else whatever was last saved for its
    /// origin, else `AuthMethod::None` (decision 4).
    pub async fn resolve(&self, url: &Url, requested: &AuthMethod) -> AuthMethod {
        if !matches!(requested, AuthMethod::None) {
            return requested.clone();
        }
        self.entries.read().await.get(&origin_of(url)).cloned().unwrap_or(AuthMethod::None)
    }

    /// The credential saved for `url`'s origin, if any, without the
    /// `resolve` fallback — used by callers that need to know whether
    /// anything was actually saved.
    pub async fn get(&self, url: &Url) -> Option<AuthMethod> {
        self.entries.read().await.get(&origin_of(url)).cloned()
    }

    /// Record `auth` as the credential for `url`'s origin, overwriting
    /// whatever was saved before, and persist it to disk. Call only after a
    /// connection using `auth` has actually succeeded (decision 4): saving
    /// an unverified credential would let one wrong guess overwrite a
    /// working one.
    pub async fn save(&self, url: &Url, auth: AuthMethod) -> io::Result<()> {
        let origin = origin_of(url);
        let snapshot = {
            let mut entries = self.entries.write().await;
            entries.insert(origin, auth);
            entries.clone()
        };
        self.write_atomic(&snapshot).await
    }

    async fn write_atomic(&self, entries: &HashMap<String, AuthMethod>) -> io::Result<()> {
        let path = self.path.clone();
        let json = serde_json::to_vec_pretty(entries).map_err(io::Error::other)?;
        tokio::task::spawn_blocking(move || crate::fs_util::write_atomic_0600(&path, &json))
            .await
            .map_err(io::Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        s.parse().unwrap()
    }

    #[test]
    fn origins_normalize_scheme_and_default_port() {
        assert_eq!(origin_of(&url("ws://example.com/rpc")), "http://example.com");
        assert_eq!(origin_of(&url("http://example.com:80/rpc")), "http://example.com");
        assert_eq!(origin_of(&url("http://example.com/rpc")), "http://example.com");
        assert_eq!(origin_of(&url("wss://example.com:443/rpc")), "https://example.com");
        assert_eq!(origin_of(&url("http://example.com:8080/rpc")), "http://example.com:8080");
    }

    #[tokio::test]
    async fn resolve_prefers_the_request_then_falls_back_to_saved() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("credentials.json"));
        let u = url("http://example.com");

        assert!(matches!(store.resolve(&u, &AuthMethod::None).await, AuthMethod::None));

        let token = AuthMethod::Bearer { token: "abc".into() };
        store.save(&u, token.clone()).await.unwrap();
        match store.resolve(&u, &AuthMethod::None).await {
            AuthMethod::Bearer { token: t } => assert_eq!(t, "abc"),
            other => panic!("expected the saved bearer token, got {other:?}"),
        }

        let explicit = AuthMethod::ApiKey { key: "xyz".into() };
        match store.resolve(&u, &explicit).await {
            AuthMethod::ApiKey { key } => assert_eq!(key, "xyz"),
            other => panic!("an explicit request auth must win over a saved one, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn saved_credentials_survive_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let u = url("http://example.com");

        {
            let store = CredentialStore::new(path.clone());
            store.save(&u, AuthMethod::Bearer { token: "abc".into() }).await.unwrap();
        }

        let reopened = CredentialStore::new(path.clone());
        match reopened.get(&u).await {
            Some(AuthMethod::Bearer { token }) => assert_eq!(token, "abc"),
            other => panic!("expected the saved token to survive a reload, got {other:?}"),
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "credentials.json must be mode 0600");
        }
    }
}
