//! Waiting on what a server does in the background (a debounced flush and
//! its `modified_at` stamp, a link catching up, an index): on the condition,
//! never a fixed sleep. A loaded machine (a full suite at load average 90,
//! 2026-11-26) delays that work, never drops it, so a test waits for what it
//! looks for under a deadline only work that never comes runs into. Shared by
//! the harness here and `tests/relay/` (by path), each test crate its own copy.

use std::time::Duration;

use pimble_core::NodeId;
use pimble_rpc::StoreChangedNotification;

/// How long a test waits for a server's own background work to land: far
/// past any delay a loaded machine makes.
pub const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// Whether `cond` held within `timeout`, asked every 50 ms.
pub async fn wait_until<F, Fut>(timeout: Duration, mut cond: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait on `changes` (subscribed before the edits) for the server's own
/// `MetadataUpdated` of each of `nodes`: the `modified_at` stamp its flush
/// gives a person's content edit.
pub async fn wait_stamped(changes: &mut jsonrpsee::core::client::Subscription<StoreChangedNotification>, nodes: &[NodeId]) {
    let mut unstamped = nodes.to_vec();
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    while !unstamped.is_empty() {
        let heard = tokio::time::timeout_at(deadline, changes.next())
            .await
            .unwrap_or_else(|_| panic!("the flush stamped {unstamped:?} within {SETTLE_DEADLINE:?}"))
            .expect("the store's change subscription stays open")
            .expect("a store change notification");
        if let (pimble_rpc::StoreChangeKind::MetadataUpdated { node_id }, None) = (&heard.change_kind, &heard.source_client_id) {
            unstamped.retain(|n| n != node_id);
        }
    }
}
