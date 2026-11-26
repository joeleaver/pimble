//! Whether an update could place anything, read from its bytes without
//! merging it: what lets the store judge a node's placement
//! (docs/THEMES_CONTRACT.md section 3.1a) only for the updates that can
//! change it, so a keystroke in a long note is not judged on a copy of
//! the whole document.
//!
//! An update places something only through a struct it inserts directly
//! into the `node` root (a new `type` or `parent_id`) or into the
//! `children` root (a new list entry). Its deletes never do: a delete
//! takes a value or an entry away, and a map key's earlier values were
//! deleted when they were overwritten, so none comes back.
//!
//! Where a struct lands is in its header. One with neither origin names
//! its parent: a root by name, or a nested type by the id of the struct
//! that holds it. One with an origin lands where that origin is (its left
//! origin first, then its right, as yrs integrates it), and the origin is
//! another struct of the same update or one the document holds.
//! [`yrs::Update`] keeps its structs to itself, so the headers are read
//! here with yrs's own v1 decoder, exactly as `Update::decode_v1` reads
//! them, and the content with [`ItemContent::decode`]. Whatever cannot be
//! told (an origin neither here nor held, a chain of origins that loops)
//! counts as placing, so the judgement is skipped only when it would have
//! found nothing.

use std::collections::HashMap;

use yrs::block::{ItemContent, BLOCK_GC_REF_NUMBER, BLOCK_SKIP_REF_NUMBER, HAS_ORIGIN, HAS_PARENT_SUB, HAS_RIGHT_ORIGIN};
use yrs::encoding::read::{Cursor, Read};
use yrs::updates::decoder::{Decoder, DecoderV1};
use yrs::{Assoc, BranchID, ClientID, OffsetKind, ReadTxn, StickyIndex, Transact, ID};

use super::{NodeDoc, ROOT_CHILDREN, ROOT_NODE};
use crate::error::{CrdtError, Result};

/// Where a struct of an update says it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Parent {
    /// A root, by name.
    Root(String),
    /// A nested type: never the `node` root nor the `children` root.
    Nested,
    /// Wherever its origins are: the left one, else the right one.
    Beside(Option<ID>, Option<ID>),
}

/// One struct the update inserts.
#[derive(Debug)]
struct Inserted {
    clock: u32,
    len: u32,
    parent: Parent,
}

/// Where a struct lands, once resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lands {
    /// The `node` root or the `children` root.
    Placing,
    /// Anywhere else.
    Elsewhere,
}

fn yrs_error(e: impl std::fmt::Display) -> CrdtError {
    CrdtError::Yrs(e.to_string())
}

/// The structs `update` inserts, per client, in clock order: the header of
/// each, read as `Update::decode_v1` reads it (skips and garbage-collected
/// ranges insert nothing, and a struct of no length is no struct).
fn inserted_structs(update: &[u8]) -> Result<HashMap<ClientID, Vec<Inserted>>> {
    let mut decoder = DecoderV1::new(Cursor::new(update));
    let clients: u32 = decoder.read_var().map_err(yrs_error)?;
    let mut structs: HashMap<ClientID, Vec<Inserted>> = HashMap::new();
    for _ in 0..clients {
        let count: u32 = decoder.read_var().map_err(yrs_error)?;
        let client = decoder.read_client().map_err(yrs_error)?;
        let mut clock: u32 = decoder.read_var().map_err(yrs_error)?;
        let list = structs.entry(client).or_default();
        for _ in 0..count {
            let info = decoder.read_info().map_err(yrs_error)?;
            match info {
                BLOCK_SKIP_REF_NUMBER => clock += decoder.read_var::<u32>().map_err(yrs_error)?,
                BLOCK_GC_REF_NUMBER => clock += decoder.read_len().map_err(yrs_error)?,
                info => {
                    let origin = if info & HAS_ORIGIN != 0 { Some(decoder.read_left_id().map_err(yrs_error)?) } else { None };
                    let right = if info & HAS_RIGHT_ORIGIN != 0 { Some(decoder.read_right_id().map_err(yrs_error)?) } else { None };
                    let parent = if origin.is_none() && right.is_none() {
                        let parent = if decoder.read_parent_info().map_err(yrs_error)? {
                            Parent::Root(decoder.read_string().map_err(yrs_error)?.to_owned())
                        } else {
                            decoder.read_left_id().map_err(yrs_error)?;
                            Parent::Nested
                        };
                        if info & HAS_PARENT_SUB != 0 {
                            decoder.read_string().map_err(yrs_error)?;
                        }
                        parent
                    } else {
                        Parent::Beside(origin, right)
                    };
                    let len = ItemContent::decode(&mut decoder, info).map_err(yrs_error)?.len(OffsetKind::Utf16);
                    if len > 0 {
                        list.push(Inserted { clock, len, parent });
                        clock += len;
                    }
                }
            }
        }
    }
    Ok(structs)
}

