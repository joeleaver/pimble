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

use argon2::{Algorithm, Argon2, Params as Argon2Params, Version};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key as XChaChaKey, KeyInit, XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Format version carried by every serialized structure this crate produces.
pub const VERSION: u8 = 1;

/// Argon2id parameters the contract fixes for password and recovery derivation.
pub const KDF_M_COST_KIB: u32 = 32 * 1024;
pub const KDF_T_COST: u32 = 3;
pub const KDF_P_COST: u32 = 1;

/// Blob magic: the first two bytes of every [`Blob`] on the wire and on disk.
pub const BLOB_MAGIC: [u8; 2] = *b"PB";

/// Domain-separation strings for HKDF and AEAD associated data. Internal only: no
/// serialized value depends on these exact bytes staying stable across versions other
/// than round-tripping this crate's own output.
const HKDF_SALT_PASSWORD: &[u8] = b"pimble-password-kdf-v1";
const HKDF_INFO_AUTH_KEY: &[u8] = b"auth_key";
const HKDF_INFO_KEK: &[u8] = b"kek";
const HKDF_INFO_KEY_ENVELOPE: &[u8] = b"pimble-key-envelope";
const AAD_ACCOUNT_KEYS: &[u8] = b"pimble-account-keys";

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const RECOVERY_CODE_BYTES: usize = 20;

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
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SymmetricKey(pub [u8; 32]);

impl SymmetricKey {
    /// A fresh random key.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        SymmetricKey(bytes)
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
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        KdfParams {
            salt: b64_encode(&salt),
            m_cost: KDF_M_COST_KIB,
            t_cost: KDF_T_COST,
            p_cost: KDF_P_COST,
        }
    }
}

/// What a password derives to: `auth_key` goes to the server as the password (the
/// server hashes it again at rest); `kek` never leaves the client and wraps the
/// account keys.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct PasswordKeys {
    pub auth_key: [u8; 32],
    pub kek: SymmetricKey,
}

/// Argon2id(password, params) → 64 bytes → HKDF-SHA256 split into the two keys.
pub fn derive_password_keys(password: &str, params: &KdfParams) -> Result<PasswordKeys> {
    let mut wide = argon2id_hash(password.as_bytes(), params, 64)?;

    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT_PASSWORD), &wide);
    let mut auth_key = [0u8; 32];
    hk.expand(HKDF_INFO_AUTH_KEY, &mut auth_key)
        .map_err(|_| CryptoError::Kdf("hkdf expand auth_key".into()))?;
    let mut kek_bytes = [0u8; 32];
    hk.expand(HKDF_INFO_KEK, &mut kek_bytes)
        .map_err(|_| CryptoError::Kdf("hkdf expand kek".into()))?;

    wide.zeroize();

    Ok(PasswordKeys {
        auth_key,
        kek: SymmetricKey(kek_bytes),
    })
}

/// `auth_key` as the base64url string the accounts API carries.
pub fn encode_auth_key(auth_key: &[u8; 32]) -> String {
    b64_encode(auth_key)
}

/// An account's private keys: X25519 for unwrapping envelopes, Ed25519 for signing them.
#[derive(Zeroize, ZeroizeOnDrop)]
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
        let static_secret = StaticSecret::random_from_rng(OsRng);
        let signing_key = SigningKey::generate(&mut OsRng);
        AccountKeys {
            encryption_secret: static_secret.to_bytes(),
            signing_secret: signing_key.to_bytes(),
        }
    }

    pub fn public_keys(&self) -> AccountPublicKeys {
        let static_secret = StaticSecret::from(self.encryption_secret);
        let x25519_public = X25519PublicKey::from(&static_secret);
        let signing_key = SigningKey::from_bytes(&self.signing_secret);
        let verifying_key = signing_key.verifying_key();
        AccountPublicKeys {
            encryption: b64_encode(x25519_public.as_bytes()),
            signing: b64_encode(verifying_key.as_bytes()),
        }
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
    let mut plaintext = [0u8; 64];
    plaintext[..32].copy_from_slice(&keys.encryption_secret);
    plaintext[32..].copy_from_slice(&keys.signing_secret);

    let nonce_bytes = random_nonce();
    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&kek.0));
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &plaintext,
                aad: AAD_ACCOUNT_KEYS,
            },
        )
        .map_err(|_| CryptoError::Decrypt)?;

    plaintext.zeroize();

    Ok(AccountKeyBlob {
        v: VERSION,
        nonce: b64_encode(&nonce_bytes),
        ciphertext: b64_encode(&ciphertext),
    })
}

