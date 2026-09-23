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
    /// "Remove Link": Ctrl+L with the caret in a link.
    RemoveLink,
    /// "Link to <href>": what was typed reads as a web link.
    Web { href: String },
    /// A note the search found.
    Node { store_id: StoreId, node_id: NodeId, title: String, store_name: Option<String> },
}

impl PickerRow {
    /// The link's href.
    pub fn href(&self) -> String {
        match self {
            PickerRow::RemoveLink => String::new(),
            PickerRow::Web { href } => href.clone(),
            PickerRow::Node { store_id, node_id, .. } => PimbleUrl::node(*store_id, *node_id).to_string(),
        }
    }

    /// The words a `[[` pick puts in the text: a note's title, or the URL
    /// as it was typed.
    pub fn link_text(&self, query: &str) -> String {
        match self {
            PickerRow::RemoveLink => String::new(),
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

/// How the picker was opened, which says where its query comes from and
/// what a pick does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// `[[` typed at `from` (the first bracket): the query is the text
    /// typed after the brackets, in the note, and a pick replaces
    /// `[[query` with the linked title.
    Typed { from: usize },
    /// Ctrl+L over `from..to`: the query is typed in the popup's own field,
    /// and a pick links those words as they are. `editing` when they are a
    /// link already (then "Remove Link" is offered too).
    Selection { from: usize, to: usize, editing: bool },
}

/// The open picker, as `AppStore::link_picker` holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerView {
    pub kind: PickerKind,
    pub query: String,
    pub rows: Vec<PickerRow>,
    pub selected: usize,
    /// Where the popup sits (`position: fixed` coordinates): under the caret
    /// or the selection's start.
    pub x: f32,
    pub y: f32,
}

thread_local! {
    /// The picker's latest search answer, which the rows are rebuilt from
    /// as the query changes before the next answer arrives.
    static HITS: std::cell::RefCell<Vec<SearchResultItem>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The pending search, debounced as the query is typed.
    static SEARCH_TIMER: std::cell::RefCell<Option<rinch::prelude::TimeoutHandle>> = const { std::cell::RefCell::new(None) };
}

/// How long the query rests before the picker searches.
const SEARCH_DEBOUNCE_MS: u32 = 120;

/// Open the picker (`kind`) under `(x, y)`, its query empty.
pub fn open(store: crate::state::AppStore, kind: PickerKind, x: f32, y: f32) {
    HITS.with(|h| h.borrow_mut().clear());
    let view = PickerView { kind, query: String::new(), rows: Vec::new(), selected: 0, x, y };
    store.link_picker.set(Some(rebuilt(store, view)));
}

/// Close the picker; what was typed stays as it is.
pub fn close(store: crate::state::AppStore) {
    if let Some(pending) = SEARCH_TIMER.with(|t| t.borrow_mut().take()) {
        rinch::prelude::clear_timeout(pending);
    }
    if rinch::prelude::untracked(|| store.link_picker.get()).is_some() {
        store.link_picker.set(None);
    }
}

/// Whether the picker is open.
pub fn is_open(store: crate::state::AppStore) -> bool {
    rinch::prelude::untracked(|| store.link_picker.with(|p| p.is_some()))
}

/// The query changed: the rows follow at once from the last answer, and a
/// search for the new query is asked once typing rests.
pub fn set_query(store: crate::state::AppStore, query: String) {
    let Some(mut view) = rinch::prelude::untracked(|| store.link_picker.get()) else { return };
    if view.query == query {
        return;
    }
    view.query = query.clone();
    view.selected = 0;
    store.link_picker.set(Some(rebuilt(store, view)));
    if let Some(pending) = SEARCH_TIMER.with(|t| t.borrow_mut().take()) {
        rinch::prelude::clear_timeout(pending);
    }
    if query.trim().is_empty() {
        return;
    }
    let handle = rinch::prelude::set_timeout(SEARCH_DEBOUNCE_MS, move || {
        SEARCH_TIMER.with(|t| t.borrow_mut().take());
        let stores = rinch::prelude::untracked(|| store.store_ids.get());
        store.send(crate::protocol::BackendCommand::Search {
            query,
            stores,
            limit: 30,
            purpose: crate::protocol::SearchPurpose::LinkPicker,
        });
    });
    SEARCH_TIMER.with(|t| *t.borrow_mut() = Some(handle));
}

/// A search the picker asked for was answered. An answer that comes after
/// the picker closed changes nothing; an error leaves the last rows.
pub fn search_answered(store: crate::state::AppStore, results: &Result<Vec<SearchResultItem>, String>) {
    let Ok(hits) = results else { return };
    let Some(view) = rinch::prelude::untracked(|| store.link_picker.get()) else { return };
    HITS.with(|h| *h.borrow_mut() = hits.clone());
    store.link_picker.set(Some(rebuilt(store, view)));
}

/// Move the highlight (+1 down, -1 up).
pub fn step(store: crate::state::AppStore, delta: isize) {
    let Some(mut view) = rinch::prelude::untracked(|| store.link_picker.get()) else { return };
    view.selected = step_selection(view.selected, view.rows.len(), delta);
    store.link_picker.set(Some(view));
}

/// Move the popup (the caret moved, or its position became known).
pub fn place(store: crate::state::AppStore, x: f32, y: f32) {
    let Some(mut view) = rinch::prelude::untracked(|| store.link_picker.get()) else { return };
    if (view.x, view.y) != (x, y) {
        view.x = x;
        view.y = y;
        store.link_picker.set(Some(view));
    }
}

/// Pick row `index` (the highlighted one for Enter): make the link in
/// `handle`'s document, or remove it, and close. Answers whether anything
/// was picked.
pub fn accept(store: crate::state::AppStore, handle: &EditorHandle, index: usize) -> bool {
    let Some(view) = rinch::prelude::untracked(|| store.link_picker.get()) else { return false };
    let Some(row) = view.rows.get(index).cloned() else { return false };
    close(store);
    match (view.kind, &row) {
        (PickerKind::Selection { from, to, .. }, PickerRow::RemoveLink) => handle.update(move |state| {
            // The mark exactly as it is on the text: removal matches attrs too.
            let link = state.doc.node_at(from)?.marks().iter().find(|m| m.type_name() == "link")?.clone();
            let mut tr = state.tr();
            tr.remove_mark(from, to, link).ok()?;
            Some(tr)
        }),
        (PickerKind::Selection { from, to, .. }, row) => insert_link(handle, from, to, "", &row.href(), true),
        (PickerKind::Typed { from }, row) => {
            let to = handle.selection().head().0;
            insert_link(handle, from, to, &row.link_text(&view.query), &row.href(), false)
        }
    }
}

/// `view`'s rows for its query and the last answer, "Remove Link" first
/// when a link is being edited.
fn rebuilt(store: crate::state::AppStore, mut view: PickerView) -> PickerView {
    let store_name = |id: StoreId| store.get_store_signal(id).map(|sig| rinch::prelude::untracked(|| sig.with(|s| s.name.clone())));
    let mut rows = HITS.with(|h| picker_rows(&view.query, &h.borrow(), store_name));
    if let PickerKind::Selection { editing: true, .. } = view.kind {
        rows.insert(0, PickerRow::RemoveLink);
    }
    view.selected = view.selected.min(rows.len().saturating_sub(1));
    view.rows = rows;
    view
}

/// One line of the popup, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerLine {
    pub key: String,
    pub index: usize,
    pub label: String,
    pub detail: String,
    pub selected: bool,
}

