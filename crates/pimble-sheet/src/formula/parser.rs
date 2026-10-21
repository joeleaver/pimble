//! Excel formula text to an [`Expr`], with Excel's precedence (lowest
//! first): comparison, `&`, `+ -`, `* /`, `^` (left-associative, as in
//! Excel), unary minus (so `-2^2` is 4), `%`, and the member dot.
//!
//! The lexer understands structured references whole: a bracket group is
//! read into a [`Spec`] with a span for every column name in it, and a word
//! or quoted name followed by `!` is a note prefix. A formula is user input:
//! every failure is a [`ParseError`] with a sentence, and nesting is bounded
//! so a pathological formula cannot exhaust the stack.

use std::str::FromStr;

use pimble_core::{NodeId, StoreId};
use uuid::Uuid;

use crate::model::NodeRef;

use super::ast::*;
use super::value::{ErrorCode, FormulaError};

/// Deeper nesting than this is refused (parentheses, calls, signs; Excel's
/// own limit is 64 levels of functions).
const MAX_NEST: usize = 64;

/// Longer chains of operators than this are refused (`1+1+…`).
const MAX_CHAIN: usize = 256;

/// Why a formula does not parse, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl ParseError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        ParseError {
            message: message.into(),
            span,
        }
    }

    /// The `#ERROR!` a cell with this formula shows.
    pub fn to_error(&self) -> FormulaError {
        FormulaError::new(ErrorCode::Error, self.message.clone())
    }
}

/// Parses a formula, shown or stored form, with or without its leading `=`.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let toks = Lexer::new(src).run()?;
    let mut p = Parser {
        src,
        toks,
        i: 0,
        depth: 0,
        chain: 0,
    };
    if matches!(p.peek(), Tok::End) {
        return Err(ParseError::new("The formula is empty", 0..src.len()));
    }
    let e = p.expr()?;
    if !matches!(p.peek(), Tok::End) {
        let t = p.tok().clone();
        return Err(ParseError::new(
            format!("I didn't expect {} here", p.describe(&t)),
            t.span,
        ));
    }
    Ok(e)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Amp,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    LParen,
    RParen,
    Comma,
    Dot,
    Percent,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Bool(bool),
    Err(ErrorCode),
    Ident(String),
    /// A note prefix; the token's span runs through the `!`.
    Note(NoteName),
    /// A table name, with the brackets written right after it.
    Table(TablePart, Option<Spec>),
    /// Brackets with no table before them.
    Bracket(Spec),
    /// `{c:<id>}` outside brackets: a column after a dot.
    ColId(String),
    /// `{r:<index>}` outside brackets: a summary's result column after a dot.
    ColPos(usize),
    /// `{rk:<key>}` outside brackets: the same, by key.
    ColKey(String),
    Op(Op),
    End,
}

#[derive(Debug, Clone, PartialEq)]
struct Token {
    tok: Tok,
    span: Span,
}

struct Lexer<'s> {
    src: &'s str,
    pos: usize,
    out: Vec<Token>,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '\\'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}

