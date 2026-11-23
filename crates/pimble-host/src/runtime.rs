//! A runtime (docs/ONE_CODEBASE_CONTRACT.md seam C): every task the server
//! starts, every timer it waits on and every clock it reads goes through one.
//!
//! [`RuntimeBackend`] is what a host implements, five small things: start a
//! task, run a heavy job off the async threads, wait until a moment, read the
//! monotonic clock, read the wall clock. [`Runtime`] is the cheap handle the
//! server holds and clones, with what every caller builds from those:
//! [`Runtime::sleep`], [`Runtime::timeout`], [`Runtime::interval`] (tokio's
//! `interval` with its default `Burst` behavior: a tick missed fires at once,
//! and the next is due a period after the one missed), [`Runtime::run_heavy`]
//! (a closure's answer, awaited) and [`TaskSet`] (tokio's `JoinSet`, as far
//! as the server used it).
//!
//! The hosts meant:
//!
//! - the desktop ([`TokioRuntime`]): `tokio::spawn`, `spawn_blocking`,
//!   `tokio::time`, all on whichever tokio runtime the caller is in, so the
//!   timings, debounces and task lifetimes are exactly the ones the server had
//!   before this seam;
//! - the browser's worker ([`LocalRuntime`], wasm32): `spawn_local`, a heavy
//!   job run inline (a worker has no threads to give it), `setTimeout`, and
//!   `performance.now()` (`std::time::Instant` panics there);
//! - a phone app hosting the server in-process: [`TokioRuntime`] on a
//!   background tokio runtime.
//!
//! `tokio::sync` (locks, channels, `broadcast`, `Notify`, `watch`) builds for
//! wasm32 and is used directly; it needs no runtime.

use std::fmt;
use std::future::Future;
use std::ops::{Add, AddAssign, Sub};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

#[cfg(target_arch = "wasm32")]
mod local;
#[cfg(not(target_arch = "wasm32"))]
mod tokio_rt;

#[cfg(target_arch = "wasm32")]
pub use local::LocalRuntime;
#[cfg(not(target_arch = "wasm32"))]
pub use tokio_rt::TokioRuntime;

/// `Send` on the desktop, where tasks move between threads; nothing in the
/// browser's worker, which has one thread and `!Send` JavaScript handles.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// A task's future, as a backend takes it.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
#[cfg(target_arch = "wasm32")]
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// A timer's future, as a backend answers it.
#[cfg(not(target_arch = "wasm32"))]
pub type SleepFuture = Pin<Box<dyn Future<Output = ()> + Send + Sync + 'static>>;
#[cfg(target_arch = "wasm32")]
pub type SleepFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// A heavy job, as a backend takes it.
#[cfg(not(target_arch = "wasm32"))]
pub type HeavyJob = Box<dyn FnOnce() + Send + 'static>;
#[cfg(target_arch = "wasm32")]
pub type HeavyJob = Box<dyn FnOnce() + 'static>;

/// A moment on a runtime's monotonic clock: the time since a fixed origin
/// (the process's first reading on the desktop, the worker's time origin in
/// the browser). Only compared with moments from the same process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(Duration);

impl Instant {
    /// The moment `since` after the clock's origin (a backend's reading).
    pub const fn from_origin(since: Duration) -> Self {
        Self(since)
    }

    /// How long after the clock's origin this moment is.
    pub const fn since_origin(self) -> Duration {
        self.0
    }

    /// How long after `earlier` this is; zero when `earlier` is later
    /// (`std::time::Instant::duration_since` since Rust 1.60).
    pub fn duration_since(self, earlier: Instant) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    pub fn saturating_duration_since(self, earlier: Instant) -> Duration {
        self.duration_since(earlier)
    }

    pub fn checked_duration_since(self, earlier: Instant) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    pub fn checked_add(self, d: Duration) -> Option<Instant> {
        self.0.checked_add(d).map(Instant)
    }

    pub fn checked_sub(self, d: Duration) -> Option<Instant> {
        self.0.checked_sub(d).map(Instant)
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, d: Duration) -> Instant {
        Instant(self.0 + d)
    }
}

