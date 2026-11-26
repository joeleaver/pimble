use super::*;

// ── Attachments (docs/DISCUSSIONS_CONTRACT.md section 1) ────────────

fn attach(tree: &mut Tree, host: NodeId, title: &str) -> (NodeId, TreeEdit) {
    let new = id();
    let edit = tree.add_attachment(new, host, "discussion", title, None, T1).unwrap();
    (new, edit)
}

#[test]
fn an_attachment_edits_only_its_own_document_and_is_in_no_list() {
    let (mut tree, root) = tree();
    let host = note(&mut tree, root, "Host");
    let before = tree.doc(host).unwrap().save();
    let (d, edit) = attach(&mut tree, host, "Topic");
    assert_eq!(edit.node_ids(), vec![d], "only the new document is written");
    assert_eq!(tree.doc(host).unwrap().save(), before, "the host is untouched");
    assert_eq!(tree.get_children(host).unwrap(), Vec::<NodeId>::new());
    assert_eq!(tree.get_children(root).unwrap(), vec![host]);
    assert_eq!(tree.attachments_of(host), vec![d]);
    assert!(tree.is_attachment(d) && !tree.is_attachment(host));
    let fields = tree.live_fields(d).unwrap();
    assert_eq!((fields.parent_id, fields.attached_to), (None, Some(host)));
    assert!(tree.validate_tree().is_empty(), "an attachment is not detached: {:?}", tree.validate_tree());
    assert!(tree.repair(T2).unwrap().is_none(), "repair never places an attachment");
    assert_eq!(tree.attachment_counts(), HashMap::from([(host, 1)]));
}

#[test]
fn an_attachment_keeps_the_content_it_was_made_with() {
    let (mut tree, root) = tree();
    let host = note(&mut tree, root, "Host");
    let mut seed = NodeDoc::new();
    seed.set_data("discussion", &serde_json::json!({ "v": 1 })).unwrap();
    let d = id();
    let edit = tree.add_attachment(d, host, "discussion", "", Some(&seed.save()), T1).unwrap();
    assert_eq!(tree.doc(d).unwrap().data_json()["discussion"]["v"], serde_json::json!(1));
    // The whole document travels as the one update.
    let mut replica = Tree::from_docs(root, HashMap::new());
    apply_edit(&mut replica, &edit);
    assert_eq!(replica.doc(d).unwrap().data_json(), tree.doc(d).unwrap().data_json());
    assert_eq!(replica.doc(d).unwrap().attached_to(), Some(host));
}

#[test]
fn a_deleted_host_hides_its_attachments_and_an_undelete_brings_them_back() {
    let (mut tree, root) = tree();
    let folder_id = folder(&mut tree, root, "F");
    let host = note(&mut tree, folder_id, "Host");
    let (d, _) = attach(&mut tree, host, "Topic");
    tree.remove_node(folder_id, T2).unwrap();
    assert!(tree.attachments_of(host).is_empty());
    assert!(tree.attachment_counts().is_empty());
    assert!(tree.has_node(d), "the attachment itself is not tombstoned");
    assert!(tree.repair(T3).unwrap().is_none(), "nor is it adopted by the root");
    tree.undelete_node(folder_id, T3).unwrap();
    assert_eq!(tree.attachments_of(host), vec![d]);
}

#[test]
fn an_attachment_is_deleted_and_put_back_without_a_list() {
    let (mut tree, root) = tree();
    let host = note(&mut tree, root, "Host");
    let (d, _) = attach(&mut tree, host, "Topic");
    let root_before = tree.doc(root).unwrap().save();
    let edit = tree.remove_node(d, T2).unwrap();
    assert_eq!(edit.node_ids(), vec![d]);
    assert!(tree.attachments_of(host).is_empty());
    let edit = tree.undelete_node(d, T3).unwrap();
    assert_eq!(edit.node_ids(), vec![d], "no list takes it back");
    assert_eq!(tree.attachments_of(host), vec![d]);
    assert_eq!(tree.doc(root).unwrap().save(), root_before);
    assert_eq!(tree.live_fields(d).unwrap().parent_id, None);
}

#[test]
fn an_attachment_cannot_hold_one_and_needs_a_live_host() {
    let (mut tree, root) = tree();
    let host = note(&mut tree, root, "Host");
    let (d, _) = attach(&mut tree, host, "Topic");
    assert!(tree.add_attachment(id(), d, "discussion", "", None, T1).is_err());
    tree.remove_node(host, T2).unwrap();
    assert!(tree.add_attachment(id(), host, "discussion", "", None, T2).is_err());
    assert!(tree.add_attachment(id(), id(), "discussion", "", None, T2).is_err());
}

#[test]
fn attachments_merge_and_converge_on_two_replicas() {
    let (mut a, root) = tree();
    let host = note(&mut a, root, "Host");
    let mut b = replica_of(&a);
    let (da, _) = attach(&mut a, host, "From a");
    let db = id();
    b.add_attachment(db, host, "discussion", "From b", None, T2).unwrap();
    sync_and_repair_round(&mut a, &mut b);
    assert_converged(&mut a, &mut b);
    let mut both = vec![da, db];
    both.sort_by_key(|id| id.to_string());
    let mut on_a = a.attachments_of(host);
    on_a.sort_by_key(|id| id.to_string());
    assert_eq!(on_a, both);
    assert_eq!(a.attachments_of(host), b.attachments_of(host), "the same order on both");
}

#[test]
fn a_transplant_makes_the_attachments_again_under_the_new_nodes() {
    let mut s = shared();
    let (d, _) = attach(&mut s.tree, s.x, "On X");
    s.tree.doc_mut(d).unwrap().set_data("discussion", &serde_json::json!({ "v": 1 })).unwrap();
    let mut made = Vec::new();
    let (new_a, _) = s.tree.transplant(HERE, s.a, s.private, Place::Last, T3, &mut fresh_ids(&mut made)).unwrap();
    let new_x = s.tree.get_children(new_a).unwrap().into_iter().find(|&c| s.tree.live_fields(c).unwrap().title == "X").unwrap();
    let moved = s.tree.attachments_of(new_x);
    assert_eq!(moved.len(), 1, "the discussion came along");
    assert_ne!(moved[0], d, "as a new document");
    assert_eq!(s.tree.live_fields(moved[0]).unwrap().title, "On X");
    assert_eq!(s.tree.doc(moved[0]).unwrap().data_json()["discussion"]["v"], serde_json::json!(1));
    assert!(made.contains(&moved[0]));
    assert!(s.tree.get_children(new_x).unwrap().is_empty(), "and is in no list");
    // The original is hidden with its tombstoned host, and comes back with it.
    assert!(s.tree.attachments_of(s.x).is_empty());
    s.tree.undelete_node(s.a, T3).unwrap();
    assert_eq!(s.tree.attachments_of(s.x), vec![d]);
    assert!(s.tree.validate_tree().is_empty(), "{:?}", s.tree.validate_tree());
}
