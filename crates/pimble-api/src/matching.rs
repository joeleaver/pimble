//! A match's conditions and order, judged over one node's document
//! (docs/QUERY_METANODES_CONTRACT.md section 3). Pure: [`crate::Pimble`]
//! walks the held documents of the place a match looks and asks this of
//! each.

use std::cmp::Ordering;

use chrono::NaiveDate;
use pimble_core::metanode::{parse_node_ref, tag_key, Condition, FieldOp, Order, Query};
use pimble_core::{node_types, NodeId, StoreId};
use pimble_crdt::{NodeDoc, NodeFields};
use pimble_sheet::input::parse_input;
use pimble_sheet::{ColumnType, Stored};

/// A value as a person typed it, read the way a sheet reads typing: a number
/// (`$1,200`), else a date (`Friday`, `2026-11-06`), else text (any case).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Number(f64),
    Date(String),
    Text(String),
}

impl Value {
    pub fn read(text: &str, today: NaiveDate) -> Option<Value> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let none = |_: &str| None;
        if let Some(Stored::Number(n)) = parse_input(&ColumnType::Number, text, today, &none) {
            return Some(Value::Number(n));
        }
        if let Some(Stored::Date(d)) = parse_input(&ColumnType::Date, text, today, &none) {
            return Some(Value::Date(d));
        }
        Some(Value::Text(text.to_lowercase()))
    }

    fn rank(&self) -> u8 {
        match self {
            Value::Number(_) => 0,
            Value::Date(_) => 1,
            Value::Text(_) => 2,
        }
    }

    /// Two values of one kind compared; `None` across kinds.
    fn compare(&self, other: &Value) -> Option<Ordering> {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
            (Value::Date(a), Value::Date(b)) => Some(a.cmp(b)),
            (Value::Text(a), Value::Text(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }

    /// An order over every value: numbers, then dates, then text.
    fn total(&self, other: &Value) -> Ordering {
        self.compare(other).unwrap_or_else(|| self.rank().cmp(&other.rank()))
    }
}

/// One condition, read once for the whole walk.
#[derive(Debug)]
enum Prepared {
    Tag(String),
    NoTag(String),
    Type(String),
    Field { name: String, is: FieldOp, value: Option<Value> },
    Text(Vec<String>),
    LinksTo(Option<(StoreId, NodeId)>),
    /// A condition still being filled in ([`Condition::is_blank`]): it holds
    /// for nothing.
    Nothing,
}

impl Prepared {
    /// Cheap ones first: the content is read only for a node the fields let through.
    fn cost(&self) -> u8 {
        match self {
            Prepared::Nothing => 0,
            Prepared::Tag(_) | Prepared::NoTag(_) | Prepared::Type(_) | Prepared::Field { .. } => 0,
            Prepared::Text(_) => 1,
            Prepared::LinksTo(_) => 2,
        }
    }
}

/// A match's conditions, ready to judge nodes with.
#[derive(Debug)]
pub(crate) struct Matcher {
    conditions: Vec<Prepared>,
    today: NaiveDate,
}

impl Matcher {
    /// `None` for a query that is not a match, or one with a condition this
    /// version does not know (it would find too much without it).
    pub fn of(query: &Query, today: NaiveDate) -> Option<Matcher> {
        let Query::Match { all, .. } = query else { return None };
        let mut conditions = Vec::new();
        for c in all {
            if c.is_blank() {
                conditions.push(Prepared::Nothing);
                continue;
            }
            conditions.push(match c {
                Condition::Tag { tag } => Prepared::Tag(tag_key(tag)),
                Condition::NoTag { no_tag } => Prepared::NoTag(tag_key(no_tag)),
                Condition::Type { node_type } => Prepared::Type(node_type.trim().to_lowercase()),
                Condition::Field { field, is, value } => Prepared::Field {
                    name: field.trim().to_lowercase(),
                    is: *is,
                    value: value.as_deref().and_then(|v| Value::read(v, today)),
                },
                Condition::Text { text } => Prepared::Text(text.split_whitespace().map(str::to_lowercase).collect()),
                Condition::LinksTo { links_to } => Prepared::LinksTo(parse_node_ref(links_to)),
                Condition::Later(_) => return None,
            });
        }
        conditions.sort_by_key(Prepared::cost);
        Some(Matcher { conditions, today })
    }

    /// Whether every condition holds for the node `fields` describes.
    pub fn matches(&self, doc: &NodeDoc, fields: &NodeFields) -> bool {
        let mut text: Option<String> = None;
        self.conditions.iter().all(|c| match c {
            Prepared::Nothing => false,
            Prepared::Tag(tag) => fields.tags.iter().any(|t| tag_key(t) == *tag),
            Prepared::NoTag(tag) => !fields.tags.iter().any(|t| tag_key(t) == *tag),
            Prepared::Type(t) => is_type(fields, t),
            Prepared::Field { name, is, value } => self.field_holds(fields, name, *is, value.as_ref()),
            Prepared::Text(words) => {
                let haystack = text.get_or_insert_with(|| format!("{}\n{}", fields.title, doc.text()).to_lowercase());
                words.iter().all(|w| haystack.contains(w.as_str()))
            }
            Prepared::LinksTo(target) => {
                let Some((store, node)) = target else { return false };
                doc.links().iter().any(|l| l.target.store == *store && l.target.node == *node)
            }
        })
    }

    fn field_holds(&self, fields: &NodeFields, name: &str, is: FieldOp, value: Option<&Value>) -> bool {
        let have = field_value(fields, name).and_then(|v| Value::read(v, self.today));
        match is {
            FieldOp::Set => have.is_some(),
            FieldOp::Empty => have.is_none(),
            _ => {
                let (Some(have), Some(want)) = (have, value) else { return false };
                let Some(order) = have.compare(want) else { return false };
                match is {
                    FieldOp::Less => order == Ordering::Less,
                    FieldOp::AtMost => order != Ordering::Greater,
                    FieldOp::Equal => order == Ordering::Equal,
                    FieldOp::AtLeast => order != Ordering::Less,
                    FieldOp::More => order == Ordering::Greater,
                    FieldOp::Set | FieldOp::Empty => unreachable!(),
                }
            }
        }
    }

    /// What `order` sorts this node by.
    pub fn key(&self, order: &Order, fields: &NodeFields) -> Key {
        match order {
            Order::Title => Key::Value(Some(Value::Text(fields.title.trim().to_lowercase()))),
            Order::Modified => Key::Time(millis(&fields.modified_at)),
            Order::Created => Key::Time(millis(&fields.created_at)),
            Order::Field(name) => Key::Value(field_value(fields, &name.trim().to_lowercase()).and_then(|v| Value::read(v, self.today))),
        }
    }
}

/// A node's field by name, any case.
fn field_value<'a>(fields: &'a NodeFields, name_lower: &str) -> Option<&'a str> {
    fields.fields.iter().find(|(n, _)| n.trim().to_lowercase() == name_lower).map(|(_, v)| v.as_str())
}

