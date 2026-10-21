//! Computing tables (contract sections 4, 6 and 7).
//!
//! An [`Engine`] is one consistent computation over what a [`Book`] shows:
//! every formula cell and every summary is computed at most once and
//! memoized, so a column read by twenty rows' `SUMIFS` is built once. Make a
//! new engine when the documents change; recomputing only what an edit
//! touches is the app's dependency graph, built over this.
//!
//! A cycle is found, not feared: the engine keeps the stack of cells and
//! summaries being computed, and when one asks for a cell already on the
//! stack, every cell from that one up is on the cycle and ends as `#CIRC!`,
//! whatever the formulas did with the value (an `IFERROR` cannot hide it).
//!
//! Other notes are read through [`Book::sheets`]; a note that is deleted,
//! unreadable or gone is `#REF!` with the contract's sentence, and one that
//! has not arrived is the loading state. A table's formula cells are always
//! computed in the context of the note that holds them, so a summary in one
//! note reading a formula column in another works.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::book::{Book, Reach};
use crate::model::{
    ColumnSnap, ColumnType, Format, NodeRef, ResultKey, RowSnap, SheetsSnap, Stored, TableSnap,
};

use super::ast::*;
use super::coerce::{self, Labels};
use super::dates;
use super::format;
use super::functions;
use super::names::{self, NameError};
use super::parser::{parse, ParseError};
use super::value::*;

/// How many cells may be computing inside each other before the engine
/// stops (a chain this long is a mistake, and the stack is finite).
const MAX_EVAL_DEPTH: usize = 48;

/// How deep one evaluation may recurse through expressions, across cells,
/// before it answers `#CALC!` instead of exhausting the stack.
const MAX_EXPR_DEPTH: usize = 320;

/// Names compare without regard to case or runs of spaces.
pub(crate) fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Cell(NodeRef, String, String, String),
    Summary(NodeRef, String),
}

enum Enter {
    Ok,
    Cycle,
    TooDeep,
}

/// A summary's computed rows: its result columns, renamed by `headers`.
#[derive(Debug, Clone)]
pub(crate) struct Summary {
    pub cols: Vec<ColMeta>,
    /// Each result column's key ([`ResultKey`]), in the same order.
    pub keys: Vec<String>,
    pub rows: Vec<SRow>,
    pub index: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct SRow {
    pub id: String,
    pub values: Vec<Value>,
    /// The table row this summary row came from, when it came from one.
    pub base: Option<RowRef>,
}

/// Where a displayed column's values come from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ColSrc<'b> {
    Own(&'b ColumnSnap),
    /// A summary's result column, by position.
    Result(usize),
}

/// A column as formulas see it: a table's own column, or a summary's
/// result column.
#[derive(Debug, Clone)]
pub(crate) struct TabCol<'b> {
    pub meta: ColMeta,
    pub src: ColSrc<'b>,
    /// A result column's key; `None` for an own column.
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ColKey {
    Own(String),
    Result(usize),
}

/// A `LET` scope.
pub(crate) struct Env {
    name: String,
    val: Val,
    up: Option<Rc<Env>>,
}

/// Where an expression is evaluated: its note, its table and row when it
/// is a formula column's, and the `LET` names in scope.
#[derive(Clone)]
pub(crate) struct Ctx<'b> {
    pub node: NodeRef,
    pub table: Option<&'b TableSnap>,
    pub row: Option<RowRef>,
    pub env: Option<Rc<Env>>,
}

impl<'b> Ctx<'b> {
    pub fn with(&self, name: &str, val: Val) -> Ctx<'b> {
        Ctx {
            env: Some(Rc::new(Env {
                name: norm(name),
                val,
                up: self.env.clone(),
            })),
            ..self.clone()
        }
    }

    pub fn lookup(&self, name: &str) -> Option<Val> {
        let n = norm(name);
        let mut e = self.env.as_ref();
        while let Some(env) = e {
            if env.name == n {
                return Some(env.val.clone());
            }
            e = env.up.as_ref();
        }
        None
    }
}

/// A displayed column of a computed table.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedColumn {
    /// The column's id; `None` for a summary's result column, which has
    /// only a name.
    pub id: Option<String>,
    pub name: String,
    pub format: Format,
    /// The table a link column's values point into.
    pub link: Option<TableAddr>,
    /// Computed (a formula column, or a summary's result) rather than typed.
    pub computed: bool,
    /// A summary's result column's key ([`ResultKey`]); `None` for a column
    /// of the table's own.
    pub result_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedRow {
    pub id: String,
    /// One per displayed column.
    pub values: Vec<Value>,
}

/// A table as it shows: its columns and every cell's value.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedTable {
    pub id: String,
    pub name: String,
    pub summary: bool,
    pub columns: Vec<ComputedColumn>,
    pub rows: Vec<ComputedRow>,
    /// A summary whose formula fails: the reason, shown over the table.
    pub error: Option<FormulaError>,
}

impl ComputedTable {
    pub fn column_index(&self, name: &str) -> Option<usize> {
        let n = norm(name);
        self.columns.iter().position(|c| norm(&c.name) == n)
    }
}

type TableKey = (NodeRef, String);
type ColumnKey = (NodeRef, String, ColKey);
type SummarySlot = Rc<Result<Summary, FormulaError>>;

/// One computation over a [`Book`].
pub struct Engine<'b> {
    book: &'b dyn Book,
    cells: RefCell<HashMap<Key, Value>>,
    columns: RefCell<HashMap<ColumnKey, Rc<Vec<Value>>>>,
    rows: RefCell<HashMap<TableKey, Rc<Vec<RowRef>>>>,
    summaries: RefCell<HashMap<TableKey, SummarySlot>>,
    parsed: RefCell<HashMap<String, Rc<Result<Expr, ParseError>>>>,
    stack: RefCell<Vec<Key>>,
    cyclic: RefCell<HashSet<Key>>,
    depth: Cell<usize>,
}

