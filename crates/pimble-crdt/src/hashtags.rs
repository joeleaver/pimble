//! Tags from a note's text (docs/HASHTAGS_CONTRACT.md).
//!
//! A note's tags are the tags entered for it (`node.tags`) plus the
//! #hashtags in its rich text. The text tags are worked out here, from the
//! text, whenever a node is read; nothing stores them. Removing one (the ×
//! on a text tag, or setting a node's tags without it) deletes every
//! occurrence of it from the text, each with one adjacent space, as plain
//! yrs deletions of the text runs: an edit that merges with anyone typing.
//!
//! The text is read with yrs directly, the way `links.rs` reads anchors, so
//! reading the tags of a node costs one pass over its textblocks' strings
//! (and a second, formatted pass only over a textblock that holds a `#`).
//! Code blocks are skipped, and so are the characters of inline code.
//! Tables are read row by row in column order, live rows and columns only.

use yrs::types::text::YChange;
use yrs::types::Attrs;
use yrs::{Any, Array, ArrayRef, GetString, Map, MapRef, Out, ReadTxn, Text, TextRef, TransactionMut};

/// rinch-editor-collab's projection keys (its `projection.rs` and `table.rs`).
const TYPE: &str = "type";
const TEXT: &str = "text";
const CONTENT: &str = "content";
const COLS: &str = "cols";
const ROWS: &str = "rows";
const CELLS: &str = "cells";
const ID: &str = "id";
const DELETED: &str = "deleted";
const CODE_BLOCK: &str = "code_block";
/// The inline-code mark: a formatting attribute of the text it covers.
const CODE_MARK: &str = "code";
/// An inline atom (an image, a hard break) is one U+FFFC in its text.
const ATOM: char = '\u{FFFC}';

/// What may come right before a `#` that starts a hashtag, besides the start
/// of a text run, whitespace and an inline atom: opening punctuation.
const OPENERS: &[char] = &['(', '[', '{', '"', '\'', '\u{201C}', '\u{2018}', '\u{00AB}', '\u{2039}', '<', '\u{00BF}', '\u{00A1}'];

/// A hashtag found in one run of characters: the `#` at `start` and the tag's
/// last character before `end` (char indexes), and the tag without `#`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    pub start: usize,
    pub end: usize,
    pub tag: String,
}

/// Whether `c` can be inside a hashtag after its first letter.
fn continues(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '/'
}

/// The hashtags in `chars`, where `masked[i]` says character `i` is not text
/// a hashtag can use (inline code, an atom's placeholder): it ends a hashtag
/// and is never part of one, and a `#` may follow it.
pub(crate) fn scan(chars: &[char], masked: &[bool]) -> Vec<Found> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let usable = |k: usize| !masked.get(k).copied().unwrap_or(true);
        if chars[i] != '#' || !usable(i) {
            i += 1;
            continue;
        }
        let opens = i == 0 || {
            let p = chars[i - 1];
            !usable(i - 1) || p.is_whitespace() || p == ATOM || OPENERS.contains(&p)
        };
        if !opens || !(usable(i + 1) && chars.get(i + 1).is_some_and(|c| c.is_alphabetic())) {
            i += 1;
            continue;
        }
        let mut end = i + 2;
        while end < chars.len() && usable(end) && continues(chars[end]) {
            end += 1;
        }
        // A trailing `-` or `/` is not part of it.
        while matches!(chars[end - 1], '-' | '/') {
            end -= 1;
        }
        out.push(Found { start: i, end, tag: chars[i + 1..end].iter().collect() });
        i = end;
    }
    out
}

/// The hashtags in plain `text`, in order, each as written (repeats kept).
pub fn hashtags_in(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let masked = vec![false; chars.len()];
    scan(&chars, &masked).into_iter().map(|f| f.tag).collect()
}

/// Whether two tags are one tag: compared without regard to case.
pub fn same_tag(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

/// `tags` with each tag once, the first spelling kept.
pub(crate) fn dedup(tags: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        if !out.iter().any(|t| same_tag(t, &tag)) {
            out.push(tag);
        }
    }
    out
}

