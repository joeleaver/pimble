//! Relaying a store, what cannot be relayed, and two members co-editing a
//! relayed folder while the owner comes and goes.

use super::*;

// ── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn relaying_a_store_uploads_nothing_and_keeps_its_twin_on_this_machine() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;

    // The accounts service holds a record, the owner's grant and the owner's
    // key envelope: account data. No name, tier relay, and it called nobody.
    {
        let s = env.stub.inner.lock().unwrap();
        assert_eq!(s.create_store_bodies.len(), 1);
        let body = &s.create_store_bodies[0];
        assert_eq!((body["name"].as_str(), body["kind"].as_str(), body["tier"].as_str(), body["store_id"].as_str()), (Some(""), Some("vault"), Some("relay"), Some(store_id.to_string().as_str())));
        assert!(!body.to_string().contains(STORE_NAME), "the owner's name for the store stays on this machine");
        let rows: Vec<_> = s.stores.iter().filter(|row| row.store_id == store_id.to_string()).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].name.as_str(), rows[0].tier.as_str(), rows[0].role.as_str()), ("", "relay", "owner"));
        assert_eq!(s.key_grants.get(&store_id.to_string()).map(Vec::len), Some(1), "the owner's own envelope of the store key");
    }
    env.assert_nothing_hosted(&[STORE_NAME, "PRIVATE-DIARY-TEXT", "socks and a map", &store_id.to_string()]).await;

    // The twin is on this machine, whole, and ciphertext.
    let twin = twin_dir(a_dir.path(), store_id);
    let whole = wait_until(Duration::from_secs(15), || async { twin_doc_count(a_dir.path(), store_id) == 6 }).await;
    assert!(whole, "every document of the store is on the twin: {}", twin_doc_count(a_dir.path(), store_id));
    for needle in [STORE_NAME, "PRIVATE-DIARY-TEXT", "socks and a map", "Holiday"] {
        assert!(!contains_bytes_recursive(&twin, needle.as_bytes()), "{needle:?} is readable in the twin");
    }

    // What the app is told: a vault link, relayed from here, and where
    // members reach the store (never where the link connects: that is this
    // process's own face, on a port that is new every run).
    let sync = alice.get_store_sync_response(store_id).await.unwrap();
    assert_eq!((sync.sync_mode, sync.relay), (StoreKind::Vault, RelaySide::Owner));
    assert_eq!(sync.remote.as_ref().map(|r| r.url.to_string()), Some(env.relay_url(store_id)));
    let listed = alice.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap();
    assert_eq!((listed.relay, listed.sync_mode, listed.name.as_str()), (RelaySide::Owner, StoreKind::Vault, STORE_NAME));
    let sync_json: serde_json::Value = serde_json::from_slice(&tfs::read(a_dir.path().join("a.pimble").join("sync.json")).unwrap()).unwrap();
    assert_eq!(sync_json["mode"].as_str(), Some("relay"));
    assert_eq!(sync_json["remote"]["url"].as_str(), Some(env.relay_url(store_id).as_str()), "the face's address, new every run, is never written down");

    let rows = alice.cloud_list_hosted_stores_response().await.unwrap();
    assert_eq!(rows.relayed, vec![store_id.to_string()]);
    assert_eq!(rows.tier_of(&store_id.to_string()), "relay");

    // Shared from here already; and its link is not something to unlink or
    // point elsewhere.
    assert_eq!(alice.cloud_relay_store(store_id).await.expect_err("relayed already").to_string(), pimble_server::relay_face::ALREADY_RELAYED_REFUSAL);
    assert_eq!(alice.set_store_sync_response(store_id, None).await.expect_err("not unlinked like that").to_string(), pimble_server::relay_face::RELAYED_LINK_REFUSAL);

    // An edit reaches the twin like any vault link's, still as ciphertext.
    write_text(&alice, store_id, fx.inside, "alice", "AND-A-TORCH").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!contains_bytes_recursive(&twin, b"AND-A-TORCH"));
    env.assert_nothing_hosted(&["AND-A-TORCH"]).await;

    // Closing the store takes it off the relay; opening it puts it back, on
    // the same twin.
    let created_at = twin_created_at(a_dir.path(), store_id).expect("the twin's manifest");
    alice.close_store(store_id).await.unwrap();
    assert!(wait_until(Duration::from_secs(10), || async { !env.stub.relay.serves(store_id) }).await, "a closed store is withdrawn from the relay");
    let reopened = alice.open_store(a_dir.path().join("a.pimble")).await.unwrap();
    assert_eq!(reopened.relay, RelaySide::Owner);
    assert!(wait_until(Duration::from_secs(20), || async { env.stub.relay.serves(store_id) }).await, "and announced again once it is open and its link has reconciled");
    assert_eq!(twin_created_at(a_dir.path(), store_id), Some(created_at), "the twin that was there is the twin that is used");

    a.stop().await.unwrap();
    assert!(wait_until(Duration::from_secs(10), || async { !env.stub.relay.serves(store_id) }).await, "a stopped server's tunnel is gone");
}