/// The popup's lines for `view`.
pub fn lines(view: &PickerView) -> Vec<PickerLine> {
    view.rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let (label, detail) = match row {
                PickerRow::RemoveLink => ("Remove Link".to_string(), String::new()),
                PickerRow::Web { href } => (format!("Link to {href}"), "opens in the browser".to_string()),
                PickerRow::Node { title, store_name, .. } => (
                    if title.trim().is_empty() { "Untitled".to_string() } else { title.clone() },
                    store_name.clone().unwrap_or_default(),
                ),
            };
            PickerLine { key: format!("{index}:{label}"), index, label, detail, selected: index == view.selected }
        })
        .collect()
}

/// What the popup says with no rows.
pub fn empty_line(view: &PickerView) -> &'static str {
    if view.query.trim().is_empty() {
        "Type to find a note, or a web address"
    } else {
        "No matches"
    }
}

/// Ctrl/Cmd+L in the editor: link the selection, or edit the link the
/// caret is in; nothing with a bare caret outside a link.
pub fn start_from_selection(store: crate::state::AppStore, handle: &EditorHandle) {
    let selection = handle.selection();
    let (from, to) = (selection.from(), selection.to());
    let (from, to, editing) = if from < to {
        (from, to, handle.link_at(from).is_some())
    } else if let Some(link) = handle.link_at(from) {
        handle.set_selection(rinch_editor_core::selection::Selection::text(link.from, link.to));
        (link.from, link.to, true)
    } else {
        return;
    };
    let (x, y) = below(handle.caret_rect(from).map(|r| (r.x, r.y, r.height)));
    open(store, PickerKind::Selection { from: from.0, to: to.0, editing }, x, y);
}

