//! Sheet nodes (docs/SHEETS_CONTRACT.md section 1, wave 1): a node made of
//! small live tables, shown in a pane instead of the editor, on the desktop
//! and in the browser from this one module.
//!
//! A pane showing a sheet holds the node's document ([`Session`]), the same
//! way a board pane does: it opens from the cached content, reconciles with
//! the server, takes peers' updates, and sends every edit it makes through
//! `applyEdit` as an ordinary yrs update. Every change, local or remote,
//! recomputes the pane's tables with the formula engine (`pimble-sheet`) and
//! hands the result to the view as plain values ([`TableView`]); the view
//! ([`ui`]) only shows them and turns presses and keys into the edits here.
//!
//! Nothing computed is stored: a cell's value, a row's label, a summary's
//! rows are all derived from the document every time.
//!
//! A pane holding a note in its editor holds a session too, over the note's
//! document ([`text_opened`], wave 2): its tables are the note's sheets in
//! text, each drawn where its `embed` atom is by a rinch node view
//! ([`in_text`]). That session hears the editor's own typing
//! ([`text_delta`]) and hands its table edits to the editors holding the
//! note, so every copy of the document in this window stays whole.

pub(crate) mod in_text;
pub(crate) mod reads;
mod ui;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use pimble_core::{node_types, NodeId, StoreId};
use pimble_crdt::NodeDoc;
use pimble_sheet::doc::{self, Placement, Sheets};
use pimble_sheet::formula::{self, ComputedTable, Engine, ErrorCode, Value};
use pimble_sheet::{input, Book, ColumnType, Format, NodeInfo, NodeRef, Reach, SheetsSnap, Stored};
use rinch::prelude::*;

use crate::panes::{PaneId, MAX_PANES};
use crate::protocol::{BackendCommand, NodeInit};
use crate::state::{ActiveEdit, AppStore};

pub(crate) use ui::render_sheet;

/// A new sheet node's icon (a Tabler icon name).
pub const SHEET_ICON: &str = "table";

// ── What the view shows ──────────────────────────────────────────────────

/// A cell, by table, row (`None`: the new-row line) and column key (a
/// column's id, or `r:<index>` for a summary's result column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellKey {
    pub table: String,
    pub row: Option<String>,
    pub col: String,
}

/// A link cell's chip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chip {
    Row,
    Node(StoreId, NodeId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CellView {
    pub text: String,
    /// The error's sentence, when the cell is one (`text` is its code).
    pub error: Option<String>,
    pub chip: Option<Chip>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColView {
    pub key: String,
    pub name: String,
    /// The faint mark after the name (`#`, `$`, `ƒ`, `→ Vehicles`).
    pub mark: String,
    /// The full type, or the formula, for the tooltip.
    pub tip: String,
    pub numeric: bool,
    pub computed: bool,
    /// A summary's result column (renamed, not edited).
    pub result: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowView {
    pub id: String,
    pub cells: Vec<CellView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableView {
    pub id: String,
    pub name: String,
    pub summary: bool,
    /// A summary's formula, shown with names.
    pub source: String,
    /// A summary whose formula fails: its code and sentence.
    pub error: Option<String>,
    pub columns: Vec<ColView>,
    pub rows: Vec<RowView>,
    /// Changes whenever anything above does: the view keys a table by id and
    /// version, so a table that did not change keeps its DOM.
    pub version: u64,
}

/// A column's settings, being changed (`col` is `None` for a new column).
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnForm {
    pub table: String,
    pub col: Option<String>,
    /// Renaming a summary's result column: only the name changes.
    pub result: Option<usize>,
    pub name: String,
    pub kind: String,
    pub target: String,
    /// A link-to-a-node column's "Nodes under": a node's title or path, as
    /// typed; empty for anywhere.
    pub under: String,
    pub formula: String,
    pub format: String,
    pub error: Option<String>,
}

/// One pane's sheet view state. Signals only, so it is `Copy`.
#[derive(Clone, Copy)]
pub struct SheetUi {
    pub tables: Signal<Vec<TableView>>,
    /// The cell being typed into.
    pub editing: Signal<Option<CellKey>>,
    pub edit_text: Signal<String>,
    /// A link cell's choices while typing, and the highlighted one.
    pub choices: Signal<Vec<String>>,
    pub choice: Signal<Option<usize>>,
    /// The computed cell the formula bar is about.
    pub selected: Signal<Option<CellKey>>,
    pub formula_text: Signal<String>,
    /// The formula bar's sentence (a formula's error, a cell's).
    pub formula_note: Signal<Option<String>>,
    pub column_form: Signal<Option<ColumnForm>>,
    /// A refused edit's sentence, shown over the tables until the next edit.
    pub message: Signal<Option<String>>,
    /// The tables a note's text shows (its `embed` atoms), in order.
    pub embedded: Signal<Vec<String>>,
}

impl SheetUi {
    pub fn new() -> Self {
        Self {
            tables: Signal::new(Vec::new()),
            editing: Signal::new(None),
            edit_text: Signal::new(String::new()),
            choices: Signal::new(Vec::new()),
            choice: Signal::new(None),
            selected: Signal::new(None),
            formula_text: Signal::new(String::new()),
            formula_note: Signal::new(None),
            column_form: Signal::new(None),
            message: Signal::new(None),
            embedded: Signal::new(Vec::new()),
        }
    }
}

impl Default for SheetUi {
    fn default() -> Self {
        Self::new()
    }
}

// ── Sessions ─────────────────────────────────────────────────────────────

/// One pane's sheet node: its document and the handle on its tables.
struct Session {
    store_id: StoreId,
    node_id: NodeId,
    doc: NodeDoc,
    sheets: Sheets,
    undo: yrs::undo::UndoManager<()>,
    /// The note's sheets in text: the pane's editor holds the note, and its
    /// subscription and node cache are the editor's.
    in_text: bool,
    /// The other notes its formulas read, as of the last recompute
    /// ([`reads`] holds them).
    reads: Vec<NodeRef>,
}

thread_local! {
    static SESSIONS: RefCell<[Option<Session>; MAX_PANES]> = const { RefCell::new([None, None, None, None]) };
    static APP: Cell<Option<AppStore>> = const { Cell::new(None) };
}

fn with_session<R>(pane: PaneId, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].as_mut().map(f))
}

/// The panes holding node `node_id`'s tables, as a sheet node or in text.
fn panes_holding(store_id: StoreId, node_id: NodeId) -> Vec<PaneId> {
    PaneId::ALL
        .into_iter()
        .filter(|pane| with_session(*pane, |s| (s.store_id, s.node_id) == (store_id, node_id)).unwrap_or(false))
        .collect()
}

/// Whether pane `pane` holds a note's sheets in text.
pub(crate) fn holds_text(pane: PaneId) -> bool {
    with_session(pane, |s| s.in_text).unwrap_or(false)
}

fn origin(pane: PaneId) -> String {
    format!("pimble-sheet-pane-{}", pane.index())
}

fn undo_options() -> yrs::undo::Options<()> {
    // One edit is one undo step: a cell committed, a column changed.
    // Spelled out: the browser build of yrs has no default clock.
    yrs::undo::Options {
        capture_timeout_millis: 0,
        tracked_origins: Default::default(),
        capture_transaction: None,
        timestamp: std::sync::Arc::new(|| crate::boards::now_ms() as u64),
        init_undo_stack: Vec::new(),
        init_redo_stack: Vec::new(),
    }
}

/// Show sheet node `node_id` in pane `pane`.
pub(crate) fn start(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, content: &[u8]) {
    crate::editor::stop_editing(store, pane);
    crate::boards::stop(store, pane);
    stop(store, pane);
    open(store, pane, store_id, node_id, content, false);
}

/// Pane `pane`'s editor opened note `node_id` from `content`: hold its
/// document for the sheets in its text.
pub(crate) fn text_opened(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, content: &[u8]) {
    stop(store, pane);
    open(store, pane, store_id, node_id, content, true);
}

/// Pane `pane`'s editor let go of its note.
pub(crate) fn text_closed(store: AppStore, pane: PaneId) {
    if holds_text(pane) {
        stop(store, pane);
    }
}

fn open(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, content: &[u8], in_text: bool) {
    APP.with(|app| app.set(Some(store)));
    let first = !store.is_editing(store_id, node_id);
    // A pane opening a sheet another pane holds starts from that pane's copy.
    let sibling = panes_holding(store_id, node_id)
        .into_iter()
        .find_map(|other| with_session(other, |s| s.doc.save()));
    let bytes = sibling.as_deref().unwrap_or(content);
    let doc = match NodeDoc::load(bytes) {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!("sheet {node_id} could not be read ({e}); showing it empty until the server's copy arrives");
            NodeDoc::new()
        }
    };
    let sheets = Sheets::of_node(&doc).with_origin(origin(pane));
    let undo = sheets.undo_manager(undo_options());
    SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()] = Some(Session { store_id, node_id, doc, sheets, undo, in_text, reads: Vec::new() }));
    let state = store.pane(pane);
    let ui = state.sheet_ui;
    ui.editing.set(None);
    ui.selected.set(None);
    ui.column_form.set(None);
    ui.message.set(None);
    if !in_text {
        state.sheet.set(Some(ActiveEdit { store_id, node_id }));
        if first {
            store.send(BackendCommand::SubscribeNodeChanges { store_id, node_id });
        }
    }
    request_reconcile(store, pane);
    recompute(store, pane);
    if in_text {
        in_text::text_changed(pane);
    }
}

