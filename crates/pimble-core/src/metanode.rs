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

use crate::{NodeId, StoreId};

/// The `node` root key naming the viewer (a plugin's id). Its presence is
/// what makes a node a metanode.
pub const VIEWER_KEY: &str = "metanode:viewer";
/// The `node` root key holding the [`Source`], as JSON text. Absent means
/// the node's children.
pub const SOURCE_KEY: &str = "metanode:source";

/// The deepest subtree a source reaches: a viewer showing more levels than
/// this is not one a person reads, and every walk of a forest stays bounded.
pub const MAX_SUBTREE_DEPTH: u32 = 16;

/// The viewer built into the app for a query metanode, a query metanode
/// (docs/QUERY_METANODES_CONTRACT.md section 5): its pane shows the setup.
pub const LIST_VIEWER: &str = "list";

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

    /// Whether it finds nodes beyond its own children (a query metanode,
    /// docs/QUERY_METANODES_CONTRACT.md): the explorer shows its finds, and
    /// it looks like one.
    pub fn is_query(&self) -> bool {
        self.source.is_match()
    }

    /// Whether its pane shows the query metanode setup ([`LIST_VIEWER`]).
    pub fn shows_setup(&self) -> bool {
        self.viewer.trim() == LIST_VIEWER
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

/// The query part of a [`Source`]. Tagged by `kind`, so later queries are
/// new kinds beside these.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Query {
    /// The metanode's children.
    #[default]
    Children,
    /// The metanode's subtree to `depth` levels: 1 is its children, 2 its
    /// children and theirs.
    Subtree { depth: u32 },
    /// The metanode's own children, then the nodes every condition in `all`
    /// holds for (docs/QUERY_METANODES_CONTRACT.md section 3), found live and
    /// never stored.
    Match {
        /// Where it looks; `None` is the metanode's own store.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        within: Option<Within>,
        #[serde(default)]
        all: Vec<Condition>,
        #[serde(default)]
        order: Order,
        #[serde(default)]
        descending: bool,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    /// A query a newer Pimble wrote. Read as the children.
    #[serde(other)]
    Later,
}

/// How many nodes a match finds when its source does not say.
pub const DEFAULT_LIMIT: u32 = 50;
/// The most a match finds, whatever its source says.
pub const MAX_LIMIT: u32 = 500;

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}

/// Where a match looks. On the wire: `{"store":"<id>"}`,
/// `{"under":"<store>/<node>"}`, `"everywhere"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Within {
    Store(StoreId),
    /// A node and everything under it, as `<store>/<node>` (a `pimble:`
    /// link reads too).
    Under(String),
    /// Every store open here.
    Everywhere,
}

/// One condition of a match. On the wire each is an object named by its key:
/// `{"tag":"waiting"}`, `{"no_tag":"done"}`, `{"text":"invoice"}`,
/// `{"field":"Due","is":"<","value":"Friday"}`, `{"links_to":"<store>/<node>"}`,
/// `{"type":"canvas"}`. One this version does not know is kept as
/// [`Condition::Later`], and the match then finds nothing (it would find
/// too much without it) and says so ([`Source::not_understood`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Condition {
    Field {
        field: String,
        is: FieldOp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
    Tag {
        tag: String,
    },
    NoTag {
        no_tag: String,
    },
    /// Every word of it in the node's title or text, any case.
    Text {
        text: String,
    },
    /// The node's content links to `<store>/<node>`.
    LinksTo {
        links_to: String,
    },
    /// What the node is: `document`, `canvas`, `sheet`, `discussion`,
    /// `kanban` (a metanode with the kanban viewer), or a node type.
    Type {
        #[serde(rename = "type")]
        node_type: String,
    },
    Later(serde_json::Value),
}

/// How a field condition compares, as `<`, `<=`, `=`, `>=`, `>`, `set`, `empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldOp {
    #[serde(rename = "<")]
    Less,
    #[serde(rename = "<=")]
    AtMost,
    #[serde(rename = "=")]
    Equal,
    #[serde(rename = ">=")]
    AtLeast,
    #[serde(rename = ">")]
    More,
    #[serde(rename = "set")]
    Set,
    #[serde(rename = "empty")]
    Empty,
}