/// The struct of `structs` that holds `id`, if the update has it.
fn struct_holding<'a>(structs: &'a HashMap<ClientID, Vec<Inserted>>, id: &ID) -> Option<&'a Inserted> {
    let list = structs.get(&id.client)?;
    let at = list.partition_point(|s| s.clock + s.len <= id.clock);
    list.get(at).filter(|s| s.clock <= id.clock)
}

impl NodeDoc {
    /// Whether merging `update` could place something in this document:
    /// write its `node` root (a new type or `parent_id` among what it
    /// holds) or add an entry to its `children` list. `false` only when no
    /// struct the update brings lands in either root, which a keystroke in
    /// the content is: the store judges a placement only when this says it
    /// may be one ([`placing`](self) has how it is read). A document with
    /// an update parked in it answers `true`, since merging could release
    /// what is parked too.
    pub fn may_place(&self, update: &[u8]) -> Result<bool> {
        if self.missing().is_some() {
            return Ok(true);
        }
        let structs = inserted_structs(update)?;
        let txn = self.doc.transact();
        let held = txn.state_vector();
        // Where a struct lands, by where its parent or its origins are. A
        // chain of origins inside the update is followed at most once per
        // struct it holds, so one that loops is "cannot be told".
        let bound: usize = structs.values().map(Vec::len).sum();
        let lands_at = |id: &ID| -> Option<Lands> {
            let mut id = *id;
            for _ in 0..=bound {
                match struct_holding(&structs, &id) {
                    Some(inserted) => match &inserted.parent {
                        Parent::Root(name) => return Some(root_lands(name)),
                        Parent::Nested => return Some(Lands::Elsewhere),
                        // Followed by its left origin (or its right when it
                        // has no left); a left that cannot be told is "cannot
                        // be told" here, though yrs might take the right.
                        Parent::Beside(left, right) => id = left.or(*right)?,
                    },
                    None => return held_lands(&txn, &id),
                }
            }
            None
        };
        for (client, list) in &structs {
            let known = held.get(client);
            for inserted in list {
                // What the document holds already is not merged again.
                if inserted.clock + inserted.len <= known {
                    continue;
                }
                let lands = match &inserted.parent {
                    Parent::Root(name) => Some(root_lands(name)),
                    Parent::Nested => Some(Lands::Elsewhere),
                    Parent::Beside(left, right) => left.and_then(|id| lands_at(&id)).or_else(|| right.and_then(|id| lands_at(&id))),
                };
                if lands != Some(Lands::Elsewhere) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn root_lands(name: &str) -> Lands {
    if name == ROOT_NODE || name == ROOT_CHILDREN {
        Lands::Placing
    } else {
        Lands::Elsewhere
    }
}

/// Where a struct the document holds is: the type it is in, as yrs finds
/// it for a sticky index on it. `None` for an id the document does not
/// hold, or one garbage-collected with the type it was in.
fn held_lands<T: ReadTxn>(txn: &T, id: &ID) -> Option<Lands> {
    let offset = StickyIndex::from_id(*id, Assoc::After).get_offset(txn)?;
    Some(match offset.branch.id() {
        BranchID::Root(name) => root_lands(&name),
        BranchID::Nested(_) => Lands::Elsewhere,
    })
}

#[cfg(test)]
mod tests {
    use pimble_core::NodeId;

    use super::*;

    const T0: &str = "2026-11-26T10:00:00Z";

    /// A peer's change of its copy.
    type Change = Box<dyn FnOnce(&mut NodeDoc)>;

    /// A node's document with a few paragraphs, and a peer's copy of it.
    fn note_and_peer() -> (NodeDoc, NodeDoc) {
        let mut doc = NodeDoc::from_plain_text("first paragraph\nsecond paragraph\nthird paragraph").unwrap();
        doc.init("document", "Note", Some(NodeId::new()), T0).unwrap();
        let peer = NodeDoc::load(&doc.save()).unwrap();
        (doc, peer)
    }

    /// What `change` makes `peer` write, as the update `doc` would merge.
    fn update_of(doc: &NodeDoc, peer: &mut NodeDoc, change: impl FnOnce(&mut NodeDoc)) -> Vec<u8> {
        let before = doc.state_vector();
        change(peer);
        peer.diff_since(&before).unwrap()
    }

    /// The fields a placement is judged by.
    fn placed(doc: &NodeDoc) -> (String, Option<NodeId>, Vec<NodeId>) {
        let fields = doc.stored_fields().unwrap();
        (fields.node_type, fields.parent_id, doc.children())
    }

    #[test]
    fn typing_in_the_content_places_nothing() {
        let (mut doc, mut peer) = note_and_peer();
        for (from, to) in [("second paragraph", "second paragraphx"), ("second paragraphx", "second paragraphxy"), ("first", "1st")] {
            let update = update_of(&doc, &mut peer, |p| {
                p.replace_text(from, to).unwrap();
            });
            assert!(!doc.may_place(&update).unwrap(), "an edit of {from:?}");
            let before = placed(&doc);
            doc.apply_update(&update).unwrap();
            assert_eq!(placed(&doc), before);
        }
        // Several edits in one update, each struct's origin in the one before.
        let update = update_of(&doc, &mut peer, |p| {
            p.replace_text("third paragraph", "third paragraph!").unwrap();
            p.replace_text("third paragraph!", "third paragraph!!").unwrap();
            p.append_markdown("a new paragraph").unwrap();
        });
        assert!(!doc.may_place(&update).unwrap());
    }

    #[test]
    fn what_holds_the_type_the_parent_or_the_list_may_place() {
        let (doc, peer) = note_and_peer();
        let changes: Vec<(&str, Change)> = vec![
            ("a new type", Box::new(|p| p.set_node_type("theme", T0).unwrap())),
            ("a new parent", Box::new(|p| p.set_parent_id(Some(NodeId::new()), T0).unwrap())),
            ("a list entry", Box::new(|p| p.insert_child(0, NodeId::new()).unwrap())),
            // Anything written straight into the `node` root counts: the key
            // of a struct with an origin is not in its header.
            ("a new title", Box::new(|p| p.set_title("Renamed", T0).unwrap())),
        ];
        for (what, change) in changes {
            let mut peer = NodeDoc::load(&peer.save()).unwrap();
            let update = update_of(&doc, &mut peer, change);
            assert!(doc.may_place(&update).unwrap(), "{what}");
        }
        // A document's very first `node` root names its parent by name.
        let mut fresh = NodeDoc::new();
        fresh.init("document", "New", Some(NodeId::new()), T0).unwrap();
        assert!(NodeDoc::new().may_place(&fresh.save()).unwrap());
    }

    #[test]
    fn what_is_held_already_or_cannot_be_told() {
        let (doc, mut peer) = note_and_peer();
        // A whole snapshot of what the document holds merges nothing.
        assert!(!doc.may_place(&doc.save()).unwrap());

        // An edit whose origin is an edit the document has not seen: where
        // it lands cannot be told, so it may place.
        let base = doc.state_vector();
        peer.replace_text("first paragraph", "first paragraph and").unwrap();
        let between = peer.state_vector();
        peer.replace_text("first paragraph and", "first paragraph and more").unwrap();
        let ahead = peer.diff_since(&between).unwrap();
        assert!(doc.may_place(&ahead).unwrap(), "an origin neither held nor in the update");

        // A document with an update parked in it: merging may release it.
        let mut parked = NodeDoc::load(&doc.save()).unwrap();
        parked.apply_update(&ahead).unwrap();
        assert!(parked.missing().is_some());
        let typing = peer.diff_since(&base).unwrap();
        assert!(parked.may_place(&typing).unwrap());
    }
}
