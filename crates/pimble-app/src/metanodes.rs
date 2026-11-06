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
//! A query metanode's forest is read for the explorer too, once its row has
//! loaded its children (docs/QUERY_METANODES_CONTRACT.md section 4): its
//! finds are rows after its own children (`AppStore::finds`), kept live the
//! same way. Every re-read walks the store, so the re-reads a change asks for
//! wait until changes rest ([`REREAD_MS`]).

use std::cell::RefCell;
use std::collections::HashMap;

use pimble_api::Forest;
use pimble_core::metanode::outline_line;
use pimble_core::{Node, NodeId, StoreId};
use rinch::prelude::*;

use crate::appearance::{display_color, IconGlyph};
use crate::panes::PaneId;
use crate::protocol::BackendCommand;
use crate::state::{display_label_from_node, ActiveEdit, AppStore, Found, LinkOpen};

/// How long changes rest before the forests they touched are read again: a
/// match walks its whole store each time, and typing is a change a keystroke.
const REREAD_MS: u32 = 300;

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
    /// The plugin shown was updated on its own since it mounted: this
    /// sentence, over its view, with "Restart Plugin".
    pub newer: Signal<Option<String>>,
    /// The metanode is a query metanode (the `list` viewer): the pane shows its
    /// setup (`crate::metanode_setup`) in place of the outline.
    pub setup: Signal<bool>,
    pub setup_ui: crate::metanode_setup::SetupUi,
}

