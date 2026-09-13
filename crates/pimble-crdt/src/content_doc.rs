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
use rinch_editor_core::{default_plugins, EditorState, Fragment, Node, Schema};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, OffsetKind, Options, ReadTxn, StateVector, Transact, Update};

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
    /// becomes an empty paragraph; empty `text` becomes a single empty paragraph). Used
    /// to migrate legacy plain-text content.
    pub fn from_plain_text(text: &str) -> Result<Self> {
        let schema = Rc::new(Schema::starter_kit());
        let mut paragraphs = Vec::new();
        for line in text.split('\n') {
            let content = if line.is_empty() {
                Fragment::empty()
            } else {
                let text_node = schema
                    .text(line)
                    .map_err(|e| CrdtError::Collab(e.to_string()))?;
                Fragment::from_node(text_node)
            };
            let paragraph = schema
                .branch("paragraph", content)
                .map_err(|e| CrdtError::Collab(e.to_string()))?;
            paragraphs.push(paragraph);
        }
        let doc_node = schema
            .branch("doc", Fragment::from_children(paragraphs))
            .map_err(|e| CrdtError::Collab(e.to_string()))?;
        let state = EditorState::create(schema.clone(), doc_node, default_plugins());
        let session = CollabSession::new(&state).map_err(|e| CrdtError::Collab(e.to_string()))?;
        Self::load(&session.snapshot())
    }

    /// Full snapshot: the v1 update encoding of the whole document from an empty state
    /// vector. Round-trips through [`ContentDoc::load`].
    pub fn save(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    /// Merge a peer's v1 update — a broadcast delta, a reconciliation diff, or a whole
    /// snapshot — into this document.
    pub fn apply_update(&mut self, update: &[u8]) -> Result<()> {
        let update = Update::decode_v1(update).map_err(|e| CrdtError::Yrs(e.to_string()))?;
        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|e| CrdtError::Yrs(e.to_string()))
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
    fn collect_leaf_text(node: &Node, out: &mut String) {
        if let Some(t) = node.text() {
            out.push_str(t);
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
}
