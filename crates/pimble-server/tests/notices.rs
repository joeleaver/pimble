//! Notices end to end (docs/NOTIFICATIONS_CONTRACT.md, wave 1), on the
//! sharing harness: the owner shares a folder with a member; the owner's
//! device writes a mention of the member into a note in it; the member's
//! server drains its inbox, opens the notice with its notice key and tells
//! its windows. Undo and redo of the mention, a restart, and the edit
//! arriving at the member's device over its link notify nobody again; the
//! same mention pasted into another note does; switched off, nothing goes.

mod common;

use std::time::Duration;

use base64::Engine;
use common::*;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::{EditOperation, MemberRole, NoticeKind};

/// Write `md` at the end of a note the way an editor's edit reaches the
/// server: one update of the note's own document.
async fn append(client: &PimbleClient, store_id: StoreId, node: NodeId, md: &str) {
    let held = client.get_node(store_id, node).await.expect("the note is there");
    let mut doc = NodeDoc::load(&held.content).expect("its document loads");
    let update = doc.append_markdown(md).expect("the Markdown applies");
    let changes = base64::engine::general_purpose::STANDARD.encode(update);
    client.apply_edit(store_id, node, "app", EditOperation::IncrementalChanges { changes }).await.expect("the edit applies");
}

/// Take the note back to `md` (an undo of what was appended).
async fn rewrite(client: &PimbleClient, store_id: StoreId, node: NodeId, md: &str) {
    let held = client.get_node(store_id, node).await.expect("the note is there");
    let mut doc = NodeDoc::load(&held.content).expect("its document loads");
    let update = doc.replace_content_markdown(md).expect("the Markdown applies");
    let changes = base64::engine::general_purpose::STANDARD.encode(update);
    client.apply_edit(store_id, node, "app", EditOperation::IncrementalChanges { changes }).await.expect("the edit applies");
}

fn has_notice_key(env: &Env, email: &str) -> bool {
    env.stub.inner.lock().unwrap().accounts.iter().any(|a| a.email == email && a.notice_key.is_some())
}

fn posted(env: &Env, from: &str, to: &str) -> usize {
    env.stub.inner.lock().unwrap().posted.iter().filter(|(f, t)| f == from && t == to).count()
}

fn queued_for(env: &Env, who: &str) -> usize {
    env.stub.inner.lock().unwrap().inbox.iter().filter(|i| i.recipient == who).count()
}

