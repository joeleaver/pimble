//! The node document's own tests: bytes, fields, children, data, boards and
//! anchors.

use pimble_core::Anchor;
use serde_json::json;
use yrs::{Any, Map, MapPrelim, TextPrelim};

use super::*;
use crate::links::Resolved;
use crate::ContentDoc;

/// docs/CONVERGENCE_CONTRACT.md section 5, as yrs carries it: a parked
/// update rides along in `save` (so a document loaded again, or a
/// snapshot made from it, parks it again), never in a diff; setting it
/// aside drops it from both and changes nothing integrated; and should
/// what it waited for come after all, the device holding it sends it all.
#[test]
fn a_parked_update_rides_in_save_until_it_is_set_aside() {
    let mut maker = NodeDoc::from_plain_text("one").unwrap();
    let base = maker.save();
    let lost = maker.append_markdown("NOBODY HOLDS THIS").unwrap();
    let waits = maker.append_markdown("WAITS FOR IT").unwrap();
    let _ = lost;

    let mut here = NodeDoc::load(&base).unwrap();
    here.apply_update(&waits).unwrap();
    let missing = here.missing().expect("parked behind what never came");
    let (sv, digest) = (here.state_vector(), here.delete_set_digest());
    assert!(!here.text().contains("WAITS FOR IT"));

    // A snapshot of it (`save`) carries the parked update; a reader of
    // that snapshot parks it again. A diff does not carry it.
    let reread = NodeDoc::load(&here.save()).unwrap();
    assert_eq!(reread.missing().as_deref(), Some(missing.as_slice()), "a document loaded again parks it again");
    let from_diff = NodeDoc::load(&here.diff_since(&crate::empty_state_vector()).unwrap()).unwrap();
    assert_eq!(from_diff.missing(), None, "a diff never carries what is parked");

    assert_eq!(here.set_aside_parked().as_deref(), Some(missing.as_slice()));
    assert_eq!(here.missing(), None);
    assert_eq!(here.state_vector(), sv, "nothing integrated changed");
    assert_eq!(here.delete_set_digest(), digest);
    assert_eq!(here.set_aside_parked(), None, "nothing left to set aside");
    let after = NodeDoc::load(&here.save()).unwrap();
    assert_eq!(after.missing(), None, "and it is gone from the saved bytes: a snapshot made now holds only what is integrated");
    assert_eq!(after.state_vector(), sv);

    // The maker comes back after all: what it sends from this vector
    // carries the lost change and the one set aside.
    here.apply_update(&maker.diff_since(&here.state_vector()).unwrap()).unwrap();
    assert!(here.text().contains("NOBODY HOLDS THIS") && here.text().contains("WAITS FOR IT"), "got {:?}", here.text());
    assert_eq!(here.missing(), None);
}

/// A note with a canvas in its text is rewritten through Markdown with the
/// picture kept (docs/MCP_CONTRACT.md: pictures go back in by their
/// `#canvas=` URL), and a picture naming a canvas the note lacks is refused.
#[test]
fn markdown_rewrites_keep_canvas_pictures() {
    let mut doc = NodeDoc::new();
    doc.init("document", "Note", None, T0).unwrap();
    {
        let mut txn = doc.ydoc().transact_mut();
        doc.data_root().insert(&mut txn, "board:c1", yrs::MapPrelim::default());
    }
    doc.write_markdown("Intro\n\n![shot](pimble-blob:a/b#canvas=c1)\n\nEnd").unwrap();
    let md = doc.markdown().unwrap();
    assert!(md.contains("![shot](pimble-blob:a/b#canvas=c1)"), "{md}");
    doc.replace_content_markdown("Intro, longer\n\n![shot](pimble-blob:a/b#canvas=c1)\n\nEnd").unwrap();
    assert!(doc.markdown().unwrap().contains("#canvas=c1"));
    let err = doc.append_markdown("![](pimble-blob:a/b#canvas=nope)").unwrap_err();
    assert!(err.to_string().contains("no canvas nope"), "{err}");
    let err = doc.append_markdown("![](pimble-blob:a/b#canvas=c1)").unwrap_err();
    assert!(err.to_string().contains("shown twice"), "{err}");
}

