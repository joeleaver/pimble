//! Numbers as text: Excel's format codes for `TEXT`, and the column formats
//! a sheet shows values in (contract section 3).
//!
//! `TEXT` understands sections (`positive;negative;zero;text`), quoted and
//! escaped literals, digit placeholders `0 # ?`, grouping and scaling
//! commas, `%`, scientific `E+00`, and the date and time codes (`yyyy`,
//! `mmm`, `dddd`, `h:mm AM/PM`, with `m` after `h` or before `s` meaning
//! minutes). Colors and conditions in brackets are skipped.
//!
//! Rounding is Excel's: half away from zero, after trimming the binary noise
//! below 15 significant digits, so 2.675 rounds to 2.68.

use crate::model::Format;

use super::coerce;
use super::dates;
use super::value::FormulaError;

/// `x` rounded to `d` decimals (negative `d` rounds to tens, hundreds, ...),
/// half away from zero.
pub fn round_half_away(x: f64, d: i32) -> f64 {
    scaled(x, d, f64::round)
}

/// Rounds the scaled value with `f` after trimming it to 15 significant
/// digits, then scales back.
pub(crate) fn scaled(x: f64, d: i32, f: fn(f64) -> f64) -> f64 {
    let d = d.clamp(-308, 308);
    let p = 10f64.powi(d.abs());
    let y = if d >= 0 { x * p } else { x / p };
    if !y.is_finite() {
        return x;
    }
    let y = f(trim15(y));
    let r = if d >= 0 { y / p } else { y * p };
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

/// `x` to 15 significant digits.
pub(crate) fn trim15(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    format!("{:.14e}", x).parse().unwrap_or(x)
}

/// The digits of |x| rounded to `d` decimals: (integer part, decimals).
fn fixed(x: f64, d: usize) -> (String, String) {
    let r = round_half_away(x.abs(), d as i32);
    let s = format!("{:.*}", d, r);
    match s.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (s, String::new()),
    }
}

