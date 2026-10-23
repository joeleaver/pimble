//! Presence end to end (docs/PRESENCE_CONTRACT.md, wave 1): frames travel
//! from one device's server through the hosted server to another's, sealed
//! on the way (the hosted server sees which document and who, not what the
//! frame says), reach only those who may read the document, and end when
//! their session does. Nothing is written anywhere.

mod common;
use common::*;

use std::time::Duration;

use pimble_rpc::{MemberRole, PresenceBody, PresenceFrame, PRESENCE_TTL_MS};

fn frame(store_id: pimble_core::StoreId, doc_id: pimble_core::NodeId, session: &str, seq: u64, device: &str) -> PresenceFrame {
    PresenceFrame {
        store_id,
        doc_id,
        session: session.to_string(),
        seq,
        who: None,
        left: false,
        body: PresenceBody { device: device.to_string(), focused: true, text: None, board: None }.encode(),
        sealed: false,
        ttl_ms: PRESENCE_TTL_MS,
    }
}

/// The next frame on `sub` within a few seconds, if any.
async fn next_frame(sub: &mut jsonrpsee::core::client::Subscription<PresenceFrame>) -> Option<PresenceFrame> {
    let item = tokio::time::timeout(Duration::from_secs(10), sub.next()).await.ok().flatten();
    item.and_then(Result::ok)
}

