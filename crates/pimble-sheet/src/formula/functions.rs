//! Excel's functions, wave 1's set (contract section 6), with Excel's
//! behavior: argument defaults, which errors propagate and which are
//! skipped, what an empty range gives.
//!
//! Most functions are elementwise: given a column where they take one value
//! they answer a column (`TEXT(Transactions[date],"yyyy-mm")`), as Excel's
//! dynamic arrays do. Aggregates, criteria, lookups and the dynamic-array
//! functions take whole lists. `IF`, `IFS`, `IFERROR`, `IFNA` and `LET`
//! evaluate their arguments only as needed.

use std::collections::HashMap;
use std::rc::Rc;

use super::ast::{Expr, ExprKind};
use super::coerce;
use super::criteria::{self, Criterion};
use super::dates;
use super::engine::{func_as_value, Ctx, Engine};
use super::format;
use super::value::*;

static BLANK: Value = Value::Blank;

/// Every function Pimble knows, by category (for the guide and for the
/// sentence an unknown name gets).
pub const CATEGORIES: &[(&str, &[&str])] = &[
    (
        "conditional",
        &[
            "IF", "IFS", "IFERROR", "IFNA", "SWITCH", "CHOOSE", "AND", "OR", "NOT", "LET",
        ],
    ),
    (
        "total",
        &[
            "SUM",
            "AVERAGE",
            "MIN",
            "MAX",
            "COUNT",
            "COUNTA",
            "COUNTBLANK",
            "MEDIAN",
            "PRODUCT",
            "SUMPRODUCT",
            "SUMIF",
            "SUMIFS",
            "COUNTIF",
            "COUNTIFS",
            "AVERAGEIF",
            "AVERAGEIFS",
            "MAXIFS",
            "MINIFS",
        ],
    ),
    (
        "lookup",
        &[
            "XLOOKUP", "VLOOKUP", "INDEX", "MATCH", "XMATCH", "ROWS", "COLUMNS",
        ],
    ),
    ("list", &["FILTER", "SORT", "UNIQUE", "HSTACK", "GROUPBY"]),
    (
        "text",
        &[
            "TEXT",
            "TEXTJOIN",
            "CONCAT",
            "CONCATENATE",
            "LEFT",
            "REPT",
            "RIGHT",
            "MID",
            "LEN",
            "UPPER",
            "LOWER",
            "PROPER",
            "TRIM",
            "VALUE",
        ],
    ),
    (
        "date",
        &[
            "TODAY", "DATE", "YEAR", "MONTH", "DAY", "WEEKDAY", "EOMONTH", "EDATE", "DAYS",
        ],
    ),
    (
        "number",
        &[
            "ROUND",
            "ROUNDUP",
            "ROUNDDOWN",
            "ABS",
            "INT",
            "MOD",
            "SQRT",
            "POWER",
            "SIGN",
        ],
    ),
    (
        "information",
        &[
            "ISBLANK", "ISNUMBER", "ISTEXT", "ISERROR", "ISNA", "NA", "TRUE", "FALSE",
        ],
    ),
];

/// Excel functions Pimble does not have yet, by the category a person
/// would look in instead.
const NOT_YET: &[(&str, &[&str])] = &[
    (
        "date",
        &[
            "NETWORKDAYS",
            "WORKDAY",
            "DATEDIF",
            "NOW",
            "HOUR",
            "MINUTE",
            "SECOND",
            "WEEKNUM",
            "ISOWEEKNUM",
            "YEARFRAC",
            "DATEVALUE",
            "TIME",
            "TIMEVALUE",
            "DAYS360",
        ],
    ),
    (
        "text",
        &[
            "SUBSTITUTE",
            "FIND",
            "SEARCH",
            "REPLACE",
            "EXACT",
            "TEXTBEFORE",
            "TEXTAFTER",
            "TEXTSPLIT",
            "CHAR",
            "CODE",
            "FIXED",
            "DOLLAR",
        ],
    ),
    (
        "number",
        &[
            "CEILING",
            "FLOOR",
            "TRUNC",
            "MROUND",
            "LOG",
            "LOG10",
            "LN",
            "EXP",
            "PI",
            "RAND",
            "RANDBETWEEN",
            "QUOTIENT",
            "SUMSQ",
            "STDEV",
            "STDEV.S",
            "STDEV.P",
            "VAR",
            "MODE",
            "LARGE",
            "SMALL",
            "RANK",
            "PERCENTILE",
        ],
    ),
    (
        "lookup",
        &[
            "HLOOKUP",
            "LOOKUP",
            "OFFSET",
            "INDIRECT",
            "ADDRESS",
            "CHOOSECOLS",
            "CHOOSEROWS",
        ],
    ),
    (
        "list",
        &[
            "VSTACK", "TAKE", "DROP", "SEQUENCE", "SORTBY", "PIVOTBY", "TOCOL", "TOROW", "WRAPROWS",
        ],
    ),
    (
        "conditional",
        &["XOR", "LAMBDA", "MAP", "REDUCE", "SCAN", "BYROW", "BYCOL"],
    ),
    (
        "information",
        &[
            "ISLOGICAL",
            "ISERR",
            "ISNONTEXT",
            "ISEVEN",
            "ISODD",
            "N",
            "TYPE",
            "ERROR.TYPE",
        ],
    ),
];

pub(crate) fn is_known(name: &str) -> bool {
    CATEGORIES.iter().any(|(_, fs)| fs.contains(&name))
}

pub(crate) fn unknown_function(name: &str) -> FormulaError {
    let hint = NOT_YET
        .iter()
        .find(|(_, fs)| fs.contains(&name))
        .and_then(|(cat, _)| CATEGORIES.iter().find(|(c, _)| c == cat))
        .map(|(cat, fs)| format!("; the {cat} functions are {}", fs.join(", ")))
        .unwrap_or_default();
    FormulaError::name(format!(
        "There's no function called {name}() in Pimble yet{hint}"
    ))
}

// ── Broadcasting ────────────────────────────────────────────────────────

/// Runs `f` once, or once per element when an argument is a list,
/// broadcasting single columns across and single rows down.
pub(crate) fn map_vals(
    args: &[&Val],
    keep_format: bool,
    f: &mut dyn FnMut(&[&Value]) -> Value,
) -> Val {
    let opts: Vec<Option<&Val>> = args.iter().map(|a| Some(*a)).collect();
    map_opt(&opts, keep_format, &mut |xs| {
        let v: Vec<&Value> = xs.iter().map(|x| x.unwrap_or(&BLANK)).collect();
        f(&v)
    })
}

