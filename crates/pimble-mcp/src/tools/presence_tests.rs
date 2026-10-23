//! The agent is present while it works (docs/PRESENCE_CONTRACT.md section 8),
//! against a real server with a real agent key: an edit says where, stamped as
//! the agent by the server, with a caret at the end of what it wrote, and the
//! session ends once its frame lapses.

use std::sync::Arc;
use std::time::Duration;

use pimble_client::PimbleClient;
use pimble_core::{Anchor, AuthMethod};
use pimble_crdt::{NodeDoc, Resolved};
use pimble_rpc::{AgentGrant, AgentRole, PresenceBody, PresenceFrame};
use pimble_server::{PimbleServer, ServerConfig};
use rmcp::handler::server::wrapper::Parameters;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::Pimble;
use crate::connection::Connection;
use crate::presence::Presence;

fn p<T: DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
    Parameters(serde_json::from_value(v).expect("params"))
}

/// The next frame `watch` hears, within `wait`.
async fn next(watch: &mut jsonrpsee::core::client::Subscription<PresenceFrame>, wait: Duration) -> PresenceFrame {
    tokio::time::timeout(wait, watch.next()).await.expect("a frame arrives").expect("the subscription is open").expect("a frame parses")
}

#[tokio::test]
async fn an_agents_edit_says_where_it_wrote_and_fades() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        credentials_path: Some(dir.path().join("credentials.json")),
        keystore_path: Some(dir.path().join("keys.json")),
        replicas_dir: Some(dir.path().join("replicas")),
        agents_path: Some(dir.path().join("agents.json")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.expect("service connects");
    let (store, root) = service.create_store(dir.path().join("work.pimble"), "Work").await.expect("store");
    let note = service.create_node(store, Some(root), "document", "Plans").await.unwrap();
    let created = service
        .create_agent("Claude", vec![AgentGrant { store, node: None, role: AgentRole::Editor }])
        .await
        .expect("an agent is made");
    let agent = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: created.key.clone() }).await.expect("the agent connects");
    let client_id = format!("agent:{}", created.agent.id);
    let short = Presence::with_ttl(client_id.clone(), 300);
    short.set_device("Claude Code");
    let pimble = Pimble::with_presence(Arc::new(Connection::with_client(agent)), client_id.clone(), short);

    let mut watch = service.subscribe_presence(store, "watcher").await.expect("presence subscribes");
    pimble.append(p(json!({ "node": "Work/Plans", "markdown": "First line\n\nThe agent wrote this" }))).await.unwrap();

    let frame = next(&mut watch, Duration::from_secs(5)).await;
    assert_eq!((frame.store_id, frame.doc_id), (store, note));
    assert_eq!(frame.session, format!("{client_id}:{note}"));
    assert_eq!(frame.ttl_ms, 300);
    assert!(!frame.left);
    let who = frame.who.clone().expect("stamped by the server");
    assert_eq!(who.agent.as_deref(), Some("Claude"));
    let body = PresenceBody::decode(&frame.body).expect("a plain body");
    assert_eq!(body.device, "Claude Code");
    let caret = body.text.expect("a caret").caret_bytes().unwrap();

    // The caret is at the end of the words inserted, in the server's copy.
    let held = service.get_node(store, note).await.unwrap();
    let doc = NodeDoc::load(&held.content).unwrap();
    let texts = doc.textblock_texts();
    let last = texts.len() - 1;
    assert_eq!(texts[last], "The agent wrote this");
    match doc.resolve_anchor(&Anchor::new(Some(caret.clone()), "")) {
        Some(Resolved::Sticky(spot)) => assert_eq!((spot.textblock, spot.offset), (last, "The agent wrote this".len())),
        other => panic!("the caret does not resolve: {other:?}"),
    }

    // A read keeps the caret where it wrote.
    pimble.get_node(p(json!({ "node": "Work/Plans" }))).await.unwrap();
    let read = next(&mut watch, Duration::from_secs(5)).await;
    assert!(read.seq > frame.seq);
    assert_eq!(PresenceBody::decode(&read.body).unwrap().text.unwrap().caret_bytes().unwrap(), caret);

    // No heartbeat: once the frame lapses the server ends the session.
    let gone = next(&mut watch, Duration::from_secs(8)).await;
    assert!(gone.left, "{gone:?}");
    assert_eq!(gone.session, frame.session);
    assert!(service.get_presence(store).await.unwrap().is_empty());
    let _ = server.stop().await;
}
