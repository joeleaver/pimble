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
//! store's link state is kept, written whole to a temporary and renamed into
//! place) before it is posted, and taken out only once the service has it,
//! so a write made while the service could not be reached, or the session
//! had lapsed, or the app quit before the post, is told when it can be: a
//! moment later, and again with backoff, and when the store is served
//! again. Writes by one person to one document a moment apart are kept as
//! one, so a long outage keeps a bounded list. A report the service refuses
//! for good (400, 403, 404) is dropped: it will never take it. This works
//! only while the owner's computer is on, as the relay does.
//!
//! **Stopping loses nothing.** The face stops before its reports
//! (`RelayHost::stop`): what it gathered and had not told yet is told as it
//! stops, and the reports then keep all of it on disk and end ([`run`]'s
//! `stop`). A post under way is let go, never a write not yet kept: every
//! write is on disk before any post carries it.
//!
//! **Each report is told once.** A part posted is first sealed as a report
//! with an id of its own and kept so; it is sent with that id until the
//! service takes it, and nothing more is added to it. The service remembers
//! the ids it took, so a report whose answer was lost (the app quit, the
//! connection broke after the post) is sent again and taken once.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use pimble_core::watch::SPELL_GAP_MS;
use pimble_core::StoreId;
use pimble_host::http::Http;
use pimble_host::{Disk, Runtime};
use pimble_rpc::WriteSeen;
use pimble_store::sync_state;
use serde::{Deserialize, Serialize};
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
    /// Asked to stop while a post was under way: everything not taken is
    /// still kept.
    Stopped,
}

/// A part of a store's writes sealed for posting: sent with its id until
/// the service takes it, nothing added to it meanwhile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Report {
    id: String,
    writes: Vec<ReportedWrite>,
}

/// What one store keeps: the reports sealed and not yet taken, oldest
/// first, and the writes not in a report yet, to which new ones are added.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reports: Vec<Report>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    writes: Vec<ReportedWrite>,
}

