//! Client-side cryptography for Pimble Cloud (docs/CRYPTO_CONTRACT.md, "Primitives").
//!
//! Pure Rust with no I/O and no async, so the same code runs in the desktop's local
//! server and in the browser. This file is the **API skeleton the PM landed**: every
//! signature here is the contract other crates code against; agent K fills in the
//! bodies (marked `todo!()`) and adds the dependencies. Names, argument orders and
//! result types do not change without the PM.
//!
//! Choices (the contract's table): Argon2id (32 MiB, 3 passes, 1 lane) with HKDF-SHA256
//! splitting 64 bytes into `auth_key` and `kek`; XChaCha20-Poly1305 for every symmetric
//! encryption with a 24-byte random nonce; X25519 for wrapping and Ed25519 for signing;
//! sealed-box style wrapping (ephemeral X25519, HKDF-SHA256, XChaCha20-Poly1305) signed
//! by the sender; a 20-byte recovery code in base32.

#![forbid(unsafe_code)]
// Skeleton: bodies are todo!(); agent K removes this allow when filling them in.
#![allow(unused_variables)]

use serde::{Deserialize, Serialize};

/// Format version carried by every serialized structure this crate produces.
pub const VERSION: u8 = 1;

/// Argon2id parameters the contract fixes for password and recovery derivation.
pub const KDF_M_COST_KIB: u32 = 32 * 1024;
pub const KDF_T_COST: u32 = 3;
pub const KDF_P_COST: u32 = 1;

/// Blob magic: the first two bytes of every [`Blob`] on the wire and on disk.
pub const BLOB_MAGIC: [u8; 2] = *b"PB";

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("malformed {0}")]
    Malformed(&'static str),
    #[error("unsupported version {0}")]
    UnsupportedVersion(u8),
    #[error("no key for key id {0}")]
    UnknownKey(KeyId),
    #[error("decryption failed")]
    Decrypt,
    #[error("signature verification failed")]
    BadSignature,
    #[error("key derivation failed: {0}")]
    Kdf(String),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

/// Identifies one symmetric key (a store key, a share key). A UUID; stores and shares
/// mint a new one on every rotation and old ones stay resolvable so old blobs decrypt.
pub type KeyId = uuid::Uuid;

/// A 32-byte symmetric key for XChaCha20-Poly1305. Zeroized on drop by K's implementation.
#[derive(Clone)]
pub struct SymmetricKey(pub [u8; 32]);

impl SymmetricKey {
    /// A fresh random key.
    pub fn generate() -> Self {
        todo!("agent K")
    }
}

/// Argon2id parameters stored with the user (docs/CRYPTO_CONTRACT.md "Data model").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KdfParams {
    /// 16 bytes, base64url without padding in JSON.
    pub salt: String,
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl KdfParams {
    /// Fresh random salt with the contract's fixed costs.
    pub fn generate() -> Self {
        todo!("agent K")
    }
}

/// What a password derives to: `auth_key` goes to the server as the password (the
/// server hashes it again at rest); `kek` never leaves the client and wraps the
/// account keys.
pub struct PasswordKeys {
    pub auth_key: [u8; 32],
    pub kek: SymmetricKey,
}

/// Argon2id(password, params) → 64 bytes → HKDF-SHA256 split into the two keys.
pub fn derive_password_keys(password: &str, params: &KdfParams) -> Result<PasswordKeys> {
    todo!("agent K")
}

/// `auth_key` as the base64url string the accounts API carries.
pub fn encode_auth_key(auth_key: &[u8; 32]) -> String {
    todo!("agent K")
}

/// An account's private keys: X25519 for unwrapping envelopes, Ed25519 for signing them.
pub struct AccountKeys {
    pub encryption_secret: [u8; 32],
    pub signing_secret: [u8; 32],
}

/// The public halves, as the accounts service stores and serves them
/// (base64url without padding in JSON).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountPublicKeys {
    pub encryption: String,
    pub signing: String,
}

