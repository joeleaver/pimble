//! Excel criteria, as `SUMIF`, `COUNTIFS` and the rest of the family read
//! them: `">100"`, `"<>Gas"`, `"Gro*"`, `">"&EOMONTH(TODAY(),-1)`, a plain
//! value, or a link.
//!
//! The rules are Excel's: a number criterion also matches text that reads as
//! that number; `<`/`>` against a number skip text and blanks; `"<>x"`
//! matches blanks; `""` matches blanks and empty text while `"="` matches
//! only blanks; text matches case-insensitively with `*`, `?` and `~`
//! wildcards; an error in the range never matches.

use std::cmp::Ordering;

use super::coerce::{self, Labels};
use super::value::{FormulaError, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Debug, Clone)]
enum Rhs {
    Number(f64),
    Bool(bool),
    /// Lower-cased.
    Text(String),
    Link(Value),
}

#[derive(Debug, Clone)]
pub(crate) struct Criterion {
    op: Op,
    rhs: Rhs,
    /// `"="` written out, which matches only true blanks.
    explicit_eq: bool,
}

impl Criterion {
    pub fn new(v: &Value, _labels: &dyn Labels) -> Result<Criterion, FormulaError> {
        let plain = |rhs| Criterion {
            op: Op::Eq,
            rhs,
            explicit_eq: false,
        };
        Ok(match v {
            Value::Error(e) => return Err(e.clone()),
            Value::Number(x) => plain(Rhs::Number(*x)),
            Value::Bool(b) => plain(Rhs::Bool(*b)),
            Value::Blank => plain(Rhs::Text(String::new())),
            Value::Row(_) | Value::Node(_) => plain(Rhs::Link(v.clone())),
            Value::Text(s) => {
                let (op, rest, explicit) = split_op(s);
                let rhs = if let Some(x) = coerce::parse_number_text(rest) {
                    Rhs::Number(x)
                } else if rest.eq_ignore_ascii_case("true") {
                    Rhs::Bool(true)
                } else if rest.eq_ignore_ascii_case("false") {
                    Rhs::Bool(false)
                } else {
                    Rhs::Text(rest.to_lowercase())
                };
                Criterion {
                    op,
                    rhs,
                    explicit_eq: explicit,
                }
            }
        })
    }

    pub fn matches(&self, v: &Value, labels: &dyn Labels) -> bool {
        if matches!(v, Value::Error(_)) {
            return false;
        }
        match self.op {
            Op::Eq => self.eq(v, labels),
            Op::Ne => !self.eq(v, labels),
            op => self.order(op, v, labels),
        }
    }

    fn eq(&self, v: &Value, labels: &dyn Labels) -> bool {
        match &self.rhs {
            Rhs::Number(n) => match v {
                Value::Number(x) => coerce::num_eq(*x, *n),
                Value::Text(s) => {
                    coerce::parse_number_text(s).is_some_and(|x| coerce::num_eq(x, *n))
                }
                _ => false,
            },
            Rhs::Bool(b) => matches!(v, Value::Bool(x) if x == b),
            Rhs::Text(t) if t.is_empty() => match v {
                Value::Blank => true,
                Value::Text(s) => s.is_empty() && !self.explicit_eq,
                _ => false,
            },
            Rhs::Text(t) => match v {
                Value::Text(_) | Value::Row(_) | Value::Node(_) => coerce::to_text(v, labels)
                    .map(|s| wildcard(t, &s.to_lowercase()))
                    .unwrap_or(false),
                _ => false,
            },
            Rhs::Link(link) => match (link, v) {
                (Value::Row(a), Value::Row(b)) => a == b,
                (Value::Node(a), Value::Node(b)) => a == b,
                (_, Value::Text(s)) => coerce::to_text(link, labels)
                    .map(|l| l.to_lowercase() == s.to_lowercase())
                    .unwrap_or(false),
                _ => false,
            },
        }
    }

    fn order(&self, op: Op, v: &Value, labels: &dyn Labels) -> bool {
        let ord = match (&self.rhs, v) {
            (Rhs::Number(n), Value::Number(x)) => coerce_cmp(*x, *n),
            (Rhs::Text(t), Value::Text(_) | Value::Row(_) | Value::Node(_)) => {
                match coerce::to_text(v, labels) {
                    Ok(s) => s.to_lowercase().cmp(t),
                    Err(_) => return false,
                }
            }
            (Rhs::Bool(b), Value::Bool(x)) => x.cmp(b),
            _ => return false,
        };
        match op {
            Op::Lt => ord == Ordering::Less,
            Op::Gt => ord == Ordering::Greater,
            Op::Le => ord != Ordering::Greater,
            Op::Ge => ord != Ordering::Less,
            Op::Eq | Op::Ne => false,
        }
    }
}

fn coerce_cmp(x: f64, n: f64) -> Ordering {
    if coerce::num_eq(x, n) {
        Ordering::Equal
    } else if x < n {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn split_op(s: &str) -> (Op, &str, bool) {
    for (p, op) in [
        ("<=", Op::Le),
        (">=", Op::Ge),
        ("<>", Op::Ne),
        ("<", Op::Lt),
        (">", Op::Gt),
        ("=", Op::Eq),
    ] {
        if let Some(rest) = s.strip_prefix(p) {
            return (op, rest, op == Op::Eq);
        }
    }
    (Op::Eq, s, false)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pat {
    Lit(char),
    One,
    Any,
}

fn pattern(p: &str) -> Vec<Pat> {
    let mut out = Vec::new();
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        match c {
            '~' => match chars.next() {
                Some(n) => out.push(Pat::Lit(n)),
                None => out.push(Pat::Lit('~')),
            },
            '*' => out.push(Pat::Any),
            '?' => out.push(Pat::One),
            c => out.push(Pat::Lit(c)),
        }
    }
    out
}

/// Whether `text` matches the wildcard pattern `pat` (both already
/// lower-cased): `*` any run, `?` one character, `~` escapes.
pub(crate) fn wildcard(pat: &str, text: &str) -> bool {
    let p = pattern(pat);
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some(Pat::Lit(c)) if *c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            Some(Pat::One) => {
                pi += 1;
                ti += 1;
            }
            Some(Pat::Any) => {
                star = Some((pi, ti));
                pi += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi.min(p.len())..].iter().all(|x| *x == Pat::Any)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard("gro*", "groceries"));
        assert!(wildcard("*s", "groceries"));
        assert!(wildcard("g?s", "gas"));
        assert!(!wildcard("g?s", "gaps"));
        assert!(wildcard("*", ""));
        assert!(wildcard("a~*b", "a*b"));
        assert!(!wildcard("a~*b", "axb"));
        assert!(wildcard("what~?", "what?"));
        assert!(wildcard("*a*b*", "xxaxxbxx"));
        assert!(!wildcard("abc", "abcd"));
    }
}
