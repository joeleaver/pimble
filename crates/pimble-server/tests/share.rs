//! End-to-end tests for the owner's half of sharing
//! (docs/NODE_DOCUMENT_CONTRACT.md section 5; `pimble_server::share`): the
//! `cloudShare*` RPCs and the share upkeep an owner's device runs (scope
//! sets, data-key wraps, the key sweep).
//!
//! The environment is `tests/common` (grown from `tests/vault_link.rs`'s):
//! `H`, a real `PimbleServer` in JWT mode holding the hosted `Vault` twins;
//! an axum stub standing in for the accounts service with the scoped
//! members, invitations and keys endpoints (`pimble-cloud`'s README,
//! "Sharing"); and real "desktop" `PimbleServer`s, one per device, each
//! with its own temp keystore and replicas directory. Three accounts to
//! start with (the owner, two to share with), and a fourth that appears
//! later, for an invitation sent before its address had an account.
//!
//! What the tests hold the owner's side to, above all: it hands over keys,
//! publishes scopes and hosts, and is never in the path of anyone's edit.
//! Two members edit the same shared folder's tree with the owner's server
//! stopped and see each other.

mod common;
use common::*;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreAccess, StoreKind, SyncState};
use pimble_crdt::NodeDoc;
use pimble_crypto::{derive_password_keys, wrap_account_keys, AccountKeys};
use pimble_rpc::{EditOperation, MemberRole, ShareMemberStatus, StoreChangeKind, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;
use uuid::Uuid;

// ── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn sharing_a_folder_gives_a_member_its_subtree_and_both_sides_edit_the_tree() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().expect("hosting made a store key");
    let mut alice_changes = alice.subscribe_store_changes(store_id).await.unwrap();

    // Share the folder: the owner is the only member, the marker is on the
    // node, the scope is published, and every document under it (and none
    // outside it) is wrapped under the share's key beside the store's.
    let answer = alice.cloud_share_node(store_id, shared, "Holiday Plans").await.expect("sharing a folder of a hosted store");
    assert_eq!(answer.share.name, "Holiday Plans");
    assert_eq!((answer.share.store_id, answer.share.node_id), (store_id, shared));
    assert!(matches!(answer.share.state, SyncState::Synced { .. }), "the wraps and the scope are in place when the call answers: {:?}", answer.share.state);
    assert_eq!(answer.members.iter().map(|m| (m.email.as_str(), m.role, m.status)).collect::<Vec<_>>(), vec![(ALICE.0, MemberRole::Owner, ShareMemberStatus::Active)]);

    let marker = alice.get_node(store_id, shared).await.unwrap().metadata.share().expect("the shared node carries the marker");
    assert_eq!((marker.name.as_str(), marker.url.as_str()), ("Holiday Plans", env.stub_url.as_str()));
    assert!(scope_keys_in(a_dir.path(), store_id).iter().any(|(id, _)| *id == marker.key_id), "the share key is in the keystore");
    assert!(env.has_share_key(store_id, shared, ALICE.0), "and wrapped to the owner's own account, for their other devices");
    assert_eq!(sorted(env.scope_on_h(store_id, shared).await.expect("a published scope")), sorted(vec![fx.inside, fx.deeper, fx.leaf]));
    for doc in [shared, fx.inside, fx.deeper, fx.leaf] {
        assert_eq!(sorted_uuids(env.wrap_key_ids(store_id, doc).await), sorted_uuids(vec![store_key_id, marker.key_id]), "{doc}");
    }
    for doc in [fx.root_id, fx.private] {
        assert_eq!(env.wrap_key_ids(store_id, doc).await, vec![store_key_id], "a document outside the share is not wrapped under its key");
    }
    let told = tokio::time::timeout(Duration::from_secs(5), async {
        let mut states = Vec::new();
        while let Some(Ok(notif)) = alice_changes.next().await {
            let StoreChangeKind::ShareStateChanged { node_id, state } = notif.change_kind else { continue };
            assert_eq!(node_id, shared);
            let synced = matches!(state, SyncState::Synced { .. });
            states.push(state);
            if synced {
                break;
            }
        }
        states
    })
    .await
    .expect("the share's state reaches the store's subscribers");
    assert_eq!(told.len(), 1, "transitions only, and a share that was kept up at once was never anything else: {told:?}");
    assert_eq!(alice.cloud_share_node(store_id, shared, "Again").await.expect_err("shared already").to_string(), "This node is shared already.");

    // Invite Bob, who has an account: the grant, and the key at once.
    let answer = alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.expect("inviting an editor");
    assert_eq!(
        answer.members.iter().map(|m| (m.email.as_str(), m.role, m.status)).collect::<Vec<_>>(),
        vec![(ALICE.0, MemberRole::Owner, ShareMemberStatus::Active), (BOB.0, MemberRole::Editor, ShareMemberStatus::Active)]
    );
    assert!(env.has_share_key(store_id, shared, BOB.0));
    let refused = alice.cloud_share_invite(store_id, shared, CAROL.0, MemberRole::Owner).await.expect_err("owner is no role of a share");
    assert!(refused.to_string().contains("editors or readers"), "{refused}");
    assert_eq!(env.scoped_rows(store_id, shared), 1, "and it changed nothing");

    // Bob's replica: the folder's subtree and nothing else.
    let (mut b, bob, b_dir) = start_local_server().await;
    env.sign_in(&bob, BOB).await;
    let rows = bob.cloud_list_hosted_stores().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].root, rows[0].shared_by.as_deref(), rows[0].name.as_str(), rows[0].role.as_str()), (Some(shared), Some(ALICE.0), "Holiday Plans", "editor"));
    let store = bob.cloud_add_hosted_store(store_id).await.expect("a share is added like any hosted store");
    assert_eq!((store.roots.clone(), store.access, store.name.as_str()), (vec![shared], StoreAccess::Full, "Holiday Plans"));
    let pulled = wait_until(Duration::from_secs(15), || async { node_text(&bob, store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "the scope's documents are pulled and read with the share's key");
    assert_eq!(child_ids(&bob, store_id, shared).await, vec![fx.inside, fx.deeper]);
    assert_eq!(child_ids(&bob, store_id, fx.deeper).await, vec![fx.leaf]);
    for outside in [fx.private, fx.root_id] {
        assert!(bob.get_node(store_id, outside).await.is_err(), "a node outside the scope is not held");
    }
    assert!(!contains_bytes_recursive(b_dir.path(), b"PRIVATE-DIARY-TEXT"));
    assert!(!contains_bytes_recursive(b_dir.path(), b"Alice's Notes"), "the owner's store name never reaches a recipient");

    // The member edits the tree and the owner's store follows: create,
    // rename, move (inside the share), delete.
    let made = bob.create_node(store_id, Some(fx.deeper), "document", "Ferry").await.unwrap();
    seed_content(&bob, store_id, made, "bob", "ferry at nine").await;
    let created = wait_until(Duration::from_secs(20), || async {
        child_ids(&alice, store_id, fx.deeper).await == vec![fx.leaf, made] && node_text(&alice, store_id, made).await.contains("ferry at nine")
    })
    .await;
    assert!(created, "a member's create reaches the owner, readable");

    // A rename is one append, the member's: the owner's device receives it
    // and writes nothing to the document. Keys and scope are all its upkeep does.
    let head_before = env.head_on_h(store_id, fx.inside).await;
    rename(&bob, store_id, fx.inside, "Packing list").await;
    let renamed = wait_until(Duration::from_secs(10), || async { node_title(&alice, store_id, fx.inside).await == "Packing list" }).await;
    assert!(renamed, "a member's rename reaches the owner");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(env.head_on_h(store_id, fx.inside).await, head_before + 1, "the owner's device is not in the path of the member's edit");

    bob.move_node(store_id, made, shared, None).await.expect("a move inside the share");
    let moved = wait_until(Duration::from_secs(10), || async {
        child_ids(&alice, store_id, shared).await == vec![fx.inside, fx.deeper, made] && child_ids(&alice, store_id, fx.deeper).await == vec![fx.leaf]
    })
    .await;
    assert!(moved, "a member's move reaches the owner: {:?}", child_ids(&alice, store_id, shared).await);
    bob.delete_node(store_id, fx.leaf).await.expect("a delete inside the share");
    let deleted = wait_until(Duration::from_secs(10), || async { child_ids(&alice, store_id, fx.deeper).await.is_empty() && alice.get_node(store_id, fx.leaf).await.is_err() }).await;
    assert!(deleted, "a member's delete reaches the owner");
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private], "and nothing a member did touched the rest of the owner's tree");

    // What the owner's upkeep did about the member's document: the store
    // key's wrap (the member could only wrap under the share's), and a
    // scope that names it, and keeps naming the deleted one (a tombstone
    // has to reach every member).
    let kept_up = wait_until(Duration::from_secs(15), || async {
        sorted_uuids(env.wrap_key_ids(store_id, made).await) == sorted_uuids(vec![store_key_id, marker.key_id])
            && env.scope_on_h(store_id, shared).await.map(sorted) == Some(sorted(vec![fx.inside, fx.deeper, fx.leaf, made]))
    })
    .await;
    assert!(kept_up, "wraps {:?}, scope {:?}", env.wrap_key_ids(store_id, made).await, env.scope_on_h(store_id, shared).await);

    // And the reverse: the owner creates, renames, moves and deletes, and
    // the member's replica follows.
    let hotel = alice.create_node(store_id, Some(shared), "document", "Hotel").await.unwrap();
    seed_content(&alice, store_id, hotel, "alice", "two nights").await;
    let created = wait_until(Duration::from_secs(30), || async { node_text(&bob, store_id, hotel).await.contains("two nights") }).await;
    assert!(created, "the owner's create reaches the member once the scope names it");
    rename(&alice, store_id, fx.deeper, "Travel").await;
    alice.move_node(store_id, hotel, fx.deeper, None).await.unwrap();
    alice.delete_node(store_id, made).await.unwrap();
    let followed = wait_until(Duration::from_secs(15), || async {
        node_title(&bob, store_id, fx.deeper).await == "Travel"
            && child_ids(&bob, store_id, fx.deeper).await == vec![hotel]
            && child_ids(&bob, store_id, shared).await == vec![fx.inside, fx.deeper]
    })
    .await;
    assert!(followed, "shared: {:?}, deeper: {:?}", child_ids(&bob, store_id, shared).await, child_ids(&bob, store_id, fx.deeper).await);
    assert_eq!(child_ids(&alice, store_id, shared).await, vec![fx.inside, fx.deeper]);
    assert!(!env.hosted_store_contains_plaintext(store_id, "ferry at nine"));
    assert!(!contains_bytes_recursive(b_dir.path(), b"PRIVATE-DIARY-TEXT"));

    // `cloudShareInfo` and removing a member, by address.
    let info = alice.cloud_share_info(store_id, shared).await.unwrap();
    assert_eq!(info.members.len(), 2);
    let after = alice.cloud_share_remove_member(store_id, shared, BOB.0).await.expect("removing a member");
    assert_eq!(after.members.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(), vec![ALICE.0]);
    assert!(!env.has_share_key(store_id, shared, BOB.0), "the service stops handing them the key");
    let nobody = alice.cloud_share_remove_member(store_id, shared, BOB.0).await.expect_err("not a member any more").to_string();
    assert!(nobody.contains("is not a member of this share"), "{nobody}");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

