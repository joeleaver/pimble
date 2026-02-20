//! Error types for pimble-crdt

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CrdtError {
    #[error("Automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),

    #[error("Document not initialized")]
    NotInitialized,

    #[error("Invalid document format")]
    InvalidFormat,

    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Type mismatch: expected {expected}, got {actual}")]
    TypeMismatch { expected: String, actual: String },

    #[error("Serialization error: {0}")]
    Serialization(String),
}

pub type Result<T> = std::result::Result<T, CrdtError>;
