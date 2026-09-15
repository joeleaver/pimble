//! `GET /api/v1/kdf`'s decoy response for an unknown email
//! (docs/CRYPTO_CONTRACT.md: "a deterministic decoy salt (HMAC-SHA256 of the
//! lowercased email under a server secret) so the endpoint reveals
//! nothing").
//!
//! Deterministic per email so the same unknown address always gets the same
//! decoy salt (a client that calls `/kdf` twice for a typo'd address must
//! not see it change), but indistinguishable from a real user's salt
//! without the server secret.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use pimble_crypto::{KdfParams, KDF_M_COST_KIB, KDF_P_COST, KDF_T_COST};

/// The decoy salt for `email_lower` under `secret` — the first 16 bytes of
/// HMAC-SHA256(secret, email_lower), matching a real `KdfParams::salt`'s
/// size ("16 bytes, base64url without padding in JSON" — docs/
/// CRYPTO_CONTRACT.md "Primitives").
pub fn decoy_kdf_params(secret: &[u8], email_lower: &str) -> KdfParams {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC-SHA256 accepts a key of any length");
    mac.update(email_lower.as_bytes());
    let digest = mac.finalize().into_bytes();
    KdfParams { salt: URL_SAFE_NO_PAD.encode(&digest[..16]), m_cost: KDF_M_COST_KIB, t_cost: KDF_T_COST, p_cost: KDF_P_COST }
}
