//! Pimble RPC - JSON-RPC protocol definitions
//!
//! This crate defines:
//! - RPC method types and parameters
//! - Request/response types
//! - Server and client traits

pub mod dispatch;
pub mod error;
pub mod methods;
pub mod types;
pub mod vault;
pub mod vault_cursor;

pub use error::*;
pub use methods::*;
pub use types::*;
pub use vault_cursor::VaultCursor;

/// The largest JSON-RPC message a Pimble server or client sends or takes:
/// big enough that no document a person makes stops syncing (a 4.9 MB canvas,
/// base64 in a sync answer, passed jsonrpsee's 10 MB default on 2026-11-18).
pub const MAX_MESSAGE_BYTES: u32 = 512 * 1024 * 1024;
