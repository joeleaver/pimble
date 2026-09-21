//! The relay face (docs/RELAY_CONTRACT.md, "The owner's server"): how a store
//! that is **not** hosted on Pimble Cloud is shared from this computer.
//!
//! Everything a hosted share does works against "a Pimble server in JWT mode
//! that holds the store's vault twin": scope sets, per-document keys, the
//! vault RPCs, scoped principals, the owner's upkeep, members' vault links,
//! the web vault client. So the relay tier is not a second implementation
//! of any of it. This server runs **a second Pimble server inside its own
//! process**, the face: JWT mode with the signed-in account's JWKS and
//! issuer, loopback only on a port that is new every run, every `Origin`
//! refused, its own `StoreManager` over `<data dir>/pimble/relay/`, holding
//! the vault twin `<store id>.pimble` of each relayed store. The owner's
//! plain store is kept in step with its twin by an ordinary vault link whose
//! remote is the face (`crate::vault_link`, `LinkEndpoint::RelayFace`);
//! members' connections arrive through the tunnel (`crate::relay_tunnel`)
//! and are ordinary authenticated connections to the face.
//!
//! The twin is the same encrypted documents a hosted twin holds, written by
//! the same link. It is derived and disposable, like the search index:
//! deleted, the link's next reconcile pushes it again, and its new
//! [`epoch`](pimble_rpc::VaultListDocsResponse::epoch) tells every link that
//! read the old logs to read these from the start.
//!
//! **Nothing is hosted** (`CLAUDE.md`): nothing here sends a byte of a store,
//! ciphertext included, anywhere but to a member's own connection.
//!
//! [`RelayHost`] is the one instance per server of all of it: the face
//! (started lazily, once, when the first relayed store's link connects;
//! started again when another account signs in), which relayed stores are
//! open here and which of them the tunnel announces, and the tunnel's task.
//! A store is announced only once its twin is known to be whole (its link
//! has reconciled and the shares' upkeep has settled; see
//! [`RelayHost::ready`]), so a member never connects to a twin that is
//! still being built.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use jsonrpsee::types::ErrorObjectOwned;
use pimble_core::{AuthMethod, RelaySide, RemoteEndpoint, StoreAccess, StoreId, StoreKind, SyncState};
use pimble_rpc::{encrypted_store_error, to_rpc_error, CloseStoreRequest, CreateStoreRequest, DeleteVaultStoreRequest, OpenStoreRequest, PimbleApiServer};
use pimble_store::{StoreError, SyncConfig, SyncMode};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use url::Url;
use uuid::Uuid;

use crate::handler::RpcHandler;
use crate::keystore::SignedInAccount;
use crate::principal::service_extensions;
use crate::server::{PimbleServer, ServerConfig};
use crate::vault_link::LinkEndpoint;

// ── What the person is told ──────────────────────────────────────────────

/// `cloudRelayStore` on a store that is hosted on Pimble Cloud.
pub const ALREADY_HOSTED_REFUSAL: &str = "This store is hosted on Pimble Cloud, and is shared from there. Nothing was changed.";
/// `cloudRelayStore` on a store that is shared from this computer already.
pub const ALREADY_RELAYED_REFUSAL: &str = "This store is already shared from this computer.";
/// `cloudRelayStore` on a store linked to another Pimble server.
pub const LINKED_REFUSAL: &str = "This store is linked to another Pimble server. Unlink it first. Nothing was changed.";
/// `cloudRelayStore` on a replica: a copy of a store that lives elsewhere.
pub const REPLICA_REFUSAL: &str = "This is a copy of a store that lives somewhere else, and can only be shared from there. Nothing was changed.";
/// `cloudStopRelaying` on a store that is not shared from this computer.
pub const NOT_RELAYED_REFUSAL: &str = "This store is not shared from this computer.";
/// `cloudStopRelaying` while a node of the store is still shared.
pub const HAS_SHARES_REFUSAL: &str = "This store still has shares. Stop sharing each of them first. Nothing was changed.";
/// `cloudStopRelaying` when the accounts service cannot be reached: the
/// store's record there goes first, so that nothing half happens.
pub const STOP_UNREACHABLE_REFUSAL: &str = "Pimble Cloud cannot be reached, so nothing was changed. Stop sharing from this computer again once it is back online.";
/// `setStoreSync` (link or unlink) and `cloudHostStore` on a store shared
/// from this computer.
pub const RELAYED_LINK_REFUSAL: &str = "This store is shared from this computer. Stop sharing it from this computer first. Nothing was changed.";

