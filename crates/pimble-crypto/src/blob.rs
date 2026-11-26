//! Encrypted blobs, the associated data each kind is sealed with, and data-key
//! wraps (docs/CRYPTO_CONTRACT.md "Blob layout").

use super::*;

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

/// The key of an account's personal document (docs/DISCUSSIONS_CONTRACT.md
/// section 8): HKDF of the account's encryption secret, so every device that
/// holds the account keys derives it and nothing else can. Never stored.
pub fn personal_key(keys: &AccountKeys) -> SymmetricKey {
    let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT_PERSONAL), &keys.encryption_secret);
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO_PERSONAL, &mut out).expect("32 bytes is a valid HKDF length");
    SymmetricKey(out)
}

/// The associated data the personal document's blob is sealed with: the
/// account it belongs to, so one account's blob never opens as another's.
pub fn personal_aad(user_id: &str) -> Vec<u8> {
    format!("pimble-personal/{user_id}").into_bytes()
}

/// The associated data the account's chosen watches are sealed with
/// (docs/NOTIFICATIONS_CONTRACT.md section 3), under [`personal_key`]: apart
/// from the personal document's, so neither blob opens as the other.
pub fn watches_aad(user_id: &str) -> Vec<u8> {
    format!("pimble-watches/{user_id}").into_bytes()
}

/// The associated data for a document's blobs.
pub fn blob_aad(store_id: &str, doc_id: &str) -> Vec<u8> {
    format!("{store_id}/{doc_id}").into_bytes()
}

/// The associated data for one part of a document's blob that was sealed in
/// parts (docs/SYNC_LIMITS_CONTRACT.md, "Snapshots and appends in parts"):
/// the document's, naming the part's index, the number of parts and the
/// group they belong to. A part therefore never opens as a whole blob (an
/// older reader, which knows nothing of parts, fails to decrypt it and never
/// applies a fragment), never as another index of its group, and never as a
/// part of another group.
pub fn blob_part_aad(store_id: &str, doc_id: &str, index: u32, count: u32, group: &str) -> Vec<u8> {
    format!("{store_id}/{doc_id}#{index}/{count}/{group}").into_bytes()
}

/// The associated data a presence frame's body is sealed with
/// (docs/PRESENCE_CONTRACT.md section 9): never the same as a document's
/// blobs', so neither can stand in for the other.
pub fn presence_aad(store_id: &str, doc_id: &str) -> Vec<u8> {
    format!("{store_id}/{doc_id}/presence").into_bytes()
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

/// The version of a sealed local file's associated data
/// ([`local_file_aad`]).
pub const LOCAL_FILE_VERSION: u8 = 1;

/// The associated data a file the browser's server keeps on its own disk is
/// sealed with (docs/ONE_CODEBASE_CONTRACT.md section 7): the file's path and
/// a format version, so a file never opens under another name, and never as a
/// document blob, a share's key or anything else sealed with other data.
pub fn local_file_aad(path: &str) -> Vec<u8> {
    format!("pimble-local/v{LOCAL_FILE_VERSION}/{path}").into_bytes()
}
