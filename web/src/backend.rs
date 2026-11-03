//! The browser's implementation of the UI's backend seam.
//!
//! The desktop answers the UI's commands on a tokio thread that owns an
//! embedded server; this answers them on the page's own task queue against
//! hosted ones, with the *same* adapter (`pimble_app::commands`): every
//! command that reads or writes a store is a call on the page's
//! `pimble_api::Pimble`, over a `Router` that sends a plain store to its
//! server and an encrypted one to the vault (docs/API_CONTRACT.md wave 5).
//! Everything specific to the browser is here: getting credentials from the
//! accounts service, keeping them fresh, and noticing when a socket dies.
//!
//! **There is no "the" server.** Every command is answered through the endpoint
//! that serves the store it names (`crate::endpoints`): the session's own for
//! every hosted store, and one of its own for each store served from its
//! owner's computer through Pimble Cloud's relay (docs/RELAY_CONTRACT.md). The
//! loop supervises all of them. The session's is the one everything waits for.
//! A relayed store's is down whenever its owner's computer is, for hours or
//! days, so it is tried on a backoff that never holds the loop up, its store
//! is listed from the account's own row of it meanwhile (`owner offline`), and
//! what it serves fills in when it connects ([`supervise_relayed`]).
//!
//! The vault client (`crate::vault`) is the one holder of an encrypted
//! store's documents in this page: the API reads through it
//! (`crate::vault_backend`), and what it decrypts from the store's
//! subscription it pumps to the UI itself. Its subscription, presence,
//! status and closing are answered by it before the API is asked.
//!
//! The command channel is crossbeam's, as on the desktop, because
//! [`pimble_app::BackendHandle`] is the one thing both backends hand the UI.
//! Nothing blocks on it: the loop drains with `try_recv` and yields to the
//! browser between passes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_app::commands::{api_command, change_events, host_command, Context};
use pimble_app::protocol::{BackendCommand, BackendEvent, BackendHandle};
use pimble_client::PimbleClient;
use pimble_core::StoreId;
use wasm_bindgen_futures::spawn_local;

use crate::api::{self, Session, TokenError};
use crate::endpoints::{Endpoints, RECONNECT_MAX_MS, RECONNECT_MIN_MS};
use crate::util::sleep_ms;
use crate::routed;
use crate::vault::{Handled, VaultClient, VaultWire};
use crate::vault_backend::{VaultBackend, WireTable};
use pimble_api::router::BackendId;
use pimble_api::rpc::{Connector, RpcBackend};
use pimble_api::{Affected, ApiError, Pimble, Router};

/// The vault, shared between the loop (connecting, pulling, keys, repairs)
/// and its backend (`crate::vault_backend`), which answers the calls the
/// router sends it. Locked per step, never across a call into the router.
type SharedVault = Arc<futures::lock::Mutex<VaultClient>>;

/// How long the loop sleeps when there is nothing to do. One frame: a command
/// posted by a keystroke is on the wire within about that long, and an idle tab
/// costs the browser sixty near-empty wake-ups a second.
const IDLE_POLL_MS: i32 = 16;

/// How long before a token's `exp` a fresh one is fetched. The contract's five
/// minutes: long enough that a slow network or a sleeping tab still renews in
/// time, short enough that a revoked grant does not linger.
const REFRESH_LEAD_SECS: f64 = 300.0;

/// How long a connection has to survive before it counts as real.
///
/// jsonrpsee's wasm client answers `connect` before the browser has opened the
/// socket, so a refused or immediately-closed connection looks like a success
/// for an instant. Nothing resets the backoff until a connection has both
/// answered an RPC and stayed up this long, which is what stops a broken
/// endpoint being retried hundreds of times a minute.
const SETTLE_MS: f64 = 2_000.0;

thread_local! {
    /// Set by [`request_refresh`], read once per turn of the loop.
    static REFRESH_WANTED: Cell<bool> = const { Cell::new(false) };

    /// What the vault said changed in encrypted stores' trees and is not
    /// shown yet, per store ([`owe`], [`show_owed`]).
    static OWED: RefCell<Vec<(StoreId, Affected)>> = const { RefCell::new(Vec::new()) };
}

/// The store subscriptions this page has asked for.
///
/// A subscription belongs to the socket it was made on, and this backend
/// replaces its socket on every token refresh — about once an hour — as well as
/// after every drop. The UI subscribes to a store when it first registers it
/// and never again (`register_opened_store`, and `StoresListed` registers only
/// stores it does not know), so without this a tab stops hearing anything new
/// after its first hour: for an encrypted store that is every `VaultAppended`,
/// which is every edit anyone else makes. The connection is this module's, so
/// restoring what it carried is too.
#[derive(Default)]
struct Subscriptions {
    wanted: HashSet<StoreId>,
}

impl Subscriptions {
    /// The UI asked for this store's changes.
    fn remember(&mut self, store_id: StoreId) {
        self.wanted.insert(store_id);
    }

    /// Which stores to subscribe to again on a new connection to one
    /// endpoint: the ones asked for that it lists. A store of that endpoint's
    /// that is gone — a grant withdrawn, a store deleted — is forgotten rather
    /// than retried forever. `served_here` says which stores are this
    /// endpoint's to list: a store served somewhere else (from its owner's
    /// computer, which may be off for days) is not gone because this endpoint
    /// does not list it, and is restored when its own connects.
    fn restore(&mut self, listed: &[StoreId], served_here: impl Fn(StoreId) -> bool) -> Vec<StoreId> {
        self.wanted.retain(|id| !served_here(*id) || listed.contains(id));
        listed.iter().copied().filter(|id| self.wanted.contains(id)).collect()
    }
}

/// The connections the router's backends answer through, as the loop last
/// saw its endpoints (refreshed before every routed command): the
/// session's, which serves every hosted store, and one per store served
/// from its owner's computer, absent while that is down. A relayed store
/// never falls back to the session's endpoint, which does not hold it.
#[derive(Clone, Default)]
pub struct Wires(Arc<Mutex<WireState>>);

