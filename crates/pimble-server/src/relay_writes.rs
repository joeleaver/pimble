//! What an owner's relay face saw others write, reported to the accounts
//! service (docs/NOTIFICATIONS_CONTRACT.md section 3, wave 3).
//!
//! A relayed store lives on its owner's computer, and its members' writes
//! reach the owner's relay face (`crate::relay_face`) through the tunnel,
//! never the hosted server. So the face gathers them as any token-verifying
//! server does (`crate::writes`), every write with the shared roots that
//! reach it, and the owner's own server hears them in its process and posts
//! them (`POST /api/v1/watches/writes`) under the owner's session: the
//! owner's device does not know who watches what, so it reports writes and
//! the service evaluates watches as it does for a hosted store. Only stores
//! the tunnel serves are reported (a twin being rebuilt is filled from the
//! store itself, and those are nobody's new writes).
//!
//! **Nothing is lost while the service is away.** Every write is kept in the
//! store's own state (`sync-state.json`'s `relay_writes` section, the way the
//! store's link state is kept) before it is posted, and taken out only once
//! the service has it, so a write made while the service could not be
//! reached, or the session had lapsed, or the app quit before the post, is
//! told when it can be: a moment later, and again with backoff, and when the
//! store is served again. Writes by one person to one document a moment
//! apart are kept as one, so a long outage keeps a bounded list. A report the
//! service refuses for good (400, 403, 404) is dropped: it will never take
//! it. This works only while the owner's computer is on, as the relay does.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use pimble_core::watch::SPELL_GAP_MS;
use pimble_core::StoreId;
use pimble_host::http::Http;
use pimble_host::Disk;
use pimble_rpc::WriteSeen;
use pimble_store::sync_state;
use tokio::sync::{broadcast, watch};
use tracing::{debug, warn};

use crate::cloud::ReportedWrite;
use crate::handler::RpcHandler;

/// The most writes one post carries (the service takes 1,000).
const PER_POST: usize = 500;
/// The most writes kept for one store; beyond it the oldest go.
const MOST_KEPT: usize = 20_000;
/// The first wait before a report is tried again, doubling to [`LONGEST_WAIT`].
const FIRST_WAIT: Duration = Duration::from_secs(2);
const LONGEST_WAIT: Duration = Duration::from_secs(5 * 60);

/// Whom the reports go to: the signed-in account's service and session.
#[derive(Debug, Clone)]
pub(crate) struct Reporter {
    pub url: String,
    pub session: String,
}

/// How a store's reports went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    /// Everything kept was taken (or refused for good).
    All,
    /// Some is still kept: try again later.
    Later,
    /// The session lapsed: kept until someone signs in again.
    Expired,
}

/// The writes not yet taken by the service, per store, kept on disk.
pub(crate) struct Outbox {
    disk: Disk,
    /// Each store's state directory (`None`: kept in memory only).
    dirs: HashMap<StoreId, Option<PathBuf>>,
    held: HashMap<StoreId, Vec<ReportedWrite>>,
}

impl Outbox {
    pub fn new(disk: Disk) -> Self {
        Self { disk, dirs: HashMap::new(), held: HashMap::new() }
    }

