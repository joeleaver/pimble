//! A twin built again, a reader through the relay, and stopping.

use super::*;

#[tokio::test]
async fn a_deleted_twin_is_built_again_and_members_converge_on_it() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor), (CAROL, MemberRole::Editor)]).await;
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (mut c, carol, c_dir) = member_device(&env, CAROL, &fx).await;

    // Enough history that the old logs' numbers are well past where the new
    // ones will start.
    for round in 0..4 {
        write_text(&bob, store_id, fx.inside, "bob", &format!("bob's line {round}")).await;
        write_text(&carol, store_id, fx.inside, "carol", &format!("carol's line {round}")).await;
    }
    let settled = wait_until(Duration::from_secs(20), || async {
        let text = node_text(&alice, store_id, fx.inside).await;
        text.contains("bob's line 3") && text.contains("carol's line 3") && node_text(&bob, store_id, fx.inside).await.contains("carol's line 3")
    })
    .await;
    assert!(settled);

    // The owner goes away; each member edits offline; and the twin is
    // deleted: it is derived and disposable.
    let created_at = twin_created_at(a_dir.path(), store_id).expect("the twin's manifest");
    drop(alice);
    a.stop().await.unwrap();
    assert!(wait_until(Duration::from_secs(15), || async { matches!(link_state(&bob, store_id).await, SyncState::Offline) && matches!(link_state(&carol, store_id).await, SyncState::Offline) }).await);
    let taxi = bob.create_node(store_id, Some(shared), "document", "Taxi").await.unwrap();
    write_text(&bob, store_id, taxi, "bob", "taxi at six").await;
    write_text(&carol, store_id, fx.inside, "carol", "OFFLINE-SUNHAT").await;
    rename(&carol, store_id, fx.deeper, "Travel").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    tfs::remove_dir_all(twin_dir(a_dir.path(), store_id)).expect("the twin is deleted while its server is stopped");

    // Back: the link builds the twin again from the store, the members read
    // the new logs from the start and offer what they hold, and everyone
    // ends up with everything.
    let (mut a, alice) = start_device_in(a_dir.path()).await;
    alice.open_store(a_dir.path().join("a.pimble")).await.expect("the store opens again");
    let converged = wait_until(Duration::from_secs(120), || async {
        let want = sorted(vec![fx.inside, fx.deeper, taxi]);
        sorted(child_ids(&alice, store_id, shared).await) == want
            && sorted(child_ids(&carol, store_id, shared).await) == want
            && node_text(&alice, store_id, taxi).await.contains("taxi at six")
            && node_text(&carol, store_id, taxi).await.contains("taxi at six")
            && node_text(&alice, store_id, fx.inside).await.contains("OFFLINE-SUNHAT")
            && node_text(&bob, store_id, fx.inside).await.contains("OFFLINE-SUNHAT")
            && node_title(&alice, store_id, fx.deeper).await == "Travel"
            && node_title(&bob, store_id, fx.deeper).await == "Travel"
    })
    .await;
    assert!(
        converged,
        "alice: {:?} / {:?}; bob: {:?}; carol: {:?}",
        sorted(child_ids(&alice, store_id, shared).await),
        node_text(&alice, store_id, fx.inside).await,
        node_title(&bob, store_id, fx.deeper).await,
        sorted(child_ids(&carol, store_id, shared).await)
    );
    let rebuilt = twin_created_at(a_dir.path(), store_id).expect("a twin again");
    assert_ne!(rebuilt, created_at, "another twin, with logs of its own");
    assert!(twin_doc_count(a_dir.path(), store_id) >= 7, "holding every document again: {}", twin_doc_count(a_dir.path(), store_id));
    assert!(!contains_bytes_recursive(&twin_dir(a_dir.path(), store_id), b"OFFLINE-SUNHAT"), "as ciphertext");

    // And it goes on working: a live edit each way on the new logs.
    write_text(&alice, store_id, taxi, "alice", "make it half past").await;
    write_text(&bob, store_id, fx.inside, "bob", "AFTER-THE-REBUILD").await;
    let live = wait_until(Duration::from_secs(30), || async {
        node_text(&carol, store_id, taxi).await.contains("make it half past") && node_text(&carol, store_id, fx.inside).await.contains("AFTER-THE-REBUILD") && node_text(&alice, store_id, fx.inside).await.contains("AFTER-THE-REBUILD")
    })
    .await;
    assert!(live, "edits made after the rebuild reach everyone");

    // A member who is away while the new logs grow comes back to entries
    // whose numbers are all below where it had read the old logs to. It
    // reads them: what it kept of the old logs went when it met the new
    // twin's epoch.
    drop(carol);
    c.stop().await.unwrap();
    write_text(&bob, store_id, fx.inside, "bob", "WHILE-CAROL-WAS-AWAY").await;
    rename(&alice, store_id, taxi, "Taxi to the ferry").await;
    assert!(wait_until(Duration::from_secs(20), || async { node_text(&alice, store_id, fx.inside).await.contains("WHILE-CAROL-WAS-AWAY") && node_title(&bob, store_id, taxi).await == "Taxi to the ferry" }).await);
    let (mut c, carol) = start_device_in(c_dir.path()).await;
    carol.open_store(c_dir.path().join("replicas").join(format!("{store_id}.pimble"))).await.expect("the member's replica opens again");
    let caught_up = wait_until(Duration::from_secs(30), || async {
        node_text(&carol, store_id, fx.inside).await.contains("WHILE-CAROL-WAS-AWAY") && node_title(&carol, store_id, taxi).await == "Taxi to the ferry"
    })
    .await;
    assert!(caught_up, "what was appended to the new logs while a member was away reaches it when it is back");
    env.assert_nothing_hosted(&["OFFLINE-SUNHAT", "taxi at six"]).await;

    a.stop().await.unwrap();
    b.stop().await.unwrap();
    c.stop().await.unwrap();
}

