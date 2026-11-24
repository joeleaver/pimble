//! The vault client's own tests: fixtures here, one module per topic.

use super::*;
use pimble_core::PimbleUrl;
use pimble_crypto::{blob_aad, dek_aad, unwrap_dek, wrap_dek};
use pimble_rpc::vault::{decode_blob, SNAPSHOT_EVERY};
use pimble_crypto::SymmetricKey;
use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use pimble_client::PimbleClient;
use pimble_core::{node_types, LinkResolution, Place, RelaySide, StoreAccess, StoreKind};
use pimble_crdt::{NodeDoc, TreeEdit};
use pimble_crypto::Blob;
use pimble_rpc::vault::{should_apply, LogSizes};
use pimble_rpc::VaultFetchResponse;
use super::account::EndedShares;
use super::helpers::{now_ms, resolve_in};
use super::shares::as_left_shares;

const T0: &str = "2026-09-18T10:00:00Z";
const T1: &str = "2026-09-18T10:00:01Z";
const T2: &str = "2026-09-18T10:00:02Z";

// ── Fixtures ────────────────────────────────────────────────────────────

/// A keyring holding one scope key, as an owner's page has for a whole
/// store (`root` `None`) or a member's for one share.
fn keyring_for(root: Option<NodeId>) -> Keyring {
    let key_id = KeyId::new_v4();
    Keyring {
        keys: HashMap::from([(key_id, SymmetricKey::generate())]),
        current: Some(key_id),
        scopes: vec![(root, key_id)],
    }
}

fn keyring() -> Keyring {
    keyring_for(None)
}

fn store_keys() -> StoreKeys {
    StoreKeys::new(keyring())
}

fn listed(root: NodeId) -> Store {
    let mut store = Store::new_local("Vault", PathBuf::new());
    store.root_node_id = root;
    store.kind = StoreKind::Vault;
    store
}

/// A peer's tree, whose edits play the part of what another device
/// appended to the hosted logs.
fn origin() -> (Tree, NodeId) {
    let root = NodeId::new();
    (Tree::new(root, "Vault", T0).unwrap(), root)
}

/// The whole state of every document `tree` holds, as one pull: what a
/// fresh page fetches (each document's log collapsed to a snapshot).
fn pull_of(tree: &Tree) -> Vec<Pulled> {
    tree.ids()
        .into_iter()
        .map(|id| Pulled {
            node_id: id,
            entries: vec![(Mark::Through(1), Ok(tree.doc(id).unwrap().save()))],
            head: 1,
        })
        .collect()
}

/// A store page holding a copy of `tree`, as it is after an open.
fn opened(tree: &Tree) -> VaultStore {
    VaultStore::assemble(listed(tree.root()), store_keys(), Vec::new(), pull_of(tree))
}

/// Feed the updates of `edit` to `store` one at a time, as the
/// notifications of one peer's edit arrive; `seq` numbers them.
fn arrive(store: &mut VaultStore, store_id: StoreId, edit: &TreeEdit, seq: &mut u64, live: Option<NodeId>) -> (Vec<BackendEvent>, bool) {
    let mut events = Vec::new();
    let mut structure = false;
    for (id, update) in &edit.touched {
        *seq += 1;
        let merged = store.merge(store_id, *id, vec![(Mark::One(*seq), Ok(update.clone()))], live == Some(*id), None);
        events.extend(merged.events);
        structure |= merged.structure;
    }
    (events, structure)
}

fn kinds_of(events: &[BackendEvent]) -> Vec<StoreChangeKind> {
    events
        .iter()
        .filter_map(|e| match e {
            BackendEvent::RemoteStoreChange { change_kind, .. } => Some(change_kind.clone()),
            _ => None,
        })
        .collect()
}

// ── The backend loop's router, for these tests ──────────────────────────

/// A plain stores' backend with no server behind it: these tests route
/// encrypted stores only.
struct Nowhere;

#[async_trait::async_trait]
impl pimble_api::rpc::Connector for Nowhere {
    async fn client(&self) -> Result<Arc<PimbleClient>, ApiError> {
        Err(ApiError::Offline { sentence: "no server in these tests".into() })
    }
}

/// No endpoint is up: what a page has while its owner's computer is off.
struct NoWires;

impl crate::vault_backend::WireTable for NoWires {
    fn wire(&self, _store: StoreId) -> Option<Arc<dyn VaultWire>> {
        None
    }
}

/// What the backend loop tells the router about a store
/// (`crate::backend::Routes::learn`).
fn learn(router: &pimble_api::Router, vault_id: Option<pimble_api::router::BackendId>, client: &VaultClient, store_id: StoreId) {
    if let Some(id) = vault_id.filter(|_| client.is_encrypted(store_id)) {
        router.serve(store_id, id);
    }
    router.set_access(store_id, client.row_access(store_id));
    router.set_away(store_id, client.away_row(store_id));
}

