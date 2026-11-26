//! The sharing harness `tests/share.rs` and `tests/share_upkeep.rs` run on
//! (each test file is its own crate, so this is a `tests/common` module):
//! `H`, a real `PimbleServer` in JWT mode holding the hosted `Vault` twins;
//! an axum stub standing in for the accounts service with the scoped
//! members, invitations and keys endpoints (`pimble-cloud`'s README,
//! "Sharing"); a TCP relay in front of `H` a test can cut; real "desktop"
//! `PimbleServer`s, one per device, each with its own temp keystore and
//! replicas directory; and the fixture most tests start from (Alice's hosted
//! store with a folder to share). Three accounts to start with (the owner,
//! two to share with), and a fourth that appears later, for an invitation
//! sent before its address had an account.
//!
//! `tests/vault_link.rs` and `tests/relay.rs` still carry harnesses of
//! their own (an earlier stub without the sharing endpoints, and one with
//! the relay's routes).

// Not every test file uses every helper.
#![allow(dead_code)]

use pimble_host::disk::testfs as tfs;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, StoreId, StoreKind};
use pimble_crdt::NodeDoc;
use pimble_crypto::{derive_password_keys, wrap_account_keys, AccountKeyBlob, AccountKeys, KdfParams, KeyEnvelope};
use pimble_rpc::{EditOperation, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;
use uuid::Uuid;

mod jwt;
mod settle;
mod stub;
mod stub_notices;
pub use jwt::*;
pub use settle::*;
pub use stub::*;
pub use stub_notices::*;

// ── Small generic helpers (mirrors tests/vault_link.rs) ───────────────────

pub async fn node_text(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    match client.get_node(store_id, node_id).await {
        Ok(node) => NodeDoc::text_of(&node.content),
        Err(_) => String::new(),
    }
}

pub async fn node_title(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> String {
    client.get_node(store_id, node_id).await.map(|node| node.metadata.title).unwrap_or_default()
}

pub async fn child_ids(client: &PimbleClient, store_id: StoreId, node_id: NodeId) -> Vec<NodeId> {
    client.get_children(store_id, node_id).await.map(|(_, children)| children.into_iter().map(|n| n.id).collect()).unwrap_or_default()
}

pub async fn rename(client: &PimbleClient, store_id: StoreId, node_id: NodeId, title: &str) {
    client.edit_node(store_id, node_id, "rename", |doc, now| doc.set_title(title, now)).await.expect("rename");
}

pub async fn seed_content(client: &PimbleClient, store_id: StoreId, node_id: NodeId, client_id: &str, text: &str) {
    let doc = NodeDoc::from_plain_text(text).unwrap();
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.save());
    client
        .apply_edit(store_id, node_id, client_id, EditOperation::IncrementalChanges { changes })
        .await
        .expect("seed content applies");
}

/// [`seed_content`] each of `seeds` as a person, then wait for the
/// `modified_at` stamp the server's debounced flush gives each of those nodes
/// (750 ms after the edit, said as the server's own `MetadataUpdated`): what
/// a test that counts appends later needs behind it. Before 2026-11-26 the
/// fixtures slept a second for it, which a loaded machine outran.
pub async fn seed_and_wait_stamped(client: &PimbleClient, store_id: StoreId, seeds: &[(NodeId, &str)]) {
    let mut changes = client.subscribe_store_changes(store_id).await.expect("subscribing to the store's changes");
    for (node_id, text) in seeds {
        seed_content(client, store_id, *node_id, "seed", text).await;
    }
    let nodes: Vec<NodeId> = seeds.iter().map(|(node_id, _)| *node_id).collect();
    wait_stamped(&mut changes, &nodes).await;
}

/// Put the picture `url` on `node_id`'s canvas the way the app does (an
/// element of kind picture in `data.board`), so it names the blob as a
/// canvas does and not as text does.
#[allow(dead_code)]
pub async fn put_on_canvas(client: &PimbleClient, store_id: StoreId, node_id: NodeId, url: &pimble_core::BlobUrl) {
    use pimble_board::{Board, Geom, Kind, NewElement, PictureGeom, Rect, Stamp};
    let node = client.get_node(store_id, node_id).await.expect("the node is there");
    let doc = NodeDoc::load(&node.content).expect("its document loads");
    let before = doc.state_vector();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let board = Board::of_node(&doc);
    if !board.exists() {
        board.create(Stamp::new(None, &now)).expect("a canvas");
    }
    let geom = Geom::Picture(PictureGeom { rect: Rect { x: 0.0, y: 0.0, w: 100.0, h: 80.0 }, src: url.to_string(), px_w: 100, px_h: 80, crop: None });
    board.add_element(Stamp::new(None, &now), NewElement::new(Kind::Picture, geom)).expect("a picture on it");
    let changes = base64::engine::general_purpose::STANDARD.encode(doc.diff_since(&before).unwrap());
    client.apply_edit(store_id, node_id, "canvas", EditOperation::IncrementalChanges { changes }).await.expect("the canvas edit applies");
}

/// How often a test device's key sweep runs, in place of the minute.
pub const SWEEP_EVERY: Duration = Duration::from_secs(1);

pub fn device_config(dir: &Path) -> ServerConfig {
    ServerConfig { share_sweep_interval: Some(SWEEP_EVERY), ..ServerConfig::in_dir(dir) }
}

/// A fresh desktop-side `PimbleServer` (plain, tokenless, loopback) with
/// its own temp keystore/credentials/replicas paths.
pub async fn start_local_server() -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (server, client) = start_device_in(dir.path()).await;
    (server, client, dir)
}

