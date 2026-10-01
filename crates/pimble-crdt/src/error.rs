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

    /// An edit refused before anything was written, as a sentence for whoever asked
    /// (a person, or an LLM through `pimble-mcp`): what is wrong and what to do.
    #[error("{0}")]
    Refused(String),
}

pub type Result<T> = std::result::Result<T, CrdtError>;