/// Stop showing pane `pane`'s sheet, if it shows one: the node cache gets the
/// document as this pane last had it, and the last pane holding the node lets
/// go of its subscription.
pub(crate) fn stop(store: AppStore, pane: PaneId) {
    let Some(session) = SESSIONS.with(|sessions| sessions.borrow_mut()[pane.index()].take()) else { return };
    end_typing();
    let state = store.pane(pane);
    if let Some(sig) = store.get_node_signal(session.store_id, session.node_id) {
        let bytes = session.doc.save();
        if session.in_text {
            // The editor wrote its copy first: this one is merged into it.
            sig.update(|node| node.content = merged(&node.content, &bytes));
        } else {
            sig.update(|node| node.content = bytes);
        }
    }
    state.sheet_ui.tables.set(Vec::new());
    state.sheet_ui.editing.set(None);
    state.sheet_ui.embedded.set(Vec::new());
    // What it read may be read by nothing now.
    reads::sync(store);
    if session.in_text {
        return;
    }
    state.sheet.set(None);
    if !store.is_editing(session.store_id, session.node_id) {
        store.send(BackendCommand::UnsubscribeNodeChanges { store_id: session.store_id, node_id: session.node_id });
    }
}

/// `ours` merged into the document `cached` (either may be empty).
fn merged(cached: &[u8], ours: &[u8]) -> Vec<u8> {
    if cached.is_empty() {
        return ours.to_vec();
    }
    match NodeDoc::load(cached) {
        Ok(mut doc) => match doc.apply_update(ours) {
            Ok(_) => doc.save(),
            Err(_) => ours.to_vec(),
        },
        Err(_) => ours.to_vec(),
    }
}

/// An edit typed in an editor of this window holding note `node_id`: the
/// sessions holding the note's sheets take it too (nothing to recompute
/// unless it touched the tables).
pub(crate) fn text_delta(store: AppStore, store_id: StoreId, node_id: NodeId, delta: &[u8]) {
    for pane in panes_holding(store_id, node_id) {
        let data = with_session(pane, |s| s.doc.apply_update(delta).map(|e| e.data).unwrap_or(false)).unwrap_or(false);
        if data {
            recompute(store, pane);
        }
    }
}

/// Ask the server for what it has beyond pane `pane`'s copy of its sheet.
pub(crate) fn request_reconcile(store: AppStore, pane: PaneId) {
    if let Some((store_id, node_id, state_vector)) = with_session(pane, |s| (s.store_id, s.node_id, s.doc.state_vector())) {
        store.send(BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector });
    }
}

/// Every sheet pane holding the node asks again (the connection came back).
pub(crate) fn request_reconcile_all(store: AppStore, store_id: StoreId, node_id: NodeId) {
    for pane in panes_holding(store_id, node_id) {
        request_reconcile(store, pane);
    }
}

/// The server's answer to a reconcile: merged into every sheet pane holding
/// the node, then what the server lacks goes back.
pub(crate) fn apply_reconcile(store: AppStore, store_id: StoreId, node_id: NodeId, diff: &[u8], server_state_vector: &[u8]) {
    let writable = store.node_access(store_id, node_id).allows_write();
    for pane in panes_holding(store_id, node_id) {
        let ours = with_session(pane, |s| {
            if !diff.is_empty() {
                if let Err(e) = s.doc.apply_update(diff) {
                    tracing::warn!("a sheet's reconcile did not apply: {e}");
                }
            }
            if writable {
                s.doc.diff_if_peer_lacks_it(server_state_vector, diff).ok().flatten()
            } else {
                None
            }
        })
        .flatten();
        if let Some(ours) = ours {
            store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes: encode(&ours) });
        }
        recompute(store, pane);
    }
}

/// A peer's update of a node: applied in every pane holding it as a sheet.
pub(crate) fn apply_remote(store: AppStore, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    // A note a sheet reads: its cached copy, which formulas read, takes it.
    reads::remote(store, store_id, node_id, bytes);
    for pane in panes_holding(store_id, node_id) {
        apply_to(store, pane, store_id, node_id, bytes);
    }
    // A formula elsewhere may read this node.
    recompute_others(store, store_id, node_id);
}

fn apply_to(store: AppStore, pane: PaneId, store_id: StoreId, node_id: NodeId, bytes: &[u8]) {
    let applied = with_session(pane, |s| {
        if (s.store_id, s.node_id) != (store_id, node_id) {
            return false;
        }
        if let Err(e) = s.doc.apply_update(bytes) {
            tracing::warn!("a sheet update did not apply: {e}");
        }
        true
    });
    if applied == Some(true) {
        recompute(store, pane);
    }
}

/// A node this window learned about (loaded, renamed): sheets whose formulas
/// name nodes recompute.
pub(crate) fn node_loaded(store: AppStore) {
    for pane in PaneId::ALL {
        if with_session(pane, |_| ()).is_some() {
            recompute(store, pane);
        }
    }
}

fn recompute_others(store: AppStore, store_id: StoreId, node_id: NodeId) {
    for pane in PaneId::ALL {
        let other = with_session(pane, |s| (s.store_id, s.node_id) != (store_id, node_id)).unwrap_or(false);
        if other {
            recompute(store, pane);
        }
    }
}

fn encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Send an update this pane made: to the server through `applyEdit`, and to
/// every other pane of this window holding the sheet, a turn later.
fn send_update(store: AppStore, from: PaneId, store_id: StoreId, node_id: NodeId, bytes: Vec<u8>) {
    store.send(BackendCommand::BroadcastChanges { store_id, node_id, changes: encode(&bytes) });
    let others: Vec<PaneId> = panes_holding(store_id, node_id).into_iter().filter(|p| *p != from).collect();
    let deliver = move || {
        for other in others {
            apply_to(store, other, store_id, node_id, &bytes);
        }
        // The note's text is open in editors here: their documents take it.
        for pane in store.panes_editing(store_id, node_id) {
            crate::editor::editor(pane).collab_receive(&bytes);
        }
        recompute_others(store, store_id, node_id);
    };
    #[cfg(test)]
    deliver();
    #[cfg(not(test))]
    let _ = set_timeout(0, deliver);
}

// ── Making one ───────────────────────────────────────────────────────────

