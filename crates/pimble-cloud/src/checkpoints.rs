//! Named places in a request where a test may hold it, so a race between two
//! requests overlaps by construction rather than by luck.
//!
//! The rules the per-key locks keep (`crate::locks`) are about what happens
//! when two requests interleave at one exact point: both past their count of
//! a store's owners, say. Two requests fired at once rarely interleave there,
//! so a test that only fires them proves little. A test arms a checkpoint
//! ([`Checkpoints::hold`]); the first request to reach it waits there until
//! the test releases it, and every later one passes straight through.
//!
//! Only builds with the `test-hooks` feature (which this crate's own tests
//! turn on through its dev-dependency on itself) can arm one. In every other
//! build [`Checkpoints`] is an empty struct and [`Checkpoints::reach`] does
//! nothing.

/// The places a request can be held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Checkpoint {
    /// A request that may take an owner away has counted the store's owners,
    /// inside the store's owner lock, and not yet written.
    OwnersCounted,
    /// An account deletion has listed the account's grants, before taking
    /// any store's owner lock.
    AccountGrantsListed,
}

#[cfg(not(feature = "test-hooks"))]
mod imp {
    use super::Checkpoint;

    /// Nothing can be held in this build.
    #[derive(Default)]
    pub struct Checkpoints;

    impl Checkpoints {
        #[inline(always)]
        pub async fn reach(&self, _at: Checkpoint) {}
    }
}

#[cfg(feature = "test-hooks")]
mod imp {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::sync::watch;

    use super::Checkpoint;

    /// The checkpoints a test has armed.
    #[derive(Default)]
    pub struct Checkpoints {
        armed: Mutex<HashMap<Checkpoint, Arc<Hold>>>,
    }

    /// One armed checkpoint: how many requests reached it, and whether the
    /// first may go on.
    pub struct Hold {
        arrivals: watch::Sender<usize>,
        released: watch::Sender<bool>,
    }

    impl Checkpoints {
        /// The first request to reach `at` from now on waits there until
        /// [`Hold::release`]; arming it again replaces the earlier hold.
        pub fn hold(&self, at: Checkpoint) -> Arc<Hold> {
            let hold = Arc::new(Hold { arrivals: watch::Sender::new(0), released: watch::Sender::new(false) });
            self.armed.lock().unwrap_or_else(|e| e.into_inner()).insert(at, Arc::clone(&hold));
            hold
        }

        /// Disarms `at`, letting any request held there go on.
        pub fn disarm(&self, at: Checkpoint) {
            if let Some(hold) = self.armed.lock().unwrap_or_else(|e| e.into_inner()).remove(&at) {
                hold.release();
            }
        }

        /// A request is at `at`: held there if it is the first since the
        /// checkpoint was armed, on its way otherwise.
        pub async fn reach(&self, at: Checkpoint) {
            let Some(hold) = self.armed.lock().unwrap_or_else(|e| e.into_inner()).get(&at).cloned() else {
                return;
            };
            let mut first = false;
            hold.arrivals.send_modify(|n| {
                *n += 1;
                first = *n == 1;
            });
            if first {
                let mut released = hold.released.subscribe();
                // The sender lives in `hold`, which this request holds.
                let _ = released.wait_for(|go| *go).await;
            }
        }
    }

    impl Hold {
        /// How many requests have reached the checkpoint.
        pub fn arrivals(&self) -> usize {
            *self.arrivals.borrow()
        }

        /// Waits until `count` requests have reached the checkpoint. The
        /// caller bounds the wait.
        pub async fn arrived(&self, count: usize) {
            let mut arrivals = self.arrivals.subscribe();
            let _ = arrivals.wait_for(|n| *n >= count).await;
        }

        /// The held request goes on.
        pub fn release(&self) {
            self.released.send_replace(true);
        }
    }
}

pub use imp::*;
