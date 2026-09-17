//! The browser's implementation of the UI's backend seam.
//!
//! The desktop runs `pimble_app::commands::process_command` on a tokio thread
//! that owns an embedded server; this runs the *same* function on the page's
//! own task queue against hosted ones. Everything specific to the browser is
//! here: getting credentials from the accounts service, keeping them fresh, and
//! noticing when a socket dies.
//!
//! **There is no "the" server.** Every command is answered through the endpoint
//! that serves the store it names (`crate::endpoints`), which today is the one
//! the session gave for everything and tomorrow may be a relay holding one
//! shared store. The loop supervises the session's endpoint — the one the store
//! list comes from — and any other endpoint connects the first time a store on
//! it is touched.
//!
//! One thing sits in front of `process_command`: the vault client
//! (`crate::vault`). A command for an encrypted store is answered from the
//! documents this page holds and never reaches the plain RPCs — which the
//! server refuses for such a store anyway. Everything else is unchanged.
//!
//! The command channel is crossbeam's, as on the desktop, because
//! [`pimble_app::BackendHandle`] is the one thing both backends hand the UI.
//! Nothing blocks on it: the loop drains with `try_recv` and yields to the
//! browser between passes.

use std::cell::Cell;
use std::sync::Arc;

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_app::commands::process_command;
use pimble_app::protocol::{BackendCommand, BackendEvent, BackendHandle};
use pimble_client::PimbleClient;
use wasm_bindgen_futures::spawn_local;

