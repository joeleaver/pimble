//! The desktop's backend: a background thread with a tokio runtime that
//! owns the API (`pimble_api::Pimble` over the RPC backend) and the
//! connection to this computer's Pimble server.
//!
//! Rinch has its own event loop and doesn't use tokio directly. So:
//! 1. a background thread runs a tokio runtime;
//! 2. channels carry `BackendCommand`s in and `BackendEvent`s out;
//! 3. Rinch is signalled to process events when they arrive.
//!
//! The connection is the implementation's (docs/API_CONTRACT.md decision
//! 11): [`Local`] joins or starts the embedded server
//! (`pimble_server::local`), notices it going away and connects again, and
//! says so to the UI; the API asks it for a live client on every call and
//! makes every store subscription again once the stores are open there. The
//! command loop knows nothing of connections.

use std::sync::{Arc, Weak};
use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_api::rpc::{Connector, RpcBackend};
use pimble_api::{ApiError, Pimble};
use pimble_client::PimbleClient;
use pimble_server::local::{ensure_connected, reconnect, server_addr, shut_down};
use pimble_server::PimbleServer;
use tokio::runtime::Runtime;

use crate::commands::{change_events, process_command, Context};
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

        thread::spawn(move || {
            let rt = Runtime::new().expect("Failed to create tokio runtime");
            rt.block_on(backend_loop(cmd_rx, event_tx, signal_ui));
        });

        Self { cmd_tx, event_rx }
    }
}

/// The connection to this computer's Pimble server: joined when one runs,
/// started (and owned) when none does, and made again the moment it closes,
/// by the same rule (an app that borrowed another instance's server notices
/// that instance quitting without waiting for its next call). Every
/// connection made is said to the UI as `Connected`, every one lost as
/// `Disconnected`.
pub(crate) struct Local {
    inner: tokio::sync::Mutex<Inner>,
    event_tx: Sender<BackendEvent>,
    signal_ui: Arc<dyn Fn() + Send + Sync>,
    client_id: String,
    /// Set when the app quits: a connection that closes then is not made
    /// again (that would start a server for an app that is leaving).
    closing: std::sync::atomic::AtomicBool,
    me: Weak<Local>,
}

struct Inner {
    client: Option<Arc<PimbleClient>>,
    /// The server this process started, when it did.
    owned: Option<PimbleServer>,
    /// Whether a connection was ever made: the first one may start a server
    /// with the saved store list, later ones join or start as `reconnect`
    /// does.
    ever: bool,
}

impl Local {
    fn new(event_tx: Sender<BackendEvent>, signal_ui: Arc<dyn Fn() + Send + Sync>, client_id: String) -> Arc<Self> {
        Arc::new_cyclic(|me| Local {
            inner: tokio::sync::Mutex::new(Inner { client: None, owned: None, ever: false }),
            event_tx,
            signal_ui,
            client_id,
            closing: std::sync::atomic::AtomicBool::new(false),
            me: me.clone(),
        })
    }

    fn emit(&self, event: BackendEvent) {
        let _ = self.event_tx.try_send(event);
        (self.signal_ui)();
    }

    /// A live client: the one held, or a new connection when there is none or
    /// it closed.
    pub(crate) async fn client(&self) -> Result<Arc<PimbleClient>, String> {
        let mut inner = self.inner.lock().await;
        if let Some(client) = &inner.client {
            if client.is_connected() {
                return Ok(client.clone());
            }
            tracing::warn!("The connection to the server closed; connecting again");
            inner.client = None;
            self.emit(BackendEvent::Disconnected);
        }
        if self.closing.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Pimble is quitting.".into());
        }
        let made = if inner.ever {
            reconnect(&mut inner.owned).await
        } else {
            ensure_connected().await.map(|(client, server)| {
                inner.owned = server;
                client
            })
        };
        let client = match made {
            Ok(client) => Arc::new(client),
            Err(e) => {
                tracing::error!("Connecting to the server failed: {e}");
                let message = if inner.ever { format!("Reconnection failed: {e}") } else { format!("Failed to connect: {e}") };
                self.emit(BackendEvent::Error { message });
                return Err(e);
            }
        };
        inner.ever = true;
        inner.client = Some(client.clone());
        self.watch(client.clone());
        self.hear_cloud(client.clone());
        self.emit(BackendEvent::Connected { server_addr: server_addr(), client_id: self.client_id.clone() });
        Ok(client)
    }

    /// Connect again the moment `client`'s WebSocket closes.
    fn watch(&self, client: Arc<PimbleClient>) {
        let me = self.me.clone();
        tokio::spawn(async move {
            client.on_disconnect().await;
            let Some(me) = me.upgrade() else { return };
            if !me.closing.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = me.client().await;
            }
        });
    }

    /// How this computer's connections to Pimble Cloud are doing, for the
    /// status bar, for as long as `client`'s connection lasts. A server from
    /// before 0.19.1 has no such subscription and the bar shows the account
    /// alone.
    fn hear_cloud(&self, client: Arc<PimbleClient>) {
        let me = self.me.clone();
        tokio::spawn(async move {
            let mut sub = match client.subscribe_cloud_connection().await {
                Ok(sub) => sub,
                Err(e) => {
                    tracing::debug!("No Pimble Cloud state from this server: {e}");
                    return;
                }
            };
            while let Some(Ok(connection)) = sub.next().await {
                let Some(me) = me.upgrade() else { return };
                me.emit(BackendEvent::CloudConnectionChanged { connection });
            }
        });
    }

    /// The app is quitting: stop the server this process owns, writing every
    /// document it holds.
    async fn shut_down(&self) {
        self.closing.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut inner = self.inner.lock().await;
        inner.client = None;
        shut_down(&mut inner.owned).await;
    }
}

