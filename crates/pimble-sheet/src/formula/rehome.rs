//! A table moving to another note (docs/SHEETS_CONTRACT.md section 1, "Convert
//! to Sheet Node"): the stored formulas on both sides rewritten so that every
//! reference still names what it named.
//!
//! A reference to a table of the formula's own note is stored with no note
//! token (`{t:<id>}`), one to another note's with it (`{n:<store>/<node>}!{t:<id>}`).
//! When table `moved` leaves note `from` for note `to`:
//!
//! - its own formulas ([`moved_out`]) name `from`'s tables with `from`'s token,
//!   and `to`'s tables, now its own note's, without one;
//! - the formulas left in `from` ([`left_behind`]) name `moved` with `to`'s
//!   token.
//!
//! Only id tokens are touched; text inside string literals is left as it is.
//! Row links name a table of their own node only, so a table tied to another
//! by a row link cannot move this way (the caller refuses it).

use crate::model::NodeRef;

use super::names::note_token;

/// The spans of `stored` outside string literals (`"..."`, `""` inside one
/// being a quote), as `(start, end)` byte ranges.
fn code_spans(stored: &str) -> Vec<(usize, usize)> {
    let bytes = stored.as_bytes();
    let mut spans = Vec::new();
    let (mut start, mut i, mut in_string) = (0, 0, false);
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if in_string {
                if bytes.get(i + 1) == Some(&b'"') {
                    i += 2;
                    continue;
                }
                in_string = false;
                start = i + 1;
            } else {
                spans.push((start, i));
                in_string = true;
            }
        }
        i += 1;
    }
    if !in_string {
        spans.push((start, bytes.len()));
    }
    spans
}

/// One table reference found in a formula: where its note token starts (or
/// its table token, when it has none), the note it names (`None` for the
/// formula's own note), the table id, and where the table token ends.
struct TableRef {
    start: usize,
    note: Option<String>,
    table: String,
    end: usize,
}

fn table_refs(stored: &str) -> Vec<TableRef> {
    let mut out = Vec::new();
    for (from, to) in code_spans(stored) {
        let code = &stored[from..to];
        let mut at = 0;
        while let Some(found) = code[at..].find("{t:") {
            let t_start = at + found;
            let Some(close) = code[t_start..].find('}') else { break };
            let t_end = t_start + close + 1;
            let table = code[t_start + 3..t_end - 1].to_string();
            // `{n:...}!` right before it names its note.
            let before = &code[..t_start];
            let (start, note) = match before.strip_suffix("}!").and_then(|b| b.rfind("{n:").map(|n| (n, &b[n + 3..]))) {
                Some((n, inner)) if !inner.contains('}') && !inner.contains('{') => (n, Some(inner.to_string())),
                _ => (t_start, None),
            };
            out.push(TableRef { start: from + start, note, table, end: from + t_end });
            at = t_end;
        }
    }
    out
}

fn note_inner(n: &NodeRef) -> String {
    format!("{}/{}", n.store, n.node)
}

fn rewrite(stored: &str, f: impl Fn(&TableRef) -> Option<String>) -> String {
    let mut out = String::with_capacity(stored.len() + 16);
    let mut last = 0;
    for r in table_refs(stored) {
        if let Some(new) = f(&r) {
            out.push_str(&stored[last..r.start]);
            out.push_str(&new);
            last = r.end;
        }
    }
    out.push_str(&stored[last..]);
    out
}

/// A formula of table `own`, moving from note `from` to note `to`, as it is
/// stored in `to`: `from`'s tables named with `from`'s token, `to`'s without.
/// `to` is `None` for a note not made yet (nothing can name it).
pub fn moved_out(stored: &str, own: &str, from: &NodeRef, to: Option<&NodeRef>) -> String {
    let (from_inner, to_inner) = (note_inner(from), to.map(note_inner));
    rewrite(stored, |r| match &r.note {
        None if r.table != own => Some(format!("{}!{{t:{}}}", note_token(from), r.table)),
        Some(n) if Some(n) == to_inner.as_ref() => Some(format!("{{t:{}}}", r.table)),
        Some(n) if *n == from_inner && r.table == own => Some(format!("{{t:{}}}", r.table)),
        _ => None,
    })
}

/// A formula left in a note after its table `moved` went to note `to`.
pub fn left_behind(stored: &str, moved: &str, to: &NodeRef) -> String {
    rewrite(stored, |r| (r.note.is_none() && r.table == moved).then(|| format!("{}!{{t:{}}}", note_token(to), r.table)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pimble_core::{NodeId, StoreId};

    fn n(x: u128) -> NodeRef {
        NodeRef { store: StoreId(uuid::Uuid::from_u128(1)), node: NodeId(uuid::Uuid::from_u128(x)) }
    }

    #[test]
    fn the_moved_table_names_its_old_note_and_the_rest_name_it_in_its_new_one() {
        let (from, to, other) = (n(10), n(20), n(30));
        let stored = format!(
            "=SUM({{t:a}}[{{c:x}}])+SUM({{t:own}}[{{c:y}}])+{}!{{t:b}}[{{c:z}}]+{}!{{t:c}}[{{c:w}}]&\"{{t:a}}\"",
            note_token(&to),
            note_token(&other)
        );
        assert_eq!(
            moved_out(&stored, "own", &from, Some(&to)),
            format!(
                "=SUM({}!{{t:a}}[{{c:x}}])+SUM({{t:own}}[{{c:y}}])+{{t:b}}[{{c:z}}]+{}!{{t:c}}[{{c:w}}]&\"{{t:a}}\"",
                note_token(&from),
                note_token(&other)
            ),
            "this note's tables get its token; the new note's lose theirs; strings stay"
        );
        let left = "=COUNTIF({t:own}[{c:v}], [@{c:n}])&\"{t:own}\"+SUM({t:a}[{c:x}])";
        assert_eq!(
            left_behind(left, "own", &to),
            format!("=COUNTIF({}!{{t:own}}[{{c:v}}], [@{{c:n}}])&\"{{t:own}}\"+SUM({{t:a}}[{{c:x}}])", note_token(&to))
        );
    }
}
