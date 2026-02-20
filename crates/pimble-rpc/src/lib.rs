//! Pimble RPC - JSON-RPC protocol definitions
//!
//! This crate defines:
//! - RPC method types and parameters
//! - Request/response types
//! - Server and client traits

pub mod error;
pub mod methods;
pub mod types;

pub use error::*;
pub use methods::*;
pub use types::*;
