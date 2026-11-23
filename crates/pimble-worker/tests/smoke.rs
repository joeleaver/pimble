//! The wasm smoke test (docs/ONE_CODEBASE_CONTRACT.md section 8, wave 3): the
//! Pimble server built for `wasm32-unknown-unknown`, run in Node under
//! wasm-bindgen-test, on the memory disk and the worker's `LocalRuntime`,
//! reached the way a page will reach it: `PimbleClient`s over `MessagePort`s
//! of a `MessageChannel`, served by the worker's entry point.
//!
//! ```sh
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!     cargo test --target wasm32-unknown-unknown -p pimble-worker --test smoke
//! ```
#![cfg(target_arch = "wasm32")]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use base64::Engine;
use pimble_client::PimbleClient;
use pimble_host::Runtime;
use pimble_rpc::{EditOperation, StoreChangeKind};
use wasm_bindgen_test::wasm_bindgen_test;
use web_sys::MessageChannel;

/// A page's client of `server`: one end of a new channel handed to the
/// worker's `connect`, the other the client's.
fn connect(server: &pimble_worker::WorkerServer) -> PimbleClient {
    let channel = MessageChannel::new().expect("a MessageChannel");
    server.connect(channel.port1()).expect("the server takes the port");
    PimbleClient::over_message_port(channel.port2())
}

/// `node`'s document's text as the server holds it.
async fn text_of(client: &PimbleClient, store: pimble_core::StoreId, node: pimble_core::NodeId) -> String {
    let node = client.get_node(store, node).await.expect("getNode");
    pimble_crdt::NodeDoc::load(&node.content).expect("a node document").text()
}

