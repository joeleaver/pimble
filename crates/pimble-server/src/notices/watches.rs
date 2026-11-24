//! Watches, this device's side (docs/NOTIFICATIONS_CONTRACT.md section 3,
//! wave 3).
//!
//! **What the person chose** is a list of [`WatchEntry`]s, account data
//! kept by the accounts service sealed under the account's personal key
//! (`pimble_crypto::personal_key`, with `watches_aad`), so every device of
//! theirs reads and changes the same list and the service reads none of it.
//! A change is a read, the change, and a write naming the version read; a
//! device that lost the race reads again.
//!
//! **What is registered** is, per store, the documents those choices reach
//! ([`docs_for`]): each chosen node, and with its subtree every node under
//! it, as opaque ids the service matches writes against. A device holding
//! the store computes it again when the choices change and when nodes come
//! and go (a structural change in a store with watches, gathered for
//! [`FOLLOW_AFTER`]), and puts it only when it differs from what the service
//! was last told. Every put names the choices' version it was computed
//! from, so a device that has not seen another's change cannot register
//! from stale choices.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use base64::Engine;
use pimble_core::{NodeId, StoreId, StoreKind};
use pimble_crdt::Tree;
use pimble_rpc::{ListWatchesResponse, StoreChangeKind, WatchEntry};
use tracing::{debug, info};

use super::kept::now_ms;
use super::Notices;
use crate::handler::{LocalChange, RpcHandler};
use crate::keystore::SignedInAccount;

/// A structural change is followed this long after the first of a burst.
const FOLLOW_AFTER: Duration = Duration::from_secs(2);
/// How many times a change of the choices is tried against other devices'.
const ATTEMPTS: usize = 4;

/// Why a store's notes cannot be watched: nobody else can change them.
pub(crate) const NOT_SHARED_REFUSAL: &str = "Only notes other people can change are watched: this store is on this computer alone. Host it on Pimble Cloud or share it first.";
/// "Unwatch" on a note no watch reaches.
pub(crate) const NOT_WATCHED: &str = "This note is not watched.";
/// Watched notes are turned off for the account.
pub(crate) const WATCHES_OFF: &str = "Watched notes are turned off in Settings.";

/// The documents of a store that `entries` (that store's) reach: each
/// chosen node that is held and not deleted, and with its subtree every
/// node under it that is not deleted. A discussion is not a note and is
/// never watched (its writers are told as replies).
/// In the order of their ids, each once.
pub(crate) fn docs_for(tree: &Tree, entries: &[WatchEntry]) -> Vec<NodeId> {
    let mut docs = Vec::new();
    for entry in entries {
        let reached = if entry.subtree { tree.subtree_ids(entry.node_id).unwrap_or_default() } else { tree.live_fields(entry.node_id).map(|_| vec![entry.node_id]).unwrap_or_default() };
        docs.extend(reached.into_iter().filter(|id| !tree.is_attachment(*id)));
    }
    docs.sort_by_key(|id| *id.as_uuid());
    docs.dedup();
    docs
}

/// The entry `unwatch` removes for `chain` (the node and its ancestors,
/// nearest first): the watch on the node itself, or else the nearest watch
/// above it that takes its subtree.
pub(crate) fn covering(entries: &[WatchEntry], store_id: StoreId, chain: &[NodeId]) -> Option<usize> {
    let mine = |i: &usize| entries[*i].store_id == store_id;
    let first = chain.first()?;
    if let Some(i) = (0..entries.len()).filter(mine).find(|i| entries[*i].node_id == *first) {
        return Some(i);
    }
    chain.iter().skip(1).find_map(|up| (0..entries.len()).filter(mine).find(|i| entries[*i].node_id == *up && entries[*i].subtree))
}

/// Whether a change of this kind can change which documents a watch with
/// its subtree reaches.
fn structural(kind: &StoreChangeKind) -> bool {
    matches!(
        kind,
        StoreChangeKind::NodeCreated { .. }
            | StoreChangeKind::NodeDeleted { .. }
            | StoreChangeKind::NodeMoved { .. }
            | StoreChangeKind::TreeStructure { .. }
            | StoreChangeKind::SharesEnded { .. }
            | StoreChangeKind::SyncStateChanged { .. }
    )
}

fn seal(account: &SignedInAccount, entries: &[WatchEntry]) -> Result<String, String> {
    let plain = serde_json::to_vec(entries).map_err(|e| e.to_string())?;
    let key = pimble_crypto::personal_key(&account.keys);
    let sealed = pimble_crypto::Blob::encrypt(&key, uuid::Uuid::nil(), &pimble_crypto::watches_aad(&account.user_id), &plain);
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sealed))
}

