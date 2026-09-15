//! The one connection this service holds to the hosted Pimble server, as the
//! service principal (its static token — docs/CLOUD_CONTRACT.md "Identity
//! and grants": "It may do everything, and only the accounts service holds
//! it.").

use std::path::PathBuf;

use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, StoreId, StoreKind};

use crate::config::Config;
use crate::error::CloudResult;

pub struct PimbleService {
    client: PimbleClient,
    stores_dir: PathBuf,
}

impl PimbleService {
    pub async fn connect(config: &Config) -> CloudResult<Self> {
        let auth = match &config.pimble_server_token {
            Some(token) => AuthMethod::Bearer { token: token.clone() },
            None => AuthMethod::None,
        };
        let client = PimbleClient::connect_with_auth(&config.pimble_server_url, &auth)
            .await
            .map_err(|e| crate::error::CloudError::Internal(format!("connecting to Pimble server at {}: {}", config.pimble_server_url, e)))?;
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
}