#[wasm_bindgen_test]
async fn the_server_answers_over_message_ports() {
    let rt = Runtime::default();
    let server = pimble_worker::start(pimble_server::ServerConfig::in_dir(std::path::Path::new("/smoke"))).await.expect("the server starts in wasm");
    assert!(server.server().store_manager().read().await.disk().kind() == pimble_host::DiskKind::Memory, "on the memory disk");
    let client = connect(&server);
    let watcher = connect(&server);
    assert_eq!(client.url().as_str(), pimble_client::message_port::MESSAGE_PORT_URL);

    let info = client.server_info().await.expect("serverInfo");
    assert_eq!(info.host, "worker");
    assert!(!info.hands_over);

    // A store on the memory disk, and a note in it.
    let (store_id, root) = client.create_store("/smoke/stores-here/smoke.pimble", "Smoke").await.expect("createStore");
    let note = client.create_node(store_id, Some(root), "document", "Groceries").await.expect("createNode");
    assert!(client.list_stores().await.unwrap().iter().any(|s| s.id == store_id));

    // Another connection subscribes; an edit through `applyEdit` is heard.
    let mut changes = watcher.subscribe_store_changes(store_id).await.expect("subscribeStoreChanges");
    let mut doc = pimble_crdt::NodeDoc::load(&client.get_node(store_id, note).await.unwrap().content).unwrap();
    let delta = doc.replace_plain_text("Buy pomegranates and a watering can.").unwrap();
    let changes_b64 = base64::engine::general_purpose::STANDARD.encode(delta);
    client.apply_edit(store_id, note, "smoke-test", EditOperation::IncrementalChanges { changes: changes_b64 }).await.expect("applyEdit");
    let heard = rt
        .timeout(Duration::from_secs(10), async {
            loop {
                let notification = changes.next().await.expect("the subscription is open").expect("a storeChanged");
                if matches!(notification.change_kind, StoreChangeKind::ContentUpdated { node_id } if node_id == note) {
                    break notification;
                }
            }
        })
        .await
        .expect("the edit heard within 10 s");
    assert_eq!(heard.store_id, store_id);
    assert_eq!(heard.source_client_id.as_deref(), Some("smoke-test"));

    // Read back, by both connections.
    assert_eq!(text_of(&client, store_id, note).await, "Buy pomegranates and a watering can.");
    assert_eq!(text_of(&watcher, store_id, note).await, "Buy pomegranates and a watering can.");

    // Keyword search finds it in the in-memory index: the store's index is
    // built in the background after it opens, and the content's own index
    // update is debounced 2 s, so ask until it answers (stemming:
    // "pomegranate" finds "pomegranates"; a prefix: "water").
    for query in ["pomegranate", "water"] {
        let found = rt
            .timeout(Duration::from_secs(20), async {
                loop {
                    let hits = client.search(query, vec![store_id], false, 10).await.expect("search");
                    if hits.iter().any(|h| h.node_id == note) {
                        break hits;
                    }
                    rt.sleep(Duration::from_millis(200)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("search for {query:?} found the note within 20 s"));
        assert_eq!(found[0].node_id, note, "{query}");
        assert!(found[0].snippet.contains("pomegranates"), "{query}: {:?}", found[0].snippet);
    }
    assert!(client.search("violin", vec![store_id], false, 10).await.unwrap().is_empty());

    // Unsubscribing is answered and the connections go on answering.
    changes.unsubscribe().await.expect("unsubscribe");
    assert!(watcher.list_stores().await.is_ok());

    // The store closes and opens again from the memory disk, with its text,
    // and its index is built again in memory.
    client.close_store(store_id).await.expect("closeStore");
    let opened = client.open_store("/smoke/stores-here/smoke.pimble").await.expect("openStore");
    assert_eq!(opened.id, store_id);
    assert_eq!(text_of(&client, store_id, note).await, "Buy pomegranates and a watering can.");
    rt.timeout(Duration::from_secs(20), async {
        while !client.search("pomegranate", vec![store_id], false, 10).await.unwrap().iter().any(|h| h.node_id == note) {
            rt.sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("found again after a reopen");
}

/// The worker's runtime, which nothing ran before this wave: a sleep, an
/// interval, a timeout both ways, an abort, a task awaited, a heavy job.
#[wasm_bindgen_test]
async fn the_local_runtime_runs_in_wasm() {
    let rt = Runtime::default();

    // A sleep takes about as long as asked, by the runtime's clock.
    let start = rt.now();
    rt.sleep(Duration::from_millis(50)).await;
    let slept = rt.elapsed(start);
    assert!(slept >= Duration::from_millis(45), "slept {slept:?}");
    assert!(rt.wall() > std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000), "a wall clock");

    // An interval: the first tick at once, then one a period apart.
    let mut interval = rt.interval(Duration::from_millis(20));
    let start = rt.now();
    for _ in 0..4 {
        interval.tick().await;
    }
    let ticked = rt.elapsed(start);
    assert!(ticked >= Duration::from_millis(55), "four ticks took {ticked:?}");

    // A timeout: one that elapses, one that does not.
    assert!(rt.timeout(Duration::from_millis(20), rt.sleep(Duration::from_secs(5))).await.is_err());
    assert_eq!(rt.timeout(Duration::from_secs(5), async { 7 }).await.ok(), Some(7));

    // A task that runs to its end, awaited.
    let ran = Rc::new(Cell::new(false));
    let flag = ran.clone();
    let task = rt.spawn(async move {
        flag.set(true);
    });
    task.await;
    assert!(ran.get());

    // An aborted task stops at its next await and counts as ended.
    let reached = Rc::new(Cell::new(false));
    let flag = reached.clone();
    let sleeper = rt.clone();
    let task = rt.spawn(async move {
        sleeper.sleep(Duration::from_millis(100)).await;
        flag.set(true);
    });
    rt.sleep(Duration::from_millis(10)).await;
    assert!(!task.is_finished());
    task.abort();
    rt.sleep(Duration::from_millis(200)).await;
    assert!(task.is_finished(), "an aborted task has ended");
    assert!(!reached.get(), "an aborted task never got past its await");

    // A heavy job runs (inline: a worker has no thread to give it).
    assert_eq!(rt.run_heavy(|| (1..=10u64).product::<u64>()).await.ok(), Some(3_628_800));
}
