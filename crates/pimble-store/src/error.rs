//! Error types for pimble-store

use pimble_core::{NodeId, StoreId};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Store not found: {0}")]
    StoreNotFound(StoreId),

    #[error("Node not found: {0}")]
    NodeNotFound(NodeId),

    #[error("Store already exists at path: {0}")]
    StoreExists(String),

    #[error("Invalid store path: {0}")]
    InvalidPath(String),

    #[error("Store not open: {0}")]
    NotOpen(StoreId),

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
