//! Key envelopes: a symmetric key sealed to one account and signed by the
//! sender (docs/CRYPTO_CONTRACT.md "Key wrapping").

use super::*;

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
