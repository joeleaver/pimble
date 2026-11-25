//! A query metanode's setup, in its pane (docs/QUERY_METANODES_CONTRACT.md
//! section 5): a query metanode with the app's own `list` viewer shows which
//! nodes it finds as a few short sentences with fields, where they are found,
//! their order and how many, and what it finds now. Built into the app for
//! both targets, not a plugin.
//!
//! The pane edits a draft of the metanode's source, and every change is
//! written through the one write (`BackendCommand::SetMetanode`, an
//! `applyEdit` of `metanode:source`), so two people editing a setup merge like
//! any edit. Typing is written once it rests ([`TYPING_MS`]); a source that
//! arrives while this pane's own write is on its way does not overwrite what
//! is being typed.

use std::cell::RefCell;
use std::collections::HashMap;

use pimble_core::metanode::{node_ref, parse_node_ref, Condition, FieldOp, Order, Source, Within, LIST_VIEWER};
use pimble_core::{NodeId, StoreId};
use rinch::prelude::*;
use rinch_tabler_icons::TablerIcon;

use crate::appearance::{display_color, IconGlyph};
use crate::metanodes::OutlineRow;
use crate::panes::PaneId;
use crate::protocol::{BackendCommand, SearchPurpose};
use crate::state::{AppStore, LinkOpen};

/// How long typing rests before the source is written.
const TYPING_MS: u32 = 400;
/// How long a write of this pane's is waited for before a source that
/// differs from it is taken (someone else's edit won).
const WRITE_WAIT_MS: f64 = 3000.0;
/// How many of what it finds the setup lists.
pub const FIRST_ROWS: usize = 10;

mod source;
mod view;
#[cfg(test)]
mod tests;

pub use source::*;
pub(crate) use view::render_setup;

/// What a node picker in the setup picks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickFor {
    /// The node a "linking to" line names.
    Condition(usize),
    /// The node "From: under" names.
    Under,
}

/// One node the picker found.
#[derive(Debug, Clone, PartialEq)]
pub struct PickRow {
    pub store_id: StoreId,
    pub node_id: NodeId,
    pub title: String,
    pub store_name: String,
}

/// What a pane's setup shows, besides its draft.
#[derive(Clone, Copy)]
pub struct SetupUi {
    /// The source as this pane edits it.
    pub draft: Signal<Option<Source>>,
    /// How many nodes it finds now.
    pub found_count: Signal<usize>,
    /// The first of them.
    pub found_rows: Signal<Vec<OutlineRow>>,
    /// The node picker, while open, and what it picks for.
    pub picking: Signal<Option<PickFor>>,
    pub pick_query: Signal<String>,
    pub picks: Signal<Vec<PickRow>>,
    /// "+ Add a condition"'s menu is open.
    pub adding: Signal<bool>,
}

impl SetupUi {
    pub fn new() -> Self {
        Self {
            draft: Signal::new(None),
            found_count: Signal::new(0),
            found_rows: Signal::new(Vec::new()),
            picking: Signal::new(None),
            pick_query: Signal::new(String::new()),
            picks: Signal::new(Vec::new()),
            adding: Signal::new(false),
        }
    }
}

