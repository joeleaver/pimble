//! A metanode in a pane (docs/METANODES_CONTRACT.md wave 1): a node set to be
//! shown by a viewer. No viewers exist yet (they come with plugins, wave 2),
//! so a pane opening one shows the fallback every app without the viewer
//! shows: the source's forest as an outline, with one line naming the
//! viewer. A row opens its node in the pane, as the explorer's does.
//!
//! The forest comes from the API (`Pimble::metanode_source`, asked with
//! `BackendCommand::GetMetanode`) and is asked again whenever a change the
//! tree is shown (`BackendEvent::Shown`) may have touched it
//! (`Forest::touched_by_view`): the change's own ids against the metanode,
//! the nodes found and the lists read, so a change elsewhere asks nothing.

use std::cell::RefCell;
use std::collections::HashMap;

use pimble_api::Forest;
use pimble_core::metanode::{outline_line, Source};
use pimble_core::{node_types, Node, NodeId, StoreId};
use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::appearance::{display_color, IconGlyph};
use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use crate::state::{display_label_from_node, ActiveEdit, AppStore, LinkOpen};

/// One row of a metanode's outline, as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineRow {
    pub store_id: StoreId,
    pub node_id: NodeId,
    /// 1 for the metanode's own children.
    pub depth: u32,
    pub label: String,
    /// A Tabler icon name: the node's own, or its type's.
    pub icon: String,
    /// The node's stored color, drawn through `display_color`.
    pub color: Option<String>,
}

/// What a pane's metanode outline shows.
#[derive(Clone, Copy)]
pub struct MetanodeUi {
    pub rows: Signal<Vec<OutlineRow>>,
    /// The lines over the outline: which viewer is missing, a source a newer
    /// Pimble wrote, or why nothing could be read.
    pub lines: Signal<Vec<String>>,
    /// The forest has been read at least once since the pane opened it.
    pub loaded: Signal<bool>,
}

impl MetanodeUi {
    pub fn new() -> Self {
        Self { rows: Signal::new(Vec::new()), lines: Signal::new(Vec::new()), loaded: Signal::new(false) }
    }
}

impl Default for MetanodeUi {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// The forests the panes show, by metanode: what each change is tested
    /// against. Dropped when no pane shows the metanode.
    static FORESTS: RefCell<HashMap<(StoreId, NodeId), Forest>> = RefCell::new(HashMap::new());
    /// Forests asked for and not answered yet; `true` when a change touched
    /// one meanwhile, so it is asked once more when the answer comes (the
    /// answer may be from before the change).
    static ASKED: RefCell<HashMap<(StoreId, NodeId), bool>> = RefCell::new(HashMap::new());
}

/// Whether `node` is a metanode, so a pane shows its outline.
pub(crate) fn is_metanode(node: &Node) -> bool {
    node.metadata.metanode.is_some()
}

/// Show metanode `(store_id, node_id)` in pane `pane`, instead of whatever
/// it held, and ask for its forest.
pub(crate) fn start(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId) {
    // A pane holds one thing: whatever it held goes.
    crate::editor::stop_editing(store, pane);
    crate::sheets::stop(store, pane);
    let state = store.pane(pane);
    state.show_editor.set(false);
    state.show_board.set(false);
    state.show_sheet.set(false);
    state.show_discussion.set(false);
    state.metanode.set(Some(ActiveEdit { store_id, node_id }));
    state.show_metanode.set(true);
    let held = FORESTS.with(|f| f.borrow().get(&(store_id, node_id)).cloned());
    match held {
        Some(forest) => fill(store, pane, Ok(&forest)),
        None => {
            state.metanode_ui.rows.set(Vec::new());
            state.metanode_ui.lines.set(Vec::new());
            state.metanode_ui.loaded.set(false);
        }
    }
    ask(store, (store_id, node_id));
}

/// Pane `pane` stops showing its metanode (another node opened there, the
/// pane emptied, or the node is a metanode no longer).
pub(crate) fn stop(store: AppStore, pane: PaneId) {
    let state = store.pane(pane);
    let Some(held) = untracked(|| state.metanode.get()) else {
        state.show_metanode.set(false);
        return;
    };
    state.metanode.set(None);
    state.show_metanode.set(false);
    state.metanode_ui.rows.set(Vec::new());
    state.metanode_ui.lines.set(Vec::new());
    let key = (held.store_id, held.node_id);
    if showing(store, key).is_empty() {
        FORESTS.with(|f| f.borrow_mut().remove(&key));
    }
}

/// The panes showing metanode `key`.
fn showing(store: AppStore, key: (StoreId, NodeId)) -> Vec<PaneId> {
    untracked(|| {
        PaneId::ALL
            .into_iter()
            .filter(|p| store.pane(*p).metanode.get().is_some_and(|m| (m.store_id, m.node_id) == key))
            .collect()
    })
}

/// Ask for `key`'s forest, once while a request is in flight.
fn ask(store: AppStore, key: (StoreId, NodeId)) {
    let in_flight = ASKED.with(|asked| {
        let mut asked = asked.borrow_mut();
        match asked.get_mut(&key) {
            Some(again) => {
                *again = true;
                true
            }
            None => {
                asked.insert(key, false);
                false
            }
        }
    });
    if !in_flight {
        store.send(BackendCommand::GetMetanode { store_id: key.0, node_id: key.1 });
    }
}

