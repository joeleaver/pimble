//! The link picker's logic (docs/LINKS_CONTRACT.md "Making a link" and "One
//! experience for both kinds"): what its rows are for a query, and how a
//! picked row becomes a link in the editor. The popup that shows the rows,
//! where it sits and which keys drive it are the UI's; nothing here draws.

use pimble_core::{NodeId, PimbleUrl, StoreId};
use pimble_rpc::SearchResultItem;
use rinch_editor_core::{AttrValue, Attrs, Fragment, Mark};

use crate::web_links::web_url;
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
    /// `@`: a person who can open the note, by the name they go by in its
    /// share (docs/NOTIFICATIONS_CONTRACT.md section 1).
    Person { account: String, name: String },
    /// `@`: "Share this with someone...", the Share dialog for the note.
    ShareThis,
}

impl PickerRow {
    /// The link's href.
    pub fn href(&self) -> String {
        match self {
            PickerRow::RemoveLink => String::new(),
            PickerRow::Web { href } => href.clone(),
            PickerRow::Node { store_id, node_id, .. } => PimbleUrl::node(*store_id, *node_id).to_string(),
            // Without an id: a pick makes the mention, and its id, once.
            PickerRow::Person { account, .. } => pimble_core::mention::href(account, None),
            PickerRow::ShareThis => String::new(),
        }
    }

    /// The words a `[[` pick puts in the text: a note's title, or the URL
    /// as it was typed.
    pub fn link_text(&self, query: &str) -> String {
        match self {
            PickerRow::RemoveLink | PickerRow::ShareThis => String::new(),
            PickerRow::Person { name, .. } => format!("@{name}"),
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
    /// Insert > "Sheet from a Node..." (docs/SHEETS_CONTRACT.md section 1):
    /// the query is typed in the popup, only sheet nodes are offered, and a
    /// pick is embedded live at `at`.
    Sheet { at: usize },
    /// `@` typed at `from` (docs/NOTIFICATIONS_CONTRACT.md section 1): the
    /// query is the text typed after it, in the note; the rows are the people
    /// who can open the note, and a pick replaces `@query` with a mention.
    Mention { from: usize },
}

/// The open picker, as `AppStore::link_picker` holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerView {
    pub kind: PickerKind,
    pub query: String,
    pub rows: Vec<PickerRow>,
    pub selected: usize,
    /// Whether the highlight was moved with the arrows: "Share this with
    /// someone..." is taken by Enter only then, so `@` and Enter in a note
    /// nobody else can open is a new line, not a dialog.
    pub moved: bool,
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
    let view = PickerView { kind, query: String::new(), rows: Vec::new(), selected: 0, moved: false, x, y };
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
    let view = rebuilt(store, view);
    // `@` and words nobody here is called: the picker steps aside and the
    // words are just words.
    if matches!(view.kind, PickerKind::Mention { .. }) {
        let anyone = view.rows.iter().any(|r| matches!(r, PickerRow::Person { .. }));
        if !anyone && !query.is_empty() {
            close(store);
        } else {
            store.link_picker.set(Some(view));
        }
        return;
    }
    store.link_picker.set(Some(view));
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
            shown: rinch::prelude::untracked(|| store.internal_kinds_shown()),
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
    view.moved = true;
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
        (PickerKind::Sheet { at }, PickerRow::Node { store_id, node_id, .. }) => {
            // A query typed into the note goes, replaced by the sheet.
            let to = typed_since(handle, at).filter(|t| *t == view.query).map(|_| handle.selection().head().0);
            crate::sheets::in_text::insert_node_embed(store, store.focused(), *store_id, *node_id, Some((at, to.unwrap_or(at))))
        }
        (PickerKind::Sheet { .. }, _) => false,
        (PickerKind::Mention { from }, PickerRow::Person { account, name }) => {
            let to = handle.selection().head().0;
            let mention = pimble_core::mention::Mention::new(account, name);
            let (text, href) = (mention.words(), mention.href());
            handle.update(move |state| {
                let mut tr = link_transaction(state, from, to, &text, &href, false)?;
                // A space after it, so what is typed next is a new word.
                tr.insert_text(" ").ok()?;
                Some(tr)
            })
        }
        (PickerKind::Mention { .. }, PickerRow::ShareThis) => {
            crate::mentions::share_focused(store);
            true
        }
        (PickerKind::Mention { .. }, _) => false,
    }
}

/// `view`'s rows for its query and the last answer, "Remove Link" first
/// when a link is being edited.
fn rebuilt(store: crate::state::AppStore, mut view: PickerView) -> PickerView {
    let store_name = |id: StoreId| store.get_store_signal(id).map(|sig| rinch::prelude::untracked(|| sig.with(|s| s.name.clone())));
    let mut rows = match view.kind {
        // Sheet nodes only.
        PickerKind::Sheet { .. } => HITS.with(|h| {
            let sheets: Vec<SearchResultItem> = h.borrow().iter().filter(|hit| hit.node_type == pimble_core::node_types::SHEET).cloned().collect();
            picker_rows("", &sheets, store_name)
        }),
        PickerKind::Mention { .. } => crate::mentions::picker_rows(store, &view.query),
        _ => HITS.with(|h| picker_rows(&view.query, &h.borrow(), store_name)),
    };
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
                PickerRow::Person { name, .. } => (name.clone(), String::new()),
                PickerRow::ShareThis => ("Share this with someone...".to_string(), String::new()),
            };
            PickerLine { key: format!("{index}:{label}"), index, label, detail, selected: index == view.selected }
        })
        .collect()
}

