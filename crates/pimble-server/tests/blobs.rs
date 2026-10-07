//! Pictures (docs/IMAGES_CONTRACT.md, wave 1): `putBlob`, `getBlob` and
//! `haveBlobs` against a real `PimbleServer`, and what they leave on disk.
//!
//! What the blob store does on its own (the header, the hash, putting under
//! a given id) is tested beside it in `pimble-store/src/blobs.rs`; what a
//! node's text names (`NodeDoc::blob_refs`) in `pimble-crdt/src/blobs.rs`.

mod common;

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use common::{fresh_signing_key, make_jwt, start_local_server};
use ed25519_dalek::SigningKey;
use jsonrpsee::ws_client::{WsClient, WsClientBuilder};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, BlobId, BlobUrl, NodeId, StoreAccess, StoreId, StoreKind, IMAGE_TOO_LARGE, MAX_IMAGE_BYTES, NOT_AN_IMAGE};
use pimble_rpc::{PimbleApiClient, PutBlobRequest, BLOBS_IN_SHARES_REFUSAL, BLOB_CHUNK_BYTES, BLOB_NOT_HERE};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;

const MIB: usize = 1024 * 1024;

/// Bytes that pass for a PNG as far as a store looks (the signature), `len`
/// of them, different for every `seed`.
fn png(len: usize, seed: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend((0..len.saturating_sub(8)).map(|i| ((i * 7 + i / 251) as u8).wrapping_add(seed)));
    bytes
}

/// A store with one document, on a fresh local server.
struct Fixture {
    _server: PimbleServer,
    client: PimbleClient,
    dir: tempfile::TempDir,
    store_path: PathBuf,
    store_id: StoreId,
    node: NodeId,
}

async fn fixture() -> Fixture {
    let (server, client, dir) = start_local_server().await;
    let store_path = dir.path().join("pictures.pimble");
    let (store_id, root) = client.create_store(&store_path, "Pictures").await.unwrap();
    let node = client.create_node(store_id, Some(root), "document", "Album").await.unwrap();
    Fixture { _server: server, client, dir, store_path, store_id, node }
}

/// The names in a store's `blobs/` (empty when there is no such directory).
fn blob_files(store_path: &Path) -> Vec<String> {
    match std::fs::read_dir(store_path.join("blobs")) {
        Ok(entries) => entries.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect(),
        Err(_) => Vec::new(),
    }
}

fn refusal<T: std::fmt::Debug>(result: Result<T, pimble_client::ClientError>, what: &str) -> String {
    result.expect_err(what).to_string()
}

/// The RPCs themselves, for what `PimbleClient` hides (a chunk on its own).
async fn raw_client(server: &PimbleServer) -> WsClient {
    WsClientBuilder::default().build(format!("ws://{}", server.addr())).await.unwrap()
}

#[tokio::test]
async fn a_picture_put_is_the_picture_got() {
    let f = fixture().await;
    let image = png(40_000, 1);

    let url = f.client.put_blob(f.store_id, f.node, "image/png", &image).await.unwrap();
    assert_eq!(url.store, f.store_id);
    assert_eq!(url.to_string(), format!("pimble-blob:{}/{}", f.store_id, url.blob));
    assert_eq!(BlobUrl::parse(&url.to_string()), Some(url));

    let (mime, bytes) = f.client.get_blob(f.store_id, url.blob).await.unwrap();
    assert_eq!(mime, "image/png");
    assert_eq!(bytes, image);

    // One file, named by the id, beside the store's documents.
    assert_eq!(blob_files(&f.store_path), vec![url.blob.to_string()]);

    // The same picture again is another blob (ids are random, never a hash).
    let again = f.client.put_blob(f.store_id, f.node, "image/png", &image).await.unwrap();
    assert_ne!(again.blob, url.blob);

    let stranger = BlobId::new();
    assert_eq!(f.client.have_blobs(f.store_id, &[url.blob, stranger, again.blob]).await.unwrap(), vec![stranger]);
    assert_eq!(refusal(f.client.get_blob(f.store_id, stranger).await, "a blob nobody put"), BLOB_NOT_HERE);

    // The other accepted types, each told by its first bytes.
    for (mime, head) in [
        ("image/jpeg", &b"\xFF\xD8\xFF\xE0\0\x10JFIF"[..]),
        ("image/gif", &b"GIF89a\x01\0\x01\0"[..]),
        ("image/webp", &b"RIFF\x24\0\0\0WEBPVP8 "[..]),
        ("image/avif", &b"\0\0\0\x1cftypavif\0\0\0\0avifmif1miaf"[..]),
    ] {
        let url = f.client.put_blob(f.store_id, f.node, mime, head).await.unwrap_or_else(|e| panic!("{mime}: {e}"));
        assert_eq!(f.client.get_blob(f.store_id, url.blob).await.unwrap(), (mime.to_string(), head.to_vec()));
    }
}

