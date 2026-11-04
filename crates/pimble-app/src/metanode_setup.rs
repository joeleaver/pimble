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

use pimble_core::metanode::{node_ref, parse_node_ref, Condition, FieldOp, Order, Query, Source, Within, LIST_VIEWER, MAX_LIMIT};
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

/// A kind of condition, as "+ Add a condition" offers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tag,
    NoTag,
    Text,
    Field,
    LinksTo,
    Type,
}

impl Kind {
    /// Every kind, in the order the menu lists them.
    pub const ALL: [Kind; 6] = [Kind::Tag, Kind::NoTag, Kind::Text, Kind::Field, Kind::LinksTo, Kind::Type];

    /// The words that start its line: "tagged [waiting]".
    pub fn words(self) -> &'static str {
        match self {
            Kind::Tag => "tagged",
            Kind::NoTag => "not tagged",
            Kind::Text => "containing",
            Kind::Field => "with",
            Kind::LinksTo => "linking to",
            Kind::Type => "that are",
        }
    }

    /// What the menu calls it.
    pub fn menu_label(self) -> &'static str {
        match self {
            Kind::Tag => "Tagged...",
            Kind::NoTag => "Not tagged...",
            Kind::Text => "Containing words...",
            Kind::Field => "With a field...",
            Kind::LinksTo => "Linking to a node...",
            Kind::Type => "Of a kind...",
        }
    }

    pub fn of(condition: &Condition) -> Option<Kind> {
        Some(match condition {
            Condition::Tag { .. } => Kind::Tag,
            Condition::NoTag { .. } => Kind::NoTag,
            Condition::Text { .. } => Kind::Text,
            Condition::Field { .. } => Kind::Field,
            Condition::LinksTo { .. } => Kind::LinksTo,
            Condition::Type { .. } => Kind::Type,
            Condition::Later(_) => return None,
        })
    }

    /// A new line of this kind, to be filled in.
    pub fn blank(self) -> Condition {
        match self {
            Kind::Tag => Condition::Tag { tag: String::new() },
            Kind::NoTag => Condition::NoTag { no_tag: String::new() },
            Kind::Text => Condition::Text { text: String::new() },
            Kind::Field => Condition::Field { field: String::new(), is: FieldOp::Equal, value: None },
            Kind::LinksTo => Condition::LinksTo { links_to: String::new() },
            Kind::Type => Condition::Type { node_type: "document".to_string() },
        }
    }
}

/// The kinds a "that are" line offers, as stored and as said.
pub const TYPES: [(&str, &str); 6] =
    [("document", "documents"), ("canvas", "canvases"), ("sheet", "sheets"), ("discussion", "discussions"), ("kanban", "kanbans"), ("folder", "folders")];

/// A field line's comparisons, as stored and as said.
pub const FIELD_OPS: [(&str, &str); 7] = [
    ("=", "equal to"),
    ("<", "before / under"),
    ("<=", "on or before / at most"),
    (">=", "on or after / at least"),
    (">", "after / over"),
    ("set", "set"),
    ("empty", "empty"),
];

fn op_key(op: FieldOp) -> &'static str {
    match op {
        FieldOp::Equal => "=",
        FieldOp::Less => "<",
        FieldOp::AtMost => "<=",
        FieldOp::AtLeast => ">=",
        FieldOp::More => ">",
        FieldOp::Set => "set",
        FieldOp::Empty => "empty",
    }
}

fn op_of(key: &str) -> FieldOp {
    match key {
        "<" => FieldOp::Less,
        "<=" => FieldOp::AtMost,
        ">=" => FieldOp::AtLeast,
        ">" => FieldOp::More,
        "set" => FieldOp::Set,
        "empty" => FieldOp::Empty,
        _ => FieldOp::Equal,
    }
}

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

// ── The source as the setup edits it (pure) ─────────────────────────────

/// The parts of a match, read from `source` (a source that is not a match yet
/// reads as an empty one in its metanode's store).
#[derive(Debug, Clone, PartialEq)]
pub struct Parts {
    pub within: Option<Within>,
    pub all: Vec<Condition>,
    pub order: Order,
    pub descending: bool,
    pub limit: u32,
}