impl Default for SetupUi {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// Per pane: the write waiting for typing to rest, and the source this
    /// pane wrote last with when (a source arriving that differs from it is
    /// taken only once the write has had time to arrive).
    static WRITES: RefCell<HashMap<PaneId, Write>> = RefCell::new(HashMap::new());
    static SEARCH_TIMER: RefCell<Option<TimeoutHandle>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Write {
    pending: Option<TimeoutHandle>,
    sent: Option<(Source, f64)>,
}

#[cfg(feature = "native")]
fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

#[cfg(not(feature = "native"))]
fn now_ms() -> f64 {
    js_sys::Date::now()
}

// ── The pane's state ───────────────────────────────────────────────────

fn ui(store: AppStore, pane: PaneId) -> SetupUi {
    store.pane(pane).metanode_ui.setup_ui
}

/// The metanode pane `pane` shows.
fn metanode_of(store: AppStore, pane: PaneId) -> Option<(StoreId, NodeId)> {
    untracked(|| store.pane(pane).metanode.get()).map(|m| (m.store_id, m.node_id))
}

/// A forest for pane `pane`'s query metanode arrived: what it finds now, and
/// its source, taken into the draft unless this pane is writing its own.
pub(crate) fn forest_arrived(store: AppStore, pane: PaneId, forest: &pimble_api::Forest, rows: &[OutlineRow]) {
    let ui = ui(store, pane);
    let found: Vec<OutlineRow> = forest
        .entries
        .iter()
        .zip(rows)
        .filter(|(entry, _)| entry.found)
        .map(|(_, row)| row.clone())
        .collect();
    ui.found_count.set(found.len());
    ui.found_rows.set(found.into_iter().take(FIRST_ROWS).collect());
    let take = WRITES.with(|w| {
        let mut w = w.borrow_mut();
        let write = w.entry(pane).or_default();
        if write.pending.is_some() {
            return false;
        }
        match &write.sent {
            Some((sent, _)) if *sent == forest.source => {
                write.sent = None;
                true
            }
            Some((_, at)) if now_ms() - at < WRITE_WAIT_MS => false,
            _ => {
                write.sent = None;
                true
            }
        }
    });
    if take && untracked(|| ui.draft.get()).as_ref() != Some(&forest.source) {
        ui.draft.set(Some(forest.source.clone()));
    }
}

/// Pane `pane` stops showing a setup.
pub(crate) fn stop(store: AppStore, pane: PaneId) {
    let ui = ui(store, pane);
    flush(store, pane);
    WRITES.with(|w| w.borrow_mut().remove(&pane));
    ui.draft.set(None);
    ui.picking.set(None);
    ui.adding.set(false);
    ui.found_rows.set(Vec::new());
    ui.found_count.set(0);
}

/// Change pane `pane`'s draft with `f`, and write it once typing has rested
/// for `delay_ms` (0: at once).
fn change(store: AppStore, pane: PaneId, delay_ms: u32, f: impl FnOnce(&mut Parts)) {
    let ui = ui(store, pane);
    let Some(draft) = untracked(|| ui.draft.get()) else { return };
    let next = edited(&draft, f);
    if next == draft {
        return;
    }
    ui.draft.set(Some(next));
    if let Some(pending) = WRITES.with(|w| w.borrow_mut().entry(pane).or_default().pending.take()) {
        clear_timeout(pending);
    }
    if delay_ms == 0 {
        flush(store, pane);
        return;
    }
    let handle = set_timeout(delay_ms, move || {
        WRITES.with(|w| w.borrow_mut().entry(pane).or_default().pending = None);
        flush(store, pane);
    });
    WRITES.with(|w| w.borrow_mut().entry(pane).or_default().pending = Some(handle));
}

/// Write pane `pane`'s draft now, if it differs from what was written.
fn flush(store: AppStore, pane: PaneId) {
    if let Some(pending) = WRITES.with(|w| w.borrow_mut().entry(pane).or_default().pending.take()) {
        clear_timeout(pending);
    }
    let Some((store_id, node_id)) = metanode_of(store, pane) else { return };
    let Some(draft) = untracked(|| ui(store, pane).draft.get()) else { return };
    let held = untracked(|| store.get_node_signal(store_id, node_id).and_then(|sig| sig.with(|n| n.metadata.metanode.as_ref().map(|m| m.source.clone()))));
    if held.as_ref() == Some(&draft) {
        return;
    }
    WRITES.with(|w| w.borrow_mut().entry(pane).or_default().sent = Some((draft.clone(), now_ms())));
    store.send(BackendCommand::SetMetanode { store_id, node_id, viewer: Some(LIST_VIEWER.to_string()), source: Some(draft) });
}

/// The draft's parts, tracked (for the setup's reactive reads).
fn current(ui: SetupUi) -> Parts {
    ui.draft.with(|d| d.as_ref().map(parts)).unwrap_or_else(|| parts(&Source::matching(Vec::new())))
}

/// One condition line, as the setup draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Line {
    index: usize,
    kind: Option<Kind>,
}

