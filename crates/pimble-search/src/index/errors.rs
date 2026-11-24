//! Mapping rhypedb's engine errors onto [`SearchError`].

use rhypedb_engine::EngineError;

use crate::error::SearchError;

pub(super) fn map_fulltext_err(e: EngineError) -> SearchError {
    match e {
        EngineError::FulltextIndexBuilding { indexed, total, .. } => {
            SearchError::IndexBuilding { done: indexed, total }
        }
        EngineError::FulltextQuery(msg) => SearchError::Query(msg),
        other => map_engine_err(other),
    }
}

pub(crate) fn map_engine_err(e: EngineError) -> SearchError {
    match e {
        EngineError::FulltextIndexBuilding { indexed, total, .. } => {
            SearchError::IndexBuilding { done: indexed, total }
        }
        EngineError::FulltextQuery(msg) => SearchError::Query(msg),
        EngineError::ModelUnavailable(msg) => SearchError::ModelUnavailable(msg),
        // rhypedb-storage's `DataDirLocked`, which the engine wraps and does
        // not re-export: told apart by its message.
        other if other.to_string().contains("data directory is locked") => SearchError::Locked(other.to_string()),
        other => SearchError::Engine(other.to_string()),
    }
}