/// The backend loop's one gate, as it asks it of a command.
fn refuse_write(client: &VaultClient, store_id: StoreId, cmd: &BackendCommand) -> Option<BackendEvent> {
    let router = pimble_api::Router::new(pimble_api::rpc::RpcBackend::new(Nowhere));
    learn(&router, None, client, store_id);
    crate::routed::gate(&router, store_id, cmd)
}

/// A command as the backend loop answers it through the API over the
/// router, with no connection up: the gate, then `api_command` over this
/// page's vault. `None` when the API hands it back. The page comes back
/// to `client`.
fn route_through(client: &mut VaultClient, cmd: BackendCommand) -> Option<Vec<BackendEvent>> {
    let store_id = store_id_of(&cmd).expect("a store's command");
    if let Some(refused) = refuse_write(client, store_id, &cmd) {
        return Some(vec![refused]);
    }
    let root = client.root_of(store_id);
    let cmd = match cmd {
        BackendCommand::CreateNode { store_id, parent_id: None, title, init, place } => BackendCommand::CreateNode { store_id, parent_id: root, title, init, place },
        cmd => cmd,
    };
    let taken = std::mem::replace(client, VaultClient::new(String::new()));
    let backend = crate::vault_backend::VaultBackend::new(taken, Arc::new(NoWires));
    let shared = backend.shared();
    let mut router = pimble_api::Router::new(pimble_api::rpc::RpcBackend::new(Nowhere));
    let vault_id = router.add(backend);
    learn(&router, Some(vault_id), &futures::executor::block_on(shared.lock()), store_id);
    let api = Arc::new(pimble_api::Pimble::new(Arc::new(router), "test"));
    let (event_tx, events) = crossbeam_channel::unbounded();
    let signal_ui: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let answer = {
        let ctx = pimble_app::commands::Context { api: &api, host: None, event_tx: &event_tx, signal_ui: &signal_ui, client_id: "test" };
        futures::executor::block_on(pimble_app::commands::api_command(&ctx, cmd))
    };
    drop(api);
    *client = Arc::try_unwrap(shared).ok().expect("the page has one owner again").into_inner();
    match answer {
        Ok(last) => {
            let mut said: Vec<BackendEvent> = events.try_iter().collect();
            said.extend(last);
            Some(said)
        }
        Err(_) => None,
    }
}

// ── Fixtures more than one topic uses ───────────────────────────────────

/// The same scope keys as `store`'s, for a second page reading what the
/// first one sent.
fn same_keys(store: &VaultStore) -> StoreKeys {
    StoreKeys::new(Keyring { keys: store.keys.scope.keys.clone(), current: store.keys.scope.current, scopes: store.keys.scope.scopes.clone() })
}

fn dek_aad_of(store_id: StoreId, node_id: NodeId) -> Vec<u8> {
    dek_aad(&store_id.to_string(), &VaultDocId::Node(node_id).as_str())
}

/// The documents of `ids` only, as a share's recipient pulls a scope.
fn pull_subset(tree: &Tree, ids: &[NodeId]) -> Vec<Pulled> {
    ids.iter()
        .map(|id| Pulled {
            node_id: *id,
            entries: vec![(Mark::Through(1), Ok(tree.doc(*id).unwrap().save()))],
            head: 1,
        })
        .collect()
}

/// A share's marker on `node`, naming `key_id` as the share key: the edit
/// that wrote it, for a test that plays it to another page.
fn mark_shared(tree: &mut Tree, node: NodeId, key_id: KeyId, name: &str) -> TreeEdit {
    let marker = pimble_core::ShareMarker {
        v: pimble_core::ShareMarker::VERSION,
        key_id,
        url: "https://pimble.app".to_string(),
        name: name.to_string(),
    };
    tree.set_custom(node, pimble_core::custom_keys::SHARE, &serde_json::to_value(&marker).unwrap(), T1).unwrap()
}

/// A scope key with its id, as the accounts service hands one over.
fn scope_key() -> (KeyId, SymmetricKey) {
    (KeyId::new_v4(), SymmetricKey::generate())
}

fn grant(root: Option<NodeId>, role: &str, name: &str) -> AccountGrant {
    AccountGrant { root, role: role.to_string(), name: name.to_string() }
}

fn row(grants: Vec<AccountGrant>, shared_by: Option<&str>) -> AccountStore {
    AccountStore { kind: "vault".to_string(), grants, shared_by: shared_by.map(str::to_string), relayed: false }
}

/// The same row for a store served from its owner's computer.
fn relayed_row(grants: Vec<AccountGrant>, shared_by: Option<&str>) -> AccountStore {
    AccountStore { relayed: true, ..row(grants, shared_by) }
}

mod documents;
mod live;
mod relay;
mod blob_keys;
mod share_keys;
mod shares;
mod moves;
mod reports;