pub fn parts(source: &Source) -> Parts {
    match &source.query {
        Query::Match { within, all, order, descending, limit } => {
            Parts { within: within.clone(), all: all.clone(), order: order.clone(), descending: *descending, limit: *limit }
        }
        _ => {
            let empty = Source::matching(Vec::new());
            parts(&empty)
        }
    }
}

/// `source` with `f` applied to its match's parts (a source that was not a
/// match becomes one). What a newer Pimble wrote beside the query is kept.
pub fn edited(source: &Source, f: impl FnOnce(&mut Parts)) -> Source {
    let mut p = parts(source);
    f(&mut p);
    Source {
        query: Query::Match { within: p.within, all: p.all, order: p.order, descending: p.descending, limit: p.limit },
        later: source.later.clone(),
    }
}

/// The text a line's one field holds (a tag, words, a field's name).
pub fn text_of(condition: &Condition) -> String {
    match condition {
        Condition::Tag { tag } => tag.clone(),
        Condition::NoTag { no_tag } => no_tag.clone(),
        Condition::Text { text } => text.clone(),
        Condition::Field { field, .. } => field.clone(),
        Condition::LinksTo { links_to } => links_to.clone(),
        Condition::Type { node_type } => node_type.clone(),
        Condition::Later(_) => String::new(),
    }
}

/// `condition` with its one field set to `text`.
pub fn with_text(condition: &Condition, text: String) -> Condition {
    match condition {
        Condition::Tag { .. } => Condition::Tag { tag: text },
        Condition::NoTag { .. } => Condition::NoTag { no_tag: text },
        Condition::Text { .. } => Condition::Text { text },
        Condition::Field { is, value, .. } => Condition::Field { field: text, is: *is, value: value.clone() },
        Condition::LinksTo { .. } => Condition::LinksTo { links_to: text },
        Condition::Type { .. } => Condition::Type { node_type: text },
        Condition::Later(v) => Condition::Later(v.clone()),
    }
}

/// "From" as the setup's choice names it: `store`, `under`, `everywhere`,
/// or `store:<id>` for another store than the metanode's.
pub fn from_key(within: &Option<Within>, own_store: StoreId) -> String {
    match within {
        None => "store".to_string(),
        Some(Within::Store(s)) if *s == own_store => "store".to_string(),
        Some(Within::Store(s)) => format!("store:{s}"),
        Some(Within::Under(_)) => "under".to_string(),
        Some(Within::Everywhere) => "everywhere".to_string(),
    }
}

/// The `within` a "From" choice means; choosing "under" keeps the node it
/// named, if it named one.
pub fn within_of(key: &str, was: &Option<Within>) -> Option<Within> {
    match key {
        "under" => Some(match was {
            Some(Within::Under(n)) => Within::Under(n.clone()),
            _ => Within::Under(String::new()),
        }),
        "everywhere" => Some(Within::Everywhere),
        other => other.strip_prefix("store:").and_then(|id| StoreId::parse(id).ok()).map(Within::Store),
    }
}

pub fn order_key(order: &Order) -> &'static str {
    match order {
        Order::Title => "title",
        Order::Modified => "modified",
        Order::Created => "created",
        Order::Field(_) => "field",
    }
}

pub fn order_of(key: &str, was: &Order) -> Order {
    match key {
        "modified" => Order::Modified,
        "created" => Order::Created,
        "field" => match was {
            Order::Field(f) => Order::Field(f.clone()),
            _ => Order::Field(String::new()),
        },
        _ => Order::Title,
    }
}

/// "At most" as typed: a whole number from 1 to [`MAX_LIMIT`]; anything else
/// leaves it as it was.
pub fn limit_of(typed: &str, was: u32) -> u32 {
    typed.trim().parse::<u32>().map(|n| n.clamp(1, MAX_LIMIT)).unwrap_or(was)
}

