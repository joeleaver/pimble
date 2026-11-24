//! The connection to the backend: connecting (and everything a connect
//! restarts), disconnecting, and the failures and errors commands answer.

use rinch::prelude::*;

use crate::persistence::load_app_state_file;
use crate::protocol::{Asked, BackendCommand};
use super::show_notice;
use crate::state::{AppStore, ConnectionState};

/// Whether an error is a refused write rather than a failure: one of the two
/// sentences of `StoreAccess`, which the desktop's server sends as the whole
/// `-32004` message and the browser backend answers itself, or any other
/// `Forbidden: ` refusal, whose text is also meant for the person.
pub(super) fn refusal_sentence(message: &str) -> Option<&str> {
    pimble_core::StoreAccess::refusal_in(message).or_else(|| message.strip_prefix("Forbidden: "))
}

/// `BackendEvent::Connected`.
pub(super) fn connected(store: AppStore, server_addr: &String, client_id: &String) {
    tracing::info!("Connected to backend at {} (client_id: {})", server_addr, client_id);
    crate::startup::mark("connected");
    store.connection.set(ConnectionState::Connected);
    store.connection_status.set("Connected".to_string());
    store.server_addr.set(server_addr.clone());
    if !client_id.is_empty() {
        store.client_id.set(client_id.clone());
    }
    // A picture that could not be fetched while there was no
    // connection is asked for again.
    crate::pictures::retry_missing();
    // The link this process was started with, once the stores
    // it names have had a moment to open.
    if let Some(link) = crate::app_links::take_launch_link() {
        rinch::core::reactive::unowned(|| {
            let _ = rinch::prelude::set_timeout(1500, move || crate::app_links::open_app_link(store, &link));
        });
    }

    // Whether the server's keystore holds a signed-in account
    // (a sign-in survives an app restart), for the status bar
    // and the Account modal's face (docs/DESKTOP_ACCOUNT_CONTRACT.md
    // decision 7). Asked first, beside the opens and not behind
    // them (docs/STARTUP_CONTRACT.md): until it answers the
    // status bar says "connecting...".
    store.cloud_answer.set(crate::state::CloudAnswer::Waiting);
    store.send(BackendCommand::CloudStatus);

    // Auto-open previously loaded stores. On a reconnect this is
    // the same list (every open store is persisted there), so
    // the new server ends up holding what the old one held; the
    // tree keeps its store ids and refreshes from `StoreOpened`.
    // Each has its row at once, "opening...", until it opens or
    // says why not; they open side by side.
    let saved_paths = load_app_state_file();
    for path in saved_paths {
        crate::startup::opening();
        tracing::info!("Auto-opening saved store: {}", path);
        store.begin_opening(&path);
        store.send(BackendCommand::OpenStore { path });
    }

    // Documents were being edited across the reconnect, and any
    // delta sent during the gap was lost. Commands run in order,
    // so the stores are open again by the time these execute:
    // every view reconciles (state vector in, diff out, then its
    // diff back), so nothing typed offline is lost and nothing
    // the new server holds is missed. The stores' subscriptions
    // are the API's to make again (`Pimble::watch`, when each
    // store's `StoreOpened` subscribes it).
    crate::doc_session::reconnected(store);
}

/// `BackendEvent::Disconnected`.
pub(super) fn disconnected(store: AppStore) {
    tracing::info!("Disconnected from backend");
    crate::inbox::answers_lost();
    store.connection.set(ConnectionState::Disconnected);
    store.connection_status.set("Disconnected".to_string());
    // The account is asked again on the next connection; until
    // then nothing is known of it, which is not "disconnected".
    store.cloud_answer.set(crate::state::CloudAnswer::Waiting);
}

/// `BackendEvent::Failed`.
pub(super) fn failed(store: AppStore, asked: &Asked, message: &String) {
    // The adapter says which request failed, so its modal gets
    // the sentence (docs/API_CONTRACT.md wave 5).
    tracing::warn!("{asked:?} failed: {message}");
    use crate::protocol::Asked;
    match asked {
        Asked::AddRemoteStore => {
            store.connect_modal_pending_add.set(false);
            store.connect_modal_busy.set(false);
            store.connect_modal_error.set(message.clone());
        }
        Asked::SetStoreSync => {
            store.link_modal_pending.set(false);
            store.link_modal_error.set(message.clone());
        }
        Asked::RemoveReplica => {
            store.remove_replica_modal_pending.set(false);
            store.remove_replica_modal_error.set(message.clone());
        }
        Asked::CreateStore if crate::import::store_failed() => {
            // The store an import was waiting for was not made.
            show_notice(store, format!("The store for the import could not be made: {message}"));
        }
        Asked::CreateStore => {
            store.new_store_modal_pending.set(false);
            store.new_store_modal_error.set(message.clone());
        }
        Asked::CreateMount => {
            store.mount_picker_pending.set(false);
            store.mount_picker_error.set(message.clone());
        }
        Asked::ListDeleted | Asked::UndeleteNode => {
            store.deleted_modal_pending.set(false);
            store.deleted_modal_error.set(message.clone());
        }
    }
}

/// `BackendEvent::Error`.
pub(super) fn error(store: AppStore, message: &String) {
    tracing::error!("Backend error: {}", message);
    crate::inbox::answers_lost();
    if untracked(|| store.deleted_modal_pending.get()) {
        // "Put Back" on a node no list names is an ordinary
        // `MoveNode`, so its failure is not told apart by the
        // request it answers: the open modal waiting on it still
        // claims it (the one guess left).
        store.deleted_modal_pending.set(false);
        store.deleted_modal_error.set(message.clone());
    } else if crate::import::store_failed() {
        // The store an import was waiting for was not made.
        show_notice(store, format!("The store for the import could not be made: {message}"));
    } else if let Some(sentence) = refusal_sentence(message) {
        // The server refused the command; the connection is fine.
        show_notice(store, sentence.to_string());
    } else {
        store.connection.set(ConnectionState::Error(message.clone()));
        store.connection_status.set(format!("Error: {}", message));
    }
}
