//! Sheet operations by name (docs/API_CONTRACT.md decision 8): what the app,
//! the MCP and plugins all do to a node's tables, written once. Naming a table,
//! column or row as a person writes it, a column's type from what was asked for,
//! a value as typed with the refusal when it does not fit, a formula column
//! checked before it is kept, values carried over when a column changes type,
//! and the rules for moving a table to another node.
//!
//! Everything here is pure: it reads a [`SheetsSnap`] and a [`Book`], or
//! writes through a [`SheetsTxn`], and refuses with a sentence a person reads.
//! How a caller says what it offers next (a tool's parameter, a menu item) is
//! the caller's: those words come in as closures (`name_error`, `hint`).

use chrono::NaiveDate;

use crate::doc::{self, Placement, Sheets, SheetsTxn};
use crate::formula::{self, ComputedTable, Engine, ErrorCode, NameError, RowRef, Value};
use crate::input;
use crate::{Book, ColumnSnap, ColumnType, Format, NodeRef, SheetsSnap, Stored, TableSnap};

/// A new sheet node's icon (a Tabler icon name).
pub const SHEET_ICON: &str = "table";

/// A new sheet node's first table, when nobody names it.
pub const FIRST_TABLE: &str = "Table1";

/// A new sheet node's first table's columns, when nobody gives any.
pub const FIRST_COLUMNS: [(&str, ColumnType); 2] = [("name", ColumnType::Text), ("amount", ColumnType::Number)];

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

/// A name as names are compared: whitespace runs as one space, any case.
pub fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Names quoted and joined with commas, or "none".
pub fn quoted_list(names: impl IntoIterator<Item = String>) -> String {
    let all: Vec<String> = names.into_iter().map(|n| format!("\"{n}\"")).collect();
    if all.is_empty() {
        "none".to_string()
    } else {
        all.join(", ")
    }
}

// ── Finding tables, columns, rows and nodes by name ──────────────────────

/// A table of this note by name (case-insensitive) or id.
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

/// The node a name means: a `pimble:` link, or a title or path one node
/// answers to (a node link column's `under`).
pub fn find_node(book: &dyn Book, name: &str) -> Result<NodeRef, String> {
    let name = name.trim().trim_matches('\'');
    if let Some(url) = pimble_core::PimbleUrl::parse(name) {
        return Ok(NodeRef { store: url.store, node: url.node });
    }
    match book.find_nodes(name).as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("No node here is called {name}.")),
        many => Err(format!(
            "Several nodes are called {name}: {}. Write the one you mean as its path.",
            many.iter().map(|n| book.display_name(n)).collect::<Vec<_>>().join(", ")
        )),
    }
}

// ── Column types ─────────────────────────────────────────────────────────

/// A formula column's format by the word for it.
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

/// What a column's type is asked to be, as words: what a person picked in a
/// form, or what a tool was given.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColumnSpec<'a> {
    /// text, number, money, date, hours, link (row_link), node_link or
    /// formula. Left out: formula when `formula` is given, otherwise the
    /// current type (text for a new column).
    pub kind: Option<&'a str>,
    /// A link column's table, by name or id.
    pub target: Option<&'a str>,
    /// A node link column's parent: a title, a path or a `pimble:` link. Empty
    /// means any node.
    pub under: Option<&'a str>,
    /// A formula column's formula as typed, starting with `=`.
    pub formula: Option<&'a str>,
    /// A formula column's format, by its word.
    pub format: Option<&'a str>,
}

impl ColumnSpec<'_> {
    /// Whether this asks for a formula column.
    pub fn wants_formula(&self) -> bool {
        self.formula.is_some() || self.kind.is_some_and(|k| k.trim().eq_ignore_ascii_case("formula"))
    }
}

