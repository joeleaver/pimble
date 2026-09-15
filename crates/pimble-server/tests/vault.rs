//! End-to-end tests for the vault (encrypted store) API
//! (docs/CRYPTO_CONTRACT.md), against a real `PimbleServer` over real
//! WebSocket connections — the same style `tests/sync.rs` and `tests/auth.rs`
//! use. Encryption itself is `pimble-crypto` (agent K's crate) and happens
//! entirely on the client side of the real system; the server only stores
//! and relays opaque blobs, so plain byte strings stand in for ciphertext
//! throughout these tests.

use std::collections::HashMap;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreId, StoreKind};
use pimble_rpc::{StoreChangeKind, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;

/// Start a `PimbleServer` bound to an OS-assigned loopback port and return
/// it plus a `PimbleClient` already connected to it (mirrors `tests/sync.rs`).
async fn start_server() -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig { addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() });
    server.start().await.expect("server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

/// Base64url (no padding) of `bytes`, the wire encoding every vault blob
/// travels as.
fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

// ── JWT test support (mirrors `tests/auth.rs`'s own helpers, duplicated
// here rather than shared: each integration test file is its own crate) ──

fn signing_key() -> SigningKey {
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

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

fn make_jwt(
    signing_key: &SigningKey,
    kid: &str,
    issuer: &str,
    sub: &str,
    email: &str,
    stores: &HashMap<StoreId, &str>,
    exp_offset_secs: i64,
) -> String {
    let header = json!({ "alg": "EdDSA", "kid": kid });
    let stores_claim: serde_json::Map<String, serde_json::Value> =
        stores.iter().map(|(id, role)| (id.to_string(), json!(role))).collect();
    let payload = json!({
        "iss": issuer,
        "sub": sub,
        "aud": "pimble",
        "exp": chrono::Utc::now().timestamp() + exp_offset_secs,
        "claims": { "email": email, "stores": stores_claim },
    });

    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature = signing_key.sign(signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());
    format!("{signing_input}.{sig_b64}")
}

async fn start_jwt_server(auth_token: &str, jwks_url: &str, issuer: &str) -> PimbleServer {
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        auth_token: Some(auth_token.to_string()),
        jwks_url: Some(jwks_url.parse().unwrap()),
        jwt_issuer: Some(issuer.to_string()),
        ..Default::default()
    });
    server.start().await.expect("server starts");
    server
}

// ── 1. Append and fetch round trip ───────────────────────────────────

#[tokio::test]
async fn append_and_fetch_round_trip() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();

    let seq1 = client.vault_append(store_id, VaultDocId::Tree, b64(b"one")).await.unwrap();
    let seq2 = client.vault_append(store_id, VaultDocId::Tree, b64(b"two")).await.unwrap();
    assert_eq!((seq1, seq2), (1, 2));

    let fetched = client.vault_fetch(store_id, VaultDocId::Tree, 0).await.unwrap();
    assert!(fetched.snapshot.is_none());
    assert_eq!(fetched.updates.len(), 2);
    assert_eq!(fetched.updates[0].seq, 1);
    assert_eq!(URL_SAFE_NO_PAD.decode(&fetched.updates[0].blob).unwrap(), b"one");
    assert_eq!(fetched.updates[1].seq, 2);
    assert_eq!(URL_SAFE_NO_PAD.decode(&fetched.updates[1].blob).unwrap(), b"two");
    assert_eq!(fetched.head, 2);

    let partial = client.vault_fetch(store_id, VaultDocId::Tree, 1).await.unwrap();
    assert_eq!(partial.updates.iter().map(|u| u.seq).collect::<Vec<_>>(), vec![2]);
    assert_eq!(partial.head, 2);
}

// ── 2. Fetch semantics around a snapshot ─────────────────────────────

#[tokio::test]
async fn fetch_semantics_around_a_snapshot() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();

    client.vault_append(store_id, VaultDocId::Tree, b64(b"a")).await.unwrap(); // seq 1
    client.vault_append(store_id, VaultDocId::Tree, b64(b"b")).await.unwrap(); // seq 2
    client.vault_append(store_id, VaultDocId::Tree, b64(b"c")).await.unwrap(); // seq 3
    client.vault_snapshot(store_id, VaultDocId::Tree, 2, b64(b"snap-of-a-b")).await.unwrap();

    // after_seq = 0: the snapshot (seq 2) is newer, so it's returned; updates
    // after max(0, 2) = 2 is just seq 3.
    let from_zero = client.vault_fetch(store_id, VaultDocId::Tree, 0).await.unwrap();
    assert_eq!(from_zero.snapshot.as_ref().map(|s| s.seq), Some(2));
    assert_eq!(from_zero.updates.iter().map(|u| u.seq).collect::<Vec<_>>(), vec![3]);
    assert_eq!(from_zero.head, 3);

    // after_seq = 2: the snapshot's seq is not *greater* than after_seq, so
    // it's omitted; updates after max(2, 2) = 2 is still just seq 3.
    let from_snapshot_seq = client.vault_fetch(store_id, VaultDocId::Tree, 2).await.unwrap();
    assert!(from_snapshot_seq.snapshot.is_none());
    assert_eq!(from_snapshot_seq.updates.iter().map(|u| u.seq).collect::<Vec<_>>(), vec![3]);

    // after_seq = 3 (the head): nothing left at all.
    let from_head = client.vault_fetch(store_id, VaultDocId::Tree, 3).await.unwrap();
    assert!(from_head.snapshot.is_none());
    assert!(from_head.updates.is_empty());
    assert_eq!(from_head.head, 3);
}

