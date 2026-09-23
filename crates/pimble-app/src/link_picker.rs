//! The link picker's logic (docs/LINKS_CONTRACT.md "Making a link" and "One
//! experience for both kinds"): what its rows are for a query, and how a
//! picked row becomes a link in the editor. The popup that shows the rows,
//! where it sits and which keys drive it are the UI's; nothing here draws.

use pimble_core::{NodeId, PimbleUrl, StoreId};
use pimble_rpc::SearchResultItem;
use rinch_editor_core::{AttrValue, Attrs, Fragment, Mark};

use crate::links::web_url;
use crate::rinch_editor::EditorHandle;

/// How many notes the picker lists.
pub const MAX_NODE_ROWS: usize = 8;

/// One row of the picker.
#[derive(Debug, Clone, PartialEq)]
pub enum PickerRow {
    /// "Link to <href>": what was typed reads as a web link.
    Web { href: String },
    /// A note the search found.
    Node { store_id: StoreId, node_id: NodeId, title: String, store_name: Option<String> },
}

impl PickerRow {
    /// The link's href.
    pub fn href(&self) -> String {
        match self {
            PickerRow::Web { href } => href.clone(),
            PickerRow::Node { store_id, node_id, .. } => PimbleUrl::node(*store_id, *node_id).to_string(),
        }
    }

    /// The words a `[[` pick puts in the text: a note's title, or the URL
    /// as it was typed.
    pub fn link_text(&self, query: &str) -> String {
        match self {
            PickerRow::Web { .. } => query.trim().to_string(),
            PickerRow::Node { title, .. } if title.trim().is_empty() => "Untitled".to_string(),
            PickerRow::Node { title, .. } => title.clone(),
        }
    }
}

/// The rows for `query` given the search's `hits`, best first: a web link
/// row when the query reads as one, then each note once (search answers a
/// hit per matching chunk), at most [`MAX_NODE_ROWS`].
pub fn picker_rows(query: &str, hits: &[SearchResultItem], store_name: impl Fn(StoreId) -> Option<String>) -> Vec<PickerRow> {
    let mut rows = Vec::new();
    if let Some(href) = web_url(query) {
        rows.push(PickerRow::Web { href });
    }
    let mut seen = std::collections::HashSet::new();
    for hit in hits {
        if rows.len() >= MAX_NODE_ROWS + 1 || !seen.insert((hit.store_id, hit.node_id)) {
            continue;
        }
        rows.push(PickerRow::Node {
            store_id: hit.store_id,
            node_id: hit.node_id,
            title: hit.title.clone(),
            store_name: store_name(hit.store_id),
        });
    }
    rows.truncate(MAX_NODE_ROWS + usize::from(matches!(rows.first(), Some(PickerRow::Web { .. }))));
    rows
}

/// The highlighted row after `step` (+1 down, -1 up) from `selected` among
/// `len` rows, wrapping at both ends.
pub fn step_selection(selected: usize, len: usize, step: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (selected as isize + step).rem_euclid(len as isize) as usize
}