/// [`map_vals`] with arguments that may be left out.
pub(crate) fn map_opt(
    args: &[Option<&Val>],
    keep_format: bool,
    f: &mut dyn FnMut(&[Option<&Value>]) -> Value,
) -> Val {
    let mut arrays: Vec<&Array> = Vec::new();
    for a in args.iter().flatten() {
        match a {
            Val::Func(g) => return Val::err(func_as_value(*g)),
            Val::Arr(x) => arrays.push(x),
            Val::One(_) => {}
        }
    }
    fn pick<'v>(a: &Option<&'v Val>, r: usize, c: usize) -> Option<&'v Value> {
        a.map(|v| match v {
            Val::One(x) => x,
            Val::Arr(x) => x.at(r, c),
            Val::Func(_) => &BLANK,
        })
    }
    if arrays.is_empty() {
        let xs: Vec<Option<&Value>> = args.iter().map(|a| pick(a, 0, 0)).collect();
        return Val::One(f(&xs));
    }
    let h = arrays.iter().map(|a| a.height).max().unwrap_or(0);
    let w = arrays.iter().map(|a| a.width()).max().unwrap_or(0);
    for a in &arrays {
        if (a.height != h && a.height != 1) || (a.width() != w && a.width() != 1) {
            return Val::err(FormulaError::value(format!(
                "Those lists are different lengths ({} and {})",
                a.height, h
            )));
        }
    }
    let template = arrays
        .iter()
        .find(|a| a.height == h && a.width() == w)
        .copied()
        .unwrap_or(arrays[0]);
    let mut cols = Vec::with_capacity(w);
    for c in 0..w {
        let mut values = Vec::with_capacity(h);
        for r in 0..h {
            let xs: Vec<Option<&Value>> = args.iter().map(|a| pick(a, r, c)).collect();
            values.push(f(&xs));
        }
        let tm = template.cols.get(c.min(template.width().saturating_sub(1)));
        let meta = match tm {
            Some(t) => ColMeta {
                name: t.meta.name.clone(),
                format: if keep_format {
                    t.meta.format
                } else {
                    crate::model::Format::Auto
                },
                link: None,
            },
            None => ColMeta::named(""),
        };
        cols.push(Column {
            meta,
            values: Rc::new(values),
        });
    }
    let rows = if template.height == h {
        template.rows.clone()
    } else {
        None
    };
    Val::arr(Array::from_columns(cols, rows))
}

// ── Argument helpers ───────────────────────────────────────────────────

type Args = [Option<Val>];

fn arity(name: &str, a: &Args, min: usize, max: usize) -> Result<(), FormulaError> {
    let n = a.len();
    let plural = |k: usize| if k == 1 { "input" } else { "inputs" };
    if n < min {
        return Err(FormulaError::value(if min == max {
            format!("{name}() needs {min} {}", plural(min))
        } else {
            format!("{name}() needs at least {min} {}", plural(min))
        }));
    }
    if n > max {
        return Err(FormulaError::value(format!(
            "{name}() takes at most {max} {}",
            plural(max)
        )));
    }
    Ok(())
}

/// One value from an argument that must be one value.
fn scalar(a: &Args, i: usize) -> Result<Option<Value>, FormulaError> {
    match a.get(i) {
        None | Some(None) => Ok(None),
        Some(Some(Val::One(v))) => Ok(Some(v.clone())),
        Some(Some(Val::Arr(x))) if x.height == 1 && x.width() == 1 => Ok(Some(x.get(0, 0).clone())),
        Some(Some(Val::Arr(_))) => Err(FormulaError::value(
            "That input takes one value, not a list",
        )),
        Some(Some(Val::Func(f))) => Err(func_as_value(*f)),
    }
}

fn num_arg(a: &Args, i: usize, default: f64) -> Result<f64, FormulaError> {
    match scalar(a, i)? {
        None => Ok(default),
        Some(v) => coerce::to_number(&v),
    }
}

fn bool_arg(a: &Args, i: usize, default: bool) -> Result<bool, FormulaError> {
    match scalar(a, i)? {
        None => Ok(default),
        Some(v) => coerce::to_bool(&v),
    }
}

/// An argument as a list of values: a column, a row, or every value of a
/// table row by row.
fn range_values(v: &Val) -> Result<Vec<Value>, FormulaError> {
    match v {
        Val::One(Value::Error(e)) => Err(e.clone()),
        Val::One(x) => Ok(vec![x.clone()]),
        Val::Arr(a) if a.width() == 1 => Ok((*a.cols[0].values).clone()),
        Val::Arr(a) => Ok(a.values().cloned().collect()),
        Val::Func(f) => Err(func_as_value(*f)),
    }
}

/// An argument as an array (one value is a 1×1 array).
fn as_array(v: &Val) -> Result<Rc<Array>, FormulaError> {
    match v {
        Val::One(Value::Error(e)) => Err(e.clone()),
        Val::One(x) => Ok(Rc::new(Array::one_row(vec![(
            ColMeta::named("value"),
            x.clone(),
        )]))),
        Val::Arr(a) => Ok(a.clone()),
        Val::Func(f) => Err(func_as_value(*f)),
    }
}

fn need<'a>(a: &'a Args, i: usize, name: &str) -> Result<&'a Val, FormulaError> {
    a.get(i)
        .and_then(|v| v.as_ref())
        .ok_or_else(|| FormulaError::value(format!("{name}() is missing an input")))
}

fn wrap(r: Result<Val, FormulaError>) -> Val {
    r.unwrap_or_else(Val::err)
}

fn val(r: Result<Value, FormulaError>) -> Value {
    r.unwrap_or_else(Value::err)
}

fn opt_num(x: Option<&Value>, default: f64) -> Result<f64, FormulaError> {
    match x {
        None => Ok(default),
        Some(v) => coerce::to_number(v),
    }
}

fn need_num(x: Option<&Value>) -> Result<f64, FormulaError> {
    coerce::to_number(x.unwrap_or(&BLANK))
}

fn trunc(x: f64) -> i64 {
    x.trunc().clamp(-1e15, 1e15) as i64
}

// ── Calls ───────────────────────────────────────────────────────────────

pub(crate) fn call<'b>(en: &Engine<'b>, name: &str, args: &[Expr], ctx: &Ctx<'b>) -> Val {
    match name {
        "IF" => return if_(en, args, ctx),
        "IFS" => return ifs(en, args, ctx),
        "IFERROR" | "IFNA" => return iferror(en, name == "IFNA", args, ctx),
        "LET" => return let_(en, args, ctx),
        _ => {}
    }
    if !is_known(name) {
        return Val::err(unknown_function(name));
    }
    let a: Vec<Option<Val>> = args
        .iter()
        .map(|x| match x.kind {
            ExprKind::Missing => None,
            _ => Some(en.eval(x, ctx)),
        })
        .collect();
    // A note that has not arrived makes the whole answer wait, whatever
    // the function would do with an error.
    for v in a.iter().flatten() {
        if let Some(e) = v.as_error() {
            if e.code == ErrorCode::Loading {
                return v.clone();
            }
        }
    }
    eager(en, name, &a)
}

fn eager(en: &Engine, name: &str, a: &Args) -> Val {
    if let Some(agg) = Agg::by_name(name) {
        return wrap(arity(name, a, 1, 255).map(|_| Val::One(aggregate(agg, a))));
    }
    match name {
        "TODAY" => wrap(arity(name, a, 0, 0).map(|_| Val::One(Value::Number(en.today().floor())))),
        "NA" => {
            wrap(arity(name, a, 0, 0).map(|_| Val::err(FormulaError::na("NA() is #N/A by design"))))
        }
        "TRUE" | "FALSE" => {
            wrap(arity(name, a, 0, 0).map(|_| Val::One(Value::Bool(name == "TRUE"))))
        }
        "AND" | "OR" => wrap(arity(name, a, 1, 255).and_then(|_| and_or(name == "AND", a))),
        "SUMPRODUCT" => wrap(arity(name, a, 1, 255).and_then(|_| sumproduct(a))),
        "COUNTIF" | "SUMIF" | "AVERAGEIF" => wrap(criteria_one(en, name, a)),
        "COUNTIFS" | "SUMIFS" | "AVERAGEIFS" | "MAXIFS" | "MINIFS" => {
            wrap(criteria_many(en, name, a))
        }
        "XLOOKUP" => wrap(xlookup(en, a)),
        "VLOOKUP" => wrap(vlookup(en, a)),
        "INDEX" => wrap(index(a)),
        "MATCH" => wrap(match_(en, a, false)),
        "XMATCH" => wrap(match_(en, a, true)),
        "FILTER" => wrap(filter(a)),
        "SORT" => wrap(sort(en, a)),
        "UNIQUE" => wrap(unique(a)),
        "HSTACK" => wrap(hstack(a)),
        "GROUPBY" => wrap(groupby(en, a)),
        "ROWS" | "COLUMNS" => wrap(arity(name, a, 1, 1).and_then(|_| {
            let v = need(a, 0, name)?;
            let (h, w) = match v {
                Val::Arr(x) => (x.height, x.width()),
                _ => (1, 1),
            };
            Ok(Val::One(Value::Number(
                if name == "ROWS" { h } else { w } as f64
            )))
        })),
        "CHOOSE" => wrap(choose(a)),
        "CONCAT" => wrap(arity(name, a, 1, 255).and_then(|_| {
            let mut s = String::new();
            for v in a.iter().flatten() {
                for x in range_values(v)? {
                    s.push_str(&coerce::to_text(&x, en)?);
                }
            }
            Ok(Val::One(Value::Text(s)))
        })),
        "TEXTJOIN" => wrap(textjoin(en, a)),
        _ => elementwise(en, name, a),
    }
}

