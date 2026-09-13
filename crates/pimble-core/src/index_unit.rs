//! [`IndexUnit`]: the search index's own content model.
//!
//! A node's content producer (a document's block projection, or a plugin's
//! `index_units`) breaks the node down into a list of these before the search
//! index chunks and embeds them. The type is decoupled from any particular
//! editor schema — `pimble-crdt`'s rich-text blocks map onto it today, but a
//! future table or structured node type can produce the same units without a
//! new dependency between crates.

use serde::{Deserialize, Serialize};

/// One chunk-able piece of a node's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexUnit {
    pub kind: UnitKind,
    /// Locator inside the node, for deep links and highlighting: a block
    /// ordinal (`"b:12"`), a table cell (`"b:12/r:3/c:1"`), or a field path
    /// (`"f:address.city"`).
    pub path: String,
    pub text: String,
}

/// What kind of content a unit carries. Drives how the chunker groups units
/// and how a search hit is rendered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum UnitKind {
    /// A paragraph, quote, or list item.
    Prose,
    /// A section heading; sets the section context for units that follow.
    /// The level (h1 = 1, h2 = 2, ...).
    Heading(u8),
    /// A code block. Never sentence-split; chunked whole, oversize pieces
    /// split on line boundaries.
    Code,
    /// One row of a table. `text` is `"h1: v1 | h2: v2"`; a table's header
    /// row is repeated as context for every other row's chunk.
    TableRow,
    /// One field of structured data, named by `String`. `text` is the
    /// field's value in text form.
    Field(String),
    /// A block kind the producer doesn't otherwise recognize, indexed as
    /// prose. Carries the original block kind name.
    Other(String),
}

impl IndexUnit {
    pub fn new(kind: UnitKind, path: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.into(),
            text: text.into(),
        }
    }
}