// ── 3. Snapshot drops old entries and survives close/reopen ─────────

#[tokio::test]
async fn snapshot_drops_old_entries_and_survives_close_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.pimble");

    let (mut server, client) = start_server().await;
    let (store_id, _root) = client.create_store_with(&path, "V", StoreKind::Vault, None).await.unwrap();

    client.vault_append(store_id, VaultDocId::Tree, b64(b"a")).await.unwrap();
    client.vault_append(store_id, VaultDocId::Tree, b64(b"b")).await.unwrap();
    client.vault_snapshot(store_id, VaultDocId::Tree, 2, b64(b"snap-a-b")).await.unwrap();

    client.close_store(store_id).await.unwrap();
    let log_path = path.join("vault").join("tree").join("log");
    let log_bytes = std::fs::read(&log_path).unwrap_or_default();
    assert!(log_bytes.is_empty(), "every entry is covered by the snapshot, so the log should hold nothing on disk");

    client.open_store(&path).await.unwrap();
    let fetched = client.vault_fetch(store_id, VaultDocId::Tree, 0).await.unwrap();
    assert_eq!(fetched.snapshot.as_ref().map(|s| s.seq), Some(2));
    assert!(fetched.updates.is_empty());
    assert_eq!(fetched.head, 2);

    server.stop().await.ok();
}

// ── 4. Snapshot required once the log is at its limit ────────────────

#[tokio::test]
async fn snapshot_required_once_the_log_hits_its_size_limit() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();

    // 4 MiB is the max single blob; 17 of them is comfortably past the 64
    // MiB log limit, so this must eventually refuse without hardcoding
    // exactly which append trips it (that depends on per-entry overhead).
    let chunk = vec![7u8; 4 * 1024 * 1024];
    let blob = b64(&chunk);
    let mut refused = false;
    for _ in 0..20 {
        match client.vault_append(store_id, VaultDocId::Tree, blob.clone()).await {
            Ok(_) => continue,
            Err(e) => {
                assert!(
                    e.to_string().to_lowercase().contains("snapshot"),
                    "expected a snapshot_required error, got: {}", e
                );
                refused = true;
                break;
            }
        }
    }
    assert!(refused, "appending enough 4 MiB blobs must eventually hit the 64 MiB log limit");
}

// ── 5. Blob size limit ────────────────────────────────────────────────

#[tokio::test]
async fn append_refuses_a_blob_over_4_mib() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();

    let oversized = vec![0u8; 4 * 1024 * 1024 + 1];
    let err = client
        .vault_append(store_id, VaultDocId::Tree, b64(&oversized))
        .await
        .expect_err("a blob over 4 MiB must be refused");
    assert!(err.to_string().to_lowercase().contains("4 mib") || err.to_string().to_lowercase().contains("limit"), "got: {}", err);
}

// ── 6. Plain RPCs answer encrypted_store on a vault store ─────────────

#[tokio::test]
async fn plain_rpcs_answer_encrypted_store_on_a_vault_store() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();
    let node_id = NodeId::new();

    let err = client.get_node(store_id, node_id).await.expect_err("getNode on a vault store must be refused");
    assert!(err.to_string().contains("encrypted"), "got: {}", err);

    let err = client.get_children(store_id, node_id).await.expect_err("getChildren on a vault store must be refused");
    assert!(err.to_string().contains("encrypted"), "got: {}", err);

    let err = client
        .create_node(store_id, None, "document", "x")
        .await
        .expect_err("createNode on a vault store must be refused");
    assert!(err.to_string().contains("encrypted"), "got: {}", err);

    let err = client.rebuild_index(store_id).await.expect_err("rebuildIndex on a vault store must be refused");
    assert!(err.to_string().contains("encrypted"), "got: {}", err);

    let sub_result = client.subscribe_node_changes(store_id, node_id).await;
    assert!(sub_result.is_err(), "subscribeNodeChanges on a vault store must be refused");

    // `subscribeStoreChanges` is the one exception: it's how a live client
    // hears `VaultAppended` without re-fetching, so it must keep working.
    client.subscribe_store_changes(store_id).await.expect("subscribeStoreChanges still works on a vault store");

    // The four vault RPCs, meanwhile, work fine on the very same store.
    client.vault_list_docs(store_id).await.expect("vaultListDocs works on a vault store");
}

// ── 7. createStore with a chosen id, and refusal when already open ───

