//! The plugin root (`data`): a plugin's JSON as nested yrs types, read whole,
//! set per key or edited at a path, and the conversions between JSON and yrs.


use pimble_core::IndexUnit;
use yrs::{Any, Array, Doc, GetString, In, Map, MapRef, Out, ReadTxn, TextPrelim, Transact};

use crate::error::{CrdtError, Result};

use super::boards::{board_texts, text_boards, BOARD};
use super::NodeDoc;

impl NodeDoc {
    // ── The plugin root ──────────────────────────────────────────────────

    /// The `data` root as JSON (a plugin's read view). An empty object when
    /// nothing was ever written.
    pub fn data_json(&self) -> serde_json::Value {
        let txn = self.doc.transact();
        map_to_json(&txn, &self.data)
    }

    /// Set one top-level key of the `data` root from JSON: objects and arrays
    /// become nested yrs maps and arrays (co-editable field by field), strings
    /// become yrs text, scalars are values. The finer plugin API comes with
    /// the plugins; this is enough to round-trip.
    pub fn set_data(&mut self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.edit(|d, txn| {
            d.put_data(txn, key, value);
            Ok(())
        })
        .map(|_| ())
    }

    /// Change one place inside the `data` root, named by `path` from a
    /// top-level key down through nested maps (by key) and arrays (by index):
    /// set a map's key or replace an array's item, insert an item into an
    /// array, or remove either. Everything else under `data` is left as it
    /// is, so two people changing different parts of a plugin's JSON (a
    /// calendar's month names, its eras) both keep what they wrote, where
    /// [`NodeDoc::set_data`] would put a whole new value in place. Answers
    /// whether anything changed (setting what is already there writes
    /// nothing); a path that names nothing, or an edit its place cannot take,
    /// is refused and nothing is written.
    pub fn edit_data(&mut self, path: &[DataStep<'_>], edit: DataEdit<'_>) -> Result<bool> {
        let (last, within) = path.split_last().ok_or_else(|| CrdtError::Refused("An edit of a plugin's data names no place.".into()))?;
        let (_, update) = self.edit(|d, txn| {
            let mut at = Out::YMap(d.data.clone());
            for step in within {
                at = match (&at, step) {
                    (Out::YMap(map), DataStep::Key(key)) => map.get(txn, key),
                    (Out::YArray(array), DataStep::Index(i)) => array.get(txn, *i),
                    _ => None,
                }
                .ok_or_else(|| CrdtError::Refused(format!("Nothing in this node's data is at {}.", describe_path(path))))?;
            }
            let wrong = || CrdtError::Refused(format!("{} is not a place that edit can change.", describe_path(path)));
            match (at, *last, edit) {
                (Out::YMap(map), DataStep::Key(key), DataEdit::Set(value)) => {
                    if map.get(txn, key).is_some_and(|now| out_to_json(txn, now) == *value) {
                        return Ok(());
                    }
                    map.insert(txn, key.to_string(), json_to_in(value));
                }
                (Out::YMap(map), DataStep::Key(key), DataEdit::Remove) => {
                    map.remove(txn, key);
                }
                (Out::YArray(array), DataStep::Index(i), DataEdit::Set(value)) if i < array.len(txn) => {
                    if array.get(txn, i).is_some_and(|now| out_to_json(txn, now) == *value) {
                        return Ok(());
                    }
                    array.remove(txn, i);
                    array.insert(txn, i, json_to_in(value));
                }
                (Out::YArray(array), DataStep::Index(i), DataEdit::Insert(value)) if i <= array.len(txn) => {
                    array.insert(txn, i, json_to_in(value));
                }
                (Out::YArray(array), DataStep::Index(i), DataEdit::Remove) if i < array.len(txn) => {
                    array.remove(txn, i);
                }
                _ => return Err(wrong()),
            }
            Ok(())
        })?;
        Ok(update.is_some())
    }

    /// The yrs document and its `data` root, for code that keeps a finer
    /// schema under `data` than JSON (a board, docs/IMAGE_MARKUP_CONTRACT.md
    /// section 3: `pimble_board::Board::of_node`). What it writes is an
    /// ordinary update of this document and travels through `applyEdit` like
    /// every other edit; it must not hold a transaction across calls here.
    pub fn ydoc(&self) -> &Doc {
        &self.doc
    }

    /// The `data` root (see [`NodeDoc::ydoc`]).
    pub fn data_root(&self) -> MapRef {
        self.data.clone()
    }

    /// The text written on this node's boards, for search
    /// (docs/IMAGE_MARKUP_CONTRACT.md section 1, "Search"): every board
    /// element with text (text elements, sticky notes, text inside shapes,
    /// callouts, connector labels, frame names), from the board node's own
    /// board (`data.board`) and from the boards of marked-up pictures in its
    /// text (`data["board:<id>"]`). One `Field` unit per element, named
    /// `board.<kind>`, its path `f:board/<element>` or
    /// `f:boards/<board>/<element>`. Only where text lives is known here;
    /// the schema itself is pimble-board's.
    pub fn board_units(&self) -> Vec<IndexUnit> {
        let txn = self.doc.transact();
        let mut units = Vec::new();
        if let Some(Out::YMap(board)) = self.data.get(&txn, BOARD) {
            board_texts(&txn, &board, "f:board", &mut units);
        }
        for (id, board) in text_boards(&txn, &self.data) {
            board_texts(&txn, &board, &format!("f:boards/{id}"), &mut units);
        }
        units
    }
}

// ── JSON in and out of the `data` root ──────────────────────────────────

/// A step into the `data` root ([`NodeDoc::edit_data`]): a map's key or an
/// array's index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataStep<'a> {
    Key(&'a str),
    Index(u32),
}

/// What [`NodeDoc::edit_data`] does at the place its path names.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DataEdit<'a> {
    /// Set a map's key, or replace an array's item, with this JSON.
    Set(&'a serde_json::Value),
    /// Insert this JSON into an array before the index named (its length:
    /// at the end).
    Insert(&'a serde_json::Value),
    /// Remove a map's key or an array's item.
    Remove,
}

/// A data path as a person reads it: `calendar.year[3].days`.
fn describe_path(path: &[DataStep<'_>]) -> String {
    let mut out = String::new();
    for step in path {
        match step {
            DataStep::Key(key) if out.is_empty() => out.push_str(key),
            DataStep::Key(key) => {
                out.push('.');
                out.push_str(key);
            }
            DataStep::Index(i) => out.push_str(&format!("[{i}]")),
        }
    }
    out
}

/// A JSON value as something to insert: objects and arrays as nested shared
/// types, strings as shared text, scalars as values.
pub(super) fn json_to_in(value: &serde_json::Value) -> In {
    use serde_json::Value;
    match value {
        Value::Null => In::Any(Any::Null),
        Value::Bool(b) => In::Any(Any::Bool(*b)),
        Value::Number(n) => In::Any(match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Any::BigInt(i),
            (None, Some(f)) => Any::Number(f),
            (None, None) => Any::Null,
        }),
        Value::String(s) => In::from(TextPrelim::new(s.clone())),
        Value::Array(items) => In::Array(items.iter().map(json_to_in).collect()),
        Value::Object(fields) => In::Map(fields.iter().map(|(k, v)| (k.as_str(), json_to_in(v))).collect()),
    }
}

/// A value read from one document as a value to write into another, its yrs
/// type kept (text without its formatting: `data` holds none).
pub(super) fn out_to_in<T: ReadTxn>(txn: &T, out: Out) -> In {
    match out {
        Out::Any(any) => In::Any(any),
        Out::YText(text) => In::from(TextPrelim::new(text.get_string(txn))),
        Out::YArray(array) => In::Array(array.iter(txn).map(|item| out_to_in(txn, item)).collect()),
        Out::YMap(map) => In::Map(map.iter(txn).map(|(key, value)| (key.to_string(), out_to_in(txn, value))).collect()),
        // XML types, subdocuments and links are never written here.
        _ => In::Any(Any::Null),
    }
}

fn map_to_json<T: ReadTxn>(txn: &T, map: &MapRef) -> serde_json::Value {
    serde_json::Value::Object(map.iter(txn).map(|(key, value)| (key.to_string(), out_to_json(txn, value))).collect())
}

fn out_to_json<T: ReadTxn>(txn: &T, out: Out) -> serde_json::Value {
    use serde_json::Value;
    match out {
        Out::Any(any) => any_to_json(&any),
        Out::YText(text) => Value::String(text.get_string(txn)),
        Out::YArray(array) => Value::Array(array.iter(txn).map(|item| out_to_json(txn, item)).collect()),
        Out::YMap(map) => map_to_json(txn, &map),
        // XML types, subdocuments and links are never written here.
        _ => Value::Null,
    }
}

fn any_to_json(any: &Any) -> serde_json::Value {
    use serde_json::Value;
    match any {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::Number(f) => serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null),
        Any::BigInt(i) => Value::from(*i),
        Any::String(s) => Value::String(s.to_string()),
        Any::Buffer(bytes) => Value::Array(bytes.iter().map(|b| Value::from(*b)).collect()),
        Any::Array(items) => Value::Array(items.iter().map(any_to_json).collect()),
        Any::Map(fields) => Value::Object(fields.iter().map(|(k, v)| (k.clone(), any_to_json(v))).collect()),
    }
}
