//! `list_people` over an agent's connection, which is how every MCP client
//! comes (docs/AGENTS_CONTRACT.md): who can open a note, with the Markdown
//! that mentions each, and a failure answered as one.

use std::sync::Arc;

use pimble_client::PimbleClient;
use pimble_core::{custom_keys, AuthMethod};
use pimble_server::{PimbleServer, ServerConfig};
use rmcp::handler::server::wrapper::Parameters;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::Pimble;
use crate::connection::Connection;

fn p<T: DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
    Parameters(serde_json::from_value(v).expect("params"))
}

#[tokio::test]
async fn an_agent_lists_who_can_open_a_shared_note() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig { ..ServerConfig::in_dir(dir.path()) });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    let (store, root) = service.create_store(dir.path().join("work.pimble"), "Work").await.unwrap();
    let shared = service.create_node(store, Some(root), "document", "Shared").await.unwrap();
    service.create_node(store, Some(shared), "document", "Plans").await.unwrap();
    let created = service
        .create_agent("Claude", vec![pimble_rpc::AgentGrant { store, node: None, role: pimble_rpc::AgentRole::Reader }])
        .await
        .unwrap();
    let agent = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: created.key }).await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(agent)));
    let ask = || pimble.list_people(p(json!({ "node": "Work/Shared/Plans" })));

    assert_eq!(ask().await.unwrap(), "Nobody else can open this note: it is in no share.");

    let marker = json!({ "v": 1, "key_id": uuid::Uuid::new_v4(), "url": "https://pimble.app", "name": "Shared" });
    service.edit_node(store, shared, "test", |doc, now| doc.set_custom(custom_keys::SHARE, &marker, now)).await.unwrap();
    let unknown = ask().await.unwrap();
    assert!(unknown.contains("not known on this computer yet"), "{unknown}");

    // What the server keeps once the accounts service has said who is in it.
    let state = dir.path().join("stores").join(store.to_string());
    std::fs::create_dir_all(&state).unwrap();
    let people = json!({ "scopes": [{ "root": shared, "people": [{ "user_id": "u-will", "name": "Will" }] }] });
    std::fs::write(state.join("people.json"), people.to_string()).unwrap();
    let listed = ask().await.unwrap();
    assert!(listed.contains("- Will: [@Will](pimble-person:u-will)"), "{listed}");
    assert!(!listed.contains("There may be others"), "{listed}");

    std::fs::write(state.join("people.json"), "not json").unwrap();
    let failed = ask().await.expect_err("a failure is answered as one");
    assert!(failed.contains("could not be read"), "{failed}");
    let _ = server.stop().await;
}