// ── Lazy forms ──────────────────────────────────────────────────────────

fn if_<'b>(en: &Engine<'b>, args: &[Expr], ctx: &Ctx<'b>) -> Val {
    if args.len() < 2 || args.len() > 3 {
        return Val::err(FormulaError::value(
            "IF() takes a condition, a value if true and a value if false",
        ));
    }
    let branch = |i: usize| -> Val {
        match args.get(i) {
            None => Val::One(Value::Bool(false)),
            Some(e) => en.eval(e, ctx),
        }
    };
    match en.eval(&args[0], ctx) {
        Val::One(c) => match coerce::to_bool(&c) {
            Ok(true) => branch(1),
            Ok(false) => branch(2),
            Err(e) => Val::err(e),
        },
        c => {
            let (y, n) = (branch(1), branch(2));
            map_vals(
                &[&c, &y, &n],
                true,
                &mut |xs| match coerce::to_bool(xs[0]) {
                    Ok(true) => xs[1].clone(),
                    Ok(false) => xs[2].clone(),
                    Err(e) => Value::err(e),
                },
            )
        }
    }
}

fn ifs<'b>(en: &Engine<'b>, args: &[Expr], ctx: &Ctx<'b>) -> Val {
    if args.len() < 2 || !args.len().is_multiple_of(2) {
        return Val::err(FormulaError::value(
            "IFS() takes pairs of a condition and a value",
        ));
    }
    for (k, pair) in args.chunks(2).enumerate() {
        match en.eval(&pair[0], ctx) {
            Val::One(c) => match coerce::to_bool(&c) {
                Ok(true) => return en.eval(&pair[1], ctx),
                Ok(false) => continue,
                Err(e) => return Val::err(e),
            },
            _ => {
                // A list of conditions: decide element by element.
                let rest: Vec<Val> = args[k * 2..].iter().map(|e| en.eval(e, ctx)).collect();
                let refs: Vec<&Val> = rest.iter().collect();
                return map_vals(&refs, true, &mut |xs| {
                    for p in xs.chunks(2) {
                        match coerce::to_bool(p[0]) {
                            Ok(true) => return p[1].clone(),
                            Ok(false) => {}
                            Err(e) => return Value::err(e),
                        }
                    }
                    Value::err(FormulaError::na("None of the IFS conditions is true"))
                });
            }
        }
    }
    Val::err(FormulaError::na("None of the IFS conditions is true"))
}

fn catches(e: &FormulaError, na_only: bool) -> bool {
    e.code.catchable() && (!na_only || e.code == ErrorCode::NA)
}

fn iferror<'b>(en: &Engine<'b>, na_only: bool, args: &[Expr], ctx: &Ctx<'b>) -> Val {
    let name = if na_only { "IFNA" } else { "IFERROR" };
    if args.len() != 2 {
        return Val::err(FormulaError::value(format!(
            "{name}() takes a value and what to use if it fails"
        )));
    }
    match en.eval(&args[0], ctx) {
        Val::One(Value::Error(e)) if catches(&e, na_only) => en.eval(&args[1], ctx),
        v @ Val::Arr(_) => {
            let alt = en.eval(&args[1], ctx);
            map_vals(&[&v, &alt], true, &mut |xs| match xs[0] {
                Value::Error(e) if catches(e, na_only) => xs[1].clone(),
                x => x.clone(),
            })
        }
        v => v,
    }
}

fn let_<'b>(en: &Engine<'b>, args: &[Expr], ctx: &Ctx<'b>) -> Val {
    if args.len() < 3 || args.len().is_multiple_of(2) {
        return Val::err(FormulaError::value(
            "LET() takes pairs of a name and a value, then a calculation",
        ));
    }
    let mut c = ctx.clone();
    for pair in args[..args.len() - 1].chunks(2) {
        let ExprKind::Name(n) = &pair[0].kind else {
            return Val::err(FormulaError::value(
                "LET()'s names are plain words, like LET(rate, 95, …)",
            ));
        };
        let v = en.eval(&pair[1], &c);
        c = c.with(n, v);
    }
    en.eval(&args[args.len() - 1], &c)
}

// ── Aggregates ─────────────────────────────────────────────────────────

fn aggregate(agg: Agg, a: &Args) -> Value {
    match agg {
        Agg::Count => {
            let mut n = 0usize;
            for v in a {
                match v {
                    None => n += 1,
                    Some(Val::One(x)) => {
                        n += match x {
                            Value::Number(_) | Value::Bool(_) => 1,
                            Value::Text(s) => coerce::parse_number_text(s).is_some() as usize,
                            _ => 0,
                        }
                    }
                    Some(Val::Arr(x)) => {
                        n += x.values().filter(|v| matches!(v, Value::Number(_))).count()
                    }
                    Some(Val::Func(f)) => return Value::err(func_as_value(*f)),
                }
            }
            Value::Number(n as f64)
        }
        Agg::CountA => {
            let mut n = 0usize;
            for v in a {
                match v {
                    None => n += 1,
                    Some(Val::One(x)) => n += !x.is_blank() as usize,
                    Some(Val::Arr(x)) => n += x.values().filter(|v| !v.is_blank()).count(),
                    Some(Val::Func(f)) => return Value::err(func_as_value(*f)),
                }
            }
            Value::Number(n as f64)
        }
        Agg::CountBlank => {
            let mut n = 0usize;
            let blank = |v: &Value| {
                matches!(v, Value::Blank) || matches!(v, Value::Text(s) if s.is_empty())
            };
            for v in a.iter().flatten() {
                match v {
                    Val::One(x) => n += blank(x) as usize,
                    Val::Arr(x) => n += x.values().filter(|v| blank(v)).count(),
                    Val::Func(f) => return Value::err(func_as_value(*f)),
                }
            }
            Value::Number(n as f64)
        }
        _ => {
            let mut xs = Vec::new();
            for v in a {
                match v {
                    None => xs.push(0.0),
                    Some(Val::One(x)) => match x {
                        Value::Number(n) => xs.push(*n),
                        Value::Bool(b) => xs.push(*b as u8 as f64),
                        Value::Text(_) => match coerce::to_number(x) {
                            Ok(n) => xs.push(n),
                            Err(e) => return Value::err(e),
                        },
                        Value::Error(e) => return Value::err(e.clone()),
                        _ => {}
                    },
                    Some(Val::Arr(arr)) => {
                        for x in arr.values() {
                            match x {
                                Value::Number(n) => xs.push(*n),
                                Value::Error(e) => return Value::err(e.clone()),
                                _ => {}
                            }
                        }
                    }
                    Some(Val::Func(f)) => return Value::err(func_as_value(*f)),
                }
            }
            numbers(agg, &xs)
        }
    }
}