#[derive(Default)]
struct WireState {
    session: Option<Arc<PimbleClient>>,
    relayed: HashSet<StoreId>,
    stores: HashMap<StoreId, Arc<PimbleClient>>,
}

impl Wires {
    fn refresh(&self, endpoints: &Endpoints) {
        let connected = |c: &Arc<PimbleClient>| c.is_connected();
        let relayed = relayed_stores(endpoints);
        let mut state = self.0.lock().unwrap();
        state.session = endpoints.client_at(&endpoints.session_url()).filter(connected);
        state.stores = relayed.iter().filter_map(|id| Some((*id, endpoints.client_for(*id).filter(connected)?))).collect();
        state.relayed = relayed.into_iter().collect();
    }
}

impl WireTable for Wires {
    fn wire(&self, store: StoreId) -> Option<Arc<dyn VaultWire>> {
        let state = self.0.lock().unwrap();
        let client = if state.relayed.contains(&store) { state.stores.get(&store) } else { state.session.as_ref() };
        client.map(|c| c.clone() as Arc<dyn VaultWire>)
    }
}

/// The plain stores' backend asks for the session's connection; the loop
/// keeps it up, so a call never connects on its own.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl Connector for Wires {
    async fn client(&self) -> Result<Arc<PimbleClient>, ApiError> {
        self.0.lock().unwrap().session.clone().ok_or_else(|| ApiError::Offline { sentence: "Pimble's server is not connected; try again in a moment.".into() })
    }
}

/// The router over the page's two backends: the session's server for plain
/// stores (the default), the vault for encrypted ones.
struct Routes {
    router: Arc<Router>,
    vault_id: BackendId,
    wires: Wires,
    /// The API over the router: what every command that reads or writes a
    /// store is answered by.
    api: Arc<Pimble>,
}

impl Routes {
    /// Keep the router's word about `store` current from what the account
    /// and the loop say of it: which backend serves it, what the account may
    /// change of it, whether its owner is away.
    fn learn(&self, vault: &VaultClient, store: StoreId) {
        if vault.is_encrypted(store) {
            self.router.serve(store, self.vault_id);
        }
        self.router.set_access(store, vault.row_access(store));
        self.router.set_away(store, vault.away_row(store));
    }
}

/// Ask the backend to mint a fresh token and connect again.
///
/// A token carries the grants the account had when it was minted, and
/// `listStores` shows exactly what the token allows. So a store created on the
/// account page is invisible to the app until both are redone — which is what
/// coming back from the account pages, and creating a store from inside the
/// app, both ask for here. The reconnect path already re-lists the stores and
/// re-opens every encrypted one, so this needs nothing of its own.
pub fn request_refresh() {
    REFRESH_WANTED.set(true);
}

/// Start the browser backend and hand back the channels the UI talks through.
///
/// `session` is the credential the page already minted, so the first connection
/// needs no round trip to the accounts service.
pub fn spawn(session: Session) -> BackendHandle {
    let (cmd_tx, cmd_rx) = bounded::<BackendCommand>(100);
    let (event_tx, event_rx) = bounded::<BackendEvent>(1000);

    spawn_local(async move {
        run(session, cmd_rx, event_tx).await;
    });

    BackendHandle { cmd_tx, event_rx }
}

