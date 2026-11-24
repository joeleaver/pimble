//! Search: the box, the debounced query and the results panel.

use std::cell::RefCell;

use rinch::prelude::*;
use rinch::core::{set_keyboard_interceptor, clear_keyboard_interceptor};
use rinch_tabler_icons::{TablerIcon, TablerIconStyle, render_tabler_icon};

use crate::protocol::BackendCommand;
use crate::state::{AppStore, SearchState};
use super::navigation::open_node;

thread_local! {
    /// The search box's `NodeHandle`, captured once when the toolbar is built
    /// so the View menu's "Focus Search" (Ctrl+Shift+F) action — constructed earlier,
    /// before the box exists — can reach it later. Same reach-across-closures
    /// need as `events::EVENT_PROCESSOR`.
    static SEARCH_INPUT: RefCell<Option<NodeHandle>> = const { RefCell::new(None) };
    /// The debounced `Search` command pending from the last keystroke, if any.
    static SEARCH_DEBOUNCE: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
}

/// Put the caret in the search box.
///
/// The desktop reaches this through the View menu's "Focus Search"; a browser
/// build, which has no menu bar to carry an accelerator, binds Ctrl+K to it
/// itself. The search box advertises the shortcut in its placeholder, so
/// exactly one of the two must always be in place.
pub fn focus_search() {
    SEARCH_INPUT.with(|cell| {
        if let Some(handle) = cell.borrow().as_ref() {
            handle.focus();
        }
    });
}

/// Rebuild the search index of every open store that has one.
///
/// An encrypted store has no server-side index and cannot have one — the
/// server has never seen a word of it — so it is skipped rather than asked.
pub fn rebuild_search_indexes(store: AppStore) {
    let store_ids = untracked(|| store.store_ids.get());
    for store_id in store_ids {
        if store.is_vault(store_id) {
            continue;
        }
        store.send(BackendCommand::RebuildIndex { store_id });
    }
}

/// Cancel a pending debounce timer and reset the search box to empty/idle.
/// Also drops the search box's Escape interceptor, installed by `oninput`
/// only while the box is non-empty (see there) so it never fights the tree
/// rename feature's own interceptor, which owns the same single global slot.
fn clear_search(store: AppStore) {
    if let Some(handle) = SEARCH_DEBOUNCE.with(|slot| slot.borrow_mut().take()) {
        clear_timeout(handle);
    }
    store.search_query.set(String::new());
    store.search_results.set(SearchState::Idle);
    clear_keyboard_interceptor();
}

/// Search for `query` as if it had been typed into the search box (a
/// Ctrl/Cmd+click on a hashtag in the text, docs/HASHTAGS_CONTRACT.md): the
/// box shows it, the results panel replaces the tree, and Escape clears it.
pub(crate) fn search_for(store: AppStore, query: String) {
    let was_empty = untracked(|| store.search_query.get()).is_empty();
    store.search_query.set(query.clone());
    if was_empty && !query.is_empty() {
        // As `oninput` installs it: only while the box is non-empty.
        set_keyboard_interceptor(move |data| {
            if data.key == "Escape" {
                run_on_main_thread(move || clear_search(store));
                return true;
            }
            false
        });
    }
    schedule_search(store, query);
}

/// Debounce search-as-you-type by 250ms (per the step 5 contract) and dispatch
/// a `BackendCommand::Search` over every open store when the timer fires.
fn schedule_search(store: AppStore, query: String) {
    if let Some(handle) = SEARCH_DEBOUNCE.with(|slot| slot.borrow_mut().take()) {
        clear_timeout(handle);
    }
    if query.is_empty() {
        store.search_results.set(SearchState::Idle);
        return;
    }
    let timeout = set_timeout(250, move || {
        SEARCH_DEBOUNCE.with(|slot| { slot.borrow_mut().take(); });
        let stores = untracked(|| store.store_ids.get());
        store.send(BackendCommand::Search { query, stores, limit: 50, purpose: crate::protocol::SearchPurpose::Panel });
    });
    SEARCH_DEBOUNCE.with(|slot| { *slot.borrow_mut() = Some(timeout); });
}

