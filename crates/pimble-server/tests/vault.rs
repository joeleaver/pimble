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
    let stores_claim = stores.iter().map(|(id, role)| (id.to_string(), json!(role))).collect();
    make_jwt_with_claims(signing_key, kid, issuer, sub, email, stores_claim, exp_offset_secs)
}

/// A JWT for a member of one store's shares: a role per shared root
/// (docs/NODE_DOCUMENT_CONTRACT.md section 5).
fn make_scoped_jwt(signing_key: &SigningKey, kid: &str, issuer: &str, sub: &str, store_id: StoreId, roots: &[(NodeId, &str)]) -> String {
    let roots: serde_json::Map<String, serde_json::Value> = roots.iter().map(|(root, role)| (root.to_string(), json!(role))).collect();
    let mut stores_claim = serde_json::Map::new();
    stores_claim.insert(store_id.to_string(), json!({ "roots": roots }));
    make_jwt_with_claims(signing_key, kid, issuer, sub, &format!("{sub}@example.com"), stores_claim, 3600)
}

fn make_jwt_with_claims(
    signing_key: &SigningKey,
    kid: &str,
    issuer: &str,
    sub: &str,
    email: &str,
    stores_claim: serde_json::Map<String, serde_json::Value>,
    exp_offset_secs: i64,
) -> String {
    let header = json!({ "alg": "EdDSA", "kid": kid });
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

/// A client from before 2026-09-17 stamps a snapshot with the number of its own
/// latest append, whatever it had applied below it, and storing one deletes the
/// log entries it may lack. Its request carries no `covers_prefix`; the server
/// acknowledges it and keeps the log whole.
#[tokio::test]
async fn a_snapshot_that_does_not_vouch_for_its_prefix_is_acknowledged_and_ignored() {
    use pimble_rpc::{PimbleApiClient, VaultSnapshotRequest};

    let (server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();
    client.vault_append(store_id, VaultDocId::Tree, b64(b"a")).await.unwrap();
    client.vault_append(store_id, VaultDocId::Tree, b64(b"b")).await.unwrap();

    // The request as an old client sends it: no `covers_prefix` field at all.
    let raw = jsonrpsee::ws_client::WsClientBuilder::default()
        .build(format!("ws://{}", server.addr()))
        .await
        .expect("raw client connects");
    let old_style: VaultSnapshotRequest = serde_json::from_value(serde_json::json!({
        "store_id": store_id,
        "doc_id": VaultDocId::Tree,
        "upto_seq": 2,
        "blob": b64(b"an old client's state"),
    }))
    .expect("the flag defaults to false");
    assert!(!old_style.covers_prefix);
    raw.vault_snapshot(old_style).await.expect("acknowledged, not refused");

    let fetched = client.vault_fetch(store_id, VaultDocId::Tree, 0).await.unwrap();
    assert!(fetched.snapshot.is_none(), "nothing was stored");
    assert_eq!(fetched.updates.iter().map(|u| u.seq).collect::<Vec<_>>(), vec![1, 2], "the log is whole");
}

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

// ── 8b. vaultAppend's client_id names the source on the notification ────

#[tokio::test]
async fn vault_append_with_a_client_id_names_it_in_the_notification() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();

    // Two independent subscribers, both watching before the append happens.
    let mut sub_a = client.subscribe_store_changes(store_id).await.unwrap();
    let mut sub_b = client.subscribe_store_changes(store_id).await.unwrap();

    let seq = client
        .vault_append_from(store_id, VaultDocId::Tree, b64(b"hi"), Some("client-a".to_string()))
        .await
        .unwrap();

    for sub in [&mut sub_a, &mut sub_b] {
        let notif = tokio::time::timeout(Duration::from_secs(2), sub.next())
            .await
            .expect("a notification should arrive within 2s")
            .expect("the subscription stream should not end")
            .expect("the notification should deserialize");

        assert_eq!(notif.source_client_id.as_deref(), Some("client-a"), "source_client_id should name the appending client");
        match notif.change_kind {
            StoreChangeKind::VaultAppended { doc_id, seq: notified_seq } => {
                assert_eq!(doc_id, VaultDocId::Tree);
                assert_eq!(notified_seq, seq);
            }
            other => panic!("expected VaultAppended, got {:?}", other),
        }
    }

    // Omitting a client_id (the plain `vault_append` wrapper) still works
    // and names no source, as before.
    client.vault_append(store_id, VaultDocId::Tree, b64(b"anonymous")).await.unwrap();
    let notif = tokio::time::timeout(Duration::from_secs(2), sub_a.next())
        .await
        .expect("a notification should arrive within 2s")
        .expect("the subscription stream should not end")
        .expect("the notification should deserialize");
    assert_eq!(notif.source_client_id, None);
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
    assert_eq!(write_err.to_string(), pimble_core::StoreAccess::READ_ONLY_REFUSAL, "a reader's refusal is the sentence, whole");

    let snapshot_err = reader
        .vault_snapshot(store_id, VaultDocId::Tree, seeded_seq, b64(b"nope"))
        .await
        .expect_err("a reader may not snapshot");
    assert_eq!(snapshot_err.to_string(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);

    let keys = pimble_rpc::VaultDocKeys { dek_id: uuid::Uuid::new_v4(), wraps: Vec::new() };
    let keys_err = reader.vault_set_doc_keys(store_id, VaultDocId::Tree, keys).await.expect_err("a reader may not set keys");
    assert_eq!(keys_err.to_string(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);

    let mut editor_grant = HashMap::new();
    editor_grant.insert(store_id, "editor");
    let editor_jwt = make_jwt(&sk, "kid-1", issuer, "editor-user", "editor@example.com", &editor_grant, 3600);
    let editor = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: editor_jwt }).await.unwrap();
    editor.vault_append(store_id, VaultDocId::Tree, b64(b"ok")).await.expect("an editor may append");
}

// ── Sharing: scope sets, data keys, deletion (docs/NODE_DOCUMENT_CONTRACT.md
// section 5) ──────────────────────────────────────────────────────────────

const NO_GRANT: &str = "no grant for this document";

fn refused_as_no_grant<T: std::fmt::Debug>(result: Result<T, pimble_client::ClientError>, what: &str) {
    let err = result.expect_err(what).to_string();
    assert!(err.contains(NO_GRANT), "{what}: expected the document refusal, got: {err}");
}

async fn listed_nodes(client: &PimbleClient, store_id: StoreId) -> std::collections::HashSet<NodeId> {
    client
        .vault_list_docs(store_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|doc| match doc.doc_id {
            VaultDocId::Node(id) => Some(id),
            VaultDocId::Tree => None,
        })
        .collect()
}

/// A vault store with a published share and a document outside it, behind
/// a server in JWT mode.
struct SharedVault {
    server: PimbleServer,
    dir: tempfile::TempDir,
    url: String,
    sk: SigningKey,
    jwks_url: String,
    issuer: &'static str,
    admin: PimbleClient,
    store_id: StoreId,
    share_root: NodeId,
    inside: NodeId,
    outside: NodeId,
}

impl SharedVault {
    async fn start() -> Self {
        let sk = signing_key();
        let jwks_url = spawn_jwks(&sk, "kid-1").await;
        let issuer = "https://issuer.example/v1";
        let server = start_jwt_server("admin-secret", &jwks_url, issuer).await;
        let url = format!("http://{}", server.addr());
        let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (store_id, _) = admin.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();

        let (share_root, inside, outside) = (NodeId::new(), NodeId::new(), NodeId::new());
        for id in [share_root, inside, outside] {
            admin.vault_append(store_id, VaultDocId::Node(id), b64(b"seed")).await.unwrap();
        }
        admin.vault_append(store_id, VaultDocId::Tree, b64(b"the retired tree document")).await.unwrap();
        admin.set_scope(store_id, pimble_rpc::Scope { root: share_root, doc_ids: vec![inside] }, false).await.unwrap();
        Self { server, dir, url, sk, jwks_url, issuer, admin, store_id, share_root, inside, outside }
    }

    async fn member(&self, sub: &str, roots: &[(NodeId, &str)]) -> PimbleClient {
        let jwt = make_scoped_jwt(&self.sk, "kid-1", self.issuer, sub, self.store_id, roots);
        PimbleClient::connect_with_auth(&self.url, &AuthMethod::Bearer { token: jwt }).await.expect("a scoped JWT connects")
    }

    async fn whole(&self, sub: &str, role: &'static str) -> PimbleClient {
        let mut grant = HashMap::new();
        grant.insert(self.store_id, role);
        let jwt = make_jwt(&self.sk, "kid-1", self.issuer, sub, &format!("{sub}@example.com"), &grant, 3600);
        PimbleClient::connect_with_auth(&self.url, &AuthMethod::Bearer { token: jwt }).await.unwrap()
    }
}

#[tokio::test]
async fn a_scoped_editor_reaches_exactly_its_scope_of_a_vault_store() {
    let v = SharedVault::start().await;
    let (store_id, share_root, inside, outside) = (v.store_id, v.share_root, v.inside, v.outside);
    let member = v.member("bob", &[(share_root, "editor")]).await;
    let keys = || pimble_rpc::VaultDocKeys { dek_id: uuid::Uuid::new_v4(), wraps: Vec::new() };

    assert_eq!(listed_nodes(&member, store_id).await, [share_root, inside].into_iter().collect(), "the list is the scope: the root is in its own");
    assert!(member.vault_list_docs(store_id).await.unwrap().iter().all(|d| d.doc_id != VaultDocId::Tree), "the retired tree document is nobody's");

    member.vault_fetch(store_id, VaultDocId::Node(inside), 0).await.expect("a fetch in scope");
    let seq = member.vault_append(store_id, VaultDocId::Node(inside), b64(b"bob-1")).await.expect("an append in scope");
    member.vault_snapshot(store_id, VaultDocId::Node(inside), seq, b64(b"snap")).await.expect("a snapshot in scope");
    member.vault_set_doc_keys(store_id, VaultDocId::Node(inside), keys()).await.expect("keys in scope");

    for (what, doc) in [("a document outside", VaultDocId::Node(outside)), ("a document that does not exist", VaultDocId::Node(NodeId::new())), ("the tree document", VaultDocId::Tree)] {
        refused_as_no_grant(member.vault_fetch(store_id, doc.clone(), 0).await, what);
        refused_as_no_grant(member.vault_append(store_id, doc.clone(), b64(b"no")).await, what);
        refused_as_no_grant(member.vault_snapshot(store_id, doc.clone(), 1, b64(b"no")).await, what);
        refused_as_no_grant(member.vault_set_doc_keys(store_id, doc, keys()).await, what);
    }
    assert_eq!(v.admin.vault_fetch(store_id, VaultDocId::Node(outside), 0).await.unwrap().head, 1, "nothing refused was written");
}

#[tokio::test]
async fn a_scoped_members_create_joins_the_scope_and_notifications_stay_inside_it() {
    let v = SharedVault::start().await;
    let (store_id, share_root, inside, outside) = (v.store_id, v.share_root, v.inside, v.outside);
    let member = v.member("bob", &[(share_root, "editor")]).await;
    let other_member = v.member("carol", &[(share_root, "reader")]).await;
    let mut other_sub = other_member.subscribe_store_changes(store_id).await.unwrap();

    // A create names its parent, which has to be the member's to write.
    let created = NodeId::new();
    refused_as_no_grant(member.vault_append(store_id, VaultDocId::Node(created), b64(b"x")).await, "a new document naming no parent");
    refused_as_no_grant(
        member.vault_append_new(store_id, VaultDocId::Node(created), b64(b"x"), None, Some(outside)).await,
        "a new document under a parent outside the scope",
    );
    refused_as_no_grant(
        member.vault_append_new(store_id, VaultDocId::Node(outside), b64(b"x"), None, Some(inside)).await,
        "a document the store has is not made the member's by naming a parent",
    );
    let reader_create = other_member.vault_append_new(store_id, VaultDocId::Node(NodeId::new()), b64(b"x"), None, Some(inside)).await;
    assert_eq!(reader_create.expect_err("a reader creates nothing").to_string(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);

    // Something outside first: what the other member hears first is then
    // proof that it did not hear that.
    v.admin.vault_append(store_id, VaultDocId::Node(outside), b64(b"not theirs")).await.unwrap();
    member.vault_append_new(store_id, VaultDocId::Node(created), b64(b"made by bob"), Some("bob-device".into()), Some(inside)).await.expect("a create under a parent in scope");

    let notif = tokio::time::timeout(Duration::from_secs(5), other_sub.next()).await.expect("the create reaches the share's other member").unwrap().unwrap();
    match notif.change_kind {
        StoreChangeKind::VaultAppended { doc_id, .. } => assert_eq!(doc_id, VaultDocId::Node(created), "in scope before it was announced, and the append outside never was"),
        other => panic!("expected VaultAppended, got {:?}", other),
    }
    assert!(listed_nodes(&other_member, store_id).await.contains(&created));
    member.vault_append(store_id, VaultDocId::Node(created), b64(b"and again")).await.expect("its maker keeps writing it");
    // One level down: under the document just made.
    let grandchild = NodeId::new();
    member.vault_append_new(store_id, VaultDocId::Node(grandchild), b64(b"deeper"), None, Some(created)).await.expect("a create under a create");

    // The owner sees the scope the members reach; a publish from a device
    // that has not pulled the new documents yet does not take them away.
    let scope_docs = |scopes: Vec<pimble_rpc::Scope>| -> std::collections::HashSet<NodeId> { scopes.into_iter().find(|s| s.root == share_root).unwrap().doc_ids.into_iter().collect() };
    assert_eq!(scope_docs(v.admin.get_scopes(store_id).await.unwrap()), [inside, created, grandchild].into_iter().collect());
    v.admin.set_scope(store_id, pimble_rpc::Scope { root: share_root, doc_ids: vec![inside] }, false).await.unwrap();
    member.vault_fetch(store_id, VaultDocId::Node(created), 0).await.expect("still the member's after a stale publish");
    // Named by a publish, then left out of the next: the owner moved it out.
    v.admin.set_scope(store_id, pimble_rpc::Scope { root: share_root, doc_ids: vec![inside, created, grandchild] }, false).await.unwrap();
    v.admin.set_scope(store_id, pimble_rpc::Scope { root: share_root, doc_ids: vec![inside, created] }, false).await.unwrap();
    refused_as_no_grant(member.vault_fetch(store_id, VaultDocId::Node(grandchild), 0).await, "a document its owner moved out of the share");
}

#[tokio::test]
async fn scope_sets_are_an_owners_to_publish_and_survive_a_restart() {
    let mut v = SharedVault::start().await;
    let (store_id, share_root, inside) = (v.store_id, v.share_root, v.inside);
    let member = v.member("bob", &[(share_root, "editor")]).await;
    let created = NodeId::new();
    member.vault_append_new(store_id, VaultDocId::Node(created), b64(b"x"), None, Some(share_root)).await.unwrap();

    let scope = pimble_rpc::Scope { root: NodeId::new(), doc_ids: vec![inside] };
    for (who, client) in [("a share's member", &member), ("a whole-store editor", &v.whole("ed", "editor").await), ("a whole-store reader", &v.whole("rd", "reader").await)] {
        let err = client.set_scope(store_id, scope.clone(), false).await.expect_err(who).to_string();
        assert!(err.contains("only available to an owner"), "{who}: {err}");
        let err = client.get_scopes(store_id).await.expect_err(who).to_string();
        assert!(err.contains("only available to an owner"), "{who}: {err}");
    }
    let owner = v.whole("ann", "owner").await;
    owner.set_scope(store_id, scope.clone(), false).await.expect("an owner publishes");
    assert_eq!(owner.get_scopes(store_id).await.unwrap().len(), 2);
    owner.set_scope(store_id, scope.clone(), true).await.expect("and removes");
    assert_eq!(owner.get_scopes(store_id).await.unwrap().len(), 1);

    // A restart: a new server over the same directory.
    let path = v.dir.path().join("v.pimble");
    assert!(path.join("scopes.json").exists());
    v.server.stop().await.unwrap();
    let server = start_jwt_server("admin-secret", &v.jwks_url, v.issuer).await;
    let url = format!("http://{}", server.addr());
    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
    admin.open_store(&path).await.unwrap();
    let jwt = make_scoped_jwt(&v.sk, "kid-1", v.issuer, "bob", store_id, &[(share_root, "editor")]);
    let member = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: jwt }).await.unwrap();
    assert_eq!(listed_nodes(&member, store_id).await, [share_root, inside, created].into_iter().collect(), "the published set and the member's create, both");
}

