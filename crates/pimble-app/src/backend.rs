//! Background thread for RPC communication
//!
//! Rinch has its own event loop and doesn't use tokio directly. We:
//! 1. Spawn a background thread with a tokio runtime
//! 2. Use channels to communicate between Rinch UI and async code
//! 3. Signal Rinch to process events when data arrives

use std::thread;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_client::PimbleClient;
use pimble_core::{MountRef, MountState, Node, NodeId, Store, StoreId, Workspace};
use pimble_server::PimbleServer;
use rand::Rng;
use tokio::runtime::Runtime;

/// Commands sent from UI to backend
#[derive(Debug)]
pub enum BackendCommand {
    Connect { url: String },
    Disconnect,

    // Store operations
    CreateStore { path: String, name: String },
    OpenStore { path: String },
    CloseStore { store_id: StoreId },
    ListStores,

    // Node operations
    CreateNode { store_id: StoreId, parent_id: Option<NodeId>, title: String },
    GetNode { store_id: StoreId, node_id: NodeId },
    GetChildren { store_id: StoreId, node_id: NodeId },
    SetNodeContent { store_id: StoreId, node_id: NodeId, content: Vec<u8> },
    RenameNode { store_id: StoreId, node_id: NodeId, title: String },
    DeleteNode { store_id: StoreId, node_id: NodeId },
    MoveNode { store_id: StoreId, node_id: NodeId, new_parent_id: NodeId, position: Option<usize> },

    // Mount operations
    CreateMount {
        store_id: StoreId,
        parent_id: NodeId,
        source_store_id: StoreId,
        source_node_id: NodeId,
        title: Option<String>,
    },
    GetMountState {
        store_id: StoreId,
        node_id: NodeId,
    },

    // Workspace operations
    CreateWorkspace { name: String, path: String },
    LoadWorkspace { path: String },
    SaveWorkspace { workspace: Workspace, path: String },
}

/// Events sent from backend to UI
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Connected { server_addr: String },
    Disconnected,
    Error { message: String },

    // Store events
    StoreCreated { store_id: StoreId, root_node_id: NodeId },
    StoreOpened { store: Store },
    StoreClosed { store_id: StoreId },
    StoreList { stores: Vec<Store> },

    // Node events
    NodeCreated { store_id: StoreId, parent_id: Option<NodeId>, node_id: NodeId },
    NodeLoaded { store_id: StoreId, node: Node },
    ChildrenLoaded { store_id: StoreId, parent_id: NodeId, children: Vec<Node> },
    NodeContentUpdated { store_id: StoreId, node_id: NodeId },
    NodeRenamed { store_id: StoreId, node_id: NodeId },
    NodeDeleted { store_id: StoreId, node_id: NodeId, parent_id: NodeId },
    NodeMoved { store_id: StoreId, node_id: NodeId, old_parent_id: NodeId, new_parent_id: NodeId },

    // Mount events
    MountCreated {
        store_id: StoreId,
        node_id: NodeId,
        mount_ref: MountRef,
    },
    MountStateChanged {
        store_id: StoreId,
        node_id: NodeId,
        state: MountState,
    },

    // Workspace events
    WorkspaceLoaded { workspace: Workspace },
    WorkspaceSaved,
}

/// Handle to communicate with the backend
#[derive(Clone)]
pub struct BackendHandle {
    pub cmd_tx: Sender<BackendCommand>,
    pub event_rx: Receiver<BackendEvent>,
}

impl BackendHandle {
    /// Spawn the backend thread and return a handle
    pub fn spawn(signal_ui: impl Fn() + Send + 'static) -> Self {
        let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(100);
        let (event_tx, event_rx) = bounded::<BackendEvent>(100);

        thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(backend_loop(cmd_rx, event_tx, signal_ui));
        });

        Self { cmd_tx, event_rx }
    }

    /// Send a command to the backend (non-blocking)
    pub fn send_command(&self, cmd: BackendCommand) -> Result<(), crossbeam_channel::TrySendError<BackendCommand>> {
        self.cmd_tx.try_send(cmd)
    }

    /// Send a command to the backend (non-blocking), ignoring result
    pub fn send(&self, cmd: BackendCommand) {
        let _ = self.cmd_tx.try_send(cmd);
    }

    /// Try to receive an event (non-blocking)
    pub fn try_recv(&self) -> Option<BackendEvent> {
        self.event_rx.try_recv().ok()
    }
}