fn sorted_uuids(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

#[tokio::test]
async fn two_members_edit_the_same_folder_with_the_owners_server_stopped() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, shared, CAROL.0, MemberRole::Editor).await.unwrap();
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;

    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (mut c, carol, _c_dir) = member_device(&env, CAROL, &fx).await;

    // Every device of the owner's is off from here on.
    drop(alice);
    a.stop().await.unwrap();
    let scope_while_off = env.scope_on_h(store_id, shared).await;

    // Create: Bob's new document reaches Carol, readable.
    let ferry = bob.create_node(store_id, Some(shared), "document", "Ferry").await.unwrap();
    seed_content(&bob, store_id, ferry, "bob", "ferry at nine").await;
    let seen = wait_until(Duration::from_secs(20), || async {
        child_ids(&carol, store_id, shared).await == vec![fx.inside, fx.deeper, ferry] && node_text(&carol, store_id, ferry).await.contains("ferry at nine")
    })
    .await;
    assert!(seen, "a member's create reaches another member with no owner device on: {:?}", child_ids(&carol, store_id, shared).await);

    // Rename, by the other one.
    rename(&carol, store_id, ferry, "Night ferry").await;
    assert!(wait_until(Duration::from_secs(10), || async { node_title(&bob, store_id, ferry).await == "Night ferry" }).await, "a rename");

    // Move inside the share, and an edit to what was moved.
    carol.move_node(store_id, ferry, fx.deeper, Some(0)).await.unwrap();
    seed_content(&carol, store_id, ferry, "carol", "cabin booked").await;
    let moved = wait_until(Duration::from_secs(10), || async {
        child_ids(&bob, store_id, fx.deeper).await == vec![ferry, fx.leaf]
            && child_ids(&bob, store_id, shared).await == vec![fx.inside, fx.deeper]
            && node_text(&bob, store_id, ferry).await.contains("cabin booked")
    })
    .await;
    assert!(moved, "a move: {:?}", child_ids(&bob, store_id, fx.deeper).await);

    // Delete.
    bob.delete_node(store_id, fx.leaf).await.unwrap();
    let deleted = wait_until(Duration::from_secs(10), || async { child_ids(&carol, store_id, fx.deeper).await == vec![ferry] && carol.get_node(store_id, fx.leaf).await.is_err() }).await;
    assert!(deleted, "a delete");

    // Both at once, in the same folder.
    let from_bob = bob.create_node(store_id, Some(fx.deeper), "document", "Bus").await.unwrap();
    let from_carol = carol.create_node(store_id, Some(fx.deeper), "document", "Tram").await.unwrap();
    let converged = wait_until(Duration::from_secs(20), || async {
        let (at_bob, at_carol) = (child_ids(&bob, store_id, fx.deeper).await, child_ids(&carol, store_id, fx.deeper).await);
        at_bob == at_carol && sorted(at_bob) == sorted(vec![ferry, from_bob, from_carol])
    })
    .await;
    assert!(converged, "bob: {:?}, carol: {:?}", child_ids(&bob, store_id, fx.deeper).await, child_ids(&carol, store_id, fx.deeper).await);
    assert_eq!(env.scope_on_h(store_id, shared).await.map(|docs| docs.len()), scope_while_off.map(|docs| docs.len() + 3), "the hosted server put the members' documents in the scope itself");
    assert_eq!(env.wrap_key_ids(store_id, ferry).await, vec![share_key_id], "wrapped under the one scope key its maker holds");

    // The owner's device comes back and converges to the same tree; its
    // upkeep wraps what the members made under the store key and confirms
    // the scope.
    let (mut a, alice) = start_device_in(a_dir.path()).await;
    alice.open_store(a_dir.path().join("a.pimble")).await.expect("the owner's store reopens");
    let caught_up = wait_until(Duration::from_secs(30), || async {
        child_ids(&alice, store_id, fx.deeper).await == child_ids(&bob, store_id, fx.deeper).await
            && child_ids(&alice, store_id, shared).await == vec![fx.inside, fx.deeper]
            && node_text(&alice, store_id, ferry).await.contains("cabin booked")
            && node_title(&alice, store_id, ferry).await == "Night ferry"
    })
    .await;
    assert!(caught_up, "the owner's store follows what the members did while it was off: {:?}", child_ids(&alice, store_id, fx.deeper).await);
    let kept_up = wait_until(Duration::from_secs(20), || async {
        let mut all = true;
        for doc in [ferry, from_bob, from_carol] {
            all &= sorted_uuids(env.wrap_key_ids(store_id, doc).await) == sorted_uuids(vec![store_key_id, share_key_id]);
        }
        all && env.scope_on_h(store_id, shared).await.map(sorted) == Some(sorted(vec![fx.inside, fx.deeper, fx.leaf, ferry, from_bob, from_carol]))
    })
    .await;
    assert!(kept_up, "scope {:?}", env.scope_on_h(store_id, shared).await);
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private]);

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn a_node_moved_into_a_share_reaches_its_member_and_one_moved_out_stops_arriving() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let (mut b, bob, b_dir) = member_device(&env, BOB, &fx).await;

    // In: a folder with a document in it, from outside the share. Wrap and
    // scope, and the member's replica asks for what its list now names.
    let budget = alice.create_node(store_id, Some(fx.root_id), "folder", "Budget").await.unwrap();
    let sums = alice.create_node(store_id, Some(budget), "document", "Sums").await.unwrap();
    seed_content(&alice, store_id, sums, "alice", "BUDGET-FIGURES").await;
    wait_all_seeded(&env, store_id, &[budget, sums]).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(env.wrap_key_ids(store_id, sums).await, vec![store_key_id]);
    assert!(bob.get_node(store_id, sums).await.is_err() && !contains_bytes_recursive(b_dir.path(), b"BUDGET-FIGURES"), "outside the share it is none of the member's");

    alice.move_node(store_id, budget, shared, None).await.unwrap();
    let wrapped = wait_until(Duration::from_secs(15), || async {
        sorted_uuids(env.wrap_key_ids(store_id, sums).await) == sorted_uuids(vec![store_key_id, share_key_id])
            && env.scope_on_h(store_id, shared).await.is_some_and(|docs| docs.contains(&budget) && docs.contains(&sums))
    })
    .await;
    assert!(wrapped, "a document entering the share is wrapped under its key and named by its scope");
    let arrived = wait_until(Duration::from_secs(40), || async {
        node_text(&bob, store_id, sums).await.contains("BUDGET-FIGURES") && child_ids(&bob, store_id, shared).await == vec![fx.inside, fx.deeper, budget]
    })
    .await;
    assert!(arrived, "and becomes readable to the member");
    seed_content(&bob, store_id, sums, "bob", "and a contingency").await;
    assert!(wait_until(Duration::from_secs(10), || async { node_text(&alice, store_id, sums).await.contains("and a contingency") }).await, "which they edit like the rest");

    // Out: leaving the share is a transplant (docs/MOVE_CONTRACT.md): the
    // original stays in the share's scope, a tombstone any member can undo,
    // while a new node with a new id and no share-key wrap lands in the
    // owner's private tree. Nothing ever leaves a scope while its share
    // stands, so the member's list stops naming it without the scope itself
    // shrinking.
    let moved = alice.move_node(store_id, fx.inside, fx.root_id, None).await.unwrap();
    let new_inside = moved.node_id;
    assert_ne!(new_inside, fx.inside, "the node that leaves the share gets a new id");
    assert_eq!(
        moved.left_shares.iter().map(|s| (s.root, s.name.as_str())).collect::<Vec<_>>(),
        vec![(shared, "Holiday Plans")],
        "it answers which share it left"
    );
    let still_scoped = wait_until(Duration::from_secs(15), || async { env.scope_on_h(store_id, shared).await.is_some_and(|docs| docs.contains(&fx.inside)) }).await;
    assert!(still_scoped, "the tombstone stays in the share's scope: nothing ever leaves a scope while its share stands");
    assert!(
        wait_until(Duration::from_secs(10), || async { child_ids(&bob, store_id, shared).await == vec![fx.deeper, budget] }).await,
        "but the member's list no longer names it"
    );
    let never_wrapped = wait_until(Duration::from_secs(15), || async { env.wrap_key_ids(store_id, new_inside).await == vec![store_key_id] }).await;
    assert!(never_wrapped, "the new private node has no wrap under the share's key");

    seed_content(&alice, store_id, new_inside, "alice", "AFTER-MOVING-OUT").await;
    rename(&alice, store_id, new_inside, "Private packing").await;
    let pushed = wait_until(Duration::from_secs(10), || async { env.head_on_h(store_id, new_inside).await >= 1 }).await;
    assert!(pushed, "the owner's later edits to the new, private node are hosted as ever");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!contains_bytes_recursive(b_dir.path(), b"AFTER-MOVING-OUT"), "and never reach the member");
    assert!(bob.get_node(store_id, new_inside).await.is_err(), "the new private node was never in any share bob holds");
    assert_eq!(child_ids(&bob, store_id, shared).await, vec![fx.deeper, budget]);

    // The member sees the original deleted and can put it back
    // (docs/MOVE_CONTRACT.md "Seeing and undoing what was removed"): both
    // notes then exist, side by side, the owner sees the same.
    let seen_deleted = wait_until(Duration::from_secs(15), || async {
        bob.list_deleted(store_id).await.unwrap_or_default().iter().any(|d| d.node.id == fx.inside && d.deleted_at.is_some())
    })
    .await;
    assert!(seen_deleted, "the member finds the tombstone in Recently Deleted");
    bob.undelete_node(store_id, fx.inside).await.expect("any editor of the share can put it back");
    let restored = wait_until(Duration::from_secs(15), || async { child_ids(&bob, store_id, shared).await.contains(&fx.inside) }).await;
    assert!(restored, "the note is back, alongside the new one that took its place outside");
    let owner_sees_both = wait_until(Duration::from_secs(15), || async {
        alice.get_node(store_id, fx.inside).await.is_ok() && alice.get_node(store_id, new_inside).await.is_ok()
    })
    .await;
    assert!(owner_sees_both, "the owner sees the same two notes");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

