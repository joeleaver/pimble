//! Store keys: fetching the envelopes this account holds, opening them, and
//! sealing a store key to somebody else.
//!
//! One symmetric key encrypts every blob in a store. It reaches an account as a
//! [`KeyEnvelope`] — sealed to their X25519 public key and signed by whoever
//! sent it — which the accounts service stores and cannot open.
//!
//! **Trust on first use.** An envelope is verified against the signing key it
//! carries, not against one fetched independently, so a malicious accounts
//! service could substitute a sender on first contact. The contract says so out
//! loud (docs/CRYPTO_CONTRACT.md, "Threat model") and phase 2a accepts it:
//! signatures still make a *later* substitution detectable.

use std::collections::HashMap;

use pimble_crypto::{unwrap_key, wrap_key, AccountPublicKeys, KeyEnvelope, KeyId, SymmetricKey};

use crate::accounts::{self, EnvelopeUpload};
use crate::session;

/// Every key this account can open for one store.
pub struct Keyring {
    /// Each key id this account holds, so a blob naming an old one still opens.
    pub keys: HashMap<KeyId, SymmetricKey>,
    /// The key new blobs are encrypted under.
    pub current: KeyId,
}

impl Keyring {
    pub fn get(&self, key_id: &KeyId) -> Option<&SymmetricKey> {
        self.keys.get(key_id)
    }

    pub fn current_key(&self) -> Option<&SymmetricKey> {
        self.keys.get(&self.current)
    }
}

/// What a store key is wrapped *for*, and the associated data of its envelope.
pub fn store_context(store_id: &str) -> String {
    format!("store:{store_id}")
}

/// Fetch and open this account's envelopes for `store_id`.
///
/// An envelope that will not open is skipped with a log line rather than
/// failing the store: a rotation leaves old envelopes in place, and one that
/// this device cannot read must not stop the ones it can.
pub async fn fetch_keyring(store_id: &str) -> Result<Keyring, String> {
    let me = session::keys().ok_or("The account keys are not unlocked.")?;
    let envelopes = accounts::store_keys(store_id).await.map_err(|e| e.message)?;

    let mut keys = HashMap::new();
    let mut order: Vec<KeyId> = Vec::new();
    for envelope in &envelopes {
        match unwrap_key(envelope, &me, &envelope.signer) {
            Ok(key) => {
                if keys.insert(envelope.key_id, key).is_none() {
                    order.push(envelope.key_id);
                }
            }
            Err(e) => tracing::warn!(
                "Skipping a key envelope for store {} (key {}): {}",
                store_id,
                envelope.key_id,
                e
            ),
        }
    }

    // Rotation adds a key id and leaves the old ones resolvable. Nothing in an
    // envelope says which is newest, so the service's own order decides, and
    // phase 2a stores have exactly one.
    let current = *order
        .last()
        .ok_or("No usable key for this store: it may not be shared with you yet.")?;
    Ok(Keyring { keys, current })
}

/// Generate a store key, seal it to this account, and upload it.
///
/// Called right after a vault store is created: without this the store exists
/// and can never be opened, by anyone.
pub async fn mint_store_key(store_id: &str) -> Result<(KeyId, SymmetricKey), String> {
    let me = session::keys().ok_or("The account keys are not unlocked.")?;
    let my_public = session::public_keys().ok_or("The account keys are not unlocked.")?;
    let my_user_id = session::user_id().ok_or("The account keys are not unlocked.")?;

    let key = SymmetricKey::generate();
    let key_id = KeyId::new_v4();
    let envelope = seal(&key, key_id, store_id, &my_public, &me)?;

    accounts::put_store_keys(
        store_id,
        vec![EnvelopeUpload { user_id: my_user_id, key_id, envelope }],
    )
    .await
    .map_err(|e| e.message)?;

    Ok((key_id, key))
}

/// Seal `key` to `recipient` and upload it, so they can read the store.
pub async fn grant_store_key(
    store_id: &str,
    key_id: KeyId,
    key: &SymmetricKey,
    recipient_user_id: &str,
    recipient: &AccountPublicKeys,
) -> Result<(), String> {
    let me = session::keys().ok_or("The account keys are not unlocked.")?;
    let envelope = seal(key, key_id, store_id, recipient, &me)?;
    accounts::put_store_keys(
        store_id,
        vec![EnvelopeUpload {
            user_id: recipient_user_id.to_string(),
            key_id,
            envelope,
        }],
    )
    .await
    .map_err(|e| e.message)
}

fn seal(
    key: &SymmetricKey,
    key_id: KeyId,
    store_id: &str,
    recipient: &AccountPublicKeys,
    me: &pimble_crypto::AccountKeys,
) -> Result<KeyEnvelope, String> {
    wrap_key(key, key_id, recipient, me, &store_context(store_id))
        .map_err(|e| format!("Sealing the store key failed: {e}"))
}
