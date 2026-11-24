//! Watches, evaluated here (docs/NOTIFICATIONS_CONTRACT.md section 3, wave
//! 3).
//!
//! **Writes in.** The hosted Pimble server tells this service the writes a
//! signed-in person makes there to the documents someone watches
//! (`subscribeWrites`: which store and document, which account, which of
//! the watchers' shares reach it, gathered over a couple of seconds), and an
//! owner's device posts what its relay face saw
//! (`POST /api/v1/watches/writes`). Neither says anything of what was
//! written. The hosted server is told which documents those are
//! ([`interest_of`]: per store, the opaque ids registered there and the
//! roots of the shares their watchers hold the store through), again
//! whenever a store's change ([`InterestChanges`]), so it never hands this
//! service a write to a note nobody watches. [`record_writes`] gathers each
//! write for every person watching that document other than its writer,
//! who is still in the store through the whole store or a share that
//! reaches the document, and has watched notes on: one `WatchBatch` per
//! (watcher, document, writer), counted in spells of editing, kept in the
//! database so a deploy loses none.
//!
//! **Notices out.** [`watch_pass`] queues one notice for each batch that is
//! due (`pimble_core::watch::Window::NOTICE`: five quiet minutes, or an hour
//! after the first write), sealed to the watcher's notice key like any
//! notice, with no title or text: the store, the document, the writer and
//! how many spells of editing, once the watcher is found to hold still what
//! they were told through. The watcher's devices fill in the rest from the
//! notes they hold. Its push and email say "Joe edited a note you watch in
//! Garden". [`spawn`] runs the pass and follows the hosted server for the
//! life of the process; tests call [`watch_pass`] with a clock of their own.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use pimble_core::watch::Window;
use pimble_core::{AuthMethod, NodeId, StoreId};
use pimble_rpc::{Notice, NoticeKind, SetWriteInterestRequest, WriteInterest};

use crate::db::{GrantRow, InboxItemRow, UserRow, WatchBatchRow};
use crate::error::CloudResult;
use crate::state::AppState;

/// How often [`watch_pass`] looks for batches that are due.
pub const WATCH_PASS_EVERY: Duration = Duration::from_secs(30);
/// A write is dated when it was made, as its server says, but never later
/// than now nor more than this long before (a relay's report told late).
const LATEST_TOLD_LATE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// How long a relay report taken is remembered, so one sent again is not
/// gathered twice (`POST /api/v1/watches/writes`'s `report`).
pub const RELAY_REPORT_REMEMBERED_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// How often the relay reports older than that are forgotten.
const FORGET_REPORTS_EVERY: Duration = Duration::from_secs(60 * 60);
/// The first wait before a store whose watched notes could not be read is
/// read again, doubling to [`LONGEST_INTEREST_WAIT`].
const FIRST_INTEREST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_INTEREST_WAIT: Duration = Duration::from_secs(60);

/// One write reported, of `count` appends or edits.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Write {
    pub doc: String,
    pub writer: String,
    #[serde(default = "one")]
    pub count: u64,
    /// The shared roots whose scope reaches the document, as the server
    /// that saw the write judged them.
    #[serde(default)]
    pub roots: Vec<String>,
    /// When the last of these writes was made (Unix milliseconds; 0: now).
    #[serde(default)]
    pub last_ms: i64,
}

fn one() -> u64 {
    1
}

/// An id as this service writes it (a UUID, lowercase and hyphenated), or
/// the text as it came.
pub(crate) fn canonical(id: &str) -> String {
    uuid::Uuid::parse_str(id).map(|u| u.to_string()).unwrap_or_else(|_| id.to_string())
}

/// What a watcher holding `grants` may be told a write through, `reached`
/// the roots that reach the document: `""` with a whole-store grant, else
/// the first of their shared roots that reaches it; `None` when none does.
pub(crate) fn told_through(grants: &[GrantRow], reached: &BTreeSet<String>) -> Option<String> {
    if grants.iter().any(GrantRow::is_whole_store) {
        return Some(String::new());
    }
    let mut held: Vec<String> = grants.iter().map(|g| canonical(&g.root)).filter(|root| reached.contains(root)).collect();
    held.sort();
    held.into_iter().next()
}

