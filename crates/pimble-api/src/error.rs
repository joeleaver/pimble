//! What went wrong, as data with the sentence a person reads
//! (docs/API_CONTRACT.md decision 9). Every error a caller of the API sees is
//! one of these: the server's refusals arrive by their JSON-RPC code, the
//! libraries' by their own types, and nobody matches text to tell them apart.

use pimble_board::BoardError;
use pimble_client::ClientError;
use pimble_core::StoreAccess;
use pimble_crdt::CrdtError;

use crate::Addr;

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// A write where this device may only read.
    ReadOnly,
    /// A write where this device may only comment (a discussion is a
    /// commenter's to write; anything else is not).
    CommentOnly,
    /// The server refused the call for any other reason it gives (no grant
    /// for the store or document, a Service-only operation).
    NoGrant { sentence: String },
    /// Nothing answers to that name: no such node, store or link.
    NotFound { sentence: String },
    /// A name that answers to several nodes; never a guess.
    Ambiguous { sentence: String, candidates: Vec<Addr> },
    /// The node is a tombstone; `undeleteNode` puts it back.
    Deleted { addr: Addr },
    /// A canvas lock refused the edit (`pimble_board`'s judgement).
    Locked { sentence: String },
    /// A library refused what was asked of a document: Markdown it cannot
    /// hold, a quote that names no block, a sheet value that does not parse.
    Refused { sentence: String },
    /// The store is encrypted and this way of reaching it cannot read it.
    EncryptedStore { sentence: String },
    /// The thing exists but cannot be had now: a store still connecting, an
    /// index still building.
    NotAvailable { sentence: String },
    /// No connection to the server, or it went away during the call.
    Offline { sentence: String },
    /// Any other error the server answered with, by its code.
    Server { code: i32, message: String },
    /// Something that should not happen did (a document that does not load).
    Failed { sentence: String },
}

impl ApiError {
    /// What a person reads.
    pub fn sentence(&self) -> String {
        match self {
            ApiError::ReadOnly => StoreAccess::READ_ONLY_REFUSAL.to_string(),
            ApiError::CommentOnly => StoreAccess::COMMENT_ONLY_REFUSAL.to_string(),
            ApiError::Deleted { addr } => format!("{} was deleted; it can be put back from Recently Deleted.", addr.link()),
            ApiError::NoGrant { sentence }
            | ApiError::NotFound { sentence }
            | ApiError::Ambiguous { sentence, .. }
            | ApiError::Locked { sentence }
            | ApiError::Refused { sentence }
            | ApiError::EncryptedStore { sentence }
            | ApiError::NotAvailable { sentence }
            | ApiError::Offline { sentence }
            | ApiError::Failed { sentence } => sentence.clone(),
            ApiError::Server { message, .. } => message.clone(),
        }
    }

    /// The refusal a write under `access` meets, if it is refused.
    pub fn of_access(access: StoreAccess) -> Option<ApiError> {
        match access {
            StoreAccess::Full => None,
            StoreAccess::Comment => Some(ApiError::CommentOnly),
            StoreAccess::Read => Some(ApiError::ReadOnly),
        }
    }

    /// Whether this is the server (or the access pre-check) saying no, as
    /// opposed to something missing or failing.
    pub fn is_refusal(&self) -> bool {
        matches!(self, ApiError::ReadOnly | ApiError::CommentOnly | ApiError::NoGrant { .. })
    }

    pub fn refused(sentence: impl Into<String>) -> Self {
        ApiError::Refused { sentence: sentence.into() }
    }

    pub fn not_found(sentence: impl Into<String>) -> Self {
        ApiError::NotFound { sentence: sentence.into() }
    }

    pub(crate) fn failed(sentence: impl Into<String>) -> Self {
        ApiError::Failed { sentence: sentence.into() }
    }

    /// A document that could not be read.
    pub(crate) fn unreadable(e: CrdtError) -> Self {
        ApiError::failed(format!("The node's document could not be read: {e}"))
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.sentence())
    }
}

impl std::error::Error for ApiError {}

/// What a tool or a status line shows: the sentence.
impl From<ApiError> for String {
    fn from(e: ApiError) -> String {
        e.sentence()
    }
}

