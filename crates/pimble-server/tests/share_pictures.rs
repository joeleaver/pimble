//! Pictures in encrypted stores and shares (docs/IMAGES_CONTRACT.md, wave
//! 3), on the sharing harness (`tests/common`): a picture is a vault
//! document of its own on the hosted twin, under its own data key wrapped
//! per scope key; the owner's upkeep wraps it for the shares whose
//! documents name it and adds it to their scopes; a member's picture is
//! uploaded with the share key's wrap and joins the share's scope as it
//! lands. The hosted server holds ciphertext only.

mod common;
use common::*;

use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_core::{BlobId, StoreId};
use pimble_rpc::{MemberRole, VaultDocId};

/// Bytes that pass for a PNG (its signature), distinct per `seed`.
fn png(len: usize, seed: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend((0..len.saturating_sub(8)).map(|i| ((i * 13 + i / 97) as u8).wrapping_add(seed)));
    bytes
}

async fn holds(client: &PimbleClient, store_id: StoreId, blob: BlobId, bytes: &[u8]) -> bool {
    client.get_blob(store_id, blob).await.is_ok_and(|(_, got)| got == bytes)
}

/// A run of a picture's bytes that nothing else would contain.
fn needle(bytes: &[u8]) -> &[u8] {
    &bytes[bytes.len() / 2..bytes.len() / 2 + 64]
}

#[tokio::test]
async fn a_shares_pictures_reach_its_members_with_the_owner_off_and_a_members_reach_the_owner() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;

    // Two pictures of the owner's: one on a canvas in what will be shared
    // (large enough to go in chunks), one in the private diary.
    let shared_pic = png(5 * 1024 * 1024 + 123, 1);
    let private_pic = png(40_000, 2);
    let shared_url = alice.put_blob(store_id, fx.inside, "image/png", &shared_pic).await.unwrap();
    put_on_canvas(&alice, store_id, fx.inside, &shared_url).await;
    let private_url = alice.put_blob(store_id, fx.private, "image/png", &private_pic).await.unwrap();
    put_on_canvas(&alice, store_id, fx.private, &private_url).await;

    // Hosted, as ciphertext: a document per picture, nothing readable.
    let on_h = wait_until(Duration::from_secs(20), || async {
        let listed = env.h_admin.vault_list_docs_with_blobs(store_id).await.map(|l| l.docs).unwrap_or_default();
        [shared_url.blob, private_url.blob].iter().all(|id| listed.iter().any(|d| d.doc_id == VaultDocId::Blob(*id) && d.head > 0))
    })
    .await;
    assert!(on_h, "the owner's link uploads both pictures to the hosted twin");
    assert!(!env.hosted_store_contains_plaintext_bytes(store_id, needle(&shared_pic)));
    assert!(!env.hosted_store_contains_plaintext_bytes(store_id, needle(&private_pic)));
    // A client from before pictures replicated is never listed one.
    assert!(env.h_admin.vault_list_docs(store_id).await.unwrap().iter().all(|d| !matches!(d.doc_id, VaultDocId::Blob(_))));

    alice.cloud_share_node(store_id, fx.shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, fx.shared, BOB.0, MemberRole::Editor).await.unwrap();
    alice.cloud_share_invite(store_id, fx.shared, CAROL.0, MemberRole::Editor).await.unwrap();
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (mut c, carol, _c_dir) = member_device(&env, CAROL, &fx).await;
    assert!(wait_until(Duration::from_secs(30), || holds(&bob, store_id, shared_url.blob, &shared_pic)).await, "the member gets the shared picture");

    // Every device of the owner's is off from here on.
    drop(alice);
    a.stop().await.unwrap();

    // The other member holds it as well, and nobody in the share is given
    // the picture that is outside it.
    assert!(wait_until(Duration::from_secs(30), || holds(&carol, store_id, shared_url.blob, &shared_pic)).await, "the other member too");
    assert!(!holds(&bob, store_id, private_url.blob, &private_pic).await, "a picture outside the share stays the owner's");
    let refused = bob.get_blob(store_id, private_url.blob).await;
    assert!(refused.is_err());

    // Bob puts a picture on a canvas in the share: Carol sees it with the
    // owner still off.
    let bobs = png(60_000, 3);
    let bobs_url = bob.put_blob(store_id, fx.leaf, "image/png", &bobs).await.unwrap();
    put_on_canvas(&bob, store_id, fx.leaf, &bobs_url).await;
    assert!(wait_until(Duration::from_secs(30), || holds(&carol, store_id, bobs_url.blob, &bobs)).await, "a member's picture reaches another member with the owner off");
    assert!(!env.hosted_store_contains_plaintext_bytes(store_id, needle(&bobs)));

    // The owner's device comes back and gets it.
    let (mut a, alice) = start_device_in(a_dir.path()).await;
    alice.open_store(a_dir.path().join("a.pimble")).await.expect("the owner's store reopens");
    assert!(wait_until(Duration::from_secs(40), || holds(&alice, store_id, bobs_url.blob, &bobs)).await, "a member's picture reaches the owner");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn a_picture_put_in_a_node_already_shared_reaches_the_member() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;
    alice.cloud_share_node(store_id, fx.shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(store_id, fx.shared, BOB.0, MemberRole::Reader).await.unwrap();
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;

    let later = png(30_000, 4);
    let url = alice.put_blob(store_id, fx.deeper, "image/png", &later).await.unwrap();
    put_on_canvas(&alice, store_id, fx.deeper, &url).await;
    assert!(wait_until(Duration::from_secs(30), || holds(&bob, store_id, url.blob, &later)).await, "a reader gets a picture put in the share after it was shared");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}
