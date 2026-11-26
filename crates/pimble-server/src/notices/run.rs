//! The work while an account is signed in: sending what waits in the
//! outbox, draining the inbox, and keeping the registered watches in step.

use std::collections::HashSet;
use std::time::Duration;

use pimble_core::{NodeId, StoreId};
use pimble_rpc::{Notice, StorePeople};
use tracing::{debug, info, warn};

use super::kept::{now_ms, scope_for, Held};
use super::{Notices, DRAIN_WAIT_SECS, RETRY_EVERY};
use crate::handler::RpcHandler;
use crate::keystore::SignedInAccount;

impl Notices {
    /// Send what waits: acknowledgements, then each notice in the outbox,
    /// addressed through the share its node is in and sealed to the
    /// recipient's notice key. One that cannot be addressed yet (the
    /// recipient has no notice key yet, the service cannot be reached)
    /// stays; one nobody can be told (the recipient left the share, the
    /// service refused it) goes.
    async fn flush(&self, handler: &RpcHandler, account: &SignedInAccount) {
        let me = account.user_id.as_str();
        let (acks, outbox) = self.with(handler, me, |kept| ((kept.acks.clone(), kept.outbox.clone()), false)).await;
        if !acks.is_empty() && crate::cloud::ack_inbox(handler.http(), &account.url, &account.session, &acks).await.is_ok() {
            self.with(handler, me, |kept| {
                kept.acks.retain(|a| !acks.contains(a));
                ((), true)
            })
            .await;
        }
        if outbox.is_empty() {
            return;
        }
        let mut done: Vec<String> = Vec::new();
        let mut refreshed: HashSet<StoreId> = HashSet::new();
        for out in outbox {
            let store_id = out.notice.store;
            let chain = chain_of(handler, store_id, out.notice.discussion.unwrap_or(out.notice.node)).await;
            let mut people = crate::people::cached(handler, store_id).await;
            let addressed = |people: &StorePeople| scope_for(people, &chain, &out.recipient).and_then(|s| s.people.iter().find(|p| p.user_id == out.recipient).and_then(|p| p.notice_key.clone()).map(|k| (s.clone(), k)));
            let mut target = addressed(&people);
            if target.is_none() && refreshed.insert(store_id) {
                people = crate::people::refresh(handler, store_id).await;
                target = addressed(&people);
            }
            let Some((scope, notice_key)) = target else {
                if scope_for(&people, &chain, &out.recipient).is_none() && refreshed.contains(&store_id) && !people.scopes.is_empty() {
                    debug!("Notices: {} cannot open that note; nobody is told", out.recipient);
                    done.push(out.notice.id.clone());
                }
                continue;
            };
            let mut notice = out.notice.clone();
            notice.from_name = if scope.my_name.trim().is_empty() { account.email.split('@').next().unwrap_or("someone").to_string() } else { scope.my_name.clone() };
            let Ok(plain) = serde_json::to_vec(&notice) else { continue };
            let sealed = match pimble_crypto::seal_notice(&plain, &notice_key, &out.recipient) {
                Ok(sealed) => sealed,
                Err(e) => {
                    warn!("Notices: could not seal a notice to {}: {}", out.recipient, e);
                    done.push(out.notice.id.clone());
                    continue;
                }
            };
            let root = scope.root.map(|r| r.to_string());
            match crate::cloud::post_notice(handler.http(), &account.url, &account.session, &out.recipient, &store_id.to_string(), root.as_deref(), &sealed, notice.kind.word()).await {
                Ok(_) => done.push(out.notice.id.clone()),
                Err(e) => match e.status() {
                    Some(400 | 403 | 404) => {
                        debug!("Notices: the service refused a notice to {}: {}", out.recipient, e);
                        done.push(out.notice.id.clone());
                    }
                    Some(401) => {
                        handler.keystore().mark_session_expired();
                        break;
                    }
                    _ => {
                        debug!("Notices: not sent yet: {}", e);
                        break;
                    }
                },
            }
        }
        if !done.is_empty() {
            self.with(handler, me, |kept| {
                kept.outbox.retain(|o| !done.contains(&o.notice.id));
                ((), true)
            })
            .await;
        }
    }

    /// Take what a drain brought: open each item new here, keep it and tell
    /// every window. An item that does not open, or whose notice names
    /// another sender than the one the service authenticated, is
    /// acknowledged away unread.
    async fn take(&self, handler: &RpcHandler, account: &SignedInAccount, key: &pimble_crypto::NoticeKey, items: Vec<crate::cloud::InboxItem>) {
        let me = account.user_id.as_str();
        let mut opened: Vec<(String, Notice)> = Vec::new();
        let mut refused: Vec<String> = Vec::new();
        for item in items {
            let notice = pimble_crypto::open_notice(&item.sealed, key, me).ok().and_then(|plain| serde_json::from_slice::<Notice>(&plain).ok());
            match notice {
                Some(notice) if notice.from == item.from => opened.push((item.id, notice)),
                _ => {
                    warn!("Notices: an item from {} did not open as a notice from them; dropped", item.from);
                    refused.push(item.id);
                }
            }
        }
        let now = now_ms();
        let fresh = self
            .with(handler, me, |kept| {
                let mut fresh = Vec::new();
                for (item, notice) in opened {
                    if kept.held.iter().any(|h| h.item == item || h.notice.id == notice.id) {
                        continue;
                    }
                    kept.held.push(Held { item, notice: notice.clone(), received_ms: now });
                    fresh.push(notice);
                }
                kept.acks.extend(refused.iter().cloned());
                let changed = !fresh.is_empty() || !refused.is_empty();
                (fresh, changed)
            })
            .await;
        for notice in fresh {
            info!("Notices: a {:?} from {} arrived", notice.kind, notice.from_name);
            let _ = self.received.send(notice);
        }
        if !refused.is_empty() {
            self.work.notify_one();
        }
    }
}