/// The forest of metanode `(store_id, node_id)` arrived (or could not be
/// read): every pane showing it shows it.
pub(crate) fn forest_read(store: AppStore, store_id: StoreId, node_id: NodeId, forest: Result<Forest, String>) {
    let key = (store_id, node_id);
    let again = ASKED.with(|asked| asked.borrow_mut().remove(&key)).unwrap_or(false);
    let panes = showing(store, key);
    if panes.is_empty() {
        return;
    }
    if let Ok(forest) = &forest {
        FORESTS.with(|f| f.borrow_mut().insert(key, forest.clone()));
    }
    for pane in panes {
        fill(store, pane, forest.as_ref().map_err(String::as_str));
    }
    if again {
        ask(store, key);
    }
}

/// A change was shown to the tree: every forest it may have touched is
/// asked for again.
pub(crate) fn view_shown(store: AppStore, view: &pimble_api::View) {
    let touched: Vec<(StoreId, NodeId)> =
        FORESTS.with(|f| f.borrow().iter().filter(|(_, forest)| forest.touched_by_view(view)).map(|(key, _)| *key).collect());
    for key in touched {
        ask(store, key);
    }
}

/// Make `(store_id, node_id)` a metanode shown with `viewer`, or with `None`
/// an ordinary node again; `source` `None` keeps the one it has. The code
/// path a viewer menu takes once plugins provide viewers (wave 2); today
/// "Show as Outline" is its one caller.
pub(crate) fn set_metanode(store: AppStore, store_id: StoreId, node_id: NodeId, viewer: Option<String>, source: Option<Source>) {
    store.send(BackendCommand::SetMetanode { store_id, node_id, viewer, source });
}

/// Pane `pane`'s outline and lines from `forest`.
fn fill(store: AppStore, pane: PaneId, forest: Result<&Forest, &str>) {
    let ui = store.pane(pane).metanode_ui;
    let forest = match forest {
        Ok(forest) => forest,
        Err(sentence) => {
            ui.lines.set(vec![sentence.to_string()]);
            ui.rows.set(Vec::new());
            ui.loaded.set(true);
            return;
        }
    };
    let mut lines = Vec::new();
    if let Some(spec) = &forest.spec {
        lines.push(outline_line(&spec.viewer));
    }
    if let Some(why) = forest.not_understood() {
        lines.push(why.to_string());
    }
    let rows: Vec<OutlineRow> = forest
        .entries
        .iter()
        .map(|entry| OutlineRow {
            store_id: entry.addr.store,
            node_id: entry.addr.node,
            depth: entry.depth,
            label: display_label_from_node(&entry.node),
            icon: icon_of(&entry.node),
            color: entry.node.metadata.color().map(str::to_string),
        })
        .collect();
    ui.lines.set(lines);
    ui.rows.set(rows);
    ui.loaded.set(true);
}

/// The icon a row shows: the node's own, or its type's as the explorer
/// draws it.
fn icon_of(node: &Node) -> String {
    if let Some(icon) = node.metadata.icon() {
        return icon.to_string();
    }
    let icon = match node.node_type.as_str() {
        node_types::FOLDER => TablerIcon::Folder,
        node_types::MOUNT => TablerIcon::Link,
        node_types::BOARD => TablerIcon::Chalkboard,
        node_types::SHEET => TablerIcon::Table,
        node_types::DISCUSSION => TablerIcon::Messages,
        _ => TablerIcon::File,
    };
    icon.name().to_string()
}

/// The outline a pane shows for a metanode: the lines, then the rows,
/// indented by depth. Rendered once per pane, shown while the pane holds a
/// metanode.
pub(crate) fn render_metanode(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let ui = state.metanode_ui;
    // Each row as drawn now: its color follows the theme.
    let drawn = move || {
        let dark = store.dark_mode.get();
        ui.rows.get().into_iter().map(|row| {
            let color = row.color.as_deref().map(|c| format!("color: {};", display_color(c, dark))).unwrap_or_default();
            // Twice: each attribute below takes its own.
            (row, color.clone(), color)
        }).collect::<Vec<_>>()
    };
    rsx! {
        div {
            class: "pimble-metanode",
            style: {move || if state.show_metanode.get() { "" } else { "display: none;" }},
            for line in ui.lines.get() {
                div {
                    key: line.clone(),
                    class: "pimble-metanode__line",
                    {line.clone()}
                }
            }
            div {
                class: "pimble-metanode__empty",
                style: {move || if ui.loaded.get() && ui.rows.with(|r| r.is_empty()) { "" } else { "display: none;" }},
                "Nothing to show here yet."
            }
            for item in drawn() {
                div {
                    key: format!("{}/{} {} {} {} {}", item.0.store_id, item.0.node_id, item.0.depth, item.0.label, item.0.icon, item.1),
                    class: "pimble-metanode__row",
                    style: format!("padding-left: {}px;", 12 + 20 * item.0.depth.saturating_sub(1)),
                    onclick: {
                        let (store_id, node_id) = (item.0.store_id, item.0.node_id);
                        move || store.link_open.set(Some(LinkOpen {
                            value: format!("node_{store_id}_{node_id}"),
                            store_id,
                            node_id,
                            anchor: None,
                            pane: Some(pane),
                        }))
                    },
                    span {
                        class: "pimble-metanode__icon",
                        style: item.1.clone(),
                        IconGlyph { name: item.0.icon.clone() }
                    }
                    span {
                        class: "pimble-metanode__label",
                        style: item.2.clone(),
                        {item.0.label.clone()}
                    }
                }
            }
        }
    }
}
