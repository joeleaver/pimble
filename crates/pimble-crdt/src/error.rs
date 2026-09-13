//! Error types for pimble-crdt

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CrdtError {
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("yrs error: {0}")]
    Yrs(String),

    #[error("collab error: {0}")]
    Collab(String),
}

pub type Result<T> = std::result::Result<T, CrdtError>;
