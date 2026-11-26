//! A whole replica whose account holds the store no more (removed from it,
//! or another account signed in on this device): its own root is recorded as
//! ended, exactly as a share's root is when that share ends
//! (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 1"). Everything in
//! it is under an ended root, the record survives a reopen, and the root that
//! the first pull adopts is the one that stays ended.

use pimble_core::{NodeId, StoreId};
use pimble_host::disk::testfs as tfs;
use pimble_store::LocalStore;
use tempfile::tempdir;

#[tokio::test]
async fn a_whole_replica_ends_at_its_own_root_and_keeps_the_record() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("whole.pimble");
    let placeholder = NodeId::new();
    let mut replica = LocalStore::create_replica(&path, StoreId::new(), "Notes", placeholder).await.unwrap();
    assert!(!replica.is_partial());
    assert!(!replica.set_ended_roots(vec![NodeId::new()]).await.unwrap(), "only its own root can end a whole store");
    assert!(replica.ended_roots().is_empty());

    assert!(replica.set_ended_roots(vec![placeholder]).await.unwrap());
    assert_eq!(replica.ended_roots(), &[placeholder]);
    assert!(!replica.is_partial(), "a whole replica stays whole");
    assert!(replica.under_ended_root(placeholder) && replica.under_ended_root(NodeId::new()), "all of it has ended");
    assert!(!replica.set_ended_roots(vec![placeholder]).await.unwrap(), "the same set again changes nothing");

    // The first pull's root replaces the placeholder: the one it has now is
    // the one that has ended.
    let adopted = NodeId::new();
    replica.set_root_node_id(adopted).await.unwrap();
    assert_eq!(replica.ended_roots(), &[adopted]);

    drop(replica);
    let mut reopened = LocalStore::open(&path).await.unwrap();
    assert_eq!(reopened.ended_roots(), &[adopted], "recorded in the manifest");
    assert!(!reopened.is_partial());

    // Held again: un-ended by leaving it out, and written so.
    assert!(reopened.set_ended_roots(Vec::new()).await.unwrap());
    assert!(!reopened.under_ended_root(adopted));
    assert!(!tfs::read_to_string(path.join("manifest.json")).unwrap().contains("ended_roots"));
}
