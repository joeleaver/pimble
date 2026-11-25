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

    // Started again. A server opens no store by itself: which stores are
    // open is its client's (the desktop app's open-store list, the worker's
    // replicas folder), so the replica is opened by its path once, as they
    // open it, and nothing else is asked. The link comes back from
    // `sync.json` through the account on its own, and catches up with what
    // changed while it was off.
    a.stop().await.expect("the device stops");
    rename(&env.h_admin, fx.store_id, fx.accounts, "Accounts, audited").await;
    let (_a, alice) = device(a_dir.path()).await;
    assert!(!alice.list_stores().await.unwrap().iter().any(|s| s.id == fx.store_id), "nothing is open before its client opens it");
    let reopened = alice.open_store(added.local_path().unwrap()).await.expect("the replica opens again");
    assert_eq!(reopened.sync_mode, StoreKind::Plain);
    assert!(wait_until(Duration::from_secs(20), || async { node_title(&alice, fx.store_id, fx.accounts).await == "Accounts, audited" }).await, "what changed while it was off arrives");
    assert!(synced(&alice, fx.store_id).await, "the link restarted from sync.json alone");
}

#[tokio::test]
async fn a_reader_pushes_nothing_until_the_owner_makes_them_an_editor() {
    let env = spawn_env().await;
    let fx = plain_hosted_store(&env).await;
    grant(&env, BOB.0, fx.store_id, "reader", None, "Allotment");
    let b_dir = tempfile::tempdir().unwrap();
    let (mut b, bob) = device(b_dir.path()).await;
    env.sign_in(&bob, BOB).await;

    let added = bob.cloud_add_hosted_store(fx.store_id).await.expect("a reader adds the store");
    assert_eq!(added.access, StoreAccess::Read);
    assert!(wait_until(Duration::from_secs(20), || async { node_text(&bob, fx.store_id, fx.tomatoes).await.contains("stake them in May") }).await);
    assert!(synced(&bob, fx.store_id).await);
    let refused = bob.create_node(fx.store_id, Some(fx.garden), "document", "Weeds").await.expect_err("a reader's replica takes no edits").to_string();
    assert!(refused.contains(StoreAccess::READ_ONLY_REFUSAL), "{refused}");

    // The owner's edits still arrive, and the link stays up.
    rename(&env.h_admin, fx.store_id, fx.tomatoes, "Tomatoes, staked").await;
    assert!(wait_until(Duration::from_secs(20), || async { node_title(&bob, fx.store_id, fx.tomatoes).await == "Tomatoes, staked" }).await);

    // Nothing is pushed. H is made to take Bob's writes (his tokens say
    // editor while the rows still say reader), and a change slips onto the
    // replica's disk while the device is off, as a sync tool or an old
    // version could leave one: the reconnect's reconcile and the live loop
    // send none of it, so H never has it.
    env.set_token_role(BOB.0, fx.store_id, "editor");
    b.stop().await.expect("the device stops");
    let replica = added.local_path().unwrap().clone();
    {
        let mut on_disk = pimble_store::LocalStore::open(&replica).await.expect("the replica opens on its own");
        on_disk.node_doc(fx.tomatoes).unwrap().append_markdown("slipped in while off").unwrap();
        on_disk.flush().await.unwrap();
    }
    let (_b, bob) = device(b_dir.path()).await;
    bob.open_store(&replica).await.expect("the replica opens again");
    assert!(node_text(&bob, fx.store_id, fx.tomatoes).await.contains("slipped in while off"));
    assert!(synced(&bob, fx.store_id).await, "the reconcile ran");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!node_text(&env.h_admin, fx.store_id, fx.tomatoes).await.contains("slipped in while off"), "a reader's replica sent nothing to H");

    // Made an editor: the grant check sees it, the link connects again with
    // a token that says so, and the replica takes edits that reach H, the
    // one it held back too.
    grant(&env, BOB.0, fx.store_id, "editor", None, "Allotment");
    let promoted = wait_until(Duration::from_secs(20), || async { bob.get_store_sync_response(fx.store_id).await.is_ok_and(|held| held.access == StoreAccess::Full) }).await;
    assert!(promoted, "the role the owner changed is picked up while connected");
    assert!(synced(&bob, fx.store_id).await);
    let weeds = bob.create_node(fx.store_id, Some(fx.garden), "document", "Weeds").await.expect("an editor's replica takes edits");
    assert!(wait_until(Duration::from_secs(20), || async { env.h_admin.get_node(fx.store_id, weeds).await.is_ok() }).await, "and they reach H");
    assert!(
        wait_until(Duration::from_secs(20), || async { node_text(&env.h_admin, fx.store_id, fx.tomatoes).await.contains("slipped in while off") }).await,
        "what a reader held back goes once they may write"
    );
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

