//! AI agents (docs/AGENTS_CONTRACT.md) against a real server: an agent's key
//! reaches what its grants reach and nothing else, through every kind of
//! call; a reader is refused writes with the person's own sentence; lifecycle
//! calls are never an agent's; and removing an agent, or narrowing it, holds
//! its open connection to that on the next call.

use std::time::Duration;

use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreAccess, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::{AgentGrant, AgentRole, EditOperation, Opener};
use pimble_server::{PimbleServer, ServerConfig};
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    server: PimbleServer,
    service: PimbleClient,
    store: StoreId,
    root: NodeId,
    /// Readable by the agent.
    project: NodeId,
    /// Inside `project`, editable by the agent.
    drafts: NodeId,
    /// Not granted at all.
    journal: NodeId,
}

async fn seed(client: &PimbleClient, store: StoreId, node: NodeId, text: &str) {
    let doc = NodeDoc::from_plain_text(text).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.save());
    client.apply_edit(store, node, "test", EditOperation::IncrementalChanges { changes }).await.expect("seed content applies");
}

/// An edit of `node`'s text by `client`, as an agent would make one.
async fn edit(client: &PimbleClient, store: StoreId, node: NodeId, text: &str) -> pimble_client::Result<()> {
    let current = client.get_node(store, node).await?;
    let base = NodeDoc::load(&current.content).unwrap();
    let richer = NodeDoc::from_plain_text(text).unwrap();
    let diff = richer.diff_since(&base.state_vector()).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(&diff);
    client.apply_edit(store, node, "agent-test", EditOperation::IncrementalChanges { changes }).await.map(|_| ())
}

async fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig {
        ..ServerConfig::in_dir(dir.path())
    });
    server.start().await.expect("server starts");
    let service = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("service connects");
    let (store, root) = service.create_store(dir.path().join("work.pimble"), "Work").await.expect("store");
    let project = service.create_node(store, Some(root), "document", "Project").await.unwrap();
    let drafts = service.create_node(store, Some(project), "document", "Drafts").await.unwrap();
    let journal = service.create_node(store, Some(root), "document", "Journal").await.unwrap();
    seed(&service, store, project, "projectword plans").await;
    seed(&service, store, drafts, "draftword text").await;
    seed(&service, store, journal, "journalword private").await;
    Fixture { _dir: dir, server, service, store, root, project, drafts, journal }
}

async fn connect_agent(server: &PimbleServer, key: &str) -> PimbleClient {
    PimbleClient::connect_with_auth(format!("http://{}", server.addr()), &AuthMethod::Bearer { token: key.to_string() })
        .await
        .expect("the agent connects")
}

