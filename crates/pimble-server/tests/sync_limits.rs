//! No size ever stops a store from syncing (docs/SYNC_LIMITS_CONTRACT.md),
//! through real servers: Alice's desktop links a store to the hosted twin
//! `H`, and her second device reads it.
//!
//! - Compaction by size: many large appends that replace each other never
//!   fill the log; the snapshot fires on size, long before 200 appends.
//! - A 20 MiB document goes both ways in parts (append and snapshot), a
//!   half-uploaded snapshot never replaces a whole one, and a reader that
//!   knows nothing of parts opens none of them.
//! - A document that cannot be read is listed with its reason while every
//!   other document keeps syncing.
//!
//! Its own test binary: the vault's limits are process-wide
//! (`pimble_store::vault::set_limits_for_tests`), and every test here runs
//! under the same ones, which an 8 MiB part fits and the compaction test's
//! appends would overflow without compaction.

mod common;

use pimble_host::disk::testfs as tfs;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use common::*;
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::vault::{self as protocol, PART_BYTES};
use pimble_rpc::{VaultDocId, VaultPart, VaultSnapshotRequest};
use uuid::Uuid;

const MIB: usize = 1024 * 1024;
/// One blob may be a part and a little more; a log may hold 32 MiB.
const BLOB_LIMIT: usize = PART_BYTES + MIB;
const LOG_LIMIT: u64 = 32 * MIB as u64;

fn limits() {
    pimble_store::vault::set_limits_for_tests(BLOB_LIMIT, LOG_LIMIT);
}

/// `len` bytes of letters and spaces that do not repeat in any way a
/// compressor or a diff could shorten.
fn noise(len: usize, seed: u64) -> String {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if i % 9 == 8 { ' ' } else { (b'a' + (state % 26) as u8) as char }
        })
        .collect()
}

/// Put `value` in the node's plugin data under `blob`, replacing what was
/// there: a large edit that is not text, as a canvas's paint is.
async fn set_blob(client: &PimbleClient, store_id: StoreId, node_id: NodeId, value: &str) {
    let value = serde_json::Value::String(value.to_string());
    client.edit_node(store_id, node_id, "painter", |doc, _| doc.set_data("blob", &value)).await.expect("the large edit is taken");
}

async fn blob_of(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    match client.get_node(store_id, node_id).await {
        Ok(node) => NodeDoc::load(&node.content).ok().and_then(|doc| doc.data_json()["blob"].as_str().map(str::to_string)).unwrap_or_default(),
        Err(_) => String::new(),
    }
}

fn log_file_len(env: &Env, store_id: StoreId, node_id: NodeId) -> u64 {
    tfs::len(env.h_store_path(store_id).join("vault").join(node_id.to_string()).join("log")).unwrap_or(0)
}

async fn snapshot_seq_on_h(env: &Env, store_id: StoreId, node_id: NodeId) -> u64 {
    env.h_admin
        .vault_list_docs(store_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|d| d.doc_id == VaultDocId::Node(node_id))
        .map(|d| d.snapshot_seq)
        .unwrap_or(0)
}

