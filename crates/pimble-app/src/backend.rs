//! Background thread for RPC communication
//!
//! Rinch has its own event loop and doesn't use tokio directly. We:
//! 1. Spawn a background thread with a tokio runtime
//! 2. Use channels to communicate between Rinch UI and async code
//! 3. Signal Rinch to process events when data arrives

use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_client::PimbleClient;
use pimble_server::local::{ensure_connected, reconnect, server_addr, shut_down};
use pimble_server::PimbleServer;
use tokio::runtime::Runtime;

use crate::commands::process_command;
use crate::protocol::{BackendCommand, BackendEvent, BackendHandle};

impl BackendHandle {
    /// Spawn the backend thread and return a handle.
    ///
    /// The desktop implementation of the seam in [`crate::protocol`]: a
    /// background thread with its own tokio runtime, which connects to (or
    /// starts) the embedded server and then runs the command loop. The web app
    /// builds the same pair of channels around a `spawn_local` task instead.
    pub fn spawn(signal_ui: impl Fn() + Send + Sync + 'static) -> Self {
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(100);
        let (event_tx, event_rx) = bounded::<BackendEvent>(1000);

        let watchdog_tx = cmd_tx.clone();
        thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(backend_loop(cmd_rx, watchdog_tx, event_tx, signal_ui));
        });

        Self { cmd_tx, event_rx }
    }
}

/// Returns true if an error looks like a connection/transport failure
/// (as opposed to a logical RPC error like "store not found").
fn is_connection_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("broken pipe")
        || lower.contains("transport")
        || lower.contains("hyper")
        || lower.contains("tcp")
        || lower.contains("eof")
        || lower.contains("not connected")
        || lower.contains("connection closed")
}

/// Watch `client` and post `ConnectionLost` to the command loop the moment
/// its WebSocket closes. This is what lets an app that borrowed another
/// instance's embedded server notice that instance quitting, instead of
/// finding out from the next failed call.
fn spawn_connection_watchdog(client: std::sync::Arc<PimbleClient>, generation: u64, cmd_tx: Sender<BackendCommand>) {
    tokio::spawn(async move {
        client.on_disconnect().await;
        tracing::warn!("Connection to the server closed (generation {})", generation);
        let _ = cmd_tx.try_send(BackendCommand::ConnectionLost { generation });
    });
}

async fn backend_loop(
    cmd_rx: Receiver<BackendCommand>,
    cmd_tx: Sender<BackendCommand>,
    event_tx: Sender<BackendEvent>,
    signal_ui: impl Fn() + Send + Sync + 'static,
) {
    let signal_arc: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(signal_ui);
    let signal_ui = signal_arc.clone();

    let client_id = uuid::Uuid::new_v4().to_string();
    tracing::info!("Backend client ID: {}", client_id);

    let mut client: Option<std::sync::Arc<PimbleClient>> = None;
    let mut owned_server: Option<PimbleServer> = None;
    // Bumped on every (re)connection; watchdogs report the generation they
    // watched so one left over from a previous connection cannot trigger a
    // second reconnect.
    let mut generation: u64 = 0;

    // Initial connection
    match ensure_connected().await {
        Ok((c, server)) => {
            let c = std::sync::Arc::new(c);
            spawn_connection_watchdog(std::sync::Arc::clone(&c), generation, cmd_tx.clone());
            client = Some(c);
            owned_server = server;
            let _ = event_tx.try_send(BackendEvent::Connected {
                server_addr: server_addr(),
                client_id: client_id.clone(),
            });
            signal_ui();
        }
        Err(e) => {
            tracing::error!("Initial connection failed: {}", e);
            let _ = event_tx.try_send(BackendEvent::Error {
                message: format!("Failed to connect: {}", e),
            });
            signal_ui();
        }
    }

    loop {
        // Block waiting for commands
        let cmd = match cmd_rx.recv() {
            Ok(cmd) => cmd,
            Err(_) => break, // Channel closed, exit
        };

        // A dead connection is handled before the command runs against it:
        // the watchdog's `ConnectionLost` for the current generation, or a
        // client that reports itself closed. Either way, reconnect (starting
        // our own embedded server if the one we borrowed is gone), tell the
        // UI, and then run the command against the new connection. A stale
        // `ConnectionLost` is dropped.
        let lost = match &cmd {
            BackendCommand::ConnectionLost { generation: g } => {
                if *g != generation {
                    continue;
                }
                true
            }
            _ => client.as_ref().map_or(false, |c| !c.is_connected()),
        };
        if lost {
            tracing::warn!("Connection lost, attempting reconnect");
            let _ = event_tx.try_send(BackendEvent::Disconnected);
            signal_ui();
            match reconnect(&mut owned_server).await {
                Ok(c) => {
                    generation += 1;
                    let c = std::sync::Arc::new(c);
                    spawn_connection_watchdog(std::sync::Arc::clone(&c), generation, cmd_tx.clone());
                    client = Some(c);
                    let _ = event_tx.try_send(BackendEvent::Connected {
                        server_addr: server_addr(),
                        client_id: client_id.clone(),
                    });
                    signal_ui();
                }
                Err(e) => {
                    tracing::error!("Reconnection failed: {}", e);
                    let _ = event_tx.try_send(BackendEvent::Error {
                        message: format!("Reconnection failed: {}", e),
                    });
                    signal_ui();
                    continue;
                }
            }
            if matches!(cmd, BackendCommand::ConnectionLost { .. }) {
                continue;
            }
        }

        let event = process_command(&mut client, cmd, &event_tx, &signal_arc, &client_id).await;

        if let Some(ref event) = event {
            // A call that failed because the connection died under it: the
            // watchdog will post `ConnectionLost` and the next command
            // reconnects; report it as a disconnect rather than a generic error.
            if let BackendEvent::Error { message } = event {
                if is_connection_error(message) {
                    tracing::warn!("Connection error on a call: {}", message);
                    let _ = event_tx.try_send(BackendEvent::Disconnected);
                    signal_ui();
                    // Wake the loop so the reconnect happens even if the UI
                    // sends nothing else for a while.
                    let _ = cmd_tx.try_send(BackendCommand::ConnectionLost { generation });
                    continue;
                }
            }
        }

        if let Some(event) = event {
            let _ = event_tx.try_send(event);
            signal_ui();
        }
    }

    // Cleanup: only stop the server if we own it
    shut_down(&mut owned_server).await;
}
