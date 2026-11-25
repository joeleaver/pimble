//! `cloudAddHostedStore` adds only what the signed-in account holds
//! (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 3", item h). Until
//! 2026-11-25 a store the accounts service listed no row for was added whole
//! and writable, and the link's first grant refresh then ended it at once as
//! "no longer shared". Now it is refused with a sentence, and nothing is made.
//!
//! The sharing tests' harness (`common`): `H` is a real hosted server in JWT
//! mode, the stub accounts service lists the rows and mints the tokens, and
//! each device is a real tokenless loopback server.

mod common;
use common::*;

use std::path::Path;
use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_core::{StoreAccess, StoreId, SyncState};
use pimble_server::{PimbleServer, ServerConfig};

/// A device that asks about its grant every second, in place of two minutes,
/// so the refresh a replica meets first comes within the test.
async fn device(dir: &Path) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig { grant_check_interval: Some(Duration::from_secs(1)), ..device_config(dir) });
    server.start().await.expect("device starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

async fn is_open_here(client: &PimbleClient, store_id: StoreId) -> bool {
    client.list_stores().await.expect("stores list").iter().any(|store| store.id == store_id)
}

/// Bob has no row for Alice's hosted store: adding it is refused, in a
/// sentence that says the account does not hold it, and no replica is made.
/// An id nobody hosts is refused the same way.
#[tokio::test]
async fn a_store_the_account_has_no_row_for_is_refused() {
    let env = spawn_env().await;
    let (mut alice_server, alice, alice_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, alice_dir.path()).await;

    let bob_dir = tempfile::tempdir().unwrap();
    let (mut bob_server, bob) = device(bob_dir.path()).await;
    env.sign_in(&bob, BOB).await;

    for store_id in [fx.store_id, StoreId::new()] {
        let refused = bob.cloud_add_hosted_store(store_id).await.expect_err("a store the account does not hold is not added");
        let said = refused.to_string();
        assert!(said.contains("does not hold store") && said.contains(&store_id.to_string()), "the refusal names the store and why: {said}");
        assert!(!is_open_here(&bob, store_id).await, "nothing was made for {store_id}");
    }

    bob_server.stop().await.unwrap();
    alice_server.stop().await.unwrap();
}

/// Alice's own store, added on her second device: the account's row says she
/// owns it, and the link's grant refreshes (one at connect, then every
/// second) leave it whole and writable.
#[tokio::test]
async fn a_store_with_a_row_is_not_ended_by_its_first_refresh() {
    let env = spawn_env().await;
    let (mut first_server, first, first_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &first, first_dir.path()).await;

    let second_dir = tempfile::tempdir().unwrap();
    let (mut second_server, second) = device(second_dir.path()).await;
    env.sign_in(&second, ALICE).await;
    second.cloud_add_hosted_store(fx.store_id).await.expect("a store the account owns is added");

    let synced = wait_until(Duration::from_secs(20), || async { matches!(second.get_store_sync(fx.store_id).await, Ok((_, SyncState::Synced { .. }))) }).await;
    assert!(synced, "the replica's link connects, and its first grant refresh has run");
    // Three more refreshes, then the replica is still held whole.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let held = second.get_store_sync_response(fx.store_id).await.expect("the replica's sync state");
    assert!(held.ended_roots.is_empty(), "nothing of it ended: {:?}", held.ended_roots);
    assert_eq!(held.access, StoreAccess::Full, "it is writable");
    let pulled = wait_until(Duration::from_secs(15), || async { node_text(&second, fx.store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "and it reads the store's notes");

    second_server.stop().await.unwrap();
    first_server.stop().await.unwrap();
}