/// Node fields (docs/SHEETS_CONTRACT.md section 8): one `node` root key
/// each, set case-insensitively, read in name order, merged per field,
/// and writable by a peer that does not hold the document.
#[test]
fn node_fields_merge_per_field() {
    let mut a = NodeDoc::new();
    a.init("document", "Client website", None, T0).unwrap();
    let mut b = NodeDoc::load(&a.save()).unwrap();
    a.set_field("rate", Some("120"), T1).unwrap();
    b.set_field("Client", Some("Acme"), T1).unwrap();
    let (sa, sb) = (a.state_vector(), b.state_vector());
    let to_b = a.diff_since(&sb).unwrap();
    let to_a = b.diff_since(&sa).unwrap();
    a.apply_update(&to_a).unwrap();
    b.apply_update(&to_b).unwrap();
    let both = vec![("Client".to_string(), "Acme".to_string()), ("rate".to_string(), "120".to_string())];
    assert_eq!(a.node_fields(), both);
    assert_eq!(b.node_fields(), both, "two first fields made apart both survive");
    a.set_field("Rate", Some("130"), T2).unwrap();
    assert_eq!(a.node_fields()[1], ("rate".to_string(), "130".to_string()), "the same field in another case");
    assert_eq!(a.fields().unwrap().into_node(NodeId::new()).metadata.fields, a.node_fields());
    // From a blank document, as a client that does not hold the node writes.
    let effect = a.apply_update(&NodeDoc::field_update("phone", Some("555"))).unwrap();
    assert!(effect.structure, "a field is metadata");
    assert!(a.node_fields().iter().any(|(n, v)| n == "phone" && v == "555"));
    a.set_field("client", None, T2).unwrap();
    assert!(!a.node_fields().iter().any(|(n, _)| n == "Client"));
}

/// An app's `embed` atom (a sheet's table in text) reads back with its
/// `data-*` attributes, and every reader of the content copes with it.
#[test]
fn an_embed_reads_back_and_every_reader_copes() {
    let embed = Block::Embed { attrs: vec![("data-sheet".to_string(), "t1".to_string())] };
    let blocks = vec![Block::plain("before"), embed.clone(), Block::plain("after")];
    let doc = NodeDoc::from_blocks(&blocks).unwrap();
    let peer = NodeDoc::load(&doc.save()).unwrap();
    assert_eq!(peer.blocks().unwrap(), blocks);
    // The embed is an empty line of the text, as a rule is.
    assert_eq!(peer.text(), "before\n\nafter");
    assert_eq!(peer.units().len(), 3);
    assert!(peer.links().is_empty());
    let md = peer.markdown().unwrap();
    assert!(md.contains("data-sheet=\"t1\""), "{md}");
}

/// `remove_embeds` takes the matching embeds out, at any depth, and
/// leaves the rest.
#[test]
fn remove_embeds_takes_out_only_the_matching_ones() {
    let embed = |id: &str| Block::Embed { attrs: vec![("data-sheet".to_string(), id.to_string())] };
    let mut doc = NodeDoc::from_blocks(&[Block::plain("a"), embed("t1"), Block::Blockquote { blocks: vec![embed("t1"), Block::plain("q")] }, embed("t2")]).unwrap();
    doc.remove_embeds("data-sheet", "t1").unwrap();
    assert_eq!(doc.blocks().unwrap(), vec![Block::plain("a"), Block::Blockquote { blocks: vec![Block::plain("q")] }, embed("t2")]);
    assert!(doc.remove_embeds("data-sheet", "t1").is_err(), "nothing left to take out");
}

const T0: &str = "2026-09-18T10:00:00Z";
const T1: &str = "2026-09-18T10:00:01Z";
const T2: &str = "2026-09-18T10:00:02Z";

fn id() -> NodeId {
    NodeId::new()
}

#[test]
fn a_new_document_is_uninitialised_and_saves_empty() {
    let doc = NodeDoc::new();
    assert!(!doc.is_initialised());
    assert!(doc.fields().is_err());
    assert_eq!(doc.text(), "");
    assert!(doc.units().is_empty());
    assert!(doc.children().is_empty());
    assert_eq!(doc.data_json(), json!({}));
    // Declaring the roots writes nothing: the bytes are an empty document.
    assert_eq!(doc.save(), ContentDoc::new().save());
    assert_eq!(NodeDoc::text_of(b"garbage"), "");
}

