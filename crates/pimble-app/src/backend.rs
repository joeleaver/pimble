//! Background thread for RPC communication
//!
//! Makepad has its own event loop and doesn't use tokio directly. We:
//! 1. Spawn a background thread with a tokio runtime
//! 2. Use channels to communicate between Makepad UI and async code
//! 3. Signal Makepad to redraw when data arrives

use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_client::PimbleClient;
use pimble_core::{Node, NodeId, Store, StoreId, Workspace};
use pimble_server::PimbleServer;
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
    MoveNode { store_id: StoreId, node_id: NodeId, new_parent_id: NodeId, position: Option<usize> },

    // Workspace operations
    CreateWorkspace { name: String, path: String },
    LoadWorkspace { path: String },
    SaveWorkspace { workspace: Workspace, path: String },
}

/// Events sent from backend to UI
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Connected,
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
    NodeMoved { store_id: StoreId, node_id: NodeId, old_parent_id: NodeId, new_parent_id: NodeId },

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

async fn backend_loop(
    cmd_rx: Receiver<BackendCommand>,
    event_tx: Sender<BackendEvent>,
    signal_ui: impl Fn(),
) {
    let mut client: Option<PimbleClient> = None;

    // Start embedded server and auto-connect
    let mut server = PimbleServer::new();
    match server.start().await {
        Ok(()) => {
            let url = format!("http://{}", server.addr());
            tracing::info!("Embedded server started on {}", server.addr());
            match PimbleClient::connect(&url).await {
                Ok(c) => {
                    client = Some(c);
                    let _ = event_tx.try_send(BackendEvent::Connected);
                    signal_ui();
                }
                Err(e) => {
                    tracing::error!("Failed to connect to embedded server: {}", e);
                    let _ = event_tx.try_send(BackendEvent::Error {
                        message: format!("Failed to connect: {}", e),
                    });
                    signal_ui();
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to start embedded server: {}", e);
            let _ = event_tx.try_send(BackendEvent::Error {
                message: format!("Server failed to start: {}", e),
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

        if let Some(event) = event {
            let _ = event_tx.try_send(event);
            signal_ui();
        }
    }

    // Cleanup: stop server and flush stores
    let store_manager = server.store_manager();
    let _ = server.stop().await;
    let mut manager = store_manager.write().await;
    let _ = manager.flush_all().await;
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
                    Some(BackendEvent::Connected)
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
