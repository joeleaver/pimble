//! Internal nodes in the MCP's listings (docs/THEMES_CONTRACT.md section
//! 3.1a): `list_children`, `find_node` and `search` leave theme nodes out
//! unless `include_internal` names their kind, an unknown kind is refused,
//! and what is left out is not counted or given a hit's place.

use std::sync::Arc;

use pimble_api::{recipes, Addr};
use pimble_client::PimbleClient;
use pimble_core::Place;
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
async fn themes_are_listed_only_when_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig::in_dir(dir.path()));
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    let (store, root) = service.create_store(dir.path().join("w.pimble"), "Work").await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));
    pimble.create_node(p(json!({ "parent": "Work", "title": "Lighthouse note", "markdown": "A lighthouse." }))).await.unwrap();
    pimble.api.create(Addr::new(store, root), Place::Last, recipes::theme("Lighthouse theme", &Default::default()).unwrap()).await.unwrap();

    let plain = pimble.list_children(p(json!({ "node": "Work" }))).await.unwrap();
    assert!(plain.contains("Lighthouse note") && !plain.contains("Lighthouse theme"), "{plain}");
    let all = pimble.list_children(p(json!({ "node": "Work", "include_internal": ["themes"] }))).await.unwrap();
    assert!(all.contains("Lighthouse note") && all.contains("Lighthouse theme"), "{all}");
    let unknown = pimble.list_children(p(json!({ "node": "Work", "include_internal": ["widgets"] }))).await.unwrap_err();
    assert!(unknown.contains("\"widgets\" is not an internal kind; the kinds are themes."), "{unknown}");
    let found = pimble.find_node(p(json!({ "title": "lighthouse" }))).await.unwrap();
    assert!(found.contains("Lighthouse note") && !found.contains("Lighthouse theme"), "{found}");
    let found = pimble.find_node(p(json!({ "title": "lighthouse", "include_internal": ["themes"] }))).await.unwrap();
    assert!(found.contains("Lighthouse theme"), "{found}");

    // A folder whose only child is a theme (put there by an older client:
    // nothing made here goes inside a theme, and a theme inside a folder is
    // as hidden) reads as having no children.
    let folder = pimble.api.create(Addr::new(store, root), Place::Last, recipes::document("Harbor folder", None).unwrap()).await.unwrap().node;
    pimble.api.create(Addr::new(store, folder), Place::Last, recipes::theme("Harbor theme", &Default::default()).unwrap()).await.unwrap();
    let listed = pimble.list_children(p(json!({ "node": "Work" }))).await.unwrap();
    let line = listed.lines().find(|l| l.contains("Harbor folder")).unwrap_or_default();
    assert!(!line.contains("child"), "{listed}");
    let listed = pimble.list_children(p(json!({ "node": "Work", "include_internal": ["themes"] }))).await.unwrap();
    assert!(listed.lines().any(|l| l.contains("Harbor folder") && l.contains("1 child")), "{listed}");

    // Once both are indexed, search shows the theme only when asked to.
    let mut found = String::new();
    for _ in 0..60 {
        found = pimble.search(p(json!({ "query": "lighthouse", "include_internal": ["themes"] }))).await.unwrap_or_default();
        if found.contains("Lighthouse theme") && found.contains("Lighthouse note") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert!(found.contains("Lighthouse theme") && found.contains("Lighthouse note"), "{found}");
    let plain = pimble.search(p(json!({ "query": "lighthouse" }))).await.unwrap();
    assert!(plain.contains("Lighthouse note") && !plain.contains("Lighthouse theme"), "{plain}");
    // The hidden theme does not take the one place there is.
    let one = pimble.search(p(json!({ "query": "lighthouse", "limit": 1 }))).await.unwrap();
    assert!(one.contains("Lighthouse note"), "{one}");
}
