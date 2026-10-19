//! Stored by identity, shown by name (contract section 6).
//!
//! A stored formula is the Excel text with each name replaced by an id
//! token: `{n:<store>/<node>}!` for a note, `{t:<table>}` for a table,
//! `{c:<column>}` for a column, inside brackets and after a dot that follows
//! a row link whose table is known. A reference to this note's own tables
//! carries no note token, so tables copied together keep pointing at each
//! other. A node's fields, a summary's result columns (which have no ids)
//! and links whose table cannot be known before computing keep their names.
//!
//! Both directions rewrite only the spans of names, so the rest of the
//! formula stays as it was typed, and `to_stored(to_shown(s)) == s`. A
//! single column's optional inner brackets are dropped when stored
//! (`[@[gas price]]` is `[@{c:…}]`) and added back when shown if the name
//! needs them, so a rename to a name with spaces still shows correctly.
//!
//! When a link's target table is needed it is found statically: a row-link
//! column names its table, a formula column's formula is followed (a few
//! levels deep), and a summary's result columns carry the table their links
//! came from, which takes computing the summary.

use crate::book::{Book, Reach};
use crate::model::{ColumnType, NodeRef, TableSnap};

use super::ast::*;
use super::engine::{norm, ColSrc, Engine};
use super::parser::{parse, ParseError};
use super::value::{Agg, TableAddr};

/// Why a formula's names cannot be turned into ids.
#[derive(Debug, Clone, PartialEq)]
pub struct NameError {
    pub kind: NameErrorKind,
    pub message: String,
    /// Where in the formula, to underline.
    pub span: Span,
    /// For an ambiguous name: what it could mean.
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameErrorKind {
    /// The formula does not parse.
    Parse,
    /// A name that names nothing.
    Unknown,
    /// A name that names several things.
    Ambiguous,
    /// A note that cannot be read right now, so its names cannot be checked.
    Unreadable,
}

/// One thing an ambiguous name could mean.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// As a person would recognize it: a note's path, a table's name.
    pub name: String,
    /// The id token that would be stored for it.
    pub token: String,
}

impl NameError {
    pub(crate) fn parse(e: &ParseError) -> Self {
        NameError {
            kind: NameErrorKind::Parse,
            message: e.message.clone(),
            span: e.span.clone(),
            candidates: Vec::new(),
        }
    }

    pub(crate) fn unknown(message: impl Into<String>, span: Span) -> Self {
        NameError {
            kind: NameErrorKind::Unknown,
            message: message.into(),
            span,
            candidates: Vec::new(),
        }
    }

    fn ambiguous(message: impl Into<String>, span: Span, candidates: Vec<Candidate>) -> Self {
        NameError {
            kind: NameErrorKind::Ambiguous,
            message: message.into(),
            span,
            candidates,
        }
    }