#[tokio::test]
async fn a_member_sees_the_owner_in_a_shared_note_and_nothing_outside_the_share() {
    let env = spawn_env().await;
    let (_a, alice, a_dir) = start_local_server().await;
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;
    alice.cloud_share_node(store_id, fx.shared, "Holiday Plans").await.expect("sharing a folder");
    alice.cloud_share_invite(store_id, fx.shared, BOB.0, MemberRole::Editor).await.expect("inviting Bob");

    let (_b, bob, _b_dir) = start_local_server().await;
    env.sign_in(&bob, BOB).await;
    bob.cloud_add_hosted_store(store_id).await.expect("Bob adds the share");
    let pulled = wait_until(Duration::from_secs(15), || async { node_text(&bob, store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "Bob's replica is synced");
    let mut bob_hears = bob.subscribe_presence(store_id, "bob-app").await.expect("Bob's app listens");

    // Alice's app opens a note in the share, and one outside it.
    let mut alice_hears = alice.subscribe_presence(store_id, "alice-app").await.expect("Alice's app listens");
    alice.publish_presence(frame(store_id, fx.private, "alice-app:1", 1, "Alice's laptop"), "alice-app").await.unwrap();
    alice.publish_presence(frame(store_id, fx.inside, "alice-app:0", 1, "Alice's laptop"), "alice-app").await.unwrap();

    let got = next_frame(&mut bob_hears).await.expect("Alice's presence reaches Bob");
    assert_eq!((got.doc_id, got.session.as_str(), got.sealed, got.left), (fx.inside, "alice-app:0", false, false), "the note outside the share never reaches him: {got:?}");
    assert_eq!(got.who.as_ref().map(|w| w.sub.clone()), Some(env.user_id(ALICE.0)), "stamped as Alice's account");
    assert_eq!(PresenceBody::decode(&got.body).map(|b| b.device), Some("Alice's laptop".to_string()), "opened on Bob's side");
    // What a late arrival sees: the same, and nothing of the private note.
    let held = bob.get_presence(store_id).await.unwrap();
    assert_eq!(held.iter().map(|f| f.doc_id).collect::<Vec<_>>(), vec![fx.inside], "{held:?}");

    // Bob opens the note too; Alice hears him, and never her own frames back.
    bob.publish_presence(frame(store_id, fx.inside, "bob-app:0", 1, "Bob's desktop"), "bob-app").await.unwrap();
    let got = next_frame(&mut alice_hears).await.expect("Bob's presence reaches Alice");
    assert_eq!(got.session, "bob-app:0");
    assert_eq!(got.who.as_ref().map(|w| w.sub.clone()), Some(env.user_id(BOB.0)));

    // The hosted server held them sealed and wrote nothing.
    assert!(!contains_bytes_recursive(&env.h_stores_dir, b"Alice's laptop"));
    assert!(!contains_bytes_recursive(&env.h_stores_dir, b"Bob's desktop"));

    // Alice's app goes away: Bob hears her leave without waiting out the
    // timeout (both of her sessions end; only the shared one reaches him).
    drop(alice_hears);
    let left = next_frame(&mut bob_hears).await.expect("a goodbye");
    assert_eq!((left.session.as_str(), left.left), ("alice-app:0", true), "{left:?}");
    let gone = wait_until(Duration::from_secs(5), || async { bob.get_presence(store_id).await.unwrap().iter().all(|f| !f.session.starts_with("alice-app")) }).await;
    assert!(gone, "and she is no longer held on Bob's side");
}

#[tokio::test]
async fn an_older_frame_is_dropped_and_a_goodbye_ends_the_session() {
    let (_s, client, dir) = start_local_server().await;
    let (store_id, root) = client.create_store(dir.path().join("p.pimble"), "Mine").await.unwrap();
    let note = client.create_node(store_id, Some(root), "document", "Note").await.unwrap();
    let mut hears = client.subscribe_presence(store_id, "other-window").await.unwrap();

    client.publish_presence(frame(store_id, note, "me:0", 2, "here"), "me").await.unwrap();
    client.publish_presence(frame(store_id, note, "me:0", 1, "stale"), "me").await.unwrap();
    let got = next_frame(&mut hears).await.unwrap();
    assert_eq!(got.seq, 2);
    assert_eq!(client.get_presence(store_id).await.unwrap().len(), 1);

    let bye = PresenceFrame { seq: 3, left: true, body: String::new(), ..frame(store_id, note, "me:0", 3, "") };
    client.publish_presence(bye, "me").await.unwrap();
    let got = next_frame(&mut hears).await.unwrap();
    assert!(got.left && got.seq == 3, "the stale frame went nowhere, the goodbye did: {got:?}");
    assert!(client.get_presence(store_id).await.unwrap().is_empty());
}

/// An agent working on Alice's computer is present to the members of a share
/// it works in (docs/PRESENCE_CONTRACT.md section 8): its frame crosses the
/// hosted server sealed, stamped as Alice's account and the agent's name,
/// with the caret it said.
#[tokio::test]
async fn an_agent_on_the_owners_computer_shows_to_a_member() {
    let env = spawn_env().await;
    let a_dir = tempfile::tempdir().unwrap();
    let mut a = pimble_server::PimbleServer::with_config(pimble_server::ServerConfig {
        agents_path: Some(a_dir.path().join("agents.json")),
        ..device_config(a_dir.path())
    });
    a.start().await.expect("Alice's server starts");
    let url = format!("http://{}", a.addr());
    let alice = pimble_client::PimbleClient::connect(&url).await.expect("Alice's app connects");
    let fx = hosted_fixture(&env, &alice, a_dir.path()).await;
    let store_id = fx.store_id;
    alice.cloud_share_node(store_id, fx.shared, "Holiday Plans").await.expect("sharing a folder");
    alice.cloud_share_invite(store_id, fx.shared, BOB.0, MemberRole::Editor).await.expect("inviting Bob");

    let (_b, bob, _b_dir) = start_local_server().await;
    env.sign_in(&bob, BOB).await;
    bob.cloud_add_hosted_store(store_id).await.expect("Bob adds the share");
    let pulled = wait_until(Duration::from_secs(15), || async { node_text(&bob, store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "Bob's replica is synced");
    let mut bob_hears = bob.subscribe_presence(store_id, "bob-app").await.expect("Bob's app listens");

    let made = alice
        .create_agent("Claude", vec![pimble_rpc::AgentGrant { store: store_id, node: None, role: pimble_rpc::AgentRole::Editor }])
        .await
        .expect("Alice makes an agent");
    let agent = pimble_client::PimbleClient::connect_with_auth(&url, &pimble_core::AuthMethod::Bearer { token: made.key.clone() })
        .await
        .expect("the agent connects");
    let client_id = format!("agent:{}", made.agent.id);
    let mut said = frame(store_id, fx.inside, &format!("{client_id}:{}", fx.inside), 1, "Claude Code");
    said.body = PresenceBody {
        device: "Claude Code".into(),
        focused: true,
        text: Some(pimble_rpc::TextPresence::new(&[1, 2, 3], None)),
        board: None,
    }
    .encode();
    said.ttl_ms = 15_000;
    agent.publish_presence(said, &client_id).await.expect("the agent says where it is");

    let got = next_frame(&mut bob_hears).await.expect("the agent's presence reaches Bob");
    assert_eq!((got.doc_id, got.left), (fx.inside, false));
    let who = got.who.expect("stamped");
    assert_eq!((who.sub, who.agent.as_deref()), (env.user_id(ALICE.0), Some("Claude")), "Alice's account, the agent's name");
    let body = PresenceBody::decode(&got.body).expect("opened on Bob's side");
    assert_eq!(body.device, "Claude Code");
    assert_eq!(body.text.and_then(|t| t.caret_bytes()), Some(vec![1, 2, 3]));
    assert_eq!(got.ttl_ms, 15_000);
    assert!(!contains_bytes_recursive(&env.h_stores_dir, b"Claude Code"));
    let _ = a.stop().await;
}
