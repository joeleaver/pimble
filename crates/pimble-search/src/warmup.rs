//! Model cache location and warm-up for the semantic embedder.
//!
//! `SearchIndex::open(_, true)` starts a background worker that constructs
//! its own `FastEmbedder` lazily, the first time it has a job to run — one
//! per store, since each store owns its own `SearchIndex`/`Vectorizer`.
//! rhypedb #18 (`EmbedOptions`/`VectorizerConfig`) serializes model
//! construction within one process (`rhypedb_embed`'s `MODEL_LOAD` mutex),
//! so two stores' workers racing to load the same model no longer panics —
//! but on a cold cache the first store to open still pays the download, and
//! fastembed's own default cache directory is `./.fastembed_cache`, relative
//! to the process's current working directory: not a stable location for a
//! long-running server.
//!
//! A caller that owns process startup (pimble-server) should call
//! [`set_model_cache_dir`] once, to a stable directory, and then
//! [`warm_embedding_model`] once, before any [`crate::SearchIndex`] opens
//! with `semantic: true` — so the download happens up front, in a known
//! place, and every later `SearchIndex`'s lazy `FastEmbedder` construction
//! (via `Vectorizer::with_config`'s `VectorizerConfig::embed`, which
//! `SearchIndex::open` points at the same directory via
//! [`model_cache_dir`]) just loads it from disk.
//!
//! `warm_embedding_model` is a no-op without the `semantic` feature;
//! [`set_model_cache_dir`]/[`model_cache_dir`] work regardless (they only
//! move a path around, which every build can do so `SearchIndex::open`
//! doesn't need its own `#[cfg]` to call them).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use crate::error::Result;

/// The directory [`set_model_cache_dir`] most recently set — read by both
/// [`warm_embedding_model`] and `SearchIndex::open` (via [`model_cache_dir`]),
/// so every `FastEmbedder` this process ever constructs (this warm-up's own,
/// and every store's lazily-created one via `VectorizerConfig::embed`) agree
/// on where the model lives.
///
/// A `OnceLock` rather than a plain `Mutex<Option<PathBuf>>`: intended to be
/// set exactly once, early in `main`, before the first `SearchIndex::open` —
/// matching [`set_model_cache_dir`]'s own "call this once, early" contract.
static MODEL_CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Point every later [`crate::SearchIndex::open`] (and [`warm_embedding_model`])
/// at `dir` for model files, creating it if it doesn't already exist. Call
/// this before opening any `SearchIndex` with `semantic: true`, or every
/// store's `FastEmbedder` falls back to `rhypedb_embed::EmbedOptions`'s own
/// default (`./.fastembed_cache`, relative to the current working
/// directory).
///
/// A `OnceLock` under the hood: the first call's `dir` sticks for the rest of
/// the process. A later call with a *different* `dir` still creates that
/// directory (so it's never left half-set-up) but has no further effect on
/// where models are actually loaded from — this mirrors calling it more than
/// once being a sign something is already off (multiple independent
/// start-up paths), not a supported way to relocate a live cache.
pub fn set_model_cache_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let _ = MODEL_CACHE_DIR.set(dir.to_path_buf());
    Ok(())
}

/// The directory [`set_model_cache_dir`] most recently set, or `None` if it
/// was never called.
pub(crate) fn model_cache_dir() -> Option<PathBuf> {
    MODEL_CACHE_DIR.get().cloned()
}

/// Force `model` to load (downloading it on first use, from whatever
/// directory [`set_model_cache_dir`] pointed at, or fastembed's own default
/// if that was never called) and report how long that took.
///
/// Embeds a single short string purely to force the load; the embedding
/// itself is discarded. Deliberately not a real batch — this exists to pay
/// the one-time model-load cost up front, not to warm up an inference batch
/// size.
///
/// A no-op returning `Ok(Duration::ZERO)` without the `semantic` feature.
pub fn warm_embedding_model(model: &str) -> Result<Duration> {
    #[cfg(feature = "semantic")]
    {
        use rhypedb_embed::{EmbedOptions, Embedder, FastEmbedder};
        let options = EmbedOptions {
            cache_dir: model_cache_dir(),
            ..EmbedOptions::default()
        };
        let start = std::time::Instant::now();
        let mut embedder = FastEmbedder::with_options(model, &options)
            .map_err(|e| crate::error::SearchError::Embedding(e.to_string()))?;
        embedder
            .embed(&["warm up"])
            .map_err(|e| crate::error::SearchError::Embedding(e.to_string()))?;
        Ok(start.elapsed())
    }
    #[cfg(not(feature = "semantic"))]
    {
        let _ = model;
        Ok(Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_model_cache_dir_creates_the_directory() {
        // `create_dir_all` runs unconditionally on the *passed* path, every
        // call, regardless of which call "won" the global `OnceLock` below —
        // so this holds no matter what other tests in this binary do.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("models");
        assert!(!target.exists());
        set_model_cache_dir(&target).unwrap();
        assert!(target.is_dir());
    }

    /// `MODEL_CACHE_DIR` is a process-global `OnceLock` (deliberately: every
    /// `FastEmbedder` in this process, warm-up's own and every
    /// `SearchIndex`'s, must agree on one directory), and Rust's test
    /// harness runs this binary's tests in parallel threads within one
    /// process — so this can only assert the invariant that holds no matter
    /// which test's call actually set it: once any call has run, some
    /// already-created directory is on record.
    #[test]
    fn model_cache_dir_is_on_record_after_any_call() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("models");
        set_model_cache_dir(&target).unwrap();
        // Our own target was created regardless of which call won the lock.
        assert!(target.is_dir());
        // The recorded directory may belong to a sibling test whose tempdir
        // is already gone, so only its presence can be asserted, never that
        // it still exists on disk.
        assert!(model_cache_dir().is_some(), "some call in this binary must have set it by now");
    }

    #[test]
    #[cfg(not(feature = "semantic"))]
    fn warm_embedding_model_is_a_no_op_without_semantic() {
        assert_eq!(warm_embedding_model("all-MiniLM-L6-v2").unwrap(), Duration::ZERO);
    }
}