    fn unreadable(message: impl Into<String>, span: Span) -> Self {
        NameError {
            kind: NameErrorKind::Unreadable,
            message: message.into(),
            span,
            candidates: Vec::new(),
        }
    }
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NameError {}

/// The id token of a note.
pub fn note_token(n: &NodeRef) -> String {
    format!("{{n:{}/{}}}", n.store, n.node)
}

/// A formula as typed (names) to its stored form (ids), for a formula of
/// the table `home_table_id` in `book.home()`, or of no table.
pub fn to_stored(
    book: &dyn Book,
    home_table_id: Option<&str>,
    shown: &str,
) -> Result<String, NameError> {
    let en = Engine::new(book);
    to_stored_with(&en, &book.home(), home_table_id, shown)
}

/// A stored formula with its ids turned into today's names. An id that no
/// longer resolves shows as `#REF!` in place of the whole reference; ids in
/// a note that cannot be read stay as they are.
pub fn to_shown(book: &dyn Book, home_table_id: Option<&str>, stored: &str) -> String {
    let en = Engine::new(book);
    to_shown_with(&en, &book.home(), home_table_id, stored)
}

pub(crate) fn to_stored_with(
    en: &Engine,
    node: &NodeRef,
    table: Option<&str>,
    shown: &str,
) -> Result<String, NameError> {
    let expr = parse(shown).map_err(|e| NameError::parse(&e))?;
    let mut w = Walk::new(en, node, table);
    w.stored(&expr, &mut Vec::new())?;
    Ok(apply(shown, w.edits))
}

pub(crate) fn to_shown_with(
    en: &Engine,
    node: &NodeRef,
    table: Option<&str>,
    stored: &str,
) -> String {
    let Ok(expr) = parse(stored) else {
        return stored.to_string();
    };
    let mut w = Walk::new(en, node, table);
    w.shown(&expr, &mut Vec::new());
    apply(stored, w.edits)
}

fn apply(src: &str, mut edits: Vec<(Span, String)>) -> String {
    edits.sort_by_key(|(s, _)| s.start);
    let mut out = String::with_capacity(src.len() + 32);
    let mut at = 0;
    for (span, text) in edits {
        if span.start < at {
            continue;
        }
        out.push_str(&src[at..span.start]);
        out.push_str(&text);
        at = span.end;
    }
    out.push_str(&src[at..]);
    out
}

/// A word the lexer reads as one name: letters, digits and `_`, starting
/// with a letter or `_`.
pub(crate) fn is_plain(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
        && !s.eq_ignore_ascii_case("true")
        && !s.eq_ignore_ascii_case("false")
}

/// A note as written before `!`.
pub fn note_text(name: &str) -> String {
    if is_plain(name) {
        format!("{name}!")
    } else {
        format!("'{}'!", name.replace('\'', "''"))
    }
}

fn table_text(name: &str, whole: bool) -> String {
    if is_plain(name) && (!whole || Agg::by_name(name).is_none()) {
        name.to_string()
    } else {
        format!(
            "'{}'{}",
            name.replace('\'', "''"),
            if whole { "[#All]" } else { "" }
        )
    }
}

fn escape_col(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '\'' | '[' | ']' | '#') {
            s.push('\'');
        }
        s.push(c);
    }
    s
}

fn col_text(name: &str, spec: &Spec, piece: &ColPiece) -> String {
    let e = escape_col(name);
    if piece.bracketed || !spec.single {
        return e;
    }
    let needs = if spec.this_row {
        !name.chars().all(|c| c.is_alphanumeric() || c == '_') || name.is_empty()
    } else {
        name.is_empty() || name != name.trim() || name.starts_with('@')
    };
    if needs {
        format!("[{e}]")
    } else {
        e
    }
}

fn member_text(name: &str) -> String {
    if is_plain(name) {
        name.to_string()
    } else {
        format!("[{}]", escape_col(name))
    }
}

type Env = Vec<(String, Option<TableAddr>)>;

struct Walk<'e, 'b> {
    en: &'e Engine<'b>,
    book: &'b dyn Book,
    node: NodeRef,
    table: Option<&'b TableSnap>,
    edits: Vec<(Span, String)>,
}

impl<'e, 'b> Walk<'e, 'b> {
    fn new(en: &'e Engine<'b>, node: &NodeRef, table: Option<&str>) -> Self {
        let t = table.and_then(|id| en.snap(node).ok().and_then(|s| s.table(id)));
        Walk {
            en,
            book: en.book(),
            node: *node,
            table: t,
            edits: Vec::new(),
        }
    }

    fn edit(&mut self, span: Span, text: impl Into<String>) {
        self.edits.push((span, text.into()));
    }

    // ── Static link targets ─────────────────────────────────────────────

    fn ref_table(
        &self,
        r: &Reference,
        node: &NodeRef,
        table: Option<&'b TableSnap>,
    ) -> Option<(NodeRef, &'b TableSnap)> {
        let n = match &r.note {
            None => *node,
            Some(np) => self.en.resolve_note(&np.name).ok()?,
        };
        let t = match &r.table {
            None if n == *node => table?,
            None => return None,
            Some(part) => self
                .en
                .find_table(self.en.snap(&n).ok()?, &part.name)
                .ok()??,
        };
        Some((n, t))
    }