/// An aggregate over a list's values, as `GROUPBY` applies it.
pub(crate) fn aggregate_list(agg: Agg, values: &[Value]) -> Value {
    let arr = Array::single_column(ColMeta::named(""), values.to_vec(), None);
    aggregate(agg, &[Some(Val::arr(arr))])
}

fn numbers(agg: Agg, xs: &[f64]) -> Value {
    let r = match agg {
        Agg::Sum => xs.iter().sum(),
        Agg::Average => {
            if xs.is_empty() {
                return Value::err(FormulaError::div0());
            }
            xs.iter().sum::<f64>() / xs.len() as f64
        }
        Agg::Min => xs
            .iter()
            .copied()
            .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.min(x))))
            .unwrap_or(0.0),
        Agg::Max => xs
            .iter()
            .copied()
            .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
            .unwrap_or(0.0),
        Agg::Product => {
            if xs.is_empty() {
                0.0
            } else {
                xs.iter().product()
            }
        }
        Agg::Median => {
            if xs.is_empty() {
                return Value::err(FormulaError::num("There is nothing to take the median of"));
            }
            let mut v = xs.to_vec();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let m = v.len() / 2;
            if v.len() % 2 == 1 {
                v[m]
            } else {
                (v[m - 1] + v[m]) / 2.0
            }
        }
        Agg::Count | Agg::CountA | Agg::CountBlank => xs.len() as f64,
    };
    if r.is_finite() {
        Value::Number(r)
    } else {
        Value::err(FormulaError::num("The result is too large"))
    }
}

fn and_or(and: bool, a: &Args) -> Result<Val, FormulaError> {
    let mut seen = false;
    let mut acc = and;
    let mut take = |b: bool| {
        seen = true;
        if and {
            acc &= b;
        } else {
            acc |= b;
        }
    };
    for v in a.iter().flatten() {
        match v {
            Val::One(Value::Blank) => {}
            Val::One(x) => take(coerce::to_bool(x)?),
            Val::Arr(arr) => {
                for x in arr.values() {
                    match x {
                        Value::Bool(b) => take(*b),
                        Value::Number(n) => take(*n != 0.0),
                        Value::Error(e) => return Err(e.clone()),
                        _ => {}
                    }
                }
            }
            Val::Func(f) => return Err(func_as_value(*f)),
        }
    }
    if !seen {
        return Err(FormulaError::value(format!(
            "{}() found no TRUE or FALSE values",
            if and { "AND" } else { "OR" }
        )));
    }
    Ok(Val::One(Value::Bool(acc)))
}

fn sumproduct(a: &Args) -> Result<Val, FormulaError> {
    let arrays: Vec<Rc<Array>> = a
        .iter()
        .map(|v| match v {
            None => Err(FormulaError::value("SUMPRODUCT() is missing an input")),
            Some(v) => as_array(v),
        })
        .collect::<Result<_, _>>()?;
    let (h, w) = (arrays[0].height, arrays[0].width());
    if arrays.iter().any(|x| x.height != h || x.width() != w) {
        return Err(FormulaError::value(
            "SUMPRODUCT() needs lists of the same size",
        ));
    }
    let mut s = 0.0;
    for r in 0..h {
        for c in 0..w {
            let mut p = 1.0;
            for x in &arrays {
                match x.get(r, c) {
                    Value::Number(n) => p *= n,
                    Value::Error(e) => return Err(e.clone()),
                    _ => p = 0.0,
                }
            }
            s += p;
        }
    }
    Ok(Val::One(Value::Number(s)))
}

// ── Criteria ────────────────────────────────────────────────────────────

fn criteria_one(en: &Engine, name: &str, a: &Args) -> Result<Val, FormulaError> {
    let count = name == "COUNTIF";
    arity(name, a, 2, if count { 2 } else { 3 })?;
    let range = range_values(need(a, 0, name)?)?;
    let target = match a.get(2).and_then(|v| v.as_ref()) {
        Some(v) => range_values(v)?,
        None => range.clone(),
    };
    if target.len() != range.len() {
        return Err(FormulaError::value(format!(
            "{name}()'s two lists must be the same length"
        )));
    }
    let crit = need(a, 1, name)?;
    Ok(map_vals(&[crit], false, &mut |c| {
        val((|| {
            let cr = Criterion::new(c[0], en)?;
            let mut hits = Vec::new();
            for (i, v) in range.iter().enumerate() {
                if cr.matches(v, en) {
                    hits.push(target[i].clone());
                }
            }
            Ok(match name {
                "COUNTIF" => Value::Number(hits.len() as f64),
                "SUMIF" => aggregate_list(Agg::Sum, &hits),
                _ => aggregate_list(Agg::Average, &hits),
            })
        })())
    }))
}

fn criteria_many(en: &Engine, name: &str, a: &Args) -> Result<Val, FormulaError> {
    let count = name == "COUNTIFS";
    let off = if count { 0 } else { 1 };
    if a.len() < off + 2 || !(a.len() - off).is_multiple_of(2) {
        return Err(FormulaError::value(format!(
            "{name}() takes {}pairs of a list and a criterion",
            if count {
                ""
            } else {
                "the list to total, then "
            }
        )));
    }
    let target = if count {
        None
    } else {
        Some(range_values(need(a, 0, name)?)?)
    };
    let mut ranges = Vec::new();
    let mut crits: Vec<&Val> = Vec::new();
    for k in (off..a.len()).step_by(2) {
        ranges.push(range_values(need(a, k, name)?)?);
        crits.push(need(a, k + 1, name)?);
    }
    let len = target.as_ref().map(|t| t.len()).unwrap_or(ranges[0].len());
    if ranges.iter().any(|r| r.len() != len) {
        return Err(FormulaError::value(format!(
            "Every list in {name}() must be the same length"
        )));
    }
    Ok(map_vals(&crits, false, &mut |cs| {
        val((|| {
            let crs: Vec<Criterion> = cs
                .iter()
                .map(|c| Criterion::new(c, en))
                .collect::<Result<_, _>>()?;
            let mut hits = Vec::new();
            let mut n = 0usize;
            for i in 0..len {
                if crs.iter().zip(&ranges).all(|(c, r)| c.matches(&r[i], en)) {
                    n += 1;
                    if let Some(t) = &target {
                        hits.push(t[i].clone());
                    }
                }
            }
            Ok(match name {
                "COUNTIFS" => Value::Number(n as f64),
                "SUMIFS" => aggregate_list(Agg::Sum, &hits),
                "AVERAGEIFS" => aggregate_list(Agg::Average, &hits),
                "MAXIFS" => aggregate_list(Agg::Max, &hits),
                _ => aggregate_list(Agg::Min, &hits),
            })
        })())
    }))
}

// ── Lookups ─────────────────────────────────────────────────────────────

fn not_in(en: &Engine, v: &Value) -> FormulaError {
    let s = coerce::to_text(v, en).unwrap_or_default();
    FormulaError::na(format!("\"{s}\" isn't in that list"))
}

fn textual(v: &Value) -> bool {
    matches!(v, Value::Text(_) | Value::Row(_) | Value::Node(_))
}

/// The first (or last) exact match; text with wildcards when asked.
fn find_exact(en: &Engine, list: &[Value], v: &Value, wild: bool, reverse: bool) -> Option<usize> {
    let pat = match (wild, v) {
        (true, Value::Text(s)) => Some(s.to_lowercase()),
        _ => None,
    };
    let hit = |e: &Value| -> bool {
        if matches!(e, Value::Error(_)) {
            return false;
        }
        match &pat {
            Some(p) => {
                textual(e)
                    && coerce::to_text(e, en)
                        .map(|t| criteria::wildcard(p, &t.to_lowercase()))
                        .unwrap_or(false)
            }
            None => coerce::equal(v, e, en).unwrap_or(false),
        }
    };
    if reverse {
        list.iter().rposition(hit)
    } else {
        list.iter().position(hit)
    }
}

