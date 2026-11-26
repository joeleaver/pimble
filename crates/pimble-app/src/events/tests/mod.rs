//! Tests of the event handlers: an `AppStore` fed events by hand, and the
//! helpers the test modules share.

use super::*;
use crossbeam_channel::bounded;
use pimble_core::{Node, NodeId, StoreId, SyncState};
use pimble_rpc::{MemberRole, ShareInfo, ShareMember, ShareMemberStatus};

use crate::protocol::{BackendHandle, CloudOp};
use crate::state::MountInfo;

/// An `AppStore` with a backend whose events this test feeds by hand, and
/// the command channel the handlers send into, so a test can see what a
/// handler asked the server for.
fn store_with_events() -> (AppStore, crossbeam_channel::Sender<BackendEvent>, crossbeam_channel::Receiver<BackendCommand>) {
    let store = AppStore::new();
    let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(64);
    let (event_tx, event_rx) = bounded::<BackendEvent>(64);
    store.backend.set(Some(BackendHandle { cmd_tx, event_rx }));
    (store, event_tx, cmd_rx)
}

/// Whether `c` asks for `node`'s children: alone, or with others when a
/// store opens (`LoadChildren`).
fn asks_children(c: &BackendCommand, node: NodeId) -> bool {
    match c {
        BackendCommand::GetChildren { node_id, .. } => *node_id == node,
        BackendCommand::LoadChildren { nodes, .. } => nodes.contains(&node),
        _ => false,
    }
}

fn pump(store: AppStore) {
    process_backend_events(store, UseTreeReturn::new(UseTreeOptions::default()));
}

fn share_info(store_id: StoreId, node_id: NodeId, name: &str) -> ShareInfo {
    ShareInfo {
        store_id,
        node_id,
        name: name.to_string(),
        state: SyncState::Syncing,
    }
}

/// docs/STARTUP_CONTRACT.md, as the events say it: a store that fails to
/// open says why on its row, one that opens takes its row's place, a
/// store's index state reaches its row (from `getStoreSync` and from its
/// notifications), and the account part waits, answers or fails.
#[test]
fn starting_up_is_said_as_it_happens() {
    use crate::state::CloudAnswer;
    let (store, events, _commands) = store_with_events();
    assert_eq!(store.cloud_answer.get(), CloudAnswer::Waiting, "connecting... until the server answers");

    store.begin_opening("/tmp/a.pimble");
    store.begin_opening("/tmp/b.pimble");
    events.send(BackendEvent::StoreOpenFailed { path: "/tmp/b.pimble".into(), message: "No such store.".into() }).unwrap();
    let mut opened = pimble_core::Store::new_local("A", "/tmp/a.pimble".into());
    opened.index = pimble_core::IndexState::Opening;
    let store_id = opened.id;
    events.send(BackendEvent::StoreOpened { store: opened }).unwrap();
    pump(store);
    let left: Vec<crate::state::OpeningStore> = store.opening_stores.get();
    assert_eq!(left.len(), 1, "the opened store's row is its own now: {left:?}");
    assert_eq!((left[0].path.as_str(), left[0].error.as_deref()), ("/tmp/b.pimble", Some("No such store.")));
    let index = || store.get_store_signal(store_id).unwrap().with(|s| s.index.clone());
    assert_eq!(index(), pimble_core::IndexState::Opening);

    events.send(BackendEvent::IndexStateChanged { store_id, state: pimble_core::IndexState::Rebuilding { done: 0, total: 9 } }).unwrap();
    pump(store);
    assert_eq!(index().row_words(), "indexing 9 notes");
    events
        .send(BackendEvent::RemoteStoreChange { store_id, change_kind: pimble_rpc::StoreChangeKind::IndexStateChanged { state: pimble_core::IndexState::Ready }, source_client_id: None })
        .unwrap();
    pump(store);
    assert!(index().is_ready());

    events.send(BackendEvent::CloudError { op: CloudOp::Status, message: "connection refused".into() }).unwrap();
    pump(store);
    assert_eq!(store.cloud_answer.get(), CloudAnswer::Failed("connection refused".into()));
    events.send(BackendEvent::CloudStatusChanged { signed_in: false, email: None, user_id: None, url: None }).unwrap();
    pump(store);
    assert_eq!(store.cloud_answer.get(), CloudAnswer::Answered);
    events.send(BackendEvent::Disconnected).unwrap();
    pump(store);
    assert_eq!(store.cloud_answer.get(), CloudAnswer::Waiting, "a lost connection is not \"disconnected\" from Pimble Cloud");
}

/// Built through serde: this crate has no chrono of its own.
fn synced() -> SyncState {
    serde_json::from_str(r#"{"state":"synced","last_sync":"2026-09-21T00:00:00Z"}"#).unwrap()
}

fn sync_changed(store_id: StoreId, access: pimble_core::StoreAccess, read_only_roots: Vec<NodeId>) -> BackendEvent {
    BackendEvent::StoreSyncChanged {
        store_id,
        remote: None,
        state: synced(),
        sync_mode: pimble_core::StoreKind::Vault,
        access,
        read_only_roots,
        ended_roots: Vec::new(),
        relay: pimble_core::RelaySide::None,
        owner_offline: false,
        waiting: Vec::new(),
        out_of_step: Vec::new(),
    }
}

/// A view of `store_id` as the API would show it.
fn shown(store_id: StoreId, nodes: Vec<Node>, lists: Vec<(NodeId, Vec<Node>)>, gone: Vec<NodeId>) -> BackendEvent {
    let lists = lists.into_iter().map(|(parent, children)| pimble_api::Listed { parent, children_store: store_id, children }).collect();
    BackendEvent::Shown { view: pimble_api::View { store: store_id, nodes, lists, gone } }
}

mod column_view;
mod nodes;
mod replicas;
mod sharing;
