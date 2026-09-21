//! `Node::access` on a share member's partial replica
//! (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"): a replica's
//! `sync.json` can hold the whole replica to read (`access`), or name the
//! roots only read while others are edited (`read_only_roots`), and the local
//! server refuses writes accordingly (`LocalStore::write_refused`). Every node
//! it returns carries the same judgement, so the app disables what would be
//! refused: before it did, a document under a read-only root took typing on
//! screen that the server refused and nothing kept.
//!
//! The JWT half of the same judgement (a scoped reader on a plain store) is in
//! `tests/auth.rs`, `a_returned_node_says_what_its_caller_may_change_of_it`.

use base64::Engine;
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, Node, NodeId, RemoteEndpoint, StoreAccess, StoreId};
use pimble_crdt::NodeDoc;
use pimble_rpc::EditOperation;
use pimble_server::{PimbleServer, ServerConfig};
use pimble_store::{LocalStore, SyncConfig, SyncMode};

/// A member's replica on disk, as a vault link leaves one:
///
/// ```text
/// Edited (scope root, edited)
/// ├── E (document)
/// └── Read inside (scope root, read)
///     └── Deep (document)          <- also under `Edited`: the wider role
/// Read (scope root, read)
/// └── R (document)
/// ```
struct Replica {
    store_id: StoreId,
    path: std::path::PathBuf,
    edited: NodeId,
    read: NodeId,
    under_edited: NodeId,
    under_read: NodeId,
    read_inside_edited: NodeId,
    deep: NodeId,
}

async fn replica_in(dir: &std::path::Path) -> Replica {
    let mut owner = LocalStore::create(dir.join("owner.pimble"), "Owner").await.unwrap();
    let root = owner.root_node_id();
    let (edited, _) = owner.create_node(Node::folder("Edited"), Some(root)).unwrap();
    let (read, _) = owner.create_node(Node::folder("Read"), Some(root)).unwrap();
    let (under_edited, _) = owner.create_node(Node::document("E"), Some(edited)).unwrap();
    let (under_read, _) = owner.create_node(Node::document("R"), Some(read)).unwrap();
    let (read_inside_edited, _) = owner.create_node(Node::folder("Read inside"), Some(edited)).unwrap();
    let (deep, _) = owner.create_node(Node::document("Deep"), Some(read_inside_edited)).unwrap();

    let path = dir.join("partial.pimble");
    let mut partial = LocalStore::create_replica_with_scope(&path, owner.id, "Shares", root, vec![edited, read, read_inside_edited]).await.unwrap();
    for id in [edited, read, under_edited, under_read, read_inside_edited, deep] {
        partial.apply_node_update(id, &owner.tree().doc(id).unwrap().save()).unwrap();
    }
    partial.flush().await.unwrap();
    Replica { store_id: owner.id, path, edited, read, under_edited, under_read, read_inside_edited, deep }
}

/// `sync.json` as a vault link writes it. No `vault_key_id`, so opening the
/// store starts no link and the file stays as the test wrote it.
fn held_as(access: StoreAccess, read_only_roots: Vec<NodeId>) -> SyncConfig {
    SyncConfig {
        remote: RemoteEndpoint { url: "ws://127.0.0.1:1/rpc".parse().unwrap(), auth: AuthMethod::None },
        last_sync: None,
        mode: SyncMode::Vault,
        via_relay: false,
        last_seq: Default::default(),
        vault_key_id: None,
        access,
        shared_by: Some("ann@example.com".into()),
        read_only_roots,
    }
}

