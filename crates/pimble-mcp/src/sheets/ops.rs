//! What the sheet tools do, over a node's document and a [`McpBook`], with
//! no connection: every tool fetches the node, runs one of these on its
//! document, and sends the update the edit made through `applyEdit` (the
//! caller's `edit`). A refused operation answers `Err` with the sentence, and
//! then nothing is sent, so a check that runs after a trial write (a formula
//! that spills) still writes nothing.
//!
//! Formulas come in and go out as people write them (names), and are turned
//! into ids with `to_stored` exactly as the app's formula editor does.

use std::collections::BTreeMap;

use pimble_sheet::doc::{self, Placement, Sheets, SheetsTxn};
use pimble_sheet::formula::{self, ComputedTable, Engine, ErrorCode, Evaluated, NameError, NameErrorKind, RowRef, Value};
use pimble_sheet::input;
use pimble_sheet::{Book, ColumnSnap, ColumnType, Format, NodeRef, SheetsSnap, Stored, TableSnap};
use schemars::JsonSchema;
use serde::Deserialize;

use super::book::McpBook;

/// How many rows a tool's answer previews after a formula is accepted.
const PREVIEW_ROWS: usize = 5;

/// A column as a tool is given it.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ColumnSpec {
    /// The column's name, unique in its table.
    pub name: String,
    /// text, number, money, date, hours, link (each cell names a row of the
    /// table `target`), node_link (each cell names a node; see `under`) or
    /// formula. Defaults to text, or to formula when `formula` is given.
    #[serde(rename = "type", default)]
    pub ty: Option<String>,
    /// A link column's table: the name of a table in the same note.
    #[serde(default)]
    pub target: Option<String>,
    /// A node_link column's parent: the node whose children its cells choose
    /// from (a title, a path like "Home/Projects", or a pimble: link). Any
    /// node when left out.
    #[serde(default)]
    pub under: Option<String>,
    /// A formula column's formula, as you would type it in Excel inside a
    /// Table, starting with =: `=[@miles]*2`, `=[@vehicle].mpg`. Read
    /// sheet_formula_guide first.
    #[serde(default)]
    pub formula: Option<String>,
    /// How a formula column's values show: auto, number, money, date, hours
    /// or percent.
    #[serde(default)]
    pub format: Option<String>,
}

/// One row's values, by column name, as a person would type them.
pub type RowValues = BTreeMap<String, serde_json::Value>;

/// A change to one row.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RowUpdate {
    /// The row: its id, or the value of its first column.
    pub row: String,
    /// The new values by column name, as typed (`"$12.50"`, `"yesterday"`, a
    /// linked row's first-column value); null or "" blanks a cell.
    pub values: RowValues,
}

/// A node's tables being read or edited: the yrs document they live in and
/// the book formulas are computed over, kept in step after every write.
pub struct Work<'b> {
    sheets: Sheets,
    pub book: &'b mut McpBook,
}

impl<'b> Work<'b> {
    pub fn new(sheets: Sheets, book: &'b mut McpBook) -> Self {
        book.set_home_snap(sheets.read());
        Self { sheets, book }
    }

    pub fn snap(&self) -> &SheetsSnap {
        self.book.home_snap()
    }

    pub fn home(&self) -> NodeRef {
        self.book.home()
    }

    /// One write transaction; the book sees its result at once.
    pub fn write<R>(&mut self, f: impl FnOnce(&mut SheetsTxn<'_, '_>) -> doc::Result<R>) -> Result<R, String> {
        let edited = self.sheets.edit(f);
        self.book.set_home_snap(self.sheets.read());
        edited.result.map_err(|e| e.to_string())
    }

    fn table(&self, id: &str) -> Result<TableSnap, String> {
        self.snap().table(id).cloned().ok_or_else(|| "That table was deleted.".to_string())
    }
}

// ── Sentences ────────────────────────────────────────────────────────────

/// A sentence ends with a stop.
pub fn sentence(s: &str) -> String {
    let s = s.trim();
    if s.ends_with(['.', '!', '?']) {
        s.to_string()
    } else {
        format!("{s}.")
    }
}

/// A formula's names that do not resolve, as the refusal says it: an
/// ambiguous note lists its candidates by path, in the form to write.
pub fn name_error(e: &NameError) -> String {
    let mut s = sentence(&e.message);
    if e.kind == NameErrorKind::Ambiguous && !e.candidates.is_empty() {
        let distinct = e.candidates.iter().any(|c| c.name != e.candidates[0].name);
        if distinct {
            let list: Vec<String> = e.candidates.iter().map(|c| formula::note_text(&c.name)).collect();
            s.push_str(&format!(" It could be {}; write the one you mean that way.", list.join(" or ")));
        }
    }
    if e.kind == NameErrorKind::Parse {
        s.push_str(" See sheet_formula_guide for the syntax.");
    }
    s
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn quoted_list(names: impl IntoIterator<Item = String>) -> String {
    let all: Vec<String> = names.into_iter().map(|n| format!("\"{n}\"")).collect();
    if all.is_empty() {
        "none".to_string()
    } else {
        all.join(", ")
    }
}

// ── Finding tables, columns and rows by name ─────────────────────────────

/// A table of this note by name (case-insensitive) or id.
/// The node a column's `under` names: a pimble: link, or a title or path
/// one node answers to.
fn find_node(work: &Work, name: &str) -> Result<NodeRef, String> {
    let name = name.trim().trim_matches('\'');
    if let Some(url) = pimble_core::PimbleUrl::parse(name) {
        return Ok(NodeRef { store: url.store, node: url.node });
    }
    match work.book.find_nodes(name).as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("No node here is called {name}.")),
        many => Err(format!(
            "Several nodes are called {name}: {}. Give the one you mean as its path or its pimble: link.",
            many.iter().map(|n| work.book.display_name(n)).collect::<Vec<_>>().join(", ")
        )),
    }
}

