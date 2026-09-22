//! RPC client implementation

use std::path::{Path, PathBuf};

use jsonrpsee::core::client::SubscriptionClientT;
// The transport differs per target but the client type does not:
// `jsonrpsee::ws_client::WsClient` and what `WasmClientBuilder` builds are both
// `jsonrpsee_core::client::Client`, so every RPC method below is written once.
use jsonrpsee::core::client::Client as RpcClient;
#[cfg(target_arch = "wasm32")]
use jsonrpsee::wasm_client::WasmClientBuilder;
#[cfg(not(target_arch = "wasm32"))]
use jsonrpsee::ws_client::{HeaderMap, HeaderValue, WsClientBuilder};
use pimble_core::{AuthMethod, Node, NodeId, RemoteEndpoint, Store, StoreAccess, StoreId, StoreKind, SyncState, Workspace};
use pimble_core::MountRef;
use pimble_rpc::{
    AddRemoteStoreRequest, ApplyEditRequest, CloseStoreRequest, CloudAddHostedStoreRequest,
    CloudHostStoreRequest, CloudHostedStoreInfo, CloudListHostedStoresResponse, CloudRelayStoreRequest, CloudShareInfoResponse, CloudShareInviteRequest, CloudShareNodeRequest, CloudShareRef,
    CloudShareRemoveMemberRequest, CloudSignInRequest, CloudStatusResponse, CloudStopRelayingRequest, DeleteVaultStoreRequest, GetScopesRequest, MemberRole, Scope,
    SetScopeRequest, VaultDocKeys, VaultSetDocKeysRequest,
    CreateMountRequest, CreateNodeRequest, CreateStoreRequest, CreateWorkspaceRequest, DeleteNodeRequest,
    EditOperation, GetChildrenRequest, GetMountStateRequest, GetNodeRequest, GetNodesRequest, GetStoreSyncRequest, GetStoreSyncResponse, SetStoreSyncRequest,
    ListDeletedRequest, ListRemoteStoresRequest, LoadWorkspaceRequest, MoveNodeRequest, MoveNodeResponse, NodeContentChangedNotification, NodeStateVector,
    TransplantNodeRequest, TransplantNodeResponse,
    OpenStoreRequest, PimbleApiClient, RebuildIndexRequest, RemoveReplicaRequest, SaveWorkspaceRequest,
    SearchRequest, SearchResultItem, StoreChangedNotification, SyncNodesRequest, UndeleteNodeRequest,
    UpdateNodeContentRequest, UpdateNodeMetadataRequest, VaultAppendRequest, VaultDocId,
    VaultDocInfo, VaultFetchRequest, VaultFetchResponse, VaultListDocsRequest, VaultListDocsResponse,
    VaultSnapshotRequest, MAX_SYNC_NODE_CONTENTS,
};
use tracing::debug;
use url::Url;

use crate::error::{ClientError, Result};

/// Client for connecting to a Pimble server via WebSocket.
///
/// Uses WebSocket transport to support both RPC calls and subscriptions.
///
/// The same API on both targets. Natively the transport is jsonrpsee's
/// `ws-client` (tokio and tungstenite) and a credential travels in a request
/// header; on `wasm32` it is jsonrpsee's `wasm-client` (the browser's own
/// `WebSocket` through web-sys), where no header can be set and the credential
/// travels as an `access_token` query parameter instead.
pub struct PimbleClient {
    client: RpcClient,
    base_url: Url,
}

/// `url` with `access_token=<token>` added to its query, which is how a
/// browser carries a credential the `WebSocket` API gives it no way to put in
/// a header (the server accepts it beside `Authorization: Bearer` and
/// `X-Api-Key`; see the cloud contract). Any existing `access_token` is
/// replaced, every other query parameter is kept, and the token is
/// percent-encoded by `Url`'s own serializer.
///
/// A URL carrying a credential belongs in a `WebSocket` constructor and
/// nowhere else: it should not be logged, put in a link, or persisted.
pub fn url_with_access_token(url: &Url, token: &str) -> Url {
    let mut out = url.clone();
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != "access_token")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    {
        let mut query = out.query_pairs_mut();
        query.clear();
        for (k, v) in &kept {
            query.append_pair(k, v);
        }
        query.append_pair("access_token", token);
    }
    out
}

/// Turn a failed [`PimbleClient::connect_with_auth`] at `url` into the
/// decision-5 wording (docs/history/HARDENING_CONTRACT.md): a rejected WebSocket
/// handshake carries the HTTP status jsonrpsee's transport reported
/// (`WsHandshakeError::Rejected { status_code }`, whose `Display` is
/// "Connection rejected with status code: NNN" and survives unchanged
/// through `Error::Transport`'s `#[error(transparent)]`), so this matches on
/// that text rather than downcasting through several transport-crate error
/// types this crate doesn't depend on directly. `401` (wrong or missing
/// token) becomes "refused the credentials"; `403` (the edge's `Origin`
/// check) becomes "refused the connection". Anything else keeps the
/// original message.
///
/// On `wasm32` the browser's `WebSocket` never reports the rejecting
/// response's status to script, so neither branch can match and a refused
/// handshake keeps its generic message. That is a browser limitation, not a
/// gap here.
pub fn describe_connect_error(url: &Url, err: &ClientError) -> String {
    let msg = err.to_string();
    // `Url` always renders a bare host with a trailing `/`; people type and
    // read server addresses without one.
    let url = url.as_str().trim_end_matches('/');
    if msg.contains("status code: 401") {
        format!("{} refused the credentials", url)
    } else if msg.contains("status code: 403") {
        format!("{} refused the connection", url)
    } else {
        format!("Failed to connect to {}: {}", url, err)
    }
}

