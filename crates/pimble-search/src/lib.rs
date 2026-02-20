//! Pimble Search - Search and indexing for semantic and full-text search
//!
//! This crate will provide (Phase 4):
//! - Vector database for semantic search
//! - Full-text search using Tantivy
//! - Embedding generation using local models

pub mod error;
pub mod index;
pub mod query;

pub use error::*;
pub use index::*;
pub use query::*;
