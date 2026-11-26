//! Large documents through the relay (docs/SYNC_LIMITS_CONTRACT.md).

use super::*;

/// No size stops a relayed store from syncing (docs/SYNC_LIMITS_CONTRACT.md):
/// a 20 MiB document goes from the owner to a member through the relay, and
/// a member's 20 MiB edit back, in parts. Every message through the relay is
/// under its 16 MiB (`relay_tunnel`'s `MAX_MESSAGE_BYTES`): an append or a
/// snapshot a part at a time, a fetch a page at a time, an announcement a
/// part at a time.
#[tokio::test]
async fn a_twenty_megabyte_document_goes_both_ways_through_the_relay() {
    fn noise(len: usize, seed: u64) -> String {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                if i % 9 == 8 { ' ' } else { (b'a' + (state % 26) as u8) as char }
            })
            .collect()
    }
    async fn set_blob(client: &PimbleClient, store_id: StoreId, node_id: NodeId, value: &str) {
        let value = serde_json::Value::String(value.to_string());
        client.edit_node(store_id, node_id, "painter", |doc, _| doc.set_data("blob", &value)).await.expect("the large edit is taken");
    }
    async fn blob_len(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> usize {
        match client.get_node(store_id, node_id).await {
            Ok(node) => NodeDoc::load(&node.content).ok().and_then(|doc| doc.data_json()["blob"].as_str().map(str::len)).unwrap_or(0),
            Err(_) => 0,
        }
    }

    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor)]).await;
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;

    let big = noise(20 * 1024 * 1024, 3);
    set_blob(&alice, store_id, fx.inside, &big).await;
    assert!(wait_until(Duration::from_secs(90), || async { blob_len(&bob, store_id, fx.inside).await == big.len() }).await, "the owner's 20 MiB document reaches the member");

    let back = noise(20 * 1024 * 1024, 4);
    set_blob(&bob, store_id, fx.leaf, &back).await;
    assert!(wait_until(Duration::from_secs(90), || async { blob_len(&alice, store_id, fx.leaf).await == back.len() }).await, "the member's 20 MiB edit reaches the owner");

    // A member who joins now reads both, from snapshots in parts, a page at
    // a time.
    let (mut c, carol, _c_dir) = {
        alice.cloud_share_invite(store_id, fx.shared, CAROL.0, MemberRole::Editor).await.unwrap();
        member_device(&env, CAROL, &fx).await
    };
    let both = wait_until(Duration::from_secs(90), || async { blob_len(&carol, store_id, fx.inside).await == big.len() && blob_len(&carol, store_id, fx.leaf).await == back.len() }).await;
    assert!(
        both,
        "a new member reads both: has {} and {} of {} bytes, waiting on {:?}",
        blob_len(&carol, store_id, fx.inside).await,
        blob_len(&carol, store_id, fx.leaf).await,
        big.len(),
        carol.get_store_sync_response(store_id).await.map(|r| r.waiting)
    );
    for device in [&alice, &bob, &carol] {
        assert!(device.get_store_sync_response(store_id).await.unwrap().waiting.is_empty(), "nothing waits");
    }

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}