async fn run(
    mut session: Session,
    cmd_rx: Receiver<BackendCommand>,
    event_tx: Sender<BackendEvent>,
) {
    // Everything here runs on the one thread a page has, so this closure is
    // called straight through; it exists because `process_command` takes the
    // desktop's cross-thread signal and the two backends share that signature.
    let signal_ui: Arc<dyn Fn() + Send + Sync> =
        Arc::new(|| rinch_core::run_on_main_thread(pimble_app::events::pump_backend_events));

    let client_id = uuid::Uuid::new_v4().to_string();
    tracing::info!("Backend client ID: {}", client_id);

    let wires = Wires::default();
    let vault_backend = VaultBackend::new(VaultClient::new(client_id.clone()), Arc::new(wires.clone()));
    let shared: SharedVault = vault_backend.shared();
    let mut router = Router::new(RpcBackend::new(wires.clone()));
    let vault_id = router.add(vault_backend);
    let router = Arc::new(router);
    let api = Arc::new(Pimble::with_presence_ttl(router.clone(), client_id.clone(), pimble_rpc::PRESENCE_TTL_MS));
    let routes = Routes { router, vault_id, wires, api };

    // Every change the API hears of a plain store, after it merged it into
    // what it holds. (An encrypted store's are the vault's to pump.)
    {
        let (api, event_tx, signal_ui, client_id) = (routes.api.clone(), event_tx.clone(), signal_ui.clone(), client_id.clone());
        spawn_local(async move {
            let mut last = None;
            while let Some(change) = api.next_change().await {
                for event in change_events(&api, change, &client_id, &mut last).await {
                    if let Err(e) = event_tx.try_send(event) {
                        tracing::warn!("Event channel full, dropped: {}", e);
                    }
                }
                signal_ui();
            }
        });
    }

    let mut endpoints = Endpoints::from_session(&session);
    shared.lock().await.set_relayed(relayed_stores(&endpoints));
    let mut subscriptions = Subscriptions::default();

    loop {
        // 1. Keep the credentials ahead of their expiry, whether or not a
        //    connection needs them yet: a reconnect must not have to wait for a
        //    round trip to the accounts service, and a grant removed upstream
        //    should stop applying at the next token rather than the next hour.
        let refresh_wanted = REFRESH_WANTED.replace(false);
        if refresh_wanted {
            // Reconnecting is what re-lists the stores and re-opens the
            // encrypted ones, and it must do so with a token that carries any
            // grant added since.
            endpoints.disconnect_all();
        }

        if refresh_wanted || expiring_soon(&session) {
            match api::fetch_token().await {
                Ok(fresh) => {
                    tracing::debug!("Refreshed the access token");
                    session = fresh;
                    // Every store's endpoint and token again: a share
                    // accepted since is a new endpoint to keep up, one given
                    // up is an endpoint let go.
                    endpoints.adopt(&session);
                    shared.lock().await.set_relayed(relayed_stores(&endpoints));
                }
                Err(TokenError::Unauthorized) => {
                    tracing::warn!("The session is gone; going back to the login page");
                    api::go_to_login();
                    return;
                }
                Err(e) => {
                    // Not fatal while the current token is still good.
                    tracing::warn!("Could not refresh the access token: {}", e);
                }
            }
        }

        // Steps 2 to 3c work on the vault directly; the lock is let go
        // before any command is routed (step 4), whose backend takes it.
        let mut ran_anything = {
            let mut vault = shared.lock().await;
            let vault = &mut *vault;

            // 2. Keep the session's endpoint up. It is the one the hosted
            //    stores come from and the one everything else waits for.
            let session_url = endpoints.session_url();
            if !supervise(&mut endpoints, &session_url, vault, &mut subscriptions, &routes, &event_tx, &signal_ui).await {
                continue;
            }

            // 2b. Keep each relayed store's endpoint up too, without ever
            //     waiting on one: a store served from its owner's computer
            //     is listed from its own endpoint, at the start and after
            //     every token refresh, and from the account's row of it
            //     while that endpoint is down.
            let mut ran_anything =
                supervise_relayed(&mut endpoints, vault, &mut subscriptions, &event_tx, &signal_ui).await;

            // 3. Apply whatever the vault subscriptions delivered since the
            //    last pass. The subscription task only forwards raw
            //    notifications; decrypting and merging them needs the vault
            //    client itself, which lives here.
            for event in vault.pump() {
                ran_anything = true;
                emit(&event_tx, &signal_ui, described(vault, event));
            }

            // 3b. Settle any tree whose merged updates have stopped
            //     arriving: the repair is debounced behind them
            //     (`REPAIR_DEBOUNCE_MS` in `crate::vault`), and what it
            //     writes is appended like an edit, through the endpoint that
            //     serves the store.
            for store_id in vault.repairs_due() {
                let Some(client) = endpoints.client_for(store_id).filter(|c| c.is_connected()) else {
                    continue;
                };
                for event in vault.repair(&client, store_id).await {
                    ran_anything = true;
                    emit(&event_tx, &signal_ui, described(vault, event));
                }
            }

            // 3c. Ask again for the key of a share whose owner had not
            //     handed it over yet. Every connection asks too
            //     (`open_listed` runs on each one); this is what makes a tab
            //     left open notice on its own. The same retry serves an open
            //     store that is owed something: the key of a share in a
            //     whole store, or the wraps of a document whose blob would
            //     not open (`KeyLook` in `crate::vault`).
            for store_id in vault.key_retries_due() {
                let Some(client) = endpoints.client_for(store_id).filter(|c| c.is_connected()) else {
                    continue;
                };
                for event in vault.retry_key(&client, store_id, &signal_ui).await {
                    ran_anything = true;
                    emit(&event_tx, &signal_ui, described(vault, event));
                }
            }
            ran_anything
        };
        // 3d. What the vault said changed in a tree, shown now that it is
        //     not locked.
        ran_anything |= show_owed(&shared, &routes, &event_tx, &signal_ui).await;

        // 4. Run whatever the UI has posted. Drain the queue rather than
        //    taking one per pass, so a burst of edits does not spread over as
        //    many frames as it has commands.
        let session_url = endpoints.session_url();
        while let Ok(cmd) = cmd_rx.try_recv() {
            ran_anything = true;
            let mut ctx = Web { endpoints: &mut endpoints, shared: &shared, routes: &routes, event_tx: &event_tx, signal_ui: &signal_ui, client_id: &client_id };
            dispatch(&mut ctx, &mut subscriptions, cmd).await;
            if !endpoints.get(&session_url).is_some_and(|e| e.is_connected()) {
                // Reconnect at the top of the next pass rather than running
                // the rest of the queue into a dead socket.
                break;
            }
        }
        // And what the commands' answers had the vault say.
        ran_anything |= show_owed(&shared, &routes, &event_tx, &signal_ui).await;

        // 5. Give the browser the thread back.
        if !ran_anything {
            sleep_ms(IDLE_POLL_MS).await;
        }
    }
}

