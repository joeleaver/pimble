//! Building a [`SearchHit`] from a stored `Node` or `Chunk` object: the
//! snippet around where the query matched and the headings above it.

use pimble_core::NodeId;
use rhypedb_engine::object::{Object, Value};

use super::outline::{block_of, Outline};
use super::prefix::PREFIX_STOP_WORDS;
use crate::query::SearchHit;

fn string_field(obj: &Object, name: &str) -> String {
    match obj.fields.get(name) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// How exactly a node matched a query as a whole: its title, one of its
/// headings, or neither (ordered so the better match is the greater).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Exact {
    No,
    Heading,
    Title,
}

/// A keyword hit on a whole node: its snippet around where the query is in
/// its text and the headings above that place. `wanted` is the query made
/// [`pimble_crdt::markdown::loose`].
pub(super) fn keyword_hit(obj: &Object, score: f32, query_text: &str, wanted: &str) -> Option<(Exact, SearchHit)> {
    let node_id = match obj.fields.get("node_id") {
        Some(Value::String(s)) => NodeId::parse(s).ok()?,
        _ => return None,
    };
    let title = string_field(obj, "title");
    let text = string_field(obj, "text");
    let outline = Outline::decode(&string_field(obj, "outline"));
    let heading = (!wanted.is_empty())
        .then(|| outline.entries.iter().position(|e| pimble_crdt::markdown::loose(&e.text) == wanted))
        .flatten();
    let exact = if !wanted.is_empty() && pimble_crdt::markdown::loose(&title) == wanted {
        Exact::Title
    } else if heading.is_some() {
        Exact::Heading
    } else {
        Exact::No
    };
    // Where the hit is: the heading the query names, else where its words are.
    let at = heading.and_then(|i| outline.entries[i].offset).or_else(|| match_position(&text, query_text));
    let section = match (heading, at) {
        (Some(i), _) => outline.path_to(i),
        (None, Some(at)) => outline.section_at_offset(at),
        (None, None) => Vec::new(),
    };
    Some((
        exact,
        SearchHit { node_id, score, title, snippet: snippet_at(&text, at), kind: "node".to_string(), path: None, section },
    ))
}

/// `distance` is `SimilarHit::distance` from the hit `semantic_search` chose
/// for this node — always a true metric distance (never a `rerank_score`;
/// `semantic_search` never reads that field, and `SearchIndex::open` keeps
/// the cross-encoder off, so it's always `None` here regardless). Purely for
/// *reporting* a "higher is better" score on `SearchHit`, uniform with
/// keyword hits' BM25 convention — this function has no say in hit order,
/// which `semantic_search` already fixed by the time it gets here.
pub(super) fn chunk_hit(node_obj: &Object, chunk_obj: &Object, distance: f32) -> Option<SearchHit> {
    let node_id = match node_obj.fields.get("node_id") {
        Some(Value::String(s)) => NodeId::parse(s).ok()?,
        _ => return None,
    };
    let title = match node_obj.fields.get("title") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let snippet = match chunk_obj.fields.get("text") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    let kind = match chunk_obj.fields.get("kind") {
        Some(Value::String(s)) => s.clone(),
        _ => "prose".to_string(),
    };
    let path = match chunk_obj.fields.get("path") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    let score = 1.0 / (1.0 + distance.max(0.0));
    // The chunk's first block, from its locator ("b:12", "b:12/r:3/c:1").
    let block = path.as_deref().and_then(block_of);
    let section = match block {
        Some(block) => Outline::decode(&string_field(node_obj, "outline")).section_at_block(block),
        None => Vec::new(),
    };
    Some(SearchHit {
        node_id,
        score,
        title,
        snippet,
        kind,
        path,
        section,
    })
}

/// Where `query_text` is in `text` (a byte offset into it): the whole query,
/// in any case; else its first word that is not a stop word; else its first
/// word. `None` when none of them is in it (or the query is only syntax).
fn match_position(text: &str, query_text: &str) -> Option<usize> {
    // Lowercasing can change a text's length (rarely); a position found in
    // the lowercased text is then near, not at, the place, and kept on a
    // char boundary of the original.
    let lower_text = text.to_lowercase();
    let terms: Vec<String> = query_text
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| c == '+' || c == '"' || c == '*').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    let whole = terms.join(" ");
    let found = (terms.len() > 1)
        .then(|| lower_text.find(&whole))
        .flatten()
        .or_else(|| terms.iter().filter(|t| !PREFIX_STOP_WORDS.contains(&t.as_str())).find_map(|t| lower_text.find(t.as_str())))
        .or_else(|| terms.first().and_then(|t| lower_text.find(t.as_str())))?;
    Some(floor_boundary(text, found))
}

/// [`snippet_at`] around where the query is in `text`.
#[cfg(test)]
fn snippet_around(text: &str, query_text: &str) -> String {
    snippet_at(text, match_position(text, query_text))
}

