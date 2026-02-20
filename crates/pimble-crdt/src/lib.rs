//! Pimble CRDT - Automerge integration for conflict-free data synchronization
//!
//! This crate provides:
//! - CRDT document management using Automerge
//! - Change tracking and merging
//! - Node content serialization

pub mod document;
pub mod error;
pub mod node_content;

pub use document::*;
pub use error::*;
pub use node_content::*;
