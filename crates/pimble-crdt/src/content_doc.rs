//! Per-node content documents, CRDT-backed with [`yrs`].
//!
//! `ContentDoc` wraps a `yrs::Doc` built with UTF-16 offsets (matching Yjs/rinch
//! semantics) and exposes exactly the primitives the rest of Pimble needs: save/load a
//! full snapshot, apply an incremental update, compute a state vector and a diff for the
//! sync protocol, and read a plain-text projection for tree labels and search.
//!
//! The document's internal shape — a `rinch-editor-collab` projection of a
//! `rinch-editor-core` editor document — is opaque to this type everywhere except
//! [`ContentDoc::from_plain_text`] and [`ContentDoc::text`], the only two operations
//! that need to know what is inside. Everything else operates on raw yrs bytes and
//! updates, agnostic to what they encode.

use std::rc::Rc;

use pimble_core::{IndexUnit, UnitKind};
use rinch_editor_collab::CollabSession;
use rinch_editor_core::{default_plugins, EditorState, Node, Schema};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, OffsetKind, Options, ReadTxn, StateVector, Transact, Update};

use crate::blocks::{blocks_from_plain_text, build_doc, Block};
use crate::error::{CrdtError, Result};

/// A node's rich-text content, backed by a yrs CRDT document.
pub struct ContentDoc {
    doc: Doc,
}

impl ContentDoc {
    /// An empty document.
    pub fn new() -> Self {
        let options = Options {
            offset_kind: OffsetKind::Utf16,
            ..Default::default()
        };
        Self {
            doc: Doc::with_options(options),
        }
    }

    /// Load a document from `bytes`, a yrs v1 update (a full snapshot or any update).
    /// Empty bytes produce an empty document, same as [`ContentDoc::new`].
    pub fn load(bytes: &[u8]) -> Result<Self> {
        let content = Self::new();
        if bytes.is_empty() {
            return Ok(content);
        }
        let update = Update::decode_v1(bytes).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        content
            .doc
            .transact_mut()
            .apply_update(update)
            .map_err(|e| CrdtError::Yrs(e.to_string()))?;
        Ok(content)
    }

    /// Build a document whose content is one paragraph per line of `text` (a blank line
    /// becomes an empty paragraph; empty `text` becomes a single empty paragraph).
    pub fn from_plain_text(text: &str) -> Result<Self> {
        Self::from_blocks(&blocks_from_plain_text(text))
    }

    /// Build a document from rich [`Block`]s (paragraphs, headings, code blocks, nested
    /// lists, marked runs): the importer's and the CLI's way in. Every block kind here
    /// is inside rinch's collaboration scope, so the result always projects.
    pub fn from_blocks(blocks: &[Block]) -> Result<Self> {
        let schema = Rc::new(Schema::starter_kit());
        let doc_node = build_doc(&schema, blocks)?;
        let state = EditorState::create(schema.clone(), doc_node, default_plugins());
        let session = CollabSession::new(&state).map_err(|e| CrdtError::Collab(e.to_string()))?;
        Self::load(&session.snapshot())
    }

    /// Replace this document's whole content with one paragraph per line of `text`, as
    /// an *edit* of the existing CRDT rather than a new document, and return the yrs v1
    /// delta that edit produced — the payload for an `applyEdit`. Peers that merge the
    /// delta converge on exactly `text`; a fresh document built from `text` would
    /// instead merge alongside the old content as extra paragraphs, since it shares no
    /// history with it. A document with no operations yet (a node whose content was
    /// never written) is seeded the way [`ContentDoc::from_plain_text`] builds one, and
    /// the delta is that whole snapshot.
    pub fn replace_plain_text(&mut self, text: &str) -> Result<Vec<u8>> {
        let schema = Rc::new(Schema::starter_kit());
        let after = build_doc(&schema, &blocks_from_plain_text(text))?;

        let has_history = !self.doc.transact().state_vector().is_empty();
        let delta = if has_history {
            let mut session = CollabSession::from_bytes(&self.save()).map_err(|e| CrdtError::Collab(e.to_string()))?;
            let before = session.projected_doc(&schema).map_err(|e| CrdtError::Collab(e.to_string()))?;
            session
                .record_local(&schema, &before, &after)
                .map_err(|e| CrdtError::Collab(e.to_string()))?;
            session.save_incremental().map_err(|e| CrdtError::Collab(e.to_string()))?
        } else {
            let state = EditorState::create(schema.clone(), after, default_plugins());
            let session = CollabSession::new(&state).map_err(|e| CrdtError::Collab(e.to_string()))?;
            session.snapshot()
        };

        self.apply_update(&delta)?;
        Ok(delta)
    }