/// A member who holds two shares of one store moves a note from one into
/// the other (docs/MOVE_CONTRACT.md, "Verification"): it leaves the first
/// share, whose other member sees the note deleted, undoes it from
/// "Recently Deleted...", and then both notes exist — the owner sees the
/// same. The new document shares nothing with the old but its text.
#[tokio::test]
async fn a_member_who_holds_two_shares_moves_a_note_from_one_into_the_other() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, holiday) = (fx.store_id, fx.shared);

    // A second share of the same store, disjoint from Holiday.
    let errands = alice.create_node(store_id, Some(fx.root_id), "folder", "Errands").await.unwrap();
    wait_all_seeded(&env, store_id, &[errands]).await;
    alice.cloud_share_node(store_id, holiday, "Holiday Plans").await.unwrap();
    alice.cloud_share_node(store_id, errands, "Errands").await.unwrap();
    // Bob holds both shares; Carol holds only Holiday, the one the note leaves.
    alice.cloud_share_invite(store_id, holiday, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, errands, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, holiday, CAROL.0, MemberRole::Editor).await.unwrap();

    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let pulled_errands = wait_until(Duration::from_secs(15), || async { bob.get_node(store_id, errands).await.is_ok() }).await;
    assert!(pulled_errands, "bob's one replica pulled both shares at once");
    let (mut c, carol, _c_dir) = member_device(&env, CAROL, &fx).await;

    // Bob moves a note out of Holiday and into Errands: two shares of one
    // store, so the note leaves Holiday (docs/MOVE_CONTRACT.md "The rule").
    let moved = bob.move_node(store_id, fx.inside, errands, None).await.expect("a move between two shares of one store");
    let new_inside = moved.node_id;
    assert_ne!(new_inside, fx.inside, "the note that leaves Holiday gets a new id");
    assert_eq!(moved.left_shares.iter().map(|s| s.root).collect::<Vec<_>>(), vec![holiday], "it names the share it left");

    // Carol, Holiday's other member, sees the note deleted and puts it back.
    let carol_sees_deleted = wait_until(Duration::from_secs(20), || async { carol.get_node(store_id, fx.inside).await.is_err() }).await;
    assert!(carol_sees_deleted, "carol sees the note leave as a delete");
    let carol_lists_it = wait_until(Duration::from_secs(15), || async {
        carol.list_deleted(store_id).await.unwrap_or_default().iter().any(|d| d.node.id == fx.inside && d.deleted_at.is_some())
    })
    .await;
    assert!(carol_lists_it, "and finds it in Recently Deleted");
    carol.undelete_node(store_id, fx.inside).await.expect("carol, an editor of Holiday, puts it back");

    // Both notes now exist, side by side, with the same text but no shared
    // history: everyone who can reach them sees both.
    let both_exist = wait_until(Duration::from_secs(20), || async {
        bob.get_node(store_id, fx.inside).await.is_ok()
            && bob.get_node(store_id, new_inside).await.is_ok()
            && node_text(&bob, store_id, fx.inside).await.contains("socks and a map")
            && node_text(&bob, store_id, new_inside).await.contains("socks and a map")
    })
    .await;
    assert!(both_exist, "bob sees both notes, each with the packing list's text");
    let owner_sees_both = wait_until(Duration::from_secs(15), || async {
        alice.get_node(store_id, fx.inside).await.is_ok() && alice.get_node(store_id, new_inside).await.is_ok()
    })
    .await;
    assert!(owner_sees_both, "the owner sees the same two notes");
    assert!(child_ids(&carol, store_id, holiday).await.contains(&fx.inside), "restored under Holiday, where carol put it back");
    assert!(child_ids(&bob, store_id, errands).await.contains(&new_inside), "the new one stayed under Errands");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