/// A note's tags as everything reads them: the entered tags first, then each
/// text tag no entered tag already is, in the order they first appear.
pub fn union(entered: &[String], text: &[String]) -> Vec<String> {
    let mut out = entered.to_vec();
    for tag in text {
        if !out.iter().any(|t| same_tag(t, tag)) {
            out.push(tag.clone());
        }
    }
    out
}

/// What setting a note's tags to `wanted` writes (docs/HASHTAGS_CONTRACT.md
/// "Removing one"), given its entered tags and its text tags now: the entered
/// tags to store (the entered ones still wanted, then each wanted tag that is
/// neither entered nor in the text, in `wanted`'s order), and the text tags
/// left out of `wanted`, to remove from the text. A text tag in `wanted` is
/// never copied into the entered tags.
pub fn split_wanted(wanted: &[String], entered: &[String], text: &[String]) -> (Vec<String>, Vec<String>) {
    let wanted = dedup(wanted.iter().map(|t| t.trim().trim_start_matches('#').trim().to_string()).filter(|t| !t.is_empty()));
    let has = |list: &[String], tag: &str| list.iter().any(|t| same_tag(t, tag));
    let mut keep: Vec<String> = Vec::new();
    for tag in &wanted {
        if let Some(e) = entered.iter().find(|e| same_tag(e, tag)) {
            if !has(&keep, e) {
                keep.push(e.clone());
            }
        } else if !has(text, tag) {
            keep.push(tag.clone());
        }
    }
    let remove = text.iter().filter(|t| !has(&wanted, t)).cloned().collect();
    (keep, remove)
}

// ── Reading the text ────────────────────────────────────────────────────

/// The `text` of every textblock under `content` a hashtag can be in (not a
/// code block), in document order, at any depth, table cells included.
pub(crate) fn textblocks<T: ReadTxn>(txn: &T, content: &ArrayRef) -> Vec<TextRef> {
    let mut out = Vec::new();
    walk_list(txn, content, &mut out);
    out
}

fn walk_list<T: ReadTxn>(txn: &T, list: &ArrayRef, out: &mut Vec<TextRef>) {
    for item in list.iter(txn) {
        if let Out::YMap(node) = item {
            walk_node(txn, &node, out);
        }
    }
}