use crate::api::{self, Session, TokenError};
use crate::endpoints::{Endpoints, RECONNECT_MAX_MS, RECONNECT_MIN_MS};
use crate::util::sleep_ms;
use crate::vault::{Handled, VaultClient};

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

    let mut vault = VaultClient::new(client_id.clone());
    let mut endpoints = Endpoints::from_session(&session);

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
                    endpoints.adopt(&session);
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

        // 2. Keep the session's endpoint up. It is the one the store list comes
        //    from, so it is the one worth supervising; any other endpoint is
        //    connected on demand by whatever first asks for a store on it.
        let session_url = endpoints.session_url();
        if !supervise(&mut endpoints, &session_url, &mut vault, &event_tx, &signal_ui, &client_id)
            .await
        {
            continue;
        }

        // 3. Apply whatever the vault subscriptions delivered since the last
        //    pass. The subscription task only forwards raw notifications;
        //    decrypting and merging them needs the vault client itself, which
        //    lives here.
        let mut ran_anything = false;
        for event in vault.pump() {
            ran_anything = true;
            emit(&event_tx, &signal_ui, event);
        }

        // 4. Run whatever the UI has posted. Drain the queue rather than
        //    taking one per pass, so a burst of edits does not spread over as
        //    many frames as it has commands.
        while let Ok(cmd) = cmd_rx.try_recv() {
            ran_anything = true;
            // The desktop watchdog posts this; nothing does here, because the
            // loop checks the sockets itself at the top of every pass.
            if matches!(cmd, BackendCommand::ConnectionLost { .. }) {
                continue;
            }
            dispatch(&mut endpoints, &mut vault, cmd, &event_tx, &signal_ui, &client_id).await;
            if !endpoints.get(&session_url).is_some_and(|e| e.is_connected()) {
                // Reconnect at the top of the next pass rather than running
                // the rest of the queue into a dead socket.
                break;
            }
        }

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
    event_tx: &Sender<BackendEvent>,
    signal_ui: &Arc<dyn Fn() + Send + Sync>,
    client_id: &str,
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
        Ok(c) => {
            let mut candidate = Some(Arc::new(c));
            let answer = process_command(
                &mut candidate,
                BackendCommand::ListStores,
                event_tx,
                signal_ui,
                client_id,
            )
            .await;
            match answer {
                Some(BackendEvent::StoresListed { stores }) => {
                    Ok((candidate, BackendEvent::StoresListed { stores }))
                }
                other => Err(match other {
                    Some(BackendEvent::Error { message }) => message,
                    _ => "the server did not answer listStores".to_string(),
                }),
            }
        }
        Err(e) => Err(e.to_string()),
    };

    match proven {
        Ok((candidate, listed)) => {
            // Every encrypted store is opened — keys fetched, tree fetched and
            // decrypted — *before* the UI sees the list, so the tree's first
            // `getChildren` has a store document to read rather than a store
            // that does not answer yet.
            if let (Some(c), BackendEvent::StoresListed { stores }) = (candidate.as_ref(), &listed) {
                // Which stores the account calls encrypted, asked again on
                // every connect: one created since the last one has to be
                // recognised before its first `getChildren`.
                vault.learn_kinds().await;
                // A store already open from before this connection: pull what
                // the server took while the socket was down, and resend what
                // this client could not deliver.
                for event in vault.catch_up(c).await {
                    emit(event_tx, signal_ui, event);
                }
                for problem in vault.open_listed(c, stores).await {
                    emit(
                        event_tx,
                        signal_ui,
                        BackendEvent::Error {
                            message: format!("Could not open an encrypted store ({problem})"),
                        },
                    );
                }
            }

            if let Some(endpoint) = endpoints.get_mut(url) {
                endpoint.client = candidate;
                endpoint.connected_at = Some(now_ms());
                endpoint.reported_failure = false;
            }
            emit(
                event_tx,
                signal_ui,
                BackendEvent::Connected {
                    server_addr: url.to_string(),
                    client_id: client_id.to_string(),
                },
            );
            emit(event_tx, signal_ui, listed);
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

/// One command: the vault client first, then the shared implementation, both
/// against the endpoint that serves the store the command names.
async fn dispatch(
    endpoints: &mut Endpoints,
    vault: &mut VaultClient,
    cmd: BackendCommand,
    event_tx: &Sender<BackendEvent>,
    signal_ui: &Arc<dyn Fn() + Send + Sync>,
    client_id: &str,
) {
    // Creating a hosted store is the accounts service's business, not any
    // Pimble server's, so it never reaches one.
    if let BackendCommand::CreateHostedStore { name, kind } = &cmd {
        let event = create_hosted_store(name, kind).await;
        emit(event_tx, signal_ui, event);
        return;
    }

    // Which server answers this command is decided by the store it names, not
    // by which connection happens to be open.
    let store_id = crate::vault::store_id_of(&cmd);
    let url = match store_id {
        Some(store_id) => endpoints.url_for(store_id),
        None => endpoints.session_url(),
    };

    // An endpoint the supervisor does not drive connects the first time a
    // store on it is touched.
    let live = match store_id {
        Some(store_id) => endpoints.client_for(store_id),
        None => endpoints.client_at(&url),
    };
    let client = match live.filter(|c| c.is_connected()) {
        Some(client) => Some(client),
        None => match endpoints.ensure_connected(&url).await {
            Ok(client) => Some(client),
            Err(message) => {
                tracing::warn!("Could not reach {}: {}", url, message);
                None
            }
        },
    };

    let cmd = match client.as_ref() {
        Some(connected) => match vault.handle(connected, cmd, signal_ui).await {
            Handled::Yes(event) => {
                if let Some(event) = event {
                    emit(event_tx, signal_ui, event);
                }
                return;
            }
            Handled::No(cmd) => cmd,
        },
        None => cmd,
    };

    let mut client = client;
    if let Some(event) = process_command(&mut client, cmd, event_tx, signal_ui, client_id).await {
        emit(event_tx, signal_ui, event);
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
    BackendEvent::HostedStoreCreated { name: created.name }
}

fn emit(event_tx: &Sender<BackendEvent>, signal_ui: &Arc<dyn Fn() + Send + Sync>, event: BackendEvent) {
    if let Err(e) = event_tx.try_send(event) {
        tracing::warn!("Event channel full, dropped: {}", e);
    }
    signal_ui();
}

/// Whether `session`'s token is inside the refresh window (or already past it).
fn expiring_soon(session: &Session) -> bool {
    (session.exp as f64) - now_ms() / 1000.0 <= REFRESH_LEAD_SECS
}

/// The page's clock, in milliseconds.
fn now_ms() -> f64 {
    js_sys::Date::now()
}