async fn start_device(dir: &std::path::Path) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        keystore_path: Some(dir.join("keys.json")),
        credentials_path: Some(dir.join("credentials.json")),
        replicas_dir: Some(dir.join("replicas")),
        ..Default::default()
    });
    server.start().await.expect("local server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

fn edit_of(text: &str) -> EditOperation {
    let changes = base64::engine::general_purpose::STANDARD.encode(NodeDoc::from_plain_text(text).unwrap().save());
    EditOperation::IncrementalChanges { changes }
}

#[tokio::test]
async fn a_replica_answers_read_under_a_read_only_root_and_full_under_an_edited_one() {
    use StoreAccess::{Full, Read};
    let dir = tempfile::tempdir().unwrap();
    let r = replica_in(dir.path()).await;
    LocalStore::open(&r.path).await.unwrap().write_sync_config(&held_as(Full, vec![r.read, r.read_inside_edited])).await.unwrap();

    let (mut server, client) = start_device(dir.path()).await;
    let opened = client.open_store(&r.path).await.unwrap();
    let store_id = r.store_id;
    assert_eq!(opened.access, Full, "something here may be written");
    assert_eq!(opened.read_only_roots, vec![r.read, r.read_inside_edited], "and the store says which roots may not");

    // getNode: each node by the judgement a write of it would meet.
    for (id, expected, what) in [
        (r.read, Read, "the root held to read"),
        (r.under_read, Read, "a document under it"),
        (r.edited, Full, "the root that is edited"),
        (r.under_edited, Full, "a document under it"),
        (r.read_inside_edited, Full, "a read root inside an edited one: the wider role"),
        (r.deep, Full, "and what is under it"),
    ] {
        assert_eq!(client.get_node(store_id, id).await.unwrap().access, expected, "getNode: {what}");
    }
    // getChildren and getNodes carry it on every node of the list.
    let (_, under_read) = client.get_children(store_id, r.read).await.unwrap();
    assert_eq!(under_read.iter().map(|n| (n.id, n.access)).collect::<Vec<_>>(), vec![(r.under_read, Read)]);
    let (_, under_edited) = client.get_children(store_id, r.edited).await.unwrap();
    assert_eq!(under_edited.len(), 2);
    assert!(under_edited.iter().all(|n| n.access == Full), "{under_edited:?}");
    let nodes = client.get_nodes(store_id, vec![r.under_read, r.under_edited]).await.unwrap();
    assert_eq!(nodes.iter().map(|n| (n.id, n.access)).collect::<Vec<_>>(), vec![(r.under_read, Read), (r.under_edited, Full)]);

    // It is the judgement the write meets, not another one.
    let refused = client.apply_edit(store_id, r.under_read, "me", edit_of("no")).await.expect_err("what was answered `read` refuses the edit");
    assert_eq!(refused.to_string(), StoreAccess::READ_ONLY_REFUSAL);
    client.apply_edit(store_id, r.under_edited, "me", edit_of("yes")).await.expect("what was answered `full` takes it");

    // Through a mount the children are the SOURCE store's, and carry its
    // judgement, whatever this device may do in the store that mounts them.
    let (own_store, own_root) = client.create_store(dir.path().join("own.pimble"), "Mine").await.unwrap();
    let (mount_read, _) = client.create_mount(own_store, own_root, store_id, r.read, Some("Read".into())).await.unwrap();
    let (mount_edited, _) = client.create_mount(own_store, own_root, store_id, r.edited, Some("Edited".into())).await.unwrap();
    assert_eq!(client.get_node(own_store, mount_read).await.unwrap().access, Full, "the mount node is this store's own");
    let (children_store, through_read) = client.get_children(own_store, mount_read).await.unwrap();
    assert_eq!(children_store, store_id);
    assert_eq!(through_read.iter().map(|n| (n.id, n.access)).collect::<Vec<_>>(), vec![(r.under_read, Read)]);
    let (_, through_edited) = client.get_children(own_store, mount_edited).await.unwrap();
    assert!(!through_edited.is_empty() && through_edited.iter().all(|n| n.access == Full), "{through_edited:?}");

    // `getStoreSync` is where a running app hears of a change.
    let sync = client.get_store_sync_response(store_id).await.unwrap();
    assert_eq!((sync.access, sync.read_only_roots), (Full, vec![r.read, r.read_inside_edited]));

    server.stop().await.unwrap();
}

/// A replica held to read answers `read` for everything, and a role the
/// owner changes reaches the answers as soon as `sync.json` says so (a vault
/// link rewrites it at every connect, `refresh_grant`), with no reopen.
#[tokio::test]
async fn a_changed_role_changes_what_the_next_answer_says() {
    use StoreAccess::{Full, Read};
    let dir = tempfile::tempdir().unwrap();
    let r = replica_in(dir.path()).await;
    LocalStore::open(&r.path).await.unwrap().write_sync_config(&held_as(Read, Vec::new())).await.unwrap();

    let (mut server, client) = start_device(dir.path()).await;
    let opened = client.open_store(&r.path).await.unwrap();
    let store_id = r.store_id;
    assert_eq!(opened.access, Read);
    assert_eq!(client.get_node(store_id, r.under_edited).await.unwrap().access, Read);
    assert!(client.get_children(store_id, r.edited).await.unwrap().1.iter().all(|n| n.access == Read));

    // The owner made this account an editor of `Edited`: what the link
    // records, written here through the same store the server has open.
    let manager = server.store_manager();
    manager.read().await.write_sync_config(store_id, &held_as(Full, vec![r.read])).await.unwrap();
    assert_eq!(client.get_node(store_id, r.under_edited).await.unwrap().access, Full);
    assert_eq!(client.get_node(store_id, r.under_read).await.unwrap().access, Read);
    let sync = client.get_store_sync_response(store_id).await.unwrap();
    assert_eq!((sync.access, sync.read_only_roots), (Full, vec![r.read]));

    server.stop().await.unwrap();
}
