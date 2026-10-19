//! What formulas compute with. A table cell holds one [`Value`]; lists and
//! tables exist only while a formula is being evaluated ([`Val`] and
//! [`Array`]), because a cell that would hold a list is `#SPILL!`.
//!
//! Errors are values, as in Excel: they flow through arithmetic and are
//! caught by `IFERROR`. Two are not Excel's and cannot be caught: `#CIRC!`
//! (a cell on a dependency cycle, which no formula may hide) and the
//! loading state (a referenced note that has not arrived yet, which must not
//! be mistaken for an answer).

use std::fmt;
use std::rc::Rc;

use crate::model::{Format, NodeRef};

/// Excel's error codes, plus Pimble's two (`#CIRC!`, loading).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// `#N/A`: a lookup found nothing.
    NA,
    /// `#VALUE!`: the wrong kind of value.
    Value,
    /// `#REF!`: points at something that is not there.
    Ref,
    /// `#NAME?`: an unknown function or name.
    Name,
    /// `#DIV/0!`
    Div0,
    /// `#NUM!`: a number out of range.
    Num,
    /// `#SPILL!`: a list where one value goes.
    Spill,
    /// `#CALC!`: a calculation with no answer (an empty FILTER).
    Calc,
    /// `#CIRC!`: a cell that depends on itself.
    Circ,
    /// `#ERROR!`: the formula does not parse.
    Error,
    /// A referenced note has not arrived yet; shown as `Loading…`.
    Loading,
}

impl ErrorCode {
    /// The code as shown in a cell.
    pub fn code(self) -> &'static str {
        match self {
            ErrorCode::NA => "#N/A",
            ErrorCode::Value => "#VALUE!",
            ErrorCode::Ref => "#REF!",
            ErrorCode::Name => "#NAME?",
            ErrorCode::Div0 => "#DIV/0!",
            ErrorCode::Num => "#NUM!",
            ErrorCode::Spill => "#SPILL!",
            ErrorCode::Calc => "#CALC!",
            ErrorCode::Circ => "#CIRC!",
            ErrorCode::Error => "#ERROR!",
            ErrorCode::Loading => "Loading…",
        }
    }

    /// The code an error literal in a formula names (`#N/A`, `#REF!`, ...).
    pub fn from_code(s: &str) -> Option<ErrorCode> {
        let all = [
            ErrorCode::NA,
            ErrorCode::Value,
            ErrorCode::Ref,
            ErrorCode::Name,
            ErrorCode::Div0,
            ErrorCode::Num,
            ErrorCode::Spill,
            ErrorCode::Calc,
            ErrorCode::Circ,
            ErrorCode::Error,
        ];
        all.into_iter().find(|c| c.code().eq_ignore_ascii_case(s))
    }

    /// Whether `IFERROR` and friends may catch it.
    pub fn catchable(self) -> bool {
        !matches!(self, ErrorCode::Circ | ErrorCode::Loading)
    }

    /// The sentence an error literal typed into a formula carries.
    fn default_message(self) -> &'static str {
        match self {
            ErrorCode::NA => "No value is available",
            ErrorCode::Value => "A value is the wrong kind",
            ErrorCode::Ref => "This formula points at something that was deleted",
            ErrorCode::Name => "A name in this formula is unknown",
            ErrorCode::Div0 => "Divided by zero",
            ErrorCode::Num => "A number is out of range",
            ErrorCode::Spill => "A list where one value goes",
            ErrorCode::Calc => "This has no answer",
            ErrorCode::Circ => "This depends on itself",
            ErrorCode::Error => "This formula doesn't parse",
            ErrorCode::Loading => "Waiting for a note to arrive",
        }
    }
}

/// An error with the sentence the formula bar shows.
#[derive(Debug, Clone, PartialEq)]
pub struct FormulaError {
    pub code: ErrorCode,
    pub message: String,
}

