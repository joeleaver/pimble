//! How the signed-in Pimble Cloud account holds a store this device keeps a
//! replica of, for every link that connects with the account's token: a
//! vault link to an encrypted store (`crate::vault_link`) and a plain link to
//! a plain store hosted on Pimble Cloud (`crate::sync_link`, `sync.json`'s
//! `via_account`; docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 1").
//!
//! Read again from the accounts service at every connect ([`refresh`]) and
//! asked about every [`GRANT_CHECK_EVERY`] while connected ([`grant_shape`]):
//! a role the owner changed, another share of the store or a removal ends
//! the connection, and the next one is made with a token minted from the
//! grant as it is now ([`mint`]). A scope root the account's rows no longer
//! name is recorded as ended in the manifest and announced once
//! ([`record_ended_roots`]). What a link pushes follows what the account may
//! write ([`may_push`]).

use std::time::Duration;

use pimble_core::{NodeId, StoreAccess, StoreId};
use tracing::{debug, info, warn};

use crate::handler::RpcHandler;
use crate::keystore::SignedInAccount;

/// How often a connected link asks the accounts service whether the
/// account's grant on the store is still what this connection was made
/// with (see [`grant_shape`]), unless `ServerConfig::grant_check_interval`
/// says otherwise.
pub(crate) const GRANT_CHECK_EVERY: Duration = Duration::from_secs(120);

/// What [`refresh`] read: whether the account owns the store, and the
/// account it read it as.
pub(crate) struct Held {
    pub owner: bool,
    pub account: SignedInAccount,
}

/// Pimble Cloud refused the account's session: the link waits for a sign-in
/// rather than asking again every 30 seconds (2026-11-19: 58 refusals in a
/// quarter of an hour, and the status bar still said "Signed in").
#[derive(Debug)]
pub(crate) struct SessionExpired;

impl std::fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the Pimble Cloud sign-in expired; nothing is tried again until someone signs in")
    }
}

impl std::error::Error for SessionExpired {}

/// A token for the signed-in account, minted now (`POST /api/v1/token`),
/// and the account it is for. Fails with no account signed in, with
/// [`SessionExpired`] when the session is known to have expired or the
/// service refuses it now (marked in the keystore and said once), and with
/// the service's error otherwise.
pub(crate) async fn mint(handler: &RpcHandler) -> anyhow::Result<(SignedInAccount, crate::cloud::TokenResponse)> {
    let account = handler.keystore().account().await.ok_or_else(|| anyhow::anyhow!("no Pimble Cloud account is signed in"))?;
    if handler.keystore().session_expired() {
        return Err(anyhow::Error::new(SessionExpired));
    }
    match crate::cloud::mint_token(handler.http(), &account.url, &account.session).await {
        Ok(minted) => Ok((account, minted)),
        Err(e) if e.status() == Some(401) => {
            if !handler.keystore().session_expired() {
                handler.keystore().mark_session_expired();
                warn!("Pimble Cloud at {} refused this computer's sign-in (it expired, or was signed out elsewhere): sign in again to sync", account.url);
                handler.cloud_changed();
            }
            Err(anyhow::Error::new(SessionExpired))
        }
        Err(e) => Err(anyhow::anyhow!("minting a cloud token from {} failed: {}", account.url, e)),
    }
}

/// Whether this replica sends changes to `node_id` at all: held as an editor
/// or the owner, or as a commenter for a discussion. A reader's replica never
/// pushes (the local server refuses a reader's writes; what changes here is
/// its own upkeep).
pub(crate) async fn may_push(handler: &RpcHandler, store_id: StoreId, node_id: NodeId) -> bool {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let access = manager.store_access(store_id);
    access.allows_write() || (access.allows_comment() && manager.is_discussion(store_id, node_id))
}

/// How the account holds the store, as far as the accounts service's rows
/// say: what a token minted now would carry. Compared between a connect and
/// now ([`GRANT_CHECK_EVERY`]); never stored.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct GrantShape {
    /// `None`: no row names the store.
    held: Option<(pimble_core::StoreAccess, std::collections::BTreeSet<String>, std::collections::BTreeSet<String>)>,
}

/// `None` when it cannot be read (no account, no network): nothing is
/// concluded from that, and the connection stands.
pub(crate) async fn grant_shape(handler: &RpcHandler, store_id: StoreId) -> Option<GrantShape> {
    let account = handler.keystore().account().await?;
    let rows = crate::cloud::list_stores(handler.http(), &account.url, &account.session).await.ok()?;
    let ids = |roots: &[NodeId]| roots.iter().map(|id| id.to_string()).collect::<std::collections::BTreeSet<_>>();
    Some(GrantShape {
        held: crate::cloud::HeldAs::from_rows(&rows, store_id).map(|held_as| (held_as.access, ids(&held_as.roots), ids(&held_as.read_only_roots))),
    })
}

