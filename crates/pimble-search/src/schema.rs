//! Schema composition for the rhypedb index: the base schema (embedded from
//! `schema.rhype`), the `// semantic:` marker mechanism that adds the `Chunk`
//! type (and `Node.chunks`) only when semantic search is wanted, and the
//! `SchemaFragment` seam a future structured node type extends through.

use sha2::{Digest, Sha256};

/// The name of the marker file `SearchIndex::open` writes into the index
/// directory, holding [`schema_hash`] of the schema text the directory was
/// built with. A caller that owns the directory's lifecycle (deciding when to
/// delete and rebuild it) compares this against the hash it would compose
/// today; a mismatch — including a missing file — means the on-disk index no
/// longer matches the schema this binary would create, and should be rebuilt.
pub const SCHEMA_HASH_FILE: &str = "schema_hash";

/// The embedded base schema, exactly as shipped in `schema.rhype`. Every
/// `// semantic: ` line is an inert comment here; [`compose_schema`] is the
/// only thing that ever transforms it.
const BASE_SCHEMA: &str = include_str!("../schema.rhype");

/// The exact text of the line inside `type Node { ... }` that
/// [`compose_schema`] replaces with a fragment's `node_fields`.
const FRAGMENT_MARKER: &str = "    // fragment-fields";

/// Extra schema a node-type plugin contributes: extra relationship fields on
/// `Node` and extra top-level type declarations those fields point at.
/// Nothing contributes one yet (no node type needs typed, filterable fields
/// beyond `text`/`title`) — the seam exists for a future `contact` or `task`
/// node type to add e.g. `type Contact { ... }` plus a `Node.contact: Contact`
/// relationship, without pimble-search itself changing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaFragment {
    /// Field declaration lines added inside `type Node { ... }`, e.g.
    /// `"contact: Contact"`. Indentation is added automatically.
    pub node_fields: Vec<String>,
    /// Complete top-level type declarations appended after the base schema,
    /// e.g. `"type Contact {\n    city: String\n}"`.
    pub extra_types: Vec<String>,
}

/// Compose the full schema text: the base schema, with the `Chunk` type and
/// `Node.chunks` included (marker lines uncommented) iff `semantic`, plus
/// every fragment's extra `Node` fields and extra type declarations.
///
/// Pure text composition — this does not touch the filesystem or parse
/// anything; pass the result to `rhypedb_schema::parser::parse_schema`.
pub fn compose_schema(semantic: bool, fragments: &[SchemaFragment]) -> String {
    let mut text = if semantic {
        BASE_SCHEMA
            .lines()
            .map(strip_semantic_marker)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        BASE_SCHEMA.to_string()
    };

    let node_fields: Vec<&str> = fragments
        .iter()
        .flat_map(|f| f.node_fields.iter().map(String::as_str))
        .collect();
    if node_fields.is_empty() {
        // Drop the marker line (and the newline right after it) entirely.
        text = text.replace(&format!("{FRAGMENT_MARKER}\n"), "");
        text = text.replace(FRAGMENT_MARKER, "");
    } else {
        let inserted = node_fields
            .iter()
            .map(|f| format!("    {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        text = text.replace(FRAGMENT_MARKER, &inserted);
    }

    for fragment in fragments {
        for extra in &fragment.extra_types {
            text.push_str("\n\n");
            text.push_str(extra);
        }
    }

    text
}

/// If `line` (after leading whitespace) starts with `// semantic:`, return it
/// with that marker (and one following space, if any) removed, keeping the
/// original indentation — turning a semantic-only comment into a live schema
/// line. Every other line is returned unchanged.
fn strip_semantic_marker(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    match trimmed.strip_prefix("// semantic:") {
        Some(rest) => format!("{indent}{}", rest.trim_start()),
        None => line.to_string(),
    }
}

/// A stable hex-encoded SHA-256 of composed schema text. Two calls to
/// [`compose_schema`] with the same arguments always hash equal; a change to
/// `schema.rhype`, to `semantic`, or to the fragment set always hashes
/// different.
pub fn schema_hash(schema_text: &str) -> String {
    hex_encode(&Sha256::digest(schema_text.as_bytes()))
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_only_schema_has_no_chunk_type() {
        let text = compose_schema(false, &[]);
        let schema = rhypedb_schema::parser::parse_schema(&text).expect("keyword-only schema should parse");
        assert!(!schema.types.contains_key("Chunk"));
        assert!(!schema.get_type("Node").unwrap().fields.iter().any(|f| f.name == "chunks"));
    }

    #[test]
    fn semantic_schema_has_chunk_type_and_no_marker_left_behind() {
        let text = compose_schema(true, &[]);
        // No line is a leftover, un-uncommented marker (the header docs above
        // and elsewhere are free to mention the marker syntax in prose).
        assert!(
            !text.lines().any(|l| l.trim_start().starts_with("// semantic:")),
            "a `// semantic:` marker line survived composition:\n{text}"
        );
        let schema = rhypedb_schema::parser::parse_schema(&text).expect("semantic schema should parse");
        assert!(schema.types.contains_key("Chunk"));
        assert!(schema.get_type("Node").unwrap().fields.iter().any(|f| f.name == "chunks"));
    }

    #[test]
    fn fragment_adds_node_field_and_extra_type() {
        let fragment = SchemaFragment {
            node_fields: vec!["contact: Contact".to_string()],
            extra_types: vec!["type Contact {\n    city: String\n}".to_string()],
        };
        let text = compose_schema(false, std::slice::from_ref(&fragment));
        assert!(
            !text.lines().any(|l| l.trim() == "// fragment-fields"),
            "the fragment-fields marker line survived composition:\n{text}"
        );
        let schema = rhypedb_schema::parser::parse_schema(&text).expect("schema with fragment should parse");
        assert!(schema.get_type("Node").unwrap().fields.iter().any(|f| f.name == "contact"));
        assert!(schema.types.contains_key("Contact"));
    }

    #[test]
    fn no_fragments_leaves_no_marker_residue() {
        let text = compose_schema(false, &[]);
        assert!(!text.lines().any(|l| l.trim() == "// fragment-fields"));
    }

    #[test]
    fn schema_hash_is_stable_and_sensitive_to_semantic_flag() {
        let a = schema_hash(&compose_schema(false, &[]));
        let b = schema_hash(&compose_schema(false, &[]));
        let c = schema_hash(&compose_schema(true, &[]));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
