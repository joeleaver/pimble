//! The Properties panel (docs/PROPERTIES_CONTRACT.md): the focused pane's
//! node's tags and fields, and where it lives, in one panel on the right of
//! the pane area, under the one toolbar. One panel for the window, following
//! the focused pane as the toolbar does (`toolbar.rs`, `TARGET`), on the
//! desktop and in the browser from this module.
//!
//! Every write is one the app already had: a tag is one edit of the node's
//! tags (`BackendCommand::SetNodeAppearance` with a `TagEdit`, made by
//! `Pimble::edit` on the tags as the document has them), a field is
//! `fields::write`. What it shows reads the node's own signal, so an edit
//! made anywhere (another window, the CLI, an agent) shows as it lands;
//! where the node lives and the tags its store uses come from the documents
//! the API holds (`BackendCommand::GetProperties`), asked again when the
//! node moves or its tags change. What points at the node (the Metanodes
//! that find it, what links to it, the table rows that choose it) comes from
//! `BackendCommand::GetReferences`, asked when the panel shows another node
//! and again once changes anywhere have rested (`store_changed`).

use std::cell::RefCell;

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;

use crate::appearance::IconGlyph;
use crate::protocol::{BackendCommand, TagEdit};
use crate::state::{AppStore, LinkOpen};

/// The panel's width before anyone drags it, and the range a drag keeps it in.
pub const DEFAULT_WIDTH: f32 = 280.0;
pub const MIN_WIDTH: f32 = 200.0;
pub const MAX_WIDTH: f32 = 560.0;

/// How many tags already used in the store are suggested at once.
const SUGGESTIONS: usize = 6;

/// What the panel shows of a node beyond its own signal, as the backend last
/// answered it (`BackendEvent::PropertiesLoaded`).
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// The nodes above it, its store's top first, each with its row's label.
    pub lives_in: Vec<(NodeId, String)>,
    /// The tags used anywhere in its store.
    pub store_tags: Vec<String>,
}

/// How long changes must rest before the references are asked again.
const REFERENCES_REST_MS: u32 = 800;
/// When they are asked once more after that: a server's search index, which
/// "Links here" reads, takes in a node's text 2 s after it rests.
const INDEX_LAG_MS: u32 = 2500;

/// A node the references name, by the label its row shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Mention {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub label: String,
}

/// One table whose rows choose the node: the node it is in, its name, and
/// the rows by their first column's value.
#[derive(Debug, Clone, PartialEq)]
pub struct RowsMention {
    pub at: Mention,
    pub table_name: String,
    pub rows: Vec<String>,
}

/// What points at a node (docs/PROPERTIES_CONTRACT.md section 5, wave 3),
/// as `BackendEvent::ReferencesLoaded` answers it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct References {
    /// The Metanodes whose queries find it.
    pub shown_in: Vec<Mention>,
    /// The nodes whose text links to it.
    pub links_here: Vec<Mention>,
    /// The tables whose link columns choose it, grouped by table.
    pub rows_here: Vec<RowsMention>,
    /// Why a part could not be read, said under the sections.
    pub not_known: Vec<String>,
}

thread_local! {
    /// The node the references were last asked for, and the debounce's
    /// count: a change bumps it, and only the last timer asks.
    static REFERENCES_ASKED: RefCell<Option<(StoreId, NodeId)>> = const { RefCell::new(None) };
    static REFERENCES_WAIT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// The new field's name box, to give it the keyboard (Edit > "Add Field").
    static NEW_FIELD_BOX: RefCell<Option<(u64, u64)>> = const { RefCell::new(None) };
    /// What the panel last asked `GetProperties` for: the node, where it was
    /// and its tags.
    static LAST_ASKED: RefCell<Option<Asked>> = const { RefCell::new(None) };
}

type Asked = (StoreId, NodeId, Option<(Option<NodeId>, Vec<String>)>);

/// The node the panel shows, tracked: the focused pane's.
fn shown(store: AppStore) -> Option<(StoreId, NodeId)> {
    crate::pane_view::pane_document(store, store.focused_pane.get())
}

