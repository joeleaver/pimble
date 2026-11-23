//! Pimble Server - Local server implementation
//!
//! This crate provides:
//! - JSON-RPC server over HTTP and WebSocket
//! - Store management
//! - Search coordination
//!
//! The same server builds for a browser's worker (`wasm32-unknown-unknown`,
//! docs/ONE_CODEBASE_CONTRACT.md wave 3): there it is reached only through
//! `PimbleServer::start_in_process`, and the modules below that are the
//! desktop's alone (the HTTP edge and its token, JWKS fetching, finding and
//! joining the embedded server, logs, diagnostics, the memory probe, the relay
//! tunnel) are not built.

pub mod agents;
#[cfg(not(target_arch = "wasm32"))]
pub mod auth;
pub mod cloud;
pub mod credentials;
mod devices;
pub use devices::InheritedDevice;
pub mod error;
pub mod faults;
mod folder_watch;
#[cfg(not(target_arch = "wasm32"))]
mod fs_util;
pub mod handler;
#[cfg(not(target_arch = "wasm32"))]
pub mod jwt;
pub mod keystore;
#[cfg(not(target_arch = "wasm32"))]
pub mod local;
#[cfg(not(target_arch = "wasm32"))]
pub mod logs;
#[cfg(not(target_arch = "wasm32"))]
pub mod diagnostics;
#[cfg(not(target_arch = "wasm32"))]
pub mod memory;
mod people;
pub mod pictures;
mod presence;
mod presence_link;
pub mod principal;
pub mod relay_face;
#[cfg(not(target_arch = "wasm32"))]
mod relay_tunnel;
pub mod server;
pub mod share;
pub mod sync_link;
pub mod vault_link;

/// What the core asks of the native-only modules, answered in a browser's
/// worker: no diagnostics yet (section 6, item 10: the worker's own, on its
/// disk, come with the old browser path's removal), no host kind and no
/// hand-over (a worker has no port to hand), and no relay tunnel to ask
/// whether a relayed store's owner is there.
#[cfg(target_arch = "wasm32")]
pub mod diagnostics {
    pub fn ids() -> Option<(String, String)> {
        None
    }
    pub fn set_account(_account: Option<String>) {}
    pub fn record(_kind: &str, _data: serde_json::Value) {}
}

#[cfg(target_arch = "wasm32")]
pub mod local {
    /// Which program this process is, as `serverInfo` reports it.
    pub fn host_kind() -> &'static str {
        "worker"
    }
    pub fn handed_over() {}
}

#[cfg(target_arch = "wasm32")]
mod relay_tunnel {
    /// A member's side: whether the relay says the store's owner is not
    /// there. The desktop reads the relay's close code with its own
    /// WebSocket (`relay_tunnel.rs`); a browser's worker cannot set the
    /// bearer header that needs, so it cannot tell, and a relayed store
    /// whose owner is off reads as plainly offline (wave 4).
    pub(crate) async fn owner_is_offline(_url: &url::Url, _token: &str) -> bool {
        false
    }
}

pub use error::*;
pub use handler::*;
pub use principal::{
    authorize, authorize_owner, authorize_service_only, no_grant_for_document_error, read_only_error, readable, scope_roots_of, service_extensions, Access, Grant,
    Principal, Role, NO_GRANT_FOR_DOCUMENT,
};
pub use server::*;
pub use sync_link::*;