/// A window of about 160 chars of `text` around byte `at`, or its first 160
/// chars without one: a best-effort preview, never an error. The window is
/// snapped outward to whitespace on each side it doesn't already touch (so it
/// never opens or closes mid-word), each such trimmed side gets an "…", and
/// every cut stays on a UTF-8 char boundary.
fn snippet_at(text: &str, at: Option<usize>) -> String {
    const WINDOW: usize = 160;
    if text.is_empty() {
        return String::new();
    }
    let (start, end) = match at {
        Some(byte_pos) => (
            byte_pos.saturating_sub(WINDOW / 2),
            (byte_pos + WINDOW / 2).min(text.len()),
        ),
        None => (0, WINDOW.min(text.len())),
    };
    let start = floor_boundary(text, start);
    let end = ceil_boundary(text, end);

    let snapped_start = snap_start_to_word_boundary(text, start, end);
    let snapped_end = snap_end_to_word_boundary(text, start, end);
    // A window with no whitespace at all in it (one very long "word") can't be
    // snapped without emptying it — fall back to the unsnapped, but still
    // char-boundary-safe, window rather than show nothing.
    let (start, end) = if snapped_start < snapped_end {
        (snapped_start, snapped_end)
    } else {
        (start, end)
    };

    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push('…');
    }
    out
}

/// If `start` isn't already the text's start, advance it past the next
/// whitespace char in `text[start..end]` so the snippet opens on a fresh
/// word instead of mid-word. Left unchanged if `start == 0` or the range has
/// no whitespace to snap to.
fn snap_start_to_word_boundary(text: &str, start: usize, end: usize) -> usize {
    if start == 0 {
        return start;
    }
    match text[start..end].char_indices().find(|(_, c)| c.is_whitespace()) {
        Some((offset, ch)) => start + offset + ch.len_utf8(),
        None => start,
    }
}

/// If `end` isn't already the text's end, retreat it to the last whitespace
/// char in `text[start..end]` so the snippet closes at the end of a word
/// instead of mid-word. Left unchanged if `end == text.len()` or the range
/// has no whitespace to snap to.
fn snap_end_to_word_boundary(text: &str, start: usize, end: usize) -> usize {
    if end == text.len() {
        return end;
    }
    match text[start..end].char_indices().rev().find(|(_, c)| c.is_whitespace()) {
        Some((offset, _)) => start + offset,
        None => end,
    }
}

fn floor_boundary(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn ceil_boundary(text: &str, mut idx: usize) -> usize {
    idx = idx.min(text.len());
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `original` that starts and ends on a whitespace boundary (or at
    /// `original`'s own start/end) — never mid-word.
    fn assert_no_partial_word(original: &str, snippet: &str) {
        let core = snippet.strip_prefix('…').unwrap_or(snippet);
        let core = core.strip_suffix('…').unwrap_or(core);
        if core.is_empty() {
            return;
        }
        let idx = original
            .find(core)
            .expect("snippet core should be a substring of the original text");
        if idx > 0 {
            let before = original[..idx].chars().last().unwrap();
            assert!(before.is_whitespace(), "snippet started mid-word: {snippet:?}");
        }
        let end = idx + core.len();
        if end < original.len() {
            let after = original[end..].chars().next().unwrap();
            assert!(after.is_whitespace(), "snippet ended mid-word: {snippet:?}");
        }
    }

    #[test]
    fn snippet_never_starts_or_ends_mid_word() {
        // A window centered on "keyword" with plenty of filler on each side
        // reliably lands the raw byte window mid-word on both edges before
        // snapping.
        let filler = "alphabet ".repeat(30);
        let text = format!("{filler}keyword{filler}");
        let snippet = snippet_around(&text, "keyword");
        assert_no_partial_word(&text, &snippet);
        assert!(snippet.starts_with('…'), "snippet was: {snippet:?}");
        assert!(snippet.ends_with('…'), "snippet was: {snippet:?}");
    }

    #[test]
    fn snippet_handles_a_multibyte_character_near_the_window_edge() {
        // Multi-byte characters (é, ö) sit throughout the filler on both
        // sides, right where the raw byte window's edges fall.
        let filler = "héllo wörld café ".repeat(15);
        let text = format!("{filler}keyword{filler}");
        let snippet = snippet_around(&text, "keyword");
        assert!(snippet.contains("keyword"));
        assert_no_partial_word(&text, &snippet);
    }

    #[test]
    fn snippet_has_no_ellipsis_when_the_window_touches_a_text_edge() {
        let text = "keyword right at the very start of a short text";
        let snippet = snippet_around(text, "keyword");
        assert!(!snippet.starts_with('…'), "snippet was: {snippet:?}");
        assert_no_partial_word(text, &snippet);
    }

    #[test]
    fn snippet_falls_back_gracefully_with_no_whitespace_to_snap_to() {
        // One giant "word" longer than the whole window: snapping can't find
        // whitespace, so it must fall back to the char-boundary-safe window
        // instead of panicking or returning an empty string.
        let text = "x".repeat(50) + "keyword" + &"y".repeat(500);
        let snippet = snippet_around(&text, "keyword");
        assert!(!snippet.is_empty());
        assert!(snippet.contains("keyword"));
    }
}
