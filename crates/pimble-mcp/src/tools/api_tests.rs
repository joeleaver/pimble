//! What changed when the tools moved onto `pimble_api` (docs/API_CONTRACT.md
//! wave 2, open decision 5), against a real server: an old link is followed
//! where its node went, a path may start anywhere, a discussion on a node
//! carries the icon the app gives it, and what is made under a mount goes to
//! its source.

use std::sync::Arc;

use pimble_client::PimbleClient;
use pimble_core::custom_keys;
use pimble_server::{PimbleServer, ServerConfig};
use rmcp::handler::server::wrapper::Parameters;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::Pimble;
use crate::connection::Connection;

fn p<T: DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
    Parameters(serde_json::from_value(v).expect("params"))
}

/// The link at the end of a tool's answer: `(pimble:...)`.
fn link_in(answer: &str) -> String {
    let start = answer.rfind("(pimble:").expect("a link") + 1;
    answer[start..start + answer[start..].find(')').unwrap()].to_string()
}

#[tokio::test]
async fn links_paths_discussions_and_mounts_as_the_app_has_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        replicas_dir: Some(dir.path().join("replicas")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    let (notes, notes_root) = service.create_store(dir.path().join("notes.pimble"), "Notes").await.unwrap();
    let (archive, archive_root) = service.create_store(dir.path().join("archive.pimble"), "Archive").await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));

    pimble.create_node(p(json!({ "parent": "Notes", "title": "Trips" }))).await.unwrap();
    let made = pimble.create_node(p(json!({ "parent": "Notes/Trips", "title": "Moab", "markdown": "Arches first." }))).await.unwrap();
    let moab = link_in(&made);
    // A path may start below the store's name.
    let read = pimble.get_node(p(json!({ "node": "Trips/Moab" }))).await.unwrap();
    assert!(read.contains("Arches first.") && read.contains("path: Notes/Trips/Moab"), "{read}");

    // A discussion on a node has the icon the app gives one.
    let started = pimble.start_discussion(p(json!({ "on": "Trips/Moab", "message": "When do we leave?" }))).await.unwrap();
    let discussion = pimble_core::PimbleUrl::parse(started.split_whitespace().nth(3).unwrap()).expect("the discussion's link");
    let node = service.get_node(discussion.store, discussion.node).await.unwrap();
    assert_eq!(node.metadata.custom.get(custom_keys::ICON), Some(&json!(pimble_api::recipes::DISCUSSION_ICON)));

    // Moved to another store, the old link still names it.
    let moved = pimble.move_node(p(json!({ "node": moab, "new_parent": "Archive" }))).await.unwrap();
    assert!(moved.contains("It has a new link"), "{moved}");
    let followed = pimble.get_node(p(json!({ "node": moab }))).await.unwrap();
    assert!(followed.contains("path: Archive/Moab"), "{followed}");

    // A node made under a mount, and a discussion node too, go to its source.
    let (mount, _) = service.create_mount(notes, notes_root, archive, archive_root, Some("Old".into())).await.unwrap();
    let _ = mount;
    let made = pimble.create_node(p(json!({ "parent": "Notes/Old", "title": "Zion" }))).await.unwrap();
    assert!(made.contains("Archive/Zion"), "{made}");
    let made = pimble.start_discussion(p(json!({ "parent": "Notes/Old", "topic": "Plans", "message": "Hello" }))).await.unwrap();
    assert!(made.contains("Archive/Plans"), "{made}");
    let _ = server.stop().await;
}
