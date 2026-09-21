//! Scope keys: fetching the envelopes this account holds for a store or for
//! one share of it, opening them, and sealing a key to somebody else.
//!
//! A **scope key** encrypts nothing directly any more. It wraps the data key
//! (DEK) of each document in its scope — the store key wraps every document's,
//! a share's key wraps those of the documents under its root
//! (docs/NODE_DOCUMENT_CONTRACT.md section 5, "Keys") — and blobs from before
//! data keys name a scope key in their header, which is why an old key id must
//! still resolve here. A scope key reaches an account as a [`KeyEnvelope`],
//! sealed to their X25519 public key and signed by whoever sent it, which the
//! accounts service stores and cannot open.
//!
//! **Who may have signed.** An envelope used to be believed only when this
//! account itself had signed it, which is true of one's own store and false of
//! a share: the key is handed over by its owner. `GET .../keys` therefore also
//! lists the store's owners as `signers`, and an envelope is verified against
//! its own signer only when that signer is this account or one of them.
//! Trust is still on first use — the signers come from the same service as the
//! envelopes (docs/CRYPTO_CONTRACT.md, "Threat model") — but a *later*
//! substitution stays detectable, and an envelope signed by a stranger is now
//! refused outright rather than opened.

use std::collections::HashMap;

use pimble_core::NodeId;
use pimble_crypto::{unwrap_key, wrap_key, AccountPublicKeys, KeyEnvelope, KeyId, SymmetricKey};

use crate::accounts::{self, EnvelopeUpload, SignerView};
use crate::session;

/// Why no keyring came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// The account holds a grant but no envelope it can open: normal for a
    /// fresh share, whose owner's Pimble has not been online since to hand the
    /// key over. Worth retrying; not worth an error.
    NoneYet,
    /// Anything else — the request failed, or the account is locked.
    Failed(String),
}

impl KeyError {
    pub fn message(&self) -> String {
        match self {
            KeyError::NoneYet => "No usable key for this store yet.".to_string(),
            KeyError::Failed(message) => message.clone(),
        }
    }
}

/// Every scope key this account can open for one store: its own store key, or
/// the key of each share of it this account holds, or both.
#[derive(Default)]
pub struct Keyring {
    /// Each key id this account holds, so a blob naming an old one still opens
    /// and a wrap under any of them unwraps.
    pub keys: HashMap<KeyId, SymmetricKey>,
    /// The key a document with no data key of its own is encrypted under: the
    /// store key for a whole store, the first share's key for a scoped member.
    /// `None` only while the keyring is empty.
    pub current: Option<KeyId>,
    /// Which scope each key came from — `None` is the store key's scope,
    /// `Some(root)` a share's. What decides the wraps a new document gets.
    pub scopes: Vec<(Option<NodeId>, KeyId)>,
}

impl Keyring {
    pub fn get(&self, key_id: &KeyId) -> Option<&SymmetricKey> {
        self.keys.get(key_id)
    }

