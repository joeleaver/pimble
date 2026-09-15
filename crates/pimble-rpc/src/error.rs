//! Error types for pimble-rpc

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RpcError {
    #[error("Method not found: {0}")]
    MethodNotFound(String),

    #[error("Invalid params: {0}")]
    InvalidParams(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Store error: {0}")]
    Store(String),

    #[error("Node error: {0}")]
    Node(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// The store's search index is still building (e.g. a backfilling
    /// fulltext or vector index); `done`/`total` report progress so the UI
    /// can say so instead of treating this as a generic failure.
    #[error("Search index is still building ({done}/{total})")]
    IndexBuilding { done: usize, total: usize },

    /// The connection's principal is authenticated but not authorized for
    /// the store (or the server-only operation) the request named
    /// (docs/CLOUD_CONTRACT.md "B: pimble-server" item 5).
    #[error("Forbidden: {0}")]
    Forbidden(String),

    /// The store is a vault (docs/CRYPTO_CONTRACT.md): only the vault RPCs apply.
    #[error("Encrypted store: {0}")]
    EncryptedStore(String),

    /// A vault document's log is at its limit; a snapshot must be uploaded
    /// before more updates are appended.
    #[error("Snapshot required: {0}")]
    SnapshotRequired(String),
}

impl RpcError {
    pub fn code(&self) -> i32 {
        match self {
            RpcError::MethodNotFound(_) => -32601,
            RpcError::InvalidParams(_) => -32602,
            RpcError::Internal(_) => -32603,
            RpcError::Store(_) => -32001,
            RpcError::Node(_) => -32002,
            RpcError::Serialization(_) => -32700,
            RpcError::IndexBuilding { .. } => -32010,
            RpcError::Forbidden(_) => -32004,
            RpcError::EncryptedStore(_) => -32005,
            RpcError::SnapshotRequired(_) => -32006,
        }
    }
}

pub type Result<T> = std::result::Result<T, RpcError>;