/// A device started (or started again) over `dir`.
pub async fn start_device_in(dir: &Path) -> (PimbleServer, PimbleClient) {
    let mut server = PimbleServer::with_config(device_config(dir));
    server.start().await.expect("local server starts");
    let client = PimbleClient::connect(format!("http://{}", server.addr())).await.expect("client connects");
    (server, client)
}

pub const ALICE: (&str, &str) = ("alice@example.com", "correct horse battery staple");
pub const BOB: (&str, &str) = ("bob@example.com", "a different horse entirely");
pub const CAROL: (&str, &str) = ("carol@example.com", "a third horse altogether");
/// Has no account until a test says so.
pub const DAVE: (&str, &str) = ("dave@example.com", "late to the stable");

pub struct Env {
    pub stub_url: String,
    pub stub: StubState,
    // Kept alive for the test's duration (dropping it would stop H).
    pub _h: PimbleServer,
    /// A client to `H` with the service token, for out-of-band verification.
    pub h_admin: PimbleClient,
    pub h_stores_dir: PathBuf,
    pub _h_dir: tempfile::TempDir,
    /// Every device's vault link reaches `H` through this, so a test can
    /// take the network away and give it back.
    pub relay: Relay,
}

/// A TCP relay in front of `H` that a test can cut.
#[derive(Clone)]
pub struct Relay {
    pub addr: std::net::SocketAddr,
    pub blocked: Arc<AtomicBool>,
    pub conns: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl Relay {
    pub async fn start(target: std::net::SocketAddr) -> Relay {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = Relay { addr: listener.local_addr().unwrap(), blocked: Arc::new(AtomicBool::new(false)), conns: Arc::new(Mutex::new(Vec::new())) };
        let this = relay.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else { break };
                if this.blocked.load(Ordering::SeqCst) {
                    drop(inbound);
                    continue;
                }
                let task = tokio::spawn(async move {
                    if let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                this.conns.lock().unwrap().push(task.abort_handle());
            }
        });
        relay
    }

    pub fn cut(&self) {
        self.blocked.store(true, Ordering::SeqCst);
        for conn in self.conns.lock().unwrap().drain(..) {
            conn.abort();
        }
    }