pub fn unwrap_account_keys(blob: &AccountKeyBlob, kek: &SymmetricKey) -> Result<AccountKeys> {
    if blob.v != VERSION {
        return Err(CryptoError::UnsupportedVersion(blob.v));
    }
    let nonce_bytes = b64_decode(&blob.nonce)?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(CryptoError::Malformed("account key blob nonce"));
    }
    let ciphertext = b64_decode(&blob.ciphertext)?;

    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&kek.0));
    let mut plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &ciphertext,
                aad: AAD_ACCOUNT_KEYS,
            },
        )
        .map_err(|_| CryptoError::Decrypt)?;

    if plaintext.len() != 64 {
        plaintext.zeroize();
        return Err(CryptoError::Malformed("account key blob plaintext"));
    }

    let mut encryption_secret = [0u8; 32];
    let mut signing_secret = [0u8; 32];
    encryption_secret.copy_from_slice(&plaintext[..32]);
    signing_secret.copy_from_slice(&plaintext[32..]);
    plaintext.zeroize();

    Ok(AccountKeys {
        encryption_secret,
        signing_secret,
    })
}

/// A new recovery code: 20 random bytes as 32 base32 characters (RFC 4648 alphabet,
/// upper case) in groups of four joined by `-`, for example `K7Q2-...`.
pub fn generate_recovery_code() -> String {
    let mut bytes = [0u8; RECOVERY_CODE_BYTES];
    OsRng.fill_bytes(&mut bytes);
    let encoded = base32_encode(&bytes);
    group_with_dashes(&encoded, 4)
}

/// The KEK a recovery code derives to (Argon2id with the recovery salt; whitespace,
/// dashes and case in `code` are normalized first).
pub fn derive_recovery_kek(code: &str, params: &KdfParams) -> Result<SymmetricKey> {
    let normalized = normalize_recovery_code(code);
    let mut out = argon2id_hash(normalized.as_bytes(), params, 32)?;
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&out);
    out.zeroize();
    Ok(SymmetricKey(key_bytes))
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
    let recipient_bytes = b64_decode_fixed::<32>(&recipient.encryption, "recipient key")?;
    let recipient_pub = X25519PublicKey::from(recipient_bytes);

    let ephemeral_secret = EphemeralSecret::random_from_rng(OsRng);
    let ephemeral_public = X25519PublicKey::from(&ephemeral_secret);
    let shared = ephemeral_secret.diffie_hellman(&recipient_pub);

    let mut wrapping_key = [0u8; 32];
    let hk = Hkdf::<Sha256>::new(None, shared.as_bytes());
    hk.expand(HKDF_INFO_KEY_ENVELOPE, &mut wrapping_key)
        .map_err(|_| CryptoError::Kdf("hkdf expand key envelope".into()))?;

    let nonce_bytes = random_nonce();
    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&wrapping_key));
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &key.0,
                aad: context.as_bytes(),
            },
        )
        .map_err(|_| CryptoError::Decrypt)?;
    wrapping_key.zeroize();

    let signing_key = SigningKey::from_bytes(&sender.signing_secret);
    let signer_public = signing_key.verifying_key();

    let sign_bytes = envelope_signing_bytes(
        VERSION,
        &key_id,
        recipient_bytes.as_slice(),
        ephemeral_public.as_bytes().as_slice(),
        &nonce_bytes,
        &ciphertext,
        context,
    );
    let signature = signing_key.sign(&sign_bytes);

    Ok(KeyEnvelope {
        v: VERSION,
        key_id,
        recipient: recipient.encryption.clone(),
        ephemeral: b64_encode(ephemeral_public.as_bytes()),
        nonce: b64_encode(&nonce_bytes),
        ciphertext: b64_encode(&ciphertext),
        signer: b64_encode(signer_public.as_bytes()),
        signature: b64_encode(&signature.to_bytes()),
        context: context.to_string(),
    })
}

/// Verifies a [`KeyEnvelope`]'s signature against `expected_signer` with no private key
/// needed: confirms the envelope names `expected_signer` as its signer and checks the
/// Ed25519 signature over exactly the bytes `wrap_key` signs. Lets a party that only
/// holds public keys (the accounts service) authenticate an envelope it relays.
pub fn verify_envelope(envelope: &KeyEnvelope, expected_signer: &str) -> Result<()> {
    if envelope.v != VERSION {
        return Err(CryptoError::UnsupportedVersion(envelope.v));
    }
    if envelope.signer != expected_signer {
        return Err(CryptoError::BadSignature);
    }

    let signer_bytes = b64_decode_fixed::<32>(expected_signer, "expected signer key")?;
    let verifying_key =
        VerifyingKey::from_bytes(&signer_bytes).map_err(|_| CryptoError::Malformed("expected signer key"))?;

    let recipient_bytes = b64_decode(&envelope.recipient)?;
    let ephemeral_bytes = b64_decode_fixed::<32>(&envelope.ephemeral, "ephemeral key")?;
    let nonce_bytes = b64_decode(&envelope.nonce)?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(CryptoError::Malformed("envelope nonce"));
    }
    let ciphertext = b64_decode(&envelope.ciphertext)?;
    let signature_bytes = b64_decode_fixed::<64>(&envelope.signature, "signature")?;
    let signature = Signature::from_bytes(&signature_bytes);

    let sign_bytes = envelope_signing_bytes(
        envelope.v,
        &envelope.key_id,
        &recipient_bytes,
        ephemeral_bytes.as_slice(),
        &nonce_bytes,
        &ciphertext,
        &envelope.context,
    );
    verifying_key
        .verify(&sign_bytes, &signature)
        .map_err(|_| CryptoError::BadSignature)
}