fn watching(user: &UserRow) -> bool {
    !user.notifications_off && !user.notify_watches_off
}

/// The stores whose interest the hosted server must be told again: which
/// documents are watched there, or the shares their watchers hold, changed.
#[derive(Debug, Default)]
pub struct InterestChanges {
    dirty: Mutex<BTreeSet<String>>,
    wake: tokio::sync::Notify,
}

impl InterestChanges {
    /// `stores` changed (a registration, the switch, a member's new share).
    pub fn changed(&self, stores: impl IntoIterator<Item = String>) {
        let mut dirty = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
        let before = dirty.len();
        dirty.extend(stores);
        if dirty.len() > before {
            self.wake.notify_one();
        }
    }

    /// The stores changed since the last take, taken out (the follower
    /// tells the hosted server of them).
    pub fn take(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.dirty.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// What the hosted server is told to tell of `store_uuid`: the documents
/// registered there, and the roots of the shares their watchers hold it
/// through (none for a watcher holding the whole store). No documents for a
/// store it does not serve (relayed, deleted, unknown).
pub async fn interest_of(state: &AppState, store_uuid: &str) -> CloudResult<Option<WriteInterest>> {
    let Ok(store_id) = StoreId::parse(store_uuid) else { return Ok(None) };
    let mut interest = WriteInterest { store_id, docs: Vec::new(), roots: Vec::new() };
    let Some(store) = state.db.find_hosted_store(store_uuid).await? else { return Ok(Some(interest)) };
    if store.deleted || store.is_relayed() {
        return Ok(Some(interest));
    }
    let rows = state.db.watched_docs_in(store_uuid).await?;
    let docs: BTreeSet<String> = rows.iter().map(|r| canonical(&r.doc_uuid)).collect();
    let watchers: BTreeSet<&str> = rows.iter().map(|r| r.user_uuid.as_str()).collect();
    let mut roots = BTreeSet::new();
    for watcher in watchers {
        let Some(user) = state.db.find_user_by_uuid(watcher).await? else { continue };
        let grants = state.db.grants_for_user_and_store(user.rid, store.rid).await?;
        if grants.iter().any(GrantRow::is_whole_store) {
            continue;
        }
        roots.extend(grants.iter().map(|g| canonical(&g.root)));
    }
    interest.docs = docs.iter().filter_map(|d| NodeId::parse(d).ok()).collect();
    interest.roots = roots.iter().filter_map(|r| NodeId::parse(r).ok()).collect();
    Ok(Some(interest))
}

/// [`interest_of`] every store with documents registered: what the hosted
/// server is told whole when this service starts following it.
pub async fn all_interest(state: &AppState) -> CloudResult<Vec<WriteInterest>> {
    let stores: BTreeSet<String> = state.db.all_watched_docs().await?.into_iter().map(|d| d.store_uuid).collect();
    let mut all = Vec::new();
    for store in stores {
        if let Some(interest) = interest_of(state, &store).await? {
            if !interest.docs.is_empty() {
                all.push(interest);
            }
        }
    }
    Ok(all)
}

/// Gather `writes` to `store_uuid` at `now_ms` for everyone watching them.
/// Answers how many (watcher, write) pairs were gathered.
pub async fn record_writes(state: &AppState, store_uuid: &str, writes: &[Write], now_ms: i64) -> CloudResult<usize> {
    gather(state, store_uuid, writes.iter().map(|write| (None, write)), now_ms).await
}

/// [`record_writes`] for the relay report `report`, each write with its
/// place in the report as it was sent: a write of it that a batch gathered
/// already (an earlier sending that failed before the report was
/// remembered) is not gathered again, even once its notice was queued
/// (`WatchBatchRow::reports`), and one the earlier sending did not reach
/// is. Each write is judged by its place, not the report by its id, as one
/// report can name the same writer and document twice (two spells of
/// editing, the second after a gap).
pub async fn record_relay_writes(state: &AppState, store_uuid: &str, report: &str, writes: &[(usize, Write)], now_ms: i64) -> CloudResult<usize> {
    gather(state, store_uuid, writes.iter().map(|(place, write)| (Some(WatchBatchRow::report_write(report, *place)), write)), now_ms).await
}

async fn gather<'w>(state: &AppState, store_uuid: &str, writes: impl Iterator<Item = (Option<String>, &'w Write)>, now_ms: i64) -> CloudResult<usize> {
    let Some(store) = state.db.find_hosted_store(store_uuid).await? else { return Ok(0) };
    if store.deleted {
        return Ok(0);
    }
    let mut gathered = 0;
    for (named, write) in writes {
        let reached: BTreeSet<String> = write.roots.iter().map(|r| canonical(r)).collect();
        let at = if write.last_ms > 0 { write.last_ms.clamp(now_ms - LATEST_TOLD_LATE_MS, now_ms) } else { now_ms };
        for watched in state.db.watchers_of(store_uuid, &write.doc).await? {
            if watched.user_uuid == write.writer {
                continue;
            }
            let Some(watcher) = state.db.find_user_by_uuid(&watched.user_uuid).await? else { continue };
            if !watching(&watcher) {
                continue;
            }
            let grants = state.db.grants_for_user_and_store(watcher.rid, store.rid).await?;
            let Some(root) = told_through(&grants, &reached) else { continue };
            let _one_at_a_time = state.watches_lock.lock().await;
            if state.db.gather_watch_write(&watcher.user_uuid, store_uuid, &write.doc, &write.writer, &root, at, named.as_deref().map(|n| (n, now_ms))).await? {
                gathered += 1;
            }
        }
    }
    Ok(gathered)
}