/// Keep one endpoint connected, proving each new connection before believing
/// it. Answers whether the loop may carry on this pass.
///
/// The proof is one RPC. `connect` answering `Ok` is not evidence of anything:
/// jsonrpsee's wasm client returns before the browser has opened the socket, so
/// a refused endpoint looks like a success for an instant. `listStores` is that
/// RPC *and* the list the tree wants — there is no "open a store by path" in
/// the browser, the token's grants are the whole list.
async fn supervise(
    endpoints: &mut Endpoints,
    url: &str,
    vault: &mut VaultClient,
    subscriptions: &mut Subscriptions,
    routes: &Routes,
    event_tx: &Sender<BackendEvent>,
    signal_ui: &Arc<dyn Fn() + Send + Sync>,
) -> bool {
    // A connection that has held up for a while is a good one; only then is it
    // safe to forget how long the last outage lasted.
    if let Some(endpoint) = endpoints.get_mut(url) {
        if let Some(since) = endpoint.connected_at {
            if endpoint.is_connected() && now_ms() - since >= SETTLE_MS {
                endpoint.backoff_ms = RECONNECT_MIN_MS;
                endpoint.connected_at = None;
            }
        }
        if endpoint.is_connected() {
            return true;
        }
        if endpoint.client.take().is_some() {
            tracing::warn!("The connection to {} closed", url);
            endpoint.connected_at = None;
            endpoint.reported_failure = true;
            emit(event_tx, signal_ui, BackendEvent::Disconnected);
        }
    } else {
        return true;
    }

    let auth = endpoints.get(url).map(|e| e.auth()).expect("the endpoint exists");
    let attempt = PimbleClient::connect_with_auth(url, &auth).await;

    let proven = match attempt {
        Ok(c) => match c.list_stores().await {
            Ok(stores) => Ok((Some(Arc::new(c)), stores)),
            Err(e) => Err(e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    };

    match proven {
        Ok((candidate, mut stores)) => {
            // A store closed in this page stays closed until it is loaded
            // again (`VaultClient::close`).
            stores.retain(|store| !vault.is_closed(store.id));
            // Every encrypted store is opened — keys fetched, every document
            // fetched and decrypted — *before* the UI sees the list, so the
            // tree's first `getChildren` has documents to read rather than a
            // store that does not answer yet.
            if let Some(c) = candidate.as_ref() {
                // What the account's grants say about each store, asked again
                // on every connect: a store created since the last one has to
                // be recognised as encrypted before its first `getChildren`,
                // and a share that has arrived since has to be recognised as
                // one before its store is described to the UI. A share that
                // has ended since is said here, before the list that no
                // longer names it: the page drops the folder with a notice,
                // and the store's row with it when it was the last.
                for event in vault.learn_rows().await {
                    emit(event_tx, signal_ui, event);
                }

                // Every subscription this page had belonged to the socket that
                // has just been replaced, so they are all gone and are made
                // again here — the UI subscribes to a store once, when it first
                // registers it, and never learns that a connection was lost.
                //
                // Before the catch-up, not after it: an append that lands
                // between the pull and the subscription would be seen by
                // neither (the desktop's vault link says the same thing in
                // `connect_and_sync`). Anything the pull has already applied
                // arrives again and merges to nothing.
                //
                // Only what this endpoint serves: a store served from its
                // owner's computer has a socket of its own, whose
                // subscriptions and catch-up are `supervise_relayed`'s.
                let relayed: HashSet<StoreId> = relayed_stores(endpoints).into_iter().collect();
                let served_here = |store_id: StoreId| !relayed.contains(&store_id);
                vault.forget_subscriptions(served_here);
                let listed_ids: Vec<StoreId> = stores.iter().map(|store| store.id).collect();
                // A plain store's subscription is the API's
                // (`Pimble::rewatch`, below, once the connection is the
                // one its backend asks for).
                for store_id in subscriptions.restore(&listed_ids, served_here) {
                    if !vault.is_encrypted(store_id) {
                        continue;
                    }
                    if let Some(BackendEvent::Error { message }) = vault.subscribe(c, store_id, signal_ui).await {
                        tracing::warn!("Subscribing to {} again failed: {}", store_id, message);
                    }
                }

                // A store already open from before this connection: pull what
                // the server took while the socket was down, and resend what
                // this client could not deliver.
                for event in vault.catch_up(c, served_here).await {
                    emit(event_tx, signal_ui, event);
                }
                for event in vault.open_listed(c, &stores, signal_ui).await {
                    emit(event_tx, signal_ui, event);
                }
                // The root the documents name rather than the placeholder the
                // hosted manifest may carry, and what the account's grants say
                // about a share: its own name, who shared it, its roots and
                // whether they may be written to. After `open_listed`, so a
                // store opened just now is described by the tree it has.
                vault.describe_all(&mut stores);
            }

            if let Some(endpoint) = endpoints.get_mut(url) {
                endpoint.client = candidate;
                endpoint.connected_at = Some(now_ms());
                endpoint.reported_failure = false;
            }
            // Every plain store the page hears again, on this connection.
            routes.wires.refresh(endpoints);
            routes.api.rewatch().await;
            emit(
                event_tx,
                signal_ui,
                BackendEvent::Connected {
                    server_addr: url.to_string(),
                    client_id: routes.api.client_id().to_string(),
                },
            );
            emit(event_tx, signal_ui, BackendEvent::StoresListed { stores });
            true
        }
        Err(message) => {
            let backoff = endpoints.get(url).map(|e| e.backoff_ms).unwrap_or(RECONNECT_MIN_MS);
            tracing::warn!("Connecting to {} failed ({}); retrying in {} ms", url, message, backoff);
            let already_said = endpoints.get(url).is_some_and(|e| e.reported_failure);
            if !already_said {
                if let Some(endpoint) = endpoints.get_mut(url) {
                    endpoint.reported_failure = true;
                }
                emit(
                    event_tx,
                    signal_ui,
                    BackendEvent::Error {
                        message: format!("Failed to connect: {message}"),
                    },
                );
            }
            sleep_ms(backoff).await;
            if let Some(endpoint) = endpoints.get_mut(url) {
                endpoint.backoff_ms = (endpoint.backoff_ms.saturating_mul(2)).min(RECONNECT_MAX_MS);
            }
            false
        }
    }
}

/// Every store the session says is served somewhere other than its own
/// endpoint: from its owner's computer, through Pimble Cloud's relay.
fn relayed_stores(endpoints: &Endpoints) -> Vec<StoreId> {
    endpoints.relay_urls().iter().flat_map(|url| endpoints.stores_at(url)).collect()
}

/// Keep the endpoint of every store served from its owner's computer
/// connected (docs/RELAY_CONTRACT.md, "The apps"), one pass of the loop at a
/// time. Answers whether anything was done.
///
/// The same proof as the session's endpoint, the same restored subscriptions,
/// the same catch-up and the same opening of what it lists, for the stores it
/// serves and no others. Two things differ. A failed attempt is never sat
/// out: the owner's computer being off is this endpoint's ordinary state for
/// hours or days, so the next attempt is a time to come back at
/// (`Endpoint::next_attempt_at`), and the loop carries on with everything
/// else. And being down is said where the person looks for the store, not in
/// the status bar: its row comes from the account's own list with `owner
/// offline` ([`VaultClient::endpoint_down`]) until the endpoint answers, when
/// it is announced as the store it is and the tree fills in
/// ([`VaultClient::endpoint_up`]). Pimble Cloud has just answered this page
/// (the session's endpoint is up, or this would not run), so "its relay does
/// not reach the store" does mean that.
async fn supervise_relayed(
    endpoints: &mut Endpoints,
    vault: &mut VaultClient,
    subscriptions: &mut Subscriptions,
    event_tx: &Sender<BackendEvent>,
    signal_ui: &Arc<dyn Fn() + Send + Sync>,
) -> bool {
    let mut ran_anything = false;
    for url in endpoints.relay_urls() {
        let served = endpoints.stores_at(&url);
        let Some(endpoint) = endpoints.get_mut(&url) else { continue };

        // A connection that has held up for a while is a good one.
        if let Some(since) = endpoint.connected_at {
            if endpoint.is_connected() && now_ms() - since >= SETTLE_MS {
                endpoint.backoff_ms = RECONNECT_MIN_MS;
                endpoint.connected_at = None;
            }
        }
        if endpoint.is_connected() {
            continue;
        }
        if endpoint.client.take().is_some() {
            // Tried again at once, and only a failed attempt says `owner
            // offline`: the relay also closes a socket whose token has run
            // out, about once an hour, and that is not the owner going
            // anywhere.
            tracing::info!("The connection to {} closed", url);
            endpoint.connected_at = None;
            endpoint.next_attempt_at = 0.0;
        }
        if now_ms() < endpoint.next_attempt_at {
            continue;
        }

        ran_anything = true;
        let auth = endpoint.auth();
        let proven = match PimbleClient::connect_with_auth(&url, &auth).await {
            Ok(c) => match c.list_stores().await {
                Ok(stores) => Ok((Some(Arc::new(c)), stores)),
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.to_string()),
        };

        match proven {
            Ok((candidate, mut stores)) => {
                // Only the stores the session named at this endpoint: what
                // else it may list is not this page's to take from it.
                stores.retain(|store| served.contains(&store.id) && !vault.is_closed(store.id));
                if let Some(c) = candidate.as_ref() {
                    // A share accepted since the session's endpoint last
                    // connected is in the account's list and not yet here
                    // (and one that ended since is said, as above).
                    for event in vault.learn_rows().await {
                        emit(event_tx, signal_ui, event);
                    }
                    let served_here = |store_id: StoreId| served.contains(&store_id);
                    vault.forget_subscriptions(served_here);
                    let listed_ids: Vec<StoreId> = stores.iter().map(|store| store.id).collect();
                    for store_id in subscriptions.restore(&listed_ids, served_here) {
                        if let Some(BackendEvent::Error { message }) = vault.subscribe(c, store_id, signal_ui).await {
                            tracing::warn!("Subscribing to {} again failed: {}", store_id, message);
                        }
                    }
                    for event in vault.catch_up(c, served_here).await {
                        emit(event_tx, signal_ui, event);
                    }
                    for event in vault.open_listed(c, &stores, signal_ui).await {
                        emit(event_tx, signal_ui, event);
                    }
                }
                if let Some(endpoint) = endpoints.get_mut(&url) {
                    endpoint.client = candidate;
                    endpoint.connected_at = Some(now_ms());
                    endpoint.reported_failure = false;
                    endpoint.next_attempt_at = 0.0;
                }
                // Back from `owner offline`: the store it is, so the tree
                // fetches it. Then the list, for a store the UI has never
                // heard of (a store it knows is left as it is).
                let back = vault.endpoint_up(&served, &stores);
                for event in back {
                    emit(event_tx, signal_ui, event);
                }
                vault.describe_all(&mut stores);
                emit(event_tx, signal_ui, BackendEvent::StoresListed { stores });
            }
            Err(message) => {
                let Some(endpoint) = endpoints.get_mut(&url) else { continue };
                if !endpoint.reported_failure {
                    endpoint.reported_failure = true;
                    tracing::info!("{} does not answer ({}): its owner's computer is offline", url, message);
                } else {
                    tracing::debug!("{} still does not answer ({})", url, message);
                }
                endpoint.next_attempt_at = now_ms() + f64::from(endpoint.backoff_ms);
                endpoint.backoff_ms = endpoint.backoff_ms.saturating_mul(2).min(RECONNECT_MAX_MS);
                for event in vault.endpoint_down(&served) {
                    emit(event_tx, signal_ui, event);
                }
            }
        }
    }
    ran_anything
}

/// What a command is routed with: the endpoints, the vault, the router over
/// the page's backends, and where events go.
struct Web<'a> {
    endpoints: &'a mut Endpoints,
    shared: &'a SharedVault,
    routes: &'a Routes,
    event_tx: &'a Sender<BackendEvent>,
    signal_ui: &'a Arc<dyn Fn() + Send + Sync>,
    client_id: &'a str,
}

impl Web<'_> {
    fn emit(&self, event: BackendEvent) {
        emit(self.event_tx, self.signal_ui, event);
    }
}

