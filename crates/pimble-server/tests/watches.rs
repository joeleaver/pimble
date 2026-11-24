//! Watches end to end on the device's side and the hosted server's
//! (docs/NOTIFICATIONS_CONTRACT.md section 3, wave 3), on the sharing
//! harness: the owner watches a folder with its subtree; her device seals
//! the choice to the stub accounts service and registers the documents it
//! reaches, again as a node is made under it; a member's write reaches the
//! hosted server's `subscribeWrites` naming the document and the writer and
//! nothing else; Unwatch on a note under the folder takes the folder's
//! watch. A store on this computer alone cannot be watched.

mod common;

use std::time::Duration;

use base64::Engine;
use common::*;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::{EditOperation, MemberRole};

fn registered(env: &Env, user: &str, store_id: StoreId) -> Vec<String> {
    let s = env.stub.inner.lock().unwrap();
    let mut docs = s.watched.get(&(user.to_string(), store_id.to_string())).cloned().unwrap_or_default();
    docs.sort();
    docs
}

async fn append(client: &PimbleClient, store_id: StoreId, node: NodeId, md: &str) {
    let held = client.get_node(store_id, node).await.expect("the note is there");
    let mut doc = NodeDoc::load(&held.content).expect("its document loads");
    let update = doc.append_markdown(md).expect("the Markdown applies");
    let changes = base64::engine::general_purpose::STANDARD.encode(update);
    client.apply_edit(store_id, node, "app", EditOperation::IncrementalChanges { changes }).await.expect("the edit applies");
}

#[tokio::test]
async fn a_watched_folder_registers_what_is_under_it_and_a_members_write_is_reported() {
    let env = spawn_env().await;
    let (_a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    alice.cloud_share_node(fx.store_id, fx.shared, "Holiday Plans").await.expect("sharing a folder");
    alice.cloud_share_invite(fx.store_id, fx.shared, BOB.0, MemberRole::Editor).await.expect("inviting Bob");
    let (_b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (alice_id, bob_id) = (env.user_id(ALICE.0), env.user_id(BOB.0));

    // Nothing is watched until she chooses it.
    assert!(alice.list_watches().await.unwrap().watches.is_empty());
    assert!(registered(&env, &alice_id, fx.store_id).is_empty());

    let listed = alice.watch_node(fx.store_id, fx.shared, true).await.expect("watching the folder");
    assert_eq!(listed.watches.len(), 1);
    assert!(listed.watches[0].subtree && listed.on);
    let (version, sealed) = env.stub.inner.lock().unwrap().watch_choices.get(&alice_id).cloned().expect("the choices were written");
    assert_eq!(version, 1);
    assert!(!sealed.contains(&fx.shared.to_string()), "sealed: no node id in the clear");
    let mut want: Vec<String> = [fx.shared, fx.inside, fx.deeper, fx.leaf].iter().map(|n| n.to_string()).collect();
    want.sort();
    assert!(wait_until(Duration::from_secs(10), || async { registered(&env, &alice_id, fx.store_id) == want }).await, "the folder and what is under it, not the diary: {:?}", registered(&env, &alice_id, fx.store_id));

    // A node made under it is registered too, a couple of seconds later.
    let new = alice.create_node(fx.store_id, Some(fx.deeper), "document", "Ferry").await.unwrap();
    assert!(wait_until(Duration::from_secs(15), || async { registered(&env, &alice_id, fx.store_id).contains(&new.to_string()) }).await, "kept in step as nodes come");

    // Bob writes in Packing: the hosted server tells who wrote which
    // document, and how many times, to whoever follows its writes.
    let hosted = env.stub.inner.lock().unwrap().h_client.clone().expect("the hosted server's client");
    let mut writes = hosted.subscribe_writes().await.expect("following the hosted server's writes");
    append(&bob, fx.store_id, fx.inside, "and a towel").await;
    let seen = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let seen = writes.next().await.expect("a write").expect("it parses");
            if seen.writer == bob_id {
                return seen;
            }
        }
    })
    .await
    .expect("Bob's write is reported");
    assert_eq!((seen.store_id, seen.doc_id), (fx.store_id, fx.inside));
    assert!(seen.count >= 1);

    // Unwatch on a note under the folder takes the folder's watch.
    let listed = alice.unwatch_node(fx.store_id, fx.inside).await.expect("unwatching");
    assert!(listed.watches.is_empty());
    assert!(wait_until(Duration::from_secs(10), || async { registered(&env, &alice_id, fx.store_id).is_empty() }).await, "nothing registered any more");
    assert!(alice.unwatch_node(fx.store_id, fx.inside).await.is_err(), "not watched now");
}

#[tokio::test]
async fn a_store_on_this_computer_alone_is_not_watched() {
    let env = spawn_env().await;
    let (_a, alice, a_dir) = start_local_server().await;
    env.sign_in(&alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(a_dir.path().join("local.pimble"), "Local").await.unwrap();
    let note = alice.create_node(store_id, Some(root_id), "document", "Mine").await.unwrap();
    let refused = alice.watch_node(store_id, note, false).await.expect_err("nobody else can change it");
    assert!(refused.to_string().contains("this store is on this computer alone"), "{refused}");
    assert!(env.stub.inner.lock().unwrap().watch_choices.is_empty(), "nothing written");
}