/// A new sheet's first table, written in the edit that makes the node (two
/// devices making one node's first table would otherwise both make one).
pub(crate) fn new_sheet_content() -> Option<Vec<u8>> {
    let sheets = Sheets::standalone();
    let edited = sheets.edit(|t| {
        let table = t.create_table("Table1", Placement::Last)?;
        t.add_column(&table, "name", ColumnType::Text, Placement::Last)?;
        t.add_column(&table, "amount", ColumnType::Number, Placement::Last)?;
        Ok(())
    });
    match edited.result {
        Ok(()) => edited.update,
        Err(e) => {
            tracing::warn!("a new sheet could not be made: {e}");
            None
        }
    }
}

/// A row's "New Sheet": a sheet node under `parent_id`.
pub(crate) fn new_sheet(store: AppStore, store_id: StoreId, parent_id: NodeId) {
    let Some(content) = new_sheet_content() else { return };
    store.send(BackendCommand::CreateNode {
        store_id,
        parent_id: Some(parent_id),
        title: "Sheet".to_string(),
        init: Some(NodeInit {
            node_type: node_types::SHEET.to_string(),
            content,
            custom: vec![
                (pimble_core::custom_keys::ICON.to_string(), serde_json::json!(SHEET_ICON)),
                (pimble_core::custom_keys::EXPLICIT_TITLE.to_string(), serde_json::json!(true)),
            ],
            request: None,
        }),
    });
}

// ── What formulas see ────────────────────────────────────────────────────

/// The engine's view of the world from pane `pane`: this node's tables from
/// the pane's own document, other nodes' from the window's node cache.
struct AppBook {
    store: AppStore,
    home: NodeRef,
    snaps: HashMap<NodeRef, SheetsSnap>,
    /// Read notes whose tables cannot be read, and why.
    absent: HashMap<NodeRef, reads::Where>,
    /// The other notes its formulas read.
    reads: Vec<NodeRef>,
    /// Untitled notes' shown labels (their first words), worked out once.
    labels: RefCell<HashMap<NodeRef, String>>,
    today: f64,
}

fn node_ref(store_id: StoreId, node_id: NodeId) -> NodeRef {
    NodeRef { store: store_id, node: node_id }
}

/// The nodes a stored formula names (`{n:<store>/<node>}`).
fn named_nodes(stored: &str, out: &mut Vec<NodeRef>) {
    let mut rest = stored;
    while let Some(at) = rest.find("{n:") {
        rest = &rest[at + 3..];
        let Some(end) = rest.find('}') else { break };
        if let Some((s, n)) = rest[..end].split_once('/') {
            if let (Ok(s), Ok(n)) = (uuid::Uuid::parse_str(s), uuid::Uuid::parse_str(n)) {
                out.push(NodeRef { store: StoreId(s), node: NodeId(n) });
            }
        }
        rest = &rest[end..];
    }
}

fn formulas_of(snap: &SheetsSnap) -> Vec<&str> {
    let mut all = Vec::new();
    for t in &snap.tables {
        if let Some(s) = &t.source {
            all.push(s.as_str());
        }
        for c in &t.columns {
            if let ColumnType::Formula { formula, .. } = &c.ty {
                all.push(formula.as_str());
            }
        }
    }
    all
}

fn cached_sheets(store: AppStore, node: &NodeRef) -> Option<SheetsSnap> {
    // A pane of this window holding the note has the freshest copy; the
    // cache is as old as the last fetch.
    if let Some(live) = panes_holding(node.store, node.node).into_iter().find_map(|p| with_session(p, |s| s.sheets.read())) {
        return Some(live);
    }
    let sig = store.get_node_signal(node.store, node.node)?;
    let bytes = untracked(|| sig.with(|n| n.content.clone()));
    let doc = NodeDoc::load(&bytes).ok()?;
    Some(Sheets::of_node(&doc).read())
}

impl AppBook {
    fn new(store: AppStore, home: NodeRef, snap: SheetsSnap) -> Self {
        let mut snaps = HashMap::new();
        let mut absent = HashMap::new();
        let mut read_notes = Vec::new();
        let mut pending: Vec<NodeRef> = Vec::new();
        for f in formulas_of(&snap) {
            named_nodes(f, &mut pending);
        }
        snaps.insert(home, snap);
        // Other notes' tables, and the notes their formulas name, a few
        // hops deep (each read once).
        let mut hops = 0;
        while !pending.is_empty() && hops < 4 {
            let mut next = Vec::new();
            for node in pending.drain(..) {
                if snaps.contains_key(&node) || absent.contains_key(&node) {
                    continue;
                }
                read_notes.push(node);
                match reads::reach(store, &node) {
                    reads::Where::Read(at) => {
                        if let Some(s) = cached_sheets(store, &at) {
                            for f in formulas_of(&s) {
                                named_nodes(f, &mut next);
                            }
                            snaps.insert(node, s);
                        } else if store.get_node_signal(at.store, at.node).is_none() {
                            store.send(BackendCommand::GetNode { store_id: at.store, node_id: at.node });
                        }
                    }
                    other => {
                        absent.insert(node, other);
                    }
                }
            }
            pending = next;
            hops += 1;
        }
        // The nodes its cells link to show by title: fetch any not here yet
        // (`node_loaded` recomputes when one arrives).
        if let Some(home_snap) = snaps.get(&home) {
            let mut asked = std::collections::HashSet::new();
            for t in &home_snap.tables {
                for r in &t.rows {
                    for v in r.cells.values() {
                        if let Stored::Node(n) = v {
                            if store.get_node_signal(n.store, n.node).is_none() && asked.insert(*n) {
                                store.send(BackendCommand::GetNode { store_id: n.store, node_id: n.node });
                            }
                        }
                    }
                }
            }
        }
        let today = chrono::Local::now().date_naive();
        use chrono::Datelike;
        let today = formula::dates::serial(today.year() as i64, today.month() as i64, today.day() as i64) as f64;
        Self { store, home, snaps, absent, reads: read_notes, labels: RefCell::new(HashMap::new()), today }
    }

    /// A note's name as the tree shows it: its title, or for an untitled
    /// note its first words (what a person reads there and types in a
    /// formula), and its parent.
    fn title(&self, node: &NodeRef) -> Option<(String, Option<NodeId>)> {
        let sig = self.store.get_node_signal(node.store, node.node)?;
        let (title, parent) = untracked(|| sig.with(|n| (n.metadata.title.clone(), n.parent_id)));
        if !title.is_empty() {
            return Some((title, parent));
        }
        let label = self
            .labels
            .borrow_mut()
            .entry(*node)
            .or_insert_with(|| untracked(|| sig.with(crate::state::display_label_from_node)))
            .clone();
        Some((label, parent))
    }
}

impl Book for AppBook {
    fn home(&self) -> NodeRef {
        self.home
    }

    fn sheets(&self, node: &NodeRef) -> Reach<'_> {
        if let Some(s) = self.snaps.get(node) {
            return Reach::Ready(s);
        }
        match self.absent.get(node) {
            Some(reads::Where::Deleted) => return Reach::Deleted,
            Some(reads::Where::NoAccess) => return Reach::NoAccess,
            Some(reads::Where::Missing) => return Reach::Missing,
            _ => {}
        }
        // Asked for (`AppBook::new`) and not here yet, or held but not
        // readable as a document yet.
        Reach::Loading
    }

    fn node(&self, node: &NodeRef) -> Option<NodeInfo> {
        let (title, parent) = self.title(node)?;
        Some(NodeInfo { title, fields: Vec::new(), parent: parent.map(|p| node_ref(node.store, p)) })
    }

    fn find_nodes(&self, name_or_path: &str) -> Vec<NodeRef> {
        let parts: Vec<String> = name_or_path.split('/').map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()).collect();
        let Some(last) = parts.last() else { return Vec::new() };
        let all: Vec<(StoreId, NodeId)> = untracked(|| self.store.node_data.with(|m| m.keys().copied().collect()));
        let mut found = Vec::new();
        for (s, n) in all {
            let r = node_ref(s, n);
            let Some((title, mut parent)) = self.title(&r) else { continue };
            if title.trim().to_lowercase() != *last {
                continue;
            }
            // Every earlier part names the next parent up, in order: a path
            // is a run of titles straight down the tree, never skipping a
            // level (a same-titled note further down must not match too).
            let mut ok = true;
            for part in parts.iter().rev().skip(1) {
                let Some(p) = parent else {
                    ok = false;
                    break;
                };
                let Some((t, up)) = self.title(&node_ref(s, p)) else {
                    ok = false;
                    break;
                };
                if t.trim().to_lowercase() != *part {
                    ok = false;
                    break;
                }
                parent = up;
            }
            if ok {
                found.push(r);
            }
        }
        found
    }

    fn display_name(&self, node: &NodeRef) -> String {
        let Some((title, mut parent)) = self.title(node) else { return "That note".to_string() };
        let mut path = title.clone();
        // The shortest path from an ancestor that names this node alone.
        for _ in 0..8 {
            if self.find_nodes(&path).len() <= 1 {
                break;
            }
            let Some(p) = parent else { break };
            let Some((t, up)) = self.title(&node_ref(node.store, p)) else { break };
            path = format!("{t}/{path}");
            parent = up;
        }
        path
    }

    fn today(&self) -> f64 {
        self.today
    }
}