/// `[[` was typed with its first bracket at `from`.
pub fn start_typed(store: crate::state::AppStore, handle: &EditorHandle, from: usize) {
    let (x, y) = below(handle.caret_rect(handle.selection().head()).map(|r| (r.x, r.y, r.height)));
    open(store, PickerKind::Typed { from }, x, y);
    // The picker opens a turn after the brackets: whatever was typed after
    // them meanwhile is already its query.
    match typed_query(&handle.doc(), from, handle.selection()) {
        Some(query) => set_query(store, query),
        None => close(store),
    }
}

/// The editor's selection changed: a `[[` picker follows what is typed
/// after the brackets and closes when the caret leaves them; a Ctrl+L
/// picker closes when the selection moves at all.
pub fn selection_changed(store: crate::state::AppStore, handle: &EditorHandle) {
    let Some(view) = rinch::prelude::untracked(|| store.link_picker.get()) else { return };
    match view.kind {
        PickerKind::Selection { .. } => close(store),
        PickerKind::Typed { from } => match typed_query(&handle.doc(), from, handle.selection()) {
            Some(query) => set_query(store, query),
            None => close(store),
        },
    }
}

/// The query of a `[[` picker whose brackets start at `from`, with the
/// caret at `selection`: what follows the brackets, in the same textblock,
/// up to the caret. `None` when the caret left it (before the brackets,
/// another block, a range), the brackets are gone, or it grew past a line's
/// worth.
pub fn typed_query(doc: &rinch_editor_core::Node, from: usize, selection: rinch_editor_core::selection::Selection) -> Option<String> {
    let head = selection.head().0;
    if selection.from() != selection.to() || head < from + 2 {
        return None;
    }
    let (Ok(start), Ok(end)) = (doc.resolve(rinch_editor_core::Pos(from)), doc.resolve(rinch_editor_core::Pos(head))) else {
        return None;
    };
    if !start.parent().same_ref(end.parent()) {
        return None;
    }
    let parent = start.parent();
    let mut block = String::new();
    for i in 0..parent.child_count() {
        match parent.child(i).text() {
            Some(t) => block.push_str(t),
            None => block.push('\u{fffc}'),
        }
    }
    let text: String = block.chars().skip(start.parent_offset()).take(end.parent_offset() - start.parent_offset()).collect();
    if !text.starts_with("[[") {
        return None;
    }
    let query: String = text.chars().skip(2).collect();
    (query.chars().count() <= 80 && !query.contains("]]")).then_some(query)
}

/// A key the editor offers first: the open picker takes the ones that
/// drive it, and in a Ctrl+L picker the typing that makes its query.
/// Answers whether it took the key.
pub fn key(store: crate::state::AppStore, handle: &EditorHandle, key: &str, modified: bool) -> bool {
    let Some(view) = rinch::prelude::untracked(|| store.link_picker.get()) else { return false };
    match key {
        "ArrowDown" => step(store, 1),
        "ArrowUp" => step(store, -1),
        "Escape" => close(store),
        "Enter" | "Tab" if !view.rows.is_empty() => {
            accept(store, handle, view.selected);
        }
        "Enter" | "Tab" => close(store),
        _ => match view.kind {
            // A `[[` query is typed in the note itself.
            PickerKind::Typed { .. } => return false,
            PickerKind::Selection { .. } if key == "Backspace" => {
                let mut query = view.query.clone();
                query.pop();
                set_query(store, query);
            }
            PickerKind::Selection { .. } if !modified && key.chars().count() == 1 => {
                set_query(store, format!("{}{key}", view.query));
            }
            // Anything else (a shortcut, a caret move) closes it and goes on.
            PickerKind::Selection { .. } => {
                close(store);
                return false;
            }
        },
    }
    true
}

/// The caret moved after layout (its rectangle is known now): a `[[`
/// picker sits under it.
pub fn caret_moved(store: crate::state::AppStore, handle: &EditorHandle) {
    let Some(PickerView { kind: PickerKind::Typed { .. }, .. }) = rinch::prelude::untracked(|| store.link_picker.get()) else { return };
    if let Some(rect) = handle.caret_rect(handle.selection().head()) {
        let (x, y) = below(Some((rect.x, rect.y, rect.height)));
        place(store, x, y);
    }
}

/// Just under a caret rectangle `(x, y, height)`; the top left when there is
/// none yet.
fn below(rect: Option<(f32, f32, f32)>) -> (f32, f32) {
    rect.map(|(x, y, h)| (x, y + h + 4.0)).unwrap_or((0.0, 0.0))
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
    let (text, href) = (text.to_string(), href.to_string());
    handle.update(move |state| link_transaction(state, from, to, &text, &href, keep_text))
}