impl<'s> Lexer<'s> {
    fn new(src: &'s str) -> Self {
        let mut pos = 0;
        let trimmed = src.trim_start();
        let lead = src.len() - trimmed.len();
        if trimmed.starts_with('=') {
            pos = lead + 1;
        }
        Lexer {
            src,
            pos,
            out: Vec::new(),
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn char_at(&self, i: usize) -> Option<char> {
        self.src.get(i..).and_then(|s| s.chars().next())
    }

    fn push(&mut self, tok: Tok, start: usize) {
        self.out.push(Token {
            tok,
            span: start..self.pos,
        });
    }

    fn run(mut self) -> Result<Vec<Token>, ParseError> {
        while let Some(c) = self.peek_char() {
            let start = self.pos;
            if c.is_whitespace() {
                self.pos += c.len_utf8();
                continue;
            }
            let next = self.char_at(self.pos + c.len_utf8());
            if c.is_ascii_digit() || (c == '.' && next.is_some_and(|n| n.is_ascii_digit())) {
                self.number()?;
            } else if c == '"' {
                self.string()?;
            } else if c == '#' {
                self.error_literal()?;
            } else if c == '\'' {
                self.quoted()?;
            } else if c == '{' {
                self.id_token()?;
            } else if c == '[' {
                let spec = self.bracket()?;
                self.push(Tok::Bracket(spec), start);
            } else if is_ident_start(c) {
                self.word()?;
            } else {
                let two = self.src.get(self.pos..self.pos + 2).unwrap_or("");
                let (op, len) = match two {
                    "<=" => (Op::Le, 2),
                    ">=" => (Op::Ge, 2),
                    "<>" => (Op::Ne, 2),
                    _ => match c {
                        '+' => (Op::Plus, 1),
                        '-' => (Op::Minus, 1),
                        '*' => (Op::Star, 1),
                        '/' => (Op::Slash, 1),
                        '^' => (Op::Caret, 1),
                        '&' => (Op::Amp, 1),
                        '=' => (Op::Eq, 1),
                        '<' => (Op::Lt, 1),
                        '>' => (Op::Gt, 1),
                        '(' => (Op::LParen, 1),
                        ')' => (Op::RParen, 1),
                        ',' | ';' => (Op::Comma, 1),
                        '.' => (Op::Dot, 1),
                        '%' => (Op::Percent, 1),
                        _ => {
                            return Err(ParseError::new(
                                format!("I don't know what \"{c}\" means here"),
                                start..start + c.len_utf8(),
                            ))
                        }
                    },
                };
                self.pos += len;
                self.push(Tok::Op(op), start);
            }
        }
        let end = self.src.len();
        self.out.push(Token {
            tok: Tok::End,
            span: end..end,
        });
        Ok(self.out)
    }

    fn number(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let b = self.src.as_bytes();
        let mut i = self.pos;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        let text = &self.src[start..i];
        let v = f64::from_str(text)
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| ParseError::new(format!("\"{text}\" isn't a number"), start..i))?;
        self.pos = i;
        self.push(Tok::Num(v), start);
        Ok(())
    }

    fn string(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let mut i = self.pos + 1;
        let mut v = String::new();
        loop {
            let Some(c) = self.char_at(i) else {
                return Err(ParseError::new(
                    "A quote is never closed",
                    start..self.src.len(),
                ));
            };
            if c == '"' {
                if self.char_at(i + 1) == Some('"') {
                    v.push('"');
                    i += 2;
                    continue;
                }
                i += 1;
                break;
            }
            v.push(c);
            i += c.len_utf8();
        }
        self.pos = i;
        self.push(Tok::Str(v), start);
        Ok(())
    }

    fn error_literal(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let rest = &self.src[start..];
        for code in [
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
        ] {
            let c = code.code();
            if rest.len() >= c.len()
                && rest.is_char_boundary(c.len())
                && rest[..c.len()].eq_ignore_ascii_case(c)
            {
                self.pos += c.len();
                self.push(Tok::Err(code), start);
                return Ok(());
            }
        }
        let word: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace())
            .take(12)
            .collect();
        Err(ParseError::new(
            format!("I don't know what \"{word}\" means here"),
            start..start + word.len().max(1),
        ))
    }

    /// `'Household budget'!` or `'Monthly costs'[...]`.
    fn quoted(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let mut i = start + 1;
        let mut name = String::new();
        loop {
            let Some(c) = self.char_at(i) else {
                return Err(ParseError::new(
                    "A quote is never closed",
                    start..self.src.len(),
                ));
            };
            if c == '\'' {
                if self.char_at(i + 1) == Some('\'') {
                    name.push('\'');
                    i += 2;
                    continue;
                }
                i += 1;
                break;
            }
            name.push(c);
            i += c.len_utf8();
        }
        let name_span = start..i;
        match self.char_at(i) {
            Some('!') => {
                self.pos = i + 1;
                self.push(Tok::Note(NoteName::Name(name)), start);
                Ok(())
            }
            Some('[') => {
                self.pos = i;
                let spec = self.bracket()?;
                let part = TablePart {
                    name: NameRef::Name(name),
                    span: name_span,
                    quoted: true,
                };
                self.push(Tok::Table(part, Some(spec)), start);
                Ok(())
            }
            _ => Err(ParseError::new(
                "A name in single quotes is a note, followed by \"!\", or a table, followed by \"[\"",
                name_span,
            )),
        }
    }

