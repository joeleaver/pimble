//! Helpers: routing a command, time, the page's device id, fetching many logs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use pimble_app::protocol::{BackendCommand, BackendEvent};
use pimble_client::PimbleClient;
use pimble_core::{LinkResolution, NodeId, StoreId};
use pimble_crdt::LinkWalk;
use pimble_rpc::vault as protocol;
use pimble_rpc::{VaultDocId, VaultFetchResponse};

use super::{VaultStore, FETCH_WINDOW};

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Which store a command is about, when it is about one.
///
/// Public because the backend loop needs it before the vault client does: a
/// command is answered through the endpoint that serves its store, whichever
/// kind of store that is.
pub fn store_id_of(cmd: &BackendCommand) -> Option<StoreId> {
    use BackendCommand::*;
    Some(match cmd {
        CloseStore { store_id }
        | CreateNode { store_id, .. }
        | CreateAttachment { store_id, .. }
        | ListAttachments { store_id, .. }
        | ListDiscussions { store_id }
        | GetNode { store_id, .. }
        | Rejudge { store_id, .. }
        | GetChildren { store_id, .. }
        | ApplyUpdate { store_id, .. }
        | RenameNode { store_id, .. }
        | SetNodeAppearance { store_id, .. }
        | GetMetanode { store_id, .. }
        | SetMetanode { store_id, .. }
        | MakeFromRecipe { store_id, .. }
        | DeleteNode { store_id, .. }
        | MoveNode { store_id, .. }
        | MoveNodes { store_id, .. }
        | UndeleteNode { store_id, .. }
        | ListDeleted { store_id }
        | GetAncestors { store_id, .. }
        | GetProperties { store_id, .. }
        | GetReferences { store_id, .. }
        | CreateMount { store_id, .. }
        | GetMountState { store_id, .. }
        | BroadcastChanges { store_id, .. }
        | ReconcileNodeContent { store_id, .. }
        | SubscribeStoreChanges { store_id }
        | RebuildIndex { store_id }
        | SetStoreSync { store_id, .. }
        | GetStoreSync { store_id }
        | PutBlob { store_id, .. }
        | StorePicture { store_id, .. }
        | RemoveReplica { store_id, .. }
        | PublishPresence { store_id, .. }
        | SubscribePresence { store_id } => *store_id,
        GetBlob { url } => url.store,
        MountRemoteStore { target_store_id, .. } => *target_store_id,
        ResolveLink { url, .. } => url.store,
        _ => return None,
    })
}

/// The placeholder root of a store listed from the account's row alone
/// ([`VaultClient::offline_row`]): the store's own id, so it is the same every
/// time and names no node of anybody's.
pub(super) fn offline_root(store_id: StoreId) -> NodeId {
    NodeId(store_id.0)
}

/// Where a link to `node_id` in `store_id` leads, among the encrypted
/// stores this page holds: the same answers, by the same rules, as the
/// server's `resolveLink` (a document a scoped page does not hold is outside
/// its shares, so no access rather than missing).
pub(super) fn resolve_in(stores: &HashMap<StoreId, VaultStore>, store_id: StoreId, node_id: NodeId) -> LinkResolution {
    let mut walk = LinkWalk::new(store_id, node_id);
    while let Some((store_id, node_id)) = walk.next() {
        let Some(store) = stores.get(&store_id) else {
            return LinkResolution::StoreNotHere { store_id, node_id };
        };
        if let Some(answer) = walk.took(store.tree.link_hop(store_id, node_id, store.is_partial())) {
            return answer;
        }
    }
    walk.nowhere()
}

/// A sentence written for the person rather than an error: the app shows it in
/// the status bar and clears it again on a timer
/// (`pimble_app::events::show_notice`, reached by a message it recognises as
/// meant to be read as it is). There is no `BackendEvent` of its own for this
/// yet; when one lands, this is the single place that changes.
pub(super) fn notice(sentence: String) -> BackendEvent {
    BackendEvent::Error { message: format!("Forbidden: {sentence}") }
}

/// A command's name, for the one error a vault store answers with.
pub(super) fn name_of(cmd: &BackendCommand) -> &'static str {
    use BackendCommand::*;
    match cmd {
        CreateMount { .. } => "Mounting",
        GetMountState { .. } => "Mount state",
        SetStoreSync { .. } => "Linking to a remote",
        RemoveReplica { .. } => "Removing a replica",
        CloseStore { .. } => "Closing a store",
        _ => "That",
    }
}