/// Whether `store_id`'s whole replica has ended at its own root `root`, read
/// only.
async fn whole_ended(client: &PimbleClient, store_id: StoreId, root: NodeId) -> bool {
    client.get_store_sync_response(store_id).await.is_ok_and(|held| held.ended_roots == vec![root] && held.access == StoreAccess::Read)
}

/// Whether `store_id`'s whole replica is held as an editor's, nothing ended.
async fn held_whole(client: &PimbleClient, store_id: StoreId) -> bool {
    client.get_store_sync_response(store_id).await.is_ok_and(|held| held.ended_roots.is_empty() && held.access == StoreAccess::Full)
}

/// A whole replica whose account holds the store no more behaves as a
/// share's replica does once every share ended (Joe, 2026-09-21): its own
/// root is recorded as ended, it takes no edits (none would ever leave the
/// device), its row says "no longer shared", and holding it again undoes
/// that. The same for a removal and for another account signed in here.
#[tokio::test]
async fn a_whole_replica_whose_grant_is_gone_ends_like_a_share() {
    let env = spawn_env().await;
    let fx = plain_hosted_store(&env).await;
    grant(&env, BOB.0, fx.store_id, "editor", None, "Allotment");
    let b_dir = tempfile::tempdir().unwrap();
    let (_b, bob) = device(b_dir.path()).await;
    env.sign_in(&bob, BOB).await;
    bob.cloud_add_hosted_store(fx.store_id).await.expect("an editor adds the whole store");
    assert!(synced(&bob, fx.store_id).await);
    bob.create_node(fx.store_id, Some(fx.garden), "document", "Peas").await.expect("an editor writes");

    // Removed from the store.
    {
        let user_id = env.user_id(BOB.0);
        env.stub.inner.lock().unwrap().stores.retain(|row| row.user_id != user_id);
    }
    assert!(wait_until(Duration::from_secs(20), || whole_ended(&bob, fx.store_id, fx.root)).await, "the whole replica's root is recorded as ended");
    let listed = bob.list_stores().await.unwrap().into_iter().find(|s| s.id == fx.store_id).expect("the row stays: the files do");
    assert!(listed.every_share_ended() && listed.shown_roots().is_empty() && listed.is_replica, "{listed:?}");
    let refused = bob.create_node(fx.store_id, Some(fx.garden), "document", "Too late").await.expect_err("it takes no edits").to_string();
    assert!(refused.contains(StoreAccess::READ_ONLY_REFUSAL), "{refused}");
    assert_eq!(node_title(&bob, fx.store_id, fx.tomatoes).await, "Tomatoes", "nothing is deleted");

    // Granted again: held as before, and writing reaches H.
    grant(&env, BOB.0, fx.store_id, "editor", None, "Allotment");
    assert!(wait_until(Duration::from_secs(40), || held_whole(&bob, fx.store_id)).await, "held again");
    let beans = bob.create_node(fx.store_id, Some(fx.garden), "document", "Beans").await.expect("writable again");
    assert!(wait_until(Duration::from_secs(20), || async { env.h_admin.get_node(fx.store_id, beans).await.is_ok() }).await);

    // Another account signed in on this device holds nothing of it; Bob
    // signing in again holds it as before.
    env.sign_in(&bob, CAROL).await;
    assert!(wait_until(Duration::from_secs(20), || whole_ended(&bob, fx.store_id, fx.root)).await, "another account ends it too");
    env.sign_in(&bob, BOB).await;
    assert!(wait_until(Duration::from_secs(40), || held_whole(&bob, fx.store_id)).await, "its own account holds it again");
}

