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
use pimble_core::{AuthMethod, MountRef, MountState, Node, NodeId, RemoteEndpoint, Store, StoreId, SyncState};
use pimble_server::PimbleServer;
use rand::Rng;
use tokio::runtime::Runtime;

/// Commands sent from UI to backend
#[derive(Debug)]
pub enum BackendCommand {
    // Store operations
    CreateStore { path: String, name: String },
    OpenStore { path: String },
    CloseStore { store_id: StoreId },
    /// List every store the server currently has open. Used to discover a
    /// store the server opened implicitly to resolve a mount (decision 4) —
    /// the client sees its id in `ChildrenLoaded.children_store_id` before it
    /// knows anything else about it.
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

    // Collaborative editing — broadcast incremental changes to server
    BroadcastChanges {
        store_id: StoreId,
        node_id: NodeId,
        /// Base64-encoded incremental yrs change bytes
        changes: String,
    },

    /// Reconcile the editor's collaboration session with the server's copy
    /// of a node's content: state vector in, the server's diff and state
    /// vector out (`NodeContentReconciled`). Stateless on both ends; the
    /// same primitive the replica sync link uses between servers.
    ReconcileNodeContent { store_id: StoreId, node_id: NodeId, state_vector: Vec<u8> },

    // Subscription operations
    SubscribeStoreChanges { store_id: StoreId },
    SubscribeNodeChanges { store_id: StoreId, node_id: NodeId },

    /// Internal: the connection watchdog saw the WebSocket close. Carries the
    /// connection generation it watched so a stale watchdog (from before a
    /// reconnect) is ignored.
    ConnectionLost { generation: u64 },

    // Search
    Search { query: String, stores: Vec<StoreId>, limit: usize },
    RebuildIndex { store_id: StoreId },

    // Replica sync (docs/history/HARDENING_CONTRACT.md "B: app") — routed through
    // this server's own `listRemoteStores`/`addRemoteStore` RPCs, never a
    // direct connection to the remote from the app itself (decision 5).
    /// Empty `token` means "use whatever this server already saved for that
    /// remote's origin" (decision 4); non-empty is sent as `AuthMethod::Bearer`.
    ListRemoteStores { url: String, token: String },
    /// Create a local replica of `remote_store_id` from `remote`, linked to
    /// it. No path: the server puts it in its own data directory. Same
    /// `token` semantics as `ListRemoteStores`.
    AddRemoteStore { url: String, remote_store_id: StoreId, token: String },
    /// Link a local store to a remote (`Some`) or unlink it (`None`).
    SetStoreSync { store_id: StoreId, remote: Option<RemoteEndpoint> },
    /// Ask for a store's current sync link and state.
    GetStoreSync { store_id: StoreId },
    /// Stop a replica's sync link, close it, and delete its directory.
    /// `force` removes one whose link is not `Synced` (decision 6).
    RemoveReplica { store_id: StoreId, force: bool },
}

/// Events sent from backend to UI
#[derive(Debug, Clone)]
pub enum BackendEvent {
    Connected { server_addr: String, client_id: String },
    Disconnected,
    Error { message: String },

    // Store events
    StoreCreated { store_id: StoreId, root_node_id: NodeId },
    StoreOpened { store: Store },
    StoreClosed { store_id: StoreId },
    /// Answer to `ListStores`: every store the server currently has open.
    StoresListed { stores: Vec<Store> },