impl AddAssign<Duration> for Instant {
    fn add_assign(&mut self, d: Duration) {
        self.0 += d;
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;
    fn sub(self, d: Duration) -> Instant {
        Instant(self.0.saturating_sub(d))
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;
    fn sub(self, earlier: Instant) -> Duration {
        self.duration_since(earlier)
    }
}

/// What a host provides. See the module documentation.
pub trait RuntimeBackend: Send + Sync + fmt::Debug {
    /// Start `task` beside the caller; it runs until it ends or is aborted.
    fn spawn(&self, task: BoxFuture<()>) -> Task;
    /// Run `job`, which blocks (hashing, decoding a picture, opening an
    /// index), where it holds up no other task: a thread on the desktop.
    /// The job reports its own answer; see [`Runtime::run_heavy`].
    fn spawn_heavy(&self, job: HeavyJob);
    /// A future ready at `deadline` (at once when it has passed).
    fn sleep_until(&self, deadline: Instant) -> SleepFuture;
    /// The monotonic clock.
    fn now(&self) -> Instant;
    /// The wall clock (`SystemTime::now`, which panics on wasm32 too).
    fn wall(&self) -> SystemTime;
}

/// What a backend's task handle does: [`Task`] wraps one.
pub trait TaskHandle: MaybeSend + Sync {
    /// Stop the task at its next await; its future is dropped.
    fn abort(&self);
    /// Whether the task has ended (finished or aborted).
    fn is_finished(&self) -> bool;
    /// Ready once the task has ended.
    fn poll_ended(&mut self, cx: &mut Context<'_>) -> Poll<()>;
}

/// A started task. Dropping it detaches the task (it keeps running), as
/// dropping tokio's `JoinHandle` does; awaiting it waits until it has ended,
/// finished or aborted.
pub struct Task(Box<dyn TaskHandle>);

// wasm32-unknown-unknown without the `atomics` target feature has one thread
// and no way to start another, so a task handle never crosses threads: the
// server's structs that hold one stay `Send` there as on the desktop
// (docs/ONE_CODEBASE_CONTRACT.md wave 3), as `local.rs`'s handle is `Sync`.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
unsafe impl Send for Task {}

impl Task {
    pub fn new(handle: impl TaskHandle + 'static) -> Self {
        Self(Box::new(handle))
    }

    pub fn abort(&self) {
        self.0.abort()
    }

    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task").field("finished", &self.is_finished()).finish()
    }
}

impl Future for Task {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.0.poll_ended(cx)
    }
}

/// A runtime: a cheap handle over a [`RuntimeBackend`], cloned by everything
/// that starts a task, waits or reads a clock. See the module documentation.
#[derive(Clone)]
pub struct Runtime(Arc<dyn RuntimeBackend>);

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Runtime").field(&self.0).finish()
    }
}

impl Default for Runtime {
    /// The target's own: [`TokioRuntime`] on the desktop, [`LocalRuntime`] on
    /// wasm32.
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::new(TokioRuntime)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Self::new(LocalRuntime)
        }
    }
}

/// [`Runtime::timeout`]'s error: the time ran out first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("deadline has elapsed")
    }
}

impl std::error::Error for Elapsed {}

/// [`Runtime::run_heavy`]'s error: the job ended without an answer (it
/// panicked, or its runtime shut down first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeavyFailed;

impl fmt::Display for HeavyFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a heavy job ended without an answer")
    }
}

impl std::error::Error for HeavyFailed {}

impl Runtime {
    pub fn new(backend: impl RuntimeBackend + 'static) -> Self {
        Self(Arc::new(backend))
    }

    pub fn from_arc(backend: Arc<dyn RuntimeBackend>) -> Self {
        Self(backend)
    }

    /// Start `future` as a task (`tokio::spawn`); what it answers is dropped.
    pub fn spawn<F>(&self, future: F) -> Task
    where
        F: Future + MaybeSend + 'static,
    {
        self.0.spawn(Box::pin(async move {
            let _ = future.await;
        }))
    }

