//! Pimble Store - Storage abstraction for local and remote stores
//!
//! This crate provides:
//! - Local file-based store implementation
//! - Store management (create, open, close)
//! - Node persistence using yrs CRDT documents

pub mod blobs;
pub mod error;
pub mod local;
pub mod manager;
pub mod registry;
pub mod sync_state;
pub mod vault;
// The folder watch (`notify`) is the desktop's only (docs/ONE_CODEBASE_CONTRACT.md seam B).
#[cfg(not(target_arch = "wasm32"))]
pub mod watch;

pub use blobs::*;
pub use error::*;
pub use local::*;
pub use manager::*;
pub use registry::*;
pub use vault::*;
#[cfg(not(target_arch = "wasm32"))]
pub use watch::{watch_folder, FolderWatch};
