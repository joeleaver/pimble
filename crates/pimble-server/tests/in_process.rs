//! The handler apart from its transport (docs/ONE_CODEBASE_CONTRACT.md wave 1):
//! a server reached through `PimbleClient::in_process`, a channel pair served
//! by `pimble_rpc::dispatch::session::serve` with no socket, answers as it
//! answers over its WebSocket, judges every call by the principal the
//! caller attached, and sends its notifications.

use pimble_host::disk::testfs as tfs;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use pimble_core::StoreAccess;
use pimble_rpc::{EditOperation, StoreChangeKind};
use pimble_server::{service_extensions, Grant, PimbleServer, Principal, Role, RpcHandler, ServerConfig};
use pimble_store::StoreManager;
use tokio::sync::RwLock;

fn edit_of(text: &str) -> EditOperation {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let doc = pimble_crdt::NodeDoc::from_plain_text(text).unwrap();
    EditOperation::IncrementalChanges { changes: STANDARD.encode(doc.save()) }
}

fn as_user(grants: HashMap<pimble_core::StoreId, Grant>) -> jsonrpsee::Extensions {
    let mut ext = jsonrpsee::Extensions::new();
    ext.insert(Principal::User { sub: "reader".into(), email: "reader@example.com".into(), grants });
    ext
}

#[tokio::test]
async fn a_store_opened_an_edit_and_its_notification_in_process() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig::in_dir(dir.path()));
    server.start_in_process().await.unwrap();
    let client = server.connect_in_process(service_extensions()).unwrap();
    let watcher = server.connect_in_process(service_extensions()).unwrap();
    assert_eq!(client.url().as_str(), pimble_client::in_process::IN_PROCESS_URL);

    // A store created, closed, and opened again.
    let path = dir.path().join("in-process.pimble");
    let (store_id, root) = client.create_store(&path, "In process").await.unwrap();
    let note = client.create_node(store_id, Some(root), "document", "Note").await.unwrap();
    client.close_store(store_id).await.unwrap();
    let opened = client.open_store(&path).await.unwrap();
    assert_eq!(opened.id, store_id);
    assert_eq!(client.get_node(store_id, note).await.unwrap().metadata.title, "Note");
    assert!(client.list_stores().await.unwrap().iter().any(|s| s.id == store_id));

    // Another connection hears the edit.
    let mut changes = watcher.subscribe_store_changes(store_id).await.unwrap();
    client.apply_edit(store_id, note, "in-process-test", edit_of("typed with no socket")).await.unwrap();
    let heard = loop {
        let notif = tokio::time::timeout(Duration::from_secs(5), changes.next()).await.expect("a storeChanged in time").unwrap().unwrap();
        if matches!(notif.change_kind, StoreChangeKind::ContentUpdated { node_id } if node_id == note) {
            break notif;
        }
    };
    assert_eq!(heard.store_id, store_id);
    assert_eq!(heard.source_client_id.as_deref(), Some("in-process-test"));

    // Unsubscribing is answered, and the connection goes on answering.
    changes.unsubscribe().await.unwrap();
    client.apply_edit(store_id, note, "in-process-test", edit_of("after the unsubscribe")).await.unwrap();
    assert!(watcher.list_stores().await.is_ok());

    drop((client, watcher));
    server.stop().await.unwrap();
}

#[tokio::test]
async fn a_reader_is_refused_in_process() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = PimbleServer::with_config(ServerConfig::in_dir(dir.path()));
    server.start_in_process().await.unwrap();
    let service = server.connect_in_process(service_extensions()).unwrap();
    let (store_id, root) = service.create_store(dir.path().join("read.pimble"), "Read").await.unwrap();
    let note = service.create_node(store_id, Some(root), "document", "Note").await.unwrap();
    let (other, _) = service.create_store(dir.path().join("other.pimble"), "Other").await.unwrap();

    let reader = server.connect_in_process(as_user(HashMap::from([(store_id, Grant::whole(Role::Reader))]))).unwrap();
    // A reader reads.
    assert_eq!(reader.get_node(store_id, note).await.unwrap().metadata.title, "Note");
    let mut changes = reader.subscribe_store_changes(store_id).await.expect("a reader may listen");
    // And is refused a write, with the sentence the app shows.
    let err = reader.apply_edit(store_id, note, "reader", edit_of("no")).await.expect_err("a reader's edit").to_string();
    assert_eq!(err, StoreAccess::READ_ONLY_REFUSAL);
    // A store it has no grant on, read or subscribed to, is refused.
    let err = reader.get_node(other, root).await.expect_err("no grant on the other store").to_string();
    assert!(err.contains("no grant for store"), "{err}");
    let err = reader.subscribe_store_changes(other).await.map(|_| ()).expect_err("no grant to subscribe with").to_string();
    assert!(err.contains("no grant for store"), "{err}");
    // A `Service`-only method is refused a user (-32004).
    match reader.create_store(dir.path().join("mine.pimble"), "Mine").await {
        Err(pimble_client::ClientError::Server { code, .. }) => assert_eq!(code, -32004),
        other => panic!("createStore by a user: {other:?}"),
    }
    // The refused edit changed nothing the reader hears.
    service.apply_edit(store_id, note, "service", edit_of("allowed")).await.unwrap();
    let notif = tokio::time::timeout(Duration::from_secs(5), changes.next()).await.expect("the service's edit").unwrap().unwrap();
    assert_eq!(notif.source_client_id.as_deref(), Some("service"));

    drop((service, reader, changes));
    server.stop().await.unwrap();
}