/// The API's way to a live client: [`Local`], which connects again when the
/// last connection closed.
struct LocalConnector(Arc<Local>);

#[async_trait::async_trait]
impl Connector for LocalConnector {
    async fn client(&self) -> Result<Arc<PimbleClient>, ApiError> {
        self.0.client().await.map_err(|e| ApiError::Offline { sentence: format!("Pimble's server could not be reached: {e}") })
    }
}

async fn backend_loop(cmd_rx: Receiver<BackendCommand>, event_tx: Sender<BackendEvent>, signal_ui: impl Fn() + Send + Sync + 'static) {
    let signal_ui: Arc<dyn Fn() + Send + Sync> = Arc::new(signal_ui);

    let client_id = uuid::Uuid::new_v4().to_string();
    tracing::info!("Backend client ID: {}", client_id);

    let local = Local::new(event_tx.clone(), signal_ui.clone(), client_id.clone());
    let api = Arc::new(Pimble::with_presence_ttl(RpcBackend::new(LocalConnector(local.clone())), client_id.clone(), pimble_rpc::PRESENCE_TTL_MS));

    // The first connection, said to the UI either way.
    let _ = local.client().await;

    // Every change the API hears, after it merged it into what it holds.
    {
        let (api, event_tx, signal_ui, client_id) = (api.clone(), event_tx.clone(), signal_ui.clone(), client_id.clone());
        tokio::spawn(async move {
            let mut last = None;
            while let Some(change) = api.next_change().await {
                for event in change_events(&api, change, &client_id, &mut last).await {
                    if let Err(e) = event_tx.try_send(event) {
                        tracing::warn!("A change was dropped, the event channel is full: {e}");
                    }
                }
                signal_ui();
            }
        });
    }

    let plugin_calls = crate::plugin_calls::PluginCalls::default();

    // Blocking on the channel is fine here: this runs on the thread that
    // owns the runtime, and every task (the listener, the watchdog, a
    // picture being stored) runs on the runtime's workers.
    while let Ok(cmd) = cmd_rx.recv() {
        // A plugin's calls run beside the command loop, each a task of its
        // own: a slow one (or a subscription, which lasts) must not hold up
        // the window's commands.
        let cmd = match plugin_calls.take(cmd, &api, &event_tx, &signal_ui) {
            Some(cmd) => cmd,
            None => continue,
        };
        // What only the server's operator does goes over the connection
        // itself; asking for it here connects again first if it closed.
        let host = local.client().await.ok();
        let ctx = Context { api: &api, host, event_tx: &event_tx, signal_ui: &signal_ui, client_id: &client_id };
        // An import is many commands, each answered as the UI's own would be
        // (docs/IMPORT_CONTRACT.md).
        let event = match cmd {
            BackendCommand::Import { store_id, parent_id, format, name, files } => {
                let mut runner = Desktop { ctx: &ctx };
                Some(crate::import::import(&mut runner, store_id, parent_id, format, &name, &files).await)
            }
            cmd => process_command(&ctx, cmd).await,
        };
        if let Some(event) = event {
            let _ = event_tx.try_send(event);
            signal_ui();
        }
    }

    // The window closed: only a server this process started is stopped.
    local.shut_down().await;
}

/// `process_command`, as `crate::import` asks it.
struct Desktop<'a, 'b> {
    ctx: &'a Context<'b>,
}