/// Ask for what points at `(s, n)`.
fn ask_references(store: AppStore, s: StoreId, n: NodeId) {
    REFERENCES_ASKED.with(|a| *a.borrow_mut() = Some((s, n)));
    store.send(BackendCommand::GetReferences { store_id: s, node_id: n });
}

/// Something changed in a store (any store: a Metanode, a link or a table
/// elsewhere may now point at the node shown): once changes rest, the
/// references of the node the panel shows are asked again. Nothing while the
/// panel is closed.
pub fn store_changed(store: AppStore) {
    if !untracked(|| store.properties_open.get()) || REFERENCES_ASKED.with(|a| a.borrow().is_none()) {
        return;
    }
    let wait = REFERENCES_WAIT.with(|w| {
        w.set(w.get() + 1);
        w.get()
    });
    // Asked when changes rest, and once more when the index has them too;
    // a newer change makes both its own.
    for after in [REFERENCES_REST_MS, REFERENCES_REST_MS + INDEX_LAG_MS] {
        rinch::core::reactive::unowned(|| {
            set_timeout(after, move || {
                if REFERENCES_WAIT.with(|w| w.get()) != wait || !untracked(|| store.properties_open.get()) {
                    return;
                }
                if let Some((s, n)) = untracked(|| shown(store)) {
                    ask_references(store, s, n);
                }
            })
        });
    }
}

/// `BackendEvent::ReferencesLoaded`: kept when it is about the node shown.
pub fn references_loaded(store: AppStore, s: StoreId, n: NodeId, references: References) {
    if untracked(|| shown(store)) == Some((s, n)) {
        store.properties_references.set(Some((s, n, references)));
    }
}

/// View > "Properties" and `Alt+Enter` flip `AppStore::properties_open`
/// through the menu's own check item; this is the same for anything else.
pub fn toggle(store: AppStore) {
    let open = untracked(|| store.properties_open.get());
    store.properties_open.set(!open);
}

/// Edit > "Add Field": the panel opens with a new field's name box taking
/// the keyboard.
pub fn add_field(store: AppStore) {
    let Some((s, n)) = untracked(|| shown(store)) else {
        crate::events::show_notice(store, "Open a note to give it a field.".to_string());
        return;
    };
    if !store.node_access(s, n).allows_write() {
        crate::events::show_notice(store, store.node_access(s, n).refusal().to_string());
        return;
    }
    store.properties_open.set(true);
    store.properties_adding_field.set(true);
    focus_new_field();
}

/// Give the new field's name box the keyboard once it shows.
fn focus_new_field() {
    rinch::core::reactive::unowned(|| {
        set_timeout(0, move || {
            if let Some((doc_key, node)) = NEW_FIELD_BOX.with(|b| *b.borrow()) {
                rinch::core::request_focus(doc_key, node);
            }
        })
    });
    // The browser owns its own focus: the box itself, once laid out.
    #[cfg(not(feature = "native"))]
    {
        use wasm_bindgen::JsCast;
        rinch::core::reactive::unowned(|| {
            set_timeout(40, move || {
                let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
                let Ok(Some(el)) = document.query_selector("input.pimble-properties__new-name") else { return };
                if let Ok(el) = el.dyn_into::<web_sys::HtmlElement>() {
                    let _ = el.focus();
                }
            })
        });
    }
}

/// The tags `typed` suggests among `used`, leaving out the ones `have`
/// already: those that start with it first, then those that contain it,
/// case-insensitively; nothing for nothing typed.
pub(crate) fn suggestions(used: &[String], have: &[String], typed: &str) -> Vec<String> {
    let typed = typed.trim().to_lowercase();
    if typed.is_empty() {
        return Vec::new();
    }
    let free = |t: &&String| !have.iter().any(|h| h.eq_ignore_ascii_case(t));
    let mut out: Vec<String> = used.iter().filter(free).filter(|t| t.to_lowercase().starts_with(&typed)).cloned().collect();
    out.extend(used.iter().filter(free).filter(|t| {
        let t = t.to_lowercase();
        !t.starts_with(&typed) && t.contains(&typed)
    }).cloned());
    out.truncate(SUGGESTIONS);
    out
}