/// A subtree of more than its root node, transplanted between two shares of
/// one store (docs/MOVE_CONTRACT.md "The operation: transplant" step 1:
/// "For `node` and its live subtree, in preorder, a new document each"):
/// every document the transplant plants, not only the one `moveNode` names,
/// must reach the hosted twin wrapped under the destination share's key —
/// otherwise a member who holds only that share could open the folder the
/// move planted and find the document inside it unreadable. Carol never
/// held Holiday, the share the subtree leaves, so anything she can read of
/// it came from Errands' own wrap alone.
#[tokio::test]
async fn a_subtree_transplanted_between_shares_is_readable_at_every_planted_node() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, holiday) = (fx.store_id, fx.shared);
    // `Tickets` (`fx.deeper`, a folder) already carries `Train` (`fx.leaf`)
    // inside it (see `Fixture`'s diagram) — a real two-node subtree, not a
    // single document, and text on the leaf a member can look for after
    // the move to know its content, not only its id, made the trip.
    seed_content(&alice, store_id, fx.leaf, "alice", "TRAIN-DEPARTS-NINE").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // A second share of the same store, disjoint from Holiday.
    let errands = alice.create_node(store_id, Some(fx.root_id), "folder", "Errands").await.unwrap();
    wait_all_seeded(&env, store_id, &[errands]).await;
    alice.cloud_share_node(store_id, holiday, "Holiday Plans").await.unwrap();
    alice.cloud_share_node(store_id, errands, "Errands").await.unwrap();
    // Bob holds both shares and makes the move; Carol holds only Errands,
    // the share the subtree lands in, and is never invited to Holiday.
    alice.cloud_share_invite(store_id, holiday, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, errands, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, errands, CAROL.0, MemberRole::Editor).await.unwrap();

    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let pulled_errands = wait_until(Duration::from_secs(15), || async { bob.get_node(store_id, errands).await.is_ok() }).await;
    assert!(pulled_errands, "bob's one replica pulled both shares at once");

    let (mut c, carol, _c_dir) = start_local_server().await;
    env.sign_in(&carol, CAROL).await;
    carol.cloud_add_hosted_store(store_id).await.expect("a share is added like any hosted store");
    let carol_pulled = wait_until(Duration::from_secs(15), || async { carol.get_node(store_id, errands).await.is_ok() }).await;
    assert!(carol_pulled, "carol's replica pulls the one share she holds");

    // Bob moves `Tickets`, with `Train` inside it, out of Holiday and into
    // Errands: two shares of one store, so the whole subtree leaves
    // Holiday and is replanted under Errands, fresh documents and all.
    let moved = bob.move_node(store_id, fx.deeper, errands, None).await.expect("a move between two shares of one store");
    let new_deeper = moved.node_id;
    assert_ne!(new_deeper, fx.deeper, "the subtree's root gets a new id");

    let got_child = wait_until(Duration::from_secs(20), || async { !child_ids(&alice, store_id, new_deeper).await.is_empty() }).await;
    assert!(got_child, "the child came along with its folder");
    let new_leaf = child_ids(&alice, store_id, new_deeper).await[0];
    assert_ne!(new_leaf, fx.leaf, "the child gets a new id too: nothing of the old document is shared with the new one");

    // Carol, who never held Holiday, reads both planted nodes once the
    // owner's vault link wraps their data keys under Errands' key and
    // publishes them into its scope — the whole subtree the transplant
    // made, not only the node `moveNode` named.
    let carol_reads_the_folder = wait_until(Duration::from_secs(20), || async { carol.get_node(store_id, new_deeper).await.is_ok() }).await;
    assert!(carol_reads_the_folder, "the planted folder reaches carol, who only holds Errands");
    let carol_reads_the_leaf = wait_until(Duration::from_secs(20), || async { node_text(&carol, store_id, new_leaf).await.contains("TRAIN-DEPARTS-NINE") }).await;
    assert!(carol_reads_the_leaf, "and so does the document planted inside it, with the text it carried before the move");
    assert!(child_ids(&carol, store_id, errands).await.contains(&new_deeper), "listed under Errands, where carol can see it");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn a_reader_reads_the_share_and_cannot_write() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    let answer = alice.cloud_share_invite(store_id, shared, CAROL.0, MemberRole::Reader).await.unwrap();
    assert_eq!(answer.members.last().map(|m| (m.email.as_str(), m.role, m.status)), Some((CAROL.0, MemberRole::Reader, ShareMemberStatus::Active)));

    let (mut c, carol, _c_dir) = member_device(&env, CAROL, &fx).await;
    assert_eq!(carol.list_stores().await.unwrap().iter().find(|s| s.id == store_id).unwrap().access, StoreAccess::Read);

    let heads_before: HashMap<VaultDocId, u64> = env.h_admin.vault_list_docs(store_id).await.unwrap().into_iter().map(|d| (d.doc_id, d.head)).collect();
    let refused = |result: Result<(), pimble_client::ClientError>| assert_eq!(result.expect_err("a reader's replica refuses writes").to_string(), StoreAccess::READ_ONLY_REFUSAL);
    refused(carol.create_node(store_id, Some(shared), "document", "no").await.map(|_| ()));
    refused(carol.move_node(store_id, fx.leaf, shared, None).await.map(|_| ()));
    refused(carol.delete_node(store_id, fx.inside).await);
    let mut metadata = carol.get_node(store_id, fx.inside).await.unwrap().metadata;
    metadata.title = "no".into();
    refused(carol.update_node_metadata(store_id, fx.inside, metadata).await);
    let changes = base64::engine::general_purpose::STANDARD.encode(NodeDoc::from_plain_text("no").unwrap().save());
    refused(carol.apply_edit(store_id, fx.inside, "carol", EditOperation::IncrementalChanges { changes }).await);
    // Sharing on is an owner's to do, whoever asks.
    assert_eq!(carol.cloud_share_node(store_id, fx.deeper, "Mine now").await.expect_err("not the reader's to share").to_string(), StoreAccess::READ_ONLY_REFUSAL);

    // And still receives everything.
    seed_content(&alice, store_id, fx.inside, "alice", "and sunscreen").await;
    rename(&alice, store_id, fx.inside, "Packing list").await;
    let received = wait_until(Duration::from_secs(10), || async {
        node_text(&carol, store_id, fx.inside).await.contains("and sunscreen") && node_title(&carol, store_id, fx.inside).await == "Packing list"
    })
    .await;
    assert!(received);
    let heads_after: HashMap<VaultDocId, u64> = env.h_admin.vault_list_docs(store_id).await.unwrap().into_iter().map(|d| (d.doc_id, d.head)).collect();
    assert_eq!(heads_after.get(&VaultDocId::Node(shared)), heads_before.get(&VaultDocId::Node(shared)), "a reader's device pushes nothing");

    a.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn a_member_can_neither_share_from_a_share_nor_change_it() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    alice.cloud_share_node(fx.store_id, fx.shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(fx.store_id, fx.shared, BOB.0, MemberRole::Editor).await.unwrap();
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;

    let refused = bob.cloud_share_node(fx.store_id, fx.deeper, "Mine now").await.expect_err("a member shares nothing on");
    assert_eq!(refused.to_string(), "Only an owner of this store can share from it. Nothing was changed.");
    assert!(bob.get_node(fx.store_id, fx.deeper).await.unwrap().metadata.share().is_none());
    assert!(env.scope_on_h(fx.store_id, fx.deeper).await.is_none());

    // Nor changes the share they are in. They may look at it: the owner is
    // whoever shared it, not the account looking.
    let not_theirs = "Only an owner of this store can change its shares. Nothing was changed.";
    assert_eq!(bob.cloud_share_invite(fx.store_id, fx.shared, CAROL.0, MemberRole::Editor).await.expect_err("not theirs to invite to").to_string(), not_theirs);
    assert_eq!(bob.cloud_share_remove_member(fx.store_id, fx.shared, BOB.0).await.expect_err("not theirs to remove from").to_string(), not_theirs);
    assert_eq!(bob.cloud_stop_sharing(fx.store_id, fx.shared).await.expect_err("not theirs to stop").to_string(), not_theirs);
    assert_eq!(env.scoped_rows(fx.store_id, fx.shared), 1);
    assert!(env.scope_on_h(fx.store_id, fx.shared).await.is_some() && bob.get_node(fx.store_id, fx.shared).await.unwrap().metadata.share().is_some());
    let seen = bob.cloud_share_info(fx.store_id, fx.shared).await.expect("a member may look at the share they are in");
    assert_eq!(
        seen.members.iter().map(|m| (m.email.as_str(), m.role)).collect::<Vec<_>>(),
        vec![(ALICE.0, MemberRole::Owner), (BOB.0, MemberRole::Editor)]
    );
    assert_eq!(alice.cloud_share_invite(fx.store_id, fx.shared, ALICE.0, MemberRole::Editor).await.expect_err("one's own store").to_string(), "This is your own store; there is nothing to invite yourself to.");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn a_member_invited_before_they_have_an_account_gets_the_key_within_a_sweep() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();

    let answer = alice.cloud_share_invite(store_id, shared, DAVE.0, MemberRole::Editor).await.expect("inviting an address with no account");
    assert_eq!(answer.members.last().map(|m| (m.email.as_str(), m.role, m.status)), Some((DAVE.0, MemberRole::Editor, ShareMemberStatus::Invited)));
    assert!(!env.has_share_key(store_id, shared, DAVE.0));

    // The account appears (signs up, verifies) and claims its invitation.
    // Nobody tells the owner's device; its sweep finds the member without a key.
    env.account_appears(DAVE);
    let handed = wait_until(SWEEP_EVERY * 5 + Duration::from_secs(5), || async { env.has_share_key(store_id, shared, DAVE.0) }).await;
    assert!(handed, "the sweep hands the share key to a member who turned up");
    let info = alice.cloud_share_info(store_id, shared).await.unwrap();
    assert_eq!(info.members.last().map(|m| (m.email.as_str(), m.status)), Some((DAVE.0, ShareMemberStatus::Active)));

    let (mut d, dave, _d_dir) = member_device(&env, DAVE, &fx).await;
    assert_eq!(child_ids(&dave, store_id, shared).await, vec![fx.inside, fx.deeper]);

    // An invitation is withdrawn by address, like a member.
    alice.cloud_share_invite(store_id, shared, "erin@example.com", MemberRole::Reader).await.unwrap();
    assert_eq!(env.scoped_rows(store_id, shared), 2);
    let after = alice.cloud_share_remove_member(store_id, shared, "erin@example.com").await.unwrap();
    assert_eq!(after.members.iter().map(|m| m.email.as_str()).collect::<Vec<_>>(), vec![ALICE.0, DAVE.0]);

    a.stop().await.unwrap();
    d.stop().await.unwrap();
}

#[tokio::test]
async fn sharing_from_a_store_that_is_not_hosted_is_refused_and_nothing_is_uploaded() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    env.sign_in(&alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(a_dir.path().join("a.pimble"), "Alice's Notes").await.unwrap();
    let folder = alice.create_node(store_id, Some(root_id), "folder", "Holiday").await.unwrap();
    let doc = alice.create_node(store_id, Some(folder), "document", "Packing").await.unwrap();
    seed_content(&alice, store_id, doc, "seed", "NEVER-HOSTED-TEXT").await;

    let refused = alice.cloud_share_node(store_id, folder, "Holiday Plans").await.expect_err("sharing never hosts");
    assert_eq!(refused.to_string(), "Sharing needs this store hosted on Pimble Cloud or shared from this computer.");
    // Whoever is or is not signed in: the sentence is about the store.
    alice.cloud_sign_out().await.unwrap();
    assert_eq!(alice.cloud_share_node(store_id, folder, "Holiday Plans").await.expect_err("still").to_string(), pimble_server::share::NOT_HOSTED_REFUSAL);

    // Nothing anywhere: the hosted server holds no store and no file, the
    // accounts service was asked nothing about stores, members or keys, and
    // the node carries no marker.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(env.h_admin.list_stores().await.unwrap().is_empty(), "the hosted server's store list is unchanged");
    assert!(env.h_admin.vault_list_docs(store_id).await.is_err(), "and it has no vault of the store");
    assert_eq!(std::fs::read_dir(&env.h_stores_dir).unwrap().count(), 0);
    assert!(!contains_bytes_recursive(env._h_dir.path(), b"NEVER-HOSTED-TEXT"));
    {
        let s = env.stub.inner.lock().unwrap();
        assert!(s.stores.is_empty() && s.key_grants.is_empty() && s.invitations.is_empty());
        assert_eq!(s.sharing_calls, 0);
    }
    assert!(alice.get_node(store_id, folder).await.unwrap().metadata.share().is_none());
    assert!(scope_keys_in(a_dir.path(), store_id).is_empty(), "no share key was made");
    let (remote, _, mode) = alice.get_store_sync_with_mode(store_id).await.unwrap();
    assert!(remote.is_none() && mode == StoreKind::Plain, "and the store is as unhosted as it was");

    a.stop().await.unwrap();
}