/// The type a column spec asks for. `current` is the column's type when it
/// is being changed: what the spec leaves out is kept from it. `table` is
/// the column's table, which a formula's bare names read. `name_error` says a
/// formula's unknown names as the caller says them.
pub fn column_type(
    book: &dyn Book,
    snap: &SheetsSnap,
    table: Option<&str>,
    spec: &ColumnSpec<'_>,
    current: Option<&ColumnType>,
    name_error: &dyn Fn(&NameError) -> String,
) -> Result<ColumnType, String> {
    let kind = match spec.kind.map(|t| t.trim().to_lowercase()) {
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
            let target = match (spec.target.filter(|t| !t.trim().is_empty()), current) {
                (Some(t), _) => {
                    let t = find_table(snap, t)?;
                    if t.is_summary() {
                        return Err(format!("A link column links to rows of a plain table; {} is a summary.", t.name));
                    }
                    t.id.clone()
                }
                (None, Some(ColumnType::RowLink { table })) => table.clone(),
                (None, _) => return Err("Pick a table to link to.".into()),
            };
            ColumnType::RowLink { table: target }
        }
        "node_link" | "node link" | "node" => {
            let under = match (spec.under, current) {
                (Some(u), _) if u.trim().trim_matches('\'').is_empty() => None,
                (Some(u), _) => Some(find_node(book, u)?),
                (None, Some(ColumnType::NodeLink { under })) => *under,
                (None, _) => None,
            };
            ColumnType::NodeLink { under }
        }
        "formula" => {
            let format = match (spec.format, current) {
                (Some(f), _) => parse_format(f)?,
                (None, Some(ColumnType::Formula { format, .. })) => *format,
                (None, Some(other)) => other.format(),
                (None, None) => Format::Auto,
            };
            let stored = match (spec.formula, current) {
                (Some(shown), _) => {
                    let shown = shown.trim();
                    if shown.trim_start_matches('=').trim().is_empty() {
                        return Err("Write the formula, starting with =.".into());
                    }
                    formula::to_stored(book, table, shown).map_err(|e| name_error(&e))?
                }
                (None, Some(ColumnType::Formula { formula, .. })) => formula.clone(),
                (None, _) => return Err("Write the formula, starting with =.".into()),
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

/// What a column of this type takes, for a refusal.
pub fn what_it_takes(ty: &ColumnType) -> &'static str {
    match ty {
        ColumnType::Number => "a number",
        ColumnType::Money => "an amount like $12.50",
        ColumnType::Date => "a date (2026-10-14, Oct 14, 10/14, today or yesterday)",
        ColumnType::Hours => "hours (1:30, 1.5, 90m or 1h 30m)",
        _ => "a value",
    }
}

/// What a value is typed into: the note's tables and the book, the nodes a
/// node link may choose from under a parent (titled), and today.
pub struct Typing<'a> {
    pub engine: &'a Engine<'a>,
    pub book: &'a dyn Book,
    pub snap: &'a SheetsSnap,
    pub children: &'a dyn Fn(&NodeRef) -> Vec<(NodeRef, String)>,
    pub today: NaiveDate,
}

/// What typing `text` into `col` stores (`None` is a blank cell). A link
/// names a row of its table by the row's first-column value (or its id); a
/// value that does not parse, or a link that matches nothing, is refused
/// unless `keep_as_typed`, when it is kept as typed (it shows #VALUE! or
/// #N/A until fixed).
pub fn parse_value(at: &Typing<'_>, col: &ColumnSnap, text: &str, keep_as_typed: bool) -> Result<Option<Stored>, String> {
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
            let Some(target) = at.snap.table(table) else {
                return Err(format!("{} links to a table that was deleted.", col.name));
            };
            let labels = row_labels(at.engine, at.book.home(), target);
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
                Some(u) => (at.children)(u).into_iter().filter(|(_, t)| norm(t) == norm(trimmed)).map(|(n, _)| n).collect(),
                None => at.book.find_nodes(trimmed),
            };
            let place = match under.and_then(|u| at.book.node(&u)) {
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
            let stored = input::parse_input(ty, trimmed, at.today, &|_| None);
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

/// A table computed, or why it cannot be, as a sentence.
pub fn compute(engine: &Engine<'_>, home: NodeRef, table: &str) -> Result<ComputedTable, String> {
    engine.compute_table(&home, table).map_err(|e| sentence(&format!("{}: {}", e.code.code(), e.message)))
}

/// A computed value as its cell shows it.
pub fn shown(engine: &Engine<'_>, v: &Value, f: Format) -> String {
    match v {
        Value::Error(e) => e.code.code().to_string(),
        v => engine.format_value(v, f),
    }
}

/// The label of a computed table's row: its first column, as shown.
pub fn computed_label(engine: &Engine<'_>, ct: &ComputedTable, i: usize) -> String {
    let row = &ct.rows[i];
    match (row.values.first(), ct.columns.first()) {
        (Some(Value::Blank), _) | (None, _) => format!("row {}", i + 1),
        (Some(v), Some(c)) => shown(engine, v, c.format),
        (Some(v), None) => shown(engine, v, Format::Auto),
    }
}

/// A formula column just written, checked: refused when a row gives a list
/// (#SPILL!), reads itself (#CIRC!), names an unknown function (#NAME?) or
/// does not parse, with `hint` adding what to do about that error; otherwise
/// the table computed and the column's index in it (`None` when the column
/// is not in it).
pub fn check_formula_column(
    engine: &Engine<'_>,
    home: NodeRef,
    table: &str,
    column: &str,
    hint: &dyn Fn(ErrorCode) -> &'static str,
) -> Result<Option<(ComputedTable, usize)>, String> {
    let ct = compute(engine, home, table)?;
    let Some(ci) = ct.columns.iter().position(|c| c.id.as_deref() == Some(column)) else {
        return Ok(None);
    };
    let name = &ct.columns[ci].name;
    for i in 0..ct.rows.len() {
        if let Value::Error(e) = &ct.rows[i].values[ci] {
            if matches!(e.code, ErrorCode::Spill | ErrorCode::Circ | ErrorCode::Name | ErrorCode::Error) {
                return Err(format!(
                    "Nothing was saved: in row \"{}\" the formula of {name} gives {}: {}{}",
                    computed_label(engine, &ct, i),
                    e.code.code(),
                    sentence(&e.message),
                    hint(e.code)
                ));
            }
        }
    }
    Ok(Some((ct, ci)))
}

// ── Changing a column's type ─────────────────────────────────────────────

/// Whether changing a column from `old` to `new` carries its values over:
/// a change of kind to anything but a formula (a formula computes its own),
/// or a link column pointed at another table. A formula's format, or a
/// node link's parent, changes nothing stored.
pub fn carries_over(old: &ColumnType, new: &ColumnType) -> bool {
    let retyped = new != old && !matches!(new, ColumnType::Formula { .. }) && new.kind() != old.kind();
    let relinked = matches!((new, old), (ColumnType::RowLink { table: a }, ColumnType::RowLink { table: b }) if a != b);
    retyped || relinked
}

/// Each row's value of column `old` carried over to type `ty`, as the text
/// that would have been typed: a formula or link column's as it shows
/// (`shown`, by row id; `$` and `,` dropped so amounts read as numbers),
/// another's as it is edited. A link in the new type names one of `choices`
/// (label and what it stores) by its label; what does not fit is kept as
/// typed and shows #VALUE! or #N/A until fixed.
pub fn carry_over(
    t: &TableSnap,
    old: &ColumnSnap,
    ty: &ColumnType,
    shown: &dyn Fn(&str) -> Option<String>,
    choices: &[(String, Stored)],
    today: NaiveDate,
) -> Vec<(String, Option<Stored>)> {
    t.rows
        .iter()
        .map(|r| {
            let text = match (&old.ty, r.cells.get(&old.id)) {
                (ColumnType::Formula { .. }, _) | (ColumnType::RowLink { .. } | ColumnType::NodeLink { .. }, Some(_)) => {
                    shown(&r.id).map(|s| s.replace(['$', ','], "")).unwrap_or_default()
                }
                (_, Some(stored)) => input::edit_text(stored, &old.ty, &|_| String::new()),
                (_, None) => String::new(),
            };
            let value = input::parse_input(ty, &text, today, &|label: &str| {
                choices.iter().find(|(l, _)| norm(l) == norm(label)).map(|(_, v)| v.clone())
            });
            (r.id.clone(), value)
        })
        .collect()
}

/// A computed column's values as they show, by row id, for [`carry_over`]:
/// errors and blanks are nothing.
pub fn shown_values(engine: &Engine<'_>, home: NodeRef, table: &str, column: &str) -> Vec<(String, String)> {
    let Ok(ct) = engine.compute_table(&home, table) else { return Vec::new() };
    let Some(ci) = ct.columns.iter().position(|c| c.id.as_deref() == Some(column)) else { return Vec::new() };
    let format = ct.columns[ci].format;
    ct.rows
        .iter()
        .filter_map(|row| match &row.values[ci] {
            Value::Error(_) | Value::Blank => None,
            v => Some((row.id.clone(), engine.format_value(v, format))),
        })
        .collect()
}

/// Rename column `col` of `t` and change its type to `ty`, in one
/// transaction: a column that becomes a formula drops its typed values, and
/// `carried` (from [`carry_over`]) is written where it differs from what is
/// stored.
pub fn change_column(
    txn: &mut SheetsTxn<'_, '_>,
    t: &TableSnap,
    col: &str,
    rename: Option<&str>,
    ty: Option<&ColumnType>,
    carried: &[(String, Option<Stored>)],
) -> doc::Result<()> {
    if let Some(name) = rename {
        txn.rename_column(&t.id, col, name)?;
    }
    let Some(ty) = ty else { return Ok(()) };
    txn.set_column_type(&t.id, col, ty.clone())?;
    if matches!(ty, ColumnType::Formula { .. }) {
        for r in &t.rows {
            if r.cells.contains_key(col) {
                txn.set_cell(&t.id, &r.id, col, None)?;
            }
        }
    }
    for (row, value) in carried {
        if t.row(row).and_then(|r| r.cells.get(col)) != value.as_ref() {
            txn.set_cell(&t.id, row, col, value.as_ref())?;
        }
    }
    Ok(())
}

// ── Moving a table to another node ───────────────────────────────────────

/// Whether a row link ties table `t` to another table of its note (it links
/// to one, or one links to it): a link reaches rows of its own note only, so
/// such a table cannot leave it.
pub fn tied(snap: &SheetsSnap, t: &TableSnap) -> bool {
    !t.row_links().is_empty() || snap.tables.iter().any(|o| o.id != t.id && o.row_links().contains(&t.id))
}

/// Why a [`tied`] table stays where it is.
pub fn tied_sentence(name: &str) -> String {
    format!("A link column ties {name} to another table of this note, and a link reaches rows of its own note only, so it cannot move to another node.")
}

/// Write the formulas and source of `after` where they differ from `before`
/// (the same table, rewritten).
pub fn write_formulas(t: &mut SheetsTxn<'_, '_>, before: &TableSnap, after: &TableSnap) -> doc::Result<()> {
    if after.source != before.source {
        t.set_source(&after.id, after.source.as_deref())?;
    }
    for (old, new) in before.columns.iter().zip(&after.columns) {
        if old.ty != new.ty {
            t.set_column_type(&after.id, &new.id, new.ty.clone())?;
        }
    }
    Ok(())
}

// ── A new sheet node ─────────────────────────────────────────────────────

/// A new sheet node's content with its first table ([`FIRST_TABLE`] with
/// [`FIRST_COLUMNS`]): written in the edit that makes the node, since two
/// devices making one node's first table would otherwise both make one.
pub fn new_sheet_content() -> doc::Result<Vec<u8>> {
    let sheets = Sheets::standalone();
    let edited = sheets.edit(|t| {
        let table = t.create_table(FIRST_TABLE, Placement::Last)?;
        for (name, ty) in FIRST_COLUMNS {
            t.add_column(&table, name, ty, Placement::Last)?;
        }
        Ok(())
    });
    edited.result?;
    Ok(sheets.diff_since(&yrs::StateVector::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A note's tables made through the document, as a person makes them.
    fn sheets(f: impl FnOnce(&mut SheetsTxn<'_, '_>) -> doc::Result<()>) -> SheetsSnap {
        let sheets = Sheets::standalone();
        sheets.edit(f).result.unwrap();
        sheets.read()
    }

    #[test]
    fn tables_columns_and_rows_by_name_or_id() {
        let snap = sheets(|t| {
            let a = t.create_table("Trips", Placement::Last)?;
            t.add_column(&a, "place", ColumnType::Text, Placement::Last)?;
            t.create_table("Cars", Placement::Last)?;
            Ok(())
        });
        let trips = find_table(&snap, " trips ").unwrap();
        assert_eq!(find_table(&snap, &trips.id).unwrap().name, "Trips");
        assert!(find_table(&snap, "nope").unwrap_err().contains("its tables are \"Trips\", \"Cars\""));
        assert!(find_table(&SheetsSnap::default(), "x").unwrap_err().contains("no tables"));
        assert_eq!(find_column(trips, "PLACE").unwrap().name, "place");
        assert!(find_column(trips, "miles").unwrap_err().contains("Trips has no column \"miles\""));
        let labels = vec![("r1".to_string(), "Civic".to_string()), ("r2".to_string(), "Van".to_string()), ("r3".to_string(), "van".to_string())];
        assert_eq!(find_row(&labels, "Cars", "civic").unwrap(), "r1");
        assert_eq!(find_row(&labels, "Cars", "r3").unwrap(), "r3");
        assert!(find_row(&labels, "Cars", "VAN").unwrap_err().starts_with("2 rows of Cars"));
        assert_eq!(find_row(&[], "Cars", "x").unwrap_err(), "Cars has no rows.");
    }

    #[test]
    fn a_table_a_link_ties_stays() {
        let snap = sheets(|t| {
            let a = t.create_table("A", Placement::Last)?;
            let b = t.create_table("B", Placement::Last)?;
            t.create_table("C", Placement::Last)?;
            t.add_column(&a, "link", ColumnType::RowLink { table: b }, Placement::Last)?;
            Ok(())
        });
        let by = |n: &str| find_table(&snap, n).unwrap();
        assert!(tied(&snap, by("A")) && tied(&snap, by("B")) && !tied(&snap, by("C")));
        assert!(tied_sentence("A").starts_with("A link column ties A to another table of this note"));
    }

    #[test]
    fn carrying_over_parses_what_was_typed() {
        let snap = sheets(|t| {
            let a = t.create_table("T", Placement::Last)?;
            let c = t.add_column(&a, "c", ColumnType::Text, Placement::Last)?;
            let rows: Vec<Vec<(String, Stored)>> =
                ["12", "far", "Civic"].iter().map(|s| vec![(c.clone(), Stored::Text(s.to_string()))]).collect();
            t.add_rows(&a, Placement::Last, &rows)?;
            Ok(())
        });
        let t = &snap.tables[0];
        let old = &t.columns[0];
        let today = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let carried = carry_over(t, old, &ColumnType::Number, &|_| None, &[], today);
        assert_eq!(carried[0].1, Some(Stored::Number(12.0)));
        assert_eq!(carried[1].1, Some(Stored::Unparsed("far".into())));
        let link = ColumnType::RowLink { table: "cars".into() };
        let choices = vec![("civic".to_string(), Stored::Row("x".into()))];
        assert_eq!(carry_over(t, old, &link, &|_| None, &choices, today)[2].1, Some(Stored::Row("x".into())));
        assert!(carries_over(&ColumnType::Text, &ColumnType::Number));
        assert!(!carries_over(&ColumnType::Text, &ColumnType::Formula { formula: "=1".into(), format: Format::Auto }));
        assert!(!carries_over(&ColumnType::NodeLink { under: None }, &ColumnType::NodeLink { under: None }));
        assert!(carries_over(&link, &ColumnType::RowLink { table: "other".into() }));
    }

    #[test]
    fn a_new_sheet_has_its_first_table() {
        let content = new_sheet_content().unwrap();
        let doc = pimble_crdt::NodeDoc::load(&content).unwrap();
        let snap = Sheets::of_node(&doc).read();
        assert_eq!(snap.tables.len(), 1);
        assert_eq!(snap.tables[0].name, FIRST_TABLE);
        let names: Vec<&str> = snap.tables[0].columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["name", "amount"]);
    }
}
