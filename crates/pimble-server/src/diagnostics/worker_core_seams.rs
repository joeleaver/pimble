//! The seams the worker core's tests drive it through: Pimble Cloud's records
//! endpoint, which can hold a request, a disk that can hold a batch's write,
//! so that a test can say "off" (and "on") at exactly those moments, and a
//! runtime whose wall clock the test sets (and steps back).

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use std::sync::atomic::AtomicUsize;
use std::time::{Duration, SystemTime};

use pimble_host::http::{Http, HttpError, HttpRequest, HttpResponse};
use pimble_host::runtime::{BoxFuture, HeavyJob, SleepFuture, TokioRuntime};
use pimble_host::{Disk, DiskBackend, DiskKind, Entry, Instant, Metadata, RuntimeBackend, Task};
use serde_json::Value;
use tokio::sync::Notify;

/// Which requests the endpoint holds until the test lets them go.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// None: every request is answered at once.
    #[default]
    Nothing,
    /// The first request alone (a test that never lets it go has it stall).
    First,
}

/// Pimble Cloud's records endpoint: takes everything, keeps what it was sent
/// (the bearer and the records) as each request arrives, and holds the
/// requests [`Hold`] says until [`Service::release`] is notified.
#[derive(Debug, Default)]
pub struct Service {
    sent: Mutex<Vec<(Option<String>, Vec<Value>)>>,
    hold: Hold,
    held_one: AtomicBool,
    pub arrived: Notify,
    pub release: Notify,
}

impl Service {
    pub fn holding_first() -> Self {
        Service { hold: Hold::First, ..Service::default() }
    }

    /// Every request that arrived: its bearer and its records.
    pub fn sent(&self) -> Vec<(Option<String>, Vec<Value>)> {
        self.sent.lock().unwrap().clone()
    }

    /// The bearer of every request, in order.
    pub fn bearers(&self) -> Vec<Option<String>> {
        self.sent().into_iter().map(|(bearer, _)| bearer).collect()
    }

    /// Every record's `data.at`, across every request, in order.
    pub fn ats(&self) -> Vec<i64> {
        self.sent().iter().flat_map(|(_, records)| records).filter_map(|r| r["data"]["at"].as_i64()).collect()
    }
}

#[async_trait::async_trait]
impl Http for Service {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let body = String::from_utf8(request.body.expect("a body").bytes).unwrap();
        let lines = body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        self.sent.lock().unwrap().push((request.bearer, lines));
        if self.hold == Hold::First && !self.held_one.swap(true, Ordering::SeqCst) {
            self.arrived.notify_one();
            self.release.notified().await;
        }
        Ok(HttpResponse { status: 200, body: Vec::new() })
    }

    async fn websocket_close_code(&self, _url: &str, _token: &str) -> Option<u16> {
        None
    }
}

/// A memory disk whose next write of a record batch, once armed, waits
/// (with nothing written yet) until [`HeldDisk::release`] is notified, and
/// which refuses to remove a batch while [`HeldDisk::fail_removals`] says so.
#[derive(Debug)]
pub struct HeldDisk {
    below: Disk,
    armed: AtomicBool,
    failing_removals: AtomicBool,
    pub arrived: Notify,
    pub release: Notify,
}

impl HeldDisk {
    pub fn new() -> Self {
        HeldDisk { below: Disk::memory(), armed: AtomicBool::new(false), failing_removals: AtomicBool::new(false), arrived: Notify::new(), release: Notify::new() }
    }

    /// Hold the next batch written.
    pub fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// From now on, every removal of a batch fails (`on`) or works again.
    pub fn fail_removals(&self, on: bool) {
        self.failing_removals.store(on, Ordering::SeqCst);
    }
}

fn is_batch(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("records-"))
}

/// A `&'static HeldDisk` is the backend, so the test keeps its handle.
#[async_trait::async_trait]
impl DiskBackend for &'static HeldDisk {
    fn kind(&self) -> DiskKind {
        DiskKind::Other
    }
    async fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.below.read(p).await
    }
    async fn read_range(&self, p: &Path, o: u64, l: usize) -> io::Result<Vec<u8>> {
        self.below.read_range(p, o, l).await
    }
    async fn metadata(&self, p: &Path) -> io::Result<Metadata> {
        self.below.metadata(p).await
    }
    async fn list(&self, p: &Path) -> io::Result<Vec<Entry>> {
        self.below.list(p).await
    }
    async fn create_dir(&self, p: &Path) -> io::Result<()> {
        self.below.create_dir(p).await
    }
    async fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.below.create_dir_all(p).await
    }
    async fn write(&self, p: &Path, b: &[u8]) -> io::Result<()> {
        if is_batch(p) && self.armed.swap(false, Ordering::SeqCst) {
            self.arrived.notify_one();
            self.release.notified().await;
        }
        self.below.write(p, b).await
    }
    async fn write_private(&self, p: &Path, b: &[u8]) -> io::Result<()> {
        self.below.write(p, b).await
    }
    async fn append(&self, p: &Path, b: &[u8]) -> io::Result<()> {
        self.below.append(p, b).await
    }
    async fn write_at(&self, p: &Path, o: u64, b: &[u8]) -> io::Result<()> {
        self.below.write_at(p, o, b).await
    }
    async fn sync_file(&self, p: &Path) -> io::Result<bool> {
        self.below.sync_file(p).await
    }
    async fn sync_dir(&self, p: &Path) {
        self.below.sync_dir(p).await
    }
    async fn rename(&self, f: &Path, t: &Path) -> io::Result<()> {
        self.below.rename(f, t).await
    }
    async fn copy(&self, f: &Path, t: &Path) -> io::Result<u64> {
        self.below.copy(f, t).await
    }
    async fn remove_file(&self, p: &Path) -> io::Result<()> {
        if is_batch(p) && self.failing_removals.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "this batch cannot be removed"));
        }
        self.below.remove_file(p).await
    }
    async fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
        self.below.remove_dir_all(p).await
    }
}

/// The host's runtime with a wall clock the test sets: it reads what
/// [`SetClock::set`] last said, and counts how often it was read.
#[derive(Debug, Default)]
pub struct SetClock {
    millis: AtomicUsize,
    reads: AtomicUsize,
}

impl SetClock {
    /// The wall clock reads `millis` after the epoch from now on.
    pub fn set(&self, millis: usize) {
        self.millis.store(millis, Ordering::SeqCst);
    }

    /// How often the wall clock was read.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

/// A `&'static SetClock` is the backend, so the test keeps its handle.
impl RuntimeBackend for &'static SetClock {
    fn spawn(&self, task: BoxFuture<()>) -> Task {
        TokioRuntime.spawn(task)
    }
    fn spawn_heavy(&self, job: HeavyJob) {
        TokioRuntime.spawn_heavy(job)
    }
    fn sleep_until(&self, deadline: Instant) -> SleepFuture {
        TokioRuntime.sleep_until(deadline)
    }
    fn now(&self) -> Instant {
        TokioRuntime.now()
    }
    fn wall(&self) -> SystemTime {
        self.reads.fetch_add(1, Ordering::SeqCst);
        SystemTime::UNIX_EPOCH + Duration::from_millis(self.millis.load(Ordering::SeqCst) as u64)
    }
}
