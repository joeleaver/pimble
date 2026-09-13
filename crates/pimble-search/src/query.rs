//! Search query and result types.

use pimble_core::NodeId;
use serde::{Deserialize, Serialize};

/// A search request against a [`crate::SearchIndex`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    /// `true` asks for a hybrid (keyword + semantic) search; ignored (treated
    /// as keyword-only) when the index has no vectorizer — see
    /// [`crate::SearchIndex::semantic_enabled`].
    pub semantic: bool,
    pub limit: usize,
}

impl SearchQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            semantic: false,
            limit: 20,
        }
    }

    pub fn with_semantic(mut self, semantic: bool) -> Self {
        self.semantic = semantic;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub node_id: NodeId,
    /// Higher is better, for both keyword (BM25) and semantic (distance,
    /// converted) hits.
    pub score: f32,
    pub title: String,
    /// A window of text around the match (keyword) or the best-matching
    /// chunk's text (semantic).
    pub snippet: String,
    /// What was matched: `"node"` for a whole-node keyword match (title/text
    /// fields), or the matched chunk's kind — one of `"prose"`, `"heading"`,
    /// `"code"`, `"table"`, `"field"`, `"other"` — for a semantic hit.
    pub kind: String,
    /// The matched chunk's locator (see `IndexUnit::path`), when the hit
    /// resolved to a specific chunk. `None` for a whole-node keyword match.
    pub path: Option<String>,
}
