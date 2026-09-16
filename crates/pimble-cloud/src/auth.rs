//! Password hashing and opaque session tokens.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

use crate::error::{CloudError, CloudResult};

pub fn hash_password(password: &str) -> CloudResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| CloudError::Internal(format!("argon2 hash: {e}")))
}

fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else { return false };
    Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok()
}

/// A fixed, valid argon2id hash of a password nobody will ever type, used as
/// the comparison target when an email doesn't exist — so
/// `POST /login` does the same argon2 work either way (docs/CLOUD_CONTRACT.md:
/// "Constant-time on unknown email (hash anyway)"). Computed once, lazily,
/// since argon2id is deliberately slow and every unknown-email login would
/// otherwise pay for it twice.
fn decoy_hash() -> &'static str {
    static DECOY: OnceLock<String> = OnceLock::new();
    DECOY.get_or_init(|| hash_password("not-a-real-password-just-a-timing-decoy").expect("hashing the decoy password cannot fail"))
}

/// Verify `password` against `stored` (`Some` for a real user, `None` for an
/// unknown email) — always does one argon2id verification, so the two cases
/// take the same time and the caller (which always returns the same generic
/// "invalid email or password" message either way) never leaks which one it
/// was through a timing side channel.
pub fn verify_password_constant_time(password: &str, stored: Option<&str>) -> bool {
    match stored {
        Some(hash) => verify_password(password, hash),
        None => {
            verify_password(password, decoy_hash());
            false
        }
    }
}

const SESSION_TOKEN_BYTES: usize = 32;
const VERIFY_TOKEN_BYTES: usize = 32;
const RECOVERY_TOKEN_BYTES: usize = 32;

/// A fresh opaque session token (returned to the client) and its hash (what
/// the `Session` row actually stores — docs/CLOUD_CONTRACT.md: "sessions of
/// 30 days stored as a hash").
pub fn new_session_token() -> (String, String) {
    let mut bytes = [0u8; SESSION_TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let token = format!("pmbl_sess_{}", hex::encode(bytes));
    let hash = hash_session_token(&token);
    (token, hash)
}

pub fn hash_session_token(token: &str) -> String {
    sha256_hex(token)
}

/// A fresh opaque email-verification token (embedded in the `/verify?token=`
/// link) and its hash (what `User::verify_token_hash` stores — Phase 1b:
/// "generates a 32-byte random token (stored hashed, 24 h expiry)").
pub fn new_verify_token() -> (String, String) {
    let mut bytes = [0u8; VERIFY_TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let hash = hash_verify_token(&token);
    (token, hash)
}

pub fn hash_verify_token(token: &str) -> String {
    sha256_hex(token)
}

/// A fresh opaque account-recovery token (embedded in the
/// `/app/recover?token=` link) and its hash (what `User::recovery_token_hash`
/// stores — docs/CRYPTO_CONTRACT.md "Phase 2a-2": "32 random bytes, stored
/// hashed, 1 hour, one use").
pub fn new_recovery_token() -> (String, String) {
    let mut bytes = [0u8; RECOVERY_TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let hash = hash_recovery_token(&token);
    (token, hash)
}

pub fn hash_recovery_token(token: &str) -> String {
    sha256_hex(token)
}

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}
