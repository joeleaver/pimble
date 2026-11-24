//! A plain store hosted on Pimble Cloud, replicated through the signed-in
//! account (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 1"):
//! `cloudAddHostedStore` makes a replica kept in step by a plain sync link
//! that connects to the hosted server with a token minted from the account
//! before every connect, and reads the account's grant at every connect and
//! every `grant_check_interval` while connected.
//!
//! The same harness as the sharing tests (`common`): `H` is a real hosted
//! server in JWT mode, the stub accounts service mints its tokens, and each
//! device is a real tokenless loopback server.

mod common;
use common::*;
use pimble_host::disk::testfs as tfs;

use std::path::Path;
use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreAccess, StoreId, StoreKind, SyncState};
use pimble_server::{PimbleServer, ServerConfig};

/// A plain store on `H` with a folder, a note in it with text, and a note
/// beside the folder; held whole by Alice (`owner`).
struct PlainFixture {
    store_id: StoreId,
    root: NodeId,
    garden: NodeId,
    tomatoes: NodeId,
    accounts: NodeId,
}

async fn plain_hosted_store(env: &Env) -> PlainFixture {
    let path = env.h_stores_dir.join(format!("{}.pimble", uuid::Uuid::new_v4()));
    let (store_id, root) = env.h_admin.create_store_with(&path, "Allotment", StoreKind::Plain, None).await.expect("H creates a plain store");
    let garden = env.h_admin.create_node(store_id, Some(root), "folder", "Garden").await.unwrap();
    let tomatoes = env.h_admin.create_node(store_id, Some(garden), "document", "Tomatoes").await.unwrap();
    seed_content(&env.h_admin, store_id, tomatoes, "seed", "stake them in May").await;
    let accounts = env.h_admin.create_node(store_id, Some(root), "document", "Accounts").await.unwrap();
    seed_content(&env.h_admin, store_id, accounts, "seed", "the shed cost forty pounds").await;
    grant(env, ALICE.0, store_id, "owner", None, "Allotment");
    PlainFixture { store_id, root, garden, tomatoes, accounts }
}

/// A row on the accounts service: `email` holds `store_id` as `role`, the
/// whole of it or the share rooted at `root`.
fn grant(env: &Env, email: &str, store_id: StoreId, role: &str, root: Option<NodeId>, name: &str) {
    let user_id = env.user_id(email);
    let mut s = env.stub.inner.lock().unwrap();
    s.stores.retain(|row| !(row.user_id == user_id && row.store_id == store_id.to_string() && row.root == root.map(|r| r.to_string())));
    s.stores.push(StubStoreRow {
        user_id,
        store_id: store_id.to_string(),
        name: name.to_string(),
        kind: "plain".to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        role: role.to_string(),
        root: root.map(|r| r.to_string()),
        shared_by: root.map(|_| ALICE.0.to_string()),
    });
}

/// A device that asks about its grant every second, in place of two minutes.
async fn device(dir: &Path) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig { grant_check_interval: Some(Duration::from_secs(1)), ..device_config(dir) });
    server.start().await.expect("device starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

async fn synced(client: &PimbleClient, store_id: StoreId) -> bool {
    wait_until(Duration::from_secs(20), || async { matches!(client.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. }))) }).await
}

#[tokio::test]
async fn a_plain_hosted_store_is_replicated_through_the_account_both_ways() {
    let env = spawn_env().await;
    let fx = plain_hosted_store(&env).await;
    let a_dir = tempfile::tempdir().unwrap();
    let (mut a, alice) = device(a_dir.path()).await;
    env.sign_in(&alice, ALICE).await;

    let added = alice.cloud_add_hosted_store(fx.store_id).await.expect("a plain hosted store is added");
    assert_eq!(added.sync_mode, StoreKind::Plain, "a plain link, not a vault one");
    assert!(added.roots.is_empty(), "held whole: a whole replica");
    let pulled = wait_until(Duration::from_secs(20), || async { node_text(&alice, fx.store_id, fx.tomatoes).await.contains("stake them in May") }).await;
    assert!(pulled, "the hosted documents arrive");
    let root = alice.list_stores().await.unwrap().into_iter().find(|s| s.id == fx.store_id).unwrap().root_node_id;
    assert_eq!(root, fx.root, "the manifest's placeholder root gives way to the documents' root");
    assert_eq!(child_ids(&alice, fx.store_id, fx.root).await, vec![fx.garden, fx.accounts]);

    // Reached with the account's token, never a saved credential.
    let sync_json = tfs::read_to_string(added.local_path().unwrap().join("sync.json")).unwrap();
    assert!(sync_json.contains("\"via_account\": true") || sync_json.contains("\"via_account\":true"), "{sync_json}");
    assert!(!sync_json.contains("eyJ"), "no token in sync.json: {sync_json}");
    assert!(synced(&alice, fx.store_id).await);

    // This device's edit reaches the hosted server, and the hosted server's
    // reaches this device.
    let beans = alice.create_node(fx.store_id, Some(fx.garden), "document", "Beans").await.unwrap();
    assert!(wait_until(Duration::from_secs(20), || async { env.h_admin.get_node(fx.store_id, beans).await.is_ok() }).await, "the new note reaches H");
    rename(&env.h_admin, fx.store_id, fx.tomatoes, "Tomatoes, 2027").await;
    assert!(wait_until(Duration::from_secs(20), || async { node_title(&alice, fx.store_id, fx.tomatoes).await == "Tomatoes, 2027" }).await, "H's rename reaches the device");

    // Started again, the link comes back from `sync.json` through the
    // account, and catches up with what changed while it was off.
    a.stop().await.expect("the device stops");
    rename(&env.h_admin, fx.store_id, fx.accounts, "Accounts, audited").await;
    let (_a, alice) = device(a_dir.path()).await;
    if !alice.list_stores().await.unwrap().iter().any(|s| s.id == fx.store_id) {
        alice.open_store(added.local_path().unwrap()).await.expect("the replica opens again");
    }
    assert!(wait_until(Duration::from_secs(20), || async { node_title(&alice, fx.store_id, fx.accounts).await == "Accounts, audited" }).await, "what changed while it was off arrives");
}