impl<'b> Engine<'b> {
    pub fn new(book: &'b dyn Book) -> Self {
        Engine {
            book,
            cells: RefCell::default(),
            columns: RefCell::default(),
            rows: RefCell::default(),
            summaries: RefCell::default(),
            parsed: RefCell::default(),
            stack: RefCell::default(),
            cyclic: RefCell::default(),
            depth: Cell::new(0),
        }
    }

    pub fn book(&self) -> &'b dyn Book {
        self.book
    }

    /// Today, as a serial.
    pub fn today(&self) -> f64 {
        self.book.today()
    }

    // ── Public computing ────────────────────────────────────────────────

    /// Every table of a node, in order. Fails when the node's sheets cannot
    /// be read (the reason is the error).
    pub fn compute_node(&self, node: &NodeRef) -> Result<Vec<ComputedTable>, FormulaError> {
        let snap = self.snap(node)?;
        Ok(snap.tables.iter().map(|t| self.compute(node, t)).collect())
    }

    /// One table of a node.
    pub fn compute_table(
        &self,
        node: &NodeRef,
        table_id: &str,
    ) -> Result<ComputedTable, FormulaError> {
        let snap = self.snap(node)?;
        let t = snap
            .table(table_id)
            .ok_or_else(|| FormulaError::reference("That table was deleted"))?;
        Ok(self.compute(node, t))
    }

    fn compute(&self, node: &NodeRef, t: &'b TableSnap) -> ComputedTable {
        let own = |c: &ColumnSnap| ComputedColumn {
            id: Some(c.id.clone()),
            name: c.name.clone(),
            format: c.ty.format(),
            link: match &c.ty {
                ColumnType::RowLink { table } => Some(TableAddr {
                    node: *node,
                    table: table.clone(),
                }),
                _ => None,
            },
            computed: matches!(c.ty, ColumnType::Formula { .. }),
            result_key: None,
        };
        let mut out = ComputedTable {
            id: t.id.clone(),
            name: t.name.clone(),
            summary: t.is_summary(),
            columns: Vec::new(),
            rows: Vec::new(),
            error: None,
        };
        if !t.is_summary() {
            out.columns = t.columns.iter().map(own).collect();
            for r in &t.rows {
                let row = RowRef {
                    node: *node,
                    table: t.id.clone(),
                    row: r.id.clone(),
                };
                let values = t
                    .columns
                    .iter()
                    .map(|c| self.plain_cell(node, t, r, &row, c))
                    .collect();
                out.rows.push(ComputedRow {
                    id: r.id.clone(),
                    values,
                });
            }
            return out;
        }
        let summary = self.summary(node, t);
        match &*summary {
            Err(e) => {
                out.error = Some(e.clone());
                out.columns = t.columns.iter().map(own).collect();
            }
            Ok(s) => {
                out.columns = s
                    .cols
                    .iter()
                    .zip(&s.keys)
                    .map(|(m, k)| ComputedColumn {
                        id: None,
                        name: m.name.clone(),
                        format: m.format,
                        link: m.link.clone(),
                        computed: true,
                        result_key: Some(k.clone()),
                    })
                    .chain(t.columns.iter().map(own))
                    .collect();
                for sr in &s.rows {
                    let row = RowRef {
                        node: *node,
                        table: t.id.clone(),
                        row: sr.id.clone(),
                    };
                    let mut values = sr.values.clone();
                    for c in &t.columns {
                        values.push(self.summary_own_cell(node, t, &row, c));
                    }
                    out.rows.push(ComputedRow {
                        id: sr.id.clone(),
                        values,
                    });
                }
            }
        }
        out
    }

    /// Runs a formula (shown or stored form) in a table's context, one row's
    /// or none, without storing it. Names that do not resolve are refused
    /// the way committing the formula would refuse them.
    pub fn evaluate(
        &self,
        node: &NodeRef,
        table_id: Option<&str>,
        row_id: Option<&str>,
        formula: &str,
    ) -> Result<Evaluated, NameError> {
        let stored = names::to_stored_with(self, node, table_id, formula)?;
        let expr = parse(&stored).map_err(|e| NameError::parse(&e))?;
        let table = match table_id {
            Some(id) => match self.snap(node).ok().and_then(|s| s.table(id)) {
                Some(t) => Some(t),
                None => return Err(NameError::unknown("That table was deleted", 0..0)),
            },
            None => None,
        };
        let row = match (table, row_id) {
            (Some(t), Some(r)) => Some(RowRef {
                node: *node,
                table: t.id.clone(),
                row: r.to_string(),
            }),
            (None, Some(_)) => {
                return Err(NameError::unknown("A row needs its table", 0..0));
            }
            _ => None,
        };
        let ctx = Ctx {
            node: *node,
            table,
            row,
            env: None,
        };
        Ok(match self.eval(&expr, &ctx) {
            Val::One(v) => Evaluated::Value(v),
            Val::Func(f) => Evaluated::Value(Value::err(func_as_value(f))),
            Val::Arr(a) if a.height == 1 && a.width() == 1 => Evaluated::Value(a.get(0, 0).clone()),
            Val::Arr(a) => Evaluated::Table(ResultTable {
                columns: a.cols.iter().map(|c| c.meta.clone()).collect(),
                rows: (0..a.height)
                    .map(|r| (0..a.width()).map(|c| a.get(r, c).clone()).collect())
                    .collect(),
            }),
        })
    }

    /// A row as a link to it shows: its first column's value.
    pub fn row_label(&self, row: &RowRef) -> String {
        let Ok(snap) = self.snap(&row.node) else {
            return "#REF!".to_string();
        };
        let Some(t) = snap.table(&row.table) else {
            return "#REF!".to_string();
        };
        let Ok(cols) = self.table_cols(&row.node, t) else {
            return "#REF!".to_string();
        };
        let Some(first) = cols.first() else {
            return "(row)".to_string();
        };
        let v = self.cell_value(&row.node, t, &first.clone(), row);
        match v {
            Value::Blank => "(blank)".to_string(),
            v => self.format_value(&v, first.meta.format),
        }
    }

    /// A value as a cell in this format shows it: money `$1,234.50`, hours
    /// `2h 30m`, dates `Mon, Oct 19`, links by their label, errors by code.
    pub fn format_value(&self, v: &Value, f: Format) -> String {
        match v {
            Value::Blank => String::new(),
            Value::Number(x) => format::format_number(*x, f, self.today()),
            Value::Text(s) => s.clone(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            Value::Row(r) => self.row_label(r),
            Value::Node(n) => self.node_title(n),
            Value::Error(e) => e.code.code().to_string(),
        }
    }

    // ── Reading notes and tables ────────────────────────────────────────

    /// A note's title for sentences.
    pub(crate) fn note_name(&self, node: &NodeRef) -> String {
        match self.book.node(node) {
            Some(info) if !info.title.is_empty() => info.title,
            _ => {
                let d = self.book.display_name(node);
                if d.is_empty() {
                    "That note".to_string()
                } else {
                    d
                }
            }
        }
    }

    pub(crate) fn snap(&self, node: &NodeRef) -> Result<&'b SheetsSnap, FormulaError> {
        match self.book.sheets(node) {
            Reach::Ready(s) => Ok(s),
            Reach::Deleted => Err(FormulaError::reference(format!(
                "{} was deleted",
                self.note_name(node)
            ))),
            Reach::NoAccess => Err(FormulaError::reference(format!(
                "You can't read {}",
                self.note_name(node)
            ))),
            Reach::Loading => Err(FormulaError::new(
                ErrorCode::Loading,
                format!("Waiting for {} to arrive", self.note_name(node)),
            )),
            Reach::Missing => Err(FormulaError::reference("The note this points at is gone")),
        }
    }

    pub(crate) fn resolve_note(&self, name: &NoteName) -> Result<NodeRef, FormulaError> {
        match name {
            NoteName::Id(n) => Ok(*n),
            NoteName::Name(s) => {
                let found = self.book.find_nodes(s);
                match found.len() {
                    0 => Err(FormulaError::reference(format!(
                        "No note is called \"{s}\""
                    ))),
                    1 => Ok(found[0]),
                    k => {
                        let names: Vec<String> = found
                            .iter()
                            .map(|n| format!("'{}'", self.book.display_name(n)))
                            .collect();
                        Err(FormulaError::name(format!(
                            "{k} notes are called \"{s}\": {}. Name the one you mean by its path",
                            names.join(", ")
                        )))
                    }
                }
            }
        }
    }

    pub(crate) fn find_table(
        &self,
        snap: &'b SheetsSnap,
        name: &NameRef,
    ) -> Result<Option<&'b TableSnap>, FormulaError> {
        match name {
            NameRef::Id(id) => Ok(snap.table(id)),
            NameRef::Dead | NameRef::Pos(_) | NameRef::Key(_) => Err(FormulaError::reference(
                "This formula points at a table that was deleted",
            )),
            NameRef::Name(n) => {
                let n = norm(n);
                let hits: Vec<&TableSnap> =
                    snap.tables.iter().filter(|t| norm(&t.name) == n).collect();
                match hits.len() {
                    0 => Ok(None),
                    1 => Ok(Some(hits[0])),
                    k => Err(FormulaError::name(format!(
                        "{k} tables here are called {}; rename one",
                        hits[0].name
                    ))),
                }
            }
        }
    }

    pub(crate) fn parse_cached(&self, src: &str) -> Rc<Result<Expr, ParseError>> {
        if let Some(p) = self.parsed.borrow().get(src) {
            return p.clone();
        }
        let p = Rc::new(parse(src));
        self.parsed.borrow_mut().insert(src.to_string(), p.clone());
        p
    }

    fn enter(&self, key: &Key) -> Enter {
        let mut stack = self.stack.borrow_mut();
        if let Some(pos) = stack.iter().position(|k| k == key) {
            let mut cyc = self.cyclic.borrow_mut();
            for k in &stack[pos..] {
                cyc.insert(k.clone());
            }
            return Enter::Cycle;
        }
        if stack.len() >= MAX_EVAL_DEPTH {
            return Enter::TooDeep;
        }
        stack.push(key.clone());
        Enter::Ok
    }

    /// Pops `key`; answers whether it turned out to be on a cycle.
    fn leave(&self, key: &Key) -> bool {
        let mut stack = self.stack.borrow_mut();
        if stack.last() == Some(key) {
            stack.pop();
        }
        self.cyclic.borrow().contains(key)
    }

    /// The displayed columns of a table: a plain table's own; a summary's
    /// result columns, then its own.
    pub(crate) fn table_cols(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
    ) -> Result<Vec<TabCol<'b>>, FormulaError> {
        let own = t.columns.iter().map(|c| TabCol {
            meta: ColMeta {
                name: c.name.clone(),
                format: c.ty.format(),
                link: match &c.ty {
                    ColumnType::RowLink { table } => Some(TableAddr {
                        node: *node,
                        table: table.clone(),
                    }),
                    _ => None,
                },
            },
            src: ColSrc::Own(c),
            key: None,
        });
        if !t.is_summary() {
            return Ok(own.collect());
        }
        let s = self.summary(node, t);
        match &*s {
            Err(e) => Err(e.clone()),
            Ok(s) => Ok(s
                .cols
                .iter()
                .enumerate()
                .map(|(i, m)| TabCol {
                    meta: m.clone(),
                    src: ColSrc::Result(i),
                    key: s.keys.get(i).cloned(),
                })
                .chain(own)
                .collect()),
        }
    }

    /// The column a piece names, by id or name.
    pub(crate) fn find_col(&self, cols: &[TabCol<'b>], name: &NameRef) -> Option<usize> {
        match name {
            NameRef::Id(id) => cols
                .iter()
                .position(|c| matches!(c.src, ColSrc::Own(o) if o.id == *id)),
            NameRef::Name(n) => {
                let n = norm(n);
                cols.iter().position(|c| norm(&c.meta.name) == n)
            }
            NameRef::Pos(i) => cols
                .iter()
                .position(|c| matches!(c.src, ColSrc::Result(j) if j == *i)),
            NameRef::Key(k) => {
                let k = norm(k);
                cols.iter()
                    .position(|c| c.key.as_deref().is_some_and(|ck| norm(ck) == k))
            }
            NameRef::Dead => None,
        }
    }

    fn no_column(&self, t: &TableSnap, cols: &[TabCol], name: &NameRef) -> FormulaError {
        match name {
            NameRef::Name(n) => {
                let list: Vec<&str> = cols.iter().map(|c| c.meta.name.as_str()).collect();
                FormulaError::reference(format!(
                    "{} has no column \"{n}\"; its columns are {}",
                    t.name,
                    list.join(", ")
                ))
            }
            NameRef::Pos(i) => {
                let n = cols
                    .iter()
                    .filter(|c| matches!(c.src, ColSrc::Result(_)))
                    .count();
                FormulaError::reference(format!(
                    "This formula reads column {} of {}'s result, and its formula now gives {n}",
                    i + 1,
                    t.name
                ))
            }
            NameRef::Key(k) => FormulaError::reference(format!(
                "This formula reads the result column that came from \"{k}\", and {}'s formula no longer gives one",
                t.name
            )),
            _ => FormulaError::reference(format!(
                "This formula points at a column of {} that was deleted",
                t.name
            )),
        }
    }

    pub(crate) fn table_rows(&self, node: &NodeRef, t: &'b TableSnap) -> Rc<Vec<RowRef>> {
        let key = (*node, t.id.clone());
        if let Some(r) = self.rows.borrow().get(&key) {
            return r.clone();
        }
        let ids: Vec<String> = if t.is_summary() {
            match &*self.summary(node, t) {
                Ok(s) => s.rows.iter().map(|r| r.id.clone()).collect(),
                Err(_) => Vec::new(),
            }
        } else {
            t.rows.iter().map(|r| r.id.clone()).collect()
        };
        let rows = Rc::new(
            ids.into_iter()
                .map(|row| RowRef {
                    node: *node,
                    table: t.id.clone(),
                    row,
                })
                .collect::<Vec<_>>(),
        );
        self.rows.borrow_mut().insert(key, rows.clone());
        rows
    }

    /// Every value of one displayed column.
    pub(crate) fn column_values(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
        col: &TabCol<'b>,
    ) -> Rc<Vec<Value>> {
        let ck = match col.src {
            ColSrc::Own(c) => ColKey::Own(c.id.clone()),
            ColSrc::Result(i) => ColKey::Result(i),
        };
        let key = (*node, t.id.clone(), ck);
        if let Some(v) = self.columns.borrow().get(&key) {
            return v.clone();
        }
        let values: Vec<Value> = match col.src {
            ColSrc::Result(i) => match &*self.summary(node, t) {
                Ok(s) => s
                    .rows
                    .iter()
                    .map(|r| r.values.get(i).cloned().unwrap_or(Value::Blank))
                    .collect(),
                Err(e) => vec![Value::err(e.clone())],
            },
            ColSrc::Own(_) => self
                .table_rows(node, t)
                .iter()
                .map(|row| self.cell_value(node, t, col, row))
                .collect(),
        };
        let values = Rc::new(values);
        self.columns.borrow_mut().insert(key, values.clone());
        values
    }

    /// The whole table as a list of its columns.
    pub(crate) fn table_array(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
        cols: &[TabCol<'b>],
    ) -> Array {
        let rows = self.table_rows(node, t);
        Array::from_columns(
            cols.iter()
                .map(|c| Column {
                    meta: c.meta.clone(),
                    values: self.column_values(node, t, c),
                })
                .collect(),
            Some(rows),
        )
    }

    /// One cell of a displayed column in a row of that table.
    pub(crate) fn cell_value(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
        col: &TabCol<'b>,
        row: &RowRef,
    ) -> Value {
        if !t.is_summary() {
            let ColSrc::Own(c) = col.src else {
                return Value::Blank;
            };
            return match t.row(&row.row) {
                Some(r) => self.plain_cell(node, t, r, row, c),
                None => Value::err(FormulaError::reference("That row was deleted")),
            };
        }
        match col.src {
            ColSrc::Result(i) => match &*self.summary(node, t) {
                Ok(s) => match s.index.get(&row.row) {
                    Some(&k) => s.rows[k].values.get(i).cloned().unwrap_or(Value::Blank),
                    None => Value::err(FormulaError::reference(
                        "That row is no longer in the summary",
                    )),
                },
                Err(e) => Value::err(e.clone()),
            },
            ColSrc::Own(c) => self.summary_own_cell(node, t, row, c),
        }
    }

    fn plain_cell(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
        r: &'b RowSnap,
        row: &RowRef,
        c: &'b ColumnSnap,
    ) -> Value {
        match &c.ty {
            ColumnType::Formula { formula, .. } => self.formula_cell(node, t, row, c, formula),
            _ => self.typed_cell(node, r, c),
        }
    }

    fn summary_own_cell(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
        row: &RowRef,
        c: &'b ColumnSnap,
    ) -> Value {
        match &c.ty {
            ColumnType::Formula { formula, .. } => self.formula_cell(node, t, row, c, formula),
            _ => Value::Blank,
        }
    }

    /// What a person typed, as the engine sees it.
    fn typed_cell(&self, node: &NodeRef, r: &RowSnap, c: &ColumnSnap) -> Value {
        let Some(s) = r.cells.get(&c.id) else {
            return Value::Blank;
        };
        let not =
            |what: &str, s: &str| Value::err(FormulaError::value(format!("\"{s}\" isn't {what}")));
        match (&c.ty, s) {
            (ColumnType::RowLink { table }, s) => {
                let target = self.snap(node).ok().and_then(|sn| sn.table(table));
                let Some(target) = target else {
                    return Value::err(FormulaError::reference(
                        "The table this column links to was deleted",
                    ));
                };
                match s {
                    Stored::Row(id) => {
                        if target.row(id).is_some() {
                            Value::Row(RowRef {
                                node: *node,
                                table: target.id.clone(),
                                row: id.clone(),
                            })
                        } else {
                            Value::err(FormulaError::reference(format!(
                                "That row of {} was deleted",
                                target.name
                            )))
                        }
                    }
                    Stored::Unparsed(x) | Stored::Text(x) => Value::err(FormulaError::na(format!(
                        "{} has no row named \"{x}\"",
                        target.name
                    ))),
                    Stored::Number(x) => Value::err(FormulaError::na(format!(
                        "{} has no row named \"{}\"",
                        target.name,
                        coerce::general(*x)
                    ))),
                    _ => Value::err(FormulaError::na(format!(
                        "That isn't a row of {}",
                        target.name
                    ))),
                }
            }
            (ColumnType::NodeLink { under }, s) => match s {
                Stored::Node(n) => Value::Node(*n),
                Stored::Unparsed(x) | Stored::Text(x) => {
                    Value::err(FormulaError::na(match under {
                        Some(u) => format!("No node under {} is called \"{x}\"", self.note_name(u)),
                        None => format!("No node is called \"{x}\""),
                    }))
                }
                _ => Value::err(FormulaError::na("That isn't a node")),
            },
            (ColumnType::Formula { .. }, _) => Value::Blank,
            (ty, Stored::Number(x)) => {
                let _ = ty;
                Value::Number(*x)
            }
            (ColumnType::Date, Stored::Date(iso) | Stored::Text(iso) | Stored::Unparsed(iso)) => {
                match dates::parse_iso(iso) {
                    Some(d) => Value::Number(d),
                    None => not("a date", iso),
                }
            }
            (ColumnType::Text, Stored::Text(x) | Stored::Unparsed(x) | Stored::Date(x)) => {
                Value::Text(x.clone())
            }
            (ColumnType::Number | ColumnType::Money, Stored::Text(x)) => {
                match coerce::parse_number_text(x) {
                    Some(v) if !x.contains(':') => Value::Number(v),
                    _ => not("a number", x),
                }
            }
            (ColumnType::Number | ColumnType::Money, Stored::Unparsed(x)) => not("a number", x),
            (ColumnType::Hours, Stored::Text(x) | Stored::Unparsed(x)) => {
                match coerce::parse_number_text(x) {
                    Some(v) if matches!(s, Stored::Text(_)) && !x.contains(':') => Value::Number(v),
                    _ => not("a length of time", x),
                }
            }
            (ColumnType::Number | ColumnType::Money | ColumnType::Hours, Stored::Date(iso)) => {
                match dates::parse_iso(iso) {
                    Some(d) => Value::Number(d),
                    None => not("a number", iso),
                }
            }
            (_, Stored::Node(n)) => Value::Node(*n),
            (_, Stored::Row(_)) => Value::err(FormulaError::value(
                "A link in a column that isn't a link column",
            )),
        }
    }

    fn formula_cell(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
        row: &RowRef,
        c: &'b ColumnSnap,
        formula: &str,
    ) -> Value {
        let key = Key::Cell(*node, t.id.clone(), row.row.clone(), c.id.clone());
        if let Some(v) = self.cells.borrow().get(&key) {
            return v.clone();
        }
        let circ = || {
            Value::err(FormulaError::new(
                ErrorCode::Circ,
                format!("\"{}\" depends on itself", c.name),
            ))
        };
        match self.enter(&key) {
            Enter::Cycle => return circ(),
            Enter::TooDeep => {
                return Value::err(FormulaError::calc(
                    "This reads through too many formulas to compute",
                ))
            }
            Enter::Ok => {}
        }
        let v = match &*self.parse_cached(formula) {
            Err(e) => Value::err(e.to_error()),
            Ok(expr) => {
                let ctx = Ctx {
                    node: *node,
                    table: Some(t),
                    row: Some(row.clone()),
                    env: None,
                };
                single(self.eval(expr, &ctx))
            }
        };
        let v = if self.leave(&key) { circ() } else { v };
        self.cells.borrow_mut().insert(key, v.clone());
        v
    }

    /// A summary's rows, computed once.
    pub(crate) fn summary(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
    ) -> Rc<Result<Summary, FormulaError>> {
        let mkey = (*node, t.id.clone());
        if let Some(s) = self.summaries.borrow().get(&mkey) {
            return s.clone();
        }
        let key = Key::Summary(*node, t.id.clone());
        let circ = || FormulaError::new(ErrorCode::Circ, format!("{} depends on itself", t.name));
        match self.enter(&key) {
            Enter::Cycle => return Rc::new(Err(circ())),
            Enter::TooDeep => {
                return Rc::new(Err(FormulaError::calc(
                    "This reads through too many formulas to compute",
                )))
            }
            Enter::Ok => {}
        }
        let res = self.compute_summary(node, t);
        let res = if self.leave(&key) { Err(circ()) } else { res };
        let res = Rc::new(res);
        self.summaries.borrow_mut().insert(mkey, res.clone());
        res
    }

    fn compute_summary(&self, node: &NodeRef, t: &'b TableSnap) -> Result<Summary, FormulaError> {
        let src = t.source.as_deref().unwrap_or("");
        let parsed = self.parse_cached(src);
        let expr = match &*parsed {
            Err(e) => return Err(e.to_error()),
            Ok(e) => e,
        };
        let ctx = Ctx {
            node: *node,
            table: Some(t),
            row: None,
            env: None,
        };
        let a = match self.eval(expr, &ctx) {
            Val::Arr(a) => a,
            Val::One(Value::Error(e)) => return Err(e),
            _ => {
                return Err(FormulaError::value(
                    "A summary is made from a list or a table, like GROUPBY(…) or FILTER(…)",
                ))
            }
        };
        let keys = result_keys(a.cols.iter().map(|c| c.meta.name.as_str()));
        let mut seen = HashSet::new();
        let cols: Vec<ColMeta> = a
            .cols
            .iter()
            .enumerate()
            .map(|(j, c)| {
                let header = |k: &ResultKey| {
                    t.headers
                        .get(k)
                        .map(|h| h.trim())
                        .filter(|h| !h.is_empty())
                };
                let base = header(&ResultKey::Key(keys[j].clone()))
                    .or_else(|| header(&ResultKey::Pos(j)))
                    .unwrap_or(&c.meta.name)
                    .to_string();
                let mut name = base.clone();
                let mut k = 2;
                while !seen.insert(norm(&name)) {
                    name = format!("{base} {k}");
                    k += 1;
                }
                ColMeta {
                    name,
                    ..c.meta.clone()
                }
            })
            .collect();
        let bases: Option<Vec<RowRef>> = a.rows.as_ref().map(|r| (**r).clone()).filter(|r| {
            let mut s = HashSet::new();
            r.iter().all(|x| s.insert(x.row.clone()))
        });
        let mut rows = Vec::with_capacity(a.height);
        let mut index = HashMap::new();
        for i in 0..a.height {
            let (id, base) = match &bases {
                Some(b) => (b[i].row.clone(), Some(b[i].clone())),
                None => (format!("#{i}"), None),
            };
            index.insert(id.clone(), i);
            rows.push(SRow {
                id,
                values: (0..a.width()).map(|c| a.get(i, c).clone()).collect(),
                base,
            });
        }
        Ok(Summary { cols, keys, rows, index })
    }

    // ── Evaluation ──────────────────────────────────────────────────────

    pub(crate) fn eval(&self, e: &Expr, ctx: &Ctx<'b>) -> Val {
        let d = self.depth.get();
        if d >= MAX_EXPR_DEPTH {
            return Val::err(FormulaError::calc(
                "This reads through too many formulas to compute",
            ));
        }
        self.depth.set(d + 1);
        let v = self.eval_inner(e, ctx);
        self.depth.set(d);
        v
    }

    fn eval_inner(&self, e: &Expr, ctx: &Ctx<'b>) -> Val {
        match &e.kind {
            ExprKind::Number(x) => Val::One(Value::Number(*x)),
            ExprKind::Text(s) => Val::One(Value::Text(s.clone())),
            ExprKind::Bool(b) => Val::One(Value::Bool(*b)),
            ExprKind::Error(c) => Val::err(FormulaError::literal(*c)),
            ExprKind::Missing => Val::One(Value::Blank),
            ExprKind::Name(n) => self.eval_name(n, ctx),
            ExprKind::Ref(r) => self.eval_ref(r, ctx),
            ExprKind::Member(o, m) => {
                let obj = self.eval(o, ctx);
                self.member(&obj, &m.name)
            }
            ExprKind::Neg(a) => {
                let v = self.eval(a, ctx);
                functions::map_vals(&[&v], true, &mut |xs| match coerce::to_number(xs[0]) {
                    Ok(x) => Value::Number(-x),
                    Err(e) => Value::err(e),
                })
            }
            ExprKind::Percent(a) => {
                let v = self.eval(a, ctx);
                functions::map_vals(&[&v], false, &mut |xs| match coerce::to_number(xs[0]) {
                    Ok(x) => Value::Number(x / 100.0),
                    Err(e) => Value::err(e),
                })
            }
            ExprKind::Binary(op, a, b) => {
                let a = self.eval(a, ctx);
                let b = self.eval(b, ctx);
                let arith = !op.is_comparison() && *op != BinOp::Concat;
                functions::map_vals(&[&a, &b], arith, &mut |xs| self.binary(*op, xs[0], xs[1]))
            }
            ExprKind::Call(name, args) => functions::call(self, name, args, ctx),
        }
    }

    pub(crate) fn binary(&self, op: BinOp, a: &Value, b: &Value) -> Value {
        let r = (|| -> Result<Value, FormulaError> {
            Ok(match op {
                BinOp::Concat => {
                    let x = coerce::to_text(a, self)?;
                    let y = coerce::to_text(b, self)?;
                    Value::Text(x + &y)
                }
                BinOp::Eq => Value::Bool(coerce::equal(a, b, self)?),
                BinOp::Ne => Value::Bool(!coerce::equal(a, b, self)?),
                BinOp::Lt => Value::Bool(coerce::compare(a, b, self)?.is_lt()),
                BinOp::Gt => Value::Bool(coerce::compare(a, b, self)?.is_gt()),
                BinOp::Le => Value::Bool(coerce::compare(a, b, self)?.is_le()),
                BinOp::Ge => Value::Bool(coerce::compare(a, b, self)?.is_ge()),
                _ => {
                    let x = coerce::to_number(a)?;
                    let y = coerce::to_number(b)?;
                    let r = match op {
                        BinOp::Add => x + y,
                        BinOp::Sub => x - y,
                        BinOp::Mul => x * y,
                        BinOp::Div => {
                            if y == 0.0 {
                                return Err(FormulaError::div0());
                            }
                            x / y
                        }
                        _ => functions::power(x, y)?,
                    };
                    if !r.is_finite() {
                        return Err(FormulaError::num("The result is too large"));
                    }
                    Value::Number(r)
                }
            })
        })();
        r.unwrap_or_else(Value::err)
    }

    fn eval_name(&self, n: &str, ctx: &Ctx<'b>) -> Val {
        if let Some(v) = ctx.lookup(n) {
            return v;
        }
        if let Some(f) = Agg::by_name(n) {
            return Val::Func(f);
        }
        let snap = match self.snap(&ctx.node) {
            Ok(s) => s,
            Err(e) => return Val::err(e),
        };
        match self.find_table(snap, &NameRef::Name(n.to_string())) {
            Ok(Some(t)) => match self.table_cols(&ctx.node, t) {
                Ok(cols) => Val::arr(self.table_array(&ctx.node, t, &cols)),
                Err(e) => Val::err(e),
            },
            Ok(None) => Val::err(FormulaError::name(format!("Nothing here is called {n}"))),
            Err(e) => Val::err(e),
        }
    }

    fn eval_ref(&self, r: &Reference, ctx: &Ctx<'b>) -> Val {
        match self.eval_ref_inner(r, ctx) {
            Ok(v) => v,
            Err(e) => Val::err(e),
        }
    }

    fn eval_ref_inner(&self, r: &Reference, ctx: &Ctx<'b>) -> Result<Val, FormulaError> {
        let node = match &r.note {
            Some(n) => self.resolve_note(&n.name)?,
            None => ctx.node,
        };
        let table = match &r.table {
            None => match ctx.table {
                Some(t) if node == ctx.node => t,
                _ => {
                    return Err(FormulaError::reference(
                        "A bare [column] only means something in a table's own formulas",
                    ))
                }
            },
            Some(part) => {
                // A field of a node: `'Client website'!rate`.
                if r.note.is_some() && r.spec.is_none() {
                    if let NameRef::Name(n) = &part.name {
                        let snap = self.snap(&node);
                        let t = match snap {
                            Ok(s) => self.find_table(s, &part.name)?,
                            Err(_) => None,
                        };
                        return Ok(match t {
                            Some(t) => {
                                let cols = self.table_cols(&node, t)?;
                                Val::arr(self.table_array(&node, t, &cols))
                            }
                            None => Val::One(self.field(&node, n)),
                        });
                    }
                }
                let snap = self.snap(&node)?;
                match self.find_table(snap, &part.name)? {
                    Some(t) => t,
                    None => {
                        return Err(match &part.name {
                            NameRef::Name(n) => {
                                let list: Vec<&str> =
                                    snap.tables.iter().map(|t| t.name.as_str()).collect();
                                let place = if r.note.is_some() {
                                    self.note_name(&node)
                                } else {
                                    "this note".to_string()
                                };
                                FormulaError::reference(if list.is_empty() {
                                    format!("There's no table called {n} in {place}")
                                } else {
                                    format!(
                                        "There's no table called {n} in {place}; its tables are {}",
                                        list.join(", ")
                                    )
                                })
                            }
                            _ => FormulaError::reference(
                                "This formula points at a table that was deleted",
                            ),
                        })
                    }
                }
            }
        };
        let cols = self.table_cols(&node, table)?;
        let Some(spec) = &r.spec else {
            return Ok(Val::arr(self.table_array(&node, table, &cols)));
        };
        if spec.area == Area::Totals {
            return Err(FormulaError::reference("Tables here have no totals row"));
        }
        let pick = |p: &ColPiece| -> Result<usize, FormulaError> {
            self.find_col(&cols, &p.name)
                .ok_or_else(|| self.no_column(table, &cols, &p.name))
        };
        let idx: Vec<usize> = match &spec.cols {
            Cols::All => (0..cols.len()).collect(),
            Cols::One(p) => vec![pick(p)?],
            Cols::Range(a, b) => {
                let (a, b) = (pick(a)?, pick(b)?);
                (a.min(b)..=a.max(b)).collect()
            }
        };
        if spec.area == Area::Headers {
            return Ok(Val::arr(Array::one_row(
                idx.iter()
                    .map(|&i| {
                        (
                            ColMeta::named(cols[i].meta.name.clone()),
                            Value::Text(cols[i].meta.name.clone()),
                        )
                    })
                    .collect(),
            )));
        }
        if spec.this_row {
            let row = match &ctx.row {
                Some(row) if row.node == node && row.table == table.id => row.clone(),
                Some(_) => {
                    return Err(FormulaError::reference(format!(
                        "[@…] reads this row, and this row isn't in {}",
                        table.name
                    )))
                }
                None => {
                    return Err(FormulaError::reference(
                        "[@column] reads this row, so it only means something in a table's formula column",
                    ))
                }
            };
            if matches!(spec.cols, Cols::All) {
                return Ok(Val::One(Value::Row(row)));
            }
            if idx.len() == 1 {
                return Ok(Val::One(self.cell_value(&node, table, &cols[idx[0]], &row)));
            }
            return Ok(Val::arr(Array::one_row(
                idx.iter()
                    .map(|&i| {
                        (
                            cols[i].meta.clone(),
                            self.cell_value(&node, table, &cols[i], &row),
                        )
                    })
                    .collect(),
            )));
        }
        let picked: Vec<TabCol<'b>> = idx.iter().map(|&i| cols[i].clone()).collect();
        Ok(Val::arr(self.table_array(&node, table, &picked)))
    }

    /// A node's field, as a value: numbers and booleans read as such.
    pub(crate) fn field(&self, node: &NodeRef, name: &str) -> Value {
        let Some(info) = self.book.node(node) else {
            return Value::err(match self.snap(node) {
                Err(e) => e,
                Ok(_) => {
                    FormulaError::reference(format!("You can't read {}", self.note_name(node)))
                }
            });
        };
        let n = norm(name);
        if let Some((_, v)) = info.fields.iter().find(|(k, _)| norm(k) == n) {
            return field_value(v);
        }
        if n == "title" || n == "name" {
            return Value::Text(info.title);
        }
        let list: Vec<&str> = info.fields.iter().map(|(k, _)| k.as_str()).collect();
        Value::err(FormulaError::reference(if list.is_empty() {
            format!("{} has no field \"{name}\"; it has no fields", info.title)
        } else {
            format!(
                "{} has no field \"{name}\"; its fields are {}",
                info.title,
                list.join(", ")
            )
        }))
    }

    /// `x.name`, elementwise over a list.
    pub(crate) fn member(&self, obj: &Val, name: &NameRef) -> Val {
        match obj {
            Val::One(v) => Val::One(self.member1(v, name)),
            Val::Func(f) => Val::err(func_as_value(*f)),
            Val::Arr(a) => {
                let label = match name {
                    NameRef::Name(n) => n.clone(),
                    _ => a
                        .cols
                        .first()
                        .map(|c| c.meta.name.clone())
                        .unwrap_or_default(),
                };
                let out = functions::map_vals(&[obj], false, &mut |xs| self.member1(xs[0], name));
                match out {
                    Val::Arr(r) => {
                        let mut r = (*r).clone();
                        for c in &mut r.cols {
                            c.meta = ColMeta::named(label.clone());
                        }
                        Val::arr(r)
                    }
                    v => v,
                }
            }
        }
    }

    fn member1(&self, v: &Value, name: &NameRef) -> Value {
        match v {
            Value::Blank => Value::Blank,
            Value::Error(_) => v.clone(),
            Value::Row(r) => self.cell_of(r, name),
            Value::Node(n) => match name {
                NameRef::Name(s) => self.field(n, s),
                _ => Value::err(FormulaError::reference("A node has fields, not columns")),
            },
            _ => Value::err(FormulaError::value(match name {
                NameRef::Name(n) => format!("Only a link can be followed with \".{n}\""),
                _ => "Only a link can be followed with a dot".to_string(),
            })),
        }
    }

    /// A row's value in a column named by id or name; a summary's row falls
    /// back to the row it came from.
    pub(crate) fn cell_of(&self, row: &RowRef, name: &NameRef) -> Value {
        let snap = match self.snap(&row.node) {
            Ok(s) => s,
            Err(e) => return Value::err(e),
        };
        let Some(t) = snap.table(&row.table) else {
            return Value::err(FormulaError::reference("That table was deleted"));
        };
        let cols = match self.table_cols(&row.node, t) {
            Ok(c) => c,
            Err(e) => return Value::err(e),
        };
        if !t.is_summary() && t.row(&row.row).is_none() {
            return Value::err(FormulaError::reference(format!(
                "That row of {} was deleted",
                t.name
            )));
        }
        match self.find_col(&cols, name) {
            Some(i) => self.cell_value(&row.node, t, &cols[i], row),
            None => {
                if t.is_summary() {
                    if let Ok(s) = &*self.summary(&row.node, t) {
                        if let Some(base) =
                            s.index.get(&row.row).and_then(|&k| s.rows[k].base.clone())
                        {
                            return self.cell_of(&base, name);
                        }
                    }
                }
                Value::err(self.no_column(t, &cols, name))
            }
        }
    }
}

/// A field's text as a value.
fn field_value(s: &str) -> Value {
    let t = s.trim();
    if t.is_empty() {
        return Value::Blank;
    }
    if t.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    match coerce::parse_number_text(t) {
        Some(x) if !t.contains(':') => Value::Number(x),
        _ => Value::Text(s.to_string()),
    }
}

pub(crate) fn func_as_value(f: Agg) -> FormulaError {
    FormulaError::value(format!(
        "{} is a function; call it like {}(…)",
        f.name(),
        f.name()
    ))
}

/// What a cell shows for an evaluated formula: one value, or `#SPILL!`.
pub(crate) fn single(v: Val) -> Value {
    match v {
        Val::One(v) => v,
        Val::Func(f) => Value::err(func_as_value(f)),
        Val::Arr(a) if a.height == 1 && a.width() == 1 => a.get(0, 0).clone(),
        Val::Arr(a) if a.height == 0 => Value::err(FormulaError::calc("This is an empty list")),
        Val::Arr(_) => Value::err(FormulaError::new(
            ErrorCode::Spill,
            "This gives a whole list, and a cell holds one value. Wrap it in SUM(…), COUNT(…) or similar",
        )),
    }
}

impl Labels for Engine<'_> {
    fn row_label(&self, row: &RowRef) -> String {
        Engine::row_label(self, row)
    }

    fn node_title(&self, node: &NodeRef) -> String {
        match self.book.node(node) {
            Some(info) => info.title,
            None => self.book.display_name(node),
        }
    }
}

/// The keys of a summary's result columns, from the names its formula gives
/// them ([`ResultKey`]): the name, `name 2` for its second use, `#<position>`
/// for a column with none.
pub(crate) fn result_keys<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    names
        .enumerate()
        .map(|(j, n)| {
            let n = n.trim();
            if n.is_empty() {
                return format!("#{j}");
            }
            let mut key = n.to_string();
            let mut k = 2;
            while !seen.insert(norm(&key)) {
                key = format!("{n} {k}");
                k += 1;
            }
            key
        })
        .collect()
}