#[tokio::test]
async fn stopping_a_share_leaves_the_documents_hosted_and_removes_scope_marker_and_members() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();
    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, shared, DAVE.0, MemberRole::Reader).await.unwrap();
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let (mut b, bob, b_dir) = member_device(&env, BOB, &fx).await;
    let from_bob = bob.create_node(store_id, Some(shared), "document", "Ferry").await.unwrap();
    assert!(wait_until(Duration::from_secs(20), || async { alice.get_node(store_id, from_bob).await.is_ok() }).await);
    let hosted_before: HashMap<VaultDocId, u64> = env.h_admin.vault_list_docs(store_id).await.unwrap().into_iter().map(|d| (d.doc_id, d.head)).collect();

    // Unreachable: nothing is changed, and the answer says so.
    env.stub.members_down.store(true, Ordering::SeqCst);
    let unreachable = alice.cloud_stop_sharing(store_id, shared).await.expect_err("the accounts service is down").to_string();
    assert!(unreachable.starts_with("Pimble Cloud cannot be reached, so nothing was changed."), "{unreachable}");
    env.stub.members_down.store(false, Ordering::SeqCst);
    env.relay.cut();
    let offline = wait_until(Duration::from_secs(10), || async { !matches!(alice.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. }))) }).await;
    assert!(offline);
    let unreachable = alice.cloud_stop_sharing(store_id, shared).await.expect_err("the hosted server is unreachable").to_string();
    assert_eq!(unreachable, "Pimble Cloud cannot be reached, so nothing was changed. Stop sharing again once this store is back online.");
    assert!(matches!(alice.cloud_share_info(store_id, shared).await.unwrap().share.state, SyncState::Offline), "a share is offline with its store's link");
    env.relay.restore();
    assert_eq!(env.scoped_rows(store_id, shared), 2);
    assert!(env.scope_on_h(store_id, shared).await.is_some());
    assert!(alice.get_node(store_id, shared).await.unwrap().metadata.share().is_some());
    let back = wait_until(Duration::from_secs(20), || async { matches!(alice.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. }))) }).await;
    assert!(back, "the link reconnects");
    let kept_up_again = wait_until(Duration::from_secs(10), || async { matches!(alice.cloud_share_info(store_id, shared).await.map(|info| info.share.state), Ok(SyncState::Synced { .. })) }).await;
    assert!(kept_up_again, "and the share is kept up again");

    alice.cloud_stop_sharing(store_id, shared).await.expect("stopping the share");

    // Members, invitation, scope, marker and key are gone.
    assert_eq!(env.scoped_rows(store_id, shared), 0);
    assert!(!env.has_share_key(store_id, shared, BOB.0));
    assert!(env.scope_on_h(store_id, shared).await.is_none(), "{:?}", env.h_admin.get_scopes(store_id).await);
    assert!(alice.get_node(store_id, shared).await.unwrap().metadata.share().is_none());
    assert!(!scope_keys_in(a_dir.path(), store_id).iter().any(|(id, _)| *id == share_key_id), "the share key is dropped");
    assert!(alice.cloud_share_info(store_id, shared).await.expect_err("no share to ask about").to_string().contains("is not shared"));
    // The owner's own whole-store grant is nobody's to remove by stopping a share.
    assert!(alice.cloud_list_hosted_stores().await.unwrap().iter().any(|row| row.store_id == store_id.to_string() && row.root.is_none() && row.role == "owner"));

    // The documents stay exactly where they are, the member's among them,
    // which the owner reads without the share key: it has the store key's wrap.
    let hosted_after: HashMap<VaultDocId, u64> = env.h_admin.vault_list_docs(store_id).await.unwrap().into_iter().map(|d| (d.doc_id, d.head)).collect();
    for (doc, head) in &hosted_before {
        assert!(hosted_after.get(doc).is_some_and(|after| after >= head), "{doc:?} is still hosted");
    }
    assert!(env.wrap_key_ids(store_id, from_bob).await.contains(&store_key_id));
    let (_, state, mode) = alice.get_store_sync_with_mode(store_id).await.unwrap();
    assert!(matches!(state, SyncState::Synced { .. }) && mode == StoreKind::Vault, "the store is still hosted, because the person hosted it");
    let marker_gone_on_h = wait_until(Duration::from_secs(10), || async { env.head_on_h(store_id, shared).await > hosted_before[&VaultDocId::Node(shared)] }).await;
    assert!(marker_gone_on_h, "the marker's removal replicates like any metadata");

    // The member's replica keeps what it had and receives nothing more.
    seed_content(&alice, store_id, fx.inside, "alice", "AFTER-THE-SHARE-STOPPED").await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!contains_bytes_recursive(b_dir.path(), b"AFTER-THE-SHARE-STOPPED"));
    assert!(node_text(&bob, store_id, fx.inside).await.contains("socks and a map"));

    // Shared again, it is a new share with a new key.
    let again = alice.cloud_share_node(store_id, shared, "Holiday, again").await.expect("sharing the node again");
    assert_eq!(again.members.len(), 1);
    assert_ne!(alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id, share_key_id);

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn deleting_a_shared_folder_stops_its_share_first_and_goes_ahead_without_the_cloud() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);

    // A share rooted inside what is deleted goes with it.
    alice.cloud_share_node(store_id, fx.deeper, "Tickets").await.unwrap();
    alice.cloud_share_invite(store_id, fx.deeper, BOB.0, MemberRole::Editor).await.unwrap();
    assert_eq!(env.scoped_rows(store_id, fx.deeper), 1);
    alice.delete_node(store_id, shared).await.expect("deleting a folder that holds a share");
    assert!(alice.get_node(store_id, fx.deeper).await.is_err());
    assert_eq!(env.scoped_rows(store_id, fx.deeper), 0, "its members are removed");
    assert!(env.scope_on_h(store_id, fx.deeper).await.is_none(), "and its scope");

    // Signed out, the deletion goes ahead all the same; what is left on the
    // account is the log's to say.
    let other = alice.create_node(store_id, Some(fx.root_id), "folder", "Recipes").await.unwrap();
    wait_all_seeded(&env, store_id, &[other]).await;
    alice.cloud_share_node(store_id, other, "Recipes").await.unwrap();
    alice.cloud_share_invite(store_id, other, CAROL.0, MemberRole::Reader).await.unwrap();
    alice.cloud_sign_out().await.unwrap();
    alice.delete_node(store_id, other).await.expect("a deletion never waits for Pimble Cloud");
    assert!(alice.get_node(store_id, other).await.is_err());
    assert_eq!(env.scoped_rows(store_id, other), 1, "the grant is left on the account");

    a.stop().await.unwrap();
}

