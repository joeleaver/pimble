//! Best-effort plain-text extraction from legacy Automerge node-content
//! documents (`nodes/{id}.automerge`), used to migrate a node's content to a
//! [`pimble_crdt::ContentDoc`] (yrs) the first time it is opened after the
//! restart.
//!
//! Real stores may contain content documents from any of three eras of
//! Pimble's Automerge experiment, so this module is deliberately permissive:
//! it never panics and never returns an error. Anything it cannot make sense
//! of is simply skipped, and a document it cannot parse at all yields an
//! empty string.
//!
//! Two shapes are recognized directly:
//! 1. The `DocumentContent` shape: a top-level `"text"` key holding a `Text`
//!    object or a string scalar.
//! 2. The `rinch-editor-core` M9 Automerge collab-snapshot shape: a
//!    top-level `"content"` list of block maps, each block either a direct
//!    `"text"` field or its own nested `"content"` list of inline "run"
//!    maps, each run a `"text"` field plus formatting metadata (`"marks"`,
//!    `"attrs"`) and a `"type"` tag. This was confirmed against every real
//!    legacy `.automerge` file available at the time this was written (577
//!    files across two stores): the only map keys ever observed are `text`,
//!    `content`, `type`, `attrs`, `marks`, and `href`, and the only `"type"`
//!    values are `paragraph`, `text`, `bold`, `italic`, `underline`, and
//!    `link` — there are no headings, lists, code blocks, or images to
//!    account for.
//!
//! Anything else falls back to a generic walk of the whole document, which
//! applies the same rule that makes shape 2 safe: a `Text` object's contents
//! always count, wherever it appears, but a string *scalar* only counts when
//! its map key is `"text"` — never a structural key like `"type"` or
//! `"href"` — so a block's `"type": "paragraph"` tag can never leak into the
//! extracted text.

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use tracing::debug;

/// Map keys that carry structural metadata, never user-visible text. A
/// scalar string under one of these keys (a block/run/mark type tag, a
/// formatting attribute, an identifier, ...) is never collected, and the
/// generic fallback walk never recurses into the value under one of these
/// keys either.
const STRUCTURAL_KEYS: &[&str] = &[
    "type",
    "node_type",
    "id",
    "kind",
    "attrs",
    "marks",
    "style",
    "level",
    "language",
    "href",
];

fn is_structural_key(key: &str) -> bool {
    STRUCTURAL_KEYS.contains(&key)
}

/// Extract the best-effort plain text from a legacy Automerge content
/// document's raw bytes.
///
/// Returns `""` if the bytes cannot be loaded as an Automerge document, or
/// if nothing text-like is found.
pub fn extract_text(bytes: &[u8]) -> String {
    let doc = match AutoCommit::load(bytes) {
        Ok(doc) => doc,
        Err(e) => {
            debug!("legacy: failed to load Automerge document: {}", e);
            return String::new();
        }
    };

    // Shape 1: DocumentContent's top-level "text".
    if let Some(text) = top_level_text(&doc) {
        if !text.is_empty() {
            return text;
        }
    }

    // Shape 2: a top-level "content" list of blocks.
    if let Ok(Some((Value::Object(ObjType::List), content_id))) = doc.get(automerge::ROOT, "content") {
        let blocks: Vec<String> = doc
            .list_range(&content_id, ..)
            .filter_map(|item| match item.value {
                Value::Object(ObjType::Map) | Value::Object(ObjType::Table) => {
                    Some(block_text(&doc, &item.id))
                }
                _ => None,
            })
            .collect();
        if !blocks.is_empty() {
            return blocks.join("\n");
        }
    }

    // Fallback: walk the whole document.
    let mut parts = Vec::new();
    walk(&doc, &automerge::ROOT, &mut parts);
    parts.join("\n")
}

/// Fast path: a top-level `"text"` key holding a `Text` object or string
/// scalar, matching the shape `DocumentContent` wrote.
fn top_level_text(doc: &AutoCommit) -> Option<String> {
    let (value, obj_id) = match doc.get(automerge::ROOT, "text") {
        Ok(Some(v)) => v,
        _ => return None,
    };
    text_value(doc, value, &obj_id)
}