pub fn find_table<'s>(snap: &'s SheetsSnap, name: &str) -> Result<&'s TableSnap, String> {
    let name = name.trim();
    if let Some(t) = snap.table(name) {
        return Ok(t);
    }
    let hits: Vec<&TableSnap> = snap.tables.iter().filter(|t| norm(&t.name) == norm(name)).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] if snap.tables.is_empty() => Err(format!("This note has no tables, so none is called \"{name}\".")),
        [] => Err(format!(
            "This note has no table called \"{name}\"; its tables are {}.",
            quoted_list(snap.tables.iter().map(|t| t.name.clone()))
        )),
        several => Err(format!(
            "{} tables here are called \"{name}\"; name one by id ({}), or rename one.",
            several.len(),
            several.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// A column of a table by name (case-insensitive) or id.
pub fn find_column<'s>(t: &'s TableSnap, name: &str) -> Result<&'s ColumnSnap, String> {
    let name = name.trim();
    if let Some(c) = t.column(name) {
        return Ok(c);
    }
    let hits: Vec<&ColumnSnap> = t.columns.iter().filter(|c| norm(&c.name) == norm(name)).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "{} has no column \"{name}\"; its columns are {}.",
            t.name,
            quoted_list(t.columns.iter().map(|c| c.name.clone()))
        )),
        several => Err(format!(
            "{} columns of {} are called \"{name}\"; name one by id ({}), or rename one.",
            several.len(),
            t.name,
            several.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn row_ref(home: NodeRef, table: &str, row: &str) -> RowRef {
    RowRef { node: home, table: table.to_string(), row: row.to_string() }
}

/// Each row's id and label (its first column's value, as computed).
pub fn row_labels(engine: &Engine<'_>, home: NodeRef, t: &TableSnap) -> Vec<(String, String)> {
    t.rows.iter().map(|r| (r.id.clone(), engine.row_label(&row_ref(home, &t.id, &r.id)))).collect()
}

/// A row by its id or its label.
pub fn find_row(labels: &[(String, String)], table: &str, name: &str) -> Result<String, String> {
    let name = name.trim();
    if let Some((id, _)) = labels.iter().find(|(id, _)| id == name) {
        return Ok(id.clone());
    }
    let hits: Vec<&String> = labels.iter().filter(|(_, l)| norm(l) == norm(name)).map(|(id, _)| id).collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] if labels.is_empty() => Err(format!("{table} has no rows.")),
        [] => {
            let some: Vec<String> = labels.iter().take(20).map(|(_, l)| l.clone()).collect();
            Err(format!(
                "{table} has no row \"{name}\" (rows are named by their first column, or by id); its rows include {}.",
                quoted_list(some)
            ))
        }
        several => Err(format!(
            "{} rows of {table} are called \"{name}\"; name one by id: {}.",
            several.len(),
            several.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        )),
    }
}

// ── Column types ─────────────────────────────────────────────────────────

pub fn parse_format(s: &str) -> Result<Format, String> {
    match s.trim().to_lowercase().as_str() {
        "" | "auto" | "automatic" => Ok(Format::Auto),
        "number" => Ok(Format::Number),
        "money" | "currency" => Ok(Format::Money),
        "date" => Ok(Format::Date),
        "hours" => Ok(Format::Hours),
        "percent" | "percentage" => Ok(Format::Percent),
        other => Err(format!("\"{other}\" is not a format; formats are auto, number, money, date, hours and percent.")),
    }
}

/// What a column's type is called in answers.
pub fn type_text(book: &McpBook, snap: &SheetsSnap, table: Option<&str>, ty: &ColumnType) -> String {
    match ty {
        ColumnType::Text => "text".into(),
        ColumnType::Number => "number".into(),
        ColumnType::Money => "money".into(),
        ColumnType::Date => "date".into(),
        ColumnType::Hours => "hours".into(),
        ColumnType::RowLink { table } => {
            let name = snap.table(table).map(|t| t.name.clone()).unwrap_or_else(|| "a deleted table".into());
            format!("link to a row of {name}")
        }
        ColumnType::NodeLink { under } => match under.and_then(|u| book.node(&u)) {
            Some(info) => format!("link to a node under {}", info.title),
            None => "link to a node".into(),
        },
        ColumnType::Formula { formula: f, format } => {
            format!("formula {}, format {}", formula::to_shown(book, table, f), format.as_str())
        }
    }
}

/// The type a column spec asks for. `current` is the column's type when it
/// is being changed: what the spec leaves out is kept from it.
pub fn column_type(work: &Work, table: Option<&str>, spec: &ColumnSpec, current: Option<&ColumnType>) -> Result<ColumnType, String> {
    let kind = match spec.ty.as_deref().map(|t| t.trim().to_lowercase()) {
        Some(k) if !k.is_empty() => k,
        _ if spec.formula.is_some() => "formula".to_string(),
        _ => current.map(|c| c.kind().to_string()).unwrap_or_else(|| "text".to_string()),
    };
    Ok(match kind.as_str() {
        "text" => ColumnType::Text,
        "number" => ColumnType::Number,
        "money" | "currency" => ColumnType::Money,
        "date" => ColumnType::Date,
        "hours" => ColumnType::Hours,
        "link" | "row_link" | "row link" => {
            let target = match (&spec.target, current) {
                (Some(t), _) => {
                    let t = find_table(work.snap(), t)?;
                    if t.is_summary() {
                        return Err(format!("A link column links to rows of a plain table; {} is a summary.", t.name));
                    }
                    t.id.clone()
                }
                (None, Some(ColumnType::RowLink { table })) => table.clone(),
                (None, _) => return Err("A link column needs `target`: the table whose rows it links to.".into()),
            };
            ColumnType::RowLink { table: target }
        }
        "node_link" | "node link" | "node" => {
            let under = match (&spec.under, current) {
                (Some(u), _) if u.trim().is_empty() => None,
                (Some(u), _) => Some(find_node(work, u)?),
                (None, Some(ColumnType::NodeLink { under })) => *under,
                (None, _) => None,
            };
            ColumnType::NodeLink { under }
        }
        "formula" => {
            let format = match (&spec.format, current) {
                (Some(f), _) => parse_format(f)?,
                (None, Some(ColumnType::Formula { format, .. })) => *format,
                (None, Some(other)) => other.format(),
                (None, None) => Format::Auto,
            };
            let stored = match (&spec.formula, current) {
                (Some(shown), _) => {
                    let shown = shown.trim();
                    if shown.trim_start_matches('=').trim().is_empty() {
                        return Err("Write the formula, starting with =.".into());
                    }
                    formula::to_stored(&*work.book, table, shown).map_err(|e| name_error(&e))?
                }
                (None, Some(ColumnType::Formula { formula, .. })) => formula.clone(),
                (None, _) => return Err("A formula column needs `formula`, starting with =.".into()),
            };
            ColumnType::Formula { formula: stored, format }
        }
        other => {
            return Err(format!(
                "\"{other}\" is not a column type; types are text, number, money, date, hours, link and formula."
            ))
        }
    })
}

