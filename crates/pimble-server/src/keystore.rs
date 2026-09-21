//! The local keystore: the signed-in Pimble Cloud account's unwrapped keys
//! and every store key this server has unwrapped, persisted at
//! `<config dir>/pimble/keys.json` mode `0600`
//! (docs/CRYPTO_CONTRACT.md "Pimble server, the desktop side").
//!
//! Loaded once at server start (or in a test, at `RpcHandler` construction),
//! held in memory, and rewritten atomically (via [`crate::fs_util::write_atomic_0600`],
//! same as [`crate::credentials::CredentialStore`]) after every change:
//! signing in or out, or adding a store key. Moving the unwrapped material
//! to the OS keychain is a follow-up, not this phase.

use std::io;
use std::path::PathBuf;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use pimble_core::StoreId;
use pimble_crypto::{AccountKeys, SymmetricKey};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::warn;
use uuid::Uuid;

/// `<dirs::config_dir()>/pimble/keys.json`.
pub fn default_keystore_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("pimble").join("keys.json")
}

fn b64_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn b64_decode_32(s: &str) -> Option<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD.decode(s).ok()?;
    bytes.try_into().ok()
}

/// The on-disk shape of the signed-in account: everything [`Keystore::sign_in`]
/// persists. `encryption_secret`/`signing_secret` are base64url — this file,
/// unlike the accounts service's own storage, holds the *unwrapped* keys,
/// which is exactly why it must never leave this machine and is mode `0600`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccountSession {
    /// The accounts service's base URL.
    url: String,
    email: String,
    /// The accounts service's own user id (`AuthResponse.user.id` /
    /// `TokenResponse`'s `sub`), needed to address a `PUT .../keys` envelope
    /// at the signed-in account itself.
    user_id: String,
    /// The long-lived session token; re-exchanged for a short-lived JWT via
    /// `POST {url}/api/v1/token` before every RPC connection.
    session: String,
    encryption_secret: String,
    signing_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreKeyEntry {
    store_id: StoreId,
    key_id: Uuid,
    /// base64url symmetric key bytes.
    key: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct KeystoreData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<AccountSession>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    store_keys: Vec<StoreKeyEntry>,
}

/// The signed-in account, unwrapped and ready to use: [`Keystore::account`]'s
/// return type.
pub struct SignedInAccount {
    pub url: String,
    pub email: String,
    pub user_id: String,
    pub session: String,
    pub keys: AccountKeys,
}

/// This server's unwrapped account keys and store keys
/// (docs/CRYPTO_CONTRACT.md). One instance per [`crate::handler::RpcHandler`]
/// (`Arc`-shared), same shape as [`crate::credentials::CredentialStore`].
pub struct Keystore {
    path: PathBuf,
    data: RwLock<KeystoreData>,
}