#[test]
fn a_metanode_is_two_keys_of_the_node_root_read_into_the_node() {
    use pimble_core::metanode::{Query, Source};
    let mut doc = NodeDoc::new();
    doc.init("document", "Work", None, T0).unwrap();
    assert!(doc.metanode().is_none());
    assert!(doc.fields().unwrap().into_node(id()).metadata.metanode.is_none());

    // A viewer alone shows the children.
    doc.set_metanode_viewer(Some("kanban"), T1).unwrap();
    let spec = doc.fields().unwrap().into_node(id()).metadata.metanode.unwrap();
    assert_eq!((spec.viewer.as_str(), &spec.source), ("kanban", &Source::children()));

    doc.set_metanode_source(Some(&Source::subtree(2)), T1).unwrap();
    let loaded = NodeDoc::load(&doc.save()).unwrap();
    assert_eq!(loaded.metanode().unwrap().source.query, Query::Subtree { depth: 2 });
    // Neither key is a field, and neither is a container: each is a string.
    assert!(loaded.node_fields().is_empty());
    assert_eq!(loaded.fields().unwrap().metanode_source.as_deref(), Some(r#"{"query":{"kind":"subtree","depth":2}}"#));

    // Clearing the viewer keeps the source for the next time.
    doc.set_metanode_viewer(None, T2).unwrap();
    assert!(doc.metanode().is_none());
    assert!(doc.fields().unwrap().metanode_source.is_some());
    doc.set_metanode_viewer(Some("kanban"), T2).unwrap();
    assert_eq!(doc.metanode().unwrap().source, Source::subtree(2));
    doc.set_metanode_source(None, T2).unwrap();
    assert_eq!(doc.metanode().unwrap().source, Source::children());
}

#[test]
fn a_metanode_set_on_another_device_merges_as_a_metadata_change() {
    use pimble_core::metanode::Query;
    let mut here = NodeDoc::new();
    here.init("document", "Work", None, T0).unwrap();
    let mut there = NodeDoc::load(&here.save()).unwrap();
    there.set_metanode_viewer(Some("calendar"), T1).unwrap();
    // A source a newer Pimble wrote is kept and read as the children.
    there.set_metanode_source(Some(&pimble_core::metanode::Source::parse(r#"{"query":{"kind":"tag","tag":"x"}}"#)), T1).unwrap();
    here.apply_update(&there.diff_since(&here.state_vector()).unwrap()).unwrap();
    let spec = here.metanode().unwrap();
    assert_eq!(spec.viewer, "calendar");
    assert_eq!(spec.source.query, Query::Later);
    assert!(spec.source.not_understood().is_some());
}

#[test]
fn every_field_round_trips_through_save_and_load() {
    let parent = id();
    let mut doc = NodeDoc::from_plain_text("body").unwrap();
    doc.init("document", "Title", Some(parent), T0).unwrap();
    doc.set_tags(&["a".into(), "b".into()], T1).unwrap();
    doc.set_custom("icon", &json!("star"), T1).unwrap();
    doc.set_custom("nested", &json!({"x": [1, 2, {"y": null}]}), T1).unwrap();
    doc.set_custom("gone", &json!(true), T1).unwrap();
    assert!(doc.remove_custom("gone", T1).unwrap());
    assert!(!doc.remove_custom("gone", T1).unwrap(), "an absent key is reported absent");
    doc.set_node_type("folder", T1).unwrap();
    doc.set_title("Renamed", T1).unwrap();
    doc.set_timestamps("2020-01-01T00:00:00Z", "2020-01-02T00:00:00Z").unwrap();
    doc.set_deleted(Some(T2)).unwrap();
    let child = id();
    doc.insert_child(0, child).unwrap();
    doc.set_data("count", &json!(3)).unwrap();

    let expected = NodeFields {
        node_type: "folder".into(),
        title: "Renamed".into(),
        parent_id: Some(parent),
        placed_under: Some(parent),
        created_at: "2020-01-01T00:00:00Z".into(),
        modified_at: "2020-01-02T00:00:00Z".into(),
        deleted_at: Some(T2.into()),
        became: None,
        attached_to: None,
        tags: vec!["a".into(), "b".into()],
        text_tags: Vec::new(),
        custom: HashMap::from([
            ("icon".to_string(), json!("star")),
            ("nested".to_string(), json!({"x": [1, 2, {"y": null}]})),
        ]),
        fields: Vec::new(),
        metanode_viewer: None,
        metanode_source: None,
    };
    assert_eq!(doc.fields().unwrap(), expected);

    let loaded = NodeDoc::load(&doc.save()).unwrap();
    assert!(loaded.is_initialised());
    assert_eq!(loaded.fields().unwrap(), expected);
    assert_eq!(loaded.children(), vec![child]);
    assert_eq!(loaded.text(), "body");
    assert_eq!(loaded.data_json(), json!({"count": 3}));

    // The tombstone clears, the parent can go, tags replace wholesale.
    let mut loaded = loaded;
    loaded.set_deleted(None).unwrap();
    loaded.set_parent_id(None, T2).unwrap();
    loaded.set_tags(&["c".into()], T2).unwrap();
    let fields = loaded.fields().unwrap();
    assert_eq!(fields.deleted_at, None);
    assert_eq!(fields.parent_id, None);
    assert_eq!(fields.tags, vec!["c".to_string()]);
    assert_eq!(fields.modified_at, T2);
}

#[test]
fn init_twice_is_an_error() {
    let mut doc = NodeDoc::new();
    doc.init("document", "One", None, T0).unwrap();
    let err = doc.init("document", "Two", None, T1).unwrap_err();
    assert!(err.to_string().contains("already initialised"), "{err}");
    assert_eq!(doc.fields().unwrap().title, "One");
}

#[test]
fn setters_stamp_modified_at_and_refuse_an_uninitialised_document() {
    let mut doc = NodeDoc::new();
    assert!(doc.set_title("x", T0).is_err());
    assert!(doc.set_deleted(Some(T0)).is_err());
    assert!(doc.touch_modified(T0).is_err());
    doc.init("document", "x", None, T0).unwrap();
    assert_eq!(doc.fields().unwrap().modified_at, T0);
    doc.set_title("y", T1).unwrap();
    assert_eq!(doc.fields().unwrap().modified_at, T1);
    doc.touch_modified(T2).unwrap();
    assert_eq!(doc.fields().unwrap().modified_at, T2);
}

#[test]
fn children_list_edits() {
    let mut doc = NodeDoc::new();
    let (a, b, c) = (id(), id(), id());
    doc.insert_child(0, a).unwrap();
    doc.insert_child(99, c).unwrap(); // clamped to the end
    doc.insert_child(1, b).unwrap();
    doc.insert_child(3, a).unwrap(); // a duplicate, as a concurrent append could leave
    assert_eq!(doc.children(), vec![a, b, c, a]);
    assert!(doc.remove_child(a).unwrap(), "removes every occurrence");
    assert_eq!(doc.children(), vec![b, c]);
    assert!(!doc.remove_child(a).unwrap());
    doc.remove_child_at(0).unwrap();
    assert_eq!(doc.children(), vec![c]);
    assert!(doc.remove_child_at(1).is_err());
}

/// The whole point of per-root effects: the server derives `ContentUpdated`
/// from `content` and the tree notifications from `structure`.
#[test]
fn apply_update_reports_the_roots_an_update_changed() {
    let mut a = NodeDoc::from_plain_text("one").unwrap();
    a.init("document", "Doc", None, T0).unwrap();
    let mut b = NodeDoc::load(&a.save()).unwrap();

    // A content-only edit.
    let delta = a.replace_plain_text("two").unwrap();
    let effect = b.apply_update(&delta).unwrap();
    assert_eq!(effect, NodeUpdateEffect { changed: true, structure: false, content: true, data: false });
    assert_eq!(b.text(), "two");

    // A rename.
    let sv = b.state_vector();
    a.set_title("Renamed", T1).unwrap();
    let effect = b.apply_update(&a.diff_since(&sv).unwrap()).unwrap();
    assert_eq!(effect, NodeUpdateEffect { changed: true, structure: true, content: false, data: false });

    // A child list change.
    let sv = b.state_vector();
    a.insert_child(0, id()).unwrap();
    let effect = b.apply_update(&a.diff_since(&sv).unwrap()).unwrap();
    assert_eq!(effect, NodeUpdateEffect { changed: true, structure: true, content: false, data: false });

    // A plugin's write.
    let sv = b.state_vector();
    a.set_data("k", &json!({"v": "text"})).unwrap();
    let diff = a.diff_since(&sv).unwrap();
    let effect = b.apply_update(&diff).unwrap();
    assert_eq!(effect, NodeUpdateEffect { changed: true, structure: false, content: false, data: true });

    // The same bytes again change nothing and name no root.
    assert_eq!(b.apply_update(&diff).unwrap(), NodeUpdateEffect::default());
    assert_eq!(b.apply_update(&delta).unwrap(), NodeUpdateEffect::default());

    // A deletion only (the tombstone cleared) is a structural change too.
    a.set_deleted(Some(T1)).unwrap();
    let sv = b.state_vector();
    b.apply_update(&a.diff_since(&sv).unwrap()).unwrap();
    let sv = b.state_vector();
    a.set_deleted(None).unwrap();
    let effect = b.apply_update(&a.diff_since(&sv).unwrap()).unwrap();
    assert!(effect.changed && effect.structure && !effect.content && !effect.data, "{effect:?}");
    assert_eq!(b.fields().unwrap().deleted_at, None);
}

/// A phase 2a content document is a node document without a `node` root:
/// the migration loads it, reads the text, and `init`s it.
#[test]
fn a_phase_2a_content_document_loads_uninitialised_with_its_text() {
    let content = ContentDoc::from_plain_text("hello\nworld").unwrap();
    let mut doc = NodeDoc::load(&content.save()).unwrap();
    assert!(!doc.is_initialised());
    assert_eq!(doc.text(), "hello\nworld");
    assert_eq!(doc.units().len(), 2);
    assert_eq!(NodeDoc::units_of(&content.save()).len(), 2);
    doc.init("document", "Hello", Some(id()), T0).unwrap();
    assert!(doc.is_initialised());
    assert_eq!(doc.text(), "hello\nworld");
    // And back the other way: the content document reads the same text.
    assert_eq!(ContentDoc::text_of(&doc.save()), "hello\nworld");
}

/// The contract's claim, proven against the crate: rinch-editor-collab
/// tolerates roots it does not own, so the editor joins a node document as
/// it joined a content document, and Pimble's roots ride along untouched.
#[test]
fn a_node_document_still_loads_as_a_rinch_collab_doc() {
    use rinch_editor_collab::{CollabDoc, CollabSession};
    use rinch_editor_core::Schema;

    let mut doc = NodeDoc::from_plain_text("hello\nworld").unwrap();
    doc.init("document", "Hello", Some(id()), T0).unwrap();
    doc.set_tags(&["t".into()], T0).unwrap();
    doc.set_custom("k", &json!(1), T0).unwrap();
    doc.insert_child(0, id()).unwrap();
    doc.set_data("plugin", &json!({"a": [1, "b"]})).unwrap();

    let bytes = doc.save();
    CollabDoc::load(&bytes).expect("CollabDoc::load checks only its own roots");
    let session = CollabSession::from_bytes(&bytes).unwrap();
    let projected = session.projected_doc(&Schema::starter_kit()).unwrap();
    assert_eq!(projected.child_count(), 2);
    assert_eq!(projected.child(0).child(0).text(), Some("hello"));
    assert_eq!(projected.child(1).child(0).text(), Some("world"));

    // The editor's snapshot of the shared document still has everything.
    let back = NodeDoc::load(&session.snapshot()).unwrap();
    assert_eq!(back.fields().unwrap().title, "Hello");
    assert_eq!(back.children().len(), 1);
    assert_eq!(back.data_json(), json!({"plugin": {"a": [1, "b"]}}));
}

#[test]
fn replace_plain_text_seeds_an_initialised_document_and_then_edits_it() {
    let mut doc = NodeDoc::new();
    doc.init("document", "Doc", None, T0).unwrap();
    let mut peer = NodeDoc::load(&doc.save()).unwrap();
    assert_eq!(doc.text(), "");

    // No projection yet: the delta is a fresh snapshot.
    let delta = doc.replace_plain_text("first").unwrap();
    assert!(peer.apply_update(&delta).unwrap().content);
    assert_eq!(doc.text(), "first");
    assert_eq!(peer.text(), "first");
    assert!(doc.is_initialised() && peer.is_initialised());

    // Now an edit of the shared history: peers converge on exactly the new text.
    let delta = doc.replace_plain_text("second\nthird").unwrap();
    peer.apply_update(&delta).unwrap();
    assert_eq!(doc.text(), "second\nthird");
    assert_eq!(peer.text(), "second\nthird");
    assert_eq!(peer.fields().unwrap().title, "Doc");
}

#[test]
fn data_json_round_trips_nested_values() {
    let value = json!({
        "string": "text",
        "int": 42,
        "negative": -7,
        "float": 2.5,
        "yes": true,
        "no": false,
        "nothing": null,
        "list": [1, "two", [3.25, false], {"deep": null}],
        "object": {"a": {"b": {"c": "d"}}, "empty": {}, "none": []},
    });
    let mut doc = NodeDoc::new();
    doc.set_data("v", &value).unwrap();
    assert_eq!(doc.data_json(), json!({"v": value}));
    let loaded = NodeDoc::load(&doc.save()).unwrap();
    assert_eq!(loaded.data_json(), json!({"v": value}));
    // A key is replaced whole.
    let mut loaded = loaded;
    loaded.set_data("v", &json!("flat")).unwrap();
    assert_eq!(loaded.data_json(), json!({"v": "flat"}));
}

#[test]
fn two_documents_setting_different_data_keys_merge_to_both() {
    let mut a = NodeDoc::new();
    a.init("plugin", "P", None, T0).unwrap();
    let mut b = NodeDoc::load(&a.save()).unwrap();
    a.set_data("left", &json!({"x": 1})).unwrap();
    b.set_data("right", &json!(["y"])).unwrap();
    let (a_sv, b_sv) = (a.state_vector(), b.state_vector());
    let to_b = a.diff_since(&b_sv).unwrap();
    let to_a = b.diff_since(&a_sv).unwrap();
    assert!(b.apply_update(&to_b).unwrap().data);
    assert!(a.apply_update(&to_a).unwrap().data);
    let merged = json!({"left": {"x": 1}, "right": ["y"]});
    assert_eq!(a.data_json(), merged);
    assert_eq!(b.data_json(), merged);
}

#[test]
fn edits_inside_data_touch_only_the_place_they_name_and_merge() {
    use super::{DataEdit::*, DataStep::*};
    let mut a = NodeDoc::new();
    a.init("plugin", "P", None, T0).unwrap();
    a.set_data("cal", &json!({"name": "Shire", "year": [{"month": "Afteryule", "days": 30}, {"day": "Lithe"}]})).unwrap();
    let mut b = NodeDoc::load(&a.save()).unwrap();
    // Two people change different parts at once: a name, and a month's length.
    assert!(a.edit_data(&[Key("cal"), Key("name")], Set(&json!("Shire Reckoning"))).unwrap());
    assert!(b.edit_data(&[Key("cal"), Key("year"), Index(0), Key("days")], Set(&json!(31))).unwrap());
    // Another inserts a month while one removes the day of no month.
    assert!(a.edit_data(&[Key("cal"), Key("year"), Index(1)], Insert(&json!({"month": "Solmath", "days": 30}))).unwrap());
    assert!(b.edit_data(&[Key("cal"), Key("year"), Index(1)], Remove).unwrap());
    let (a_sv, b_sv) = (a.state_vector(), b.state_vector());
    let (to_b, to_a) = (a.diff_since(&b_sv).unwrap(), b.diff_since(&a_sv).unwrap());
    b.apply_update(&to_b).unwrap();
    a.apply_update(&to_a).unwrap();
    let merged = json!({"cal": {"name": "Shire Reckoning", "year": [{"month": "Afteryule", "days": 31}, {"month": "Solmath", "days": 30}]}});
    assert_eq!(a.data_json(), merged);
    assert_eq!(b.data_json(), merged);
    // Setting what is there writes nothing; a place that is not there is refused.
    assert!(!a.edit_data(&[Key("cal"), Key("name")], Set(&json!("Shire Reckoning"))).unwrap());
    let refused = a.edit_data(&[Key("cal"), Key("eras"), Index(0), Key("name")], Set(&json!("x"))).unwrap_err();
    assert_eq!(refused.to_string(), "Nothing in this node's data is at cal.eras[0].name.");
    assert!(a.edit_data(&[Key("cal"), Key("year"), Index(9)], Remove).is_err());
    assert!(a.edit_data(&[Key("cal"), Key("name")], Remove).unwrap());
    assert_eq!(a.data_json()["cal"].get("name"), None);
}

#[test]
fn diff_if_peer_lacks_it_is_none_once_both_sides_have_exchanged() {
    let mut local = NodeDoc::from_plain_text("hello").unwrap();
    local.init("document", "Doc", None, T0).unwrap();
    let remote = NodeDoc::load(&local.save()).unwrap();
    let remote_diff = remote.diff_since(&local.state_vector()).unwrap();
    assert!(local.diff_if_peer_lacks_it(&remote.state_vector(), &remote_diff).unwrap().is_none());

    // A structural deletion the peer lacks is pushed.
    let mut local = local;
    local.set_deleted(Some(T1)).unwrap();
    local.set_deleted(None).unwrap();
    let remote_diff = remote.diff_since(&local.state_vector()).unwrap();
    assert!(local.diff_if_peer_lacks_it(&remote.state_vector(), &remote_diff).unwrap().is_some());
}

/// A node card on a board is a link (docs/IMAGE_MARKUP_CONTRACT.md
/// section 4), so the boards that point at a node count in its backlinks.
#[test]
fn a_board_node_card_is_a_link() {
    let mut doc = NodeDoc::new();
    doc.init("board", "Board", None, T0).unwrap();
    let target = pimble_core::PimbleUrl { store: pimble_core::StoreId::new(), node: id(), anchor: None };
    {
        let data = doc.data_root();
        let mut txn = doc.ydoc().transact_mut();
        let board: MapRef = data.insert(&mut txn, "board", MapPrelim::default());
        let elements: MapRef = board.insert(&mut txn, "elements", MapPrelim::default());
        let card: MapRef = elements.insert(&mut txn, "c1", MapPrelim::default());
        card.insert(&mut txn, "kind", "card");
        let geom: HashMap<String, Any> =
            [("link".to_string(), Any::String(target.to_string().into())), ("x".to_string(), Any::Number(0.0))].into();
        card.insert(&mut txn, "geom", Any::Map(geom.into()));
        card.insert(&mut txn, "text", TextPrelim::new("Target"));
        let pen: MapRef = elements.insert(&mut txn, "p1", MapPrelim::default());
        pen.insert(&mut txn, "kind", "pen");
    }
    let links = NodeDoc::links_of(&doc.save());
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target, target);
    assert_eq!(links[0].path, "f:board/c1");
    assert_eq!(links[0].text, "Target");
}

/// An agent's caret (docs/PRESENCE_CONTRACT.md section 8) is at the end of
/// what it wrote: after an appended paragraph, after replaced words, and
/// where words were removed; and it stays there through a later edit
/// before it.
#[test]
fn the_caret_after_an_edit_is_at_the_end_of_what_it_wrote() {
    let at = |doc: &NodeDoc, sticky: Vec<u8>| match doc.resolve_anchor(&Anchor::new(Some(sticky), "")) {
        Some(Resolved::Sticky(spot)) => (spot.textblock, spot.offset),
        other => panic!("{other:?}"),
    };
    let mut doc = NodeDoc::from_plain_text("one\ntwo").unwrap();
    let before = doc.textblock_texts();
    doc.append_markdown("three more").unwrap();
    let appended = doc.sticky_after_change(&before).unwrap();
    assert_eq!(at(&doc, appended.clone()), (2, 10));

    let before = doc.textblock_texts();
    doc.replace_text("two", "a second line").unwrap();
    assert_eq!(at(&doc, doc.sticky_after_change(&before).unwrap()), (1, 13));

    let before = doc.textblock_texts();
    doc.replace_text("a second line", "a line").unwrap();
    assert_eq!(at(&doc, doc.sticky_after_change(&before).unwrap()), (1, 2));

    // Nothing changed in the text: no caret.
    doc.init("document", "Note", None, "2026-10-22T00:00:00Z").unwrap();
    let before = doc.textblock_texts();
    doc.set_title("Renamed", "2026-10-22T00:00:00Z").unwrap();
    assert_eq!(doc.sticky_after_change(&before), None);

    // Typing before the appended words leaves the caret after them.
    doc.replace_text("three", "now three").unwrap();
    assert_eq!(at(&doc, appended), (2, 14));
}