fn book_for(store: AppStore, pane: PaneId) -> Option<AppBook> {
    let (home, snap) = with_session(pane, |s| (node_ref(s.store_id, s.node_id), s.sheets.read()))?;
    Some(AppBook::new(store, home, snap))
}

// ── Computing the view ───────────────────────────────────────────────────

fn mark_of(ty: &ColumnType, snap: &SheetsSnap, book: &AppBook) -> (String, String) {
    match ty {
        ColumnType::Text => (String::new(), "Text".to_string()),
        ColumnType::Number => ("#".to_string(), "Number".to_string()),
        ColumnType::Money => ("$".to_string(), "Money".to_string()),
        ColumnType::Date => ("date".to_string(), "Date".to_string()),
        ColumnType::Hours => ("h".to_string(), "Hours".to_string()),
        ColumnType::RowLink { table } => {
            let name = snap.table(table).map(|t| t.name.clone()).unwrap_or_else(|| "?".to_string());
            (format!("→ {name}"), format!("Links to a row of {name}"))
        }
        ColumnType::NodeLink { under } => match under.and_then(|u| book.node(&u)) {
            Some(info) => (format!("→ {}", info.title), format!("Links to a node under {}", info.title)),
            None => ("→ node".to_string(), "Links to any node".to_string()),
        },
        ColumnType::Formula { .. } => ("ƒ".to_string(), String::new()),
    }
}

fn is_numeric(f: Format) -> bool {
    matches!(f, Format::Number | Format::Money | Format::Hours | Format::Date | Format::Percent)
}

fn cell_view(engine: &Engine<'_>, v: &Value, f: Format) -> CellView {
    match v {
        Value::Error(e) => CellView {
            text: if e.code == ErrorCode::Loading { "Loading…".to_string() } else { e.code.code().to_string() },
            error: Some(e.message.clone()),
            chip: None,
        },
        Value::Row(r) => CellView { text: engine.row_label(r), error: None, chip: Some(Chip::Row) },
        Value::Node(n) => CellView { text: engine.format_value(v, f), error: None, chip: Some(Chip::Node(n.store, n.node)) },
        _ => CellView { text: engine.format_value(v, f), error: None, chip: None },
    }
}