/// What the count line says.
pub fn count_line(count: usize, all: &[Condition]) -> String {
    if all.iter().any(Condition::is_blank) {
        return "Fill in each condition to find nodes.".to_string();
    }
    match count {
        0 => "It finds nothing now.".to_string(),
        1 => "It finds 1 node now.".to_string(),
        n => format!("It finds {n} nodes now."),
    }
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
        store.send(BackendCommand::Search { query, stores, limit: 30, purpose: SearchPurpose::MetanodeSetup(pane) });
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

// ── Drawing ────────────────────────────────────────────────────────────

fn option(value: &str, label: &str) -> SelectOption {
    SelectOption::new(value, label)
}

fn type_options() -> Vec<SelectOption> {
    TYPES.iter().map(|(v, l)| option(v, l)).collect()
}

fn op_options() -> Vec<SelectOption> {
    FIELD_OPS.iter().map(|(v, l)| option(v, l)).collect()
}

fn order_options() -> Vec<SelectOption> {
    vec![option("title", "Title"), option("modified", "Last changed"), option("created", "Created"), option("field", "A field")]
}

fn direction_options() -> Vec<SelectOption> {
    vec![option("asc", "Ascending"), option("desc", "Descending")]
}

/// "From"'s choices: this store, under a node, everywhere, and another store
/// when the source names one.
fn from_options(store: AppStore, pane: PaneId) -> Vec<SelectOption> {
    let ui = ui(store, pane);
    let mut options = vec![option("store", "This store"), option("under", "Under a node"), option("everywhere", "Everywhere")];
    if let Some(Within::Store(other)) = current(ui).within {
        let own = metanode_of(store, pane).map(|(s, _)| s);
        if own != Some(other) {
            let name = store.get_store_signal(other).map(|sig| untracked(|| sig.with(|s| s.name.clone()))).unwrap_or_else(|| "another store".to_string());
            options.push(option(&format!("store:{other}"), &format!("In {name}")));
        }
    }
    options
}

fn placeholder_of(kind: Option<Kind>) -> &'static str {
    if kind == Some(Kind::Text) {
        "words"
    } else {
        "a tag"
    }
}

fn shown(on: bool) -> &'static str {
    if on {
        ""
    } else {
        "display: none;"
    }
}

