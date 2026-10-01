//! `pimble-mcp`: an MCP server over stdio through which an LLM reads, searches
//! and writes the stores open on this computer (docs/MCP_CONTRACT.md).
//!
//! stdout carries the protocol and nothing else: every log line goes to stderr.

// A GUI client that launches this on Windows shows no console window; the
// pipes it hands over still carry the protocol.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod connection;
mod nodes;
mod tools;

use std::sync::Arc;

use rmcp::ServiceExt;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let conn = Arc::new(connection::Connection::new());
    // Connect (or start the server) at once, so the first tool call does not wait
    // for the search model to warm up.
    if let Err(e) = conn.client().await {
        tracing::warn!("{e}");
    }

    let served = tools::Pimble::new(conn.clone()).serve(rmcp::transport::stdio()).await;
    match served {
        Ok(service) => {
            if let Err(e) = service.waiting().await {
                tracing::error!("the MCP session ended with an error: {e}");
            }
        }
        Err(e) => tracing::error!("the MCP session could not start: {e}"),
    }
    conn.shut_down().await;
}