    pub fn restore(&self) {
        self.blocked.store(false, Ordering::SeqCst);
    }
}

impl Env {
    pub fn h_store_path(&self, store_id: StoreId) -> PathBuf {
        self.h_stores_dir.join(format!("{store_id}.pimble"))
    }

    pub fn hosted_store_contains_plaintext(&self, store_id: StoreId, needle: &str) -> bool {
        contains_bytes_recursive(&self.h_store_path(store_id).join("vault"), needle.as_bytes())
    }

    #[allow(dead_code)]
    pub fn hosted_store_contains_plaintext_bytes(&self, store_id: StoreId, needle: &[u8]) -> bool {
        contains_bytes_recursive(&self.h_store_path(store_id), needle)
    }

    pub async fn sign_in(&self, client: &PimbleClient, who: (&str, &str)) {
        client.cloud_sign_in(&self.stub_url, who.0, who.1).await.expect("sign in");
    }

    /// The address gets an account (verified, with keys), and claims the
    /// invitations that were waiting for it.
    pub fn account_appears(&self, who: (&str, &str)) {
        let account = stub_account(who.0, who.1);
        let mut s = self.stub.inner.lock().unwrap();
        s.accounts.push(account);
        s.claim_invitations(who.0);
    }

    /// Whether `email`'s account holds an envelope for the share rooted at `root`.
    /// The account id of `email`.
    pub fn user_id(&self, email: &str) -> String {
        self.stub.inner.lock().unwrap().account_by_email(Some(email)).user_id.clone()
    }

    /// Make the tokens minted for `email` say `role` for the whole of
    /// `store_id` whatever the grant rows say (`StubInner::token_role`), so
    /// a test can have the hosted server take writes the rows refuse. One
    /// lock for the lookup and the insert: `user_id` takes the same std
    /// mutex, and calling it while holding the lock deadlocks the test.
    pub fn set_token_role(&self, email: &str, store_id: StoreId, role: &str) {
        let mut inner = self.stub.inner.lock().unwrap();
        let user_id = inner.account_by_email(Some(email)).user_id.clone();
        inner.token_role.insert((user_id, store_id.to_string()), role.to_string());
    }

    pub fn has_share_key(&self, store_id: StoreId, root: NodeId, email: &str) -> bool {
        let s = self.stub.inner.lock().unwrap();
        let Some(user_id) = s.accounts.iter().find(|a| a.email == email).map(|a| a.user_id.clone()) else { return false };
        s.has_key(&user_id, &store_id.to_string(), &Some(root.to_string()))
    }

    /// The scoped grants and invitations the accounts service holds for a share.
    pub fn scoped_rows(&self, store_id: StoreId, root: NodeId) -> usize {
        let s = self.stub.inner.lock().unwrap();
        let (store, root) = (store_id.to_string(), Some(root.to_string()));
        s.stores.iter().filter(|row| row.store_id == store && row.root == root).count() + s.invitations.iter().filter(|i| i.store_id == store && i.root == root).count()
    }

    /// The published scope of `root` on the hosted server, `None` when it has none.
    pub async fn scope_on_h(&self, store_id: StoreId, root: NodeId) -> Option<Vec<NodeId>> {
        self.h_admin.get_scopes(store_id).await.ok()?.into_iter().find(|scope| scope.root == root).map(|scope| scope.doc_ids)
    }

    /// The scope keys a hosted document's data key is wrapped under.
    pub async fn wrap_key_ids(&self, store_id: StoreId, doc: NodeId) -> Vec<Uuid> {
        self.h_admin
            .vault_fetch(store_id, VaultDocId::Node(doc), u64::MAX)
            .await
            .ok()
            .and_then(|fetch| fetch.keys)
            .map(|keys| keys.wraps.iter().map(|w| w.scope_key_id).collect())
            .unwrap_or_default()
    }

