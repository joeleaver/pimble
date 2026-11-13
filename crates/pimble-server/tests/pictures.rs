//! Pictures between plain servers (docs/IMAGES_CONTRACT.md, wave 2): a
//! replica's sync link copies the blobs its documents name, both ways,
//! after its reconcile and after every live change; a picture missing
//! everywhere never holds a document up and arrives when its bytes do; a
//! transplant takes its pictures with it. Two real `PimbleServer`s over
//! real WebSockets, as `sync.rs`.

mod common;

use std::time::Duration;

use common::{put_on_canvas, start_local_server, wait_until};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, BlobId, BlobUrl, NodeId, Place, RemoteEndpoint, StoreId};

const WAIT: Duration = Duration::from_secs(15);

/// Bytes that pass for a PNG (its signature), `len` of them, distinct per `seed`.
fn png(len: usize, seed: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend((0..len.saturating_sub(8)).map(|i| ((i * 7 + i / 251) as u8).wrapping_add(seed)));
    bytes
}

async fn holds(client: &PimbleClient, store_id: StoreId, blob: BlobId, bytes: &[u8]) -> bool {
    client.get_blob(store_id, blob).await.is_ok_and(|(_, got)| got == bytes)
}

/// B holds a store with one document; A replicates it.
struct Pair {
    _a: (pimble_server::PimbleServer, tempfile::TempDir),
    b_server: pimble_server::PimbleServer,
    _b_dir: tempfile::TempDir,
    a: PimbleClient,
    b: PimbleClient,
    store_id: StoreId,
    root: NodeId,
    doc: NodeId,
}

async fn pair() -> Pair {
    let (b_server, b, b_dir) = start_local_server().await;
    let (store_id, root) = b.create_store(b_dir.path().join("b.pimble"), "B").await.unwrap();
    let doc = b.create_node(store_id, Some(root), "document", "Album").await.unwrap();
    let (a_server, a, a_dir) = start_local_server().await;
    let remote = RemoteEndpoint { url: format!("http://{}", b_server.addr()).parse().unwrap(), auth: AuthMethod::None };
    a.add_remote_store(remote, store_id, Some(a_dir.path().join("a.pimble"))).await.expect("A replicates B's store");
    assert!(wait_until(WAIT, || async { a.get_node(store_id, doc).await.is_ok() }).await, "the document reaches A");
    Pair { _a: (a_server, a_dir), b_server, _b_dir: b_dir, a, b, store_id, root, doc }
}

#[tokio::test]
async fn a_picture_on_a_canvas_reaches_the_other_server_live_both_ways() {
    let p = pair().await;

    // On B's canvas: A fetches it.
    let small = png(30_000, 1);
    let from_b = p.b.put_blob(p.store_id, p.doc, "image/png", &small).await.unwrap();
    put_on_canvas(&p.b, p.store_id, p.doc, &from_b).await;
    assert!(wait_until(WAIT, || holds(&p.a, p.store_id, from_b.blob, &small)).await, "B's picture reaches A");

    // On A's, and large enough to go in chunks: B is sent it.
    let large = png(7 * 1024 * 1024, 2);
    let from_a = p.a.put_blob(p.store_id, p.doc, "image/png", &large).await.unwrap();
    put_on_canvas(&p.a, p.store_id, p.doc, &from_a).await;
    assert!(wait_until(WAIT, || holds(&p.b, p.store_id, from_a.blob, &large)).await, "A's picture reaches B");
}

#[tokio::test]
async fn pictures_made_on_both_sides_of_a_cut_link_cross_when_it_is_back() {
    let p = pair().await;
    p.a.set_store_sync(p.store_id, None).await.unwrap();

    let on_a = png(20_000, 3);
    let on_b = png(20_000, 4);
    let a_note = p.a.create_node(p.store_id, Some(p.root), "document", "A's").await.unwrap();
    let url_a = p.a.put_blob(p.store_id, a_note, "image/png", &on_a).await.unwrap();
    put_on_canvas(&p.a, p.store_id, a_note, &url_a).await;
    let url_b = p.b.put_blob(p.store_id, p.doc, "image/png", &on_b).await.unwrap();
    put_on_canvas(&p.b, p.store_id, p.doc, &url_b).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!holds(&p.b, p.store_id, url_a.blob, &on_a).await, "nothing crosses a cut link");

    let remote = RemoteEndpoint { url: format!("http://{}", p.b_server.addr()).parse().unwrap(), auth: AuthMethod::None };
    p.a.set_store_sync(p.store_id, Some(remote)).await.unwrap();
    assert!(wait_until(WAIT, || holds(&p.b, p.store_id, url_a.blob, &on_a)).await, "A's picture reaches B after the reconnect");
    assert!(wait_until(WAIT, || holds(&p.a, p.store_id, url_b.blob, &on_b)).await, "B's picture reaches A after the reconnect");
}

#[tokio::test]
async fn a_picture_whose_bytes_are_nowhere_yet_holds_nothing_up_and_arrives_later() {
    let p = pair().await;
    // B's document names a picture before anyone holds its bytes (an upload
    // still on its way from a third device).
    let blob = BlobId::new();
    let url = BlobUrl::new(p.store_id, blob);
    put_on_canvas(&p.b, p.store_id, p.doc, &url).await;
    let text_after = p.b.create_node(p.store_id, Some(p.root), "document", "After").await.unwrap();
    assert!(wait_until(WAIT, || async { p.a.get_node(p.store_id, text_after).await.is_ok() }).await, "documents keep flowing");
    assert!(p.a.get_blob(p.store_id, blob).await.is_err());

    // The bytes land on B under that id: A fetches them on its own.
    let image = png(15_000, 5);
    p.b.put_blob_copy(p.store_id, p.doc, blob, "image/png", &image).await.unwrap();
    assert!(wait_until(Duration::from_secs(20), || holds(&p.a, p.store_id, blob, &image)).await, "the picture arrives when its bytes do");
}

#[tokio::test]
async fn a_transplanted_node_takes_its_pictures_to_the_store_it_lands_in() {
    let (_server, client, dir) = start_local_server().await;
    let (from, from_root) = client.create_store(dir.path().join("from.pimble"), "From").await.unwrap();
    let (to, to_root) = client.create_store(dir.path().join("to.pimble"), "To").await.unwrap();
    let note = client.create_node(from, Some(from_root), "document", "Moving").await.unwrap();
    let image = png(12_000, 6);
    let url = client.put_blob(from, note, "image/png", &image).await.unwrap();
    put_on_canvas(&client, from, note, &url).await;

    let moved = client.transplant_node(from, note, to, to_root, Place::Last).await.unwrap();
    let node = client.get_node(to, moved.node_id).await.unwrap();
    let named = pimble_crdt::NodeDoc::pictures_of(&node.content);
    assert_eq!(named, vec![BlobUrl::new(to, url.blob)], "the copy names its picture in its new store");
    assert_eq!(client.get_blob(to, url.blob).await.unwrap().1, image);
}