/// What the popup says under its rows; empty when it says nothing.
pub fn quiet_line(view: &PickerView) -> &'static str {
    if let PickerKind::Mention { .. } = view.kind {
        let anyone = view.rows.iter().any(|r| matches!(r, PickerRow::Person { .. }));
        return if anyone { "" } else { "Only you can open this note" };
    }
    if view.rows.is_empty() {
        empty_line(view)
    } else {
        ""
    }
}

/// What the popup says with no rows.
pub fn empty_line(view: &PickerView) -> &'static str {
    if let PickerKind::Mention { .. } = view.kind {
        return "Only you can open this note";
    }
    if let PickerKind::Sheet { .. } = view.kind {
        return if view.query.trim().is_empty() { "Type to find a sheet" } else { "No sheets match" };
    }
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

/// `@` was typed at `from` (docs/NOTIFICATIONS_CONTRACT.md section 1): the
/// picker of the people who can open the note, under the caret.
pub fn start_mention(store: crate::state::AppStore, handle: &EditorHandle, from: usize) {
    let (x, y) = below(handle.caret_rect(handle.selection().head()).map(|r| (r.x, r.y, r.height)));
    open(store, PickerKind::Mention { from }, x, y);
    match typed_mention_query(&handle.doc(), from, handle.selection()) {
        Some(query) => set_query(store, query),
        None => close(store),
    }
}

/// The query of an `@` picker whose `@` is at `from`, with the caret at
/// `selection`: what follows the `@`, in the same textblock, up to the
/// caret. `None` when the caret left it, the `@` is gone, or what follows is
/// longer than a name (40 characters, or more than three words).
pub fn typed_mention_query(doc: &rinch_editor_core::Node, from: usize, selection: rinch_editor_core::selection::Selection) -> Option<String> {
    if selection.head().0 < from + 1 {
        return None;
    }
    let text = text_up_to_caret(doc, from, selection)?;
    let query = text.strip_prefix('@')?;
    (query.chars().count() <= 40 && query.matches(' ').count() <= 3 && !query.contains('\u{fffc}')).then(|| query.to_string())
}

/// The editor's selection changed: a `[[` picker follows what is typed
/// after the brackets and closes when the caret leaves them; a Ctrl+L
/// picker closes when the selection moves at all.
pub fn selection_changed(store: crate::state::AppStore, handle: &EditorHandle) {
    let Some(view) = rinch::prelude::untracked(|| store.link_picker.get()) else { return };
    match view.kind {
        PickerKind::Selection { .. } => close(store),
        // Its query is typed in the popup, or (in the browser) after where it
        // opened, in the note.
        PickerKind::Sheet { at } => {
            if let Some(query) = typed_since(handle, at) {
                set_query(store, query);
            }
        }
        PickerKind::Typed { from } => match typed_query(&handle.doc(), from, handle.selection()) {
            Some(query) => set_query(store, query),
            None => close(store),
        },
        PickerKind::Mention { from } => match typed_mention_query(&handle.doc(), from, handle.selection()) {
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
    if selection.head().0 < from + 2 {
        return None;
    }
    let text = text_up_to_caret(doc, from, selection)?;
    if !text.starts_with("[[") {
        return None;
    }
    let query: String = text.chars().skip(2).collect();
    (query.chars().count() <= 80 && !query.contains("]]")).then_some(query)
}

/// The text from `from` to the caret, in the same textblock; `None` when the
/// caret is not there (before it, another block, a range).
fn text_up_to_caret(doc: &rinch_editor_core::Node, from: usize, selection: rinch_editor_core::selection::Selection) -> Option<String> {
    let head = selection.head().0;
    if selection.from() != selection.to() || head < from {
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
    Some(text)
}

/// What was typed into the note after a board picker opened at `at` (in the
/// browser the editor keeps typed characters; the desktop hands them to the
/// popup through `key`).
fn typed_since(handle: &EditorHandle, at: usize) -> Option<String> {
    text_up_to_caret(&handle.doc(), at, handle.selection()).filter(|t| !t.is_empty() && t.chars().count() <= 80)
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
        // `@`: Enter takes a person, and "Share this with someone..." only
        // when the arrows chose it; otherwise it is the note's Enter.
        "Enter" | "Tab" if matches!(view.kind, PickerKind::Mention { .. }) => {
            let takes = match view.rows.get(view.selected) {
                Some(PickerRow::Person { .. }) => true,
                Some(PickerRow::ShareThis) => view.moved,
                _ => false,
            };
            if !takes {
                close(store);
                return false;
            }
            accept(store, handle, view.selected);
        }
        "Enter" | "Tab" if !view.rows.is_empty() => {
            accept(store, handle, view.selected);
        }
        "Enter" | "Tab" => close(store),
        _ => match view.kind {
            // A `[[` or `@` query is typed in the note itself.
            PickerKind::Typed { .. } | PickerKind::Mention { .. } => return false,
            PickerKind::Selection { .. } | PickerKind::Sheet { .. } if key == "Backspace" => {
                let mut query = view.query.clone();
                query.pop();
                set_query(store, query);
            }
            PickerKind::Selection { .. } | PickerKind::Sheet { .. } if !modified && key.chars().count() == 1 => {
                set_query(store, format!("{}{key}", view.query));
            }
            // Anything else (a shortcut, a caret move) closes it and goes on.
            PickerKind::Selection { .. } | PickerKind::Sheet { .. } => {
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
    let Some(PickerView { kind: PickerKind::Typed { .. } | PickerKind::Mention { .. }, .. }) = rinch::prelude::untracked(|| store.link_picker.get()) else { return };
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
mod tests;
