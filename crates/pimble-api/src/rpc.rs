//! The API over a Pimble server's JSON-RPC (`pimble_client::PimbleClient`),
//! native and in the browser. How a connection is made and remade is the
//! caller's ([`Connector`]): the desktop's processes join or start the local
//! server (`pimble_server::local`), the browser mints a token first. Every
//! call asks the connector for a live client, so a server that went away is
//! reconnected to on the next call, and a subscription that ended with the
//! old connection is made again by the API above.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use pimble_client::PimbleClient;
use pimble_core::{BlobId, BlobUrl, DeletedNode, LinkResolution, Node, NodeId, Place, Store, StoreId};
use pimble_rpc::{EditOperation, PresenceFrame, SearchResultItem};

use crate::backend::{Backend, ChangeStream, MaybeSend, MaybeSync, Moved, NodeInit, StoreChange, SyncedDoc};
use crate::ApiError;

/// How the RPC backend gets a live client.
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait Connector: MaybeSend + MaybeSync {
    /// A connected client: the one held, or a new connection when it closed.
    async fn client(&self) -> Result<Arc<PimbleClient>, ApiError>;
}

/// A connection made once, by someone else (a test, a page that owns its
/// own reconnect loop). It is not remade here.
pub struct Connected(pub Arc<PimbleClient>);

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl Connector for Connected {
    async fn client(&self) -> Result<Arc<PimbleClient>, ApiError> {
        Ok(self.0.clone())
    }
}

pub struct RpcBackend {
    connector: Box<dyn Connector>,
}

impl RpcBackend {
    pub fn new(connector: impl Connector + 'static) -> Self {
        Self { connector: Box::new(connector) }
    }

    /// Over a client that is already connected.
    pub fn over(client: PimbleClient) -> Self {
        Self::new(Connected(Arc::new(client)))
    }

    async fn client(&self) -> Result<Arc<PimbleClient>, ApiError> {
        self.connector.client().await
    }
}

