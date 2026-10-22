//! Presence (docs/PRESENCE_CONTRACT.md): who is in which document, held in
//! memory and never stored.
//!
//! The hub keeps the latest frame of every live session, so that someone who
//! arrives late (`getPresence`) sees everyone at once, and hands each frame to
//! the `subscribePresence` subscribers who may read its document and to this
//! server's links (an in-process broadcast, like local changes). A session
//! ends with its goodbye frame, when its frames stop for longer than their
//! `ttl_ms`, or when the client or link that published it goes away: then
//! the hub sends a goodbye in its name so nobody waits for the timeout.
//! Nothing here touches a store's files.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use pimble_core::{NodeId, StoreId};
use pimble_rpc::PresenceFrame;
use tokio::sync::broadcast;

/// The longest a frame may ask to be held.
pub(crate) const MAX_TTL: Duration = Duration::from_secs(60);
/// How often expired sessions are swept.
pub(crate) const SWEEP_EVERY: Duration = Duration::from_secs(5);

type Key = (StoreId, NodeId, String);

struct Held {
    frame: PresenceFrame,
    expires: Instant,
    /// The client or link that brought it here.
    origin: String,
}

/// A `subscribePresence` subscriber: what is sent to it goes through a task
/// that owns its sink alone (a `SubscriptionSink` deregisters the
/// subscription when any clone of it is dropped).
pub(crate) struct PresenceSub {
    pub tx: tokio::sync::mpsc::UnboundedSender<jsonrpsee::SubscriptionMessage>,
    /// The scope roots of the subscriber's grant; `None` for the whole store.
    pub roots: Option<Vec<NodeId>>,
    /// Its own client: what it publishes is not sent back to it.
    pub client_id: String,
}

/// A frame as this server's links see it: what it is and where it came from,
/// so a link does not send a frame back to where it came from.
#[derive(Debug, Clone)]
pub(crate) struct Relayed {
    pub frame: PresenceFrame,
    pub origin: String,
}

pub(crate) struct PresenceHub {
    held: Mutex<HashMap<Key, Held>>,
    subs: tokio::sync::Mutex<HashMap<StoreId, Vec<PresenceSub>>>,
    local: broadcast::Sender<Relayed>,
    sweeping: AtomicBool,
}

impl Default for PresenceHub {
    fn default() -> Self {
        Self::new()
    }
}

impl PresenceHub {
    pub(crate) fn new() -> Self {
        let (local, _) = broadcast::channel(1024);
        Self { held: Mutex::new(HashMap::new()), subs: tokio::sync::Mutex::new(HashMap::new()), local, sweeping: AtomicBool::new(false) }
    }

    /// Take `frame` in from `origin`. Answers whether it is news: a frame not
    /// newer than the session's latest is dropped, and so is a goodbye for a
    /// session nobody here holds.
    pub(crate) fn accept(&self, frame: &PresenceFrame, origin: &str) -> bool {
        let key = (frame.store_id, frame.doc_id, frame.session.clone());
        let mut held = self.held.lock().unwrap();
        if let Some(h) = held.get(&key) {
            if frame.seq <= h.frame.seq {
                return false;
            }
        }
        if frame.left {
            return held.remove(&key).is_some();
        }
        let ttl = Duration::from_millis(frame.ttl_ms).min(MAX_TTL);
        held.insert(key, Held { frame: frame.clone(), expires: Instant::now() + ttl, origin: origin.to_string() });
        true
    }

    /// Every live frame of `store_id`.
    pub(crate) fn frames_of(&self, store_id: StoreId) -> Vec<PresenceFrame> {
        self.held_in(store_id).into_iter().map(|r| r.frame).collect()
    }

    /// Every live frame of `store_id` and where each came from.
    pub(crate) fn held_in(&self, store_id: StoreId) -> Vec<Relayed> {
        let now = Instant::now();
        self.held
            .lock()
            .unwrap()
            .values()
            .filter(|h| h.frame.store_id == store_id && h.expires > now)
            .map(|h| Relayed { frame: h.frame.clone(), origin: h.origin.clone() })
            .collect()
    }

    /// End every session `origin` brought in `store_id` (or in every store,
    /// for `None`), answering a goodbye for each.
    pub(crate) fn end_sessions_of(&self, store_id: Option<StoreId>, origin: &str) -> Vec<Relayed> {
        let mut held = self.held.lock().unwrap();
        let ended: Vec<Key> = held.iter().filter(|(k, h)| h.origin == origin && store_id.is_none_or(|s| s == k.0)).map(|(k, _)| k.clone()).collect();
        ended.into_iter().filter_map(|k| held.remove(&k)).map(|h| Relayed { frame: goodbye(&h.frame), origin: h.origin }).collect()
    }

    /// Drop the sessions that went quiet, answering a goodbye for each.
    pub(crate) fn expire(&self) -> Vec<Relayed> {
        let now = Instant::now();
        let mut held = self.held.lock().unwrap();
        let ended: Vec<Key> = held.iter().filter(|(_, h)| h.expires <= now).map(|(k, _)| k.clone()).collect();
        ended.into_iter().filter_map(|k| held.remove(&k)).map(|h| Relayed { frame: goodbye(&h.frame), origin: h.origin }).collect()
    }

    pub(crate) async fn add_sub(&self, store_id: StoreId, sub: PresenceSub) {
        self.subs.lock().await.entry(store_id).or_default().push(sub);
    }

