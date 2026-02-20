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
            addr: "127.0.0.1:9876".parse().unwrap(),
        }
    }
}

/// The Pimble server
pub struct PimbleServer {
    config: ServerConfig,
    store_manager: Arc<RwLock<StoreManager>>,
    handle: Option<ServerHandle>,
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
        }
    }

    /// Get a reference to the store manager
    pub fn store_manager(&self) -> Arc<RwLock<StoreManager>> {
        Arc::clone(&self.store_manager)
    }

    /// Start the server
    pub async fn start(&mut self) -> Result<()> {
        let server = Server::builder()
            .build(&self.config.addr)
            .await
            .map_err(|e| crate::ServerError::Server(e.to_string()))?;

        let handler = RpcHandler::new(Arc::clone(&self.store_manager));
        let methods = handler.into_rpc();

        info!("Starting Pimble server on {}", self.config.addr);
        let handle = server.start(methods);
        self.handle = Some(handle);

        Ok(())
    }

    /// Stop the server
    pub async fn stop(&mut self) -> Result<()> {
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

    /// Get the server address
    pub fn addr(&self) -> SocketAddr {
        self.config.addr
    }
}

impl Default for PimbleServer {
    fn default() -> Self {
        Self::new()
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