/// The node and its ancestors, nearest first (a discussion's host stands
/// for its parent): what says which shares a node is in.
pub(crate) async fn chain_of(handler: &RpcHandler, store_id: StoreId, node: NodeId) -> Vec<NodeId> {
    let manager = handler.store_manager_handle();
    let manager = manager.read().await;
    let mut chain = vec![node];
    let Ok(tree) = manager.tree(store_id) else { return chain };
    let mut at = node;
    while chain.len() < 1000 {
        let Some(fields) = tree.doc(at).and_then(|d| d.fields().ok()) else { break };
        let Some(up) = fields.parent_id.or(fields.attached_to) else { break };
        if chain.contains(&up) {
            break;
        }
        chain.push(up);
        at = up;
    }
    chain
}

/// This device holds the account's notice key: from the accounts service
/// when one of the account's devices made it, else made here and
/// published (docs/NOTIFICATIONS_CONTRACT.md section 2: an existing account
/// gets one from its next signed-in device).
pub(crate) async fn ensure_notice_key(handler: &RpcHandler, account: &SignedInAccount) -> Result<pimble_crypto::NoticeKey, String> {
    if let Some(key) = handler.keystore().notice_key().await {
        return Ok(key);
    }
    let fetch = || async {
        let keys = crate::cloud::me_keys(handler.http(), &account.url, &account.session).await.map_err(|e| e.to_string())?;
        match keys.notice_key {
            Some(view) => pimble_crypto::unwrap_notice_key(&view.envelope, &account.keys, &account.user_id, &view.public_key).map(Some).map_err(|e| format!("the account's notice key did not open: {e}")),
            None => Ok(None),
        }
    };
    let key = match fetch().await? {
        Some(key) => key,
        None => {
            let key = pimble_crypto::NoticeKey::generate();
            let envelope = pimble_crypto::wrap_notice_key(&key, &account.keys, &account.user_id).map_err(|e| e.to_string())?;
            if crate::cloud::put_notice_key(handler.http(), &account.url, &account.session, &key.public(), &envelope).await.map_err(|e| e.to_string())? {
                info!("Notices: made this account's notice key");
                key
            } else {
                // Another device of the account's made one a moment ago.
                fetch().await?.ok_or("the account's notice key went missing")?
            }
        }
    };
    handler.keystore().set_notice_key(&account.user_id, &key).await.map_err(|e| e.to_string())?;
    Ok(key)
}

/// The work while an account is signed in and notifications are on: the
/// outbox, and the inbox's long poll.
pub(super) async fn run(handler: RpcHandler) {
    let Some(account) = handler.keystore().account().await else { return };
    if handler.keystore().session_expired() {
        return;
    }
    let notices = handler.notices();
    let rt = handler.runtime().clone();
    // The switch is the account's: whatever another device set.
    if let Ok(me) = crate::cloud::me(handler.http(), &account.url, &account.session).await {
        notices
            .with(&handler, &account.user_id, |kept| {
                let changed = kept.on != me.notifications
                    || kept.email != me.notification_email
                    || kept.push != me.notification_push
                    || kept.push_key != me.push_key
                    || kept.watches_on != me.notification_watches;
                kept.on = me.notifications;
                kept.email = me.notification_email;
                kept.push = me.notification_push;
                kept.push_key = me.push_key.clone();
                kept.watches_on = me.notification_watches;
                if !(kept.on && kept.watches_on) {
                    kept.forget_watches();
                }
                ((), changed)
            })
            .await;
    }
    if !notices.with(&handler, &account.user_id, |kept| (kept.on, false)).await {
        return;
    }
    let mut backoff = Duration::from_secs(2);
    let key = loop {
        match ensure_notice_key(&handler, &account).await {
            Ok(key) => break key,
            Err(e) => {
                debug!("Notices: no notice key yet: {}", e);
                rt.sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(300));
            }
        }
    };
    handler.keystore().publish_notice_key().await;
    // The watches as the account keeps them, and the documents they reach
    // in the stores open here.
    if notices.watching(&handler, &account).await {
        notices.refresh_watches(&handler, &account).await;
        notices.register_all(&handler, &account).await;
    }

    let sending = async {
        loop {
            notices.flush(&handler, &account).await;
            let waiting = notices.with(&handler, &account.user_id, |kept| (!kept.outbox.is_empty() || !kept.acks.is_empty(), false)).await;
            let work = notices.work.notified();
            if waiting {
                tokio::select! {
                    _ = work => {}
                    _ = rt.sleep(RETRY_EVERY) => {}
                }
            } else {
                work.await;
            }
        }
    };
    let receiving = async {
        // From the start at every run (each item is taken once by id), then
        // from the cursor the service answers.
        let mut cursor = 0u64;
        let mut backoff = Duration::from_secs(1);
        loop {
            match crate::cloud::drain_inbox(handler.http(), &account.url, &account.session, cursor, DRAIN_WAIT_SECS).await {
                Ok(body) => {
                    backoff = Duration::from_secs(1);
                    cursor = body.cursor.max(cursor);
                    if !body.items.is_empty() {
                        notices.take(&handler, &account, &key, body.items).await;
                    }
                }
                Err(e) if e.status() == Some(401) => {
                    handler.keystore().mark_session_expired();
                    handler.cloud_changed();
                    return;
                }
                Err(e) => {
                    debug!("Notices: the inbox could not be drained: {}", e);
                    rt.sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    };
    let following = notices.follow_structure(&handler, &account);
    tokio::select! {
        _ = sending => {}
        _ = receiving => {}
        _ = following => {}
    }
}

