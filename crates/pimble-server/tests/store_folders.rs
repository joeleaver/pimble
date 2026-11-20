//! One store folder opened by two Pimble servers, each with a data
//! directory of its own: two computers whose sync tool has already delivered,
//! or one computer running two Pimbles (docs/STORE_FOLDERS_CONTRACT.md). An
//! edit made through one server's RPC reaches the other through the folder
//! alone, is announced there once, and each says the other has the folder
//! open.

use std::time::{Duration, Instant};

use base64::Engine as _;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::{EditOperation, StoreChangeKind, StoreChangedNotification};
use pimble_server::{PimbleServer, ServerConfig};
use tempfile::TempDir;

/// A server whose every path is inside `dir` (a computer of its own).
async fn computer(dir: &TempDir) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        keystore_path: Some(dir.path().join("keys.json")),
        credentials_path: Some(dir.path().join("credentials.json")),
        replicas_dir: Some(dir.path().join("replicas")),
        agents_path: Some(dir.path().join("agents.json")),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

async fn wait_until<F, Fut>(within: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + within;
    loop {
        if check().await {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn append(client: &PimbleClient, store_id: StoreId, node: NodeId, words: &str) {
    let held = client.get_node(store_id, node).await.unwrap();
    let mut doc = NodeDoc::load(&held.content).unwrap();
    let before = doc.state_vector();
    doc.append_markdown(words).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.diff_since(&before).unwrap());
    client.apply_edit(store_id, node, "test-editor", EditOperation::IncrementalChanges { changes }).await.expect("the edit applies");
}

async fn text_of(client: &PimbleClient, store_id: StoreId, node: NodeId) -> String {
    client.get_node(store_id, node).await.map(|n| NodeDoc::text_of(&n.content)).unwrap_or_default()
}

/// How long an edit may take to cross: the content flush (750 ms), the
/// folder's settle (500 ms), with room for a loaded machine.
const BOUND: Duration = Duration::from_secs(10);

#[tokio::test]
async fn an_edit_through_one_server_reaches_the_other_through_the_folder() {
    let (dir_a, dir_b, folder) = (TempDir::new().unwrap(), TempDir::new().unwrap(), TempDir::new().unwrap());
    let (mut a_server, a) = computer(&dir_a).await;
    let (mut b_server, b) = computer(&dir_b).await;
    let path = folder.path().join("Shared.pimble");
    let (store_id, root) = a.create_store(&path, "Shared").await.unwrap();
    let note = a.create_node(store_id, Some(root), "document", "Note").await.unwrap();
    append(&a, store_id, note, "first words").await;
    // A's flush, so the folder holds the note before B opens it.
    assert!(wait_until(BOUND, || async { path.join("nodes").join(format!("{note}.yrs")).exists() }).await);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    b.open_store(&path).await.expect("B opens the same folder");
    assert!(text_of(&b, store_id, note).await.contains("first words"));

    let mut b_changes = b.subscribe_store_changes(store_id).await.unwrap();

    // A writes; B has it within the bound, through nothing but the folder.
    append(&a, store_id, note, "typed on A").await;
    assert!(wait_until(BOUND, || async { text_of(&b, store_id, note).await.contains("typed on A") }).await, "B sees A's edit");

    // Announced once on B: each kind the change earns, once, from the folder,
    // and nothing again for B's own write that may follow.
    let source = format!("folder:{store_id}");
    let mut heard: Vec<StoreChangedNotification> = Vec::new();
    let quiet_until = Instant::now() + Duration::from_secs(4);
    while let Ok(Some(Ok(notification))) = tokio::time::timeout(quiet_until.saturating_duration_since(Instant::now()), b_changes.next()).await {
        heard.push(notification);
    }
    let from_folder: Vec<&StoreChangedNotification> = heard.iter().filter(|n| n.source_client_id.as_deref() == Some(source.as_str())).collect();
    let content: Vec<_> = from_folder.iter().filter(|n| matches!(n.change_kind, StoreChangeKind::ContentUpdated { node_id } if node_id == note)).collect();
    assert_eq!(content.len(), 1, "A's edit is announced on B once: {heard:?}");
    assert!(content[0].update.is_some(), "with the bytes, for B's windows and links");

    // The other way, and a node B makes joins A's tree.
    let made = b.create_node(store_id, Some(root), "document", "Made on B").await.unwrap();
    append(&b, store_id, note, "typed on B").await;
    assert!(
        wait_until(BOUND, || async { a.get_children(store_id, root).await.map(|(_, c)| c.iter().any(|n| n.id == made)).unwrap_or(false) }).await,
        "A's tree has B's new node"
    );
    assert!(wait_until(BOUND, || async { text_of(&a, store_id, note).await.contains("typed on B") }).await);

    // Each says the other has the folder open, running the same Pimble.
    let listed = b.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap();
    assert_eq!(listed.also_open_on.len(), 1, "{:?}", listed.also_open_on);
    assert!(!listed.also_open_on[0].older);
    let listed = a.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap();
    assert_eq!(listed.also_open_on.len(), 1, "A reads B's file within a minute, or when it opens: {:?}", listed.also_open_on);

    // Both flushed, both folders' file of the note hold everything: one file
    // per node, no conflict copies.
    a_server.stop().await.unwrap();
    b_server.stop().await.unwrap();
    let text = NodeDoc::load(&std::fs::read(path.join("nodes").join(format!("{note}.yrs"))).unwrap()).unwrap().text();
    assert!(text.contains("typed on A") && text.contains("typed on B"), "{text:?}");
}

/// A computer whose Pimble is older is named, with the sentence asking for it
/// to be updated; this computer's own file is in `computers/` too, and the
/// per-computer state is outside the folder.
#[tokio::test]
async fn an_older_pimble_on_another_computer_is_named() {
    let (dir, folder) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let path = folder.path().join("Mine.pimble");
    {
        let (mut server, client) = computer(&dir).await;
        client.create_store(&path, "Mine").await.unwrap();
        server.stop().await.unwrap();
    }
    std::fs::create_dir_all(path.join("computers")).unwrap();
    let seen = chrono::Utc::now() - chrono::Duration::minutes(3);
    std::fs::write(
        path.join("computers").join("0123456789abcdef0123.json"),
        serde_json::to_vec(&serde_json::json!({ "device": "0123456789abcdef0123", "name": "old-laptop", "last_seen": seen, "version": "0.1.0" })).unwrap(),
    )
    .unwrap();

    let (mut server, client) = computer(&dir).await;
    let store = client.open_store(&path).await.unwrap();
    assert_eq!(store.also_open_on.len(), 1, "{:?}", store.also_open_on);
    let other = &store.also_open_on[0];
    assert_eq!((other.name.as_str(), other.version.as_str(), other.older), ("old-laptop", "0.1.0", true));
    assert!(other.sentence(chrono::Utc::now()).starts_with("Also open on old-laptop (seen 3 minutes ago)"));
    assert!(other.update_sentence().unwrap().contains("Please update Pimble on old-laptop"));
    let own: Vec<_> = std::fs::read_dir(path.join("computers")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(own.len(), 2, "this computer says it is there too: {own:?}");
    assert!(!path.join("index").exists(), "the search index is not in the folder");
    assert!(dir.path().join("stores").join(store.id.to_string()).join("index").exists(), "it is in this computer's state directory");
    server.stop().await.unwrap();
}
