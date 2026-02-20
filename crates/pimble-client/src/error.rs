//! Error types for pimble-client

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Connection error: {0}")]
    Connection(String),

    #[error("RPC error: {0}")]
    Rpc(String),

    #[error("Not connected")]
    NotConnected,

    #[error("Timeout")]
    Timeout,

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ClientError>;
