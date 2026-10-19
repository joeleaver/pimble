//! The sheet document in yrs (docs/SHEETS_CONTRACT.md section 2).
//!
//! ```text
//! data root (shared with boards and plugins), one key per table:
//! "sheet:<table id>": Map {
//!   v: 1, name: String, order: String, source: String?, headers: Map<"<index">, String>,
//!   columns: Map<column id, Map {
//!     name, order, type: "text" | "number" | "money" | "date" | "hours" | "row_link" | "node_link" | "formula",
//!     target: String? (row_link: the table id),
//!     under: Any? (node_link: [store uuid, node uuid], absent = anywhere),
//!     formula: String? (stored form), format: String?,
//!   }>,
//!   rows: Map<row id, Map { order: String, cells: Map<column id, cell> }>,
//! }
//! ```
//!
//! A cell is plain data: a number, a string, `{date}`, `{row}`, `{node}` or
//! `{typed}` (text that did not parse). Names, formulas, orders and cells are
//! each one last-writer-wins value, never a yrs `Text`: two people editing
//! one formula end with one person's whole formula (contract section 2).
//! Ids never move inside arrays, so concurrent edits to different rows,
//! columns and cells merge.
//!
//! A [`Sheets`] is a handle on where a node's tables live in its yrs document;
//! it holds no state of its own. Every write runs in one transaction and
//! answers the update it produced, an ordinary yrs update of the node's
//! document, which the caller sends through `applyEdit` like every other edit
//! (CLAUDE.md: "one way to write node content").
//!
//! The layout is flat on purpose: each table is a key of the node's `data`
//! root, prefixed `sheet:`, and there is no `sheets` container map. The data
//! root is a yrs root type and always exists, so two devices that each make a
//! node's first table while apart add two different keys and the merge keeps
//! both. A lazily created container map would be made by both, and the merge
//! would keep one of them whole and drop the other's tables. Keys without the
//! prefix (boards, plugins) belong to others and are never read or written.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use pimble_core::{NodeId, StoreId};
use pimble_crdt::{order, NodeDoc};
use yrs::types::{Event, PathSegment};
use yrs::undo::UndoManager;
use yrs::{Any, DeepObservable, Doc, Map, MapPrelim, MapRef, OffsetKind, Origin, Out, ReadTxn, StateVector, Transact, TransactionMut};

use crate::model::*;

/// What a table's key in a node's `data` root starts with.
pub const TABLE_KEY_PREFIX: &str = "sheet:";

/// The key of the table `id` in the `data` root.
pub fn table_key(id: &str) -> String {
    format!("{TABLE_KEY_PREFIX}{id}")
}

/// The tables of a data root: `(table id, table map)`.
fn tables_of<T: ReadTxn>(txn: &T, root: &MapRef) -> Vec<(String, MapRef)> {
    root.iter(txn)
        .filter_map(|(k, v)| match (k.strip_prefix(TABLE_KEY_PREFIX), v) {
            (Some(id), Out::YMap(m)) => Some((id.to_string(), m)),
            _ => None,
        })
        .collect()
}
/// The schema version written to `v`.
pub const VERSION: i64 = 1;

pub(crate) mod keys {
    pub const V: &str = "v";
    pub const NAME: &str = "name";
    pub const ORDER: &str = "order";
    pub const SOURCE: &str = "source";
    pub const HEADERS: &str = "headers";
    pub const COLUMNS: &str = "columns";
    pub const ROWS: &str = "rows";
    pub const CELLS: &str = "cells";
    pub const TYPE: &str = "type";
    pub const TARGET: &str = "target";
    pub const UNDER: &str = "under";
    pub const FORMULA: &str = "formula";
    pub const FORMAT: &str = "format";
    // Cell shapes.
    pub const DATE: &str = "date";
    pub const ROW: &str = "row";
    pub const NODE: &str = "node";
    pub const TYPED: &str = "typed";
}
use keys::*;

/// Why a sheet write was refused. A refused single write changed nothing.
/// The messages are whole sentences, for the person.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SheetError {
    #[error("This note has no table {0}.")]
    NoTable(String),
    #[error("The table has no column {0}.")]
    NoColumn(String),
    #[error("The table has no row {0}.")]
    NoRow(String),
    #[error("A table needs a name.")]
    EmptyTableName,
    #[error("A column needs a name.")]
    EmptyColumnName,
    #[error("\"{0}\" is not a table name; use letters, digits and underscores, and don't start with a digit.")]
    InvalidTableName(String),
    #[error("This note already has a table called \"{0}\".")]
    DuplicateTable(String),
    #[error("{table} already has a column called \"{name}\".")]
    DuplicateColumn { table: String, name: String },
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, SheetError>;