#[tokio::test]
async fn a_second_device_of_the_owners_keeps_the_share_up_from_the_replicated_marker() {
    let env = spawn_env().await;
    let (mut a1, alice1, a1_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice1, a1_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, _) = scope_keys_in(a1_dir.path(), store_id).pop().unwrap();

    // The second device is up before the share exists: the marker reaches
    // it live, through replication, and with it the share's upkeep.
    let (mut a2, alice2, a2_dir) = start_local_server().await;
    env.sign_in(&alice2, ALICE).await;
    alice2.cloud_add_hosted_store(store_id).await.unwrap();
    assert!(wait_until(Duration::from_secs(15), || async { node_text(&alice2, store_id, fx.inside).await.contains("socks and a map") }).await);

    alice1.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    let share_key_id = alice1.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let has_key = wait_until(Duration::from_secs(20), || async { scope_keys_in(a2_dir.path(), store_id).iter().any(|(id, _)| *id == share_key_id) }).await;
    assert!(has_key, "the owner's other device fetches its own envelope of the share key");
    drop(alice1);
    a1.stop().await.unwrap();

    // Everything an owner's device does for a share, from the second one.
    let info = alice2.cloud_share_info(store_id, shared).await.expect("the share is this device's to keep up too");
    assert_eq!(info.share.name, "Holiday Plans");
    alice2.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    assert!(env.has_share_key(store_id, shared, BOB.0));
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;

    let made = bob.create_node(store_id, Some(shared), "document", "Ferry").await.unwrap();
    let kept_up = wait_until(Duration::from_secs(30), || async {
        sorted_uuids(env.wrap_key_ids(store_id, made).await) == sorted_uuids(vec![store_key_id, share_key_id]) && env.scope_on_h(store_id, shared).await.is_some_and(|docs| docs.contains(&made))
    })
    .await;
    assert!(kept_up, "wraps {:?}", env.wrap_key_ids(store_id, made).await);
    let hotel = alice2.create_node(store_id, Some(shared), "document", "Hotel").await.unwrap();
    assert!(wait_until(Duration::from_secs(30), || async { bob.get_node(store_id, hotel).await.is_ok() }).await, "and the member follows what it does");

    a2.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn a_document_from_before_data_keys_gets_one_when_it_comes_under_a_share() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    let (store_key_id, store_key) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();

    // A whole document from before data keys, under the folder: its blobs
    // are under the store key itself, made by hand as a phase 2a client
    // would have. The folder's list names it, so it is in the tree.
    let old = NodeId::new();
    let old_key = VaultDocId::Node(old);
    let mut old_doc = NodeDoc::from_plain_text("WRITTEN-BEFORE-DATA-KEYS").unwrap();
    old_doc.init("document", "Old", Some(shared), &chrono::Utc::now().to_rfc3339()).unwrap();
    let aad = pimble_crypto::blob_aad(&store_id.to_string(), &old_key.as_str());
    let blob = pimble_crypto::Blob::encrypt(&store_key, store_key_id, &aad, &old_doc.save());
    env.h_admin.vault_append(store_id, old_key.clone(), URL_SAFE_NO_PAD.encode(blob)).await.unwrap();
    let here = wait_until(Duration::from_secs(15), || async { child_ids(&alice, store_id, shared).await.contains(&old) }).await;
    assert!(here, "the owner's device reads it with the store key and repair lists it");
    seed_content(&alice, store_id, old, "alice", "and edited since").await;
    assert!(wait_until(Duration::from_secs(10), || async { env.head_on_h(store_id, old).await >= 2 }).await);
    assert!(env.wrap_key_ids(store_id, old).await.is_empty(), "no device makes keys for a document that exists, until it comes under a share");

    alice.cloud_share_node(store_id, shared, "Holiday Plans").await.unwrap();
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let rekeyed = wait_until(Duration::from_secs(20), || async {
        let Ok(fetch) = env.h_admin.vault_fetch(store_id, old_key.clone(), 0).await else { return false };
        let Some(keys) = &fetch.keys else { return false };
        let under_the_dek = |blob: &str| pimble_crypto::Blob::key_id(&URL_SAFE_NO_PAD.decode(blob).unwrap()).unwrap() == keys.dek_id;
        fetch.snapshot.as_ref().is_some_and(|s| under_the_dek(&s.blob)) && fetch.updates.iter().all(|u| under_the_dek(&u.blob))
    })
    .await;
    assert!(rekeyed, "a data key, and a snapshot under it in place of the blobs no member could read");
    assert_eq!(sorted_uuids(env.wrap_key_ids(store_id, old).await), sorted_uuids(vec![store_key_id, share_key_id]));

    alice.cloud_share_invite(store_id, shared, BOB.0, MemberRole::Editor).await.unwrap();
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let read = wait_until(Duration::from_secs(15), || async {
        let text = node_text(&bob, store_id, old).await;
        text.contains("WRITTEN-BEFORE-DATA-KEYS") && text.contains("and edited since")
    })
    .await;
    assert!(read, "the member reads what was written before data keys: {:?}", node_text(&bob, store_id, old).await);
    // What the owner's device writes next goes out under the data key.
    seed_content(&alice, store_id, old, "alice", "after the share").await;
    assert!(wait_until(Duration::from_secs(10), || async { node_text(&bob, store_id, old).await.contains("after the share") }).await);

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn a_big_folder_is_wrapped_in_slices_and_its_scope_published_once_all_of_it_is_readable() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    env.sign_in(&alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(a_dir.path().join("a.pimble"), "Alice's Notes").await.unwrap();
    let shared = alice.create_node(store_id, Some(root_id), "folder", "Archive").await.unwrap();
    // More documents than one pass wraps before it lets the link's loop turn.
    let mut docs = Vec::new();
    for i in 0..150 {
        docs.push(alice.create_node(store_id, Some(shared), "document", format!("Note {i}")).await.unwrap());
    }
    alice.cloud_host_store(store_id).await.unwrap();
    let mut all = docs.clone();
    all.extend([root_id, shared]);
    wait_all_seeded(&env, store_id, &all).await;
    let (store_key_id, _) = scope_keys_in(a_dir.path(), store_id).pop().unwrap();

    alice.cloud_share_node(store_id, shared, "Archive").await.expect("sharing a big folder");
    let share_key_id = alice.get_node(store_id, shared).await.unwrap().metadata.share().unwrap().key_id;
    let published = wait_until(Duration::from_secs(30), || async { env.scope_on_h(store_id, shared).await.is_some() }).await;
    assert!(published);
    // Wraps first, scope second: the moment the scope is there, everything
    // it names can be read with the share's key.
    assert_eq!(sorted(env.scope_on_h(store_id, shared).await.unwrap()), sorted(docs.clone()));
    for doc in &docs {
        assert_eq!(sorted_uuids(env.wrap_key_ids(store_id, *doc).await), sorted_uuids(vec![store_key_id, share_key_id]), "{doc}");
    }
    let settled = wait_until(Duration::from_secs(10), || async { matches!(alice.cloud_share_info(store_id, shared).await.map(|info| info.share.state), Ok(SyncState::Synced { .. })) }).await;
    assert!(settled);

    a.stop().await.unwrap();
}

/// The roots a member's replica is told have ended, from now until `want`
/// have all been named (in one notification or several).
async fn shares_ended_naming(changes: &mut jsonrpsee::core::client::Subscription<pimble_rpc::StoreChangedNotification>, want: &[NodeId]) -> Vec<Vec<NodeId>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut told: Vec<Vec<NodeId>> = Vec::new();
        while let Some(Ok(notif)) = changes.next().await {
            let StoreChangeKind::SharesEnded { node_ids } = notif.change_kind else { continue };
            assert_eq!(notif.source_client_id, None, "derived by this server, nobody's edit");
            told.push(node_ids);
            if want.iter().all(|root| told.iter().flatten().any(|named| named == root)) {
                break;
            }
        }
        told
    })
    .await
    .expect("the member's replica is told which shares ended")
}

/// Joe, 2026-09-21, what a removed member's machine shows: the folder
/// leaves the explorer with a notice, and the files go when the replica is
/// removed. The server's half: a root the account's rows no longer name is
/// recorded as ended (never dropped from the scope roots), announced once,
/// read only and unfound by a search; the share still held goes on as
/// before; a root granted again is un-ended; with every share ended the
/// replica is read only, whole on disk, and `removeReplica` is what deletes it.
#[tokio::test]
async fn a_share_that_ended_is_announced_and_kept_on_disk_until_the_replica_is_removed() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, holiday) = (fx.store_id, fx.shared);
    let recipes = alice.create_node(store_id, Some(fx.root_id), "folder", "Recipes").await.unwrap();
    let soup = alice.create_node(store_id, Some(recipes), "document", "Soup").await.unwrap();
    seed_content(&alice, store_id, soup, "seed", "leeks and potatoes").await;
    alice.cloud_share_node(store_id, holiday, "Holiday Plans").await.unwrap();
    alice.cloud_share_node(store_id, recipes, "Recipes").await.unwrap();
    alice.cloud_share_invite(store_id, holiday, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, recipes, BOB.0, MemberRole::Editor).await.unwrap();

    // Bob's device asks about its grant every second, in place of two minutes.
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = PimbleServer::with_config(ServerConfig { grant_check_interval: Some(Duration::from_secs(1)), ..device_config(b_dir.path()) });
    b.start().await.unwrap();
    let bob = PimbleClient::connect(format!("http://{}", b.addr())).await.unwrap();
    env.sign_in(&bob, BOB).await;
    let added = bob.cloud_add_hosted_store(store_id).await.unwrap();
    assert_eq!(sorted(added.roots.clone()), sorted(vec![holiday, recipes]));
    assert!(added.ended_roots.is_empty());
    let replica_path = added.local_path().cloned().expect("a replica is a local store");
    let pulled = wait_until(Duration::from_secs(20), || async {
        node_text(&bob, store_id, fx.inside).await.contains("socks and a map") && node_text(&bob, store_id, soup).await.contains("leeks and potatoes")
    })
    .await;
    assert!(pulled, "both shares are pulled");
    let found = |hits: Vec<pimble_rpc::SearchResultItem>| hits.into_iter().map(|hit| hit.node_id).collect::<Vec<_>>();
    let indexed = wait_until(Duration::from_secs(20), || async { bob.search("socks", vec![store_id], false, 10).await.map(found).unwrap_or_default().contains(&fx.inside) }).await;
    assert!(indexed, "a document of the share is found by a search while the share is held");
    let mut bob_changes = bob.subscribe_store_changes(store_id).await.unwrap();

    // Removed from one of the two.
    alice.cloud_share_remove_member(store_id, holiday, BOB.0).await.unwrap();
    assert_eq!(shares_ended_naming(&mut bob_changes, &[holiday]).await, vec![vec![holiday]]);
    let listed = bob.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap();
    assert_eq!(sorted(listed.roots.clone()), sorted(vec![holiday, recipes]), "the scope roots are never shrunk");
    assert_eq!((listed.ended_roots.clone(), listed.shown_roots()), (vec![holiday], vec![recipes]));
    let held = bob.get_store_sync_response(store_id).await.unwrap();
    assert_eq!((held.ended_roots.clone(), held.access), (vec![holiday], StoreAccess::Full));
    assert!(held.read_only_roots.contains(&holiday), "an ended root is held read only, as before: {:?}", held.read_only_roots);

    // Nothing was deleted, nothing under it takes an edit, and a search
    // does not find what the explorer no longer shows.
    assert!(node_text(&bob, store_id, fx.inside).await.contains("socks and a map"));
    assert_eq!(child_ids(&bob, store_id, holiday).await, vec![fx.inside, fx.deeper]);
    let refused = bob.create_node(store_id, Some(holiday), "document", "Too late").await.expect_err("an ended share takes no edits").to_string();
    assert!(refused.contains(StoreAccess::READ_ONLY_REFUSAL), "{refused}");
    assert!(!bob.search("socks", vec![store_id], false, 10).await.map(found).unwrap().contains(&fx.inside));

    // The share still held syncs and takes edits, both ways.
    let synced = wait_until(Duration::from_secs(20), || async { matches!(bob.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. }))) }).await;
    assert!(synced, "the link is up again with a token for the share that is left");
    rename(&bob, store_id, soup, "Leek soup").await;
    let reached = wait_until(Duration::from_secs(20), || async { node_title(&alice, store_id, soup).await == "Leek soup" }).await;
    assert!(reached, "the member's edit in the share still held reaches the owner");
    let bread = alice.create_node(store_id, Some(recipes), "document", "Bread").await.unwrap();
    let arrived = wait_until(Duration::from_secs(30), || async { child_ids(&bob, store_id, recipes).await == vec![soup, bread] }).await;
    assert!(arrived, "and the owner's reaches the member: {:?}", child_ids(&bob, store_id, recipes).await);
    // What the owner does in the ended share no longer arrives.
    rename(&alice, store_id, fx.inside, "Packing, revised").await;

    // Invited again: un-ended by the same comparison, and editable again.
    alice.cloud_share_invite(store_id, holiday, BOB.0, MemberRole::Editor).await.unwrap();
    let unended = wait_until(Duration::from_secs(30), || async {
        bob.get_store_sync_response(store_id).await.is_ok_and(|held| held.ended_roots.is_empty() && held.read_only_roots.is_empty())
    })
    .await;
    assert!(unended, "a root granted again is no longer ended");
    assert!(bob.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap().ended_roots.is_empty());
    let caught_up = wait_until(Duration::from_secs(30), || async { node_title(&bob, store_id, fx.inside).await == "Packing, revised" }).await;
    assert!(caught_up, "and what was missed meanwhile arrives");
    let ferry = bob.create_node(store_id, Some(holiday), "document", "Ferry").await.expect("the share takes edits again");
    assert!(wait_until(Duration::from_secs(20), || async { alice.get_node(store_id, ferry).await.is_ok() }).await);

    // Removed from every share: every root ended, read only, all of it
    // still on disk.
    alice.cloud_share_remove_member(store_id, holiday, BOB.0).await.unwrap();
    alice.cloud_share_remove_member(store_id, recipes, BOB.0).await.unwrap();
    let told = shares_ended_naming(&mut bob_changes, &[holiday, recipes]).await;
    assert_eq!(sorted(told.into_iter().flatten().collect()), sorted(vec![holiday, recipes]), "each root is announced once");
    let all_ended = wait_until(Duration::from_secs(30), || async {
        bob.get_store_sync_response(store_id).await.is_ok_and(|held| held.ended_roots.len() == 2 && held.access == StoreAccess::Read)
    })
    .await;
    assert!(all_ended, "{:?}", bob.get_store_sync_response(store_id).await);
    let listed = bob.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap();
    assert!(listed.every_share_ended() && listed.shown_roots().is_empty(), "nothing of it is shown: {listed:?}");
    assert_eq!(sorted(listed.roots.clone()), sorted(vec![holiday, recipes]), "and it is a partial replica still");
    for (doc, text) in [(fx.inside, "socks and a map"), (soup, "leeks and potatoes")] {
        assert!(node_text(&bob, store_id, doc).await.contains(text), "nothing was deleted");
        assert!(replica_path.join("nodes").join(format!("{doc}.yrs")).exists());
    }
    let refused = bob.create_node(store_id, Some(recipes), "document", "Too late").await.expect_err("read only throughout").to_string();
    assert!(refused.contains(StoreAccess::READ_ONLY_REFUSAL), "{refused}");
    assert!(bob.search("leeks", vec![store_id], false, 10).await.map(found).unwrap().is_empty());

    // Removing the replica is where the files go.
    bob.remove_replica(store_id, true).await.expect("an ended replica can be removed");
    assert!(!replica_path.exists(), "the directory is deleted with the replica");
    assert!(bob.list_stores().await.unwrap().iter().all(|s| s.id != store_id));

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