/// What a match's finds are ordered by: `"title"`, `"modified"`, `"created"`,
/// `{"field":"<name>"}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    #[default]
    Title,
    Modified,
    Created,
    Field(String),
}

/// A store, or a node, a description names; [`Source::describe_named`] asks
/// its caller what each is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named<'a> {
    /// The metanode's own store.
    Here,
    Store(StoreId),
    /// A node as a source names it (`<store>/<node>`).
    Node(&'a str),
}

impl Named<'_> {
    /// A key for it, for a caller that looks names up in a map.
    pub fn key(&self) -> String {
        match self {
            Named::Here => "here".to_string(),
            Named::Store(store) => format!("store:{store}"),
            Named::Node(node) => format!("node:{}", node.trim()),
        }
    }
}

/// A node named as `<store>/<node>` or by its `pimble:` link.
pub fn parse_node_ref(raw: &str) -> Option<(StoreId, NodeId)> {
    let raw = raw.trim();
    if let Some(url) = crate::PimbleUrl::parse(raw) {
        return Some((url.store, url.node));
    }
    let (store, node) = raw.split_once('/')?;
    Some((StoreId::parse(store.trim()).ok()?, NodeId(uuid::Uuid::parse_str(node.trim()).ok()?)))
}

/// A node as a source names it: `<store>/<node>`.
pub fn node_ref(store: StoreId, node: NodeId) -> String {
    format!("{store}/{node}")
}

/// A tag as a condition names it, the way tags compare: no `#`, any case.
pub fn tag_key(tag: &str) -> String {
    tag.trim().trim_start_matches('#').trim().to_lowercase()
}

impl Condition {
    /// Whether the condition is still being filled in (its value is empty):
    /// it holds for nothing, so a query metanode finds nothing until it is
    /// written out.
    pub fn is_blank(&self) -> bool {
        match self {
            Condition::Tag { tag } => tag.trim().trim_start_matches('#').trim().is_empty(),
            Condition::NoTag { no_tag } => no_tag.trim().trim_start_matches('#').trim().is_empty(),
            Condition::Text { text } => text.trim().is_empty(),
            Condition::LinksTo { links_to } => links_to.trim().is_empty(),
            Condition::Type { node_type } => node_type.trim().is_empty(),
            Condition::Field { field, is, value } => {
                field.trim().is_empty() || (!matches!(is, FieldOp::Set | FieldOp::Empty) && value.as_deref().unwrap_or("").trim().is_empty())
            }
            Condition::Later(_) => false,
        }
    }

    /// The condition as a source says it: "tagged waiting".
    fn describe(&self, name: &dyn Fn(Named) -> Option<String>) -> String {
        match self {
            Condition::Tag { tag } => format!("tagged {}", tag.trim().trim_start_matches('#')),
            Condition::NoTag { no_tag } => format!("not tagged {}", no_tag.trim().trim_start_matches('#')),
            Condition::Text { text } => format!("containing \"{}\"", text.trim()),
            Condition::Field { field, is, value } => field_words(field, *is, value.as_deref()),
            Condition::LinksTo { links_to } => format!("linking to {}", name(Named::Node(links_to)).unwrap_or_else(|| links_to.clone())),
            Condition::Type { node_type } => format!("that are {}", type_plural(node_type)),
            Condition::Later(_) => "with a condition this Pimble doesn't know".to_string(),
        }
    }

