//! The browser's implementation of the UI's backend seam.
//!
//! The desktop runs `pimble_app::commands::process_command` on a tokio thread
//! that owns an embedded server; this runs the *same* function on the page's
//! own task queue against a hosted one. Everything specific to the browser is
//! here: getting a credential from the accounts service, keeping it fresh, and
//! noticing when the socket dies.
//!
//! The command channel is crossbeam's, as on the desktop, because
//! [`pimble_app::BackendHandle`] is the one thing both backends hand the UI.
//! Nothing blocks on it: the loop drains with `try_recv` and yields to the
//! browser between passes.

use std::sync::Arc;

use crossbeam_channel::{bounded, Receiver, Sender};
use pimble_app::commands::process_command;
use pimble_app::protocol::{BackendCommand, BackendEvent, BackendHandle};
use pimble_client::PimbleClient;
use pimble_core::AuthMethod;
use wasm_bindgen_futures::{spawn_local, JsFuture};

use crate::api::{self, Session, TokenError};

/// How long the loop sleeps when there is nothing to do. One frame: a command
/// posted by a keystroke is on the wire within about that long, and an idle tab
/// costs the browser sixty near-empty wake-ups a second.
const IDLE_POLL_MS: i32 = 16;

/// How long before a token's `exp` a fresh one is fetched. The contract's five
/// minutes: long enough that a slow network or a sleeping tab still renews in
/// time, short enough that a revoked grant does not linger.
const REFRESH_LEAD_SECS: f64 = 300.0;

/// Backoff bounds for a failed connection attempt.
const RECONNECT_MIN_MS: i32 = 1_000;
const RECONNECT_MAX_MS: i32 = 30_000;

/// How long a connection has to survive before it counts as real.
///
/// jsonrpsee's wasm client answers `connect` before the browser has opened the
/// socket, so a refused or immediately-closed connection looks like a success
/// for an instant. Nothing resets the backoff until a connection has both
/// answered an RPC and stayed up this long, which is what stops a broken
/// endpoint being retried hundreds of times a minute.
const SETTLE_MS: f64 = 2_000.0;

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

    let mut client: Option<Arc<PimbleClient>> = None;
    let mut backoff_ms = RECONNECT_MIN_MS;
    // When the current connection proved itself, so the backoff is only reset
    // once it has lasted.
    let mut connected_at: Option<f64> = None;
    // A run of failed attempts says so once, not once per attempt: the status
    // bar should read "Disconnected" while the server is unreachable, not flap.
    let mut reported_failure = false;

    loop {
        // 1. Keep the credential ahead of its expiry, whether or not the
        //    connection needs it yet: a reconnect must not have to wait for a
        //    round trip to the accounts service, and a grant removed upstream
        //    should stop applying at the next token rather than the next hour.
        if expiring_soon(&session) {
            match api::fetch_token().await {
                Ok(fresh) => {
                    tracing::debug!("Refreshed the access token");
                    session = fresh;
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

        // 2. A connection that has held up for a while is a good one; only
        //    then is it safe to forget how long the last outage lasted.
        if let Some(since) = connected_at {
            if client.as_ref().is_some_and(|c| c.is_connected()) && now_ms() - since >= SETTLE_MS {
                backoff_ms = RECONNECT_MIN_MS;
                connected_at = None;
            }
        }

        // 3. Connect, or notice that the socket has gone and connect again.
        if !client.as_ref().is_some_and(|c| c.is_connected()) {
            if client.take().is_some() {
                tracing::warn!("The connection to the Pimble server closed");
                connected_at = None;
                emit(&event_tx, &signal_ui, BackendEvent::Disconnected);
                reported_failure = true;
            }

            let auth = AuthMethod::Bearer {
                token: session.token.clone(),
            };
            let attempt = PimbleClient::connect_with_auth(&session.rpc_url, &auth).await;

            // `connect` answering `Ok` is not yet evidence of anything. One RPC
            // is: it proves the socket really opened, that whatever sits
            // between here and the server passes WebSocket frames, and that
            // the credential was accepted. `listStores` is that RPC *and* the
            // list the tree wants — there is no "open a store by path" in the
            // browser, the token's grants are the whole list.
            let proven = match attempt {
                Ok(c) => {
                    let mut candidate = Some(Arc::new(c));
                    let answer = process_command(
                        &mut candidate,
                        BackendCommand::ListStores,
                        &event_tx,
                        &signal_ui,
                        &client_id,
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
                    client = candidate;
                    connected_at = Some(now_ms());
                    reported_failure = false;
                    emit(
                        &event_tx,
                        &signal_ui,
                        BackendEvent::Connected {
                            server_addr: session.rpc_url.clone(),
                            client_id: client_id.clone(),
                        },
                    );
                    emit(&event_tx, &signal_ui, listed);
                }
                Err(message) => {
                    tracing::warn!(
                        "Connecting to {} failed ({}); retrying in {} ms",
                        session.rpc_url,
                        message,
                        backoff_ms
                    );
                    // One message per outage, not one per attempt.
                    if !reported_failure {
                        reported_failure = true;
                        emit(
                            &event_tx,
                            &signal_ui,
                            BackendEvent::Error {
                                message: format!("Failed to connect: {}", message),
                            },
                        );
                    }
                    sleep_ms(backoff_ms).await;
                    backoff_ms = (backoff_ms.saturating_mul(2)).min(RECONNECT_MAX_MS);
                    continue;
                }
            }
        }

        // 4. Run whatever the UI has posted. Drain the queue rather than
        //    taking one per pass, so a burst of edits does not spread over as
        //    many frames as it has commands.
        let mut ran_anything = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            ran_anything = true;
            // The desktop watchdog posts this; nothing does here, because the
            // loop checks the socket itself at the top of every pass.
            if matches!(cmd, BackendCommand::ConnectionLost { .. }) {
                continue;
            }
            dispatch(&mut client, cmd, &event_tx, &signal_ui, &client_id).await;
            if client.as_ref().is_some_and(|c| !c.is_connected()) {
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

/// One command, through the shared implementation, with its answer posted.
async fn dispatch(
    client: &mut Option<Arc<PimbleClient>>,
    cmd: BackendCommand,
    event_tx: &Sender<BackendEvent>,
    signal_ui: &Arc<dyn Fn() + Send + Sync>,
    client_id: &str,
) {
    if let Some(event) = process_command(client, cmd, event_tx, signal_ui, client_id).await {
        emit(event_tx, signal_ui, event);
    }
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

/// `setTimeout` as a future, so the loop can yield to the browser.
async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}