const SERVER_URL: &str = "http://127.0.0.1:9876";
const SERVER_ADDR: &str = "127.0.0.1:9876";
const MAX_CONNECT_ATTEMPTS: u32 = 6;
const BASE_RETRY_MS: u64 = 250;

/// Try to connect to an existing server, or start one and connect.
/// Returns the client and optionally the server we started (if we own it).
async fn ensure_connected() -> Result<(PimbleClient, Option<PimbleServer>), String> {
    let mut rng = rand::rng();

    for attempt in 0..MAX_CONNECT_ATTEMPTS {
        // First, try connecting to an existing server
        if let Ok(client) = PimbleClient::connect(SERVER_URL).await {
            // Verify it's actually alive by making a cheap call
            if client.list_stores().await.is_ok() {
                tracing::info!("Connected to existing server at {}", SERVER_ADDR);
                return Ok((client, None));
            }
        }

        // No server running — try to start one
        let mut server = PimbleServer::new();
        match server.start().await {
            Ok(()) => {
                tracing::info!("Started embedded server on {}", SERVER_ADDR);
                // Connect to the server we just started
                match PimbleClient::connect(SERVER_URL).await {
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
                tracing::debug!(
                    "Failed to start server (attempt {}): {}",
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
        "Failed to connect or start server after {} attempts",
        MAX_CONNECT_ATTEMPTS
    ))
}

/// Try to reconnect after a connection loss, optionally starting a new server.
async fn reconnect(owned_server: &mut Option<PimbleServer>) -> Result<PimbleClient, String> {
    // If we owned the server previously, stop it first (it may be dead anyway)
    if let Some(mut server) = owned_server.take() {
        let _ = server.stop().await;
    }

    let (client, new_server) = ensure_connected().await?;
    *owned_server = new_server;
    Ok(client)
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
}

async fn backend_loop(
    cmd_rx: Receiver<BackendCommand>,
    event_tx: Sender<BackendEvent>,
    signal_ui: impl Fn(),
) {
    let mut client: Option<PimbleClient> = None;
    let mut owned_server: Option<PimbleServer> = None;

    // Initial connection
    match ensure_connected().await {
        Ok((c, server)) => {
            client = Some(c);
            owned_server = server;
            let _ = event_tx.try_send(BackendEvent::Connected {
                server_addr: SERVER_ADDR.to_string(),
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

        let event = process_command(&mut client, cmd).await;

        if let Some(ref event) = event {
            // Check if this is a connection error — if so, try to reconnect
            if let BackendEvent::Error { message } = event {
                if is_connection_error(message) {
                    tracing::warn!("Connection error detected, attempting reconnect: {}", message);
                    let _ = event_tx.try_send(BackendEvent::Disconnected);
                    signal_ui();

                    match reconnect(&mut owned_server).await {
                        Ok(c) => {
                            client = Some(c);
                            let _ = event_tx.try_send(BackendEvent::Connected {
                                server_addr: SERVER_ADDR.to_string(),
                            });
                            signal_ui();
                            // Don't send the original error — we recovered
                            continue;
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
                }
            }
        }

        if let Some(event) = event {
            let _ = event_tx.try_send(event);
            signal_ui();
        }
    }

    // Cleanup: only stop the server if we own it
    if let Some(mut server) = owned_server.take() {
        let store_manager = server.store_manager();
        let _ = server.stop().await;
        let mut manager = store_manager.write().await;
        let _ = manager.flush_all().await;
    }
}

async fn process_command(
    client: &mut Option<PimbleClient>,
    cmd: BackendCommand,
) -> Option<BackendEvent> {
    match cmd {
        BackendCommand::Connect { url } => {
            match PimbleClient::connect(&url).await {
                Ok(c) => {
                    *client = Some(c);
                    Some(BackendEvent::Connected { server_addr: url })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::Disconnect => {
            *client = None;
            Some(BackendEvent::Disconnected)
        }

        BackendCommand::CreateStore { path, name } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_store(&path, &name).await {
                Ok((store_id, root_node_id)) => {
                    Some(BackendEvent::StoreCreated { store_id, root_node_id })
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::OpenStore { path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.open_store(&path).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CloseStore { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.close_store(store_id).await {
                Ok(()) => Some(BackendEvent::StoreClosed { store_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ListStores => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.list_stores().await {
                Ok(stores) => Some(BackendEvent::StoreList { stores }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CreateNode { store_id, parent_id, title } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_node(store_id, parent_id, "document", &title).await {
                Ok(node_id) => Some(BackendEvent::NodeCreated { store_id, parent_id, node_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetNode { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_node(store_id, node_id).await {
                Ok(node) => Some(BackendEvent::NodeLoaded { store_id, node }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetChildren { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_children(store_id, node_id).await {
                Ok(children) => Some(BackendEvent::ChildrenLoaded {
                    store_id,
                    parent_id: node_id,
                    children
                }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::SetNodeContent { store_id, node_id, content } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.set_node_content_bytes(store_id, node_id, content).await {
                Ok(()) => Some(BackendEvent::NodeContentUpdated { store_id, node_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::RenameNode { store_id, node_id, title } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_node(store_id, node_id).await {
                Ok(mut node) => {
                    node.metadata.title = title;
                    node.metadata.custom.insert(
                        "explicit_title".to_string(),
                        serde_json::Value::Bool(true),
                    );
                    match c.update_node_metadata(store_id, node_id, node.metadata).await {
                        Ok(()) => Some(BackendEvent::NodeRenamed { store_id, node_id }),
                        Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
                    }
                }
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::DeleteNode { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            // Get parent before deleting
            let parent_id = match c.get_node(store_id, node_id).await {
                Ok(node) => node.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())),
                Err(e) => return Some(BackendEvent::Error { message: e.to_string() }),
            };
            match c.delete_node(store_id, node_id).await {
                Ok(()) => Some(BackendEvent::NodeDeleted { store_id, node_id, parent_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::MoveNode { store_id, node_id, new_parent_id, position } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            // Get old parent before moving
            let old_parent_id = match c.get_node(store_id, node_id).await {
                Ok(node) => node.parent_id.unwrap_or(NodeId(uuid::Uuid::nil())),
                Err(e) => return Some(BackendEvent::Error { message: e.to_string() }),
            };
            match c.move_node(store_id, node_id, new_parent_id, position).await {
                Ok(()) => Some(BackendEvent::NodeMoved { store_id, node_id, old_parent_id, new_parent_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CreateMount { store_id, parent_id, source_store_id, source_node_id, title } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_mount(store_id, parent_id, source_store_id, source_node_id, title).await {
                Ok((node_id, mount_ref)) => Some(BackendEvent::MountCreated { store_id, node_id, mount_ref }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetMountState { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_mount_state(store_id, node_id).await {
                Ok((state, _mount_ref)) => Some(BackendEvent::MountStateChanged { store_id, node_id, state }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::CreateWorkspace { name, path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.create_workspace(&name, &path).await {
                Ok(workspace) => Some(BackendEvent::WorkspaceLoaded { workspace }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::LoadWorkspace { path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.load_workspace(&path).await {
                Ok(workspace) => Some(BackendEvent::WorkspaceLoaded { workspace }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::SaveWorkspace { workspace, path } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.save_workspace(workspace, &path).await {
                Ok(()) => Some(BackendEvent::WorkspaceSaved),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }
    }
}