fn open(account: &SignedInAccount, choices: &crate::cloud::WatchChoices) -> Result<Vec<WatchEntry>, String> {
    let Some(sealed) = choices.sealed.as_deref().filter(|s| !s.is_empty()) else { return Ok(Vec::new()) };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sealed).map_err(|e| format!("the watches are not base64: {e}"))?;
    let key = pimble_crypto::personal_key(&account.keys);
    let plain = pimble_crypto::Blob::decrypt(&key, &pimble_crypto::watches_aad(&account.user_id), &bytes).map_err(|e| format!("the watches could not be opened: {e}"))?;
    serde_json::from_slice(&plain).map_err(|e| format!("the watches could not be read: {e}"))
}

impl Notices {
    /// Whether notifications and watched notes are both on for `account`.
    pub(super) async fn watching(&self, handler: &RpcHandler, account: &SignedInAccount) -> bool {
        self.with(handler, &account.user_id, |kept| (kept.on && kept.watches_on, false)).await
    }

    /// The watches the account chose, as this device holds them.
    pub(crate) async fn list_watches(&self, handler: &RpcHandler) -> ListWatchesResponse {
        let Some(account) = handler.keystore().account().await else { return ListWatchesResponse::default() };
        self.with(handler, &account.user_id, |kept| (ListWatchesResponse { watches: kept.watches.clone(), on: kept.on && kept.watches_on }, false)).await
    }

    /// Watch a node (again, to change `subtree`).
    pub(crate) async fn watch(&self, handler: &RpcHandler, entry: WatchEntry) -> Result<ListWatchesResponse, String> {
        let account = handler.keystore().account().await.ok_or("Nobody is signed in.")?;
        if !self.watching(handler, &account).await {
            return Err(WATCHES_OFF.into());
        }
        if handler.sync_mode_of(entry.store_id).await != StoreKind::Vault {
            return Err(NOT_SHARED_REFUSAL.into());
        }
        {
            let manager = handler.store_manager_handle();
            let manager = manager.read().await;
            let tree = manager.tree(entry.store_id).map_err(|e| e.to_string())?;
            if tree.live_fields(entry.node_id).is_err() || tree.is_attachment(entry.node_id) {
                return Err("There is no such note here.".into());
            }
        }
        self.change_choices(handler, &account, |list| {
            list.retain(|e| !(e.store_id == entry.store_id && e.node_id == entry.node_id));
            list.push(entry);
            true
        })
        .await?;
        info!("Watching a note{} in store {}", if entry.subtree { " and what is under it" } else { "" }, entry.store_id);
        self.register(handler, &account, &[entry.store_id]).await;
        Ok(self.list_watches(handler).await)
    }

    /// Stop watching `node_id`: the watch on it, or the nearest one above it
    /// that takes its subtree.
    pub(crate) async fn unwatch(&self, handler: &RpcHandler, store_id: StoreId, node_id: NodeId) -> Result<ListWatchesResponse, String> {
        let account = handler.keystore().account().await.ok_or("Nobody is signed in.")?;
        let chain = super::run::chain_of(handler, store_id, node_id).await;
        let found = self
            .change_choices(handler, &account, |list| match covering(list, store_id, &chain) {
                Some(i) => {
                    list.remove(i);
                    true
                }
                None => false,
            })
            .await?;
        if !found {
            return Err(NOT_WATCHED.into());
        }
        self.register(handler, &account, &[store_id]).await;
        Ok(self.list_watches(handler).await)
    }

    /// The switch for watched notes alone: the service is told first (off
    /// deletes every watch and what was gathered for them), then this
    /// device forgets them too.
    pub(crate) async fn set_watches_on(&self, handler: &RpcHandler, on: bool) -> Result<(), String> {
        let account = handler.keystore().account().await.ok_or("Nobody is signed in.")?;
        let switches = crate::cloud::Switches { watches: Some(on), ..Default::default() };
        crate::cloud::put_notifications(handler.http(), &account.url, &account.session, &switches).await.map_err(|e| e.to_string())?;
        self.with(handler, &account.user_id, |kept| {
            kept.watches_on = on;
            if !on {
                kept.forget_watches();
            }
            ((), true)
        })
        .await;
        info!("Watched notes turned {}", if on { "on" } else { "off" });
        Ok(())
    }