    /// Why a found node is found, after "because": "it is tagged #waiting".
    fn reason(&self, name: &dyn Fn(Named) -> Option<String>) -> String {
        match self {
            Condition::Tag { tag } => format!("it is tagged #{}", tag.trim().trim_start_matches('#')),
            Condition::NoTag { no_tag } => format!("it isn't tagged #{}", no_tag.trim().trim_start_matches('#')),
            Condition::Text { text } => format!("it contains \"{}\"", text.trim()),
            Condition::Field { field, is, value } => format!("it has {}", field_words(field, *is, value.as_deref()).trim_start_matches("with ")),
            Condition::LinksTo { links_to } => format!("it links to {}", name(Named::Node(links_to)).unwrap_or_else(|| links_to.clone())),
            Condition::Type { node_type } => format!("it is {}", type_singular(node_type)),
            Condition::Later(_) => "of a condition this Pimble doesn't know".to_string(),
        }
    }
}

fn field_words(field: &str, is: FieldOp, value: Option<&str>) -> String {
    let field = field.trim();
    let value = value.unwrap_or("").trim();
    // A number compares as more or less, anything else (a date) as before or after.
    let numeric = value.trim_start_matches(['-', '$']).replace(',', "").parse::<f64>().is_ok();
    let op = match (is, numeric) {
        (FieldOp::Set, _) => return format!("with {field} set"),
        (FieldOp::Empty, _) => return format!("with no {field}"),
        (FieldOp::Equal, _) => "equal to",
        (FieldOp::Less, true) => "under",
        (FieldOp::AtMost, true) => "at most",
        (FieldOp::AtLeast, true) => "at least",
        (FieldOp::More, true) => "over",
        (FieldOp::Less, false) => "before",
        (FieldOp::AtMost, false) => "on or before",
        (FieldOp::AtLeast, false) => "on or after",
        (FieldOp::More, false) => "after",
    };
    format!("with {field} {op} {value}")
}

fn type_plural(t: &str) -> String {
    match t.trim() {
        "canvas" => "canvases".to_string(),
        "document" | "sheet" | "discussion" | "kanban" | "folder" => format!("{}s", t.trim()),
        other => format!("of type {other}"),
    }
}

fn type_singular(t: &str) -> String {
    match t.trim() {
        "canvas" | "document" | "sheet" | "discussion" | "kanban" | "folder" => format!("a {}", t.trim()),
        other => format!("of type {other}"),
    }
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
            Query::Children | Query::Match { .. } | Query::Later => 1,
        }
    }

    /// Why this source is shown other than as it was written, when it is: a
    /// newer Pimble wrote a part of it this one does not know.
    pub fn not_understood(&self) -> Option<&'static str> {
        let later_condition = matches!(&self.query, Query::Match { all, .. } if all.iter().any(|c| matches!(c, Condition::Later(_))));
        (self.query == Query::Later || later_condition || !self.later.is_empty()).then_some(NEWER_SOURCE)
    }

    /// A match of `all` in the metanode's own store, by title, at most
    /// [`DEFAULT_LIMIT`].
    pub fn matching(all: Vec<Condition>) -> Self {
        Self {
            query: Query::Match { within: None, all, order: Order::Title, descending: false, limit: DEFAULT_LIMIT },
            later: BTreeMap::new(),
        }
    }

    /// Whether this source finds nodes beyond the metanode's own children.
    pub fn is_match(&self) -> bool {
        matches!(self.query, Query::Match { .. })
    }

    /// How many nodes a match finds at most, from 1 to [`MAX_LIMIT`].
    pub fn limit(&self) -> usize {
        match &self.query {
            Query::Match { limit, .. } => (*limit).clamp(1, MAX_LIMIT) as usize,
            _ => 0,
        }
    }

    /// The source in words, of the node labelled `of`: "the children of
    /// Work", "the children of Work, two levels", "the nodes tagged waiting
    /// in this store, by title, at most 50".
    pub fn describe(&self, of: &str) -> String {
        self.describe_named(of, &|_| None)
    }

    /// [`Source::describe`], with `name` saying what a store or node a match
    /// names is called ("in Personal" rather than "in this store").
    pub fn describe_named(&self, of: &str, name: &dyn Fn(Named) -> Option<String>) -> String {
        if let Query::Match { within, all, order, descending, .. } = &self.query {
            let what = if all.is_empty() {
                "every node".to_string()
            } else {
                format!("the nodes {}", all.iter().map(|c| c.describe(name)).collect::<Vec<_>>().join(" and "))
            };
            let place = match within {
                None => name(Named::Here).map(|n| format!("in {n}")).unwrap_or_else(|| "in this store".to_string()),
                Some(Within::Store(store)) => format!("in {}", name(Named::Store(*store)).unwrap_or_else(|| "another store".to_string())),
                Some(Within::Under(node)) => format!("under {}", name(Named::Node(node)).unwrap_or_else(|| node.clone())),
                Some(Within::Everywhere) => "everywhere".to_string(),
            };
            return format!("{what} {place}, {}, at most {}", order_words(order, *descending), self.limit());
        }
        match self.depth() {
            1 => format!("the children of {of}"),
            depth => format!("the children of {of}, {} levels", number_word(depth)),
        }
    }

    /// Why a node a match found is found, after "Shown here because": "it
    /// is tagged #waiting and it isn't tagged #done". `None` for a source
    /// that finds nothing.
    pub fn found_reason(&self, name: &dyn Fn(Named) -> Option<String>) -> Option<String> {
        let Query::Match { all, .. } = &self.query else { return None };
        if all.is_empty() {
            return Some("this list shows every node where it looks".to_string());
        }
        Some(all.iter().map(|c| c.reason(name)).collect::<Vec<_>>().join(" and "))
    }
}

