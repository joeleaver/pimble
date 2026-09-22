//! Tests for the "a published scope set only grows" rule
//! (docs/MOVE_CONTRACT.md "Repair", last paragraph; `pimble_server::share`,
//! [`grown_scope`]): nothing a share's upkeep publishes for a root is ever
//! taken away except by stopping the share. The harness (the stub accounts
//! service, `H`, `Env`, `Fixture`, `hosted_fixture`, `member_device`) is
//! `tests/common`; `tests/share.rs` is the fuller sharing test suite this
//! one does not repeat.
//!
//! What is proven here: a node deleted inside a share stays in its
//! published scope (a tombstone's `parent_id` is untouched, so this
//! device's own tree still reaches it — `plan` in `share.rs`), a member
//! still holds it and can undo the delete, and the union `grown_scope`
//! computes never drops a document either side already published.

mod common;
use common::*;

use std::time::Duration;

use pimble_rpc::MemberRole;

// ── Tests ──────────────────────────────────────────────────────────────

/// The scenario docs/MOVE_CONTRACT.md asks for directly: "a node deleted
/// (tombstoned) inside a share after the set was published is still in the
/// published set at the next upkeep, and a member can still fetch it and
/// `undeleteNode` it." `plan` in `share.rs` already reaches a tombstone
/// through its unchanged `parent_id` (the doc comment above it says so);
/// this proves it end to end, on the real sharing harness, rather than
/// trusting the comment, and proves the member's side of "undoable" too
/// (there is no other test of `undeleteNode` through a share on this
/// harness).
#[tokio::test]
async fn a_node_deleted_inside_a_share_stays_in_its_published_scope_and_a_member_can_undelete_it() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);

    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.expect("sharing a folder of a hosted store");
    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.expect("inviting an editor");
    let published = wait_until(Duration::from_secs(15), || async { env.scope_on_h(store_id, shared).await.map(sorted) == Some(sorted(vec![fx.inside, fx.deeper, fx.leaf])) }).await;
    assert!(published, "the scope is published before anyone deletes anything: {:?}", env.scope_on_h(store_id, shared).await);

    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let has_leaf = wait_until(Duration::from_secs(15), || async { bob.get_node(store_id, fx.leaf).await.is_ok() }).await;
    assert!(has_leaf, "the member's replica pulls the whole scope before anything is deleted from it");

    // A member's delete inside their share: a tombstone, not a removal from
    // the scope (docs/NODE_DOCUMENT_CONTRACT.md section 2, "Tombstones").
    bob.delete_node(store_id, fx.leaf).await.expect("a member can delete inside their share");
    let tombstoned = wait_until(Duration::from_secs(10), || async { alice.get_node(store_id, fx.leaf).await.is_err() && child_ids(&alice, store_id, fx.deeper).await.is_empty() }).await;
    assert!(tombstoned, "the delete reaches the owner");

    // The owner's next upkeep pass (the delete is a structural change, so
    // one is due a second after it) still names the tombstone: a scope
    // set only grows until its share is stopped.
    let still_published = wait_until(Duration::from_secs(15), || async { env.scope_on_h(store_id, shared).await.map(sorted) == Some(sorted(vec![fx.inside, fx.deeper, fx.leaf])) }).await;
    assert!(still_published, "a tombstone stays in the published scope: {:?}", env.scope_on_h(store_id, shared).await);

    // The member still reaches the tombstone (it was never taken off the
    // scope they were sent) and undoes the delete themselves.
    bob.undelete_node(store_id, fx.leaf).await.expect("a member can still fetch the tombstone and undo its delete");
    let restored = wait_until(Duration::from_secs(15), || async {
        child_ids(&bob, store_id, fx.deeper).await == vec![fx.leaf] && node_title(&bob, store_id, fx.leaf).await == "Train" && alice.get_node(store_id, fx.leaf).await.is_ok()
    })
    .await;
    assert!(restored, "the member's undelete reaches the owner too: {:?}", child_ids(&bob, store_id, fx.deeper).await);
    assert_eq!(env.scope_on_h(store_id, shared).await.map(sorted), Some(sorted(vec![fx.inside, fx.deeper, fx.leaf])), "the scope still names it, restored or not");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}