/// What typing `text` in the tag box gives: the tags a comma finished, and
/// what is left to keep typing.
pub(crate) fn split_typed(text: &str) -> (Vec<String>, String) {
    match text.rfind(',') {
        Some(at) => {
            let done = text[..at].split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
            (done, text[at + 1..].trim_start().to_string())
        }
        None => (Vec::new(), text.to_string()),
    }
}

/// A node's created and changed times, in this device's local time.
pub(crate) fn when(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local).format("%-d %b %Y, %H:%M").to_string()
}

/// Add or take off one tag of `(s, n)`, as typed (the spelling the store
/// already uses for it when it has one).
fn edit_tag(store: AppStore, s: StoreId, n: NodeId, change: TagEdit) {
    if !store.node_access(s, n).allows_write() {
        crate::events::show_notice(store, store.node_access(s, n).refusal().to_string());
        return;
    }
    let change = match change {
        TagEdit::Add(tag) => {
            let used = untracked(|| store.properties_context.with(|c| c.as_ref().map(|c| c.store_tags.clone()).unwrap_or_default()));
            let tag = tag.trim().trim_start_matches('#').trim().to_string();
            if tag.is_empty() {
                return;
            }
            TagEdit::Add(used.into_iter().find(|t| t.eq_ignore_ascii_case(&tag)).unwrap_or(tag))
        }
        other => other,
    };
    store.send(BackendCommand::SetNodeAppearance { store_id: s, node_id: n, icon: None, color: None, background: None, tags: Some(change) });
}

/// What the panel shows of a node from its own signal.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    parent_id: Option<NodeId>,
    tags: Vec<String>,
    fields: Vec<(String, String)>,
    created_at: chrono::DateTime<chrono::Utc>,
    modified_at: chrono::DateTime<chrono::Utc>,
}

/// The tags and fields of `(s, n)` as this window has them, tracked: one
/// signal read at a time (`pane_view::editor_read_only`).
fn node_values(store: AppStore, node: Option<(StoreId, NodeId)>) -> Option<Shown> {
    let (s, n) = node?;
    let sig = store.node_data.with(|map| map.get(&(s, n)).copied())?;
    Some(sig.with(|node| Shown {
        parent_id: node.parent_id,
        tags: node.metadata.tags.clone(),
        fields: node.metadata.fields.clone(),
        created_at: node.metadata.created_at,
        modified_at: node.metadata.modified_at,
    }))
}

/// How the focused pane reached its node, when not through its own place:
/// "Shown through the mount Recipes." / "Found by Waiting."
fn reached_through(store: AppStore) -> Option<String> {
    let pane = store.focused_pane.get();
    let value = store.pane(pane).selected.get()?;
    if let Some(found) = crate::state::found_row(&value) {
        let (s, n) = found.metanode;
        let name = untracked(|| store.get_node_signal(s, n).map(|sig| sig.with(crate::state::display_label_from_node)));
        return Some(format!("Found by {}.", name.unwrap_or_else(|| "a Metanode".to_string())));
    }
    // The innermost mount crossed is the last segment of the value's path.
    let (_, rest) = value.split_once('/')?;
    let last = rest.rsplit('/').next()?;
    let (s, n) = last.split_once('_')?;
    let (s, n) = (StoreId(s.parse().ok()?), NodeId(n.parse().ok()?));
    let (name, metanode) = untracked(|| {
        store.get_node_signal(s, n).map(|sig| sig.with(|node| (crate::state::display_label_from_node(node), crate::metanodes::is_metanode(node))))
    })?;
    Some(if metanode { format!("Shown by the Metanode {name}.") } else { format!("Shown through the mount {name}.") })
}

/// Open `(s, n)` in the focused pane, as a followed link does.
fn open(store: AppStore, s: StoreId, n: NodeId) {
    store.link_open.set(Some(LinkOpen {
        value: format!("node_{s}_{n}"),
        store_id: s,
        node_id: n,
        anchor: None,
        pane: Some(store.focused()),
    }));
}

