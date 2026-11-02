//! The conformance suite (docs/API_CONTRACT.md section 4): the same calls
//! against any [`Backend`], with the answers and the [`ApiError`]s every
//! backend must give. It runs against the RPC backend over a real server
//! (`tests/conformance.rs`) and against the browser's vault (`web/`), so
//! "the vault answers what the server answers" is checked rather than hoped.
//!
//! What a backend can do may differ, and the difference is said, not
//! hidden: each run names the [`StoreCapabilities`] it expects.
//!
//! Test support only (the `conformance` feature): it panics on the first
//! answer that differs, naming the check.

use pimble_core::{node_types, LinkResolution, NodeId, Place, StoreId};
use pimble_crdt::{MoveRefused, NodeDoc};

use crate::backend::{Backend, NodeInit, StoreCapabilities};
use crate::ApiError;

/// Where the suite works: an empty store's id and its root, which it may
/// fill.
#[derive(Debug, Clone, Copy)]
pub struct Fixture {
    pub store: StoreId,
    pub root: NodeId,
}

/// The time the suite's own edits are stamped with.
const NOW: &str = "2026-11-02T12:00:00.000Z";

fn document(title: &str) -> NodeInit {
    NodeInit { node_type: node_types::DOCUMENT.to_string(), title: title.to_string(), content: None, custom: Vec::new() }
}

async fn titles(b: &dyn Backend, fx: Fixture, parent: NodeId) -> Vec<String> {
    let (at, nodes) = b.children(fx.store, parent).await.expect("children answer");
    assert_eq!(at, fx.store, "children live in the store asked about");
    nodes.into_iter().map(|n| n.metadata.title).collect()
}

