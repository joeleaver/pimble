//! Pimble Plugins - WASM plugin host and built-in plugins
//!
//! This crate provides (Phase 6):
//! - WASM plugin host using wasmtime
//! - Plugin interface definitions
//! - Built-in plugin implementations

pub mod error;
pub mod host;
pub mod interface;

pub use error::*;
pub use host::*;
pub use interface::*;