fn same_kind(a: &Value, b: &Value) -> bool {
    matches!(
        (a, b),
        (Value::Number(_), Value::Number(_)) | (Value::Bool(_), Value::Bool(_))
    ) || (textual(a) && textual(b))
}

/// The closest value below (`smaller`) or above, when there is no exact match.
fn find_nearest(
    en: &Engine,
    list: &[Value],
    v: &Value,
    smaller: bool,
    reverse: bool,
) -> Option<usize> {
    if let Some(i) = find_exact(en, list, v, false, reverse) {
        return Some(i);
    }
    let mut best: Option<usize> = None;
    let order: Vec<usize> = if reverse {
        (0..list.len()).rev().collect()
    } else {
        (0..list.len()).collect()
    };
    for i in order {
        let e = &list[i];
        if !same_kind(e, v) {
            continue;
        }
        let Ok(o) = coerce::compare(e, v, en) else {
            continue;
        };
        let fits = if smaller { o.is_lt() } else { o.is_gt() };
        if !fits {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => {
                let ob = coerce::compare(e, &list[b], en).unwrap_or(std::cmp::Ordering::Equal);
                if smaller {
                    ob.is_gt()
                } else {
                    ob.is_lt()
                }
            }
        };
        if better {
            best = Some(i);
        }
    }
    best
}

/// `MATCH`'s sorted modes: the last value at most (or at least) `v`.
fn find_sorted(en: &Engine, list: &[Value], v: &Value, at_most: bool) -> Option<usize> {
    let mut found = None;
    for (i, e) in list.iter().enumerate() {
        if e.is_blank() || !same_kind(e, v) {
            continue;
        }
        if let Ok(o) = coerce::compare(e, v, en) {
            if (at_most && o.is_le()) || (!at_most && o.is_ge()) {
                found = Some(i);
            }
        }
    }
    found
}

fn lookup_list(v: &Val, what: &str) -> Result<Vec<Value>, FormulaError> {
    match v {
        Val::Arr(a) if a.width() > 1 && a.height > 1 => Err(FormulaError::value(format!(
            "{what} needs one column or one row to look in"
        ))),
        v => range_values(v),
    }
}

fn xlookup(en: &Engine, a: &Args) -> Result<Val, FormulaError> {
    arity("XLOOKUP", a, 3, 6)?;
    let la = lookup_list(need(a, 1, "XLOOKUP")?, "XLOOKUP")?;
    let ret = as_array(need(a, 2, "XLOOKUP")?)?;
    let mode = trunc(num_arg(a, 4, 0.0)?);
    let search = trunc(num_arg(a, 5, 1.0)?);
    if ![0, -1, 1, 2].contains(&mode) {
        return Err(FormulaError::value("XLOOKUP's match mode is 0, -1, 1 or 2"));
    }
    if ![1, -1, 2, -2].contains(&search) {
        return Err(FormulaError::value(
            "XLOOKUP's search mode is 1, -1, 2 or -2",
        ));
    }
    let reverse = search < 0;
    let vertical = ret.height == la.len();
    let horizontal = !vertical && ret.width() == la.len() && ret.height == 1;
    if !vertical && !horizontal {
        return Err(FormulaError::value(
            "XLOOKUP's lists must be the same length",
        ));
    }
    let not_found = a.get(3).cloned().flatten();
    let one = |v: &Value| -> Result<Val, FormulaError> {
        if let Value::Error(e) = v {
            return Err(e.clone());
        }
        let i = match mode {
            0 => find_exact(en, &la, v, false, reverse),
            2 => find_exact(en, &la, v, true, reverse),
            -1 => find_nearest(en, &la, v, true, reverse),
            _ => find_nearest(en, &la, v, false, reverse),
        };
        let Some(i) = i else {
            return match &not_found {
                Some(nf) => Ok(nf.clone()),
                None => Err(not_in(en, v)),
            };
        };
        Ok(if horizontal {
            Val::One(ret.get(0, i).clone())
        } else if ret.width() == 1 {
            Val::One(ret.get(i, 0).clone())
        } else if let Some(rows) = &ret.rows {
            // A whole table row: the row itself, so `.column` can follow it.
            Val::One(Value::Row(rows[i].clone()))
        } else {
            Val::arr(ret.pick_rows(&[i]))
        })
    };
    match need(a, 0, "XLOOKUP")? {
        Val::One(v) => one(v),
        lv => Ok(map_vals(&[lv], false, &mut |xs| match one(xs[0]) {
            Ok(r) => super::engine::single(r),
            Err(e) => Value::err(e),
        })),
    }
}

fn vlookup(en: &Engine, a: &Args) -> Result<Val, FormulaError> {
    arity("VLOOKUP", a, 3, 4)?;
    let table = as_array(need(a, 1, "VLOOKUP")?)?;
    let k = trunc(num_arg(a, 2, 1.0)?);
    if k < 1 {
        return Err(FormulaError::value("VLOOKUP's column number starts at 1"));
    }
    if k as usize > table.width() {
        return Err(FormulaError::reference(format!(
            "That table has {} column{}",
            table.width(),
            if table.width() == 1 { "" } else { "s" }
        )));
    }
    let approx = bool_arg(a, 3, true)?;
    let first: Vec<Value> = (*table.cols[0].values).clone();
    let col = &table.cols[k as usize - 1].values;
    let lv = need(a, 0, "VLOOKUP")?;
    Ok(map_vals(&[lv], false, &mut |xs| {
        let v = xs[0];
        if let Value::Error(e) = v {
            return Value::err(e.clone());
        }
        let i = if approx {
            find_sorted(en, &first, v, true)
        } else {
            find_exact(en, &first, v, true, false)
        };
        match i {
            Some(i) => col[i].clone(),
            None => Value::err(not_in(en, v)),
        }
    }))
}

fn index(a: &Args) -> Result<Val, FormulaError> {
    arity("INDEX", a, 2, 3)?;
    let arr = as_array(need(a, 0, "INDEX")?)?;
    let r = trunc(num_arg(a, 1, 0.0)?);
    let c_given = matches!(a.get(2), Some(Some(_)));
    let mut c = trunc(num_arg(a, 2, 0.0)?);
    let mut r = r;
    if !c_given {
        if arr.height == 1 && arr.width() > 1 {
            c = r;
            r = 1;
        } else {
            c = 1;
        }
    }
    let out = || FormulaError::reference("That is outside the list");
    if r < 0 || c < 0 || r as usize > arr.height || c as usize > arr.width() {
        return Err(out());
    }
    match (r, c) {
        (0, 0) => Ok(Val::Arr(arr)),
        (0, c) => Ok(Val::arr(arr.pick_cols(c as usize - 1, c as usize - 1))),
        (r, 0) => Ok(Val::arr(arr.pick_rows(&[r as usize - 1]))),
        (r, c) => Ok(Val::One(arr.get(r as usize - 1, c as usize - 1).clone())),
    }
}