const NOT_SIGNED_IN: &str = "no Pimble Cloud account is signed in";

/// `<relay dir>/<store id>.pimble`: a relayed store's twin.
fn twin_path(dir: &Path, store_id: StoreId) -> PathBuf {
    dir.join(format!("{store_id}.pimble"))
}

/// The `iss` claim of a token this server has just been handed by the
/// accounts service, read without verifying it: it is what every token that
/// service mints carries, and so what the face is told to require. (The
/// hosted server is given the same value by whoever deploys it.)
fn issuer_of(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("iss")?.as_str().map(str::to_string)
}

struct Face {
    server: PimbleServer,
    handler: RpcHandler,
    url: Url,
    /// The accounts service whose tokens the face takes.
    account_url: String,
    issuer: String,
}

#[derive(Default)]
struct Inner {
    face: Option<Face>,
    tunnel: Option<JoinHandle<()>>,
    /// The relayed stores open here: `true` once the tunnel announces them.
    stores: HashMap<StoreId, bool>,
}

pub(crate) struct RelayHost {
    /// Where the twins live: `<data dir>/pimble/relay` unless the server's
    /// configuration says otherwise.
    dir: PathBuf,
    inner: Mutex<Inner>,
    /// What the tunnel announces. Written under `inner`'s lock.
    serve: watch::Sender<HashSet<StoreId>>,
    /// The running face's URL, written under `inner`'s lock and read without
    /// it: the tunnel asks for every member's connection, and must not wait
    /// behind a face that is starting or a twin that is being opened.
    url: std::sync::Mutex<Option<Url>>,
}