/// A second share of a store whose partial replica is open here adds its
/// root to that replica (the already-open path), and rewriting `sync.json`
/// for it keeps when the replica last synced.
#[tokio::test]
async fn another_share_joins_the_open_replica_and_keeps_its_last_sync() {
    let env = spawn_env().await;
    let fx = plain_hosted_store(&env).await;
    grant(&env, BOB.0, fx.store_id, "editor", Some(fx.garden), "The garden");
    let b_dir = tempfile::tempdir().unwrap();
    let faults = std::sync::Arc::new(pimble_server::faults::LinkFaults::new());
    let mut server = PimbleServer::with_config(ServerConfig {
        grant_check_interval: Some(Duration::from_secs(1)),
        link_faults: Some(std::sync::Arc::clone(&faults)),
        ..device_config(b_dir.path())
    });
    server.start().await.expect("device starts");
    let bob = PimbleClient::connect(format!("http://{}", server.addr())).await.unwrap();
    env.sign_in(&bob, BOB).await;
    bob.cloud_add_hosted_store(fx.store_id).await.expect("the first share");
    assert!(synced(&bob, fx.store_id).await);

    let record = b_dir.path().join("stores").join(fx.store_id.to_string()).join("sync-state.json");
    let last_sync = || -> Option<String> {
        let record: serde_json::Value = serde_json::from_slice(&tfs::read(&record).ok()?).ok()?;
        record["link"]["last_sync"].as_str().map(str::to_string)
    };
    assert!(wait_until(Duration::from_secs(10), || async { last_sync().is_some() }).await, "the link recorded its sync");
    let synced_at = last_sync();

    // Offline, so the restarted link cannot sync and write a new time: what
    // is there afterwards is what the call kept.
    faults.set_offline(true);
    grant(&env, BOB.0, fx.store_id, "editor", Some(fx.accounts), "The accounts");
    let added = bob.cloud_add_hosted_store(fx.store_id).await.expect("another share joins the replica here");
    assert_eq!(added.roots, vec![fx.garden, fx.accounts]);
    assert_eq!(last_sync(), synced_at, "the rewrite keeps when it last synced");

    faults.set_offline(false);
    assert!(
        wait_until(Duration::from_secs(20), || async { node_text(&bob, fx.store_id, fx.accounts).await.contains("the shed cost forty pounds") }).await,
        "the new share's documents arrive"
    );
    assert!(synced(&bob, fx.store_id).await);
    let sync_json = tfs::read_to_string(added.local_path().unwrap().join("sync.json")).unwrap();
    assert!(sync_json.contains("\"via_account\": true") || sync_json.contains("\"via_account\":true"), "{sync_json}");
}

/// A mount in a replica held through the account whose source is another
/// store the account holds on Pimble Cloud: the source is added the way
/// `cloudAddHostedStore` adds one, with the account's token, never through
/// the mounting store's address with a saved credential (there is none).
#[tokio::test]
async fn a_mount_in_an_account_replica_resolves_its_source_through_the_account() {
    let env = spawn_env().await;
    let fx = plain_hosted_store(&env).await;
    let source_path = env.h_stores_dir.join(format!("{}.pimble", uuid::Uuid::new_v4()));
    let (source, source_root) = env.h_admin.create_store_with(&source_path, "Seeds", StoreKind::Plain, None).await.unwrap();
    let packet = env.h_admin.create_node(source, Some(source_root), "document", "Packet").await.unwrap();
    grant(&env, ALICE.0, source, "owner", None, "Seeds");
    let (mount, _) = env.h_admin.create_mount(fx.store_id, fx.garden, source, source_root, Some("Seeds".into())).await.expect("H mounts one store in the other");
    // H fills the mount's `source_path` hint with where the source lives on
    // H's disk. A device never shares that disk; here H and the device do,
    // and the hint would open H's own store files on the device. Drop it, as
    // tests/remote_mounts.rs does.
    let mut mount_ref = env.h_admin.get_node(fx.store_id, mount).await.unwrap().mount_ref().expect("a mount node");
    mount_ref.source_path = None;
    let value = serde_json::to_value(&mount_ref).unwrap();
    env.h_admin.edit_node(fx.store_id, mount, "test", |doc, now| doc.set_custom("mount_ref", &value, now)).await.expect("the hint is dropped");
    grant(&env, BOB.0, fx.store_id, "editor", None, "Allotment");
    grant(&env, BOB.0, source, "editor", None, "Seeds");

    let b_dir = tempfile::tempdir().unwrap();
    let (_b, bob) = device(b_dir.path()).await;
    env.sign_in(&bob, BOB).await;
    bob.cloud_add_hosted_store(fx.store_id).await.expect("the mounting store is added");
    assert!(synced(&bob, fx.store_id).await);
    assert!(wait_until(Duration::from_secs(20), || async { bob.get_node(fx.store_id, mount).await.is_ok() }).await, "the mount node arrives");

    let live = wait_until(Duration::from_secs(30), || async {
        matches!(bob.get_mount_state(fx.store_id, mount).await, Ok((pimble_core::MountState::Live, _)))
    })
    .await;
    assert!(live, "the source resolves: {:?}", bob.get_mount_state(fx.store_id, mount).await.map(|(state, _)| state));
    let (lives_in, children) = bob.get_children(fx.store_id, mount).await.expect("the mount's children");
    assert_eq!(lives_in, source);
    assert_eq!(children.iter().map(|c| c.id).collect::<Vec<_>>(), vec![packet]);
    let replica = bob.list_stores().await.unwrap().into_iter().find(|s| s.id == source).expect("the source is a replica here");
    let sync_json = tfs::read_to_string(replica.local_path().unwrap().join("sync.json")).unwrap();
    assert!(sync_json.contains("\"via_account\": true") || sync_json.contains("\"via_account\":true"), "held through the account: {sync_json}");
}
