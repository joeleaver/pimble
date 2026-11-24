use super::*;

use std::collections::BTreeMap;

use pimble_core::custom_keys::SHARE;
use pimble_core::{PimbleUrl, Place, StoreId};

use crate::error::CrdtError;

mod attachments;
mod concurrency;
mod convergence;
mod list_wins;
mod operations;
mod repair;
mod shares;
mod transplant;

/// The store the tests' trees belong to (what a transplant's tombstones name).
const HERE: StoreId = StoreId(::uuid::Uuid::from_u128(0x5707e));
/// Another store, for transplants between two trees.
const THERE: StoreId = StoreId(::uuid::Uuid::from_u128(0x7e7e));

const T0: &str = "2026-09-18T10:00:00Z";
const T1: &str = "2026-09-18T10:00:01Z";
const T2: &str = "2026-09-18T10:00:02Z";
const T3: &str = "2026-09-18T10:00:03Z";

fn id() -> NodeId {
    NodeId::new()
}

fn tree() -> (Tree, NodeId) {
    let root = id();
    (Tree::new(root, "Store", T0).unwrap(), root)
}

/// A second tree holding copies of every document of `a`, as a replica
/// that reconciled once would.
fn replica_of(a: &Tree) -> Tree {
    let docs = a.ids().into_iter().map(|id| (id, NodeDoc::load(&a.doc(id).unwrap().save()).unwrap())).collect();
    Tree::from_docs(a.root(), docs)
}

fn apply_edit(tree: &mut Tree, edit: &TreeEdit) {
    for (id, update) in &edit.touched {
        tree.apply_update(*id, update).unwrap();
    }
}

fn ids_of(edit: &TreeEdit) -> HashSet<NodeId> {
    edit.node_ids().into_iter().collect()
}

/// Everything a tree holds, by id: the fields, the stored list, the text.
type Snapshot = BTreeMap<String, (Option<NodeFields>, Vec<NodeId>, String)>;

fn snapshot(tree: &Tree) -> Snapshot {
    tree.ids()
        .into_iter()
        .map(|id| {
            let doc = tree.doc(id).unwrap();
            (id.to_string(), (doc.fields().ok(), doc.children(), doc.text()))
        })
        .collect()
}

fn assert_same(a: &Tree, b: &Tree) {
    let (sa, sb) = (snapshot(a), snapshot(b));
    assert_eq!(sa.keys().collect::<Vec<_>>(), sb.keys().collect::<Vec<_>>(), "held documents differ");
    for (id, va) in &sa {
        assert_eq!(va, &sb[id], "document {id} differs");
    }
}

/// Exchange every document's diff both ways, creating documents the other
/// side lacks, then repair both and exchange the repairs: one round as the
/// server performs it.
fn sync(a: &mut Tree, b: &mut Tree) {
    let ids: HashSet<NodeId> = a.ids().into_iter().chain(b.ids()).collect();
    for id in ids {
        if a.doc(id).is_none() {
            a.insert_doc(id, NodeDoc::new());
        }
        if b.doc(id).is_none() {
            b.insert_doc(id, NodeDoc::new());
        }
        let (a_sv, b_sv) = (a.doc(id).unwrap().state_vector(), b.doc(id).unwrap().state_vector());
        let to_b = a.doc(id).unwrap().diff_since(&b_sv).unwrap();
        let to_a = b.doc(id).unwrap().diff_since(&a_sv).unwrap();
        b.apply_update(id, &to_b).unwrap();
        a.apply_update(id, &to_a).unwrap();
    }
}

fn sync_and_repair_round(a: &mut Tree, b: &mut Tree) {
    sync(a, b);
    let repair_a = a.repair(T3).unwrap();
    let repair_b = b.repair(T3).unwrap();
    if let Some(edit) = &repair_a {
        apply_edit(b, edit);
    }
    if let Some(edit) = &repair_b {
        apply_edit(a, edit);
    }
}

/// Both trees are well formed, nothing is left to repair, and every
/// document is the same on both sides.
fn assert_converged(a: &mut Tree, b: &mut Tree) {
    let issues = a.validate_tree();
    assert!(issues.is_empty(), "a not well formed: {issues:?}");
    let issues = b.validate_tree();
    assert!(issues.is_empty(), "b not well formed: {issues:?}");
    assert!(a.repair(T3).unwrap().is_none(), "a should have nothing left to repair");
    assert!(b.repair(T3).unwrap().is_none(), "b should have nothing left to repair");
    assert_same(a, b);
}

fn marker(name: &str) -> serde_json::Value {
    serde_json::json!({ "v": 1, "key_id": "5b0f6c1e-0000-4000-8000-00000000c0de", "url": "https://pimble.example", "name": name })
}

fn share(tree: &mut Tree, id: NodeId) -> TreeEdit {
    tree.set_custom(id, SHARE, &marker("Shared"), T1).unwrap()
}

fn folder(tree: &mut Tree, parent: NodeId, title: &str) -> NodeId {
    let new = id();
    tree.add_node(new, Some(parent), Place::Last, "folder", title, T1).unwrap();
    new
}

fn note(tree: &mut Tree, parent: NodeId, title: &str) -> NodeId {
    let new = id();
    tree.add_node(new, Some(parent), Place::Last, "document", title, T1).unwrap();
    new
}

/// An id source that records what it handed out.
fn fresh_ids(handed_out: &mut Vec<NodeId>) -> impl FnMut() -> NodeId + '_ {
    move || {
        let new = NodeId::new();
        handed_out.push(new);
        new
    }
}

fn never_asked() -> NodeId {
    panic!("a plain move needs no new id")
}

/// What a client that does not keep the rule writes: a `parent_id`, and
/// no list. `None` removes it.
fn tamper_parent(tree: &mut Tree, node: NodeId, parent: Option<NodeId>) -> TreeEdit {
    let (_, update) = tree.docs[&node]
        .edit_node(|d, txn| {
            d.write_parent_only(txn, parent);
            Ok(())
        })
        .unwrap();
    let mut edit = TreeEdit::default();
    edit.push(node, update);
    edit
}

/// The store the share tests use:
/// ```text
/// root ─ private ─ own
///      └ r0* ─ a ─ x
///            │   └ r2* ─ y
///            └ b
/// ```
/// `*` carries a share marker; `r2` is a share inside a share.
struct Shared {
    tree: Tree,
    root: NodeId,
    private: NodeId,
    own: NodeId,
    r0: NodeId,
    a: NodeId,
    b: NodeId,
    x: NodeId,
    r2: NodeId,
    y: NodeId,
}

fn shared() -> Shared {
    let (mut tree, root) = tree();
    let private = folder(&mut tree, root, "Private");
    let own = note(&mut tree, private, "Own");
    let r0 = folder(&mut tree, root, "Shared");
    let a = folder(&mut tree, r0, "A");
    let b = folder(&mut tree, r0, "B");
    let x = note(&mut tree, a, "X");
    let r2 = folder(&mut tree, a, "Shared inside");
    let y = note(&mut tree, r2, "Y");
    share(&mut tree, r0);
    share(&mut tree, r2);
    Shared { tree, root, private, own, r0, a, b, x, r2, y }
}
