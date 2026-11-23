//! The browser worker's runtime (wasm32): one thread, so a task is a
//! `spawn_local` future, a heavy job runs where it is asked, timers are
//! `setTimeout` (gloo's, cleared when dropped) and the clocks are
//! `performance.now()` and `Date.now()` (`web-time`, which works in workers).

use std::cell::Cell;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures::future::{AbortHandle, Abortable};

use super::{BoxFuture, HeavyJob, Instant, RuntimeBackend, SleepFuture, Task, TaskHandle};

/// See the module documentation.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalRuntime;

impl RuntimeBackend for LocalRuntime {
    fn spawn(&self, task: BoxFuture<()>) -> Task {
        let (abort, registration) = AbortHandle::new_pair();
        let ended = Rc::new(Ended::default());
        let done = ended.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = Abortable::new(task, registration).await;
            done.finished.set(true);
            if let Some(waker) = done.waker.take() {
                waker.wake();
            }
        });
        Task::new(Handle { abort, ended })
    }

    fn spawn_heavy(&self, job: HeavyJob) {
        job();
    }

    fn sleep_until(&self, deadline: Instant) -> SleepFuture {
        let wait = deadline.duration_since(self.now());
        let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
        Box::pin(gloo_timers::future::TimeoutFuture::new(millis))
    }

    fn now(&self) -> Instant {
        // `web_time::Instant` is `performance.now()`; its origin is the
        // worker's time origin.
        thread_local! {
            static ORIGIN: web_time::Instant = web_time::Instant::now();
        }
        let since = ORIGIN.with(|origin| web_time::Instant::now().duration_since(*origin));
        Instant::from_origin(since)
    }

    fn wall(&self) -> SystemTime {
        let since_epoch = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or(Duration::ZERO);
        SystemTime::UNIX_EPOCH + since_epoch
    }
}

#[derive(Default)]
struct Ended {
    finished: Cell<bool>,
    waker: Cell<Option<std::task::Waker>>,
}

struct Handle {
    abort: AbortHandle,
    ended: Rc<Ended>,
}

// The worker has one thread: a task handle never leaves it.
unsafe impl Sync for Handle {}

impl TaskHandle for Handle {
    fn abort(&self) {
        self.abort.abort();
    }

    fn is_finished(&self) -> bool {
        self.ended.finished.get()
    }

    fn poll_ended(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.ended.finished.get() {
            return Poll::Ready(());
        }
        self.ended.waker.set(Some(cx.waker().clone()));
        Poll::Pending
    }
}
