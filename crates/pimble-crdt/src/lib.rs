//! Pimble CRDT - yrs for node content and the store document
//!
//! This crate provides:
//! - Per-node content documents backed by yrs (`ContentDoc`)
//! - The store document (tree structure + node metadata) backed by yrs
//!   (`StoreDocument`)
//! - A read-only legacy Automerge reader for one-time migration of old
//!   `store.automerge` files (`LegacyStoreDocument`)
//! - Shared error types

pub mod content_doc;
pub mod error;
pub mod legacy_store_document;
pub mod store_document;

pub use content_doc::*;
pub use error::*;
pub use legacy_store_document::*;
pub use store_document::*;