impl Kept {
    fn len(&self) -> usize {
        self.reports.iter().map(|r| r.writes.len()).sum::<usize>() + self.writes.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Keep no more than [`MOST_KEPT`]: the oldest go, from the oldest
    /// report on. Answers how many went.
    fn bound(&mut self) -> usize {
        let mut over = self.len().saturating_sub(MOST_KEPT);
        let dropped = over;
        while over > 0 {
            match self.reports.first_mut() {
                Some(first) => {
                    let n = over.min(first.writes.len());
                    first.writes.drain(..n);
                    over -= n;
                    if first.writes.is_empty() {
                        self.reports.remove(0);
                    }
                }
                None => {
                    self.writes.drain(..over.min(self.writes.len()));
                    over = 0;
                }
            }
        }
        dropped
    }
}

/// Until `stop` says so (or its sender is gone).
async fn stop_asked(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

/// The writes not yet taken by the service, per store, kept on disk.
pub(crate) struct Outbox {
    disk: Disk,
    /// Each store's state directory (`None`: kept in memory only).
    dirs: HashMap<StoreId, Option<PathBuf>>,
    held: HashMap<StoreId, Kept>,
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
        let kept: Kept = match &dir {
            Some(dir) => sync_state::read_section(&self.disk, dir, sync_state::RELAY_WRITES)
                .await
                .and_then(|section| serde_json::from_value(section).ok())
                .unwrap_or_default(),
            None => Kept::default(),
        };
        self.dirs.insert(store, dir);
        if !kept.is_empty() {
            debug!("Relay writes: {} kept for store {} from before", kept.len(), store);
            let held = self.held.entry(store).or_default();
            // Kept from before goes first: it is older than anything heard
            // since.
            let mut writes = kept.writes;
            writes.append(&mut held.writes);
            held.writes = writes;
            let mut reports = kept.reports;
            reports.append(&mut held.reports);
            held.reports = reports;
        }
    }

    /// Keep `writes` to `store` (loaded first), on disk before anything is
    /// posted.
    pub async fn add(&mut self, store: StoreId, writes: Vec<ReportedWrite>) {
        let held = self.held.entry(store).or_default();
        for write in writes {
            match held.writes.iter_mut().rev().find(|w| w.doc == write.doc && w.writer == write.writer) {
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
                _ => held.writes.push(write),
            }
        }
        let dropped = held.bound();
        if dropped > 0 {
            warn!("Relay writes: {} of store {} were never taken by the service and are dropped (the oldest)", dropped, store);
        }
        self.save(store).await;
    }

    /// The stores anything is kept for.
    pub fn waiting(&self) -> Vec<StoreId> {
        self.held.iter().filter(|(_, kept)| !kept.is_empty()).map(|(s, _)| *s).collect()
    }

    /// Everything kept for `store`, oldest first.
    #[cfg(test)]
    fn kept(&self, store: StoreId) -> Vec<ReportedWrite> {
        let Some(kept) = self.held.get(&store) else { return Vec::new() };
        kept.reports.iter().flat_map(|r| r.writes.iter()).chain(kept.writes.iter()).cloned().collect()
    }

    async fn save(&mut self, store: StoreId) {
        let Some(Some(dir)) = self.dirs.get(&store) else { return };
        let section = match self.held.get(&store).filter(|kept| !kept.is_empty()) {
            Some(kept) => match serde_json::to_value(kept) {
                Ok(value) => Some(value),
                Err(e) => {
                    warn!("Relay writes: what store {} keeps could not be written down: {}", store, e);
                    return;
                }
            },
            None => None,
        };
        if let Err(e) = sync_state::set_section(&self.disk, dir, sync_state::RELAY_WRITES, section).await {
            warn!("Relay writes: what store {} keeps could not be saved: {}", store, e);
        }
    }

    /// The oldest report of `store` to post: the one sealed already, or the
    /// oldest writes sealed into one now and kept so before it is posted.
    async fn next_report(&mut self, store: StoreId) -> Option<Report> {
        let kept = self.held.get_mut(&store)?;
        if kept.reports.is_empty() {
            if kept.writes.is_empty() {
                return None;
            }
            let n = kept.writes.len().min(PER_POST);
            let writes: Vec<ReportedWrite> = kept.writes.drain(..n).collect();
            kept.reports.push(Report { id: uuid::Uuid::new_v4().to_string(), writes });
            self.save(store).await;
        }
        self.held.get(&store)?.reports.first().cloned()
    }

    /// Post what `store` keeps, a report at a time, taking out each the
    /// service took (or refused for good). A post under way when `stop` is
    /// asked is let go: what it carried is still kept, and sent again (with
    /// its id) the next time.
    pub async fn send(&mut self, http: &dyn Http, to: Option<&Reporter>, store: StoreId, stop: &mut watch::Receiver<bool>) -> Sent {
        let Some(to) = to else { return if self.held.get(&store).is_none_or(Kept::is_empty) { Sent::All } else { Sent::Later } };
        loop {
            let Some(report) = self.next_report(store).await else { return Sent::All };
            let store_id = store.to_string();
            let posted = tokio::select! {
                biased;
                _ = stop_asked(stop) => return Sent::Stopped,
                posted = crate::cloud::post_relay_writes(http, &to.url, &to.session, &store_id, &report.id, &report.writes) => posted,
            };
            match posted {
                Ok(()) => {}
                Err(e) if matches!(e.status(), Some(400 | 403 | 404)) => {
                    debug!("Relay writes: the service refused {} for store {}: {}", report.writes.len(), store, e);
                }
                Err(e) if e.status() == Some(401) => return Sent::Expired,
                Err(e) => {
                    debug!("Relay writes: {} for store {} not reported yet: {}", report.writes.len(), store, e);
                    return Sent::Later;
                }
            }
            if let Some(kept) = self.held.get_mut(&store) {
                kept.reports.retain(|r| r.id != report.id);
            }
            self.save(store).await;
        }
    }
}

/// What the reports need of the owner's server: its disk, its HTTP, its
/// clock, where each store's state lives, and who is signed in.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub(crate) trait Owner: Send + Sync {
    fn disk(&self) -> &Disk;
    fn http(&self) -> &dyn Http;
    fn runtime(&self) -> &Runtime;
    /// `store`'s state directory: where its link state lives, whether or
    /// not it is open now.
    async fn state_dir(&self, store: StoreId) -> Option<PathBuf>;
    /// The signed-in account's service and session.
    async fn reporter(&self) -> Option<Reporter>;
    /// The service said the session lapsed.
    fn session_expired(&self);
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Owner for RpcHandler {
    fn disk(&self) -> &Disk {
        RpcHandler::disk(self)
    }

    fn http(&self) -> &dyn Http {
        RpcHandler::http(self)
    }

    fn runtime(&self) -> &Runtime {
        RpcHandler::runtime(self)
    }

    async fn state_dir(&self, store: StoreId) -> Option<PathBuf> {
        let manager = self.store_manager_handle();
        let manager = manager.read().await;
        manager.state_dir(store).or_else(|| manager.state_root().map(|root| root.join(store.to_string())))
    }

    async fn reporter(&self) -> Option<Reporter> {
        self.keystore().account().await.map(|account| Reporter { url: account.url.clone(), session: account.session.clone() })
    }

    fn session_expired(&self) {
        self.keystore().mark_session_expired();
    }
}

fn reported(seen: WriteSeen) -> ReportedWrite {
    ReportedWrite { doc: seen.doc_id.to_string(), writer: seen.writer, count: seen.count, roots: seen.roots.iter().map(|r| r.to_string()).collect(), last_ms: seen.last_ms }
}

/// Keep `first` and every write already waiting in `writes` that is to a
/// `served` store, per store, on disk.
async fn keep_heard(owner: &impl Owner, outbox: &mut Outbox, first: Option<WriteSeen>, writes: &mut broadcast::Receiver<WriteSeen>, served: &watch::Receiver<HashSet<StoreId>>) {
    let mut by_store: BTreeMap<StoreId, Vec<ReportedWrite>> = BTreeMap::new();
    let mut take = |seen: WriteSeen| {
        if served.borrow().contains(&seen.store_id) {
            by_store.entry(seen.store_id).or_default().push(reported(seen));
        }
    };
    if let Some(first) = first {
        take(first);
    }
    loop {
        match writes.try_recv() {
            Ok(seen) => take(seen),
            Err(broadcast::error::TryRecvError::Lagged(n)) => warn!("Relay writes: {} were not reported (fell behind)", n),
            Err(_) => break,
        }
    }
    for (store, reported) in by_store {
        outbox.load(store, owner.state_dir(store).await).await;
        outbox.add(store, reported).await;
    }
}

/// Hear `writes` (the face's) and report those to `served` stores, under
/// whoever is signed in to `owner` (the owner's server), until `stop` is
/// asked (or its sender is gone) or the face's writes end. Then whatever
/// was told and not kept yet (the face tells what it gathered as it stops,
/// before `stop` is asked) is kept on disk, and nothing more is posted:
/// what was not taken stays kept for the next time.
pub(crate) async fn run(owner: impl Owner, mut writes: broadcast::Receiver<WriteSeen>, mut served: watch::Receiver<HashSet<StoreId>>, mut stop: watch::Receiver<bool>) {
    let mut outbox = Outbox::new(owner.disk().clone());
    let mut wait = FIRST_WAIT;
    let mut look_again = true;
    'reporting: loop {
        // What the stores served kept from before, and what failed a moment
        // ago, is sent first.
        if look_again {
            look_again = false;
            let stores: Vec<StoreId> = served.borrow().iter().copied().collect();
            for store in stores {
                outbox.load(store, owner.state_dir(store).await).await;
            }
            let to = owner.reporter().await;
            let mut later = false;
            for store in outbox.waiting() {
                match outbox.send(owner.http(), to.as_ref(), store, &mut stop).await {
                    Sent::All => {}
                    Sent::Later => later = true,
                    Sent::Expired => {
                        owner.session_expired();
                        later = true;
                    }
                    Sent::Stopped => break 'reporting,
                }
            }
            wait = if later { (wait * 2).min(LONGEST_WAIT) } else { FIRST_WAIT };
        }
        let retry = !outbox.waiting().is_empty();
        tokio::select! {
            biased;
            _ = stop_asked(&mut stop) => break,
            first = writes.recv() => {
                let first = match first {
                    Ok(seen) => seen,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Relay writes: {} were not reported (fell behind)", n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                // Whatever else is waiting goes in the same reports, per store.
                keep_heard(&owner, &mut outbox, Some(first), &mut writes, &served).await;
                wait = FIRST_WAIT;
                look_again = true;
            }
            changed = served.changed() => {
                if changed.is_err() {
                    break;
                }
                look_again = true;
            }
            _ = owner.runtime().sleep(wait), if retry => look_again = true,
        }
    }
    // Stopping: what the face told as it stopped is kept, not posted.
    keep_heard(&owner, &mut outbox, None, &mut writes, &served).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::NodeId;
    use pimble_host::http::{HttpError, HttpRequest, HttpResponse};
    use std::sync::{Arc, Mutex};

    /// The service's answers, one per post (200 once they run out): a
    /// status, or 0 (not reachable), [`TOOK_IT_ANSWER_LOST`] or [`HANGS`].
    const TOOK_IT_ANSWER_LOST: u16 = 1;
    const HANGS: u16 = 2;

    /// A service that answers each post with the next answer it was given,
    /// takes each report once (by its id, as the accounts service does),
    /// and keeps what it took.
    #[derive(Debug, Default)]
    struct Service {
        answers: Mutex<Vec<u16>>,
        took: Mutex<Vec<ReportedWrite>>,
        /// The id of every report posted, in order.
        posted: Mutex<Vec<String>>,
        /// The ids of the reports taken.
        taken: Mutex<HashSet<String>>,
    }

    #[async_trait::async_trait]
    impl Http for Service {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
            let status = { self.answers.lock().unwrap().pop().unwrap_or(200) };
            let body: serde_json::Value = serde_json::from_slice(&request.json.unwrap()).unwrap();
            let id = body["report"].as_str().expect("every report has an id").to_string();
            match status {
                0 => return Err(HttpError("the service is not reachable".into())),
                HANGS => std::future::pending::<()>().await,
                _ => {}
            }
            self.posted.lock().unwrap().push(id.clone());
            if (status == 200 || status == TOOK_IT_ANSWER_LOST) && self.taken.lock().unwrap().insert(id) {
                let writes: Vec<ReportedWrite> = serde_json::from_value(body["writes"].clone()).unwrap();
                self.took.lock().unwrap().extend(writes);
            }
            if status == TOOK_IT_ANSWER_LOST {
                return Err(HttpError("the connection broke before the answer".into()));
            }
            Ok(HttpResponse { status, body: b"{}".to_vec() })
        }

        /// Reports go to the accounts service over plain calls only; a test of
        /// them never asks a relay how it answers a socket.
        async fn websocket_close_code(&self, _url: &str, _token: &str) -> Option<u16> {
            None
        }
    }