impl RelayHost {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir, inner: Mutex::new(Inner::default()), serve: watch::channel(HashSet::new()).0, url: std::sync::Mutex::new(None) }
    }

    /// Everything a relayed store's link needs before it connects: the face
    /// running for `account`'s service, the store's twin open on it (created
    /// when there is none, which is also how a deleted twin comes back), and
    /// the tunnel's task. Answers the face's URL, which holds for this run
    /// of the face only. `token` is one the accounts service has just
    /// minted; its issuer is the one the face requires.
    ///
    /// With `fresh`, a twin left over from an earlier time the store was
    /// relayed is deleted first: `cloudRelayStore` makes a new store key, and
    /// blobs under an older one are nobody's to read.
    pub(crate) async fn prepare(&self, handler: &RpcHandler, store_id: StoreId, account: &SignedInAccount, token: &str, fresh: bool) -> anyhow::Result<Url> {
        let mut inner = self.inner.lock().await;
        let issuer = issuer_of(token).ok_or_else(|| anyhow::anyhow!("the token minted by {} names no issuer", account.url))?;

        // Another service, or another issuer: the face verifies against the
        // wrong keys. It goes, and with it what it served.
        if inner.face.as_ref().is_some_and(|face| face.account_url != account.url || face.issuer != issuer) {
            info!("Relay face: the signed-in account's service changed; starting again");
            self.stop_locked(&mut inner).await;
        }
        if inner.face.is_none() {
            let face = self.start_face(account, issuer).await?;
            *self.url.lock().unwrap() = Some(face.url.clone());
            inner.face = Some(face);
        }
        let face = inner.face.as_ref().expect("started above");
        let url = face.url.clone();
        self.ensure_twin(face, store_id, fresh).await?;

        if fresh || !inner.stores.contains_key(&store_id) {
            inner.stores.insert(store_id, false);
            self.publish(&inner);
        }
        if inner.tunnel.as_ref().is_none_or(|task| task.is_finished()) {
            inner.tunnel = Some(tokio::spawn(crate::relay_tunnel::run(handler.clone(), self.serve.subscribe())));
        }
        Ok(url)
    }

    async fn start_face(&self, account: &SignedInAccount, issuer: String) -> anyhow::Result<Face> {
        tokio::fs::create_dir_all(&self.dir).await.map_err(|e| anyhow::anyhow!("could not create {}: {}", self.dir.display(), e))?;
        let jwks_url = crate::cloud::jwks_url(&account.url).map_err(|e| anyhow::anyhow!(e))?;
        let config = ServerConfig {
            addr: "127.0.0.1:0".parse().expect("a loopback address"),
            // For lifecycle calls only, and those are made in-process: held
            // in memory, never written down, never sent anywhere.
            auth_token: Some(crate::auth::generate_token()),
            jwks_url: Some(jwks_url),
            jwt_issuer: Some(issuer.clone()),
            allowed_origins: Vec::new(),
            // A face signs in to nothing, links nothing and replicates
            // nothing; these only keep it off the real config and data
            // directories.
            credentials_path: Some(self.dir.join(".face").join("credentials.json")),
            keystore_path: Some(self.dir.join(".face").join("keys.json")),
            replicas_dir: Some(self.dir.join(".face").join("replicas")),
            relay_dir: Some(self.dir.join(".face").join("relay")),
            share_sweep_interval: None,
            grant_check_interval: None,
        };
        let mut server = PimbleServer::relay_face(config);
        server.start().await.map_err(|e| anyhow::anyhow!("the relay face did not start: {}", e))?;
        let handler = server.handler().expect("a started server has a handler");
        let url: Url = format!("ws://{}", server.addr()).parse().expect("a socket address is a URL's authority");
        info!("Relay face listening on {} for tokens from {}", server.addr(), issuer);
        Ok(Face { server, handler, url, account_url: account.url.clone(), issuer })
    }

    /// The store's twin, open on the face. One that is there is opened; one
    /// that is not (never made, or deleted) is created empty, and the
    /// store's link fills it. One that will not open is as good as deleted.
    async fn ensure_twin(&self, face: &Face, store_id: StoreId, fresh: bool) -> anyhow::Result<()> {
        let ext = service_extensions();
        let path = twin_path(&self.dir, store_id);
        let open = face.handler.store_manager_handle().read().await.is_open(store_id);
        if open && !fresh {
            // Deleted from under the running face: the open store is a
            // handle on nothing.
            if path.join("manifest.json").exists() {
                return Ok(());
            }
            let _ = face.handler.close_store(&ext, CloseStoreRequest { store_id }).await;
        } else if open {
            let _ = face.handler.close_store(&ext, CloseStoreRequest { store_id }).await;
        }
        if fresh {
            remove_twin_dir(&self.dir, store_id).await;
        }
        if path.exists() {
            match face.handler.open_store(&ext, OpenStoreRequest { path: path.clone() }).await {
                Ok(opened) if opened.store.id == store_id && opened.store.kind == StoreKind::Vault => return Ok(()),
                Ok(opened) => {
                    warn!("Relay face: {} holds store {} ({:?}), not the twin of {}; building it again", path.display(), opened.store.id, opened.store.kind, store_id);
                    let _ = face.handler.close_store(&ext, CloseStoreRequest { store_id: opened.store.id }).await;
                }
                Err(e) => warn!("Relay face: the twin at {} does not open ({}); building it again", path.display(), e.message()),
            }
            remove_twin_dir(&self.dir, store_id).await;
        }
        // No name: the owner's name for the store is the owner's, and a
        // member with a grant on the whole store would be told this one.
        face.handler
            .create_store(&ext, CreateStoreRequest { path, name: String::new(), kind: StoreKind::Vault, store_id: Some(store_id) })
            .await
            .map_err(|e| anyhow::anyhow!("the relay face could not create the twin of {}: {}", store_id, e.message()))?;
        info!("Relay face: an empty twin for store {}; its link fills it", store_id);
        Ok(())
    }

    /// The twin holds everything the store does and its shares are in place
    /// (the store's link says when): the tunnel may announce it.
    pub(crate) async fn ready(&self, store_id: StoreId) {
        let mut inner = self.inner.lock().await;
        if let Some(served) = inner.stores.get_mut(&store_id) {
            if !*served {
                *served = true;
                info!("Store {} is shared from this computer: announcing it to the relay", store_id);
                self.publish(&inner);
            }
        }
    }

    /// The store is no longer relayed from here (closed, or
    /// `cloudStopRelaying`): withdrawn from the tunnel, its twin closed, and
    /// with `delete` removed. The face stays for the others; the tunnel
    /// ends by itself once there is nothing to announce.
    pub(crate) async fn withdraw(&self, store_id: StoreId, delete: bool) {
        let mut inner = self.inner.lock().await;
        if inner.stores.remove(&store_id).is_some() {
            self.publish(&inner);
        }
        if let Some(face) = &inner.face {
            let ext = service_extensions();
            if face.handler.store_manager_handle().read().await.is_open(store_id) {
                let closed = if delete {
                    face.handler.delete_vault_store(&ext, DeleteVaultStoreRequest { store_id }).await.map(|_| ())
                } else {
                    face.handler.close_store(&ext, CloseStoreRequest { store_id }).await.map(|_| ())
                };
                if let Err(e) = closed {
                    warn!("Relay face: could not let go of the twin of {}: {}", store_id, e.message());
                }
            }
        }
        if delete {
            remove_twin_dir(&self.dir, store_id).await;
        }
    }

    /// Whether `store_id` is relayed from here right now.
    pub(crate) async fn is_relaying(&self, store_id: StoreId) -> bool {
        self.inner.lock().await.stores.contains_key(&store_id)
    }

    /// The face's URL while it runs: where the tunnel takes a member's
    /// connection.
    pub(crate) fn face_url(&self) -> Option<Url> {
        self.url.lock().unwrap().clone()
    }

    /// Another account signed in, or this one signed out: the tunnel is the
    /// old session's. A new one is started if anything is relayed; it waits,
    /// with the links, for an account.
    pub(crate) async fn account_changed(&self, handler: &RpcHandler) {
        let mut inner = self.inner.lock().await;
        if let Some(task) = inner.tunnel.take() {
            task.abort();
        }
        if !inner.stores.is_empty() {
            inner.tunnel = Some(tokio::spawn(crate::relay_tunnel::run(handler.clone(), self.serve.subscribe())));
        }
    }

    /// With the server: the tunnel, then the face.
    pub(crate) async fn stop(&self) {
        let mut inner = self.inner.lock().await;
        self.stop_locked(&mut inner).await;
    }

    async fn stop_locked(&self, inner: &mut Inner) {
        if let Some(task) = inner.tunnel.take() {
            task.abort();
        }
        inner.stores.clear();
        self.publish(inner);
        *self.url.lock().unwrap() = None;
        if let Some(mut face) = inner.face.take() {
            // Boxed: a server's `stop` stops its relay host, which stops a
            // server, and a future cannot contain itself.
            if let Err(e) = Box::pin(face.server.stop()).await {
                warn!("Relay face: did not stop cleanly: {}", e);
            }
            debug!("Relay face stopped");
        }
    }

    fn publish(&self, inner: &Inner) {
        let served: HashSet<StoreId> = inner.stores.iter().filter(|(_, served)| **served).map(|(id, _)| *id).collect();
        self.serve.send_if_modified(|current| {
            if *current == served {
                return false;
            }
            *current = served;
            true
        });
    }
}

