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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// This process's server handed over to a newer one (`handOver`): its next
/// reconnect only looks for that one and starts no server of its own, so the
/// two never race for the port.
static HANDED_OVER: AtomicBool = AtomicBool::new(false);

/// The server this process ran was asked to hand over and is stopping.
pub fn handed_over() {
    HANDED_OVER.store(true, Ordering::SeqCst);
}

/// This build's version, the one a server it finds is compared with.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `x.y.z` as numbers; anything unreadable is the oldest there is.
fn version_key(v: &str) -> (u64, u64, u64) {
    let mut parts = v.split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// Whether a server built as `theirs` is older than this build.
pub fn is_older(theirs: &str) -> bool {
    version_key(theirs) < version_key(VERSION)
}

/// What a probe found answering on the port.
enum Found {
    /// A server this build may use.
    Usable(PimbleClient),
    /// An older server that agreed to hand over and is stopping.
    HandingOver,
    /// An older server that cannot be asked to: the sentence that says so.
    TooOld(String),
}

/// A Pimble server answering at `url`: used when it is this version or newer;
/// asked to hand over when it is older and can (docs/MCP_CONTRACT.md "Which
/// server it talks to"); refused with a sentence when it is older and cannot.
async fn probe(url: &str, addr: &str) -> Option<Found> {
    let client = PimbleClient::connect(url).await.ok()?;
    client.list_stores().await.ok()?;
    match client.server_info().await {
        Ok(info) if !is_older(&info.version) => Some(Found::Usable(client)),
        Ok(info) if info.hands_over => {
            tracing::info!("The Pimble server on {} is version {}, older than this {}: asking it to hand over", addr, info.version, VERSION);
            match client.hand_over().await {
                Ok(()) => Some(Found::HandingOver),
                Err(e) => Some(Found::TooOld(format!("An older Pimble server ({}) is running on {} and would not hand over ({e}). {}", info.version, addr, quit_hint(addr)))),
            }
        }
        Ok(info) => Some(Found::TooOld(format!("An older Pimble server ({}) is running on {}. {}", info.version, addr, quit_hint(addr)))),
        // From before 0.12.2, which is when servers started saying.
        Err(_) => Some(Found::TooOld(format!("An older Pimble server (from before 0.12.2) is running on {}. {}", addr, quit_hint(addr)))),
    }
}

/// Which process holds the port, as a sentence to act on.
fn quit_hint(addr: &str) -> String {
    match port_holder(addr) {
        Some((pid, name)) => format!("Quit {name} (process {pid}), then start Pimble again."),
        None => "Quit whatever started it (another Pimble, or an AI tool's pimble-mcp), then start Pimble again.".to_string(),
    }
}

/// The process listening on `addr`'s port, from `/proc` (Linux only).
#[cfg(target_os = "linux")]
fn port_holder(addr: &str) -> Option<(u32, String)> {
    let port: u16 = addr.rsplit(':').next()?.parse().ok()?;
    let want = format!("{port:04X}");
    let mut inode = None;
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else { continue };
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // local_address is HEX_IP:HEX_PORT; state 0A is LISTEN.
            if cols.len() > 9 && cols[3] == "0A" && cols[1].rsplit(':').next() == Some(want.as_str()) {
                inode = Some(cols[9].to_string());
            }
        }
    }
    let target = format!("socket:[{}]", inode?);
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path()).is_ok_and(|l| l.to_string_lossy() == target) {
                let name = std::fs::read_to_string(entry.path().join("comm")).map(|n| n.trim().to_string()).unwrap_or_else(|_| "it".to_string());
                return Some((pid, name));
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn port_holder(_addr: &str) -> Option<(u32, String)> {
    None
}

/// Try to connect to an existing server, or start one and connect.
/// Returns the client and optionally the server we started (if we own it).
pub async fn ensure_connected() -> Result<(PimbleClient, Option<PimbleServer>), String> {
    connect_or_start(false).await
}

/// `join_only`: after this process's server handed over, the newer one
/// needs a moment to start; only look for it, a little longer than usual.
async fn connect_or_start(join_only: bool) -> Result<(PimbleClient, Option<PimbleServer>), String> {
    let addr = server_addr();
    let url = server_url();
    let attempts = if join_only { MAX_CONNECT_ATTEMPTS + 4 } else { MAX_CONNECT_ATTEMPTS };

    for attempt in 0..attempts {
        // First, try connecting to an existing server (another Pimble instance),
        // verifying it is really ours with a cheap call. Bounded by PROBE_TIMEOUT.
        match tokio::time::timeout(PROBE_TIMEOUT, probe(&url, &addr)).await {
            Ok(Some(Found::Usable(client))) => {
                tracing::info!("Connected to existing server at {}", addr);
                return Ok((client, None));
            }
            Ok(Some(Found::TooOld(sentence))) => return Err(sentence),
            Ok(Some(Found::HandingOver)) => {
                // Give it a moment to write everything and free the port.
                tokio::time::sleep(Duration::from_millis(300)).await;
                continue;
            }
            Ok(None) => {}
            Err(_) => tracing::warn!(
                "Something on {} accepted the connection but did not answer as a Pimble server",
                addr
            ),
        }

        if join_only {
            tokio::time::sleep(Duration::from_millis(BASE_RETRY_MS * 2)).await;
            continue;
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
        if attempt + 1 < attempts {
            let base = BASE_RETRY_MS * 2u64.pow(attempt);
            // Drawn and dropped at once: the thread-local generator is not `Send`,
            // and this future is (pimble-mcp runs it on a multi-threaded runtime).
            let jitter = rand::rng().random_range(0..=base / 2);
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

    let join_only = HANDED_OVER.swap(false, Ordering::SeqCst);
    let (client, new_server) = connect_or_start(join_only).await?;
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

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(version_key("0.12.10") > version_key("0.12.9"));
        assert!(version_key("0.12.1") < version_key("0.13.0"));
        assert_eq!(version_key("1.2.3-beta"), (1, 2, 3));
        assert_eq!(version_key("nonsense"), (0, 0, 0), "unreadable is the oldest");
        assert!(is_older("0.0.1"));
        assert!(!is_older(VERSION), "the same version is not older");
    }
}