/// The transaction [`insert_link`] dispatches, for whoever holds the state
/// rather than the handle (the paste hook, which runs mid-dispatch).
pub fn link_transaction(
    state: &rinch_editor_core::state::EditorState,
    from: usize,
    to: usize,
    text: &str,
    href: &str,
    keep_text: bool,
) -> Option<rinch_editor_core::state::Transaction> {
    let link_type = state.schema().mark_type("link")?.clone();
    let link = Mark::new(link_type, Attrs::from_iter([("href", AttrValue::from(href.to_string()))]));
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
    let node = state.schema().text_with_marks(text, marks).ok()?;
    let end = from + node.text_len();
    tr.replace_with(from, to, Fragment::from_node(node)).ok()?;
    tr.set_selection(rinch_editor_core::selection::Selection::cursor(rinch_editor_core::Pos(end)));
    tr.set_stored_marks(Some(kept));
    Some(tr)
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
    fn a_typed_pick_follows_the_answer_and_links_the_title() {
        let store = crate::state::AppStore::new();
        let handle = crate::rinch_editor::create_editor();
        typed(&handle, "see [[pla");
        let (s, a, b) = (StoreId::new(), NodeId::new(), NodeId::new());

        open(store, PickerKind::Typed { from: 5 }, 10.0, 20.0);
        set_query(store, "pla".into());
        assert!(store.link_picker.get().unwrap().rows.is_empty(), "nothing answered yet");
        search_answered(store, &Ok(vec![hit(s, a, "Plan"), hit(s, b, "Plants")]));
        step(store, 1);
        let view = store.link_picker.get().unwrap();
        assert_eq!((view.rows.len(), view.selected, view.x, view.y), (2, 1, 10.0, 20.0));

        assert!(accept(store, &handle, 1));
        assert_eq!(store.link_picker.get(), None, "a pick closes it");
        let href = format!("pimble:{s}/{b}");
        assert_eq!(runs(&handle), vec![("see ".into(), None), ("Plants".into(), Some(href))]);

        // An answer after it closed changes nothing.
        search_answered(store, &Ok(vec![hit(s, a, "Plan")]));
        assert_eq!(store.link_picker.get(), None);
    }

    #[test]
    fn a_selection_is_linked_as_it_is_and_an_edited_link_can_be_removed() {
        let store = crate::state::AppStore::new();
        let handle = crate::rinch_editor::create_editor();
        typed(&handle, "read the plan today");
        open(store, PickerKind::Selection { from: 6, to: 14, editing: false }, 0.0, 0.0);
        set_query(store, "example.com".into());
        assert!(accept(store, &handle, 0));
        assert_eq!(
            runs(&handle),
            vec![("read ".into(), None), ("the plan".into(), Some("https://example.com".into())), (" today".into(), None)]
        );

        open(store, PickerKind::Selection { from: 6, to: 14, editing: true }, 0.0, 0.0);
        assert_eq!(store.link_picker.get().unwrap().rows, vec![PickerRow::RemoveLink]);
        assert!(accept(store, &handle, 0));
        assert_eq!(runs(&handle), vec![("read the plan today".into(), None)]);
    }

    #[test]
    fn the_typed_query_is_what_follows_the_brackets_up_to_the_caret() {
        use rinch_editor_core::selection::Selection;
        use rinch_editor_core::Pos;
        let handle = crate::rinch_editor::create_editor();
        typed(&handle, "see [[ven dors");
        let doc = handle.doc();
        // The brackets are 5..7; the caret moves along the text.
        assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(7))).as_deref(), Some(""));
        assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(10))).as_deref(), Some("ven"));
        assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(15))).as_deref(), Some("ven dors"));
        assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(6))), None, "inside the brackets");
        assert_eq!(typed_query(&doc, 5, Selection::cursor(Pos(3))), None, "before them");
        assert_eq!(typed_query(&doc, 5, Selection::text(Pos(7), Pos(10))), None, "a range");
        assert_eq!(typed_query(&doc, 4, Selection::cursor(Pos(10))), None, "no brackets there");
    }

    #[test]
    fn the_highlight_wraps_at_both_ends() {
        assert_eq!(step_selection(0, 3, -1), 2);
        assert_eq!(step_selection(2, 3, 1), 0);
        assert_eq!(step_selection(1, 3, 1), 2);
        assert_eq!(step_selection(0, 0, 1), 0);
    }
}
