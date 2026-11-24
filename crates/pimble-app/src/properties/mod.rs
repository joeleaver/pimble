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
//!
//! The panel is split by what it shows: `tags`, `fields`, `place` (where
//! the node lives and its times) and `references` (what points at it); this
//! module holds what they share and draws the panel around them.

mod fields;
mod place;
mod references;
mod tags;

use std::cell::RefCell;

use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;

use crate::appearance::IconGlyph;
use crate::protocol::BackendCommand;
use crate::state::{AppStore, LinkOpen};

pub use fields::add_field;
pub(crate) use place::when;
pub use references::{references_loaded, store_changed, Mention, References, RowsMention};

/// The panel's width before anyone drags it, and the range a drag keeps it in.
pub const DEFAULT_WIDTH: f32 = 280.0;
pub const MIN_WIDTH: f32 = 200.0;
pub const MAX_WIDTH: f32 = 560.0;

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

thread_local! {
    /// What the panel last asked `GetProperties` for: the node, where it was
    /// and its tags.
    static LAST_ASKED: RefCell<Option<Asked>> = const { RefCell::new(None) };
}

type Asked = (StoreId, NodeId, Option<(Option<NodeId>, Vec<String>)>);

/// The node the panel shows, tracked: the focused pane's.
fn shown(store: AppStore) -> Option<(StoreId, NodeId)> {
    crate::pane_view::pane_document(store, store.focused_pane.get())
}

/// View > "Properties" and `Alt+Enter` flip `AppStore::properties_open`
/// through the menu's own check item; this is the same for anything else.
pub fn toggle(store: AppStore) {
    let open = untracked(|| store.properties_open.get());
    store.properties_open.set(!open);
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
/// signal read at a time (`pane_view::document::editor_read_only`).
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

/// The shown node's values, tracked.
fn panel_node(store: AppStore) -> Option<Shown> {
    node_values(store, shown(store))
}

/// Whether this device may change the shown node, tracked.
fn panel_writable(store: AppStore) -> bool {
    shown(store).is_some_and(|(s, n)| store.node_access_tracked(s, n).allows_write())
}

/// The backend's last answer about the shown node, tracked; none while it is
/// about another node.
fn panel_context(store: AppStore) -> Option<Context> {
    let here = shown(store)?;
    store.properties_context.with(|c| c.clone().filter(|c| (c.store_id, c.node_id) == here))
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
            references::forget_asked();
            return;
        }
        let Some((s, n)) = shown(store) else { return };
        // Another node: what points at it is asked at once (changes ask
        // again once they rest, `store_changed`).
        references::ask_if_another(store, s, n);
        // The node's signal changes with every edit of it; only a move or a
        // change of its tags asks again.
        let asked = (s, n, node_values(store, Some((s, n))).map(|node| (node.parent_id, node.tags)));
        if LAST_ASKED.with(|l| l.borrow().as_ref() == Some(&asked)) {
            return;
        }
        LAST_ASKED.with(|l| *l.borrow_mut() = Some(asked));
        store.send(BackendCommand::GetProperties { store_id: s, node_id: n });
    });

    let note = move || shown(store).map(|(s, n)| crate::state::access_note(store.node_access_tracked(s, n))).unwrap_or("");
    // Why the node is not syncing, while its store's link holds it back
    // (docs/SYNC_LIMITS_CONTRACT.md, "Saying what waits").
    let waiting = move || shown(store).and_then(|(s, n)| store.waiting_reason(s, n)).map(|reason| format!("Waiting to sync. {reason}")).unwrap_or_default();
    // Out of step on this device, since when and why (docs/CONVERGENCE_CONTRACT.md section 4).
    let out_of_step = move || shown(store).and_then(|(s, n)| store.out_of_step_of(s, n)).map(|entry| crate::out_of_step::notice(&entry)).unwrap_or_default();

    let tags = tags::render_tags(__scope, store);
    let fields = fields::render_fields(__scope, store);
    let lives_in = place::render_lives_in(__scope, store);
    let [shown_in, links_here, rows_here] = references::render_references(__scope, store);
    let times = place::render_times(__scope, store);

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
                    div {
                        class: "pimble-properties__note pimble-properties__waiting",
                        style: {move || if waiting().is_empty() { "display: none;" } else { "" }},
                        {move || waiting()}
                    }
                    div {
                        class: "pimble-properties__note pimble-properties__out-of-step",
                        style: {move || if out_of_step().is_empty() { "display: none;" } else { "" }},
                        {move || out_of_step()}
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