/// A caller's own refusal, written as a sentence (a closure given to
/// `Pimble::edit` that judged the document and said no).
impl From<String> for ApiError {
    fn from(sentence: String) -> Self {
        ApiError::Refused { sentence }
    }
}

impl From<&str> for ApiError {
    fn from(sentence: &str) -> Self {
        ApiError::Refused { sentence: sentence.to_string() }
    }
}

/// The server's answer by its code (`pimble_rpc::RpcError::code`). A
/// `-32004` names which refusal it is with `StoreAccess`'s sentences, which
/// are the server's whole message for those two; that is reading the
/// constant the server wrote, not guessing from text.
impl From<ClientError> for ApiError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Server { code: -32004, message } => match StoreAccess::refusal_in(&message) {
                Some(s) if s == StoreAccess::READ_ONLY_REFUSAL => ApiError::ReadOnly,
                Some(s) if s == StoreAccess::COMMENT_ONLY_REFUSAL => ApiError::CommentOnly,
                _ => ApiError::NoGrant { sentence: message },
            },
            ClientError::Server { code: -32005, message } => ApiError::EncryptedStore { sentence: message },
            ClientError::Server { code: -32010, message } => ApiError::NotAvailable { sentence: message },
            ClientError::Server { code, message } => ApiError::Server { code, message },
            ClientError::Connection(m) => ApiError::Offline { sentence: format!("Pimble's server could not be reached: {m}") },
            ClientError::NotConnected | ClientError::Timeout => {
                ApiError::Offline { sentence: "Pimble's server is not connected; try again.".into() }
            }
            ClientError::Rpc(m) => ApiError::Failed { sentence: m },
            ClientError::Serialization(e) => ApiError::Failed { sentence: e.to_string() },
        }
    }
}

/// A document library's answer: a refusal is its sentence, anything else
/// is an edit that could not be made.
impl From<CrdtError> for ApiError {
    fn from(e: CrdtError) -> Self {
        match e {
            CrdtError::Refused(sentence) => ApiError::Refused { sentence },
            other => ApiError::Failed { sentence: format!("The edit could not be made: {other}") },
        }
    }
}

/// A canvas's answer: what a lock guards is `Locked`, anything else a refusal.
impl From<BoardError> for ApiError {
    fn from(e: BoardError) -> Self {
        match e {
            BoardError::ElementLocked { .. }
            | BoardError::LayerLocked(_)
            | BoardError::LayerLockedAgainstRemoval(_)
            | BoardError::EveryLayerLocked => ApiError::Locked { sentence: e.to_string() },
            other => ApiError::Refused { sentence: other.to_string() },
        }
    }
}

impl From<pimble_sheet::doc::SheetError> for ApiError {
    fn from(e: pimble_sheet::doc::SheetError) -> Self {
        ApiError::Refused { sentence: e.to_string() }
    }
}

impl From<pimble_discussion::DiscussionError> for ApiError {
    fn from(e: pimble_discussion::DiscussionError) -> Self {
        ApiError::Refused { sentence: e.to_string() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_are_told_apart_by_code() {
        let read = ClientError::Server { code: -32004, message: StoreAccess::READ_ONLY_REFUSAL.into() };
        assert_eq!(ApiError::from(read), ApiError::ReadOnly);
        let comment = ClientError::Server { code: -32004, message: format!("Forbidden: {}", StoreAccess::COMMENT_ONLY_REFUSAL) };
        assert_eq!(ApiError::from(comment), ApiError::CommentOnly);
        let grant = ClientError::Server { code: -32004, message: "Forbidden: no grant for this document".into() };
        assert!(matches!(ApiError::from(grant), ApiError::NoGrant { .. }));
        // Any other code carries the server's words as they are.
        let other = ApiError::from(ClientError::Server { code: -32000, message: "Node not found".into() });
        assert_eq!(other.sentence(), "Node not found");
        assert!(matches!(ApiError::from(ClientError::NotConnected), ApiError::Offline { .. }));
    }

    #[test]
    fn library_refusals_keep_their_sentence() {
        let e = ApiError::from(CrdtError::Refused("Line 3 is a task list.".into()));
        assert_eq!(e, ApiError::refused("Line 3 is a task list."));
        assert!(matches!(ApiError::from(BoardError::EveryLayerLocked), ApiError::Locked { .. }));
    }
}