    /// `{n:<store>/<node>}!`, `{t:<id>}` and `{c:<id>}`.
    fn id_token(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let rest = &self.src[start..];
        let Some(close) = rest.find('}') else {
            return Err(ParseError::new(
                "A \"{\" is never closed",
                start..self.src.len(),
            ));
        };
        let body = &rest[1..close];
        let end = start + close + 1;
        let (kind, id) = body.split_once(':').unwrap_or(("", ""));
        match kind {
            "n" => {
                let node = parse_node_token(id).ok_or_else(|| {
                    ParseError::new(format!("\"{body}\" isn't a note's id"), start..end)
                })?;
                if self.char_at(end) != Some('!') {
                    return Err(ParseError::new(
                        "A note is followed by \"!\" and what is in it",
                        start..end,
                    ));
                }
                self.pos = end + 1;
                self.push(Tok::Note(NoteName::Id(node)), start);
            }
            "t" => {
                self.pos = end;
                let part = TablePart {
                    name: NameRef::Id(id.to_string()),
                    span: start..end,
                    quoted: false,
                };
                let spec = if self.peek_char() == Some('[') {
                    Some(self.bracket()?)
                } else {
                    None
                };
                self.push(Tok::Table(part, spec), start);
            }
            "c" => {
                self.pos = end;
                self.push(Tok::ColId(id.to_string()), start);
            }
            "r" => {
                let i = id.trim().parse::<usize>().map_err(|_| {
                    ParseError::new(format!("\"{body}\" isn't a result column"), start..end)
                })?;
                self.pos = end;
                self.push(Tok::ColPos(i), start);
            }
            "rk" => {
                self.pos = end;
                self.push(Tok::ColKey(decode_key(id)), start);
            }
            _ => {
                return Err(ParseError::new(
                    "Lists written in braces, like {1,2,3}, aren't supported yet",
                    start..end,
                ))
            }
        }
        Ok(())
    }

    fn word(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let mut end = start;
        for (k, c) in self.src[start..].char_indices() {
            if (k == 0 && is_ident_start(c)) || (k > 0 && is_ident_char(c)) {
                end = start + k + c.len_utf8();
            } else {
                break;
            }
        }
        let mut w = &self.src[start..end];
        // A dot inside a word belongs to it only in a function's name
        // (`STDEV.S(`); otherwise it is the member dot (`v.mpg`).
        if w.contains('.') && self.char_at(end) != Some('(') {
            let cut = w.find('.').unwrap_or(w.len());
            w = &w[..cut];
            end = start + cut;
        }
        self.pos = end;
        match self.char_at(end) {
            Some('!') => {
                self.pos = end + 1;
                self.push(Tok::Note(NoteName::Name(w.to_string())), start);
            }
            Some('[') => {
                let part = TablePart {
                    name: NameRef::Name(w.to_string()),
                    span: start..end,
                    quoted: false,
                };
                let spec = self.bracket()?;
                self.push(Tok::Table(part, Some(spec)), start);
            }
            next => {
                let up = w.to_ascii_uppercase();
                if (up == "TRUE" || up == "FALSE") && next != Some('(') {
                    self.push(Tok::Bool(up == "TRUE"), start);
                } else {
                    self.push(Tok::Ident(w.to_string()), start);
                }
            }
        }
        Ok(())
    }

    /// The bracket group at the cursor, read whole.
    fn bracket(&mut self) -> Result<Spec, ParseError> {
        let open = self.pos;
        let close = matching_bracket(self.src, open)?;
        self.pos = close + 1;
        parse_spec(self.src, open, close)
    }
}

/// `<store uuid>/<node uuid>`.
fn parse_node_token(id: &str) -> Option<NodeRef> {
    let (s, n) = id.split_once('/')?;
    Some(NodeRef {
        store: StoreId(Uuid::parse_str(s.trim()).ok()?),
        node: NodeId(Uuid::parse_str(n.trim()).ok()?),
    })
}