/// Run every check; answers the names of the checks that passed (all of
/// them, or it panicked).
pub async fn run(b: &dyn Backend, fx: Fixture, capabilities: StoreCapabilities) -> Vec<&'static str> {
    let mut passed = Vec::new();
    let store = fx.store;
    let root = fx.root;

    // ── Capabilities, stated ─────────────────────────────────────────────
    assert_eq!(b.capabilities(store), capabilities, "capabilities");
    passed.push("capabilities");

    // ── Create, at each kind of place ────────────────────────────────────
    let a = b.create_node(store, root, document("A"), Place::Last).await.expect("create A");
    let bb = b.create_node(store, root, document("B"), Place::Last).await.expect("create B");
    let c = b.create_node(store, root, document("C"), Place::First).await.expect("create C");
    let d = b.create_node(store, root, document("D"), Place::After(a)).await.expect("create D");
    assert_eq!(titles(b, fx, root).await, ["C", "A", "D", "B"], "create: places");
    passed.push("create");

    let node = b.fetch_one(store, a).await.expect("fetch A");
    assert_eq!(node.id, a);
    assert_eq!(node.metadata.title, "A");
    assert_eq!(node.parent_id, Some(root), "fetch: parent");
    assert_eq!(node.node_type, node_types::DOCUMENT);
    let missing = NodeId::new();
    let fetched: std::collections::HashSet<NodeId> = b.fetch(store, vec![a, missing, c]).await.expect("fetch several").into_iter().map(|n| n.id).collect();
    assert_eq!(fetched, [a, c].into_iter().collect(), "fetch: a node not there is left out");
    assert_eq!(b.fetch_one(store, missing).await.expect_err("fetch a missing node"), ApiError::node_not_found(missing), "fetch: missing");
    passed.push("fetch");

    // ── The one write ────────────────────────────────────────────────────
    let mut doc = NodeDoc::load(&node.content).expect("the content is the node's document");
    let sv = doc.state_vector();
    doc.replace_plain_text("Hello, conformance").expect("text");
    doc.set_title("Alpha", NOW).expect("title");
    let delta = doc.diff_since(&sv).expect("delta");
    b.apply(store, a, delta, "conformance").await.expect("apply");
    let after = b.fetch_one(store, a).await.expect("fetch after the edit");
    assert_eq!(after.metadata.title, "Alpha", "edit: title");
    assert_eq!(NodeDoc::load(&after.content).expect("document").text(), "Hello, conformance", "edit: text");
    passed.push("edit");

    let (docs, _) = b.sync(store, vec![(a, pimble_crdt::empty_state_vector())], false).await.expect("sync");
    assert_eq!(docs.len(), 1, "sync: one document asked, one answered");
    assert_eq!(docs[0].node, a);
    assert_eq!(NodeDoc::load(&docs[0].diff).expect("diff").text(), "Hello, conformance", "sync: the whole document from nothing");
    passed.push("sync");

    // ── Moves ────────────────────────────────────────────────────────────
    let moved = b.move_node(store, d, a, Place::Last).await.expect("move D under A");
    assert_eq!(moved.node, d, "move: a move inside a store keeps the id");
    assert!(moved.left_shares.is_empty());
    assert_eq!(titles(b, fx, root).await, ["C", "Alpha", "B"], "move: gone from the old list");
    assert_eq!(titles(b, fx, a).await, ["D"], "move: in the new one");
    passed.push("move");

    // Each node once, answered in the order named, landing in that order.
    let moved = b.move_nodes(store, vec![bb, c, bb], a, Place::First).await.expect("moveNodes");
    assert_eq!(moved.iter().map(|m| m.node).collect::<Vec<_>>(), vec![bb, c], "moveNodes: each once, in the order named");
    assert_eq!(titles(b, fx, a).await, ["B", "C", "D"], "moveNodes: in order at the place");
    assert!(titles(b, fx, root).await == ["Alpha"], "moveNodes: gone from the old list");
    passed.push("move_nodes");

    assert_eq!(b.move_node(store, a, a, Place::Last).await.expect_err("into itself"), ApiError::of_move(MoveRefused::IntoItself), "move: into itself");
    assert_eq!(b.move_node(store, a, d, Place::Last).await.expect_err("into a descendant"), ApiError::of_move(MoveRefused::IntoDescendant), "move: into a descendant");
    assert_eq!(b.move_node(store, missing, a, Place::Last).await.expect_err("a missing node"), ApiError::of_move(MoveRefused::NotFound(missing)), "move: missing");
    // One refusal refuses them all, before anything moves.
    assert_eq!(
        b.move_nodes(store, vec![c, a], d, Place::Last).await.expect_err("one of them into its descendant"),
        ApiError::of_move(MoveRefused::IntoDescendant),
        "moveNodes: one refusal"
    );
    assert_eq!(titles(b, fx, a).await, ["B", "C", "D"], "moveNodes: a refusal moves nothing");
    passed.push("move_refusals");

    // ── Delete, the deleted list, undelete ───────────────────────────────
    b.delete(store, bb).await.expect("delete B");
    assert_eq!(titles(b, fx, a).await, ["C", "D"], "delete: gone from its list");
    assert!(b.fetch_one(store, bb).await.is_err(), "delete: a tombstone is not fetched");
    assert_eq!(b.resolve_link(store, bb).await.expect("resolve a tombstone"), LinkResolution::Deleted { store_id: store, node_id: bb });
    let deleted = b.list_deleted(store).await.expect("list deleted");
    let entry = deleted.iter().find(|e| e.node.id == bb).expect("list_deleted: the tombstone is listed");
    assert_eq!(entry.parent_title.as_deref(), Some("Alpha"), "list_deleted: the folder it was in");
    assert!(entry.deleted_at.is_some(), "list_deleted: when");
    assert!(entry.put_back_under.is_none(), "list_deleted: undelete puts a tombstone back where it was");
    assert_eq!(deleted.len(), 1, "list_deleted: only the top of what was deleted");
    passed.push("delete");

    b.undelete(store, bb).await.expect("undelete B");
    // Back under the folder it was deleted from, at the end of its list.
    assert_eq!(titles(b, fx, a).await, ["C", "D", "B"], "undelete: back under its folder");
    assert!(b.list_deleted(store).await.expect("list deleted").is_empty(), "undelete: nothing deleted");
    passed.push("undelete");

    assert_eq!(b.delete(store, root).await.expect_err("delete the root"), ApiError::invalid_operation("Cannot delete the root node"), "delete: the root");
    passed.push("delete_refusals");

    // ── Links ────────────────────────────────────────────────────────────
    assert_eq!(b.resolve_link(store, a).await.expect("resolve"), LinkResolution::Live { store_id: store, node_id: a });
    assert_eq!(b.resolve_link(store, missing).await.expect("resolve missing"), LinkResolution::Missing);
    let elsewhere = StoreId::new();
    assert_eq!(
        b.resolve_link(elsewhere, missing).await.expect("resolve elsewhere"),
        LinkResolution::StoreNotHere { store_id: elsewhere, node_id: missing },
        "resolve: a store not here"
    );
    passed.push("resolve_link");

    passed
}