/// A share that ended and is granted again is one the app offers under "Add
/// Hosted Store..." (its root is not shown any more): adding it un-ends it
/// at once, with no wait for the link's next look at the grant, and the
/// same replica, with everything it held, carries on.
#[tokio::test]
async fn a_share_granted_again_is_added_again_to_the_replica_that_kept_it() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, holiday) = (fx.store_id, fx.shared);
    alice.cloud_share_node(store_id, holiday, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, holiday, BOB.0, MemberRole::Editor).await.unwrap();
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let mut bob_changes = bob.subscribe_store_changes(store_id).await.unwrap();

    // Removed; the link reads the grant again at its next connect.
    alice.cloud_share_remove_member(store_id, holiday, BOB.0).await.unwrap();
    env.relay.cut();
    env.relay.restore();
    assert_eq!(shares_ended_naming(&mut bob_changes, &[holiday]).await, vec![vec![holiday]]);
    let ended = bob.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap();
    assert!(ended.every_share_ended() && ended.access == StoreAccess::Read, "{ended:?}");
    let refused = bob.cloud_add_hosted_store(store_id).await.expect_err("nothing of it is granted").to_string();
    assert!(!refused.is_empty());

    alice.cloud_share_invite(store_id, holiday, BOB.0, MemberRole::Editor).await.unwrap();
    let again = bob.cloud_add_hosted_store(store_id).await.expect("a share granted again is added again");
    assert_eq!((again.roots.clone(), again.ended_roots.clone(), again.access), (vec![holiday], Vec::new(), StoreAccess::Full));
    assert!(node_text(&bob, store_id, fx.inside).await.contains("socks and a map"), "the replica that kept it");
    let ferry = bob.create_node(store_id, Some(holiday), "document", "Ferry").await.expect("and it takes edits again");
    assert!(wait_until(Duration::from_secs(20), || async { alice.get_node(store_id, ferry).await.is_ok() }).await, "which reach the owner");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

// ── Against the REAL accounts service (crates/pimble-cloud) ──────────────
//
// Everything above drives a stub. This walks the owner's half of sharing
// against the real `rhypedb-server` and `pimble-cloud` binaries, the way
// `tests/vault_link.rs`'s interop test walks hosting: real signups, the
// verification links `LogMailer` logs, scoped members, invitations claimed
// at verification, scoped key envelopes, and the token's role per root as
// the real service mints it. Skips itself (saying why on stderr) when
// either binary is missing.