fn group(int: &str) -> String {
    let n = int.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A number with grouping and at most `max` decimals (trailing zeros
/// dropped).
fn grouped(x: f64, max: usize) -> String {
    let (int, frac) = fixed(x, max);
    let frac = frac.trim_end_matches('0');
    let neg = x < 0.0 && (int != "0" || !frac.is_empty());
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    s.push_str(&group(&int));
    if !frac.is_empty() {
        s.push('.');
        s.push_str(frac);
    }
    s
}

/// `$1,234.50`.
pub fn money(x: f64) -> String {
    let (int, frac) = fixed(x, 2);
    let neg = x < 0.0 && (int != "0" || frac != "00");
    format!("{}${}.{}", if neg { "-" } else { "" }, group(&int), frac)
}

/// `2h 30m`.
pub fn hours(x: f64) -> String {
    let neg = x < 0.0;
    let a = x.abs();
    let mut h = a.floor();
    let mut m = round_half_away((a - h) * 60.0, 0);
    if m >= 60.0 {
        h += 1.0;
        m = 0.0;
    }
    let s = match (h > 0.0, m > 0.0) {
        (true, true) => format!("{h}h {m}m"),
        (true, false) => format!("{h}h"),
        _ => format!("{m}m"),
    };
    if neg && (h > 0.0 || m > 0.0) {
        format!("-{s}")
    } else {
        s
    }
}

/// `Mon, Oct 19`, with the year when it is not `today`'s year.
pub fn short_date(serial: f64, today: f64) -> String {
    if !dates::in_range(serial) {
        return grouped(serial, 2);
    }
    let (y, m, d) = dates::ymd(serial);
    let wd = dates::DAYS[dates::weekday(serial) as usize];
    let mon = &dates::MONTHS[(m.max(1) - 1) as usize][..3];
    let s = format!("{}, {mon} {d}", &wd[..3]);
    if dates::in_range(today) && dates::ymd(today).0 == y {
        s
    } else {
        format!("{s}, {y}")
    }
}

/// A number shown in a column format. `today` decides whether a date shows
/// its year.
pub fn format_number(x: f64, format: Format, today: f64) -> String {
    match format {
        Format::Money => money(x),
        Format::Hours => hours(x),
        Format::Date => short_date(x, today),
        Format::Percent => format!("{}%", grouped(x * 100.0, 1)),
        Format::Number | Format::Auto => grouped(x, 2),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Ft {
    Lit(String),
    /// `0`, `#` or `?`.
    Digit(char),
    Dot,
    Comma,
    Percent,
    /// `E+` (true) or `E-`.
    Exp(bool),
    Year(usize),
    /// Month, or minutes once resolved.
    Month(usize),
    Minute(usize),
    Day(usize),
    Hour(usize),
    Second(usize),
    AmPm(&'static str, &'static str),
    At,
    General,
}

fn tokenize(sec: &str) -> Vec<Ft> {
    let c: Vec<char> = sec.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let run = |i: usize, ch: char| {
        c[i..]
            .iter()
            .take_while(|x| x.eq_ignore_ascii_case(&ch))
            .count()
    };
    while i < c.len() {
        let ch = c[i];
        let rest: String = c[i..].iter().collect();
        let lower = rest.to_ascii_lowercase();
        match ch {
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < c.len() && c[i] != '"' {
                    s.push(c[i]);
                    i += 1;
                }
                i += 1;
                out.push(Ft::Lit(s));
            }
            '\\' => {
                if i + 1 < c.len() {
                    out.push(Ft::Lit(c[i + 1].to_string()));
                }
                i += 2;
            }
            '_' => {
                out.push(Ft::Lit(" ".into()));
                i += 2;
            }
            '*' => i += 2,
            '[' => {
                while i < c.len() && c[i] != ']' {
                    i += 1;
                }
                i += 1;
            }
            '0' | '#' | '?' => {
                out.push(Ft::Digit(ch));
                i += 1;
            }
            '.' => {
                out.push(Ft::Dot);
                i += 1;
            }
            ',' => {
                out.push(Ft::Comma);
                i += 1;
            }
            '%' => {
                out.push(Ft::Percent);
                i += 1;
            }
            '@' => {
                out.push(Ft::At);
                i += 1;
            }
            'E' | 'e' if matches!(c.get(i + 1), Some('+') | Some('-')) => {
                out.push(Ft::Exp(c[i + 1] == '+'));
                i += 2;
            }
            _ if lower.starts_with("general") => {
                out.push(Ft::General);
                i += 7;
            }
            _ if lower.starts_with("am/pm") => {
                out.push(if rest.starts_with("am/pm") {
                    Ft::AmPm("am", "pm")
                } else {
                    Ft::AmPm("AM", "PM")
                });
                i += 5;
            }
            _ if lower.starts_with("a/p") => {
                out.push(if rest.starts_with("a/p") {
                    Ft::AmPm("a", "p")
                } else {
                    Ft::AmPm("A", "P")
                });
                i += 3;
            }
            'y' | 'Y' => {
                let n = run(i, 'y');
                out.push(Ft::Year(n));
                i += n;
            }
            'm' | 'M' => {
                let n = run(i, 'm');
                out.push(Ft::Month(n));
                i += n;
            }
            'd' | 'D' => {
                let n = run(i, 'd');
                out.push(Ft::Day(n));
                i += n;
            }
            'h' | 'H' => {
                let n = run(i, 'h');
                out.push(Ft::Hour(n));
                i += n;
            }
            's' | 'S' => {
                let n = run(i, 's');
                out.push(Ft::Second(n));
                i += n;
            }
            _ => {
                out.push(Ft::Lit(ch.to_string()));
                i += 1;
            }
        }
    }
    // `m` right after an hour, or right before seconds, is minutes.
    let idx: Vec<usize> = (0..out.len())
        .filter(|&k| !matches!(out[k], Ft::Lit(_)))
        .collect();
    for (p, &k) in idx.iter().enumerate() {
        if let Ft::Month(n) = out[k] {
            let after_hour = p > 0 && matches!(out[idx[p - 1]], Ft::Hour(_));
            let before_sec = idx
                .get(p + 1)
                .is_some_and(|&q| matches!(out[q], Ft::Second(_)));
            if (after_hour || before_sec) && n <= 2 {
                out[k] = Ft::Minute(n);
            }
        }
    }
    out
}

fn split_sections(f: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut quoted = false;
    let mut esc = false;
    for ch in f.chars() {
        if esc {
            esc = false;
        } else if ch == '\\' {
            esc = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch == ';' && !quoted {
            out.push(String::new());
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.push(ch);
        }
    }
    out
}

fn is_date(t: &[Ft]) -> bool {
    t.iter().any(|x| {
        matches!(
            x,
            Ft::Year(_)
                | Ft::Month(_)
                | Ft::Minute(_)
                | Ft::Day(_)
                | Ft::Hour(_)
                | Ft::Second(_)
                | Ft::AmPm(..)
        )
    })
}

/// `TEXT(value, format)` for a number.
pub fn text_number(x: f64, format: &str) -> Result<String, FormulaError> {
    let secs = split_sections(format);
    let (sec, v, signed) = match secs.len() {
        1 => (&secs[0], x, true),
        2 => {
            if x < 0.0 {
                (&secs[1], -x, false)
            } else {
                (&secs[0], x, false)
            }
        }
        _ => {
            if x < 0.0 {
                (&secs[1], -x, false)
            } else if x == 0.0 {
                (&secs[2], x, false)
            } else {
                (&secs[0], x, false)
            }
        }
    };
    let toks = tokenize(sec);
    if is_date(&toks) {
        return render_date(&toks, v);
    }
    Ok(render_number(&toks, v, signed))
}

/// `TEXT(value, format)` for text: the fourth section's `@`, or the text.
pub fn text_text(s: &str, format: &str) -> String {
    let secs = split_sections(format);
    let sec = if secs.len() >= 4 {
        Some(&secs[3])
    } else if secs.len() == 1 && secs[0].contains('@') {
        Some(&secs[0])
    } else {
        None
    };
    match sec {
        Some(sec) => tokenize(sec)
            .into_iter()
            .map(|t| match t {
                Ft::At => s.to_string(),
                Ft::Lit(l) => l,
                _ => String::new(),
            })
            .collect(),
        None => s.to_string(),
    }
}

fn render_date(toks: &[Ft], v: f64) -> Result<String, FormulaError> {
    if !dates::in_range(v) {
        return Err(FormulaError::value(format!(
            "{} isn't a date Pimble can show",
            coerce::general(v)
        )));
    }
    let total = (v * 86_400.0).round() as i64;
    let day = total.div_euclid(86_400);
    let sod = total.rem_euclid(86_400);
    let (y, m, d) = dates::ymd(day as f64);
    let wd = dates::weekday(day as f64) as usize;
    let (hh, mi, ss) = (sod / 3600, (sod / 60) % 60, sod % 60);
    let twelve = toks.iter().any(|t| matches!(t, Ft::AmPm(..)));
    let mut out = String::new();
    for t in toks {
        match t {
            Ft::Lit(l) => out.push_str(l),
            Ft::Dot => out.push('.'),
            Ft::Comma => out.push(','),
            Ft::Percent => out.push('%'),
            Ft::Digit(c) => out.push(*c),
            Ft::Exp(p) => out.push_str(if *p { "E+" } else { "E-" }),
            Ft::At | Ft::General => {}
            Ft::Year(n) => {
                if *n <= 2 {
                    out.push_str(&format!("{:02}", y.rem_euclid(100)));
                } else {
                    out.push_str(&format!("{y:04}"));
                }
            }
            Ft::Month(n) => {
                let name = dates::MONTHS[(m.max(1) - 1) as usize];
                match n {
                    1 => out.push_str(&m.to_string()),
                    2 => out.push_str(&format!("{m:02}")),
                    3 => out.push_str(&name[..3]),
                    5 => out.push_str(&name[..1]),
                    _ => out.push_str(name),
                }
            }
            Ft::Minute(n) => {
                if *n >= 2 {
                    out.push_str(&format!("{mi:02}"));
                } else {
                    out.push_str(&mi.to_string());
                }
            }
            Ft::Day(n) => match n {
                1 => out.push_str(&d.to_string()),
                2 => out.push_str(&format!("{d:02}")),
                3 => out.push_str(&dates::DAYS[wd][..3]),
                _ => out.push_str(dates::DAYS[wd]),
            },
            Ft::Hour(n) => {
                let h = if twelve {
                    match hh % 12 {
                        0 => 12,
                        h => h,
                    }
                } else {
                    hh
                };
                if *n >= 2 {
                    out.push_str(&format!("{h:02}"));
                } else {
                    out.push_str(&h.to_string());
                }
            }
            Ft::Second(n) => {
                if *n >= 2 {
                    out.push_str(&format!("{ss:02}"));
                } else {
                    out.push_str(&ss.to_string());
                }
            }
            Ft::AmPm(am, pm) => out.push_str(if hh < 12 { am } else { pm }),
        }
    }
    Ok(out)
}

fn render_number(toks: &[Ft], x: f64, signed: bool) -> String {
    if toks.contains(&Ft::General) {
        let g = coerce::general(if signed { x } else { x.abs() });
        return toks
            .iter()
            .map(|t| match t {
                Ft::General => g.clone(),
                Ft::Lit(l) => l.clone(),
                _ => String::new(),
            })
            .collect();
    }
    let is_digit = |t: &Ft| matches!(t, Ft::Digit(_));
    let exp_at = toks.iter().position(|t| matches!(t, Ft::Exp(_)));
    let num_end = exp_at.unwrap_or(toks.len());
    let dot_at = toks[..num_end].iter().position(|t| *t == Ft::Dot);
    let int_end = dot_at.unwrap_or(num_end);
    let int_toks = &toks[..int_end];
    let frac_toks = dot_at.map(|d| &toks[d + 1..num_end]).unwrap_or(&[]);
    let exp_toks = exp_at.map(|e| &toks[e + 1..]).unwrap_or(&[]);

    let first_digit = int_toks.iter().position(is_digit);
    let last_digit = int_toks.iter().rposition(is_digit);
    // A comma between digit placeholders groups; commas right after the
    // last one scale by a thousand each.
    let grouping = match (first_digit, last_digit) {
        (Some(a), Some(b)) => int_toks[a..b].contains(&Ft::Comma),
        _ => false,
    };
    // Scaling commas follow the section's last digit placeholder, before
    // or after the decimal point.
    let num_toks = &toks[..num_end];
    let scale = match num_toks.iter().rposition(is_digit) {
        Some(b) => num_toks[b + 1..]
            .iter()
            .take_while(|t| **t == Ft::Comma)
            .count(),
        None => 0,
    };
    let pct = toks.iter().filter(|t| **t == Ft::Percent).count();
    let neg = signed && x < 0.0;
    let mut v = x.abs();
    for _ in 0..pct {
        v *= 100.0;
    }
    for _ in 0..scale {
        v /= 1000.0;
    }
    let frac_n = frac_toks.iter().filter(|t| is_digit(t)).count();
    let int_n = int_toks.iter().filter(|t| is_digit(t)).count();

    let mut exp_val = 0i32;
    if exp_at.is_some() && v != 0.0 {
        let k = int_n.max(1) as i32;
        exp_val = v.log10().floor() as i32 - (k - 1);
        let mut m = v / 10f64.powi(exp_val);
        m = round_half_away(m, frac_n as i32);
        if m >= 10f64.powi(k) {
            exp_val += 1;
        }
        v /= 10f64.powi(exp_val);
    }
    let (int_digits, frac_digits) = fixed(v, frac_n);
    let int_digits = if int_digits == "0" {
        String::new()
    } else {
        int_digits
    };

    let mut out = String::new();
    if neg && (!int_digits.is_empty() || frac_digits.bytes().any(|b| b != b'0')) {
        out.push('-');
    }
    // The integer part, filled from the right.
    if grouping {
        let zeros = int_toks.iter().filter(|t| **t == Ft::Digit('0')).count();
        let mut digits = int_digits.clone();
        while digits.len() < zeros {
            digits.insert(0, '0');
        }
        let body = group(&digits);
        for (k, t) in int_toks.iter().enumerate() {
            if Some(k) == first_digit {
                out.push_str(&body);
            }
            let inside =
                matches!((first_digit, last_digit), (Some(a), Some(b)) if k >= a && k <= b);
            match t {
                Ft::Lit(l) if !inside => out.push_str(l),
                Ft::Percent => out.push('%'),
                _ => {}
            }
        }
    } else {
        let mut parts: Vec<String> = Vec::new();
        let mut digits: Vec<char> = int_digits.chars().collect();
        for (k, t) in int_toks.iter().enumerate().rev() {
            match t {
                Ft::Digit(c) => {
                    if Some(k) == first_digit {
                        let rest: String = digits.drain(..).collect();
                        if rest.is_empty() {
                            match c {
                                '0' => parts.push("0".into()),
                                '?' => parts.push(" ".into()),
                                _ => {}
                            }
                        } else {
                            parts.push(rest);
                        }
                    } else if let Some(d) = digits.pop() {
                        parts.push(d.to_string());
                    } else {
                        match c {
                            '0' => parts.push("0".into()),
                            '?' => parts.push(" ".into()),
                            _ => {}
                        }
                    }
                }
                Ft::Lit(l) => parts.push(l.clone()),
                Ft::Percent => parts.push("%".into()),
                Ft::At | Ft::Comma => {}
                _ => {}
            }
        }
        if first_digit.is_none() && !int_digits.is_empty() && (frac_n > 0 || exp_at.is_some()) {
            // `.00` with no integer placeholder still shows the integer.
            parts.push(int_digits.clone());
        }
        for p in parts.into_iter().rev() {
            out.push_str(&p);
        }
    }
    if dot_at.is_some() {
        out.push('.');
        // Trailing `#` placeholders drop trailing zeros.
        let fd: Vec<char> = frac_digits.chars().collect();
        let places: Vec<char> = frac_toks
            .iter()
            .filter_map(|t| if let Ft::Digit(c) = t { Some(*c) } else { None })
            .collect();
        let mut keep = fd.len();
        while keep > 0 && fd[keep - 1] == '0' && places[keep - 1] != '0' {
            keep -= 1;
        }
        let mut di = 0;
        for t in frac_toks {
            match t {
                Ft::Digit(c) => {
                    if di < keep {
                        out.push(fd[di]);
                    } else if *c == '?' {
                        out.push(' ');
                    }
                    di += 1;
                }
                Ft::Lit(l) => out.push_str(l),
                Ft::Percent => out.push('%'),
                _ => {}
            }
        }
    }
    if let Some(e) = exp_at {
        let plus = matches!(toks[e], Ft::Exp(true));
        out.push('E');
        if exp_val < 0 {
            out.push('-');
        } else if plus {
            out.push('+');
        }
        let n = exp_toks.iter().filter(|t| is_digit(t)).count().max(1);
        out.push_str(&format!("{:0n$}", exp_val.abs(), n = n));
        for t in exp_toks {
            match t {
                Ft::Lit(l) => out.push_str(l),
                Ft::Percent => out.push('%'),
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(x: f64, f: &str) -> String {
        text_number(x, f).unwrap()
    }

    #[test]
    fn number_codes() {
        assert_eq!(t(1234.5, "$#,##0.00"), "$1,234.50");
        assert_eq!(t(0.256, "0.0%"), "25.6%");
        assert_eq!(t(5.0, "000"), "005");
        assert_eq!(t(-1234.5, "#,##0.00;(#,##0.00)"), "(1,234.50)");
        assert_eq!(t(1_234_567.0, "#,##0,"), "1,235");
        assert_eq!(t(1.23456, "0.00"), "1.23");
        assert_eq!(t(12.0, "#.##"), "12.");
        assert_eq!(t(0.5, "#.##"), ".5");
        assert_eq!(t(2.675, "0.00"), "2.68");
        assert_eq!(t(-3.0, "0"), "-3");
        assert_eq!(t(5_551_234.0, "000-0000"), "555-1234");
        assert_eq!(t(12345.678, "0.00E+00"), "1.23E+04");
        assert_eq!(t(0.0, "0;-0;\"zero\""), "zero");
        assert_eq!(t(42.0, "General"), "42");
        assert_eq!(t(1234.0, "0"), "1234");
        assert_eq!(t(0.0, "#,##0"), "0");
    }

    #[test]
    fn date_codes() {
        let d = 46_314.0; // Monday 2026-10-19
        assert_eq!(t(d, "yyyy-mm"), "2026-10");
        assert_eq!(t(d, "dddd, mmmm d, yyyy"), "Monday, October 19, 2026");
        assert_eq!(t(d, "ddd mmm dd yy"), "Mon Oct 19 26");
        assert_eq!(t(d, "m/d/yyyy"), "10/19/2026");
        assert_eq!(t(0.5, "h:mm AM/PM"), "12:00 PM");
        assert_eq!(t(d + 0.75, "hh:mm:ss"), "18:00:00");
        assert_eq!(t(d + 0.25 + 5.0 / 1440.0, "h:mm"), "6:05");
    }

    #[test]
    fn display() {
        assert_eq!(money(1234.5), "$1,234.50");
        assert_eq!(money(-12.0), "-$12.00");
        assert_eq!(hours(2.5), "2h 30m");
        assert_eq!(hours(1.75), "1h 45m");
        assert_eq!(hours(3.0), "3h");
        assert_eq!(hours(0.25), "15m");
        assert_eq!(format_number(1234.567, Format::Number, 0.0), "1,234.57");
        assert_eq!(format_number(1234.0, Format::Auto, 0.0), "1,234");
        assert_eq!(format_number(0.256, Format::Percent, 0.0), "25.6%");
        let today = 46_314.0;
        assert_eq!(short_date(46_314.0, today), "Mon, Oct 19");
        assert_eq!(short_date(46_314.0 - 365.0, today), "Sun, Oct 19, 2025");
    }
}
