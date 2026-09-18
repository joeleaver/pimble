//! Tests for decision 8 of docs/history/HARDENING_CONTRACT.md: an update the server
//! already has changes nothing. The bug this guards against ("Found while
//! doing it" in the contract): a yrs v1 diff is never actually empty (`[0,
//! 0]` at minimum, plus the sender's whole delete set), so a naive
//! `if !diff.is_empty()` gate is always true — every reconcile re-applied
//! (and re-flushed, re-broadcast, re-indexed) an update the peer already
//! had, on every node, forever.
//!
//! `resending_an_applied_*` call `RpcHandler` directly (like
//! `content_sync.rs`/`store_sync.rs`), subscribing through
//! `RpcHandler::into_rpc()` on a clone so the original handle stays usable
//! for the direct calls that drive the scenario.
//! `relinking_two_already_synced_servers_produces_no_notifications` runs two
//! real `PimbleServer`s end to end, like `sync.rs`.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use pimble_core::{AuthMethod, NodeId, RemoteEndpoint, StoreId, SyncState};
use pimble_crdt::{NodeDoc, Tree};
use pimble_rpc::{
    ApplyEditRequest, CreateNodeRequest, CreateStoreRequest, EditOperation, GetNodeRequest, NodeStateVector,
    PimbleApiServer, StoreChangeKind, StoreChangedNotification, SyncNodesRequest, UpdateNodeContentRequest,
};
use pimble_server::{PimbleServer, RpcHandler, ServerConfig};
use pimble_store::StoreManager;
use tokio::sync::RwLock;

/// Real time past the server's content-flush debounce (750ms), which is
/// also when a person's content edit earns its `modified_at` stamp.
const PAST_FLUSH_DEBOUNCE: Duration = Duration::from_millis(1_200);

/// Set up an `RpcHandler` backed by a fresh temporary store, with one
/// document node created under the root. Returns the handler plus the
/// store/node ids, and the `TempDir` guard.
async fn new_handler_with_store() -> (RpcHandler, StoreId, NodeId, tempfile::TempDir) {
    let store_manager = Arc::new(RwLock::new(StoreManager::new()));
    let handler = RpcHandler::new(store_manager);

    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("test.pimble");

    let create_resp = handler
        .create_store(&pimble_server::service_extensions(), CreateStoreRequest { kind: Default::default(), store_id: None, path: store_path, name: "Test Store".into() })
        .await
        .unwrap();
    let node_resp = handler
        .create_node(&pimble_server::service_extensions(), CreateNodeRequest {
            store_id: create_resp.store_id,
            parent_id: Some(create_resp.root_node_id),
            node_type: "document".into(),
            title: "Doc".into(),
        })
        .await
        .unwrap();

    (handler, create_resp.store_id, node_resp.node_id, dir)
}

/// Re-sending an `applyEdit` the server already fully merged produces no
/// `storeChanged`/`nodeChanged` notification and leaves `modified_at`
/// unchanged — unlike the first (real) application of the same bytes, which
/// notifies and, once the flush debounce has passed, stamps the node.
#[tokio::test]
async fn resending_an_applied_content_edit_produces_no_notification_or_modified_at_change() {
    let (handler, store_id, node_id, _dir) = new_handler_with_store().await;

    let base = NodeDoc::from_plain_text("Hello").unwrap();
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(base.save());
    handler
        .update_node_content(&pimble_server::service_extensions(), UpdateNodeContentRequest { store_id, node_id, content: content_b64, client_id: None })
        .await
        .unwrap();

    // Subscribe only after seeding (whose own `updateNodeContent` also
    // notifies): the queue must be empty going into the real edit below, or
    // the seed's notification would be mistaken for the edit's and shift
    // everything that follows by one.
    let mut module = handler.clone().into_rpc();
    module.extensions_mut().insert(pimble_server::Principal::Service);
    let mut sub = module.subscribe_unbounded("pimble_subscribeStoreChanges", (store_id,)).await.unwrap();

    let richer = NodeDoc::from_plain_text("Hello\nWorld").unwrap();
    let diff = richer.diff_since(&base.state_vector()).unwrap();
    let changes_b64 = base64::engine::general_purpose::STANDARD.encode(&diff);
    let edit = ApplyEditRequest {
        store_id,
        node_id,
        client_id: "peer".into(),
        operation: EditOperation::IncrementalChanges { changes: changes_b64 },
    };

    // The real application: must notify.
    let seeded_modified = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id }).await.unwrap().node.metadata.modified_at;
    handler.apply_edit(&pimble_server::service_extensions(), edit.clone()).await.unwrap();
    let (notif, _sub_id) = tokio::time::timeout(Duration::from_secs(1), sub.next::<StoreChangedNotification>())
        .await
        .expect("expected a notification for the real edit")
        .expect("subscription still open")
        .expect("notification decodes");
    assert!(
        matches!(notif.change_kind, StoreChangeKind::ContentUpdated { node_id: n } if n == node_id),
        "expected ContentUpdated for the real edit, got {:?}", notif.change_kind
    );
    assert!(notif.update.is_some(), "the edit's bytes ride the notification");

    // A person's edit stamps `modified_at` with the flush: one stamp, its
    // own `MetadataUpdated` carrying the stamp's bytes.
    let (stamp, _sub_id) = tokio::time::timeout(PAST_FLUSH_DEBOUNCE, sub.next::<StoreChangedNotification>())
        .await
        .expect("expected the modified_at stamp after the flush debounce")
        .expect("subscription still open")
        .expect("notification decodes");
    assert!(
        matches!(stamp.change_kind, StoreChangeKind::MetadataUpdated { node_id: n } if n == node_id),
        "expected MetadataUpdated for the stamp, got {:?}", stamp.change_kind
    );
    assert!(stamp.update.is_some() && stamp.source_client_id.is_none(), "the server's own edit, with bytes");
    let modified_before = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id }).await.unwrap().node.metadata.modified_at;
    assert!(modified_before > seeded_modified, "the edit moved modified_at");

    // The resend: same bytes, already fully merged.
    handler.apply_edit(&pimble_server::service_extensions(), edit).await.unwrap();

    let resend_notif = tokio::time::timeout(PAST_FLUSH_DEBOUNCE, sub.next::<StoreChangedNotification>()).await;
    assert!(resend_notif.is_err(), "expected no notification for the resend, got {:?}", resend_notif);

    let modified_after = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id }).await.unwrap().node.metadata.modified_at;
    assert_eq!(modified_before, modified_after, "resending an already-merged edit must not touch modified_at");
}

