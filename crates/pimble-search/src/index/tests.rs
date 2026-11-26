//! The index as a whole: opening, and letting go of its directory.

use super::*;

/// A second open of an index that is open is refused as `Locked`, the
/// one error the server waits on and never rebuilds over
/// (docs/STARTUP_CONTRACT.md); once the first lets go it opens.
#[test]
fn a_held_index_is_locked_until_let_go() {
    let dir = tempfile::tempdir().unwrap();
    let first = SearchIndex::open(dir.path(), false).unwrap();
    let second = SearchIndex::open(dir.path(), false);
    assert!(matches!(&second, Err(e) if e.is_locked()), "{:?}", second.err());
    drop(first);
    assert!(SearchIndex::open(dir.path(), false).is_ok());
}

/// Dropping a semantic index lets go of its directory at once, even
/// while the process's embedding worker holds the index's queue (it
/// upgrades every index's queue for a whole round of batches, model
/// loading included): `closeStore` promises the lock is free when it
/// answers. Until 2026-11-24 the queue's sink kept the vector index, and
/// through it the database's storage, alive until the worker let go, and
/// a reopen right after a close found the directory locked by its own
/// process (CI, `a_locked_index_is_waited_for_and_never_rebuilt`).
#[cfg(feature = "semantic")]
#[test]
fn a_dropped_index_lets_go_while_the_worker_holds_its_queue() {
    let dir = tempfile::tempdir().unwrap();
    let index = SearchIndex::open(dir.path(), true).unwrap();
    let held_by_worker = Arc::clone(index.pending.as_ref().expect("a semantic index has a queue"));
    drop(index);
    let reopened = SearchIndex::open(dir.path(), true);
    assert!(reopened.is_ok(), "{:?}", reopened.err());
    drop(held_by_worker);
}
