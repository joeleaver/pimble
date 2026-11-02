//! Error types for pimble-client

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Connection error: {0}")]
    Connection(String),

    /// A call the server answered with an error: its JSON-RPC code
    /// (`pimble_rpc::RpcError::code`, e.g. `-32004` for a refusal) and its
    /// message as is, which is what a person reads.
    #[error("{message}")]
    Server { code: i32, message: String },

    /// A transport failure during a call, or an answer this client could
    /// not read.
    #[error("{0}")]
    Rpc(String),

    #[error("Not connected")]
    NotConnected,

    #[error("Timeout")]
    Timeout,

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl ClientError {
    /// The server's JSON-RPC code, for an error the server returned.
    pub fn code(&self) -> Option<i32> {
        match self {
            ClientError::Server { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Whether the server refused the call (`-32004`): no grant, read-only,
    /// comment-only, or a Service-only operation.
    pub fn is_forbidden(&self) -> bool {
        self.code() == Some(-32004)
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;
