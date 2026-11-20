//! Tags from a note's text (docs/HASHTAGS_CONTRACT.md): get_node lists the
//! #hashtags of the text among the tags and says which they are, set_tags
//! writes entered tags only and removes from the text a hashtag left out of
//! its list, and search finds a note by a hashtag in it.

use std::sync::Arc;

use pimble_client::PimbleClient;
use pimble_server::{PimbleServer, ServerConfig};
use rmcp::handler::server::wrapper::Parameters;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::Pimble;
use crate::connection::Connection;

fn p<T: DeserializeOwned>(v: serde_json::Value) -> Parameters<T> {
    Parameters(serde_json::from_value(v).expect("params"))
}

fn line<'a>(read: &'a str, prefix: &str) -> Option<&'a str> {
    read.lines().find_map(|l| l.strip_prefix(prefix))
}

#[tokio::test]
async fn get_node_and_set_tags_know_the_text_tags() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        replicas_dir: Some(dir.path().join("replicas")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    service.create_store(dir.path().join("notes.pimble"), "Notes").await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));

    let md = "Sow #marigold seeds and #Basil today.\n\nMore #marigold later, not `#code`.";
    pimble.create_node(p(json!({ "parent": "Notes", "title": "Garden", "markdown": md }))).await.unwrap();
    pimble.set_tags(p(json!({ "node": "Notes/Garden", "tags": ["outdoors", "marigold", "basil"] }))).await.unwrap();
    let read = pimble.get_node(p(json!({ "node": "Notes/Garden" }))).await.unwrap();
    // The text tags in the list were not entered again: one each.
    assert_eq!(line(&read, "tags: "), Some("outdoors, marigold, Basil"), "{read}");
    assert_eq!(line(&read, "tags from the text: "), Some("#marigold, #Basil"), "{read}");

    // A text tag left out of the list goes from the text, every occurrence.
    let said = pimble.set_tags(p(json!({ "node": "Notes/Garden", "tags": ["outdoors", "basil"] }))).await.unwrap();
    assert!(said.contains("Removed from the text: #marigold."), "{said}");
    let read = pimble.get_node(p(json!({ "node": "Notes/Garden" }))).await.unwrap();
    assert_eq!(line(&read, "tags: "), Some("outdoors, Basil"), "{read}");
    assert!(!read.contains("#marigold") && read.contains("Sow seeds and #Basil today.") && read.contains("More later"), "{read}");

    // Search finds the note by a hashtag in its text.
    let mut found = String::new();
    for _ in 0..60 {
        found = pimble.search(p(json!({ "query": "basil" }))).await.unwrap_or_default();
        if found.contains("Garden") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert!(found.contains("Garden"), "{found}");
    let _ = server.stop().await;
}
