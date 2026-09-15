//! Server startup and management

use std::net::SocketAddr;
use std::path::PathBuf;
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
    /// Required credential for every JSON-RPC request (docs/
    /// HARDENING_CONTRACT.md decisions 1-2): `Authorization: Bearer
    /// <token>` or `X-Api-Key: <token>`, checked at the HTTP edge before any
    /// request reaches the store. `None` means no token is required, which
    /// `start()` only allows on a loopback address — the app's embedded
    /// server runs this way.
    pub auth_token: Option<String>,
    /// Where saved per-remote credentials live (decision 4). `None` uses
    /// [`crate::credentials::default_credentials_path`]; tests set this to a
    /// temp path so they never touch the real config directory.
    pub credentials_path: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:7462".parse().unwrap(),
            auth_token: None,
            credentials_path: None,
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
    /// Refuses to bind a non-loopback address without `config.auth_token`
    /// (docs/history/HARDENING_CONTRACT.md decision 2): an open port reachable from
    /// outside this machine with no credential check is never allowed, even
    /// by a caller who forgot to configure one.
    ///
    /// Before accepting any RPC (in particular, before any `openStore` can
    /// reach [`RpcHandler`]), warms up the semantic search embedding model
    /// once — see [`warm_up_embedding_model`]. This is what turns "two
    /// stores opening at once each lazily construct their own embedder and
    /// race to download the same model" into "one download, serially, before
    /// any store exists to race over."
    pub async fn start(&mut self) -> Result<()> {
        // An empty or whitespace-only token is never valid: treating it as
        // "no token configured" would silently disable the non-loopback
        // refusal below, and treating it literally would make
        // `AuthMiddleware` admit `Authorization: Bearer ` (nothing after
        // it) and an empty `X-Api-Key`.
        if let Some(token) = &self.config.auth_token {
            if token.trim().is_empty() {
                return Err(crate::ServerError::Server(
                    "refusing to start with an empty auth_token; pass None for no token, or a real one".to_string(),
                ));
            }
        }

        if !self.config.addr.ip().is_loopback() && self.config.auth_token.is_none() {
            return Err(crate::ServerError::Server(format!(
                "refusing to bind {} (not a loopback address) without an auth token; \
                 pass ServerConfig::auth_token or start with a token file (pimble-cli server --token-file)",
                self.config.addr
            )));
        }

        // The HTTP-edge auth layer (decisions 1-2), run on every request
        // before it reaches the JSON-RPC dispatch — including the WebSocket
        // upgrade handshake.
        let http_middleware = tower::ServiceBuilder::new().layer(crate::auth::AuthLayer::new(self.config.auth_token.clone()));

        let server = Server::builder()
            .set_http_middleware(http_middleware)
            .build(&self.config.addr)
            .await
            .map_err(|e| crate::ServerError::Server(e.to_string()))?;

        // `local_addr()` must be read before `start()`, which consumes the
        // `Server` by value; this is what makes port 0 (bind to any free
        // port, used by tests) resolve to something `addr()` can report.
        let local_addr = server.local_addr().map_err(|e| crate::ServerError::Server(e.to_string()))?;
        self.local_addr = Some(local_addr);

        let semantic_available = warm_up_embedding_model().await;

        let credentials_path = self.config.credentials_path.clone().unwrap_or_else(crate::credentials::default_credentials_path);
        let handler = RpcHandler::with_credentials_path(Arc::clone(&self.store_manager), semantic_available, credentials_path);
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

/// Resolve on the first `SIGINT` (Ctrl+C) or, on Unix, `SIGTERM` —
/// whichever arrives first (docs/history/HARDENING_CONTRACT.md decision 13). A
/// container or `systemd stop` sends `SIGTERM`, not `SIGINT`; without this a
/// headless server killed that way skips its flush instead of stopping
/// cleanly.
pub async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(sigterm) => sigterm,
            Err(e) => {
                tracing::warn!("Could not install a SIGTERM handler ({}); only SIGINT will stop this server", e);
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Start a server and run it until shutdown
pub async fn run_server(config: ServerConfig) -> Result<()> {
    let mut server = PimbleServer::with_config(config);
    server.start().await?;

    wait_for_shutdown_signal().await;

    info!("Shutting down...");
    server.stop().await?;

    // Flush all stores
    let manager = server.store_manager();
    let mut manager = manager.write().await;
    manager.flush_all().await.map_err(crate::ServerError::Store)?;

    Ok(())
}
