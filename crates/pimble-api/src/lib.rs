//! The one Pimble API (docs/API_CONTRACT.md): what the app, the MCP, the
//! CLI's node commands and plugins use to read and write stores, the same
//! way for all of them.
//!
//! - [`Backend`] is what an implementation provides (transport only);
//!   [`rpc::RpcBackend`] is the one over a Pimble server's JSON-RPC.
//! - [`Pimble`] holds the node documents it has read (kept current by the
//!   stores' subscriptions), answers reads from them, and writes through
//!   [`Pimble::edit`], the one write.
//! - [`names`]: links, ids and paths; [`recipes`]: what a new node of each
//!   kind starts as; [`presence`]: frames; [`identity`]: who is writing;
//!   [`book`]: the formula engine's view of held documents.
//!
//! No tokio here: the library builds for `wasm32-unknown-unknown` too.

pub mod api;
pub mod backend;
pub mod book;
pub mod error;
mod held;
pub mod identity;
pub mod names;
pub mod presence;
pub mod recipes;
pub mod router;
pub mod rpc;
pub mod metanode;
pub mod view;
#[cfg(feature = "conformance")]
pub mod conformance;
mod tree_view;

pub use api::{now, Change, Edited, MovedTo, Pimble, Walked};
pub use backend::{Backend, MaybeSend, MaybeSync, Moved, NodeInit, StoreChange};
pub use backend::StoreCapabilities;
pub use error::ApiError;
pub use router::Router;
pub use view::{Affected, Listed, View};
pub use metanode::{Forest, ForestEntry, Judged, MetanodeEdit};
pub use identity::Me;
pub use names::label;
pub use pimble_core::plan;

use pimble_core::{NodeId, PimbleUrl, StoreId};

/// A node by its canonical address: the store it lives in (a mount's
/// source, for a node seen through a mount) and its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Addr {
    pub store: StoreId,
    pub node: NodeId,
}

impl Addr {
    pub fn new(store: StoreId, node: NodeId) -> Self {
        Self { store, node }
    }

    /// Its `pimble:` link, what "Copy Link" gives.
    pub fn link(&self) -> String {
        PimbleUrl { store: self.store, node: self.node, anchor: None }.to_string()
    }
}