    /// Read the choices, change them with `f`, and write them back if they
    /// are still at the version read; again when another device wrote in
    /// between. Answers what `f` answered (false: nothing to change, nothing
    /// written).
    async fn change_choices(&self, handler: &RpcHandler, account: &SignedInAccount, f: impl Fn(&mut Vec<WatchEntry>) -> bool) -> Result<bool, String> {
        for _ in 0..ATTEMPTS {
            let body = crate::cloud::get_watches(handler.http(), &account.url, &account.session).await.map_err(|e| e.to_string())?;
            let mut list = open(account, &body.choices)?;
            if !f(&mut list) {
                self.hold(handler, account, list, body.choices.version, Some(body.stores)).await;
                return Ok(false);
            }
            let sealed = seal(account, &list)?;
            match crate::cloud::put_watch_choices(handler.http(), &account.url, &account.session, body.choices.version, &sealed).await.map_err(|e| e.to_string())? {
                Some(version) => {
                    self.hold(handler, account, list, version, Some(body.stores)).await;
                    return Ok(true);
                }
                None => debug!("Watches: another device changed them first; reading them again"),
            }
        }
        Err("Your other devices kept changing your watches at the same moment; try again.".into())
    }

    /// Keep `list` at `version` as the account's watches, and what the
    /// service said is registered.
    async fn hold(&self, handler: &RpcHandler, account: &SignedInAccount, list: Vec<WatchEntry>, version: u64, stores: Option<Vec<crate::cloud::WatchedStore>>) {
        self.with(handler, &account.user_id, |kept| {
            kept.watches = list;
            kept.watches_version = version;
            if let Some(stores) = stores {
                kept.registered = stores.into_iter().map(|s| (s.store_id, s.docs.iter().filter_map(|d| NodeId::parse(d).ok()).collect())).collect();
            }
            ((), true)
        })
        .await;
    }

    /// Read the account's watches from the service.
    pub(super) async fn refresh_watches(&self, handler: &RpcHandler, account: &SignedInAccount) {
        match crate::cloud::get_watches(handler.http(), &account.url, &account.session).await {
            Ok(body) => match open(account, &body.choices) {
                Ok(list) => self.hold(handler, account, list, body.choices.version, Some(body.stores)).await,
                Err(e) => debug!("Watches: {}", e),
            },
            Err(e) if e.status() == Some(401) => handler.keystore().mark_session_expired(),
            Err(e) => debug!("Watches: could not be read: {}", e),
        }
    }

    /// Register every store with watches (or with documents registered)
    /// that is open here.
    pub(super) async fn register_all(&self, handler: &RpcHandler, account: &SignedInAccount) {
        let stores = self
            .with(handler, &account.user_id, |kept| {
                let mut stores: BTreeSet<StoreId> = kept.watches.iter().map(|e| e.store_id).collect();
                stores.extend(kept.registered.keys().filter_map(|s| StoreId::parse(s).ok()));
                (stores.into_iter().collect::<Vec<_>>(), false)
            })
            .await;
        self.register(handler, account, &stores).await;
    }

    /// Put the documents the watches reach in each of `stores` that is open
    /// here, where they differ from what the service was last told.
    pub(super) async fn register(&self, handler: &RpcHandler, account: &SignedInAccount, stores: &[StoreId]) {
        for &store_id in stores {
            for _ in 0..ATTEMPTS {
                let (entries, version, registered) = self
                    .with(handler, &account.user_id, |kept| {
                        let entries: Vec<WatchEntry> = kept.watches.iter().filter(|e| e.store_id == store_id).copied().collect();
                        ((entries, kept.watches_version, kept.registered.get(&store_id.to_string()).cloned()), false)
                    })
                    .await;
                let docs: Option<Vec<NodeId>> = {
                    let manager = handler.store_manager_handle();
                    let manager = manager.read().await;
                    manager.tree(store_id).ok().map(|tree| docs_for(tree, &entries))
                };
                // Not open here: whichever device holds it registers it.
                let Some(docs) = docs else { break };
                let unchanged = match &registered {
                    Some(held) => *held == docs,
                    None => docs.is_empty(),
                };
                if unchanged {
                    break;
                }
                let ids: Vec<String> = docs.iter().map(|d| d.to_string()).collect();
                match crate::cloud::put_watched_docs(handler.http(), &account.url, &account.session, &store_id.to_string(), &ids, version).await {
                    Ok(true) => {
                        debug!("Watches: {} documents registered in store {}", docs.len(), store_id);
                        self.with(handler, &account.user_id, |kept| {
                            if docs.is_empty() {
                                kept.registered.remove(&store_id.to_string());
                            } else {
                                kept.registered.insert(store_id.to_string(), docs);
                            }
                            ((), true)
                        })
                        .await;
                        break;
                    }
                    // Another device changed the choices: read them, again.
                    Ok(false) => self.refresh_watches(handler, account).await,
                    Err(e) => {
                        if e.status() == Some(401) {
                            handler.keystore().mark_session_expired();
                        }
                        debug!("Watches: store {} not registered: {}", store_id, e);
                        break;
                    }
                }
            }
        }
    }