fn table_view(engine: &Engine<'_>, book: &AppBook, snap: &SheetsSnap, computed: &ComputedTable) -> TableView {
    let table = snap.table(&computed.id);
    let mut result_index = 0;
    let columns: Vec<ColView> = computed
        .columns
        .iter()
        .map(|c| {
            let own = c.id.as_ref().and_then(|id| table.and_then(|t| t.column(id)));
            match own {
                Some(col) => {
                    let (mark, mut tip) = mark_of(&col.ty, snap, book);
                    if let ColumnType::Formula { formula, .. } = &col.ty {
                        tip = format!("Formula: {}", formula::to_shown(book, Some(&computed.id), formula));
                    }
                    ColView {
                        key: col.id.clone(),
                        name: col.name.clone(),
                        mark,
                        tip,
                        numeric: is_numeric(c.format),
                        computed: c.computed,
                        result: false,
                    }
                }
                None => {
                    let key = format!("r:{result_index}");
                    result_index += 1;
                    ColView {
                        key,
                        name: c.name.clone(),
                        mark: String::new(),
                        tip: "From the summary's formula".to_string(),
                        numeric: is_numeric(c.format),
                        computed: true,
                        result: true,
                    }
                }
            }
        })
        .collect();
    let rows = computed
        .rows
        .iter()
        .map(|r| RowView {
            id: r.id.clone(),
            cells: r.values.iter().zip(&computed.columns).map(|(v, c)| cell_view(engine, v, c.format)).collect(),
        })
        .collect();
    let source = table
        .and_then(|t| t.source.as_deref())
        .map(|s| formula::to_shown(book, Some(&computed.id), s))
        .unwrap_or_default();
    let mut view = TableView {
        id: computed.id.clone(),
        name: computed.name.clone(),
        summary: computed.summary,
        source,
        error: computed.error.as_ref().map(|e| format!("{} {}", e.code.code(), e.message)),
        columns,
        rows,
        version: 0,
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{view:?}").hash(&mut h);
    view.version = h.finish();
    view
}

/// Recompute pane `pane`'s tables and hand them to the view.
pub(crate) fn recompute(store: AppStore, pane: PaneId) {
    let Some(book) = book_for(store, pane) else { return };
    let home = book.home;
    let engine = Engine::new(&book);
    let snap = book.snaps.get(&home).cloned().unwrap_or_default();
    let views = match engine.compute_node(&home) {
        Ok(tables) => tables.iter().map(|t| table_view(&engine, &book, &snap, t)).collect(),
        Err(e) => {
            tracing::warn!("a sheet could not be computed: {e}");
            Vec::new()
        }
    };
    let read = book.reads.clone();
    let changed = with_session(pane, |s| std::mem::replace(&mut s.reads, read.clone()) != read).unwrap_or(false);
    if changed {
        reads::sync(store);
    }
    let ui = store.pane(pane).sheet_ui;
    ui.tables.set(views);
    refresh_formula_bar(store, pane);
    // A peer's update may have brought or taken a table's atom, and the
    // editor tells of its own changes only.
    in_text::text_changed(pane);
}

// ── Editing ──────────────────────────────────────────────────────────────

fn say(store: AppStore, pane: PaneId, sentence: impl Into<String>) {
    store.pane(pane).sheet_ui.message.set(Some(sentence.into()));
}

/// One local edit of pane `pane`'s sheet: refused (with the sentence) when
/// this device may only read the node, sent through `applyEdit` otherwise.
fn local_edit<R>(store: AppStore, pane: PaneId, f: impl FnOnce(&mut doc::SheetsTxn<'_, '_>) -> doc::Result<R>) -> Option<R> {
    let held = with_session(pane, |s| (s.store_id, s.node_id))?;
    if !store.node_access(held.0, held.1).allows_write() {
        say(store, pane, pimble_core::StoreAccess::READ_ONLY_REFUSAL);
        return None;
    }
    let (result, update) = with_session(pane, |s| {
        let edited = s.sheets.edit(f);
        (edited.result, edited.update)
    })?;
    if let Some(bytes) = update {
        send_update(store, pane, held.0, held.1, bytes);
    }
    match result {
        Ok(r) => {
            store.pane(pane).sheet_ui.message.set(None);
            recompute(store, pane);
            Some(r)
        }
        Err(e) => {
            say(store, pane, e.to_string());
            recompute(store, pane);
            None
        }
    }
}

/// Undo (or redo) this pane's last sheet edit.
pub(crate) fn undo(store: AppStore, pane: PaneId, redo: bool) -> bool {
    let Some(held) = with_session(pane, |s| (s.store_id, s.node_id)) else { return false };
    let update = with_session(pane, |s| s.sheets.undo(&mut s.undo, redo)).flatten();
    if let Some(bytes) = update {
        send_update(store, pane, held.0, held.1, bytes);
        recompute(store, pane);
    }
    true
}

fn snapshot(pane: PaneId) -> Option<SheetsSnap> {
    with_session(pane, |s| s.sheets.read())
}

fn next_table_name(snap: &SheetsSnap, base: &str) -> String {
    let taken = |n: &str| snap.tables.iter().any(|t| t.name.eq_ignore_ascii_case(n));
    (1..).map(|i| format!("{base}{i}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
}

pub(crate) fn add_table(store: AppStore, pane: PaneId) {
    let Some(snap) = snapshot(pane) else { return };
    let name = next_table_name(&snap, "Table");
    local_edit(store, pane, |t| new_table(t, &name));
}

/// A new plain table named `name`, with a text and a number column.
fn new_table(t: &mut doc::SheetsTxn<'_, '_>, name: &str) -> doc::Result<String> {
    let table = t.create_table(name, Placement::Last)?;
    t.add_column(&table, "name", ColumnType::Text, Placement::Last)?;
    t.add_column(&table, "amount", ColumnType::Number, Placement::Last)?;
    Ok(table)
}

/// A summary of the first plain table: grouped by its first link column (or
/// its first column), totalling its first number column.
pub(crate) fn add_summary(store: AppStore, pane: PaneId) {
    let Some(snap) = snapshot(pane) else { return };
    let Some(src) = snap.tables.iter().find(|t| !t.is_summary()) else {
        say(store, pane, "A summary is made from a table; add a table first.");
        return;
    };
    let by = src
        .columns
        .iter()
        .find(|c| matches!(c.ty, ColumnType::RowLink { .. } | ColumnType::NodeLink { .. }))
        .or_else(|| src.columns.first());
    let Some(by) = by else {
        say(store, pane, "That table has no columns to group by.");
        return;
    };
    let val = src
        .columns
        .iter()
        .find(|c| c.id != by.id && matches!(c.ty, ColumnType::Number | ColumnType::Money | ColumnType::Hours));
    let shown = match val {
        Some(v) => format!("=GROUPBY({}[{}], {}[{}], SUM)", src.name, bracket(&by.name), src.name, bracket(&v.name)),
        None => format!("=GROUPBY({}[{}], {}[{}], COUNTA)", src.name, bracket(&by.name), src.name, bracket(&by.name)),
    };
    let Some(book) = book_for(store, pane) else { return };
    let stored = match formula::to_stored(&book, None, &shown) {
        Ok(s) => s,
        Err(e) => {
            say(store, pane, e.message);
            return;
        }
    };
    let name = next_table_name(&snap, "Summary");
    local_edit(store, pane, |t| {
        let table = t.create_table(&name, Placement::Last)?;
        t.set_source(&table, Some(&stored))
    });
}

fn bracket(name: &str) -> String {
    if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        name.to_string()
    } else {
        format!("[{name}]")
    }
}

pub(crate) fn rename_table(store: AppStore, pane: PaneId, table: &str, text: &str) {
    let name = doc::table_name_from(text);
    let current = snapshot(pane).and_then(|s| s.table(table).map(|t| t.name.clone()));
    if current.as_deref() == Some(name.as_str()) {
        return;
    }
    local_edit(store, pane, |t| t.rename_table(table, &name));
}

/// A table's "×": in a sheet node the table goes; in text its atom leaves
/// the text and the table is listed under it ([`in_text::remove_embeds`]).
pub(crate) fn delete_table(store: AppStore, pane: PaneId, table: &str) {
    if holds_text(pane) {
        in_text::remove_embeds(pane, table);
    } else {
        discard_table(store, pane, table);
    }
}

/// Delete a table and everything in it.
pub(crate) fn discard_table(store: AppStore, pane: PaneId, table: &str) {
    local_edit(store, pane, |t| t.delete_table(table));
}

pub(crate) fn set_source(store: AppStore, pane: PaneId, table: &str, shown: &str) {
    let Some(book) = book_for(store, pane) else { return };
    match formula::to_stored(&book, Some(table), shown) {
        Ok(stored) => {
            local_edit(store, pane, |t| t.set_source(table, Some(&stored)));
        }
        Err(e) => say(store, pane, e.message),
    }
}

pub(crate) fn delete_row(store: AppStore, pane: PaneId, table: &str, row: &str) {
    local_edit(store, pane, |t| t.delete_row(table, row));
}

/// The labels a link column's cells can name, with what each stores.
fn link_choices(store: AppStore, pane: PaneId, ty: &ColumnType) -> Vec<(String, Stored)> {
    match ty {
        ColumnType::RowLink { table } => {
            let tables = untracked(|| store.pane(pane).sheet_ui.tables.get());
            let Some(view) = tables.iter().find(|t| &t.id == table) else { return Vec::new() };
            view.rows
                .iter()
                .filter_map(|r| r.cells.first().map(|c| (c.text.clone(), Stored::Row(r.id.clone()))))
                .filter(|(label, _)| !label.is_empty())
                .collect()
        }
        ColumnType::NodeLink { under } => {
            let all: Vec<(StoreId, NodeId)> = untracked(|| store.node_data.with(|m| m.keys().copied().collect()));
            let mut out: Vec<(String, Stored)> = all
                .into_iter()
                .filter_map(|(s, n)| {
                    let sig = store.get_node_signal(s, n)?;
                    let (stored_title, parent) = untracked(|| sig.with(|node| (node.metadata.title.clone(), node.parent_id)));
                    let fits = match under {
                        Some(u) => u.store == s && parent == Some(u.node),
                        None => true,
                    };
                    if !fits {
                        return None;
                    }
                    // As the tree shows it: an untitled note by its first words.
                    let title = if stored_title.is_empty() { store.display_label(s, n) } else { stored_title };
                    (!title.is_empty()).then(|| (title, Stored::Node(node_ref(s, n))))
                })
                .collect();
            out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            out
        }
        _ => Vec::new(),
    }
}

fn today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

/// The column a cell key names, in this pane's document.
fn column_of(pane: PaneId, key: &CellKey) -> Option<(pimble_sheet::TableSnap, pimble_sheet::ColumnSnap)> {
    let snap = snapshot(pane)?;
    let table = snap.table(&key.table)?.clone();
    let col = table.column(&key.col)?.clone();
    Some((table, col))
}

/// A press on a cell: a typed cell starts typing; a computed one shows its
/// formula in the formula bar.
pub(crate) fn cell_pressed(store: AppStore, pane: PaneId, key: CellKey) {
    let ui = store.pane(pane).sheet_ui;
    if untracked(|| ui.editing.get()).as_ref() == Some(&key) {
        return;
    }
    commit(store, pane, Move::Stay);
    let typed = column_of(pane, &key).is_some_and(|(t, c)| !t.is_summary() && !matches!(c.ty, ColumnType::Formula { .. }));
    if typed {
        ui.selected.set(None);
        begin_typing(store, pane, key);
    } else {
        ui.selected.set(Some(key));
        refresh_formula_bar(store, pane);
    }
}

fn begin_typing(store: AppStore, pane: PaneId, key: CellKey) {
    tracing::debug!("sheet typing in {key:?}");
    let ui = store.pane(pane).sheet_ui;
    let Some((table, col)) = column_of(pane, &key) else { return };
    let text = key
        .row
        .as_ref()
        .and_then(|r| table.row(r))
        .and_then(|r| r.cells.get(&col.id).cloned())
        .map(|stored| {
            let choices = link_choices(store, pane, &col.ty);
            input::edit_text(&stored, &col.ty, &|s: &Stored| {
                choices.iter().find(|(_, v)| v == s).map(|(l, _)| l.clone()).unwrap_or_default()
            })
        })
        .unwrap_or_default();
    ui.edit_text.set(text);
    ui.choices.set(Vec::new());
    ui.choice.set(None);
    ui.editing.set(Some(key));
    if matches!(col.ty, ColumnType::RowLink { .. } | ColumnType::NodeLink { .. }) {
        filter_choices(store, pane, true);
    }
    // The nodes a link-to-a-node column chooses from may not be here yet.
    if let ColumnType::NodeLink { under: Some(u) } = &col.ty {
        store.send(BackendCommand::GetChildren { store_id: u.store, node_id: u.node });
    }
    TYPING.with(|t| t.set(Some(pane)));
    rinch::core::set_keyboard_interceptor(move |data| {
        let key = data.key.clone();
        let handled = matches!(key.as_str(), "Enter" | "Tab" | "Escape" | "ArrowUp" | "ArrowDown");
        // The interceptor hears releases too: act on the press only, and
        // keep the release from the runtime as well.
        if data.kind != rinch::core::KeyEventKind::Down {
            return handled;
        }
        if handled {
            run_on_main_thread(move || typing_key(store, pane, &key));
        }
        handled
    });
    ui::focus_edit_input(pane);
}

thread_local! {
    /// The pane whose cell is being typed into, if any.
    static TYPING: Cell<Option<PaneId>> = const { Cell::new(None) };
}

fn end_typing() {
    if TYPING.with(|t| t.take()).is_some() {
        rinch::core::clear_keyboard_interceptor();
    }
}

/// A key pressed in one of pane `pane`'s tables in text, which the editor
/// offers its node view before anything else (the keyboard interceptor a
/// sheet node's typing uses never hears it): what typing in a cell or in a
/// one-line field does with it, a turn later. `true` when it was used.
pub(crate) fn key_in_table(store: AppStore, pane: PaneId, key: &str) -> bool {
    if TYPING.with(Cell::get) == Some(pane) {
        if key == "Escape" && untracked(|| store.pane(pane).sheet_ui.choice.get()).is_none() {
            // The edit is dropped and, the cell's input gone, the keyboard
            // goes back to the text after the table: rinch does that with
            // an Escape the view leaves.
            cancel(store, pane);
            return false;
        }
        if !matches!(key, "Enter" | "Tab" | "Escape" | "ArrowUp" | "ArrowDown") {
            return false;
        }
        let key = key.to_string();
        let _ = set_timeout(0, move || typing_key(store, pane, &key));
        return true;
    }
    ui::field_key(store, pane, key)
}

/// Where typing goes after a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Move {
    Stay,
    Down,
    Up,
    Right,
}

fn typing_key(store: AppStore, pane: PaneId, key: &str) {
    tracing::debug!("sheet typing key {key}");
    let ui = store.pane(pane).sheet_ui;
    let choices = untracked(|| ui.choices.get());
    let choice = untracked(|| ui.choice.get());
    match key {
        "Escape" if !choices.is_empty() && choice.is_some() => ui.choice.set(None),
        "Escape" => cancel(store, pane),
        "ArrowDown" | "ArrowUp" if !choices.is_empty() => {
            let n = choices.len();
            let next = match (choice, key) {
                (None, "ArrowDown") => 0,
                (None, _) => n - 1,
                (Some(i), "ArrowDown") => (i + 1).min(n - 1),
                (Some(i), _) => i.saturating_sub(1),
            };
            ui.choice.set(Some(next));
        }
        "ArrowDown" => commit(store, pane, Move::Down),
        "ArrowUp" => commit(store, pane, Move::Up),
        "Enter" | "Tab" => {
            if let Some(i) = choice {
                if let Some(label) = choices.get(i) {
                    ui.edit_text.set(label.clone());
                }
            }
            commit(store, pane, if key == "Tab" { Move::Right } else { Move::Down });
        }
        _ => {}
    }
}

/// The text in the cell changed: a link cell's choices follow it.
pub(crate) fn typed(store: AppStore, pane: PaneId, text: String) {
    let ui = store.pane(pane).sheet_ui;
    ui.edit_text.set(text);
    filter_choices(store, pane, false);
}

fn filter_choices(store: AppStore, pane: PaneId, all: bool) {
    let ui = store.pane(pane).sheet_ui;
    let Some(key) = untracked(|| ui.editing.get()) else { return };
    let Some((_, col)) = column_of(pane, &key) else { return };
    if !matches!(col.ty, ColumnType::RowLink { .. } | ColumnType::NodeLink { .. }) {
        return;
    }
    let q = if all { String::new() } else { untracked(|| ui.edit_text.get()).trim().to_lowercase() };
    let all: Vec<String> = link_choices(store, pane, &col.ty).into_iter().map(|(l, _)| l).collect();
    let exact = all.iter().any(|l| l.to_lowercase() == q);
    let mut labels: Vec<String> = all.into_iter().filter(|l| l.to_lowercase().contains(&q)).collect();
    // A name that is not there yet can be made under the column's parent
    // (contract section 3).
    let offer = match (&col.ty, q.is_empty() || exact) {
        (ColumnType::NodeLink { under: Some(u) }, false) => {
            let typed = untracked(|| ui.edit_text.get()).trim().to_string();
            let parent = store.display_label(u.store, u.node);
            let label = format!("Create \"{typed}\" under {parent}");
            labels.push(label.clone());
            Some(CreateOffer { label, title: typed, under: *u })
        }
        _ => None,
    };
    OFFER.with(|o| *o.borrow_mut() = offer);
    ui.choice.set(if q.is_empty() || labels.is_empty() { None } else { Some(0) });
    ui.choices.set(labels);
}

/// The "Create ... under ..." choice of the link cell being typed into.
struct CreateOffer {
    label: String,
    title: String,
    under: NodeRef,
}

/// A node being made for a link cell: where its link goes when it arrives.
struct CreateJob {
    pane: PaneId,
    node: (StoreId, NodeId),
    key: CellKey,
}

thread_local! {
    static OFFER: RefCell<Option<CreateOffer>> = const { RefCell::new(None) };
    static CREATES: RefCell<HashMap<u64, CreateJob>> = RefCell::new(HashMap::new());
    /// Request ids of node creations for link cells.
    static NEXT_CREATE: Cell<u64> = const { Cell::new(3 << 48) };
}

/// The children of a node arrived: a link-to-a-node cell being typed into
/// that chooses from them shows them.
pub(crate) fn children_loaded(store: AppStore, store_id: StoreId, parent_id: NodeId) {
    let Some(pane) = TYPING.with(Cell::get) else { return };
    let Some(key) = untracked(|| store.pane(pane).sheet_ui.editing.get()) else { return };
    if let Some((_, col)) = column_of(pane, &key) {
        if matches!(col.ty, ColumnType::NodeLink { under: Some(u) } if u.store == store_id && u.node == parent_id) {
            filter_choices(store, pane, false);
        }
    }
}

/// A node made for a link cell: the cell links to it.
pub(crate) fn node_created(store: AppStore, request: u64, store_id: StoreId, node_id: NodeId) {
    let Some(job) = CREATES.with(|c| c.borrow_mut().remove(&request)) else { return };
    if with_session(job.pane, |s| (s.store_id, s.node_id)) != Some(job.node) {
        return;
    }
    let value = Stored::Node(node_ref(store_id, node_id));
    match &job.key.row {
        Some(row) => {
            local_edit(store, job.pane, |t| t.set_cell(&job.key.table, row, &job.key.col, Some(&value)));
        }
        None => {
            local_edit(store, job.pane, |t| t.add_row(&job.key.table, Placement::Last, &[(job.key.col.clone(), value.clone())]));
        }
    }
}

/// A press on one of a link cell's choices.
pub(crate) fn choose(store: AppStore, pane: PaneId, label: String) {
    store.pane(pane).sheet_ui.edit_text.set(label);
    commit(store, pane, Move::Stay);
}

pub(crate) fn cancel(store: AppStore, pane: PaneId) {
    end_typing();
    let ui = store.pane(pane).sheet_ui;
    ui.editing.set(None);
    ui.choices.set(Vec::new());
}

/// Write what was typed into the cell being edited, then move.
pub(crate) fn commit(store: AppStore, pane: PaneId, mv: Move) {
    let ui = store.pane(pane).sheet_ui;
    let Some(key) = untracked(|| ui.editing.get()) else { return };
    end_typing();
    ui.editing.set(None);
    ui.choices.set(Vec::new());
    let text = untracked(|| ui.edit_text.get());
    let Some((table, col)) = column_of(pane, &key) else { return };
    // "Create ... under ...": the node is made, and the cell links to it
    // when it arrives.
    if let Some(offer) = OFFER.with(|o| o.borrow_mut().take()).filter(|o| o.label == text) {
        let Some(node) = with_session(pane, |s| (s.store_id, s.node_id)) else { return };
        let request = NEXT_CREATE.with(|n| n.replace(n.get() + 1));
        CREATES.with(|c| c.borrow_mut().insert(request, CreateJob { pane, node, key: key.clone() }));
        store.send(BackendCommand::CreateNode {
            store_id: offer.under.store,
            parent_id: Some(offer.under.node),
            title: offer.title,
            init: Some(NodeInit {
                node_type: node_types::DOCUMENT.to_string(),
                content: Vec::new(),
                custom: vec![(pimble_core::custom_keys::EXPLICIT_TITLE.to_string(), serde_json::json!(true))],
                request: Some(request),
            }),
        });
        return;
    }
    let trimmed = text.trim();
    let mut row_id = key.row.clone();
    if trimmed.starts_with('=') && trimmed.len() > 1 {
        // Excel Tables: a formula typed into an empty column becomes the
        // whole column's formula.
        let others_empty = table.rows.iter().all(|r| Some(&r.id) == key.row.as_ref() || !r.cells.contains_key(&col.id));
        if !others_empty {
            say(store, pane, format!("{} holds typed values. To make it a formula column, change its type.", col.name));
            return;
        }
        let Some(book) = book_for(store, pane) else { return };
        match formula::to_stored(&book, Some(&table.id), trimmed) {
            Ok(stored) => {
                let format = col.ty.format();
                local_edit(store, pane, |t| {
                    if let Some(r) = &key.row {
                        t.set_cell(&table.id, r, &col.id, None)?;
                    }
                    t.set_column_type(&table.id, &col.id, ColumnType::Formula { formula: stored, format })?;
                    if table.rows.is_empty() {
                        t.add_row(&table.id, Placement::Last, &[])?;
                    }
                    Ok(())
                });
            }
            Err(e) => say(store, pane, e.message),
        }
        return;
    }
    let choices = link_choices(store, pane, &col.ty);
    let value = input::parse_input(&col.ty, &text, today(), &|label: &str| {
        choices.iter().find(|(l, _)| l.eq_ignore_ascii_case(label.trim())).map(|(_, v)| v.clone())
    });
    let current = key.row.as_ref().and_then(|r| table.row(r)).and_then(|r| r.cells.get(&col.id).cloned());
    if value == current {
        // Nothing changed: no edit.
    } else if let Some(row) = &key.row {
        local_edit(store, pane, |t| t.set_cell(&table.id, row, &col.id, value.as_ref()));
    } else if let Some(v) = &value {
        row_id = local_edit(store, pane, |t| t.add_row(&table.id, Placement::Last, &[(col.id.clone(), v.clone())]));
    }
    move_typing(store, pane, &key, row_id, mv);
}

fn move_typing(store: AppStore, pane: PaneId, key: &CellKey, row: Option<String>, mv: Move) {
    tracing::debug!("sheet move {mv:?} from {key:?} (row now {row:?})");
    if mv == Move::Stay {
        return;
    }
    let Some(snap) = snapshot(pane) else { return };
    let Some(table) = snap.table(&key.table) else { return };
    let typed: Vec<&str> = table
        .columns
        .iter()
        .filter(|c| !matches!(c.ty, ColumnType::Formula { .. }))
        .map(|c| c.id.as_str())
        .collect();
    let rows: Vec<Option<String>> = table.rows.iter().map(|r| Some(r.id.clone())).chain(std::iter::once(None)).collect();
    let at = rows.iter().position(|r| *r == row).unwrap_or(rows.len() - 1);
    let next = match mv {
        Move::Right => {
            let i = typed.iter().position(|c| *c == key.col).map(|i| i + 1).unwrap_or(0);
            match typed.get(i) {
                Some(c) => CellKey { table: key.table.clone(), row: row.clone(), col: c.to_string() },
                // Past the last column: the next row's first.
                None => CellKey {
                    table: key.table.clone(),
                    row: rows.get(at + 1).cloned().flatten(),
                    col: typed.first().map(|c| c.to_string()).unwrap_or_default(),
                },
            }
        }
        Move::Down if row.is_none() => {
            // Enter on the new-row line that made a row: the line again,
            // first column, for the next entry.
            CellKey { table: key.table.clone(), row: None, col: typed.first().map(|c| c.to_string()).unwrap_or_default() }
        }
        Move::Down => CellKey { table: key.table.clone(), row: rows.get(at + 1).cloned().flatten(), col: key.col.clone() },
        Move::Up => CellKey { table: key.table.clone(), row: rows.get(at.saturating_sub(1)).cloned().flatten(), col: key.col.clone() },
        Move::Stay => return,
    };
    let next = if key.row.is_none() && mv == Move::Down && row.is_some() {
        CellKey { table: key.table.clone(), row: None, col: typed.first().map(|c| c.to_string()).unwrap_or_default() }
    } else {
        next
    };
    if !next.col.is_empty() {
        begin_typing(store, pane, next);
    }
}

// ── The formula bar ──────────────────────────────────────────────────────

fn refresh_formula_bar(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).sheet_ui;
    let Some(key) = untracked(|| ui.selected.get()) else { return };
    let Some(book) = book_for(store, pane) else { return };
    let note = untracked(|| ui.tables.get())
        .iter()
        .find(|t| t.id == key.table)
        .and_then(|t| {
            let ci = t.columns.iter().position(|c| c.key == key.col)?;
            let row = t.rows.iter().find(|r| Some(&r.id) == key.row.as_ref())?;
            row.cells.get(ci)?.error.clone()
        });
    ui.formula_note.set(note);
    if let Some((_, col)) = column_of(pane, &key) {
        if let ColumnType::Formula { formula, .. } = &col.ty {
            ui.formula_text.set(formula::to_shown(&book, Some(&key.table), formula));
        }
    }
}

