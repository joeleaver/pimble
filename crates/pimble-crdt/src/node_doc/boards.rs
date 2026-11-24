//! Board text and cards, for search and links (see `NodeDoc::board_units`).

use pimble_core::IndexUnit;
use yrs::{Any, GetString, Map, MapRef, Out, ReadTxn};

use crate::links::LinkRef;

use super::string_value;

/// A board node's board, under `data`.
pub(super) const BOARD: &str = "board";
/// The prefix of the keys, in `data`, of the boards of marked-up pictures in
/// a node's text: `board:<id>`, one key of the data root per board, so two
/// devices marking up their first pictures while apart never write two maps
/// to one key.
const TEXT_BOARD_PREFIX: &str = "board:";

/// The boards of marked-up pictures in a node's text, by id, in id order.
pub(super) fn text_boards<T: ReadTxn>(txn: &T, data: &MapRef) -> Vec<(String, MapRef)> {
    let mut boards: Vec<(String, MapRef)> = data
        .iter(txn)
        .filter_map(|(key, out)| match (key.strip_prefix(TEXT_BOARD_PREFIX), out) {
            (Some(id), Out::YMap(map)) => Some((id.to_string(), map)),
            _ => None,
        })
        .collect();
    boards.sort_by(|a, b| a.0.cmp(&b.0));
    boards
}

/// One unit per element of `board` with text, in element id order.
pub(super) fn board_texts<T: ReadTxn>(txn: &T, board: &MapRef, prefix: &str, units: &mut Vec<IndexUnit>) {
    // What an element or layer means (its note and named fields,
    // `pimble_board` "Meaning") is searched beside the text it shows.
    fn meaning<T: ReadTxn>(txn: &T, map: &MapRef) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(Out::YText(note)) = map.get(txn, "note") {
            out.push(note.get_string(txn));
        }
        let mut fields: Vec<String> = map
            .iter(txn)
            .filter_map(|(k, v)| Some(format!("{}: {}", k.strip_prefix("field:")?, string_value(Some(v))?)))
            .collect();
        fields.sort();
        out.extend(fields);
        out.retain(|s| !s.trim().is_empty());
        out
    }
    let Some(Out::YMap(elements)) = board.get(txn, "elements") else { return };
    let mut found: Vec<(String, String, String)> = Vec::new();
    for (id, out) in elements.iter(txn) {
        let Out::YMap(element) = out else { continue };
        let mut parts = Vec::new();
        if let Some(Out::YText(text)) = element.get(txn, "text") {
            parts.push(text.get_string(txn));
        }
        parts.extend(meaning(txn, &element));
        parts.retain(|s| !s.trim().is_empty());
        if parts.is_empty() {
            continue;
        }
        let kind = string_value(element.get(txn, "kind")).unwrap_or_default();
        found.push((id.to_string(), kind, parts.join("\n")));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, kind, text) in found {
        units.push(IndexUnit::new(pimble_core::UnitKind::Field(format!("board.{kind}")), format!("{prefix}/{id}"), text));
    }
    let Some(Out::YMap(layers)) = board.get(txn, "layers") else { return };
    let mut layer_units: Vec<(String, String)> = Vec::new();
    for (id, out) in layers.iter(txn) {
        let Out::YMap(layer) = out else { continue };
        let parts = meaning(txn, &layer);
        if parts.is_empty() {
            continue;
        }
        let name = match layer.get(txn, "name") {
            Some(Out::YText(t)) => t.get_string(txn),
            _ => String::new(),
        };
        layer_units.push((id.to_string(), std::iter::once(name).chain(parts).collect::<Vec<_>>().join("\n")));
    }
    layer_units.sort();
    for (id, text) in layer_units {
        units.push(IndexUnit::new(pimble_core::UnitKind::Field("board.layer".to_string()), format!("{prefix}/layers/{id}"), text));
    }
}

/// One link per node card of `board` whose link is a `pimble:` URL, in
/// element id order.
pub(super) fn board_cards<T: ReadTxn>(txn: &T, board: &MapRef, prefix: &str, out: &mut Vec<LinkRef>) {
    let Some(Out::YMap(elements)) = board.get(txn, "elements") else { return };
    let mut found: Vec<(String, LinkRef)> = Vec::new();
    for (id, element) in elements.iter(txn) {
        let Out::YMap(element) = element else { continue };
        if string_value(element.get(txn, "kind")).as_deref() != Some("card") {
            continue;
        }
        let href = match element.get(txn, "geom") {
            Some(Out::Any(Any::Map(geom))) => match geom.get("link") {
                Some(Any::String(s)) => s.to_string(),
                _ => continue,
            },
            _ => continue,
        };
        let Some(target) = pimble_core::PimbleUrl::parse(&href) else { continue };
        let text = match element.get(txn, "text") {
            Some(Out::YText(t)) => t.get_string(txn),
            _ => String::new(),
        };
        found.push((id.to_string(), LinkRef { target, path: format!("{prefix}/{id}"), text }));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    out.extend(found.into_iter().map(|(_, link)| link));
}