fn match_(en: &Engine, a: &Args, x: bool) -> Result<Val, FormulaError> {
    let name = if x { "XMATCH" } else { "MATCH" };
    arity(name, a, 2, if x { 4 } else { 3 })?;
    let list = lookup_list(need(a, 1, name)?, name)?;
    let mode = trunc(num_arg(a, 2, if x { 0.0 } else { 1.0 })?);
    let search = trunc(num_arg(a, 3, 1.0)?);
    let lv = need(a, 0, name)?;
    Ok(map_vals(&[lv], false, &mut |xs| {
        let v = xs[0];
        if let Value::Error(e) = v {
            return Value::err(e.clone());
        }
        let i = if x {
            let rev = search < 0;
            match mode {
                0 => find_exact(en, &list, v, false, rev),
                2 => find_exact(en, &list, v, true, rev),
                -1 => find_nearest(en, &list, v, true, rev),
                1 => find_nearest(en, &list, v, false, rev),
                _ => {
                    return Value::err(FormulaError::value("XMATCH's match mode is 0, -1, 1 or 2"))
                }
            }
        } else {
            match mode {
                0 => find_exact(en, &list, v, true, false),
                m if m > 0 => find_sorted(en, &list, v, true),
                _ => find_sorted(en, &list, v, false),
            }
        };
        match i {
            Some(i) => Value::Number((i + 1) as f64),
            None => Value::err(not_in(en, v)),
        }
    }))
}

// ── Lists ───────────────────────────────────────────────────────────────

fn filter(a: &Args) -> Result<Val, FormulaError> {
    arity("FILTER", a, 2, 3)?;
    let arr = as_array(need(a, 0, "FILTER")?)?;
    let inc = range_values(need(a, 1, "FILTER")?)?;
    if inc.len() != arr.height {
        return Err(FormulaError::value(format!(
            "FILTER()'s list has {} rows and its condition {}",
            arr.height,
            inc.len()
        )));
    }
    let mut keep = Vec::new();
    for (i, v) in inc.iter().enumerate() {
        if coerce::to_bool(v)? {
            keep.push(i);
        }
    }
    if keep.is_empty() {
        return match a.get(2) {
            Some(Some(v)) => Ok(v.clone()),
            _ => Err(FormulaError::calc("Nothing matches the filter")),
        };
    }
    Ok(Val::arr(arr.pick_rows(&keep)))
}

fn sort(en: &Engine, a: &Args) -> Result<Val, FormulaError> {
    arity("SORT", a, 1, 4)?;
    let v = need(a, 0, "SORT")?;
    let Val::Arr(arr) = v else {
        return Ok(v.clone());
    };
    let by = trunc(num_arg(a, 1, 1.0)?);
    let order = trunc(num_arg(a, 2, 1.0)?);
    if bool_arg(a, 3, false)? {
        return Err(FormulaError::value("SORT by columns isn't supported yet"));
    }
    if by < 1 || by as usize > arr.width() {
        return Err(FormulaError::value(format!(
            "SORT(): there is no column {by}"
        )));
    }
    if order != 1 && order != -1 {
        return Err(FormulaError::value(
            "SORT()'s order is 1 (ascending) or -1 (descending)",
        ));
    }
    let col = &arr.cols[by as usize - 1].values;
    let mut idx: Vec<usize> = (0..arr.height).collect();
    idx.sort_by(|&p, &q| ordered(&col[p], &col[q], order < 0, en));
    Ok(Val::arr(arr.pick_rows(&idx)))
}

/// Sort order, ascending or descending; blanks go last either way.
fn ordered(a: &Value, b: &Value, descending: bool, en: &Engine) -> std::cmp::Ordering {
    match (a.is_blank(), b.is_blank()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => {
            let o = coerce::sort_cmp(a, b, en);
            if descending {
                o.reverse()
            } else {
                o
            }
        }
    }
}

