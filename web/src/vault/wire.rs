//! What the vault's writes go through: [`VaultWire`] and its implementations.

use std::sync::Arc;

use pimble_client::{ClientError, PimbleClient};
use pimble_core::{NodeId, StoreId};
use pimble_rpc::{VaultAppendResponse, VaultDocId, VaultDocKeys, VaultFetchResponse, VaultPart};

/// What the vault's writes go through: a Pimble server's vault RPCs, on the
/// endpoint that serves the store. A trait so a test can keep a server's
/// logs in memory and run the writes end to end (the conformance suite,
/// `crate::vault_backend`); the page's own is [`PimbleClient`].
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait VaultWire: pimble_api::MaybeSend + pimble_api::MaybeSync {
    /// `vaultAppend`: the blob numbered, for a new document with its parent
    /// or the node a new discussion is on, and its wrapped data key; `part`
    /// for one part of an append sent in parts (docs/SYNC_LIMITS_CONTRACT.md).
    /// The whole answer: the numbers and the log's sizes.
    #[allow(clippy::too_many_arguments)]
    async fn append(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        blob: String,
        client_id: Option<String>,
        parent_id: Option<NodeId>,
        discussion_host: Option<NodeId>,
        keys: Option<VaultDocKeys>,
        part: Option<VaultPart>,
    ) -> Result<VaultAppendResponse, ClientError>;

    async fn snapshot(&self, store_id: StoreId, doc_id: VaultDocId, upto_seq: u64, blob: String, part: Option<VaultPart>) -> Result<(), ClientError>;

    async fn fetch(&self, store_id: StoreId, doc_id: VaultDocId, after_seq: u64) -> Result<VaultFetchResponse, ClientError>;
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl VaultWire for PimbleClient {
    async fn append(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        blob: String,
        client_id: Option<String>,
        parent_id: Option<NodeId>,
        discussion_host: Option<NodeId>,
        keys: Option<VaultDocKeys>,
        part: Option<VaultPart>,
    ) -> Result<VaultAppendResponse, ClientError> {
        self.vault_append_request(pimble_rpc::VaultAppendRequest {
            store_id,
            doc_id,
            blob,
            client_id,
            parent_id: discussion_host.or(parent_id),
            keys,
            discussion: discussion_host.is_some(),
            part,
        })
        .await
    }

    async fn snapshot(&self, store_id: StoreId, doc_id: VaultDocId, upto_seq: u64, blob: String, part: Option<VaultPart>) -> Result<(), ClientError> {
        self.vault_snapshot_request(pimble_rpc::VaultSnapshotRequest { store_id, doc_id, upto_seq, blob, covers_prefix: true, part }).await
    }

    async fn fetch(&self, store_id: StoreId, doc_id: VaultDocId, after_seq: u64) -> Result<VaultFetchResponse, ClientError> {
        self.vault_fetch(store_id, doc_id, after_seq).await
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl<T: VaultWire + ?Sized> VaultWire for Arc<T> {
    async fn append(
        &self,
        store_id: StoreId,
        doc_id: VaultDocId,
        blob: String,
        client_id: Option<String>,
        parent_id: Option<NodeId>,
        discussion_host: Option<NodeId>,
        keys: Option<VaultDocKeys>,
        part: Option<VaultPart>,
    ) -> Result<VaultAppendResponse, ClientError> {
        (**self).append(store_id, doc_id, blob, client_id, parent_id, discussion_host, keys, part).await
    }

    async fn snapshot(&self, store_id: StoreId, doc_id: VaultDocId, upto_seq: u64, blob: String, part: Option<VaultPart>) -> Result<(), ClientError> {
        (**self).snapshot(store_id, doc_id, upto_seq, blob, part).await
    }

    async fn fetch(&self, store_id: StoreId, doc_id: VaultDocId, after_seq: u64) -> Result<VaultFetchResponse, ClientError> {
        (**self).fetch(store_id, doc_id, after_seq).await
    }
}
