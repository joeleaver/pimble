//! The server refuses a child under an internal node (docs/THEMES_CONTRACT.md
//! section 3.1a): every way a node is placed in a plain store (`createNode`,
//! `moveNode`, `moveNodes`, `transplantNode`, and an `applyEdit` judged on
//! what its merge would leave) answers "Nothing goes inside a theme.", the
//! sentence the clients refuse it with, and changes nothing. These call
//! `RpcHandler` directly over a store in a temp directory.

use std::sync::Arc;

use base64::Engine;
use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::internal::InternalKind;
use pimble_core::{node_types, NodeId, Place, StoreId};
use pimble_crdt::{NodeDoc, Tree};
use pimble_rpc::{
    ApplyEditRequest, CreateNodeRequest, CreateStoreRequest, EditOperation, GetChildrenRequest, ListDeletedRequest, MoveNodeRequest,
    MoveNodesRequest, NodeStateVector, PimbleApiServer, SyncNodesRequest, TransplantNodeRequest,
};
use pimble_server::{service_extensions, RpcHandler};
use pimble_store::StoreManager;
use tokio::sync::RwLock;

const T0: &str = "2026-11-26T10:00:00Z";

fn holds_nothing() -> &'static str {
    InternalKind::Theme.holds_nothing()
}

fn assert_refused<T: std::fmt::Debug>(answer: Result<T, ErrorObjectOwned>) {
    let err = answer.expect_err("a child under a theme must be refused");
    assert_eq!(err.message(), holds_nothing(), "{err:?}");
}

struct Fixture {
    handler: RpcHandler,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let handler = RpcHandler::in_dir(Arc::new(RwLock::new(StoreManager::new())), dir.path());
        Fixture { handler, dir }
    }

    async fn store(&self, name: &str) -> (StoreId, NodeId) {
        let created = self
            .handler
            .create_store(&service_extensions(), CreateStoreRequest {
                kind: Default::default(),
                store_id: None,
                path: self.dir.path().join(format!("{name}.pimble")),
                name: name.into(),
            })
            .await
            .unwrap();
        (created.store_id, created.root_node_id)
    }

    async fn try_create(&self, store_id: StoreId, parent: NodeId, node_type: &str, title: &str) -> Result<NodeId, ErrorObjectOwned> {
        self.handler
            .create_node(&service_extensions(), CreateNodeRequest {
                store_id,
                parent_id: Some(parent),
                node_type: node_type.into(),
                title: title.into(),
                content: None,
                custom: Default::default(),
                place: Default::default(),
            })
            .await
            .map(|created| created.node_id)
    }

    async fn create(&self, store_id: StoreId, parent: NodeId, node_type: &str, title: &str) -> NodeId {
        self.try_create(store_id, parent, node_type, title).await.unwrap()
    }

    async fn children(&self, store_id: StoreId, node_id: NodeId) -> Vec<NodeId> {
        let answer = self.handler.get_children(&service_extensions(), GetChildrenRequest { store_id, node_id }).await.unwrap();
        answer.children.into_iter().map(|node| node.id).collect()
    }

    /// Every document the server holds, as a peer's `Tree`.
    async fn peer_tree(&self, store_id: StoreId, root_id: NodeId) -> Tree {
        let ext = service_extensions();
        let listing = self
            .handler
            .sync_nodes(&ext, SyncNodesRequest { store_id, nodes: Vec::new(), list_unknown: true, max_bytes: None })
            .await
            .unwrap();
        let empty = b64(&pimble_crdt::empty_state_vector());
        let fetched = self
            .handler
            .sync_nodes(&ext, SyncNodesRequest {
                store_id,
                nodes: listing.unknown_ids.iter().map(|id| NodeStateVector { node_id: *id, state_vector: empty.clone() }).collect(),
                list_unknown: false,
                max_bytes: None,
            })
            .await
            .unwrap();
        let docs = fetched.nodes.into_iter().map(|n| (n.node_id, NodeDoc::load(&unb64(&n.diff)).unwrap())).collect();
        Tree::from_docs(root_id, docs)
    }

    async fn apply(&self, store_id: StoreId, node_id: NodeId, client_id: &str, update: &[u8]) -> Result<(), ErrorObjectOwned> {
        self.handler
            .apply_edit(&service_extensions(), ApplyEditRequest {
                store_id,
                node_id,
                client_id: client_id.into(),
                operation: EditOperation::IncrementalChanges { changes: b64(update) },
            })
            .await
            .map(|_| ())
    }
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}

