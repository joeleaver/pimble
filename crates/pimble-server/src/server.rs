//! Server startup and management

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use jsonrpsee::server::{Server, ServerHandle};
#[cfg(not(target_arch = "wasm32"))]
use pimble_rpc::PimbleApiServer;
use pimble_store::StoreManager;
use tokio::sync::RwLock;
#[cfg(not(target_arch = "wasm32"))]
use tracing::info;
use url::Url;

use crate::handler::RpcHandler;
use crate::Result;

/// Configuration for the Pimble server
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind to
    pub addr: SocketAddr,
    /// Required credential for every JSON-RPC request (docs/
    /// HARDENING_CONTRACT.md decisions 1-2): `Authorization: Bearer
    /// <token>` or `X-Api-Key: <token>`, checked at the HTTP edge before any
    /// request reaches the store. `None` means no static-token check;
    /// `start()` only allows binding beyond loopback when this or `jwks_url`
    /// (with `jwt_issuer`) is set — the app's embedded server runs with
    /// neither.
    pub auth_token: Option<String>,
    /// The JWKS endpoint for JWT verification (docs/CLOUD_CONTRACT.md "B:
    /// pimble-server" item 1): `--jwks URL` / `PIMBLE_JWKS_URL`. Only takes
    /// effect together with `jwt_issuer`; audience is always fixed to
    /// `pimble`.
    pub jwks_url: Option<Url>,
    /// The `iss` claim every accepted JWT must carry: `--issuer ISS` /
    /// `PIMBLE_JWT_ISSUER`. Only takes effect together with `jwks_url`.
    pub jwt_issuer: Option<String>,
    /// Origins a WebSocket handshake's `Origin` header may carry and still
    /// be accepted (docs/CLOUD_CONTRACT.md "B: pimble-server" item 3):
    /// `--allow-origin` (repeatable) / `PIMBLE_ALLOW_ORIGINS` (comma
    /// separated). Empty (the default, and always for the embedded app
    /// server) refuses every request that carries an `Origin` header at
    /// all, as before this contract.
    pub allowed_origins: Vec<String>,
    /// Where saved per-remote credentials live (decision 4). `None` uses
    /// [`crate::credentials::default_credentials_path`]; tests set this to a
    /// temp path so they never touch the real config directory.
    pub credentials_path: Option<PathBuf>,
    /// The directory this server creates replicas in and recognises them
    /// by (`addRemoteStore` with `path: None`, every replica a remote
    /// mount's resolution creates, `Store::is_replica`, `removeReplica`).
    /// `None` uses [`crate::handler::default_replicas_dir`]
    /// (`<data dir>/pimble/replicas`); tests set this to a temp directory
    /// so they never write into the real data directory.
    pub replicas_dir: Option<PathBuf>,
    /// Where this server's signed-in Pimble Cloud account and unwrapped
    /// store keys live (docs/CRYPTO_CONTRACT.md). `None` uses
    /// [`crate::keystore::default_keystore_path`] (`<config dir>/pimble/keys.json`);
    /// tests set this to a temp path so a sign-in never touches the real
    /// config directory.
    pub keystore_path: Option<PathBuf>,
    /// How often each hosted store's key sweep runs (`crate::share`: a
    /// share's member who has the grant and not yet the key is handed it).
    /// `None` is the default minute; tests shorten it.
    pub share_sweep_interval: Option<std::time::Duration>,
    /// How often a connected vault link asks the accounts service whether
    /// the account's grant on its store has changed (`crate::vault_link`'s
    /// `grant_shape`: a role the owner changed, another share, a removal).
    /// `None` is the default two minutes; tests shorten it.
    pub grant_check_interval: Option<std::time::Duration>,
    /// Where the encrypted twins of the stores shared from this computer
    /// live (docs/RELAY_CONTRACT.md; `crate::relay_face`). `None` puts it
    /// beside the replicas directory, as `relay`: `<data dir>/pimble/relay`
    /// by default, and inside a test's temp directory whenever
    /// `replicas_dir` is.
    pub relay_dir: Option<PathBuf>,
    /// Where this device's AI agents live (docs/AGENTS_CONTRACT.md). `None`
    /// uses [`crate::agents::default_agents_path`] on a loopback server with
    /// no JWT verifier (the app's own), and no agents anywhere else; tests
    /// set a temp path.
    pub agents_path: Option<PathBuf>,
    /// The version `serverInfo` reports. `None` is this build's; tests set an
    /// older one to see a newer client take the port over.
    pub reported_version: Option<String>,
    /// Whether this server's vault links say what they hold of each document
    /// and send the other devices what their reports lack
    /// (docs/CONVERGENCE_CONTRACT.md wave 1). `true`; a test turns it off to
    /// show the gap it closes.
    pub vault_reports: bool,
    /// How long a document's difference from what the other devices hold
    /// must last, with the link connected, before it is out of step
    /// (docs/CONVERGENCE_CONTRACT.md section 4). `None` is five minutes;
    /// tests shorten it.
    pub out_of_step_after: Option<std::time::Duration>,
    /// How long a parked update waits, with every reporting device lacking
    /// what it waits for, before it is set aside (docs/CONVERGENCE_CONTRACT.md
    /// section 5). `None` is 24 hours; tests shorten it.
    pub set_aside_after: Option<std::time::Duration>,
    /// Faults a test injects into this server's vault links
    /// (`crate::faults`). `None` outside the fault test.
    #[doc(hidden)]
    pub link_faults: Option<std::sync::Arc<crate::faults::LinkFaults>>,
    /// The disk everything this server keeps is on
    /// (docs/ONE_CODEBASE_CONTRACT.md seam B): its stores, their state
    /// directories, the keystore, the saved credentials, the agents file, the
    /// relay twins. The machine's own on the desktop; a memory disk in a
    /// browser told not to remember itself, and in the suite run on one
    /// (`PIMBLE_TEST_DISK=memory`, which `Disk::default` reads in tests).
    pub disk: pimble_host::Disk,
    /// Where this server keeps what is its own (seam E): every `*_path` and
    /// `*_dir` above that is `None` is laid out under these
    /// ([`pimble_host::Paths::native`] by default: the platform's data and
    /// config directories).
    pub paths: pimble_host::Paths,
    /// How this server reaches the Pimble Cloud accounts service (seam D):
    /// `reqwest` on the desktop, `fetch` in a browser's worker.
    pub http: Arc<dyn pimble_host::http::Http>,
    /// What this server's tasks, timers and clocks run on (seam C): tokio,
    /// on the caller's runtime, on the desktop; `spawn_local` and
    /// `setTimeout` in a browser's worker.
    pub runtime: pimble_host::Runtime,
    /// Where the keystore rests between runs (section 7, `KeyRest`): `None`
    /// is `keys.json` (`keystore_path`) on `disk`, the desktop's; a browser's
    /// worker hands its device-key store, or memory for "don't remember this
    /// browser".
    pub key_rest: Option<Arc<dyn pimble_host::KeyRest>>,
    /// A browser's device id and answered reports from before its server ran
    /// in a worker, taken by each store's first device entry (wave 4).
    pub inherited_device: Option<crate::devices::InheritedDevice>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:7462".parse().unwrap(),
            auth_token: None,
            jwks_url: None,
            jwt_issuer: None,
            allowed_origins: Vec::new(),
            credentials_path: None,
            replicas_dir: None,
            keystore_path: None,
            share_sweep_interval: None,
            grant_check_interval: None,
            relay_dir: None,
            agents_path: None,
            reported_version: None,
            vault_reports: true,
            out_of_step_after: None,
            set_aside_after: None,
            link_faults: None,
            disk: pimble_host::Disk::default(),
            paths: default_paths(),
            http: default_http(),
            runtime: pimble_host::Runtime::default(),
            key_rest: None,
            inherited_device: None,
        }
    }
}

