//! Pictures in encrypted stores (docs/IMAGES_CONTRACT.md, wave 4).

use pimble_api::ApiError;
use pimble_core::{BlobId, BlobUrl, NodeId, StoreId};
use pimble_rpc::vault as protocol;
use pimble_rpc::VaultDocId;

use super::{VaultClient, VaultWire};

impl VaultClient {
    // ── Pictures (docs/IMAGES_CONTRACT.md, wave 4) ─────────────────────────

    /// Put a picture for `node_id`'s text or canvas: a new vault document
    /// `blob-<id>`, its entries under a fresh data key wrapped under every
    /// scope key this page holds that covers the node (as a new document's
    /// is), the first naming the node, which is what admits a share
    /// member's upload. The page keeps the bytes, so it shows at once.
    pub async fn put_picture(&mut self, wire: &dyn VaultWire, store_id: StoreId, node_id: NodeId, mime: &str, bytes: &[u8]) -> Result<BlobUrl, ApiError> {
        let store = self.stores.get(&store_id).ok_or_else(|| ApiError::not_found(format!("store {store_id} is not open here")))?;
        let blob = BlobId::new();
        let scope_key_ids = store.scope_key_ids_for(node_id);
        let (dek_id, dek, keys) = protocol::new_picture_key(store_id, blob, &scope_key_ids, &store.keys.scope)
            .ok_or_else(|| ApiError::refused("This page holds no key to put a picture in this store with yet."))?;
        let mut keys = Some(keys);
        for chunk in protocol::picture_chunks(mime, bytes) {
            let sealed = protocol::seal_picture(store_id, blob, dek_id, &dek, &chunk);
            let first = keys.is_some();
            wire.append(store_id, VaultDocId::Blob(blob), sealed, Some(self.client_id.clone()), first.then_some(node_id), None, keys.take(), None)
                .await
                .map_err(|e| ApiError::store_error(e.to_string()))?;
        }
        self.pictures.insert((store_id, blob), (mime.to_string(), bytes.to_vec()));
        Ok(BlobUrl::new(store_id, blob))
    }

    /// Forget the pictures this page holds (a test of reading one back).
    #[cfg(test)]
    pub(crate) fn forget_pictures(&mut self) {
        self.pictures.clear();
    }

    /// A picture of `store_id`: the page's copy, or fetched and decrypted
    /// through `wire`. "This picture has not arrived yet." while it is not
    /// there whole, or not readable with a key this page holds: the app asks
    /// again when the note opens and when the connection comes back.
    pub async fn get_picture(&mut self, wire: Option<&dyn VaultWire>, store_id: StoreId, blob: BlobId) -> Result<(String, Vec<u8>), ApiError> {
        if let Some(picture) = self.pictures.get(&(store_id, blob)) {
            return Ok(picture.clone());
        }
        let not_here = || ApiError::not_found(pimble_rpc::BLOB_NOT_HERE);
        let Some(wire) = wire else {
            tracing::debug!("Picture {blob} of {store_id}: no connection to fetch it through yet");
            return Err(not_here());
        };
        let Some(store) = self.stores.get(&store_id) else {
            tracing::warn!("Picture {blob} of {store_id}: the store is not held in this page");
            return Err(not_here());
        };
        let fetched = match wire.fetch(store_id, VaultDocId::Blob(blob), 0).await {
            Ok(fetched) => fetched,
            Err(e) => {
                tracing::warn!("Picture {blob} of {store_id}: fetching it failed: {e}");
                return Err(not_here());
            }
        };
        match protocol::open_picture(store_id, blob, &fetched, &store.keys.scope) {
            Ok(Some(picture)) => {
                self.pictures.insert((store_id, blob), picture.clone());
                Ok(picture)
            }
            Ok(None) => {
                tracing::info!("Picture {blob} of {store_id}: not whole yet ({} entries)", fetched.head);
                Err(not_here())
            }
            Err(e) => {
                tracing::warn!("Picture {blob} of {store_id}: {e}");
                Err(not_here())
            }
        }
    }
}