fn walk_node<T: ReadTxn>(txn: &T, node: &MapRef, out: &mut Vec<TextRef>) {
    if let Some(Out::YText(text)) = node.get(txn, TEXT) {
        let code = matches!(node.get(txn, TYPE), Some(Out::Any(Any::String(t))) if &*t == CODE_BLOCK);
        if !code {
            out.push(text);
        }
        return;
    }
    if let Some(Out::YArray(children)) = node.get(txn, CONTENT) {
        walk_list(txn, &children, out);
        return;
    }
    // A table: its live rows in order, each row's cells in live column order.
    let (Some(Out::YArray(cols)), Some(Out::YArray(rows))) = (node.get(txn, COLS), node.get(txn, ROWS)) else { return };
    let live = |line: &MapRef| !matches!(line.get(txn, DELETED), Some(Out::Any(Any::Bool(true))));
    let col_ids: Vec<String> = cols
        .iter(txn)
        .filter_map(|c| match c {
            Out::YMap(col) if live(&col) => match col.get(txn, ID) {
                Some(Out::Any(Any::String(id))) => Some(id.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    for row in rows.iter(txn) {
        let Out::YMap(row) = row else { continue };
        if !live(&row) {
            continue;
        }
        let Some(Out::YMap(cells)) = row.get(txn, CELLS) else { continue };
        for id in &col_ids {
            if let Some(Out::YMap(cell)) = cells.get(txn, id) {
                if let Some(Out::YArray(blocks)) = cell.get(txn, CONTENT) {
                    walk_list(txn, &blocks, out);
                }
            }
        }
    }
}

/// One textblock's characters, which of them a hashtag cannot use (inline
/// code, an atom), and each character's UTF-16 offset in the yrs text (with
/// the text's length after the last). `None` when it holds no `#`, the
/// common case, which costs only the string.
fn read_text<T: ReadTxn>(txn: &T, text: &TextRef) -> Option<(Vec<char>, Vec<bool>, Vec<u32>)> {
    if !text.get_string(txn).contains('#') {
        return None;
    }
    let mut chars = Vec::new();
    let mut masked = Vec::new();
    for chunk in text.diff(txn, YChange::identity) {
        let code = chunk.attributes.as_deref().is_some_and(is_code);
        match chunk.insert {
            Out::Any(Any::String(s)) => {
                for c in s.chars() {
                    chars.push(c);
                    masked.push(code || c == ATOM);
                }
            }
            // An embedded value (never written by the projection) is one
            // place in the text, not text.
            _ => {
                chars.push(ATOM);
                masked.push(true);
            }
        }
    }
    let mut offsets = Vec::with_capacity(chars.len() + 1);
    let mut at = 0u32;
    for c in &chars {
        offsets.push(at);
        at += c.len_utf16() as u32;
    }
    offsets.push(at);
    Some((chars, masked, offsets))
}

fn is_code(attrs: &Attrs) -> bool {
    attrs.iter().any(|(k, v)| &**k == CODE_MARK && !matches!(v, Any::Null | Any::Undefined | Any::Bool(false)))
}

/// Every hashtag in the content, each as written, in document order
/// (repeats kept).
pub(crate) fn all_in<T: ReadTxn>(txn: &T, content: &ArrayRef) -> Vec<String> {
    let mut out = Vec::new();
    for text in textblocks(txn, content) {
        if let Some((chars, masked, _)) = read_text(txn, &text) {
            out.extend(scan(&chars, &masked).into_iter().map(|f| f.tag));
        }
    }
    out
}

/// The text tags of the content: each hashtag once, as first written, in the
/// order they first appear.
pub(crate) fn tags_in<T: ReadTxn>(txn: &T, content: &ArrayRef) -> Vec<String> {
    dedup(all_in(txn, content))
}

/// Delete every occurrence of each of `tags` (any case) from the content,
/// each with one adjacent space (the one after it, else the one before it)
/// so no double space is left. Answers how many occurrences went.
pub(crate) fn remove(txn: &mut TransactionMut, content: &ArrayRef, tags: &[String]) -> usize {
    if tags.is_empty() {
        return 0;
    }
    let wanted: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();
    let mut removed = 0;
    for text in textblocks(&*txn, content) {
        let Some((chars, masked, offsets)) = read_text(&*txn, &text) else { continue };
        let space = |k: usize| chars[k] == ' ' && !masked[k];
        // Ranges in char indexes, worked out front to back so a space is
        // never taken twice, then deleted back to front so each index holds.
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut free_from = 0;
        for found in scan(&chars, &masked) {
            if !wanted.contains(&found.tag.to_lowercase()) {
                continue;
            }
            let (mut start, mut end) = (found.start, found.end);
            if end < chars.len() && space(end) {
                end += 1;
            } else if start > free_from && space(start - 1) {
                start -= 1;
            }
            ranges.push((start, end));
            free_from = end;
        }
        for &(start, end) in ranges.iter().rev() {
            text.remove_range(txn, offsets[start], offsets[end] - offsets[start]);
        }
        removed += ranges.len();
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(text: &str) -> Vec<String> {
        hashtags_in(text)
    }

    #[test]
    fn what_counts_as_a_hashtag() {
        assert_eq!(tags("#pimble"), ["pimble"]);
        assert_eq!(tags("a note about #pimble today"), ["pimble"]);
        assert_eq!(tags("(#one) [#two] \"#three\""), ["one", "two", "three"]);
        assert_eq!(tags("#a_b #c-d #e/f #g1"), ["a_b", "c-d", "e/f", "g1"]);
        assert_eq!(tags("#trailing- #slash/ #both-/"), ["trailing", "slash", "both"]);
        assert_eq!(tags("#tag. #tag, #tag!"), ["tag", "tag", "tag"]);
        assert_eq!(tags("#café #日本 #Ωmega"), ["café", "日本", "Ωmega"]);
        assert_eq!(tags("#1 #! a#b # lone ## #-x"), Vec::<String>::new());
        assert_eq!(tags("##double"), Vec::<String>::new());
        assert_eq!(tags("#a#b"), ["a"]);
        assert_eq!(tags("tab\t#t and\u{a0}#nbsp"), ["t", "nbsp"]);
    }

    #[test]
    fn masked_characters_end_and_open() {
        let chars: Vec<char> = "x#codey #after".chars().collect();
        // `#codey` is inline code (masked): not a hashtag, and the `#` after
        // the space still is.
        let mut masked = vec![false; chars.len()];
        for m in masked.iter_mut().take(7).skip(1) {
            *m = true;
        }
        assert_eq!(scan(&chars, &masked).into_iter().map(|f| f.tag).collect::<Vec<_>>(), ["after"]);
        // A hashtag right after inline code counts; one running into it stops.
        let chars: Vec<char> = "ab#tag".chars().collect();
        assert_eq!(scan(&chars, &[true, true, false, false, false, false])[0].tag, "tag");
        let chars: Vec<char> = "#tagxy".chars().collect();
        assert_eq!(scan(&chars, &[false, false, false, false, true, true])[0].tag, "tag");
    }

    #[test]
    fn union_and_split() {
        let s = |v: &[&str]| v.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(union(&s(&["pimble", "work"]), &s(&["Pimble", "home"])), s(&["pimble", "work", "home"]));
        // Entered tags kept as entered; a text tag in the list is not copied;
        // a text tag left out goes from the text; a new one is entered.
        let (entered, remove) = split_wanted(&s(&["Work", "home", "new"]), &s(&["work", "old"]), &s(&["home", "gone"]));
        assert_eq!(entered, s(&["work", "new"]));
        assert_eq!(remove, s(&["gone"]));
        // Both entered and in the text, left out: loses both.
        let (entered, remove) = split_wanted(&s(&[]), &s(&["both"]), &s(&["Both"]));
        assert!(entered.is_empty());
        assert_eq!(remove, s(&["Both"]));
        // Both, kept: stays entered, and stays in the text.
        let (entered, remove) = split_wanted(&s(&["BOTH"]), &s(&["both"]), &s(&["Both"]));
        assert_eq!(entered, s(&["both"]));
        assert!(remove.is_empty());
    }

    // ── On node documents ───────────────────────────────────────────────

    use crate::NodeDoc;

    const T0: &str = "2026-11-20T10:00:00Z";
    const T1: &str = "2026-11-20T11:00:00Z";

    fn note(md: &str) -> NodeDoc {
        let mut doc = NodeDoc::new();
        doc.init("document", "Note", None, T0).unwrap();
        doc.replace_content_markdown(md).unwrap();
        doc
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn text_tags_come_from_every_kind_of_block_but_code() {
        let doc = note(
            "# Plan for #Pimble\n\nA paragraph with #pimble again and `#notcode` inline, then [#linked](https://example.com/#anchor).\n\n\
             - a list item #listed\n  - nested #deeper\n\n> a quote #quoted\n\n\
             | Name | Tag |\n| --- | --- |\n| cell | #celled |\n\n```\n#fenced code\n```\n",
        );
        assert_eq!(doc.text_tags(), strs(&["Pimble", "linked", "listed", "deeper", "quoted", "celled"]));
        assert_eq!(doc.text_tag_counts()[0], ("Pimble".to_string(), 2));
    }

    #[test]
    fn the_node_has_the_union_entered_first() {
        let mut doc = note("About #pimble and #home.");
        doc.set_tags(&strs(&["Pimble", "work"]), T1).unwrap();
        let fields = doc.fields().unwrap();
        assert_eq!(fields.tags, strs(&["Pimble", "work"]), "the entered tags as stored");
        assert_eq!(fields.text_tags, strs(&["pimble", "home"]));
        let node = fields.into_node(pimble_core::NodeId::new());
        assert_eq!(node.metadata.tags, strs(&["Pimble", "work", "home"]), "shown once, as entered");
        assert_eq!(node.metadata.text_tags, strs(&["pimble", "home"]));
        assert!(doc.stored_fields().unwrap().text_tags.is_empty(), "the cheap read skips the text");
    }

    #[test]
    fn entered_tags_never_change_because_of_text() {
        let mut doc = note("Start.");
        doc.set_tags(&strs(&["kept"]), T1).unwrap();
        doc.append_markdown("Now #kept and #more.").unwrap();
        doc.replace_content_markdown("Nothing tagged here.").unwrap();
        let fields = doc.fields().unwrap();
        assert_eq!(fields.tags, strs(&["kept"]));
        assert!(fields.text_tags.is_empty());
    }

    #[test]
    fn removing_a_hashtag_takes_every_occurrence_and_one_space() {
        let mut doc = note("Call #Bob today.\n\nThen #bob, and #bobby #bob\n\nkeep `#bob` here");
        assert_eq!(doc.remove_hashtag("BOB").unwrap(), 3);
        assert_eq!(doc.markdown().unwrap().trim(), "Call today.\n\nThen, and #bobby\n\nkeep `#bob` here");
        assert_eq!(doc.remove_hashtag("bob").unwrap(), 0, "the inline code is not a hashtag");
        let mut doc = note("#only");
        doc.remove_hashtag("only").unwrap();
        assert!(doc.text_tags().is_empty());
        assert_eq!(doc.text().trim(), "");
    }

    #[test]
    fn removing_keeps_marks_on_the_text_around_it() {
        let mut doc = note("Some **bold #tag words** stay.");
        doc.remove_hashtag("tag").unwrap();
        assert_eq!(doc.markdown().unwrap().trim(), "Some **bold words** stay.");
    }

    #[test]
    fn removal_merges_with_a_concurrent_edit() {
        let base = note("One #draft here.\n\nTwo #draft there.");
        let mut a = NodeDoc::load(&base.save()).unwrap();
        let mut b = NodeDoc::load(&base.save()).unwrap();
        let before = a.state_vector();
        a.remove_hashtag("draft").unwrap();
        let from_a = a.diff_since(&before).unwrap();
        // Meanwhile b types into the same paragraphs.
        let from_b = b.replace_text("there", "there, typed meanwhile").unwrap();
        a.apply_update(&from_b).unwrap();
        b.apply_update(&from_a).unwrap();
        assert_eq!(a.markdown().unwrap(), b.markdown().unwrap());
        assert_eq!(a.markdown().unwrap().trim(), "One here.\n\nTwo there, typed meanwhile.");
        assert!(a.text_tags().is_empty());
    }

    #[test]
    fn a_hashtag_typed_on_one_device_is_a_tag_on_another() {
        let base = note("Plain.");
        let mut a = NodeDoc::load(&base.save()).unwrap();
        let mut b = NodeDoc::load(&base.save()).unwrap();
        let update = a.append_markdown("Now #shared.").unwrap();
        b.apply_update(&update).unwrap();
        assert_eq!(b.fields().unwrap().into_node(pimble_core::NodeId::new()).metadata.tags, strs(&["shared"]));
    }

    #[test]
    fn setting_tags_writes_entered_ones_and_removes_text_ones_left_out() {
        let mut doc = note("Text has #alpha and #beta.");
        doc.set_tags(&strs(&["entered"]), T0).unwrap();
        // The list as Properties or set_tags gives it: alpha kept (not
        // copied), beta left out (removed from the text), entered dropped,
        // new added.
        doc.set_all_tags(&strs(&["ALPHA", "new"]), T1).unwrap();
        let fields = doc.fields().unwrap();
        assert_eq!(fields.tags, strs(&["new"]));
        assert_eq!(fields.text_tags, strs(&["alpha"]));
        assert_eq!(doc.markdown().unwrap().trim(), "Text has #alpha and.");
        assert_eq!(fields.all_tags(), strs(&["new", "alpha"]));
    }
}