#[tokio::test]
async fn a_19_mib_picture_goes_through_in_chunks() {
    let f = fixture().await;
    let image = png(19 * MIB, 2);
    assert!(image.len() > 6 * BLOB_CHUNK_BYTES, "more chunks than one message could carry");

    let url = f.client.put_blob(f.store_id, f.node, "image/png", &image).await.unwrap();
    let (mime, bytes) = f.client.get_blob(f.store_id, url.blob).await.unwrap();
    assert_eq!(mime, "image/png");
    assert!(bytes == image, "the bytes came back as they went, all {} of them", image.len());
    assert_eq!(blob_files(&f.store_path), vec![url.blob.to_string()], "the upload's part file is gone");

    // Exactly the limit is a picture too.
    let limit = png(MAX_IMAGE_BYTES as usize, 3);
    let url = f.client.put_blob(f.store_id, f.node, "image/png", &limit).await.unwrap();
    assert!(f.client.get_blob(f.store_id, url.blob).await.unwrap().1 == limit);
}

#[tokio::test]
async fn a_picture_over_20_mib_is_refused_with_the_sentence() {
    let f = fixture().await;
    let image = png(MAX_IMAGE_BYTES as usize + 1, 4);
    let err = refusal(f.client.put_blob(f.store_id, f.node, "image/png", &image).await, "one byte over");
    assert_eq!(err, "Pictures can be at most 20 MiB.");
    assert_eq!(err, IMAGE_TOO_LARGE);
    assert!(blob_files(&f.store_path).is_empty(), "{:?}", blob_files(&f.store_path));

    // Declaring less than is sent does not get more in: a chunk may not run
    // past the length the upload was started with.
    let raw = raw_client(&f._server).await;
    let chunk = |offset: usize, blob_id| PutBlobRequest {
        store_id: f.store_id,
        node_id: f.node,
        mime: "image/png".into(),
        bytes: STANDARD.encode(&image[offset..offset + MIB]),
        offset: offset as u64,
        total: Some(2 * MIB as u64 - 1),
        blob_id,
    };
    let first = raw.put_blob(chunk(0, None)).await.unwrap();
    assert!(!first.complete);
    let err = raw.put_blob(chunk(MIB, Some(first.url.blob))).await.unwrap_err().to_string();
    assert!(err.contains("runs past the length"), "{err}");
}

#[tokio::test]
async fn svg_and_bytes_that_are_not_their_type_are_refused() {
    let f = fixture().await;
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";

    for (mime, bytes, what) in [
        ("image/svg+xml", &svg[..], "SVG is never kept"),
        ("image/png", &svg[..], "an SVG that says it is a PNG"),
        ("image/jpeg", &png(500, 5)[..], "a PNG that says it is a JPEG"),
        ("text/html", &b"<html></html>"[..], "not an image at all"),
        ("image/png", &b"\x89PNG but not really"[..], "a wrong magic number"),
    ] {
        assert_eq!(refusal(f.client.put_blob(f.store_id, f.node, mime, bytes).await, what), NOT_AN_IMAGE, "{what}");
    }
    assert_eq!(refusal(f.client.put_blob(f.store_id, f.node, "image/png", b"").await, "nothing"), "A picture cannot be empty.");

    // In chunks too: the wrong bytes are refused, and leave nothing.
    let not_png = vec![b'<'; BLOB_CHUNK_BYTES + 1000];
    assert_eq!(refusal(f.client.put_blob(f.store_id, f.node, "image/png", &not_png).await, "chunked"), NOT_AN_IMAGE);
    assert!(blob_files(&f.store_path).is_empty(), "{:?}", blob_files(&f.store_path));
}