/// The index of the `]` closing the `[` at `open`; `'` escapes a character.
fn matching_bracket(src: &str, open: usize) -> Result<usize, ParseError> {
    let mut depth = 0usize;
    let mut chars = src[open..].char_indices();
    while let Some((k, c)) = chars.next() {
        match c {
            '\'' => {
                chars.next();
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(open + k);
                }
            }
            _ => {}
        }
    }
    Err(ParseError::new("A \"[\" is never closed", open..src.len()))
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn skip_ws(src: &str, mut i: usize, end: usize) -> usize {
    while i < end {
        match src[i..].chars().next() {
            Some(c) if c.is_whitespace() => i += c.len_utf8(),
            _ => break,
        }
    }
    i
}

/// A column piece's name: the text between `start` and `end`, trimmed.
fn piece(src: &str, start: usize, end: usize, outer: Span, bracketed: bool) -> ColPiece {
    let raw = &src[start..end];
    let lead = raw.len() - raw.trim_start().len();
    let trimmed = raw.trim();
    let span = start + lead..start + lead + trimmed.len();
    let text = unescape(trimmed);
    let name = if trimmed.eq_ignore_ascii_case("#REF!") {
        NameRef::Dead
    } else if let Some(id) = trimmed
        .strip_prefix("{c:")
        .and_then(|s| s.strip_suffix('}'))
    {
        NameRef::Id(id.to_string())
    } else if let Some(i) = trimmed
        .strip_prefix("{r:")
        .and_then(|s| s.strip_suffix('}'))
        .and_then(|s| s.trim().parse::<usize>().ok())
    {
        NameRef::Pos(i)
    } else if let Some(k) = trimmed
        .strip_prefix("{rk:")
        .and_then(|s| s.strip_suffix('}'))
    {
        NameRef::Key(decode_key(k))
    } else {
        NameRef::Name(text)
    };
    ColPiece {
        name,
        span,
        outer,
        bracketed,
    }
}

enum Special {
    ThisRow,
    Data,
    Headers,
    Totals,
}

fn special(text: &str) -> Option<Special> {
    let t: String = text
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(match t.as_str() {
        "#this row" => Special::ThisRow,
        "#data" | "#all" => Special::Data,
        "#headers" => Special::Headers,
        "#totals" => Special::Totals,
        _ => return None,
    })
}

/// `#Average` (or, stored, `#{s:<id>}`) in `src[hash..end]`: the summary row
/// of that name (contract section 4b). One to a reference.
fn summary_part(src: &str, hash: usize, end: usize, spec: &mut Spec) -> Result<(), ParseError> {
    let raw = &src[hash + 1..end];
    let lead = raw.len() - raw.trim_start().len();
    let text = raw.trim();
    let span = hash + 1 + lead..hash + 1 + lead + text.len();
    if text.is_empty() {
        return Err(ParseError::new("A \"#\" names a part of a table, like [#Totals], or a summary row, like [#Average]", hash..end));
    }
    if spec.summary.is_some() || spec.area != Area::Data {
        return Err(ParseError::new("A reference reads one part of a table", hash..end));
    }
    let name = match text.strip_prefix("{s:").and_then(|t| t.strip_suffix('}')) {
        Some(id) => NameRef::Id(id.to_string()),
        None => NameRef::Name(text.to_string()),
    };
    spec.area = Area::Totals;
    spec.summary = Some(SummaryPiece { name, span });
    Ok(())
}

