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

    /// Whether the call failed because a message was larger than one
    /// message may be (`pimble_rpc::MAX_MESSAGE_BYTES`): jsonrpsee's
    /// "Request is too big" (`-32007`) or "Response is too big"
    /// (`-32008`) from the server, or the transport refusing a message.
    pub fn is_too_big(&self) -> bool {
        match self {
            ClientError::Server { code, .. } => *code == TOO_BIG_REQUEST || *code == TOO_BIG_RESPONSE,
            ClientError::Rpc(text) => {
                let text = text.to_ascii_lowercase();
                text.contains("too big") || text.contains("too large")
            }
            _ => false,
        }
    }
}

/// jsonrpsee's code for a request over the server's limit.
pub const TOO_BIG_REQUEST: i32 = -32007;
/// jsonrpsee's code for an answer over the server's limit.
pub const TOO_BIG_RESPONSE: i32 = -32008;

/// What a person or an agent reads when one document is larger than one
/// message may be (docs/DRAWING_CONTRACT.md "Large canvases"): which
/// document, and what to do about it.
pub fn too_big_sentence(store: pimble_core::StoreId, node: pimble_core::NodeId) -> String {
    format!(
        "The document of node pimble:{store}/{node} is larger than Pimble sends in one message ({} MB), so it cannot be loaded or changed from here. \
         If it is a canvas, flatten its paint layers (Flatten in the layers panel, or flatten_layer) or split it into several canvases.",
        pimble_rpc::MAX_MESSAGE_BYTES / (1024 * 1024)
    )
}

pub type Result<T> = std::result::Result<T, ClientError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// jsonrpsee's "too big" answers and the transport's refusal are told
    /// apart from every other error, and the sentence names the document
    /// and what to do (docs/DRAWING_CONTRACT.md "Large canvases").
    #[test]
    fn too_big_is_recognized_and_said() {
        assert!(ClientError::Server { code: TOO_BIG_RESPONSE, message: "Response is too big".into() }.is_too_big());
        assert!(ClientError::Server { code: TOO_BIG_REQUEST, message: "Request is too big".into() }.is_too_big());
        assert!(ClientError::Rpc("The message was too large".into()).is_too_big());
        assert!(!ClientError::Server { code: -32004, message: "no grant".into() }.is_too_big());
        assert!(!ClientError::Rpc("connection closed".into()).is_too_big());
        let (store, node) = (pimble_core::StoreId::new(), pimble_core::NodeId::new());
        let sentence = too_big_sentence(store, node);
        assert!(sentence.contains(&format!("pimble:{store}/{node}")), "{sentence}");
        assert!(sentence.contains("flatten") && sentence.contains("256 MB"), "{sentence}");
    }
}