/// Extract one block's text: its own direct `"text"` field if present,
/// otherwise its runs (a nested `"content"` list) concatenated with no
/// separator. Formatting metadata (`"marks"`, `"attrs"`, `"type"`) is never
/// inspected.
fn block_text(doc: &AutoCommit, block: &ObjId) -> String {
    if let Ok(Some((value, child_id))) = doc.get(block, "text") {
        if let Some(text) = text_value(doc, value, &child_id) {
            return text;
        }
    }

    if let Ok(Some((Value::Object(ObjType::List), content_id))) = doc.get(block, "content") {
        return doc
            .list_range(&content_id, ..)
            .map(|item| run_text(doc, item.value, &item.id))
            .collect::<Vec<_>>()
            .join("");
    }

    String::new()
}

/// Extract one inline run's text from its own `"text"` field, if the run is
/// a map that has one.
fn run_text(doc: &AutoCommit, value: Value, id: &ObjId) -> String {
    match value {
        Value::Object(ObjType::Map) | Value::Object(ObjType::Table) => {
            match doc.get(id, "text") {
                Ok(Some((v, child_id))) => text_value(doc, v, &child_id).unwrap_or_default(),
                _ => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// Resolve a `(Value, ObjId)` pair read from a `"text"` key into a plain
/// string: a `Text` object's contents, or a string scalar's contents.
fn text_value(doc: &AutoCommit, value: Value, obj_id: &ObjId) -> Option<String> {
    match value {
        Value::Object(ObjType::Text) => match doc.text(obj_id) {
            Ok(text) => Some(text),
            Err(e) => {
                debug!("legacy: failed to read text object: {}", e);
                None
            }
        },
        Value::Scalar(s) => match s.as_ref() {
            ScalarValue::Str(s) => Some(s.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Generic fallback walk over a map, list, table, or text object, in
/// document order. A `Text` object's contents always count, wherever it
/// appears. A string scalar counts only when its map key is `"text"`; list
/// elements have no key and so never contribute a bare scalar. Recursion
/// never descends into the value under a structural key (see
/// [`STRUCTURAL_KEYS`]).
fn walk(doc: &AutoCommit, obj: &ObjId, out: &mut Vec<String>) {
    match doc.object_type(obj) {
        Ok(ObjType::Map) | Ok(ObjType::Table) => {
            for item in doc.map_range(obj, ..) {
                if let Value::Object(ObjType::Text) = item.value {
                    if let Ok(text) = doc.text(&item.id) {
                        if !text.is_empty() {
                            out.push(text);
                        }
                    }
                    continue;
                }
                if is_structural_key(item.key) {
                    continue;
                }
                match item.value {
                    Value::Object(_) => walk(doc, &item.id, out),
                    Value::Scalar(s) => {
                        if item.key == "text" {
                            if let ScalarValue::Str(s) = s.as_ref() {
                                if !s.is_empty() {
                                    out.push(s.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(ObjType::List) => {
            for item in doc.list_range(obj, ..) {
                match item.value {
                    Value::Object(ObjType::Text) => {
                        if let Ok(text) = doc.text(&item.id) {
                            if !text.is_empty() {
                                out.push(text);
                            }
                        }
                    }
                    Value::Object(_) => walk(doc, &item.id, out),
                    // List elements have no map key, so a bare scalar never
                    // counts as text (matches the rule that only a "text"
                    // key does).
                    Value::Scalar(_) => {}
                }
            }
        }
        Ok(ObjType::Text) => {
            if let Ok(text) = doc.text(obj) {
                if !text.is_empty() {
                    out.push(text);
                }
            }
        }
        Err(e) => debug!("legacy: failed to read object during walk: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    #[test]
    fn top_level_text_object() {
        let mut doc = AutoCommit::new();
        let text_id = doc
            .put_object(automerge::ROOT, "text", ObjType::Text)
            .unwrap();
        doc.splice_text(&text_id, 0, 0, "Hello, World!").unwrap();
        let bytes = doc.save();

        assert_eq!(extract_text(&bytes), "Hello, World!");
    }

    #[test]
    fn top_level_text_string_scalar() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "text", "Just a string").unwrap();
        let bytes = doc.save();

        assert_eq!(extract_text(&bytes), "Just a string");
    }

    /// The real shape found in every sampled legacy store: a top-level
    /// `"content"` list of blocks, each block a `"type": "paragraph"` map
    /// with its own `"content"` list of inline "text"-type runs carrying
    /// `"marks"` and the actual `"text"` string. Structural tags must never
    /// leak into the extracted text, and blocks must be newline-separated
    /// while a block's own runs are not separated at all.
    #[test]
    fn collab_snapshot_shape_blocks_and_runs() {
        let mut doc = AutoCommit::new();
        let content = doc
            .put_object(automerge::ROOT, "content", ObjType::List)
            .unwrap();

        // Block 0: a single run.
        let block0 = doc.insert_object(&content, 0, ObjType::Map).unwrap();
        doc.put(&block0, "type", "paragraph").unwrap();
        doc.put(&block0, "attrs", "").ok(); // structural noise; harmless if ignored
        let block0_content = doc
            .put_object(&block0, "content", ObjType::List)
            .unwrap();
        let run0 = doc.insert_object(&block0_content, 0, ObjType::Map).unwrap();
        doc.put(&run0, "type", "text").unwrap();
        let marks0 = doc.put_object(&run0, "marks", ObjType::List).unwrap();
        let mark0 = doc.insert_object(&marks0, 0, ObjType::Map).unwrap();
        doc.put(&mark0, "type", "bold").unwrap();
        doc.put(&run0, "text", "Hello").unwrap();

        // Block 1: two runs concatenated with no separator, one of them a
        // link with an href attribute that must never leak into the text.
        let block1 = doc.insert_object(&content, 1, ObjType::Map).unwrap();
        doc.put(&block1, "type", "paragraph").unwrap();
        let block1_content = doc
            .put_object(&block1, "content", ObjType::List)
            .unwrap();
        let run1 = doc.insert_object(&block1_content, 0, ObjType::Map).unwrap();
        doc.put(&run1, "type", "text").unwrap();
        doc.put(&run1, "text", " ").unwrap();
        let run2 = doc.insert_object(&block1_content, 1, ObjType::Map).unwrap();
        doc.put(&run2, "type", "text").unwrap();
        let run2_marks = doc.put_object(&run2, "marks", ObjType::List).unwrap();
        let link_mark = doc.insert_object(&run2_marks, 0, ObjType::Map).unwrap();
        doc.put(&link_mark, "type", "link").unwrap();
        let link_attrs = doc.put_object(&link_mark, "attrs", ObjType::Map).unwrap();
        doc.put(&link_attrs, "href", "https://example.com/secret").unwrap();
        doc.put(&run2, "text", "example.com").unwrap();

        // Block 2: a direct "text" field on the block itself (the rarer
        // shape seen on some empty paragraphs), rather than a nested
        // "content" list of runs.
        let block2 = doc.insert_object(&content, 2, ObjType::Map).unwrap();
        doc.put(&block2, "type", "paragraph").unwrap();
        let block2_text = doc.put_object(&block2, "text", ObjType::Text).unwrap();
        doc.splice_text(&block2_text, 0, 0, "Direct block text").unwrap();

        let bytes = doc.save();
        let text = extract_text(&bytes);

        assert_eq!(text, "Hello\n example.com\nDirect block text");

        // Structural tags and attribute values must never appear.
        assert!(!text.contains("paragraph"));
        assert!(!text.contains("bold"));
        assert!(!text.contains("link"));
        assert!(!text.contains("https://example.com/secret"));
    }

    #[test]
    fn fallback_walk_never_collects_structural_keys() {
        let mut doc = AutoCommit::new();
        let blocks = doc
            .put_object(automerge::ROOT, "blocks", ObjType::List)
            .unwrap();
        let block0 = doc.insert_object(&blocks, 0, ObjType::Map).unwrap();
        // "kind" is a structural key even though it isn't one this
        // particular fallback shape was designed around.
        doc.put(&block0, "kind", "paragraph").unwrap();
        let block0_text = doc.put_object(&block0, "text", ObjType::Text).unwrap();
        doc.splice_text(&block0_text, 0, 0, "First paragraph").unwrap();

        let block1 = doc.insert_object(&blocks, 1, ObjType::Map).unwrap();
        let block1_text = doc.put_object(&block1, "text", ObjType::Text).unwrap();
        doc.splice_text(&block1_text, 0, 0, "Second paragraph").unwrap();

        let bytes = doc.save();
        let text = extract_text(&bytes);
        assert!(text.contains("First paragraph"));
        assert!(text.contains("Second paragraph"));
        // The structural "kind": "paragraph" tag must never appear on its
        // own — only as part of the real "paragraph" text above.
        assert_eq!(text.matches("paragraph").count(), 2);
    }

    #[test]
    fn garbage_bytes_yield_empty_string() {
        assert_eq!(extract_text(b"not an automerge document"), "");
        assert_eq!(extract_text(&[]), "");
    }

    #[test]
    fn empty_document_yields_empty_string() {
        let mut doc = AutoCommit::new();
        let bytes = doc.save();
        assert_eq!(extract_text(&bytes), "");
    }
}
