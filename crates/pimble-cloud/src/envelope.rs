//! Verifying a [`pimble_crypto::KeyEnvelope`]'s signature server-side
//! (`PUT /api/v1/stores/{id}/keys`, docs/CRYPTO_CONTRACT.md).
//!
//! `pimble_crypto::unwrap_key` (agent K's crate, now fully implemented)
//! verifies the same signature, but needs the *recipient's* private
//! `AccountKeys` to unwrap the key afterwards — key material the server
//! never has. Its signing-bytes helper (`envelope_signing_bytes` in
//! `pimble-crypto/src/lib.rs`) is a private `fn`, not exported, so this file
//! reimplements exactly that layout to verify the signature alone, without
//! unwrapping anything. Confirmed against K's source (`wrap_key`/
//! `unwrap_key`, `crates/pimble-crypto/src/lib.rs`) rather than guessed from
//! the doc comment alone: the signed bytes are `v || key_id || recipient ||
//! ephemeral || nonce || ciphertext || context`, where `recipient`,
//! `ephemeral`, `nonce` and `ciphertext` are each field's **decoded raw
//! bytes** (base64url `URL_SAFE_NO_PAD`, matching `KeyEnvelope`'s own
//! encoding), not the base64url string; `context` is the plain UTF-8 string
//! as stored (it is never base64 to begin with). If `pimble-crypto` ever
//! exports its signing-bytes helper, this function should call that instead
//! of reimplementing it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use pimble_crypto::KeyEnvelope;

use crate::error::{CloudError, CloudResult};

fn decode(field: &'static str, s: &str) -> CloudResult<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s).map_err(|_| CloudError::BadRequest(format!("{field} is not valid base64url")))
}

fn decode_32(field: &'static str, s: &str) -> CloudResult<[u8; 32]> {
    decode(field, s)?.try_into().map_err(|_| CloudError::BadRequest(format!("{field} must decode to 32 bytes")))
}

fn decode_64(field: &'static str, s: &str) -> CloudResult<[u8; 64]> {
    decode(field, s)?.try_into().map_err(|_| CloudError::BadRequest(format!("{field} must decode to 64 bytes")))
}

/// The bytes `envelope.signature` is an Ed25519 signature over — see this
/// module's doc comment for why this mirrors `pimble-crypto`'s own private
/// `envelope_signing_bytes` rather than calling it.
fn envelope_signing_bytes(envelope: &KeyEnvelope) -> CloudResult<Vec<u8>> {
    let recipient = decode("envelope recipient", &envelope.recipient)?;
    let ephemeral = decode("envelope ephemeral", &envelope.ephemeral)?;
    let nonce = decode("envelope nonce", &envelope.nonce)?;
    let ciphertext = decode("envelope ciphertext", &envelope.ciphertext)?;

    let mut buf = Vec::with_capacity(1 + 16 + recipient.len() + ephemeral.len() + nonce.len() + ciphertext.len() + envelope.context.len());
    buf.push(envelope.v);
    buf.extend_from_slice(envelope.key_id.as_bytes());
    buf.extend_from_slice(&recipient);
    buf.extend_from_slice(&ephemeral);
    buf.extend_from_slice(&nonce);
    buf.extend_from_slice(&ciphertext);
    buf.extend_from_slice(envelope.context.as_bytes());
    Ok(buf)
}

/// Verifies `envelope.signature` against `expected_signer` (base64url
/// Ed25519 public key — the caller's `public_signing_key`, per
/// docs/CRYPTO_CONTRACT.md: "each envelope's signature must verify against
/// the caller's public signing key"). `Err(BadRequest)` on anything
/// malformed; `Err(Unauthorized)` on a well-formed signature that doesn't
/// verify.
pub fn verify_envelope_signature(envelope: &KeyEnvelope, expected_signer: &str) -> CloudResult<()> {
    if envelope.signer != expected_signer {
        return Err(CloudError::BadRequest("envelope's signer does not match the caller's public signing key".to_string()));
    }
    let signer_bytes = decode_32("envelope signer", &envelope.signer)?;
    let verifying_key =
        VerifyingKey::from_bytes(&signer_bytes).map_err(|_| CloudError::BadRequest("envelope signer is not a valid Ed25519 public key".to_string()))?;
    let signature_bytes = decode_64("envelope signature", &envelope.signature)?;
    let signature = Signature::from_bytes(&signature_bytes);
    verifying_key
        .verify(&envelope_signing_bytes(envelope)?, &signature)
        .map_err(|_| CloudError::Unauthorized("envelope signature does not verify".to_string()))
}