// ── cloudRelayStore, cloudStopRelaying ───────────────────────────────────

impl RpcHandler {
    /// `cloudRelayStore`: the person asked for this store to be shared from
    /// this computer. What `cloudHostStore` does, minus anything that
    /// touches the hosted server: a store key (keystore), the store's record
    /// on the accounts service as `tier: "relay"` with **no name**, the
    /// owner's own key envelope; and in place of a hosted twin, one on this
    /// machine's relay face, kept by the store's vault link. Nothing of the
    /// store, ciphertext included, goes to Pimble Cloud.
    pub(crate) async fn relay_store(&self, store_id: StoreId) -> Result<(), ErrorObjectOwned> {
        info!("Sharing store {} from this computer", store_id);

        // The refusals, before anything is asked of anyone.
        let mut store = {
            let manager = self.store_manager_handle();
            let manager = manager.read().await;
            match manager.store_kind(store_id) {
                Some(StoreKind::Plain) => manager.get_store_info(store_id).map_err(to_rpc_error)?,
                Some(StoreKind::Vault) => return Err(encrypted_store_error(format!("store {} is encrypted storage, not a store of its own", store_id))),
                None => return Err(to_rpc_error(StoreError::NotOpen(store_id))),
            }
        };
        self.mark_replica(&mut store);
        let config = self.store_manager_handle().read().await.read_sync_config(store_id).await.map_err(to_rpc_error)?;
        match config.map(|config| config.mode) {
            Some(SyncMode::Relay) => return Err(to_rpc_error(ALREADY_RELAYED_REFUSAL)),
            Some(SyncMode::Vault) if !store.is_replica && store.roots.is_empty() => return Err(to_rpc_error(ALREADY_HOSTED_REFUSAL)),
            Some(SyncMode::Sync) if !store.is_replica => return Err(to_rpc_error(LINKED_REFUSAL)),
            _ => {}
        }
        if store.is_replica || !store.roots.is_empty() {
            return Err(to_rpc_error(REPLICA_REFUSAL));
        }
        let account = self.keystore().account().await.ok_or_else(|| to_rpc_error(NOT_SIGNED_IN))?;

        // The face and an empty twin first: if this machine cannot serve the
        // store, nothing has been recorded anywhere.
        let minted = crate::cloud::mint_token(&account.url, &account.session).await.map_err(to_rpc_error)?;
        self.relay().prepare(self, store_id, &account, &minted.token, true).await.map_err(to_rpc_error)?;

        let key_id = match self.record_relayed_store(&account, store_id).await {
            Ok(key_id) => key_id,
            Err(e) => {
                self.relay().withdraw(store_id, true).await;
                return Err(e);
            }
        };

        // `remote.url` is where members reach the store, for the record: the
        // link connects to the face, whose port is new every run.
        let member_url = crate::cloud::relay_member_url(&account.url, store_id).map_err(to_rpc_error)?;
        crate::vault_link::forget_progress(self, store_id).await;
        let written = self
            .store_manager_handle()
            .read()
            .await
            .write_sync_config(
                store_id,
                &SyncConfig {
                    remote: RemoteEndpoint { url: member_url, auth: AuthMethod::None },
                    last_sync: None,
                    mode: SyncMode::Relay,
                    via_relay: false,
                    last_seq: Default::default(),
                    vault_key_id: Some(key_id),
                    access: StoreAccess::Full,
                    shared_by: None,
                    read_only_roots: Vec::new(),
                },
            )
            .await;
        if let Err(e) = written {
            self.relay().withdraw(store_id, true).await;
            if let Err(undo) = crate::cloud::delete_store(&account.url, &account.session, &store_id.to_string()).await {
                warn!("Store {}: its record on {} could not be removed again: {}", store_id, account.url, undo);
            }
            return Err(to_rpc_error(e));
        }

        self.ensure_vault_link_started(store_id, LinkEndpoint::RelayFace, key_id).await;
        Ok(())
    }

