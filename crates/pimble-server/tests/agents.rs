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
use pimble_rpc::{AgentGrant, AgentRole, EditOperation};
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
        addr: "127.0.0.1:0".parse().unwrap(),
        credentials_path: Some(dir.path().join("credentials.json")),
        keystore_path: Some(dir.path().join("keys.json")),
        replicas_dir: Some(dir.path().join("replicas")),
        agents_path: Some(dir.path().join("agents.json")),
        ..Default::default()
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