/// One look over every batch as of `now_ms`: each due one becomes a notice
/// in its watcher's inbox (or nothing, when the watcher turned watches off,
/// no longer holds what they were told through, or has no notice key yet)
/// and is gone. Answers how many notices were queued.
pub async fn watch_pass(state: &AppState, now_ms: i64) -> CloudResult<usize> {
    // Taken out under the lock a write is gathered under, so a write lands
    // in a batch before it is taken or in a new one after, never in one
    // being taken. Taken (gone, or left with no spells while it names relay
    // reports) before it is told, so a pass that fails half way
    // never tells it twice.
    let due: Vec<WatchBatchRow> = {
        let _one_at_a_time = state.watches_lock.lock().await;
        let mut due = Vec::new();
        for batch in state.db.watch_batches().await? {
            if batch.gathered().is_some_and(|g| g.is_due(Window::NOTICE, now_ms)) {
                state.db.take_watch_batch(&batch).await?;
                due.push(batch);
            }
        }
        due
    };
    let mut queued = 0;
    for batch in &due {
        if queue_notice(state, batch).await? {
            queued += 1;
        }
    }
    Ok(queued)
}

async fn queue_notice(state: &AppState, batch: &WatchBatchRow) -> CloudResult<bool> {
    let Some(watcher) = state.db.find_user_by_uuid(&batch.watcher_uuid).await? else { return Ok(false) };
    if !watching(&watcher) || watcher.notice_public_key.is_empty() {
        return Ok(false);
    }
    let Some(store) = state.db.find_hosted_store(&batch.store_uuid).await? else { return Ok(false) };
    if store.deleted {
        return Ok(false);
    }
    // Still held as it was told: the whole store, or the share that reached
    // the document then (a member who left, or whose share moved, hears
    // nothing more).
    let grants = state.db.grants_for_user_and_store(watcher.rid, store.rid).await?;
    let root = if grants.iter().any(GrantRow::is_whole_store) {
        String::new()
    } else if !batch.root.is_empty() && grants.iter().any(|g| canonical(&g.root) == batch.root) {
        batch.root.clone()
    } else {
        return Ok(false);
    };
    let (Ok(store_id), Ok(node)) = (StoreId::parse(&batch.store_uuid), NodeId::parse(&batch.doc_uuid)) else { return Ok(false) };
    let item_uuid = uuid::Uuid::new_v4().to_string();
    // The names as the watcher knows them: the writer's in the watcher's
    // share, and the share's.
    let mut item = InboxItemRow {
        rid: 0,
        item_uuid: item_uuid.clone(),
        recipient_uuid: watcher.user_uuid.clone(),
        sender_uuid: batch.writer_uuid.clone(),
        store_uuid: batch.store_uuid.clone(),
        root: root.clone(),
        sealed: String::new(),
        created_at_ms: batch.last_at_ms,
        delivered: false,
        delivered_at_ms: 0,
        kind: NoticeKind::Watch.word().to_string(),
        emailed: false,
        count: batch.count,
    };
    let (from_name, _share) = crate::routes::inbox::names_for(state, &item).await?;
    let notice = Notice {
        id: item_uuid.clone(),
        kind: NoticeKind::Watch,
        from: batch.writer_uuid.clone(),
        from_name,
        store: store_id,
        node,
        discussion: None,
        anchor: None,
        snippet: String::new(),
        at: chrono::DateTime::from_timestamp_millis(batch.last_at_ms).unwrap_or_default().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        count: u32::try_from(batch.count).unwrap_or(u32::MAX),
    };
    let plain = serde_json::to_vec(&notice).map_err(|e| crate::error::CloudError::Internal(e.to_string()))?;
    let sealed = match pimble_crypto::seal_notice(&plain, &watcher.notice_public_key, &watcher.user_uuid) {
        Ok(sealed) => sealed,
        Err(e) => {
            tracing::warn!(error = %e, "a watch notice could not be sealed to its watcher's notice key");
            return Ok(false);
        }
    };
    item = state.db.create_watch_item_at(&item_uuid, &watcher.user_uuid, &batch.writer_uuid, &batch.store_uuid, &root, &sealed, batch.count, chrono::Utc::now().timestamp_millis()).await?;
    state.wake_inbox(&watcher.user_uuid);
    if state.vapid.is_some() && !watcher.notify_push_off {
        let state = state.clone();
        tokio::spawn(async move {
            crate::notify::push_item(&state, &watcher, &item).await;
        });
    }
    Ok(true)
}

