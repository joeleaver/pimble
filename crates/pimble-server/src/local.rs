//! The Pimble server on this computer, as every local client finds it
//! (docs/MCP_CONTRACT.md "Which server it talks to"): the desktop app and
//! `pimble-mcp` share this code, so they agree on where the server is, join
//! one that is running, start one when none is, and hand it back and forth as
//! either of them comes and goes.
//!
//! Also the one list of stores open on this computer, `open_stores` in
//! `<config dir>/pimble/state.json`: the app writes it as stores open and
//! close, and whoever starts a server opens what it names.

use std::path::PathBuf;
use std::time::Duration;

use pimble_client::PimbleClient;
use rand::Rng;

use crate::server::{PimbleServer, ServerConfig};

// 7462 spells PIMB on a phone keypad. (The previous 9876 collided with the
// Blender MCP add-on's default port: its raw TCP socket accepted our WebSocket
// handshake and never answered, so the app sat at "Connecting..." forever.)
//
// `PIMBLE_APP_ADDR` (a `host:port`) moves both the address the embedded server
// binds and the URL this client connects to, so a second instance — a test one
// beside the app someone is actually using — runs on its own port and never
// borrows the other's server.
pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:7462";

/// The address the local server binds and its clients connect to.
pub fn server_addr() -> String {
    let Ok(addr) = std::env::var("PIMBLE_APP_ADDR") else {
        return DEFAULT_SERVER_ADDR.to_string();
    };
    match addr.parse::<std::net::SocketAddr>() {
        Ok(_) => addr,
        Err(e) => {
            tracing::warn!("PIMBLE_APP_ADDR ({addr}) is not a host:port ({e}); using {DEFAULT_SERVER_ADDR}");
            DEFAULT_SERVER_ADDR.to_string()
        }
    }
}

/// That address as the URL of its JSON-RPC endpoint.
pub fn server_url() -> String {
    format!("http://{}", server_addr())
}

const MAX_CONNECT_ATTEMPTS: u32 = 6;
const BASE_RETRY_MS: u64 = 250;
/// Upper bound on probing an existing server. A foreign listener on our port
/// (anything that accepts TCP but never speaks JSON-RPC) must fail fast.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Try to connect to an existing server, or start one and connect.
/// Returns the client and optionally the server we started (if we own it).
pub async fn ensure_connected() -> Result<(PimbleClient, Option<PimbleServer>), String> {
    let mut rng = rand::rng();
    let addr = server_addr();
    let url = server_url();

    for attempt in 0..MAX_CONNECT_ATTEMPTS {
        // First, try connecting to an existing server (another Pimble instance),
        // verifying it is really ours with a cheap call. Bounded by PROBE_TIMEOUT.
        let probe = async {
            let client = PimbleClient::connect(&url).await.ok()?;
            client.list_stores().await.ok()?;
            Some(client)
        };
        match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            Ok(Some(client)) => {
                tracing::info!("Connected to existing server at {}", addr);
                return Ok((client, None));
            }
            Ok(None) => {}
            Err(_) => tracing::warn!(
                "Something on {} accepted the connection but did not answer as a Pimble server",
                addr
            ),
        }

        // No server running — try to start one
        let mut server = match addr.parse() {
            Ok(addr) => PimbleServer::with_config(ServerConfig {
                addr,
                ..Default::default()
            }),
            Err(_) => PimbleServer::new(),
        };
        match server.start().await {
            Ok(()) => {
                tracing::info!("Started embedded server on {}", addr);
                // Connect to the server we just started
                match PimbleClient::connect(&url).await {
                    Ok(client) => return Ok((client, Some(server))),
                    Err(e) => {
                        tracing::warn!("Started server but failed to connect: {}", e);
                        let _ = server.stop().await;
                        // Fall through to retry
                    }
                }
            }
            Err(e) => {
                // Port might be claimed by another instance that's still starting up
                tracing::warn!(
                    "Failed to start server on {} (attempt {}): {}",
                    addr,
                    attempt + 1,
                    e
                );
            }
        }

        // Exponential backoff with jitter before retrying
        if attempt + 1 < MAX_CONNECT_ATTEMPTS {
            let base = BASE_RETRY_MS * 2u64.pow(attempt);
            let jitter = rng.random_range(0..=base / 2);
            let delay = Duration::from_millis(base + jitter);
            tracing::debug!("Retrying connection in {:?} (attempt {})", delay, attempt + 1);
            tokio::time::sleep(delay).await;
        }
    }

    Err(format!(
        "Failed to connect or start a server on {} after {} attempts (is the port in use?)",
        addr, MAX_CONNECT_ATTEMPTS
    ))
}

/// Try to reconnect after a connection loss, optionally starting a new server.
pub async fn reconnect(owned_server: &mut Option<PimbleServer>) -> Result<PimbleClient, String> {
    // If we owned the server previously, stop it first (it may be dead anyway)
    if let Some(mut server) = owned_server.take() {
        let _ = server.stop().await;
    }

    let (client, new_server) = ensure_connected().await?;
    *owned_server = new_server;
    Ok(client)
}

/// Stop a server this process owns, and write every document it holds.
pub async fn shut_down(owned_server: &mut Option<PimbleServer>) {
    if let Some(mut server) = owned_server.take() {
        let store_manager = server.store_manager();
        let _ = server.stop().await;
        let mut manager = store_manager.write().await;
        let _ = manager.flush_all().await;
    }
}

// ── The open-store list ──────────────────────────────────────────────────

/// `<config dir>/pimble/state.json`, where the open-store list (and the app's
/// other remembered choices) live.
pub fn state_file_path() -> PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    config_dir.join("pimble").join("state.json")
}

fn read_state() -> serde_json::Value {
    let Ok(json) = std::fs::read_to_string(state_file_path()) else {
        return serde_json::json!({});
    };
    serde_json::from_str(&json).unwrap_or_else(|_| serde_json::json!({}))
}

/// The paths of the stores open on this computer, as last saved.
pub fn load_open_stores() -> Vec<String> {
    read_state()["open_stores"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Add `path` to the saved list (a read-modify-write of that one key, keeping
/// everything else in the file), unless it is there already.
pub fn add_open_store(path: &str) -> std::io::Result<()> {
    let mut state = read_state();
    let mut paths = load_open_stores();
    if paths.iter().any(|p| p == path) {
        return Ok(());
    }
    paths.push(path.to_string());
    state["open_stores"] = serde_json::json!(paths);
    let file = state_file_path();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&file, serde_json::to_string_pretty(&state).unwrap_or_default())
}