/// The desktop's: the platform's data and config directories.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn default_paths() -> pimble_host::Paths {
    pimble_host::Paths::native()
}

/// A browser's worker: folders at the root of its disk (the origin private
/// file system's root in wave 4; a memory disk's until then).
#[cfg(target_arch = "wasm32")]
pub(crate) fn default_paths() -> pimble_host::Paths {
    pimble_host::Paths::new("/pimble", "/pimble-config")
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn default_http() -> Arc<dyn pimble_host::http::Http> {
    Arc::new(pimble_host::http::ReqwestHttp::default())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn default_http() -> Arc<dyn pimble_host::http::Http> {
    Arc::new(pimble_host::http::FetchHttp)
}

impl ServerConfig {
    /// A loopback server on a free port that keeps every per-computer path
    /// under `dir`: replicas (`dir/replicas`, so each store's state root is
    /// `dir/stores`), the relay twins (`dir/relay`), the keystore, the saved
    /// credentials and the agents file. What every test starts from
    /// (`..ServerConfig::in_dir(dir)`), so no test reads or writes the real
    /// config or data directories; `dir` must outlive the server.
    pub fn in_dir(dir: &std::path::Path) -> Self {
        Self {
            addr: "127.0.0.1:0".parse().unwrap(),
            credentials_path: Some(dir.join("credentials.json")),
            replicas_dir: Some(dir.join("replicas")),
            keystore_path: Some(dir.join("keys.json")),
            relay_dir: Some(dir.join("relay")),
            agents_path: Some(dir.join("agents.json")),
            paths: pimble_host::Paths::in_dir(dir),
            ..Default::default()
        }
    }

    fn replicas_dir(&self) -> PathBuf {
        self.replicas_dir.clone().unwrap_or_else(|| self.paths.replicas())
    }
}

/// The Pimble server
pub struct PimbleServer {
    config: ServerConfig,
    store_manager: Arc<RwLock<StoreManager>>,
    #[cfg(not(target_arch = "wasm32"))]
    handle: Option<ServerHandle>,
    /// The handler the running server dispatches to, kept so `stop()` can
    /// stop the links it started.
    handler: Option<RpcHandler>,
    /// The socket the server actually bound to, resolved once at `start()`
    /// (`Server::local_addr()`), so a config with port 0 (as used in tests)
    /// still has a meaningful `addr()`.
    local_addr: Option<SocketAddr>,
    /// A relay face (`crate::relay_face`): a server inside another server's
    /// process that holds vault twins and nothing else. It has no search
    /// index to warm a model for.
    face: bool,
}

impl PimbleServer {
    /// Create a new server with default configuration
    pub fn new() -> Self {
        Self::with_config(ServerConfig::default())
    }

    /// Create a new server with custom configuration
    pub fn with_config(config: ServerConfig) -> Self {
        let state_root = crate::handler::state_root_beside(&config.replicas_dir());
        let disk = config.disk.clone();
        let runtime = config.runtime.clone();
        Self {
            config,
            // Each store's per-computer state (its index, its link's read
            // positions) lives beside the replicas directory, outside the
            // store's folder (docs/STORE_FOLDERS_CONTRACT.md rule 1), and so
            // inside a test's temp directory whenever its replicas are.
            store_manager: Arc::new(RwLock::new(StoreManager::with_state_root(state_root).on_disk(disk).on_runtime(runtime))),
            #[cfg(not(target_arch = "wasm32"))]
            handle: None,
            handler: None,
            local_addr: None,
            face: false,
        }
    }

    /// A relay face: see [`crate::relay_face`].
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn relay_face(config: ServerConfig) -> Self {
        Self { face: true, ..Self::with_config(config) }
    }

    /// The handler a started server dispatches to, for calls made to it
    /// in-process.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn handler(&self) -> Option<RpcHandler> {
        self.handler.clone()
    }

    /// Get a reference to the store manager
    pub fn store_manager(&self) -> Arc<RwLock<StoreManager>> {
        Arc::clone(&self.store_manager)
    }

    /// Start the server.
    ///
    /// Refuses to bind a non-loopback address without `config.auth_token`
    /// (docs/history/HARDENING_CONTRACT.md decision 2): an open port reachable from
    /// outside this machine with no credential check is never allowed, even
    /// by a caller who forgot to configure one.
    ///
    /// Before accepting any RPC (in particular, before any `openStore` can
    /// reach [`RpcHandler`]), warms up the semantic search embedding model
    /// once — see [`warm_up_embedding_model`]. This is what turns "two
    /// stores opening at once each lazily construct their own embedder and
    /// race to download the same model" into "one download, serially, before
    /// any store exists to race over."
    ///
    /// The desktop's only: a browser's worker has no port, and starts with
    /// [`PimbleServer::start_in_process`].
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn start(&mut self) -> Result<()> {
        // An empty or whitespace-only token is never valid: treating it as
        // "no token configured" would silently disable the non-loopback
        // refusal below, and treating it literally would make
        // `AuthMiddleware` admit `Authorization: Bearer ` (nothing after
        // it) and an empty `X-Api-Key`.
        if let Some(token) = &self.config.auth_token {
            if token.trim().is_empty() {
                return Err(crate::ServerError::Server(
                    "refusing to start with an empty auth_token; pass None for no token, or a real one".to_string(),
                ));
            }
        }

        // A JWT verifier configured (jwks_url + jwt_issuer, both required
        // together) counts as a verifier just as much as the static token
        // does (docs/CLOUD_CONTRACT.md "B: pimble-server" item 1: "A server
        // with neither verifier still refuses to bind beyond loopback").
        let jwt_configured = self.config.jwks_url.is_some() && self.config.jwt_issuer.is_some();
        if self.config.jwks_url.is_some() != self.config.jwt_issuer.is_some() {
            return Err(crate::ServerError::Server(
                "jwks_url and jwt_issuer must both be set, or both left unset".to_string(),
            ));
        }

        if !self.config.addr.ip().is_loopback() && self.config.auth_token.is_none() && !jwt_configured {
            return Err(crate::ServerError::Server(format!(
                "refusing to bind {} (not a loopback address) without an auth token or a JWT verifier; \
                 pass ServerConfig::auth_token (or start with a token file, pimble-cli server --token-file) \
                 and/or ServerConfig::jwks_url/jwt_issuer (pimble-cli server --jwks/--issuer)",
                self.config.addr
            )));
        }

        let jwt = if let (Some(jwks_url), Some(issuer)) = (&self.config.jwks_url, &self.config.jwt_issuer) {
            Some(crate::jwt::JwtVerifier::new(jwks_url.clone(), issuer.clone()).await.map_err(|e| {
                crate::ServerError::Server(format!("failed to start the JWT verifier for {}: {}", jwks_url, e))
            })?)
        } else {
            None
        };

        // The HTTP-edge auth layer (docs/CLOUD_CONTRACT.md "B: pimble-server"
        // items 1-3), run on every request before it reaches the JSON-RPC
        // dispatch — including the WebSocket upgrade handshake.
        // This device's agents: the app's own loopback server only, never a
        // relay face or a server verifying Pimble Cloud's tokens.
        let agents = match &self.config.agents_path {
            _ if self.face => None,
            Some(path) => Some(Arc::new(crate::agents::AgentRegistry::load(&self.config.disk, path.clone()).await)),
            None if self.config.addr.ip().is_loopback() && !jwt_configured => {
                Some(Arc::new(crate::agents::AgentRegistry::load(&self.config.disk, self.config.paths.agents()).await))
            }
            None => None,
        };
        let http_middleware = tower::ServiceBuilder::new().layer(
            crate::auth::AuthLayer::new(self.config.auth_token.clone(), jwt, self.config.allowed_origins.clone())
                .with_agents(agents.clone()),
        );

        let server = Server::builder()
            .set_http_middleware(http_middleware)
            // jsonrpsee's 10 MB defaults made a store whose one document
            // outgrew them unloadable for every new client (2026-11-18).
            .max_request_body_size(pimble_rpc::MAX_MESSAGE_BYTES)
            .max_response_body_size(pimble_rpc::MAX_MESSAGE_BYTES)
            .build(&self.config.addr)
            .await
            .map_err(|e| crate::ServerError::Server(e.to_string()))?;

        // `local_addr()` must be read before `start()`, which consumes the
        // `Server` by value; this is what makes port 0 (bind to any free
        // port, used by tests) resolve to something `addr()` can report.
        let local_addr = server.local_addr().map_err(|e| crate::ServerError::Server(e.to_string()))?;
        self.local_addr = Some(local_addr);

        let handler = self.make_handler(agents, jwt_configured, true);
        // The keystore is read before the first call, as it always was.
        handler.keystore().load().await;
        let methods = handler.clone().into_rpc();

        info!("Starting Pimble server on {}", local_addr);
        let handle = server.start(methods);
        handler.set_server_handle(handle.clone());
        self.handle = Some(handle);

        Ok(())
    }

    /// The handler this server dispatches to, configured from `config`, and
    /// kept so `stop()` can stop what it started. Every transport reaches
    /// this one handler (`start`'s jsonrpsee server, `connect_in_process`).
    fn make_handler(&mut self, agents: Option<Arc<crate::agents::AgentRegistry>>, jwt_configured: bool, listening: bool) -> RpcHandler {
        // A relay face holds vault twins only, and a vault has no index.
        // The model warms up when the first index wants it, not before the
        // server listens (docs/STARTUP_CONTRACT.md "Built": it took up to a
        // second before the app could connect, and loading it busies every
        // core): only an index open waits for it, and indexes open in the
        // background after their stores have answered.
        let (warmed, warmup) = tokio::sync::watch::channel(None);
        let wanted = Arc::new(tokio::sync::Notify::new());
        // A browser's worker searches by keyword only (semantic search there
        // is wave 6), so nothing waits for a model.
        if self.face || cfg!(target_arch = "wasm32") {
            let _ = warmed.send(Some(false));
        } else {
            let wanted = Arc::clone(&wanted);
            let runtime = self.config.runtime.clone();
            self.config.runtime.spawn(async move {
                wanted.notified().await;
                #[cfg(not(target_arch = "wasm32"))]
                let _ = warmed.send(Some(warm_up_embedding_model(&runtime).await));
                #[cfg(target_arch = "wasm32")]
                let _ = (warmed, runtime);
            });
        }
        let semantic_available = false;

        let credentials_path = self.config.credentials_path.clone().unwrap_or_else(|| self.config.paths.credentials());
        let replicas_dir = self.config.replicas_dir();
        let keystore_path = self.config.keystore_path.clone().unwrap_or_else(|| self.config.paths.keystore());
        let mut handler = RpcHandler::with_all_paths(Arc::clone(&self.store_manager), semantic_available, credentials_path, replicas_dir, keystore_path).with_semantic_warmup(warmup, wanted);
        handler = handler.with_http(Arc::clone(&self.config.http));
        if let Some(rest) = self.config.key_rest.clone() {
            handler = handler.with_key_rest(rest);
        }
        if let Some(inherited) = self.config.inherited_device.clone() {
            handler = handler.with_inherited_device(inherited);
        }
        if let Some(relay_dir) = self.config.relay_dir.clone() {
            handler = handler.with_relay_dir(relay_dir);
        }
        if let Some(every) = self.config.share_sweep_interval {
            handler = handler.with_share_sweep_interval(every);
        }
        if let Some(every) = self.config.grant_check_interval {
            handler = handler.with_grant_check_interval(every);
        }
        handler = handler.with_vault_reports(self.config.vault_reports);
        if let Some(after) = self.config.out_of_step_after {
            handler = handler.with_out_of_step_after(after);
        }
        if let Some(after) = self.config.set_aside_after {
            handler = handler.with_set_aside_after(after);
        }
        if let Some(faults) = self.config.link_faults.clone() {
            handler = handler.with_link_faults(faults);
        }
        handler = handler.with_agents(agents);
        // The app's own server (loopback, tokenless, no JWT) hands over to a
        // newer one (docs/MCP_CONTRACT.md "Which server it talks to").
        // A server with no port has none to hand over.
        let local_own = listening && !self.face && self.config.addr.ip().is_loopback() && self.config.auth_token.is_none() && !jwt_configured;
        handler = handler.with_hand_over(local_own);
        if let Some(version) = self.config.reported_version.clone() {
            handler = handler.with_reported_version(version);
        }
        self.handler = Some(handler.clone());
        handler
    }

    /// Start the server without listening on any socket: its handler is
    /// reached only through [`PimbleServer::connect_in_process`]
    /// (docs/ONE_CODEBASE_CONTRACT.md seam A; what a browser's worker or a
    /// phone's in-process server will be). The config's paths, intervals and
    /// switches apply as for [`PimbleServer::start`]; nothing about the HTTP
    /// edge does (there is none, so no token or JWT verifier and no agents).
    pub async fn start_in_process(&mut self) -> Result<()> {
        if self.handler.is_some() {
            return Err(crate::ServerError::Server("this server has started already".to_string()));
        }
        let agents = match self.config.agents_path.clone() {
            Some(path) => Some(Arc::new(crate::agents::AgentRegistry::load(&self.config.disk, path).await)),
            None => None,
        };
        let handler = self.make_handler(agents, false, false);
        handler.keystore().load().await;
        Ok(())
    }

    /// A client of this server over an in-process channel, every call
    /// carrying `ext` (the connection's principal: what the HTTP edge
    /// attaches to a WebSocket's calls, here attached by the caller, e.g.
    /// [`crate::service_extensions`]). `None` before the server has started.
    /// The connection is served on a task of the server's runtime until the
    /// client is dropped.
    pub fn connect_in_process(&self, ext: http::Extensions) -> Option<pimble_client::PimbleClient> {
        let handler = Arc::new(self.handler.clone()?);
        let (client_end, server_end) = pimble_rpc::dispatch::session::channel();
        self.config.runtime.spawn(pimble_rpc::dispatch::session::serve(handler, ext, server_end));
        Some(pimble_client::PimbleClient::in_process(client_end))
    }

    /// Serve one connection whose JSON-RPC text arrives on `incoming` and
    /// whose answers and notifications go to `send` (which says `false` once
    /// the other side has gone), every call carrying `ext`: what a transport
    /// other than the in-process channel feeds, such as a browser worker's
    /// `MessagePort` (crates/pimble-worker). Served on a task of the server's
    /// runtime until either side ends; `false` before the server has started.
    pub fn serve_connection<I, F>(&self, ext: http::Extensions, incoming: I, send: F) -> bool
    where
        I: futures::Stream<Item = String> + Unpin + pimble_host::runtime::MaybeSend + 'static,
        F: FnMut(String) -> bool + pimble_host::runtime::MaybeSend + 'static,
    {
        let Some(handler) = self.handler.clone() else { return false };
        self.config.runtime.spawn(pimble_rpc::dispatch::session::serve_with(Arc::new(handler), ext, incoming, send));
        true
    }

    /// Stop the server
    pub async fn stop(&mut self) -> Result<()> {
        // The links first: a link is a task of its own, and one left running
        // after its server stopped keeps applying to (and writing the files
        // of) stores a restarted server in the same process has open again.
        // Then what is shared from this computer: the tunnel, so no member
        // is piped to a face that is going, and the face.
        if let Some(handler) = self.handler.take() {
            // The folder watches too, for the same reason: each is a task
            // that merges into the stores this server holds.
            handler.stop_folder_watches();
            handler.stop_links().await;
            handler.relay().stop().await;
        }

        // Flush any pending tree/content changes before the handle stops, so
        // an app shutdown never depends on the caller remembering to flush
        // (debounced content flushes in particular may still be pending).
        {
            let mut manager = self.store_manager.write().await;
            manager.flush_all().await?;
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(handle) = self.handle.take() {
            handle.stop().map_err(|e| crate::ServerError::Server(e.to_string()))?;
            info!("Pimble server stopped");
        }
        Ok(())
    }

    /// Wait for the server to finish
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn wait(&self) {
        if let Some(ref handle) = self.handle {
            handle.clone().stopped().await;
        }
    }

    /// The address the server is bound to: the actual bound socket
    /// (`Server::local_addr()`) once `start()` has run, so a `ServerConfig`
    /// with port 0 resolves to the OS-assigned port; falls back to the
    /// configured address before `start()`.
    pub fn addr(&self) -> SocketAddr {
        self.local_addr.unwrap_or(self.config.addr)
    }
}

impl Default for PimbleServer {
    fn default() -> Self {
        Self::new()
    }
}

/// The model semantic search embeds chunks with — kept in one place so
/// [`warm_up_embedding_model`] and every `SearchIndex`'s own lazy
/// `Vectorizer` (via the schema's `@vectorize(model: "...")` directive)
/// agree on it.
#[cfg(not(target_arch = "wasm32"))]
const EMBEDDING_MODEL: &str = "all-MiniLM-L6-v2";

/// Point the embedding model cache at a stable, per-user directory and force
/// the model to load once, before any store's `SearchIndex` can start its
/// own background worker and lazily (and, with more than one store opening
/// at once, racily) load it instead. Returns whether semantic search is
/// available for this run: `true` on success, `false` on any failure (no
/// network on first run, disk full, model registry mismatch, ...) — a
/// missing model is not fatal to the server, it just means every store opens
/// keyword-only (see [`RpcHandler::with_semantic_available`]) until a build
/// with the model cached, or with network access, runs again.
///
/// A no-op (always `true`) when pimble-search's `semantic` feature isn't
/// compiled in — both `pimble_search` calls below already are.
///
/// Runs `warm_embedding_model` (a blocking, synchronous ONNX Runtime call)
/// as the runtime's heavy job (a blocking thread on the desktop) so it can't stall the async runtime other RPCs will
/// shortly run on; `start` still awaits it, so no store can open before it
/// resolves.
#[cfg(not(target_arch = "wasm32"))]
async fn warm_up_embedding_model(runtime: &pimble_host::Runtime) -> bool {
    // The machine's, whatever the server's paths: a cache of a download,
    // shared by every server in the process (tests included).
    let cache_dir = pimble_host::Paths::native().data.join("models");

    if let Err(e) = pimble_search::set_model_cache_dir(&cache_dir) {
        tracing::warn!(
            "Could not create embedding model cache dir {:?} ({}); running search keyword-only",
            cache_dir, e
        );
        return false;
    }

    match runtime.run_heavy(|| pimble_search::warm_embedding_model(EMBEDDING_MODEL)).await {
        Ok(Ok(elapsed)) => {
            info!("Embedding model '{}' ready in {:.1}s", EMBEDDING_MODEL, elapsed.as_secs_f64());
            true
        }
        Ok(Err(e)) => {
            tracing::warn!(
                "Embedding model '{}' warm-up failed ({}); running search keyword-only",
                EMBEDDING_MODEL, e
            );
            false
        }
        Err(failed) => {
            tracing::warn!(
                "Embedding model warm-up task panicked ({}); running search keyword-only",
                failed
            );
            false
        }
    }
}

/// Resolve on the first `SIGINT` (Ctrl+C) or, on Unix, `SIGTERM` —
/// whichever arrives first (docs/history/HARDENING_CONTRACT.md decision 13). A
/// container or `systemd stop` sends `SIGTERM`, not `SIGINT`; without this a
/// headless server killed that way skips its flush instead of stopping
/// cleanly.
#[cfg(not(target_arch = "wasm32"))]
pub async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(sigterm) => sigterm,
            Err(e) => {
                tracing::warn!("Could not install a SIGTERM handler ({}); only SIGINT will stop this server", e);
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Start a server and run it until shutdown
#[cfg(not(target_arch = "wasm32"))]
pub async fn run_server(config: ServerConfig) -> Result<()> {
    let mut server = PimbleServer::with_config(config);
    server.start().await?;

    wait_for_shutdown_signal().await;

    info!("Shutting down...");
    server.stop().await?;

    // Flush all stores
    let manager = server.store_manager();
    let mut manager = manager.write().await;
    manager.flush_all().await.map_err(crate::ServerError::Store)?;

    Ok(())
}