// ── Values as typed ──────────────────────────────────────────────────────

fn json_text(v: &serde_json::Value) -> Result<String, String> {
    Ok(match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        other => return Err(format!("{other} is not a value a cell takes; give text as a person would type it.")),
    })
}

fn what_it_takes(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Number => "a number",
        ColumnType::Money => "an amount like $12.50",
        ColumnType::Date => "a date (2026-10-14, Oct 14, 10/14, today or yesterday)",
        ColumnType::Hours => "hours (1:30, 1.5, 90m or 1h 30m)",
        _ => "a value",
    }
}

/// What typing `text` into `col` stores (`None` is a blank cell). A link
/// names a row of its table by the row's first-column value (or its id); a
/// value that does not parse, or a link that matches nothing, is refused
/// unless `keep_as_typed`, when it is kept as typed (it shows #VALUE! or
/// #N/A until fixed).
pub fn parse_value(engine: &Engine<'_>, work: &Work, col: &ColumnSnap, value: &serde_json::Value, keep_as_typed: bool) -> Result<Option<Stored>, String> {
    let text = json_text(value)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match &col.ty {
        ColumnType::Formula { .. } => Err(format!(
            "{} is a formula column: its values are computed. Change its formula with update_column.",
            col.name
        )),
        ColumnType::RowLink { table } => {
            let Some(target) = work.snap().table(table) else {
                return Err(format!("{} links to a table that was deleted.", col.name));
            };
            let labels = row_labels(engine, work.home(), target);
            if let Some((id, _)) = labels.iter().find(|(id, _)| id == trimmed) {
                return Ok(Some(Stored::Row(id.clone())));
            }
            let hits: Vec<&String> = labels.iter().filter(|(_, l)| norm(l) == norm(trimmed)).map(|(id, _)| id).collect();
            match hits.as_slice() {
                [one] => Ok(Some(Stored::Row((*one).clone()))),
                [] if keep_as_typed => Ok(Some(Stored::Unparsed(trimmed.to_string()))),
                [] => Err(format!(
                    "No row of {} is called \"{trimmed}\", so {} can't link to it; its rows are {}. Add the row first, or pass keep_as_typed to keep the text (it shows #N/A).",
                    target.name,
                    col.name,
                    quoted_list(labels.iter().take(20).map(|(_, l)| l.clone()))
                )),
                several => Err(format!(
                    "{} rows of {} are called \"{trimmed}\"; give the row's id instead: {}.",
                    several.len(),
                    target.name,
                    several.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                )),
            }
        }
        ColumnType::NodeLink { under } => {
            if let Some(url) = pimble_core::PimbleUrl::parse(trimmed) {
                return Ok(Some(Stored::Node(NodeRef { store: url.store, node: url.node })));
            }
            let hits: Vec<NodeRef> = match under {
                Some(u) => work.book.children_of(u).into_iter().filter(|(_, t)| norm(t) == norm(trimmed)).map(|(n, _)| n).collect(),
                None => work.book.find_nodes(trimmed),
            };
            let place = match under.and_then(|u| work.book.node(&u)) {
                Some(info) => format!("under {}", info.title),
                None => "here".to_string(),
            };
            match hits.as_slice() {
                [one] => Ok(Some(Stored::Node(*one))),
                [] if keep_as_typed => Ok(Some(Stored::Unparsed(trimmed.to_string()))),
                [] => Err(format!(
                    "No node {place} is called \"{trimmed}\", so {} can't link to it. Create it first (create_node), give its pimble: link, or pass keep_as_typed to keep the text (it shows #N/A).",
                    col.name
                )),
                several => Err(format!(
                    "{} nodes {place} are called \"{trimmed}\"; give the one you mean as its pimble: link: {}.",
                    several.len(),
                    several.iter().map(|n| format!("pimble:{}/{}", n.store, n.node)).collect::<Vec<_>>().join(", ")
                )),
            }
        }
        ty => {
            let stored = input::parse_input(ty, trimmed, work.book.today_date(), &|_| None);
            match stored {
                Some(Stored::Unparsed(t)) if !keep_as_typed => Err(format!(
                    "\"{t}\" is not {}, so it can't go in {}; pass keep_as_typed to keep it as typed (it shows #VALUE!).",
                    what_it_takes(ty),
                    col.name
                )),
                other => Ok(other),
            }
        }
    }
}

/// The text a typed cell shows here: what typing it again would store.
pub fn typed_text(engine: &Engine<'_>, home: NodeRef, stored: &Stored, ty: &ColumnType) -> String {
    match (stored, ty) {
        (Stored::Number(n), ColumnType::Money) => formula::money(*n),
        _ => input::edit_text(stored, ty, &|s| match (s, ty) {
            (Stored::Row(id), ColumnType::RowLink { table }) => engine.row_label(&row_ref(home, table, id)),
            (Stored::Node(n), _) => engine.format_value(&Value::Node(*n), Format::Auto),
            (Stored::Row(id), _) => id.clone(),
            _ => String::new(),
        }),
    }
}

// ── Computing and checking ───────────────────────────────────────────────

fn compute(engine: &Engine<'_>, home: NodeRef, table: &str) -> Result<ComputedTable, String> {
    engine.compute_table(&home, table).map_err(|e| sentence(&format!("{}: {}", e.code.code(), e.message)))
}

/// A computed value as its cell shows it.
fn shown(engine: &Engine<'_>, v: &Value, f: Format) -> String {
    match v {
        Value::Error(e) => e.code.code().to_string(),
        v => engine.format_value(v, f),
    }
}

/// The label of a computed table's row: its first column, as shown.
fn computed_label(engine: &Engine<'_>, ct: &ComputedTable, i: usize) -> String {
    let row = &ct.rows[i];
    match (row.values.first(), ct.columns.first()) {
        (Some(Value::Blank), _) | (None, _) => format!("row {}", i + 1),
        (Some(v), Some(c)) => shown(engine, v, c.format),
        (Some(v), None) => shown(engine, v, Format::Auto),
    }
}