    fn write(doc: &str, writer: &str, last_ms: i64) -> ReportedWrite {
        ReportedWrite { doc: doc.into(), writer: writer.into(), count: 1, roots: vec!["r".into()], last_ms }
    }

    fn to() -> Reporter {
        Reporter { url: "https://cloud.test".into(), session: "s".into() }
    }

    /// A stop nobody asks for (its sender kept with it).
    fn no_stop() -> (watch::Sender<bool>, watch::Receiver<bool>) {
        watch::channel(false)
    }

    #[tokio::test]
    async fn writes_the_service_could_not_take_are_kept_on_disk_and_sent_later() {
        let disk = Disk::memory();
        let dir = PathBuf::from("/state/store");
        let store = StoreId::new();
        let service = Service::default();
        let (_keep, mut stop) = no_stop();
        let mut outbox = Outbox::new(disk.clone());
        outbox.load(store, Some(dir.clone())).await;
        outbox.add(store, vec![write("d", "ann", 0), write("e", "ann", 0)]).await;

        // The service is away, then the session lapsed: nothing is lost.
        service.answers.lock().unwrap().push(0);
        assert_eq!(outbox.send(&service, Some(&to()), store, &mut stop).await, Sent::Later);
        service.answers.lock().unwrap().push(401);
        assert_eq!(outbox.send(&service, Some(&to()), store, &mut stop).await, Sent::Expired);
        assert_eq!(outbox.send(&service, None, store, &mut stop).await, Sent::Later, "nobody signed in");
        assert!(service.took.lock().unwrap().is_empty());

        // The app quits and starts again: what was kept is read back.
        let mut again = Outbox::new(disk.clone());
        again.load(store, Some(dir.clone())).await;
        assert_eq!(again.kept(store).len(), 2);
        assert_eq!(again.send(&service, Some(&to()), store, &mut stop).await, Sent::All);
        assert_eq!(service.took.lock().unwrap().len(), 2);
        assert!(again.waiting().is_empty());
        assert!(sync_state::read_section(&disk, &dir, sync_state::RELAY_WRITES).await.is_none(), "nothing kept once taken");
    }