    /// The open subscribers of `store_id`, as `(sender, roots, client)`;
    /// closed ones are dropped on the way.
    pub(crate) async fn subs_of(&self, store_id: StoreId) -> Vec<(tokio::sync::mpsc::UnboundedSender<jsonrpsee::SubscriptionMessage>, Option<Vec<NodeId>>, String)> {
        let mut subs = self.subs.lock().await;
        let Some(list) = subs.get_mut(&store_id) else { return Vec::new() };
        list.retain(|s| !s.tx.is_closed());
        list.iter().map(|s| (s.tx.clone(), s.roots.clone(), s.client_id.clone())).collect()
    }

    pub(crate) fn subscribe_local(&self) -> broadcast::Receiver<Relayed> {
        self.local.subscribe()
    }

    pub(crate) fn relay_locally(&self, frame: &PresenceFrame, origin: &str) {
        let _ = self.local.send(Relayed { frame: frame.clone(), origin: origin.to_string() });
    }

    /// Whether the sweep still has to be started (the first caller starts it).
    pub(crate) fn start_sweeping(&self) -> bool {
        !self.sweeping.swap(true, Ordering::SeqCst)
    }
}

/// The goodbye a server sends in a session's name when it ends.
pub(crate) fn goodbye(frame: &PresenceFrame) -> PresenceFrame {
    PresenceFrame { seq: frame.seq + 1, left: true, body: String::new(), sealed: false, ..frame.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64, session: &str) -> PresenceFrame {
        PresenceFrame {
            store_id: StoreId(uuid::Uuid::nil()),
            doc_id: NodeId(uuid::Uuid::nil()),
            session: session.to_string(),
            seq,
            who: None,
            left: false,
            body: String::new(),
            sealed: false,
            ttl_ms: 30_000,
        }
    }

    #[test]
    fn a_frame_not_newer_than_the_held_one_is_dropped() {
        let hub = PresenceHub::new();
        assert!(hub.accept(&frame(1, "c:0"), "c"));
        assert!(!hub.accept(&frame(1, "c:0"), "link"), "the same frame back round a chain is no news");
        assert!(hub.accept(&frame(2, "c:0"), "c"));
        assert_eq!(hub.frames_of(StoreId(uuid::Uuid::nil())).len(), 1);
    }

    #[test]
    fn a_goodbye_ends_the_session_and_a_second_is_no_news() {
        let hub = PresenceHub::new();
        hub.accept(&frame(1, "c:0"), "c");
        let bye = goodbye(&frame(1, "c:0"));
        assert!(hub.accept(&bye, "c"));
        assert!(!hub.accept(&PresenceFrame { seq: 3, ..bye }, "c"));
        assert!(hub.frames_of(StoreId(uuid::Uuid::nil())).is_empty());
    }

    #[test]
    fn a_client_going_away_ends_only_its_own_sessions() {
        let hub = PresenceHub::new();
        hub.accept(&frame(1, "a:0"), "a");
        hub.accept(&frame(1, "b:0"), "link-1");
        let ended = hub.end_sessions_of(None, "a");
        assert_eq!(ended.len(), 1);
        assert!(ended[0].frame.left && ended[0].frame.seq == 2 && ended[0].frame.session == "a:0");
        assert_eq!(hub.frames_of(StoreId(uuid::Uuid::nil())).len(), 1);
    }

    #[test]
    fn a_quiet_session_expires() {
        let hub = PresenceHub::new();
        hub.accept(&PresenceFrame { ttl_ms: 0, ..frame(1, "a:0") }, "a");
        hub.accept(&frame(1, "b:0"), "b");
        let ended = hub.expire();
        assert_eq!(ended.iter().map(|r| r.frame.session.as_str()).collect::<Vec<_>>(), vec!["a:0"]);
    }
}

/// Send `frame` to the subscribers of its store who may read its document,
/// except the one it came from. A free function over the hub and the store
/// manager so the sweep can hold neither alive.
pub(crate) async fn deliver(hub: &PresenceHub, manager: &tokio::sync::RwLock<pimble_store::StoreManager>, frame: &PresenceFrame, origin: &str) {
    let subs = hub.subs_of(frame.store_id).await;
    if subs.is_empty() {
        return;
    }
    let Ok(msg) = jsonrpsee::SubscriptionMessage::from_json(frame) else { return };
    let mut allowed: HashMap<Vec<NodeId>, bool> = HashMap::new();
    {
        let manager = manager.read().await;
        for (_, roots, _) in &subs {
            if let Some(roots) = roots {
                allowed
                    .entry(roots.clone())
                    .or_insert_with(|| crate::handler::scope_set_of(&manager, frame.store_id, roots).contains(&frame.doc_id));
            }
        }
    }
    for (tx, roots, client) in subs {
        if client == origin {
            continue;
        }
        if let Some(roots) = &roots {
            if !allowed.get(roots).copied().unwrap_or(false) {
                continue;
            }
        }
        let _ = tx.send(msg.clone());
    }
}

/// Every [`SWEEP_EVERY`], end the sessions that went quiet and tell everyone.
/// Stops once the server that owns the hub is gone.
pub(crate) fn spawn_sweep(hub: std::sync::Weak<PresenceHub>, manager: std::sync::Weak<tokio::sync::RwLock<pimble_store::StoreManager>>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_EVERY).await;
            let (Some(hub), Some(manager)) = (hub.upgrade(), manager.upgrade()) else { return };
            for ended in hub.expire() {
                deliver(&hub, &manager, &ended.frame, &ended.origin).await;
                hub.relay_locally(&ended.frame, &ended.origin);
            }
        }
    });
}