/// One search-result row, pre-resolved to plain fields the results panel
/// renders directly — `SearchResultItem` itself isn't `PartialEq`, which the
/// rsx `for` loop requires (see `rinch:rinch` Rule 9), and pimble-rpc isn't
/// this crate's to change.
#[derive(Clone, PartialEq)]
struct SearchRow {
    key: String,
    value: String,
    title: String,
    store_name: String,
    snippet: String,
    kind: &'static str,
}

/// Resolve raw search hits into display-ready `SearchRow`s (looking up each
/// store's name from already-cached signals — cheap, no new fetch). A plain
/// function so the rsx `for`'s iterable expression is a bare call with no
/// turbofish or closure braces for the macro's brace-stopping parser to trip
/// on (see `parse_expr_before_brace` in rinch-macros).
fn search_rows(store: AppStore, items: Vec<pimble_rpc::SearchResultItem>) -> Vec<SearchRow> {
    items.into_iter().map(|item| SearchRow {
        key: format!("{}_{}", item.store_id, item.node_id),
        value: format!("node_{}_{}", item.store_id, item.node_id),
        title: if item.title.is_empty() { "Untitled".to_string() } else { item.title },
        store_name: store.get_store_signal(item.store_id)
            .map(|sig| untracked(|| sig.with(|s| s.name.clone())))
            .unwrap_or_default(),
        snippet: item.snippet,
        kind: kind_label(&item.kind),
    }).collect()
}

/// One search result row for `key`, its text read from the current results
/// (`search_field`), so a keystroke that keeps it rewrites it in place.
fn search_result_row(__scope: &mut RenderScope, store: AppStore, tree_state: UseTreeReturn, key: String) -> NodeHandle {
    let value = format!("node_{key}");
    let (k1, k2, k3, k4, k5) = (key.clone(), key.clone(), key.clone(), key.clone(), key);
    rsx! {
        div {
            class: "pimble-search-result",
            onclick: move || {
                clear_search(store);
                open_node(store, tree_state, value.clone());
            },
            div {
                class: "pimble-search-result__title-row",
                span { class: "pimble-search-result__title", {move || search_field(store, &k1, |r| r.title.clone())} }
                span {
                    class: "pimble-search-result__kind",
                    style: {move || if search_field(store, &k2, |r| r.kind.to_string()).is_empty() { "display: none;" } else { "" }},
                    {move || search_field(store, &k3, |r| r.kind.to_string())}
                }
            }
            div { class: "pimble-search-result__store", {move || search_field(store, &k4, |r| r.store_name.clone())} }
            div { class: "pimble-search-result__snippet", {move || search_field(store, &k5, |r| r.snippet.clone())} }
        }
    }
}

/// The keys of the search results shown now (`SearchRow::key`), in rank order.
fn search_keys(store: AppStore) -> Vec<String> {
    store.search_results.with(|state| match state {
        SearchState::Results(items) => items.iter().map(|i| format!("{}_{}", i.store_id, i.node_id)).collect(),
        _ => Vec::new(),
    })
}

/// One field of the result row `key`, read from the current results.
fn search_field(store: AppStore, key: &str, pick: fn(&SearchRow) -> String) -> String {
    let item = store.search_results.with(|state| match state {
        SearchState::Results(items) => items.iter().find(|i| format!("{}_{}", i.store_id, i.node_id) == key).cloned(),
        _ => None,
    });
    item.map(|item| search_rows(store, vec![item]).first().map(pick).unwrap_or_default()).unwrap_or_default()
}

/// What the search panel says above its results while a store it searched
/// is still being indexed; nothing while no search is shown.
fn search_indexing_note(store: AppStore) -> String {
    if store.search_results.with(|state| matches!(state, SearchState::Idle)) {
        return String::new();
    }
    store.search_indexing_note(&[])
}

/// What the search panel says in place of a list, if anything.
fn search_message(store: AppStore) -> String {
    store.search_results.with(|state| match state {
        SearchState::Error(message) => message.clone(),
        SearchState::Results(items) if items.is_empty() => "No results".to_string(),
        _ => String::new(),
    })
}

/// A subtle, human-readable label for a search hit's `kind` (the index unit
/// kind the hit's chunk carries: prose, a heading, code, a table row, or a
/// structured field). Empty or unrecognized kinds render nothing.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "prose" | "document" => "",
        "heading" => "heading",
        "code" => "code",
        "table_row" | "table-row" | "tablerow" => "table row",
        "field" => "field",
        "folder" => "folder",
        _ => "",
    }
}