impl crate::import::Runner for Desktop<'_, '_> {
    async fn run(&mut self, cmd: BackendCommand) -> Option<BackendEvent> {
        process_command(self.ctx, cmd).await
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use pimble_import::{Files, Format};
    use pimble_server::{PimbleServer, ServerConfig};

    /// File > Import end to end on the desktop's own path: a real server, the
    /// runner over `process_command`, an RTF file and a Scrivener project
    /// written under a node of an open store.
    #[tokio::test]
    async fn imports_land_under_the_chosen_node() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = PimbleServer::with_config(ServerConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            keystore_path: Some(dir.path().join("keys.json")),
            credentials_path: Some(dir.path().join("credentials.json")),
            replicas_dir: Some(dir.path().join("replicas")),
            ..Default::default()
        });
        server.start().await.unwrap();
        let c = std::sync::Arc::new(PimbleClient::connect(format!("http://{}", server.addr())).await.unwrap());
        let (store_id, root) = c.create_store(dir.path().join("t.pimble"), "T").await.unwrap();
        let parent = c.create_node(store_id, Some(root), "document", "Inbox").await.unwrap();

        let (event_tx, _event_rx) = crossbeam_channel::unbounded();
        let signal: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(|| {});
        let api = Arc::new(Pimble::new(RpcBackend::over(PimbleClient::connect(format!("http://{}", server.addr())).await.unwrap()), "test"));
        let ctx = Context { api: &api, host: Some(c.clone()), event_tx: &event_tx, signal_ui: &signal, client_id: "test" };
        let mut runner = Desktop { ctx: &ctx };

        let rtf = Files::from([("Letter.rtf".to_string(), br"{\rtf1\ansi Dear {\b Sam},\par}".to_vec())]);
        let event = crate::import::import(&mut runner, store_id, Some(parent), Format::Rtf, "Letter.rtf", &rtf).await;
        let BackendEvent::Imported { node_id, count: 1, .. } = event else { panic!("{event:?}") };
        let letter = c.get_node(store_id, node_id).await.unwrap();
        assert_eq!(letter.metadata.title, "Letter");
        assert_eq!(letter.parent_id, Some(parent));
        assert_eq!(pimble_crdt::NodeDoc::text_of(&letter.content).trim(), "Dear Sam,");

        let scrivx = r#"<ScrivenerProject><Binder>
            <BinderItem UUID="A" Type="DraftFolder"><Title>Draft</Title>
              <MetaData><LabelID>1</LabelID></MetaData>
              <Children>
                <BinderItem UUID="B" Type="Text"><Title>One</Title></BinderItem>
                <BinderItem UUID="C" Type="Text"><Title>Two</Title></BinderItem>
              </Children>
            </BinderItem>
          </Binder>
          <LabelSettings><Labels><Label ID="1" Color="1 0 0">Hot</Label></Labels></LabelSettings>
        </ScrivenerProject>"#;
        let scriv = Files::from([
            ("Novel.scrivx".to_string(), scrivx.as_bytes().to_vec()),
            ("Files/Data/B/content.rtf".to_string(), br"{\rtf1\ansi First.\par}".to_vec()),
        ]);
        let event = crate::import::import(&mut runner, store_id, Some(parent), Format::Scrivener, "Novel.scriv", &scriv).await;
        let BackendEvent::Imported { node_id: novel, count: 4, title, .. } = event else { panic!("{event:?}") };
        assert_eq!(title, "Novel");

        let (_, under_parent) = c.get_children(store_id, parent).await.unwrap();
        assert_eq!(under_parent.iter().map(|n| n.metadata.title.as_str()).collect::<Vec<_>>(), ["Letter", "Novel"]);
        let (_, binder) = c.get_children(store_id, novel).await.unwrap();
        let draft = &binder[0];
        assert_eq!(draft.node_type, "folder");
        assert_eq!(draft.metadata.color(), Some("#ff0000"));
        assert_eq!(draft.metadata.tags, ["Hot"]);
        let (_, chapters) = c.get_children(store_id, draft.id).await.unwrap();
        assert_eq!(chapters.iter().map(|n| n.metadata.title.as_str()).collect::<Vec<_>>(), ["One", "Two"]);
        let one = c.get_node(store_id, chapters[0].id).await.unwrap();
        assert_eq!(pimble_crdt::NodeDoc::text_of(&one.content).trim(), "First.");

        // Something that is not what it says is refused before anything is made.
        let bad = Files::from([("x.rtf".to_string(), b"hello".to_vec())]);
        let event = crate::import::import(&mut runner, store_id, Some(parent), Format::Rtf, "x.rtf", &bad).await;
        assert!(matches!(event, BackendEvent::ImportFailed { .. }), "{event:?}");
        assert_eq!(c.get_children(store_id, parent).await.unwrap().1.len(), 2);
    }
}
