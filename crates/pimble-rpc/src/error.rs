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
        }
    }
}

pub type Result<T> = std::result::Result<T, RpcError>;
