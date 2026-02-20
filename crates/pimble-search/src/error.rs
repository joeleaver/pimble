//! Error types for pimble-search

use pimble_core::StoreId;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SearchError {
    #[error("Index not found for store: {0}")]
    IndexNotFound(StoreId),

    #[error("Index error: {0}")]
    IndexError(String),

    #[error("Query error: {0}")]
    QueryError(String),

    #[error("Embedding error: {0}")]
    EmbeddingError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SearchError>;