/// Verifies the envelope's signature against `expected_signer` (the sender's public
/// signing key as the accounts service reported it, via [`verify_envelope`]) and
/// unwraps with `me`.
pub fn unwrap_key(envelope: &KeyEnvelope, me: &AccountKeys, expected_signer: &str) -> Result<SymmetricKey> {
    verify_envelope(envelope, expected_signer)?;

    let ephemeral_bytes = b64_decode_fixed::<32>(&envelope.ephemeral, "ephemeral key")?;
    let nonce_bytes = b64_decode(&envelope.nonce)?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(CryptoError::Malformed("envelope nonce"));
    }
    let ciphertext = b64_decode(&envelope.ciphertext)?;

    let ephemeral_public = X25519PublicKey::from(ephemeral_bytes);
    let my_secret = StaticSecret::from(me.encryption_secret);
    let shared = my_secret.diffie_hellman(&ephemeral_public);

    let mut wrapping_key = [0u8; 32];
    let hk = Hkdf::<Sha256>::new(None, shared.as_bytes());
    hk.expand(HKDF_INFO_KEY_ENVELOPE, &mut wrapping_key)
        .map_err(|_| CryptoError::Kdf("hkdf expand key envelope".into()))?;

    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&wrapping_key));
    let mut plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &ciphertext,
                aad: envelope.context.as_bytes(),
            },
        )
        .map_err(|_| CryptoError::Decrypt)?;
    wrapping_key.zeroize();

    if plaintext.len() != 32 {
        plaintext.zeroize();
        return Err(CryptoError::Malformed("unwrapped key length"));
    }
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&plaintext);
    plaintext.zeroize();

    Ok(SymmetricKey(key_bytes))
}

/// An encrypted document update or snapshot as the vault stores and relays it
/// (docs/CRYPTO_CONTRACT.md "Blob layout"): `PB` · version · key id (16) · nonce (24) ·
/// ciphertext. `aad` is `"{store_id}/{doc_id}"`.
pub struct Blob;

/// `PB` (2) + version (1) + key id (16) + nonce (24).
const BLOB_HEADER_LEN: usize = 2 + 1 + 16 + NONCE_LEN;

impl Blob {
    pub fn encrypt(key: &SymmetricKey, key_id: KeyId, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let nonce_bytes = random_nonce();
        let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&key.0));
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce_bytes), Payload { msg: plaintext, aad })
            .expect("XChaCha20-Poly1305 encryption cannot fail for valid inputs");

        let mut out = Vec::with_capacity(BLOB_HEADER_LEN + ciphertext.len());
        out.extend_from_slice(&BLOB_MAGIC);
        out.push(VERSION);
        out.extend_from_slice(key_id.as_bytes());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        out
    }

    /// The key id a blob names, without decrypting (so a caller can find the key).
    pub fn key_id(blob: &[u8]) -> Result<KeyId> {
        let header = BlobHeader::parse(blob)?;
        Ok(header.key_id)
    }

    pub fn decrypt(key: &SymmetricKey, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>> {
        let header = BlobHeader::parse(blob)?;
        let ciphertext = &blob[BLOB_HEADER_LEN..];
        let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&key.0));
        cipher
            .decrypt(
                XNonce::from_slice(&header.nonce),
                Payload { msg: ciphertext, aad },
            )
            .map_err(|_| CryptoError::Decrypt)
    }
}

struct BlobHeader {
    key_id: KeyId,
    nonce: [u8; NONCE_LEN],
}

impl BlobHeader {
    fn parse(blob: &[u8]) -> Result<Self> {
        if blob.len() < BLOB_HEADER_LEN {
            return Err(CryptoError::Malformed("blob too short"));
        }
        if blob[0..2] != BLOB_MAGIC {
            return Err(CryptoError::Malformed("blob magic"));
        }
        let version = blob[2];
        if version != VERSION {
            return Err(CryptoError::UnsupportedVersion(version));
        }
        let key_id = KeyId::from_slice(&blob[3..19]).map_err(|_| CryptoError::Malformed("blob key id"))?;
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&blob[19..19 + NONCE_LEN]);
        Ok(BlobHeader { key_id, nonce })
    }
}