#[tokio::test]
async fn through_the_relay_a_reader_cannot_write_and_nothing_outside_the_share_is_reached() {
    let env = spawn_env().await;
    env.holds_another_store(BOB);
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    share_with(&alice, &fx, &[(BOB, MemberRole::Reader), (CAROL, MemberRole::Editor)]).await;

    // Bob's replica: read, and refused every write here as it would be there.
    let (mut b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    assert_eq!(bob.list_stores().await.unwrap().iter().find(|s| s.id == store_id).unwrap().access, StoreAccess::Read);
    let refused = |result: Result<(), pimble_client::ClientError>| assert_eq!(result.expect_err("a reader's replica refuses writes").to_string(), StoreAccess::READ_ONLY_REFUSAL);
    refused(bob.create_node(store_id, Some(shared), "document", "no").await.map(|_| ()));
    refused(bob.delete_node(store_id, fx.inside).await);
    let changes = base64::engine::general_purpose::STANDARD.encode(NodeDoc::from_plain_text("no").unwrap().save());
    refused(bob.apply_edit(store_id, fx.inside, "bob", EditOperation::IncrementalChanges { changes }).await);
    write_text(&alice, store_id, fx.inside, "alice", "and sunscreen").await;
    assert!(wait_until(Duration::from_secs(15), || async { node_text(&bob, store_id, fx.inside).await.contains("and sunscreen") }).await, "and still receives everything");

    // The relay takes the token minted for this store and no other.
    let (bob_token, bob_general) = env.tokens(BOB, store_id);
    let refused_before = env.stub.relay.wrong_token.load(Ordering::SeqCst);
    assert!(PimbleClient::connect_with_auth(env.relay_url(store_id), &AuthMethod::Bearer { token: bob_general }).await.is_err());
    assert_eq!(env.stub.relay.wrong_token.load(Ordering::SeqCst), refused_before + 1, "the account's general token is refused before anything reaches the owner");

    // Straight at the owner's face, through the relay, as a reader: the
    // scope is listed and read, nothing is written, nothing else is reached.
    let reader = PimbleClient::connect_with_auth(env.relay_url(store_id), &AuthMethod::Bearer { token: bob_token }).await.expect("a member's connection through the relay");
    let listed: HashSet<VaultDocId> = reader.vault_list_docs(store_id).await.expect("the scope is listed").into_iter().map(|d| d.doc_id).collect();
    let scope: HashSet<VaultDocId> = [shared, fx.inside, fx.deeper, fx.leaf].into_iter().map(VaultDocId::Node).collect();
    assert_eq!(listed, scope, "a member is listed the share and nothing else");
    assert!(reader.vault_fetch(store_id, VaultDocId::Node(fx.inside), 0).await.is_ok());
    let blob = URL_SAFE_NO_PAD.encode(b"PB-not-really");
    assert_eq!(reader.vault_append(store_id, VaultDocId::Node(fx.inside), blob.clone()).await.expect_err("a reader's append").to_string(), StoreAccess::READ_ONLY_REFUSAL);
    for outside in [fx.private, fx.root_id] {
        let refusal = reader.vault_fetch(store_id, VaultDocId::Node(outside), 0).await.expect_err("outside the share").to_string();
        assert!(refusal.contains(pimble_server::NO_GRANT_FOR_DOCUMENT), "{refusal}");
    }

    // As an editor: the share is written, and what is outside it is no more
    // reachable than for a reader.
    let (carol_token, _) = env.tokens(CAROL, store_id);
    let editor = PimbleClient::connect_with_auth(env.relay_url(store_id), &AuthMethod::Bearer { token: carol_token }).await.expect("an editor's connection through the relay");
    for outside in [fx.private, fx.root_id] {
        let refusal = editor.vault_fetch(store_id, VaultDocId::Node(outside), 0).await.expect_err("outside the share").to_string();
        assert!(refusal.contains(pimble_server::NO_GRANT_FOR_DOCUMENT), "{refusal}");
        let refusal = editor.vault_append(store_id, VaultDocId::Node(outside), blob.clone()).await.expect_err("outside the share").to_string();
        assert!(refusal.contains(pimble_server::NO_GRANT_FOR_DOCUMENT), "{refusal}");
    }
    // The relay face is a vault and a JWT server like the hosted one: no
    // plain RPC answers, and no lifecycle RPC is a member's to call.
    assert!(editor.get_node(store_id, fx.inside).await.is_err());
    assert!(editor.close_store(store_id).await.is_err());
    assert!(editor.delete_vault_store(store_id).await.is_err());
    assert!(alice.get_node(store_id, fx.private).await.is_ok(), "and the owner's store is as it was");

    a.stop().await.unwrap();
    b.stop().await.unwrap();
}

#[tokio::test]
async fn stopping_is_refused_while_a_share_stands_and_undoes_everything_after() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = relayed_fixture(&env, &alice, a_dir.path()).await;
    let (store_id, shared) = (fx.store_id, fx.shared);
    share_with(&alice, &fx, &[(BOB, MemberRole::Editor)]).await;
    let store_dir = a_dir.path().join("a.pimble");

    // A share stands: refused, and nothing has changed.
    assert_eq!(alice.cloud_stop_relaying(store_id).await.expect_err("a share is in place").to_string(), pimble_server::relay_face::HAS_SHARES_REFUSAL);
    assert_eq!(alice.get_store_sync_response(store_id).await.unwrap().relay, RelaySide::Owner);
    assert!(tfs::exists(&twin_dir(a_dir.path(), store_id)) && env.stub.relay.serves(store_id));
    assert!(env.stub.inner.lock().unwrap().stores.iter().any(|row| row.store_id == store_id.to_string() && row.root.is_none()));

    // The share stopped, the way back is open: the record, the tunnel's
    // entry, the link, the twin and `sync.json` all go.
    alice.cloud_stop_sharing(store_id, shared).await.expect("the share is stopped first");
    alice.cloud_stop_relaying(store_id).await.expect("then the store is no longer shared from here");
    let sync = alice.get_store_sync_response(store_id).await.unwrap();
    assert!(sync.remote.is_none() && sync.relay == RelaySide::None && sync.sync_mode == StoreKind::Plain, "{sync:?}");
    assert!(!tfs::exists(&twin_dir(a_dir.path(), store_id)), "the twin is deleted");
    let state_dir = a_dir.path().join("stores").join(store_id.to_string());
    let record: serde_json::Value = tfs::read(state_dir.join("sync-state.json")).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    assert!(!tfs::exists(&store_dir.join("sync.json")) && record.get("vault").is_none() && record.get("link").is_none() && record.get("convergence").is_none(), "{record}");
    {
        let s = env.stub.inner.lock().unwrap();
        assert!(!s.stores.iter().any(|row| row.store_id == store_id.to_string()), "the accounts service's record is removed");
        assert!(!s.key_grants.contains_key(&store_id.to_string()));
    }
    assert!(wait_until(Duration::from_secs(10), || async { !env.stub.relay.serves(store_id) }).await, "and the relay serves it no more");
    assert_eq!(alice.cloud_stop_relaying(store_id).await.expect_err("already stopped").to_string(), pimble_server::relay_face::NOT_RELAYED_REFUSAL);

    // The store itself is untouched, and is an unshared, unhosted store
    // again: sharing says what it needs, and offers nothing by itself.
    assert_eq!(child_ids(&alice, store_id, fx.root_id).await, vec![shared, fx.private]);
    assert!(node_text(&alice, store_id, fx.private).await.contains("PRIVATE-DIARY-TEXT"));
    assert_eq!(
        alice.cloud_share_node(store_id, shared, "Again").await.expect_err("neither hosted nor relayed").to_string(),
        "Sharing needs this store hosted on Pimble Cloud or shared from this computer."
    );

    // And it can be shared from here again, on a twin of its own.
    alice.cloud_relay_store(store_id).await.expect("relayed again");
    let again = wait_until(Duration::from_secs(20), || async { env.stub.relay.serves(store_id) && twin_doc_count(a_dir.path(), store_id) == 6 }).await;
    assert!(again, "announced again, on a whole twin: {}", twin_doc_count(a_dir.path(), store_id));
    env.assert_nothing_hosted(&[STORE_NAME, "PRIVATE-DIARY-TEXT"]).await;

    a.stop().await.unwrap();
}
