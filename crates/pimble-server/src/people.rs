//! Who is who in a store's shares (docs/PRESENCE_CONTRACT.md section 3).
//!
//! Every member of a share goes by a name of their own choosing there, and
//! nobody in a share sees anybody else's email. Documents, discussion
//! messages and (later) presence frames name people by account id; this
//! module is how this device turns an id into the name it goes by. The
//! accounts service answers it (`GET /me/shares`, then `GET members` per
//! scope), and the answer is kept beside the store as `people.json`, derived
//! data rebuilt whenever it is asked again, so names still show offline.
//! Nothing here is store content: the file is never synced, and deleting it
//! loses nothing.

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{Person, ScopePeople, StorePeople};
use tracing::{debug, warn};

use crate::handler::RpcHandler;
use crate::keystore::SignedInAccount;

const FILE_NAME: &str = "people.json";

async fn file_of(handler: &RpcHandler, store_id: StoreId) -> Option<std::path::PathBuf> {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    manager.get_store_info(store_id).ok().and_then(|store| store.local_path().map(|p| p.join(FILE_NAME)))
}

/// What this device last learned, empty when it never has.
pub(crate) async fn cached(handler: &RpcHandler, store_id: StoreId) -> StorePeople {
    let Some(path) = file_of(handler, store_id).await else { return StorePeople::default() };
    match tokio::fs::read_to_string(&path).await {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => StorePeople::default(),
    }
}

/// Ask the accounts service now. `None` when it cannot be asked (nobody
/// signed in, no network, a store no row names).
pub(crate) async fn fetch(account: &SignedInAccount, store_id: StoreId) -> Option<StorePeople> {
    let shares = match crate::cloud::list_shares(&account.url, &account.session).await {
        Ok(shares) => shares,
        Err(e) => {
            debug!("People of store {}: could not list the account's shares: {}", store_id, e);
            return None;
        }
    };
    let id = store_id.to_string();
    let mut scopes = Vec::new();
    for share in shares.iter().filter(|share| share.store_id == id) {
        let root: Option<NodeId> = share.scope_root();
        let listing = match crate::cloud::list_members(&account.url, &account.session, &id, root.as_ref()).await {
            Ok(listing) => listing,
            Err(e) => {
                debug!("People of store {}: could not list scope {:?}: {}", store_id, root, e);
                return None;
            }
        };
        scopes.push(ScopePeople {
            root,
            people: listing.people.into_iter().map(|person| Person { user_id: person.user_id, name: person.name }).collect(),
            my_name: share.appears_as.clone(),
            my_name_chosen: !share.my_name.trim().is_empty(),
            share_name: share.name.clone(),
        });
    }
    Some(StorePeople { scopes })
}

/// Ask again and, when the answer differs from what is kept, keep it and
/// tell this server's subscribers (`StoreChangeKind::PeopleChanged`).
/// Best effort: what is kept stays when the service cannot be asked.
pub(crate) async fn refresh(handler: &RpcHandler, store_id: StoreId) -> StorePeople {
    let kept = cached(handler, store_id).await;
    let Some(account) = handler.keystore().account().await else { return kept };
    let Some(fresh) = fetch(&account, store_id).await else { return kept };
    if fresh != kept {
        if let Some(path) = file_of(handler, store_id).await {
            match serde_json::to_vec_pretty(&fresh) {
                Ok(bytes) => {
                    if let Err(e) = crate::fs_util::write_atomic_0600(&path, &bytes) {
                        warn!("People of store {}: could not write {}: {}", store_id, FILE_NAME, e);
                    }
                }
                Err(e) => warn!("People of store {}: {}", store_id, e),
            }
        }
        handler.notify_people_changed(store_id).await;
    }
    fresh
}