fn row_key(arr: &Array, r: usize) -> String {
    arr.cols
        .iter()
        .map(|c| coerce::group_key(&c.values[r]))
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

fn unique(a: &Args) -> Result<Val, FormulaError> {
    arity("UNIQUE", a, 1, 3)?;
    let v = need(a, 0, "UNIQUE")?;
    let Val::Arr(arr) = v else {
        return Ok(v.clone());
    };
    if bool_arg(a, 1, false)? {
        return Err(FormulaError::value("UNIQUE by columns isn't supported yet"));
    }
    let once = bool_arg(a, 2, false)?;
    let mut count: HashMap<String, usize> = HashMap::new();
    let mut first: Vec<(String, usize)> = Vec::new();
    for r in 0..arr.height {
        let k = row_key(arr, r);
        let e = count.entry(k.clone()).or_insert(0);
        if *e == 0 {
            first.push((k, r));
        }
        *e += 1;
    }
    let idx: Vec<usize> = first
        .into_iter()
        .filter(|(k, _)| !once || count.get(k) == Some(&1))
        .map(|(_, r)| r)
        .collect();
    if idx.is_empty() {
        return Err(FormulaError::calc("No value appears exactly once"));
    }
    Ok(Val::arr(arr.pick_rows(&idx)))
}

fn hstack(a: &Args) -> Result<Val, FormulaError> {
    arity("HSTACK", a, 1, 255)?;
    let arrays: Vec<Rc<Array>> = a.iter().flatten().map(as_array).collect::<Result<_, _>>()?;
    let h = arrays.iter().map(|x| x.height).max().unwrap_or(0);
    let pad = || {
        Value::err(FormulaError::na(
            "HSTACK() filled this in: its lists are different lengths",
        ))
    };
    let mut cols = Vec::new();
    for x in &arrays {
        for c in &x.cols {
            let mut values = (*c.values).clone();
            values.resize_with(h, pad);
            cols.push(Column {
                meta: c.meta.clone(),
                values: Rc::new(values),
            });
        }
    }
    let rows = arrays[0].rows.clone().filter(|r| {
        arrays
            .iter()
            .all(|x| x.height == h && x.rows.as_ref().is_some_and(|o| o == r))
    });
    Ok(Val::arr(Array::from_columns(cols, rows)))
}

fn groupby(en: &Engine, a: &Args) -> Result<Val, FormulaError> {
    arity("GROUPBY", a, 3, 8)?;
    let keys = as_array(need(a, 0, "GROUPBY")?)?;
    let values = as_array(need(a, 1, "GROUPBY")?)?;
    let agg = match need(a, 2, "GROUPBY")? {
        Val::Func(f) => *f,
        _ => {
            return Err(FormulaError::value(
                "GROUPBY needs a function like SUM, AVERAGE or COUNT as its third input",
            ))
        }
    };
    if keys.height != values.height {
        return Err(FormulaError::value(format!(
            "GROUPBY()'s groups have {} rows and its values {}",
            keys.height, values.height
        )));
    }
    let total = trunc(num_arg(a, 4, 0.0)?);
    if !(-1..=1).contains(&total) {
        return Err(FormulaError::value(
            "GROUPBY()'s subtotals aren't supported yet; its totals are 0 (none), 1 or -1",
        ));
    }
    let nk = keys.width();
    let ncols = nk + values.width();
    let sort_by = trunc(num_arg(a, 5, 1.0)?);
    if sort_by == 0 || sort_by.unsigned_abs() as usize > ncols {
        return Err(FormulaError::value(format!(
            "GROUPBY() can sort by a column from 1 to {ncols}"
        )));
    }
    let include: Option<Vec<Value>> = match a.get(6) {
        Some(Some(v)) => {
            let inc = range_values(v)?;
            if inc.len() != keys.height {
                return Err(FormulaError::value(
                    "GROUPBY()'s filter must be as long as its groups",
                ));
            }
            Some(inc)
        }
        _ => None,
    };
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, (usize, Vec<usize>)> = HashMap::new();
    let mut all = Vec::new();
    for r in 0..keys.height {
        if let Some(inc) = &include {
            if !coerce::to_bool(&inc[r])? {
                continue;
            }
        }
        for c in 0..nk {
            if let Value::Error(e) = keys.get(r, c) {
                return Err(e.clone());
            }
        }
        all.push(r);
        let k = row_key(&keys, r);
        groups
            .entry(k.clone())
            .or_insert_with(|| {
                order.push(k);
                (r, Vec::new())
            })
            .1
            .push(r);
    }
    // One row per group: its keys, then each value column aggregated.
    let mut rows: Vec<Vec<Value>> = order
        .iter()
        .map(|k| {
            let (first, members) = &groups[k];
            let mut row: Vec<Value> = (0..nk).map(|c| keys.get(*first, c).clone()).collect();
            for c in 0..values.width() {
                let vs: Vec<Value> = members.iter().map(|&r| values.get(r, c).clone()).collect();
                row.push(aggregate_list(agg, &vs));
            }
            row
        })
        .collect();
    let sc = sort_by.unsigned_abs() as usize - 1;
    rows.sort_by(|p, q| {
        let mut o = ordered(&p[sc], &q[sc], sort_by < 0, en);
        for c in 0..nk {
            if o.is_ne() {
                break;
            }
            o = coerce::sort_cmp(&p[c], &q[c], en);
        }
        o
    });
    if total != 0 {
        let mut row: Vec<Value> = (0..nk)
            .map(|c| {
                if c == 0 {
                    Value::Text("Total".into())
                } else {
                    Value::Blank
                }
            })
            .collect();
        for c in 0..values.width() {
            let vs: Vec<Value> = all.iter().map(|&r| values.get(r, c).clone()).collect();
            row.push(aggregate_list(agg, &vs));
        }
        if total > 0 {
            rows.push(row);
        } else {
            rows.insert(0, row);
        }
    }
    let counts = matches!(agg, Agg::Count | Agg::CountA | Agg::CountBlank);
    let metas: Vec<ColMeta> = keys
        .cols
        .iter()
        .map(|c| c.meta.clone())
        .chain(values.cols.iter().map(|c| ColMeta {
            name: c.meta.name.clone(),
            format: if counts {
                crate::model::Format::Number
            } else {
                c.meta.format
            },
            link: None,
        }))
        .collect();
    let cols = metas
        .into_iter()
        .enumerate()
        .map(|(j, meta)| Column {
            meta,
            values: Rc::new(rows.iter().map(|r| r[j].clone()).collect()),
        })
        .collect();
    Ok(Val::arr(Array::from_columns(cols, None)))
}

fn choose(a: &Args) -> Result<Val, FormulaError> {
    arity("CHOOSE", a, 2, 255)?;
    let pick = |x: f64| -> Result<usize, FormulaError> {
        let i = trunc(x);
        if i < 1 || i as usize >= a.len() {
            return Err(FormulaError::value(format!(
                "CHOOSE() has {} choices; {i} isn't one",
                a.len() - 1
            )));
        }
        Ok(i as usize)
    };
    match need(a, 0, "CHOOSE")? {
        Val::One(v) => {
            let i = pick(coerce::to_number(v)?)?;
            Ok(a[i].clone().unwrap_or(Val::One(Value::Blank)))
        }
        _ => {
            let opts: Vec<Option<&Val>> = a.iter().map(|v| v.as_ref()).collect();
            Ok(map_opt(&opts, true, &mut |xs| {
                val(need_num(xs[0])
                    .and_then(pick)
                    .map(|i| xs[i].cloned().unwrap_or(Value::Blank)))
            }))
        }
    }
}

fn textjoin(en: &Engine, a: &Args) -> Result<Val, FormulaError> {
    arity("TEXTJOIN", a, 3, 255)?;
    let delim = coerce::to_text(&scalar(a, 0)?.unwrap_or(Value::Blank), en)?;
    let skip = bool_arg(a, 1, true)?;
    let mut parts = Vec::new();
    for v in a[2..].iter().flatten() {
        for x in range_values(v)? {
            let s = coerce::to_text(&x, en)?;
            if !(skip && s.is_empty()) {
                parts.push(s);
            }
        }
    }
    Ok(Val::One(Value::Text(parts.join(&delim))))
}

// ── Elementwise functions ───────────────────────────────────────────────

pub(crate) fn power(x: f64, y: f64) -> Result<f64, FormulaError> {
    if x == 0.0 && y == 0.0 {
        return Err(FormulaError::num("0^0 has no answer"));
    }
    if x == 0.0 && y < 0.0 {
        return Err(FormulaError::div0());
    }
    let r = x.powf(y);
    if !r.is_finite() {
        return Err(FormulaError::num(if x < 0.0 {
            "A negative number has no fractional power"
        } else {
            "The result is too large"
        }));
    }
    Ok(r)
}

fn date_arg(x: Option<&Value>) -> Result<f64, FormulaError> {
    let s = need_num(x)?;
    if !dates::in_range(s) {
        return Err(FormulaError::num(format!(
            "{} isn't a date",
            coerce::general(s)
        )));
    }
    Ok(s)
}

fn checked_serial(s: i64) -> Result<Value, FormulaError> {
    if !(0..=dates::MAX_SERIAL).contains(&s) {
        return Err(FormulaError::num("That date is out of range"));
    }
    Ok(Value::Number(s as f64))
}

fn proper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_letter = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if prev_letter {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_letter = true;
        } else {
            out.push(c);
            prev_letter = false;
        }
    }
    out
}

fn elementwise(en: &Engine, name: &str, a: &Args) -> Val {
    let (min, max) = match name {
        "ABS" | "INT" | "SIGN" | "SQRT" | "YEAR" | "MONTH" | "DAY" | "LEN" | "UPPER" | "LOWER"
        | "PROPER" | "TRIM" | "VALUE" | "NOT" | "ISBLANK" | "ISNUMBER" | "ISTEXT" | "ISERROR"
        | "ISNA" => (1, 1),
        "ROUND" | "ROUNDUP" | "ROUNDDOWN" | "MOD" | "POWER" | "EOMONTH" | "EDATE" | "DAYS"
        | "TEXT" => (2, 2),
        "LEFT" | "RIGHT" | "WEEKDAY" => (1, 2),
        "REPT" => (2, 2),
        "MID" | "DATE" => (3, 3),
        "SWITCH" => (3, 255),
        "CONCATENATE" => (1, 255),
        _ => return Val::err(unknown_function(name)),
    };
    if let Err(e) = arity(name, a, min, max) {
        return Val::err(e);
    }
    let keep = matches!(
        name,
        "ABS" | "ROUND" | "ROUNDUP" | "ROUNDDOWN" | "INT" | "SWITCH"
    );
    let opts: Vec<Option<&Val>> = a.iter().map(|v| v.as_ref()).collect();
    map_opt(&opts, keep, &mut |xs| val(scalar_fn(en, name, xs)))
}