/// A role per shared root on a vault store: what a member only reads
/// refuses their write with the reader's sentence; what they edit takes it.
#[tokio::test]
async fn a_reader_of_one_vault_scope_and_editor_of_another() {
    let v = SharedVault::start().await;
    let (store_id, read_root, read_doc) = (v.store_id, v.share_root, v.inside);
    let (edit_root, edit_doc) = (NodeId::new(), NodeId::new());
    for id in [edit_root, edit_doc] {
        v.admin.vault_append(store_id, VaultDocId::Node(id), b64(b"seed")).await.unwrap();
    }
    v.admin.set_scope(store_id, pimble_rpc::Scope { root: edit_root, doc_ids: vec![edit_doc] }, false).await.unwrap();
    let member = v.member("dana", &[(read_root, "reader"), (edit_root, "editor")]).await;

    assert_eq!(listed_nodes(&member, store_id).await, [read_root, read_doc, edit_root, edit_doc].into_iter().collect());
    member.vault_fetch(store_id, VaultDocId::Node(read_doc), 0).await.expect("reads what it reads");
    let err = member.vault_append(store_id, VaultDocId::Node(read_doc), b64(b"no")).await.expect_err("no write under the read root");
    assert_eq!(err.to_string(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);
    let err = member.vault_append_new(store_id, VaultDocId::Node(NodeId::new()), b64(b"no"), None, Some(read_doc)).await.expect_err("no create under it either");
    assert_eq!(err.to_string(), pimble_core::StoreAccess::READ_ONLY_REFUSAL);
    member.vault_append(store_id, VaultDocId::Node(edit_doc), b64(b"yes")).await.expect("writes what it edits");
    refused_as_no_grant(member.vault_fetch(store_id, VaultDocId::Node(v.outside), 0).await, "and the rest is nobody's");
}

#[tokio::test]
async fn data_keys_round_trip_and_merge_by_scope_key() {
    let (_server, client) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let (store_id, _root) = client.create_store_with(dir.path().join("v.pimble"), "V", StoreKind::Vault, None).await.unwrap();
    let node = NodeId::new();
    let doc = VaultDocId::Node(node);
    let aad = pimble_crypto::dek_aad(&store_id.to_string(), &doc.as_str());

    client.vault_append(store_id, doc.clone(), b64(b"from before data keys")).await.unwrap();
    assert_eq!(client.vault_fetch(store_id, doc.clone(), 0).await.unwrap().keys, None, "a document from before data keys has none");
    assert_eq!(client.vault_list_docs(store_id).await.unwrap()[0].dek_id, None);

    let (dek, dek_id) = (pimble_crypto::SymmetricKey::generate(), uuid::Uuid::new_v4());
    let (store_key, store_key_id) = (pimble_crypto::SymmetricKey::generate(), uuid::Uuid::new_v4());
    let (share_key, share_key_id) = (pimble_crypto::SymmetricKey::generate(), uuid::Uuid::new_v4());
    let under_store = pimble_crypto::wrap_dek(&dek, &store_key, store_key_id, &aad);
    let under_share = pimble_crypto::wrap_dek(&dek, &share_key, share_key_id, &aad);

    client.vault_set_doc_keys(store_id, doc.clone(), pimble_rpc::VaultDocKeys { dek_id, wraps: vec![under_store.clone()] }).await.unwrap();
    let fetched = client.vault_fetch(store_id, doc.clone(), 0).await.unwrap().keys.expect("keys come back with the document");
    assert_eq!(fetched, pimble_rpc::VaultDocKeys { dek_id, wraps: vec![under_store.clone()] });
    assert_eq!(client.vault_list_docs(store_id).await.unwrap()[0].dek_id, Some(dek_id));
    let unwrapped = pimble_crypto::unwrap_dek(&fetched.wraps[0], &store_key, &aad).unwrap();
    assert_eq!(unwrapped.0, dek.0, "and open to the data key they were made from");

    // The same data key under another scope key: added. Under the same
    // scope key again: that wrap replaced. Another data key: a rotation.
    client.vault_set_doc_keys(store_id, doc.clone(), pimble_rpc::VaultDocKeys { dek_id, wraps: vec![under_share.clone()] }).await.unwrap();
    let merged = client.vault_fetch(store_id, doc.clone(), 0).await.unwrap().keys.unwrap();
    assert_eq!(merged.wraps.len(), 2);
    assert!(merged.wraps.contains(&under_store) && merged.wraps.contains(&under_share));
    let under_store_again = pimble_crypto::wrap_dek(&dek, &store_key, store_key_id, &aad);
    client.vault_set_doc_keys(store_id, doc.clone(), pimble_rpc::VaultDocKeys { dek_id, wraps: vec![under_store_again.clone()] }).await.unwrap();
    let rewrapped = client.vault_fetch(store_id, doc.clone(), 0).await.unwrap().keys.unwrap();
    assert_eq!(rewrapped.wraps.len(), 2);
    assert!(rewrapped.wraps.contains(&under_store_again) && !rewrapped.wraps.contains(&under_store));

    let (rotated, rotated_id) = (pimble_crypto::SymmetricKey::generate(), uuid::Uuid::new_v4());
    let rotated_wrap = pimble_crypto::wrap_dek(&rotated, &store_key, store_key_id, &aad);
    client.vault_set_doc_keys(store_id, doc.clone(), pimble_rpc::VaultDocKeys { dek_id: rotated_id, wraps: vec![rotated_wrap.clone()] }).await.unwrap();
    assert_eq!(client.vault_fetch(store_id, doc.clone(), 0).await.unwrap().keys.unwrap(), pimble_rpc::VaultDocKeys { dek_id: rotated_id, wraps: vec![rotated_wrap] });

    // On disk beside the document, and back after a reopen; keys can come
    // before a document's first blob.
    let path = dir.path().join("v.pimble");
    assert!(path.join("vault").join(doc.as_str()).join("keys.json").exists());
    let early = VaultDocId::Node(NodeId::new());
    client.vault_set_doc_keys(store_id, early.clone(), pimble_rpc::VaultDocKeys { dek_id, wraps: vec![under_share] }).await.unwrap();
    client.close_store(store_id).await.unwrap();
    client.open_store(&path).await.unwrap();
    assert_eq!(client.vault_fetch(store_id, doc, 0).await.unwrap().keys.unwrap().dek_id, rotated_id);
    let early_fetch = client.vault_fetch(store_id, early, 0).await.unwrap();
    assert_eq!((early_fetch.head, early_fetch.keys.map(|k| k.dek_id)), (0, Some(dek_id)));
}

#[tokio::test]
async fn delete_vault_store_removes_an_open_vault_and_nothing_else() {
    let sk = signing_key();
    let jwks_url = spawn_jwks(&sk, "kid-1").await;
    let issuer = "https://issuer.example/v1";
    let server = start_jwt_server("admin-secret", &jwks_url, issuer).await;
    let url = format!("http://{}", server.addr());
    let admin = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: "admin-secret".into() }).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let vault_path = dir.path().join("v.pimble");
    let plain_path = dir.path().join("p.pimble");
    let (vault_id, _) = admin.create_store_with(&vault_path, "V", StoreKind::Vault, None).await.unwrap();
    let (plain_id, _) = admin.create_store(&plain_path, "P").await.unwrap();
    admin.vault_append(vault_id, VaultDocId::Node(NodeId::new()), b64(b"ciphertext")).await.unwrap();

    // The accounts service's call, and nobody else's: not even an owner's.
    let mut grant = HashMap::new();
    grant.insert(vault_id, "owner");
    let owner_jwt = make_jwt(&sk, "kid-1", issuer, "ann", "ann@example.com", &grant, 3600);
    let owner = PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token: owner_jwt }).await.unwrap();
    let err = owner.delete_vault_store(vault_id).await.expect_err("Service-only").to_string();
    assert!(err.contains("only available to this server's own operator"), "{err}");

    let err = admin.delete_vault_store(plain_id).await.expect_err("a plain store is refused").to_string();
    assert!(err.contains("is not a vault store"), "{err}");
    assert!(plain_path.join("manifest.json").exists(), "and left exactly where it was");
    admin.delete_vault_store(StoreId::new()).await.expect_err("a store that is not open here");

    admin.delete_vault_store(vault_id).await.expect("the open vault goes");
    assert!(!vault_path.exists(), "its directory with it");
    assert!(admin.list_stores().await.unwrap().iter().all(|s| s.id != vault_id));
    admin.vault_list_docs(vault_id).await.expect_err("nothing answers for it any more");
    admin.delete_vault_store(vault_id).await.expect_err("twice is an error, not a second removal");
    assert!(plain_path.join("manifest.json").exists());
}