    /// The accounts-service half of `cloudRelayStore`: the record, and the
    /// store key with the owner's own envelope of it. Account data, as for a
    /// hosted store; the record is taken back if the key cannot be put.
    async fn record_relayed_store(&self, account: &SignedInAccount, store_id: StoreId) -> Result<Uuid, ErrorObjectOwned> {
        let id = store_id.to_string();
        let view = crate::cloud::create_relayed_store(&account.url, &account.session, &id).await.map_err(to_rpc_error)?;
        if view.store_id != id {
            return Err(to_rpc_error(format!("cloud service recorded store {} instead of the requested {}", view.store_id, id)));
        }
        let key = pimble_crypto::SymmetricKey::generate();
        let key_id = Uuid::new_v4();
        let handed_over = async {
            let envelope = pimble_crypto::wrap_key(&key, key_id, &account.keys.public_keys(), &account.keys, &format!("store:{}", store_id)).map_err(to_rpc_error)?;
            crate::cloud::put_store_key(&account.url, &account.session, &id, &account.user_id, key_id, &envelope, None).await.map_err(to_rpc_error)?;
            self.keystore().add_store_key(store_id, key_id, &key).await.map_err(to_rpc_error)
        }
        .await;
        if let Err(e) = handed_over {
            if let Err(undo) = crate::cloud::delete_store(&account.url, &account.session, &id).await {
                warn!("Store {}: its record on {} could not be removed again: {}", store_id, account.url, undo);
            }
            return Err(e);
        }
        Ok(key_id)
    }

