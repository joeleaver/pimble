//! What the tests of the vault's backend stand a page up with.

use std::collections::HashMap;

use pimble_core::{NodeId, Store, StoreId, StoreKind};
use pimble_crypto::{KeyId, SymmetricKey};

use crate::keys::Keyring;

use super::{AccountGrant, AccountStore, StoreKeys, VaultClient, VaultStore};

/// What the tests of the vault's backend (`crate::vault_backend`) stand a
/// page up with: an encrypted store that has just been made, held whole,
/// under a store key of its own, and the account's word about it.
#[cfg(test)]
impl VaultClient {
    pub(crate) fn hold_new_store(&mut self, store_id: StoreId, name: &str) -> NodeId {
        let root = NodeId::new();
        let key_id = KeyId::new_v4();
        let keyring = Keyring { keys: HashMap::from([(key_id, SymmetricKey::generate())]), current: Some(key_id), scopes: vec![(None, key_id)] };
        let mut listed = Store::new_local(name, std::path::PathBuf::new());
        listed.id = store_id;
        listed.root_node_id = root;
        listed.kind = StoreKind::Vault;
        self.stores.insert(store_id, VaultStore::assemble(listed, StoreKeys::new(keyring), Vec::new(), Vec::new()));
        root
    }

    /// The account's row of a store, with one grant of `role` on it whole.
    pub(crate) fn grant_whole(&mut self, store_id: StoreId, role: &str) {
        self.rows.insert(
            store_id,
            AccountStore {
                kind: "vault".into(),
                grants: vec![AccountGrant { root: None, role: role.into(), name: String::new() }],
                ..Default::default()
            },
        );
    }

    /// The backend loop found the store's endpoint down.
    pub(crate) fn owner_away(&mut self, store_id: StoreId) {
        self.owner_offline.insert(store_id);
    }

    /// Whether the store holds an edit no connection has delivered yet.
    pub(crate) fn has_unsent(&self, store_id: StoreId, node_id: NodeId) -> bool {
        self.stores.get(&store_id).and_then(|s| s.docs.get(&node_id)).is_some_and(|d| d.unsent)
    }
}
