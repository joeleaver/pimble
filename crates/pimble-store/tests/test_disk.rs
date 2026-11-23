//! The suite's disk switch does what it says (docs/ONE_CODEBASE_CONTRACT.md
//! section 8): with `PIMBLE_TEST_DISK=memory` a store made the ordinary way
//! lives on the memory disk and nothing of it reaches the machine's disk;
//! without it, it is a directory as always.

use pimble_host::disk::testfs as tfs;
use pimble_host::DiskKind;
use pimble_store::{LocalStore, StoreManager};

#[tokio::test]
async fn a_store_lives_on_the_disk_the_suite_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.pimble");
    let store = LocalStore::create(&path, "S").await.unwrap();
    let expected = if tfs::is_memory() { DiskKind::Memory } else { DiskKind::Native };
    assert_eq!(store.disk().kind(), expected);
    assert_eq!(StoreManager::new().disk().kind(), expected);
    assert!(tfs::exists(path.join("manifest.json")));
    assert_eq!(path.join("manifest.json").exists(), !tfs::is_memory(), "the machine's disk holds the store exactly when the suite runs on it");
}