    #[tokio::test]
    async fn a_report_whose_answer_was_lost_is_sent_again_with_its_id_and_taken_once() {
        let disk = Disk::memory();
        let dir = PathBuf::from("/state/store");
        let store = StoreId::new();
        let service = Service::default();
        let (_keep, mut stop) = no_stop();
        let mut outbox = Outbox::new(disk.clone());
        outbox.load(store, Some(dir.clone())).await;
        outbox.add(store, vec![write("d", "ann", 0)]).await;
        service.answers.lock().unwrap().push(TOOK_IT_ANSWER_LOST);
        assert_eq!(outbox.send(&service, Some(&to()), store, &mut stop).await, Sent::Later);
        assert_eq!(service.took.lock().unwrap().len(), 1, "the service has it");

        // Another write of the same spell comes: it is not added to the
        // report sent already, which the service would not take again.
        outbox.add(store, vec![write("d", "ann", 1_000)]).await;
        // The app quits before the answer: the report is read back as it was.
        let mut again = Outbox::new(disk.clone());
        again.load(store, Some(dir.clone())).await;
        assert_eq!(again.send(&service, Some(&to()), store, &mut stop).await, Sent::All);
        let posted = service.posted.lock().unwrap().clone();
        assert_eq!(posted.len(), 3, "the report twice, then the new write");
        assert_eq!(posted[0], posted[1], "sent again with the same id");
        assert_ne!(posted[1], posted[2]);
        let took = service.took.lock().unwrap().clone();
        assert_eq!(took.iter().map(|w| w.last_ms).collect::<Vec<_>>(), vec![0, 1_000], "each write taken once");
    }