/// The same, for a structural update: a peer's tree edit (a node created,
/// sent as the two documents' updates) already fully reflected in the
/// server's documents produces no notification and touches no node's
/// `modified_at`.
#[tokio::test]
async fn resending_an_applied_structure_update_produces_no_notification_or_modified_at_change() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let mut module = handler.clone().into_rpc();
    module.extensions_mut().insert(pimble_server::Principal::Service);
    let mut sub = module.subscribe_unbounded("pimble_subscribeStoreChanges", (store_id,)).await.unwrap();

    // A peer holding the server's one document (the root), as `syncNodes`
    // hands it out, adds a node in its own tree.
    let root_bytes = {
        let resp = handler
            .sync_nodes(&pimble_server::service_extensions(), SyncNodesRequest {
                store_id,
                nodes: vec![NodeStateVector { node_id: root_id, state_vector: base64::engine::general_purpose::STANDARD.encode(pimble_crdt::empty_state_vector()) }],
                list_unknown: false,
            })
            .await
            .unwrap();
        base64::engine::general_purpose::STANDARD.decode(&resp.nodes[0].diff).unwrap()
    };
    let mut peer = Tree::from_docs(root_id, [(root_id, NodeDoc::load(&root_bytes).unwrap())].into_iter().collect());
    let new_node_id = NodeId::new();
    let edit = peer.add_node(new_node_id, Some(root_id), None, "document", "From Peer", "2026-09-18T10:00:00Z").unwrap();
    let requests: Vec<ApplyEditRequest> = edit
        .touched
        .iter()
        .map(|(node_id, update)| ApplyEditRequest {
            store_id,
            node_id: *node_id,
            client_id: "peer".into(),
            operation: EditOperation::IncrementalChanges { changes: base64::engine::general_purpose::STANDARD.encode(update) },
        })
        .collect();

    // The real application: must notify, once per document.
    for request in &requests {
        handler.apply_edit(&pimble_server::service_extensions(), request.clone()).await.unwrap();
    }
    let mut kinds = Vec::new();
    for _ in 0..2 {
        let (notif, _sub_id) = tokio::time::timeout(Duration::from_secs(1), sub.next::<StoreChangedNotification>())
            .await
            .expect("expected a notification for the real update")
            .expect("subscription still open")
            .expect("notification decodes");
        kinds.push(notif.change_kind);
    }
    assert!(
        kinds.iter().any(|k| matches!(k, StoreChangeKind::NodeCreated { node_id, parent_id } if *node_id == new_node_id && *parent_id == root_id)),
        "expected NodeCreated for the peer's node, got {:?}", kinds
    );
    assert!(
        kinds.iter().any(|k| matches!(k, StoreChangeKind::TreeStructure { node_ids } if node_ids == &vec![root_id])),
        "expected TreeStructure for the root's list, got {:?}", kinds
    );

    let modified_before = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: root_id }).await.unwrap().node.metadata.modified_at;

    // The resend: same bytes, already fully merged.
    for request in requests {
        handler.apply_edit(&pimble_server::service_extensions(), request).await.unwrap();
    }

    let resend_notif = tokio::time::timeout(PAST_FLUSH_DEBOUNCE, sub.next::<StoreChangedNotification>()).await;
    assert!(resend_notif.is_err(), "expected no notification for the resend, got {:?}", resend_notif);

    let modified_after = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: root_id }).await.unwrap().node.metadata.modified_at;
    assert_eq!(modified_before, modified_after, "resending an already-merged structure update must not touch modified_at");
}