#[tokio::test]
async fn an_agent_reaches_its_grants_and_nothing_else() {
    let f = fixture().await;
    let created = f
        .service
        .create_agent(
            "Claude Desktop",
            vec![
                AgentGrant { store: f.store, node: Some(f.project), role: AgentRole::Reader },
                AgentGrant { store: f.store, node: Some(f.drafts), role: AgentRole::Editor },
            ],
        )
        .await
        .expect("an agent is made");
    assert_eq!(f.service.list_agents().await.unwrap().len(), 1);
    let agent = connect_agent(&f.server, &created.key).await;

    // Its store, under its real name, showing the roots it reaches.
    let stores = agent.list_stores().await.unwrap();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].name, "Work", "an agent sees its person's own store name");
    assert_eq!(stores[0].id, f.store);

    // Reads: inside the grants, yes; outside, refused.
    assert!(agent.get_node(f.store, f.project).await.is_ok());
    assert!(agent.get_node(f.store, f.drafts).await.is_ok());
    assert!(agent.get_node(f.store, f.journal).await.is_err(), "the journal was never granted");
    assert!(agent.get_children(f.store, f.root).await.map(|(_, c)| c.iter().all(|n| n.id != f.journal)).unwrap_or(true));

    // Writes: the editor root yes; the reader root refused with the sentence.
    edit(&agent, f.store, f.drafts, "draftword text, and more").await.expect("drafts is the agent's to edit");
    let refused = edit(&agent, f.store, f.project, "projectword changed").await.expect_err("project is read-only to it");
    assert!(refused.to_string().contains(StoreAccess::READ_ONLY_REFUSAL), "{refused}");
    assert!(edit(&agent, f.store, f.journal, "journalword changed").await.is_err());

    // Lifecycle is never an agent's.
    assert!(agent.create_store(f._dir.path().join("other.pimble"), "Other").await.is_err());
    assert!(agent.list_agents().await.is_err(), "an agent cannot list or make agents");

    // Search: what it reaches, never the journal.
    let mut found_project = false;
    for _ in 0..50 {
        let hits = agent.search("projectword", vec![], false, 10).await.unwrap_or_default();
        if hits.iter().any(|h| h.node_id == f.project) {
            found_project = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(found_project, "the agent finds what it may read");
    let mut indexed = false;
    for _ in 0..50 {
        if f.service.search("journalword", vec![], false, 10).await.unwrap_or_default().iter().any(|h| h.node_id == f.journal) {
            indexed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(indexed, "the journal is indexed (the person finds it)");
    let hits = agent.search("journalword", vec![], false, 10).await.unwrap_or_default();
    assert!(hits.iter().all(|h| h.node_id != f.journal), "the agent never finds the journal: {hits:?}");
}

#[tokio::test]
async fn removing_or_narrowing_an_agent_holds_its_open_connection_to_it() {
    let f = fixture().await;
    let created = f
        .service
        .create_agent("Cursor", vec![AgentGrant { store: f.store, node: None, role: AgentRole::Editor }])
        .await
        .unwrap();
    let agent = connect_agent(&f.server, &created.key).await;
    edit(&agent, f.store, f.journal, "journalword private, edited").await.expect("a whole-store editor edits");

    // Narrowed to reading the project: the same connection, the next call.
    f.service
        .update_agent(&created.agent.id, None, Some(vec![AgentGrant { store: f.store, node: Some(f.project), role: AgentRole::Reader }]))
        .await
        .unwrap();
    assert!(agent.get_node(f.store, f.journal).await.is_err(), "narrowed away");
    assert!(edit(&agent, f.store, f.project, "projectword changed").await.is_err(), "now only a reader");
    assert!(agent.get_node(f.store, f.project).await.is_ok());

    // Removed: nothing, on the open connection and on a new one.
    f.service.remove_agent(&created.agent.id).await.unwrap();
    assert!(agent.get_node(f.store, f.project).await.is_err());
    assert!(
        PimbleClient::connect_with_auth(format!("http://{}", f.server.addr()), &AuthMethod::Bearer { token: created.key.clone() })
            .await
            .is_err(),
        "a removed agent's key opens nothing"
    );
}

/// Write who is who in `store` as this device keeps it
/// (`<state dir>/people.json`), as a refresh from the accounts service would.
fn keep_people(f: &Fixture, json: &str) {
    let dir = f._dir.path().join("stores").join(f.store.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("people.json"), json).unwrap();
}

/// Put a share's marker on `node`.
async fn mark_shared(f: &Fixture, node: NodeId) {
    let marker = serde_json::json!({ "v": 1, "key_id": uuid::Uuid::new_v4(), "url": "https://pimble.app", "name": "Plans" });
    f.service.edit_node(f.store, node, "test", |doc, now| doc.set_custom(pimble_core::custom_keys::SHARE, &marker, now)).await.unwrap();
}

#[tokio::test]
async fn an_agent_learns_who_can_open_a_note_it_reads_and_no_more() {
    let f = fixture().await;
    let created = f
        .service
        .create_agent("Claude", vec![AgentGrant { store: f.store, node: Some(f.drafts), role: AgentRole::Reader }])
        .await
        .unwrap();
    let agent = connect_agent(&f.server, &created.key).await;

    let alone = agent.who_can_open(f.store, f.drafts).await.expect("a note it reads");
    assert!(alone.people.is_empty() && !alone.incomplete, "{alone:?}");

    // Shared, and this computer has not been told who is in it.
    mark_shared(&f, f.project).await;
    let unknown = agent.who_can_open(f.store, f.drafts).await.unwrap();
    assert!(unknown.people.is_empty() && unknown.incomplete, "{unknown:?}");

    let scope = |root: NodeId, id: &str, name: &str| {
        serde_json::json!({ "root": root, "share_name": "Plans", "people": [{ "user_id": id, "name": name, "notice_key": "a-notice-key" }] })
    };
    keep_people(&f, &serde_json::json!({ "scopes": [scope(f.project, "u-will", "Will"), scope(f.journal, "u-sam", "Sam")] }).to_string());
    // The share is above what the agent may read: its members can open the
    // note all the same. Another share's are not named.
    let known = agent.who_can_open(f.store, f.drafts).await.unwrap();
    assert_eq!(known.people, vec![Opener { user_id: "u-will".into(), name: "Will".into() }]);
    assert!(!known.incomplete, "{known:?}");
    assert!(!serde_json::to_string(&known).unwrap().contains("a-notice-key"));

    // Only for a note it may read, and `cloudPeople` stays closed.
    for node in [f.journal, f.project, NodeId::new()] {
        let refused = agent.who_can_open(f.store, node).await.expect_err("outside its grant").to_string();
        assert!(refused.contains("no grant for this document"), "{refused}");
    }
    assert!(agent.who_can_open(StoreId::new(), f.drafts).await.is_err(), "another store");
    let closed = agent.cloud_people(f.store).await.expect_err("the operator's alone").to_string();
    assert!(closed.contains("only available to this server's own operator"), "{closed}");

    // The operator asks too; a node that is not there is an error.
    assert_eq!(f.service.who_can_open(f.store, f.journal).await.unwrap().people[0].name, "Sam");
    assert!(f.service.who_can_open(f.store, NodeId::new()).await.is_err());

    // What cannot be read is an error, never "nobody".
    keep_people(&f, "not json");
    let failed = agent.who_can_open(f.store, f.drafts).await.expect_err("a broken list").to_string();
    assert!(failed.contains("could not be read"), "{failed}");
}

#[tokio::test]
async fn grants_must_name_what_is_here() {
    let f = fixture().await;
    let missing_store = f.service.create_agent("Bad", vec![AgentGrant { store: StoreId::new(), node: None, role: AgentRole::Reader }]).await;
    assert!(missing_store.is_err());
    let missing_node = f.service.create_agent("Bad", vec![AgentGrant { store: f.store, node: Some(NodeId::new()), role: AgentRole::Reader }]).await;
    assert!(missing_node.is_err());
    assert!(f.service.create_agent("  ", vec![]).await.is_err(), "an agent has a name");
    assert!(f.service.list_agents().await.unwrap().is_empty(), "nothing was made");
}

/// `backlinks` (docs/PROPERTIES_CONTRACT.md section 5, "Links here"): the
/// nodes whose text links to a node, from the index, filtered to what the
/// caller may read; a node the caller cannot reach is refused.
#[tokio::test]
async fn backlinks_answer_what_the_caller_may_read() {
    let f = fixture().await;
    let link = pimble_core::PimbleUrl::node(f.store, f.drafts).to_string();
    for node in [f.project, f.journal] {
        let text = format!("See [the drafts]({link})");
        f.service.edit_node(f.store, node, "test", |doc, _| doc.append_markdown(&text).map(|_| ())).await.unwrap();
    }
    let mut linking = Vec::new();
    for _ in 0..50 {
        linking = f.service.backlinks(f.store, f.drafts).await.unwrap();
        if linking.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    linking.sort_by_key(|n| n.0);
    let mut want = vec![f.project, f.journal];
    want.sort_by_key(|n| n.0);
    assert_eq!(linking, want, "the person sees every linking node");
    assert!(f.service.backlinks(f.store, f.journal).await.unwrap().is_empty());

    let created = f
        .service
        .create_agent("Claude Desktop", vec![AgentGrant { store: f.store, node: Some(f.project), role: AgentRole::Reader }])
        .await
        .unwrap();
    let agent = connect_agent(&f.server, &created.key).await;
    assert_eq!(agent.backlinks(f.store, f.drafts).await.unwrap(), vec![f.project], "never the journal it cannot read");
    assert!(agent.backlinks(f.store, f.journal).await.is_err(), "a node outside its grants is refused");
}
