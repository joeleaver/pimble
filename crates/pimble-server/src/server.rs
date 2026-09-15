//! Server startup and management

use std::net::SocketAddr;
use std::sync::Arc;

use jsonrpsee::server::{Server, ServerHandle};
use pimble_rpc::PimbleApiServer;
use pimble_store::StoreManager;
use tokio::sync::RwLock;
use tracing::info;

use crate::handler::RpcHandler;
use crate::Result;

/// Configuration for the Pimble server
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind to
    pub addr: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:7462".parse().unwrap(),
        }
    }
}

/// The Pimble server
pub struct PimbleServer {
    config: ServerConfig,
    store_manager: Arc<RwLock<StoreManager>>,
    handle: Option<ServerHandle>,
    /// The socket the server actually bound to, resolved once at `start()`
    /// (`Server::local_addr()`), so a config with port 0 (as used in tests)
    /// still has a meaningful `addr()`.
    local_addr: Option<SocketAddr>,
}

impl PimbleServer {
    /// Create a new server with default configuration
    pub fn new() -> Self {
        Self::with_config(ServerConfig::default())
    }

    /// Create a new server with custom configuration
    pub fn with_config(config: ServerConfig) -> Self {
        Self {
            config,
            store_manager: Arc::new(RwLock::new(StoreManager::new())),
            handle: None,
            local_addr: None,
        }
    }

    /// Get a reference to the store manager
    pub fn store_manager(&self) -> Arc<RwLock<StoreManager>> {
        Arc::clone(&self.store_manager)
    }

    /// Start the server.
    ///
    /// Before accepting any RPC (in particular, before any `openStore` can
    /// reach [`RpcHandler`]), warms up the semantic search embedding model
    /// once — see [`warm_up_embedding_model`]. This is what turns "two
    /// stores opening at once each lazily construct their own embedder and
    /// race to download the same model" into "one download, serially, before
    /// any store exists to race over."
    pub async fn start(&mut self) -> Result<()> {
        let server = Server::builder()
            .build(&self.config.addr)
            .await
            .map_err(|e| crate::ServerError::Server(e.to_string()))?;

        // `local_addr()` must be read before `start()`, which consumes the
        // `Server` by value; this is what makes port 0 (bind to any free
        // port, used by tests) resolve to something `addr()` can report.
        let local_addr = server.local_addr().map_err(|e| crate::ServerError::Server(e.to_string()))?;
        self.local_addr = Some(local_addr);

        let semantic_available = warm_up_embedding_model().await;

        let handler = RpcHandler::with_semantic_available(Arc::clone(&self.store_manager), semantic_available);
        let methods = handler.into_rpc();

        info!("Starting Pimble server on {}", local_addr);
        let handle = server.start(methods);
        self.handle = Some(handle);

        Ok(())
    }

    /// Stop the server
    pub async fn stop(&mut self) -> Result<()> {
        // Flush any pending tree/content changes before the handle stops, so
        // an app shutdown never depends on the caller remembering to flush
        // (debounced content flushes in particular may still be pending).
        {
            let mut manager = self.store_manager.write().await;
            manager.flush_all().await?;
        }

        if let Some(handle) = self.handle.take() {
            handle.stop().map_err(|e| crate::ServerError::Server(e.to_string()))?;
            info!("Pimble server stopped");
        }
        Ok(())
    }

    /// Wait for the server to finish
    pub async fn wait(&self) {
        if let Some(ref handle) = self.handle {
            handle.clone().stopped().await;
        }
    }

    /// The address the server is bound to: the actual bound socket
    /// (`Server::local_addr()`) once `start()` has run, so a `ServerConfig`
    /// with port 0 resolves to the OS-assigned port; falls back to the
    /// configured address before `start()`.
    pub fn addr(&self) -> SocketAddr {
        self.local_addr.unwrap_or(self.config.addr)
    }
}

impl Default for PimbleServer {
    fn default() -> Self {
        Self::new()
    }
}

/// The model semantic search embeds chunks with — kept in one place so
/// [`warm_up_embedding_model`] and every `SearchIndex`'s own lazy
/// `Vectorizer` (via the schema's `@vectorize(model: "...")` directive)
/// agree on it.
const EMBEDDING_MODEL: &str = "all-MiniLM-L6-v2";

/// Point the embedding model cache at a stable, per-user directory and force
/// the model to load once, before any store's `SearchIndex` can start its
/// own background worker and lazily (and, with more than one store opening
/// at once, racily) load it instead. Returns whether semantic search is
/// available for this run: `true` on success, `false` on any failure (no
/// network on first run, disk full, model registry mismatch, ...) — a
/// missing model is not fatal to the server, it just means every store opens
/// keyword-only (see [`RpcHandler::with_semantic_available`]) until a build
/// with the model cached, or with network access, runs again.
///
/// A no-op (always `true`) when pimble-search's `semantic` feature isn't
/// compiled in — both `pimble_search` calls below already are.
///
/// Runs `warm_embedding_model` (a blocking, synchronous ONNX Runtime call)
/// on a blocking thread so it can't stall the async runtime other RPCs will
/// shortly run on; `start` still awaits it, so no store can open before it
/// resolves.
async fn warm_up_embedding_model() -> bool {
    let cache_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("pimble")
        .join("models");

    if let Err(e) = pimble_search::set_model_cache_dir(&cache_dir) {
        tracing::warn!(
            "Could not create embedding model cache dir {:?} ({}); running search keyword-only",
            cache_dir, e
        );
        return false;
    }

    match tokio::task::spawn_blocking(|| pimble_search::warm_embedding_model(EMBEDDING_MODEL)).await {
        Ok(Ok(elapsed)) => {
            info!("Embedding model '{}' ready in {:.1}s", EMBEDDING_MODEL, elapsed.as_secs_f64());
            true
        }
        Ok(Err(e)) => {
            tracing::warn!(
                "Embedding model '{}' warm-up failed ({}); running search keyword-only",
                EMBEDDING_MODEL, e
            );
            false
        }
        Err(join_err) => {
            tracing::warn!(
                "Embedding model warm-up task panicked ({}); running search keyword-only",
                join_err
            );
            false
        }
    }
}

/// Start a server and run it until shutdown
pub async fn run_server(config: ServerConfig) -> Result<()> {
    let mut server = PimbleServer::with_config(config);
    server.start().await?;

    // Wait for Ctrl+C
    tokio::signal::ctrl_c()
        .await
        .map_err(|e| crate::ServerError::Io(e))?;

    info!("Shutting down...");
    server.stop().await?;

    // Flush all stores
    let manager = server.store_manager();
    let mut manager = manager.write().await;
    manager.flush_all().await.map_err(crate::ServerError::Store)?;

    Ok(())
}