/// One command: the account's and the page's own first, then [`route`].
async fn dispatch(ctx: &mut Web<'_>, subscriptions: &mut Subscriptions, cmd: BackendCommand) {
    // What the UI asks to watch, it asks once. The socket it was asked on is
    // replaced on every token refresh, so the ask is remembered and made again
    // on each new connection.
    if let BackendCommand::SubscribeStoreChanges { store_id } = &cmd {
        subscriptions.remember(*store_id);
    }

    // Creating a hosted store is the accounts service's business, not any
    // Pimble server's, so it never reaches one.
    if let BackendCommand::CreateHostedStore { name, kind } = &cmd {
        let event = create_hosted_store(name, kind).await;
        if let Some(event) = pimble_app::protocol::Asked::answer(Some(pimble_app::protocol::Asked::CreateStore), Some(event)) {
            ctx.emit(event);
        }
        return;
    }

    // Names are the account's (docs/PRESENCE_CONTRACT.md section 3): the
    // accounts service answers them, no Pimble server.
    if let Some(event) = names_command(&cmd).await {
        ctx.emit(event);
        return;
    }

    // The personal document is the account's, not a store's: this page
    // seals and syncs it itself (docs/DISCUSSIONS_CONTRACT.md section 8).
    if let BackendCommand::SyncPersonal { state, enabled, turn_on } = cmd {
        match crate::personal_sync::sync(&state, enabled, turn_on).await {
            Ok((state, off)) => ctx.emit(BackendEvent::PersonalSynced { state, off }),
            Err(e) => tracing::debug!("personal document not synced: {e}"),
        }
        return;
    }

    // An import is many commands, each routed exactly as the UI's own would
    // be (docs/IMPORT_CONTRACT.md).
    if let BackendCommand::Import { store_id, parent_id, format, name, files } = cmd {
        let event = pimble_app::import::import(ctx, store_id, parent_id, format, &name, &files).await;
        ctx.emit(event);
        return;
    }

    if let Some(event) = route(ctx, cmd).await {
        let event = described(&*ctx.shared.lock().await, event);
        ctx.emit(event);
    }
}