/// Alice's hosted store with one document beside the root, both seeded.
async fn hosted(env: &Env, alice: &PimbleClient, dir: &std::path::Path, name: &str) -> (StoreId, NodeId, NodeId) {
    env.sign_in(alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(dir.join("a.pimble"), name).await.unwrap();
    let doc = alice.create_node(store_id, Some(root_id), "document", "Canvas").await.unwrap();
    seed_and_wait_stamped(alice, store_id, &[(doc, "small to start")]).await;
    alice.cloud_host_store(store_id).await.unwrap();
    wait_all_seeded(env, store_id, &[root_id, doc]).await;
    (store_id, root_id, doc)
}

/// Sixty appends of 600 KiB each, every one replacing the last (paint that
/// is painted over): 36 MiB of log were it never compacted, past the 32 MiB
/// limit and well under 200 appends. The size rule snapshots as it goes, the
/// log stays small, and a fresh device reads the last one.
#[tokio::test]
async fn many_large_appends_never_fill_the_log() {
    limits();
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let (store_id, _root, doc) = hosted(&env, &alice, a_dir.path(), "Paint").await;

    let mut largest_log = 0u64;
    for i in 0..60u64 {
        set_blob(&alice, store_id, doc, &noise(600 * 1024, i)).await;
        // One at a time, as a person paints: each reaches the twin before
        // the next is made.
        let reached = wait_until(Duration::from_secs(20), || async {
            env.h_admin.vault_fetch(store_id, VaultDocId::Node(doc), u64::MAX).await.map(|f| f.head >= i + 2).unwrap_or(false)
        })
        .await;
        assert!(reached, "append {i} reaches the twin");
        largest_log = largest_log.max(log_file_len(&env, store_id, doc));
    }
    assert!(snapshot_seq_on_h(&env, store_id, doc).await > 0, "a snapshot fired by size, long before 200 appends");
    assert!(largest_log < 8 * MIB as u64, "the log never grew near its limit (largest {largest_log} bytes)");
    let sync = alice.get_store_sync_response(store_id).await.unwrap();
    assert!(sync.waiting.is_empty(), "nothing waits: {:?}", sync.waiting);

    let (mut c, carol_device, _c_dir) = start_local_server().await;
    env.sign_in(&carol_device, ALICE).await;
    carol_device.cloud_add_hosted_store(store_id).await.expect("the other device adds the store");
    let last = noise(600 * 1024, 59);
    let read = wait_until(Duration::from_secs(30), || async { blob_of(&carol_device, store_id, doc).await == last }).await;
    assert!(read, "the other device reads the last append");

    a.stop().await.unwrap();
    c.stop().await.unwrap();
}

/// A 20 MiB document goes to the twin in parts and comes back whole on
/// another device; that device's 20 MiB edit comes back in parts too; the
/// snapshot the size rule makes of it is in parts; a snapshot cut short
/// leaves the whole one standing; and a reader that knows nothing of parts
/// (the whole-blob associated data an older Pimble uses) opens none.
#[tokio::test]
async fn a_twenty_megabyte_document_syncs_in_parts_both_ways() {
    limits();
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let (store_id, root_id, doc) = hosted(&env, &alice, a_dir.path(), "Big").await;
    let doc_key = VaultDocId::Node(doc);

    let big = noise(20 * MIB, 1);
    set_blob(&alice, store_id, doc, &big).await;

    // On the twin: the append went as one group of parts, and the size rule
    // snapshotted it, in parts as well.
    let snapshotted = wait_until(Duration::from_secs(60), || async { snapshot_seq_on_h(&env, store_id, doc).await > 0 }).await;
    assert!(snapshotted, "the size rule snapshots the 20 MiB document");
    let fetched = env.h_admin.vault_fetch(store_id, doc_key.clone(), 0).await.unwrap();
    let snapshot = fetched.snapshot.clone().expect("a snapshot");
    assert!(snapshot.part.is_some() && fetched.snapshot_parts.len() >= 2, "the snapshot is in parts");
    assert!(!env.hosted_store_contains_plaintext(store_id, &big[..64]), "ciphertext only");

    // A reader that knows nothing of parts opens none of them.
    let scope: std::collections::HashMap<Uuid, pimble_crypto::SymmetricKey> = scope_keys_in(a_dir.path(), store_id).into_iter().collect();
    let mut old_reader = protocol::DocKeys::default();
    old_reader.note_wraps(doc, fetched.keys.clone());
    for entry in std::iter::once(&snapshot).chain(fetched.snapshot_parts.iter()) {
        assert!(old_reader.open(store_id, doc, &entry.blob, &scope).is_err(), "a part never opens as a whole blob");
    }
    let mut reader = protocol::DocKeys::default();
    reader.note_wraps(doc, fetched.keys.clone());
    let units = protocol::fetched_units(&fetched);
    let whole = reader.open_unit(store_id, doc, &units[0], &scope).expect("a reader of parts opens the snapshot");
    let mut replayed = NodeDoc::new();
    replayed.apply_update(&whole).unwrap();
    assert_eq!(replayed.data_json()["blob"].as_str().map(str::len), Some(big.len()));

    // A snapshot cut short on the twin replaces nothing.
    let held = snapshot_seq_on_h(&env, store_id, doc).await;
    let half = VaultSnapshotRequest {
        store_id,
        doc_id: doc_key.clone(),
        upto_seq: fetched.head,
        blob: snapshot.blob.clone(),
        covers_prefix: true,
        part: Some(VaultPart { index: 0, count: 2, group: Uuid::new_v4() }),
    };
    env.h_admin.vault_snapshot_request(half).await.expect("a first part is taken");
    assert_eq!(snapshot_seq_on_h(&env, store_id, doc).await, held, "half a snapshot replaces nothing");
    let again = env.h_admin.vault_fetch(store_id, doc_key.clone(), 0).await.unwrap();
    assert_eq!(again.snapshot.as_ref().map(|s| s.blob.clone()), Some(snapshot.blob.clone()));

    // Alice's other device reads it whole, from the snapshot in parts.
    let (mut c, other, _c_dir) = start_local_server().await;
    env.sign_in(&other, ALICE).await;
    other.cloud_add_hosted_store(store_id).await.expect("the other device adds the store");
    let read = wait_until(Duration::from_secs(60), || async { blob_of(&other, store_id, doc).await.len() == big.len() }).await;
    assert!(read, "the other device reads the 20 MiB document");
    assert_eq!(blob_of(&other, store_id, doc).await, big);

    // The other way: a 20 MiB edit made there reaches Alice's first device
    // live, as a group of parts applied once all of them are in.
    let second = other.create_node(store_id, Some(root_id), "document", "Second").await.unwrap();
    let big2 = noise(20 * MIB, 2);
    set_blob(&other, store_id, second, &big2).await;
    let back = wait_until(Duration::from_secs(60), || async { blob_of(&alice, store_id, second).await.len() == big2.len() }).await;
    assert!(back, "the other device's 20 MiB edit reaches the first");
    assert_eq!(blob_of(&alice, store_id, second).await, big2);
    let on_h = env.h_admin.vault_fetch(store_id, VaultDocId::Node(second), 0).await.unwrap();
    let parted = on_h.updates.iter().filter(|e| e.part.is_some()).count() + on_h.snapshot_parts.len();
    assert!(parted >= 2, "it went in parts");
    for device in [&alice, &other] {
        assert!(device.get_store_sync_response(store_id).await.unwrap().waiting.is_empty());
    }

    a.stop().await.unwrap();
    c.stop().await.unwrap();
}

/// Two documents on the twin this device cannot read (one under a key it
/// does not hold, one in a format newer than it reads) wait, each listed
/// with its reason, in `getStoreSync` and `listStores`; another document's
/// edits keep going both ways.
#[tokio::test]
async fn a_waiting_document_is_listed_with_its_reason_while_others_keep_syncing() {
    limits();
    let env = spawn_env().await;
    let (mut a, alice, a_dir) = start_local_server().await;
    let (store_id, root_id, doc) = hosted(&env, &alice, a_dir.path(), "Mixed").await;
    let locked = alice.create_node(store_id, Some(root_id), "document", "Locked").await.unwrap();
    let newer = alice.create_node(store_id, Some(root_id), "document", "Newer").await.unwrap();
    wait_all_seeded(&env, store_id, &[locked, newer]).await;

    // Another device's append under a key Alice's device was never given.
    let foreign = pimble_crypto::Blob::encrypt(
        &pimble_crypto::SymmetricKey::generate(),
        Uuid::new_v4(),
        &pimble_crypto::blob_aad(&store_id.to_string(), &locked.to_string()),
        b"another device's edit",
    );
    env.h_admin.vault_append_from(store_id, VaultDocId::Node(locked), URL_SAFE_NO_PAD.encode(&foreign), Some("another-device".into())).await.unwrap();
    // And a blob of a format version this Pimble does not read.
    let scope = scope_keys_in(a_dir.path(), store_id);
    let mut raw = pimble_crypto::Blob::encrypt(&scope[0].1, scope[0].0, &pimble_crypto::blob_aad(&store_id.to_string(), &newer.to_string()), b"from the future");
    raw[2] = pimble_crypto::VERSION + 1;
    env.h_admin.vault_append_from(store_id, VaultDocId::Node(newer), URL_SAFE_NO_PAD.encode(&raw), Some("a-newer-device".into())).await.unwrap();

    let listed = wait_until(Duration::from_secs(20), || async {
        let waiting = alice.get_store_sync_response(store_id).await.map(|s| s.waiting).unwrap_or_default();
        waiting.iter().any(|w| w.node_id == locked) && waiting.iter().any(|w| w.node_id == newer)
    })
    .await;
    assert!(listed, "both documents are listed as waiting");
    let waiting = alice.get_store_sync_response(store_id).await.unwrap().waiting;
    assert_eq!(waiting.len(), 2, "and nothing else: {waiting:?}");
    assert_eq!(waiting.iter().find(|w| w.node_id == locked).unwrap().reason, protocol::WAITING_FOR_KEY);
    assert_eq!(waiting.iter().find(|w| w.node_id == newer).unwrap().reason, protocol::MADE_BY_NEWER);
    let store = alice.list_stores().await.unwrap().into_iter().find(|s| s.id == store_id).unwrap();
    assert_eq!(store.waiting.len(), 2, "listStores says so too");

    // Every other document keeps syncing, both ways.
    seed_content(&alice, store_id, doc, "editor", " and more").await;
    let (mut c, other, _c_dir) = start_local_server().await;
    env.sign_in(&other, ALICE).await;
    other.cloud_add_hosted_store(store_id).await.unwrap();
    let there = wait_until(Duration::from_secs(20), || async { node_text(&other, store_id, doc).await.contains("and more") }).await;
    assert!(there, "an edit of another document goes out");
    seed_content(&other, store_id, doc, "other", " from the other device").await;
    let back = wait_until(Duration::from_secs(20), || async { node_text(&alice, store_id, doc).await.contains("from the other device") }).await;
    assert!(back, "and one comes in");

    a.stop().await.unwrap();
    c.stop().await.unwrap();
}