#[tokio::test]
async fn relaying_is_refused_for_what_cannot_be_shared_from_here() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;

    // Nobody signed in: nothing is asked of anyone.
    let (plain_id, _) = alice.create_store(a_dir.path().join("plain.pimble"), "Plain").await.unwrap();
    let refused = alice.cloud_relay_store(plain_id).await.expect_err("no account").to_string();
    assert!(refused.contains("no Pimble Cloud account is signed in"), "{refused}");
    assert_eq!(alice.cloud_stop_relaying(plain_id).await.expect_err("not relayed").to_string(), pimble_server::relay_face::NOT_RELAYED_REFUSAL);

    // A hosted store is shared from Pimble Cloud.
    env.sign_in(&alice, ALICE).await;
    let (hosted_id, _) = alice.create_store(a_dir.path().join("hosted.pimble"), "Hosted").await.unwrap();
    alice.cloud_host_store(hosted_id).await.unwrap();
    assert_eq!(alice.cloud_relay_store(hosted_id).await.expect_err("hosted").to_string(), pimble_server::relay_face::ALREADY_HOSTED_REFUSAL);

    // A replica of it, on another of the owner's devices, lives elsewhere.
    let (mut a2, alice2, a2_dir) = start_local_server().await;
    env.sign_in(&alice2, ALICE).await;
    alice2.cloud_add_hosted_store(hosted_id).await.expect("the owner's other device adds the hosted store");
    assert_eq!(alice2.cloud_relay_store(hosted_id).await.expect_err("a replica").to_string(), pimble_server::relay_face::REPLICA_REFUSAL);

    // None of it recorded anything, or started a face.
    {
        let s = env.stub.inner.lock().unwrap();
        assert!(s.create_store_bodies.iter().all(|body| body["tier"].as_str() != Some("relay")));
    }
    assert!(!tfs::exists(&a_dir.path().join("relay")) && !tfs::exists(&a2_dir.path().join("relay")), "no relay face was started for a refusal");

    a.stop().await.unwrap();
    a2.stop().await.unwrap();
}

