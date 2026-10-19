//! What a person types into a cell, parsed by the column's type (docs/SHEETS_CONTRACT.md
//! section 3), and the text shown when a cell is edited again.
//!
//! Typed text that does not parse as its column's type is kept as typed
//! ([`Stored::Unparsed`]) and shows `#VALUE!` until it is fixed.

use chrono::{Datelike, Duration, NaiveDate};

use crate::model::{ColumnType, Stored};

/// Parse what was typed into a cell of a column of type `ty`.
///
/// `None` is a blank cell (nothing but whitespace was typed). `today` is the
/// caller's idea of today, so parsing is pure. `link` resolves the trimmed
/// text of a link column to the row or node it names (the caller knows the
/// choices); `None` from it makes the cell [`Stored::Unparsed`].
///
/// A formula column takes no typed input: this answers `None` for it, and
/// the caller handles text that starts with `=` (it turns the column into
/// that formula) before it gets here.
pub fn parse_input(ty: &ColumnType, text: &str, today: NaiveDate, link: &dyn Fn(&str) -> Option<Stored>) -> Option<Stored> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let unparsed = || Stored::Unparsed(text.to_string());
    match ty {
        ColumnType::Text => Some(Stored::Text(text.to_string())),
        ColumnType::Number | ColumnType::Money => Some(parse_number(text).map(Stored::Number).unwrap_or_else(unparsed)),
        ColumnType::Hours => Some(parse_hours(text).map(Stored::Number).unwrap_or_else(unparsed)),
        ColumnType::Date => Some(parse_date(text, today).map(|d| Stored::Date(d.format("%Y-%m-%d").to_string())).unwrap_or_else(unparsed)),
        ColumnType::RowLink { .. } | ColumnType::NodeLink { .. } => Some(link(text).unwrap_or_else(unparsed)),
        ColumnType::Formula { .. } => None,
    }
}

/// `1234.5`, `1,234.5`, `$1,234.50`, `-3`, `.5`. Strict: anything else is not
/// a number. Commas must group by three.
fn parse_number(text: &str) -> Option<f64> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut rest = t.as_str();
    let mut negative = false;
    if let Some(r) = rest.strip_prefix('-').or_else(|| rest.strip_prefix('\u{2212}')) {
        negative = true;
        rest = r;
    }
    rest = rest.strip_prefix('$').unwrap_or(rest);
    if !negative {
        // `$-3` as well as `-$3`.
        if let Some(r) = rest.strip_prefix('-') {
            negative = true;
            rest = r;
        }
    }
    let (int, frac) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    if int.contains(',') {
        let mut groups = int.split(',');
        let first = groups.next()?;
        if first.is_empty() || first.len() > 3 || !first.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        for g in groups {
            if g.len() != 3 || !g.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
        }
    } else if !int.bytes().all(|b| b.is_ascii_digit()) || (int.is_empty() && frac.is_none()) {
        return None;
    }
    let clean: String = format!("{}{}{}", if int.is_empty() { "0" } else { "" }, int.replace(',', ""), frac.map(|f| format!(".{f}")).unwrap_or_default());
    let v: f64 = clean.parse().ok()?;
    v.is_finite().then_some(if negative { -v } else { v })
}

/// Leading ASCII digits and the rest.
fn split_digits(s: &str) -> (&str, &str) {
    let end = s.bytes().position(|b| !b.is_ascii_digit()).unwrap_or(s.len());
    s.split_at(end)
}

/// A decimal number (`1.5`, `.5`, `2`) that fills `s` exactly.
fn decimal(s: &str) -> Option<f64> {
    let (int, rest) = split_digits(s);
    let frac = match rest.strip_prefix('.') {
        Some(f) => {
            let (d, tail) = split_digits(f);
            if d.is_empty() || !tail.is_empty() {
                return None;
            }
            Some(d)
        }
        None if rest.is_empty() && !int.is_empty() => None,
        None => return None,
    };
    if int.is_empty() && frac.is_none() {
        return None;
    }
    format!("{}{}", if int.is_empty() { "0" } else { int }, frac.map(|f| format!(".{f}")).unwrap_or_default()).parse().ok()
}

/// `1:30`, `1.5`, `1.5h`, `90m`, `1h30`, `1h 30m`, as hours; a leading `-`
/// makes it negative (what [`hours_text`] writes for a negative amount).
fn parse_hours(text: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    let t: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    let (neg, t) = match t.strip_prefix('-').or_else(|| t.strip_prefix('\u{2212}')) {
        Some(r) => (true, r.to_string()),
        None => (false, t),
    };
    let minutes_ok = |m: &str| m.len() <= 2 && m.parse::<u32>().is_ok_and(|m| m < 60);
    let v = (|| {
        if let Some((h, m)) = t.split_once(':') {
            if !h.is_empty() && h.bytes().all(|b| b.is_ascii_digit()) && !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit()) && minutes_ok(m) {
                return Some(h.parse::<f64>().ok()? + m.parse::<f64>().ok()? / 60.0);
            }
            return None;
        }
        if let Some(d) = decimal(t.strip_suffix('h').unwrap_or(&t)) {
            return Some(d);
        }
        if let Some(m) = t.strip_suffix('m') {
            if !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit()) {
                return Some(m.parse::<f64>().ok()? / 60.0);
            }
        }
        // `1h30`, `1h30m`.
        let (h, rest) = split_digits(&t);
        let rest = rest.strip_prefix('h')?;
        let m = rest.strip_suffix('m').unwrap_or(rest);
        if h.is_empty() || m.is_empty() || !m.bytes().all(|b| b.is_ascii_digit()) || !minutes_ok(m) {
            return None;
        }
        Some(h.parse::<f64>().ok()? + m.parse::<f64>().ok()? / 60.0)
    })()?;
    Some(if neg { -v } else { v })
}