    pub fn current_key(&self) -> Option<&SymmetricKey> {
        self.current.and_then(|id| self.keys.get(&id))
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The key id this account holds for one scope, when it holds one.
    pub fn key_id_for(&self, root: Option<NodeId>) -> Option<KeyId> {
        self.scopes.iter().rev().find(|(scope, _)| *scope == root).map(|(_, id)| *id)
    }

    /// Take another scope's keys in. The first scope's `current` stands:
    /// a whole-store keyring is fetched with no roots at all, and a scoped
    /// one's first root is the store's first.
    fn absorb(&mut self, other: Keyring) {
        self.keys.extend(other.keys);
        self.scopes.extend(other.scopes);
        if self.current.is_none() {
            self.current = other.current;
        }
    }
}

/// What a store key is wrapped *for*, and the associated data of its envelope.
pub fn store_context(store_id: &str) -> String {
    format!("store:{store_id}")
}

/// The keys of every scope this account holds on `store_id`: the store key
/// when `roots` is empty, each share's key otherwise.
///
/// A scope whose key has not reached this account yet is skipped, so a member
/// of two shares who has been handed one of them reads that one. Everything
/// missing is [`KeyError::NoneYet`], which the caller retries rather than
/// reports as a failure.
pub async fn fetch_scope_keyring(store_id: &str, roots: &[NodeId]) -> Result<Keyring, KeyError> {
    let scopes: Vec<Option<NodeId>> = if roots.is_empty() {
        vec![None]
    } else {
        roots.iter().copied().map(Some).collect()
    };

    let mut keyring = Keyring::default();
    let mut failure = None;
    for scope in scopes {
        match fetch_keyring(store_id, scope).await {
            Ok(one) => keyring.absorb(one),
            Err(KeyError::NoneYet) => {}
            Err(KeyError::Failed(message)) => {
                tracing::warn!("Fetching the keys of store {} scope {:?} failed: {}", store_id, scope, message);
                failure.get_or_insert(message);
            }
        }
    }

    if keyring.is_empty() {
        return Err(match failure {
            Some(message) => KeyError::Failed(message),
            None => KeyError::NoneYet,
        });
    }
    Ok(keyring)
}

/// Fetch and open this account's envelopes for one scope of `store_id`: the
/// store key with no `root`, a share's key with one.
///
/// An envelope that will not open is skipped with a log line rather than
/// failing the scope: a rotation leaves old envelopes in place, and one that
/// this device cannot read must not stop the ones it can.
pub async fn fetch_keyring(store_id: &str, root: Option<NodeId>) -> Result<Keyring, KeyError> {
    let me = session::keys().ok_or(KeyError::Failed("The account keys are not unlocked.".to_string()))?;
    let own_signer = session::public_keys()
        .ok_or(KeyError::Failed("The account keys are not unlocked.".to_string()))?
        .signing;
    let fetched = accounts::store_keys(store_id, root.map(|r| r.to_string()).as_deref())
        .await
        .map_err(|e| KeyError::Failed(e.message))?;

    let mut keys = HashMap::new();
    let mut order: Vec<KeyId> = Vec::new();
    for envelope in &fetched.envelopes {
        let signer = expected_signer(envelope, &own_signer, &fetched.signers);
        match unwrap_key(envelope, &me, signer) {
            Ok(key) => {
                if keys.insert(envelope.key_id, key).is_none() {
                    order.push(envelope.key_id);
                }
            }
            Err(e) => tracing::warn!(
                "Skipping a key envelope for store {} scope {:?} (key {}): {}",
                store_id,
                root,
                envelope.key_id,
                e
            ),
        }
    }

    // Rotation adds a key id and leaves the old ones resolvable. Nothing in an
    // envelope says which is newest, so the service's own order decides, and
    // phase 2a stores have exactly one.
    let current = order.last().copied().ok_or(KeyError::NoneYet)?;
    Ok(Keyring { keys, current: Some(current), scopes: vec![(root, current)] })
}

/// The signing key an envelope may be verified against: this account's own, or
/// a listed signer's, whichever the envelope names. An envelope naming neither
/// is checked against this account's own and so refused by `unwrap_key`'s
/// signature check — which is the point: a stranger's signature opens nothing.
pub fn expected_signer<'a>(envelope: &'a KeyEnvelope, own_signer: &'a str, signers: &'a [SignerView]) -> &'a str {
    if envelope.signer == own_signer {
        return own_signer;
    }
    signers
        .iter()
        .find(|s| s.public_signing_key == envelope.signer)
        .map(|s| s.public_signing_key.as_str())
        .unwrap_or(own_signer)
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
        vec![EnvelopeUpload { user_id: my_user_id, key_id, envelope, root: None }],
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
            root: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope_signed_by(signer: &str) -> KeyEnvelope {
        KeyEnvelope {
            v: 1,
            key_id: KeyId::new_v4(),
            recipient: String::new(),
            ephemeral: String::new(),
            nonce: String::new(),
            ciphertext: String::new(),
            signer: signer.to_string(),
            signature: String::new(),
            context: "store:x".to_string(),
        }
    }

    fn signer(key: &str) -> SignerView {
        SignerView { user_id: "u".to_string(), email: "o@example.com".to_string(), public_signing_key: key.to_string() }
    }

    #[test]
    fn an_envelope_is_believed_from_this_account_or_an_owner_and_from_nobody_else() {
        let owners = [signer("owner-key")];

        // One's own store: the envelope this account sealed to itself.
        assert_eq!(expected_signer(&envelope_signed_by("mine"), "mine", &owners), "mine");

        // A share: the key was handed over by the store's owner, whose
        // signature the service lists.
        assert_eq!(expected_signer(&envelope_signed_by("owner-key"), "mine", &owners), "owner-key");

        // A stranger's: checked against this account's own key, which it is
        // not, so `unwrap_key`'s signature check refuses it.
        assert_eq!(
            expected_signer(&envelope_signed_by("someone-else"), "mine", &owners),
            "mine",
            "an unlisted signer opens nothing"
        );

        // A service that lists no signers at all leaves only one's own.
        assert_eq!(expected_signer(&envelope_signed_by("owner-key"), "mine", &[]), "mine");
    }

    #[test]
    fn a_keyring_names_the_key_of_each_scope_it_holds() {
        let (store_key, share_key) = (KeyId::new_v4(), KeyId::new_v4());
        let root = NodeId::new();
        let mut keyring = Keyring {
            keys: HashMap::from([
                (store_key, SymmetricKey::generate()),
                (share_key, SymmetricKey::generate()),
            ]),
            current: Some(store_key),
            scopes: vec![(None, store_key), (Some(root), share_key)],
        };

        assert_eq!(keyring.key_id_for(None), Some(store_key));
        assert_eq!(keyring.key_id_for(Some(root)), Some(share_key));
        assert_eq!(keyring.key_id_for(Some(NodeId::new())), None, "a share this account does not hold");
        assert!(!keyring.is_empty());

        // Absorbing a second scope keeps the first scope's `current`: the
        // store key for a whole store, the first share's for a member.
        let other_root = NodeId::new();
        let other = KeyId::new_v4();
        keyring.absorb(Keyring {
            keys: HashMap::from([(other, SymmetricKey::generate())]),
            current: Some(other),
            scopes: vec![(Some(other_root), other)],
        });
        assert_eq!(keyring.current, Some(store_key));
        assert_eq!(keyring.key_id_for(Some(other_root)), Some(other));
    }
}
