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

    /// Plain-text projection: blocks joined by `'\n'`. `""` if the document is empty or
    /// cannot be projected — never panics, logs at debug on failure.
    pub fn text(&self) -> String {
        let session = match CollabSession::from_bytes(&self.save()) {
            Ok(session) => session,
            Err(e) => {
                tracing::debug!("ContentDoc::text: CollabSession::from_bytes failed: {e}");
                return String::new();
            }
        };
        let schema = Schema::starter_kit();
        match session.projected_doc(&schema) {
            Ok(node) => Self::project_text(&node),
            Err(e) => {
                tracing::debug!("ContentDoc::text: projected_doc failed: {e}");
                String::new()
            }
        }
    }

    /// Convenience for callers holding bytes: [`ContentDoc::load`] then
    /// [`ContentDoc::text`], `""` on any error.
    pub fn text_of(bytes: &[u8]) -> String {
        Self::load(bytes).map(|d| d.text()).unwrap_or_default()
    }

    /// Join the top-level children of `doc` (its blocks) with `'\n'`, concatenating each
    /// block's own leaf text with no separator.
    fn project_text(doc: &Node) -> String {
        doc.content()
            .iter()
            .map(|block| {
                let mut s = String::new();
                Self::collect_leaf_text(block, &mut s);
                s
            })
            .collect::<Vec<_>>()
            .join("\n")
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