    /// The table a link column's values point into.
    fn col_target(
        &self,
        node: &NodeRef,
        t: &'b TableSnap,
        name: &NameRef,
        depth: usize,
    ) -> Option<TableAddr> {
        // Own columns first, without computing anything.
        let own = match name {
            NameRef::Id(id) => t.column(id),
            NameRef::Name(n) => {
                let n = norm(n);
                t.columns.iter().find(|c| norm(&c.name) == n)
            }
            NameRef::Dead => return None,
        };
        if let Some(c) = own {
            return match &c.ty {
                ColumnType::RowLink { table } => Some(TableAddr {
                    node: *node,
                    table: table.clone(),
                }),
                ColumnType::Formula { formula, .. } if depth < 6 => {
                    let p = self.en.parse_cached(formula);
                    let e = p.as_ref().as_ref().ok()?;
                    self.target(e, node, Some(t), &mut Vec::new(), depth + 1)
                }
                _ => None,
            };
        }
        if !t.is_summary() {
            return None;
        }
        let cols = self.en.table_cols(node, t).ok()?;
        let i = self.en.find_col(&cols, name)?;
        match cols[i].src {
            ColSrc::Result(_) => cols[i].meta.link.clone(),
            ColSrc::Own(_) => None,
        }
    }

    fn target(
        &self,
        e: &Expr,
        node: &NodeRef,
        table: Option<&'b TableSnap>,
        env: &mut Env,
        depth: usize,
    ) -> Option<TableAddr> {
        if depth > 8 {
            return None;
        }
        match &e.kind {
            ExprKind::Name(n) => {
                let key = norm(n);
                if let Some((_, t)) = env.iter().rev().find(|(k, _)| *k == key) {
                    return t.clone();
                }
                // A whole table: where a lookup returns one of its rows.
                let snap = self.en.snap(node).ok()?;
                let t = self.en.find_table(snap, &NameRef::Name(n.clone())).ok()??;
                Some(TableAddr {
                    node: *node,
                    table: t.id.clone(),
                })
            }
            ExprKind::Ref(r) => {
                let (n, t) = self.ref_table(r, node, table)?;
                let whole = TableAddr {
                    node: n,
                    table: t.id.clone(),
                };
                let Some(spec) = r.spec.as_ref() else {
                    return Some(whole);
                };
                match &spec.cols {
                    Cols::One(p) => self.col_target(&n, t, &p.name, depth),
                    // `[@]` is this row; a range or the whole table gives
                    // its rows to a lookup.
                    Cols::All | Cols::Range(..) => Some(whole),
                }
            }
            ExprKind::Member(o, m) => {
                let tt = self.target(o, node, table, env, depth + 1)?;
                let snap = self.en.snap(&tt.node).ok()?;
                let t = snap.table(&tt.table)?;
                self.col_target(&tt.node, t, &m.name, depth + 1)
            }
            ExprKind::Call(name, args) => {
                let arg = |i: usize, env: &mut Env| {
                    args.get(i)
                        .and_then(|a| self.target(a, node, table, env, depth + 1))
                };
                match name.as_str() {
                    "XLOOKUP" => arg(2, env),
                    "INDEX" | "FILTER" | "SORT" | "UNIQUE" | "IFERROR" | "IFNA" => arg(0, env),
                    "IF" => arg(1, env).or_else(|| arg(2, env)),
                    "CHOOSE" => arg(1, env),
                    "LET" if args.len() >= 3 && args.len() % 2 == 1 => {
                        let before = env.len();
                        for pair in args[..args.len() - 1].chunks(2) {
                            if let ExprKind::Name(n) = &pair[0].kind {
                                let t = self.target(&pair[1], node, table, env, depth + 1);
                                env.push((norm(n), t));
                            }
                        }
                        let r = self.target(&args[args.len() - 1], node, table, env, depth + 1);
                        env.truncate(before);
                        r
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ── Shown to stored ─────────────────────────────────────────────────

    fn stored(&mut self, e: &Expr, env: &mut Env) -> Result<(), NameError> {
        match &e.kind {
            ExprKind::Name(n) => {
                if env.iter().any(|(k, _)| *k == norm(n)) || Agg::by_name(n).is_some() {
                    return Ok(());
                }
                let snap = self
                    .en
                    .snap(&self.node)
                    .map_err(|err| NameError::unreadable(err.message, e.span.clone()))?;
                let hits: Vec<&TableSnap> = snap
                    .tables
                    .iter()
                    .filter(|t| norm(&t.name) == norm(n))
                    .collect();
                match hits.len() {
                    1 => {
                        let id = hits[0].id.clone();
                        self.edit(e.span.clone(), format!("{{t:{id}}}"));
                        Ok(())
                    }
                    0 => {
                        let list: Vec<&str> = snap.tables.iter().map(|t| t.name.as_str()).collect();
                        Err(NameError::unknown(
                            if list.is_empty() {
                                format!("Nothing here is called {n}")
                            } else {
                                format!(
                                    "Nothing here is called {n}; the tables here are {}",
                                    list.join(", ")
                                )
                            },
                            e.span.clone(),
                        ))
                    }
                    k => Err(NameError::ambiguous(
                        format!("{k} tables here are called {n}; rename one"),
                        e.span.clone(),
                        hits.iter()
                            .map(|t| Candidate {
                                name: t.name.clone(),
                                token: format!("{{t:{}}}", t.id),
                            })
                            .collect(),
                    )),
                }
            }
            ExprKind::Ref(r) => self.stored_ref(r),
            ExprKind::Member(o, m) => {
                self.stored(o, env)?;
                let NameRef::Name(s) = &m.name else {
                    return Ok(());
                };
                let Some(tt) = self.target(o, &self.node.clone(), self.table, &mut env.clone(), 0)
                else {
                    return Ok(());
                };
                let Ok(snap) = self.en.snap(&tt.node) else {
                    return Ok(());
                };
                let Some(t) = snap.table(&tt.table) else {
                    return Ok(());
                };
                let hits: Vec<_> = t
                    .columns
                    .iter()
                    .filter(|c| norm(&c.name) == norm(s))
                    .collect();
                match hits.len() {
                    1 => {
                        let id = hits[0].id.clone();
                        self.edit(m.span.clone(), format!("{{c:{id}}}"));
                        Ok(())
                    }
                    0 => {
                        if t.is_summary() {
                            return Ok(());
                        }
                        let list: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
                        Err(NameError::unknown(
                            format!(
                                "{} has no column \"{s}\"; its columns are {}",
                                t.name,
                                list.join(", ")
                            ),
                            m.span.clone(),
                        ))
                    }
                    k => Err(NameError::ambiguous(
                        format!("{k} columns of {} are called {s}; rename one", t.name),
                        m.span.clone(),
                        hits.iter()
                            .map(|c| Candidate {
                                name: c.name.clone(),
                                token: format!("{{c:{}}}", c.id),
                            })
                            .collect(),
                    )),
                }
            }
            ExprKind::Call(name, args)
                if name == "LET" && args.len() >= 3 && args.len() % 2 == 1 =>
            {
                let before = env.len();
                for pair in args[..args.len() - 1].chunks(2) {
                    let ExprKind::Name(n) = &pair[0].kind else {
                        self.stored(&pair[0], env)?;
                        self.stored(&pair[1], env)?;
                        continue;
                    };
                    self.stored(&pair[1], env)?;
                    let t = self.target(
                        &pair[1],
                        &self.node.clone(),
                        self.table,
                        &mut env.clone(),
                        0,
                    );
                    env.push((norm(n), t));
                }
                let r = self.stored(&args[args.len() - 1], env);
                env.truncate(before);
                r
            }
            _ => {
                for c in e.children() {
                    self.stored(c, env)?;
                }
                Ok(())
            }
        }
    }

    fn stored_ref(&mut self, r: &Reference) -> Result<(), NameError> {
        let mut note_edit: Option<(Span, NodeRef)> = None;
        let node = match &r.note {
            None => self.node,
            Some(np) => match &np.name {
                NoteName::Id(n) => *n,
                NoteName::Name(s) => {
                    let found = self.book.find_nodes(s);
                    match found.len() {
                        0 => {
                            return Err(NameError::unknown(
                                format!("No note is called \"{s}\""),
                                np.span.clone(),
                            ))
                        }
                        1 => {
                            note_edit = Some((np.span.clone(), found[0]));
                            found[0]
                        }
                        k => {
                            return Err(NameError::ambiguous(
                                format!("{k} notes are called \"{s}\"; which one?"),
                                np.span.clone(),
                                found
                                    .iter()
                                    .map(|n| Candidate {
                                        name: self.book.display_name(n),
                                        token: note_token(n),
                                    })
                                    .collect(),
                            ))
                        }
                    }
                }
            },
        };
        let names_needed = matches!(r.table.as_ref().map(|p| &p.name), Some(NameRef::Name(_)))
            || r.spec
                .as_ref()
                .is_some_and(|s| pieces(s).iter().any(|p| matches!(p.name, NameRef::Name(_))));
        let snap = match self.en.snap(&node) {
            Ok(s) => s,
            Err(err) => {
                if names_needed {
                    let span = r.table.as_ref().map(|p| p.span.clone()).unwrap_or(0..0);
                    return Err(NameError::unreadable(
                        format!("{}, so its names can't be checked", err.message),
                        span,
                    ));
                }
                if let Some((span, n)) = note_edit {
                    self.edit(span, format!("{}!", note_token(&n)));
                }
                return Ok(());
            }
        };
        let note_label = || {
            if r.note.is_some() {
                self.en.note_name(&node)
            } else {
                "this note".to_string()
            }
        };
        let mut table_edit: Option<(Span, String)> = None;
        let table: &TableSnap = match &r.table {
            None => match self.table {
                Some(t) if node == self.node => t,
                _ => {
                    return Err(NameError::unknown(
                        "A bare [column] only means something in a table's own formulas",
                        r.spec.as_ref().map(|s| s.span.clone()).unwrap_or(0..0),
                    ))
                }
            },
            Some(part) => match &part.name {
                NameRef::Dead => return Ok(()),
                NameRef::Id(id) => match snap.table(id) {
                    Some(t) => t,
                    None => {
                        if let Some((span, n)) = note_edit {
                            self.edit(span, format!("{}!", note_token(&n)));
                        }
                        return Ok(());
                    }
                },
                NameRef::Name(n) => {
                    let hits: Vec<&TableSnap> = snap
                        .tables
                        .iter()
                        .filter(|t| norm(&t.name) == norm(n))
                        .collect();
                    match hits.len() {
                        1 => {
                            table_edit = Some((part.span.clone(), format!("{{t:{}}}", hits[0].id)));
                            hits[0]
                        }
                        0 => {
                            // `'Client website'!rate`: a node's field.
                            if r.note.is_some() && r.spec.is_none() {
                                let ok = self.book.node(&node).is_some_and(|info| {
                                    norm(n) == "title"
                                        || norm(n) == "name"
                                        || info.fields.iter().any(|(k, _)| norm(k) == norm(n))
                                });
                                if ok {
                                    if let Some((span, nr)) = note_edit {
                                        self.edit(span, format!("{}!", note_token(&nr)));
                                    }
                                    return Ok(());
                                }
                                return Err(NameError::unknown(
                                    format!("{} has no table or field called {n}", note_label()),
                                    part.span.clone(),
                                ));
                            }
                            let list: Vec<&str> =
                                snap.tables.iter().map(|t| t.name.as_str()).collect();
                            return Err(NameError::unknown(
                                if list.is_empty() {
                                    format!("There's no table called {n} in {}", note_label())
                                } else {
                                    format!(
                                        "There's no table called {n} in {}; its tables are {}",
                                        note_label(),
                                        list.join(", ")
                                    )
                                },
                                part.span.clone(),
                            ));
                        }
                        k => {
                            return Err(NameError::ambiguous(
                                format!(
                                    "{k} tables in {} are called {n}; rename one",
                                    note_label()
                                ),
                                part.span.clone(),
                                hits.iter()
                                    .map(|t| Candidate {
                                        name: t.name.clone(),
                                        token: format!("{{t:{}}}", t.id),
                                    })
                                    .collect(),
                            ))
                        }
                    }
                }
            },
        };
        // `Vehicles[#All]` and `Vehicles[]` are the whole table, stored as `{t:…}`.
        if let Some((span, text)) = table_edit {
            let whole = r
                .spec
                .as_ref()
                .filter(|s| !s.this_row && s.area == Area::Data && matches!(s.cols, Cols::All));
            let span = match whole {
                Some(spec) => span.start..spec.span.end,
                None => span,
            };
            self.edit(span, text);
        }
        // The note prefix: none for this note's own tables.
        if let Some(np) = &r.note {
            if node == self.node {
                self.edit(np.span.clone(), "");
            } else if let Some((span, n)) = note_edit {
                self.edit(span, format!("{}!", note_token(&n)));
            }
        }
        let Some(spec) = &r.spec else { return Ok(()) };
        if spec.this_row {
            match self.table {
                Some(t) if node == self.node && t.id == table.id => {}
                Some(_) => {
                    return Err(NameError::unknown(
                        format!("[@…] reads this row, and this row isn't in {}", table.name),
                        spec.span.clone(),
                    ))
                }
                None => {
                    return Err(NameError::unknown(
                        "[@column] reads this row, so it only means something in a table's formula column",
                        spec.span.clone(),
                    ))
                }
            }
        }
        for p in pieces(spec) {
            let NameRef::Name(s) = &p.name else { continue };
            let hits: Vec<_> = table
                .columns
                .iter()
                .filter(|c| norm(&c.name) == norm(s))
                .collect();
            match hits.len() {
                1 => {
                    let span = if spec.single {
                        p.outer.clone()
                    } else {
                        p.span.clone()
                    };
                    self.edit(span, format!("{{c:{}}}", hits[0].id));
                }
                0 => {
                    // A summary's result columns have only names.
                    if table.is_summary() {
                        match self.en.table_cols(&node, table) {
                            Err(_) => continue,
                            Ok(cols) if self.en.find_col(&cols, &p.name).is_some() => continue,
                            Ok(cols) => {
                                let list: Vec<String> =
                                    cols.iter().map(|c| c.meta.name.clone()).collect();
                                return Err(NameError::unknown(
                                    format!(
                                        "{} has no column \"{s}\"; its columns are {}",
                                        table.name,
                                        list.join(", ")
                                    ),
                                    p.span.clone(),
                                ));
                            }
                        }
                    }
                    let list: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
                    return Err(NameError::unknown(
                        format!(
                            "{} has no column \"{s}\"; its columns are {}",
                            table.name,
                            list.join(", ")
                        ),
                        p.span.clone(),
                    ));
                }
                k => {
                    return Err(NameError::ambiguous(
                        format!("{k} columns of {} are called {s}; rename one", table.name),
                        p.span.clone(),
                        hits.iter()
                            .map(|c| Candidate {
                                name: c.name.clone(),
                                token: format!("{{c:{}}}", c.id),
                            })
                            .collect(),
                    ))
                }
            }
        }
        Ok(())
    }

    // ── Stored to shown ─────────────────────────────────────────────────

    fn shown(&mut self, e: &Expr, env: &mut Env) {
        match &e.kind {
            ExprKind::Ref(r) => self.shown_ref(r, e.span.clone()),
            ExprKind::Member(o, m) => {
                if let NameRef::Id(id) = &m.name {
                    let tt = self.target(o, &self.node.clone(), self.table, &mut env.clone(), 0);
                    let name = tt
                        .and_then(|tt| {
                            let snap = self.en.snap(&tt.node).ok()?;
                            snap.table(&tt.table)?.column(id).map(|c| c.name.clone())
                        })
                        .or_else(|| {
                            // The target could not be worked out: column ids
                            // are unique, so look in this note's tables.
                            let snap = self.en.snap(&self.node).ok()?;
                            snap.tables
                                .iter()
                                .find_map(|t| t.column(id))
                                .map(|c| c.name.clone())
                        });
                    match name {
                        Some(n) => self.edit(m.span.clone(), member_text(&n)),
                        None => {
                            self.edit(e.span.clone(), "#REF!");
                            return;
                        }
                    }
                }
                self.shown(o, env);
            }
            ExprKind::Call(name, args)
                if name == "LET" && args.len() >= 3 && args.len() % 2 == 1 =>
            {
                let before = env.len();
                for pair in args[..args.len() - 1].chunks(2) {
                    self.shown(&pair[1], env);
                    if let ExprKind::Name(n) = &pair[0].kind {
                        let t = self.target(
                            &pair[1],
                            &self.node.clone(),
                            self.table,
                            &mut env.clone(),
                            0,
                        );
                        env.push((norm(n), t));
                    }
                }
                self.shown(&args[args.len() - 1], env);
                env.truncate(before);
            }
            _ => {
                for c in e.children() {
                    self.shown(c, env);
                }
            }
        }
    }

    fn shown_ref(&mut self, r: &Reference, span: Span) {
        let mut local: Vec<(Span, String)> = Vec::new();
        let node = match &r.note {
            None => self.node,
            Some(np) => match &np.name {
                NoteName::Name(_) => return,
                NoteName::Id(n) => {
                    if matches!(self.book.sheets(n), Reach::Missing) {
                        self.edit(span, "#REF!");
                        return;
                    }
                    local.push((np.span.clone(), note_text(&self.book.display_name(n))));
                    *n
                }
            },
        };
        let Ok(snap) = self.en.snap(&node) else {
            // Unreadable: the note is named, its ids stay as they are.
            self.edits.extend(local);
            return;
        };
        let table: &TableSnap = match &r.table {
            None => match self.table {
                Some(t) if node == self.node => t,
                _ => {
                    self.edits.extend(local);
                    return;
                }
            },
            Some(part) => match &part.name {
                NameRef::Dead => {
                    self.edits.extend(local);
                    return;
                }
                NameRef::Id(id) => match snap.table(id) {
                    Some(t) => {
                        local.push((part.span.clone(), table_text(&t.name, r.spec.is_none())));
                        t
                    }
                    None => {
                        self.edit(span, "#REF!");
                        return;
                    }
                },
                NameRef::Name(n) => match self.en.find_table(snap, &NameRef::Name(n.clone())) {
                    Ok(Some(t)) => t,
                    _ => {
                        self.edits.extend(local);
                        return;
                    }
                },
            },
        };
        if let Some(spec) = &r.spec {
            for p in pieces(spec) {
                if let NameRef::Id(id) = &p.name {
                    match table.column(id) {
                        Some(c) => {
                            let text = col_text(&c.name, spec, p);
                            local.push((p.span.clone(), text));
                        }
                        None => {
                            self.edit(span, "#REF!");
                            return;
                        }
                    }
                }
            }
        }
        self.edits.extend(local);
    }
}

fn pieces(spec: &Spec) -> Vec<&ColPiece> {
    match &spec.cols {
        Cols::All => vec![],
        Cols::One(p) => vec![p],
        Cols::Range(a, b) => vec![a, b],
    }
}