#[tokio::test]
async fn a_mention_reaches_the_member_once_sealed_and_opened_by_their_server() {
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    alice.cloud_share_node(fx.store_id, fx.shared, "Holiday Plans").await.expect("sharing a folder");
    alice.cloud_share_invite(fx.store_id, fx.shared, BOB.0, MemberRole::Editor).await.expect("inviting Bob");
    let (_b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (alice_id, bob_id) = (env.user_id(ALICE.0), env.user_id(BOB.0));

    // Each signed-in device made its account's notice key.
    assert!(wait_until(Duration::from_secs(15), || async { has_notice_key(&env, ALICE.0) && has_notice_key(&env, BOB.0) }).await, "both accounts have a notice key");
    let mut heard = bob.subscribe_notices().await.expect("Bob's window listens");

    // Alice mentions Bob in Packing.
    let mention = format!("[@Bob](pimble-person:{bob_id}?m=abc123) can you bring the map?");
    append(&alice, fx.store_id, fx.inside, &mention).await;
    let notice = tokio::time::timeout(Duration::from_secs(20), heard.next()).await.expect("Bob's server hears the notice").expect("a notice").expect("it parses");
    assert_eq!(notice.kind, NoticeKind::Mention);
    assert_eq!((notice.from.as_str(), notice.from_name.as_str()), (alice_id.as_str(), "alice"));
    assert_eq!((notice.store, notice.node, notice.discussion), (fx.store_id, fx.inside, None));
    assert_eq!(notice.anchor.as_deref(), Some("abc123"));
    assert!(notice.snippet.contains("@Bob can you bring the map?"), "{}", notice.snippet);
    // What the service held is sealed: not a word of the note in it.
    assert!(env.stub.inner.lock().unwrap().inbox.iter().all(|i| !i.sealed.contains("map") && i.root == Some(fx.shared.to_string())), "sealed, through the share");
    let listed = bob.list_notices().await.unwrap();
    assert!(listed.on);
    assert_eq!(listed.notices.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), vec![notice.id.as_str()]);
    assert_eq!(posted(&env, &alice_id, &bob_id), 1);

    // Undo, then redo: the same mention again, told to nobody again. The
    // edit reached Bob's own replica over its link: his server tells nobody.
    rewrite(&alice, fx.store_id, fx.inside, "socks and a map").await;
    append(&alice, fx.store_id, fx.inside, &mention).await;
    let arrived = wait_until(Duration::from_secs(15), || async { node_text(&bob, fx.store_id, fx.inside).await.contains("bring the map") }).await;
    assert!(arrived, "the redo reached Bob's replica");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(posted(&env, &alice_id, &bob_id), 1, "undo and redo notify nobody again");
    assert_eq!(posted(&env, &bob_id, &alice_id), 0, "an edit arriving over a link is not Bob's to notify");

    // A restart remembers what was told.
    a.stop().await.unwrap();
    let (a2, alice2) = start_device_in(a_dir.path()).await;
    alice2.open_store(a_dir.path().join("a.pimble")).await.expect("the owner's store reopens");
    rewrite(&alice2, fx.store_id, fx.inside, "socks and a map").await;
    append(&alice2, fx.store_id, fx.inside, &mention).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(posted(&env, &alice_id, &bob_id), 1, "a restart does not forget");

    // The same mention pasted into another note of the share is news.
    append(&alice2, fx.store_id, fx.leaf, &mention).await;
    let second = tokio::time::timeout(Duration::from_secs(20), heard.next()).await.expect("the second notice").expect("a notice").expect("parses");
    assert_eq!((second.node, second.anchor.as_deref()), (fx.leaf, Some("abc123")));
    assert_eq!(posted(&env, &alice_id, &bob_id), 2);
    assert!(env.stub.inner.lock().unwrap().kinds.iter().all(|k| k == "mention"), "the kind goes with each notice (wave 2)");

    // Bob opens them: acknowledged, the service keeps them no longer.
    assert_eq!(queued_for(&env, &bob_id), 2);
    bob.acknowledge_notices(vec![notice.id.clone(), second.id.clone()]).await.unwrap();
    assert!(wait_until(Duration::from_secs(10), || async { queued_for(&env, &bob_id) == 0 }).await, "acknowledged");

    // A mention of someone outside the share (Carol) tells nobody.
    let carol_id = env.user_id(CAROL.0);
    append(&alice2, fx.store_id, fx.inside, &format!("[@Carol](pimble-person:{carol_id}?m=c1)")).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(posted(&env, &alice_id, &carol_id), 0);

    // Wave 2: the channels and a browser's push subscription go through the
    // device's server to the service; signing out takes the subscription back.
    let listed = bob.list_notices().await.unwrap();
    assert!(listed.email && listed.push, "on unless turned off");
    assert_eq!(listed.push_key.as_deref(), Some("BStubVapidPublicKey"));
    bob.set_notification_channels(Some(false), None).await.unwrap();
    let listed = bob.list_notices().await.unwrap();
    assert!(!listed.email && listed.push && listed.on);
    assert!(env.stub.inner.lock().unwrap().accounts.iter().any(|a| a.user_id == bob_id && a.email_off && !a.push_off));
    bob.register_push("https://fcm.googleapis.com/fcm/send/bob".into(), "p256dh".into(), "auth".into()).await.unwrap();
    bob.register_push("https://fcm.googleapis.com/fcm/send/bob2".into(), "p256dh".into(), "auth".into()).await.unwrap();
    bob.unregister_push("https://fcm.googleapis.com/fcm/send/bob2".into()).await.unwrap();
    assert_eq!(env.stub.inner.lock().unwrap().push_subs, vec![(bob_id.clone(), "https://fcm.googleapis.com/fcm/send/bob".to_string())]);
    bob.cloud_sign_out().await.unwrap();
    assert!(env.stub.inner.lock().unwrap().push_subs.is_empty(), "signing out takes the browser's subscription back");

    // Alice turns notifications off: nothing more goes from her device.
    alice2.set_notifications(false).await.unwrap();
    assert!(!alice2.list_notices().await.unwrap().on);
    append(&alice2, fx.store_id, fx.deeper, &format!("[@Bob](pimble-person:{bob_id}?m=off1)")).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(posted(&env, &alice_id, &bob_id), 2, "switched off: nothing is sent");
    drop(a2);
}

