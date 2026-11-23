//! The desktop's runtime: tokio, exactly as the server called it before the
//! seam. Every call goes to the tokio runtime the caller is in (each test's
//! own, the app's background one), never to one captured at construction.

use std::future::Future;
use std::sync::OnceLock;
use std::task::{Context, Poll};
use std::time::SystemTime;

use super::{BoxFuture, HeavyJob, Instant, RuntimeBackend, SleepFuture, Task, TaskHandle};

/// tokio's spawn, `spawn_blocking`, timers and clock. See the module
/// documentation.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioRuntime;

/// The clock's origin: the process's first reading. tokio's `Instant` is
/// `std::time::Instant` (the workspace never turns on tokio's `test-util`).
fn origin() -> tokio::time::Instant {
    static ORIGIN: OnceLock<tokio::time::Instant> = OnceLock::new();
    *ORIGIN.get_or_init(tokio::time::Instant::now)
}

fn to_tokio(at: Instant) -> tokio::time::Instant {
    origin() + at.since_origin()
}

impl RuntimeBackend for TokioRuntime {
    fn spawn(&self, task: BoxFuture<()>) -> Task {
        Task::new(Handle(tokio::spawn(task)))
    }

    fn spawn_heavy(&self, job: HeavyJob) {
        drop(tokio::task::spawn_blocking(job));
    }

    fn sleep_until(&self, deadline: Instant) -> SleepFuture {
        Box::pin(tokio::time::sleep_until(to_tokio(deadline)))
    }

    fn now(&self) -> Instant {
        Instant::from_origin(tokio::time::Instant::now().saturating_duration_since(origin()))
    }

    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }
}

struct Handle(tokio::task::JoinHandle<()>);

impl TaskHandle for Handle {
    fn abort(&self) {
        self.0.abort()
    }

    fn is_finished(&self) -> bool {
        self.0.is_finished()
    }

    fn poll_ended(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        std::pin::Pin::new(&mut self.0).poll(cx).map(|_| ())
    }
}