fn parse_spec(src: &str, open: usize, close: usize) -> Result<Spec, ParseError> {
    let span = open..close + 1;
    let mut spec = Spec {
        this_row: false,
        area: Area::Data,
        cols: Cols::All,
        single: false,
        span: span.clone(),
        summary: None,
    };
    let mut i = skip_ws(src, open + 1, close);
    if src[i..].starts_with('@') {
        spec.this_row = true;
        i = skip_ws(src, i + 1, close);
    }
    if i >= close {
        return Ok(spec);
    }
    let inner = &src[i..close];
    if inner.starts_with('[') {
        return parse_list(src, i, close, spec);
    }
    if inner.starts_with('#') && !inner.trim().eq_ignore_ascii_case("#REF!") {
        match special(inner) {
            Some(Special::ThisRow) => spec.this_row = true,
            Some(Special::Data) => {}
            Some(Special::Headers) => spec.area = Area::Headers,
            Some(Special::Totals) => spec.area = Area::Totals,
            None => {
                let hash = i + inner.find('#').unwrap_or(0);
                summary_part(src, hash, close, &mut spec)?;
            }
        }
        return Ok(spec);
    }
    let p = piece(src, i, close, i..close, false);
    spec.cols = Cols::One(ColPiece {
        outer: p.span.clone(),
        ..p
    });
    spec.single = true;
    Ok(spec)
}

/// `[[#This Row],[a]]`, `[[a]:[b]]`, `[@[gas price]]`.
fn parse_list(src: &str, mut i: usize, close: usize, mut spec: Spec) -> Result<Spec, ParseError> {
    let mut cols: Vec<ColPiece> = Vec::new();
    let mut range = false;
    let mut items = 0;
    let mut specials = 0;
    loop {
        i = skip_ws(src, i, close);
        if !src[i..].starts_with('[') {
            return Err(ParseError::new(
                "Inside a reference, each part is in brackets, like [[#This Row],[miles]]",
                i..close,
            ));
        }
        let end = matching_bracket(src, i)?;
        if end > close {
            return Err(ParseError::new("A \"[\" is never closed", i..close));
        }
        let text = &src[i + 1..end];
        items += 1;
        if text.trim_start().starts_with('#') && !text.trim().eq_ignore_ascii_case("#REF!") {
            specials += 1;
            match special(text) {
                Some(Special::ThisRow) => spec.this_row = true,
                Some(Special::Data) => {}
                Some(Special::Headers) => spec.area = Area::Headers,
                Some(Special::Totals) => spec.area = Area::Totals,
                None => {
                    let hash = i + 1 + text.find('#').unwrap_or(0);
                    summary_part(src, hash, end, &mut spec)?;
                }
            }
        } else {
            cols.push(piece(src, i + 1, end, i..end + 1, true));
        }
        i = skip_ws(src, end + 1, close);
        if i >= close {
            break;
        }
        match src[i..].chars().next() {
            Some(',') => i += 1,
            Some(':') => {
                if range || cols.is_empty() {
                    return Err(ParseError::new(
                        "A range is two columns, like [[from]:[to]]",
                        i..i + 1,
                    ));
                }
                range = true;
                i += 1;
            }
            _ => {
                return Err(ParseError::new(
                    "Inside a reference, parts are separated by \",\"",
                    i..i + 1,
                ))
            }
        }
    }
    spec.cols = match (cols.len(), range) {
        (0, false) => Cols::All,
        (1, false) => Cols::One(cols.remove(0)),
        (2, true) => {
            let b = cols.remove(1);
            Cols::Range(cols.remove(0), b)
        }
        _ => {
            return Err(ParseError::new(
                "A reference names one column, or a range of them like [[from]:[to]]",
                spec.span.clone(),
            ))
        }
    };
    spec.single = items == 1 && specials == 0 && matches!(spec.cols, Cols::One(_));
    Ok(spec)
}

struct Parser<'s> {
    src: &'s str,
    toks: Vec<Token>,
    i: usize,
    depth: usize,
    chain: usize,
}

impl<'s> Parser<'s> {
    fn tok(&self) -> &Token {
        &self.toks[self.i.min(self.toks.len() - 1)]
    }

    fn peek(&self) -> &Tok {
        &self.tok().tok
    }

    fn next(&mut self) -> Token {
        let t = self.tok().clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }

    fn is_op(&self, op: Op) -> bool {
        matches!(self.peek(), Tok::Op(o) if *o == op)
    }

    fn describe(&self, t: &Token) -> String {
        match t.tok {
            Tok::End => "the end".to_string(),
            _ => format!("\"{}\"", &self.src[t.span.clone()]),
        }
    }

