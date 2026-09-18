//! Error types for pimble-store

use std::path::PathBuf;

use pimble_core::{NodeId, StoreId};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Store not found: {0}")]
    StoreNotFound(StoreId),

    /// A manifest version this crate neither reads nor migrates
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 3: version 3 is migrated at
    /// open, version 4 is current).
    #[error(
        "Store at {path:?} predates the formats this version reads (found manifest version {version}, expected 3 or 4) and must be re-imported"
    )]
    UnsupportedFormat { path: PathBuf, version: u32 },

    #[error("Node not found: {0}")]
    NodeNotFound(NodeId),

    #[error("Store already exists at path: {0}")]
    StoreExists(String),

    #[error("Invalid store path: {0}")]
    InvalidPath(String),

    #[error("Store not open: {0}")]
    NotOpen(StoreId),

    #[error("Invalid operation: {0}")]
    InvalidOperation(String),

    #[error("Mount cycle detected: {chain:?}")]
    MountCycle { chain: Vec<(StoreId, NodeId)> },

    #[error("Mount depth exceeded: {depth} (max 16)")]
    MountDepthExceeded { depth: usize },

    #[error("Mount source store unavailable: {store_id}")]
    MountSourceUnavailable { store_id: StoreId },

    #[error("Node {node_id} is a mount point and has no children of its own; create under the mount's source instead")]
    MountHasNoChildren { node_id: NodeId },

    /// A vault blob (docs/CRYPTO_CONTRACT.md) exceeds the 4 MiB per-blob limit.
    #[error("Vault blob of {size} bytes exceeds the 4 MiB limit")]
    VaultBlobTooLarge { size: usize },

    /// A vault document's log would exceed 64 MiB; the caller must upload a
    /// snapshot (`vaultSnapshot`) before appending more.
    #[error("Vault document log is at its size limit; a snapshot is required before more updates can be appended")]
    VaultSnapshotRequired,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("CRDT error: {0}")]
    Crdt(#[from] pimble_crdt::CrdtError),

    #[error("Core error: {0}")]
    Core(#[from] pimble_core::CoreError),
}

pub type Result<T> = std::result::Result<T, StoreError>;