    // Node events
    NodeCreated { store_id: StoreId, parent_id: Option<NodeId>, node_id: NodeId },
    NodeLoaded { store_id: StoreId, node: Node },
    /// `children` live in `children_store_id`: the same as `store_id` for an
    /// ordinary parent, the mount's source store when `parent_id` is a mount point.
    ChildrenLoaded { store_id: StoreId, parent_id: NodeId, children_store_id: StoreId, children: Vec<Node> },
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
        mount_ref: MountRef,
    },

    /// Answer to `ReconcileNodeContent`: everything the server has beyond
    /// the session's state vector (may be empty), and the server's own state
    /// vector, so the session can send back what the server lacks.
    NodeContentReconciled { store_id: StoreId, node_id: NodeId, diff: Vec<u8>, server_state_vector: Vec<u8> },

    // Remote change events (from subscriptions)
    RemoteStoreChange { store_id: StoreId, change_kind: pimble_rpc::StoreChangeKind, source_client_id: Option<String> },

    // Collaborative editing
    /// Remote incremental changes arrived — apply to the local editor's collab
    /// session. No node identity carried: pimble has one shared editor pane and
    /// the subscription that produces this is already scoped to that node.
    RemoteChanges { changes: String },

    // Search
    /// The outcome of a `Search` command. Carries `Err` rather than folding
    /// into the generic `Error` event so a failed search (including "the
    /// index is still building") shows inline in the results panel without
    /// touching the connection status bar or the reconnect-on-error path.
    SearchResults { results: Result<Vec<pimble_rpc::SearchResultItem>, String> },
    IndexRebuilt { store_id: StoreId, indexed: usize },

    // Replica sync (docs/SYNC_CONTRACT.md "B: app side")
    /// Answer to `ListRemoteStores`: the remote's open stores, or the error
    /// connecting to / querying it (shown inline in the connect modal, never
    /// folded into the generic `Error` event).
    RemoteStoresListed { url: String, result: Result<Vec<Store>, String> },
    /// A store's sync link and state changed: the answer to `SetStoreSync` or
    /// `GetStoreSync`, or a live `SyncStateChanged` notification (which
    /// carries only the state — the event handler keeps the known `remote`).
    StoreSyncChanged { store_id: StoreId, remote: Option<RemoteEndpoint>, state: SyncState },
    /// Answer to `RemoveReplica`: the replica is gone. Handled exactly like
    /// `StoreClosed` (tree + saved open-store list cleanup).
    ReplicaRemoved { store_id: StoreId },
}

/// Handle to communicate with the backend
#[derive(Clone)]
pub struct BackendHandle {
    pub cmd_tx: Sender<BackendCommand>,
    pub event_rx: Receiver<BackendEvent>,
}

impl BackendHandle {
    /// Spawn the backend thread and return a handle
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

    /// Send a command to the backend (non-blocking), ignoring result
    pub fn send(&self, cmd: BackendCommand) {
        if let Err(e) = self.cmd_tx.try_send(cmd) {
            tracing::error!("Backend channel send failed: {}", e);
        }
    }

    /// Try to receive an event (non-blocking)
    pub fn try_recv(&self) -> Option<BackendEvent> {
        self.event_rx.try_recv().ok()
    }
}

