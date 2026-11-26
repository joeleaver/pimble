//! Search through [`Pimble`]: the index's hits as the backend answers them,
//! and the same less the hits of internal kinds a caller does not show.

use pimble_core::internal::{hidden, InternalKind};
use pimble_core::StoreId;
use pimble_rpc::SearchResultItem;

use crate::{ApiError, Pimble};

impl Pimble {
    pub async fn search(&self, query: &str, stores: Vec<StoreId>, semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError> {
        self.backend.search(query, stores, semantic, limit).await
    }

    /// [`Pimble::search`] less the hits of internal kinds not in `shown`
    /// (docs/THEMES_CONTRACT.md section 3.1a), still up to `limit` of them:
    /// left out before the limit, never after it, so a query that matches
    /// internal nodes still answers as many hits as there are. The index
    /// is asked again with twice the room until enough hits are kept or it
    /// has no more.
    pub async fn search_shown(
        &self,
        query: &str,
        stores: Vec<StoreId>,
        semantic: bool,
        limit: usize,
        shown: &[InternalKind],
    ) -> Result<Vec<SearchResultItem>, ApiError> {
        let mut asked = limit;
        loop {
            let found = self.backend.search(query, stores.clone(), semantic, asked).await?;
            let exhausted = found.len() < asked;
            let mut kept: Vec<SearchResultItem> = found.into_iter().filter(|hit| !hidden(&hit.node_type, shown)).collect();
            if kept.len() >= limit || exhausted {
                kept.truncate(limit);
                return Ok(kept);
            }
            asked = asked.saturating_mul(2);
        }
    }
}