    /// Read what `store` kept from an earlier run, once.
    pub async fn load(&mut self, store: StoreId, dir: Option<PathBuf>) {
        if self.dirs.contains_key(&store) {
            return;
        }
        let kept: Vec<ReportedWrite> = match &dir {
            Some(dir) => sync_state::read_section(&self.disk, dir, sync_state::RELAY_WRITES)
                .await
                .and_then(|section| section.get("writes").cloned())
                .and_then(|writes| serde_json::from_value(writes).ok())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        self.dirs.insert(store, dir);
        if !kept.is_empty() {
            debug!("Relay writes: {} kept for store {} from before", kept.len(), store);
            self.held.entry(store).or_default().extend(kept);
        }
    }

    /// Keep `writes` to `store` (loaded first), on disk before anything is
    /// posted.
    pub async fn add(&mut self, store: StoreId, writes: Vec<ReportedWrite>) {
        let held = self.held.entry(store).or_default();
        for write in writes {
            match held.iter_mut().rev().find(|w| w.doc == write.doc && w.writer == write.writer) {
                // A moment after the last one kept: the same spell of editing.
                Some(last) if (write.last_ms - last.last_ms).abs() < SPELL_GAP_MS => {
                    last.count = last.count.saturating_add(write.count);
                    last.last_ms = last.last_ms.max(write.last_ms);
                    for root in write.roots {
                        if !last.roots.contains(&root) {
                            last.roots.push(root);
                        }
                    }
                }
                _ => held.push(write),
            }
        }
        if held.len() > MOST_KEPT {
            let over = held.len() - MOST_KEPT;
            warn!("Relay writes: {} of store {} were never taken by the service and are dropped (the oldest)", over, store);
            held.drain(..over);
        }
        self.save(store).await;
    }

    /// Whether anything is kept for any store.
    pub fn waiting(&self) -> Vec<StoreId> {
        self.held.iter().filter(|(_, w)| !w.is_empty()).map(|(s, _)| *s).collect()
    }

    pub fn kept(&self, store: StoreId) -> &[ReportedWrite] {
        self.held.get(&store).map(Vec::as_slice).unwrap_or_default()
    }

    async fn save(&mut self, store: StoreId) {
        let Some(Some(dir)) = self.dirs.get(&store) else { return };
        let writes = self.held.get(&store).filter(|w| !w.is_empty());
        let section = writes.map(|w| serde_json::json!({ "writes": w }));
        if let Err(e) = sync_state::set_section(&self.disk, dir, sync_state::RELAY_WRITES, section).await {
            warn!("Relay writes: what store {} keeps could not be saved: {}", store, e);
        }
    }

    /// Post what `store` keeps, a part at a time, taking out each part the
    /// service took (or refused for good).
    pub async fn send(&mut self, http: &dyn Http, to: Option<&Reporter>, store: StoreId) -> Sent {
        let Some(to) = to else { return if self.kept(store).is_empty() { Sent::All } else { Sent::Later } };
        let mut outcome = Sent::All;
        loop {
            let part: Vec<ReportedWrite> = self.kept(store).iter().take(PER_POST).cloned().collect();
            if part.is_empty() {
                break;
            }
            match crate::cloud::post_relay_writes(http, &to.url, &to.session, &store.to_string(), &part).await {
                Ok(()) => {}
                Err(e) if matches!(e.status(), Some(400 | 403 | 404)) => {
                    debug!("Relay writes: the service refused {} for store {}: {}", part.len(), store, e);
                }
                Err(e) if e.status() == Some(401) => {
                    outcome = Sent::Expired;
                    break;
                }
                Err(e) => {
                    debug!("Relay writes: {} for store {} not reported yet: {}", part.len(), store, e);
                    outcome = Sent::Later;
                    break;
                }
            }
            if let Some(held) = self.held.get_mut(&store) {
                held.drain(..part.len().min(held.len()));
            }
            self.save(store).await;
        }
        outcome
    }
}

/// `store`'s state directory on the owner's server: where its link state
/// lives, whether or not it is open now.
async fn dir_of(host: &RpcHandler, store: StoreId) -> Option<PathBuf> {
    let manager = host.store_manager_handle();
    let manager = manager.read().await;
    manager.state_dir(store).or_else(|| manager.state_root().map(|root| root.join(store.to_string())))
}

async fn reporter(host: &RpcHandler) -> Option<Reporter> {
    host.keystore().account().await.map(|account| Reporter { url: account.url.clone(), session: account.session.clone() })
}

fn reported(seen: WriteSeen) -> ReportedWrite {
    ReportedWrite { doc: seen.doc_id.to_string(), writer: seen.writer, count: seen.count, roots: seen.roots.iter().map(|r| r.to_string()).collect(), last_ms: seen.last_ms }
}

/// Hear `writes` (the face's) and report those to `served` stores, under
/// whoever is signed in to `host` (the owner's server). Ends when the face
/// goes; what was not taken stays kept for the next time.
pub(crate) async fn run(host: RpcHandler, mut writes: broadcast::Receiver<WriteSeen>, mut served: watch::Receiver<HashSet<StoreId>>) {
    let mut outbox = Outbox::new(host.disk().clone());
    let mut wait = FIRST_WAIT;
    let mut look_again = true;
    loop {
        // What the stores served kept from before, and what failed a moment
        // ago, is sent first.
        if look_again {
            look_again = false;
            let stores: Vec<StoreId> = served.borrow().iter().copied().collect();
            for store in stores {
                outbox.load(store, dir_of(&host, store).await).await;
            }
            let to = reporter(&host).await;
            let mut later = false;
            for store in outbox.waiting() {
                match outbox.send(host.http(), to.as_ref(), store).await {
                    Sent::All => {}
                    Sent::Later => later = true,
                    Sent::Expired => {
                        host.keystore().mark_session_expired();
                        later = true;
                    }
                }
            }
            wait = if later { (wait * 2).min(LONGEST_WAIT) } else { FIRST_WAIT };
        }
        let retry = !outbox.waiting().is_empty();
        tokio::select! {
            first = writes.recv() => {
                let first = match first {
                    Ok(seen) => seen,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Relay writes: {} were not reported (fell behind)", n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                // Whatever else is waiting goes in the same reports, per store.
                let mut by_store: BTreeMap<StoreId, Vec<ReportedWrite>> = BTreeMap::new();
                let mut take = |seen: WriteSeen| {
                    if served.borrow().contains(&seen.store_id) {
                        by_store.entry(seen.store_id).or_default().push(reported(seen));
                    }
                };
                take(first);
                while let Ok(seen) = writes.try_recv() {
                    take(seen);
                }
                for (store, reported) in by_store {
                    outbox.load(store, dir_of(&host, store).await).await;
                    outbox.add(store, reported).await;
                }
                wait = FIRST_WAIT;
                look_again = true;
            }
            changed = served.changed() => {
                if changed.is_err() {
                    return;
                }
                look_again = true;
            }
            _ = host.runtime().sleep(wait), if retry => look_again = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_host::http::{HttpError, HttpRequest, HttpResponse};
    use std::sync::Mutex;

    /// A service that answers each post with the next status it was given
    /// (200 once they run out), and keeps what it took.
    #[derive(Debug, Default)]
    struct Service {
        answers: Mutex<Vec<u16>>,
        took: Mutex<Vec<ReportedWrite>>,
    }

    #[async_trait::async_trait]
    impl Http for Service {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
            let status = { self.answers.lock().unwrap().pop().unwrap_or(200) };
            if status == 0 {
                return Err(HttpError("the service is not reachable".into()));
            }
            if status == 200 {
                let body: serde_json::Value = serde_json::from_slice(&request.json.unwrap()).unwrap();
                let writes: Vec<ReportedWrite> = serde_json::from_value(body["writes"].clone()).unwrap();
                self.took.lock().unwrap().extend(writes);
            }
            Ok(HttpResponse { status, body: b"{}".to_vec() })
        }
    }

    fn write(doc: &str, writer: &str, last_ms: i64) -> ReportedWrite {
        ReportedWrite { doc: doc.into(), writer: writer.into(), count: 1, roots: vec!["r".into()], last_ms }
    }

    fn to() -> Reporter {
        Reporter { url: "https://cloud.test".into(), session: "s".into() }
    }

    #[tokio::test]
    async fn writes_the_service_could_not_take_are_kept_on_disk_and_sent_later() {
        let disk = Disk::memory();
        let dir = PathBuf::from("/state/store");
        let store = StoreId::new();
        let service = Service::default();
        let mut outbox = Outbox::new(disk.clone());
        outbox.load(store, Some(dir.clone())).await;
        outbox.add(store, vec![write("d", "ann", 0), write("e", "ann", 0)]).await;

        // The service is away, then the session lapsed: nothing is lost.
        service.answers.lock().unwrap().push(0);
        assert_eq!(outbox.send(&service, Some(&to()), store).await, Sent::Later);
        service.answers.lock().unwrap().push(401);
        assert_eq!(outbox.send(&service, Some(&to()), store).await, Sent::Expired);
        assert_eq!(outbox.send(&service, None, store).await, Sent::Later, "nobody signed in");
        assert!(service.took.lock().unwrap().is_empty());

        // The app quits and starts again: what was kept is read back.
        let mut again = Outbox::new(disk.clone());
        again.load(store, Some(dir.clone())).await;
        assert_eq!(again.kept(store).len(), 2);
        assert_eq!(again.send(&service, Some(&to()), store).await, Sent::All);
        assert_eq!(service.took.lock().unwrap().len(), 2);
        assert!(again.waiting().is_empty());
        assert!(sync_state::read_section(&disk, &dir, sync_state::RELAY_WRITES).await.is_none(), "nothing kept once taken");
    }

    #[tokio::test]
    async fn a_spell_of_writes_is_kept_as_one_and_a_refusal_is_dropped() {
        let store = StoreId::new();
        let mut outbox = Outbox::new(Disk::memory());
        outbox.load(store, None).await;
        outbox.add(store, vec![write("d", "ann", 0), write("d", "bob", 1_000)]).await;
        let mut later = write("d", "ann", 10_000);
        later.roots = vec!["r".into(), "s".into()];
        outbox.add(store, vec![later, write("d", "ann", 10_000 + SPELL_GAP_MS)]).await;
        let kept = outbox.kept(store);
        assert_eq!(kept.len(), 3, "Ann's two writes ten seconds apart are one, the one half a minute on is not");
        assert_eq!((kept[0].count, kept[0].last_ms, kept[0].roots.clone()), (2, 10_000, vec!["r".to_string(), "s".to_string()]));

        let service = Service::default();
        service.answers.lock().unwrap().push(403);
        assert_eq!(outbox.send(&service, Some(&to()), store).await, Sent::All, "refused for good: not kept");
        assert!(outbox.waiting().is_empty());
        assert!(service.took.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn many_writes_go_a_part_at_a_time() {
        let store = StoreId::new();
        let mut outbox = Outbox::new(Disk::memory());
        outbox.load(store, None).await;
        let many: Vec<ReportedWrite> = (0..PER_POST + 10).map(|i| write(&format!("d{i}"), "ann", 0)).collect();
        outbox.add(store, many).await;
        let service = Service::default();
        // The second part fails: the first stays taken, the second kept.
        service.answers.lock().unwrap().extend([0, 200]);
        assert_eq!(outbox.send(&service, Some(&to()), store).await, Sent::Later);
        assert_eq!(service.took.lock().unwrap().len(), PER_POST);
        assert_eq!(outbox.kept(store).len(), 10);
        assert_eq!(outbox.send(&service, Some(&to()), store).await, Sent::All);
        assert_eq!(service.took.lock().unwrap().len(), PER_POST + 10);
    }
}