#[tokio::test]
async fn create_node_refuses_a_theme_as_parent() {
    let fx = Fixture::new();
    let (store, root) = fx.store("Store").await;
    let theme = fx.create(store, root, node_types::THEME, "Theme").await;

    assert_refused(fx.try_create(store, theme, "document", "Inside").await);
    assert!(fx.children(store, theme).await.is_empty());
    // A theme still goes where a node may go.
    fx.create(store, root, node_types::THEME, "Another").await;
}

#[tokio::test]
async fn move_node_refuses_a_theme_as_new_parent() {
    let fx = Fixture::new();
    let (store, root) = fx.store("Store").await;
    let theme = fx.create(store, root, node_types::THEME, "Theme").await;
    let note = fx.create(store, root, "document", "Note").await;

    let answer = fx
        .handler
        .move_node(&service_extensions(), MoveNodeRequest { store_id: store, node_id: note, new_parent_id: theme, place: Place::Last })
        .await;
    assert_refused(answer);
    assert!(fx.children(store, theme).await.is_empty());
    assert_eq!(fx.children(store, root).await, vec![theme, note]);
}

#[tokio::test]
async fn move_nodes_refuses_a_theme_as_new_parent_before_moving_any() {
    let fx = Fixture::new();
    let (store, root) = fx.store("Store").await;
    let theme = fx.create(store, root, node_types::THEME, "Theme").await;
    let a = fx.create(store, root, "document", "a").await;
    let b = fx.create(store, root, "document", "b").await;

    let answer = fx
        .handler
        .move_nodes(&service_extensions(), MoveNodesRequest { store_id: store, node_ids: vec![a, b], new_parent_id: theme, place: Place::Last })
        .await;
    assert_refused(answer);
    assert!(fx.children(store, theme).await.is_empty());
    assert_eq!(fx.children(store, root).await, vec![theme, a, b]);
}

#[tokio::test]
async fn transplant_node_refuses_a_theme_as_new_parent() {
    let fx = Fixture::new();
    let (from, from_root) = fx.store("From").await;
    let (to, to_root) = fx.store("To").await;
    let theme = fx.create(to, to_root, node_types::THEME, "Theme").await;
    let note = fx.create(from, from_root, "document", "Note").await;
    let ext = service_extensions();

    // Between stores.
    let answer = fx
        .handler
        .transplant_node(&ext, TransplantNodeRequest { from_store_id: from, node_id: note, to_store_id: to, new_parent_id: theme, place: Place::Last })
        .await;
    assert_refused(answer);
    assert!(fx.children(to, theme).await.is_empty());
    assert_eq!(fx.children(from, from_root).await, vec![note], "the source keeps the node");

    // Within one store, which is a move.
    let local_theme = fx.create(from, from_root, node_types::THEME, "Local theme").await;
    let answer = fx
        .handler
        .transplant_node(&ext, TransplantNodeRequest { from_store_id: from, node_id: note, to_store_id: from, new_parent_id: local_theme, place: Place::Last })
        .await;
    assert_refused(answer);
    assert!(fx.children(from, local_theme).await.is_empty());
}