    /// `cloudStopRelaying`: the way back. Refused while the store has shares
    /// (each is stopped first, which takes its members and its scope down
    /// properly). The accounts service's record goes first, so that a
    /// service that cannot be reached leaves everything as it was; then the
    /// link, the tunnel's entry, the twin and `sync.json`.
    pub(crate) async fn stop_relaying(&self, store_id: StoreId) -> Result<(), ErrorObjectOwned> {
        info!("Stopping sharing store {} from this computer", store_id);
        {
            let manager = self.store_manager_handle();
            let manager = manager.read().await;
            if !manager.is_open(store_id) {
                return Err(to_rpc_error(StoreError::NotOpen(store_id)));
            }
        }
        if self.link_kind_of(store_id).await.1 != RelaySide::Owner {
            return Err(to_rpc_error(NOT_RELAYED_REFUSAL));
        }
        if crate::share::has_shares(&*self.store_manager_handle().read().await, store_id) {
            return Err(to_rpc_error(HAS_SHARES_REFUSAL));
        }
        let account = self.keystore().account().await.ok_or_else(|| to_rpc_error(NOT_SIGNED_IN))?;
        match crate::cloud::delete_store(&account.url, &account.session, &store_id.to_string()).await {
            Ok(()) => {}
            // Gone already: there is nothing left there to undo.
            Err(e) if e.status() == Some(404) => debug!("Store {}: no record of it on {} any more", store_id, account.url),
            Err(e @ crate::cloud::CloudError::Request { .. }) => {
                warn!("Store {}: {}", store_id, e);
                return Err(to_rpc_error(STOP_UNREACHABLE_REFUSAL));
            }
            Err(e) => return Err(to_rpc_error(e)),
        }

        self.stop_vault_link(store_id).await;
        self.shares().forget_store(store_id);
        self.relay().withdraw(store_id, true).await;
        crate::vault_link::forget_progress(self, store_id).await;
        self.store_manager_handle().read().await.clear_sync_config(store_id).await.map_err(to_rpc_error)?;
        self.notify_sync_state_changed(store_id, SyncState::Offline).await;
        Ok(())
    }
}

/// Delete a twin's directory: that one directory inside the relay directory,
/// named by the store's id, never a path anyone gives.
async fn remove_twin_dir(dir: &Path, store_id: StoreId) {
    let path = twin_path(dir, store_id);
    if !path.starts_with(dir) || !path.exists() {
        return;
    }
    if let Err(e) = tokio::fs::remove_dir_all(&path).await {
        warn!("Relay face: could not delete {}: {}", path.display(), e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_issuer_is_read_off_a_token_without_verifying_it() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"iss":"https://pimble.app/api/v1","sub":"u","aud":"pimble"}"#);
        assert_eq!(issuer_of(&format!("e30.{payload}.sig")).as_deref(), Some("https://pimble.app/api/v1"));
        assert_eq!(issuer_of("not-a-token"), None);
        assert_eq!(issuer_of(&format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(b"{}"))), None);
    }
}