    /// Run `job` where it blocks nothing else (`spawn_blocking` on the
    /// desktop, inline in the browser's worker) and answer what it returns.
    pub async fn run_heavy<T, F>(&self, job: F) -> Result<T, HeavyFailed>
    where
        T: MaybeSend + 'static,
        F: FnOnce() -> T + MaybeSend + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.0.spawn_heavy(Box::new(move || {
            let _ = tx.send(job());
        }));
        rx.await.map_err(|_| HeavyFailed)
    }

    /// The monotonic clock (`Instant::now`).
    pub fn now(&self) -> Instant {
        self.0.now()
    }

    /// How long since `earlier` (`Instant::elapsed`).
    pub fn elapsed(&self, earlier: Instant) -> Duration {
        self.now().duration_since(earlier)
    }

    /// The wall clock (`SystemTime::now`).
    pub fn wall(&self) -> SystemTime {
        self.0.wall()
    }

    /// Ready `duration` from now (`tokio::time::sleep`).
    pub fn sleep(&self, duration: Duration) -> Sleep {
        self.sleep_until(self.now() + duration)
    }

    /// Ready at `deadline` (`tokio::time::sleep_until`); it can be moved
    /// with [`Sleep::reset`].
    pub fn sleep_until(&self, deadline: Instant) -> Sleep {
        Sleep { runtime: self.clone(), deadline, timer: self.0.sleep_until(deadline) }
    }

    /// `future`'s answer, or [`Elapsed`] when `duration` passes first
    /// (`tokio::time::timeout`: the future is polled before the timer, so one
    /// already ready answers even with no time given).
    pub fn timeout<F: Future>(&self, duration: Duration, future: F) -> Timeout<F> {
        Timeout { future, timer: self.sleep(duration) }
    }

    /// Ticks every `period`, the first at once (`tokio::time::interval`).
    pub fn interval(&self, period: Duration) -> Interval {
        self.interval_at(self.now(), period)
    }

    /// Ticks every `period` from `start` (`tokio::time::interval_at`).
    pub fn interval_at(&self, start: Instant, period: Duration) -> Interval {
        assert!(period > Duration::ZERO, "an interval's period must be more than zero");
        Interval { runtime: self.clone(), period, timer: self.sleep_until(start) }
    }
}

/// A timer ([`Runtime::sleep`], [`Runtime::sleep_until`]): ready at its
/// deadline. `Unpin`, so a loop's `select!` can poll `&mut sleep`.
pub struct Sleep {
    runtime: Runtime,
    deadline: Instant,
    timer: SleepFuture,
}

impl Sleep {
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Ready at `deadline` instead (`tokio::time::Sleep::reset`).
    pub fn reset(&mut self, deadline: Instant) {
        self.deadline = deadline;
        self.timer = self.runtime.0.sleep_until(deadline);
    }

    /// Whether the deadline has passed.
    pub fn is_elapsed(&self) -> bool {
        self.runtime.now() >= self.deadline
    }
}

impl fmt::Debug for Sleep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sleep").field("deadline", &self.deadline).finish()
    }
}

impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.timer.as_mut().poll(cx)
    }
}

/// [`Runtime::timeout`]'s future.
pub struct Timeout<F> {
    future: F,
    timer: Sleep,
}

impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, Elapsed>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: `future` is structurally pinned and never moved out;
        // `timer` is `Unpin`.
        let this = unsafe { self.get_unchecked_mut() };
        let future = unsafe { Pin::new_unchecked(&mut this.future) };
        if let Poll::Ready(answer) = future.poll(cx) {
            return Poll::Ready(Ok(answer));
        }
        match Pin::new(&mut this.timer).poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(Elapsed)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A repeating timer ([`Runtime::interval`]). Each tick is due a period after
/// the one before it, so ticks missed while the caller was busy fire one
/// after another at once (tokio's `MissedTickBehavior::Burst`, its default).
/// [`Interval::tick`] is cancel-safe: a tick not taken stays due.
pub struct Interval {
    runtime: Runtime,
    period: Duration,
    timer: Sleep,
}

impl Interval {
    /// Wait for the next tick and answer when it was due.
    pub async fn tick(&mut self) -> Instant {
        std::future::poll_fn(|cx| self.poll_tick(cx)).await
    }