fn month_of(word: &str) -> Option<u32> {
    let w = word.to_lowercase();
    const NAMES: [&str; 12] =
        ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
    if w.len() < 3 {
        return None;
    }
    let i = NAMES.iter().position(|n| n.starts_with(&w) && (w.len() == 3 || n.len() >= w.len()))?;
    // "Sept" is common; any prefix of three or more letters is taken.
    Some(i as u32 + 1)
}

/// A day number, with an ordinal suffix allowed (`14th`).
fn day_of(word: &str) -> Option<u32> {
    let lower = word.to_lowercase();
    let w = ["st", "nd", "rd", "th"].iter().find_map(|s| lower.strip_suffix(s)).unwrap_or(&lower);
    if w.is_empty() || w.len() > 2 || !w.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    w.parse().ok()
}

fn year_of(word: &str) -> Option<i32> {
    if word.len() == 4 && word.bytes().all(|b| b.is_ascii_digit()) {
        word.parse().ok()
    } else {
        None
    }
}

/// `today`, `yesterday`, `tomorrow`, `2026-10-14`, `10/14`, `10/14/26`,
/// `10/14/2026`, `Oct 14`, `October 14`, `Oct 14 2026`, `14 Oct 2026`.
/// A date without a year is in `today`'s year.
fn parse_date(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let t = text.trim().to_lowercase();
    match t.as_str() {
        "today" => return Some(today),
        "yesterday" => return today.checked_sub_signed(Duration::days(1)),
        "tomorrow" => return today.checked_add_signed(Duration::days(1)),
        _ => {}
    }
    let num = |s: &str| -> Option<u32> {
        (!s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
    };
    if t.contains('-') {
        let parts: Vec<&str> = t.split('-').collect();
        if let [y, m, d] = parts[..] {
            return NaiveDate::from_ymd_opt(year_of(y)?, num(m)?, num(d)?);
        }
        return None;
    }
    if t.contains('/') {
        let parts: Vec<&str> = t.split('/').collect();
        let (m, d, y) = match parts[..] {
            [m, d] => (m, d, None),
            [m, d, y] => (m, d, Some(y)),
            _ => return None,
        };
        let year = match y {
            None => today.year(),
            Some(y) if y.len() == 2 && y.bytes().all(|b| b.is_ascii_digit()) => 2000 + y.parse::<i32>().ok()?,
            Some(y) => year_of(y)?,
        };
        return NaiveDate::from_ymd_opt(year, num(m)?, num(d)?);
    }
    let words: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).collect();
    let (month, day, year) = match words[..] {
        [a, b] => match month_of(a) {
            Some(m) => (m, day_of(b)?, None),
            None => (month_of(b)?, day_of(a)?, None),
        },
        [a, b, c] => match month_of(a) {
            Some(m) => (m, day_of(b)?, Some(year_of(c)?)),
            None => (month_of(b)?, day_of(a)?, Some(year_of(c)?)),
        },
        _ => return None,
    };
    NaiveDate::from_ymd_opt(year.unwrap_or(today.year()), month, day)
}

/// The text shown when a stored cell is edited again, so that typing it back
/// gives the same cell: numbers plain with up to ten decimals and no
/// grouping, dates ISO, hours as [`hours_text`], links through `label`, and
/// unparsed text as typed.
pub fn edit_text(stored: &Stored, ty: &ColumnType, label: &dyn Fn(&Stored) -> String) -> String {
    match stored {
        Stored::Text(s) | Stored::Unparsed(s) | Stored::Date(s) => s.clone(),
        Stored::Row(_) | Stored::Node(_) => label(stored),
        Stored::Number(n) => match ty {
            ColumnType::Hours => hours_text(*n),
            _ => plain_number(*n),
        },
    }
}

/// A number without grouping or exponent, to at most ten decimals.
fn plain_number(n: f64) -> String {
    if !n.is_finite() {
        return String::new();
    }
    let s = format!("{n:.10}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Hours as a person says them: `2h 30m`, `45m`, `3h`; negative with a
/// leading `-`. Rounded to the minute.
pub fn hours_text(hours: f64) -> String {
    if !hours.is_finite() {
        return String::new();
    }
    let total = (hours.abs() * 60.0).round() as u64;
    let (h, m) = (total / 60, total % 60);
    let body = match (h, m) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    };
    if hours < 0.0 && total > 0 {
        format!("-{body}")
    } else {
        body
    }
}
