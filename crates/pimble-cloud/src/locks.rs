//! Locks held per key: one store's owners, one account's password material.
//!
//! The managed RhypeDB has no transaction that spans a read and a write, so a
//! rule like "a store keeps at least one owner" or "a password change writes
//! over what it read" is one step only when every request that could break it
//! takes the same lock for that key first, and reads again inside it. This
//! service is one process (docs/DEPLOY.md), so a lock in its memory is enough.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::OwnedMutexGuard;

/// One `tokio` mutex per key, made on first use and dropped once nobody holds
/// or waits for it, so the map stays the size of the work in flight.
#[derive(Default)]
pub struct KeyedLocks {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl KeyedLocks {
    /// Waits for `key`'s lock and holds it until the guard drops. Requests
    /// for other keys are never held up by it.
    pub async fn lock(&self, key: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
            // A lock only the map holds has nobody holding or waiting for it.
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            Arc::clone(locks.entry(key.to_string()).or_default())
        };
        lock.lock_owned().await
    }

    /// How many keys have a lock held or waited for; for tests.
    pub fn in_use(&self) -> usize {
        self.locks.lock().unwrap_or_else(|e| e.into_inner()).values().filter(|lock| Arc::strong_count(lock) > 1).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn one_key_waits_and_another_does_not() {
        let locks = Arc::new(KeyedLocks::default());
        let held = locks.lock("a").await;
        let other = tokio::time::timeout(Duration::from_secs(5), locks.lock("b")).await;
        assert!(other.is_ok(), "another key is not held up");
        drop(other);
        let waiting = {
            let locks = Arc::clone(&locks);
            tokio::spawn(async move { locks.lock("a").await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the same key waits for the holder");
        drop(held);
        let guard = tokio::time::timeout(Duration::from_secs(5), waiting).await.expect("the waiter gets the lock").unwrap();
        drop(guard);
        let _ = locks.lock("c").await;
        assert_eq!(locks.in_use(), 0, "nothing held, nothing kept");
        assert!(locks.locks.lock().unwrap().len() <= 1, "unused locks are dropped from the map");
    }
}
