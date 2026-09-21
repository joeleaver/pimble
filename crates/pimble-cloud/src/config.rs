//! Environment variables (docs/CLOUD_CONTRACT.md, section C's table).

use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// `PORT` — what the axum server binds on this host. Default `8080`.
    pub port: u16,
    /// `RHYPEDB_ADDR` — the accounts database's binary-protocol address.
    /// Default `127.0.0.1:4201`.
    pub rhypedb_addr: String,
    /// `PIMBLE_SERVER_URL` — the hosted Pimble server this service creates
    /// stores on. Default `http://127.0.0.1:7462`.
    pub pimble_server_url: String,
    /// `PIMBLE_SERVER_TOKEN` — the service principal's static token.
    pub pimble_server_token: Option<String>,
    /// `PIMBLE_STORES_DIR` — where `POST /stores` asks the Pimble server to
    /// create a new store's directory. Default `/app/data/stores`.
    pub pimble_stores_dir: PathBuf,
    /// `JKBASE_AUTH_ISSUER_URL` — jkbase-Auth's per-project token endpoint
    /// base (e.g. `https://auth.jkbase.app/v1/projects/<id>`). Unset means
    /// development signing.
    pub jkbase_auth_issuer_url: Option<String>,
    /// `JKBASE_AUTH_KEY` — the `jkbk_…` bearer key for `jkbase_auth_issuer_url`.
    pub jkbase_auth_key: Option<String>,
    /// `PIMBLE_CLOUD_DEV_SIGNING_SEED` — 32 bytes hex, the Ed25519 seed for
    /// development-mode local JWT signing (used only when
    /// `jkbase_auth_issuer_url` is unset). A random seed is generated (and a
    /// warning logged) when this is unset too, so a dev server still starts,
    /// but its tokens won't verify across a restart.
    pub dev_signing_seed: Option<String>,
    /// `PIMBLE_CLOUD_PUBLIC_URL` — this service's own public origin, used to
    /// build `iss` for locally-signed tokens, `rpc_url` in `/token`
    /// responses, and whether the session cookie is marked `Secure`. Default
    /// `http://127.0.0.1:8080`.
    pub public_url: String,
    /// `GITHUB_REPO` — `owner/repo` for `/releases`. Default `joeleaver/pimble`.
    pub github_repo: String,
    /// `PIMBLE_CLOUD_RELEASES_BASE_URL` — overrides the GitHub API base URL
    /// `/releases` calls (default `https://api.github.com`). Not part of the
    /// public contract; exists so tests can point it at a local stub instead
    /// of the real network (docs/CLOUD_CONTRACT.md: "handles an HTTP stub
    /// (or skip if it would need the network)").
    pub releases_base_url: Option<String>,
    /// `RESEND_API_KEY` — Phase 1b. Unset means `LogMailer` (verification
    /// links are logged, not emailed).
    pub resend_api_key: Option<String>,
    /// `PIMBLE_MAIL_FROM` — the `from` address on every mail this service
    /// sends. Default `Pimble <no-reply@m.pimble.app>`.
    pub mail_from: String,
    /// `PIMBLE_CLOUD_KDF_DECOY_SECRET` — the HMAC key `GET /kdf` derives an
    /// unknown email's decoy salt from (docs/CRYPTO_CONTRACT.md "Client-
    /// derived login"). Unset means a random secret for this process only
    /// (logs a warning, same pattern as `dev_signing_seed`): the decoy is
    /// still deterministic within one process's lifetime, which is all the
    /// endpoint promises, but restarting changes every unknown email's
    /// decoy salt.
    pub kdf_decoy_secret: Option<String>,
    /// Members plus pending invitations one store may have
    /// (docs/SHARING_CONTRACT.md: "fifty members plus invitations per store");
    /// `PUT members` answers 409 beyond it. Deliberately **not** an
    /// environment variable — the contract fixes the number, and an operator
    /// raising it would quietly change what the service promises. It is a
    /// field rather than a constant only so a test can lower it to something
    /// it can reach in a second.
    pub max_members_per_store: usize,
    /// The relay's limits and timings (docs/RELAY_CONTRACT.md). The contract
    /// fixes them, so like `max_members_per_store` they are **not**
    /// environment variables; a field only so a test can reach a limit
    /// without sixty-four sockets, or an idle timeout without ninety seconds.
    pub relay: crate::relay::RelayLimits,
}

/// The value [`Config::from_env`] always uses for
/// [`Config::max_members_per_store`].
pub const DEFAULT_MAX_MEMBERS_PER_STORE: usize = 50;

fn env_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            port: env_var("PORT").and_then(|v| v.parse().ok()).unwrap_or(8080),
            rhypedb_addr: env_var("RHYPEDB_ADDR").unwrap_or_else(|| "127.0.0.1:4201".to_string()),
            pimble_server_url: env_var("PIMBLE_SERVER_URL").unwrap_or_else(|| "http://127.0.0.1:7462".to_string()),
            pimble_server_token: env_var("PIMBLE_SERVER_TOKEN"),
            pimble_stores_dir: env_var("PIMBLE_STORES_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/app/data/stores")),
            jkbase_auth_issuer_url: env_var("JKBASE_AUTH_ISSUER_URL"),
            jkbase_auth_key: env_var("JKBASE_AUTH_KEY"),
            dev_signing_seed: env_var("PIMBLE_CLOUD_DEV_SIGNING_SEED"),
            public_url: env_var("PIMBLE_CLOUD_PUBLIC_URL").unwrap_or_else(|| "http://127.0.0.1:8080".to_string()),
            github_repo: env_var("GITHUB_REPO").unwrap_or_else(|| "joeleaver/pimble".to_string()),
            releases_base_url: env_var("PIMBLE_CLOUD_RELEASES_BASE_URL"),
            resend_api_key: env_var("RESEND_API_KEY"),
            mail_from: env_var("PIMBLE_MAIL_FROM").unwrap_or_else(|| "Pimble <no-reply@m.pimble.app>".to_string()),
            kdf_decoy_secret: env_var("PIMBLE_CLOUD_KDF_DECOY_SECRET"),
            max_members_per_store: DEFAULT_MAX_MEMBERS_PER_STORE,
            relay: crate::relay::RelayLimits::default(),
        }
    }

    /// Whether the session cookie should be marked `Secure` (docs/
    /// CLOUD_CONTRACT.md: "Secure in prod") — true whenever `public_url` is
    /// `https`.
    pub fn cookie_secure(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}
