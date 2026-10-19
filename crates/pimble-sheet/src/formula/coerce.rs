//! Excel's conversions between kinds of values, and its comparison rules.
//!
//! Text that looks like a number is a number in arithmetic (`"3"+1` is 4),
//! a blank is 0 or "" as needed, and comparing different kinds ranks
//! numbers below text below booleans. Numbers are equal when they agree to
//! Excel's 15 significant digits, so `0.1+0.2=0.3` is TRUE. A link shows as
//! its label wherever text is wanted, and two links are equal only when
//! they point at the same row or node.

use std::cmp::Ordering;

use crate::model::NodeRef;

use super::dates;
use super::value::{FormulaError, RowRef, Value};

/// How links read as text: a row by its label, a node by its title.
pub(crate) trait Labels {
    fn row_label(&self, row: &RowRef) -> String;
    fn node_title(&self, node: &NodeRef) -> String;
}

pub(crate) fn to_number(v: &Value) -> Result<f64, FormulaError> {
    match v {
        Value::Blank => Ok(0.0),
        Value::Number(x) => Ok(*x),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Text(s) => parse_number_text(s)
            .ok_or_else(|| FormulaError::value(format!("\"{s}\" isn't a number"))),
        Value::Row(_) | Value::Node(_) => Err(FormulaError::value(
            "A link isn't a number; follow it to a number with a dot, like [@vehicle].mpg",
        )),
        Value::Error(e) => Err(e.clone()),
    }
}

pub(crate) fn to_text(v: &Value, labels: &dyn Labels) -> Result<String, FormulaError> {
    match v {
        Value::Blank => Ok(String::new()),
        Value::Number(x) => Ok(general(*x)),
        Value::Text(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_string()),
        Value::Row(r) => Ok(labels.row_label(r)),
        Value::Node(n) => Ok(labels.node_title(n)),
        Value::Error(e) => Err(e.clone()),
    }
}

/// A condition's truth: a nonzero number, TRUE, or the text "TRUE".
pub(crate) fn to_bool(v: &Value) -> Result<bool, FormulaError> {
    match v {
        Value::Blank => Ok(false),
        Value::Number(x) => Ok(*x != 0.0),
        Value::Bool(b) => Ok(*b),
        Value::Text(s) => {
            if s.eq_ignore_ascii_case("true") {
                Ok(true)
            } else if s.eq_ignore_ascii_case("false") {
                Ok(false)
            } else {
                Err(FormulaError::value(format!("\"{s}\" isn't TRUE or FALSE")))
            }
        }
        Value::Row(_) | Value::Node(_) => Err(FormulaError::value("A link isn't TRUE or FALSE")),
        Value::Error(e) => Err(e.clone()),
    }
}

/// Text as a number the way Excel reads it: `1,234.5`, `$12`, `-3`, `1e3`,
/// `10%`, an ISO date (its serial), or a time `1:30` (a fraction of a day).
pub(crate) fn parse_number_text(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(d) = dates::parse_iso(t) {
        return Some(d);
    }
    if let Some(time) = parse_time(t) {
        return Some(time);
    }
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r.trim_start()),
        None => (false, t.strip_prefix('+').unwrap_or(t).trim_start()),
    };
    let rest = rest.strip_prefix('$').unwrap_or(rest).trim_start();
    let mut pct = 0;
    let mut body = rest;
    while let Some(b) = body.strip_suffix('%') {
        pct += 1;
        body = b.trim_end();
    }
    // Commas group the integer part only: `1,234.5`, never `1,2`.
    let (int, frac) = match body.find(['.', 'e', 'E']) {
        Some(i) => (&body[..i], &body[i..]),
        None => (body, ""),
    };
    if int.contains(',') {
        let groups: Vec<&str> = int.split(',').collect();
        let ok = !groups[0].is_empty()
            && groups[0].len() <= 3
            && groups[1..].iter().all(|g| g.len() == 3);
        if !ok {
            return None;
        }
    }
    let digits: String = int.chars().filter(|c| *c != ',').collect::<String>() + frac;
    if digits.is_empty()
        || !digits
            .bytes()
            .all(|b| b.is_ascii_digit() || b"eE+-.".contains(&b))
    {
        return None;
    }
    if !digits
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_digit() || b == b'.')
    {
        return None;
    }
    let mut v: f64 = digits.parse().ok().filter(|v: &f64| v.is_finite())?;
    for _ in 0..pct {
        v /= 100.0;
    }
    Some(if neg { -v } else { v })
}

/// `h:mm` or `h:mm:ss` as a fraction of a day.
fn parse_time(t: &str) -> Option<f64> {
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let mut n = [0.0f64; 3];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        n[i] = p.parse().ok()?;
    }
    if n[1] >= 60.0 || n[2] >= 60.0 {
        return None;
    }
    Some((n[0] * 3600.0 + n[1] * 60.0 + n[2]) / 86_400.0)
}

/// A number as Excel turns it into text: up to 15 significant digits, and
/// scientific notation for very large or very small numbers.
pub fn general(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return "0".to_string();
    }
    let s = format!("{:.14e}", x);
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if neg { "-" } else { "" };
    if !(-10..15).contains(&exp) {
        let (first, rest) = digits.split_at(1);
        let m = if rest.is_empty() {
            first.to_string()
        } else {
            format!("{first}.{rest}")
        };
        let es = if exp < 0 { "-" } else { "+" };
        return format!("{sign}{m}E{es}{:02}", exp.abs());
    }
    if exp >= 0 {
        let e = exp as usize;
        if digits.len() <= e + 1 {
            format!("{sign}{digits}{}", "0".repeat(e + 1 - digits.len()))
        } else {
            format!("{sign}{}.{}", &digits[..=e], &digits[e + 1..])
        }
    } else {
        format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
    }
}