#[tokio::test]
async fn create_store_with_a_chosen_id_and_refusal_when_already_open() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let id = StoreId::new();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, Some(id)).await.unwrap();
    assert_eq!(store_id, id);

    let dir2 = tempfile::tempdir().unwrap();
    let second_path = dir2.path().join("v2.pimble");
    let err = client
        .create_store_with(&second_path, "V2", StoreKind::Vault, Some(id))
        .await
        .expect_err("an id already open must be refused");
    assert!(err.to_string().to_lowercase().contains("already open"), "got: {}", err);
    assert!(!second_path.exists(), "no second store directory should have been created");
}

// ── 8. Subscription delivers VaultAppended with the blob ─────────────

#[tokio::test]
async fn subscription_delivers_vault_appended_with_the_blob() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();

    let mut sub = client.subscribe_store_changes(store_id).await.unwrap();

    let blob = b64(b"hello-vault");
    let seq = client.vault_append(store_id, VaultDocId::Tree, blob.clone()).await.unwrap();

    let notif = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("a notification should arrive within 2s")
        .expect("the subscription stream should not end")
        .expect("the notification should deserialize");

    match notif.change_kind {
        StoreChangeKind::VaultAppended { doc_id, seq: notified_seq } => {
            assert_eq!(doc_id, VaultDocId::Tree);
            assert_eq!(notified_seq, seq);
        }
        other => panic!("expected VaultAppended, got {:?}", other),
    }
    assert_eq!(notif.update.as_deref(), Some(blob.as_str()), "the blob should ride the notification so a live subscriber never re-fetches");
}

// ── 9. A truncated trailing log record is dropped when the store reopens ──

#[tokio::test]
async fn a_truncated_trailing_log_record_is_dropped_when_the_store_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.pimble");

    let store_id = {
        let (mut server, client) = start_server().await;
        let (store_id, _root) = client.create_store_with(&path, "V", StoreKind::Vault, None).await.unwrap();
        client.vault_append(store_id, VaultDocId::Tree, b64(b"good")).await.unwrap();
        server.stop().await.unwrap();
        store_id
    };

    // Simulate a crash mid-append directly on disk: a record whose declared
    // length claims far more body bytes than are actually present.
    let log_path = path.join("vault").join("tree").join("log");
    let mut bytes = std::fs::read(&log_path).unwrap();
    bytes.extend_from_slice(&2u64.to_le_bytes());
    bytes.extend_from_slice(&100u32.to_le_bytes());
    bytes.extend_from_slice(b"short");
    std::fs::write(&log_path, &bytes).unwrap();

    let (mut server, client) = start_server().await;
    client.open_store(&path).await.expect("reopening the vault store succeeds despite the truncated tail");
    let fetched = client.vault_fetch(store_id, VaultDocId::Tree, 0).await.unwrap();
    assert_eq!(fetched.updates.len(), 1, "the truncated record must be dropped, not the good one before it");
    assert_eq!(URL_SAFE_NO_PAD.decode(&fetched.updates[0].blob).unwrap(), b"good");
    assert_eq!(fetched.head, 1);

    server.stop().await.ok();
}

// ── 10. Reader/editor authorization on the vault RPCs (JWT) ──────────

#[tokio::test]
async fn a_reader_may_fetch_but_not_append_and_an_editor_may() {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer).await;
    let url = format!("http://{}", server.addr());

    // The static token is still "Service": it sets up the vault store a JWT
    // principal could never create itself (`createStore` is Service-only).
    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() })
        .await
        .expect("the static token still works alongside a JWT verifier");
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = admin.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();
    let seeded_seq = admin.vault_append(store_id, VaultDocId::Tree, b64(b"seed")).await.unwrap();

    let mut reader_grant = HashMap::new();
    reader_grant.insert(store_id, "reader");
    let reader_jwt = make_jwt(&sk, "kid-1", issuer, "reader-user", "reader@example.com", &reader_grant, 3600);
    let reader = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: reader_jwt }).await.expect("a valid JWT connects");

    let fetched = reader.vault_fetch(store_id, VaultDocId::Tree, 0).await.expect("a reader may fetch");
    assert_eq!(fetched.updates.iter().map(|u| u.seq).collect::<Vec<_>>(), vec![seeded_seq]);
    reader.vault_list_docs(store_id).await.expect("a reader may list docs");

    let write_err = reader
        .vault_append(store_id, VaultDocId::Tree, b64(b"nope"))
        .await
        .expect_err("a reader may not append");
    assert!(write_err.to_string().contains("Forbidden"), "expected a Forbidden error, got: {}", write_err);

    let snapshot_err = reader
        .vault_snapshot(store_id, VaultDocId::Tree, seeded_seq, b64(b"nope"))
        .await
        .expect_err("a reader may not snapshot");
    assert!(snapshot_err.to_string().contains("Forbidden"), "expected a Forbidden error, got: {}", snapshot_err);

    let mut editor_grant = HashMap::new();
    editor_grant.insert(store_id, "editor");
    let editor_jwt = make_jwt(&sk, "kid-1", issuer, "editor-user", "editor@example.com", &editor_grant, 3600);
    let editor = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: editor_jwt }).await.unwrap();
    editor.vault_append(store_id, VaultDocId::Tree, b64(b"ok")).await.expect("an editor may append");
}
