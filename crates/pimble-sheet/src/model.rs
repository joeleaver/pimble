//! What a node's sheets hold, read once from the document (contract section
//! 2). These are plain values: the document module reads them out of yrs
//! ([`crate::doc`]), the formula engine computes over them ([`crate::formula`]),
//! and the app and the MCP show them. Nothing here is computed.

use std::collections::{BTreeMap, BTreeSet};

use pimble_core::{NodeId, StoreId};

/// A node, by the canonical address every node has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub store: StoreId,
    pub node: NodeId,
}

/// How a formula column's values are shown (contract section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Auto,
    Number,
    Money,
    Date,
    Hours,
    Percent,
}

impl Format {
    /// The word stored in the document.
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Auto => "auto",
            Format::Number => "number",
            Format::Money => "money",
            Format::Date => "date",
            Format::Hours => "hours",
            Format::Percent => "percent",
        }
    }

    /// The format a stored word names; anything unknown is automatic.
    pub fn parse(s: &str) -> Format {
        match s {
            "number" => Format::Number,
            "money" => Format::Money,
            "date" => Format::Date,
            "hours" => Format::Hours,
            "percent" => Format::Percent,
            _ => Format::Auto,
        }
    }
}

/// A column's type (contract section 3: the UI calls it "type").
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnType {
    Text,
    Number,
    Money,
    /// Stored as an ISO date; the engine sees an Excel serial number.
    Date,
    /// A number of hours.
    Hours,
    /// Each cell names a row of the table with this id, in the same node.
    RowLink { table: String },
    /// Each cell names a node; the picker offers the children of `under`, or
    /// any node when it is absent.
    NodeLink { under: Option<NodeRef> },
    /// One formula for the whole column, in its stored form (ids, not names;
    /// contract section 6).
    Formula { formula: String, format: Format },
}

impl ColumnType {
    /// The word stored as the column's `type`.
    pub fn kind(&self) -> &'static str {
        match self {
            ColumnType::Text => "text",
            ColumnType::Number => "number",
            ColumnType::Money => "money",
            ColumnType::Date => "date",
            ColumnType::Hours => "hours",
            ColumnType::RowLink { .. } => "row_link",
            ColumnType::NodeLink { .. } => "node_link",
            ColumnType::Formula { .. } => "formula",
        }
    }

    /// The format values of this column are shown in.
    pub fn format(&self) -> Format {
        match self {
            ColumnType::Number => Format::Number,
            ColumnType::Money => Format::Money,
            ColumnType::Date => Format::Date,
            ColumnType::Hours => Format::Hours,
            ColumnType::Formula { format, .. } => *format,
            _ => Format::Auto,
        }
    }
}

/// What a person typed into a cell, as stored (contract section 2).
#[derive(Debug, Clone, PartialEq)]
pub enum Stored {
    Number(f64),
    Text(String),
    /// An ISO date, `YYYY-MM-DD`.
    Date(String),
    /// A row of the column's table, by id.
    Row(String),
    /// A node.
    Node(NodeRef),
    /// Text that did not parse as the column's type, kept as typed; it shows
    /// `#VALUE!` (or `#N/A` in a link column) until it is fixed.
    Unparsed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSnap {
    pub id: String,
    pub name: String,
    pub order: String,
    pub ty: ColumnType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RowSnap {
    pub id: String,
    pub order: String,
    /// By column id. A column with no entry is blank in this row.
    pub cells: BTreeMap<String, Stored>,
}

/// Which of a summary's result columns something means (contract section 4).
///
/// A result column has no id of its own: it is whatever the summary's formula
/// gives. Its key is the name the formula gives it before any rename (`at` for
/// `Expenses[at]`; `at 2` for a second `at`; `#3` for an unnamed fourth), so a
/// reshape that keeps a column (`GROUPBY(at, …)` becoming
/// `GROUPBY(HSTACK(date, at), …)`) keeps everything that reads or renames it.
/// `Pos` is how formulas and headers written before keys name one, by
/// position; it still works, and an edit of the summary's formula rewrites the
/// note's own uses to keys first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResultKey {
    Pos(usize),
    Key(String),
}

impl ResultKey {
    /// The key of a `headers` entry as stored: `k:<key>`, or a position.
    pub fn from_stored(s: &str) -> Option<ResultKey> {
        match s.strip_prefix("k:") {
            Some(k) => Some(ResultKey::Key(k.to_string())),
            None => s.parse::<usize>().ok().map(ResultKey::Pos),
        }
    }

    pub fn to_stored(&self) -> String {
        match self {
            ResultKey::Pos(i) => i.to_string(),
            ResultKey::Key(k) => format!("k:{k}"),
        }
    }
}

/// What one column of a summary row shows (contract section 4b): one of the
/// menu's aggregates, or a formula in its stored form.
#[derive(Debug, Clone, PartialEq)]
pub enum SummaryCell {
    Sum,
    Average,
    Count,
    Min,
    Max,
    /// The stored formula, without its `=`.
    Formula(String),
}

impl SummaryCell {
    /// The value stored in the summary row's `cells` map.
    pub fn to_stored(&self) -> String {
        match self {
            SummaryCell::Sum => "sum".into(),
            SummaryCell::Average => "average".into(),
            SummaryCell::Count => "count".into(),
            SummaryCell::Min => "min".into(),
            SummaryCell::Max => "max".into(),
            SummaryCell::Formula(f) => format!("={f}"),
        }
    }

