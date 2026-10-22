//! Presence across a link (docs/PRESENCE_CONTRACT.md section 2): a task
//! beside each connected sync or vault link that carries presence frames both
//! ways, as the link carries edits. On an encrypted link a frame's body is
//! sealed under its document's data key on the way out and opened on the way
//! in; the remote holds it sealed and never stores it. A remote from before
//! presence answers no subscription, and the link carries edits as before.
//!
//! The task ends with the link's connection ([`PresenceLink`] is dropped),
//! and every session it brought here ends with it.

use std::collections::HashMap;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_crypto::{Blob, SymmetricKey};
use pimble_rpc::{PresenceFrame, VaultDocId};
use tokio::sync::broadcast;
use tracing::debug;
use uuid::Uuid;

use crate::handler::RpcHandler;

/// The running task; dropping it stops it and ends the sessions it brought.
pub(crate) struct PresenceLink {
    task: tokio::task::JoinHandle<()>,
    handler: RpcHandler,
    store_id: StoreId,
    link_id: String,
}

impl Drop for PresenceLink {
    fn drop(&mut self) {
        self.task.abort();
        let (handler, store_id, link_id) = (self.handler.clone(), self.store_id, self.link_id.clone());
        tokio::spawn(async move { handler.end_presence_of(store_id, &link_id).await });
    }
}

/// Start carrying presence over `client` for `store_id`; `sealed` for a
/// vault link (the remote is ciphertext).
pub(crate) fn spawn(handler: &RpcHandler, client: Arc<PimbleClient>, store_id: StoreId, link_id: &str, sealed: bool) -> PresenceLink {
    let task = tokio::spawn(run(handler.clone(), client, store_id, link_id.to_string(), sealed));
    PresenceLink { task, handler: handler.clone(), store_id, link_id: link_id.to_string() }
}

async fn run(handler: RpcHandler, client: Arc<PimbleClient>, store_id: StoreId, link_id: String, sealed: bool) {
    let mut sub = match client.subscribe_presence(store_id, &link_id).await {
        Ok(sub) => sub,
        Err(e) => {
            debug!("Link {link_id}: the remote carries no presence for store {store_id} ({e})");
            return;
        }
    };
    let mut local = handler.subscribe_local_presence();
    let mut keys = Keys { sealed, keys: HashMap::new() };

    // What is here now goes there, and what is there now comes here.
    for held in handler.presence_of(store_id) {
        if held.origin != link_id {
            send(&handler, &client, &mut keys, held.frame, &link_id).await;
        }
    }
    if let Ok(frames) = client.get_presence(store_id).await {
        for frame in frames {
            receive(&handler, &client, &mut keys, frame, &link_id).await;
        }
    }

    loop {
        tokio::select! {
            item = sub.next() => match item {
                Some(Ok(frame)) => receive(&handler, &client, &mut keys, frame, &link_id).await,
                _ => return,
            },
            relayed = local.recv() => match relayed {
                Ok(relayed) if relayed.frame.store_id == store_id && relayed.origin != link_id => {
                    send(&handler, &client, &mut keys, relayed.frame, &link_id).await;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

async fn receive(handler: &RpcHandler, client: &PimbleClient, keys: &mut Keys, mut frame: PresenceFrame, link_id: &str) {
    if frame.sealed {
        let Some(body) = keys.open(handler, client, &frame).await else { return };
        frame.body = body;
        frame.sealed = false;
    }
    handler.publish_presence_from(frame, link_id).await;
}

async fn send(handler: &RpcHandler, client: &PimbleClient, keys: &mut Keys, mut frame: PresenceFrame, link_id: &str) {
    if keys.sealed && !frame.left && !frame.sealed {
        let Some(body) = keys.seal(handler, client, &frame).await else { return };
        frame.body = body;
        frame.sealed = true;
    }
    if let Err(e) = client.publish_presence(frame, link_id).await {
        debug!("Link {link_id}: a presence frame was not taken ({e})");
    }
}

/// Each document's data key, unwrapped once per connection.
struct Keys {
    sealed: bool,
    keys: HashMap<NodeId, (Uuid, SymmetricKey)>,
}

impl Keys {
    /// The document's data key, asked of the remote the first time.
    async fn key(&mut self, handler: &RpcHandler, client: &PimbleClient, store_id: StoreId, doc_id: NodeId, wanted: Option<Uuid>) -> Option<(Uuid, SymmetricKey)> {
        if let Some((id, key)) = self.keys.get(&doc_id) {
            if wanted.is_none_or(|w| w == *id) {
                return Some((*id, key.clone()));
            }
        }
        let fetched = client.vault_fetch(store_id, VaultDocId::Node(doc_id), u64::MAX).await.ok()?;
        let doc_keys = fetched.keys?;
        let aad = pimble_crypto::dek_aad(&store_id.to_string(), &doc_id.to_string());
        for wrap in &doc_keys.wraps {
            let Some(scope_key) = handler.keystore().store_key(store_id, wrap.scope_key_id).await else { continue };
            if let Ok(dek) = pimble_crypto::unwrap_dek(wrap, &scope_key, &aad) {
                self.keys.insert(doc_id, (doc_keys.dek_id, dek.clone()));
                return Some((doc_keys.dek_id, dek));
            }
        }
        None
    }

    async fn seal(&mut self, handler: &RpcHandler, client: &PimbleClient, frame: &PresenceFrame) -> Option<String> {
        let plain = URL_SAFE_NO_PAD.decode(&frame.body).ok()?;
        let (key_id, key) = self.key(handler, client, frame.store_id, frame.doc_id, None).await?;
        let aad = pimble_crypto::presence_aad(&frame.store_id.to_string(), &frame.doc_id.to_string());
        Some(URL_SAFE_NO_PAD.encode(Blob::encrypt(&key, key_id, &aad, &plain)))
    }

    async fn open(&mut self, handler: &RpcHandler, client: &PimbleClient, frame: &PresenceFrame) -> Option<String> {
        let blob = URL_SAFE_NO_PAD.decode(&frame.body).ok()?;
        let key_id = Blob::key_id(&blob).ok()?;
        let (_, key) = self.key(handler, client, frame.store_id, frame.doc_id, Some(key_id)).await?;
        let aad = pimble_crypto::presence_aad(&frame.store_id.to_string(), &frame.doc_id.to_string());
        let plain = Blob::decrypt(&key, &aad, &blob).ok()?;
        Some(URL_SAFE_NO_PAD.encode(plain))
    }
}
