//! `cloudAddHostedStore`: a store the signed-in Pimble Cloud account holds
//! (its own on another device, or shares of somebody else's) becomes a
//! replica here, kept in step by a link that connects with the account's
//! token: a vault link for an encrypted store, a plain link for a plain one
//! (`hosted_plain`). Also the way a mount resolves its source when the
//! mounting store is itself held through the account (`mount_sources`).

use std::time::Duration;

use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::{NodeId, StoreId, SyncState};
use pimble_rpc::OpenStoreResponse;
use tracing::info;

use super::{hosted_plain, to_rpc_error, RpcHandler, RELAYED_PLACEHOLDER_NAME};
use crate::cloud::HeldAs;
use crate::vault_link::LinkEndpoint;

/// The sentence for a store the signed-in account has no row for: nothing
/// of it is the account's to add, whole or in part. Before 2026-11-25
/// `cloudAddHostedStore` added such a store whole and writable, and the
/// link's first grant refresh ended it at once as "no longer shared".
pub(crate) fn not_held_refusal(store_id: StoreId) -> String {
    format!("The signed-in Pimble Cloud account does not hold store {store_id}, so it cannot be added here.")
}

impl RpcHandler {
    /// Add the hosted store `store_id` as a replica here, as the signed-in
    /// account holds it, or add to the share's replica already here the
    /// roots of a share given since. A store no row names is refused
    /// ([`not_held_refusal`]).
    pub(crate) async fn add_hosted_store(&self, store_id: StoreId) -> Result<OpenStoreResponse, ErrorObjectOwned> {
        info!("Adding hosted store {} as a local replica", store_id);

        let account = self.keystore.account().await.ok_or_else(|| to_rpc_error("no Pimble Cloud account is signed in"))?;

        // How the account holds the store: the whole of it, or shares of it
        // (a row per grant, docs/NODE_DOCUMENT_CONTRACT.md section 5).
        let rows = crate::cloud::list_stores(self.http(), &account.url, &account.session).await.map_err(to_rpc_error)?;
        let held_as = HeldAs::from_rows(&rows, store_id).ok_or_else(|| to_rpc_error(not_held_refusal(store_id)))?;
        // A store shared from its owner's computer has no name on Pimble
        // Cloud (docs/RELAY_CONTRACT.md), and a store's name is in no
        // document: held whole (the owner's other device), it is called what
        // it is until the person renames it here.
        let name = held_as.name.clone().filter(|name| !name.trim().is_empty()).unwrap_or_else(|| {
            if rows.iter().any(|row| row.store_id == store_id.to_string() && row.is_relayed()) {
                RELAYED_PLACEHOLDER_NAME.to_string()
            } else {
                store_id.to_string()
            }
        });

        // Open here already: an error for a whole store, and for a share's
        // replica unless the account has been given another share of the
        // same store since, whose root joins the replica, or has been given
        // again one that had ended (it is still a scope root here, and not
        // shown until it is un-ended).
        let open_roots = {
            let manager = self.store_manager.read().await;
            manager.is_open(store_id).then(|| (manager.scope_roots(store_id), manager.ended_roots(store_id)))
        };
        if let Some((open_roots, ended_roots)) = &open_roots {
            let adds_a_root = !open_roots.is_empty() && held_as.roots.iter().any(|root| !open_roots.contains(root) || ended_roots.contains(root));
            if !adds_a_root {
                return Err(to_rpc_error(format!("store {} is already open here", store_id)));
            }
        }

        // A plain store: its hosted server holds its documents in the clear,
        // so there are no keys to fetch, and a plain link held through the
        // account keeps the replica in step (`hosted_plain`).
        let is_plain = rows.iter().any(|row| row.store_id.eq_ignore_ascii_case(&store_id.to_string()) && row.kind == hosted_plain::PLAIN_KIND);
        if is_plain {
            return self.add_hosted_plain_store(store_id, &held_as, &name, open_roots.is_some()).await;
        }

        // The scope keys this account has been handed: the store key, or
        // each share's. An envelope is believed when the account itself or
        // one of the store's owners signed it.
        let key_id = crate::vault_link::fetch_scope_keys(self, &account, store_id, &held_as.roots)
            .await
            .map_err(to_rpc_error)?
            .ok_or_else(|| {
                to_rpc_error(if held_as.roots.is_empty() {
                    format!("no key envelopes for store {} on this account", store_id)
                } else {
                    format!(
                        "no key for this share has reached this account yet (store {}): it is handed over by one of the owner's devices the next time one is online",
                        store_id
                    )
                })
            })?;

        if open_roots.is_some() {
            // Another root for the replica that is here: the link is
            // restarted so that it lists again and pulls the new scope.
            {
                let mut manager = self.store_manager.write().await;
                for root in &held_as.roots {
                    manager.add_scope_root(store_id, *root).await.map_err(to_rpc_error)?;
                }
                // A share granted again is no longer ended. One that has
                // ended since is the link's to find, and to announce
                // (`crate::vault_link`'s `refresh_grant`, at the connect below).
                let still_ended: Vec<NodeId> = manager.ended_roots(store_id).into_iter().filter(|root| !held_as.roots.contains(root)).collect();
                manager.set_ended_roots(store_id, still_ended).await.map_err(to_rpc_error)?;
                // One share's replica carries that share's name; with
                // another it is "Shared by ...", as one added with both is.
                manager.set_partial_replica_name(store_id, &name).await.map_err(to_rpc_error)?;
            }
            self.stop_vault_link(store_id).await;
            let rpc_url = self.link_hosted_store(store_id, &account, key_id, &held_as).await?;
            self.ensure_vault_link_started(store_id, LinkEndpoint::Remote(rpc_url), key_id).await;
            let mut store = self.store_manager.read().await.get_store_info(store_id).map_err(to_rpc_error)?;
            // `Vault`, and relayed or not: `link_hosted_store` has just
            // written `sync.json`.
            self.describe_link(&mut store).await;
            self.mark_replica(&mut store);
            return Ok(OpenStoreResponse { store });
        }

        // An *empty* replica (never `create_local_store_with`, which would
        // give it its own freshly generated root): a vault store has
        // no plaintext root id to ask for ahead of time the way a `Plain`
        // remote's does for `addRemoteStore`, so this mirrors
        // `StoreManager::create_replica`'s own reasoning exactly — two
        // independently created roots for the same store id would merge
        // into a duplicated, disconnected tree once the vault link pulls
        // the real one. The placeholder root id here is manifest-only
        // bookkeeping, corrected below once the pull lands the real one. A
        // share's replica is partial: its roots are the shared nodes, known
        // now, and the first of them is the root older callers are shown.
        let path = self.default_replica_path(store_id);
        let created_id = {
            let mut manager = self.store_manager.write().await;
            if held_as.roots.is_empty() {
                manager.create_replica(&path, store_id, &name, NodeId::new()).await.map_err(to_rpc_error)?
            } else {
                manager.create_partial_replica(&path, store_id, &name, held_as.roots.clone()).await.map_err(to_rpc_error)?
            }
        };

        self.start_index_open(created_id).await;
        self.watch_folder(created_id).await;

        let rpc_url = self.link_hosted_store(created_id, &account, key_id, &held_as).await?;
        self.ensure_vault_link_started(created_id, LinkEndpoint::Remote(rpc_url), key_id).await;

        // Wait up to 10s for the first pull to land, same as
        // `addRemoteStore` (`create_replica_from`) does for a plain replica,
        // so the response's `root_node_id` reflects the real tree rather
        // than the placeholder above whenever the pull is fast enough.
        let deadline = self.runtime.now() + Duration::from_secs(10);
        let sync_state = loop {
            let state = self.sync_state_of(created_id).await;
            let is_synced = matches!(state, SyncState::Synced { .. });
            if is_synced || self.runtime.now() >= deadline {
                break state;
            }
            self.runtime.sleep(Duration::from_millis(50)).await;
        };

        // The vault link rewrites the manifest root once the pull merges the
        // real tree in (`adopt_document_root`); do the same here so the
        // answer carries it even when the pull was fast. Read after
        // `sync.json` is written, so the answer says how the store is held
        // (`access`, `shared_by`, `roots`).
        self.adopt_document_root(created_id).await;
        let mut store = self.store_manager.read().await.get_store_info(created_id).map_err(to_rpc_error)?;
        // Always `Vault`: `link_hosted_store` above just wrote `sync.json`
        // with `mode: "vault"`, and with whether the store is relayed.
        self.describe_link(&mut store).await;
        store.sync_state = sync_state;
        self.mark_replica(&mut store);

        Ok(OpenStoreResponse { store })
    }
}