fn hint(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Spill => " For a list, make a summary (set_summary) instead.",
        ErrorCode::Circ => " It reads itself, through its own column or one that reads it.",
        ErrorCode::Name => " sheet_formula_guide lists the functions Pimble has.",
        ErrorCode::Error => " See sheet_formula_guide for the syntax.",
        _ => "",
    }
}

/// A formula column just written, checked: refused when a row gives a list
/// (#SPILL!), reads itself (#CIRC!), names an unknown function (#NAME?) or
/// does not parse; otherwise its first values.
pub fn check_formula_column(work: &Work, table: &str, column: &str) -> Result<String, String> {
    let engine = Engine::new(&*work.book);
    let ct = compute(&engine, work.home(), table)?;
    let Some(ci) = ct.columns.iter().position(|c| c.id.as_deref() == Some(column)) else {
        return Ok(String::new());
    };
    let name = ct.columns[ci].name.clone();
    for i in 0..ct.rows.len() {
        if let Value::Error(e) = &ct.rows[i].values[ci] {
            if matches!(e.code, ErrorCode::Spill | ErrorCode::Circ | ErrorCode::Name | ErrorCode::Error) {
                return Err(format!(
                    "Nothing was saved: in row \"{}\" the formula of {name} gives {}: {}{}",
                    computed_label(&engine, &ct, i),
                    e.code.code(),
                    sentence(&e.message),
                    hint(e.code)
                ));
            }
        }
    }
    Ok(preview_column(&engine, &ct, ci))
}

fn preview_column(engine: &Engine<'_>, ct: &ComputedTable, ci: usize) -> String {
    if ct.rows.is_empty() {
        return format!("{} has no rows yet, so nothing was computed.", ct.name);
    }
    let mut out = format!("{} in the first rows:\n", ct.columns[ci].name);
    for i in 0..ct.rows.len().min(PREVIEW_ROWS) {
        let v = &ct.rows[i].values[ci];
        let mut line = format!("- {}: {}", computed_label(engine, ct, i), shown(engine, v, ct.columns[ci].format));
        if let Value::Error(e) = v {
            line.push_str(&format!(" ({})", sentence(&e.message)));
        }
        out.push_str(&line);
        out.push('\n');
    }
    if ct.rows.len() > PREVIEW_ROWS {
        out.push_str(&format!("... and {} more rows.\n", ct.rows.len() - PREVIEW_ROWS));
    }
    out
}

// ── Showing ──────────────────────────────────────────────────────────────