impl Line {
    fn key(&self) -> String {
        format!("{}:{:?}", self.index, self.kind)
    }
}

/// The lines, keyed by place and kind so typing in one keeps its field.
fn lines(ui: SetupUi) -> Vec<Line> {
    current(ui)
        .all
        .iter()
        .enumerate()
        .map(|(index, c)| {
            Line { index, kind: Kind::of(c) }
        })
        .collect()
}

fn condition_at(ui: SetupUi, index: usize) -> Option<Condition> {
    current(ui).all.get(index).cloned()
}

/// A node named `raw` (`<store>/<node>`), as the setup shows it: its title
/// where this window holds it.
fn node_name(store: AppStore, raw: &str) -> String {
    match parse_node_ref(raw) {
        Some((s, n)) if store.get_node_signal(s, n).is_some() => store.display_label(s, n),
        Some(_) => "a node not open here".to_string(),
        None => String::new(),
    }
}

/// What a "linking to" line names, as it reads now.
fn linked_name(store: AppStore, ui: SetupUi, index: usize) -> String {
    match condition_at(ui, index) {
        Some(Condition::LinksTo { links_to }) => node_name(store, &links_to),
        _ => String::new(),
    }
}

// ── The node picker ────────────────────────────────────────────────────

fn open_picker(store: AppStore, pane: PaneId, target: PickFor) {
    let ui = ui(store, pane);
    ui.pick_query.set(String::new());
    ui.picks.set(Vec::new());
    ui.picking.set(Some(target));
}

fn set_pick_query(store: AppStore, pane: PaneId, query: String) {
    ui(store, pane).pick_query.set(query.clone());
    if let Some(pending) = SEARCH_TIMER.with(|t| t.borrow_mut().take()) {
        clear_timeout(pending);
    }
    if query.trim().is_empty() {
        ui(store, pane).picks.set(Vec::new());
        return;
    }
    let handle = set_timeout(150, move || {
        SEARCH_TIMER.with(|t| t.borrow_mut().take());
        let stores = untracked(|| store.store_ids.get());
        store.send(BackendCommand::Search { query, stores, limit: 30, shown: untracked(|| store.internal_kinds_shown()), purpose: SearchPurpose::MetanodeSetup(pane) });
    });
    SEARCH_TIMER.with(|t| *t.borrow_mut() = Some(handle));
}

/// A search the picker of pane `pane` asked for was answered: each node once.
pub(crate) fn search_answered(store: AppStore, pane: PaneId, results: &Result<Vec<pimble_rpc::SearchResultItem>, String>) {
    let Ok(hits) = results else { return };
    let ui = ui(store, pane);
    if untracked(|| ui.picking.get()).is_none() {
        return;
    }
    let mut seen = std::collections::HashSet::new();
    let rows: Vec<PickRow> = hits
        .iter()
        .filter(|h| seen.insert((h.store_id, h.node_id)))
        .take(8)
        .map(|h| PickRow {
            store_id: h.store_id,
            node_id: h.node_id,
            title: if h.title.trim().is_empty() { "Untitled".to_string() } else { h.title.clone() },
            store_name: store.get_store_signal(h.store_id).map(|sig| untracked(|| sig.with(|s| s.name.clone()))).unwrap_or_default(),
        })
        .collect();
    ui.picks.set(rows);
}

fn pick(store: AppStore, pane: PaneId, row: &PickRow) {
    let ui = ui(store, pane);
    let Some(target) = untracked(|| ui.picking.get()) else { return };
    ui.picking.set(None);
    let raw = node_ref(row.store_id, row.node_id);
    match target {
        PickFor::Condition(index) => change(store, pane, 0, move |p| {
            if let Some(c) = p.all.get_mut(index) {
                *c = Condition::LinksTo { links_to: raw };
            }
        }),
        PickFor::Under => change(store, pane, 0, move |p| p.within = Some(Within::Under(raw))),
    }
}

