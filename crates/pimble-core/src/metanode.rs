//! Metanodes (docs/METANODES_CONTRACT.md): a node that says which nodes it
//! shows (its [`Source`]) and which viewer shows them (a plugin's id). A
//! metanode is any node with a viewer (open decision 1, taken as recommended): a
//! folder can become a kanban and go back to an outline with nothing lost.
//!
//! Both live on the node's own `node` root as plain keys, like fields
//! (`field:<name>`): no container is made on first use, so two devices
//! setting a metanode while apart each write one key and one of them wins, as
//! with any key. The viewer's own settings are the viewer's, under its id in
//! the node's `data`; nothing here reads them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The `node` root key naming the viewer (a plugin's id). Its presence is
/// what makes a node a metanode.
pub const VIEWER_KEY: &str = "metanode:viewer";
/// The `node` root key holding the [`Source`], as JSON text. Absent means
/// the node's children.
pub const SOURCE_KEY: &str = "metanode:source";

/// The deepest subtree a source reaches: a viewer showing more levels than
/// this is not one a person reads, and every walk of a forest stays bounded.
pub const MAX_SUBTREE_DEPTH: u32 = 16;

/// What a metanode stores: the viewer and the source. A node
/// carries one only when it has a viewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metanode {
    /// The viewer's id, as its plugin names it (`kanban`).
    pub viewer: String,
    #[serde(default)]
    pub source: Source,
}

impl Metanode {
    /// The metanode read from its two stored keys: `None` without a viewer.
    pub fn from_stored(viewer: Option<&str>, source: Option<&str>) -> Option<Self> {
        let viewer = viewer.map(str::trim).filter(|v| !v.is_empty())?;
        Some(Self { viewer: viewer.to_string(), source: source.map(Source::parse).unwrap_or_default() })
    }

    /// What the metanode is, for a person or an agent reading about the node:
    /// "a metanode: the children of Work, two levels, shown with the kanban
    /// viewer". `of` is the metanode's own label.
    pub fn describe(&self, of: &str) -> String {
        format!("a metanode: {}, shown with the {} viewer", self.source.describe(of), self.viewer)
    }
}

/// Which nodes a metanode shows (docs/METANODES_CONTRACT.md section 1): a query, and
/// in later waves filters and a limit on top. Stored as JSON text under
/// [`SOURCE_KEY`] as `{"query":{"kind":"subtree","depth":2}}`.
///
/// What a newer Pimble writes that this one does not know is kept rather
/// than refused: an unknown query reads as [`Query::Later`], and keys beside
/// `query` (filters, a limit) are kept in `later`. Either way the source is
/// evaluated as the node's children and [`Source::not_understood`] says so,
/// so an older app shows the fallback with a sentence instead of failing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub query: Query,
    /// Parts of a source this version does not know, kept as they were
    /// written so that rewriting the source loses nothing.
    #[serde(flatten)]
    pub later: BTreeMap<String, serde_json::Value>,
}

/// The query part of a [`Source`]. Tagged by `kind`, so later queries (a
/// tag, a search, backlinks, a field condition) are new kinds beside these.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Query {
    /// The metanode's children.
    #[default]
    Children,
    /// The metanode's subtree to `depth` levels: 1 is its children, 2 its
    /// children and theirs.
    Subtree { depth: u32 },
    /// A query a newer Pimble wrote. Read as the children.
    #[serde(other)]
    Later,
}

impl Source {
    /// The node's children: what a metanode with no source shows.
    pub fn children() -> Self {
        Self::default()
    }

    /// The node's subtree to `depth` levels (1 is its children).
    pub fn subtree(depth: u32) -> Self {
        Self { query: Query::Subtree { depth }, later: BTreeMap::new() }
    }

    /// The source stored as `raw`. Text that is not a source at all (a
    /// damaged key, a format from a much newer Pimble) reads as a query this
    /// version does not know, so it is shown as the children with a sentence.
    pub fn parse(raw: &str) -> Self {
        serde_json::from_str(raw).unwrap_or(Self { query: Query::Later, later: BTreeMap::new() })
    }