/// Follow the hosted server's writes for as long as it runs, connecting
/// again (with backoff) whenever the connection ends: tell it whole which
/// documents are watched, then follow what it tells and tell it again of
/// each store whose watches change. A write made while this service is not
/// connected is not told: watches are best effort.
async fn follow_hosted_writes(state: AppState) {
    let auth = match &state.config.pimble_server_token {
        Some(token) => AuthMethod::Bearer { token: token.clone() },
        None => AuthMethod::None,
    };
    let mut backoff = Duration::from_secs(1);
    loop {
        match pimble_client::PimbleClient::connect_with_auth(&state.config.pimble_server_url, &auth).await {
            Ok(client) => {
                if follow(&state, &client).await {
                    backoff = Duration::from_secs(1);
                    tracing::info!("the hosted server's writes ended; following them again");
                }
            }
            Err(e) => tracing::debug!(error = %e, "the hosted server is not reachable for its writes"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// One connection's following; answers whether its writes were followed
/// and the hosted server told what is watched (so the connection was good).
async fn follow(state: &AppState, client: &pimble_client::PimbleClient) -> bool {
    // Following first, so no write between the two is missed.
    let writes = match client.subscribe_writes().await {
        Ok(writes) => writes,
        Err(e) => {
            tracing::warn!(error = %e, "the hosted server's writes could not be followed");
            return false;
        }
    };
    // Every change so far is in what is told whole.
    state.watch_interest.take();
    let told = match all_interest(state).await {
        Ok(stores) => client.set_write_interest(SetWriteInterestRequest { stores, whole: true }).await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = told {
        tracing::warn!(error = %e, "the hosted server could not be told which notes are watched");
        return false;
    }
    follow_writes(&Hosted { state, client }, writes, &state.watch_interest).await
}

/// What [`follow_writes`] does with what it hears, apart from the loop
/// itself: the hosted server and the database ([`Hosted`]), or a test's.
trait Following {
    /// Gather a write the hosted server told.
    async fn record(&self, seen: pimble_rpc::WriteSeen);
    /// What is watched in `store` now ([`interest_of`]).
    async fn interest(&self, store: &str) -> CloudResult<Option<WriteInterest>>;
    /// Tell the hosted server `stores`; whether it was told.
    async fn tell(&self, stores: Vec<WriteInterest>) -> bool;
}

struct Hosted<'a> {
    state: &'a AppState,
    client: &'a pimble_client::PimbleClient,
}

impl Following for Hosted<'_> {
    async fn record(&self, seen: pimble_rpc::WriteSeen) {
        let write = Write {
            doc: seen.doc_id.to_string(),
            writer: seen.writer,
            count: u64::from(seen.count),
            roots: seen.roots.iter().map(|r| r.to_string()).collect(),
            last_ms: seen.last_ms,
        };
        if let Err(e) = record_writes(self.state, &seen.store_id.to_string(), &[write], chrono::Utc::now().timestamp_millis()).await {
            tracing::warn!(error = %e, "a write was not gathered for watches");
        }
    }

    async fn interest(&self, store: &str) -> CloudResult<Option<WriteInterest>> {
        interest_of(self.state, store).await
    }

    async fn tell(&self, stores: Vec<WriteInterest>) -> bool {
        match self.client.set_write_interest(SetWriteInterestRequest { stores, whole: false }).await {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(error = %e, "the hosted server could not be told which notes are watched");
                false
            }
        }
    }
}

/// Follow `writes` until they end, telling the hosted server of each store
/// whose watches change (`changes`). A store whose watched notes could not
/// be read is read again at its one deadline ([`Unread::due`]): the writes
/// arriving meanwhile never put it off. Answers whether the connection was
/// good (false once telling failed).
async fn follow_writes<S, E>(following: &impl Following, mut writes: S, changes: &InterestChanges) -> bool
where
    S: futures::Stream<Item = Result<pimble_rpc::WriteSeen, E>> + Unpin,
{
    use futures::StreamExt;
    let mut unread = Unread::default();
    loop {
        // The deadline, not a wait from now: every turn of the loop sleeps
        // until the same moment.
        let due = unread.due;
        tokio::select! {
            seen = writes.next() => {
                let Some(Ok(seen)) = seen else { return true };
                following.record(seen).await;
            }
            _ = changes.wake.notified() => {
                let mut changed = changes.take();
                // A store waiting to be read again is read with the rest.
                changed.extend(unread.take());
                if !tell_interest(following, changed, &mut unread).await {
                    // Told whole on the next connection.
                    return true;
                }
            }
            _ = tokio::time::sleep_until(due.unwrap_or_else(tokio::time::Instant::now)), if due.is_some() => {
                let again = unread.take();
                if !tell_interest(following, again, &mut unread).await {
                    return true;
                }
            }
        }
    }
}

/// Tell the hosted server what is watched in `stores` now. A store whose
/// watched notes could not be read waits in `unread` to be read again, with
/// backoff (a database that fails at once would otherwise be asked again
/// at once, for ever). Answers whether the hosted server was told.
async fn tell_interest(following: &impl Following, stores: BTreeSet<String>, unread: &mut Unread) -> bool {
    let mut told = Vec::new();
    let mut failed = BTreeSet::new();
    for store in stores {
        match following.interest(&store).await {
            Ok(Some(interest)) => told.push(interest),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "a store's watched notes could not be read; reading them again later");
                failed.insert(store);
            }
        }
    }
    unread.failed(failed, tokio::time::Instant::now());
    if told.is_empty() {
        return true;
    }
    following.tell(told).await
}