    /// Nodes come and go: each store with watches whose tree changed is
    /// registered again, a moment after the first change of a burst. Runs
    /// for as long as the account's work does.
    pub(super) async fn follow_structure(&self, handler: &RpcHandler, account: &SignedInAccount) {
        let rt = handler.runtime().clone();
        let mut changes = handler.subscribe_local_changes().await;
        let mut due: HashSet<StoreId> = HashSet::new();
        let mut since = 0i64;
        loop {
            let wait = Duration::from_millis((since + FOLLOW_AFTER.as_millis() as i64 - now_ms()).max(0) as u64);
            tokio::select! {
                change = changes.recv() => match change {
                    Ok(LocalChange::Store(n)) if structural(&n.change_kind) => {
                        if due.is_empty() {
                            since = now_ms();
                        }
                        due.insert(n.store_id);
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if due.is_empty() {
                            since = now_ms();
                        }
                        due.extend(self.with(handler, &account.user_id, |kept| (kept.watches.iter().map(|e| e.store_id).collect::<Vec<_>>(), false)).await);
                    }
                    // The server is stopping: the account's work ends with it.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                },
                _ = rt.sleep(wait), if !due.is_empty() => {
                    let watched: HashSet<StoreId> = self
                        .with(handler, &account.user_id, |kept| {
                            let mut s: HashSet<StoreId> = kept.watches.iter().map(|e| e.store_id).collect();
                            s.extend(kept.registered.keys().filter_map(|k| StoreId::parse(k).ok()));
                            (s, false)
                        })
                        .await;
                    let stores: Vec<StoreId> = due.drain().filter(|s| watched.contains(s)).collect();
                    if !stores.is_empty() && self.watching(handler, account).await {
                        self.register(handler, account, &stores).await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(store_id: StoreId, node_id: NodeId, subtree: bool) -> WatchEntry {
        WatchEntry { store_id, node_id, subtree }
    }

    #[test]
    fn a_watch_reaches_its_node_and_with_its_subtree_what_is_under_it_but_no_discussion_or_deleted_node() {
        let store = StoreId::new();
        let now = "2026-11-23T00:00:00Z";
        let root = NodeId::new();
        let mut tree = Tree::new(root, "Store", now).unwrap();
        let (folder, a, b, gone) = (NodeId::new(), NodeId::new(), NodeId::new(), NodeId::new());
        tree.add_node(folder, Some(root), pimble_core::Place::Last, "text", "Folder", now).unwrap();
        tree.add_node(a, Some(folder), pimble_core::Place::Last, "text", "A", now).unwrap();
        tree.add_node(b, Some(a), pimble_core::Place::Last, "text", "B", now).unwrap();
        tree.add_node(gone, Some(folder), pimble_core::Place::Last, "text", "Gone", now).unwrap();
        tree.remove_node(gone, now).unwrap();
        let talk = NodeId::new();
        tree.add_attachment(talk, a, pimble_core::node_types::DISCUSSION, "Talk", None, now).unwrap();

        assert_eq!(docs_for(&tree, &[entry(store, folder, false)]), vec![folder]);
        let mut all = vec![folder, a, b];
        all.sort_by_key(|id| *id.as_uuid());
        assert_eq!(docs_for(&tree, &[entry(store, folder, true), entry(store, a, false)]), all, "each once; a deleted node and a discussion are not watched");
        assert!(docs_for(&tree, &[entry(store, gone, false)]).is_empty());
    }

    #[test]
    fn unwatching_takes_the_nodes_own_watch_or_the_nearest_subtree_watch_above_it() {
        let (store, other) = (StoreId::new(), StoreId::new());
        let (note, folder, top) = (NodeId::new(), NodeId::new(), NodeId::new());
        let chain = [note, folder, top];
        let list = vec![entry(store, top, true), entry(store, folder, true), entry(other, note, false)];
        assert_eq!(covering(&list, store, &chain), Some(1), "the nearest above, not the top");
        let list = vec![entry(store, folder, false), entry(store, top, true)];
        assert_eq!(covering(&list, store, &chain), Some(1), "a watch above without its subtree does not reach the note");
        let list = vec![entry(store, top, true), entry(store, note, false)];
        assert_eq!(covering(&list, store, &chain), Some(1), "the note's own watch first");
        assert_eq!(covering(&[entry(other, note, false)], store, &chain), None, "another store's watch is not this one");
    }
}
