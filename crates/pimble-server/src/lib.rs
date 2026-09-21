//! Pimble Server - Local server implementation
//!
//! This crate provides:
//! - JSON-RPC server over HTTP and WebSocket
//! - Store management
//! - Search coordination

pub mod auth;
pub mod cloud;
pub mod credentials;
pub mod error;
mod fs_util;
pub mod handler;
pub mod jwt;
pub mod keystore;
pub mod principal;
pub mod relay_face;
mod relay_tunnel;
pub mod server;
pub mod share;
pub mod sync_link;
pub mod vault_link;

pub use error::*;
pub use handler::*;
pub use principal::{
    authorize, authorize_owner, authorize_service_only, no_grant_for_document_error, read_only_error, readable, scope_roots_of, service_extensions, Access, Grant,
    Principal, Role, NO_GRANT_FOR_DOCUMENT,
};
pub use server::*;
pub use sync_link::*;