    /// The text stored under [`SOURCE_KEY`].
    pub fn to_stored(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// How many levels below the metanode the forest reaches, from 1 (the
    /// children) to [`MAX_SUBTREE_DEPTH`]. A source this version does not
    /// understand reaches its children.
    pub fn depth(&self) -> u32 {
        match self.query {
            Query::Subtree { depth } => depth.clamp(1, MAX_SUBTREE_DEPTH),
            Query::Children | Query::Later => 1,
        }
    }

    /// Why this source is shown other than as it was written, when it is: a
    /// newer Pimble wrote a part of it this one does not know.
    pub fn not_understood(&self) -> Option<&'static str> {
        (self.query == Query::Later || !self.later.is_empty()).then_some(NEWER_SOURCE)
    }

    /// The source in words, of the node labelled `of`: "the children of
    /// Work", "the children of Work, two levels".
    pub fn describe(&self, of: &str) -> String {
        match self.depth() {
            1 => format!("the children of {of}"),
            depth => format!("the children of {of}, {} levels", number_word(depth)),
        }
    }
}

/// What a metanode whose source a newer Pimble wrote says beside its
/// outline. A person reads "view": what they see is the node shown a way.
pub const NEWER_SOURCE: &str = "Part of this view's source needs a newer Pimble; it shows the children here.";

/// What a move or delete says when it takes nodes out of a metanode's source
/// (docs/METANODES_CONTRACT.md section 1.3), beside `plan`'s own warnings.
pub fn leaves_metanode_warning(count: usize) -> String {
    if count == 1 { "This won't be in this view any more.".to_string() } else { "These won't be in this view any more.".to_string() }
}

/// The line over a metanode's outline when this app has no such viewer
/// (docs/METANODES_CONTRACT.md section 1.4).
pub fn outline_line(viewer: &str) -> String {
    format!("Shown as an outline: the {viewer} viewer isn't installed.")
}

fn number_word(n: u32) -> String {
    const WORDS: [&str; 11] = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
    WORDS.get(n as usize).map(|w| w.to_string()).unwrap_or_else(|| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_round_trip_in_their_stored_shape() {
        assert_eq!(Source::children().to_stored(), r#"{"query":{"kind":"children"}}"#);
        assert_eq!(Source::subtree(2).to_stored(), r#"{"query":{"kind":"subtree","depth":2}}"#);
        for source in [Source::children(), Source::subtree(3)] {
            assert_eq!(Source::parse(&source.to_stored()), source);
        }
        assert_eq!(Source::parse("{}"), Source::children());
    }

    #[test]
    fn a_newer_query_reads_as_the_children_with_a_sentence() {
        let source = Source::parse(r#"{"query":{"kind":"tag","tag":"urgent"}}"#);
        assert_eq!(source.query, Query::Later);
        assert_eq!(source.depth(), 1);
        assert_eq!(source.not_understood(), Some(NEWER_SOURCE));
        // Garbage is not a failure either.
        assert_eq!(Source::parse("not json").not_understood(), Some(NEWER_SOURCE));
    }

    #[test]
    fn newer_parts_beside_the_query_are_kept_and_named() {
        let raw = r#"{"query":{"kind":"subtree","depth":2},"limit":{"count":20,"by":"modified"}}"#;
        let source = Source::parse(raw);
        assert_eq!(source.query, Query::Subtree { depth: 2 });
        assert_eq!(source.not_understood(), Some(NEWER_SOURCE));
        let again = Source::parse(&source.to_stored());
        assert_eq!(again, source, "rewriting a newer source loses nothing");
    }

    #[test]
    fn depth_is_bounded_and_described() {
        assert_eq!(Source::subtree(0).depth(), 1);
        assert_eq!(Source::subtree(1000).depth(), MAX_SUBTREE_DEPTH);
        assert_eq!(Source::children().describe("Work"), "the children of Work");
        assert_eq!(Source::subtree(2).describe("Work"), "the children of Work, two levels");
        let spec = Metanode::from_stored(Some("kanban"), Some(&Source::subtree(2).to_stored())).unwrap();
        assert_eq!(spec.describe("Work"), "a metanode: the children of Work, two levels, shown with the kanban viewer");
        assert!(Metanode::from_stored(None, Some("{}")).is_none());
        assert!(Metanode::from_stored(Some("  "), None).is_none());
        assert_eq!(Metanode::from_stored(Some("kanban"), None).unwrap().source, Source::children());
    }
}