impl Keystore {
    /// Load `path`, if it exists. A missing, unreadable or unparseable file
    /// starts empty (logged for the latter two) rather than failing server
    /// startup — the worst case is having to sign in again; the next save
    /// overwrites whatever was there.
    pub fn new(path: PathBuf) -> Self {
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<KeystoreData>(&bytes).unwrap_or_else(|e| {
                warn!(
                    "Keystore {} could not be parsed ({}); starting with no signed-in account (the next save overwrites it)",
                    path.display(), e
                );
                KeystoreData::default()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => KeystoreData::default(),
            Err(e) => {
                warn!(
                    "Keystore {} could not be read ({}); starting with no signed-in account (the next save overwrites it)",
                    path.display(), e
                );
                KeystoreData::default()
            }
        };
        Self { path, data: RwLock::new(data) }
    }

    /// Persist a fresh sign-in, replacing any previous account (a store key
    /// saved under a previous account is left in place — it's keyed by
    /// store id and key id, not by account).
    pub async fn sign_in(
        &self,
        url: String,
        email: String,
        user_id: String,
        session: String,
        keys: &AccountKeys,
    ) -> io::Result<()> {
        let snapshot = {
            let mut data = self.data.write().await;
            data.account = Some(AccountSession {
                url,
                email,
                user_id,
                session,
                encryption_secret: b64_encode(&keys.encryption_secret),
                signing_secret: b64_encode(&keys.signing_secret),
            });
            data.clone()
        };
        self.write_atomic(&snapshot).await
    }

    /// Forget the signed-in account. Store keys already unwrapped are left
    /// in place: they decrypt content that stays on disk either way, and a
    /// future sign-in (even to a different account, if this store is
    /// shared) can reuse them without re-fetching envelopes.
    pub async fn sign_out(&self) -> io::Result<()> {
        let snapshot = {
            let mut data = self.data.write().await;
            data.account = None;
            data.clone()
        };
        self.write_atomic(&snapshot).await
    }

    /// The signed-in account, unwrapped, or `None` if no one is signed in
    /// (or the on-disk entry is corrupt — treated the same as signed out,
    /// rather than panicking).
    pub async fn account(&self) -> Option<SignedInAccount> {
        let data = self.data.read().await;
        let account = data.account.as_ref()?;
        let encryption_secret = b64_decode_32(&account.encryption_secret)?;
        let signing_secret = b64_decode_32(&account.signing_secret)?;
        Some(SignedInAccount {
            url: account.url.clone(),
            email: account.email.clone(),
            user_id: account.user_id.clone(),
            session: account.session.clone(),
            keys: AccountKeys { encryption_secret, signing_secret },
        })
    }

    /// Record a store key, replacing any existing entry for the same
    /// (store id, key id) — rotation mints a new key id rather than
    /// overwriting one in place, so this is only ever a genuine insert in
    /// practice.
    pub async fn add_store_key(&self, store_id: StoreId, key_id: Uuid, key: &SymmetricKey) -> io::Result<()> {
        let snapshot = {
            let mut data = self.data.write().await;
            data.store_keys.retain(|e| !(e.store_id == store_id && e.key_id == key_id));
            data.store_keys.push(StoreKeyEntry { store_id, key_id, key: b64_encode(&key.0) });
            data.clone()
        };
        self.write_atomic(&snapshot).await
    }

    /// Forget a store key: a share's, once its owner has stopped sharing.
    /// Forgetting one that is not held changes nothing.
    pub async fn remove_store_key(&self, store_id: StoreId, key_id: Uuid) -> io::Result<()> {
        let snapshot = {
            let mut data = self.data.write().await;
            let before = data.store_keys.len();
            data.store_keys.retain(|e| !(e.store_id == store_id && e.key_id == key_id));
            if data.store_keys.len() == before {
                return Ok(());
            }
            data.clone()
        };
        self.write_atomic(&snapshot).await
    }

    /// The symmetric key for (`store_id`, `key_id`), if this server has
    /// unwrapped one — what [`crate::vault_link::VaultLink`] looks a vault
    /// blob's key id up against, logging and skipping the blob on `None`
    /// rather than failing the whole link.
    pub async fn store_key(&self, store_id: StoreId, key_id: Uuid) -> Option<SymmetricKey> {
        let data = self.data.read().await;
        let entry = data.store_keys.iter().find(|e| e.store_id == store_id && e.key_id == key_id)?;
        let bytes = b64_decode_32(&entry.key)?;
        Some(SymmetricKey(bytes))
    }

    async fn write_atomic(&self, data: &KeystoreData) -> io::Result<()> {
        let path = self.path.clone();
        let json = serde_json::to_vec_pretty(data).map_err(io::Error::other)?;
        tokio::task::spawn_blocking(move || crate::fs_util::write_atomic_0600(&path, &json))
            .await
            .map_err(io::Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> AccountKeys {
        AccountKeys::generate()
    }

    #[tokio::test]
    async fn no_file_starts_signed_out() {
        let dir = tempfile::tempdir().unwrap();
        let ks = Keystore::new(dir.path().join("keys.json"));
        assert!(ks.account().await.is_none());
    }

    #[tokio::test]
    async fn sign_in_then_reload_recovers_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let k = keys();
        {
            let ks = Keystore::new(path.clone());
            ks.sign_in("http://cloud.example".into(), "a@example.com".into(), "user-1".into(), "sess-1".into(), &k)
                .await
                .unwrap();
        }

        let ks = Keystore::new(path);
        let account = ks.account().await.expect("account persisted");
        assert_eq!(account.email, "a@example.com");
        assert_eq!(account.url, "http://cloud.example");
        assert_eq!(account.user_id, "user-1");
        assert_eq!(account.session, "sess-1");
        assert_eq!(account.keys.encryption_secret, k.encryption_secret);
        assert_eq!(account.keys.signing_secret, k.signing_secret);
    }

    #[tokio::test]
    async fn sign_out_clears_the_account_but_keeps_store_keys() {
        let dir = tempfile::tempdir().unwrap();
        let ks = Keystore::new(dir.path().join("keys.json"));
        ks.sign_in("u".into(), "e".into(), "id".into(), "s".into(), &keys()).await.unwrap();

        let store_id = StoreId::new();
        let key_id = Uuid::new_v4();
        let key = SymmetricKey::generate();
        ks.add_store_key(store_id, key_id, &key).await.unwrap();

        ks.sign_out().await.unwrap();
        assert!(ks.account().await.is_none());
        let recovered = ks.store_key(store_id, key_id).await.expect("store key survives sign-out");
        assert_eq!(recovered.0, key.0);
    }

    #[tokio::test]
    async fn store_key_round_trips_and_is_scoped_by_store_and_key_id() {
        let dir = tempfile::tempdir().unwrap();
        let ks = Keystore::new(dir.path().join("keys.json"));

        let store_a = StoreId::new();
        let store_b = StoreId::new();
        let key_1 = Uuid::new_v4();
        let key_2 = Uuid::new_v4();
        let sym_1 = SymmetricKey::generate();
        let sym_2 = SymmetricKey::generate();

        ks.add_store_key(store_a, key_1, &sym_1).await.unwrap();
        ks.add_store_key(store_a, key_2, &sym_2).await.unwrap();

        assert_eq!(ks.store_key(store_a, key_1).await.unwrap().0, sym_1.0);
        assert_eq!(ks.store_key(store_a, key_2).await.unwrap().0, sym_2.0);
        assert!(ks.store_key(store_b, key_1).await.is_none());
        assert!(ks.store_key(store_a, Uuid::new_v4()).await.is_none());
    }

    #[tokio::test]
    async fn removing_a_store_key_forgets_that_key_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let ks = Keystore::new(path.clone());
        let store = StoreId::new();
        let (kept, dropped) = (Uuid::new_v4(), Uuid::new_v4());
        ks.add_store_key(store, kept, &SymmetricKey::generate()).await.unwrap();
        ks.add_store_key(store, dropped, &SymmetricKey::generate()).await.unwrap();

        ks.remove_store_key(store, dropped).await.unwrap();
        ks.remove_store_key(store, Uuid::new_v4()).await.unwrap();

        let reloaded = Keystore::new(path);
        assert!(reloaded.store_key(store, kept).await.is_some());
        assert!(reloaded.store_key(store, dropped).await.is_none());
    }

    #[tokio::test]
    async fn a_corrupt_file_starts_empty_rather_than_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        std::fs::write(&path, b"not json").unwrap();
        let ks = Keystore::new(path);
        assert!(ks.account().await.is_none());
    }
}
