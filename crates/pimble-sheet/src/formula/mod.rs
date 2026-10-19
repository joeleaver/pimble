//! The Excel formula engine (docs/SHEETS_CONTRACT.md sections 4 and 6).
//!
//! Formulas are Excel's, as they work inside an Excel Table: structured
//! references (`[@miles]`, `Transactions[amount]`, `Entries[[hours]:[billable]]`),
//! Excel's operators, precedence and functions, serial dates and Excel's
//! errors. Pimble adds two things: a dot follows a link (`[@vehicle].mpg`,
//! `[@project].rate`), and other notes are named like other workbooks
//! (`'Household budget'!Transactions[amount]`, `'Client website'!rate`).
//!
//! - [`parse`]: formula text, shown or stored, to an [`ast::Expr`].
//! - [`to_stored`] / [`to_shown`]: names to ids and back ("stored by
//!   identity, shown by name").
//! - [`Engine`]: computes a node's tables ([`Engine::compute_node`]), runs
//!   one formula ([`Engine::evaluate`]), labels rows and formats values.
//! - [`Value`] and [`FormulaError`]: what a cell holds, errors with the
//!   sentence the formula bar shows.
//!
//! Known differences from Excel, all deliberate: a whole table row returned
//! by `XLOOKUP` is a link to that row (so `.column` can follow it) rather
//! than a spilled row; `SORT` puts blanks last; `GROUPBY` adds no totals row
//! unless asked (open decision 5); `#CIRC!` and the loading state are
//! values that `IFERROR` cannot catch; there are no A1 references, array
//! constants or `LAMBDA`.

pub mod ast;
mod coerce;
mod criteria;
pub mod dates;
mod engine;
mod format;
mod functions;
mod names;
mod parser;
mod value;

pub use coerce::general as general_number;
pub use engine::{ComputedColumn, ComputedRow, ComputedTable, Engine};
pub use format::{format_number, hours, money, round_half_away, short_date, text_number};
pub use functions::CATEGORIES as FUNCTIONS;
pub use names::{note_text, note_token, to_shown, to_stored, Candidate, NameError, NameErrorKind};
pub use parser::{parse, ParseError};
pub use value::{
    ColMeta, ErrorCode, Evaluated, FormulaError, ResultTable, RowRef, TableAddr, Value,
};