/// The associated data for a document's blobs.
pub fn blob_aad(store_id: &str, doc_id: &str) -> Vec<u8> {
    format!("{store_id}/{doc_id}").into_bytes()
}

/// A document's data key (DEK) wrapped under one scope key: the store key, or
/// the key of a share the document is under (docs/NODE_DOCUMENT_CONTRACT.md
/// section 5, "Keys"). A document carries one wrap per scope key that may read
/// it; a node entering a share gets one more wrap, never a re-encryption. JSON,
/// versioned, base64url fields. XChaCha20-Poly1305 under the scope key with
/// [`dek_aad`] as associated data, so a wrap cannot be replayed onto another
/// document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WrappedDek {
    pub v: u8,
    /// The key that wraps: which store key or share key to unwrap with.
    pub scope_key_id: KeyId,
    pub nonce: String,
    pub ciphertext: String,
}

/// Wrap `dek` under `scope_key` (identified by `scope_key_id`) for the document
/// `aad` names ([`dek_aad`]).
pub fn wrap_dek(dek: &SymmetricKey, scope_key: &SymmetricKey, scope_key_id: KeyId, aad: &[u8]) -> WrappedDek {
    let nonce_bytes = random_nonce();
    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&scope_key.0));
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), Payload { msg: &dek.0, aad })
        .expect("XChaCha20-Poly1305 encryption cannot fail for valid inputs");
    WrappedDek { v: VERSION, scope_key_id, nonce: b64_encode(&nonce_bytes), ciphertext: b64_encode(&ciphertext) }
}

/// Unwrap with the scope key `wrapped.scope_key_id` names, for the document
/// `aad` names. A wrong key, a wrong document or a tampered wrap is
/// [`CryptoError::Decrypt`].
pub fn unwrap_dek(wrapped: &WrappedDek, scope_key: &SymmetricKey, aad: &[u8]) -> Result<SymmetricKey> {
    if wrapped.v != VERSION {
        return Err(CryptoError::UnsupportedVersion(wrapped.v));
    }
    let nonce_bytes = b64_decode(&wrapped.nonce)?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err(CryptoError::Malformed("wrap nonce"));
    }
    let ciphertext = b64_decode(&wrapped.ciphertext)?;
    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&scope_key.0));
    let mut plaintext = cipher
        .decrypt(XNonce::from_slice(&nonce_bytes), Payload { msg: &ciphertext, aad })
        .map_err(|_| CryptoError::Decrypt)?;
    if plaintext.len() != 32 {
        plaintext.zeroize();
        return Err(CryptoError::Malformed("unwrapped data key length"));
    }
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&plaintext);
    plaintext.zeroize();
    Ok(SymmetricKey(key_bytes))
}

/// The associated data for a document's data-key wraps.
pub fn dek_aad(store_id: &str, doc_id: &str) -> Vec<u8> {
    format!("{store_id}/{doc_id}/dek").into_bytes()
}

// --- Internal helpers -------------------------------------------------------------

fn b64_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn b64_decode(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| CryptoError::Malformed("base64"))
}