/// The stores whose watched notes could not be read, and when to read them
/// again: [`FIRST_INTEREST_WAIT`] after the first read that fails, doubling
/// with each one after it that fails too, up to [`LONGEST_INTEREST_WAIT`];
/// a read that leaves nothing unread starts it over.
#[derive(Debug)]
struct Unread {
    stores: BTreeSet<String>,
    wait: Duration,
    failing: bool,
    /// When `stores` are read again; set when they start waiting and kept
    /// until they are taken, whatever else happens meanwhile.
    due: Option<tokio::time::Instant>,
}

impl Default for Unread {
    fn default() -> Self {
        Self { stores: BTreeSet::new(), wait: FIRST_INTEREST_WAIT, failing: false, due: None }
    }
}

impl Unread {
    /// The stores to read again now (they wait no more).
    fn take(&mut self) -> BTreeSet<String> {
        self.due = None;
        std::mem::take(&mut self.stores)
    }

    /// A read at `now` ended with `failed` unread: they wait.
    fn failed(&mut self, failed: BTreeSet<String>, now: tokio::time::Instant) {
        if failed.is_empty() {
            if self.stores.is_empty() {
                self.failing = false;
                self.wait = FIRST_INTEREST_WAIT;
            }
            return;
        }
        self.wait = if self.failing { (self.wait * 2).min(LONGEST_INTEREST_WAIT) } else { FIRST_INTEREST_WAIT };
        self.failing = true;
        self.stores.extend(failed);
        self.due = Some(now + self.wait);
    }
}