#[tokio::test]
async fn a_corrupted_file_on_disk_reads_as_absent() {
    let f = fixture().await;
    let image = png(30_000, 6);
    let url = f.client.put_blob(f.store_id, f.node, "image/png", &image).await.unwrap();
    let path = f.store_path.join("blobs").join(url.blob.to_string());

    // One byte of the image changed: the header's hash no longer matches.
    let mut file = std::fs::read(&path).unwrap();
    assert_eq!(&file[..4], b"PIMG");
    let at = file.len() - 100;
    file[at] ^= 0xFF;
    std::fs::write(&path, &file).unwrap();
    assert_eq!(refusal(f.client.get_blob(f.store_id, url.blob).await, "a blob that fails its hash"), BLOB_NOT_HERE);

    // Cut short: missing by its length alone, without reading it.
    std::fs::write(&path, &file[..file.len() / 2]).unwrap();
    assert_eq!(f.client.have_blobs(f.store_id, &[url.blob]).await.unwrap(), vec![url.blob]);
    assert_eq!(refusal(f.client.get_blob(f.store_id, url.blob).await, "a truncated blob"), BLOB_NOT_HERE);
}

#[tokio::test]
async fn an_abandoned_chunked_upload_leaves_nothing() {
    let f = fixture().await;
    let kept = f.client.put_blob(f.store_id, f.node, "image/png", &png(10_000, 7)).await.unwrap();

    // The first chunk of a picture whose client then goes away.
    let image = png(5 * MIB, 8);
    let raw = raw_client(&f._server).await;
    let first = raw
        .put_blob(PutBlobRequest {
            store_id: f.store_id,
            node_id: f.node,
            mime: "image/png".into(),
            bytes: STANDARD.encode(&image[..BLOB_CHUNK_BYTES]),
            offset: 0,
            total: Some(image.len() as u64),
            blob_id: None,
        })
        .await
        .unwrap();
    assert_eq!((first.received, first.complete), (BLOB_CHUNK_BYTES as u64, false));
    drop(raw);

    // Nothing answers to its id while it is unfinished...
    let id = first.url.blob;
    assert_eq!(refusal(f.client.get_blob(f.store_id, id).await, "half a picture"), BLOB_NOT_HERE);
    assert_eq!(f.client.have_blobs(f.store_id, &[id, kept.blob]).await.unwrap(), vec![id]);
    assert!(!blob_files(&f.store_path).contains(&id.to_string()), "no file is named by the id until the blob is whole");

    // ...and nothing of it is left once the store has been opened again.
    f.client.close_store(f.store_id).await.unwrap();
    f.client.open_store(&f.store_path).await.unwrap();
    assert_eq!(blob_files(&f.store_path), vec![kept.blob.to_string()]);
    assert_eq!(refusal(f.client.get_blob(f.store_id, id).await, "after reopening"), BLOB_NOT_HERE);
}

