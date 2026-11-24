//! The browser's backend on its own server (docs/ONE_CODEBASE_CONTRACT.md
//! wave 4): the desktop's shape (`pimble-app/src/backend.rs`) over a
//! `MessagePort` instead of a loopback WebSocket. Every command goes through
//! the same `pimble_api::Pimble` over the same RPC backend and the same
//! `pimble_app::commands`; the server in this browser's worker answers it, as
//! the embedded server answers the desktop's.
//!
//! What is the browser's here: [`Local`], the connection (a port from the
//! hub, made again when the hub ends it: the leader tab closed, its worker
//! replaced), and keeping the account's stores here ([`keep_account_stores`],
//! section 5: every store the account is granted is a replica in this
//! browser, added by the same `cloudAddHostedStore` the desktop's "Add Hosted
//! Store..." calls).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_api::rpc::{Connector, RpcBackend};
use pimble_api::{ApiError, Pimble};
use pimble_app::commands::{change_events, process_command, Context};
use pimble_app::protocol::{Asked, BackendCommand, BackendEvent, BackendHandle};
use pimble_client::PimbleClient;
use wasm_bindgen_futures::spawn_local;

use super::hub;
use crate::util::sleep_ms;

/// One frame, as the old backend's loop waits.
const IDLE_POLL_MS: i32 = 16;
/// How often the account's stores are looked at again: the desktop link's
/// grant check (`GRANT_CHECK_EVERY`), two minutes.
const ACCOUNT_EVERY_MS: f64 = 120_000.0;
/// How long a new connection has to answer before it is given up and made
/// again.
const PROVE_WITHIN_MS: i32 = 15_000;

thread_local! {
    /// The connection, for the pages (sign-in, the account) that talk to the
    /// server outside the command loop.
    static LOCAL: RefCell<Option<Rc<Local>>> = const { RefCell::new(None) };
}

/// Where the connection's events go: the UI's channel and its wake-up.
type Events = (Sender<BackendEvent>, Arc<dyn Fn() + Send + Sync>);

/// The connection to this browser's server: made through the hub, proved
/// with one call, and made again the moment it ends. Every connection made
/// is said to the UI as `Connected`, every one lost as `Disconnected`.
pub struct Local {
    client: futures::lock::Mutex<Option<Arc<PimbleClient>>>,
    events: RefCell<Option<Events>>,
    client_id: String,
    /// How many connections have been made: a new one means the old one's
    /// subscriptions went with it.
    generation: std::cell::Cell<u64>,
    /// Notices said before the UI listened, said when it does.
    waiting_notices: RefCell<Vec<String>>,
    me: Weak<Local>,
}

/// The connection this page holds, made on first use.
pub fn local() -> Rc<Local> {
    LOCAL.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| {
                let client_id = uuid::Uuid::new_v4().to_string();
                Rc::new_cyclic(|me| Local { client: futures::lock::Mutex::new(None), events: RefCell::new(None), client_id, generation: std::cell::Cell::new(0), waiting_notices: RefCell::new(Vec::new()), me: me.clone() })
            })
            .clone()
    })
}

async fn within<T>(ms: i32, fut: impl std::future::Future<Output = T>) -> Option<T> {
    use futures::future::{select, Either};
    match select(Box::pin(fut), Box::pin(sleep_ms(ms))).await {
        Either::Left((value, _)) => Some(value),
        Either::Right(_) => None,
    }
}

impl Local {
    fn emit(&self, event: BackendEvent) {
        if let Some((tx, signal)) = self.events.borrow().as_ref() {
            let _ = tx.try_send(event);
            signal();
        }
    }