fn b64_decode_fixed<const N: usize>(s: &str, what: &'static str) -> Result<[u8; N]> {
    let bytes = b64_decode(s)?;
    if bytes.len() != N {
        return Err(CryptoError::Malformed(what));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn random_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

/// Argon2id(input, params.salt, params costs) with the given output length. `input` is
/// either UTF-8 password bytes or a normalized recovery code's bytes.
fn argon2id_hash(input: &[u8], params: &KdfParams, output_len: usize) -> Result<Vec<u8>> {
    let salt = b64_decode(&params.salt)?;
    let argon2_params = Argon2Params::new(params.m_cost, params.t_cost, params.p_cost, Some(output_len))
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params);
    let mut out = vec![0u8; output_len];
    argon2
        .hash_password_into(input, &salt, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(out)
}

/// The bytes an envelope's Ed25519 signature covers: `v || key_id || recipient ||
/// ephemeral || nonce || ciphertext || context`, all raw (not base64).
fn envelope_signing_bytes(
    v: u8,
    key_id: &KeyId,
    recipient: &[u8],
    ephemeral: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
    context: &str,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(
        1 + 16 + recipient.len() + ephemeral.len() + nonce.len() + ciphertext.len() + context.len(),
    );
    buf.push(v);
    buf.extend_from_slice(key_id.as_bytes());
    buf.extend_from_slice(recipient);
    buf.extend_from_slice(ephemeral);
    buf.extend_from_slice(nonce);
    buf.extend_from_slice(ciphertext);
    buf.extend_from_slice(context.as_bytes());
    buf
}

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 (upper case), no padding.
fn base32_encode(data: &[u8]) -> String {
    let mut output = String::with_capacity((data.len() * 8).div_ceil(5));
    let mut buffer: u32 = 0;
    let mut bits_left: u32 = 0;
    for &byte in data {
        buffer = (buffer << 8) | byte as u32;
        bits_left += 8;
        while bits_left >= 5 {
            bits_left -= 5;
            let idx = ((buffer >> bits_left) & 0x1f) as usize;
            output.push(BASE32_ALPHABET[idx] as char);
        }
    }
    if bits_left > 0 {
        let idx = ((buffer << (5 - bits_left)) & 0x1f) as usize;
        output.push(BASE32_ALPHABET[idx] as char);
    }
    output
}

fn group_with_dashes(s: &str, group_len: usize) -> String {
    s.as_bytes()
        .chunks(group_len)
        .map(|chunk| std::str::from_utf8(chunk).expect("ascii base32 alphabet"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Strips whitespace and dashes and upper-cases, so a recovery code round-trips
/// regardless of how it is typed back in.
fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(|c| c.to_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_key_wraps_under_a_scope_key_and_only_unwraps_for_its_document() {
        let dek = SymmetricKey::generate();
        let store_key = SymmetricKey::generate();
        let share_key = SymmetricKey::generate();
        let aad = dek_aad("store", "doc");

        let under_store = wrap_dek(&dek, &store_key, KeyId::new_v4(), &aad);
        let under_share = wrap_dek(&dek, &share_key, KeyId::new_v4(), &aad);
        assert_eq!(unwrap_dek(&under_store, &store_key, &aad).unwrap().0, dek.0);
        assert_eq!(unwrap_dek(&under_share, &share_key, &aad).unwrap().0, dek.0);

        // The wrong scope key, another document, and a tampered wrap all fail.
        assert!(matches!(unwrap_dek(&under_store, &share_key, &aad), Err(CryptoError::Decrypt)));
        assert!(matches!(unwrap_dek(&under_store, &store_key, &dek_aad("store", "other")), Err(CryptoError::Decrypt)));
        let mut tampered = under_store.clone();
        tampered.ciphertext = b64_encode(&[0u8; 48]);
        assert!(matches!(unwrap_dek(&tampered, &store_key, &aad), Err(CryptoError::Decrypt)));

        // JSON round trip, as the server stores and relays it.
        let json = serde_json::to_string(&under_share).unwrap();
        let back: WrappedDek = serde_json::from_str(&json).unwrap();
        assert_eq!(back, under_share);
    }

    // ---- Known-answer tests -------------------------------------------------------

    /// RFC 5869 Appendix A.1, Test Case 1 (HKDF-SHA256, basic test case).
    #[test]
    fn hkdf_sha256_rfc5869_test_case_1() {
        let ikm = hex::decode("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b").unwrap();
        let salt = hex::decode("000102030405060708090a0b0c").unwrap();
        let info = hex::decode("f0f1f2f3f4f5f6f7f8f9").unwrap();
        let expected_prk =
            hex::decode("077709362c2e32df0ddc3f0dc47bba63 90b6c73bb50f9c3122ec844ad7c2b3e5".replace(' ', ""))
                .unwrap();
        let expected_okm = hex::decode(
            "3cb25f25faacd57a90434f64d0362f2a 2d2d0a90cf1a5a4c5db02d56ecc4c5bf 34007208d5b887185865"
                .replace(' ', ""),
        )
        .unwrap();

        let (prk, hk) = Hkdf::<Sha256>::extract(Some(&salt), &ikm);
        assert_eq!(prk.as_slice(), expected_prk.as_slice());

        let mut okm = [0u8; 42];
        hk.expand(&info, &mut okm).unwrap();
        assert_eq!(okm.as_slice(), expected_okm.as_slice());
    }

    /// draft-irtf-cfrg-xchacha-03 Appendix A.3.1, the developer-friendly
    /// AEAD_XCHACHA20_POLY1305 test vector.
    #[test]
    fn xchacha20poly1305_draft_vector() {
        let key = hex::decode("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f").unwrap();
        let nonce = hex::decode("404142434445464748494a4b4c4d4e4f5051525354555657").unwrap();
        let aad = hex::decode("50515253c0c1c2c3c4c5c6c7").unwrap();
        let plaintext = hex::decode(
            "4c616469657320616e642047656e746c656d656e206f662074686520636c6173\
             73206f66202739393a204966204920636f756c64206f6666657220796f75206f\
             6e6c79206f6e652074697020666f7220746865206675747572652c2073756e73\
             637265656e20776f756c642062652069742e",
        )
        .unwrap();
        let ciphertext = hex::decode(
            "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb\
             731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b452\
             2f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff9\
             21f9664c97637da9768812f615c68b13b52e",
        )
        .unwrap();
        let tag = hex::decode("c0875924c1c7987947deafd8780acf49").unwrap();

        let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&key));
        let sealed = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .unwrap();

        let mut expected = ciphertext.clone();
        expected.extend_from_slice(&tag);
        assert_eq!(sealed, expected);

        let opened = cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &sealed,
                    aad: &aad,
                },
            )
            .unwrap();
        assert_eq!(opened, plaintext);
    }

    /// Argon2id at the contract's parameters (32 MiB, t=3, p=1) against a fixed
    /// input/salt. Not from an external KAT (RFC 9106's vectors use different
    /// parameters); computed once with this implementation and pinned here as a
    /// regression so a future change to the KDF wiring is caught.
    #[test]
    fn argon2id_fixed_input_regression() {
        let params = KdfParams {
            salt: b64_encode(b"0123456789abcdef"), // 16 bytes
            m_cost: KDF_M_COST_KIB,
            t_cost: KDF_T_COST,
            p_cost: KDF_P_COST,
        };
        let out = argon2id_hash(b"correct horse battery staple", &params, 64).unwrap();
        assert_eq!(out.len(), 64);
        let expected = hex::decode(
            "282e6b05d59dbc214153c48c2198f2bfceaa2ec7a1b513fd8b1b97402ac0ec3\
             8d40d37b5270ca6ebd113eda3bb09d0eef1c2c384e3c9285e24f783c3b6fc9f81",
        )
        .unwrap();
        // Self-computed regression vector (see doc comment): this assertion pins the
        // value this test observed the first time it ran, recorded so a later change
        // to the KDF wiring is caught. If argon2id_hash is intentionally changed,
        // regenerate this constant from the new implementation's output.
        assert_eq!(out, expected, "argon2id regression vector changed: {}", hex::encode(&out));
    }

    // ---- Round trips ----------------------------------------------------------------

    #[test]
    fn symmetric_key_generate_is_random() {
        let a = SymmetricKey::generate();
        let b = SymmetricKey::generate();
        assert_ne!(a.0, b.0);
    }

    #[test]
    fn password_keys_round_trip_and_deterministic() {
        let params = KdfParams::generate();
        let a = derive_password_keys("hunter2", &params).unwrap();
        let b = derive_password_keys("hunter2", &params).unwrap();
        assert_eq!(a.auth_key, b.auth_key);
        assert_eq!(a.kek.0, b.kek.0);
        assert_ne!(a.auth_key, a.kek.0);

        let different = derive_password_keys("hunter3", &params).unwrap();
        assert_ne!(a.auth_key, different.auth_key);
    }

    #[test]
    fn encode_auth_key_is_base64url() {
        let key = [7u8; 32];
        let encoded = encode_auth_key(&key);
        let decoded = b64_decode(&encoded).unwrap();
        assert_eq!(decoded, key);
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn account_keys_wrap_unwrap_round_trip() {
        let keys = AccountKeys::generate();
        let kek = SymmetricKey::generate();
        let blob = wrap_account_keys(&keys, &kek).unwrap();
        let recovered = unwrap_account_keys(&blob, &kek).unwrap();
        assert_eq!(recovered.encryption_secret, keys.encryption_secret);
        assert_eq!(recovered.signing_secret, keys.signing_secret);
    }

    #[test]
    fn account_key_blob_wrong_key_fails() {
        let keys = AccountKeys::generate();
        let kek = SymmetricKey::generate();
        let wrong_kek = SymmetricKey::generate();
        let blob = wrap_account_keys(&keys, &kek).unwrap();
        let result = unwrap_account_keys(&blob, &wrong_kek);
        assert!(matches!(result, Err(CryptoError::Decrypt)));
    }

    #[test]
    fn account_key_blob_tamper_detected() {
        let keys = AccountKeys::generate();
        let kek = SymmetricKey::generate();
        let mut blob = wrap_account_keys(&keys, &kek).unwrap();
        let mut raw = b64_decode(&blob.ciphertext).unwrap();
        raw[0] ^= 0xff;
        blob.ciphertext = b64_encode(&raw);
        let result = unwrap_account_keys(&blob, &kek);
        assert!(matches!(result, Err(CryptoError::Decrypt)));
    }

    #[test]
    fn account_key_blob_json_round_trip_field_names() {
        let keys = AccountKeys::generate();
        let kek = SymmetricKey::generate();
        let blob = wrap_account_keys(&keys, &kek).unwrap();
        let json = serde_json::to_value(&blob).unwrap();
        assert_eq!(json.get("v").unwrap().as_u64().unwrap(), 1);
        assert!(json.get("nonce").unwrap().is_string());
        assert!(json.get("ciphertext").unwrap().is_string());
        let round_tripped: AccountKeyBlob = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, blob);
    }

    #[test]
    fn recovery_code_derives_matching_kek() {
        let code = generate_recovery_code();
        // 8 groups of 4 chars joined by '-' = 32 chars + 7 dashes.
        assert_eq!(code.len(), 32 + 7);
        assert_eq!(code.matches('-').count(), 7);

        let params = KdfParams::generate();
        let a = derive_recovery_kek(&code, &params).unwrap();
        let b = derive_recovery_kek(&code, &params).unwrap();
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn recovery_code_normalization_accepts_variants() {
        let code = generate_recovery_code();
        let params = KdfParams::generate();
        let canonical = derive_recovery_kek(&code, &params).unwrap();

        let lower = code.to_lowercase();
        let no_dashes = code.replace('-', "");
        let extra_spaces = code.replace('-', "  -  ").to_lowercase();
        let mixed = format!(" {} ", code.replace('-', " "));

        for variant in [lower, no_dashes, extra_spaces, mixed] {
            let derived = derive_recovery_kek(&variant, &params).unwrap();
            assert_eq!(derived.0, canonical.0, "variant {variant:?} did not normalize");
        }
    }

    #[test]
    fn recovery_kek_different_from_password_kek() {
        let params = KdfParams::generate();
        let code = generate_recovery_code();
        let recovery_kek = derive_recovery_kek(&code, &params).unwrap();
        let password_keys = derive_password_keys("hunter2", &params).unwrap();
        assert_ne!(recovery_kek.0, password_keys.kek.0);
    }

    #[test]
    fn key_envelope_wrap_unwrap_round_trip() {
        let sender = AccountKeys::generate();
        let recipient = AccountKeys::generate();
        let recipient_public = recipient.public_keys();
        let sender_public = sender.public_keys();

        let key = SymmetricKey::generate();
        let key_id = KeyId::new_v4();
        let envelope = wrap_key(&key, key_id, &recipient_public, &sender, "store:abc").unwrap();
        assert_eq!(envelope.key_id, key_id);
        assert_eq!(envelope.context, "store:abc");

        let recovered = unwrap_key(&envelope, &recipient, &sender_public.signing).unwrap();
        assert_eq!(recovered.0, key.0);
    }

    #[test]
    fn key_envelope_wrong_recipient_fails() {
        let sender = AccountKeys::generate();
        let recipient = AccountKeys::generate();
        let someone_else = AccountKeys::generate();
        let recipient_public = recipient.public_keys();
        let sender_public = sender.public_keys();

        let key = SymmetricKey::generate();
        let envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "store:abc").unwrap();

        let result = unwrap_key(&envelope, &someone_else, &sender_public.signing);
        assert!(matches!(result, Err(CryptoError::Decrypt)));
    }

    #[test]
    fn key_envelope_wrong_expected_signer_fails() {
        let sender = AccountKeys::generate();
        let impostor = AccountKeys::generate();
        let recipient = AccountKeys::generate();
        let recipient_public = recipient.public_keys();
        let impostor_public = impostor.public_keys();

        let key = SymmetricKey::generate();
        let envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "store:abc").unwrap();

        // Verifying against a different signer's public key than the one that
        // actually signed must fail signature verification.
        let result = unwrap_key(&envelope, &recipient, &impostor_public.signing);
        assert!(matches!(result, Err(CryptoError::BadSignature)));
    }

    #[test]
    fn key_envelope_tamper_detected() {
        let sender = AccountKeys::generate();
        let recipient = AccountKeys::generate();
        let recipient_public = recipient.public_keys();
        let sender_public = sender.public_keys();

        let key = SymmetricKey::generate();
        let mut envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "store:abc").unwrap();
        let mut raw = b64_decode(&envelope.ciphertext).unwrap();
        raw[0] ^= 0xff;
        envelope.ciphertext = b64_encode(&raw);

        let result = unwrap_key(&envelope, &recipient, &sender_public.signing);
        assert!(matches!(result, Err(CryptoError::BadSignature)));
    }

    #[test]
    fn verify_envelope_accepts_correct_and_rejects_tampered() {
        let sender = AccountKeys::generate();
        let recipient = AccountKeys::generate();
        let recipient_public = recipient.public_keys();
        let sender_public = sender.public_keys();

        let key = SymmetricKey::generate();
        let envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "store:abc").unwrap();

        // A correct envelope verifies with no private key at all.
        assert!(verify_envelope(&envelope, &sender_public.signing).is_ok());

        // Tampering with any signed field is caught.
        let mut tampered_context = envelope.clone();
        tampered_context.context = "store:other".to_string();
        assert!(matches!(
            verify_envelope(&tampered_context, &sender_public.signing),
            Err(CryptoError::BadSignature)
        ));

        let mut tampered_key_id = envelope.clone();
        tampered_key_id.key_id = KeyId::new_v4();
        assert!(matches!(
            verify_envelope(&tampered_key_id, &sender_public.signing),
            Err(CryptoError::BadSignature)
        ));

        let mut tampered_ciphertext = envelope.clone();
        let mut raw = b64_decode(&tampered_ciphertext.ciphertext).unwrap();
        raw[0] ^= 0xff;
        tampered_ciphertext.ciphertext = b64_encode(&raw);
        assert!(matches!(
            verify_envelope(&tampered_ciphertext, &sender_public.signing),
            Err(CryptoError::BadSignature)
        ));

        // A claimed signer that doesn't match the envelope's own `signer` field is
        // rejected before any signature math.
        let impostor = AccountKeys::generate().public_keys();
        assert!(matches!(
            verify_envelope(&envelope, &impostor.signing),
            Err(CryptoError::BadSignature)
        ));
    }

    #[test]
    fn key_envelope_json_round_trip_field_names() {
        let sender = AccountKeys::generate();
        let recipient = AccountKeys::generate();
        let recipient_public = recipient.public_keys();

        let key = SymmetricKey::generate();
        let envelope = wrap_key(&key, KeyId::new_v4(), &recipient_public, &sender, "share:xyz").unwrap();
        let json = serde_json::to_value(&envelope).unwrap();
        for field in [
            "v",
            "key_id",
            "recipient",
            "ephemeral",
            "nonce",
            "ciphertext",
            "signer",
            "signature",
            "context",
        ] {
            assert!(json.get(field).is_some(), "missing field {field}");
        }
        let round_tripped: KeyEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, envelope);
    }

    #[test]
    fn blob_round_trip() {
        let key = SymmetricKey::generate();
        let key_id = KeyId::new_v4();
        let aad = blob_aad("store-1", "node-1");
        let plaintext = b"hello, encrypted world";

        let blob = Blob::encrypt(&key, key_id, &aad, plaintext);
        assert_eq!(&blob[0..2], &BLOB_MAGIC);
        assert_eq!(blob[2], VERSION);

        assert_eq!(Blob::key_id(&blob).unwrap(), key_id);

        let decrypted = Blob::decrypt(&key, &aad, &blob).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn blob_key_id_readable_without_key() {
        let key = SymmetricKey::generate();
        let key_id = KeyId::new_v4();
        let aad = blob_aad("s", "d");
        let blob = Blob::encrypt(&key, key_id, &aad, b"data");
        assert_eq!(Blob::key_id(&blob).unwrap(), key_id);
    }

    #[test]
    fn blob_wrong_key_fails() {
        let key = SymmetricKey::generate();
        let wrong_key = SymmetricKey::generate();
        let aad = blob_aad("s", "d");
        let blob = Blob::encrypt(&key, KeyId::new_v4(), &aad, b"data");
        let result = Blob::decrypt(&wrong_key, &aad, &blob);
        assert!(matches!(result, Err(CryptoError::Decrypt)));
    }

    #[test]
    fn blob_wrong_aad_fails() {
        let key = SymmetricKey::generate();
        let blob = Blob::encrypt(&key, KeyId::new_v4(), &blob_aad("s", "d"), b"data");
        let result = Blob::decrypt(&key, &blob_aad("s", "other-doc"), &blob);
        assert!(matches!(result, Err(CryptoError::Decrypt)));
    }

    #[test]
    fn blob_tamper_detected() {
        let key = SymmetricKey::generate();
        let aad = blob_aad("s", "d");
        let mut blob = Blob::encrypt(&key, KeyId::new_v4(), &aad, b"data");
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        let result = Blob::decrypt(&key, &aad, &blob);
        assert!(matches!(result, Err(CryptoError::Decrypt)));
    }

    #[test]
    fn blob_wrong_magic_rejected() {
        let key = SymmetricKey::generate();
        let aad = blob_aad("s", "d");
        let mut blob = Blob::encrypt(&key, KeyId::new_v4(), &aad, b"data");
        blob[0] = b'X';
        assert!(matches!(Blob::key_id(&blob), Err(CryptoError::Malformed(_))));
        assert!(matches!(Blob::decrypt(&key, &aad, &blob), Err(CryptoError::Malformed(_))));
    }

    #[test]
    fn blob_unsupported_version_rejected() {
        let key = SymmetricKey::generate();
        let aad = blob_aad("s", "d");
        let mut blob = Blob::encrypt(&key, KeyId::new_v4(), &aad, b"data");
        blob[2] = 99;
        assert!(matches!(Blob::key_id(&blob), Err(CryptoError::UnsupportedVersion(99))));
    }

    #[test]
    fn blob_too_short_rejected() {
        let short = vec![b'P', b'B', 1];
        assert!(matches!(Blob::key_id(&short), Err(CryptoError::Malformed(_))));
    }

    #[test]
    fn kdf_params_json_round_trip() {
        let params = KdfParams::generate();
        let json = serde_json::to_string(&params).unwrap();
        let round_tripped: KdfParams = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, params);
    }

    #[test]
    fn account_public_keys_json_round_trip() {
        let keys = AccountKeys::generate();
        let public = keys.public_keys();
        let json = serde_json::to_string(&public).unwrap();
        let round_tripped: AccountPublicKeys = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, public);
    }
}