/// The setup a pane shows for a query metanode. Rendered once per pane, shown
/// while the pane holds one (`MetanodeUi::setup`).
pub(crate) fn render_setup(__scope: &mut RenderScope, store: AppStore, pane: PaneId) -> NodeHandle {
    let state = store.pane(pane);
    let mui = state.metanode_ui;
    let ui = mui.setup_ui;
    let own_store = move || metanode_of(store, pane).map(|(s, _)| s);
    let found_drawn = move || {
        let dark = store.dark_mode.get();
        ui.found_rows.get().into_iter().map(|row| {
            let color = row.color.as_deref().map(|c| format!("color: {};", display_color(c, dark))).unwrap_or_default();
            (row, color.clone(), color)
        }).collect::<Vec<_>>()
    };
    rsx! {
        div {
            class: "pimble-setup",
            style: {move || shown(state.show_metanode.get() && mui.setup.get())},

            div { class: "pimble-setup__heading", "Show nodes" }
            for line in lines(ui) {
                div {
                    key: line.key(),
                    class: "pimble-setup__line",
                    span { class: "pimble-setup__words", {line.kind.map_or("with a condition this Pimble doesn't know", Kind::words)} }
                    // tagged, not tagged, containing: one field.
                    div {
                        class: "pimble-setup__field",
                        style: shown(matches!(line.kind, Some(Kind::Tag | Kind::NoTag | Kind::Text))),
                        TextInput {
                            size: "xs",
                            placeholder: placeholder_of(line.kind),
                            value_fn: {
                                let i = line.index;
                                move || condition_at(ui, i).map(|c| text_of(&c)).unwrap_or_default()
                            },
                            oninput: {
                                let i = line.index;
                                move |v: String| change(store, pane, TYPING_MS, move |p| {
                                    if let Some(c) = p.all.get_mut(i) {
                                        *c = with_text(c, v);
                                    }
                                })
                            },
                        }
                    }
                    // with [field] [op] [value]
                    div {
                        class: "pimble-setup__field",
                        style: shown(line.kind == Some(Kind::Field)),
                        TextInput {
                            size: "xs",
                            placeholder: "a field",
                            value_fn: {
                                let i = line.index;
                                move || match condition_at(ui, i) { Some(Condition::Field { field, .. }) => field, _ => String::new() }
                            },
                            oninput: {
                                let i = line.index;
                                move |v: String| change(store, pane, TYPING_MS, move |p| {
                                    if let Some(Condition::Field { field, .. }) = p.all.get_mut(i) {
                                        *field = v;
                                    }
                                })
                            },
                        }
                    }
                    div {
                        class: "pimble-setup__field",
                        style: shown(line.kind == Some(Kind::Field)),
                        Select {
                            size: "xs",
                            value_fn: {
                                let i = line.index;
                                move || match condition_at(ui, i) { Some(Condition::Field { is, .. }) => op_key(is).to_string(), _ => "=".to_string() }
                            },
                            onchange: {
                                let i = line.index;
                                move |v: String| change(store, pane, 0, move |p| {
                                    if let Some(Condition::Field { is, .. }) = p.all.get_mut(i) {
                                        *is = op_of(&v);
                                    }
                                })
                            },
                            data: op_options(),
                        }
                    }
                    div {
                        class: "pimble-setup__field",
                        style: {
                            let i = line.index;
                            let field = line.kind == Some(Kind::Field);
                            move || shown(field && !matches!(condition_at(ui, i), Some(Condition::Field { is: FieldOp::Set | FieldOp::Empty, .. })))
                        },
                        TextInput {
                            size: "xs",
                            placeholder: "a value",
                            value_fn: {
                                let i = line.index;
                                move || match condition_at(ui, i) { Some(Condition::Field { value, .. }) => value.unwrap_or_default(), _ => String::new() }
                            },
                            oninput: {
                                let i = line.index;
                                move |v: String| change(store, pane, TYPING_MS, move |p| {
                                    if let Some(Condition::Field { value, .. }) = p.all.get_mut(i) {
                                        *value = Some(v);
                                    }
                                })
                            },
                        }
                    }
                    // linking to [a node]
                    span {
                        class: "pimble-setup__node",
                        style: shown(line.kind == Some(Kind::LinksTo)),
                        {move || linked_name(store, ui, line.index)}
                    }
                    div {
                        style: shown(line.kind == Some(Kind::LinksTo)),
                        Button {
                            variant: "default",
                            size: "xs",
                            onclick: {
                                let i = line.index;
                                move || open_picker(store, pane, PickFor::Condition(i))
                            },
                            "Pick a node..."
                        }
                    }
                    // that are [kind]
                    div {
                        class: "pimble-setup__field",
                        style: shown(line.kind == Some(Kind::Type)),
                        Select {
                            size: "xs",
                            value_fn: {
                                let i = line.index;
                                move || match condition_at(ui, i) { Some(Condition::Type { node_type }) => node_type, _ => String::new() }
                            },
                            onchange: {
                                let i = line.index;
                                move |v: String| change(store, pane, 0, move |p| {
                                    if let Some(c) = p.all.get_mut(i) {
                                        *c = Condition::Type { node_type: v };
                                    }
                                })
                            },
                            data: type_options(),
                        }
                    }
                    div {
                        class: "pimble-setup__remove",
                        title: "Remove this condition",
                        ActionIcon {
                            icon: TablerIcon::X,
                            variant: "subtle",
                            size: "xs",
                            onclick: {
                                let i = line.index;
                                move || change(store, pane, 0, move |p| {
                                    if i < p.all.len() {
                                        p.all.remove(i);
                                    }
                                })
                            },
                        }
                    }
                }
            }
            div {
                class: "pimble-setup__add",
                DropdownMenu {
                    opened_fn: move || ui.adding.get(),
                    on_close: move || ui.adding.set(false),
                    close_on_click_outside: true,
                    close_on_item_click: true,
                    position: "bottom-start",
                    DropdownMenuTarget {
                        div {
                            style: "display: inline-flex;",
                            Button {
                                variant: "subtle",
                                size: "xs",
                                onclick: move || ui.adding.set(!untracked(|| ui.adding.get())),
                                "+ Add a condition"
                            }
                        }
                    }
                    DropdownMenuDropdown {
                        for kind in Kind::ALL {
                            DropdownMenuItem {
                                key: kind.menu_label(),
                                onclick: move || {
                                    ui.adding.set(false);
                                    change(store, pane, 0, move |p| p.all.push(kind.blank()));
                                },
                                {kind.menu_label()}
                            }
                        }
                    }
                }
            }

            div { class: "pimble-setup__heading", "From" }
            div {
                class: "pimble-setup__line",
                div {
                    class: "pimble-setup__field",
                    Select {
                        size: "xs",
                        value_fn: move || own_store().map(|own| from_key(&current(ui).within, own)).unwrap_or_default(),
                        onchange: move |v: String| change(store, pane, 0, move |p| p.within = within_of(&v, &p.within)),
                        data: {move || from_options(store, pane)},
                    }
                }
                span {
                    class: "pimble-setup__node",
                    style: {move || shown(matches!(current(ui).within, Some(Within::Under(_))))},
                    {move || match current(ui).within { Some(Within::Under(n)) => node_name(store, &n), _ => String::new() }}
                }
                div {
                    style: {move || shown(matches!(current(ui).within, Some(Within::Under(_))))},
                    Button {
                        variant: "default",
                        size: "xs",
                        onclick: move || open_picker(store, pane, PickFor::Under),
                        "Pick a node..."
                    }
                }
            }

            // The node picker, under whatever asked for it.
            div {
                class: "pimble-setup__picker",
                style: {move || shown(ui.picking.get().is_some())},
                div {
                    class: "pimble-setup__line",
                    div {
                        class: "pimble-setup__field pimble-setup__field--wide",
                        TextInput {
                            size: "xs",
                            placeholder: "Type to find a node",
                            value_fn: move || ui.pick_query.get(),
                            oninput: move |v: String| set_pick_query(store, pane, v),
                        }
                    }
                    Button { variant: "subtle", size: "xs", onclick: move || ui.picking.set(None), "Cancel" }
                }
                for row in ui.picks.get() {
                    div {
                        key: format!("{}/{}", row.store_id, row.node_id),
                        class: "pimble-metanode__row",
                        onclick: {
                            let row = row.clone();
                            move || pick(store, pane, &row)
                        },
                        span { class: "pimble-metanode__label", {row.title.clone()} }
                        span { class: "pimble-setup__dim", {row.store_name.clone()} }
                    }
                }
            }

            div { class: "pimble-setup__heading", "Order" }
            div {
                class: "pimble-setup__line",
                span { class: "pimble-setup__words", "by" }
                div {
                    class: "pimble-setup__field",
                    Select {
                        size: "xs",
                        value_fn: move || order_key(&current(ui).order).to_string(),
                        onchange: move |v: String| change(store, pane, 0, move |p| p.order = order_of(&v, &p.order)),
                        data: order_options(),
                    }
                }
                div {
                    class: "pimble-setup__field",
                    style: {move || shown(matches!(current(ui).order, Order::Field(_)))},
                    TextInput {
                        size: "xs",
                        placeholder: "a field",
                        value_fn: move || match current(ui).order { Order::Field(f) => f, _ => String::new() },
                        oninput: move |v: String| change(store, pane, TYPING_MS, move |p| p.order = Order::Field(v)),
                    }
                }
                div {
                    class: "pimble-setup__field",
                    Select {
                        size: "xs",
                        value_fn: move || if current(ui).descending { "desc".to_string() } else { "asc".to_string() },
                        onchange: move |v: String| change(store, pane, 0, move |p| p.descending = v == "desc"),
                        data: direction_options(),
                    }
                }
                span { class: "pimble-setup__words", "at most" }
                div {
                    class: "pimble-setup__field pimble-setup__field--narrow",
                    TextInput {
                        size: "xs",
                        value_fn: move || current(ui).limit.to_string(),
                        oninput: move |v: String| change(store, pane, TYPING_MS, move |p| p.limit = limit_of(&v, p.limit)),
                    }
                }
            }

            div {
                class: "pimble-setup__heading",
                {move || count_line(ui.found_count.get(), &current(ui).all)}
            }
            for item in found_drawn() {
                div {
                    key: format!("{}/{} {} {} {}", item.0.store_id, item.0.node_id, item.0.label, item.0.icon, item.1),
                    class: "pimble-metanode__row pimble-setup__found",
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
            div {
                class: "pimble-setup__dim pimble-setup__more",
                style: {move || shown(ui.found_count.get() > FIRST_ROWS)},
                {move || format!("and {} more, in the explorer under this metanode", ui.found_count.get().saturating_sub(FIRST_ROWS))}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setup_edits_its_source_as_a_match() {
        // A source that is not a match yet becomes one.
        let s = edited(&Source::children(), |p| p.all.push(Kind::Tag.blank()));
        assert!(s.is_match());
        let s = edited(&s, |p| p.all[0] = with_text(&p.all[0], "waiting".into()));
        assert_eq!(parts(&s).all, vec![Condition::Tag { tag: "waiting".into() }]);
        // Every kind's blank line is blank, and a typed one is not.
        for kind in Kind::ALL {
            let blank = kind.blank();
            assert_eq!(Kind::of(&blank), Some(kind));
            assert_eq!(blank.is_blank(), kind != Kind::Type, "{kind:?}");
        }
        assert!(!Condition::Field { field: "Due".into(), is: FieldOp::Set, value: None }.is_blank());
        assert!(Condition::Field { field: "Due".into(), is: FieldOp::Less, value: None }.is_blank());
        // What was written beside the query by a newer Pimble is kept.
        let mut newer = Source::matching(Vec::new());
        newer.later.insert("group".into(), serde_json::json!("x"));
        assert_eq!(edited(&newer, |p| p.limit = 5).later, newer.later);
    }

    #[test]
    fn from_order_and_limit_read_as_chosen() {
        let own = StoreId::new();
        let other = StoreId::new();
        assert_eq!(from_key(&None, own), "store");
        assert_eq!(from_key(&Some(Within::Store(own)), own), "store");
        assert_eq!(from_key(&Some(Within::Store(other)), own), format!("store:{other}"));
        assert_eq!(within_of("store", &Some(Within::Everywhere)), None);
        assert_eq!(within_of("everywhere", &None), Some(Within::Everywhere));
        assert_eq!(within_of(&format!("store:{other}"), &None), Some(Within::Store(other)));
        // Choosing "under" again keeps the node it named.
        let under = Some(Within::Under("a/b".into()));
        assert_eq!(within_of("under", &under), under);
        assert_eq!(within_of("under", &None), Some(Within::Under(String::new())));
        assert_eq!(order_of("field", &Order::Field("Due".into())), Order::Field("Due".into()));
        assert_eq!(order_of("modified", &Order::Title), Order::Modified);
        assert_eq!(order_key(&Order::Created), "created");
        assert_eq!(limit_of("20", 50), 20);
        assert_eq!(limit_of("0", 50), 1);
        assert_eq!(limit_of("100000", 50), MAX_LIMIT);
        assert_eq!(limit_of("lots", 50), 50);
        for (key, _) in FIELD_OPS {
            assert_eq!(op_key(op_of(key)), key);
        }
    }

    #[test]
    fn the_count_line() {
        assert_eq!(count_line(0, &[]), "It finds nothing now.");
        assert_eq!(count_line(1, &[]), "It finds 1 node now.");
        assert_eq!(count_line(7, &[Condition::Tag { tag: "x".into() }]), "It finds 7 nodes now.");
        assert_eq!(count_line(0, &[Condition::Tag { tag: String::new() }]), "Fill in each condition to find nodes.");
    }
}