/// The timestamp this page's edits carry (`created_at`, `modified_at`,
/// `deleted_at`), rfc3339 like the server's. The crdt crate never reads a
/// clock; what two replicas write is decided by their callers alone.
#[cfg(target_arch = "wasm32")]
pub(super) fn now_rfc3339() -> String {
    js_sys::Date::new_0().to_iso_string().as_string().unwrap_or_default()
}

/// The same off the browser: the tests run natively, where there is no
/// JavaScript clock to ask.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The page's clock, in milliseconds.
#[cfg(target_arch = "wasm32")]
pub(super) fn now_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn now_ms() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64
}

/// The page's device id for state reports: kept in `localStorage`, so every
/// load of this browser reports into one slot; a fresh one where storage is
/// not to be had.
#[cfg(target_arch = "wasm32")]
pub(super) fn device_id() -> String {
    const KEY: &str = "pimble.vault-device";
    let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
    if let Some(id) = storage.as_ref().and_then(|s| s.get_item(KEY).ok().flatten()).filter(|id| !id.is_empty() && id.len() <= 64) {
        return id;
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(storage) = storage {
        let _ = storage.set_item(KEY, &id);
    }
    id
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn device_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(target_arch = "wasm32")]
fn answered_key(store_id: StoreId) -> String {
    format!("pimble.vault-answered.{store_id}")
}

/// Which reports this browser has answered in `store_id` (see
/// [`VaultStore::answered`]); nothing where storage is not to be had.
#[cfg(target_arch = "wasm32")]
pub(super) fn load_answered(store_id: StoreId) -> protocol::Comparing {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|s| s.get_item(&answered_key(store_id)).ok().flatten())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn load_answered(_store_id: StoreId) -> protocol::Comparing {
    protocol::Comparing::default()
}

#[cfg(target_arch = "wasm32")]
pub(super) fn save_answered(store_id: StoreId, answered: &protocol::Comparing) {
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else { return };
    if answered.is_empty() {
        let _ = storage.remove_item(&answered_key(store_id));
    } else if let Ok(json) = serde_json::to_string(answered) {
        let _ = storage.set_item(&answered_key(store_id), &json);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn save_answered(_store_id: StoreId, _answered: &protocol::Comparing) {}

/// About 120 characters around the match, or the start of the text.
pub(super) fn snippet_around(text: &str, at: Option<usize>) -> String {
    if text.is_empty() {
        return String::new();
    }
    let start = at.unwrap_or(0).saturating_sub(40);
    let start = floor_char_boundary(text, start);
    let end = floor_char_boundary(text, (start + 160).min(text.len()));
    let mut snippet = text[start..end].replace('\n', " ");
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < text.len() {
        snippet.push('…');
    }
    snippet
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// One document's `vaultFetch` answer, or why there is none.
type Fetched = (NodeId, Result<VaultFetchResponse, String>);

/// `vaultFetch` for several documents at once: `(node id, after seq)` in,
/// each answer (or failure) out, in windows of [`FETCH_WINDOW`].
///
/// The browser has one thread and the futures have to be driven together, so
/// each is handed to the page as a promise and the window is `Promise.all`;
/// every promise resolves whatever the fetch did, with the outcome kept
/// aside, so one refused document never fails the window.
pub(super) async fn fetch_many(client: &Arc<PimbleClient>, store_id: StoreId, wanted: Vec<(NodeId, u64)>) -> Vec<Fetched> {
    let results: Rc<RefCell<Vec<Fetched>>> = Rc::new(RefCell::new(Vec::with_capacity(wanted.len())));
    for window in wanted.chunks(FETCH_WINDOW) {
        let promises = js_sys::Array::new();
        for &(node_id, after_seq) in window {
            let client = client.clone();
            let results = results.clone();
            promises.push(&wasm_bindgen_futures::future_to_promise(async move {
                let fetched = client
                    .vault_fetch(store_id, VaultDocId::Node(node_id), after_seq)
                    .await
                    .map_err(|e| e.to_string());
                results.borrow_mut().push((node_id, fetched));
                Ok(wasm_bindgen::JsValue::NULL)
            }));
        }
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::all(&promises)).await;
    }
    let collected = std::mem::take(&mut *results.borrow_mut());
    collected
}

/// `update` is on the server (this client appended it, or fetched it): move
/// the record of what the server holds. It only moves when the update continues
/// from what is already known, so it can lag, never overstate.
pub(super) fn advance(known_sv: &mut Vec<u8>, update: &[u8]) {
    if let Ok(next) = pimble_crdt::advance_state_vector(known_sv, update) {
        *known_sv = next;
    }
}