fn md(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

/// One table in one line, for listings: kind, rows, columns with types.
pub fn table_summary(book: &McpBook, snap: &SheetsSnap, t: &TableSnap) -> String {
    let kind = if t.is_summary() { "summary" } else { "table" };
    let mut result: Vec<String> = Vec::new();
    let count = if t.is_summary() {
        // A summary's rows and first columns are its formula's result.
        let engine = Engine::new(book);
        match engine.compute_table(&book.home(), &t.id) {
            Ok(ct) if ct.error.is_none() => {
                result = ct.columns.iter().filter(|c| c.id.is_none()).map(|c| format!("{} (from the formula)", c.name)).collect();
                Some(ct.rows.len())
            }
            _ => None,
        }
    } else {
        Some(t.rows.len())
    };
    let rows = match count {
        Some(1) => ", 1 row".to_string(),
        Some(n) => format!(", {n} rows"),
        None => ", its formula fails".to_string(),
    };
    let cols: Vec<String> = result.into_iter().chain(t
        .columns
        .iter()
        .map(|c| {
            let ty = match &c.ty {
                ColumnType::Formula { format, .. } => format!("formula, {}", format.as_str()),
                ty => type_text(book, snap, Some(&t.id), ty),
            };
            format!("{} ({ty})", c.name)
        }))
        .collect();
    let cols = if cols.is_empty() { "no columns".to_string() } else { cols.join(", ") };
    format!("{} ({kind}{rows}): {cols}", t.name)
}

/// Rows to show: those whose cells match every `(column, value)` (the text
/// shown, case-insensitive, or the same number), and are among `rows` when
/// it is set, at most `limit`; with `totals`, a line summing each number
/// column over them.
#[derive(Default)]
pub struct Filter {
    pub equals: Vec<(String, String)>,
    pub limit: usize,
    pub rows: Option<std::collections::HashSet<String>>,
    pub totals: bool,
}

fn number_of(s: &str) -> Option<f64> {
    s.trim().replace(['$', ','], "").trim_end_matches('%').parse().ok()
}

fn matches(cell: &str, v: &Value, wanted: &str) -> bool {
    if norm(cell) == norm(wanted) {
        return true;
    }
    match (v.as_number(), number_of(wanted)) {
        (Some(a), Some(b)) => (a - b).abs() < 1e-9,
        _ => false,
    }
}

/// A table as Markdown: what it is, its columns (type, link target, formula,
/// format), and its rows with typed values as typed and computed values as
/// they show, each error listed with its sentence.
pub fn render_table(work: &Work, table: &str, filter: &Filter) -> Result<String, String> {
    let t = work.table(table)?;
    let home = work.home();
    let book: &McpBook = &*work.book;
    let engine = Engine::new(book);
    let ct = compute(&engine, home, table)?;
    let mut out = format!("### {}\n", t.name);
    out.push_str(&format!("table id: {}\n", t.id));
    if let Some(source) = &t.source {
        out.push_str(&format!("summary of: {}\n", formula::to_shown(book, Some(&t.id), source)));
        if let Some(e) = &ct.error {
            out.push_str(&format!("The summary's formula fails: {} {}\n", e.code.code(), sentence(&e.message)));
        }
    }
    out.push_str("columns:\n");
    let mut result_index = 0;
    for c in &ct.columns {
        match c.id.as_ref().and_then(|id| t.column(id)) {
            Some(col) => out.push_str(&format!("- {}: {}\n", col.name, type_text(book, work.snap(), Some(&t.id), &col.ty))),
            None => {
                let renamed = if t.headers.contains_key(&result_index) { ", renamed" } else { "" };
                out.push_str(&format!("- {}: from the summary's formula (result column {}{renamed})\n", c.name, result_index + 1));
                result_index += 1;
            }
        }
    }
    let mut wanted = Vec::new();
    for (name, value) in &filter.equals {
        let ci = ct.column_index(name).ok_or_else(|| {
            format!("{} has no column \"{name}\"; its columns are {}.", t.name, quoted_list(ct.columns.iter().map(|c| c.name.clone())))
        })?;
        wanted.push((ci, value.clone()));
    }
    if ct.columns.is_empty() {
        out.push_str("(no columns)\n");
        return Ok(out);
    }
    let with_ids = !t.is_summary();
    let mut header: Vec<String> = ct.columns.iter().map(|c| md(&c.name)).collect();
    if with_ids {
        header.push("row id".into());
    }
    let mut lines = vec![format!("| {} |", header.join(" | ")), format!("|{}", " --- |".repeat(header.len()))];
    let mut errors = Vec::new();
    let mut matched = 0;
    let mut sums: BTreeMap<usize, f64> = BTreeMap::new();
    for (i, row) in ct.rows.iter().enumerate() {
        let stored_row = t.row(&row.id);
        let mut cells = Vec::with_capacity(ct.columns.len());
        for (ci, c) in ct.columns.iter().enumerate() {
            let v = &row.values[ci];
            let own = c.id.as_ref().and_then(|id| t.column(id));
            let typed = match (own, stored_row) {
                (Some(col), Some(r)) if !c.computed => r.cells.get(&col.id).map(|s| (s, &col.ty)),
                _ => None,
            };
            let text = match (typed, v) {
                (Some((Stored::Unparsed(s), _)), _) => {
                    format!("{} (typed \"{s}\")", if let Value::Error(e) = v { e.code.code() } else { "#VALUE!" })
                }
                (Some((s, ty)), _) => typed_text(&engine, home, s, ty),
                (None, v) => shown(&engine, v, c.format),
            };
            cells.push((text, v));
        }
        if !wanted.iter().all(|(ci, value)| matches(&cells[*ci].0, cells[*ci].1, value)) {
            continue;
        }
        if filter.rows.as_ref().is_some_and(|only| !only.contains(&row.id)) {
            continue;
        }
        for (ci, (_, v)) in cells.iter().enumerate() {
            if let Value::Number(n) = v {
                *sums.entry(ci).or_insert(0.0) += *n;
            }
        }
        matched += 1;
        if matched > filter.limit {
            continue;
        }
        for (ci, (_, v)) in cells.iter().enumerate() {
            if let Value::Error(e) = v {
                errors.push(format!(
                    "- row \"{}\", {}: {} {}",
                    computed_label(&engine, &ct, i),
                    ct.columns[ci].name,
                    e.code.code(),
                    sentence(&e.message)
                ));
            }
        }
        let mut texts: Vec<String> = cells.into_iter().map(|(t, _)| md(&t)).collect();
        if with_ids {
            texts.push(row.id.clone());
        }
        lines.push(format!("| {} |", texts.join(" | ")));
    }
    if ct.rows.is_empty() {
        out.push_str("(no rows)\n");
    } else if matched == 0 {
        out.push_str(&format!("No row of {} matches the filter.\n", t.name));
    } else {
        out.push_str(&lines.join("\n"));
        out.push('\n');
        if filter.totals && !sums.is_empty() {
            let parts: Vec<String> = sums
                .iter()
                .filter(|(ci, _)| matches!(ct.columns[**ci].format, Format::Number | Format::Money | Format::Hours | Format::Auto))
                .map(|(ci, n)| format!("{} {}", ct.columns[*ci].name, shown(&engine, &Value::Number(*n), ct.columns[*ci].format)))
                .collect();
            if !parts.is_empty() {
                out.push_str(&format!("totals: {}\n", parts.join(", ")));
            }
        }
        if matched > filter.limit {
            out.push_str(&format!("(showing {} of {matched} rows; raise limit or filter to see more)\n", filter.limit));
        }
    }
    if !errors.is_empty() {
        out.push_str("errors:\n");
        out.push_str(&errors.join("\n"));
        out.push('\n');
    }
    Ok(out)
}

/// Every table of the node, rendered.
pub fn render_all(work: &Work, limit: usize) -> String {
    let ids: Vec<String> = work.snap().tables.iter().map(|t| t.id.clone()).collect();
    let mut out = String::new();
    let duplicates = duplicate_names(work.snap());
    for name in duplicates {
        out.push_str(&format!("Two tables here are called {name}; rename one.\n"));
    }
    for id in ids {
        match render_table(work, &id, &Filter { equals: Vec::new(), limit, ..Default::default() }) {
            Ok(s) => out.push_str(&s),
            Err(e) => out.push_str(&format!("(a table could not be shown: {e})\n")),
        }
        out.push('\n');
    }
    out
}

fn duplicate_names(snap: &SheetsSnap) -> Vec<String> {
    let mut seen: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for t in &snap.tables {
        seen.entry(norm(&t.name)).or_insert((t.name.clone(), 0)).1 += 1;
    }
    seen.into_values().filter(|(_, n)| *n > 1).map(|(name, _)| name).collect()
}

/// A one-off evaluation (contract section 10, `evaluate`), as text.
pub fn evaluate(work: &Work, table: Option<&str>, row: Option<&str>, shown_formula: &str) -> Result<String, String> {
    let home = work.home();
    let (table_id, row_id) = match table {
        Some(name) => {
            let t = find_table(work.snap(), name)?.clone();
            let row_id = match row {
                Some(r) => {
                    let engine = Engine::new(&*work.book);
                    Some(find_row(&row_labels(&engine, home, &t), &t.name, r)?)
                }
                None => None,
            };
            (Some(t.id), row_id)
        }
        None if row.is_some() => return Err("Name the table the row is in.".into()),
        None => (None, None),
    };
    let engine = Engine::new(&*work.book);
    let result = engine
        .evaluate(&home, table_id.as_deref(), row_id.as_deref(), shown_formula)
        .map_err(|e| name_error(&e))?;
    Ok(match result {
        Evaluated::Value(Value::Error(e)) => format!("{} {}{}", e.code.code(), sentence(&e.message), hint(e.code)),
        Evaluated::Value(v) => {
            let text = engine.format_value(&v, Format::Auto);
            match &v {
                Value::Number(x) if text != formula::general_number(*x) => format!("{text} ({})", formula::general_number(*x)),
                Value::Text(s) => format!("\"{s}\""),
                _ => text,
            }
        }
        Evaluated::Table(rt) => {
            let mut out = format!("A list of {} rows:\n", rt.rows.len());
            out.push_str(&format!("| {} |\n", rt.columns.iter().map(|c| md(&c.name)).collect::<Vec<_>>().join(" | ")));
            out.push_str(&format!("|{}\n", " --- |".repeat(rt.columns.len().max(1))));
            for r in rt.rows.iter().take(200) {
                let cells: Vec<String> =
                    r.iter().zip(&rt.columns).map(|(v, c)| md(&shown(&engine, v, c.format))).collect();
                out.push_str(&format!("| {} |\n", cells.join(" | ")));
            }
            if rt.rows.len() > 200 {
                out.push_str(&format!("... and {} more rows.\n", rt.rows.len() - 200));
            }
            out
        }
    })
}

// ── Tables ───────────────────────────────────────────────────────────────

fn wants_formula(spec: &ColumnSpec) -> bool {
    spec.formula.is_some() || spec.ty.as_deref().is_some_and(|k| k.trim().eq_ignore_ascii_case("formula"))
}

/// A new table with its columns and rows, in one edit; every formula is
/// checked and nothing is written when one is refused. Answers its id.
pub fn create_table(work: &mut Work, name: &str, columns: &[ColumnSpec], rows: &[RowValues], keep_as_typed: bool, after: Option<&str>) -> Result<String, String> {
    let clean = doc::table_name_from(name);
    let placement = match after {
        Some(a) => Placement::After(find_table(work.snap(), a)?.id.clone()),
        None => Placement::Last,
    };
    let table = work.write(|t| t.create_table(&clean, placement))?;
    // Every column first (formula columns as placeholders), so formulas can
    // name any of them, then the formulas in the order given.
    let mut ids = Vec::new();
    for spec in columns {
        let ty = if wants_formula(spec) { ColumnType::Text } else { column_type(work, Some(&table), spec, None)? };
        let id = work.write(|t| t.add_column(&table, &spec.name, ty, Placement::Last))?;
        ids.push(id);
    }
    for (spec, id) in columns.iter().zip(&ids) {
        if wants_formula(spec) {
            let ty = column_type(work, Some(&table), spec, None)?;
            work.write(|t| t.set_column_type(&table, id, ty))?;
        }
    }
    if !rows.is_empty() {
        add_rows(work, &table, rows, keep_as_typed, None)?;
    }
    for (spec, id) in columns.iter().zip(&ids) {
        if matches!(work.snap().table(&table).and_then(|t| t.column(id)).map(|c| &c.ty), Some(ColumnType::Formula { .. })) {
            check_formula_column(work, &table, id).map_err(|e| format!("{e} (column {})", spec.name))?;
        }
    }
    Ok(table)
}

/// Formulas of this note that read `needle` (a `{t:…}` or `{c:…}` token).
fn readers(snap: &SheetsSnap, needle: &str, except_table: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    for t in &snap.tables {
        if Some(t.id.as_str()) == except_table {
            continue;
        }
        if t.source.as_deref().is_some_and(|s| s.contains(needle)) {
            out.push(format!("{}'s summary formula", t.name));
        }
        for c in &t.columns {
            if let ColumnType::Formula { formula, .. } = &c.ty {
                if formula.contains(needle) {
                    out.push(format!("{}[{}]", t.name, c.name));
                }
            }
        }
    }
    out
}

pub fn rename_table(work: &mut Work, table: &str, new_name: &str) -> Result<String, String> {
    let t = find_table(work.snap(), table)?.clone();
    let clean = doc::table_name_from(new_name);
    work.write(|txn| txn.rename_table(&t.id, &clean))?;
    Ok(format!("Renamed the table {} to {clean}; formulas that read it follow the new name.", t.name))
}

pub fn delete_table(work: &mut Work, table: &str) -> Result<String, String> {
    let t = find_table(work.snap(), table)?.clone();
    let reading = readers(work.snap(), &format!("{{t:{}}}", t.id), Some(&t.id));
    let links: Vec<String> = work
        .snap()
        .tables
        .iter()
        .flat_map(|o| o.columns.iter().filter(|c| matches!(&c.ty, ColumnType::RowLink { table } if *table == t.id)).map(move |c| format!("{}[{}]", o.name, c.name)))
        .collect();
    work.write(|txn| txn.delete_table(&t.id))?;
    let mut out = format!("Deleted the table {}.", t.name);
    if !reading.is_empty() {
        out.push_str(&format!(" These formulas read it and now show #REF!: {}.", reading.join(", ")));
    }
    if !links.is_empty() {
        out.push_str(&format!(" These link columns pointed at it: {}.", links.join(", ")));
    }
    Ok(out)
}

/// A summary's formula and the names of its result columns (by position;
/// an empty name takes back a rename). `table` is an existing summary, an
/// empty table, or the name of a new one.
pub fn set_summary(work: &mut Work, table: &str, shown: &str, columns: Option<&[String]>) -> Result<String, String> {
    if shown.trim().trim_start_matches('=').trim().is_empty() {
        return Err("Write the summary's formula, starting with =.".into());
    }
    let (id, made) = match find_table(work.snap(), table) {
        Ok(t) if t.is_summary() || (t.columns.is_empty() && t.rows.is_empty()) => (t.id.clone(), false),
        Ok(t) => {
            return Err(format!(
                "{} is a plain table with columns of its own; name a summary, or a new table name to make one.",
                t.name
            ))
        }
        Err(_) if work.snap().tables.iter().all(|t| norm(&t.name) != norm(table)) => {
            let clean = doc::table_name_from(table);
            (work.write(|t| t.create_table(&clean, Placement::Last))?, true)
        }
        Err(e) => return Err(e),
    };
    let stored = formula::to_stored(&*work.book, Some(&id), shown.trim()).map_err(|e| name_error(&e))?;
    work.write(|t| t.set_source(&id, Some(&stored)))?;
    let (width, error) = {
        let engine = Engine::new(&*work.book);
        let ct = compute(&engine, work.home(), &id)?;
        (ct.columns.iter().filter(|c| c.id.is_none()).count(), ct.error.clone())
    };
    if let Some(e) = &error {
        if matches!(e.code, ErrorCode::Name | ErrorCode::Circ | ErrorCode::Error | ErrorCode::Ref | ErrorCode::Spill) {
            return Err(format!("Nothing was saved: the summary's formula gives {} {}{}", e.code.code(), sentence(&e.message), hint(e.code)));
        }
    }
    if let Some(names) = columns {
        if names.len() > width {
            return Err(format!(
                "The summary's formula gives {width} column(s), so {} names are too many.",
                names.len()
            ));
        }
        for (i, name) in names.iter().enumerate() {
            let name = doc::column_name_from(name);
            work.write(|t| t.set_header(&id, i, if name.is_empty() { None } else { Some(&name) }))?;
        }
    }
    let name = work.table(&id)?.name;
    let mut out = format!("{} the summary {name}.\n", if made { "Made" } else { "Set" });
    if let Some(e) = error {
        out.push_str(&format!("It computes to {} {}\n", e.code.code(), sentence(&e.message)));
    }
    out.push_str(&render_table(work, &id, &Filter { equals: Vec::new(), limit: PREVIEW_ROWS, ..Default::default() })?);
    Ok(out)
}

// ── Columns ──────────────────────────────────────────────────────────────

/// A new column; a formula column is checked, and the answer carries its
/// first values.
pub fn add_column(work: &mut Work, table: &str, spec: &ColumnSpec) -> Result<String, String> {
    let t = find_table(work.snap(), table)?.clone();
    if t.is_summary() && !wants_formula(spec) {
        return Err(format!("{} is a summary: its own columns are formula columns, which read the result's columns with [@column].", t.name));
    }
    if !wants_formula(spec) {
        let ty = column_type(work, Some(&t.id), spec, None)?;
        let id = work.write(|txn| txn.add_column(&t.id, &spec.name, ty, Placement::Last))?;
        let name = work.table(&t.id)?.column(&id).map(|c| c.name.clone()).unwrap_or_default();
        return Ok(format!("Added the column {name} to {}.", t.name));
    }
    // A formula that names only columns there are is checked as it is; one
    // that names the new column itself is given it first, so that it is
    // #CIRC! rather than an unknown name.
    let before = column_type(work, Some(&t.id), spec, None);
    let id = work.write(|txn| txn.add_column(&t.id, &spec.name, ColumnType::Text, Placement::Last))?;
    let ty = match before {
        Ok(ty) => ty,
        Err(first) => column_type(work, Some(&t.id), spec, None).map_err(|_| first)?,
    };
    work.write(|txn| txn.set_column_type(&t.id, &id, ty))?;
    let preview = check_formula_column(work, &t.id, &id)?;
    let name = work.table(&t.id)?.column(&id).map(|c| c.name.clone()).unwrap_or_default();
    Ok(format!("Added the formula column {name} to {}.\n{preview}", t.name))
}

/// Each row's value carried over to a new type as the text that would have
/// been typed (a formula column's: its computed values), as the app does.
fn carry_over(work: &Work, t: &TableSnap, old: &ColumnSnap, ty: &ColumnType) -> Vec<(String, Option<Stored>)> {
    let engine = Engine::new(&*work.book);
    let home = work.home();
    let ct = engine.compute_table(&home, &t.id).ok();
    let ci = ct.as_ref().and_then(|ct| ct.columns.iter().position(|c| c.id.as_deref() == Some(old.id.as_str())));
    let labels = match ty {
        ColumnType::RowLink { table } => work.snap().table(table).map(|tt| row_labels(&engine, home, tt)).unwrap_or_default(),
        _ => Vec::new(),
    };
    t.rows
        .iter()
        .map(|r| {
            let text = match (&old.ty, r.cells.get(&old.id)) {
                (ColumnType::Formula { .. }, _) | (ColumnType::RowLink { .. } | ColumnType::NodeLink { .. }, Some(_)) => ct
                    .as_ref()
                    .and_then(|ct| ct.rows.iter().find(|x| x.id == r.id).zip(ci))
                    .and_then(|(x, i)| match &x.values[i] {
                        Value::Error(_) | Value::Blank => None,
                        v => Some(engine.format_value(v, ct.as_ref().map(|c| c.columns[i].format).unwrap_or_default())),
                    })
                    .map(|s| s.replace(['$', ','], ""))
                    .unwrap_or_default(),
                (_, Some(stored)) => input::edit_text(stored, &old.ty, &|_| String::new()),
                (_, None) => String::new(),
            };
            let value = input::parse_input(ty, &text, work.book.today_date(), &|label: &str| {
                labels.iter().find(|(_, l)| norm(l) == norm(label)).map(|(id, _)| Stored::Row(id.clone()))
            });
            (r.id.clone(), value)
        })
        .collect()
}

/// Rename a column, change its type (values carried over as typed), its
/// link target, formula or format.
pub fn update_column(work: &mut Work, table: &str, column: &str, new_name: Option<&str>, spec: &ColumnSpec) -> Result<String, String> {
    let t = find_table(work.snap(), table)?.clone();
    let old = find_column(&t, column)?.clone();
    let changes_type = spec.ty.is_some() || spec.target.is_some() || spec.formula.is_some() || spec.format.is_some();
    let ty = if changes_type { column_type(work, Some(&t.id), spec, Some(&old.ty))? } else { old.ty.clone() };
    if t.is_summary() && !matches!(ty, ColumnType::Formula { .. }) {
        return Err(format!("{} is a summary: its own columns are formula columns.", t.name));
    }
    if matches!(old.ty, ColumnType::Formula { .. }) && matches!(spec.format, Some(_)) && !matches!(ty, ColumnType::Formula { .. }) {
        return Err("A format is for formula columns.".into());
    }
    let retyped = ty != old.ty;
    let carried = if (retyped && !matches!(ty, ColumnType::Formula { .. }) && ty.kind() != old.ty.kind()) || matches!((&ty, &old.ty), (ColumnType::RowLink { table: a }, ColumnType::RowLink { table: b }) if a != b) {
        carry_over(work, &t, &old, &ty)
    } else {
        Vec::new()
    };
    let rename = new_name.map(str::trim).filter(|n| !n.is_empty() && *n != old.name).map(str::to_string);
    if rename.is_none() && !retyped {
        return Err("That would change nothing, so nothing was written.".into());
    }
    work.write(|txn| {
        if let Some(n) = &rename {
            txn.rename_column(&t.id, &old.id, n)?;
        }
        if retyped {
            txn.set_column_type(&t.id, &old.id, ty.clone())?;
            if matches!(ty, ColumnType::Formula { .. }) {
                for r in &t.rows {
                    if r.cells.contains_key(&old.id) {
                        txn.set_cell(&t.id, &r.id, &old.id, None)?;
                    }
                }
            }
            for (row, value) in &carried {
                if t.row(row).and_then(|r| r.cells.get(&old.id)) != value.as_ref() {
                    txn.set_cell(&t.id, row, &old.id, value.as_ref())?;
                }
            }
        }
        Ok(())
    })?;
    let name = work.table(&t.id)?.column(&old.id).map(|c| c.name.clone()).unwrap_or_default();
    let mut out = format!("Changed the column {name} of {}.", t.name);
    if !carried.is_empty() {
        let unparsed = carried.iter().filter(|(_, v)| matches!(v, Some(Stored::Unparsed(_)))).count();
        out.push_str(" Its values were carried over as typed");
        if unparsed > 0 {
            out.push_str(&format!("; {unparsed} did not fit the new type and show #VALUE! (or #N/A) until fixed"));
        }
        out.push('.');
    }
    if matches!(ty, ColumnType::Formula { .. }) {
        let preview = check_formula_column(work, &t.id, &old.id)?;
        out.push('\n');
        out.push_str(&preview);
    }
    Ok(out)
}

pub fn delete_column(work: &mut Work, table: &str, column: &str) -> Result<String, String> {
    let t = find_table(work.snap(), table)?.clone();
    let c = find_column(&t, column)?.clone();
    let reading = readers(work.snap(), &format!("{{c:{}}}", c.id), None);
    work.write(|txn| txn.delete_column(&t.id, &c.id))?;
    let mut out = format!("Deleted the column {} of {}.", c.name, t.name);
    if !reading.is_empty() {
        out.push_str(&format!(" These formulas read it and now show #REF!: {}.", reading.join(", ")));
    }
    Ok(out)
}

// ── Rows ─────────────────────────────────────────────────────────────────

fn plain(t: &TableSnap) -> Result<(), String> {
    if t.is_summary() {
        Err(format!("{} is a summary: its rows come from its formula and can't be typed.", t.name))
    } else {
        Ok(())
    }
}

/// Values as typed, parsed by each column's type.
fn parse_row(engine: &Engine<'_>, work: &Work, t: &TableSnap, values: &RowValues, keep_as_typed: bool) -> Result<Vec<(String, Option<Stored>)>, String> {
    let mut out = Vec::new();
    for (name, value) in values {
        let col = find_column(t, name)?;
        out.push((col.id.clone(), parse_value(engine, work, col, value, keep_as_typed)?));
    }
    Ok(out)
}

/// New rows, values by column name as typed. Answers their ids.
pub fn add_rows(work: &mut Work, table: &str, rows: &[RowValues], keep_as_typed: bool, after: Option<&str>) -> Result<Vec<String>, String> {
    if rows.is_empty() {
        return Err("Give at least one row.".into());
    }
    let t = find_table(work.snap(), table)?.clone();
    plain(&t)?;
    let (parsed, placement) = {
        let engine = Engine::new(&*work.book);
        let mut parsed = Vec::new();
        for values in rows {
            let cells = parse_row(&engine, work, &t, values, keep_as_typed)?;
            parsed.push(cells.into_iter().filter_map(|(c, v)| v.map(|v| (c, v))).collect::<Vec<_>>());
        }
        let placement = match after {
            Some(a) => Placement::After(find_row(&row_labels(&engine, work.home(), &t), &t.name, a)?),
            None => Placement::Last,
        };
        (parsed, placement)
    };
    work.write(|txn| txn.add_rows(&t.id, placement, &parsed))
}

/// Rows changed, cell by cell: a cell whose value stays the same is not
/// written. Rows are found before anything changes, so a row renamed by
/// one update is still found by its old name in the next.
pub fn update_rows(work: &mut Work, table: &str, updates: &[RowUpdate], keep_as_typed: bool) -> Result<usize, String> {
    if updates.is_empty() {
        return Err("Give at least one row to change.".into());
    }
    let t = find_table(work.snap(), table)?.clone();
    plain(&t)?;
    let mut edits = Vec::new();
    {
        let engine = Engine::new(&*work.book);
        let labels = row_labels(&engine, work.home(), &t);
        for u in updates {
            let row = find_row(&labels, &t.name, &u.row)?;
            for (col, value) in parse_row(&engine, work, &t, &u.values, keep_as_typed)? {
                if t.row(&row).and_then(|r| r.cells.get(&col)) != value.as_ref() {
                    edits.push((row.clone(), col, value));
                }
            }
        }
    }
    if edits.is_empty() {
        return Err("Those rows already hold those values, so nothing was written.".into());
    }
    let n = edits.len();
    work.write(|txn| {
        for (row, col, value) in &edits {
            txn.set_cell(&t.id, row, col, value.as_ref())?;
        }
        Ok(())
    })?;
    Ok(n)
}

pub fn delete_rows(work: &mut Work, table: &str, rows: &[String]) -> Result<usize, String> {
    if rows.is_empty() {
        return Err("Name at least one row to delete.".into());
    }
    let t = find_table(work.snap(), table)?.clone();
    plain(&t)?;
    let ids = {
        let engine = Engine::new(&*work.book);
        let labels = row_labels(&engine, work.home(), &t);
        let mut ids = Vec::new();
        for r in rows {
            let id = find_row(&labels, &t.name, r)?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    };
    work.write(|txn| {
        for id in &ids {
            txn.delete_row(&t.id, id)?;
        }
        Ok(())
    })?;
    Ok(ids.len())
}

/// The update that makes a new sheet node's first table: written in the
/// edit that creates the node, as the app's "New Sheet" does.
pub fn new_sheet_content(name: &str, columns: &[ColumnSpec], rows: &[RowValues], keep_as_typed: bool, home: NodeRef, today: chrono::NaiveDate) -> Result<Vec<u8>, String> {
    let sheets = Sheets::standalone();
    let mut book = McpBook::new(home, SheetsSnap::default(), today);
    let mut work = Work::new(sheets.clone(), &mut book);
    let defaults = [
        ColumnSpec { name: "name".into(), ..Default::default() },
        ColumnSpec { name: "amount".into(), ty: Some("number".into()), ..Default::default() },
    ];
    let columns = if columns.is_empty() { &defaults[..] } else { columns };
    create_table(&mut work, name, columns, rows, keep_as_typed, None)?;
    Ok(sheets.diff_since(&yrs::StateVector::default()))
}

#[cfg(test)]
mod tests;