/// The panel, rendered once: hidden while closed.
pub(crate) fn render_panel(__scope: &mut RenderScope, store: AppStore) -> NodeHandle {
    // Ask for where the node lives and its store's tags whenever the panel
    // shows another node, or the node moves or its tags change.
    let _ = rinch::Effect::new(move || {
        if !store.properties_open.get() {
            // Opened again, it asks again.
            LAST_ASKED.with(|l| *l.borrow_mut() = None);
            REFERENCES_ASKED.with(|a| *a.borrow_mut() = None);
            return;
        }
        let Some((s, n)) = shown(store) else { return };
        // Another node: what points at it is asked at once (changes ask
        // again once they rest, `store_changed`).
        if REFERENCES_ASKED.with(|a| *a.borrow() != Some((s, n))) {
            ask_references(store, s, n);
        }
        // The node's signal changes with every edit of it; only a move or a
        // change of its tags asks again.
        let asked = (s, n, node_values(store, Some((s, n))).map(|node| (node.parent_id, node.tags)));
        if LAST_ASKED.with(|l| l.borrow().as_ref() == Some(&asked)) {
            return;
        }
        LAST_ASKED.with(|l| *l.borrow_mut() = Some(asked));
        store.send(BackendCommand::GetProperties { store_id: s, node_id: n });
    });

    let node = move || node_values(store, shown(store));
    let writable = move || shown(store).is_some_and(|(s, n)| store.node_access_tracked(s, n).allows_write());
    let note = move || shown(store).map(|(s, n)| crate::state::access_note(store.node_access_tracked(s, n))).unwrap_or("");
    let context = move || {
        let here = shown(store)?;
        store.properties_context.with(|c| c.clone().filter(|c| (c.store_id, c.node_id) == here))
    };

    // ── Tags ────────────────────────────────────────────────────────────
    let typed = Signal::new(String::new());
    let take_typed = move || {
        let Some((s, n)) = untracked(|| shown(store)) else { return };
        let text = untracked(|| typed.get());
        typed.set(String::new());
        for tag in text.split(',') {
            if !tag.trim().is_empty() {
                edit_tag(store, s, n, TagEdit::Add(tag.to_string()));
            }
        }
    };
    let offered = move || {
        let have = node().map(|n| n.tags).unwrap_or_default();
        let used = context().map(|c| c.store_tags).unwrap_or_default();
        suggestions(&used, &have, &typed.get())
    };
    let tags = rsx! {
        div {
            class: "pimble-properties__section",
            div { class: "pimble-properties__heading", "Tags" }
            div {
                class: "pimble-properties__chips",
                for tag in node().map(|n| n.tags).unwrap_or_default() {
                    span {
                        key: tag.clone(),
                        class: "pimble-properties__chip",
                        {tag.clone()}
                        span {
                            class: "pimble-properties__chip-remove",
                            style: {move || if writable() { "" } else { "display: none;" }},
                            title: "Remove this tag",
                            onclick: {
                                let tag = tag.clone();
                                move || {
                                    if let Some((s, n)) = untracked(|| shown(store)) {
                                        edit_tag(store, s, n, TagEdit::Remove(tag.clone()));
                                    }
                                }
                            },
                            "×"
                        }
                    }
                }
                span {
                    class: "pimble-properties__empty",
                    style: {move || if !writable() && node().is_none_or(|n| n.tags.is_empty()) { "" } else { "display: none;" }},
                    "No tags"
                }
            }
            div {
                style: {move || if writable() { "" } else { "display: none;" }},
                input {
                    class: "pimble-properties__input pimble-properties__tag-input",
                    placeholder: "Add a tag",
                    value: move || typed.get(),
                    oninput: move |v: String| {
                        // A comma finishes the tags before it.
                        let (done, rest) = split_typed(&v);
                        if done.is_empty() {
                            typed.set(v);
                            return;
                        }
                        typed.set(rest);
                        if let Some((s, n)) = untracked(|| shown(store)) {
                            for tag in done {
                                edit_tag(store, s, n, TagEdit::Add(tag));
                            }
                        }
                    },
                    onsubmit: take_typed,
                }
                div {
                    class: "pimble-properties__suggestions",
                    for tag in offered() {
                        div {
                            key: tag.clone(),
                            class: "pimble-properties__suggestion",
                            onclick: {
                                let tag = tag.clone();
                                move || {
                                    typed.set(String::new());
                                    if let Some((s, n)) = untracked(|| shown(store)) {
                                        edit_tag(store, s, n, TagEdit::Add(tag.clone()));
                                    }
                                }
                            },
                            {tag.clone()}
                        }
                    }
                }
            }
        }
    };

    // ── Fields ──────────────────────────────────────────────────────────
    let new_name = Signal::new(String::new());
    let new_value = Signal::new(String::new());
    let add = move || {
        let Some((s, n)) = untracked(|| shown(store)) else { return };
        let (name, value) = untracked(|| (new_name.get(), new_value.get()));
        if name.trim().is_empty() {
            return;
        }
        crate::fields::write(store, s, n, &name, Some(value.trim()));
        new_name.set(String::new());
        new_value.set(String::new());
        store.properties_adding_field.set(false);
    };
    let cancel = move || {
        new_name.set(String::new());
        new_value.set(String::new());
        store.properties_adding_field.set(false);
    };
    let new_name_box = rsx! {
        input {
            class: "pimble-properties__input pimble-properties__new-name",
            placeholder: "name",
            value: move || new_name.get(),
            oninput: move |v: String| new_name.set(v),
            onsubmit: add,
            onkeydown: move |k: &rinch::core::KeyEventData| {
                if k.key == "Escape" {
                    cancel();
                    return true;
                }
                false
            },
        }
    };
    NEW_FIELD_BOX.with(|b| *b.borrow_mut() = Some((new_name_box.doc_key(), new_name_box.node_id().0)));
    let fields = rsx! {
        div {
            class: "pimble-properties__section",
            div { class: "pimble-properties__heading", "Fields" }
            for (name, value) in node().map(|n| n.fields).unwrap_or_default() {
                div {
                    // Drawn again when a value changes anywhere.
                    key: format!("{name}={value}"),
                    class: "pimble-properties__field",
                    {field_row(__scope, store, name.clone(), value.clone(), writable)}
                }
            }
            div {
                class: "pimble-properties__empty",
                style: {move || if !writable() && node().is_none_or(|n| n.fields.is_empty()) { "" } else { "display: none;" }},
                "No fields"
            }
            div {
                class: "pimble-properties__field pimble-properties__field--new",
                style: {move || if writable() && store.properties_adding_field.get() { "" } else { "display: none;" }},
                {new_name_box}
                input {
                    class: "pimble-properties__input",
                    placeholder: "value",
                    value: move || new_value.get(),
                    oninput: move |v: String| new_value.set(v),
                    onsubmit: add,
                    onkeydown: move |k: &rinch::core::KeyEventData| {
                        if k.key == "Escape" {
                            cancel();
                            return true;
                        }
                        false
                    },
                }
                span { class: "pimble-properties__remove", title: "Cancel", onclick: cancel, "×" }
            }
            div {
                class: "pimble-properties__add",
                style: {move || if writable() && !store.properties_adding_field.get() { "" } else { "display: none;" }},
                onclick: move || {
                    store.properties_adding_field.set(true);
                    focus_new_field();
                },
                "+ Add field"
            }
        }
    };

    // ── Lives in, created and changed ───────────────────────────────────
    let steps = move || -> Vec<(StoreId, NodeId, String)> {
        let Some((s, n)) = shown(store) else { return Vec::new() };
        let store_sig = store.store_data.with(|map| map.get(&s).copied());
        let Some((store_name, root)) = store_sig.map(|sig| sig.with(|st| (st.name.clone(), st.root_node_id))) else { return Vec::new() };
        let mut out = vec![(s, root, store_name)];
        if let Some(c) = context() {
            out.extend(c.lives_in.into_iter().filter(|(id, _)| *id != root && *id != n).map(|(id, label)| (s, id, label)));
        }
        out
    };
    let lives_in = rsx! {
        div {
            class: "pimble-properties__section",
            div { class: "pimble-properties__heading", "Lives in" }
            div {
                class: "pimble-properties__path",
                for (i, (s, n, label)) in steps().into_iter().enumerate() {
                    span {
                        key: format!("{i} {n} {label}"),
                        class: "pimble-properties__step",
                        span { class: "pimble-properties__sep", style: if i == 0 { "display: none;" } else { "" }, " / " }
                        span {
                            class: "pimble-properties__link",
                            onclick: move || open(store, s, n),
                            {label.clone()}
                        }
                    }
                }
            }
            div {
                class: "pimble-properties__through",
                style: {move || if reached_through(store).is_some() { "" } else { "display: none;" }},
                {move || reached_through(store).unwrap_or_default()}
            }
        }
    };

    // ── Shown in, Links here, Rows pointing here (wave 3) ───────────────
    let references = move || -> Option<References> {
        let here = shown(store)?;
        store.properties_references.with(|r| r.as_ref().filter(|(s, n, _)| (*s, *n) == here).map(|(_, _, r)| r.clone()))
    };
    // A section's line when it lists nothing: "…" until the first answer.
    let none_yet = move |empty: fn(&References) -> bool| -> Option<&'static str> {
        match references() {
            None => Some("…"),
            Some(r) if empty(&r) => Some("None"),
            Some(_) => None,
        }
    };
    let shown_in = rsx! {
        div {
            class: "pimble-properties__section",
            div {
                class: "pimble-properties__heading pimble-properties__heading--line",
                "Shown in"
                span { class: "pimble-properties__count", {move || none_yet(|r| r.shown_in.is_empty()).unwrap_or("").to_string()} }
            }
            for m in references().map(|r| r.shown_in).unwrap_or_default() {
                div {
                    key: format!("{} {} {}", m.store_id, m.node_id, m.label),
                    class: "pimble-properties__ref",
                    span {
                        class: "pimble-properties__link",
                        onclick: {
                            let (s, n) = (m.store_id, m.node_id);
                            move || open(store, s, n)
                        },
                        {m.label.clone()}
                    }
                }
            }
        }
    };
    let links_here = rsx! {
        div {
            class: "pimble-properties__section",
            div {
                class: "pimble-properties__heading pimble-properties__heading--line",
                "Links here"
                span { class: "pimble-properties__count", {move || none_yet(|r| r.links_here.is_empty()).unwrap_or("").to_string()} }
            }
            for m in references().map(|r| r.links_here).unwrap_or_default() {
                div {
                    key: format!("{} {} {}", m.store_id, m.node_id, m.label),
                    class: "pimble-properties__ref",
                    span {
                        class: "pimble-properties__link",
                        onclick: {
                            let (s, n) = (m.store_id, m.node_id);
                            move || open(store, s, n)
                        },
                        {m.label.clone()}
                    }
                }
            }
        }
    };
    let rows_here = rsx! {
        div {
            class: "pimble-properties__section",
            div {
                class: "pimble-properties__heading pimble-properties__heading--line",
                "Rows pointing here"
                span { class: "pimble-properties__count", {move || none_yet(|r| r.rows_here.is_empty()).unwrap_or("").to_string()} }
            }
            for (i, g) in references().map(|r| r.rows_here).unwrap_or_default().into_iter().enumerate() {
                div {
                    key: format!("{i} {} {} {} {}", g.at.node_id, g.table_name, g.at.label, g.rows.join("\u{1f}")),
                    class: "pimble-properties__ref",
                    div {
                        class: "pimble-properties__ref-head",
                        span {
                            class: "pimble-properties__link",
                            onclick: {
                                let (s, n) = (g.at.store_id, g.at.node_id);
                                move || open(store, s, n)
                            },
                            {table_line(&g)}
                        }
                    }
                    div { class: "pimble-properties__rows", {rows_line(&g.rows)} }
                }
            }
            div {
                class: "pimble-properties__through",
                style: {move || if references().is_some_and(|r| !r.not_known.is_empty()) { "" } else { "display: none;" }},
                {move || references().map(|r| r.not_known.join(" ")).unwrap_or_default()}
            }
        }
    };
    let times = rsx! {
        div {
            class: "pimble-properties__section",
            div {
                class: "pimble-properties__times",
                {move || node().map(|n| format!("Created {} · Changed {}", when(n.created_at), when(n.modified_at))).unwrap_or_default()}
            }
        }
    };

    let width_now = move || store.properties_width.get();
    rsx! {
        div {
            class: "pimble-properties-wrap",
            style: {move || if store.properties_open.get() { format!("width: {}px;", width_now()) } else { "display: none;".to_string() }},
            // The panel's left edge, dragged to resize it.
            div {
                class: "pimble-properties__resizer",
                onmousedown: move || {
                    let start_width = untracked(|| store.properties_width.get());
                    let start_x = rinch::core::get_click_context().mouse_x;
                    let width_at = move |x: f32| (start_width - (x - start_x)).clamp(MIN_WIDTH, MAX_WIDTH);
                    rinch::core::Drag::absolute()
                        .on_move(move |x, _| store.properties_width.set(width_at(x)))
                        .on_end(move |x, _| store.properties_width.set(width_at(x)))
                        .start();
                },
            }
            div {
                class: "pimble-properties",
                div {
                    class: "pimble-properties__head",
                    span {
                        class: "pimble-properties__icon",
                        for (name, color) in crate::pane_view::pane_icon(store, store.focused_pane.get()) {
                            span {
                                key: format!("{name} {color}"),
                                style: if color.is_empty() { String::new() } else { format!("color: {color};") },
                                IconGlyph { name: name.clone() }
                            }
                        }
                    }
                    span {
                        class: "pimble-properties__name",
                        {move || if shown(store).is_some() { crate::pane_view::pane_title(store, store.focused_pane.get()) } else { "Properties".to_string() }}
                    }
                    span {
                        class: "pimble-properties__close",
                        title: "Close (Alt+Enter)",
                        onclick: move || store.properties_open.set(false),
                        "×"
                    }
                }
                div {
                    class: "pimble-properties__none",
                    style: {move || if shown(store).is_none() { "" } else { "display: none;" }},
                    "Choose a node to see its properties."
                }
                div {
                    class: "pimble-properties__body",
                    style: {move || if shown(store).is_some() { "" } else { "display: none;" }},
                    div {
                        class: "pimble-properties__note",
                        style: {move || if note().is_empty() { "display: none;" } else { "" }},
                        {move || note().to_string()}
                    }
                    {tags}
                    {fields}
                    {lives_in}
                    {shown_in}
                    {links_here}
                    {rows_here}
                    {times}
                }
            }
        }
    }
}

