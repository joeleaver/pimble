//! The format an automatic formula column shows its values in (contract
//! section 3), read from the formula as Excel reads a new formula's number
//! format from what it refers to: `=[@start] + 7` shows a date because
//! `start` is a date column, `='Trip'!trip_start + [@day] - 1` because the
//! field holds a date, `=[@hours] * [@rate]` money because `rate` is money.
//!
//! The rules, over the formula's shape:
//! - a column shows its own format (an automatic formula column, its
//!   inferred one); a field shows the format its text was typed in (an ISO
//!   date, `$120`, `15%`); a row link's `.column`, that column's;
//! - `+` and `-` keep a date when one side is a date (date + days is a date)
//!   and lose it when both are (the days between); otherwise the first side
//!   with a format wins;
//! - `*` and `/` keep money, and the format of a side multiplied by a plain
//!   number or a percent; a date times anything is a number;
//! - the date functions answer dates; `SUM`, `MIN`, `MAX`, `ROUND`, `IF`,
//!   the lookups and their kin pass on the format of the values they return;
//! - anything else is automatic (a number shows plainly).

use crate::model::{ColumnSnap, ColumnType, Format, NodeRef, TableSnap};

use super::ast::*;
use super::dates;
use super::engine::{norm, Engine};
use super::value::TableAddr;

/// How deep the inference follows a formula's parts and the formula
/// columns it reads before it settles for automatic.
const MAX_DEPTH: usize = 64;

impl<'b> Engine<'b> {
    /// The format a column's values show in: its type's, or for an
    /// automatic formula column, the one its formula implies.
    pub(crate) fn column_format(&self, node: &NodeRef, t: &'b TableSnap, c: &'b ColumnSnap) -> Format {
        let ColumnType::Formula { formula, format: Format::Auto } = &c.ty else {
            return c.ty.format();
        };
        let key = (*node, t.id.clone(), c.id.clone());
        if let Some(f) = self.formats().borrow().get(&key) {
            return *f;
        }
        // A formula column that reads itself, directly or around a loop,
        // settles as automatic.
        self.formats().borrow_mut().insert(key.clone(), Format::Auto);
        let f = match &*self.parse_cached(formula) {
            Ok(expr) => self.format_of(expr, node, Some(t), 0),
            Err(_) => Format::Auto,
        };
        self.formats().borrow_mut().insert(key, f);
        f
    }

    fn format_of(&self, e: &Expr, node: &NodeRef, table: Option<&'b TableSnap>, depth: usize) -> Format {
        if depth >= MAX_DEPTH {
            return Format::Auto;
        }
        let of = |e: &Expr| self.format_of(e, node, table, depth + 1);
        match &e.kind {
            ExprKind::Ref(r) => self.ref_format(r, node, table).unwrap_or(Format::Auto),
            ExprKind::Member(o, m) => self.member_format(o, &m.name, node, table).unwrap_or(Format::Auto),
            ExprKind::Neg(a) => of(a),
            ExprKind::Percent(_) => Format::Percent,
            ExprKind::Binary(op, a, b) => binary_format(*op, of(a), of(b)),
            ExprKind::Call(name, args) => {
                let pick = |idx: &[usize]| first_format(idx.iter().filter_map(|&i| args.get(i)).map(&of));
                match name.as_str() {
                    "TODAY" | "NOW" | "DATE" | "DATEVALUE" | "EDATE" | "EOMONTH" | "WORKDAY" => Format::Date,
                    "SUM" | "MIN" | "MAX" | "AVERAGE" | "MEDIAN" | "ABS" | "ROUND" | "ROUNDUP" | "ROUNDDOWN"
                    | "INT" | "IFERROR" | "IFNA" => pick(&[0, 1]),
                    "INDEX" | "SUMIFS" | "AVERAGEIFS" | "MAXIFS" | "MINIFS" | "FILTER" | "SORT"
                    | "UNIQUE" => pick(&[0]),
                    "IF" => pick(&[1, 2]),
                    "IFS" => pick(&(1..args.len()).step_by(2).collect::<Vec<_>>()),
                    "SUMIF" | "AVERAGEIF" => pick(&[2, 0]),
                    "XLOOKUP" => pick(&[2]),
                    "VLOOKUP" => Format::Auto,
                    "CHOOSE" => pick(&(1..args.len()).collect::<Vec<_>>()),
                    "SWITCH" => pick(&(2..args.len()).step_by(2).chain([args.len().saturating_sub(1)]).collect::<Vec<_>>()),
                    "LET" => args.last().map(&of).unwrap_or(Format::Auto),
                    _ => Format::Auto,
                }
            }
            _ => Format::Auto,
        }
    }