    /// Full snapshot: the v1 update encoding of the whole document from an empty state
    /// vector. Round-trips through [`ContentDoc::load`].
    pub fn save(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    /// Merge a peer's v1 update — a broadcast delta, a reconciliation diff, or a whole
    /// snapshot — into this document. Returns whether it changed anything (decision 8 of
    /// docs/history/HARDENING_CONTRACT.md): `false` when every part of `update` was already
    /// reflected in this document. Computed from the merging transaction's own
    /// before/after state and delete set — never from `update`'s byte length, since a
    /// yrs v1 update is never actually empty (see [`crate::sync_util`]).
    pub fn apply_update(&mut self, update: &[u8]) -> Result<bool> {
        let update = Update::decode_v1(update).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        let mut txn = self.doc.transact_mut();
        txn.apply_update(update).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        Ok(*txn.before_state() != *txn.after_state() || !txn.delete_set().is_empty())
    }

    /// This document's state vector, v1-encoded.
    pub fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    /// Everything this document has that a peer at `state_vector` (v1-encoded) lacks.
    pub fn diff_since(&self, state_vector: &[u8]) -> Result<Vec<u8>> {
        let sv = StateVector::decode_v1(state_vector).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        Ok(self.doc.transact().encode_diff_v1(&sv))
    }

    /// Computes [`ContentDoc::diff_since`] against `remote_sv` and returns it only if it
    /// tells the peer something it doesn't already know (docs/history/HARDENING_CONTRACT.md
    /// decision 7): a struct beyond `remote_sv`, or one of our deletions missing from
    /// `remote_diff`'s delete set. `remote_diff` is the diff the peer just sent us (from
    /// its own `diff_since` against our last-known state) — decoded here only for its
    /// delete set, never applied. `None` when the peer already has everything, which a
    /// bare `diff_since(remote_sv).is_empty()` check can never detect (see
    /// [`crate::sync_util`]).
    pub fn diff_if_peer_lacks_it(&self, remote_sv: &[u8], remote_diff: &[u8]) -> Result<Option<Vec<u8>>> {
        let local_diff = self.diff_since(remote_sv)?;
        if crate::sync_util::peer_lacks_something(&local_diff, remote_diff)? {
            Ok(Some(local_diff))
        } else {
            Ok(None)
        }
    }

    /// Plain-text projection: the units' text (see [`ContentDoc::units`]) joined by
    /// `'\n'`. `""` if the document is empty or cannot be projected — never panics,
    /// logs at debug on failure.
    pub fn text(&self) -> String {
        self.units()
            .iter()
            .map(|u| u.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Convenience for callers holding bytes: [`ContentDoc::load`] then
    /// [`ContentDoc::text`], `""` on any error.
    pub fn text_of(bytes: &[u8]) -> String {
        Self::load(bytes).map(|d| d.text()).unwrap_or_default()
    }

    /// Index units: one per top-level block, in document order, for the search
    /// index's chunker. Maps `paragraph` to [`UnitKind::Prose`], `heading` to
    /// [`UnitKind::Heading`] (reading the `level` attribute, default 1), `code_block`
    /// to [`UnitKind::Code`], and any other block kind through as
    /// [`UnitKind::Other`] (so a future table or callout block indexes without a
    /// crate change). Each unit's `path` is `"b:{ordinal}"`. `[]` if the document is
    /// empty or cannot be projected — never panics, logs at debug on failure.
    pub fn units(&self) -> Vec<IndexUnit> {
        let session = match CollabSession::from_bytes(&self.save()) {
            Ok(session) => session,
            Err(e) => {
                tracing::debug!("ContentDoc::units: CollabSession::from_bytes failed: {e}");
                return Vec::new();
            }
        };
        let schema = Schema::starter_kit();
        match session.projected_doc(&schema) {
            Ok(node) => Self::project_units(&node),
            Err(e) => {
                tracing::debug!("ContentDoc::units: projected_doc failed: {e}");
                Vec::new()
            }
        }
    }

    /// Convenience for callers holding bytes: [`ContentDoc::load`] then
    /// [`ContentDoc::units`], `[]` on any error.
    pub fn units_of(bytes: &[u8]) -> Vec<IndexUnit> {
        Self::load(bytes).map(|d| d.units()).unwrap_or_default()
    }

    /// One [`IndexUnit`] per top-level child of `doc` (its blocks), each carrying
    /// that block's own leaf text (concatenated with no separator) and a `path` of
    /// `"b:{ordinal}"`.
    fn project_units(doc: &Node) -> Vec<IndexUnit> {
        doc.content()
            .iter()
            .enumerate()
            .map(|(ordinal, block)| {
                let mut text = String::new();
                Self::collect_leaf_text(block, &mut text);
                let kind = match block.type_name() {
                    "paragraph" => UnitKind::Prose,
                    "heading" => {
                        let level = block
                            .attrs()
                            .get_int("level")
                            .unwrap_or(1)
                            .clamp(1, u8::MAX as i64) as u8;
                        UnitKind::Heading(level)
                    }
                    "code_block" => UnitKind::Code,
                    other => UnitKind::Other(other.to_string()),
                };
                IndexUnit::new(kind, format!("b:{ordinal}"), text)
            })
            .collect()
    }

    /// Append every leaf text node under `node`, depth-first, in document order.
    /// The text under `node`, with one newline between text blocks (so a list's
    /// items read as lines, not as one run-on word) and none inside a block.
    fn collect_leaf_text(node: &Node, out: &mut String) {
        if let Some(t) = node.text() {
            out.push_str(t);
        } else if node.is_textblock() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for child in node.content().iter() {
                Self::collect_leaf_text(child, out);
            }
        } else {
            for child in node.content().iter() {
                Self::collect_leaf_text(child, out);
            }
        }
    }
}

impl Default for ContentDoc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rinch_editor_core::Fragment;

    /// A replacement is an edit of the shared history: a peer holding the old
    /// snapshot that merges the delta ends up with exactly the new text, not the
    /// old and new paragraphs side by side (which is what merging a document built
    /// from scratch produces).
    #[test]
    fn replace_plain_text_converges_on_the_new_text() {
        let mut doc = ContentDoc::from_plain_text("v1").unwrap();
        let mut peer = ContentDoc::load(&doc.save()).unwrap();

        let delta = doc.replace_plain_text("v2\nsecond line").unwrap();
        assert_eq!(doc.text(), "v2\nsecond line");

        assert!(peer.apply_update(&delta).unwrap());
        assert_eq!(peer.text(), "v2\nsecond line");

        // And again, with a different block count in the other direction.
        let delta = doc.replace_plain_text("v3").unwrap();
        peer.apply_update(&delta).unwrap();
        assert_eq!(doc.text(), "v3");
        assert_eq!(peer.text(), "v3");
    }

    /// A never-written document has no history to edit; the delta is the whole
    /// seeded snapshot and still leaves both sides on the text.
    #[test]
    fn replace_plain_text_seeds_an_empty_document() {
        let mut doc = ContentDoc::new();
        let mut peer = ContentDoc::new();
        let delta = doc.replace_plain_text("hello").unwrap();
        peer.apply_update(&delta).unwrap();
        assert_eq!(doc.text(), "hello");
        assert_eq!(peer.text(), "hello");
    }

    /// Rich blocks survive the trip into the CRDT and back out through the
    /// projection: headings keep their level, lists nest, marks stay on their
    /// runs, and a replica loading the bytes reads the same thing.
    #[test]
    fn rich_blocks_round_trip_through_the_projection() {
        use crate::blocks::{Align, Block, ListItem, Mark, Run};
        let blocks = vec![
            Block::Heading { level: 2, runs: vec![Run::plain("Title")] },
            Block::Paragraph {
                runs: vec![
                    Run::plain("plain "),
                    Run::marked("bold", vec![Mark::Bold]),
                    Run::marked(" link", vec![Mark::Link { href: "https://example.com".into() }]),
                    Run::marked(" red", vec![Mark::TextColor { color: "#ff0000".into() }]),
                ],
                align: Align::Center,
                indent: 1,
            },
            Block::BulletList {
                items: vec![
                    ListItem { blocks: vec![Block::plain("one")] },
                    ListItem {
                        blocks: vec![
                            Block::plain("two"),
                            Block::OrderedList { start: 3, items: vec![ListItem { blocks: vec![Block::plain("nested")] }] },
                        ],
                    },
                ],
            },
            Block::CodeBlock { text: "let x = 1;".into() },
        ];
        let doc = ContentDoc::from_blocks(&blocks).unwrap();
        let peer = ContentDoc::load(&doc.save()).unwrap();
        let units = peer.units();
        assert_eq!(units.len(), 4, "one unit per top-level block: {:?}", units);
        assert!(matches!(units[0].kind, UnitKind::Heading(2)));
        assert_eq!(units[0].text, "Title");
        assert_eq!(units[1].text, "plain bold link red");
        assert!(matches!(units[2].kind, UnitKind::Other(ref k) if k == "bullet_list"));
        assert_eq!(units[2].text.replace('\n', "|"), "one|two|nested".to_string(), "{:?}", units[2].text);
        assert!(matches!(units[3].kind, UnitKind::Code));

        // Marks and attrs came back, not just text.
        let session = CollabSession::from_bytes(&peer.save()).unwrap();
        let node = session.projected_doc(&Schema::starter_kit()).unwrap();
        let para = node.child(1);
        assert_eq!(para.attrs().get_str("text_align"), Some("center"));
        assert_eq!(para.attrs().get_int("indent"), Some(1));
        let names: Vec<String> = (0..para.child_count())
            .flat_map(|i| para.child(i).marks().iter().map(|m| m.type_name().to_string()).collect::<Vec<_>>())
            .collect();
        assert_eq!(names, vec!["bold", "link", "text_color"]);
    }

    #[test]
    fn empty_blocks_build_one_empty_paragraph() {
        let doc = ContentDoc::from_blocks(&[]).unwrap();
        assert_eq!(doc.units().len(), 1);
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn round_trip_plain_text() {
        let doc = ContentDoc::from_plain_text("a\nb").unwrap();
        let bytes = doc.save();
        let loaded = ContentDoc::load(&bytes).unwrap();
        assert_eq!(loaded.text(), "a\nb");
    }

    /// Build doc(heading[level=2]("Title"), code_block("fn f() {}"), custom_block("x"))
    /// directly through the schema builders (bypassing `from_plain_text`, which only
    /// produces paragraphs) to exercise every arm of `project_units`.
    fn mixed_blocks_doc() -> ContentDoc {
        use rinch_editor_core::Attrs;

        let schema = Rc::new(Schema::starter_kit());
        let heading = schema
            .create_node(
                "heading",
                Attrs::new().with("level", 2i64),
                Fragment::from_node(schema.text("Title").unwrap()),
            )
            .unwrap();
        let code = schema
            .create_node(
                "code_block",
                Attrs::new(),
                Fragment::from_node(schema.text("fn f() {}").unwrap()),
            )
            .unwrap();
        // A block kind `project_units` doesn't special-case. `bullet_list` is a
        // collab-supported list container (unlike e.g. `blockquote`, which
        // `CollabSession::new` below would reject outright), so it exercises the
        // `Other` arm without failing document construction.
        let list_item = schema
            .branch(
                "list_item",
                Fragment::from_node(
                    schema
                        .branch("paragraph", Fragment::from_node(schema.text("quoted").unwrap()))
                        .unwrap(),
                ),
            )
            .unwrap();
        let other = schema
            .branch("bullet_list", Fragment::from_node(list_item))
            .unwrap();
        let doc_node = schema
            .branch("doc", Fragment::from_children(vec![heading, code, other]))
            .unwrap();
        let state = EditorState::create(schema.clone(), doc_node, default_plugins());
        let session = CollabSession::new(&state).unwrap();
        ContentDoc::load(&session.snapshot()).unwrap()
    }

    #[test]
    fn units_map_heading_level_code_and_other() {
        let doc = mixed_blocks_doc();
        let units = doc.units();
        assert_eq!(units.len(), 3);

        assert_eq!(units[0].kind, UnitKind::Heading(2));
        assert_eq!(units[0].path, "b:0");
        assert_eq!(units[0].text, "Title");

        assert_eq!(units[1].kind, UnitKind::Code);
        assert_eq!(units[1].path, "b:1");
        assert_eq!(units[1].text, "fn f() {}");

        assert_eq!(units[2].kind, UnitKind::Other("bullet_list".to_string()));
        assert_eq!(units[2].path, "b:2");
        assert_eq!(units[2].text, "quoted");
    }

    #[test]
    fn text_is_units_joined_by_newline() {
        let doc = mixed_blocks_doc();
        assert_eq!(doc.text(), "Title\nfn f() {}\nquoted");
    }

    #[test]
    fn two_docs_converge_via_diff_exchange() {
        use yrs::{GetString, Text, Transact};

        let mut a = ContentDoc::new();
        let mut b = ContentDoc::new();

        // Diverge: each replica edits the same named text field independently, with
        // neither having seen the other's insertions yet.
        {
            let txt = a.doc.get_or_insert_text("scratch");
            let mut txn = a.doc.transact_mut();
            txt.insert(&mut txn, 0, "hello ");
        }
        {
            let txt = b.doc.get_or_insert_text("scratch");
            let mut txn = b.doc.transact_mut();
            txt.insert(&mut txn, 0, "world");
        }

        let a_sv = a.state_vector();
        let b_sv = b.state_vector();

        let diff_for_b = a.diff_since(&b_sv).unwrap();
        let diff_for_a = b.diff_since(&a_sv).unwrap();

        b.apply_update(&diff_for_b).unwrap();
        a.apply_update(&diff_for_a).unwrap();

        // Both replicas have now seen the same insertions; state vectors converge and
        // so does the merged text (yrs's own convergence, exercised through
        // ContentDoc's state-vector/diff/apply_update primitives).
        // Compare decoded state vectors: the v1 encoding iterates a HashMap, so
        // byte order is not stable across replicas even when the vectors are equal.
        assert_eq!(
            StateVector::decode_v1(&a.state_vector()).unwrap(),
            StateVector::decode_v1(&b.state_vector()).unwrap()
        );
        let a_text = a
            .doc
            .get_or_insert_text("scratch")
            .get_string(&a.doc.transact());
        let b_text = b
            .doc
            .get_or_insert_text("scratch")
            .get_string(&b.doc.transact());
        assert_eq!(a_text, b_text);
    }

    #[test]
    fn load_empty_is_empty() {
        let doc = ContentDoc::load(&[]).unwrap();
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn text_of_garbage_is_empty_string() {
        assert_eq!(ContentDoc::text_of(b"garbage"), "");
    }

    #[test]
    fn new_is_default_and_empty() {
        let doc = ContentDoc::default();
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn apply_update_reports_whether_it_changed_anything() {
        let a = ContentDoc::from_plain_text("hello").unwrap();
        let mut b = ContentDoc::new();

        let b_sv = b.state_vector();
        let diff = a.diff_since(&b_sv).unwrap();

        assert!(b.apply_update(&diff).unwrap(), "merging new content must report changed");
        assert!(
            !b.apply_update(&diff).unwrap(),
            "resending an already-merged update must report unchanged"
        );
    }

    #[test]
    fn diff_if_peer_lacks_it_is_none_once_both_sides_have_exchanged() {
        let local = ContentDoc::from_plain_text("hello").unwrap();
        let remote = ContentDoc::load(&local.save()).unwrap(); // remote already has everything

        let remote_sv = remote.state_vector();
        let local_sv = local.state_vector();
        // What remote would send local, from local's own state vector: since remote has
        // exactly what local has, this carries no new structs — just remote's (empty)
        // delete set.
        let remote_diff = remote.diff_since(&local_sv).unwrap();

        let push = local.diff_if_peer_lacks_it(&remote_sv, &remote_diff).unwrap();
        assert!(push.is_none(), "expected nothing to push, remote already has everything: {:?}", push);
    }

    #[test]
    fn diff_if_peer_lacks_it_is_some_when_a_struct_is_missing() {
        let a = ContentDoc::from_plain_text("hello").unwrap();
        let b = ContentDoc::new();

        let b_sv = b.state_vector();
        // b's diff to a (an empty document to an empty state) carries nothing.
        let diff_for_a = b.diff_since(&b_sv).unwrap();

        let push = a.diff_if_peer_lacks_it(&b_sv, &diff_for_a).unwrap();
        assert!(push.is_some(), "expected a's content to be pushed to empty b");
    }

    #[test]
    fn diff_if_peer_lacks_it_is_some_when_a_deletion_is_missing() {
        use yrs::{Text, Transact};

        let a = ContentDoc::new();
        {
            let txt = a.doc.get_or_insert_text("scratch");
            let mut txn = a.doc.transact_mut();
            txt.insert(&mut txn, 0, "hello");
        }
        let b = ContentDoc::load(&a.save()).unwrap();

        // b deletes content a still has.
        {
            let txt = b.doc.get_or_insert_text("scratch");
            let mut txn = b.doc.transact_mut();
            txt.remove_range(&mut txn, 0, 5);
        }

        // a's diff to b (from a's stale state vector) carries none of b's deletion.
        let a_sv = a.state_vector();
        let diff_for_b = a.diff_since(&a_sv).unwrap();

        let push = b.diff_if_peer_lacks_it(&a_sv, &diff_for_b).unwrap();
        assert!(push.is_some(), "expected b's deletion to be pushed back to a");
    }
}
