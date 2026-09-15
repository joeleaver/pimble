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
use pimble_crdt::ContentDoc;
use pimble_rpc::{
    ApplyEditRequest, ApplyStoreUpdateRequest, CreateNodeRequest, CreateStoreRequest,
    EditOperation, GetNodeRequest, PimbleApiServer, StoreChangeKind, StoreChangedNotification,
    UpdateNodeContentRequest,
};
use pimble_server::{PimbleServer, RpcHandler, ServerConfig};
use pimble_store::StoreManager;
use tokio::sync::RwLock;

/// Set up an `RpcHandler` backed by a fresh temporary store, with one
/// document node created under the root. Returns the handler plus the
/// store/node ids, and the `TempDir` guard.
async fn new_handler_with_store() -> (RpcHandler, StoreId, NodeId, tempfile::TempDir) {
    let store_manager = Arc::new(RwLock::new(StoreManager::new()));
    let handler = RpcHandler::new(store_manager);

    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("test.pimble");

    let create_resp = handler
        .create_store(&pimble_server::service_extensions(), CreateStoreRequest { path: store_path, name: "Test Store".into() })
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
/// unchanged — unlike the first (real) application of the same bytes.
#[tokio::test]
async fn resending_an_applied_content_edit_produces_no_notification_or_modified_at_change() {
    let (handler, store_id, node_id, _dir) = new_handler_with_store().await;

    let base = ContentDoc::from_plain_text("Hello").unwrap();
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

    let richer = ContentDoc::from_plain_text("Hello\nWorld").unwrap();
    let diff = richer.diff_since(&base.state_vector()).unwrap();
    let changes_b64 = base64::engine::general_purpose::STANDARD.encode(&diff);
    let edit = ApplyEditRequest {
        store_id,
        node_id,
        client_id: "peer".into(),
        operation: EditOperation::IncrementalChanges { changes: changes_b64 },
    };

    // The real application: must notify.
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

    let modified_before = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id }).await.unwrap().node.metadata.modified_at;

    // The resend: same bytes, already fully merged.
    handler.apply_edit(&pimble_server::service_extensions(), edit).await.unwrap();

    let resend_notif = tokio::time::timeout(Duration::from_millis(500), sub.next::<StoreChangedNotification>()).await;
    assert!(resend_notif.is_err(), "expected no notification for the resend, got {:?}", resend_notif);

    let modified_after = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id }).await.unwrap().node.metadata.modified_at;
    assert_eq!(modified_before, modified_after, "resending an already-merged edit must not touch modified_at");
}

/// The same, for `applyStoreUpdate`/`TreeStructure`: a peer's diff already
/// fully reflected in the server's store document produces no notification
/// and touches no node's `modified_at`.
#[tokio::test]
async fn resending_an_applied_store_update_produces_no_notification_or_modified_at_change() {
    let (handler, store_id, root_id, _dir) = new_handler_with_store().await;

    let mut module = handler.clone().into_rpc();
    module.extensions_mut().insert(pimble_server::Principal::Service);
    let mut sub = module.subscribe_unbounded("pimble_subscribeStoreChanges", (store_id,)).await.unwrap();

    // Bootstrap an independent, fully mergeable replica of the server's
    // current store document (same "disjoint state vector" technique
    // `store_sync.rs` uses), then have it add a node and diff just that.
    let bootstrap = handler
        .sync_store_document(&pimble_server::service_extensions(), pimble_rpc::SyncStoreDocumentRequest {
            store_id,
            state_vector: base64::engine::general_purpose::STANDARD.encode(
                pimble_crdt::StoreDocument::new("bootstrap", NodeId::new()).unwrap().state_vector(),
            ),
        })
        .await
        .unwrap();
    let snapshot = base64::engine::general_purpose::STANDARD.decode(&bootstrap.diff).unwrap();
    let mut peer = pimble_crdt::StoreDocument::load(&snapshot).unwrap();
    let peer_sv_before = peer.state_vector();
    let new_node_id = NodeId::new();
    peer.add_node(new_node_id, Some(root_id), "document", "From Peer").unwrap();
    let update = peer.diff_since(&peer_sv_before).unwrap();
    let update_b64 = base64::engine::general_purpose::STANDARD.encode(&update);
    let request = ApplyStoreUpdateRequest { store_id, client_id: "peer".into(), update: update_b64 };

    // The real application: must notify.
    handler.apply_store_update(&pimble_server::service_extensions(), request.clone()).await.unwrap();
    let (notif, _sub_id) = tokio::time::timeout(Duration::from_secs(1), sub.next::<StoreChangedNotification>())
        .await
        .expect("expected a notification for the real update")
        .expect("subscription still open")
        .expect("notification decodes");
    assert!(
        matches!(&notif.change_kind, StoreChangeKind::TreeStructure { node_ids } if node_ids.contains(&new_node_id)),
        "expected TreeStructure naming the new node, got {:?}", notif.change_kind
    );

    let modified_before = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: root_id }).await.unwrap().node.metadata.modified_at;

    // The resend: same bytes, already fully merged.
    handler.apply_store_update(&pimble_server::service_extensions(), request).await.unwrap();

    let resend_notif = tokio::time::timeout(Duration::from_millis(500), sub.next::<StoreChangedNotification>()).await;
    assert!(resend_notif.is_err(), "expected no notification for the resend, got {:?}", resend_notif);

    let modified_after = handler.get_node(&pimble_server::service_extensions(), GetNodeRequest { store_id, node_id: root_id }).await.unwrap().node.metadata.modified_at;
    assert_eq!(modified_before, modified_after, "resending an already-merged store update must not touch modified_at");
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
/// both sides' store documents carry a real delete set — not just
/// insertions), unlinked and relinked with no further edits: decision 7
/// means neither side pushes a no-op `applyStoreUpdate`, so decision 8 never
/// even has a resend to swallow — no `ContentUpdated`/`TreeStructure`
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
