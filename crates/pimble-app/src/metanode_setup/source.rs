//! A query metanode's source as the setup edits it, pure: the kinds of
//! condition it offers, and the source taken apart into the parts its lines
//! edit and put back together.

use pimble_core::metanode::{Condition, FieldOp, Order, Query, Source, Within, MAX_LIMIT};
use pimble_core::StoreId;

/// A kind of condition, as "+ Add a condition" offers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tag,
    NoTag,
    Text,
    Field,
    LinksTo,
    Type,
}

impl Kind {
    /// Every kind, in the order the menu lists them.
    pub const ALL: [Kind; 6] = [Kind::Tag, Kind::NoTag, Kind::Text, Kind::Field, Kind::LinksTo, Kind::Type];

    /// The words that start its line: "tagged [waiting]".
    pub fn words(self) -> &'static str {
        match self {
            Kind::Tag => "tagged",
            Kind::NoTag => "not tagged",
            Kind::Text => "containing",
            Kind::Field => "with",
            Kind::LinksTo => "linking to",
            Kind::Type => "that are",
        }
    }

    /// What the menu calls it.
    pub fn menu_label(self) -> &'static str {
        match self {
            Kind::Tag => "Tagged...",
            Kind::NoTag => "Not tagged...",
            Kind::Text => "Containing words...",
            Kind::Field => "With a field...",
            Kind::LinksTo => "Linking to a node...",
            Kind::Type => "Of a kind...",
        }
    }

    pub fn of(condition: &Condition) -> Option<Kind> {
        Some(match condition {
            Condition::Tag { .. } => Kind::Tag,
            Condition::NoTag { .. } => Kind::NoTag,
            Condition::Text { .. } => Kind::Text,
            Condition::Field { .. } => Kind::Field,
            Condition::LinksTo { .. } => Kind::LinksTo,
            Condition::Type { .. } => Kind::Type,
            Condition::Later(_) => return None,
        })
    }

    /// A new line of this kind, to be filled in.
    pub fn blank(self) -> Condition {
        match self {
            Kind::Tag => Condition::Tag { tag: String::new() },
            Kind::NoTag => Condition::NoTag { no_tag: String::new() },
            Kind::Text => Condition::Text { text: String::new() },
            Kind::Field => Condition::Field { field: String::new(), is: FieldOp::Equal, value: None },
            Kind::LinksTo => Condition::LinksTo { links_to: String::new() },
            Kind::Type => Condition::Type { node_type: "document".to_string() },
        }
    }
}

/// The kinds a "that are" line offers, as stored and as said.
pub const TYPES: [(&str, &str); 6] =
    [("document", "documents"), ("canvas", "canvases"), ("sheet", "sheets"), ("discussion", "discussions"), ("kanban", "kanbans"), ("folder", "folders")];

/// A field line's comparisons, as stored and as said.
pub const FIELD_OPS: [(&str, &str); 7] = [
    ("=", "equal to"),
    ("<", "before / under"),
    ("<=", "on or before / at most"),
    (">=", "on or after / at least"),
    (">", "after / over"),
    ("set", "set"),
    ("empty", "empty"),
];

pub(super) fn op_key(op: FieldOp) -> &'static str {
    match op {
        FieldOp::Equal => "=",
        FieldOp::Less => "<",
        FieldOp::AtMost => "<=",
        FieldOp::AtLeast => ">=",
        FieldOp::More => ">",
        FieldOp::Set => "set",
        FieldOp::Empty => "empty",
    }
}

pub(super) fn op_of(key: &str) -> FieldOp {
    match key {
        "<" => FieldOp::Less,
        "<=" => FieldOp::AtMost,
        ">=" => FieldOp::AtLeast,
        ">" => FieldOp::More,
        "set" => FieldOp::Set,
        "empty" => FieldOp::Empty,
        _ => FieldOp::Equal,
    }
}

/// The parts of a match, read from `source` (a source that is not a match yet
/// reads as an empty one in its metanode's store).
#[derive(Debug, Clone, PartialEq)]
pub struct Parts {
    pub within: Option<Within>,
    pub all: Vec<Condition>,
    pub order: Order,
    pub descending: bool,
    pub limit: u32,
}

