//! Error types for pimble-search

use pimble_core::StoreId;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SearchError {
    /// No open index for this store (an ownership-layer error; `SearchIndex`
    /// itself never returns this, but a caller managing one index per store
    /// finds it a convenient shared variant).
    #[error("index not found for store: {0}")]
    IndexNotFound(StoreId),

    /// The composed schema text failed to parse.
    #[error("schema error: {0}")]
    Schema(String),

    /// Any other rhypedb engine error (storage, write conflict, not found, …).
    #[error("index engine error: {0}")]
    Engine(String),

    /// A `.matches`/full-text query was malformed (bad phrase syntax, no
    /// searchable terms, …).
    #[error("query error: {0}")]
    Query(String),

    /// Embedding/vector search failed (only reachable with the `semantic`
    /// feature and a live vectorizer).
    #[error("embedding error: {0}")]
    Embedding(String),

    /// A `.matches` field's full-text index is still backfilling. `done` of
    /// `total` objects have been indexed so far.
    #[error("full-text index is still building ({done} of {total} objects indexed)")]
    IndexBuilding { done: u64, total: u64 },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SearchError>;