// 7462 spells PIMB on a phone keypad. (The previous 9876 collided with the
// Blender MCP add-on's default port: its raw TCP socket accepted our WebSocket
// handshake and never answered, so the app sat at "Connecting..." forever.)
const SERVER_URL: &str = "http://127.0.0.1:7462";
const SERVER_ADDR: &str = "127.0.0.1:7462";
const MAX_CONNECT_ATTEMPTS: u32 = 6;
const BASE_RETRY_MS: u64 = 250;
/// Upper bound on probing an existing server. A foreign listener on our port
/// (anything that accepts TCP but never speaks JSON-RPC) must fail fast.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Try to connect to an existing server, or start one and connect.
/// Returns the client and optionally the server we started (if we own it).
async fn ensure_connected() -> Result<(PimbleClient, Option<PimbleServer>), String> {
    let mut rng = rand::rng();

    for attempt in 0..MAX_CONNECT_ATTEMPTS {
        // First, try connecting to an existing server (another Pimble instance),
        // verifying it is really ours with a cheap call. Bounded by PROBE_TIMEOUT.
        let probe = async {
            let client = PimbleClient::connect(SERVER_URL).await.ok()?;
            client.list_stores().await.ok()?;
            Some(client)
        };
        match tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            Ok(Some(client)) => {
                tracing::info!("Connected to existing server at {}", SERVER_ADDR);
                return Ok((client, None));
            }
            Ok(None) => {}
            Err(_) => tracing::warn!(
                "Something on {} accepted the connection but did not answer as a Pimble server",
                SERVER_ADDR
            ),
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
                tracing::warn!(
                    "Failed to start server on {} (attempt {}): {}",
                    SERVER_ADDR,
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
        SERVER_ADDR, MAX_CONNECT_ATTEMPTS
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
                server_addr: SERVER_ADDR.to_string(),
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
                        server_addr: SERVER_ADDR.to_string(),
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
    if let Some(mut server) = owned_server.take() {
        let store_manager = server.store_manager();
        let _ = server.stop().await;
        let mut manager = store_manager.write().await;
        let _ = manager.flush_all().await;
    }
}

async fn process_command(
    client: &mut Option<std::sync::Arc<PimbleClient>>,
    cmd: BackendCommand,
    event_tx: &Sender<BackendEvent>,
    signal_ui: &std::sync::Arc<dyn Fn() + Send + Sync>,
    client_id: &str,
) -> Option<BackendEvent> {
    match cmd {
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
                Ok(stores) => Some(BackendEvent::StoresListed { stores }),
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
                Ok((children_store_id, children)) => Some(BackendEvent::ChildrenLoaded {
                    store_id,
                    parent_id: node_id,
                    children_store_id,
                    children
                }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::SetNodeContent { store_id, node_id, content } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.set_node_content_bytes(store_id, node_id, content, Some(client_id.to_string())).await {
                Ok(()) => Some(BackendEvent::NodeContentUpdated { store_id, node_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::BroadcastChanges { store_id, node_id, changes } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            use pimble_rpc::EditOperation;
            if let Err(e) = c.apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes }).await {
                tracing::warn!("BroadcastChanges failed: {}", e);
            }
            None
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
                Ok((state, mount_ref)) => Some(BackendEvent::MountStateChanged { store_id, node_id, state, mount_ref }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ReconcileNodeContent { store_id, node_id, state_vector } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.sync_node_content(store_id, node_id, &state_vector).await {
                Ok((diff, server_state_vector)) => Some(BackendEvent::NodeContentReconciled {
                    store_id,
                    node_id,
                    diff,
                    server_state_vector,
                }),
                Err(e) => Some(BackendEvent::Error { message: format!("Reconcile failed: {}", e) }),
            }
        }

        BackendCommand::SubscribeStoreChanges { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.subscribe_store_changes(store_id).await {
                Ok(mut sub) => {
                    let tx = event_tx.clone();
                    let signal = signal_ui.clone();
                    tokio::spawn(async move {
                        while let Some(Ok(notification)) = sub.next().await {
                            let event = BackendEvent::RemoteStoreChange {
                                store_id: notification.store_id,
                                change_kind: notification.change_kind,
                                source_client_id: notification.source_client_id,
                            };
                            if let Err(e) = tx.try_send(event) {
                                tracing::warn!("RemoteStoreChange channel full, dropped: {}", e);
                            }
                            signal();
                        }
                    });
                    None // No immediate event
                }
                Err(e) => Some(BackendEvent::Error { message: format!("Subscribe failed: {}", e) }),
            }
        }

        BackendCommand::SubscribeNodeChanges { store_id, node_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.subscribe_node_changes(store_id, node_id).await {
                Ok(mut sub) => {
                    let tx = event_tx.clone();
                    let signal = signal_ui.clone();
                    let my_client_id = client_id.to_string();
                    tokio::spawn(async move {
                        loop {
                            let notification = match sub.next().await {
                                Some(Ok(n)) => n,
                                Some(Err(e)) => {
                                    tracing::warn!("Node subscription error: {}", e);
                                    continue;
                                }
                                None => break,
                            };

                            tracing::info!("Node sub: received notification, source={:?}, has_op={}", notification.source_client_id, notification.operation.is_some());

                            // Skip our own echoes
                            if let Some(ref source) = notification.source_client_id {
                                if source == &my_client_id {
                                    continue;
                                }
                            }

                            // Forward incremental changes if present
                            if let Some(ref op) = notification.operation {
                                use pimble_rpc::EditOperation;
                                let EditOperation::IncrementalChanges { changes } = op;
                                if let Err(e) = tx.try_send(BackendEvent::RemoteChanges {
                                    changes: changes.clone(),
                                }) {
                                    tracing::warn!("RemoteChanges channel full, dropped: {}", e);
                                }
                            }
                            signal();
                        }
                    });
                    None
                }
                Err(e) => Some(BackendEvent::Error { message: format!("Subscribe failed: {}", e) }),
            }
        }

        // Handled in `backend_loop` before dispatch; never reaches here.
        BackendCommand::ConnectionLost { .. } => None,

        BackendCommand::Search { query, stores, limit } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::SearchResults { results: Err("Not connected".into()) });
            };
            // Ask for hybrid (keyword + semantic); a server built without an ONNX
            // link mode answers keyword-only.
            match c.search(query, stores, true, limit).await {
                Ok(results) => Some(BackendEvent::SearchResults { results: Ok(results) }),
                Err(e) => Some(BackendEvent::SearchResults { results: Err(e.to_string()) }),
            }
        }

        BackendCommand::RebuildIndex { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.rebuild_index(store_id).await {
                Ok(indexed) => Some(BackendEvent::IndexRebuilt { store_id, indexed }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::ListRemoteStores { url, token } => {
            // Through this server's own `listRemoteStores` (decision 5): the
            // app never connects to a remote itself. `c` here is our
            // connection to the local embedded/shared server.
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::RemoteStoresListed { url, result: Err("Not connected".into()) });
            };
            let remote = match remote_endpoint(&url, &token) {
                Ok(r) => r,
                Err(message) => return Some(BackendEvent::RemoteStoresListed { url, result: Err(message) }),
            };
            match c.list_remote_stores(remote).await {
                Ok(stores) => Some(BackendEvent::RemoteStoresListed { url, result: Ok(stores) }),
                Err(e) => Some(BackendEvent::RemoteStoresListed { url, result: Err(e.to_string()) }),
            }
        }

        BackendCommand::AddRemoteStore { url, remote_store_id, token } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            let remote = match remote_endpoint(&url, &token) {
                Ok(r) => r,
                Err(message) => return Some(BackendEvent::Error { message }),
            };
            // No path: the server puts the replica in its own data directory.
            match c.add_remote_store(remote, remote_store_id, None).await {
                Ok(store) => Some(BackendEvent::StoreOpened { store }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::SetStoreSync { store_id, remote } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.set_store_sync(store_id, remote).await {
                Ok((remote, state)) => Some(BackendEvent::StoreSyncChanged { store_id, remote, state }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::GetStoreSync { store_id } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.get_store_sync(store_id).await {
                Ok((remote, state)) => Some(BackendEvent::StoreSyncChanged { store_id, remote, state }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }

        BackendCommand::RemoveReplica { store_id, force } => {
            let Some(c) = client.as_ref() else {
                return Some(BackendEvent::Error { message: "Not connected".into() });
            };
            match c.remove_replica(store_id, force).await {
                Ok(()) => Some(BackendEvent::ReplicaRemoved { store_id }),
                Err(e) => Some(BackendEvent::Error { message: e.to_string() }),
            }
        }
    }
}

/// Build a `RemoteEndpoint` for a URL typed into a modal: an empty token
/// means "use whatever this server already saved for that origin"
/// (`AuthMethod::None`, docs/history/HARDENING_CONTRACT.md decision 4); a non-empty
/// one is sent as `AuthMethod::Bearer`.
fn remote_endpoint(url: &str, token: &str) -> Result<RemoteEndpoint, String> {
    let url: url::Url = url.parse().map_err(|e| format!("Invalid remote URL: {}", e))?;
    let auth = if token.is_empty() { AuthMethod::None } else { AuthMethod::Bearer { token: token.to_string() } };
    Ok(RemoteEndpoint { url, auth })
}