impl MetanodeUi {
    pub fn new() -> Self {
        Self {
            rows: Signal::new(Vec::new()),
            lines: Signal::new(Vec::new()),
            loaded: Signal::new(false),
            plugin: Signal::new(false),
            asking: Signal::new(None),
            newer: Signal::new(None),
            setup: Signal::new(false),
            setup_ui: crate::metanode_setup::SetupUi::new(),
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
    /// Forests a change touched, waiting for changes to rest.
    static TOUCHED: RefCell<Vec<(StoreId, NodeId)>> = const { RefCell::new(Vec::new()) };
    static REREAD: RefCell<Option<rinch::prelude::TimeoutHandle>> = const { RefCell::new(None) };
}

/// Whether `node` is a query metanode, whose row shows what it finds.
pub(crate) fn is_query(node: &Node) -> bool {
    node.metadata.metanode.as_ref().is_some_and(|m| m.is_query())
}

/// Whether the explorer shows what metanode `key` finds: its row has loaded
/// its children, and it is a query metanode.
fn in_explorer(store: AppStore, key: (StoreId, NodeId)) -> bool {
    store.has_children_loaded(key.0, key.1)
        && store.get_node_signal(key.0, key.1).is_some_and(|sig| untracked(|| sig.with(is_query)))
}

/// The children of row `(store_id, node_id)` were loaded: a query
/// metanode's row asks what it finds, to show them after its own.
pub(crate) fn children_loaded(store: AppStore, store_id: StoreId, node_id: NodeId) {
    if in_explorer(store, (store_id, node_id)) {
        ask(store, (store_id, node_id));
    }
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
    state.metanode_ui.newer.set(None);
    state.metanode_ui.setup.set(false);
    crate::metanode_setup::stop(store, pane);
    crate::plugins::hide(pane);
    let key = (held.store_id, held.node_id);
    if showing(store, key).is_empty() && !in_explorer(store, key) {
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
/// read): every pane showing it shows it, and its row in the explorer shows
/// what it finds. `lives` says where each found node lives.
pub(crate) fn forest_read(
    store: AppStore,
    store_id: StoreId,
    node_id: NodeId,
    forest: Result<Forest, String>,
    lives: &[(pimble_api::Addr, String)],
) {
    let key = (store_id, node_id);
    let again = ASKED.with(|asked| asked.borrow_mut().remove(&key)).unwrap_or(false);
    let panes = showing(store, key);
    let explorer = in_explorer(store, key);
    if panes.is_empty() && !explorer {
        // Nothing shows it now: its finds go with it.
        if untracked(|| store.finds.with(|f| f.contains_key(&key))) {
            store.finds.update(|f| {
                f.remove(&key);
            });
            store.bump_tree_structure();
        }
        return;
    }
    if let Ok(forest) = &forest {
        FORESTS.with(|f| f.borrow_mut().insert(key, forest.clone()));
        if explorer {
            show_finds(store, key, forest, lives);
        }
    }
    for pane in panes {
        fill(store, pane, forest.as_ref().map_err(String::as_str));
    }
    if again {
        ask(store, key);
    }
}

/// What `forest` finds, as the explorer's rows under metanode `key`: each
/// found node held for its row, with where it lives and what a refused
/// action on it says. The tree is rebuilt only when the rows changed.
fn show_finds(store: AppStore, key: (StoreId, NodeId), forest: &Forest, lives: &[(pimble_api::Addr, String)]) {
    let reason = forest.reason.clone().unwrap_or_else(|| "this metanode finds it".to_string());
    let mut found = Vec::new();
    for entry in forest.found() {
        let lives_in = lives.iter().find(|(a, _)| *a == entry.addr).map(|(_, l)| l.clone()).unwrap_or_default();
        let refusal = pimble_core::plan::found_refusal(&reason, &lives_in);
        // Its row reads the node from the cache, as every row does.
        store.track_mount_info(entry.addr.store, &entry.node);
        store.upsert_node(entry.addr.store, entry.node.clone());
        found.push(Found { store_id: entry.addr.store, node_id: entry.addr.node, lives_in, refusal });
    }
    let changed = untracked(|| store.finds.with(|f| f.get(&key) != Some(&found)));
    if changed {
        store.finds.update(|f| {
            if found.is_empty() && !forest.source.is_match() {
                f.remove(&key);
            } else {
                f.insert(key, found);
            }
        });
        store.bump_tree_structure();
    }
}

/// A change was shown to the tree: every forest it may have touched is
/// asked for again once changes rest ([`REREAD_MS`]).
pub(crate) fn view_shown(store: AppStore, view: &pimble_api::View) {
    let touched: Vec<(StoreId, NodeId)> =
        FORESTS.with(|f| f.borrow().iter().filter(|(_, forest)| forest.touched_by_view(view)).map(|(key, _)| *key).collect());
    // A metanode whose source changed to a match (a query metanode's setup)
    // starts showing its finds in the explorer.
    let became: Vec<(StoreId, NodeId)> = view
        .nodes
        .iter()
        .map(|n| (view.store, n.id))
        .filter(|key| !FORESTS.with(|f| f.borrow().contains_key(key)) && in_explorer(store, *key))
        .collect();
    if touched.is_empty() && became.is_empty() {
        return;
    }
    TOUCHED.with(|t| {
        let mut t = t.borrow_mut();
        for key in touched.into_iter().chain(became) {
            if !t.contains(&key) {
                t.push(key);
            }
        }
    });
    if let Some(pending) = REREAD.with(|r| r.borrow_mut().take()) {
        rinch::prelude::clear_timeout(pending);
    }
    let handle = rinch::prelude::set_timeout(REREAD_MS, move || {
        REREAD.with(|r| r.borrow_mut().take());
        for key in TOUCHED.with(|t| std::mem::take(&mut *t.borrow_mut())) {
            if !showing(store, key).is_empty() || in_explorer(store, key) {
                ask(store, key);
            } else {
                FORESTS.with(|f| f.borrow_mut().remove(&key));
            }
        }
    });
    REREAD.with(|r| *r.borrow_mut() = Some(handle));
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
    let mut newer = None;
    // A query metanode shows its setup, built into the app.
    let setup = forest.spec.as_ref().is_some_and(|spec| spec.shows_setup());
    if let Some(spec) = forest.spec.as_ref().filter(|_| !setup) {
        match viewer(store, pane, &spec.viewer, forest) {
            Viewer::Plugin => plugin = true,
            Viewer::PluginAsking(sentence) => {
                plugin = true;
                asking = Some(sentence);
            }
            Viewer::PluginNewer(sentence) => {
                plugin = true;
                newer = Some(sentence);
            }
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
    if setup {
        if untracked(|| ui.plugin.get()) {
            crate::plugins::hide(pane);
        }
        crate::metanode_setup::forest_arrived(store, pane, forest, &rows);
    } else if untracked(|| ui.setup.get()) {
        crate::metanode_setup::stop(store, pane);
    }
    ui.setup.set(setup);
    ui.lines.set(lines);
    ui.rows.set(rows);
    ui.loaded.set(true);
    ui.plugin.set(plugin);
    ui.asking.set(asking);
    ui.newer.set(newer);
}

/// What a pane shows for a metanode's viewer.
enum Viewer {
    /// The plugin providing it, mounted in the pane.
    Plugin,
    /// Mounted, under its request for the stores the metanode's mounts reach.
    PluginAsking(String),
    /// Mounted, under the line saying a newer version is ready.
    PluginNewer(String),
    /// The outline, under this sentence (its plugin was refused or stopped).
    Outline(String),
    /// The outline, under the plugin's request to be allowed what it needs.
    Ask(String),
    /// The outline, under the line naming the missing viewer.
    Missing,
}

fn viewer(store: AppStore, pane: PaneId, viewer: &str, forest: &Forest) -> Viewer {
    match crate::plugins::show(store, pane, viewer, forest) {
        crate::plugins::Outcome::Shown => Viewer::Plugin,
        crate::plugins::Outcome::ShownAsking(sentence) => Viewer::PluginAsking(sentence),
        crate::plugins::Outcome::ShownNewer(sentence) => Viewer::PluginNewer(sentence),
        crate::plugins::Outcome::Outline(sentence) => Viewer::Outline(sentence),
        crate::plugins::Outcome::Ask(sentence) => Viewer::Ask(sentence),
        crate::plugins::Outcome::Missing => Viewer::Missing,
    }
}

/// The plugin in pane `pane` stopped by itself (it ran too long, was refused or
/// trapped): the pane shows the outline, under the sentence saying so.
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
pub(crate) fn refill(store: AppStore, pane: PaneId) {
    let Some(held) = untracked(|| store.pane(pane).metanode.get()) else { return };
    let forest = FORESTS.with(|f| f.borrow().get(&(held.store_id, held.node_id)).cloned());
    if let Some(forest) = forest {
        fill(store, pane, Ok(&forest));
    }
}

/// "Allow" under a plugin's request in pane `pane`.
fn allow_plugin(store: AppStore, pane: PaneId) {
    crate::plugins::allow(store, pane);
}

/// "Not now" under a plugin's request in pane `pane`.
fn decline_plugin(store: AppStore, pane: PaneId) {
    crate::plugins::decline(store, pane);
}

/// The plugins folder finished loading: every pane showing a metanode looks for
/// its viewer again.
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
    pimble_ui::appearance::node_icon(node)
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
    let plugin_slot = Some(crate::plugins::render_slot(__scope, pane, ui.plugin));
    let drawn = move || if ui.plugin.get() || ui.setup.get() { Vec::new() } else { drawn() };
    // A query metanode's setup goes here (`crate::metanode_setup`).
    let setup_slot = crate::metanode_setup::render_setup(__scope, store, pane);
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
            if ui.newer.with(|n| n.is_some()) {
                div {
                    class: "pimble-metanode__ask",
                    style: "display: flex; align-items: center; gap: 8px; flex-wrap: wrap;",
                    span {
                        class: "pimble-metanode__line",
                        {move || ui.newer.get().unwrap_or_default()}
                    }
                    Button { variant: "default", size: "xs", onclick: move || crate::plugins::restart(store, pane), "Restart Plugin" }
                }
            }
            {plugin_slot}
            {setup_slot}
            div {
                class: "pimble-metanode__empty",
                style: {move || if ui.loaded.get() && !ui.plugin.get() && !ui.setup.get() && ui.rows.with(|r| r.is_empty()) { "" } else { "display: none;" }},
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