#[tokio::test]
async fn a_reply_in_a_discussion_tells_the_others_who_wrote_in_it() {
    use pimble_discussion::Author;
    let env = spawn_env().await;
    let (_a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    alice.cloud_share_node(fx.store_id, fx.shared, "Holiday Plans").await.unwrap();
    alice.cloud_share_invite(fx.store_id, fx.shared, BOB.0, MemberRole::Editor).await.unwrap();
    let (_b, bob, _b_dir) = member_device(&env, BOB, &fx).await;
    let (alice_id, bob_id) = (env.user_id(ALICE.0), env.user_id(BOB.0));
    assert!(wait_until(Duration::from_secs(15), || async { has_notice_key(&env, ALICE.0) && has_notice_key(&env, BOB.0) }).await);
    let mut heard = bob.subscribe_notices().await.unwrap();

    // Bob starts a discussion on Packing and writes in it; Alice answers.
    let discussion = bob
        .create_attachment(fx.store_id, fx.inside, pimble_core::node_types::DISCUSSION, "Packing list", Some(pimble_discussion::new_content(None)), Default::default())
        .await
        .expect("a discussion");
    post(&bob, fx.store_id, discussion, Author::new(&bob_id, "bob"), "Who brings the tent?", "2026-11-23T10:00:00.000Z").await;
    let arrived = wait_until(Duration::from_secs(30), || async {
        alice.get_node(fx.store_id, discussion).await.is_ok_and(|n| NodeDoc::load(&n.content).is_ok_and(|d| pimble_discussion::Discussion::of_node(&d).read().messages.len() == 1))
    })
    .await;
    assert!(arrived, "the discussion reached Alice");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(posted(&env, &bob_id, &alice_id), 0, "nobody else wrote in it yet");
    assert_eq!(posted(&env, &alice_id, &bob_id), 0, "Bob's message reached Alice over her link: not hers to notify");
    post(&alice, fx.store_id, discussion, Author::new(&alice_id, "alice"), "I will.", "2026-11-23T10:01:00.000Z").await;
    let notice = tokio::time::timeout(Duration::from_secs(20), heard.next()).await.expect("Bob hears the reply").expect("a notice").expect("parses");
    assert_eq!(notice.kind, NoticeKind::Reply);
    assert_eq!((notice.node, notice.discussion), (fx.inside, Some(discussion)));
    assert_eq!(notice.snippet, "I will.");
    assert_eq!(notice.from_name, "alice");
    assert_eq!(posted(&env, &alice_id, &bob_id), 1);
}

/// Post `text` in a discussion the way the app does: one update of its document.
async fn post(client: &PimbleClient, store_id: StoreId, discussion: NodeId, who: pimble_discussion::Author, text: &str, at: &str) {
    use pimble_discussion::{Body, Discussion, Stamp};
    let held = client.get_node(store_id, discussion).await.expect("the discussion is there");
    let doc = NodeDoc::load(&held.content).unwrap();
    let (_, update) = Discussion::of_node(&doc).edit(|t| t.post(Stamp::new(Some(&who), at), None, &Body::plain(text))).into_result().unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(update.expect("an update"));
    client.apply_edit(store_id, discussion, "app", EditOperation::IncrementalChanges { changes }).await.expect("the message applies");
}