    pub async fn head_on_h(&self, store_id: StoreId, doc: NodeId) -> u64 {
        self.h_admin.vault_fetch(store_id, VaultDocId::Node(doc), u64::MAX).await.map(|fetch| fetch.head).unwrap_or(0)
    }
}

pub fn contains_bytes_recursive(dir: &Path, needle: &[u8]) -> bool {
    let Ok(entries) = tfs::read_dir(dir) else { return false };
    for path in entries {
        if tfs::is_dir(&path) {
            if contains_bytes_recursive(&path, needle) {
                return true;
            }
        } else if let Ok(bytes) = tfs::read(&path) {
            if bytes.windows(needle.len()).any(|w| w == needle) {
                return true;
            }
        }
    }
    false
}

pub async fn spawn_env() -> Env {
    let signing_key = fresh_signing_key();
    let kid = "test-kid".to_string();
    let issuer = "https://cloud.test".to_string();

    let h_dir = tempfile::tempdir().unwrap();
    let h_stores_dir = h_dir.path().join("stores");
    tfs::create_dir_all(&h_stores_dir).unwrap();

    let state: StubState = Arc::new(Stub {
        inner: Mutex::new(StubInner {
            accounts: vec![stub_account(ALICE.0, ALICE.1), stub_account(BOB.0, BOB.1), stub_account(CAROL.0, CAROL.1)],
            stores: Vec::new(),
            invitations: Vec::new(),
            key_grants: HashMap::new(),
            signing_key,
            kid,
            issuer: issuer.clone(),
            h_client: None,
            h_rpc_url: String::new(),
            h_stores_dir: h_stores_dir.clone(),
            sharing_calls: 0,
            share_names: HashMap::new(),
            inbox: Vec::new(),
            posted: Vec::new(),
            next_seq: 0,
            kinds: Vec::new(),
            push_subs: Vec::new(),
            watch_choices: HashMap::new(),
            watched: HashMap::new(),
            token_role: HashMap::new(),
        }),
        members_down: AtomicBool::new(false),
        shares_stalled: AtomicBool::new(false),
        hold_next_share_list: AtomicBool::new(false),
        share_list_held: tokio::sync::Notify::new(),
        share_list_release: tokio::sync::Notify::new(),
        store_list_calls: AtomicUsize::new(0),
        refuse_sessions: AtomicBool::new(false),
        token_calls: std::sync::atomic::AtomicUsize::new(0),
        inbox_wake: tokio::sync::Notify::new(),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_addr = listener.local_addr().unwrap();
    let router = stub_router(Arc::clone(&state));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let stub_url = format!("http://{stub_addr}");
    let jwks_url = format!("{stub_url}/jwks.json");

    let h_service_token = "h-service-token".to_string();
    let mut h = PimbleServer::with_config(ServerConfig {
        auth_token: Some(h_service_token.clone()),
        jwks_url: Some(jwks_url.parse().unwrap()),
        jwt_issuer: Some(issuer),
        ..ServerConfig::in_dir(h_dir.path())
    });
    h.start().await.expect("H starts");

    let h_url = format!("http://{}", h.addr());
    let h_client = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token.clone() }).await.expect("service client connects to H");
    let h_admin = PimbleClient::connect_with_auth(&h_url, &AuthMethod::Bearer { token: h_service_token }).await.expect("admin client connects to H");

    let relay = Relay::start(h.addr()).await;
    {
        let mut s = state.inner.lock().unwrap();
        s.h_client = Some(Arc::new(h_client));
        s.h_rpc_url = format!("ws://{}", relay.addr);
    }

    Env { stub_url, stub: state, _h: h, h_admin, h_stores_dir, _h_dir: h_dir, relay }
}