/// A client error as the API says it; a call that failed because the
/// connection went is `Offline`, whatever words the transport used.
fn err(client: &PimbleClient, e: pimble_client::ClientError) -> ApiError {
    match ApiError::from(e) {
        ApiError::Failed { .. } if !client.is_connected() => {
            ApiError::Offline { sentence: "The connection to Pimble's server closed during the call; try again.".into() }
        }
        other => other,
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl Backend for RpcBackend {
    async fn list_stores(&self) -> Result<Vec<Store>, ApiError> {
        let c = self.client().await?;
        c.list_stores().await.map_err(|e| err(&c, e))
    }

    async fn fetch_one(&self, store: StoreId, node: NodeId) -> Result<Node, ApiError> {
        let c = self.client().await?;
        c.get_node(store, node).await.map_err(|e| err(&c, e))
    }

    async fn fetch(&self, store: StoreId, nodes: Vec<NodeId>) -> Result<Vec<Node>, ApiError> {
        let c = self.client().await?;
        c.get_nodes(store, nodes).await.map_err(|e| err(&c, e))
    }

    async fn children(&self, store: StoreId, node: NodeId) -> Result<(StoreId, Vec<Node>), ApiError> {
        let c = self.client().await?;
        c.get_children(store, node).await.map_err(|e| err(&c, e))
    }

    async fn sync(&self, store: StoreId, have: Vec<(NodeId, Vec<u8>)>, list_unknown: bool) -> Result<(Vec<SyncedDoc>, Vec<NodeId>), ApiError> {
        let c = self.client().await?;
        let (docs, unknown) = c.sync_nodes(store, &have, list_unknown).await.map_err(|e| err(&c, e))?;
        Ok((docs.into_iter().map(|(node, diff, state_vector)| SyncedDoc { node, diff, state_vector }).collect(), unknown))
    }

    async fn apply(&self, store: StoreId, node: NodeId, update: Vec<u8>, client_id: &str) -> Result<(), ApiError> {
        let c = self.client().await?;
        let changes = base64::engine::general_purpose::STANDARD.encode(update);
        c.apply_edit(store, node, client_id, EditOperation::IncrementalChanges { changes }).await.map_err(|e| err(&c, e))
    }

    async fn create_node(&self, store: StoreId, parent: NodeId, init: NodeInit, place: Place) -> Result<NodeId, ApiError> {
        let c = self.client().await?;
        c.create_node_with(store, Some(parent), init.node_type, init.title, init.content, init.custom.into_iter().collect(), place)
            .await
            .map_err(|e| err(&c, e))
    }

    async fn create_attachment(&self, store: StoreId, host: NodeId, init: NodeInit) -> Result<NodeId, ApiError> {
        let c = self.client().await?;
        c.create_attachment(store, host, init.node_type, init.title, init.content, init.custom.into_iter().collect())
            .await
            .map_err(|e| err(&c, e))
    }

    async fn move_node(&self, store: StoreId, node: NodeId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        let c = self.client().await?;
        let r = c.move_node(store, node, parent, place).await.map_err(|e| err(&c, e))?;
        Ok(Moved { node: r.node_id, left_shares: r.left_shares })
    }

    async fn move_nodes(&self, store: StoreId, nodes: Vec<NodeId>, parent: NodeId, place: Place) -> Result<Vec<Moved>, ApiError> {
        let c = self.client().await?;
        let r = c.move_nodes(store, nodes, parent, place).await.map_err(|e| err(&c, e))?;
        Ok(r.moved.into_iter().map(|m| Moved { node: m.node_id, left_shares: m.left_shares }).collect())
    }

    async fn transplant(&self, from: StoreId, node: NodeId, to: StoreId, parent: NodeId, place: Place) -> Result<Moved, ApiError> {
        let c = self.client().await?;
        let r = c.transplant_node(from, node, to, parent, place).await.map_err(|e| err(&c, e))?;
        Ok(Moved { node: r.node_id, left_shares: r.left_shares })
    }

    async fn delete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        let c = self.client().await?;
        c.delete_node(store, node).await.map_err(|e| err(&c, e))
    }

    async fn undelete(&self, store: StoreId, node: NodeId) -> Result<(), ApiError> {
        let c = self.client().await?;
        c.undelete_node(store, node).await.map_err(|e| err(&c, e))
    }

    async fn resolve_link(&self, store: StoreId, node: NodeId) -> Result<LinkResolution, ApiError> {
        let c = self.client().await?;
        c.resolve_link(store, node).await.map_err(|e| err(&c, e))
    }

    async fn list_deleted(&self, store: StoreId) -> Result<Vec<DeletedNode>, ApiError> {
        let c = self.client().await?;
        c.list_deleted(store).await.map_err(|e| err(&c, e))
    }

    async fn list_attachments(&self, store: StoreId, host: NodeId) -> Result<Vec<Node>, ApiError> {
        let c = self.client().await?;
        c.list_attachments(store, host).await.map_err(|e| err(&c, e))
    }

    async fn search(&self, query: &str, stores: Vec<StoreId>, semantic: bool, limit: usize) -> Result<Vec<SearchResultItem>, ApiError> {
        let c = self.client().await?;
        c.search(query, stores, semantic, limit).await.map_err(|e| err(&c, e))
    }

    async fn put_blob(&self, store: StoreId, node: NodeId, mime: &str, bytes: &[u8]) -> Result<BlobUrl, ApiError> {
        let c = self.client().await?;
        c.put_blob(store, node, mime, bytes).await.map_err(|e| err(&c, e))
    }

    async fn get_blob(&self, store: StoreId, blob: BlobId) -> Result<(String, Vec<u8>), ApiError> {
        let c = self.client().await?;
        c.get_blob(store, blob).await.map_err(|e| err(&c, e))
    }

    async fn publish_presence(&self, frame: PresenceFrame, client_id: &str) -> Result<(), ApiError> {
        let c = self.client().await?;
        c.publish_presence(frame, client_id).await.map_err(|e| err(&c, e))
    }

    async fn who_am_i(&self) -> Result<(Option<String>, String), ApiError> {
        let c = self.client().await?;
        c.who_am_i().await.map_err(|e| err(&c, e))
    }

    async fn subscribe(&self, store: StoreId) -> Result<ChangeStream, ApiError> {
        let c = self.client().await?;
        let sub = c.subscribe_store_changes(store).await.map_err(|e| err(&c, e))?;
        // A notification that does not parse is skipped: the next one, or the
        // reconcile after a resubscribe, brings the document along anyway.
        let b64 = base64::engine::general_purpose::STANDARD;
        let stream = sub.filter_map(move |item| {
            let change = match item {
                Ok(n) => Some(StoreChange {
                    store: n.store_id,
                    update: n.update.as_deref().and_then(|u| b64.decode(u).ok()),
                    kind: n.change_kind,
                    source: n.source_client_id,
                }),
                Err(e) => {
                    tracing::debug!("a change notification did not parse: {e}");
                    None
                }
            };
            futures::future::ready(change)
        });
        Ok(Box::pin(stream))
    }
}