impl FormulaError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        FormulaError {
            code,
            message: message.into(),
        }
    }

    /// The error an error literal (`#N/A` typed into a formula) is.
    pub fn literal(code: ErrorCode) -> Self {
        FormulaError::new(code, code.default_message())
    }

    pub(crate) fn na(m: impl Into<String>) -> Self {
        Self::new(ErrorCode::NA, m)
    }
    pub(crate) fn value(m: impl Into<String>) -> Self {
        Self::new(ErrorCode::Value, m)
    }
    pub(crate) fn reference(m: impl Into<String>) -> Self {
        Self::new(ErrorCode::Ref, m)
    }
    pub(crate) fn name(m: impl Into<String>) -> Self {
        Self::new(ErrorCode::Name, m)
    }
    pub(crate) fn div0() -> Self {
        Self::new(ErrorCode::Div0, "Divided by zero")
    }
    pub(crate) fn num(m: impl Into<String>) -> Self {
        Self::new(ErrorCode::Num, m)
    }
    pub(crate) fn calc(m: impl Into<String>) -> Self {
        Self::new(ErrorCode::Calc, m)
    }
}

impl fmt::Display for FormulaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.message)
    }
}

impl std::error::Error for FormulaError {}

/// A table, by the node it lives in and its id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableAddr {
    pub node: NodeRef,
    pub table: String,
}

/// A row of a table. For a summary's rows the id is the source row's id
/// when the summary's rows came from one table's rows (`FILTER(Trips, …)`),
/// and `#<index>` otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowRef {
    pub node: NodeRef,
    pub table: String,
    pub row: String,
}

impl RowRef {
    pub fn table_addr(&self) -> TableAddr {
        TableAddr {
            node: self.node,
            table: self.table.clone(),
        }
    }
}

/// One value: what a cell holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// An empty cell: 0 in arithmetic, "" in text.
    Blank,
    Number(f64),
    Text(String),
    Bool(bool),
    /// A link to a row; shown as the row's label.
    Row(RowRef),
    /// A link to a node; shown as its title.
    Node(NodeRef),
    Error(FormulaError),
}

impl Value {
    pub fn is_blank(&self) -> bool {
        matches!(self, Value::Blank)
    }

    pub fn error(&self) -> Option<&FormulaError> {
        match self {
            Value::Error(e) => Some(e),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(x) => Some(*x),
            _ => None,
        }
    }

    pub(crate) fn err(e: FormulaError) -> Value {
        Value::Error(e)
    }
}

impl From<FormulaError> for Value {
    fn from(e: FormulaError) -> Self {
        Value::Error(e)
    }
}

/// What a column of a computed list is called and how it is shown.
#[derive(Debug, Clone, PartialEq)]
pub struct ColMeta {
    pub name: String,
    pub format: Format,
    /// The table its values link into, when it is a link column carried
    /// through (`Transactions[category]`, `FILTER(Entries[project], …)` is
    /// not a row link and has none).
    pub link: Option<TableAddr>,
}

impl ColMeta {
    pub(crate) fn named(name: impl Into<String>) -> Self {
        ColMeta {
            name: name.into(),
            format: Format::Auto,
            link: None,
        }
    }
}

/// One column of an [`Array`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Column {
    pub meta: ColMeta,
    pub values: Rc<Vec<Value>>,
}

/// A list or a table inside evaluation: columns of equal height, and the
/// rows they came from when they came from one table's rows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Array {
    pub cols: Vec<Column>,
    pub height: usize,
    pub rows: Option<Rc<Vec<RowRef>>>,
}

impl Array {
    pub fn width(&self) -> usize {
        self.cols.len()
    }

    pub fn get(&self, r: usize, c: usize) -> &Value {
        &self.cols[c].values[r]
    }

    /// The value at (r, c) when this array is broadcast to a larger shape: a
    /// single column repeats across, a single row repeats down.
    pub fn at(&self, r: usize, c: usize) -> &Value {
        let r = if self.height == 1 { 0 } else { r };
        let c = if self.cols.len() == 1 { 0 } else { c };
        &self.cols[c].values[r]
    }