impl pimble_app::import::Runner for Web<'_> {
    async fn run(&mut self, cmd: BackendCommand) -> Option<BackendEvent> {
        route(self, cmd).await
    }
}

/// The stores a command names: a transplant's two, a search's, or the one
/// every other command that names a store names.
fn stores_of(cmd: &BackendCommand) -> Vec<StoreId> {
    match cmd {
        BackendCommand::TransplantNode { from_store_id, to_store_id, .. } => vec![*from_store_id, *to_store_id],
        BackendCommand::Search { stores, .. } => stores.clone(),
        other => crate::vault::store_id_of(other).into_iter().collect(),
    }
}

/// Answer a command: through the router (`crate::routed`) where the API
/// covers it, otherwise from the vault client for an encrypted store and
/// from the store's server for a plain one. What happens on the way (an
/// endpoint found down) is emitted; the answer is returned.
async fn route(ctx: &mut Web<'_>, cmd: BackendCommand) -> Option<BackendEvent> {
    // A failure of a request a modal waits on names it.
    let asked = pimble_app::protocol::Asked::of(&cmd);
    let answer = route_one(ctx, cmd).await;
    pimble_app::protocol::Asked::answer(asked, answer)
}

async fn route_one(ctx: &mut Web<'_>, cmd: BackendCommand) -> Option<BackendEvent> {
    // Which server answers this command is decided by the store it names, not
    // by which connection happens to be open.
    let store_id = crate::vault::store_id_of(&cmd);
    let stores = stores_of(&cmd);

    let root = {
        let mut vault = ctx.shared.lock().await;
        // A store served from its owner's computer whose socket has died
        // since the supervisor last looked: down, as of now. The supervisor
        // tries it again on its next pass and says so if it is back.
        for &id in &stores {
            if ctx.endpoints.is_relayed(id) && !ctx.endpoints.client_for(id).is_some_and(|c| c.is_connected()) {
                for event in vault.endpoint_down(&[id]) {
                    ctx.emit(event);
                }
            }
        }
        // The router's word about each store, from the account's grants and
        // the loop's view of its endpoint.
        for &id in &stores {
            ctx.routes.learn(&vault, id);
        }
        store_id.and_then(|id| vault.root_of(id))
    };

    // A reader's write is refused here, before anything is asked of any
    // server: the answer would be this same sentence, and it is the same one
    // whether the store is encrypted or plain
    // (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Roles"). A transplant's two
    // stores are gated by the router itself.
    if !matches!(cmd, BackendCommand::TransplantNode { .. }) {
        if let Some(event) = store_id.and_then(|id| routed::gate(&ctx.routes.router, id, &cmd)) {
            return Some(event);
        }
    }

    // The endpoints the command's stores are served by, connected (a
    // relayed store's is the supervisor's to connect, on its backoff: never
    // once per command), and the router's backends told which are up.
    for &id in &stores {
        if ctx.endpoints.is_relayed(id) {
            continue;
        }
        let url = ctx.endpoints.url_for(id);
        if !ctx.endpoints.client_at(&url).is_some_and(|c| c.is_connected()) {
            if let Err(message) = ctx.endpoints.ensure_connected(&url).await {
                tracing::warn!("Could not reach {}: {}", url, message);
            }
        }
    }
    ctx.routes.wires.refresh(ctx.endpoints);

    // A store whose owner's computer is off is answered from what this page
    // holds of it, which may be nothing: there is no connection to ask, and
    // that is no error (docs/RELAY_CONTRACT.md, "The apps").
    let cmd = match ctx.shared.lock().await.handle_unreachable(cmd) {
        Handled::Yes(event) => return event,
        Handled::No(cmd) => cmd,
    };

    let url = match store_id {
        Some(store_id) => ctx.endpoints.url_for(store_id),
        None => ctx.endpoints.session_url(),
    };
    let live = match store_id {
        Some(store_id) => ctx.endpoints.client_for(store_id),
        None => ctx.endpoints.client_at(&url),
    };
    let client = match live.filter(|c| c.is_connected()) {
        Some(client) => Some(client),
        None if store_id.is_some_and(|id| ctx.endpoints.is_relayed(id)) => None,
        None => match ctx.endpoints.ensure_connected(&url).await {
            Ok(client) => Some(client),
            Err(message) => {
                tracing::warn!("Could not reach {}: {}", url, message);
                None
            }
        },
    };

    // What an encrypted store answers itself: its subscription, presence
    // (sealed under the document's key), its status, closing it.
    let owned = store_id.is_some_and(|id| ctx.routes.router.served_by(id) == ctx.routes.vault_id);
    let cmd = match (cmd, client.as_ref()) {
        (BackendCommand::PublishPresence { store_id, node_id, session, focused, left, text, board }, Some(c)) if owned => {
            let frame = pimble_app::commands::presence_frame(&ctx.routes.api, store_id, node_id, &session, focused, left, text, board);
            ctx.shared.lock().await.publish_presence(c, frame).await;
            return None;
        }
        (cmd, Some(c)) => match ctx.shared.lock().await.handle(c, cmd, ctx.signal_ui).await {
            Handled::Yes(event) => return event,
            Handled::No(cmd) => cmd,
        },
        (cmd, None) => cmd,
    };

    // A node made with no parent goes under the root this page's own
    // documents name (an encrypted store's listing may carry a placeholder).
    let cmd = match cmd {
        BackendCommand::CreateNode { store_id, parent_id: None, title, init, place } if root.is_some() => {
            BackendCommand::CreateNode { store_id, parent_id: root, title, init, place }
        }
        cmd => cmd,
    };

    // Every read and write of a store: the API, through the router.
    let context = Context { api: &ctx.routes.api, host: client, event_tx: ctx.event_tx, signal_ui: ctx.signal_ui, client_id: ctx.client_id };
    let cmd = match api_command(&context, cmd).await {
        Ok(event) => return event,
        Err(cmd) => cmd,
    };

    // What is left is the server's operator's, which an encrypted store and
    // a store whose owner is away cannot be asked.
    let cmd = match ctx.shared.lock().await.refuse_unreachable(cmd) {
        Handled::Yes(event) => return event,
        Handled::No(cmd) => cmd,
    };
    let cmd = match ctx.shared.lock().await.refuse(cmd) {
        Handled::Yes(event) => return event,
        Handled::No(cmd) => cmd,
    };
    host_command(&context, cmd).await
}