    /// A live client: the one held, or a new connection when there is none
    /// or it ended.
    pub async fn client(&self) -> Result<Arc<PimbleClient>, String> {
        let mut held = self.client.lock().await;
        if let Some(client) = held.as_ref() {
            if client.is_connected() {
                return Ok(client.clone());
            }
            tracing::warn!("The connection to this browser's server ended; connecting again");
            *held = None;
            self.emit(BackendEvent::Disconnected);
        }
        let mut wait = 100;
        let client = loop {
            match self.connect().await {
                Ok(client) => break Arc::new(client),
                Err(e) => {
                    tracing::warn!("This browser's server did not answer ({e}); trying again in {wait} ms");
                    sleep_ms(wait).await;
                    wait = (wait * 2).min(5_000);
                }
            }
        };
        *held = Some(client.clone());
        drop(held);
        self.generation.set(self.generation.get() + 1);
        self.watch(client.clone());
        self.hear_cloud(client.clone());
        self.hear_notices(client.clone());
        self.emit(BackendEvent::Connected { server_addr: "this browser".to_string(), client_id: self.client_id.clone() });
        Ok(client)
    }

    /// A sentence for the status bar (`BackendEvent::Notice`), kept until the
    /// UI listens.
    pub fn notice(&self, message: String) {
        if self.events.borrow().is_some() {
            self.emit(BackendEvent::Notice { message });
        } else {
            self.waiting_notices.borrow_mut().push(message);
        }
    }

    /// Let go of the connection held (the hub is replacing the server): the
    /// next call connects again.
    pub async fn reset(&self) {
        if self.client.lock().await.take().is_some() {
            self.emit(BackendEvent::Disconnected);
        }
    }

    /// Say the connection held to a UI that has just started listening.
    async fn announce(&self) {
        let held = self.client.lock().await.clone();
        match held {
            Some(client) if client.is_connected() => {
                self.hear_cloud(client.clone());
                self.hear_notices(client);
                self.emit(BackendEvent::Connected { server_addr: "this browser".to_string(), client_id: self.client_id.clone() });
            }
            _ => {
                let _ = self.client().await;
            }
        }
    }

    async fn connect(&self) -> Result<PimbleClient, String> {
        let port = within(PROVE_WITHIN_MS, hub::connect()).await.ok_or("no server took the connection")??;
        let client = PimbleClient::over_message_port(port);
        match within(PROVE_WITHIN_MS, client.server_info()).await {
            Some(Ok(info)) => {
                tracing::info!("Connected to this browser's server (Pimble {}, {})", info.version, info.host);
                Ok(client)
            }
            Some(Err(e)) => Err(e.to_string()),
            None => Err("the server did not answer".to_string()),
        }
    }

    /// Connect again the moment `client`'s connection ends.
    fn watch(&self, client: Arc<PimbleClient>) {
        let me = self.me.clone();
        spawn_local(async move {
            client.on_disconnect().await;
            if let Some(me) = me.upgrade() {
                let _ = me.client().await;
            }
        });
    }

    /// How this browser's connections to Pimble Cloud are doing, for the
    /// status bar, for as long as `client`'s connection lasts.
    fn hear_cloud(&self, client: Arc<PimbleClient>) {
        let me = self.me.clone();
        spawn_local(async move {
            let Ok(mut sub) = client.subscribe_cloud_connection().await else { return };
            while let Some(Ok(connection)) = sub.next().await {
                let Some(me) = me.upgrade() else { return };
                me.emit(BackendEvent::CloudConnectionChanged { connection });
            }
        });
    }
}

impl Local {
    /// The notices this browser's server receives for the account
    /// (docs/NOTIFICATIONS_CONTRACT.md section 4), for as long as `client`'s
    /// connection lasts.
    fn hear_notices(&self, client: Arc<PimbleClient>) {
        let me = self.me.clone();
        spawn_local(async move {
            let Ok(mut sub) = client.subscribe_notices().await else { return };
            while let Some(Ok(notice)) = sub.next().await {
                let Some(me) = me.upgrade() else { return };
                me.emit(BackendEvent::NoticeReceived { notice });
            }
        });
    }
}

/// The API's way to a live client.
struct LocalConnector(Rc<Local>);

#[async_trait::async_trait(?Send)]
impl Connector for LocalConnector {
    async fn client(&self) -> Result<Arc<PimbleClient>, ApiError> {
        self.0.client().await.map_err(|e| ApiError::Offline { sentence: format!("This browser's Pimble server could not be reached: {e}") })
    }
}