/// The search box and the results panel that replaces the tree while
/// it holds a query: `(bar, panel)`.
pub(super) fn render_search(__scope: &mut RenderScope, store: AppStore, tree_state: UseTreeReturn) -> (NodeHandle, NodeHandle) {
    // ── Search box + results panel ────────────────────────────────
    // The results panel replaces the tree while `search_query` is
    // non-empty; Esc (via `clear_search`'s interceptor) restores it.
    let search_icon = render_tabler_icon(__scope, TablerIcon::Search, TablerIconStyle::Outline);
    let search_input = rsx! {
        input {
            r#type: "text",
            class: "pimble-search-bar__input",
            placeholder: format!("Search... ({})", crate::shortcuts::chord(crate::shortcuts::SEARCH)),
            // Raw elements have no special "value_fn" prop — that's a
            // `TextInput`-component-only field. On a raw `<input>` any
            // closure-valued prop just becomes a reactively-set attribute
            // named literally after the prop, so `value_fn` was writing a
            // phantom `value_fn` attribute the renderer never reads
            // (`paint_input_value` only reads `"value"`). Bind `value`
            // itself so clearing the signal actually clears the box.
            value: move || store.search_query.get(),
            oninput: move |val: String| {
                let was_empty = untracked(|| store.search_query.get()).is_empty();
                let now_empty = val.is_empty();
                store.search_query.set(val.clone());
                if was_empty && !now_empty {
                    // Only while the box is non-empty — see `clear_search`.
                    set_keyboard_interceptor(move |data| {
                        if data.key == "Escape" {
                            run_on_main_thread(move || clear_search(store));
                            return true;
                        }
                        false
                    });
                } else if !was_empty && now_empty {
                    clear_keyboard_interceptor();
                }
                schedule_search(store, val);
            },
            onsubmit: move || {
                let first = untracked(|| match store.search_results.get() {
                    SearchState::Results(items) => items.into_iter().next(),
                    _ => None,
                });
                if let Some(item) = first {
                    let value = format!("node_{}_{}", item.store_id, item.node_id);
                    clear_search(store);
                    open_node(store, tree_state, value);
                }
            },
        }
    };
    SEARCH_INPUT.with(|cell| { *cell.borrow_mut() = Some(search_input.clone()); });

    let search_bar = rsx! {
        div {
            class: "pimble-search-bar",
            // The field itself, a box in the header band, so it reads as
            // something to type in.
            div {
                class: "pimble-search-bar__field",
                span { class: "pimble-search-bar__icon", {search_icon} }
                {search_input}
                if !store.search_query.get().is_empty() {
                    div {
                        title: "Clear the search",
                        style: "display: inline-flex;",
                        ActionIcon {
                            icon: TablerIcon::X,
                            variant: "subtle",
                            size: "xs",
                            class: "pimble-search-bar__clear",
                            onclick: move || clear_search(store),
                        }
                    }
                }
            }
        }
    };

    let search_panel = rsx! {
        div {
            class: "pimble-search-results",
            // What the panel says when it has no list: an error, or that
            // nothing matched.
            div {
                class: "pimble-search-results__message",
                style: {move || if search_message(store).is_empty() { "display: none;" } else { "" }},
                {move || search_message(store)}
            }
            // A store the search covers is still being indexed: its notes
            // may be missing (docs/STARTUP_CONTRACT.md "Indexing is said
            // where search is").
            div {
                class: "pimble-search-results__indexing",
                style: {move || if search_indexing_note(store).is_empty() { "display: none;" } else { "" }},
                {move || search_indexing_note(store)}
            }
            // The results, keyed by node, and each row's text read from
            // the current results: a keystroke that keeps a note among
            // the results rewrites its title and snippet in place, and
            // only a note that newly appears is built (2026-11-12: the
            // whole list was rebuilt on every change, about 200 DOM nodes
            // a keystroke).
            for key in search_keys(store) {
                div {
                    key: key.clone(),
                    style: "display: contents;",
                    {search_result_row(__scope, store, tree_state, key.clone())}
                }
            }
        }
    };
    (search_bar, search_panel)
}