    pub fn poll_tick(&mut self, cx: &mut Context<'_>) -> Poll<Instant> {
        match Pin::new(&mut self.timer).poll(cx) {
            Poll::Ready(()) => {
                let due = self.timer.deadline();
                self.timer.reset(due + self.period);
                Poll::Ready(due)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    /// The next tick a period from now (`tokio::time::Interval::reset`).
    pub fn reset(&mut self) {
        let next = self.runtime.now() + self.period;
        self.timer.reset(next);
    }

    pub fn period(&self) -> Duration {
        self.period
    }
}

impl fmt::Debug for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Interval").field("period", &self.period).field("next", &self.timer.deadline).finish()
    }
}

/// Tasks started together and ended together (tokio's `JoinSet`, as far as
/// the server used one): each started task is kept until it is pruned or
/// joined.
#[derive(Debug, Default)]
pub struct TaskSet {
    tasks: Vec<Task>,
}

impl TaskSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start `future` on `runtime` and keep its task.
    pub fn spawn<F>(&mut self, runtime: &Runtime, future: F)
    where
        F: Future + MaybeSend + 'static,
    {
        self.tasks.push(runtime.spawn(future));
    }

    /// Forget the tasks that have ended, waiting for none.
    pub fn prune_finished(&mut self) {
        self.tasks.retain(|task| !task.is_finished());
    }

    pub fn abort_all(&self) {
        for task in &self.tasks {
            task.abort();
        }
    }

    /// Wait until every task kept has ended, and forget them.
    pub async fn join_all(&mut self) {
        for task in self.tasks.drain(..) {
            task.await;
        }
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn a_timeout_answers_a_ready_future_even_with_no_time() {
        let rt = Runtime::default();
        assert_eq!(rt.timeout(Duration::ZERO, async { 7 }).await, Ok(7));
        assert_eq!(rt.timeout(Duration::ZERO, std::future::pending::<()>()).await, Err(Elapsed));
        assert_eq!(rt.timeout(Duration::from_millis(20), std::future::pending::<()>()).await, Err(Elapsed));
    }

    #[tokio::test]
    async fn an_interval_ticks_at_once_then_every_period_and_bursts_when_behind() {
        let rt = Runtime::default();
        let period = Duration::from_millis(30);
        let mut ticks = rt.interval(period);
        let first = ticks.tick().await;
        let second = ticks.tick().await;
        assert_eq!(second - first, period);
        // Behind by more than two periods: the missed ticks fire at once.
        rt.sleep(period * 3).await;
        let before = rt.now();
        let third = ticks.tick().await;
        let fourth = ticks.tick().await;
        assert!(rt.now() - before < period, "missed ticks fire at once");
        assert_eq!(third, second + period);
        assert_eq!(fourth, third + period);
    }

    #[tokio::test]
    async fn a_sleep_moves_when_reset() {
        let rt = Runtime::default();
        let start = rt.now();
        let mut sleep = rt.sleep(Duration::from_secs(60));
        sleep.reset(rt.now() + Duration::from_millis(10));
        (&mut sleep).await;
        assert!(rt.now() - start < Duration::from_secs(5));
        assert!(sleep.is_elapsed());
    }

    #[tokio::test]
    async fn tasks_abort_finish_and_join() {
        let rt = Runtime::default();
        let ran = Arc::new(AtomicUsize::new(0));
        let r = ran.clone();
        let task = rt.spawn(async move {
            r.fetch_add(1, Ordering::SeqCst);
        });
        task.await;
        assert_eq!(ran.load(Ordering::SeqCst), 1);

        let forever = rt.spawn(std::future::pending::<()>());
        assert!(!forever.is_finished());
        forever.abort();
        let mut set = TaskSet::new();
        set.spawn(&rt, std::future::pending::<()>());
        set.spawn(&rt, async {});
        set.abort_all();
        set.join_all().await;
        assert!(set.is_empty());
        rt.timeout(Duration::from_secs(5), forever).await.expect("an aborted task ends");
    }

    #[tokio::test]
    async fn a_heavy_job_answers_and_a_panicking_one_fails() {
        let rt = Runtime::default();
        assert_eq!(rt.run_heavy(|| 2 + 2).await, Ok(4));
        assert_eq!(rt.run_heavy(|| -> u32 { panic!("on purpose") }).await, Err(HeavyFailed));
    }
}