/// The formula bar's text was committed: the column's formula changes.
pub(crate) fn set_formula(store: AppStore, pane: PaneId, shown: String) {
    let ui = store.pane(pane).sheet_ui;
    let Some(key) = untracked(|| ui.selected.get()) else { return };
    let Some((table, col)) = column_of(pane, &key) else { return };
    let ColumnType::Formula { format, .. } = col.ty else { return };
    let Some(book) = book_for(store, pane) else { return };
    match formula::to_stored(&book, Some(&table.id), &shown) {
        Ok(stored) => {
            local_edit(store, pane, |t| t.set_column_type(&table.id, &col.id, ColumnType::Formula { formula: stored, format }));
        }
        Err(e) => ui.formula_note.set(Some(e.message)),
    }
}

// ── Column settings ──────────────────────────────────────────────────────

fn kind_of(ty: &ColumnType) -> &'static str {
    ty.kind()
}

pub(crate) fn open_column(store: AppStore, pane: PaneId, table: &str, col: Option<&str>) {
    let ui = store.pane(pane).sheet_ui;
    commit(store, pane, Move::Stay);
    let Some(snap) = snapshot(pane) else { return };
    let Some(t) = snap.table(table) else { return };
    if let Some(index) = col.and_then(|c| c.strip_prefix("r:")).and_then(|i| i.parse::<usize>().ok()) {
        let name = untracked(|| ui.tables.get())
            .iter()
            .find(|v| v.id == table)
            .and_then(|v| v.columns.iter().find(|c| c.key == format!("r:{index}")).map(|c| c.name.clone()))
            .unwrap_or_default();
        ui.column_form.set(Some(ColumnForm {
            table: table.to_string(),
            col: None,
            result: Some(index),
            name,
            kind: "formula".to_string(),
            target: String::new(),
            under: String::new(),
            formula: String::new(),
            format: "auto".to_string(),
            error: None,
        }));
        return;
    }
    let book = book_for(store, pane);
    let column = col.and_then(|c| t.column(c));
    let first_table = snap.tables.iter().find(|x| !x.is_summary()).map(|x| x.id.clone()).unwrap_or_default();
    let form = match column {
        Some(c) => ColumnForm {
            table: table.to_string(),
            col: Some(c.id.clone()),
            result: None,
            name: c.name.clone(),
            kind: kind_of(&c.ty).to_string(),
            target: match &c.ty {
                ColumnType::RowLink { table } => table.clone(),
                _ => first_table,
            },
            under: match (&c.ty, &book) {
                (ColumnType::NodeLink { under: Some(u) }, Some(b)) => b.display_name(u),
                _ => String::new(),
            },
            formula: match (&c.ty, &book) {
                (ColumnType::Formula { formula, .. }, Some(b)) => formula::to_shown(b, Some(table), formula),
                _ => "=".to_string(),
            },
            format: c.ty.format().as_str().to_string(),
            error: None,
        },
        None => ColumnForm {
            table: table.to_string(),
            col: None,
            result: None,
            name: String::new(),
            kind: if t.is_summary() { "formula".to_string() } else { "text".to_string() },
            target: first_table,
            under: String::new(),
            formula: "=".to_string(),
            format: "auto".to_string(),
            error: None,
        },
    };
    ui.column_form.set(Some(form));
}

