//! The Pimble windows hearing `openInApp` (docs/SETTINGS_CONTRACT.md
//! section 12, docs/NOTIFICATIONS_CONTRACT.md "Built in wave 4"), and the
//! callers waiting for a window to say it took a link.
//!
//! A window listens through `subscribeAppLinks`; the one open longest takes
//! each link. A caller that asks with `confirm` hears `delivered` only when
//! the window answers `appLinkTaken` with the link's `ack`: a link merely
//! queued for a window that is hung, or partway through quitting, would
//! otherwise count as taken, and the launch that handed it would open
//! nothing.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pimble_rpc::HandedLink;
use tokio::sync::{mpsc, oneshot};

/// The windows listening, oldest first, and the confirmations awaited.
#[derive(Default)]
pub(crate) struct AppLinks {
    windows: Mutex<Vec<mpsc::UnboundedSender<HandedLink>>>,
    waiting: Mutex<HashMap<u64, oneshot::Sender<()>>>,
    next_ack: AtomicU64,
}

impl AppLinks {
    /// A window starts listening: the links handed to it arrive here.
    pub(crate) fn listen(&self) -> mpsc::UnboundedReceiver<HandedLink> {
        let (tx, rx) = mpsc::unbounded_channel();
        lock(&self.windows).push(tx);
        rx
    }

    /// Hand `url` to the oldest window still listening. Without `confirm`,
    /// whether one was there to queue it for; with it, whether that window
    /// said it took it within `wait` (`runtime` measures the wait).
    pub(crate) async fn hand(&self, runtime: &pimble_host::Runtime, url: String, activation_token: Option<String>, confirm: bool, wait: Duration) -> bool {
        let (ack, answer) = if confirm {
            let ack = self.next_ack.fetch_add(1, Ordering::Relaxed) + 1;
            let (tx, rx) = oneshot::channel();
            lock(&self.waiting).insert(ack, tx);
            (Some(ack), Some(rx))
        } else {
            (None, None)
        };
        let link = HandedLink { url, activation_token, ack };
        let queued = {
            let mut windows = lock(&self.windows);
            // Gone windows are dropped; the oldest still listening takes it.
            windows.retain(|w| !w.is_closed());
            windows.iter().any(|w| w.send(link.clone()).is_ok())
        };
        let (Some(ack), Some(answer)) = (ack, answer) else { return queued };
        let taken = queued && matches!(runtime.timeout(wait, answer).await, Ok(Ok(())));
        lock(&self.waiting).remove(&ack);
        taken
    }

    /// A window took the link handed with `ack`; whether anyone still waited
    /// for it (a confirmation after the wait ran out finds nobody).
    pub(crate) fn taken(&self, ack: u64) -> bool {
        lock(&self.waiting).remove(&ack).is_some_and(|tx| tx.send(()).is_ok())
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