#[tokio::test]
async fn a_blob_survives_closing_and_reopening_the_store_and_the_server() {
    let f = fixture().await;
    let small = png(20_000, 9);
    let large = png(4 * MIB, 10);
    let a = f.client.put_blob(f.store_id, f.node, "image/png", &small).await.unwrap();
    let b = f.client.put_blob(f.store_id, f.node, "image/png", &large).await.unwrap();

    f.client.close_store(f.store_id).await.unwrap();
    assert!(f.client.get_blob(f.store_id, a.blob).await.is_err(), "a closed store answers nothing");
    f.client.open_store(&f.store_path).await.unwrap();
    assert_eq!(f.client.get_blob(f.store_id, a.blob).await.unwrap().1, small);
    assert!(f.client.get_blob(f.store_id, b.blob).await.unwrap().1 == large);

    // Another server over the same files: a restart.
    let Fixture { _server: mut server, client, dir, store_path, store_id, .. } = f;
    drop(client);
    server.stop().await.unwrap();
    let (_server, client) = common::start_device_in(dir.path()).await;
    client.open_store(&store_path).await.unwrap();
    assert_eq!(client.get_blob(store_id, a.blob).await.unwrap(), ("image/png".to_string(), small));
    assert!(client.have_blobs(store_id, &[a.blob, b.blob]).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_picture_put_for_a_mount_is_kept_in_the_store_the_mount_shows() {
    let f = fixture().await;
    let (source, source_root) = f.client.create_store(f.dir.path().join("source.pimble"), "Source").await.unwrap();
    let chapter = f.client.create_node(source, Some(source_root), "document", "Chapter").await.unwrap();
    let (mount, _) = f.client.create_mount(f.store_id, f.node, source, chapter, None).await.unwrap();

    let image = png(9000, 11);
    let url = f.client.put_blob(f.store_id, mount, "image/png", &image).await.unwrap();
    assert_eq!(url.store, source, "the URL names the canonical store");
    assert_eq!(f.client.get_blob(source, url.blob).await.unwrap().1, image);
    assert_eq!(f.client.have_blobs(f.store_id, &[url.blob]).await.unwrap(), vec![url.blob], "and the mounting store holds nothing");

    // A node that is not there has no text to put a picture in.
    assert!(f.client.put_blob(f.store_id, NodeId::new(), "image/png", &image).await.is_err());
}

#[tokio::test]
async fn a_vault_store_answers_as_it_does_to_every_plain_rpc() {
    let (_server, client, dir) = start_local_server().await;
    let (vault, root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();
    let raw = raw_client(&_server).await;

    let put = raw
        .put_blob(PutBlobRequest { store_id: vault, node_id: root, mime: "image/png".into(), bytes: STANDARD.encode(png(100, 0)), offset: 0, total: None, blob_id: None })
        .await
        .unwrap_err();
    let get = raw.get_blob(pimble_rpc::GetBlobRequest { store_id: vault, blob_id: BlobId::new(), offset: 0 }).await.unwrap_err();
    let have = raw.have_blobs(pimble_rpc::HaveBlobsRequest { store_id: vault, ids: vec![BlobId::new()] }).await.unwrap_err();
    for err in [put, get, have] {
        let jsonrpsee::core::client::Error::Call(object) = err else { panic!("expected an RPC error, got {err:?}") };
        assert_eq!(object.code(), -32005, "{object:?}");
    }
}

// ── Who may put and who may get (a server in JWT mode) ──────────────────

async fn spawn_jwks(signing_key: &SigningKey, kid: &str) -> String {
    let x = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
    let jwks = json!({ "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": kid, "x": x } ] });
    let app = axum::Router::new().route(
        "/jwks.json",
        axum::routing::get(move || {
            let jwks = jwks.clone();
            async move { axum::Json(jwks) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{}/jwks.json", addr)
}

const ISSUER: &str = "https://issuer.example/v1";

/// A hosted plain store with a shared folder and a document outside it, and
/// the service client that set it up.
struct Hosted {
    _server: PimbleServer,
    _dir: tempfile::TempDir,
    url: String,
    sk: SigningKey,
    admin: PimbleClient,
    store_id: StoreId,
    shared: NodeId,
    inside: NodeId,
    outside: NodeId,
}

impl Hosted {
    async fn start() -> Self {
        let sk = fresh_signing_key();
        let jwks_url = spawn_jwks(&sk, "kid-1").await;
        let mut server = PimbleServer::with_config(ServerConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            auth_token: Some("admin-secret".into()),
            jwks_url: Some(jwks_url.parse().unwrap()),
            jwt_issuer: Some(ISSUER.into()),
            ..Default::default()
        });
        server.start().await.unwrap();
        let url = format!("http://{}", server.addr());
        let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (store_id, root) = admin.create_store(dir.path().join("s.pimble"), "S").await.unwrap();
        let shared = admin.create_node(store_id, Some(root), "folder", "Shared").await.unwrap();
        let inside = admin.create_node(store_id, Some(shared), "document", "Inside").await.unwrap();
        let outside = admin.create_node(store_id, Some(root), "document", "Outside").await.unwrap();
        Self { _server: server, _dir: dir, url, sk, admin, store_id, shared, inside, outside }
    }

    /// A client whose token carries `claim` for the store: a role's name for
    /// the whole store, or `{ "roots": { <node>: <role> } }` for a share.
    async fn user(&self, sub: &str, claim: serde_json::Value) -> PimbleClient {
        let mut stores = serde_json::Map::new();
        stores.insert(self.store_id.to_string(), claim);
        let jwt = make_jwt(&self.sk, "kid-1", ISSUER, sub, &format!("{sub}@example.com"), &stores);
        PimbleClient::connect_with_auth(&self.url, &AuthMethod::Bearer { token: jwt }).await.expect("a JWT connects")
    }
}

#[tokio::test]
async fn a_reader_may_get_a_picture_and_not_put_one() {
    let h = Hosted::start().await;
    let image = png(12_000, 12);
    let url = h.admin.put_blob(h.store_id, h.inside, "image/png", &image).await.unwrap();

    let reader = h.user("rita", json!("reader")).await;
    assert_eq!(reader.get_blob(h.store_id, url.blob).await.unwrap(), ("image/png".to_string(), image.clone()));
    assert!(reader.have_blobs(h.store_id, &[url.blob]).await.unwrap().is_empty());
    assert_eq!(
        refusal(reader.put_blob(h.store_id, h.inside, "image/png", &image).await, "a reader's put"),
        StoreAccess::READ_ONLY_REFUSAL,
        "the sentence every reader refusal carries, as the whole message"
    );
    // In chunks as well: every chunk is judged.
    assert_eq!(refusal(reader.put_blob(h.store_id, h.inside, "image/png", &png(4 * MIB, 13)).await, "a reader's chunks"), StoreAccess::READ_ONLY_REFUSAL);

    let editor = h.user("ed", json!("editor")).await;
    let theirs = editor.put_blob(h.store_id, h.outside, "image/png", &image).await.unwrap();
    assert_eq!(reader.get_blob(h.store_id, theirs.blob).await.unwrap().1, image);

    // Someone with no grant on the store learns nothing and adds nothing.
    let mut elsewhere = serde_json::Map::new();
    elsewhere.insert(StoreId::new().to_string(), json!("owner"));
    let jwt = make_jwt(&h.sk, "kid-1", ISSUER, "sam", "sam@example.com", &elsewhere);
    let stranger = PimbleClient::connect_with_auth(&h.url, &AuthMethod::Bearer { token: jwt }).await.unwrap();
    for err in [
        refusal(stranger.get_blob(h.store_id, url.blob).await, "no grant: get"),
        refusal(stranger.have_blobs(h.store_id, &[url.blob]).await, "no grant: have"),
        refusal(stranger.put_blob(h.store_id, h.inside, "image/png", &image).await, "no grant: put"),
    ] {
        assert!(err.contains("no grant for store"), "{err}");
    }
}

#[tokio::test]
async fn a_shares_member_puts_where_they_may_write_and_cannot_get_yet() {
    let h = Hosted::start().await;
    let image = png(8000, 14);
    let owners = h.admin.put_blob(h.store_id, h.inside, "image/png", &image).await.unwrap();
    let roots = |role: &str| json!({ "roots": { h.shared.to_string(): role } });

    // An editor of the share: judged as `applyEdit` on the node would be.
    let member = h.user("mia", roots("editor")).await;
    let theirs = member.put_blob(h.store_id, h.inside, "image/png", &image).await.expect("a node in the share");
    assert_eq!(h.admin.get_blob(h.store_id, theirs.blob).await.unwrap().1, image);
    let err = refusal(member.put_blob(h.store_id, h.outside, "image/png", &image).await, "a node outside the share");
    assert!(err.contains("no grant for this document"), "{err}");

    // A reader of the share may not put at all.
    let reader = h.user("ron", roots("reader")).await;
    assert_eq!(refusal(reader.put_blob(h.store_id, h.inside, "image/png", &image).await, "a share's reader"), StoreAccess::READ_ONLY_REFUSAL);

    // Reading in a share waits for wave 2 (a blob judged by the nodes that
    // name it); until then a member is told so, whichever blob they ask for.
    for who in [&member, &reader] {
        for blob in [owners.blob, theirs.blob, BlobId::new()] {
            let err = refusal(who.get_blob(h.store_id, blob).await, "a member's get");
            assert_eq!(err, BLOBS_IN_SHARES_REFUSAL);
        }
        assert_eq!(refusal(who.have_blobs(h.store_id, &[owners.blob]).await, "a member's have"), BLOBS_IN_SHARES_REFUSAL);
    }
}

/// A `haveBlobs` or `getBlob` naming something that is not a blob id never
/// reaches the store: the id is a file name, and only an id parses as one.
#[tokio::test]
async fn a_blob_id_that_is_not_one_never_reaches_the_disk() {
    let f = fixture().await;
    let raw = raw_client(&f._server).await;
    use jsonrpsee::core::client::ClientT;
    for bad in ["../manifest.json", "..", "AAAAAAAAAAAAAAAAAAAAAAAAAA", "aaaaaaaaaaaaaaaaaaaaaaaaa", "aaaaaaaaaaaaa/aaaaaaaaaaaa"] {
        let params = jsonrpsee::rpc_params![json!({ "store_id": f.store_id, "blob_id": bad })];
        let answer: Result<serde_json::Value, _> = raw.request("pimble_getBlob", params).await;
        let err = answer.expect_err(bad).to_string();
        assert!(err.to_lowercase().contains("invalid params") || err.contains("blob id"), "{bad}: {err}");
    }
}
