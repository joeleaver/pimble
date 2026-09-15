//! Pimble Store - Storage abstraction for local and remote stores
//!
//! This crate provides:
//! - Local file-based store implementation
//! - Store management (create, open, close)
//! - Node persistence using yrs CRDT documents

pub mod error;
pub mod local;
pub mod manager;
pub mod registry;
pub mod vault;

pub use error::*;
pub use local::*;
pub use manager::*;
pub use registry::*;
pub use vault::*;
