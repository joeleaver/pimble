//! The one connection this service holds to the hosted Pimble server, as the
//! service principal (its static token — docs/CLOUD_CONTRACT.md "Identity
//! and grants": "It may do everything, and only the accounts service holds
//! it.").
//!
//! Talks to the Pimble server over a raw jsonrpsee connection (`PimbleApiClient`,
//! generated in `pimble-rpc`) rather than through `pimble_client::PimbleClient`:
//! `createStore` now takes `kind` and an optional `store_id`
//! (docs/CRYPTO_CONTRACT.md), but `PimbleClient::create_store`'s convenience
//! wrapper hasn't been extended to pass them, and `pimble-client/src/client.rs`
//! has a concurrent editor as of this writing (agent A/B's vault-RPC work) —
//! adding a method there risks clobbering in-flight work outside this crate's
//! scope. This connects the same way `PimbleClient::connect_with_auth` does
//! (a `WsClientBuilder` with the token in an `Authorization` header) and
//! calls `create_store` directly.

use std::path::PathBuf;

use jsonrpsee::ws_client::{HeaderMap, HeaderValue, WsClient, WsClientBuilder};
use pimble_core::{StoreId, StoreKind};
use pimble_rpc::{CreateStoreRequest, PimbleApiClient};

use crate::config::Config;
use crate::error::{CloudError, CloudResult};

pub struct PimbleService {
    client: WsClient,
    stores_dir: PathBuf,
}

impl PimbleService {
    pub async fn connect(config: &Config) -> CloudResult<Self> {
        let ws_url = to_ws_url(&config.pimble_server_url)?;
        let mut builder = WsClientBuilder::default();
        if let Some(token) = &config.pimble_server_token {
            let mut headers = HeaderMap::new();
            let value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| CloudError::Internal(format!("invalid PIMBLE_SERVER_TOKEN for a header: {e}")))?;
            headers.insert("authorization", value);
            builder = builder.set_headers(headers);
        }
        let client = builder
            .build(&ws_url)
            .await
            .map_err(|e| CloudError::Internal(format!("connecting to Pimble server at {}: {e}", config.pimble_server_url)))?;
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
        let request = CreateStoreRequest { path, name: name.to_string(), kind, store_id };
        let response = self
            .client
            .create_store(request)
            .await
            .map_err(|e| CloudError::Internal(format!("pimble server: createStore failed: {e}")))?;
        Ok((response.store_id, dir_name))
    }
}

/// `http(s)://` to `ws(s)://`; `ws(s)://` passes through unchanged — the same
/// scheme swap `PimbleClient::connect_with_auth` does.
fn to_ws_url(url: &str) -> CloudResult<String> {
    if let Some(rest) = url.strip_prefix("http://") {
        Ok(format!("ws://{rest}"))
    } else if let Some(rest) = url.strip_prefix("https://") {
        Ok(format!("wss://{rest}"))
    } else if url.starts_with("ws://") || url.starts_with("wss://") {
        Ok(url.to_string())
    } else {
        Err(CloudError::Internal(format!("PIMBLE_SERVER_URL has an unsupported scheme: {url}")))
    }
}
