//! One large canvas must not lock its store (docs/DRAWING_CONTRACT.md
//! "Large canvases"): a paint layer of 25,000 single-point dabs made a
//! canvas document of about 15 MB, and every fresh client's `syncNodes` of
//! its store failed with jsonrpsee's "Response is too big" (a 10 MB
//! default on both ends). The server and the native client now take
//! messages up to `MAX_MESSAGE_BYTES`, and `PimbleClient::sync_nodes` asks
//! for answers of at most `SYNC_ANSWER_BYTES`, so a large document travels
//! alone (`max_bytes`, `deferred`).

mod isolated;

use std::sync::Arc;

use base64::Engine;
use pimble_board::{brush, Board, NewStroke, Stamp};
use pimble_core::{node_types, NodeId};
use pimble_crdt::NodeDoc;
use pimble_rpc::{CreateNodeRequest, CreateStoreRequest, EditOperation, NodeStateVector, PimbleApiServer, SyncNodesRequest, MAX_MESSAGE_BYTES, SYNC_ANSWER_BYTES};
use pimble_server::RpcHandler;
use isolated::TempServer;
use pimble_store::StoreManager;
use tokio::sync::RwLock;

/// A canvas document like the one that locked the store: a paint layer of
/// `dabs` single-point strokes spread over a county-sized area. Its bytes.
fn dabbed_canvas(dabs: usize) -> Vec<u8> {
    let board = Board::standalone();
    let stamp = Stamp::new(None, "2026-11-18T10:00:00.000Z");
    let (_, first) = board.edit(stamp, |t| { t.create()?; t.paint_layer_for(None) }).into_result().unwrap();
    let mut doc = NodeDoc::new();
    doc.apply_update(&first.unwrap()).unwrap();
    let layer = board.snapshot().layers.iter().find(|l| l.kind == pimble_board::LayerKind::Paint).unwrap().id.clone();
    let mut seed = 7u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as f32 / (1u64 << 31) as f32
    };
    for _ in 0..(dabs / 500) {
        let (_, update) = board
            .edit(stamp, |t| {
                for _ in 0..500 {
                    let (x, y) = (next() * 11800.0, next() * 11000.0);
                    t.add_stroke(NewStroke::new(&layer, &brush::ROUND, pimble_board::Color::rgb(40, 120, 40)).with_size(20.0).with_points(vec![[x, y, 1.0, 0.0]]))?;
                }
                Ok(())
            })
            .into_result()
            .unwrap();
        doc.apply_update(&update.unwrap()).unwrap();
    }
    doc.save()
}

