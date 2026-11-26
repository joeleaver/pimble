//! Client-side cryptography for Pimble Cloud (docs/CRYPTO_CONTRACT.md, "Primitives").
//!
//! Pure Rust with no I/O and no async, so the same code runs in the desktop's local
//! server and in the browser. The account keys and what wraps them are in `account`,
//! changing what wraps them in `rotate`, key envelopes in `envelope`, blobs and
//! data-key wraps in `blob`, notices in `notice`; everything is re-exported here.
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
const HKDF_SALT_PERSONAL: &[u8] = b"pimble-personal-v1";
const HKDF_INFO_PERSONAL: &[u8] = b"pimble personal";

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

mod account;
mod blob;
mod envelope;
mod notice;
mod rotate;

pub use account::*;
pub use blob::*;
pub use envelope::*;
pub use notice::*;
pub use rotate::*;

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
mod tests;

#[cfg(test)]
mod notice_vector;
