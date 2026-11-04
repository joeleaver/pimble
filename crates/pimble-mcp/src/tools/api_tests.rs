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

/// `move_node` is judged as the explorer judges a drop before anything is
/// sent, says what else a move does, and takes several nodes; `delete_node`
/// asks `can_delete` first.
#[tokio::test]
async fn moves_and_deletes_are_judged_before_they_are_sent() {
    use pimble_api::plan;
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        replicas_dir: Some(dir.path().join("replicas")),
        agents_path: Some(dir.path().join("agents.json")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    let (notes, notes_root) = service.create_store(dir.path().join("notes.pimble"), "Notes").await.unwrap();
    let (archive, archive_root) = service.create_store(dir.path().join("archive.pimble"), "Archive").await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));

    pimble.create_node(p(json!({ "parent": "Notes", "title": "Trips" }))).await.unwrap();
    pimble.create_node(p(json!({ "parent": "Notes/Trips", "title": "Moab" }))).await.unwrap();
    pimble.create_node(p(json!({ "parent": "Notes/Trips", "title": "Zion" }))).await.unwrap();
    pimble.create_node(p(json!({ "parent": "Notes", "title": "Inbox" }))).await.unwrap();

    // Into itself, and beside a store.
    let refused = pimble.move_node(p(json!({ "node": "Notes/Trips", "new_parent": "Notes/Trips/Moab" }))).await.unwrap_err();
    assert_eq!(refused, plan::INTO_ITSELF);
    let refused = pimble.move_node(p(json!({ "node": "Notes/Trips/Moab", "new_parent": "Notes", "after": "Archive" }))).await.unwrap_err();
    assert_eq!(refused, plan::BESIDE_A_STORE);
    // `after` names a child of the new parent.
    let refused = pimble.move_node(p(json!({ "node": "Notes/Inbox", "new_parent": "Notes", "after": "Notes/Trips/Zion" }))).await.unwrap_err();
    assert!(refused.contains("must be a child"), "{refused}");

    // Several at once, in their order; where it already is, nothing.
    let moved = pimble.move_node(p(json!({ "nodes": ["Notes/Trips/Moab", "Notes/Trips/Zion"], "new_parent": "Notes/Inbox" }))).await.unwrap();
    assert!(moved.starts_with("2 items into Inbox."), "{moved}");
    let listed = pimble.list_children(p(json!({ "node": "Notes/Inbox" }))).await.unwrap();
    assert!(listed.find("Moab").unwrap() < listed.find("Zion").unwrap(), "{listed}");
    let stays = pimble.move_node(p(json!({ "node": "Notes/Inbox/Zion", "new_parent": "Notes/Inbox" }))).await.unwrap();
    assert!(stays.starts_with(plan::WHERE_IT_IS), "{stays}");

    // Into another store: the answer says what else it does.
    let moved = pimble.move_node(p(json!({ "node": "Notes/Inbox/Zion", "new_parent": "Archive" }))).await.unwrap();
    assert!(moved.contains("A new node in Archive; it is deleted from Notes") && moved.contains("It has a new link"), "{moved}");

    // A mount whose store is not open here.
    service.create_mount(notes, notes_root, archive, archive_root, Some("Old".into())).await.unwrap();
    service.close_store(archive).await.unwrap();
    let refused = pimble.move_node(p(json!({ "node": "Notes/Trips", "new_parent": "Notes/Old" }))).await.unwrap_err();
    assert_eq!(refused, plan::MOUNT_NOT_HERE);

    // A reader may move and delete nothing, and is told so before anything is sent.
    let created = service
        .create_agent("Reader", vec![pimble_rpc::AgentGrant { store: notes, node: None, role: pimble_rpc::AgentRole::Reader }])
        .await
        .unwrap();
    let reader = PimbleClient::connect_with_auth(&url, &pimble_core::AuthMethod::Bearer { token: created.key.clone() }).await.unwrap();
    let reader = Pimble::new(Arc::new(Connection::with_client(reader)));
    let refused = reader.move_node(p(json!({ "node": "Notes/Inbox/Moab", "new_parent": "Notes/Trips" }))).await.unwrap_err();
    assert_eq!(refused, pimble_core::StoreAccess::READ_ONLY_REFUSAL);
    let refused = reader.delete_node(p(json!({ "node": "Notes/Inbox/Moab" }))).await.unwrap_err();
    assert_eq!(refused, pimble_core::StoreAccess::READ_ONLY_REFUSAL);
    let _ = server.stop().await;
}