/// A failed call as `ClientError::Rpc`. For an error the server returned,
/// that is the server's own message: jsonrpsee renders the whole
/// `ErrorObject { code, message, data }` otherwise, and that is what a user
/// would read in the app. Transport and protocol failures keep their text
/// (the app recognises a dead connection by it).
fn rpc_error(e: jsonrpsee::core::client::Error) -> ClientError {
    match e {
        jsonrpsee::core::client::Error::Call(obj) => ClientError::Rpc(obj.message().to_string()),
        other => ClientError::Rpc(other.to_string()),
    }
}

impl PimbleClient {
    /// Connect to a Pimble server via WebSocket, sending no authentication.
    /// Equivalent to `connect_with_auth(url, &AuthMethod::None)`.
    ///
    /// Accepts HTTP URLs (http://, https://) and automatically converts them
    /// to WebSocket URLs (ws://, wss://).
    pub async fn connect(url: impl AsRef<str>) -> Result<Self> {
        Self::connect_with_auth(url, &AuthMethod::None).await
    }

    /// Connect to a Pimble server via WebSocket, authenticating the
    /// handshake per `auth` (docs/SYNC_CONTRACT.md decision 10):
    /// `Bearer { token }` sends `Authorization: Bearer <token>`, `ApiKey
    /// { key }` sends `X-Api-Key: <key>`, `None` sends nothing. `OAuth2` is
    /// not supported here and is an error. The server checks nothing yet;
    /// this only plumbs the header through.
    ///
    /// Accepts HTTP URLs (http://, https://) and automatically converts them
    /// to WebSocket URLs (ws://, wss://).
    pub async fn connect_with_auth(url: impl AsRef<str>, auth: &AuthMethod) -> Result<Self> {
        let base_url: Url = url
            .as_ref()
            .parse()
            .map_err(|e| ClientError::Connection(format!("Invalid URL: {}", e)))?;

        // Convert http:// to ws:// for WebSocket connection
        let ws_url = match base_url.scheme() {
            "http" => {
                let mut ws = base_url.clone();
                ws.set_scheme("ws").map_err(|_| ClientError::Connection("Failed to set ws scheme".into()))?;
                ws
            }
            "https" => {
                let mut ws = base_url.clone();
                ws.set_scheme("wss").map_err(|_| ClientError::Connection("Failed to set wss scheme".into()))?;
                ws
            }
            "ws" | "wss" => base_url.clone(),
            other => return Err(ClientError::Connection(format!("Unsupported scheme: {}", other))),
        };

        let client = Self::build_transport(&ws_url, auth).await?;

        // `ws_url` may carry the credential on wasm32, so only the base URL is
        // ever logged.
        debug!("Connected to Pimble server at {}", base_url);

        Ok(Self { client, base_url })
    }

