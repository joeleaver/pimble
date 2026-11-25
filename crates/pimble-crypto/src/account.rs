//! Password and recovery-code derivation, and the account keys they wrap
//! (docs/CRYPTO_CONTRACT.md "Primitives").

use super::*;

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
