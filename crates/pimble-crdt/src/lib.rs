//! Pimble CRDT - yrs for node content and the store document
//!
//! This crate provides:
//! - Per-node content documents backed by yrs (`ContentDoc`)
//! - The store document (tree structure + node metadata) backed by yrs
//!   (`StoreDocument`)
//! - Shared error types

pub mod content_doc;
pub mod error;
pub mod store_document;

pub use content_doc::*;
pub use error::*;
pub use store_document::*;
