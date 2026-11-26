//! Who is who in a store's shares (docs/PRESENCE_CONTRACT.md section 3).
//!
//! Every member of a share goes by a name of their own choosing there, and
//! nobody in a share sees anybody else's email. Documents, discussion
//! messages and (later) presence frames name people by account id; this
//! module is how this device turns an id into the name it goes by. The
//! accounts service answers it (`GET /me/shares`, then `GET members` per
//! scope), and the answer is kept in the store's state directory as
//! `people.json`, derived data rebuilt whenever it is asked again, so names
//! still show offline. Nothing here is store content: the file is never
//! synced, and deleting it loses nothing. It is the signed-in account's view
//! (its own name in each share), so it is this computer's and not the
//! folder's (docs/STORE_FOLDERS_CONTRACT.md rule 1, decided 2026-11-19).

use std::time::Duration;

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{Person, ScopePeople, StorePeople};
use tokio::sync::oneshot;
use tracing::{debug, warn};

use crate::handler::RpcHandler;
use crate::keystore::SignedInAccount;

const FILE_NAME: &str = "people.json";

/// How long a change to a share waits to see itself in what is kept. The
/// asking goes on after that.
const CHANGE_WAIT: Duration = Duration::from_secs(3);

async fn file_of(handler: &RpcHandler, store_id: StoreId) -> Option<std::path::PathBuf> {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    manager
        .state_dir(store_id)
        .map(|dir| dir.join(FILE_NAME))
        .or_else(|| manager.get_store_info(store_id).ok().and_then(|store| store.local_path().map(|p| p.join(FILE_NAME))))
}

/// What this device last learned, empty when it never has or cannot read it.
pub(crate) async fn cached(handler: &RpcHandler, store_id: StoreId) -> StorePeople {
    kept(handler, store_id).await.ok().flatten().unwrap_or_default()
}