/// A group of rows pointing here, as its head says it: "Budget in Projects".
pub(crate) fn table_line(g: &RowsMention) -> String {
    let table = if g.table_name.trim().is_empty() { "A table" } else { g.table_name.trim() };
    format!("{table} in {}", g.at.label)
}

/// The rows of a group, by their first column's value: "Alpha, Beta", or
/// "2 rows" when none has one to say.
pub(crate) fn rows_line(rows: &[String]) -> String {
    let named: Vec<&str> = rows.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).collect();
    let count = if rows.len() == 1 { "1 row".to_string() } else { format!("{} rows", rows.len()) };
    if named.is_empty() {
        return count;
    }
    let mut line = named.iter().take(8).copied().collect::<Vec<_>>().join(", ");
    if named.len() > 8 {
        line.push_str(&format!(", … ({count})"));
    } else if named.len() < rows.len() {
        line.push_str(&format!(" ({count})"));
    }
    line
}

/// One field's row: its name and value, each edited in place (Enter or
/// leaving the box writes it), and a remove button; text only on a node
/// this device may only read.
fn field_row(__scope: &mut RenderScope, store: AppStore, name: String, value: String, writable: impl Fn() -> bool + Copy + 'static) -> NodeHandle {
    let name_text = Signal::new(name.clone());
    let value_text = Signal::new(value.clone());
    let rename = {
        let old = name.clone();
        move |typed: String| {
            let Some((s, n)) = untracked(|| shown(store)) else { return };
            let typed = typed.trim().to_string();
            // A name emptied, or the same name, changes nothing.
            if typed.is_empty() || typed.eq_ignore_ascii_case(&old) {
                name_text.set(old.clone());
                return;
            }
            let value = untracked(|| value_text.get());
            crate::fields::write(store, s, n, &typed, Some(value.trim()));
            crate::fields::write(store, s, n, &old, None);
        }
    };
    let set_value = {
        let name = name.clone();
        move |typed: String| {
            if let Some((s, n)) = untracked(|| shown(store)) {
                crate::fields::write(store, s, n, &name, Some(typed.trim()));
            }
        }
    };
    let remove = {
        let name = name.clone();
        move || {
            if let Some((s, n)) = untracked(|| shown(store)) {
                crate::fields::write(store, s, n, &name, None);
            }
        }
    };
    rsx! {
        div {
            class: "pimble-properties__field-inner",
            div {
                class: "pimble-properties__field-edit",
                style: {move || if writable() { "" } else { "display: none;" }},
                input {
                    class: "pimble-properties__input pimble-properties__field-name",
                    value: move || name_text.get(),
                    oninput: move |v: String| name_text.set(v),
                    onchange: rename,
                }
                input {
                    class: "pimble-properties__input",
                    value: move || value_text.get(),
                    oninput: move |v: String| value_text.set(v),
                    onchange: set_value,
                }
                span { class: "pimble-properties__remove", title: "Remove this field", onclick: remove, "×" }
            }
            div {
                class: "pimble-properties__field-read",
                style: {move || if writable() { "display: none;" } else { "" }},
                span { class: "pimble-properties__field-name-text", {name.clone()} }
                span { {value.clone()} }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn suggestions_start_with_what_is_typed_then_contain_it() {
        let used = tags(&["Important", "Tax", "Waiting", "Travel", "Taxes"]);
        assert_eq!(suggestions(&used, &[], "ta"), tags(&["Tax", "Taxes", "Important"]));
        assert_eq!(suggestions(&used, &tags(&["tax"]), "TA"), tags(&["Taxes", "Important"]), "a tag the node has is not offered, in any case");
        assert!(suggestions(&used, &[], "  ").is_empty(), "nothing typed offers nothing");
    }

    #[test]
    fn a_comma_finishes_the_tags_before_it() {
        assert_eq!(split_typed("tax"), (Vec::new(), "tax".to_string()));
        assert_eq!(split_typed("tax,"), (tags(&["tax"]), String::new()));
        assert_eq!(split_typed("tax, , waiting, im"), (tags(&["tax", "waiting"]), "im".to_string()));
    }

    #[test]
    fn a_tag_edit_keeps_one_of_each_whatever_its_case() {
        let have = tags(&["Tax", "Waiting"]);
        assert_eq!(TagEdit::Add("tax".into()).apply(&have), have, "a tag it has in another case is that tag");
        assert_eq!(TagEdit::Add(" Home ".into()).apply(&have), tags(&["Tax", "Waiting", "Home"]));
        assert_eq!(TagEdit::Remove("waiting".into()).apply(&have), tags(&["Tax"]));
    }

    #[test]
    fn a_group_of_rows_says_its_table_and_rows() {
        let at = Mention { store_id: StoreId::new(), node_id: NodeId::new(), label: "Projects".into() };
        let g = RowsMention { at, table_name: "Budget".into(), rows: tags(&["Alpha", "Beta"]) };
        assert_eq!(table_line(&g), "Budget in Projects");
        assert_eq!(table_line(&RowsMention { table_name: " ".into(), ..g.clone() }), "A table in Projects");
        assert_eq!(rows_line(&g.rows), "Alpha, Beta");
        assert_eq!(rows_line(&tags(&["", ""])), "2 rows", "rows with nothing to say are counted");
        assert_eq!(rows_line(&tags(&["Alpha", ""])), "Alpha (2 rows)");
        let many: Vec<String> = (1..=10).map(|i| format!("r{i}")).collect();
        assert_eq!(rows_line(&many), "r1, r2, r3, r4, r5, r6, r7, r8, … (10 rows)");
    }

    #[test]
    fn times_are_shown_in_local_time() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-04T12:30:00Z").unwrap().with_timezone(&chrono::Utc);
        let local = at.with_timezone(&chrono::Local);
        assert_eq!(when(at), local.format("%-d %b %Y, %H:%M").to_string());
        assert!(when(at).contains("2026"));
    }
}