/// Every `Store` the UI is handed names, for an encrypted store this page
/// has decrypted, the root its own documents name (`VaultClient::describe`).
/// The store list is described where it is built, in `supervise`; this is the
/// same for any other answer that carries a store.
fn described(vault: &VaultClient, event: BackendEvent) -> BackendEvent {
    match event {
        BackendEvent::StoreOpened { mut store } => {
            vault.describe(&mut store);
            BackendEvent::StoreOpened { store }
        }
        BackendEvent::StoresListed { mut stores } => {
            vault.describe_all(&mut stores);
            BackendEvent::StoresListed { stores }
        }
        other => other,
    }
}

/// Make a store on the account this session belongs to.
///
/// Two steps that must both happen for an encrypted store: the accounts service
/// records it, and this page mints its key and seals it to the creator. A vault
/// store with no key envelope is one nobody can ever open, so a failure of the
/// second is a failure of the whole thing.
///
/// The new grant is only in a token minted after it, so this asks for a fresh
/// one and a reconnect; the store appears when `listStores` next answers.
/// The Shares dialog's commands and `GetPeople`, answered from the accounts
/// service under the events the desktop's server answers them with. `None`
/// for any other command.
async fn names_command(cmd: &BackendCommand) -> Option<BackendEvent> {
    use pimble_app::protocol::CloudOp;
    let failed = |message: String| BackendEvent::CloudError { op: CloudOp::Shares, message };
    match cmd {
        BackendCommand::CloudListShares => Some(shares_listed().await.unwrap_or_else(failed)),
        BackendCommand::CloudSetDisplayName { name } => Some(match crate::accounts::put_display_name(name.trim()).await {
            Ok(()) => shares_listed().await.unwrap_or_else(failed),
            Err(e) => failed(e.message),
        }),
        BackendCommand::CloudSetShareName { store_id, root, name } => {
            let root = root.map(|r| r.to_string());
            Some(match crate::accounts::put_share_name(&store_id.to_string(), root.as_deref(), name.trim()).await {
                Ok(()) => shares_listed().await.unwrap_or_else(failed),
                Err(e) => failed(e.message),
            })
        }
        BackendCommand::GetPeople { store_id } => match people_of(*store_id).await {
            Ok(people) => Some(BackendEvent::PeopleLoaded { store_id: *store_id, people }),
            Err(message) => {
                tracing::debug!("people of store {store_id} not loaded: {message}");
                None
            }
        },
        _ => None,
    }
}

/// `CloudSharesListed`. Every store this page's account holds is in its
/// explorer, so every share is "here".
async fn shares_listed() -> Result<BackendEvent, String> {
    let me = crate::accounts::me().await.map_err(|e| e.message)?;
    let shares = crate::accounts::list_shares().await.map_err(|e| e.message)?;
    let default_name = if me.name.is_empty() { me.email.split('@').next().unwrap_or_default().to_string() } else { me.name };
    let shares = shares
        .into_iter()
        .filter_map(|row| {
            Some(pimble_rpc::CloudShareRow {
                store_id: StoreId::parse(&row.store_id).ok()?,
                root: row.root.as_deref().and_then(|r| pimble_core::NodeId::parse(r).ok()),
                relayed: row.tier == "relay",
                name: row.name,
                role: row.role,
                owned: row.owned,
                owner_name: row.owner_name,
                my_name: row.my_name,
                appears_as: row.appears_as,
                here: true,
            })
        })
        .collect();
    Ok(BackendEvent::CloudSharesListed { shares: pimble_rpc::CloudListSharesResponse { display_name: me.display_name, default_name, shares } })
}

/// Who is who in one store's shares, asked of the accounts service now.
async fn people_of(store_id: StoreId) -> Result<pimble_rpc::StorePeople, String> {
    let id = store_id.to_string();
    let shares = crate::accounts::list_shares().await.map_err(|e| e.message)?;
    let mut scopes = Vec::new();
    for share in shares.into_iter().filter(|share| share.store_id == id) {
        let listing = crate::accounts::list_members(&id, share.root.as_deref()).await.map_err(|e| e.message)?;
        scopes.push(pimble_rpc::ScopePeople {
            root: share.root.as_deref().and_then(|r| pimble_core::NodeId::parse(r).ok()),
            people: listing.people.into_iter().map(|p| pimble_rpc::Person { user_id: p.user_id, name: p.name }).collect(),
            my_name: share.appears_as,
            my_name_chosen: !share.my_name.trim().is_empty(),
            share_name: share.name.clone(),
        });
    }
    Ok(pimble_rpc::StorePeople { scopes })
}