    /// Every value, row by row.
    pub fn values(&self) -> impl Iterator<Item = &Value> + '_ {
        (0..self.height).flat_map(move |r| self.cols.iter().map(move |c| &c.values[r]))
    }

    pub fn single_column(
        meta: ColMeta,
        values: Vec<Value>,
        rows: Option<Rc<Vec<RowRef>>>,
    ) -> Array {
        let height = values.len();
        let rows = rows.filter(|r| r.len() == height);
        Array {
            cols: vec![Column {
                meta,
                values: Rc::new(values),
            }],
            height,
            rows,
        }
    }

    /// One row of values, e.g. `HSTACK(1, 2)`.
    pub fn one_row(cols: Vec<(ColMeta, Value)>) -> Array {
        Array {
            cols: cols
                .into_iter()
                .map(|(meta, v)| Column {
                    meta,
                    values: Rc::new(vec![v]),
                })
                .collect(),
            height: 1,
            rows: None,
        }
    }

    pub fn from_columns(cols: Vec<Column>, rows: Option<Rc<Vec<RowRef>>>) -> Array {
        let height = cols.first().map(|c| c.values.len()).unwrap_or(0);
        let rows = rows.filter(|r| r.len() == height);
        Array { cols, height, rows }
    }

    /// The rows at these indices, in this order.
    pub fn pick_rows(&self, idx: &[usize]) -> Array {
        Array {
            cols: self
                .cols
                .iter()
                .map(|c| Column {
                    meta: c.meta.clone(),
                    values: Rc::new(idx.iter().map(|&i| c.values[i].clone()).collect()),
                })
                .collect(),
            height: idx.len(),
            rows: self
                .rows
                .as_ref()
                .map(|rows| Rc::new(idx.iter().map(|&i| rows[i].clone()).collect())),
        }
    }

    /// These columns, by index.
    pub fn pick_cols(&self, lo: usize, hi: usize) -> Array {
        Array {
            cols: self.cols[lo..=hi].to_vec(),
            height: self.height,
            rows: self.rows.clone(),
        }
    }
}

/// The aggregates a bare name can pass as a function value (`GROUPBY(…, SUM)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Agg {
    Sum,
    Average,
    Min,
    Max,
    Count,
    CountA,
    CountBlank,
    Median,
    Product,
}

impl Agg {
    pub fn by_name(n: &str) -> Option<Agg> {
        Some(match n.to_ascii_uppercase().as_str() {
            "SUM" => Agg::Sum,
            "AVERAGE" => Agg::Average,
            "MIN" => Agg::Min,
            "MAX" => Agg::Max,
            "COUNT" => Agg::Count,
            "COUNTA" => Agg::CountA,
            "COUNTBLANK" => Agg::CountBlank,
            "MEDIAN" => Agg::Median,
            "PRODUCT" => Agg::Product,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Agg::Sum => "SUM",
            Agg::Average => "AVERAGE",
            Agg::Min => "MIN",
            Agg::Max => "MAX",
            Agg::Count => "COUNT",
            Agg::CountA => "COUNTA",
            Agg::CountBlank => "COUNTBLANK",
            Agg::Median => "MEDIAN",
            Agg::Product => "PRODUCT",
        }
    }
}

/// A value inside evaluation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Val {
    One(Value),
    Arr(Rc<Array>),
    Func(Agg),
}

impl Val {
    pub fn err(e: FormulaError) -> Val {
        Val::One(Value::Error(e))
    }

    pub fn arr(a: Array) -> Val {
        Val::Arr(Rc::new(a))
    }

    /// The error this is, when it is one scalar error.
    pub fn as_error(&self) -> Option<&FormulaError> {
        match self {
            Val::One(Value::Error(e)) => Some(e),
            _ => None,
        }
    }
}

impl From<Value> for Val {
    fn from(v: Value) -> Self {
        Val::One(v)
    }
}

/// What a one-off evaluation answers (the MCP's `evaluate`): one value, or
/// the list or table the formula gives.
#[derive(Debug, Clone, PartialEq)]
pub enum Evaluated {
    Value(Value),
    Table(ResultTable),
}

/// A list or table result, as plain values.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultTable {
    pub columns: Vec<ColMeta>,
    /// Row by row.
    pub rows: Vec<Vec<Value>>,
}
