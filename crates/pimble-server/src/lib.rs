//! Pimble Server - Local server implementation
//!
//! This crate provides:
//! - JSON-RPC server over HTTP and WebSocket
//! - Store management
//! - Search coordination

pub mod error;
pub mod handler;
pub mod server;
pub mod sync_link;

pub use error::*;
pub use handler::*;
pub use server::*;
pub use sync_link::*;