#[tokio::test]
async fn a_reader_pushes_nothing_until_the_owner_makes_them_an_editor() {
    let env = spawn_env().await;
    let fx = plain_hosted_store(&env).await;
    grant(&env, BOB.0, fx.store_id, "reader", None, "Allotment");
    let b_dir = tempfile::tempdir().unwrap();
    let (_b, bob) = device(b_dir.path()).await;
    env.sign_in(&bob, BOB).await;

    let added = bob.cloud_add_hosted_store(fx.store_id).await.expect("a reader adds the store");
    assert_eq!(added.access, StoreAccess::Read);
    assert!(wait_until(Duration::from_secs(20), || async { node_text(&bob, fx.store_id, fx.tomatoes).await.contains("stake them in May") }).await);
    assert!(synced(&bob, fx.store_id).await);
    let refused = bob.create_node(fx.store_id, Some(fx.garden), "document", "Weeds").await.expect_err("a reader's replica takes no edits").to_string();
    assert!(refused.contains(StoreAccess::READ_ONLY_REFUSAL), "{refused}");

    // The owner's edits still arrive, and the link stays up: nothing it
    // holds is pushed (a push would be refused by H and logged).
    rename(&env.h_admin, fx.store_id, fx.tomatoes, "Tomatoes, staked").await;
    assert!(wait_until(Duration::from_secs(20), || async { node_title(&bob, fx.store_id, fx.tomatoes).await == "Tomatoes, staked" }).await);

    // Made an editor: the grant check sees it, the link connects again with
    // a token that says so, and the replica takes edits that reach H.
    grant(&env, BOB.0, fx.store_id, "editor", None, "Allotment");
    let promoted = wait_until(Duration::from_secs(20), || async { bob.get_store_sync_response(fx.store_id).await.is_ok_and(|held| held.access == StoreAccess::Full) }).await;
    assert!(promoted, "the role the owner changed is picked up while connected");
    assert!(synced(&bob, fx.store_id).await);
    let weeds = bob.create_node(fx.store_id, Some(fx.garden), "document", "Weeds").await.expect("an editor's replica takes edits");
    assert!(wait_until(Duration::from_secs(20), || async { env.h_admin.get_node(fx.store_id, weeds).await.is_ok() }).await, "and they reach H");
}

#[tokio::test]
async fn a_share_of_a_plain_hosted_store_is_a_partial_replica() {
    let env = spawn_env().await;
    let fx = plain_hosted_store(&env).await;
    grant(&env, BOB.0, fx.store_id, "editor", Some(fx.garden), "The garden");
    let b_dir = tempfile::tempdir().unwrap();
    let (_b, bob) = device(b_dir.path()).await;
    env.sign_in(&bob, BOB).await;

    let added = bob.cloud_add_hosted_store(fx.store_id).await.expect("a member adds the share");
    assert_eq!(added.roots, vec![fx.garden], "rooted at the shared node");
    assert!(wait_until(Duration::from_secs(20), || async { node_text(&bob, fx.store_id, fx.tomatoes).await.contains("stake them in May") }).await, "the share arrives");
    assert!(synced(&bob, fx.store_id).await);
    assert!(bob.get_node(fx.store_id, fx.accounts).await.is_err(), "what is outside the share never reaches the member");

    let peas = bob.create_node(fx.store_id, Some(fx.garden), "document", "Peas").await.expect("an editor of the share writes in it");
    assert!(wait_until(Duration::from_secs(20), || async { child_ids(&env.h_admin, fx.store_id, fx.garden).await.contains(&peas) }).await, "the member's note reaches H under the shared folder");

    // Removed from the share: the root is recorded as ended, and the
    // replica takes no more edits under it.
    {
        let user_id = env.user_id(BOB.0);
        env.stub.inner.lock().unwrap().stores.retain(|row| row.user_id != user_id);
    }
    let ended = wait_until(Duration::from_secs(20), || async { bob.get_store_sync_response(fx.store_id).await.is_ok_and(|held| held.ended_roots == vec![fx.garden]) }).await;
    assert!(ended, "a removal is recorded as an ended share");
    assert!(bob.create_node(fx.store_id, Some(fx.garden), "document", "Too late").await.is_err());
}
