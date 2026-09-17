//! Pimble CRDT - yrs for node content and the store document
//!
//! This crate provides:
//! - Per-node content documents backed by yrs (`ContentDoc`)
//! - The store document (tree structure + node metadata) backed by yrs
//!   (`StoreDocument`)
//! - Shared error types

pub mod blocks;
pub mod content_doc;
pub mod error;
pub mod store_document;
mod sync_util;

pub use blocks::{blocks_from_plain_text, Align, Block, ListItem, Mark, Run};
pub use content_doc::*;
pub use error::*;
pub use store_document::*;
pub use sync_util::{advance_state_vector, empty_state_vector, state_vector_exceeds};