/// The store keys a device's keystore holds for `store_id`, off its `keys.json`.
pub fn scope_keys_in(server_dir: &Path, store_id: StoreId) -> Vec<(Uuid, pimble_crypto::SymmetricKey)> {
    let json: serde_json::Value = serde_json::from_slice(&tfs::read(server_dir.join("keys.json")).expect("the keystore file")).unwrap();
    json["store_keys"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry["store_id"].as_str() == Some(store_id.to_string().as_str()))
        .map(|entry| {
            let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(entry["key"].as_str().unwrap()).unwrap().try_into().unwrap();
            (entry["key_id"].as_str().unwrap().parse().unwrap(), pimble_crypto::SymmetricKey(bytes))
        })
        .collect()
}

pub async fn wait_all_seeded(env: &Env, store_id: StoreId, docs: &[NodeId]) {
    let seeded = wait_until(Duration::from_secs(15), || async {
        let listed = env.h_admin.vault_list_docs(store_id).await.unwrap_or_default();
        docs.iter().all(|id| listed.iter().any(|d| d.doc_id == VaultDocId::Node(*id) && d.head > 0))
    })
    .await;
    assert!(seeded, "every document must reach the hosted twin");
}

pub fn sorted(mut ids: Vec<NodeId>) -> Vec<String> {
    let mut out: Vec<String> = ids.drain(..).map(|id| id.to_string()).collect();
    out.sort();
    out
}

/// Alice's hosted store, the shape most tests start from:
///
/// ```text
/// root
/// ├── Holiday (folder)        <- what gets shared
/// │   ├── Packing (document, "socks and a map")
/// │   └── Tickets (folder)
/// │       └── Train (document)
/// └── Diary (document, "PRIVATE-DIARY-TEXT")
/// ```
pub struct Fixture {
    pub store_id: StoreId,
    pub root_id: NodeId,
    pub shared: NodeId,
    pub inside: NodeId,
    pub deeper: NodeId,
    pub leaf: NodeId,
    pub private: NodeId,
}

pub async fn hosted_fixture(env: &Env, alice: &PimbleClient, alice_dir: &Path) -> Fixture {
    env.sign_in(alice, ALICE).await;
    let (store_id, root_id) = alice.create_store(alice_dir.join("a.pimble"), "Alice's Notes").await.unwrap();
    let shared = alice.create_node(store_id, Some(root_id), "folder", "Holiday").await.unwrap();
    let inside = alice.create_node(store_id, Some(shared), "document", "Packing").await.unwrap();
    let deeper = alice.create_node(store_id, Some(shared), "folder", "Tickets").await.unwrap();
    let leaf = alice.create_node(store_id, Some(deeper), "document", "Train").await.unwrap();
    let private = alice.create_node(store_id, Some(root_id), "document", "Diary").await.unwrap();
    // A person's content edit earns a `modified_at` stamp on the server's
    // flush debounce (750 ms). It lands before hosting, so that every
    // append a test counts later is one the test made.
    seed_and_wait_stamped(alice, store_id, &[(inside, "socks and a map"), (private, "PRIVATE-DIARY-TEXT")]).await;
    alice.cloud_host_store(store_id).await.unwrap();
    wait_all_seeded(env, store_id, &[root_id, shared, inside, deeper, leaf, private]).await;
    Fixture { store_id, root_id, shared, inside, deeper, leaf, private }
}

/// A member's device: signed in, the share added as a partial replica, and
/// its first document pulled.
pub async fn member_device(env: &Env, who: (&str, &str), fx: &Fixture) -> (PimbleServer, PimbleClient, tempfile::TempDir) {
    let (server, client, dir) = start_local_server().await;
    env.sign_in(&client, who).await;
    client.cloud_add_hosted_store(fx.store_id).await.expect("a share is added like any hosted store");
    let pulled = wait_until(Duration::from_secs(15), || async { node_text(&client, fx.store_id, fx.inside).await.contains("socks and a map") }).await;
    assert!(pulled, "{}'s replica pulls the scope and reads it with the share's key", who.0);
    (server, client, dir)
}
