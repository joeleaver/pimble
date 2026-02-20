//! Error types for pimble-server

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ServerError {
    #[error("Server error: {0}")]
    Server(String),

    #[error("Store error: {0}")]
    Store(#[from] pimble_store::StoreError),

    #[error("RPC error: {0}")]
    Rpc(#[from] pimble_rpc::RpcError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ServerError>;