/// Read again, from the accounts service, how the signed-in account holds
/// this store, and bring `sync.json` (`access`, `shared_by`, the roots only
/// read), a partial replica's scope roots and the roots among them that have
/// ended in line with it: a role changed by the owner, another share of the
/// same store, a removal from one. Best effort: the token the connect mints
/// next is what the hosted server judges. `who` names the link in the log.
///
/// `None` when the rows cannot be read (no account, no network).
pub(crate) async fn refresh(handler: &RpcHandler, store_id: StoreId, who: &str) -> Option<Held> {
    let account = handler.keystore().account().await?;
    let rows = match crate::cloud::list_stores(handler.http(), &account.url, &account.session).await {
        Ok(rows) => rows,
        Err(e) => {
            debug!("{} for store {}: could not list the account's stores: {}", who, store_id, e);
            return None;
        }
    };
    let owner = crate::cloud::is_owner_of(&rows, store_id);
    let manager = handler.store_manager_handle();
    let local_roots = manager.read().await.scope_roots(store_id);
    let Some(mut held_as) = crate::cloud::HeldAs::from_rows(&rows, store_id) else {
        // No row at all. For a share's replica that is a removal from every
        // share of the store: the hosted server takes nothing from this
        // account any more, so nothing is taken here either (an edit
        // accepted here would sit on this device for good, looking saved).
        if !local_roots.is_empty() {
            let manager = manager.read().await;
            if let Ok(Some(mut config)) = manager.read_sync_config(store_id).await {
                if config.access != StoreAccess::Read {
                    info!("{} for store {}: the account holds no share of it any more; read only here now", who, store_id);
                    config.access = StoreAccess::Read;
                    if let Err(e) = manager.write_sync_config(store_id, &config).await {
                        warn!("{} for store {}: could not record how the store is held: {}", who, store_id, e);
                    }
                }
            }
            drop(manager);
            // Every share of it has ended. The documents stay where they
            // are until the replica is removed; the app stops showing them.
            record_ended_roots(handler, store_id, local_roots.clone(), who).await;
        }
        return Some(Held { owner, account });
    };
    // A root this replica holds and the account does not any more (removed
    // from that share, or its owner stopped sharing it) stays here as it
    // last was and takes no edits, for the same reason.
    for root in local_roots.iter().filter(|root| !held_as.roots.contains(root)) {
        if !held_as.read_only_roots.contains(root) {
            held_as.read_only_roots.push(*root);
        }
    }
    // Only a partial replica takes roots: a whole replica holds every
    // document already, whatever the account's grant has become since.
    if !local_roots.is_empty() {
        let mut added = false;
        for root in held_as.roots.iter().filter(|root| !local_roots.contains(root)) {
            match manager.write().await.add_scope_root(store_id, *root).await {
                Ok(()) => added = true,
                Err(e) => warn!("{} for store {}: could not add scope root {}: {}", who, store_id, root, e),
            }
        }
        // One share's replica carries that share's name; with another it is
        // "Shared by ...", as a replica added with both would be.
        if let (true, Some(name)) = (added, &held_as.name) {
            if let Err(e) = manager.write().await.set_partial_replica_name(store_id, name).await {
                warn!("{} for store {}: could not rename the replica: {}", who, store_id, e);
            }
        }
    }
    {
        let manager = manager.read().await;
        if let Ok(Some(mut config)) = manager.read_sync_config(store_id).await {
            let read_only_roots = if local_roots.is_empty() { Vec::new() } else { held_as.read_only_roots.clone() };
            let comment_roots = if local_roots.is_empty() { Vec::new() } else { held_as.comment_roots.clone() };
            if config.access != held_as.access || config.shared_by != held_as.shared_by || config.read_only_roots != read_only_roots || config.comment_roots != comment_roots {
                info!("{} for store {}: held as {:?} now (shared by {:?})", who, store_id, held_as.access, held_as.shared_by);
                config.access = held_as.access;
                config.shared_by = held_as.shared_by.clone();
                config.read_only_roots = read_only_roots;
                config.comment_roots = comment_roots;
                if let Err(e) = manager.write_sync_config(store_id, &config).await {
                    warn!("{} for store {}: could not record how the store is held: {}", who, store_id, e);
                }
            }
        }
    }
    // The shares that ended: exactly the roots held here that the account's
    // rows no longer name, so one granted again is un-ended by the same
    // comparison. After `sync.json`, so that whoever hears of it and asks
    // how the store is held reads the ended root as read only already. A
    // whole-store grant names no root and leaves none out: nothing of the
    // store has stopped being this account's.
    if !local_roots.is_empty() {
        let ended = if held_as.roots.is_empty() { Vec::new() } else { local_roots.iter().filter(|root| !held_as.roots.contains(root)).copied().collect() };
        record_ended_roots(handler, store_id, ended, who).await;
    }
    Some(Held { owner, account })
}

/// Record in a partial replica's manifest which of its scope roots the
/// account no longer holds (`StoreManifest::ended_roots`; the scope roots
/// themselves are never shrunk, and nothing else on disk is touched), and
/// when that changed anything, tell the store's subscribers which roots
/// ended just now (`StoreChangeKind::SharesEnded`): the app stops showing
/// them and says so. A set that did not change, which is every connect
/// after the one that learned of it, says nothing.
pub(crate) async fn record_ended_roots(handler: &RpcHandler, store_id: StoreId, ended: Vec<NodeId>, who: &str) {
    let manager = handler.store_manager_handle();
    let (before, changed) = {
        let mut manager = manager.write().await;
        let before = manager.ended_roots(store_id);
        (before, manager.set_ended_roots(store_id, ended.clone()).await)
    };
    match changed {
        Ok(false) => {}
        Ok(true) => {
            let just_now: Vec<NodeId> = ended.iter().filter(|root| !before.contains(root)).copied().collect();
            info!("{} for store {}: {} share(s) of it ended just now; {} of the shares held here have ended in all", who, store_id, just_now.len(), ended.len());
            handler.notify_shares_ended(store_id, just_now).await;
        }
        Err(e) => warn!("{} for store {}: could not record which shares have ended: {}", who, store_id, e),
    }
}
