//! `cloudAddHostedStore` for a plain store hosted on Pimble Cloud
//! (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 1"): a replica here
//! kept in step by a plain sync link (`crate::sync_link`) that connects with
//! a token minted from the signed-in account (`sync.json`'s `via_account`),
//! on the desktop and in the browser's worker server alike.
//!
//! The vault branch's shape without its keys: the hosted server holds the
//! store's documents in the clear, so there is nothing to fetch or wrap. A
//! whole-store grant makes a whole replica, a share a partial one rooted at
//! the shared nodes; a grant that adds a share of a store already here adds
//! its root to the replica and connects again.

use std::time::Duration;

use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::{AuthMethod, NodeId, RemoteEndpoint, StoreId, SyncState};
use pimble_rpc::OpenStoreResponse;
use pimble_store::{SyncConfig, SyncMode};

use super::{to_rpc_error, RpcHandler};
use crate::cloud::HeldAs;

/// The kind of an accounts-service row for a plain store (`POST /stores`'s
/// `kind`).
pub(crate) const PLAIN_KIND: &str = "plain";

/// How long `cloudAddHostedStore` waits for the first reconcile before it
/// answers, as `addRemoteStore` does.
const FIRST_SYNC_WAIT: Duration = Duration::from_secs(10);

impl RpcHandler {
    /// Add the plain hosted store `store_id` as a replica here, held as
    /// `held_as` says, or add the roots of a share given since to the
    /// partial replica already open here (`open_roots` is `Some` then; the
    /// caller has refused every other store already open).
    pub(super) async fn add_hosted_plain_store(
        &self,
        store_id: StoreId,
        held_as: &HeldAs,
        name: &str,
        already_open: bool,
    ) -> Result<OpenStoreResponse, ErrorObjectOwned> {
        // Where it is: the hosted server, named by the same answer that
        // carries the token (a plain store is never relayed: the relay
        // serves an encrypted twin, docs/RELAY_CONTRACT.md).
        let (_, minted) = crate::grant::mint(self).await.map_err(to_rpc_error)?;
        let rpc_url = minted.reach(store_id, None).map_err(to_rpc_error)?.url;

        if already_open {
            {
                let mut manager = self.store_manager.write().await;
                for root in &held_as.roots {
                    manager.add_scope_root(store_id, *root).await.map_err(to_rpc_error)?;
                }
                // A share granted again is no longer ended; one that has
                // ended since is the link's to find and announce
                // (`crate::grant::refresh`, at the connect below).
                let still_ended: Vec<NodeId> = manager.ended_roots(store_id).into_iter().filter(|root| !held_as.roots.contains(root)).collect();
                manager.set_ended_roots(store_id, still_ended).await.map_err(to_rpc_error)?;
                manager.set_partial_replica_name(store_id, name).await.map_err(to_rpc_error)?;
            }
            self.stop_link(store_id).await;
            self.write_account_sync_config(store_id, &rpc_url, held_as).await?;
            self.ensure_link_started(store_id, RemoteEndpoint { url: rpc_url, auth: AuthMethod::None }).await;
            let mut store = self.store_manager.read().await.get_store_info(store_id).map_err(to_rpc_error)?;
            self.describe_link(&mut store).await;
            self.mark_replica(&mut store);
            return Ok(OpenStoreResponse { store });
        }

        // An empty replica, as for a vault twin: its root is a placeholder
        // in the manifest until the first pull brings the real root's
        // document (`adopt_document_root`), never a document of its own that
        // would merge with the real one into a duplicated tree. A share's
        // replica is partial, rooted at the shared nodes.
        let path = self.default_replica_path(store_id);
        let newly_opened = {
            let mut manager = self.store_manager.write().await;
            if held_as.roots.is_empty() {
                manager.create_replica(&path, store_id, name, NodeId::new()).await.map_err(to_rpc_error)?;
            } else {
                manager.create_partial_replica(&path, store_id, name, held_as.roots.clone()).await.map_err(to_rpc_error)?;
            }
            drop(manager);
            self.write_account_sync_config(store_id, &rpc_url, held_as).await?;
            self.store_manager.write().await.opened_since()
        };
        // Index, folder watch, the link `sync.json` describes, tree repair:
        // everything an opened store gets.
        self.adopt_newly_opened(newly_opened).await;
        self.ensure_link_started(store_id, RemoteEndpoint { url: rpc_url, auth: AuthMethod::None }).await;

        let deadline = self.runtime.now() + FIRST_SYNC_WAIT;
        let sync_state = loop {
            let state = self.sync_state_of(store_id).await;
            if matches!(state, SyncState::Synced { .. }) || self.runtime.now() >= deadline {
                break state;
            }
            self.runtime.sleep(Duration::from_millis(50)).await;
        };
        self.adopt_document_root(store_id).await;
        let mut store = self.store_manager.read().await.get_store_info(store_id).map_err(to_rpc_error)?;
        self.describe_link(&mut store).await;
        store.sync_state = sync_state;
        self.mark_replica(&mut store);
        Ok(OpenStoreResponse { store })
    }

    /// `sync.json` for a plain link held through the account: the hosted
    /// server's address with no credential (the link mints one before every
    /// connect), and how the account holds the store.
    async fn write_account_sync_config(&self, store_id: StoreId, rpc_url: &url::Url, held_as: &HeldAs) -> Result<(), ErrorObjectOwned> {
        let config = SyncConfig {
            remote: RemoteEndpoint { url: rpc_url.clone(), auth: AuthMethod::None },
            last_sync: None,
            mode: SyncMode::Sync,
            via_relay: false,
            via_account: true,
            last_seq: Default::default(),
            vault_key_id: None,
            access: held_as.access,
            shared_by: held_as.shared_by.clone(),
            read_only_roots: held_as.read_only_roots.clone(),
            comment_roots: held_as.comment_roots.clone(),
        };
        self.store_manager.read().await.write_sync_config(store_id, &config).await.map_err(to_rpc_error)
    }
}