    fn deeper(&mut self, span: Span) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_NEST {
            return Err(ParseError::new("This formula is nested too deeply", span));
        }
        Ok(())
    }

    fn longer(&mut self, span: Span) -> Result<(), ParseError> {
        self.chain += 1;
        if self.chain > MAX_CHAIN {
            return Err(ParseError::new("This formula is too long", span));
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.comparison()
    }

    fn binary_level(
        &mut self,
        ops: &[(Op, BinOp)],
        next: fn(&mut Self) -> Result<Expr, ParseError>,
    ) -> Result<Expr, ParseError> {
        let mut a = next(self)?;
        let base = self.chain;
        while let Tok::Op(o) = *self.peek() {
            let Some(&(_, bop)) = ops.iter().find(|(op, _)| *op == o) else {
                break;
            };
            let t = self.next();
            self.longer(t.span)?;
            let b = next(self)?;
            let span = a.span.start..b.span.end;
            a = Expr {
                kind: ExprKind::Binary(bop, Box::new(a), Box::new(b)),
                span,
            };
        }
        self.chain = base;
        Ok(a)
    }

    fn comparison(&mut self) -> Result<Expr, ParseError> {
        self.binary_level(
            &[
                (Op::Eq, BinOp::Eq),
                (Op::Ne, BinOp::Ne),
                (Op::Lt, BinOp::Lt),
                (Op::Gt, BinOp::Gt),
                (Op::Le, BinOp::Le),
                (Op::Ge, BinOp::Ge),
            ],
            Self::concat,
        )
    }

    fn concat(&mut self) -> Result<Expr, ParseError> {
        self.binary_level(&[(Op::Amp, BinOp::Concat)], Self::additive)
    }

    fn additive(&mut self) -> Result<Expr, ParseError> {
        self.binary_level(
            &[(Op::Plus, BinOp::Add), (Op::Minus, BinOp::Sub)],
            Self::multiplicative,
        )
    }

    fn multiplicative(&mut self) -> Result<Expr, ParseError> {
        self.binary_level(
            &[(Op::Star, BinOp::Mul), (Op::Slash, BinOp::Div)],
            Self::power,
        )
    }

    fn power(&mut self) -> Result<Expr, ParseError> {
        self.binary_level(&[(Op::Caret, BinOp::Pow)], Self::unary)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.is_op(Op::Minus) || self.is_op(Op::Plus) {
            let t = self.next();
            self.deeper(t.span.clone())?;
            let inner = self.unary()?;
            self.depth -= 1;
            let span = t.span.start..inner.span.end;
            if matches!(t.tok, Tok::Op(Op::Plus)) {
                return Ok(Expr {
                    kind: inner.kind,
                    span,
                });
            }
            return Ok(Expr {
                kind: ExprKind::Neg(Box::new(inner)),
                span,
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut a = self.primary()?;
        let base = self.chain;
        loop {
            if self.is_op(Op::Percent) {
                let t = self.next();
                self.longer(t.span.clone())?;
                let span = a.span.start..t.span.end;
                a = Expr {
                    kind: ExprKind::Percent(Box::new(a)),
                    span,
                };
            } else if self.is_op(Op::Dot) {
                let dot = self.next();
                self.longer(dot.span.clone())?;
                let t = self.next();
                let name = match &t.tok {
                    Tok::Ident(w) => NameRef::Name(w.clone()),
                    Tok::Bool(b) => NameRef::Name(if *b { "TRUE" } else { "FALSE" }.to_string()),
                    Tok::ColId(id) => NameRef::Id(id.clone()),
                    Tok::ColPos(i) => NameRef::Pos(*i),
                    Tok::ColKey(k) => NameRef::Key(k.clone()),
                    Tok::Bracket(spec) if spec.single && !spec.this_row => match &spec.cols {
                        Cols::One(p) => p.name.clone(),
                        _ => unreachable_member(&t.span)?,
                    },
                    Tok::Err(ErrorCode::Ref) => NameRef::Dead,
                    _ => return Err(ParseError::new(
                        "After \".\" comes the name of a column or a field, like [@vehicle].mpg",
                        t.span,
                    )),
                };
                let span = a.span.start..t.span.end;
                a = Expr {
                    kind: ExprKind::Member(Box::new(a), MemberName { name, span: t.span }),
                    span,
                };
            } else {
                break;
            }
        }
        self.chain = base;
        Ok(a)
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let t = self.next();
        let span = t.span.clone();
        let kind = match t.tok {
            Tok::Num(v) => ExprKind::Number(v),
            Tok::Str(s) => ExprKind::Text(s),
            Tok::Bool(b) => ExprKind::Bool(b),
            Tok::Err(c) => ExprKind::Error(c),
            Tok::Note(name) => {
                let note = NotePart {
                    name,
                    span: span.clone(),
                };
                let n = self.next();
                let r = match n.tok {
                    Tok::Ident(w) => Reference {
                        note: Some(note),
                        table: Some(TablePart {
                            name: NameRef::Name(w),
                            span: n.span.clone(),
                            quoted: false,
                        }),
                        spec: None,
                    },
                    Tok::Table(part, spec) => Reference {
                        note: Some(note),
                        table: Some(part),
                        spec,
                    },
                    _ => {
                        return Err(ParseError::new(
                            "After a note's name and \"!\" comes a table or a field, like 'Budget'!Transactions[amount]",
                            n.span,
                        ))
                    }
                };
                return Ok(Expr {
                    kind: ExprKind::Ref(Box::new(r)),
                    span: span.start..n.span.end,
                });
            }
            Tok::Table(part, spec) => ExprKind::Ref(Box::new(Reference {
                note: None,
                table: Some(part),
                spec,
            })),
            Tok::Bracket(spec) => ExprKind::Ref(Box::new(Reference {
                note: None,
                table: None,
                spec: Some(spec),
            })),
            Tok::Ident(w) => {
                if self.is_op(Op::LParen) {
                    self.next();
                    self.deeper(span.clone())?;
                    let mut args = Vec::new();
                    if !self.is_op(Op::RParen) {
                        loop {
                            if self.is_op(Op::Comma) || self.is_op(Op::RParen) {
                                let at = self.tok().span.start;
                                args.push(Expr {
                                    kind: ExprKind::Missing,
                                    span: at..at,
                                });
                            } else {
                                args.push(self.expr()?);
                            }
                            if self.is_op(Op::Comma) {
                                self.next();
                                continue;
                            }
                            break;
                        }
                    }
                    let close = self.next();
                    if !matches!(close.tok, Tok::Op(Op::RParen)) {
                        return Err(ParseError::new(
                            format!(
                                "Expected \")\" to close {w}( before {}",
                                self.describe(&close)
                            ),
                            close.span,
                        ));
                    }
                    self.depth -= 1;
                    return Ok(Expr {
                        kind: ExprKind::Call(w.to_ascii_uppercase(), args),
                        span: span.start..close.span.end,
                    });
                }
                ExprKind::Name(w)
            }
            Tok::Op(Op::LParen) => {
                self.deeper(span.clone())?;
                let inner = self.expr()?;
                let close = self.next();
                if !matches!(close.tok, Tok::Op(Op::RParen)) {
                    return Err(ParseError::new(
                        format!("Expected \")\" before {}", self.describe(&close)),
                        close.span,
                    ));
                }
                self.depth -= 1;
                return Ok(Expr {
                    kind: inner.kind,
                    span: span.start..close.span.end,
                });
            }
            Tok::End => {
                return Err(ParseError::new("The formula stops too soon", span));
            }
            _ => {
                return Err(ParseError::new(
                    format!("I didn't expect {} here", self.describe(&t)),
                    span,
                ))
            }
        };
        Ok(Expr { kind, span })
    }
}

fn unreachable_member(span: &Span) -> Result<NameRef, ParseError> {
    Err(ParseError::new(
        "After \".\" comes the name of a column or a field",
        span.clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Expr {
        parse(s).unwrap_or_else(|e| panic!("{s}: {e:?}"))
    }

    #[test]
    fn precedence() {
        // -2^2 is (-2)^2.
        let e = p("=-2^2");
        assert!(
            matches!(e.kind, ExprKind::Binary(BinOp::Pow, ref a, _) if matches!(a.kind, ExprKind::Neg(_)))
        );
        // 2^3^2 is (2^3)^2.
        let e = p("2^3^2");
        assert!(
            matches!(e.kind, ExprKind::Binary(BinOp::Pow, ref a, _) if matches!(a.kind, ExprKind::Binary(BinOp::Pow, ..)))
        );
        let e = p("1+2&3=4");
        assert!(matches!(e.kind, ExprKind::Binary(BinOp::Eq, ..)));
    }

    #[test]
    fn structured() {
        let e = p("[@[gas price]]");
        let ExprKind::Ref(r) = e.kind else { panic!() };
        let spec = r.spec.unwrap();
        assert!(spec.this_row && spec.single);
        let Cols::One(c) = spec.cols else { panic!() };
        assert_eq!(c.name, NameRef::Name("gas price".into()));
        assert_eq!(&"[@[gas price]]"[c.span.clone()], "gas price");
        assert_eq!(&"[@[gas price]]"[c.outer.clone()], "[gas price]");

        let e = p("Entries[[hours]:[billable]]");
        let ExprKind::Ref(r) = e.kind else { panic!() };
        assert!(matches!(r.spec.unwrap().cols, Cols::Range(..)));

        let e = p("Trips[[#This Row],[miles]]");
        let ExprKind::Ref(r) = e.kind else { panic!() };
        let s = r.spec.unwrap();
        assert!(s.this_row && !s.single);

        let e = p("'Household budget'!Transactions[amount]");
        let ExprKind::Ref(r) = e.kind else { panic!() };
        assert_eq!(
            r.note.unwrap().name,
            NoteName::Name("Household budget".into())
        );

        let e = p("[@category].[monthly budget]");
        let ExprKind::Member(_, m) = e.kind else {
            panic!()
        };
        assert_eq!(m.name, NameRef::Name("monthly budget".into()));
    }

    #[test]
    fn errors_are_sentences() {
        assert!(parse("=").is_err());
        assert!(parse("\"abc").is_err());
        assert!(parse("SUM(1,2").is_err());
        assert!(parse("[abc").is_err());
        assert!(parse("1 +").is_err());
        assert!(parse("§").is_err());
        let deep = "(".repeat(1000) + "1" + &")".repeat(1000);
        assert!(parse(&deep).is_err());
        let long = vec!["1"; 5000].join("+");
        assert!(parse(&long).is_err());
    }
}

/// A result column's key as it sits in a stored formula, `{rk:<key>}`: every
/// character that could end the token or the brackets around it (and `%`) is
/// written `%XX`, so any column name round-trips.
pub(crate) fn key_token(key: &str) -> String {
    let mut out = String::from("{rk:");
    for c in key.chars() {
        if c.is_alphanumeric() || c == ' ' || c == '_' || c == '-' || c == '.' {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out.push('}');
    out
}

/// The key a `{rk:…}` token's body names.
fn decode_key(body: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = body.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod key_token_tests {
    use super::*;

    fn key_of(formula: &str) -> NameRef {
        let e = parse(formula).unwrap();
        let ExprKind::Ref(r) = &e.kind else { panic!("{e:?}") };
        let spec = r.spec.as_ref().unwrap();
        let Cols::One(p) = &spec.cols else { panic!() };
        p.name.clone()
    }

    #[test]
    fn any_name_round_trips_through_a_key_token() {
        for k in ["at", "day total", "a]b", "x}y", "50%", "it's", "café ☕", "{rk:x}", "#3", "a,b:c"] {
            let token = key_token(k);
            assert!(!token[4..token.len() - 1].contains(['}', '{', ']', '[', '\'']), "{token}");
            assert_eq!(key_of(&format!("=[@{token}]")), NameRef::Key(k.to_string()), "{k}");
        }
    }

    #[test]
    fn a_key_token_after_a_dot_is_a_key() {
        let e = parse(&format!("=[@{{c:x}}].{}", key_token("day total"))).unwrap();
        let ExprKind::Member(_, m) = &e.kind else { panic!("{e:?}") };
        assert_eq!(m.name, NameRef::Key("day total".into()));
    }
}