    /// A reference's format: one column's, or a field's.
    fn ref_format(&self, r: &Reference, node: &NodeRef, table: Option<&'b TableSnap>) -> Option<Format> {
        let at = match &r.note {
            Some(n) => self.resolve_note(&n.name).ok()?,
            None => *node,
        };
        let t = match &r.table {
            None if at == *node => table?,
            None => return None,
            Some(part) => {
                let found = self.snap(&at).ok().and_then(|s| self.find_table(s, &part.name).ok().flatten());
                match (found, &part.name) {
                    (Some(t), _) => t,
                    // A field of a node: `'Trip'!trip_start`.
                    (None, NameRef::Name(n)) if r.note.is_some() && r.spec.is_none() => {
                        return Some(self.field_format(&at, n));
                    }
                    _ => return None,
                }
            }
        };
        let spec = r.spec.as_ref()?;
        if spec.area != Area::Data {
            return None;
        }
        let Cols::One(p) = &spec.cols else {
            return None;
        };
        self.col_format(&at, t, &p.name)
    }

    /// One column's format. A plain table's column is looked up alone: going
    /// through [`Engine::table_cols`] would infer every sibling, and one whose
    /// inference is under way would be read as automatic and kept so.
    fn col_format(&self, at: &NodeRef, t: &'b TableSnap, name: &NameRef) -> Option<Format> {
        if !t.is_summary() {
            return Some(self.column_format(at, t, own_col(t, name)?));
        }
        let cols = self.table_cols(at, t).ok()?;
        Some(cols[self.find_col(&cols, name)?].meta.format)
    }

    /// `[@vehicle].mpg`: the format of the column a row link's member names.
    fn member_format(&self, obj: &Expr, name: &NameRef, node: &NodeRef, table: Option<&'b TableSnap>) -> Option<Format> {
        let ExprKind::Ref(r) = &obj.kind else {
            return None;
        };
        let at = match &r.note {
            Some(n) => self.resolve_note(&n.name).ok()?,
            None => *node,
        };
        let t = match &r.table {
            None if at == *node => table?,
            Some(part) => self.find_table(self.snap(&at).ok()?, &part.name).ok()??,
            None => return None,
        };
        let Cols::One(p) = &r.spec.as_ref()?.cols else {
            return None;
        };
        let link = match t.is_summary() {
            false => match &own_col(t, &p.name)?.ty {
                ColumnType::RowLink { table } => TableAddr { node: at, table: table.clone() },
                _ => return None,
            },
            true => {
                let cols = self.table_cols(&at, t).ok()?;
                cols[self.find_col(&cols, &p.name)?].meta.link.clone()?
            }
        };
        let target = self.snap(&link.node).ok()?.table(&link.table)?;
        self.col_format(&link.node, target, name)
    }

    fn field_format(&self, node: &NodeRef, name: &str) -> Format {
        let Some(info) = self.book().node(node) else {
            return Format::Auto;
        };
        let n = norm(name);
        match info.fields.iter().find(|(k, _)| norm(k) == n) {
            Some((_, v)) => typed_format(v),
            None => Format::Auto,
        }
    }
}

/// A plain table's column, by id or name.
fn own_col<'b>(t: &'b TableSnap, name: &NameRef) -> Option<&'b ColumnSnap> {
    match name {
        NameRef::Id(id) => t.columns.iter().find(|c| c.id == *id),
        NameRef::Name(n) => {
            let n = norm(n);
            t.columns.iter().find(|c| norm(&c.name) == n)
        }
        _ => None,
    }
}

/// The format a value typed as text implies: an ISO date, `$120`, `15%`.
fn typed_format(s: &str) -> Format {
    let t = s.trim();
    if dates::parse_iso(t).is_some() {
        Format::Date
    } else if t.trim_start_matches('-').trim_start().starts_with('$') {
        Format::Money
    } else if t.ends_with('%') {
        Format::Percent
    } else {
        Format::Auto
    }
}

fn first_format(fs: impl Iterator<Item = Format>) -> Format {
    fs.into_iter().find(|f| *f != Format::Auto).unwrap_or(Format::Auto)
}

fn binary_format(op: BinOp, a: Format, b: Format) -> Format {
    use Format::*;
    match op {
        BinOp::Add | BinOp::Sub => match (a, b) {
            (Date, Date) => Auto,
            (Date, _) | (_, Date) => Date,
            (Auto, f) | (f, _) => f,
        },
        BinOp::Mul | BinOp::Div => match (a, b) {
            (Date, _) | (_, Date) => Auto,
            (Money, _) | (_, Money) if op == BinOp::Mul => Money,
            (Money, Money) => Auto,
            (Money, _) => Money,
            (f, Auto | Number | Percent) => f,
            (Auto | Number | Percent, f) if op == BinOp::Mul => f,
            _ => Auto,
        },
        _ => Auto,
    }
}
