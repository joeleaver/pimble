//! A metanode in a pane (docs/METANODES_CONTRACT.md): a node set to be shown by
//! a viewer. On the desktop, a viewer an installed plugin provides is mounted in
//! the pane (`crate::plugins`, docs/PLUGINS_CONTRACT.md wave 1). Otherwise the
//! pane shows the fallback every app without the viewer shows: the source's
//! forest as an outline, with one line naming the viewer, or saying why its
//! plugin is not running. A row opens its node in the pane, as the explorer's
//! does.
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
    /// A plugin's view is in the pane, in place of the outline.
    pub plugin: Signal<bool>,
    /// The viewer's plugin asks to be allowed what it needs: this sentence,
    /// over the outline, with "Allow" and "Not now" (docs/PLUGINS_CONTRACT.md
    /// decision 2).
    pub asking: Signal<Option<String>>,
}

impl MetanodeUi {
    pub fn new() -> Self {
        Self {
            rows: Signal::new(Vec::new()),
            lines: Signal::new(Vec::new()),
            loaded: Signal::new(false),
            plugin: Signal::new(false),
            asking: Signal::new(None),
        }
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
    state.metanode_ui.plugin.set(false);
    state.metanode_ui.asking.set(None);
    #[cfg(feature = "native")]
    crate::plugins::hide(pane);
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
    let mut plugin = false;
    let mut asking = None;
    if let Some(spec) = &forest.spec {
        match viewer(store, pane, &spec.viewer, forest) {
            Viewer::Plugin => plugin = true,
            Viewer::Outline(sentence) => lines.push(sentence),
            Viewer::Ask(sentence) => asking = Some(sentence),
            Viewer::Missing => lines.push(outline_line(&spec.viewer)),
        }
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
    ui.plugin.set(plugin);
    ui.asking.set(asking);
}

/// What a pane shows for a metanode's viewer.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
enum Viewer {
    /// The plugin providing it, mounted in the pane.
    Plugin,
    /// The outline, under this sentence (its plugin was refused or stopped).
    Outline(String),
    /// The outline, under the plugin's request to be allowed what it needs.
    Ask(String),
    /// The outline, under the line naming the missing viewer.
    Missing,
}

#[cfg(feature = "native")]
fn viewer(store: AppStore, pane: PaneId, viewer: &str, forest: &Forest) -> Viewer {
    match crate::plugins::show(store, pane, viewer, forest) {
        crate::plugins::Outcome::Shown => Viewer::Plugin,
        crate::plugins::Outcome::Outline(sentence) => Viewer::Outline(sentence),
        crate::plugins::Outcome::Ask(sentence) => Viewer::Ask(sentence),
        crate::plugins::Outcome::Missing => Viewer::Missing,
    }
}

/// The browser runs no plugins yet (docs/PLUGINS_CONTRACT.md wave 3).
#[cfg(not(feature = "native"))]
fn viewer(_store: AppStore, _pane: PaneId, _viewer: &str, _forest: &Forest) -> Viewer {
    Viewer::Missing
}

/// The plugin in pane `pane` stopped by itself (it ran too long, was refused or
/// trapped): the pane shows the outline, under the sentence saying so.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn plugin_stopped(store: AppStore, pane: PaneId, sentence: &str) {
    let ui = store.pane(pane).metanode_ui;
    ui.plugin.set(false);
    ui.lines.update(|lines| {
        lines.retain(|l| l != sentence);
        lines.insert(0, sentence.to_string());
    });
}

/// Pane `pane` shows its metanode again from the forest held for it (a plugin
/// was allowed, declined or revoked there).
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn refill(store: AppStore, pane: PaneId) {
    let Some(held) = untracked(|| store.pane(pane).metanode.get()) else { return };
    let forest = FORESTS.with(|f| f.borrow().get(&(held.store_id, held.node_id)).cloned());
    if let Some(forest) = forest {
        fill(store, pane, Ok(&forest));
    }
}

/// "Allow" under a plugin's request in pane `pane`.
fn allow_plugin(store: AppStore, pane: PaneId) {
    #[cfg(feature = "native")]
    crate::plugins::allow(store, pane);
    #[cfg(not(feature = "native"))]
    let _ = (store, pane);
}

/// "Not now" under a plugin's request in pane `pane`.
fn decline_plugin(store: AppStore, pane: PaneId) {
    #[cfg(feature = "native")]
    crate::plugins::decline(store, pane);
    #[cfg(not(feature = "native"))]
    let _ = (store, pane);
}

/// The plugins folder finished loading: every pane showing a metanode looks for
/// its viewer again.
#[cfg_attr(not(feature = "native"), allow(dead_code))]
pub(crate) fn plugins_ready(store: AppStore) {
    for pane in PaneId::ALL {
        let Some(held) = untracked(|| store.pane(pane).metanode.get()) else { continue };
        let forest = FORESTS.with(|f| f.borrow().get(&(held.store_id, held.node_id)).cloned());
        if let Some(forest) = forest {
            fill(store, pane, Ok(&forest));
        }
    }
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
    // A plugin's view goes here (`crate::plugins`); the outline is hidden while it
    // shows.
    #[cfg(feature = "native")]
    let plugin_slot = Some(crate::plugins::render_slot(__scope, pane, ui.plugin));
    #[cfg(not(feature = "native"))]
    let plugin_slot: Option<NodeHandle> = None;
    let drawn = move || if ui.plugin.get() { Vec::new() } else { drawn() };
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
            if ui.asking.with(|a| a.is_some()) {
                div {
                    class: "pimble-metanode__ask",
                    style: "display: flex; align-items: center; gap: 8px; flex-wrap: wrap;",
                    span {
                        class: "pimble-metanode__line",
                        {move || ui.asking.get().unwrap_or_default()}
                    }
                    Button { variant: "filled", size: "xs", onclick: move || allow_plugin(store, pane), "Allow" }
                    Button { variant: "default", size: "xs", onclick: move || decline_plugin(store, pane), "Not now" }
                }
            }
            {plugin_slot}
            div {
                class: "pimble-metanode__empty",
                style: {move || if ui.loaded.get() && !ui.plugin.get() && ui.rows.with(|r| r.is_empty()) { "" } else { "display: none;" }},
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