use std::process::{Child, Command, Stdio};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// The minimal stand-in for jkbase's edge: `/rpc*` to `H`, the rest to
/// `pimble-cloud` (see `tests/vault_link.rs`).
async fn run_edge_proxy(listen_port: u16, cloud_port: u16, h_port: u16) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", listen_port)).await.expect("proxy binds");
    loop {
        let Ok((mut inbound, _)) = listener.accept().await else { continue };
        tokio::spawn(async move {
            let mut peek_buf = [0u8; 4096];
            let n = match inbound.peek(&mut peek_buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let head = String::from_utf8_lossy(&peek_buf[..n]);
            let is_rpc = head.starts_with("GET /rpc") || head.starts_with("POST /rpc");
            let target_port = if is_rpc { h_port } else { cloud_port };
            let Ok(mut outbound) = tokio::net::TcpStream::connect(("127.0.0.1", target_port)).await else { return };
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

fn find_rhypedb_server() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RHYPEDB_SERVER_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let home = dirs::home_dir()?;
    ["dev/rhypedb/target/release/rhypedb-server", "dev/rhypedb/target/debug/rhypedb-server"].iter().map(|rel| home.join(rel)).find(|p| p.is_file())
}

fn find_pimble_cloud_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PIMBLE_CLOUD_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?.to_path_buf();
    ["target/release/pimble-cloud", "target/debug/pimble-cloud"].iter().map(|rel| workspace_root.join(rel)).find(|p| p.is_file())
}

/// A real signup with real `pimble-crypto` output, then the verification
/// link `LogMailer` logged for it (the newest one in the log).
async fn sign_up_and_verify(cloud_url: &str, cloud_log: &Path, email: &str, password: &str) {
    let kdf = cheap_kdf_params();
    let password_keys = derive_password_keys(password, &kdf).unwrap();
    let account_keys = AccountKeys::generate();
    let recovery_kdf = cheap_kdf_params();
    let recovery_kek = pimble_crypto::derive_recovery_kek(&pimble_crypto::generate_recovery_code(), &recovery_kdf).unwrap();
    let logged_before = std::fs::read_to_string(cloud_log).map(|log| log.len()).unwrap_or(0);

    let signup = reqwest::Client::new()
        .post(format!("{cloud_url}/api/v1/signup"))
        .json(&json!({
            "email": email,
            "auth_key": pimble_crypto::encode_auth_key(&password_keys.auth_key),
            "kdf": kdf,
            "public_keys": account_keys.public_keys(),
            "account_key_blob": wrap_account_keys(&account_keys, &password_keys.kek).unwrap(),
            "recovery_salt": recovery_kdf.salt,
            "recovery_key_blob": wrap_account_keys(&account_keys, &recovery_kek).unwrap(),
        }))
        .send()
        .await
        .expect("signup request sends");
    assert_eq!(signup.status(), 202, "signup of {email}");

    let mut verify_url = None;
    let found = wait_until(Duration::from_secs(5), || {
        let log = std::fs::read_to_string(cloud_log).unwrap_or_default();
        let link = log.get(logged_before..).and_then(|fresh| {
            let start = fresh.find("/api/v1/verify?token=")?;
            let rest = &fresh[start..];
            let end = rest.find(|c: char| c.is_whitespace() || c == '"' || c == '\\').unwrap_or(rest.len());
            Some(rest[..end].to_string())
        });
        let found = link.is_some();
        if found {
            verify_url = link;
        }
        async move { found }
    })
    .await;
    assert!(found, "the verification link for {email} must appear in pimble-cloud's log");
    let no_redirect = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let verified = no_redirect.get(format!("{cloud_url}{}", verify_url.unwrap())).send().await.expect("verify link is reachable");
    assert_eq!(verified.status(), 303);
    assert!(verified.headers().get("location").unwrap().to_str().unwrap().contains("verified=1"), "verification of {email}");
}

#[tokio::test]
async fn sharing_against_the_real_accounts_service() {
    let Some(rhypedb_bin) = find_rhypedb_server() else {
        eprintln!("SKIPPING sharing_against_the_real_accounts_service: no rhypedb-server binary found (set RHYPEDB_SERVER_BIN, or build ~/dev/rhypedb)");
        return;
    };
    let Some(cloud_bin) = find_pimble_cloud_binary() else {
        eprintln!("SKIPPING sharing_against_the_real_accounts_service: no pimble-cloud binary found (set PIMBLE_CLOUD_BIN, or `cargo build -p pimble-cloud --release`)");
        return;
    };

    let rhypedb_data_dir = tempfile::tempdir().unwrap();
    let (rhypedb_http_port, rhypedb_tcp_port) = (free_port(), free_port());
    let schema_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("pimble-cloud").join("schema.rhype");
    let _rhypedb = ChildGuard(
        Command::new(&rhypedb_bin)
            .args(["--schema", schema_path.to_str().unwrap(), "--data-dir", rhypedb_data_dir.path().to_str().unwrap()])
            .args(["--listen", &format!("127.0.0.1:{rhypedb_http_port}"), "--tcp-listen", &format!("127.0.0.1:{rhypedb_tcp_port}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("rhypedb-server spawns"),
    );
    let rhypedb_ready = wait_until(Duration::from_secs(15), || async {
        reqwest::get(format!("http://127.0.0.1:{rhypedb_http_port}/health")).await.map(|r| r.status().is_success()).unwrap_or(false)
    })
    .await;
    assert!(rhypedb_ready, "rhypedb-server must come up within 15s");

    let h_dir = tempfile::tempdir().unwrap();
    let h_token = "h-service-token".to_string();
    let (cloud_internal_port, proxy_port) = (free_port(), free_port());
    let cloud_url = format!("http://127.0.0.1:{proxy_port}");
    let issuer = format!("{cloud_url}/api/v1");
    let mut h = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(h_token.clone()),
        jwks_url: Some(format!("{issuer}/.well-known/jwks.json").parse().unwrap()),
        jwt_issuer: Some(issuer),
        keystore_path: Some(h_dir.path().join("keys.json")),
        credentials_path: Some(h_dir.path().join("credentials.json")),
        replicas_dir: Some(h_dir.path().join("replicas")),
        ..Default::default()
    });
    h.start().await.expect("H starts");
    let h_url = format!("http://{}", h.addr());
    let h_admin = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_token.clone() }).await.expect("admin client connects to H");

    let cloud_stores_dir = tempfile::tempdir().unwrap();
    let cloud_log = h_dir.path().join("cloud.log");
    let cloud_log_file = std::fs::File::create(&cloud_log).unwrap();
    let cloud_log_file_err = cloud_log_file.try_clone().unwrap();
    let _cloud = ChildGuard(
        Command::new(&cloud_bin)
            .env("RHYPEDB_ADDR", format!("127.0.0.1:{rhypedb_tcp_port}"))
            .env("PIMBLE_SERVER_URL", &h_url)
            .env("PIMBLE_SERVER_TOKEN", &h_token)
            .env("PIMBLE_STORES_DIR", cloud_stores_dir.path())
            .env("PIMBLE_CLOUD_DEV_SIGNING_SEED", format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()))
            .env("PIMBLE_CLOUD_PUBLIC_URL", &cloud_url)
            .env("PORT", cloud_internal_port.to_string())
            .env("RUST_LOG", "pimble_cloud=info")
            .stdout(Stdio::from(cloud_log_file))
            .stderr(Stdio::from(cloud_log_file_err))
            .spawn()
            .expect("pimble-cloud spawns"),
    );
    let cloud_ready = wait_until(Duration::from_secs(15), || async {
        reqwest::get(format!("http://127.0.0.1:{cloud_internal_port}/api/v1/health")).await.map(|r| r.status().is_success()).unwrap_or(false)
    })
    .await;
    assert!(cloud_ready, "pimble-cloud must come up within 15s (see {:?})", cloud_log);
    tokio::spawn(run_edge_proxy(proxy_port, cloud_internal_port, h.addr().port()));
    let proxy_ready = wait_until(Duration::from_secs(5), || async {
        reqwest::get(format!("{cloud_url}/api/v1/health")).await.map(|r| r.status().is_success()).unwrap_or(false)
    })
    .await;
    assert!(proxy_ready, "the local edge proxy must come up within 5s");

    let tag = Uuid::new_v4().simple().to_string();
    let (alice_email, bob_email, late_email) = (format!("alice-{tag}@example.com"), format!("bob-{tag}@example.com"), format!("late-{tag}@example.com"));
    let password = "correct horse battery staple";
    sign_up_and_verify(&cloud_url, &cloud_log, &alice_email, password).await;
    sign_up_and_verify(&cloud_url, &cloud_log, &bob_email, password).await;

    // The owner: a hosted store with a folder to share and a document not to.
    let (mut a, alice, a_dir) = start_local_server().await;
    alice.cloud_sign_in(&cloud_url, &alice_email, password).await.expect("the owner signs in");
    let (store_id, root_id) = alice.create_store(a_dir.path().join("a.pimble"), "Alice's Notes").await.unwrap();
    let shared = alice.create_node(store_id, Some(root_id), "folder", "Holiday").await.unwrap();
    let inside = alice.create_node(store_id, Some(shared), "document", "Packing").await.unwrap();
    let private = alice.create_node(store_id, Some(root_id), "document", "Diary").await.unwrap();
    seed_content(&alice, store_id, inside, "seed", "socks and a map").await;
    seed_content(&alice, store_id, private, "seed", "PRIVATE-DIARY-TEXT").await;
    alice.cloud_host_store(store_id).await.expect("hosting against the real service");
    let seeded = wait_until(Duration::from_secs(15), || async {
        let listed = h_admin.vault_list_docs(store_id).await.unwrap_or_default();
        [root_id, shared, inside, private].iter().all(|id| listed.iter().any(|d| d.doc_id == VaultDocId::Node(*id) && d.head > 0))
    })
    .await;
    assert!(seeded);

    // Share, and invite an account that exists: the grant, and the key at once.
    let answer = alice.cloud_share_node(store_id, shared, "Holiday Plans").await.expect("cloudShareNode against the real service");
    assert_eq!(answer.members.iter().map(|m| (m.email.as_str(), m.role)).collect::<Vec<_>>(), vec![(alice_email.as_str(), MemberRole::Owner)]);
    assert!(matches!(answer.share.state, SyncState::Synced { .. }), "{:?}", answer.share.state);
    let answer = alice.cloud_share_invite(store_id, shared, &bob_email, MemberRole::Editor).await.expect("cloudShareInvite against the real service");
    assert_eq!(
        answer.members.iter().map(|m| (m.email.as_str(), m.role, m.status)).collect::<Vec<_>>(),
        vec![(alice_email.as_str(), MemberRole::Owner, ShareMemberStatus::Active), (bob_email.as_str(), MemberRole::Editor, ShareMemberStatus::Active)]
    );

    // The member: the real service's rows and token (a role per root) make
    // a partial replica, and both sides edit the tree.
    let (mut b, bob, b_dir) = start_local_server().await;
    bob.cloud_sign_in(&cloud_url, &bob_email, password).await.expect("the member signs in");
    let rows = bob.cloud_list_hosted_stores().await.unwrap();
    assert_eq!(rows.iter().map(|r| (r.root, r.name.as_str(), r.role.as_str(), r.shared_by.as_deref())).collect::<Vec<_>>(), vec![(Some(shared), "Holiday Plans", "editor", Some(alice_email.as_str()))]);
    let store = bob.cloud_add_hosted_store(store_id).await.expect("the share is added as a partial replica");
    assert_eq!((store.roots.clone(), store.name.as_str()), (vec![shared], "Holiday Plans"));
    assert!(wait_until(Duration::from_secs(15), || async { node_text(&bob, store_id, inside).await.contains("socks and a map") }).await, "read with the share's key");
    assert!(bob.get_node(store_id, private).await.is_err() && !contains_bytes_recursive(b_dir.path(), b"PRIVATE-DIARY-TEXT"));
    let ferry = bob.create_node(store_id, Some(shared), "document", "Ferry").await.unwrap();
    assert!(wait_until(Duration::from_secs(20), || async { child_ids(&alice, store_id, shared).await == vec![inside, ferry] }).await, "the member's create reaches the owner");
    rename(&alice, store_id, ferry, "Night ferry").await;
    assert!(wait_until(Duration::from_secs(10), || async { node_title(&bob, store_id, ferry).await == "Night ferry" }).await, "the owner's rename reaches the member");

    // An address with no account: invited, claimed when it verifies, and
    // handed the key by the owner's sweep.
    let answer = alice.cloud_share_invite(store_id, shared, &late_email, MemberRole::Reader).await.expect("inviting an address with no account");
    assert_eq!(answer.members.last().map(|m| (m.email.as_str(), m.status)), Some((late_email.as_str(), ShareMemberStatus::Invited)));
    sign_up_and_verify(&cloud_url, &cloud_log, &late_email, password).await;
    let handed = wait_until(SWEEP_EVERY * 5 + Duration::from_secs(5), || async {
        alice.cloud_share_info(store_id, shared).await.is_ok_and(|info| info.members.iter().any(|m| m.email == late_email && m.status == ShareMemberStatus::Active))
    })
    .await;
    assert!(handed, "the sweep hands the key to the member who turned up: {:?}", alice.cloud_share_info(store_id, shared).await.map(|info| info.members));
    let (mut l, late, _l_dir) = start_local_server().await;
    late.cloud_sign_in(&cloud_url, &late_email, password).await.unwrap();
    assert_eq!(late.cloud_add_hosted_store(store_id).await.expect("the late member adds the share").access, StoreAccess::Read);
    assert!(wait_until(Duration::from_secs(15), || async { node_text(&late, store_id, inside).await.contains("socks and a map") }).await);

    // Remove one, stop the rest.
    let after = alice.cloud_share_remove_member(store_id, shared, &late_email).await.expect("cloudShareRemoveMember against the real service");
    assert_eq!(after.members.len(), 2);
    alice.cloud_stop_sharing(store_id, shared).await.expect("cloudStopSharing against the real service");
    assert!(bob.cloud_list_hosted_stores().await.unwrap().is_empty(), "the member's grant is gone");
    assert!(h_admin.get_scopes(store_id).await.unwrap().iter().all(|scope| scope.root != shared), "and the scope");
    assert!(alice.get_node(store_id, shared).await.unwrap().metadata.share().is_none(), "and the marker");
    assert!(h_admin.vault_list_docs(store_id).await.unwrap().iter().any(|d| d.doc_id == VaultDocId::Node(ferry)), "the documents stay hosted");
    assert!(alice.cloud_list_hosted_stores().await.unwrap().iter().any(|row| row.store_id == store_id.to_string() && row.role == "owner"));

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    l.stop().await.unwrap();
    h.stop().await.unwrap();
}