fn order_words(order: &Order, descending: bool) -> String {
    match (order, descending) {
        (Order::Title, false) => "by title".to_string(),
        (Order::Title, true) => "by title, Z to A".to_string(),
        (Order::Modified, true) => "last changed first".to_string(),
        (Order::Modified, false) => "least recently changed first".to_string(),
        (Order::Created, true) => "newest first".to_string(),
        (Order::Created, false) => "oldest first".to_string(),
        (Order::Field(f), false) => format!("by {}", f.trim()),
        (Order::Field(f), true) => format!("by {}, highest first", f.trim()),
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

    #[test]
    fn a_match_round_trips_in_the_contracts_shape() {
        let store = StoreId::new();
        let raw = format!(
            r#"{{"query":{{"kind":"match","within":{{"store":"{store}"}},"all":[{{"tag":"waiting"}},{{"no_tag":"done"}},{{"text":"invoice"}},{{"field":"Due","is":"<","value":"Friday"}},{{"field":"Owner","is":"set"}},{{"links_to":"{store}/{node}"}},{{"type":"canvas"}}],"order":{{"field":"Due"}},"descending":true,"limit":20}}}}"#,
            node = NodeId::new()
        );
        let source = Source::parse(&raw);
        let Query::Match { within, all, order, descending, limit } = &source.query else { panic!("{source:?}") };
        assert_eq!(within, &Some(Within::Store(store)));
        assert_eq!(all.len(), 7);
        assert_eq!(all[0], Condition::Tag { tag: "waiting".into() });
        assert_eq!(all[3], Condition::Field { field: "Due".into(), is: FieldOp::Less, value: Some("Friday".into()) });
        assert_eq!(all[4], Condition::Field { field: "Owner".into(), is: FieldOp::Set, value: None });
        assert_eq!(all[6], Condition::Type { node_type: "canvas".into() });
        assert_eq!(order, &Order::Field("Due".into()));
        assert!(*descending);
        assert_eq!(*limit, 20);
        assert_eq!(source.not_understood(), None);
        assert_eq!(source.to_stored(), raw, "written back exactly as read");
        assert_eq!(Source::parse(&source.to_stored()), source);

        // The other places and orders.
        for within in [r#""everywhere""#, r#"{"under":"a/b"}"#] {
            let raw = format!(r#"{{"query":{{"kind":"match","within":{within},"all":[],"order":"modified","descending":false,"limit":50}}}}"#);
            assert_eq!(Source::parse(&raw).to_stored(), raw);
        }
        // Defaults: its own store, by title, at most 50.
        let bare = Source::parse(r#"{"query":{"kind":"match","all":[{"tag":"x"}]}}"#);
        assert_eq!(bare, Source::matching(vec![Condition::Tag { tag: "x".into() }]));
        assert_eq!(bare.limit(), DEFAULT_LIMIT as usize);
        assert_eq!(bare.depth(), 1);
        assert!(bare.is_match());
    }

    #[test]
    fn a_match_says_itself_in_words() {
        let mut source = Source::matching(vec![Condition::Tag { tag: "waiting".into() }]);
        if let Query::Match { order, descending, .. } = &mut source.query {
            *order = Order::Created;
            *descending = true;
        }
        assert_eq!(source.describe("Waiting"), "the nodes tagged waiting in this store, newest first, at most 50");
        let named = source.describe_named("Waiting", &|n| (n == Named::Here).then(|| "Personal".to_string()));
        assert_eq!(named, "the nodes tagged waiting in Personal, newest first, at most 50");
        let both = Source::matching(vec![
            Condition::Tag { tag: "#waiting".into() },
            Condition::NoTag { no_tag: "done".into() },
            Condition::Field { field: "Due".into(), is: FieldOp::Less, value: Some("Friday".into()) },
            Condition::Field { field: "Cost".into(), is: FieldOp::AtLeast, value: Some("100".into()) },
            Condition::Type { node_type: "canvas".into() },
        ]);
        assert_eq!(
            both.describe("X"),
            "the nodes tagged waiting and not tagged done and with Due before Friday and with Cost at least 100 and that are canvases in this store, by title, at most 50"
        );
        assert_eq!(
            both.found_reason(&|_| None).unwrap(),
            "it is tagged #waiting and it isn't tagged #done and it has Due before Friday and it has Cost at least 100 and it is a canvas"
        );
        assert_eq!(Source::subtree(2).found_reason(&|_| None), None);
        let spec = Metanode { viewer: "list".into(), source: Source::matching(Vec::new()) };
        assert_eq!(spec.describe("All"), "a metanode: every node in this store, by title, at most 50, shown with the list viewer");
    }

    /// What the Pimble before matches does with one: its `Query` had
    /// children, subtree and `#[serde(other)] Later`, so a match is a
    /// query it does not know, shown as the children with a sentence.
    #[test]
    fn an_older_reader_sees_a_match_as_a_newer_query() {
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum OldQuery {
            Children,
            Subtree { depth: u32 },
            #[serde(other)]
            Later,
        }
        #[derive(Debug, Deserialize)]
        struct OldSource {
            query: OldQuery,
        }
        let new = Source::matching(vec![Condition::Tag { tag: "waiting".into() }]).to_stored();
        let old: OldSource = serde_json::from_str(&new).unwrap();
        assert_eq!(old.query, OldQuery::Later);
    }

    /// A condition from a newer Pimble is kept, written back as it was, and
    /// said not to be understood; the match it is in finds nothing here.
    #[test]
    fn a_newer_condition_is_kept_and_named() {
        let raw = r#"{"query":{"kind":"match","all":[{"tag":"x"},{"near":"home","km":3}],"order":"title","descending":false,"limit":50}}"#;
        let source = Source::parse(raw);
        let Query::Match { all, .. } = &source.query else { panic!() };
        assert!(matches!(all[1], Condition::Later(_)));
        assert_eq!(source.not_understood(), Some(NEWER_SOURCE));
        assert_eq!(Source::parse(&source.to_stored()), source, "rewriting it loses nothing");
    }

    #[test]
    fn node_refs_read_both_ways() {
        let (store, node) = (StoreId::new(), NodeId::new());
        assert_eq!(parse_node_ref(&node_ref(store, node)), Some((store, node)));
        assert_eq!(parse_node_ref(&crate::PimbleUrl::node(store, node).to_string()), Some((store, node)));
        assert_eq!(parse_node_ref("nonsense"), None);
        assert_eq!(tag_key(" #Waiting "), "waiting");
    }
}
