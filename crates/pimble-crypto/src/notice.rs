//! Notices (docs/NOTIFICATIONS_CONTRACT.md section 2): the account's notice
//! key and notices sealed to it.

use super::*;

// --- Notices (docs/NOTIFICATIONS_CONTRACT.md section 2) ---------------------------

const HKDF_INFO_NOTICE: &[u8] = b"pimble-notice";
/// A sealed notice: version (1) · ephemeral X25519 public key (32) · nonce (24) · ciphertext.
const NOTICE_HEADER_LEN: usize = 1 + 32 + NONCE_LEN;

/// An account's notice key: an X25519 key pair used for notices and nothing
/// else. Its public half is published beside the account's other public
/// keys; its secret is wrapped under the account key ([`wrap_notice_key`])
/// and each device unwraps it once and keeps it, so a device that holds only
/// this key (a browser's service worker, later) can open the notices it is
/// sent and never a note.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct NoticeKey {
    pub secret: [u8; 32],
}

impl NoticeKey {
    pub fn generate() -> Self {
        NoticeKey { secret: StaticSecret::random_from_rng(OsRng).to_bytes() }
    }

    /// The public half, base64url without padding.
    pub fn public(&self) -> String {
        let secret = StaticSecret::from(self.secret);
        b64_encode(X25519PublicKey::from(&secret).as_bytes())
    }
}

/// What a notice key's envelope is for: its own context, so the envelope
/// never unwraps as a store key and no store key's as a notice key.
pub fn notice_key_context(user_id: &str) -> String {
    format!("notice:{user_id}")
}

/// The notice key's secret sealed to the account itself and signed by it:
/// the account's own [`KeyEnvelope`], as a store key is wrapped to its owner.
pub fn wrap_notice_key(notice: &NoticeKey, me: &AccountKeys, user_id: &str) -> Result<KeyEnvelope> {
    wrap_key(&SymmetricKey(notice.secret), uuid::Uuid::nil(), &me.public_keys(), me, &notice_key_context(user_id))
}

/// Open the notice key's envelope with the account keys; its signature must
/// be the account's own, its context this account's notice key, and the
/// secret must match `public` (the half the accounts service publishes), so
/// a substituted envelope is refused rather than used.
pub fn unwrap_notice_key(envelope: &KeyEnvelope, me: &AccountKeys, user_id: &str, public: &str) -> Result<NoticeKey> {
    if envelope.context != notice_key_context(user_id) {
        return Err(CryptoError::Malformed("notice key envelope context"));
    }
    let own_signer = me.public_keys().signing;
    let key = unwrap_key(envelope, me, &own_signer)?;
    let notice = NoticeKey { secret: key.0 };
    if notice.public() != public {
        return Err(CryptoError::Malformed("notice key does not match its public half"));
    }
    Ok(notice)
}

/// The associated data a notice is sealed with: its recipient, so a notice
/// sealed to one account never opens as another's.
fn notice_aad(recipient: &str) -> Vec<u8> {
    format!("pimble-notice/{recipient}").into_bytes()
}

/// Seal `plaintext` (a notice's JSON) to `recipient_public` (the recipient's
/// notice key) for the account `recipient`: ephemeral X25519 to the notice
/// key, HKDF-SHA256 (`"pimble-notice"`), XChaCha20-Poly1305. Answers
/// base64url. Unsigned: the accounts service stamps the sender it
/// authenticated beside the sealed body, and the recipient checks the
/// notice's own `from` against that stamp.
pub fn seal_notice(plaintext: &[u8], recipient_public: &str, recipient: &str) -> Result<String> {
    let public = X25519PublicKey::from(b64_decode_fixed::<32>(recipient_public, "notice key")?);
    let ephemeral = EphemeralSecret::random_from_rng(OsRng);
    let ephemeral_public = X25519PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&public);
    let mut key = notice_wrapping_key(shared.as_bytes(), ephemeral_public.as_bytes(), public.as_bytes())?;
    let nonce = random_nonce();
    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&key));
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad: &notice_aad(recipient) })
        .map_err(|_| CryptoError::Decrypt)?;
    key.zeroize();
    let mut out = Vec::with_capacity(NOTICE_HEADER_LEN + ciphertext.len());
    out.push(VERSION);
    out.extend_from_slice(ephemeral_public.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(b64_encode(&out))
}

/// Open a notice [`seal_notice`] sealed to `notice` for the account
/// `recipient`. A notice for anyone else, or tampered with, is
/// [`CryptoError::Decrypt`].
pub fn open_notice(sealed: &str, notice: &NoticeKey, recipient: &str) -> Result<Vec<u8>> {
    let bytes = b64_decode(sealed)?;
    if bytes.len() < NOTICE_HEADER_LEN {
        return Err(CryptoError::Malformed("sealed notice"));
    }
    if bytes[0] != VERSION {
        return Err(CryptoError::UnsupportedVersion(bytes[0]));
    }
    let mut ephemeral = [0u8; 32];
    ephemeral.copy_from_slice(&bytes[1..33]);
    let nonce = &bytes[33..NOTICE_HEADER_LEN];
    let secret = StaticSecret::from(notice.secret);
    let own_public = X25519PublicKey::from(&secret);
    let shared = secret.diffie_hellman(&X25519PublicKey::from(ephemeral));
    let mut key = notice_wrapping_key(shared.as_bytes(), &ephemeral, own_public.as_bytes())?;
    let cipher = XChaCha20Poly1305::new(XChaChaKey::from_slice(&key));
    let plaintext = cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: &bytes[NOTICE_HEADER_LEN..], aad: &notice_aad(recipient) })
        .map_err(|_| CryptoError::Decrypt);
    key.zeroize();
    plaintext
}

/// HKDF-SHA256 over the shared secret, salted with both public keys.
fn notice_wrapping_key(shared: &[u8], ephemeral: &[u8], recipient: &[u8]) -> Result<[u8; 32]> {
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(ephemeral);
    salt.extend_from_slice(recipient);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO_NOTICE, &mut out).map_err(|_| CryptoError::Kdf("hkdf expand notice".into()))?;
    Ok(out)
}