pub(crate) fn close_column(store: AppStore, pane: PaneId) {
    store.pane(pane).sheet_ui.column_form.set(None);
}

fn form_error(store: AppStore, pane: PaneId, sentence: impl Into<String>) {
    let s = sentence.into();
    store.pane(pane).sheet_ui.column_form.update(|f| {
        if let Some(f) = f {
            f.error = Some(s);
        }
    });
}

/// "Done" in the column settings.
pub(crate) fn apply_column(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).sheet_ui;
    let Some(form) = untracked(|| ui.column_form.get()) else { return };
    let name = doc::column_name_from(&form.name);
    if name.is_empty() {
        return form_error(store, pane, "Give the column a name.");
    }
    if let Some(index) = form.result {
        if local_edit(store, pane, |t| t.set_header(&form.table, index, Some(&name))).is_some() {
            ui.column_form.set(None);
        }
        return;
    }
    let Some(snap) = snapshot(pane) else { return };
    let Some(table) = snap.table(&form.table) else { return };
    if table.columns.iter().any(|c| Some(&c.id) != form.col.as_ref() && c.name.eq_ignore_ascii_case(&name)) {
        return form_error(store, pane, format!("{} already has a column called \"{name}\".", table.name));
    }
    let ty = match form.kind.as_str() {
        "number" => ColumnType::Number,
        "money" => ColumnType::Money,
        "date" => ColumnType::Date,
        "hours" => ColumnType::Hours,
        "row_link" => {
            if form.target.is_empty() {
                return form_error(store, pane, "Pick a table to link to.");
            }
            ColumnType::RowLink { table: form.target.clone() }
        }
        "node_link" => {
            let under = form.under.trim().trim_matches('\'');
            if under.is_empty() {
                ColumnType::NodeLink { under: None }
            } else {
                let Some(book) = book_for(store, pane) else { return };
                match book.find_nodes(under).as_slice() {
                    [one] => ColumnType::NodeLink { under: Some(*one) },
                    [] => return form_error(store, pane, format!("No node here is called {under}.")),
                    many => {
                        let paths: Vec<String> = many.iter().map(|n| book.display_name(n)).collect();
                        return form_error(store, pane, format!("Several nodes are called {under}: {}. Write the one you mean as its path.", paths.join(", ")));
                    }
                }
            }
        }
        "formula" => {
            let shown = form.formula.trim();
            if shown.trim_start_matches('=').trim().is_empty() {
                return form_error(store, pane, "Write the formula, starting with =.");
            }
            let Some(book) = book_for(store, pane) else { return };
            match formula::to_stored(&book, Some(&form.table), shown) {
                Ok(stored) => ColumnType::Formula { formula: stored, format: Format::parse(&form.format) },
                Err(e) => return form_error(store, pane, e.message),
            }
        }
        _ => ColumnType::Text,
    };
    let done = match &form.col {
        None => local_edit(store, pane, |t| t.add_column(&form.table, &name, ty, Placement::Last).map(|_| ())),
        Some(col) => {
            let old = table.column(col).cloned();
            // Carry each row's value over as the text that would have been
            // typed (formula columns: their computed values).
            let carried: Vec<(String, Option<Stored>)> = match &old {
                Some(old) if old.ty != ty && !matches!(ty, ColumnType::Formula { .. }) => {
                    carry_over(store, pane, table, old, &ty)
                }
                _ => Vec::new(),
            };
            local_edit(store, pane, |t| {
                if old.as_ref().is_some_and(|o| o.name != name) {
                    t.rename_column(&form.table, col, &name)?;
                }
                if old.as_ref().is_some_and(|o| o.ty != ty) {
                    t.set_column_type(&form.table, col, ty.clone())?;
                    if matches!(ty, ColumnType::Formula { .. }) {
                        for r in &table.rows {
                            t.set_cell(&form.table, &r.id, col, None)?;
                        }
                    }
                    for (row, value) in &carried {
                        t.set_cell(&form.table, row, col, value.as_ref())?;
                    }
                }
                Ok(())
            })
        }
    };
    if done.is_some() {
        ui.column_form.set(None);
    }
}

