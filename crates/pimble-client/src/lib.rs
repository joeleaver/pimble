//! Pimble Client - Client library for connecting to Pimble servers
//!
//! This crate provides:
//! - JSON-RPC client for communicating with servers
//! - High-level API for store and node operations
//! - Connection management

pub mod client;
pub mod error;
pub mod in_process;
#[cfg(target_arch = "wasm32")]
pub mod message_port;

pub use client::*;
pub use error::*;