/// What a type condition names, as stored: a canvas is a `board` (and the
/// old `canvas` and `image` types), a kanban a metanode with that viewer.
fn is_type(fields: &NodeFields, wanted: &str) -> bool {
    match wanted {
        "canvas" => [node_types::BOARD, node_types::CANVAS, node_types::IMAGE].contains(&fields.node_type.as_str()),
        "kanban" => fields.metanode_viewer.as_deref().map(str::trim) == Some("kanban"),
        other => fields.node_type.eq_ignore_ascii_case(other),
    }
}

fn millis(raw: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(raw).map(|t| t.timestamp_millis()).unwrap_or(0)
}

/// What a found node is sorted by.
#[derive(Debug, Clone)]
pub(crate) enum Key {
    Time(i64),
    /// `None` (a field the node does not have) always sorts last.
    Value(Option<Value>),
}

/// `a` before `b` in a match's order: by key (reversed when `descending`,
/// except that a missing value stays last), then by title, then by id, so
/// the order is the same every time.
pub(crate) fn compare(a: (&Key, &str, NodeId), b: (&Key, &str, NodeId), descending: bool) -> Ordering {
    let flip = |o: Ordering| if descending { o.reverse() } else { o };
    let by_key = match (a.0, b.0) {
        (Key::Time(x), Key::Time(y)) => flip(x.cmp(y)),
        (Key::Value(Some(x)), Key::Value(Some(y))) => flip(x.total(y)),
        (Key::Value(Some(_)), Key::Value(None)) => Ordering::Less,
        (Key::Value(None), Key::Value(Some(_))) => Ordering::Greater,
        _ => Ordering::Equal,
    };
    by_key.then_with(|| a.1.cmp(b.1)).then_with(|| a.2.to_string().cmp(&b.2.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap()
    }

    #[test]
    fn values_read_as_typed() {
        assert_eq!(Value::read("$1,200", today()), Some(Value::Number(1200.0)));
        assert_eq!(Value::read("2026-11-06", today()), Some(Value::Date("2026-11-06".into())));
        assert_eq!(Value::read("Open", today()), Some(Value::Text("open".into())));
        assert_eq!(Value::read("  ", today()), None);
        let fri = Value::read("2026-11-06", today()).unwrap();
        let mon = Value::read("2026-11-09", today()).unwrap();
        assert_eq!(fri.compare(&mon), Some(Ordering::Less));
        assert_eq!(fri.compare(&Value::Number(3.0)), None);
    }
}