/// What [`Sheets::edit`] answers: the closure's result, and the update the
/// transaction produced whether or not the closure succeeded (yrs has no
/// rollback, so a closure that fails after writing has still written, and the
/// caller must still send those bytes). `None` when nothing changed.
#[derive(Debug)]
pub struct Edited<R> {
    pub result: Result<R>,
    pub update: Option<Vec<u8>>,
}

impl<R> Edited<R> {
    /// The result with the update, for a caller that treats a failure as
    /// nothing written (true of every single write [`SheetsTxn`] offers: each
    /// checks before it writes).
    pub fn into_result(self) -> Result<(R, Option<Vec<u8>>)> {
        let Edited { result, update } = self;
        result.map(|r| (r, update))
    }
}

/// Where a new table, column or row goes among its siblings.
#[derive(Debug, Clone, PartialEq)]
pub enum Placement {
    First,
    Last,
    /// After the sibling with this id.
    After(String),
    /// Before the sibling with this id.
    Before(String),
}

/// A random id for a table, column or row: a v4 uuid without hyphens.
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Typed text as a table name: spaces become underscores, anything that is
/// not a letter, digit or underscore is dropped (Excel's table naming). A
/// leading digit is kept (the result may still be refused by
/// [`SheetsTxn::create_table`]; prefix it if a name is required).
pub fn table_name_from(text: &str) -> String {
    text.trim()
        .chars()
        .filter_map(|c| match c {
            c if c.is_whitespace() => Some('_'),
            c if c.is_alphanumeric() || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

/// Typed text as a column name: `[ ] @ #` dropped, trimmed, inner whitespace
/// collapsed to single spaces.
pub fn column_name_from(text: &str) -> String {
    let cleaned: String = text.chars().filter(|c| !matches!(c, '[' | ']' | '@' | '#')).collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `name` is a well-formed table name.
pub fn is_table_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => chars.all(|c| c.is_alphanumeric() || c == '_'),
        _ => false,
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Where a node's tables live in a yrs document, and the origin its writes
/// carry.
#[derive(Clone)]
pub struct Sheets {
    doc: Doc,
    root: MapRef,
    origin: Option<Origin>,
}

impl Sheets {
    /// The tables of a node: `data.sheets` (contract section 2). Sheet nodes
    /// and text nodes keep them in the same place.
    pub fn of_node(node: &NodeDoc) -> Self {
        Self { doc: node.ydoc().clone(), root: node.data_root(), origin: None }
    }

    /// Tables in a document of their own (root map `data`), for tests,
    /// benches and tools that read a document's bytes.
    pub fn standalone() -> Self {
        let doc = Doc::with_options(yrs::Options { offset_kind: OffsetKind::Utf16, ..Default::default() });
        let root = doc.get_or_insert_map(pimble_crdt::ROOT_DATA);
        Self { doc, root, origin: None }
    }

    /// Every write through this handle carries `origin` (yrs
    /// `transact_mut_with`), which is what a per-person `UndoManager` tracks.
    pub fn with_origin(mut self, origin: impl Into<Origin>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Whether the node has at least one table (made here or by any peer).
    pub fn exists(&self) -> bool {
        let txn = self.doc.transact();
        !tables_of(&txn, &self.root).is_empty()
    }

    /// The node's `data` root, where the tables live beside other keys.
    pub fn root(&self) -> &MapRef {
        &self.root
    }

    /// The map of table `id`, when it exists.
    pub fn table_map<T: ReadTxn>(&self, txn: &T, id: &str) -> Option<MapRef> {
        match self.root.get(txn, &table_key(id)) {
            Some(Out::YMap(m)) => Some(m),
            _ => None,
        }
    }

    /// Run `f` in one write transaction and answer its result with the
    /// update the transaction produced.
    pub fn edit<R>(&self, f: impl FnOnce(&mut SheetsTxn<'_, '_>) -> Result<R>) -> Edited<R> {
        let mut txn = match &self.origin {
            Some(origin) => self.doc.transact_mut_with(origin.clone()),
            None => self.doc.transact_mut(),
        };
        let mut st = SheetsTxn { txn: &mut txn, location: self };
        let result = f(&mut st);
        txn.commit();
        let changed = *txn.before_state() != *txn.after_state() || !txn.delete_set().is_empty();
        let update = changed.then(|| txn.encode_update_v1());
        Edited { result, update }
    }

    /// Every table, read in one transaction (empty when there are none).
    /// Tables, columns and rows come in order (then id); an entry that is not
    /// shaped like one is skipped, and two tables or columns with one name
    /// (a merge can make them) are both read.
    pub fn read(&self) -> SheetsSnap {
        let txn = self.doc.transact();
        read_sheets(&txn, &self.root)
    }

    /// One table, when it exists.
    pub fn table(&self, id: &str) -> Option<TableSnap> {
        let txn = self.doc.transact();
        let t = self.table_map(&txn, id)?;
        Some(read_table(&txn, id, &t))
    }

    /// Watch the tables for changes, local and remote, so a reader can
    /// re-read only the tables named. Changes to other keys of the `data`
    /// root (boards, plugins) are not reported.
    pub fn watch(&self) -> SheetsWatch {
        let changes = Arc::new(Mutex::new(Changes::default()));
        let sink = changes.clone();
        let subscription = self.root.observe_deep(move |txn, events| {
            let mut c = sink.lock().unwrap();
            for event in events.iter() {
                let path = event.path();
                match path.front() {
                    // The data root itself: tables added or removed.
                    None => {
                        if let Event::Map(e) = event {
                            c.tables.extend(e.keys(txn).keys().filter_map(|k| k.strip_prefix(TABLE_KEY_PREFIX)).map(str::to_string));
                        } else {
                            c.all = true;
                        }
                    }
                    Some(PathSegment::Key(k)) => {
                        if let Some(id) = k.strip_prefix(TABLE_KEY_PREFIX) {
                            c.tables.insert(id.to_string());
                        }
                    }
                    Some(_) => c.all = true,
                }
            }
        });
        SheetsWatch { changes, _subscription: subscription }
    }

    /// A per-person undo manager over the tables: it tracks only transactions
    /// carrying this handle's origin (set one with [`Sheets::with_origin`]),
    /// so undo takes back my edits and never a collaborator's. The scope is
    /// the whole `data` root, so use an origin of its own for sheet edits.
    pub fn undo_manager(&self, mut options: yrs::undo::Options<()>) -> UndoManager<()> {
        if let Some(origin) = &self.origin {
            options.tracked_origins.insert(origin.clone());
        }
        let mut manager = UndoManager::with_options(options);
        manager.expand_scope(&self.doc, &self.root);
        manager
    }

    /// Undo (or with `redo`, redo) one step of `manager`, answering the
    /// update to send: what the undo changed, as a diff from the state before.
    pub fn undo(&self, manager: &mut UndoManager<()>, redo: bool) -> Option<Vec<u8>> {
        let before = self.doc.transact().state_vector();
        let done = if redo { manager.redo_blocking() } else { manager.undo_blocking() };
        if !done {
            return None;
        }
        // A diff, not a state-vector comparison: an undo that only deletes
        // does not move the state vector.
        Some(self.doc.transact().encode_diff_v1(&before))
    }

    /// Everything this document has that a peer at `sv` lacks.
    pub fn diff_since(&self, sv: &StateVector) -> Vec<u8> {
        self.doc.transact().encode_diff_v1(sv)
    }
}

/// What changed since [`SheetsWatch::take`] was last called.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Changes {
    /// Something only a whole re-read can account for.
    pub all: bool,
    /// Tables added, changed or removed, by id.
    pub tables: HashSet<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.all && self.tables.is_empty()
    }
}

/// A subscription to the tables' changes (see [`Sheets::watch`]); dropping it
/// ends the subscription.
pub struct SheetsWatch {
    changes: Arc<Mutex<Changes>>,
    _subscription: yrs::Subscription,
}

impl SheetsWatch {
    /// What changed since the last call.
    pub fn take(&self) -> Changes {
        std::mem::take(&mut *self.changes.lock().unwrap())
    }
}

// ── Writing ──────────────────────────────────────────────────────────────

/// A write transaction on a node's tables. Every write checks what it needs
/// before it writes, so a refused write changes nothing.
pub struct SheetsTxn<'a, 'doc> {
    txn: &'a mut TransactionMut<'doc>,
    location: &'a Sheets,
}

impl<'a, 'doc> SheetsTxn<'a, 'doc> {
    // ── Tables ───────────────────────────────────────────────────────────

    /// A new table with no columns and no rows. The name is trimmed, then must
    /// be a table name ([`is_table_name`]) and unique in the node,
    /// case-insensitively.
    pub fn create_table(&mut self, name: &str, placement: Placement) -> Result<String> {
        let name = self.check_table_name(name, None)?;
        let keys = self.table_order_keys(None, &placement, 1)?;
        let id = new_id();
        let table: MapRef = self.location.root.insert(self.txn, table_key(&id), MapPrelim::default());
        table.insert(self.txn, V, Any::BigInt(VERSION));
        table.insert(self.txn, NAME, name);
        table.insert(self.txn, ORDER, keys[0].clone());
        table.insert(self.txn, COLUMNS, MapPrelim::default());
        table.insert(self.txn, ROWS, MapPrelim::default());
        Ok(id)
    }

    /// Rename a table (same rules as [`SheetsTxn::create_table`]; renaming a
    /// table to its own name in another case is allowed).
    pub fn rename_table(&mut self, table: &str, name: &str) -> Result<()> {
        let t = self.table_map(table)?;
        let name = self.check_table_name(name, Some(table))?;
        t.insert(self.txn, NAME, name);
        Ok(())
    }

    /// Set a table's order key directly (see [`SheetsTxn::move_table`] for
    /// placing it among the others).
    pub fn set_table_order(&mut self, table: &str, order_key: &str) -> Result<()> {
        let t = self.table_map(table)?;
        t.insert(self.txn, ORDER, order_key.to_string());
        Ok(())
    }

    pub fn move_table(&mut self, table: &str, placement: Placement) -> Result<()> {
        let t = self.table_map(table)?;
        let keys = self.table_order_keys(Some(table), &placement, 1)?;
        t.insert(self.txn, ORDER, keys[0].clone());
        Ok(())
    }

    /// Delete a table with its columns and rows.
    pub fn delete_table(&mut self, table: &str) -> Result<()> {
        self.table_map(table)?;
        self.location.root.remove(self.txn, &table_key(table));
        Ok(())
    }

    /// A summary's formula (stored form), or `None` for a plain table.
    pub fn set_source(&mut self, table: &str, source: Option<&str>) -> Result<()> {
        let t = self.table_map(table)?;
        match source {
            Some(s) => {
                t.insert(self.txn, SOURCE, s.to_string());
            }
            None => {
                t.remove(self.txn, SOURCE);
            }
        }
        Ok(())
    }

    /// Rename (or with `None`, un-rename) a summary's result column by position.
    pub fn set_header(&mut self, table: &str, index: usize, header: Option<&str>) -> Result<()> {
        let t = self.table_map(table)?;
        let headers = self.sub_map(&t, HEADERS);
        match header {
            Some(h) => {
                headers.insert(self.txn, index.to_string(), h.to_string());
            }
            None => {
                headers.remove(self.txn, &index.to_string());
            }
        }
        Ok(())
    }

    // ── Columns ──────────────────────────────────────────────────────────

    /// A new column. The name goes through [`column_name_from`], then must be
    /// unique in the table, case-insensitively.
    pub fn add_column(&mut self, table: &str, name: &str, ty: ColumnType, placement: Placement) -> Result<String> {
        let t = self.table_map(table)?;
        let name = self.check_column_name(&t, name, None)?;
        let keys = self.column_order_keys(&t, None, &placement, 1)?;
        let columns = self.sub_map(&t, COLUMNS);
        let id = new_id();
        let c: MapRef = columns.insert(self.txn, id.clone(), MapPrelim::default());
        c.insert(self.txn, NAME, name);
        c.insert(self.txn, ORDER, keys[0].clone());
        write_type(self.txn, &c, &ty);
        Ok(id)
    }

    pub fn rename_column(&mut self, table: &str, column: &str, name: &str) -> Result<()> {
        let t = self.table_map(table)?;
        let c = self.column_map(&t, column)?;
        let name = self.check_column_name(&t, name, Some(column))?;
        c.insert(self.txn, NAME, name);
        Ok(())
    }

    /// Change a column's type: writes `type` and the fields it uses and
    /// removes the stale ones. Cells are left as they are; carrying values
    /// over is the caller's (it re-parses what was typed).
    pub fn set_column_type(&mut self, table: &str, column: &str, ty: ColumnType) -> Result<()> {
        let t = self.table_map(table)?;
        let c = self.column_map(&t, column)?;
        write_type(self.txn, &c, &ty);
        Ok(())
    }

    pub fn move_column(&mut self, table: &str, column: &str, placement: Placement) -> Result<()> {
        let t = self.table_map(table)?;
        let c = self.column_map(&t, column)?;
        let keys = self.column_order_keys(&t, Some(column), &placement, 1)?;
        c.insert(self.txn, ORDER, keys[0].clone());
        Ok(())
    }

    /// Delete a column and its cell in every row.
    pub fn delete_column(&mut self, table: &str, column: &str) -> Result<()> {
        let t = self.table_map(table)?;
        self.column_map(&t, column)?;
        if let Some(columns) = child_map(self.txn, &t, COLUMNS) {
            columns.remove(self.txn, column);
        }
        if let Some(rows) = child_map(self.txn, &t, ROWS) {
            let row_maps: Vec<MapRef> = rows
                .iter(self.txn)
                .filter_map(|(_, v)| match v {
                    Out::YMap(m) => Some(m),
                    _ => None,
                })
                .collect();
            for row in row_maps {
                if let Some(cells) = child_map(self.txn, &row, CELLS) {
                    cells.remove(self.txn, column);
                }
            }
        }
        Ok(())
    }

    // ── Rows ─────────────────────────────────────────────────────────────

    /// A new row holding `cells` (by column id; a column left out is blank).
    pub fn add_row(&mut self, table: &str, placement: Placement, cells: &[(String, Stored)]) -> Result<String> {
        let mut ids = self.add_rows(table, placement, &[cells.to_vec()])?;
        Ok(ids.remove(0))
    }

    /// Many rows in one placement, keeping their order. Every column named
    /// is checked before anything is written.
    pub fn add_rows(&mut self, table: &str, placement: Placement, rows: &[Vec<(String, Stored)>]) -> Result<Vec<String>> {
        let t = self.table_map(table)?;
        let columns: HashSet<String> = child_map(self.txn, &t, COLUMNS)
            .map(|m| m.keys(self.txn).map(|k| k.to_string()).collect())
            .unwrap_or_default();
        for cells in rows {
            if let Some((c, _)) = cells.iter().find(|(c, _)| !columns.contains(c)) {
                return Err(SheetError::NoColumn(c.clone()));
            }
        }
        let keys = self.row_order_keys(&t, None, &placement, rows.len())?;
        let rows_map = self.sub_map(&t, ROWS);
        let mut ids = Vec::with_capacity(rows.len());
        for (cells, key) in rows.iter().zip(keys) {
            let id = new_id();
            let row: MapRef = rows_map.insert(self.txn, id.clone(), MapPrelim::default());
            row.insert(self.txn, ORDER, key);
            let cell_map: MapRef = row.insert(self.txn, CELLS, MapPrelim::default());
            for (column, value) in cells {
                cell_map.insert(self.txn, column.clone(), stored_to_any(value));
            }
            ids.push(id);
        }
        Ok(ids)
    }

    /// Set one cell, or with `None` remove its entry (the cell is blank).
    pub fn set_cell(&mut self, table: &str, row: &str, column: &str, value: Option<&Stored>) -> Result<()> {
        let t = self.table_map(table)?;
        self.column_map(&t, column)?;
        let r = self.row_map(&t, row)?;
        let cells = self.sub_map(&r, CELLS);
        match value {
            Some(v) => {
                cells.insert(self.txn, column.to_string(), stored_to_any(v));
            }
            None => {
                cells.remove(self.txn, column);
            }
        }
        Ok(())
    }

    pub fn move_row(&mut self, table: &str, row: &str, placement: Placement) -> Result<()> {
        let t = self.table_map(table)?;
        let r = self.row_map(&t, row)?;
        let keys = self.row_order_keys(&t, Some(row), &placement, 1)?;
        r.insert(self.txn, ORDER, keys[0].clone());
        Ok(())
    }

    pub fn delete_row(&mut self, table: &str, row: &str) -> Result<()> {
        let t = self.table_map(table)?;
        self.row_map(&t, row)?;
        if let Some(rows) = child_map(self.txn, &t, ROWS) {
            rows.remove(self.txn, row);
        }
        Ok(())
    }

    // ── Lookups ──────────────────────────────────────────────────────────

    fn table_map(&self, id: &str) -> Result<MapRef> {
        self.location.table_map(self.txn, id).ok_or_else(|| SheetError::NoTable(id.to_string()))
    }

    fn column_map(&self, table: &MapRef, id: &str) -> Result<MapRef> {
        match child_map(self.txn, table, COLUMNS).and_then(|c| c.get(self.txn, id)) {
            Some(Out::YMap(m)) => Ok(m),
            _ => Err(SheetError::NoColumn(id.to_string())),
        }
    }

    fn row_map(&self, table: &MapRef, id: &str) -> Result<MapRef> {
        match child_map(self.txn, table, ROWS).and_then(|c| c.get(self.txn, id)) {
            Some(Out::YMap(m)) => Ok(m),
            _ => Err(SheetError::NoRow(id.to_string())),
        }
    }

    /// The child map `key` of `parent`, made when a malformed or merged
    /// document lacks it.
    fn sub_map(&mut self, parent: &MapRef, key: &str) -> MapRef {
        match child_map(self.txn, parent, key) {
            Some(m) => m,
            None => parent.insert(self.txn, key.to_string(), MapPrelim::default()),
        }
    }

    // ── Names ────────────────────────────────────────────────────────────

    fn check_table_name(&self, name: &str, except: Option<&str>) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(SheetError::EmptyTableName);
        }
        if !is_table_name(name) {
            return Err(SheetError::InvalidTableName(name.to_string()));
        }
        for (id, t) in tables_of(self.txn, &self.location.root) {
            if Some(id.as_str()) == except {
                continue;
            }
            if get_string(self.txn, &t, NAME).is_some_and(|n| same_name(&n, name)) {
                return Err(SheetError::DuplicateTable(name.to_string()));
            }
        }
        Ok(name.to_string())
    }

    fn check_column_name(&self, table: &MapRef, name: &str, except: Option<&str>) -> Result<String> {
        let name = column_name_from(name);
        if name.is_empty() {
            return Err(SheetError::EmptyColumnName);
        }
        if let Some(columns) = child_map(self.txn, table, COLUMNS) {
            for (id, v) in columns.iter(self.txn) {
                if Some(id) == except {
                    continue;
                }
                if let Out::YMap(c) = v {
                    if get_string(self.txn, &c, NAME).is_some_and(|n| same_name(&n, &name)) {
                        let table_name = get_string(self.txn, table, NAME).unwrap_or_default();
                        return Err(SheetError::DuplicateColumn { table: table_name, name });
                    }
                }
            }
        }
        Ok(name)
    }

    // ── Order keys ───────────────────────────────────────────────────────

    fn table_order_keys(&self, except: Option<&str>, placement: &Placement, n: usize) -> Result<Vec<String>> {
        let mut siblings: Vec<(String, String)> = tables_of(self.txn, &self.location.root)
            .into_iter()
            .map(|(id, t)| (get_string(self.txn, &t, ORDER).unwrap_or_default(), id))
            .collect();
        siblings.sort();
        place(siblings, except, placement, n, |id| SheetError::NoTable(id.to_string()))
    }

    fn column_order_keys(&self, table: &MapRef, except: Option<&str>, placement: &Placement, n: usize) -> Result<Vec<String>> {
        let siblings = child_map(self.txn, table, COLUMNS).map(|m| siblings_of(self.txn, &m)).unwrap_or_default();
        place(siblings, except, placement, n, |id| SheetError::NoColumn(id.to_string()))
    }

    fn row_order_keys(&self, table: &MapRef, except: Option<&str>, placement: &Placement, n: usize) -> Result<Vec<String>> {
        let siblings = child_map(self.txn, table, ROWS).map(|m| siblings_of(self.txn, &m)).unwrap_or_default();
        place(siblings, except, placement, n, |id| SheetError::NoRow(id.to_string()))
    }
}

/// `(order, id)` of every map entry of `map`, sorted.
fn siblings_of<T: ReadTxn>(txn: &T, map: &MapRef) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = map
        .iter(txn)
        .filter_map(|(id, v)| match v {
            Out::YMap(m) => Some((get_string(txn, &m, ORDER).unwrap_or_default(), id.to_string())),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// `n` order keys for `placement` among `siblings` (sorted), leaving out the
/// sibling `except` (the one being moved).
fn place(
    siblings: Vec<(String, String)>,
    except: Option<&str>,
    placement: &Placement,
    n: usize,
    missing: impl Fn(&str) -> SheetError,
) -> Result<Vec<String>> {
    let others: Vec<&(String, String)> = siblings.iter().filter(|(_, id)| Some(id.as_str()) != except).collect();
    let (a, b): (Option<&str>, Option<&str>) = match placement {
        Placement::First => (None, others.first().map(|(o, _)| o.as_str())),
        Placement::Last => (others.last().map(|(o, _)| o.as_str()), None),
        Placement::After(id) => {
            let i = others.iter().position(|(_, s)| s == id).ok_or_else(|| missing(id))?;
            (Some(others[i].0.as_str()), others.get(i + 1).map(|(o, _)| o.as_str()))
        }
        Placement::Before(id) => {
            let i = others.iter().position(|(_, s)| s == id).ok_or_else(|| missing(id))?;
            (i.checked_sub(1).map(|j| others[j].0.as_str()), Some(others[i].0.as_str()))
        }
    };
    Ok(order::n_between(a, b, n))
}

/// Write a column's `type` and the fields that type uses; remove the others.
fn write_type(txn: &mut TransactionMut, column: &MapRef, ty: &ColumnType) {
    column.insert(txn, TYPE, ty.kind().to_string());
    for key in [TARGET, UNDER, FORMULA, FORMAT] {
        column.remove(txn, key);
    }
    match ty {
        ColumnType::RowLink { table } => {
            column.insert(txn, TARGET, table.clone());
        }
        ColumnType::NodeLink { under } => {
            if let Some(n) = under {
                column.insert(txn, UNDER, node_to_any(n));
            }
        }
        ColumnType::Formula { formula, format } => {
            column.insert(txn, FORMULA, formula.clone());
            column.insert(txn, FORMAT, format.as_str().to_string());
        }
        _ => {}
    }
}

// ── Encoding ─────────────────────────────────────────────────────────────

fn node_to_any(n: &NodeRef) -> Any {
    Any::Array(Arc::from(vec![Any::String(n.store.0.to_string().into()), Any::String(n.node.0.to_string().into())]))
}

fn any_to_node(a: &Any) -> Option<NodeRef> {
    let Any::Array(items) = a else { return None };
    match &items[..] {
        [Any::String(s), Any::String(n)] => Some(NodeRef {
            store: StoreId(uuid::Uuid::parse_str(s).ok()?),
            node: NodeId(uuid::Uuid::parse_str(n).ok()?),
        }),
        _ => None,
    }
}

fn tagged(key: &str, value: Any) -> Any {
    let mut m = HashMap::new();
    m.insert(key.to_string(), value);
    Any::Map(Arc::new(m))
}

fn stored_to_any(v: &Stored) -> Any {
    match v {
        Stored::Number(n) => Any::Number(*n),
        Stored::Text(s) => Any::String(s.as_str().into()),
        Stored::Date(d) => tagged(DATE, Any::String(d.as_str().into())),
        Stored::Row(id) => tagged(ROW, Any::String(id.as_str().into())),
        Stored::Node(n) => tagged(NODE, node_to_any(n)),
        Stored::Unparsed(t) => tagged(TYPED, Any::String(t.as_str().into())),
    }
}

/// A cell, when it is shaped like one.
fn any_to_stored(a: &Any) -> Option<Stored> {
    match a {
        Any::Number(n) => Some(Stored::Number(*n)),
        Any::BigInt(n) => Some(Stored::Number(*n as f64)),
        Any::String(s) => Some(Stored::Text(s.to_string())),
        Any::Map(m) => {
            if let Some(Any::String(s)) = m.get(DATE) {
                Some(Stored::Date(s.to_string()))
            } else if let Some(Any::String(s)) = m.get(ROW) {
                Some(Stored::Row(s.to_string()))
            } else if let Some(n) = m.get(NODE) {
                any_to_node(n).map(Stored::Node)
            } else if let Some(Any::String(s)) = m.get(TYPED) {
                Some(Stored::Unparsed(s.to_string()))
            } else {
                None
            }
        }
        _ => None,
    }
}

// ── Reading ──────────────────────────────────────────────────────────────

fn child_map<T: ReadTxn>(txn: &T, parent: &MapRef, key: &str) -> Option<MapRef> {
    match parent.get(txn, key) {
        Some(Out::YMap(m)) => Some(m),
        _ => None,
    }
}

fn get_string<T: ReadTxn>(txn: &T, map: &MapRef, key: &str) -> Option<String> {
    match map.get(txn, key) {
        Some(Out::Any(Any::String(s))) => Some(s.to_string()),
        _ => None,
    }
}

fn read_sheets<T: ReadTxn>(txn: &T, root: &MapRef) -> SheetsSnap {
    let mut tables: Vec<TableSnap> = tables_of(txn, root).into_iter().map(|(id, t)| read_table(txn, &id, &t)).collect();
    tables.sort_by(|a, b| (&a.order, &a.id).cmp(&(&b.order, &b.id)));
    SheetsSnap { tables }
}

fn read_table<T: ReadTxn>(txn: &T, id: &str, t: &MapRef) -> TableSnap {
    let mut columns: Vec<ColumnSnap> = child_map(txn, t, COLUMNS)
        .map(|m| {
            m.iter(txn)
                .filter_map(|(cid, v)| match v {
                    Out::YMap(c) => Some(ColumnSnap {
                        id: cid.to_string(),
                        name: get_string(txn, &c, NAME).unwrap_or_default(),
                        order: get_string(txn, &c, ORDER).unwrap_or_default(),
                        ty: read_type(txn, &c),
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    columns.sort_by(|a, b| (&a.order, &a.id).cmp(&(&b.order, &b.id)));
    let known: HashSet<&str> = columns.iter().map(|c| c.id.as_str()).collect();

    let mut rows: Vec<RowSnap> = child_map(txn, t, ROWS)
        .map(|m| {
            m.iter(txn)
                .filter_map(|(rid, v)| match v {
                    Out::YMap(r) => {
                        let mut cells = BTreeMap::new();
                        if let Some(cm) = child_map(txn, &r, CELLS) {
                            for (cid, cv) in cm.iter(txn) {
                                // A cell of a column that is gone (deleted while
                                // someone typed in it) is not read.
                                if !known.contains(cid) {
                                    continue;
                                }
                                if let Out::Any(a) = cv {
                                    if let Some(s) = any_to_stored(&a) {
                                        cells.insert(cid.to_string(), s);
                                    }
                                }
                            }
                        }
                        Some(RowSnap { id: rid.to_string(), order: get_string(txn, &r, ORDER).unwrap_or_default(), cells })
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| (&a.order, &a.id).cmp(&(&b.order, &b.id)));

    let headers = child_map(txn, t, HEADERS)
        .map(|m| {
            m.iter(txn)
                .filter_map(|(k, v)| match (k.parse::<usize>(), v) {
                    (Ok(i), Out::Any(Any::String(s))) => Some((i, s.to_string())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    TableSnap {
        id: id.to_string(),
        name: get_string(txn, t, NAME).unwrap_or_default(),
        order: get_string(txn, t, ORDER).unwrap_or_default(),
        source: get_string(txn, t, SOURCE),
        headers,
        columns,
        rows,
    }
}

/// A column's type; an unknown or missing `type` reads as text.
fn read_type<T: ReadTxn>(txn: &T, c: &MapRef) -> ColumnType {
    match get_string(txn, c, TYPE).as_deref() {
        Some("number") => ColumnType::Number,
        Some("money") => ColumnType::Money,
        Some("date") => ColumnType::Date,
        Some("hours") => ColumnType::Hours,
        Some("row_link") => ColumnType::RowLink { table: get_string(txn, c, TARGET).unwrap_or_default() },
        Some("node_link") => {
            let under = match c.get(txn, UNDER) {
                Some(Out::Any(a)) => any_to_node(&a),
                _ => None,
            };
            ColumnType::NodeLink { under }
        }
        Some("formula") => ColumnType::Formula {
            formula: get_string(txn, c, FORMULA).unwrap_or_default(),
            format: get_string(txn, c, FORMAT).map(|f| Format::parse(&f)).unwrap_or_default(),
        },
        _ => ColumnType::Text,
    }
}