impl AccountKeys {
    pub fn generate() -> Self {
        todo!("agent K")
    }
    pub fn public_keys(&self) -> AccountPublicKeys {
        todo!("agent K")
    }
}

/// The account private keys wrapped under a KEK (the password's, or the recovery
/// code's). JSON, versioned. Stored by the accounts service, opaque to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountKeyBlob {
    pub v: u8,
    /// 24 bytes, base64url.
    pub nonce: String,
    /// XChaCha20-Poly1305 over the 64 bytes `encryption_secret || signing_secret`,
    /// associated data `"pimble-account-keys"`. base64url.
    pub ciphertext: String,
}

pub fn wrap_account_keys(keys: &AccountKeys, kek: &SymmetricKey) -> Result<AccountKeyBlob> {
    todo!("agent K")
}

pub fn unwrap_account_keys(blob: &AccountKeyBlob, kek: &SymmetricKey) -> Result<AccountKeys> {
    todo!("agent K")
}

/// A new recovery code: 20 random bytes as 32 base32 characters (RFC 4648 alphabet,
/// upper case) in groups of four joined by `-`, for example `K7Q2-...`.
pub fn generate_recovery_code() -> String {
    todo!("agent K")
}

/// The KEK a recovery code derives to (Argon2id with the recovery salt; whitespace,
/// dashes and case in `code` are normalized first).
pub fn derive_recovery_kek(code: &str, params: &KdfParams) -> Result<SymmetricKey> {
    todo!("agent K")
}

/// A symmetric key sealed to one recipient and signed by the sender
/// (docs/CRYPTO_CONTRACT.md "Key wrapping"). JSON, versioned, all fields base64url.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyEnvelope {
    pub v: u8,
    pub key_id: KeyId,
    /// Recipient's X25519 public key (so a recipient can pick its envelopes).
    pub recipient: String,
    /// Sender's ephemeral X25519 public key.
    pub ephemeral: String,
    pub nonce: String,
    /// XChaCha20-Poly1305 over the 32-byte key under HKDF-SHA256(ECDH, info =
    /// `"pimble-key-envelope"`), associated data = `context`.
    pub ciphertext: String,
    /// Sender's Ed25519 public key.
    pub signer: String,
    /// Ed25519 over `v || key_id || recipient || ephemeral || nonce || ciphertext || context`.
    pub signature: String,
    /// What the key is for, e.g. `"store:<store id>"` or `"share:<share id>"`.
    pub context: String,
}

pub fn wrap_key(
    key: &SymmetricKey,
    key_id: KeyId,
    recipient: &AccountPublicKeys,
    sender: &AccountKeys,
    context: &str,
) -> Result<KeyEnvelope> {
    todo!("agent K")
}

/// Verifies the envelope's signature against `expected_signer` (the sender's public
/// signing key as the accounts service reported it) and unwraps with `me`.
pub fn unwrap_key(envelope: &KeyEnvelope, me: &AccountKeys, expected_signer: &str) -> Result<SymmetricKey> {
    todo!("agent K")
}

/// An encrypted document update or snapshot as the vault stores and relays it
/// (docs/CRYPTO_CONTRACT.md "Blob layout"): `PB` · version · key id (16) · nonce (24) ·
/// ciphertext. `aad` is `"{store_id}/{doc_id}"`.
pub struct Blob;

impl Blob {
    pub fn encrypt(key: &SymmetricKey, key_id: KeyId, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        todo!("agent K")
    }

    /// The key id a blob names, without decrypting (so a caller can find the key).
    pub fn key_id(blob: &[u8]) -> Result<KeyId> {
        todo!("agent K")
    }

    pub fn decrypt(key: &SymmetricKey, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>> {
        todo!("agent K")
    }
}

/// The associated data for a document's blobs.
pub fn blob_aad(store_id: &str, doc_id: &str) -> Vec<u8> {
    format!("{store_id}/{doc_id}").into_bytes()
}