/// An `applyEdit` is judged on what merging it would leave: a peer's move
/// under a theme (the node's `parent_id`, the theme's list) and a peer's
/// new node made under one are refused document by document, and nothing
/// is under the theme afterwards.
#[tokio::test]
async fn apply_edit_refuses_an_update_that_puts_a_child_under_a_theme() {
    let fx = Fixture::new();
    let (store, root) = fx.store("Store").await;
    let theme = fx.create(store, root, node_types::THEME, "Theme").await;
    let note = fx.create(store, root, "document", "Note").await;

    // A move: the node's own document and the theme's list are refused.
    let mut peer = fx.peer_tree(store, root).await;
    let edit = peer.move_node(note, theme, Place::Last, T0).unwrap();
    for (id, update) in &edit.touched {
        let answer = fx.apply(store, *id, "older-client", update).await;
        if *id == note || *id == theme {
            assert_refused(answer);
        }
    }
    assert!(fx.children(store, theme).await.is_empty());
    // The old parent's update went in: it only takes the note out of its
    // list, and records there that the note left. Repair leaves it so (a
    // device whose copy of a node names a parent that says it left must
    // not put it back, or it and the client that made the move would undo
    // each other's repair for as long as both are connected), so the note
    // is a live node no list names: in "Recently Deleted..." with a Put
    // Back to the root, never lost.
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert!(!fx.children(store, root).await.contains(&note), "repair does not undo the old parent's update");
    let deleted = fx.handler.list_deleted(&service_extensions(), ListDeletedRequest { store_id: store }).await.unwrap();
    let entry = deleted.nodes.iter().find(|d| d.node.id == note).expect("the note is offered back");
    assert_eq!((entry.deleted_at.as_deref(), entry.put_back_under), (None, Some(root)));
    fx.handler
        .move_node(&service_extensions(), MoveNodeRequest { store_id: store, node_id: note, new_parent_id: root, place: Place::Last })
        .await
        .unwrap();
    assert!(fx.children(store, root).await.contains(&note), "Put Back lists it under the root again");

    // A new node made under the theme: its first update and the theme's list.
    let mut peer = fx.peer_tree(store, root).await;
    let made = NodeId::new();
    let edit = peer.add_node(made, Some(theme), Place::Last, "document", "Inside", T0).unwrap();
    for (id, update) in &edit.touched {
        assert!(*id == made || *id == theme, "a create touches the node and its parent");
        assert_refused(fx.apply(store, *id, "older-client", update).await);
    }
    assert!(fx.children(store, theme).await.is_empty());

    // An edit of the theme that places nothing still merges.
    let mut peer = fx.peer_tree(store, root).await;
    let edit = peer.set_title(theme, "Renamed theme", T0).unwrap();
    for (id, update) in &edit.touched {
        fx.apply(store, *id, "older-client", update).await.unwrap();
    }
}

/// A relayed edit (a sync or vault link's) was judged where it was made and
/// is never refused here, since a replica that refused it would stay apart
/// from its peer for good; and a child already under a theme keeps taking
/// edits, since only what an update changes is judged.
#[tokio::test]
async fn apply_edit_takes_a_relayed_edit_and_edits_of_a_child_already_there() {
    let fx = Fixture::new();
    let (store, root) = fx.store("Store").await;
    let theme = fx.create(store, root, node_types::THEME, "Theme").await;
    let note = fx.create(store, root, "document", "Note").await;

    let mut peer = fx.peer_tree(store, root).await;
    let edit = peer.move_node(note, theme, Place::Last, T0).unwrap();
    for (id, update) in &edit.touched {
        fx.apply(store, *id, "sync-link:peer", update).await.unwrap();
    }
    assert_eq!(fx.children(store, theme).await, vec![note]);

    let mut peer = fx.peer_tree(store, root).await;
    let edit = peer.set_title(note, "Renamed", T0).unwrap();
    for (id, update) in &edit.touched {
        fx.apply(store, *id, "older-client", update).await.unwrap();
    }
}

/// An update that makes a node holding children a theme would put them
/// inside one: refused, whether the children are listed or only named by
/// their own `parent_id`. A childless node may become one.
#[tokio::test]
async fn apply_edit_refuses_making_a_node_with_children_a_theme() {
    let fx = Fixture::new();
    let (store, root) = fx.store("Store").await;
    let folder = fx.create(store, root, "folder", "Folder").await;
    fx.create(store, folder, "document", "Child").await;
    let empty = fx.create(store, root, "folder", "Empty").await;

    let mut peer = fx.peer_tree(store, root).await;
    let edit = peer.set_node_type(folder, node_types::THEME, T0).unwrap();
    for (id, update) in &edit.touched {
        assert_refused(fx.apply(store, *id, "older-client", update).await);
    }
    let edit = peer.set_node_type(empty, node_types::THEME, T0).unwrap();
    for (id, update) in &edit.touched {
        fx.apply(store, *id, "older-client", update).await.unwrap();
    }
}