/// Numbers equal to 15 significant digits.
pub(crate) fn num_eq(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= a.abs().max(b.abs()) * 1e-15
}

fn num_cmp(a: f64, b: f64) -> Ordering {
    if num_eq(a, b) {
        Ordering::Equal
    } else if a < b {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

/// The kinds, in Excel's comparison rank.
fn rank(v: &Value) -> u8 {
    match v {
        Value::Number(_) => 0,
        Value::Text(_) | Value::Row(_) | Value::Node(_) => 1,
        Value::Bool(_) => 2,
        Value::Blank => 3,
        Value::Error(_) => 4,
    }
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// A blank compared with something is that kind's empty value.
fn unblank(v: &Value, other: &Value) -> Value {
    if !v.is_blank() {
        return v.clone();
    }
    match other {
        Value::Number(_) => Value::Number(0.0),
        Value::Bool(_) => Value::Bool(false),
        Value::Blank => Value::Number(0.0),
        _ => Value::Text(String::new()),
    }
}

/// Excel's `<`/`>` order of two values.
pub(crate) fn compare(a: &Value, b: &Value, labels: &dyn Labels) -> Result<Ordering, FormulaError> {
    if let Value::Error(e) = a {
        return Err(e.clone());
    }
    if let Value::Error(e) = b {
        return Err(e.clone());
    }
    let a = unblank(a, b);
    let b = unblank(b, &a);
    let (ra, rb) = (rank(&a), rank(&b));
    if ra != rb {
        return Ok(ra.cmp(&rb));
    }
    Ok(match (&a, &b) {
        (Value::Number(x), Value::Number(y)) => num_cmp(*x, *y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => lower(&to_text(&a, labels)?).cmp(&lower(&to_text(&b, labels)?)),
    })
}

/// Excel's `=`: links by identity, text without regard to case.
pub(crate) fn equal(a: &Value, b: &Value, labels: &dyn Labels) -> Result<bool, FormulaError> {
    match (a, b) {
        (Value::Row(x), Value::Row(y)) => Ok(x == y),
        (Value::Node(x), Value::Node(y)) => Ok(x == y),
        _ => Ok(compare(a, b, labels)? == Ordering::Equal),
    }
}

/// The order `SORT` and `GROUPBY` put values in: numbers, text, booleans,
/// errors, and blanks last.
pub(crate) fn sort_cmp(a: &Value, b: &Value, labels: &dyn Labels) -> Ordering {
    let (ra, rb) = (rank(a), rank(b));
    let (ra, rb) = (sort_rank(ra), sort_rank(rb));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Error(_), Value::Error(_)) | (Value::Blank, Value::Blank) => Ordering::Equal,
        _ => {
            let x = to_text(a, labels).unwrap_or_default();
            let y = to_text(b, labels).unwrap_or_default();
            lower(&x).cmp(&lower(&y))
        }
    }
}

fn sort_rank(r: u8) -> u8 {
    // numbers, text, booleans, errors, blanks
    match r {
        3 => 4,
        4 => 3,
        r => r,
    }
}

/// A key that groups equal values (`UNIQUE`, `GROUPBY`): text without
/// regard to case, numbers to 15 digits, links by identity.
pub(crate) fn group_key(v: &Value) -> String {
    match v {
        Value::Blank => "_".to_string(),
        Value::Number(x) => format!("n{}", general(*x)),
        Value::Text(s) if s.is_empty() => "_".to_string(),
        Value::Text(s) => format!("s{}", lower(s)),
        Value::Bool(b) => format!("b{b}"),
        Value::Row(r) => format!("r{}/{}/{}/{}", r.node.store, r.node.node, r.table, r.row),
        Value::Node(n) => format!("o{}/{}", n.store, n.node),
        Value::Error(e) => format!("e{}", e.code.code()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_numbers() {
        assert_eq!(general(1.0 / 3.0), "0.333333333333333");
        assert_eq!(general(2.0 / 3.0), "0.666666666666667");
        assert_eq!(general(0.1 + 0.2), "0.3");
        assert_eq!(general(1234.5), "1234.5");
        assert_eq!(general(-42.0), "-42");
        assert_eq!(general(1e15), "1E+15");
        assert_eq!(general(123_456_789_012_345.0), "123456789012345");
        assert_eq!(general(1.5e20), "1.5E+20");
        assert_eq!(general(0.000_001), "0.000001");
        assert_eq!(general(1e-12), "1E-12");
        assert_eq!(general(100.0), "100");
    }

    #[test]
    fn number_text() {
        assert_eq!(parse_number_text("1,234.5"), Some(1234.5));
        assert_eq!(parse_number_text("$12"), Some(12.0));
        assert_eq!(parse_number_text("-$12"), Some(-12.0));
        assert_eq!(parse_number_text("10%"), Some(0.1));
        assert_eq!(parse_number_text("1e3"), Some(1000.0));
        assert_eq!(parse_number_text("1:30"), Some(0.0625));
        assert_eq!(parse_number_text("2026-10-19"), Some(46_314.0));
        assert_eq!(parse_number_text("1,2"), None);
        assert_eq!(parse_number_text("abc"), None);
        assert_eq!(parse_number_text("inf"), None);
        assert_eq!(parse_number_text("NaN"), None);
        assert_eq!(parse_number_text(""), None);
    }
}
