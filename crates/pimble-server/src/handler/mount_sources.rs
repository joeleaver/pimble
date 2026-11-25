//! Where a mount's source store is looked for when it is not on this server
//! (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 1), and making a replica
//! of it from there: `RpcHandler::resolve_mount`'s remote half.
//!
//! A mounting store held through the signed-in Pimble Cloud account (a plain
//! replica whose `sync.json` says `via_account`, or one kept by a vault link)
//! has no credential for its remote: the link mints the account's token
//! before every connect. Its mount sources are reached the same way, through
//! the account (`RpcHandler::add_hosted_store`, what `cloudAddHostedStore`
//! does), never through the mounting store's address with whatever
//! credential `credentials.json` holds for it (docs/ONE_CODEBASE_CONTRACT.md,
//! "Built: wave 5, part 1").

use pimble_core::{AuthMethod, MountRef, RemoteEndpoint, StoreId};
use tracing::{debug, info, warn};

use super::RpcHandler;

/// One place a mount's source may be had from, in the order they are tried.
#[derive(Debug, Clone)]
pub(super) enum SourceRoute {
    /// Through the signed-in account: the source as the account holds it.
    Account,
    /// A remote server, with the credential this server saved for it.
    Remote(RemoteEndpoint),
}

impl RpcHandler {
    /// Where `mount_ref`'s source might be had, in the order decision 1
    /// tries them. A mounting store held through the account: the account
    /// first, then the mount ref's `source_remote` when it names another
    /// server than the mounting store's. Otherwise the mount ref's
    /// `source_remote` (a URL only, never a credential), then the mounting
    /// store's own remote from its `sync.json`. A remote is returned with
    /// `AuthMethod::None`, so [`Self::connect_to_remote`] resolves whatever
    /// credential this server has saved for that origin.
    pub(super) async fn remote_candidates_for(&self, mounting_store: StoreId, mount_ref: &MountRef) -> Vec<SourceRoute> {
        let mounting_link = {
            let manager = self.store_manager.read().await;
            manager.read_sync_config(mounting_store).await.ok().flatten()
        };
        let through_account = mounting_link.as_ref().is_some_and(|config| config.via_account || config.mode.is_vault_link());
        let mounting_url = mounting_link.as_ref().map(|config| config.remote.url.clone());

        let mut candidates: Vec<SourceRoute> = Vec::new();
        if through_account {
            candidates.push(SourceRoute::Account);
        }
        if let Some(url) = &mount_ref.source_remote {
            if !(through_account && mounting_url.as_ref() == Some(url)) {
                candidates.push(SourceRoute::Remote(RemoteEndpoint { url: url.clone(), auth: AuthMethod::None }));
            }
        }
        if let (false, Some(config)) = (through_account, &mounting_link) {
            if !candidates.iter().any(|c| matches!(c, SourceRoute::Remote(r) if r.url == config.remote.url)) {
                candidates.push(SourceRoute::Remote(Self::without_auth(&config.remote)));
            }
        }
        candidates
    }

    /// Create a replica of mount source `source` from the first of
    /// `candidates` that has it, then tell every mount of that source what
    /// happened (decision 5). Runs detached: whichever RPC triggered it has
    /// already answered `Connecting`.
    ///
    /// A candidate that cannot be reached, or that has no such store, is
    /// just the next one's turn; when none works the reason is kept so the
    /// notification says why, in the remote's own words ("... refused the
    /// credentials"). Nothing retries on a timer — the next resolution of
    /// the same mount tries again (decision 3's "the last creation attempt
    /// failed" is what a fan-out reports in the meantime).
    pub(super) async fn create_mount_source_replica(&self, source: StoreId, candidates: Vec<SourceRoute>) {
        let mut errors: Vec<String> = Vec::new();
        let mut created = false;

        for route in &candidates {
            let (made, from) = match route {
                SourceRoute::Account => (self.add_hosted_store(source).await.map(|_| ()), "the signed-in account".to_string()),
                SourceRoute::Remote(remote) => (self.create_replica_from(remote.clone(), source, None, false).await.map(|_| ()), remote.url.to_string()),
            };
            match made {
                Ok(()) => {
                    info!("Created a replica of mount source {} from {}", source, from);
                    created = true;
                    break;
                }
                Err(e) => {
                    let message = e.message().to_string();
                    debug!("Mount source {} not available from {}: {}", source, from, message);
                    errors.push(message);
                }
            }
        }

        {
            let mut mounts = self.mounts.lock().unwrap();
            mounts.in_flight.remove(&source);
            if !created {
                let reason = if errors.iter().all(|e| e.contains("has no open store")) {
                    format!("no remote has store {}", source)
                } else {
                    errors.join("; ")
                };
                warn!("Could not replicate mount source {}: {}", source, reason);
                mounts.failed.insert(source, reason);
            }
        }

        self.notify_mount_states_for_source(source).await;
    }
}