pub fn parts(source: &Source) -> Parts {
    match &source.query {
        Query::Match { within, all, order, descending, limit } => {
            Parts { within: within.clone(), all: all.clone(), order: order.clone(), descending: *descending, limit: *limit }
        }
        _ => {
            let empty = Source::matching(Vec::new());
            parts(&empty)
        }
    }
}

/// `source` with `f` applied to its match's parts (a source that was not a
/// match becomes one). What a newer Pimble wrote beside the query is kept.
pub fn edited(source: &Source, f: impl FnOnce(&mut Parts)) -> Source {
    let mut p = parts(source);
    f(&mut p);
    Source {
        query: Query::Match { within: p.within, all: p.all, order: p.order, descending: p.descending, limit: p.limit },
        later: source.later.clone(),
    }
}

/// The text a line's one field holds (a tag, words, a field's name).
pub fn text_of(condition: &Condition) -> String {
    match condition {
        Condition::Tag { tag } => tag.clone(),
        Condition::NoTag { no_tag } => no_tag.clone(),
        Condition::Text { text } => text.clone(),
        Condition::Field { field, .. } => field.clone(),
        Condition::LinksTo { links_to } => links_to.clone(),
        Condition::Type { node_type } => node_type.clone(),
        Condition::Later(_) => String::new(),
    }
}

/// `condition` with its one field set to `text`.
pub fn with_text(condition: &Condition, text: String) -> Condition {
    match condition {
        Condition::Tag { .. } => Condition::Tag { tag: text },
        Condition::NoTag { .. } => Condition::NoTag { no_tag: text },
        Condition::Text { .. } => Condition::Text { text },
        Condition::Field { is, value, .. } => Condition::Field { field: text, is: *is, value: value.clone() },
        Condition::LinksTo { .. } => Condition::LinksTo { links_to: text },
        Condition::Type { .. } => Condition::Type { node_type: text },
        Condition::Later(v) => Condition::Later(v.clone()),
    }
}

/// "From" as the setup's choice names it: `store`, `under`, `everywhere`,
/// or `store:<id>` for another store than the metanode's.
pub fn from_key(within: &Option<Within>, own_store: StoreId) -> String {
    match within {
        None => "store".to_string(),
        Some(Within::Store(s)) if *s == own_store => "store".to_string(),
        Some(Within::Store(s)) => format!("store:{s}"),
        Some(Within::Under(_)) => "under".to_string(),
        Some(Within::Everywhere) => "everywhere".to_string(),
    }
}

/// The `within` a "From" choice means; choosing "under" keeps the node it
/// named, if it named one.
pub fn within_of(key: &str, was: &Option<Within>) -> Option<Within> {
    match key {
        "under" => Some(match was {
            Some(Within::Under(n)) => Within::Under(n.clone()),
            _ => Within::Under(String::new()),
        }),
        "everywhere" => Some(Within::Everywhere),
        other => other.strip_prefix("store:").and_then(|id| StoreId::parse(id).ok()).map(Within::Store),
    }
}

pub fn order_key(order: &Order) -> &'static str {
    match order {
        Order::Title => "title",
        Order::Modified => "modified",
        Order::Created => "created",
        Order::Field(_) => "field",
    }
}

pub fn order_of(key: &str, was: &Order) -> Order {
    match key {
        "modified" => Order::Modified,
        "created" => Order::Created,
        "field" => match was {
            Order::Field(f) => Order::Field(f.clone()),
            _ => Order::Field(String::new()),
        },
        _ => Order::Title,
    }
}

/// "At most" as typed: a whole number from 1 to [`MAX_LIMIT`]; anything else
/// leaves it as it was.
pub fn limit_of(typed: &str, was: u32) -> u32 {
    typed.trim().parse::<u32>().map(|n| n.clamp(1, MAX_LIMIT)).unwrap_or(was)
}

/// What the count line says.
pub fn count_line(count: usize, all: &[Condition]) -> String {
    if all.iter().any(Condition::is_blank) {
        return "Fill in each condition to find nodes.".to_string();
    }
    match count {
        0 => "It finds nothing now.".to_string(),
        1 => "It finds 1 node now.".to_string(),
        n => format!("It finds {n} nodes now."),
    }
}

