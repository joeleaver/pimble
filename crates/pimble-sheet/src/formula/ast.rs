//! A parsed formula. Every name keeps the byte span it was written at, so
//! turning names into ids and back ([`super::names`]) rewrites only those
//! spans and leaves the rest of the formula exactly as the person typed it.
//!
//! Names come in two forms that the parser accepts alike: the shown form
//! (`'Household budget'!Transactions[amount]`) and the stored form, where
//! each name is an id token (`{n:<store>/<node>}!{t:<table>}[{c:<column>}]`).

use std::ops::Range;

use crate::model::NodeRef;

use super::value::ErrorCode;

pub type Span = Range<usize>;

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Number(f64),
    Text(String),
    Bool(bool),
    /// An error literal, `#N/A`.
    Error(ErrorCode),
    /// An argument left out: `f(a,,b)`.
    Missing,
    /// A bare word: a `LET` name, an aggregate passed as a function
    /// (`SUM`), or a table of this note (`Vehicles`).
    Name(String),
    Ref(Box<Reference>),
    /// `x.name`: follows a link to a row's column or a node's field.
    Member(Box<Expr>, MemberName),
    Neg(Box<Expr>),
    Percent(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// The name is upper-cased.
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

impl BinOp {
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge
        )
    }
}

/// A name, or the id it stands for, or a reference that was deleted
/// (`#REF!` where the name was).
#[derive(Debug, Clone, PartialEq)]
pub enum NameRef {
    Name(String),
    Id(String),
    Dead,
}

/// A structured reference, possibly in another note:
/// `'Note'!Table[spec]`, `Table`, `[spec]`, `'Note'!field`.
#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    pub note: Option<NotePart>,
    /// `None` for a bare `[spec]`, which means the formula's own table.
    pub table: Option<TablePart>,
    /// `None` for a whole table (`Vehicles`) or a note's field (`'Client website'!rate`).
    pub spec: Option<Spec>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NoteName {
    Name(String),
    Id(NodeRef),
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotePart {
    pub name: NoteName,
    /// From the first character to the `!`, inclusive.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TablePart {
    pub name: NameRef,
    pub span: Span,
    /// Written as `'Monthly costs'[...]`, for a name that is not a plain word.
    pub quoted: bool,
}

/// Which part of a table a reference means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    Data,
    Headers,
    Totals,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cols {
    All,
    One(ColPiece),
    Range(ColPiece, ColPiece),
}

/// The inside of the brackets of a structured reference.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    /// `@` or `[#This Row]`.
    pub this_row: bool,
    pub area: Area,
    pub cols: Cols,
    /// One column written alone: `[miles]`, `[@miles]`, `[@[gas price]]`,
    /// `[[amount]]`. Its inner brackets are optional, so they are dropped in
    /// the stored form and added back when the name needs them.
    pub single: bool,
    /// The brackets and everything between them.
    pub span: Span,
}

/// A column named inside brackets.
#[derive(Debug, Clone, PartialEq)]
pub struct ColPiece {
    pub name: NameRef,
    /// The name's own text.
    pub span: Span,
    /// The name with its inner brackets, when it has them.
    pub outer: Span,
    pub bracketed: bool,
}

/// What follows a dot: `.mpg`, `.[monthly budget]`, `.{c:<id>}`.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberName {
    pub name: NameRef,
    /// The piece after the dot, brackets included.
    pub span: Span,
}

impl Expr {
    /// Every direct sub-expression.
    pub fn children(&self) -> Vec<&Expr> {
        match &self.kind {
            ExprKind::Member(o, _) | ExprKind::Neg(o) | ExprKind::Percent(o) => vec![o],
            ExprKind::Binary(_, a, b) => vec![a, b],
            ExprKind::Call(_, args) => args.iter().collect(),
            _ => vec![],
        }
    }
}