async fn start_server() -> (TempServer, pimble_client::PimbleClient) {
    let mut server = TempServer::isolated();
    server.start().await.expect("server starts");
    let client = pimble_client::PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

/// Through a real server and client: a canvas document past 10 MB (its
/// answer base64, about 20 MB) is written with `applyEdit` and read back
/// whole by a fresh client's `syncNodes`, with the store's other documents
/// beside it, which the server answers apart from the large one.
#[tokio::test]
async fn a_canvas_past_ten_megabytes_loads_for_a_fresh_client() {
    let (server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, root_id) = client.create_store(dir.path().join("big.pimble"), "Big").await.unwrap();
    let small = client.create_node(store_id, Some(root_id), "document", "Small").await.unwrap();
    let canvas = client.create_node(store_id, Some(root_id), node_types::BOARD, "County").await.unwrap();
    let bytes = dabbed_canvas(25_000);
    assert!(bytes.len() > 10 << 20, "the canvas is past jsonrpsee's default: {} bytes", bytes.len());
    assert!(bytes.len() < MAX_MESSAGE_BYTES as usize / 2);
    let changes = base64::engine::general_purpose::STANDARD.encode(&bytes);
    client.apply_edit(store_id, canvas, "painter", EditOperation::IncrementalChanges { changes }).await.expect("a large edit is taken");

    // A fresh client, as a new pimble-mcp is.
    let fresh = pimble_client::PimbleClient::connect(format!("http://{}", server.addr())).await.unwrap();
    let (none, unknown) = fresh.sync_nodes(store_id, &[], true).await.expect("the listing");
    assert!(none.is_empty());
    assert!(unknown.contains(&canvas) && unknown.contains(&small));
    let empty = pimble_crdt::empty_state_vector();
    let wanted: Vec<(NodeId, Vec<u8>)> = unknown.iter().map(|id| (*id, empty.clone())).collect();
    let (answers, _) = fresh.sync_nodes(store_id, &wanted, false).await.expect("the store loads");
    assert_eq!(answers.len(), wanted.len(), "every document is answered");
    let (_, diff, _) = answers.iter().find(|(id, _, _)| *id == canvas).unwrap();
    let doc = NodeDoc::load(diff).unwrap();
    let held = Board::of_node(&doc).snapshot();
    let layer = held.layers.iter().find(|l| l.kind == pimble_board::LayerKind::Paint).unwrap();
    assert_eq!(held.paint_ops(&layer.id).len(), 25_000, "the whole canvas arrived");
    // And the node itself reads.
    assert!(fresh.get_node(store_id, canvas).await.is_ok());
}

/// `max_bytes` keeps an answer small: the documents that would take it past
/// the limit are listed as deferred (always answering one), and asking
/// again for them, with nothing kept on the server between the calls,
/// answers the rest. Without `max_bytes` everything is answered at once.
#[tokio::test]
async fn sync_nodes_defers_what_passes_max_bytes() {
    let manager = Arc::new(RwLock::new(StoreManager::new()));
    let dir = tempfile::tempdir().unwrap();
    let handler = RpcHandler::in_dir(manager, dir.path());
    let ext = pimble_server::service_extensions();
    let made = handler
        .create_store(&ext, CreateStoreRequest { kind: Default::default(), store_id: None, path: dir.path().join("s.pimble"), name: "S".into() })
        .await
        .unwrap();
    let store_id = made.store_id;
    let mut ids = Vec::new();
    for i in 0..5 {
        let text = format!("Note {i}: {}", "words ".repeat(400));
        let content = base64::engine::general_purpose::STANDARD.encode(NodeDoc::from_plain_text(&text).unwrap().save());
        let id = handler
            .create_node(&ext, CreateNodeRequest {
                store_id,
                parent_id: Some(made.root_node_id),
                node_type: "document".into(),
                title: format!("Note {i}"),
                content: Some(content),
                custom: Default::default(),
                place: Default::default(),
            })
            .await
            .unwrap()
            .node_id;
        ids.push(id);
    }
    let b64 = |v: &[u8]| base64::engine::general_purpose::STANDARD.encode(v);
    let empty = b64(&pimble_crdt::empty_state_vector());
    let ask = |nodes: &[NodeId], max_bytes: Option<u64>| SyncNodesRequest {
        store_id,
        nodes: nodes.iter().map(|id| NodeStateVector { node_id: *id, state_vector: empty.clone() }).collect(),
        list_unknown: false,
        max_bytes,
    };
    let all = handler.sync_nodes(&ext, ask(&ids, None)).await.unwrap();
    assert_eq!(all.nodes.len(), 5);
    assert!(all.deferred.is_empty(), "nothing is deferred unless asked");
    let one = all.nodes[0].diff.len() as u64;

    // Room for about two: those two, the rest deferred in order.
    let first = handler.sync_nodes(&ext, ask(&ids, Some(one * 2 + one / 2))).await.unwrap();
    assert_eq!(first.nodes.len(), 2);
    assert_eq!(first.deferred, ids[2..].to_vec());
    // Room for none: still one, so asking again always gets somewhere.
    let tiny = handler.sync_nodes(&ext, ask(&ids, Some(1))).await.unwrap();
    assert_eq!(tiny.nodes.len(), 1);
    assert_eq!(tiny.deferred.len(), 4);
    // The deferred asked for again: the rest.
    let rest = handler.sync_nodes(&ext, ask(&first.deferred, Some(u64::MAX))).await.unwrap();
    let got: std::collections::HashSet<NodeId> = first.nodes.iter().chain(&rest.nodes).map(|n| n.node_id).collect();
    assert_eq!(first.nodes.len() + rest.nodes.len(), ids.len());
    assert_eq!(got, ids.iter().copied().collect());
    assert!(SYNC_ANSWER_BYTES < MAX_MESSAGE_BYTES as u64);
}