fn scalar_fn(en: &Engine, name: &str, xs: &[Option<&Value>]) -> Result<Value, FormulaError> {
    let x0 = xs.first().copied().flatten();
    let n = |i: usize| need_num(xs.get(i).copied().flatten());
    let t = |i: usize| coerce::to_text(xs.get(i).copied().flatten().unwrap_or(&BLANK), en);
    let num = |r: f64| -> Result<Value, FormulaError> {
        if r.is_finite() {
            Ok(Value::Number(r))
        } else {
            Err(FormulaError::num("The result is too large"))
        }
    };
    match name {
        "ABS" => num(n(0)?.abs()),
        "INT" => num(n(0)?.floor()),
        "SIGN" => {
            let x = n(0)?;
            num(if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            })
        }
        "SQRT" => {
            let x = n(0)?;
            if x < 0.0 {
                return Err(FormulaError::num("A negative number has no square root"));
            }
            num(x.sqrt())
        }
        "ROUND" => num(format::round_half_away(
            n(0)?,
            trunc(n(1)?).clamp(-400, 400) as i32,
        )),
        "ROUNDUP" => num(format::scaled(
            n(0)?,
            trunc(n(1)?).clamp(-400, 400) as i32,
            |y| y.signum() * y.abs().ceil(),
        )),
        "ROUNDDOWN" => num(format::scaled(
            n(0)?,
            trunc(n(1)?).clamp(-400, 400) as i32,
            f64::trunc,
        )),
        "MOD" => {
            let (x, y) = (n(0)?, n(1)?);
            if y == 0.0 {
                return Err(FormulaError::div0());
            }
            num(format::trim15(x - y * (x / y).floor()))
        }
        "POWER" => num(power(n(0)?, n(1)?)?),
        "DATE" => {
            let (mut y, m, d) = (trunc(n(0)?), trunc(n(1)?), trunc(n(2)?));
            if (0..1900).contains(&y) {
                y += 1900;
            }
            if !(0..10_000).contains(&y) {
                return Err(FormulaError::num("DATE()'s year is from 1900 to 9999"));
            }
            checked_serial(dates::serial(y, m, d))
        }
        "YEAR" => Ok(Value::Number(dates::ymd(date_arg(x0)?).0 as f64)),
        "MONTH" => Ok(Value::Number(dates::ymd(date_arg(x0)?).1 as f64)),
        "DAY" => Ok(Value::Number(dates::ymd(date_arg(x0)?).2 as f64)),
        "WEEKDAY" => {
            let d = date_arg(x0)?;
            let w = dates::weekday(d) as i64; // Sunday 0
            let ty = trunc(opt_num(xs.get(1).copied().flatten(), 1.0)?);
            let r = match ty {
                1 | 17 => w + 1,
                2 | 11 => (w + 6) % 7 + 1,
                3 => (w + 6) % 7,
                12..=16 => (w - (ty - 10) + 7) % 7 + 1,
                _ => return Err(FormulaError::num("WEEKDAY()'s type is 1, 2, 3 or 11 to 17")),
            };
            Ok(Value::Number(r as f64))
        }
        "EOMONTH" => {
            let (y, m, _) = dates::ymd(date_arg(x0)?);
            let k = trunc(n(1)?);
            checked_serial(dates::serial(y, m as i64 + k + 1, 0))
        }
        "EDATE" => {
            let (y, m, d) = dates::ymd(date_arg(x0)?);
            let k = trunc(n(1)?);
            let total = y * 12 + (m as i64 - 1) + k;
            let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
            if !(0..10_000).contains(&ny) {
                return Err(FormulaError::num("That date is out of range"));
            }
            let nd = d.min(dates::days_in_month(ny, nm));
            checked_serial(dates::serial(ny, nm as i64, nd as i64))
        }
        "DAYS" => num((n(0)?.trunc()) - (n(1)?.trunc())),
        "TEXT" => {
            let f = t(1)?;
            match x0.unwrap_or(&BLANK) {
                Value::Error(e) => Err(e.clone()),
                Value::Blank => format::text_number(0.0, &f).map(Value::Text),
                Value::Number(x) => format::text_number(*x, &f).map(Value::Text),
                Value::Bool(b) => Ok(Value::Text(if *b { "TRUE" } else { "FALSE" }.into())),
                Value::Text(s) => match coerce::parse_number_text(s) {
                    Some(x) => format::text_number(x, &f).map(Value::Text),
                    None => Ok(Value::Text(format::text_text(s, &f))),
                },
                v => Ok(Value::Text(format::text_text(&coerce::to_text(v, en)?, &f))),
            }
        }
        "LEFT" | "RIGHT" => {
            let s = t(0)?;
            let k = opt_num(xs.get(1).copied().flatten(), 1.0)?;
            if k < 0.0 {
                return Err(FormulaError::value(format!(
                    "{name}() can't take fewer than 0 characters"
                )));
            }
            let k = trunc(k) as usize;
            let chars: Vec<char> = s.chars().collect();
            let k = k.min(chars.len());
            Ok(Value::Text(if name == "LEFT" {
                chars[..k].iter().collect()
            } else {
                chars[chars.len() - k..].iter().collect()
            }))
        }
        "REPT" => {
            // Excel: the count is truncated; a negative one, or a result over
            // 32,767 characters, is #VALUE!.
            let s = t(0)?;
            let k = n(1)?;
            if k < 0.0 {
                return Err(FormulaError::value("REPT() can't repeat fewer than 0 times"));
            }
            let k = trunc(k) as usize;
            if s.chars().count().saturating_mul(k) > 32_767 {
                return Err(FormulaError::value(
                    "REPT() would make more than 32,767 characters",
                ));
            }
            Ok(Value::Text(s.repeat(k)))
        }
        "MID" => {
            let s = t(0)?;
            let (start, k) = (n(1)?, n(2)?);
            if start < 1.0 || k < 0.0 {
                return Err(FormulaError::value(
                    "MID() starts at 1 and takes 0 or more characters",
                ));
            }
            Ok(Value::Text(
                s.chars()
                    .skip(trunc(start) as usize - 1)
                    .take(trunc(k) as usize)
                    .collect(),
            ))
        }
        "LEN" => Ok(Value::Number(t(0)?.chars().count() as f64)),
        "UPPER" => Ok(Value::Text(t(0)?.to_uppercase())),
        "LOWER" => Ok(Value::Text(t(0)?.to_lowercase())),
        "PROPER" => Ok(Value::Text(proper(&t(0)?))),
        "TRIM" => Ok(Value::Text(
            t(0)?
                .split(' ')
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        )),
        "VALUE" => match x0.unwrap_or(&BLANK) {
            Value::Number(x) => Ok(Value::Number(*x)),
            Value::Blank => Ok(Value::Number(0.0)),
            Value::Error(e) => Err(e.clone()),
            v => {
                let s = coerce::to_text(v, en)?;
                coerce::parse_number_text(&s)
                    .map(Value::Number)
                    .ok_or_else(|| FormulaError::value(format!("\"{s}\" isn't a number")))
            }
        },
        "CONCATENATE" => {
            let mut s = String::new();
            for x in xs {
                s.push_str(&coerce::to_text(x.unwrap_or(&BLANK), en)?);
            }
            Ok(Value::Text(s))
        }
        "NOT" => Ok(Value::Bool(!coerce::to_bool(x0.unwrap_or(&BLANK))?)),
        "ISBLANK" => Ok(Value::Bool(matches!(x0, None | Some(Value::Blank)))),
        "ISNUMBER" => Ok(Value::Bool(matches!(x0, Some(Value::Number(_))))),
        "ISTEXT" => Ok(Value::Bool(matches!(x0, Some(Value::Text(_))))),
        "ISERROR" => Ok(Value::Bool(matches!(x0, Some(Value::Error(_))))),
        "ISNA" => Ok(Value::Bool(
            matches!(x0, Some(Value::Error(e)) if e.code == ErrorCode::NA),
        )),
        "SWITCH" => {
            let v = x0.unwrap_or(&BLANK);
            if let Value::Error(e) = v {
                return Err(e.clone());
            }
            let rest = &xs[1..];
            for p in rest.chunks(2) {
                if p.len() == 2 && coerce::equal(v, p[0].unwrap_or(&BLANK), en)? {
                    return Ok(p[1].cloned().unwrap_or(Value::Blank));
                }
            }
            if rest.len() % 2 == 1 {
                return Ok(rest[rest.len() - 1].cloned().unwrap_or(Value::Blank));
            }
            Err(FormulaError::na("SWITCH() found no match"))
        }
        _ => Err(unknown_function(name)),
    }
}
