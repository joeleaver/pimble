//! Error types for pimble-core

use thiserror::Error;

use crate::{NodeId, StoreId};

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("Node not found: {0}")]
    NodeNotFound(NodeId),

    #[error("Store not found: {0}")]
    StoreNotFound(StoreId),

    #[error("Invalid node type: {0}")]
    InvalidNodeType(String),

    #[error("Invalid link target: {0}")]
    InvalidLinkTarget(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Invalid UUID: {0}")]
    InvalidUuid(#[from] uuid::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;