fn carry_over(store: AppStore, pane: PaneId, table: &pimble_sheet::TableSnap, old: &pimble_sheet::ColumnSnap, ty: &ColumnType) -> Vec<(String, Option<Stored>)> {
    let shown = untracked(|| store.pane(pane).sheet_ui.tables.get());
    let view = shown.iter().find(|t| t.id == table.id);
    let ci = view.and_then(|v| v.columns.iter().position(|c| c.key == old.id));
    let choices = link_choices(store, pane, ty);
    table
        .rows
        .iter()
        .map(|r| {
            let text = match (&old.ty, r.cells.get(&old.id)) {
                (ColumnType::Formula { .. }, _) | (ColumnType::RowLink { .. } | ColumnType::NodeLink { .. }, Some(_)) => view
                    .and_then(|v| v.rows.iter().find(|x| x.id == r.id))
                    .and_then(|x| ci.and_then(|i| x.cells.get(i)))
                    .filter(|c| c.error.is_none())
                    .map(|c| c.text.replace(['$', ','], ""))
                    .unwrap_or_default(),
                (_, Some(stored)) => input::edit_text(stored, &old.ty, &|_| String::new()),
                (_, None) => String::new(),
            };
            let value = input::parse_input(ty, &text, today(), &|label: &str| {
                choices.iter().find(|(l, _)| l.eq_ignore_ascii_case(label.trim())).map(|(_, v)| v.clone())
            });
            (r.id.clone(), value)
        })
        .collect()
}

pub(crate) fn delete_column(store: AppStore, pane: PaneId) {
    let ui = store.pane(pane).sheet_ui;
    let Some(form) = untracked(|| ui.column_form.get()) else { return };
    if let Some(col) = &form.col {
        if local_edit(store, pane, |t| t.delete_column(&form.table, col)).is_some() {
            ui.column_form.set(None);
        }
    }
}

/// The tables a link column can point at, for the column settings.
pub(crate) fn plain_tables(pane: PaneId) -> Vec<(String, String)> {
    snapshot(pane)
        .map(|s| s.tables.iter().filter(|t| !t.is_summary()).map(|t| (t.id.clone(), t.name.clone())).collect())
        .unwrap_or_default()
}