#[tokio::test]
async fn two_members_co_edit_a_relayed_folders_tree_and_outlast_the_owner_going_away() {
    let env = spawn_env().await;
    env.holds_another_store(BOB);
    env.holds_another_store(CAROL);
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor), (CAROL, MemberRole::Editor)]).await;

    let (mut b, bob, b_dir) = member_device(&env, BOB, &fx).await;
    let (mut c, carol, c_dir) = member_device(&env, CAROL, &fx).await;
    for (client, dir) in [(&bob, &b_dir), (&carol, &c_dir)] {
        // A member's link's endpoint is the relay, and its replica holds the
        // share and nothing else.
        let sync = client.get_store_sync_response(store_id).await.unwrap();
        assert_eq!(sync.remote.as_ref().map(|r| r.url.to_string()), Some(env.relay_url(store_id)));
        assert_eq!((sync.relay, sync.sync_mode), (RelaySide::Member, StoreKind::Vault));
        assert!(matches!(sync.state, SyncState::Synced { .. }), "{:?}", sync.state);
        assert_eq!(child_ids(client, store_id, shared).await, vec![fx.inside, fx.deeper]);
        assert!(client.get_node(store_id, fx.private).await.is_err(), "a node outside the share is not held");
        assert!(!contains_bytes_recursive(dir.path(), b"PRIVATE-DIARY-TEXT"));
        assert!(!contains_bytes_recursive(dir.path(), STORE_NAME.as_bytes()), "the owner's store name never reaches a member");
    }
    assert!(env.stub.relay.piped.load(Ordering::SeqCst) >= 2, "both members came in through the relay");
    assert_eq!(env.stub.relay.wrong_token.load(Ordering::SeqCst), 0, "each with the token minted for this store alone");

    // The TREE, co-edited through the relay: create, text, rename, move,
    // delete, each by one member and seen by the other and by the owner.
    let ferry = bob.create_node(store_id, Some(fx.deeper), "document", "Ferry").await.unwrap();
    write_text(&bob, store_id, ferry, "bob", "ferry at nine").await;
    let created = wait_until(Duration::from_secs(20), || async {
        child_ids(&carol, store_id, fx.deeper).await == vec![fx.leaf, ferry]
            && node_text(&carol, store_id, ferry).await.contains("ferry at nine")
            && child_ids(&alice, store_id, fx.deeper).await == vec![fx.leaf, ferry]
            && node_text(&alice, store_id, ferry).await.contains("ferry at nine")
    })
    .await;
    assert!(created, "one member's create reaches the other member and the owner, readable");

    rename(&carol, store_id, ferry, "Night ferry").await;
    carol.move_node(store_id, ferry, shared, Place::Last).await.expect("a move inside the share");
    write_text(&carol, store_id, fx.inside, "carol", "and sunscreen").await;
    let followed = wait_until(Duration::from_secs(20), || async {
        node_title(&bob, store_id, ferry).await == "Night ferry"
            && child_ids(&bob, store_id, shared).await == vec![fx.inside, fx.deeper, ferry]
            && child_ids(&bob, store_id, fx.deeper).await == vec![fx.leaf]
            && node_text(&bob, store_id, fx.inside).await.contains("and sunscreen")
            && child_ids(&alice, store_id, shared).await == vec![fx.inside, fx.deeper, ferry]
            && node_title(&alice, store_id, ferry).await == "Night ferry"
    })
    .await;
    assert!(followed, "the other member's rename, move and text reach everyone: bob sees {:?}", child_ids(&bob, store_id, shared).await);

    bob.delete_node(store_id, fx.leaf).await.expect("a delete inside the share");
    let deleted = wait_until(Duration::from_secs(20), || async {
        child_ids(&carol, store_id, fx.deeper).await.is_empty() && carol.get_node(store_id, fx.leaf).await.is_err() && child_ids(&alice, store_id, fx.deeper).await.is_empty()
    })
    .await;
    assert!(deleted, "a member's delete reaches the other member and the owner");
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private], "and nothing a member did touched the rest of the owner's tree");

    // And the owner's own edits reach both.
    let hotel = alice.create_node(store_id, Some(shared), "document", "Hotel").await.unwrap();
    write_text(&alice, store_id, hotel, "alice", "two nights").await;
    let reached = wait_until(Duration::from_secs(30), || async {
        node_text(&bob, store_id, hotel).await.contains("two nights") && node_text(&carol, store_id, hotel).await.contains("two nights")
    })
    .await;
    assert!(reached, "the owner's create reaches the members once the scope names it");
    env.assert_nothing_hosted(&["ferry at nine", "two nights", "Night ferry"]).await;

    // The owner's computer goes off. Members' links go Offline, and what
    // their reconnects are told is `owner offline`.
    let created_at = twin_created_at(a_dir.path(), store_id).expect("the twin's manifest");
    drop(alice);
    a.stop().await.unwrap();
    let offline = wait_until(Duration::from_secs(15), || async {
        matches!(link_state(&bob, store_id).await, SyncState::Offline) && matches!(link_state(&carol, store_id).await, SyncState::Offline)
    })
    .await;
    assert!(offline, "with the owner gone, members' links are offline: {:?} {:?}", link_state(&bob, store_id).await, link_state(&carol, store_id).await);
    // And they say why: the relay answered that the owner is not there. It
    // is the only reason a member's app may blame on somebody else's
    // computer (the owner's own link never says it).
    let said_why = wait_until(Duration::from_secs(15), || async {
        bob.get_store_sync_response(store_id).await.is_ok_and(|a| a.owner_offline) && carol.get_store_sync_response(store_id).await.is_ok_and(|a| a.owner_offline)
    })
    .await;
    assert!(said_why, "a member's sync answer names the reason");
    assert!(wait_until(Duration::from_secs(15), || async { env.stub.relay.owner_offline.load(Ordering::SeqCst) >= 2 }).await, "and their reconnects are answered `owner offline`");
    assert_eq!(env.stub.relay.open_conns(), 0, "the relay holds nothing once they are gone");

    // They keep reading and editing what they have; their edits wait on
    // their own devices, and they do not see each other.
    let taxi = bob.create_node(store_id, Some(shared), "document", "Taxi").await.expect("a member edits offline");
    write_text(&bob, store_id, taxi, "bob", "taxi at six").await;
    rename(&carol, store_id, fx.deeper, "Travel").await;
    write_text(&carol, store_id, hotel, "carol", "with breakfast").await;
    carol.delete_node(store_id, ferry).await.expect("a member deletes offline");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(carol.get_node(store_id, taxi).await.is_err(), "two members do not see each other while the owner is away");
    assert_ne!(node_title(&bob, store_id, fx.deeper).await, "Travel");
    // `Offline`, and staying there: a try that meets `owner offline` is not
    // a connection being made, so the state does not flap at every one.
    for _ in 0..40 {
        assert!(matches!(link_state(&bob, store_id).await, SyncState::Offline) && matches!(link_state(&carol, store_id).await, SyncState::Offline));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // The owner is back: the same twin, and everything converges.
    let (mut a, alice) = start_device_in(a_dir.path()).await;
    let reopened = alice.open_store(a_dir.path().join("a.pimble")).await.expect("the store opens again");
    assert_eq!(reopened.relay, RelaySide::Owner);
    let converged = wait_until(Duration::from_secs(90), || async {
        let want = sorted(vec![fx.inside, fx.deeper, hotel, taxi]);
        sorted(child_ids(&alice, store_id, shared).await) == want
            && sorted(child_ids(&bob, store_id, shared).await) == want
            && sorted(child_ids(&carol, store_id, shared).await) == want
            && node_text(&alice, store_id, taxi).await.contains("taxi at six")
            && node_text(&carol, store_id, taxi).await.contains("taxi at six")
            && node_title(&alice, store_id, fx.deeper).await == "Travel"
            && node_title(&bob, store_id, fx.deeper).await == "Travel"
            && node_text(&alice, store_id, hotel).await.contains("with breakfast")
            && node_text(&bob, store_id, hotel).await.contains("with breakfast")
    })
    .await;
    assert!(
        converged,
        "alice {:?}, bob {:?}, carol {:?}",
        sorted(child_ids(&alice, store_id, shared).await),
        sorted(child_ids(&bob, store_id, shared).await),
        sorted(child_ids(&carol, store_id, shared).await)
    );
    assert_eq!(twin_created_at(a_dir.path(), store_id), Some(created_at), "the twin was reused, not built again");
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private]);
    env.assert_nothing_hosted(&["taxi at six", "with breakfast"]).await;

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}