    /// Open the WebSocket to `ws_url`, carrying `auth` in a request header.
    #[cfg(not(target_arch = "wasm32"))]
    async fn build_transport(ws_url: &Url, auth: &AuthMethod) -> Result<RpcClient> {
        let mut builder = WsClientBuilder::default();
        match auth {
            AuthMethod::None => {}
            AuthMethod::Bearer { token } => {
                let mut headers = HeaderMap::new();
                let value = HeaderValue::from_str(&format!("Bearer {}", token))
                    .map_err(|e| ClientError::Connection(format!("Invalid bearer token: {}", e)))?;
                headers.insert("authorization", value);
                builder = builder.set_headers(headers);
            }
            AuthMethod::ApiKey { key } => {
                let mut headers = HeaderMap::new();
                let value = HeaderValue::from_str(key)
                    .map_err(|e| ClientError::Connection(format!("Invalid API key: {}", e)))?;
                headers.insert("x-api-key", value);
                builder = builder.set_headers(headers);
            }
            AuthMethod::OAuth2 { .. } => {
                return Err(ClientError::Connection(
                    "OAuth2 auth is not supported by connect_with_auth".into(),
                ));
            }
            AuthMethod::CloudSession { .. } => {
                // docs/CRYPTO_CONTRACT.md: a `CloudSession` is a long-lived
                // accounts-service session, not something a WebSocket
                // handshake carries directly. A caller (a sync/vault link)
                // must mint a short-lived JWT via `POST {url}/api/v1/token`
                // first and connect with `AuthMethod::Bearer { token: jwt }`
                // instead.
                return Err(ClientError::Connection(
                    "CloudSession auth must be resolved to a Bearer token (via POST /api/v1/token) before connecting".into(),
                ));
            }
        }

        builder
            .build(ws_url)
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))
    }

    /// Open the browser's `WebSocket` to `ws_url`, carrying `auth` as an
    /// `access_token` query parameter: the `WebSocket` constructor takes a URL
    /// and a subprotocol list and nothing else, so a header is not available.
    /// `Bearer` and `ApiKey` both put their secret there and the server tries
    /// each of its verifiers against it.
    #[cfg(target_arch = "wasm32")]
    async fn build_transport(ws_url: &Url, auth: &AuthMethod) -> Result<RpcClient> {
        let ws_url = match auth {
            AuthMethod::None => ws_url.clone(),
            AuthMethod::Bearer { token } => url_with_access_token(ws_url, token),
            AuthMethod::ApiKey { key } => url_with_access_token(ws_url, key),
            AuthMethod::OAuth2 { .. } => {
                return Err(ClientError::Connection(
                    "OAuth2 auth is not supported by connect_with_auth".into(),
                ));
            }
            AuthMethod::CloudSession { .. } => {
                return Err(ClientError::Connection(
                    "CloudSession auth must be resolved to a Bearer token (via POST /api/v1/token) before connecting".into(),
                ));
            }
        };

        WasmClientBuilder::default()
            .build(ws_url.as_str())
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))
    }

    /// Whether the WebSocket connection is still up. False once the server
    /// has gone away (the background task has closed), even before any call
    /// has failed.
    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    /// Resolves once the connection is lost (or immediately if it already
    /// is). Lets a caller notice a dead server without waiting for a call to
    /// fail.
    pub async fn on_disconnect(&self) {
        self.client.on_disconnect().await
    }

    /// Get the server URL
    pub fn url(&self) -> &Url {
        &self.base_url
    }

    /// Connect with `auth`, and on a `401`/`403` handshake rejection return
    /// the decision-5 wording (docs/history/HARDENING_CONTRACT.md) naming `url`
    /// instead of the raw transport error: "refused the credentials" for a
    /// wrong or missing token, "refused the connection" for the edge's
    /// `Origin` check. Any other failure keeps its own message. Used
    /// wherever a server connects to a remote on a caller's behalf
    /// (`addRemoteStore`, `setStoreSync`, `listRemoteStores`, a sync link) so
    /// the error a client eventually sees always explains which of the two
    /// happened.
    pub async fn connect_with_auth_describing_errors(url: &Url, auth: &AuthMethod) -> Result<Self> {
        Self::connect_with_auth(url.as_str(), auth)
            .await
            .map_err(|e| ClientError::Connection(describe_connect_error(url, &e)))
    }

    // ========================================================================
    // Store Operations
    // ========================================================================

    /// Create a new local `Plain` store with a freshly generated id. See
    /// [`PimbleClient::create_store_with`] for a chosen `kind`/`store_id`
    /// (docs/CRYPTO_CONTRACT.md).
    pub async fn create_store(&self, path: impl AsRef<Path>, name: impl Into<String>) -> Result<(StoreId, NodeId)> {
        self.create_store_with(path, name, StoreKind::Plain, None).await
    }

    /// Create a new store of `kind` (`Plain` or `Vault`), under `store_id`
    /// when given (refused if that id is already open) or a freshly
    /// generated one otherwise (docs/CRYPTO_CONTRACT.md: the accounts
    /// service uses `store_id` to create a hosted twin of a local store
    /// under the local store's own id). The returned `NodeId` is a `Vault`
    /// store's manifest root id, which has no meaning there — a vault store
    /// has no tree on this server.
    pub async fn create_store_with(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        kind: StoreKind,
        store_id: Option<StoreId>,
    ) -> Result<(StoreId, NodeId)> {
        let request = CreateStoreRequest {
            path: path.as_ref().to_path_buf(),
            name: name.into(),
            kind,
            store_id,
        };

        let response = self
            .client
            .create_store(request)
            .await
            .map_err(rpc_error)?;

        Ok((response.store_id, response.root_node_id))
    }

    /// Open an existing store
    pub async fn open_store(&self, path: impl AsRef<Path>) -> Result<Store> {
        let request = OpenStoreRequest {
            path: path.as_ref().to_path_buf(),
        };

        let response = self
            .client
            .open_store(request)
            .await
            .map_err(rpc_error)?;

        Ok(response.store)
    }

    /// Close a store
    pub async fn close_store(&self, store_id: StoreId) -> Result<()> {
        let request = CloseStoreRequest { store_id };

        self.client
            .close_store(request)
            .await
            .map_err(rpc_error)?;

        Ok(())
    }

    /// List all open stores
    pub async fn list_stores(&self) -> Result<Vec<Store>> {
        let response = self
            .client
            .list_stores()
            .await
            .map_err(rpc_error)?;

        Ok(response.stores)
    }

    // ========================================================================
    // Node Operations
    // ========================================================================

    /// Get a single node
    pub async fn get_node(&self, store_id: StoreId, node_id: NodeId) -> Result<Node> {
        let request = GetNodeRequest { store_id, node_id };

        let response = self
            .client
            .get_node(request)
            .await
            .map_err(rpc_error)?;

        Ok(response.node)
    }

    /// Get multiple nodes
    pub async fn get_nodes(&self, store_id: StoreId, node_ids: Vec<NodeId>) -> Result<Vec<Node>> {
        let request = GetNodesRequest { store_id, node_ids };

        let response = self
            .client
            .get_nodes(request)
            .await
            .map_err(rpc_error)?;

        Ok(response.nodes)
    }

    /// Create a new node
    pub async fn create_node(
        &self,
        store_id: StoreId,
        parent_id: Option<NodeId>,
        node_type: impl Into<String>,
        title: impl Into<String>,
    ) -> Result<NodeId> {
        let request = CreateNodeRequest {
            store_id,
            parent_id,
            node_type: node_type.into(),
            title: title.into(),
        };

        let response = self
            .client
            .create_node(request)
            .await
            .map_err(rpc_error)?;

        Ok(response.node_id)
    }

    /// Update a node's metadata
    pub async fn update_node_metadata(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        metadata: pimble_core::NodeMetadata,
    ) -> Result<()> {
        let request = UpdateNodeMetadataRequest {
            store_id,
            node_id,
            metadata,
        };

        self.client
            .update_node_metadata(request)
            .await
            .map_err(rpc_error)?;

        Ok(())
    }

    /// Seed a node's content with a whole document snapshot (`updateNodeContent`).
    /// A merge, not a replacement: only right for a node whose content was
    /// never written (see `UpdateNodeContentRequest`).
    pub async fn set_node_content_bytes(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        content: Vec<u8>,
        client_id: Option<String>,
    ) -> Result<()> {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&content);

        let request = UpdateNodeContentRequest {
            store_id,
            node_id,
            content: encoded,
            client_id,
        };

        self.client
            .update_node_content(request)
            .await
            .map_err(rpc_error)?;

        Ok(())
    }

    /// Delete a node and its subtree (a tombstone; see `undelete_node`).
    pub async fn delete_node(&self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        let request = DeleteNodeRequest { store_id, node_id };

        self.client
            .delete_node(request)
            .await
            .map_err(rpc_error)?;

        Ok(())
    }

    /// Bring a deleted node and what the same deletion took with it back.
    pub async fn undelete_node(&self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        let request = UndeleteNodeRequest { store_id, node_id };

        self.client
            .undelete_node(request)
            .await
            .map_err(rpc_error)?;

        Ok(())
    }

    /// Move a node to a new parent. The answer names the id the node has
    /// now: `node_id` after a plain move, a new one after a transplant (the
    /// move left a share, docs/MOVE_CONTRACT.md), with the shares it left.
    pub async fn move_node(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> Result<MoveNodeResponse> {
        let request = MoveNodeRequest {
            store_id,
            node_id,
            new_parent_id,
            position,
        };

        self.client
            .move_node(request)
            .await
            .map_err(rpc_error)
    }

    /// Move a node into another store: always a transplant
    /// (docs/MOVE_CONTRACT.md "Between stores"). The answer names the new
    /// root's id in `to_store_id`.
    pub async fn transplant_node(
        &self,
        from_store_id: StoreId,
        node_id: NodeId,
        to_store_id: StoreId,
        new_parent_id: NodeId,
        position: Option<usize>,
    ) -> Result<TransplantNodeResponse> {
        let request = TransplantNodeRequest { from_store_id, node_id, to_store_id, new_parent_id, position };

        self.client
            .transplant_node(request)
            .await
            .map_err(rpc_error)
    }

    /// What "Recently Deleted..." shows for a store.
    pub async fn list_deleted(&self, store_id: StoreId) -> Result<Vec<pimble_core::DeletedNode>> {
        let request = ListDeletedRequest { store_id };

        self.client
            .list_deleted(request)
            .await
            .map(|answer| answer.nodes)
            .map_err(rpc_error)
    }

    /// Get children of a node. Returns the canonical store the children live
    /// in (the mount's source store when `node_id` is a mount point) and the
    /// children themselves; address each child by `(store, child.id)`.
    pub async fn get_children(&self, store_id: StoreId, node_id: NodeId) -> Result<(StoreId, Vec<Node>)> {
        let request = GetChildrenRequest { store_id, node_id };

        let response = self
            .client
            .get_children(request)
            .await
            .map_err(rpc_error)?;

        Ok((response.store_id, response.children))
    }

    // ========================================================================
    // Mount Operations
    // ========================================================================

    /// Create a mount point node in a store
    pub async fn create_mount(
        &self,
        store_id: StoreId,
        parent_id: NodeId,
        source_store_id: StoreId,
        source_node_id: NodeId,
        title: Option<String>,
    ) -> Result<(NodeId, MountRef)> {
        let request = CreateMountRequest {
            store_id,
            parent_id,
            source_store_id,
            source_node_id,
            title,
        };

        let response = self
            .client
            .create_mount(request)
            .await
            .map_err(rpc_error)?;

        Ok((response.node_id, response.mount_ref))
    }

    /// Get the state of a mount point
    pub async fn get_mount_state(
        &self,
        store_id: StoreId,
        node_id: NodeId,
    ) -> Result<(pimble_core::MountState, MountRef)> {
        let request = GetMountStateRequest { store_id, node_id };

        let response = self
            .client
            .get_mount_state(request)
            .await
            .map_err(rpc_error)?;

        Ok((response.state, response.mount_ref))
    }

    // ========================================================================
    // Replica Sync Operations (docs/SYNC_CONTRACT.md)
    // ========================================================================

    /// Create a local replica of `remote_store_id` as held by `remote`,
    /// linked to it. Returns the opened store. `path: None` lets the server
    /// choose the replica's location (`<data dir>/pimble/replicas/<store
    /// id>.pimble`); `Some` places it there instead (refused if it exists).
    pub async fn add_remote_store(
        &self,
        remote: RemoteEndpoint,
        remote_store_id: StoreId,
        path: Option<PathBuf>,
    ) -> Result<Store> {
        let request = AddRemoteStoreRequest {
            remote,
            remote_store_id,
            path,
        };
        let response = self
            .client
            .add_remote_store(request)
            .await
            .map_err(rpc_error)?;
        Ok(response.store)
    }

    /// Link a local store to its twin on `remote` (`Some`) or unlink it
    /// (`None`). Returns the link and its state.
    pub async fn set_store_sync(
        &self,
        store_id: StoreId,
        remote: Option<RemoteEndpoint>,
    ) -> Result<(Option<RemoteEndpoint>, SyncState)> {
        let request = SetStoreSyncRequest { store_id, remote };
        let response = self
            .client
            .set_store_sync(request)
            .await
            .map_err(rpc_error)?;
        Ok((response.remote, response.state))
    }

    /// Like [`PimbleClient::set_store_sync`], but also reports the link's
    /// mode (`Plain` or `Vault`), the way [`PimbleClient::get_store_sync_with_mode`]
    /// does for a read. The two-element wrapper stays for its callers.
    pub async fn set_store_sync_with_mode(
        &self,
        store_id: StoreId,
        remote: Option<RemoteEndpoint>,
    ) -> Result<(Option<RemoteEndpoint>, SyncState, StoreKind)> {
        let request = SetStoreSyncRequest { store_id, remote };
        let response = self
            .client
            .set_store_sync(request)
            .await
            .map_err(rpc_error)?;
        Ok((response.remote, response.state, response.sync_mode))
    }

    /// A store's sync link (`None` when unlinked) and its current state.
    pub async fn get_store_sync(&self, store_id: StoreId) -> Result<(Option<RemoteEndpoint>, SyncState)> {
        let request = GetStoreSyncRequest { store_id };
        let response = self
            .client
            .get_store_sync(request)
            .await
            .map_err(rpc_error)?;
        Ok((response.remote, response.state))
    }

    /// Like [`PimbleClient::get_store_sync`], but also reports whether the
    /// link is an ordinary `Plain` sync link or a `Vault` link
    /// (docs/CRYPTO_CONTRACT.md) — a separate method so the existing
    /// two-element tuple callers (and `BackendCommand::GetStoreSync`'s event
    /// shape) don't have to change.
    pub async fn get_store_sync_with_mode(&self, store_id: StoreId) -> Result<(Option<RemoteEndpoint>, SyncState, StoreKind)> {
        let request = GetStoreSyncRequest { store_id };
        let response = self
            .client
            .get_store_sync(request)
            .await
            .map_err(rpc_error)?;
        Ok((response.remote, response.state, response.sync_mode))
    }

    /// Like [`PimbleClient::get_store_sync_with_mode`], with what this
    /// device may change in the store (`Store::access`: a share's reader
    /// reads, docs/NODE_DOCUMENT_CONTRACT.md section 5). A separate method
    /// for the same reason.
    pub async fn get_store_sync_with_access(&self, store_id: StoreId) -> Result<(Option<RemoteEndpoint>, SyncState, StoreKind, StoreAccess)> {
        let response = self.client.get_store_sync(GetStoreSyncRequest { store_id }).await.map_err(rpc_error)?;
        Ok((response.remote, response.state, response.sync_mode, response.access))
    }

    /// A store's sync answer whole: the link, its state and mode, and what
    /// this device may change in the store (`access`, `read_only_roots`).
    /// What the app asks with, since it keeps all of it.
    pub async fn get_store_sync_response(&self, store_id: StoreId) -> Result<GetStoreSyncResponse> {
        self.client.get_store_sync(GetStoreSyncRequest { store_id }).await.map_err(rpc_error)
    }

    /// [`PimbleClient::set_store_sync`], answering whole like
    /// [`PimbleClient::get_store_sync_response`].
    pub async fn set_store_sync_response(&self, store_id: StoreId, remote: Option<RemoteEndpoint>) -> Result<GetStoreSyncResponse> {
        self.client.set_store_sync(SetStoreSyncRequest { store_id, remote }).await.map_err(rpc_error)
    }

    /// The stores `remote` has open, fetched by the server this client is
    /// connected to (with `remote.auth`, or its saved credential for that
    /// remote when `remote.auth` is `None`).
    pub async fn list_remote_stores(&self, remote: RemoteEndpoint) -> Result<Vec<Store>> {
        let request = ListRemoteStoresRequest { remote };
        let response = self
            .client
            .list_remote_stores(request)
            .await
            .map_err(rpc_error)?;
        Ok(response.stores)
    }

    /// Stop a replica's sync link, close it, and delete its directory.
    /// `force` removes a replica whose link is not `Synced` (its unsynced
    /// changes are lost).
    pub async fn remove_replica(&self, store_id: StoreId, force: bool) -> Result<()> {
        let request = RemoveReplicaRequest { store_id, force };
        self.client
            .remove_replica(request)
            .await
            .map_err(rpc_error)?;
        Ok(())
    }

    // ========================================================================
    // Workspace Operations
    // ========================================================================

    /// Load a workspace from file
    pub async fn load_workspace(&self, path: impl AsRef<Path>) -> Result<Workspace> {
        let request = LoadWorkspaceRequest {
            path: path.as_ref().to_path_buf(),
        };

        let response = self
            .client
            .load_workspace(request)
            .await
            .map_err(rpc_error)?;

        Ok(response.workspace)
    }

    /// Save a workspace to file
    pub async fn save_workspace(&self, workspace: Workspace, path: impl AsRef<Path>) -> Result<()> {
        let request = SaveWorkspaceRequest {
            workspace,
            path: path.as_ref().to_path_buf(),
        };

        self.client
            .save_workspace(request)
            .await
            .map_err(rpc_error)?;

        Ok(())
    }

    /// Create a new workspace
    pub async fn create_workspace(
        &self,
        name: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<Workspace> {
        let request = CreateWorkspaceRequest {
            name: name.into(),
            path: path.as_ref().to_path_buf(),
        };

        let response = self
            .client
            .create_workspace(request)
            .await
            .map_err(rpc_error)?;

        Ok(response.workspace)
    }

    // ========================================================================
    // Edit Operations (collaborative editing)
    // ========================================================================

    /// Apply an edit operation to a node and broadcast to other clients.
    pub async fn apply_edit(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        client_id: &str,
        operation: EditOperation,
    ) -> Result<()> {
        let request = ApplyEditRequest {
            store_id,
            node_id,
            client_id: client_id.to_string(),
            operation,
        };

        self.client
            .apply_edit(request)
            .await
            .map_err(rpc_error)?;

        Ok(())
    }

    // ========================================================================
    // Sync Operations
    // ========================================================================

    /// Sync one node's document: send our yrs state vector, get back
    /// everything the server has beyond it plus the server's own state
    /// vector. Stateless on both ends. A one-node `syncNodes`; an error if
    /// the server does not have the node.
    pub async fn sync_node_content(
        &self,
        store_id: StoreId,
        node_id: NodeId,
        state_vector: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut nodes = self.sync_node_contents(store_id, &[(node_id, state_vector.to_vec())]).await?;
        match nodes.pop() {
            Some((id, diff, server_sv)) if id == node_id => Ok((diff, server_sv)),
            _ => Err(ClientError::Rpc(format!("Node not found: {}", node_id))),
        }
    }

    /// Sync the documents of many nodes: for each `(node, state vector)`,
    /// get back `(node, diff, server state vector)`, in request order, for
    /// every node the server has (others are left out). The name is from
    /// the days documents held content only; it is `sync_nodes` without the
    /// unknown-id listing, kept for its callers.
    pub async fn sync_node_contents(
        &self,
        store_id: StoreId,
        nodes: &[(NodeId, Vec<u8>)],
    ) -> Result<Vec<(NodeId, Vec<u8>, Vec<u8>)>> {
        let (answers, _) = self.sync_nodes(store_id, nodes, false).await?;
        Ok(answers)
    }

    /// Sync node documents (docs/NODE_DOCUMENT_CONTRACT.md section 4): for
    /// each `(node, state vector)` named, `(node, diff, server state
    /// vector)` back, in request order, for every node the server has; and,
    /// with `list_unknown`, the ids of every document the server holds that
    /// `nodes` did not name (tombstones included). Splits the list into
    /// requests of at most `MAX_SYNC_NODE_CONTENTS` nodes; the unknown ids
    /// are asked for once and every named id is taken out of the answer, so
    /// the split is invisible to the caller. `nodes` may be empty: with
    /// `list_unknown` that asks for every id the store holds.
    pub async fn sync_nodes(
        &self,
        store_id: StoreId,
        nodes: &[(NodeId, Vec<u8>)],
        list_unknown: bool,
    ) -> Result<(Vec<(NodeId, Vec<u8>, Vec<u8>)>, Vec<NodeId>)> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;

        let mut answers = Vec::with_capacity(nodes.len());
        let mut unknown = Vec::new();
        // One request even for an empty list, so `list_unknown` on nothing
        // still answers with everything.
        let chunks: Vec<&[(NodeId, Vec<u8>)]> = if nodes.is_empty() {
            vec![&[]]
        } else {
            nodes.chunks(MAX_SYNC_NODE_CONTENTS).collect()
        };
        for (i, chunk) in chunks.into_iter().enumerate() {
            let request = SyncNodesRequest {
                store_id,
                nodes: chunk
                    .iter()
                    .map(|(node_id, sv)| NodeStateVector { node_id: *node_id, state_vector: b64.encode(sv) })
                    .collect(),
                list_unknown: list_unknown && i == 0,
            };

            let response = self
                .client
                .sync_nodes(request)
                .await
                .map_err(rpc_error)?;

            for entry in response.nodes {
                let diff = b64
                    .decode(&entry.diff)
                    .map_err(|e| ClientError::Rpc(format!("Invalid base64 diff: {}", e)))?;
                let server_sv = b64
                    .decode(&entry.state_vector)
                    .map_err(|e| ClientError::Rpc(format!("Invalid base64 state vector: {}", e)))?;
                answers.push((entry.node_id, diff, server_sv));
            }
            unknown.extend(response.unknown_ids);
        }

        if list_unknown {
            // The server answered relative to the first chunk alone; every
            // id a later chunk named is known to the caller.
            let named: std::collections::HashSet<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
            unknown.retain(|id| !named.contains(id));
        }

        Ok((answers, unknown))
    }

    // ========================================================================
    // Subscription Operations
    // ========================================================================

    /// Subscribe to store changes (tree structure, metadata).
    /// Returns a subscription stream that yields `StoreChangedNotification`.
    pub async fn subscribe_store_changes(
        &self,
        store_id: StoreId,
    ) -> Result<jsonrpsee::core::client::Subscription<StoreChangedNotification>> {
        use jsonrpsee::core::params::ArrayParams;

        let mut params = ArrayParams::new();
        params.insert(store_id)?;

        let sub = self.client
            .subscribe::<StoreChangedNotification, _>(
                "pimble_subscribeStoreChanges",
                params,
                "pimble_unsubscribeStoreChanges",
            )
            .await
            .map_err(rpc_error)?;

        Ok(sub)
    }

    /// Subscribe to node content changes.
    /// Returns a subscription stream that yields `NodeContentChangedNotification`.
    pub async fn subscribe_node_changes(
        &self,
        store_id: StoreId,
        node_id: NodeId,
    ) -> Result<jsonrpsee::core::client::Subscription<NodeContentChangedNotification>> {
        use jsonrpsee::core::params::ArrayParams;

        let mut params = ArrayParams::new();
        params.insert(store_id)?;
        params.insert(node_id)?;

        let sub = self.client
            .subscribe::<NodeContentChangedNotification, _>(
                "pimble_subscribeNodeChanges",
                params,
                "pimble_unsubscribeNodeChanges",
            )
            .await
            .map_err(rpc_error)?;

        Ok(sub)
    }

    // ========================================================================
    // Search Operations
    // ========================================================================

    /// Search across stores
    pub async fn search(
        &self,
        query: impl Into<String>,
        stores: Vec<StoreId>,
        semantic: bool,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>> {
        let request = SearchRequest {
            query: query.into(),
            stores,
            semantic,
            limit,
        };

        let response = self
            .client
            .search(request)
            .await
            .map_err(rpc_error)?;

        Ok(response.results)
    }

    /// Rebuild a store's search index from scratch: delete the on-disk
    /// index and re-index every node from the store's documents. Returns
    /// the number of nodes indexed.
    pub async fn rebuild_index(&self, store_id: StoreId) -> Result<usize> {
        let request = RebuildIndexRequest { store_id };

        let response = self
            .client
            .rebuild_index(request)
            .await
            .map_err(rpc_error)?;

        Ok(response.indexed)
    }

    // ========================================================================
    // Vault (encrypted store) Operations — docs/CRYPTO_CONTRACT.md
    // ========================================================================
    //
    // Thin pass-throughs by design: the server stores and relays opaque blobs,
    // and every byte of meaning is put there by `pimble-crypto` on the caller's
    // side. Nothing here encrypts, decrypts, or inspects a blob.

    /// Append an encrypted update to a vault document. Answers its sequence number.
    pub async fn vault_append(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        blob: String,
    ) -> Result<u64> {
        self.vault_append_from(store_id, doc_id, blob, None).await
    }

    /// Like [`PimbleClient::vault_append`], but attributes the append to
    /// `client_id`: it rides the `VaultAppended` notification's
    /// `source_client_id`, the same way `applyEdit`'s `client_id` does, so a
    /// caller (e.g. the desktop `VaultLink`, with `vault-link:<uuid>`) can
    /// drop its own echo by identity, keeping a seen-seq set as a second
    /// guard. A separate method rather than a new required parameter on
    /// `vault_append`, so every existing caller (the web vault client, the
    /// CLI, `VaultLink`) keeps compiling unchanged.
    pub async fn vault_append_from(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        blob: String,
        client_id: Option<String>,
    ) -> Result<u64> {
        self.vault_append_new(store_id, doc_id, blob, client_id, None).await
    }

    /// [`PimbleClient::vault_append_from`] naming the new document's parent,
    /// which a member whose grant is scoped to a subtree must do for a
    /// document the store does not have yet (docs/NODE_DOCUMENT_CONTRACT.md
    /// section 5, "Scope sets").
    pub async fn vault_append_new(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        blob: String,
        client_id: Option<String>,
        parent_id: Option<NodeId>,
    ) -> Result<u64> {
        self.vault_append_with_keys(store_id, doc_id, blob, client_id, parent_id, None).await
    }

    /// [`PimbleClient::vault_append_new`] carrying the new document's wrapped
    /// data key, which the server stores together with the first append
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Keys"): the way a document
    /// is created, so that no blob is ever under a key the server lacks.
    pub async fn vault_append_with_keys(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        blob: String,
        client_id: Option<String>,
        parent_id: Option<NodeId>,
        keys: Option<VaultDocKeys>,
    ) -> Result<u64> {
        let response = self
            .client
            .vault_append(VaultAppendRequest { store_id, doc_id, blob, client_id, parent_id, keys })
            .await
            .map_err(rpc_error)?;
        Ok(response.seq)
    }

    // ── Sharing on node documents, docs/NODE_DOCUMENT_CONTRACT.md section 5 ──

    /// Set a document's wrapped data keys.
    pub async fn vault_set_doc_keys(&self, store_id: StoreId, doc_id: VaultDocId, keys: VaultDocKeys) -> Result<()> {
        self.client.vault_set_doc_keys(VaultSetDocKeysRequest { store_id, doc_id, keys }).await.map_err(rpc_error)?;
        Ok(())
    }

    /// Publish (or remove) a share's scope on the hosted server.
    pub async fn set_scope(&self, store_id: StoreId, scope: Scope, remove: bool) -> Result<()> {
        self.client.set_scope(SetScopeRequest { store_id, scope, remove }).await.map_err(rpc_error)?;
        Ok(())
    }

    /// The store's published scopes.
    pub async fn get_scopes(&self, store_id: StoreId) -> Result<Vec<Scope>> {
        let response = self.client.get_scopes(GetScopesRequest { store_id }).await.map_err(rpc_error)?;
        Ok(response.scopes)
    }

    /// Share a node of a local store, named `name`. `Service`-only.
    pub async fn cloud_share_node(&self, store_id: StoreId, node_id: NodeId, name: &str) -> Result<CloudShareInfoResponse> {
        self.client
            .cloud_share_node(CloudShareNodeRequest { store_id, node_id, name: name.to_string() })
            .await
            .map_err(rpc_error)
    }

    /// A shared node's share and its members.
    pub async fn cloud_share_info(&self, store_id: StoreId, node_id: NodeId) -> Result<CloudShareInfoResponse> {
        self.client.cloud_share_info(CloudShareRef { store_id, node_id }).await.map_err(rpc_error)
    }

    /// Invite an address to a share, or change the role it has.
    pub async fn cloud_share_invite(&self, store_id: StoreId, node_id: NodeId, email: &str, role: MemberRole) -> Result<CloudShareInfoResponse> {
        self.client
            .cloud_share_invite(CloudShareInviteRequest { store_id, node_id, email: email.to_string(), role })
            .await
            .map_err(rpc_error)
    }

    /// Remove a member or a pending invitation from a share.
    pub async fn cloud_share_remove_member(&self, store_id: StoreId, node_id: NodeId, email: &str) -> Result<CloudShareInfoResponse> {
        self.client
            .cloud_share_remove_member(CloudShareRemoveMemberRequest { store_id, node_id, email: email.to_string() })
            .await
            .map_err(rpc_error)
    }

    /// Stop sharing a node.
    pub async fn cloud_stop_sharing(&self, store_id: StoreId, node_id: NodeId) -> Result<()> {
        self.client.cloud_stop_sharing(CloudShareRef { store_id, node_id }).await.map_err(rpc_error)?;
        Ok(())
    }

    /// Everything a vault document holds beyond `after_seq`: the snapshot (when
    /// its seq is greater) and the updates after it.
    pub async fn vault_fetch(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        after_seq: u64,
    ) -> Result<VaultFetchResponse> {
        self.client
            .vault_fetch(VaultFetchRequest { store_id, doc_id, after_seq })
            .await
            .map_err(rpc_error)
    }

    /// Store a snapshot covering every update up to and including `upto_seq`.
    /// `upto_seq` must be a [`pimble_rpc::VaultCursor::applied_through`] value:
    /// the server deletes every log entry at or below it, so the blob has to
    /// reflect all of them. The request says so (`covers_prefix`).
    pub async fn vault_snapshot(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        upto_seq: u64,
        blob: String,
    ) -> Result<()> {
        self.client
            .vault_snapshot(VaultSnapshotRequest { store_id, doc_id, upto_seq, blob, covers_prefix: true })
            .await
            .map_err(rpc_error)?;
        Ok(())
    }

    /// Every document in a vault store, with its head and snapshot sequence.
    pub async fn vault_list_docs(&self, store_id: StoreId) -> Result<Vec<VaultDocInfo>> {
        let response = self
            .client
            .vault_list_docs(VaultListDocsRequest { store_id })
            .await
            .map_err(rpc_error)?;
        Ok(response.docs)
    }

    /// [`PimbleClient::vault_list_docs`] with the rest of the answer: the
    /// `epoch` that says which log the sequence numbers belong to.
    pub async fn vault_list_docs_response(&self, store_id: StoreId) -> Result<VaultListDocsResponse> {
        self.client.vault_list_docs(VaultListDocsRequest { store_id }).await.map_err(rpc_error)
    }

    // ========================================================================
    // Cloud (Pimble Cloud account) Operations, docs/CRYPTO_CONTRACT.md
    // ========================================================================

    /// Sign in to a Pimble Cloud account.
    pub async fn cloud_sign_in(&self, url: impl Into<String>, email: impl Into<String>, password: impl Into<String>) -> Result<()> {
        self.client
            .cloud_sign_in(CloudSignInRequest { url: url.into(), email: email.into(), password: password.into() })
            .await
            .map_err(rpc_error)?;
        Ok(())
    }

    /// Forget the signed-in account.
    pub async fn cloud_sign_out(&self) -> Result<()> {
        self.client.cloud_sign_out().await.map_err(rpc_error)?;
        Ok(())
    }

    /// Whether an account is signed in, and as whom.
    pub async fn cloud_status(&self) -> Result<CloudStatusResponse> {
        self.client.cloud_status().await.map_err(rpc_error)
    }

    /// Host a local store's encrypted twin on Pimble Cloud.
    pub async fn cloud_host_store(&self, store_id: StoreId) -> Result<StoreId> {
        let response = self.client.cloud_host_store(CloudHostStoreRequest { store_id }).await.map_err(rpc_error)?;
        Ok(response.store_id)
    }

    /// Share a local store from this computer, with nothing uploaded
    /// (docs/RELAY_CONTRACT.md): its encrypted twin stays on this machine and
    /// Pimble Cloud's relay pipes members' connections to it.
    pub async fn cloud_relay_store(&self, store_id: StoreId) -> Result<StoreId> {
        let response = self.client.cloud_relay_store(CloudRelayStoreRequest { store_id }).await.map_err(rpc_error)?;
        Ok(response.store_id)
    }

    /// Stop sharing a store from this computer. Refused while it has shares.
    pub async fn cloud_stop_relaying(&self, store_id: StoreId) -> Result<()> {
        self.client.cloud_stop_relaying(CloudStopRelayingRequest { store_id }).await.map_err(rpc_error)?;
        Ok(())
    }

    /// Every store the signed-in account has a grant on.
    pub async fn cloud_list_hosted_stores(&self) -> Result<Vec<CloudHostedStoreInfo>> {
        let response = self.client.cloud_list_hosted_stores().await.map_err(rpc_error)?;
        Ok(response.stores)
    }

    /// [`PimbleClient::cloud_list_hosted_stores`] with the rest of the
    /// answer: which of the rows are relay-tier
    /// (`CloudListHostedStoresResponse::tier_of`).
    pub async fn cloud_list_hosted_stores_response(&self) -> Result<CloudListHostedStoresResponse> {
        self.client.cloud_list_hosted_stores().await.map_err(rpc_error)
    }

    /// Add an already-hosted store as a local replica.
    pub async fn cloud_add_hosted_store(&self, store_id: StoreId) -> Result<Store> {
        let response = self.client.cloud_add_hosted_store(CloudAddHostedStoreRequest { store_id }).await.map_err(rpc_error)?;
        Ok(response.store)
    }

    /// Hosted side: close a vault store and delete its directory
    /// (docs/NODE_DOCUMENT_CONTRACT.md section 5). `Service`-only.
    pub async fn delete_vault_store(&self, store_id: StoreId) -> Result<()> {
        self.client.delete_vault_store(DeleteVaultStoreRequest { store_id }).await.map_err(rpc_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_goes_in_the_query() {
        let url: Url = "wss://pimble.example/rpc".parse().unwrap();
        let out = url_with_access_token(&url, "abc123");
        assert_eq!(out.as_str(), "wss://pimble.example/rpc?access_token=abc123");
    }

    #[test]
    fn access_token_keeps_other_parameters_and_replaces_its_own() {
        let url: Url = "wss://pimble.example/rpc?store=one&access_token=stale".parse().unwrap();
        let out = url_with_access_token(&url, "fresh");
        let pairs: Vec<(String, String)> = out
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("store".to_string(), "one".to_string()),
                ("access_token".to_string(), "fresh".to_string()),
            ]
        );
    }

    #[test]
    fn access_token_is_percent_encoded() {
        let url: Url = "wss://pimble.example/rpc".parse().unwrap();
        // A JWT never contains these, but a static token set by hand might.
        let out = url_with_access_token(&url, "a b&c=d");
        assert_eq!(out.query().unwrap(), "access_token=a+b%26c%3Dd");
        assert_eq!(
            out.query_pairs().next().unwrap().1.into_owned(),
            "a b&c=d",
            "the server reads back exactly what was put in"
        );
    }
}
