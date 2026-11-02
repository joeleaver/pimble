//! Sheets (docs/SHEETS_CONTRACT.md): small live tables in a node's `data`
//! root, computed with Excel's formula language. One crate for the app
//! (desktop and web) and the MCP server, native and wasm32, no UI:
//!
//! - [`model`]: what a node's sheets hold, as plain values ([`SheetsSnap`]).
//! - [`doc`]: the sheet document over yrs (contract section 2). Every write
//!   is one transaction and answers an ordinary yrs update of the node's
//!   document, which goes through `applyEdit` like any other edit.
//! - [`input`]: what a person types into a cell, parsed by the column's type.
//! - [`formula`]: the Excel formula engine (contract section 6): parsing,
//!   names to ids and back, evaluation, and the computed tables.
//! - [`book`]: what the engine can see beyond its own node, and naming nodes
//!   by path.
//! - [`ops`]: tables, columns, rows and nodes by name, column types from
//!   words, values as typed, formula checks, carry-over and moving rules:
//!   what the app, the MCP and plugins do to a note's tables, written once.

pub mod book;
pub mod doc;
pub mod formula;
pub mod index;
pub mod input;
pub mod model;
pub mod ops;

pub use book::{Book, NodeInfo, Reach, Titles};
pub use model::*;