async fn create_hosted_store(name: &str, kind: &str) -> BackendEvent {
    if kind == "vault" && !crate::session::is_unlocked() {
        return BackendEvent::Error {
            message: "Unlock your account before making an encrypted store.".to_string(),
        };
    }

    let created = match crate::accounts::create_store(name, kind, None).await {
        Ok(created) => created,
        Err(e) => return BackendEvent::Error { message: e.message },
    };

    if kind == "vault" {
        if let Err(message) = crate::keys::mint_store_key(&created.store_id).await {
            return BackendEvent::Error {
                message: format!("The store was created but its key could not be stored: {message}"),
            };
        }
    }

    request_refresh();
    BackendEvent::HostedStoreCreated { name: created.name, store_id: StoreId::parse(&created.store_id).ok() }
}

fn emit(event_tx: &Sender<BackendEvent>, signal_ui: &Arc<dyn Fn() + Send + Sync>, event: BackendEvent) {
    owe(&event);
    if let Err(e) = event_tx.try_send(event) {
        tracing::warn!("Event channel full, dropped: {}", e);
    }
    signal_ui();
}

/// Note what an event from the vault says changed in an encrypted store's
/// tree (the kinds the server would derive, a document's content), to be
/// shown once the vault is not locked: the view reads it through the API,
/// whose backend for that store is the vault itself ([`show_owed`]).
fn owe(event: &BackendEvent) {
    let (store, affected) = match event {
        BackendEvent::RemoteStoreChange { store_id, change_kind, .. } => (*store_id, Affected::of(change_kind)),
        BackendEvent::NodeContentUpdated { store_id, node_id } => (*store_id, Affected::rows(vec![*node_id])),
        _ => return,
    };
    if affected.is_empty() {
        return;
    }
    OWED.with(|owed| {
        let mut owed = owed.borrow_mut();
        match owed.iter_mut().find(|(s, _)| *s == store) {
            Some((_, all)) => *all = std::mem::take(all).and(affected),
            None => owed.push((store, affected)),
        }
    });
}

/// Show what the vault said changed since the last time (see [`owe`]): an
/// encrypted store's tree is updated from the same `Shown` a plain store's
/// is, read from the one holder of its documents. Answers whether there was
/// anything to show.
async fn show_owed(shared: &SharedVault, routes: &Routes, event_tx: &Sender<BackendEvent>, signal_ui: &Arc<dyn Fn() + Send + Sync>) -> bool {
    let owed = OWED.with(|owed| std::mem::take(&mut *owed.borrow_mut()));
    if owed.is_empty() {
        return false;
    }
    {
        // The router reads an encrypted store from the vault only once it
        // knows the store is one.
        let vault = shared.lock().await;
        for (store, _) in &owed {
            routes.learn(&vault, *store);
        }
    }
    for (store, affected) in owed {
        if let Some(event) = pimble_app::commands::shown(&routes.api, store, &affected).await {
            emit(event_tx, signal_ui, event);
        }
    }
    true
}

/// Whether `session`'s token is inside the refresh window (or already past it).
fn expiring_soon(session: &Session) -> bool {
    (session.exp as f64) - now_ms() / 1000.0 <= REFRESH_LEAD_SECS
}

/// The page's clock, in milliseconds.
fn now_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(test)]
mod tests {
    use super::{StoreId, Subscriptions};

    #[test]
    fn a_new_connection_subscribes_again_to_what_was_asked_for() {
        let a = StoreId::new();
        let b = StoreId::new();
        let mut subscriptions = Subscriptions::default();
        subscriptions.remember(a);
        subscriptions.remember(b);
        // Asking twice is one subscription, not two.
        subscriptions.remember(a);

        let again = subscriptions.restore(&[a, b], |_| true);
        assert_eq!(again, vec![a, b]);
        // And again on the connection after that: a restore does not consume.
        assert_eq!(subscriptions.restore(&[a, b], |_| true), vec![a, b]);
    }

    #[test]
    fn a_store_that_is_no_longer_listed_is_forgotten() {
        let kept = StoreId::new();
        let gone = StoreId::new();
        let mut subscriptions = Subscriptions::default();
        subscriptions.remember(kept);
        subscriptions.remember(gone);

        assert_eq!(subscriptions.restore(&[kept], |_| true), vec![kept]);
        // A grant withdrawn while the socket was down: not retried forever,
        // and not restored if the store comes back without the UI asking.
        assert_eq!(subscriptions.restore(&[kept, gone], |_| true), vec![kept]);
    }

    /// Subscriptions are restored per endpoint (docs/RELAY_CONTRACT.md): a
    /// store served from its owner's computer is not on the session's list,
    /// which is no reason to forget it, and it is subscribed again when its
    /// own endpoint connects, however long that was down.
    #[test]
    fn a_store_served_elsewhere_is_restored_by_its_own_endpoint() {
        let hosted = StoreId::new();
        let relayed = StoreId::new();
        let mut subscriptions = Subscriptions::default();
        subscriptions.remember(hosted);
        subscriptions.remember(relayed);

        // The session's endpoint reconnects, twice, while the owner is away.
        let at_session = |id: StoreId| id != relayed;
        assert_eq!(subscriptions.restore(&[hosted], at_session), vec![hosted]);
        assert_eq!(subscriptions.restore(&[hosted], at_session), vec![hosted]);

        // The owner's computer is back: its endpoint lists the store.
        let at_relay = |id: StoreId| id == relayed;
        assert_eq!(subscriptions.restore(&[relayed], at_relay), vec![relayed]);
        // A relay endpoint that no longer lists it (the share was stopped)
        // forgets it, and the hosted store is none of its business.
        assert!(subscriptions.restore(&[], at_relay).is_empty());
        assert!(subscriptions.restore(&[relayed], at_relay).is_empty());
        assert_eq!(subscriptions.restore(&[hosted], at_session), vec![hosted]);
    }

    #[test]
    fn a_store_listed_but_never_subscribed_to_is_left_alone() {
        // The UI subscribes when it registers a store; one it has not
        // registered yet is not this loop's business.
        let listed = StoreId::new();
        let mut subscriptions = Subscriptions::default();
        assert!(subscriptions.restore(&[listed], |_| true).is_empty());
    }
}