fn without_subscription_id(answer: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(answer).unwrap();
    if value["result"].is_u64() {
        value["result"] = serde_json::json!("<a subscription id>");
        return value.to_string();
    }
    answer.to_string()
}

/// The same requests, as text, answered by jsonrpsee's server path
/// (`into_rpc`, what the WebSocket server runs) and by the in-process
/// session: the same bytes, errors included.
#[tokio::test]
async fn both_transports_answer_the_same_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let handler = RpcHandler::in_dir(Arc::new(RwLock::new(StoreManager::new())), dir.path());
    let (store_id, root) = {
        let service = service_extensions();
        use pimble_rpc::PimbleApiServer;
        let created = handler
            .create_store(&service, pimble_rpc::CreateStoreRequest { kind: Default::default(), store_id: None, path: dir.path().join("wire.pimble"), name: "Wire".into() })
            .await
            .unwrap();
        (created.store_id, created.root_node_id)
    };
    // A store that cannot be created there (a file is in the way), the same
    // refusal from both.
    tfs::write(dir.path().join("blocker"), b"").unwrap();
    let blocked = dir.path().join("blocker").join("x.pimble");
    let reader = || as_user(HashMap::from([(store_id, Grant::whole(Role::Reader))]));
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"pimble_nope","params":[]}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"pimble_getNode","params":[1]}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":3,"method":"pimble_getNode","params":[]}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":4,"method":"pimble_getNode"}"#.to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":5,"method":"pimble_getNode","params":[{{"store_id":"{store_id}","node_id":"{root}"}}]}}"#),
        format!(r#"{{"jsonrpc":"2.0","id":"six","method":"pimble_getNode","params":{{"request":{{"store_id":"{store_id}","node_id":"{root}"}}}}}}"#),
        format!(r#"{{"jsonrpc":"2.0","id":7,"method":"pimble_getChildren","params":[{{"store_id":"{store_id}","node_id":"{root}"}}]}}"#),
        format!(r#"{{"jsonrpc":"2.0","id":8,"method":"pimble_applyEdit","params":[{{"store_id":"{store_id}","node_id":"{root}","client_id":"x","operation":{{"IncrementalChanges":{{"changes":"AAA="}}}}}}]}}"#),
        r#"{"jsonrpc":"2.0","id":9,"method":"pimble_listStores","params":[]}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":10,"method":"pimble_whoAmI","params":[]}"#.to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":11,"method":"pimble_createStore","params":[{{"path":{},"name":"x"}}]}}"#, serde_json::to_string(&blocked).unwrap()),
        r#"{"jsonrpc":"2.0","id":12,"method":"pimble_subscribeStoreChanges","params":["not an id"]}"#.to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":13,"method":"pimble_subscribeNodeChanges","params":["{}","{root}"]}}"#, pimble_core::StoreId::new()),
        r#"{"jsonrpc":"2.0","id":14,"method":"pimble_unsubscribeStoreChanges","params":["123"]}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":15,"method":"pimble_serverInfo"}"#.to_string(),
    ];
    for ext in [service_extensions(), reader()] {
        let mut module = pimble_rpc::PimbleApiServer::into_rpc(handler.clone());
        module.extensions_mut().insert(ext.get::<Principal>().cloned().unwrap());
        let (client_end, server_end) = pimble_rpc::dispatch::session::channel();
        let session = tokio::spawn(pimble_rpc::dispatch::session::serve(Arc::new(handler.clone()), ext, server_end));
        let pimble_rpc::dispatch::session::ClientEnd { to_server, mut from_server } = client_end;
        for request in &requests {
            let (native, _notifications) = module.raw_json_request(request, 16).await.unwrap();
            to_server.send(request.clone()).unwrap();
            let in_process = tokio::time::timeout(Duration::from_secs(5), from_server.recv()).await.expect("an answer").unwrap();
            // A subscription id is the one thing chosen per server (jsonrpsee
            // draws a random integer; the session counts).
            assert_eq!(without_subscription_id(&in_process), without_subscription_id(&native), "{request}");
        }
        drop(to_server);
        session.await.unwrap();
    }
}