/// Start the backend and hand back the channels the UI talks through.
pub fn spawn() -> BackendHandle {
    let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(100);
    let (event_tx, event_rx) = bounded::<BackendEvent>(1000);
    spawn_local(run(cmd_rx, event_tx));
    BackendHandle { cmd_tx, event_rx }
}

async fn run(cmd_rx: Receiver<BackendCommand>, event_tx: Sender<BackendEvent>) {
    let signal_ui: Arc<dyn Fn() + Send + Sync> = Arc::new(|| rinch_core::run_on_main_thread(pimble_app::events::pump_backend_events));
    let local = local();
    *local.events.borrow_mut() = Some((event_tx.clone(), signal_ui.clone()));
    let client_id = local.client_id.clone();
    tracing::info!("Backend client ID: {client_id} (this browser's own server)");

    // An `Arc` because `pimble_app::commands::Context` (the desktop's too)
    // takes one; in a page nothing crosses a thread.
    #[allow(clippy::arc_with_non_send_sync)]
    let api = Arc::new(Pimble::with_presence_ttl(RpcBackend::new(LocalConnector(local.clone())), client_id.clone(), pimble_rpc::PRESENCE_TTL_MS));

    // The connection (made already by the page that started the app), said
    // to the UI.
    local.announce().await;
    // A notice that came before the UI listened.
    for message in local.waiting_notices.take() {
        let _ = event_tx.try_send(BackendEvent::Notice { message });
        signal_ui();
    }

    // Every change the API hears, after it merged it into what it holds.
    {
        let (api, event_tx, signal_ui, client_id) = (api.clone(), event_tx.clone(), signal_ui.clone(), client_id.clone());
        spawn_local(async move {
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

    // The account's stores, kept here, now and every two minutes.
    {
        let (event_tx, signal_ui) = (event_tx.clone(), signal_ui.clone());
        spawn_local(async move {
            loop {
                if let Ok(client) = self::local().client().await {
                    if let Some(stores) = keep_account_stores(&client).await {
                        let _ = event_tx.try_send(BackendEvent::StoresListed { stores });
                        signal_ui();
                    }
                }
                sleep_ms(ACCOUNT_EVERY_MS as i32).await;
            }
        });
    }

    let plugin_calls = pimble_app::plugin_calls::PluginCalls::default();
    let mut generation = local.generation.get();
    loop {
        let mut ran = false;
        // A new connection (the leader tab changed, its worker was replaced):
        // every store this window watches is subscribed to again there.
        if local.generation.get() != generation {
            generation = local.generation.get();
            api.rewatch().await;
            if let Ok(client) = local.client().await {
                if let Ok(stores) = client.list_stores().await {
                    let _ = event_tx.try_send(BackendEvent::StoresListed { stores });
                    signal_ui();
                }
            }
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            ran = true;
            let Some(cmd) = plugin_calls.take(cmd, &api, &event_tx, &signal_ui) else { continue };
            let host = local.client().await.ok();
            // The stores open side by side, and the account is asked beside
            // them (docs/STARTUP_CONTRACT.md), as on the desktop.
            if matches!(cmd, BackendCommand::OpenStore { .. } | BackendCommand::CloudStatus | BackendCommand::LoadChildren { .. }) {
                let (api, event_tx, signal_ui, client_id) = (api.clone(), event_tx.clone(), signal_ui.clone(), client_id.clone());
                spawn_local(async move {
                    let ctx = Context { api: &api, host, event_tx: &event_tx, signal_ui: &signal_ui, client_id: &client_id };
                    if let Some(event) = process_command(&ctx, cmd).await {
                        let _ = event_tx.try_send(event);
                        signal_ui();
                    }
                });
                continue;
            }
            let ctx = Context { api: &api, host, event_tx: &event_tx, signal_ui: &signal_ui, client_id: &client_id };
            let event = match cmd {
                BackendCommand::Import { store_id, parent_id, format, name, files } => {
                    let mut runner = Runner { ctx: &ctx };
                    Some(pimble_app::import::import(&mut runner, store_id, parent_id, format, &name, &files).await)
                }
                BackendCommand::SendDiagnostics { .. } => Some(BackendEvent::DiagnosticsSent {
                    result: Err("Diagnostics are not sent from the browser's own server yet; the old path (?server=old) sends them.".to_string()),
                }),
                BackendCommand::CreateHostedStore { name, kind } => {
                    let event = create_store(&ctx, &name, &kind).await;
                    Asked::answer(Some(Asked::CreateStore), Some(event))
                }
                cmd => process_command(&ctx, cmd).await,
            };
            if let Some(event) = event {
                let _ = event_tx.try_send(event);
                signal_ui();
            }
        }
        if !ran {
            sleep_ms(IDLE_POLL_MS).await;
        }
    }
}

struct Runner<'a, 'b> {
    ctx: &'a Context<'b>,
}

impl pimble_app::import::Runner for Runner<'_, '_> {
    async fn run(&mut self, cmd: BackendCommand) -> Option<BackendEvent> {
        process_command(self.ctx, cmd).await
    }
}

/// "New Store..." in the browser: a store made in this browser and hosted on
/// Pimble Cloud at once, encrypted, which is what the button has always made
/// here (the person asked for a store on their account; nothing else is
/// uploaded). The desktop's "Host on Pimble Cloud..." is the same call.
async fn create_store(ctx: &Context<'_>, name: &str, kind: &str) -> BackendEvent {
    let Some(client) = ctx.host.as_ref() else { return BackendEvent::Error { message: "Not connected".into() } };
    if kind != "vault" {
        return BackendEvent::Error { message: "The browser makes encrypted stores only.".into() };
    }
    let path = format!("/pimble/replicas/{}.pimble", uuid::Uuid::new_v4());
    let (store_id, _) = match client.create_store(&path, name).await {
        Ok(made) => made,
        Err(e) => return BackendEvent::Error { message: e.to_string() },
    };
    if let Err(e) = client.cloud_host_store(store_id).await {
        return BackendEvent::Error { message: format!("The store was made in this browser but not hosted: {e}") };
    }
    if let Ok(stores) = client.list_stores().await {
        let _ = ctx.event_tx.try_send(BackendEvent::StoresListed { stores });
    }
    BackendEvent::HostedStoreCreated { name: name.to_string(), store_id: Some(store_id) }
}

/// Every store the account is granted, a replica in this browser (section 5):
/// each encrypted store not here yet (or a share of one whose root is not) is
/// added as the desktop's "Add Hosted Store..." adds it. Answers the stores
/// this server has open, for the tree; `None` when nobody is signed in.
/// Taken one tab at a time (a Web Lock), so two tabs never add one store
/// twice.
pub async fn keep_account_stores(client: &PimbleClient) -> Option<Vec<pimble_core::Store>> {
    let _turn = hub::exclusive("pimble-account-stores").await;
    let status = client.cloud_status().await.ok()?;
    if !status.signed_in {
        return None;
    }
    match client.cloud_list_hosted_stores().await {
        Ok(rows) => {
            let open = client.list_stores().await.unwrap_or_default();
            let mut asked: HashSet<String> = HashSet::new();
            for row in &rows {
                if row.kind != "vault" {
                    tracing::info!("{} is a plain hosted store; the browser's own server keeps encrypted stores only for now", row.store_id);
                    continue;
                }
                let Ok(store_id) = pimble_core::StoreId::parse(&row.store_id) else { continue };
                let here = open.iter().find(|s| s.id == store_id);
                let wanted = match here {
                    None => true,
                    Some(store) => row.root.is_some_and(|root| !store.roots.is_empty() && (!store.roots.contains(&root) || store.ended_roots.contains(&root))),
                };
                if !wanted || !asked.insert(row.store_id.clone()) {
                    continue;
                }
                match client.cloud_add_hosted_store(store_id).await {
                    Ok(store) => tracing::info!("{} ({}) is kept in this browser now", store.name, store.id),
                    Err(e) => tracing::info!("{} is not here yet: {e}", row.store_id),
                }
            }
        }
        Err(e) => tracing::info!("The account's stores could not be listed: {e}"),
    }
    client.list_stores().await.ok()
}