/// What this device last learned: `None` when it never has, an error when
/// what it kept cannot be read.
pub(crate) async fn kept(handler: &RpcHandler, store_id: StoreId) -> Result<Option<StorePeople>, String> {
    let Some(path) = file_of(handler, store_id).await else { return Ok(None) };
    let failed = |e: &dyn std::fmt::Display| {
        warn!("People of store {}: could not read {}: {}", store_id, path.display(), e);
        format!("Who is in this store's shares could not be read on this computer: {e}")
    };
    match handler.disk().read_to_string(&path).await {
        Ok(json) => serde_json::from_str(&json).map(Some).map_err(|e| failed(&e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(failed(&e)),
    }
}

/// Ask the accounts service now. `None` when it cannot be asked (nobody
/// signed in, no network) or the account holds nothing of the store, whose
/// empty answer would read as "nobody".
pub(crate) async fn fetch(http: &dyn pimble_host::http::Http, account: &SignedInAccount, store_id: StoreId) -> Option<StorePeople> {
    let id = store_id.to_string();
    let rows = match crate::cloud::list_stores(http, &account.url, &account.session).await {
        Ok(rows) => rows,
        Err(e) => {
            debug!("People of store {}: could not list the account's stores: {}", store_id, e);
            return None;
        }
    };
    let Some(row) = rows.iter().find(|row| row.store_id == id) else {
        debug!("People of store {}: the signed-in account does not hold it", store_id);
        return None;
    };
    let shares = match crate::cloud::list_shares(http, &account.url, &account.session).await {
        Ok(shares) => shares,
        Err(e) => {
            debug!("People of store {}: could not list the account's shares: {}", store_id, e);
            return None;
        }
    };
    let mut scopes = Vec::new();
    for share in shares.iter().filter(|share| share.store_id == id) {
        let root: Option<NodeId> = share.scope_root();
        let listing = match crate::cloud::list_members(http, &account.url, &account.session, &id, root.as_ref()).await {
            Ok(listing) => listing,
            Err(e) => {
                debug!("People of store {}: could not list scope {:?}: {}", store_id, root, e);
                return None;
            }
        };
        scopes.push(ScopePeople {
            root,
            people: persons(listing.people),
            my_name: share.appears_as.clone(),
            my_name_chosen: !share.my_name.trim().is_empty(),
            share_name: share.name.clone(),
        });
    }
    // An owner's list of shares leaves out the store's other owners, so who
    // holds the whole of it is asked for itself.
    if crate::cloud::is_owner_of(&rows, store_id) && !scopes.iter().any(|scope| scope.root.is_none()) {
        let listing = match crate::cloud::list_members(http, &account.url, &account.session, &id, None).await {
            Ok(listing) => listing,
            Err(e) => {
                debug!("People of store {}: could not list who holds it: {}", store_id, e);
                return None;
            }
        };
        if listing.people.iter().any(|person| person.user_id != account.user_id) {
            let my_name = listing.people.iter().find(|person| person.user_id == account.user_id).map(|person| person.name.clone()).unwrap_or_default();
            // Not a share anyone is asked to choose a name for.
            scopes.push(ScopePeople { root: None, people: persons(listing.people), my_name, my_name_chosen: true, share_name: row.name.clone() });
        }
    }
    Some(StorePeople { scopes, alone: Vec::new() })
}

fn persons(listed: Vec<crate::cloud::PersonView>) -> Vec<Person> {
    listed.into_iter().map(|person| Person { user_id: person.user_id, name: person.name, notice_key: person.notice_key }).collect()
}

/// [`refresh`] in a task of its own; the receiver answers when it is done.
pub(crate) fn refresh_in_background(handler: &RpcHandler, store_id: StoreId) -> oneshot::Receiver<()> {
    let (done, finished) = oneshot::channel();
    let handler = handler.clone();
    let runtime = handler.runtime().clone();
    runtime.spawn(async move {
        refresh(&handler, store_id).await;
        let _ = done.send(());
    });
    finished
}

/// Ask again after this device has asked the accounts service to change who
/// is in a share, so that the next question is answered with the change,
/// waiting [`CHANGE_WAIT`] at most.
pub(crate) async fn refresh_after_change(handler: &RpcHandler, store_id: StoreId) {
    let finished = refresh_in_background(handler, store_id);
    let _ = handler.runtime().timeout(CHANGE_WAIT, finished).await;
}

/// Ask again and, when the answer differs from what is kept, keep it and
/// tell this server's subscribers (`StoreChangeKind::PeopleChanged`).
/// Best effort: what is kept stays when the service cannot be asked.
pub(crate) async fn refresh(handler: &RpcHandler, store_id: StoreId) -> StorePeople {
    // `None` when nothing readable is kept: an empty answer is then written
    // too, since "nobody" and "never told" are not the same.
    let on_disk = kept(handler, store_id).await.ok().flatten();
    let held = on_disk.clone().unwrap_or_default();
    let Some(account) = handler.keystore().account().await else { return held };
    let Some(mut fresh) = fetch(handler.http(), &account, store_id).await else { return held };
    fresh.alone = shares_with_nobody_else(handler, &account, store_id, &fresh).await;
    if on_disk.as_ref() != Some(&fresh) {
        if let Some(path) = file_of(handler, store_id).await {
            match serde_json::to_vec_pretty(&fresh) {
                Ok(bytes) => {
                    if let Err(e) = handler.disk().write_private_atomic(&path, &bytes).await {
                        warn!("People of store {}: could not write {}: {}", store_id, FILE_NAME, e);
                    }
                }
                Err(e) => warn!("People of store {}: {}", store_id, e),
            }
        }
    }
    if fresh != held {
        handler.notify_people_changed(store_id).await;
    }
    fresh
}

/// The shares marked in this store that `fresh` does not name and that
/// nobody but this account is in: the account's list of shares leaves out one
/// of its own that nobody else holds or is invited to. Asked on the owner's
/// devices only (a member's replica has scope roots).
async fn shares_with_nobody_else(handler: &RpcHandler, account: &SignedInAccount, store_id: StoreId, fresh: &StorePeople) -> Vec<NodeId> {
    let unlisted: Vec<NodeId> = {
        let manager = handler.store_manager_handle();
        let manager = manager.read().await;
        if !manager.scope_roots(store_id).is_empty() {
            return Vec::new();
        }
        let Ok(tree) = manager.tree(store_id) else { return Vec::new() };
        crate::share::markers_in(tree).into_iter().map(|(root, _)| root).filter(|root| !fresh.scopes.iter().any(|scope| scope.root == Some(*root))).collect()
    };
    let mut alone = Vec::new();
    for root in unlisted {
        match crate::cloud::list_members(handler.http(), &account.url, &account.session, &store_id.to_string(), Some(&root)).await {
            // `members` is the share's own grants and invitations.
            Ok(listing) if listing.members.iter().all(|member| member.user_id.as_deref() == Some(account.user_id.as_str())) => alone.push(root),
            Ok(_) => {}
            Err(e) => debug!("People of store {}: could not list scope {}: {}", store_id, root, e),
        }
    }
    alone.sort_by_key(|root| root.to_string());
    alone
}