    #[tokio::test]
    async fn a_spell_of_writes_is_kept_as_one_and_a_refusal_is_dropped() {
        let store = StoreId::new();
        let (_keep, mut stop) = no_stop();
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
        assert_eq!(outbox.send(&service, Some(&to()), store, &mut stop).await, Sent::All, "refused for good: not kept");
        assert!(outbox.waiting().is_empty());
        assert!(service.took.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn many_writes_go_a_part_at_a_time() {
        let store = StoreId::new();
        let (_keep, mut stop) = no_stop();
        let mut outbox = Outbox::new(Disk::memory());
        outbox.load(store, None).await;
        let many: Vec<ReportedWrite> = (0..PER_POST + 10).map(|i| write(&format!("d{i}"), "ann", 0)).collect();
        outbox.add(store, many).await;
        let service = Service::default();
        // The second part fails: the first stays taken, the second kept.
        service.answers.lock().unwrap().extend([0, 200]);
        assert_eq!(outbox.send(&service, Some(&to()), store, &mut stop).await, Sent::Later);
        assert_eq!(service.took.lock().unwrap().len(), PER_POST);
        assert_eq!(outbox.kept(store).len(), 10);
        assert_eq!(outbox.send(&service, Some(&to()), store, &mut stop).await, Sent::All);
        assert_eq!(service.took.lock().unwrap().len(), PER_POST + 10);
    }

    #[tokio::test]
    async fn a_post_under_way_is_let_go_at_stop_and_its_writes_stay_kept() {
        let disk = Disk::memory();
        let dir = PathBuf::from("/state/store");
        let store = StoreId::new();
        let service = Service::default();
        service.answers.lock().unwrap().push(HANGS);
        let (stopping, mut stop) = no_stop();
        let mut outbox = Outbox::new(disk.clone());
        outbox.load(store, Some(dir.clone())).await;
        outbox.add(store, vec![write("d", "ann", 0)]).await;
        let asked = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stopping.send(true).unwrap();
        };
        let reporter = to();
        let (sent, ()) = tokio::join!(outbox.send(&service, Some(&reporter), store, &mut stop), asked);
        assert_eq!(sent, Sent::Stopped);
        let mut again = Outbox::new(disk);
        again.load(store, Some(dir)).await;
        assert_eq!(again.kept(store).len(), 1, "on disk, to be sent the next time");
    }

    /// The owner's server, as the reports see it.
    struct TestOwner {
        disk: Disk,
        service: Arc<Service>,
        runtime: Runtime,
    }

    #[async_trait::async_trait]
    impl Owner for TestOwner {
        fn disk(&self) -> &Disk {
            &self.disk
        }
        fn http(&self) -> &dyn Http {
            &*self.service
        }
        fn runtime(&self) -> &Runtime {
            &self.runtime
        }
        async fn state_dir(&self, store: StoreId) -> Option<PathBuf> {
            Some(PathBuf::from(format!("/state/{store}")))
        }
        async fn reporter(&self) -> Option<Reporter> {
            Some(to())
        }
        fn session_expired(&self) {}
    }

    /// What the face does as it stops (it tells what it gathered), and then
    /// the stop: every write told is kept on disk, none lost, whether the
    /// reports had heard it yet or were posting it.
    #[tokio::test]
    async fn stopping_keeps_what_the_face_told_as_it_stopped() {
        let disk = Disk::memory();
        let store = StoreId::new();
        let service = Arc::new(Service::default());
        // Every post hangs: nothing reaches the service before the stop.
        service.answers.lock().unwrap().extend([HANGS; 4]);
        let owner = TestOwner { disk: disk.clone(), service: Arc::clone(&service), runtime: Runtime::default() };
        let (face, writes) = broadcast::channel(16);
        let (_served_tx, served) = watch::channel(HashSet::from([store]));
        let (stopping, stop) = watch::channel(false);
        let reports = tokio::spawn(run(owner, writes, served, stop));
        let seen = |writer: &str| WriteSeen { store_id: store, doc_id: NodeId::new(), writer: writer.into(), count: 1, roots: vec![], last_ms: 0 };
        face.send(seen("ann")).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The face stops: it tells what it gathered, then the reports stop.
        face.send(seen("bob")).unwrap();
        stopping.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), reports).await.expect("the reports end once asked").unwrap();
        let mut outbox = Outbox::new(disk);
        outbox.load(store, Some(PathBuf::from(format!("/state/{store}")))).await;
        let mut writers: Vec<String> = outbox.kept(store).into_iter().map(|w| w.writer).collect();
        writers.sort();
        assert_eq!(writers, vec!["ann".to_string(), "bob".to_string()], "both kept, to be told the next time");
        assert!(service.took.lock().unwrap().is_empty());
    }
}
