//! The one connection this service holds to the hosted Pimble server, as the
//! service principal (its static token — docs/CLOUD_CONTRACT.md "Identity
//! and grants": "It may do everything, and only the accounts service holds
//! it.").

use std::path::PathBuf;

use pimble_client::{ClientError, PimbleClient};
use pimble_core::{AuthMethod, StoreId, StoreKind};

use crate::config::Config;
use crate::error::CloudResult;

pub struct PimbleService {
    client: PimbleClient,
    stores_dir: PathBuf,
}

/// Connect to the Pimble server at `url`, retrying for up to `wait` while
/// nothing answers there. A server that answers, even with a refusal (401,
/// 403), and a connection still failing after `wait`, are returned as before.
async fn connect_waiting(url: &str, auth: &AuthMethod, wait: std::time::Duration) -> CloudResult<PimbleClient> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut delay = std::time::Duration::from_millis(100);
    let mut logged = false;
    loop {
        match PimbleClient::connect_with_auth(url, auth).await {
            Ok(client) => return Ok(client),
            Err(ClientError::Connection(e)) if !e.contains("status code") && tokio::time::Instant::now() + delay < deadline => {
                if !logged {
                    tracing::info!(%url, error = %e, "the Pimble server is not accepting connections yet; waiting");
                    logged = true;
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(2));
            }
            Err(e) => return Err(crate::error::CloudError::Internal(format!("connecting to Pimble server at {}: {}", url, e))),
        }
    }
}

impl PimbleService {
    pub async fn connect(config: &Config) -> CloudResult<Self> {
        let auth = match &config.pimble_server_token {
            Some(token) => AuthMethod::Bearer { token: token.clone() },
            None => AuthMethod::None,
        };
        let client = connect_waiting(&config.pimble_server_url, &auth, crate::DEPENDENCY_WAIT).await?;
        Ok(Self { client, stores_dir: config.pimble_stores_dir.clone() })
    }

    /// Create a new hosted store named `name`, of the given `kind`, at a
    /// fresh path inside the configured stores directory. `store_id`, when
    /// given, asks the Pimble server to create it under that exact id (so
    /// the hosted twin of a local store shares the local store's id;
    /// refused server-side if that id is already open). Returns the Pimble
    /// server's own `StoreId` and the directory name it was created under
    /// (what `HostedStore::dir_name` records).
    pub async fn create_store(&self, name: &str, kind: StoreKind, store_id: Option<StoreId>) -> CloudResult<(StoreId, String)> {
        let dir_name = format!("{}.pimble", store_id.map(|id| id.as_uuid().to_string()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()));
        let path = self.stores_dir.join(&dir_name);
        let (store_id, _root_node_id) = self.client.create_store_with(&path, name, kind, store_id).await?;
        Ok((store_id, dir_name))
    }

    /// Close a hosted **vault** store and delete its directory
    /// (docs/SHARING_CONTRACT.md: `deleteVaultStore`, Service-only) — what
    /// `DELETE /stores/{id}` does for a vault store, so the ciphertext of a
    /// share goes with the share. The caller must treat a failure as
    /// non-fatal: the accounts row is marked deleted either way, and a store
    /// left behind on the hosted disk is unreachable (no grant names it any
    /// more) rather than dangerous.
    pub async fn delete_vault_store(&self, store_id: StoreId) -> CloudResult<()> {
        self.client.delete_vault_store(store_id).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A Pimble server with `token`, on `port` (0 for any), and a temporary
    /// directory for what it keeps.
    async fn pimble_server(port: u16, token: &str) -> (pimble_server::PimbleServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut server = pimble_server::PimbleServer::with_config(pimble_server::ServerConfig {
            addr: format!("127.0.0.1:{port}").parse().unwrap(),
            auth_token: Some(token.to_string()),
            credentials_path: Some(dir.path().join("credentials.json")),
            replicas_dir: Some(dir.path().join("replicas")),
            ..Default::default()
        });
        server.start().await.expect("server starts");
        (server, dir)
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// The hosted server starting a moment after this service is the jkbase
    /// deployment case: the connect waits for it instead of failing.
    #[tokio::test]
    async fn connect_waits_for_a_pimble_server_that_is_still_starting() {
        let port = free_port();
        let server = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            pimble_server(port, "t").await
        });
        let auth = AuthMethod::Bearer { token: "t".to_string() };
        connect_waiting(&format!("http://127.0.0.1:{port}"), &auth, Duration::from_secs(10))
            .await
            .expect("connected once the server listened");
        let (mut server, _dir) = server.await.unwrap();
        server.stop().await.unwrap();
    }

    /// A server that answers with a refusal is up: waiting would only delay
    /// the error.
    #[tokio::test]
    async fn a_refusal_is_not_waited_out() {
        let (mut server, _dir) = pimble_server(0, "right").await;
        let auth = AuthMethod::Bearer { token: "wrong".to_string() };
        let started = std::time::Instant::now();
        let err = connect_waiting(&format!("http://{}", server.addr()), &auth, Duration::from_secs(30))
            .await
            .err()
            .expect("the token is refused");
        assert!(started.elapsed() < Duration::from_secs(5), "waited on a refusal: {:?} ({err})", started.elapsed());
        server.stop().await.unwrap();
    }
}