/// Metanodes (docs/METANODES_CONTRACT.md wave 1): `set_metanode` sets and
/// clears the viewer and source, `get_node` describes one, `get_metanode`
/// lists what it shows as an outline in the tree's order.
#[tokio::test]
async fn metanodes_are_set_described_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        replicas_dir: Some(dir.path().join("replicas")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    service.create_store(dir.path().join("work.pimble"), "Work").await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));
    pimble.create_node(p(json!({ "parent": "Work", "title": "Board" }))).await.unwrap();
    for column in ["To do", "Doing"] {
        pimble.create_node(p(json!({ "parent": "Work/Board", "title": column }))).await.unwrap();
    }
    pimble.create_node(p(json!({ "parent": "Work/Board/To do", "title": "Write" }))).await.unwrap();
    pimble.create_node(p(json!({ "parent": "Work/Board/To do", "title": "Ship" }))).await.unwrap();
    pimble.create_node(p(json!({ "parent": "Work/Board/To do/Write", "title": "Too deep" }))).await.unwrap();

    // Not a metanode yet: read as its children.
    let plain = pimble.get_metanode(p(json!({ "node": "Work/Board" }))).await.unwrap();
    assert!(plain.contains("is not a metanode") && !plain.contains("Write"), "{plain}");

    let set = pimble.set_metanode(p(json!({ "node": "Work/Board", "viewer": "kanban", "depth": 2 }))).await.unwrap();
    assert!(set.contains("a metanode: the children of Board, two levels, shown with the kanban viewer"), "{set}");
    let read = pimble.get_node(p(json!({ "node": "Work/Board" }))).await.unwrap();
    assert!(read.contains("(a metanode: the children of Board, two levels, shown with the kanban viewer; get_metanode"), "{read}");

    let shown = pimble.get_metanode(p(json!({ "node": "Work/Board" }))).await.unwrap();
    let lines: Vec<&str> = shown.lines().skip(1).collect();
    assert_eq!(lines.len(), 4, "{shown}");
    assert!(lines[0].starts_with("- To do (pimble:") && lines[1].starts_with("  - Write (pimble:"), "{shown}");
    assert!(lines[2].starts_with("  - Ship (") && lines[3].starts_with("- Doing ("), "{shown}");
    assert!(!shown.contains("Too deep"), "{shown}");

    // Setting what it is already changes nothing and says so.
    let same = pimble.set_metanode(p(json!({ "node": "Work/Board", "viewer": "kanban" }))).await.unwrap_err();
    assert!(same.contains("change nothing"), "{same}");
    assert!(pimble.set_metanode(p(json!({ "node": "Work/Board", "viewer": "kanban", "depth": 0 }))).await.is_err());

    // Cleared: an ordinary node again, its source kept for next time.
    let cleared = pimble.set_metanode(p(json!({ "node": "Work/Board" }))).await.unwrap();
    assert!(cleared.contains("an ordinary node again"), "{cleared}");
    let read = pimble.get_node(p(json!({ "node": "Work/Board" }))).await.unwrap();
    assert!(!read.contains("a metanode"), "{read}");
    let again = pimble.set_metanode(p(json!({ "node": "Work/Board", "viewer": "calendar" }))).await.unwrap();
    assert!(again.contains("two levels, shown with the calendar viewer"), "{again}");
    let _ = server.stop().await;
}

/// Query metanodes (docs/QUERY_METANODES_CONTRACT.md section 7): `set_metanode`
/// takes a query and makes the node a list, `get_metanode` lists its own
/// children and then what it finds, each marked found with where it lives.
#[tokio::test]
async fn a_query_metanode_is_set_and_lists_its_finds() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        replicas_dir: Some(dir.path().join("replicas")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let url = format!("http://{}", server.addr());
    let service = PimbleClient::connect(&url).await.unwrap();
    service.create_store(dir.path().join("work.pimble"), "Work").await.unwrap();
    let pimble = Pimble::new(Arc::new(Connection::with_client(PimbleClient::connect(&url).await.unwrap())));
    for (parent, title) in [("Work", "Projects"), ("Work/Projects", "Alpha"), ("Work/Projects", "Beta"), ("Work", "Waiting"), ("Work/Waiting", "Mine")] {
        pimble.create_node(p(json!({ "parent": parent, "title": title }))).await.unwrap();
    }
    pimble.set_tags(p(json!({ "node": "Work/Projects/Alpha", "tags": ["waiting"] }))).await.unwrap();
    pimble.set_tags(p(json!({ "node": "Work/Projects/Beta", "tags": ["waiting", "done"] }))).await.unwrap();
    let alpha = pimble.api.resolve("Work/Projects/Alpha").await.unwrap();
    service.edit_node(alpha.store, alpha.node, "test", |doc, at| doc.set_field("Due", Some("2026-11-06"), at)).await.unwrap();

    let set = pimble
        .set_metanode(p(json!({ "node": "Work/Waiting", "query": { "all": [{ "tag": "waiting" }, { "no_tag": "done" }], "order": "created", "descending": true } })))
        .await
        .unwrap();
    assert!(set.contains("the nodes tagged waiting and not tagged done in Work, newest first, at most 50, shown with the list viewer"), "{set}");

    let shown = pimble.get_metanode(p(json!({ "node": "Work/Waiting" }))).await.unwrap();
    let lines: Vec<&str> = shown.lines().collect();
    assert!(lines[1].starts_with("- Mine (pimble:") && !lines[1].contains("found"), "{shown}");
    assert!(lines[2].starts_with("Found (1;"), "{shown}");
    assert!(lines[3].starts_with("- Alpha (pimble:") && lines[3].ends_with("[found, in Work / Projects]"), "{shown}");
    assert!(!shown.contains("Beta"), "{shown}");

    // A field condition; a bad one is refused with a sentence.
    pimble
        .set_metanode(p(json!({ "node": "Work/Waiting", "query": { "all": [{ "field": "Due", "is": "<", "value": "2026-12-01" }] } })))
        .await
        .unwrap();
    assert!(pimble.get_metanode(p(json!({ "node": "Work/Waiting" }))).await.unwrap().contains("- Alpha ("));
    let refused = pimble.set_metanode(p(json!({ "node": "Work/Waiting", "query": { "all": [{ "field": "Due", "is": "<" }] } }))).await.unwrap_err();
    assert!(refused.contains("needs a `value`"), "{refused}");
    let refused = pimble.set_metanode(p(json!({ "node": "Work/Waiting", "depth": 2, "query": {} }))).await.unwrap_err();
    assert!(refused.contains("not both"), "{refused}");
    let _ = server.stop().await;
}
