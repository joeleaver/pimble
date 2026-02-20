//! Search query types and execution

use pimble_core::{NodeId, StoreId};
use serde::{Deserialize, Serialize};

/// Search query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// The search query string
    pub query: String,

    /// Which stores to search (empty = all)
    pub stores: Vec<StoreId>,

    /// Whether to use semantic (vector) search
    pub semantic: bool,

    /// Filters to apply
    pub filters: SearchFilters,

    /// Maximum number of results
    pub limit: usize,
}

impl SearchQuery {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            stores: Vec::new(),
            semantic: true,
            filters: SearchFilters::default(),
            limit: 20,
        }
    }

    pub fn with_stores(mut self, stores: Vec<StoreId>) -> Self {
        self.stores = stores;
        self
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

/// Search filters
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilters {
    /// Filter by node types
    pub node_types: Vec<String>,

    /// Filter by tags (any match)
    pub tags: Vec<String>,

    /// Filter by date range
    pub created_after: Option<chrono::DateTime<chrono::Utc>>,
    pub created_before: Option<chrono::DateTime<chrono::Utc>>,
}

/// A single search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// The node ID
    pub node_id: NodeId,

    /// The store containing the node
    pub store_id: StoreId,

    /// Relevance score (0-1)
    pub score: f32,

    /// Node title
    pub title: String,

    /// Snippet of matching content
    pub snippet: String,

    /// Deep link anchor within the node (if applicable)
    pub deep_link: Option<String>,
}

/// Collection of search results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    /// The query that was executed
    pub query: String,

    /// Total number of matches (may be more than returned)
    pub total_matches: usize,

    /// The result items
    pub results: Vec<SearchResult>,
}

impl SearchResults {
    pub fn empty(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            total_matches: 0,
            results: Vec::new(),
        }
    }
}
