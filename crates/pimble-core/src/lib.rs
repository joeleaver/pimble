//! Pimble Core - Core types and traits for the personal information manager
//!
//! This crate defines the fundamental data structures used throughout Pimble:
//! - `Node`: The basic unit of content
//! - `Store`: A container for a tree of nodes
//! - `Workspace`: User's view into one or more stores

pub mod node;
pub mod store;
pub mod workspace;
pub mod error;

pub use node::*;
pub use store::*;
pub use workspace::*;
pub use error::*;