// ── Two real servers, relinking with no edits ───────────────────────────

/// Poll `cond` every 50ms until it returns `true` or `timeout` elapses.
async fn wait_until<F, Fut>(timeout: Duration, mut cond: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if cond().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn start_server() -> (PimbleServer, pimble_client::PimbleClient) {
    let mut server = PimbleServer::with_config(ServerConfig { addr: "127.0.0.1:0".parse().unwrap(), ..Default::default() });
    server.start().await.expect("server starts");
    let client = pimble_client::PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

fn remote_endpoint(addr: std::net::SocketAddr) -> RemoteEndpoint {
    RemoteEndpoint { url: format!("http://{}", addr).parse().unwrap(), auth: AuthMethod::None }
}

/// Wait up to `window` for a `ContentUpdated` or `TreeStructure` notification
/// on `sub`; any other kind (e.g. `SyncStateChanged`, expected around a
/// relink) is drained and ignored. Returns the offending notification, if
/// any showed up.
async fn wait_for_an_offending_notification(
    sub: &mut jsonrpsee::core::client::Subscription<StoreChangedNotification>,
    window: Duration,
) -> Option<StoreChangedNotification> {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, sub.next()).await {
            Ok(Some(Ok(notif))) => {
                if matches!(notif.change_kind, StoreChangeKind::ContentUpdated { .. } | StoreChangeKind::TreeStructure { .. }) {
                    return Some(notif);
                }
            }
            _ => return None,
        }
    }
}

/// Two servers, synced once (with a node already created and deleted, so
/// both sides' documents carry real delete sets and a tombstone — not just
/// insertions), unlinked and relinked with no further edits: decision 7
/// means neither side pushes a no-op `applyEdit`, so decision 8 never even
/// has a resend to swallow — no `ContentUpdated`/`TreeStructure`
/// notification appears on either side, and no node's `modified_at` moves.
#[tokio::test]
async fn relinking_two_already_synced_servers_produces_no_notifications() {
    let (server_b, client_b) = start_server().await;
    let b_dir = tempfile::tempdir().unwrap();
    let (store_id, root_id) = client_b.create_store(&b_dir.path().join("b.pimble"), "B").await.unwrap();
    let doc_id = client_b.create_node(store_id, Some(root_id), "document", "Doc").await.unwrap();

    let (_server_a, client_a) = start_server().await;
    let a_dir = tempfile::tempdir().unwrap();
    client_a
        .add_remote_store(remote_endpoint(server_b.addr()), store_id, Some(a_dir.path().join("a.pimble")))
        .await
        .expect("addRemoteStore succeeds");

    // A real delete before ever unlinking, so both sides' delete sets are
    // non-empty from here on.
    let doomed = client_a.create_node(store_id, Some(root_id), "document", "Doomed").await.unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || async { client_b.get_node(store_id, doomed).await.is_ok() }).await,
        "the new node should replicate to B before it's deleted"
    );
    client_a.delete_node(store_id, doomed).await.unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || async { client_b.get_node(store_id, doomed).await.is_err() }).await,
        "the delete should replicate to B before unlinking"
    );
    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(client_a.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. })))
        }).await,
        "link should reach Synced before unlinking"
    );

    let mut sub_a = client_a.subscribe_store_changes(store_id).await.unwrap();
    let mut sub_b = client_b.subscribe_store_changes(store_id).await.unwrap();

    // Unlink, then relink to the same remote — no edits in between.
    client_a.set_store_sync(store_id, None).await.unwrap();
    client_a.set_store_sync(store_id, Some(remote_endpoint(server_b.addr()))).await.unwrap();

    assert!(
        wait_until(Duration::from_secs(5), || async {
            matches!(client_a.get_store_sync(store_id).await, Ok((_, SyncState::Synced { .. })))
        }).await,
        "link should re-reach Synced after relinking"
    );

    let a_doc_before = client_a.get_node(store_id, doc_id).await.unwrap().metadata.modified_at;
    let b_doc_before = client_b.get_node(store_id, doc_id).await.unwrap().metadata.modified_at;

    let offending_a = wait_for_an_offending_notification(&mut sub_a, Duration::from_secs(1)).await;
    let offending_b = wait_for_an_offending_notification(&mut sub_b, Duration::from_secs(1)).await;
    assert!(offending_a.is_none(), "A saw an unexpected notification from the no-op relink: {:?}", offending_a);
    assert!(offending_b.is_none(), "B saw an unexpected notification from the no-op relink: {:?}", offending_b);

    let a_doc_after = client_a.get_node(store_id, doc_id).await.unwrap().metadata.modified_at;
    let b_doc_after = client_b.get_node(store_id, doc_id).await.unwrap().metadata.modified_at;
    assert_eq!(a_doc_before, a_doc_after, "A's untouched node must keep its modified_at across a no-op relink");
    assert_eq!(b_doc_before, b_doc_after, "B's untouched node must keep its modified_at across a no-op relink");
}