    /// A stored value read back; anything unknown is nothing.
    pub fn from_stored(s: &str) -> Option<SummaryCell> {
        Some(match s {
            "sum" => SummaryCell::Sum,
            "average" => SummaryCell::Average,
            "count" => SummaryCell::Count,
            "min" => SummaryCell::Min,
            "max" => SummaryCell::Max,
            _ => SummaryCell::Formula(s.strip_prefix('=')?.to_string()),
        })
    }

    /// The menu's word for it, as the app shows it.
    pub fn label(&self) -> &'static str {
        match self {
            SummaryCell::Sum => "Sum",
            SummaryCell::Average => "Average",
            SummaryCell::Count => "Count",
            SummaryCell::Min => "Min",
            SummaryCell::Max => "Max",
            SummaryCell::Formula(_) => "Formula",
        }
    }
}

/// A summary row of a plain table (contract section 4b).
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRowSnap {
    pub id: String,
    pub name: String,
    pub order: String,
    /// By column id. A column with no entry shows nothing.
    pub cells: BTreeMap<String, SummaryCell>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableSnap {
    pub id: String,
    pub name: String,
    pub order: String,
    /// A summary's formula, stored form; `None` for a plain table.
    pub source: Option<String>,
    /// A summary's renamed result columns, by key (or, written before keys,
    /// by position).
    pub headers: BTreeMap<ResultKey, String>,
    /// In column order (then id).
    pub columns: Vec<ColumnSnap>,
    /// In row order (then id). A summary has none.
    pub rows: Vec<RowSnap>,
    /// Its summary rows, top to bottom (contract section 4b).
    pub summary_rows: Vec<SummaryRowSnap>,
    /// The column its rows are grouped by, when they are.
    pub group_by: Option<String>,
    /// Drawn with its fields down the left and each record as a column
    /// (contract section 4c); only the drawing turns.
    pub rotated: bool,
}

impl TableSnap {
    /// The tables of its own note this table reads (by id, itself left out):
    /// a row link's target, and every table a formula or a summary's source
    /// names without a note. A table that reads another, or is read by one,
    /// cannot leave its note alone without that reference breaking.
    pub fn tables_read(&self) -> BTreeSet<String> {
        fn scan(stored: &str, out: &mut BTreeSet<String>) {
            let mut rest = stored;
            while let Some(at) = rest.find("{t:") {
                let before = &stored[..stored.len() - rest.len() + at];
                rest = &rest[at + 3..];
                let Some(end) = rest.find('}') else { break };
                // `{n:<store>/<node>}!{t:..}` names another note's table.
                if !before.ends_with("}!") {
                    out.insert(rest[..end].to_string());
                }
                rest = &rest[end..];
            }
        }
        let mut out = BTreeSet::new();
        if let Some(source) = &self.source {
            scan(source, &mut out);
        }
        for s in &self.summary_rows {
            for cell in s.cells.values() {
                if let SummaryCell::Formula(f) = cell {
                    scan(f, &mut out);
                }
            }
        }
        for column in &self.columns {
            match &column.ty {
                ColumnType::RowLink { table } => {
                    out.insert(table.clone());
                }
                ColumnType::Formula { formula, .. } => scan(formula, &mut out),
                _ => {}
            }
        }
        out.remove(&self.id);
        out
    }

    /// The tables of its own note this table's row link columns point at.
    pub fn row_links(&self) -> BTreeSet<String> {
        self.columns
            .iter()
            .filter_map(|c| match &c.ty {
                ColumnType::RowLink { table } if *table != self.id => Some(table.clone()),
                _ => None,
            })
            .collect()
    }

    /// This table with every formula and its summary's source passed
    /// through `f` (a stored formula in, its rewrite out).
    pub fn map_formulas(&self, f: impl Fn(&str) -> String) -> TableSnap {
        let mut t = self.clone();
        t.source = t.source.as_deref().map(&f);
        for c in &mut t.columns {
            if let ColumnType::Formula { formula, .. } = &mut c.ty {
                *formula = f(formula);
            }
        }
        for s in &mut t.summary_rows {
            for cell in s.cells.values_mut() {
                if let SummaryCell::Formula(formula) = cell {
                    *formula = f(formula);
                }
            }
        }
        t
    }

    pub fn summary_row(&self, id: &str) -> Option<&SummaryRowSnap> {
        self.summary_rows.iter().find(|s| s.id == id)
    }

    pub fn column(&self, id: &str) -> Option<&ColumnSnap> {
        self.columns.iter().find(|c| c.id == id)
    }

    pub fn row(&self, id: &str) -> Option<&RowSnap> {
        self.rows.iter().find(|r| r.id == id)
    }

    pub fn is_summary(&self) -> bool {
        self.source.is_some()
    }
}

/// Every table of one node (`data.sheets`), in table order (then id).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SheetsSnap {
    pub tables: Vec<TableSnap>,
}

impl SheetsSnap {
    pub fn table(&self, id: &str) -> Option<&TableSnap> {
        self.tables.iter().find(|t| t.id == id)
    }
}
