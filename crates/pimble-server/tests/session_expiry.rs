//! A Pimble Cloud session that is refused (it expired, or was signed out
//! elsewhere) stops every encrypted link from asking again until a sign-in,
//! and says so (2026-11-19: a desktop asked 58 times in a quarter of an hour,
//! while the status bar still read "Signed in", and nothing synced).

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_session_stops_the_links_says_so_and_a_sign_in_brings_them_back() {
    let env = spawn_env().await;
    let (_a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;

    // The session goes; the link learns it at its next connect.
    env.stub.refuse_sessions.store(true, Ordering::SeqCst);
    env.relay.cut();
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.relay.restore();
    let expired = wait_until(Duration::from_secs(20), || async {
        alice.cloud_status().await.ok().and_then(|s| s.connection).is_some_and(|c| c.session_expired)
    })
    .await;
    assert!(expired, "the status says the sign-in expired: {:?}", alice.cloud_status().await.ok().and_then(|s| s.connection));

    // No more asking while it stays so.
    let before = env.stub.token_calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(env.stub.token_calls.load(Ordering::SeqCst), before, "nothing asks for a token until someone signs in");

    // An edit made meanwhile waits, and goes once signed in again.
    let doc = pimble_rpc::VaultDocId::Node(fx.inside);
    let head = env.h_admin.vault_fetch(fx.store_id, doc.clone(), 0).await.unwrap().head;
    seed_content(&alice, fx.store_id, fx.inside, "edit", "WRITTEN-WHILE-EXPIRED").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(env.h_admin.vault_fetch(fx.store_id, doc.clone(), 0).await.unwrap().head, head, "nothing goes while expired");
    env.stub.refuse_sessions.store(false, Ordering::SeqCst);
    env.sign_in(&alice, ALICE).await;
    let back = wait_until(Duration::from_secs(20), || async {
        alice.cloud_status().await.ok().and_then(|s| s.connection).is_some_and(|c| !c.session_expired)
            && env.h_admin.vault_fetch(fx.store_id, doc.clone(), 0).await.is_ok_and(|f| f.head > head)
    })
    .await;
    assert!(back, "a sign-in clears it, the link connects again, and the edit made meanwhile goes");
}