/// Watches' jobs, for the life of the process: following the hosted
/// server's writes, [`watch_pass`] every [`WATCH_PASS_EVERY`], and
/// forgetting the relay reports taken more than
/// [`RELAY_REPORT_REMEMBERED_MS`] ago every hour, both the reports and the
/// batches' record of them. Started
/// by `build_router` (the service), never by the tests.
pub fn spawn(state: AppState) {
    tokio::spawn(follow_hosted_writes(state.clone()));
    let forgetting = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(FORGET_REPORTS_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let before = chrono::Utc::now().timestamp_millis() - RELAY_REPORT_REMEMBERED_MS;
            if let Err(e) = forgetting.db.forget_relay_reports_before(before).await {
                tracing::warn!(error = %e, "the relay reports taken long ago could not be forgotten");
            }
            let _one_at_a_time = forgetting.watches_lock.lock().await;
            if let Err(e) = forgetting.db.forget_batch_reports_before(before).await {
                tracing::warn!(error = %e, "the relay reports batches gathered long ago could not be forgotten");
            }
        }
    });
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(WATCH_PASS_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = watch_pass(&state, chrono::Utc::now().timestamp_millis()).await {
                tracing::warn!(error = %e, "the watch pass failed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::Instant;

    fn stores(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_store_that_cannot_be_read_waits_longer_each_time() {
        let now = Instant::now();
        let mut unread = Unread::default();
        unread.failed(BTreeSet::new(), now);
        assert!(unread.stores.is_empty() && unread.due.is_none(), "nothing failed: nothing waits");
        unread.failed(stores(&["s"]), now);
        assert_eq!(unread.due, Some(now + FIRST_INTEREST_WAIT));
        assert_eq!((unread.wait, unread.take()), (FIRST_INTEREST_WAIT, stores(&["s"])));
        assert_eq!(unread.due, None, "taken: nothing is due");
        let mut waits = Vec::new();
        for _ in 0..8 {
            unread.failed(stores(&["s"]), now);
            waits.push(unread.wait.as_secs());
            unread.take();
        }
        assert_eq!(waits, vec![2, 4, 8, 16, 32, 60, 60, 60], "doubling, to a minute");
        unread.failed(BTreeSet::new(), now);
        unread.failed(stores(&["t"]), now);
        assert_eq!(unread.wait, FIRST_INTEREST_WAIT, "a read that worked starts it over");
    }

    /// A following whose reads of store "s" fail `failures` times, and that
    /// notes when each read and each telling happened.
    struct Fake {
        start: Instant,
        failures: AtomicUsize,
        reads: Mutex<Vec<Duration>>,
        told: Mutex<Vec<Duration>>,
        recorded: AtomicUsize,
    }

    impl Fake {
        fn failing(failures: usize) -> Fake {
            Fake { start: Instant::now(), failures: AtomicUsize::new(failures), reads: Mutex::default(), told: Mutex::default(), recorded: AtomicUsize::new(0) }
        }
    }

    impl Following for Fake {
        async fn record(&self, _seen: pimble_rpc::WriteSeen) {
            self.recorded.fetch_add(1, Ordering::SeqCst);
        }

        async fn interest(&self, _store: &str) -> CloudResult<Option<WriteInterest>> {
            self.reads.lock().unwrap().push(self.start.elapsed());
            if self.failures.load(Ordering::SeqCst) > 0 {
                self.failures.fetch_sub(1, Ordering::SeqCst);
                return Err(crate::error::CloudError::Internal("the database is away".into()));
            }
            Ok(Some(WriteInterest { store_id: StoreId::new(), docs: vec![NodeId::new()], roots: Vec::new() }))
        }

        async fn tell(&self, _stores: Vec<WriteInterest>) -> bool {
            self.told.lock().unwrap().push(self.start.elapsed());
            true
        }
    }

    /// A hosted write every 300 ms, for ever.
    fn writes_every_300ms() -> impl futures::Stream<Item = Result<pimble_rpc::WriteSeen, std::convert::Infallible>> + Unpin {
        Box::pin(futures::stream::unfold((), |()| async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let seen = pimble_rpc::WriteSeen { store_id: StoreId::new(), doc_id: NodeId::new(), writer: "olga".into(), count: 1, roots: Vec::new(), last_ms: 0 };
            Some((Ok(seen), ()))
        }))
    }

    /// The loop itself, under writes arriving faster than the wait: a store
    /// whose read failed is read again at its deadline (1 s, then 2 s after
    /// that), not put off by every write.
    #[tokio::test(start_paused = true)]
    async fn a_store_unread_is_read_again_while_writes_keep_coming() {
        let fake = Fake::failing(2);
        let changes = InterestChanges::default();
        changes.changed(["s".to_string()]);
        let followed = tokio::time::timeout(Duration::from_secs(5), follow_writes(&fake, writes_every_300ms(), &changes)).await;
        assert!(followed.is_err(), "the writes never end: still following at the end of the test");
        let secs = |all: &Mutex<Vec<Duration>>| all.lock().unwrap().iter().map(|d| d.as_millis()).collect::<Vec<_>>();
        assert_eq!(secs(&fake.reads), vec![0, 1000, 3000], "read at once, failing; again after 1 s, failing; again 2 s later");
        assert_eq!(secs(&fake.told), vec![3000], "told once the read worked");
        assert_eq!(fake.recorded.load(Ordering::SeqCst), 16, "every write gathered meanwhile");
    }

    /// With nothing unread, the loop never wakes for a read of its own.
    #[tokio::test(start_paused = true)]
    async fn nothing_unread_is_never_read_again() {
        let fake = Fake::failing(0);
        let changes = InterestChanges::default();
        changes.changed(["s".to_string()]);
        let _ = tokio::time::timeout(Duration::from_secs(5), follow_writes(&fake, writes_every_300ms(), &changes)).await;
        assert_eq!(fake.reads.lock().unwrap().len(), 1);
        assert_eq!(fake.told.lock().unwrap().len(), 1);
    }
}
