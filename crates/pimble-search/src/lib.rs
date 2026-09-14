//! Pimble Search — the search and graph index, on rhypedb.
//!
//! One [`SearchIndex`] per open store, backed by a rhypedb database at
//! `store.pimble/index/rhypedb/`. It is derived and disposable: the yrs
//! documents (`pimble-crdt`) remain the source of truth, and deleting the
//! index directory and re-running every node through [`SearchIndex::upsert`]
//! rebuilds it exactly.
//!
//! See `docs/STEP5_CONTRACT.md` for the design this crate implements.

pub mod chunk;
pub mod error;
pub mod index;
pub mod query;
pub mod schema;
pub mod warmup;

pub use chunk::*;
pub use error::*;
pub use index::*;
pub use query::*;
pub use schema::{compose_schema, schema_hash, SchemaFragment, SCHEMA_HASH_FILE};
pub use warmup::{set_model_cache_dir, warm_embedding_model};