/// Make the link a picked row stands for, in one editor transaction (one
/// undo step).
///
/// - `keep_text`: the words in `from..to` stay and are linked (Ctrl+L over a
///   selection).
/// - otherwise `from..to` (the `[[query` run) is replaced by `text`, linked,
///   and the caret goes after it with the marks that were there but the
///   link, so what is typed next is not linked.
///
/// Answers whether the editor took the change.
pub fn insert_link(handle: &EditorHandle, from: usize, to: usize, text: &str, href: &str, keep_text: bool) -> bool {
    let href = href.to_string();
    let text = text.to_string();
    handle.update(move |state| {
        let link_type = state.schema().mark_type("link")?.clone();
        let link = Mark::new(link_type, Attrs::from_iter([("href", AttrValue::from(href))]));
        let mut tr = state.tr();
        if keep_text {
            tr.add_mark(from, to, link).ok()?;
            return Some(tr);
        }
        let kept: Vec<Mark> = state
            .doc
            .resolve(rinch_editor_core::Pos(from))
            .ok()?
            .marks()
            .into_iter()
            .filter(|m| m.type_name() != "link")
            .collect();
        let mut marks = kept.clone();
        marks.push(link);
        let node = state.schema().text_with_marks(&text, marks).ok()?;
        let end = from + node.text_len();
        tr.replace_with(from, to, Fragment::from_node(node)).ok()?;
        tr.set_selection(rinch_editor_core::selection::Selection::cursor(rinch_editor_core::Pos(end)));
        tr.set_stored_marks(Some(kept));
        Some(tr)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(store_id: StoreId, node_id: NodeId, title: &str) -> SearchResultItem {
        SearchResultItem {
            node_id,
            store_id,
            score: 1.0,
            title: title.into(),
            snippet: String::new(),
            kind: "prose".into(),
            node_type: "document".into(),
            path: String::new(),
        }
    }

    #[test]
    fn a_url_is_offered_first_and_each_note_once() {
        let (s, a, b) = (StoreId::new(), NodeId::new(), NodeId::new());
        let hits = vec![hit(s, a, "Plan"), hit(s, a, "Plan"), hit(s, b, "")];
        let rows = picker_rows("example.com/plan", &hits, |_| Some("Notes".into()));
        assert_eq!(rows[0], PickerRow::Web { href: "https://example.com/plan".into() });
        assert_eq!(rows.len(), 3, "the web row, then each note once: {rows:?}");
        assert_eq!(rows[2].link_text("q"), "Untitled");
        assert_eq!(rows[1].href(), format!("pimble:{s}/{a}"));
        assert_eq!(rows[0].link_text(" example.com/plan "), "example.com/plan");

        let plain = picker_rows("plan", &hits, |_| None);
        assert!(matches!(plain[0], PickerRow::Node { .. }), "words are not a URL");
        let many: Vec<_> = (0..20).map(|i| hit(s, NodeId::new(), &format!("n{i}"))).collect();
        assert_eq!(picker_rows("n", &many, |_| None).len(), MAX_NODE_ROWS);
        assert_eq!(picker_rows("n.io", &many, |_| None).len(), MAX_NODE_ROWS + 1, "the web row does not take a note's place");
    }

    /// Each text run of the first paragraph with its link's href, if any.
    fn runs(handle: &EditorHandle) -> Vec<(String, Option<String>)> {
        let doc = handle.doc();
        let para = doc.child(0);
        (0..para.child_count())
            .map(|i| {
                let run = para.child(i);
                let href = run.marks().iter().find(|m| m.type_name() == "link").and_then(|m| m.attrs.get_str("href")).map(str::to_string);
                (run.text().unwrap_or_default().to_string(), href)
            })
            .collect()
    }

    fn typed(handle: &EditorHandle, text: &str) {
        let text = text.to_string();
        assert!(handle.update(move |s| {
            let mut tr = s.tr();
            tr.insert_text(&text).ok()?;
            Some(tr)
        }));
    }

    #[test]
    fn a_pick_replaces_the_query_with_linked_words_and_what_follows_is_not_linked() {
        let handle = crate::rinch_editor::create_editor();
        typed(&handle, "see [[pla");
        // The paragraph's text starts at 1: `[[pla` is 5..10.
        assert!(insert_link(&handle, 5, 10, "Plan", "pimble:x", false));
        typed(&handle, " next");
        assert_eq!(runs(&handle), vec![("see ".into(), None), ("Plan".into(), Some("pimble:x".into())), (" next".into(), None)]);
    }

    #[test]
    fn a_pick_over_a_selection_links_its_words() {
        let handle = crate::rinch_editor::create_editor();
        typed(&handle, "read the plan today");
        assert!(insert_link(&handle, 6, 14, "ignored", "https://example.com", true));
        assert_eq!(
            runs(&handle),
            vec![("read ".into(), None), ("the plan".into(), Some("https://example.com".into())), (" today".into(), None)]
        );
    }

    #[test]
    fn the_highlight_wraps_at_both_ends() {
        assert_eq!(step_selection(0, 3, -1), 2);
        assert_eq!(step_selection(2, 3, 1), 0);
        assert_eq!(step_selection(1, 3, 1), 2);
        assert_eq!(step_selection(0, 0, 1), 0);
    }
}
