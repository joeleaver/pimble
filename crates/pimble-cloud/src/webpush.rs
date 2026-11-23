//! Web Push (docs/NOTIFICATIONS_CONTRACT.md section 4, wave 2): a push to a
//! browser's push service, encrypted to that browser (RFC 8291 over RFC
//! 8188's `aes128gcm`) and signed as this service (VAPID, RFC 8292), so
//! neither the push service nor anyone between can read it.
//!
//! Written over the RustCrypto crates (`p256`, `aes-gcm`, `hkdf`) rather than
//! the `web-push` crate, whose encryption (`ece`) links OpenSSL; this service
//! is rustls only. [`decrypt`] is the browser's side, for the tests.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;

/// The record size this service writes: one record holds the whole push.
const RECORD_SIZE: u32 = 4096;
/// The most plaintext a push may carry: one record, less its delimiter and
/// tag (RFC 8188), and well under what push services accept (4 KiB bodies).
pub const MAX_PLAINTEXT: usize = 3800;
/// How long a VAPID token is good for (RFC 8292: at most a day).
const VAPID_TTL_SECS: i64 = 12 * 60 * 60;

#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("malformed {0}")]
    Malformed(&'static str),
    #[error("the push is too large")]
    TooLarge,
    #[error("encryption failed")]
    Crypto,
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(s: &str, what: &'static str) -> Result<Vec<u8>, PushError> {
    // Browsers hand out unpadded base64url; tolerate padding and the
    // standard alphabet too, as other servers do.
    let cleaned: String = s.trim().trim_end_matches('=').chars().map(|c| match c {
        '+' => '-',
        '/' => '_',
        c => c,
    }).collect();
    URL_SAFE_NO_PAD.decode(cleaned).map_err(|_| PushError::Malformed(what))
}

/// This service's VAPID key: a P-256 key pair whose public half browsers
/// subscribe with (`applicationServerKey`) and whose private half signs
/// every push.
#[derive(Clone)]
pub struct VapidKey {
    signing: SigningKey,
}

impl std::fmt::Debug for VapidKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VapidKey").field("public", &self.public()).finish_non_exhaustive()
    }
}

impl VapidKey {
    /// A new key (`pimble-cloud --generate-vapid-key`, and the tests).
    pub fn generate() -> Self {
        VapidKey { signing: SigningKey::random(&mut OsRng) }
    }

    /// From the private scalar, base64url (`PIMBLE_VAPID_PRIVATE_KEY`).
    pub fn from_private(private: &str) -> Result<Self, PushError> {
        let bytes = unb64(private, "VAPID private key")?;
        Ok(VapidKey { signing: SigningKey::from_slice(&bytes).map_err(|_| PushError::Malformed("VAPID private key"))? })
    }

    /// The private scalar, base64url: what `PIMBLE_VAPID_PRIVATE_KEY` holds.
    pub fn private(&self) -> String {
        b64(&self.signing.to_bytes())
    }

    /// The public key, uncompressed (65 bytes), base64url: what a browser
    /// subscribes with and what every push's `k=` names.
    pub fn public(&self) -> String {
        b64(self.signing.verifying_key().to_encoded_point(false).as_bytes())
    }

    /// The `Authorization` header of a push to `endpoint`: `vapid t=<JWT>,
    /// k=<public key>`, the token for the endpoint's origin, signed ES256.
    pub fn authorization(&self, endpoint: &str, subject: &str, now_secs: i64) -> Result<String, PushError> {
        let url = url::Url::parse(endpoint).map_err(|_| PushError::Malformed("endpoint"))?;
        let audience = url.origin().ascii_serialization();
        let header = b64(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = b64(serde_json::json!({ "aud": audience, "exp": now_secs + VAPID_TTL_SECS, "sub": subject }).to_string().as_bytes());
        let signing_input = format!("{header}.{claims}");
        let signature: Signature = self.signing.sign(signing_input.as_bytes());
        Ok(format!("vapid t={signing_input}.{}, k={}", b64(&signature.to_bytes()), self.public()))
    }
}

/// HKDF-SHA256 expand of `prk_salt`/`ikm` to `N` bytes under `info`.
fn hkdf<const N: usize>(salt: &[u8], ikm: &[u8], info: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, &mut out).expect("N is a valid HKDF length");
    out
}

/// RFC 8291 section 3.4: the input keying material from the ECDH secret,
/// the browser's auth secret and both public keys.
fn webpush_ikm(ecdh_secret: &[u8], auth: &[u8], ua_public: &[u8], as_public: &[u8]) -> [u8; 32] {
    let mut info = Vec::with_capacity(14 + 65 + 65);
    info.extend_from_slice(b"WebPush: info\0");
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public);
    hkdf::<32>(auth, ecdh_secret, &info)
}

/// RFC 8188 section 2.2/2.3: the content-encryption key and nonce of a
/// record, from the IKM and the salt.
fn cek_and_nonce(salt: &[u8], ikm: &[u8]) -> ([u8; 16], [u8; 12]) {
    (hkdf::<16>(salt, ikm, b"Content-Encoding: aes128gcm\0"), hkdf::<12>(salt, ikm, b"Content-Encoding: nonce\0"))
}

/// Encrypt `plaintext` to the browser whose subscription keys are `p256dh`
/// (its uncompressed P-256 public key) and `auth` (its 16-byte secret), both
/// base64url as the browser hands them out: the body of a push with
/// `Content-Encoding: aes128gcm`.
pub fn encrypt(plaintext: &[u8], p256dh: &str, auth: &str) -> Result<Vec<u8>, PushError> {
    if plaintext.len() > MAX_PLAINTEXT {
        return Err(PushError::TooLarge);
    }
    let ua_public_bytes = unb64(p256dh, "p256dh")?;
    let ua_public = PublicKey::from_sec1_bytes(&ua_public_bytes).map_err(|_| PushError::Malformed("p256dh"))?;
    let auth = unb64(auth, "auth")?;
    if auth.len() != 16 {
        return Err(PushError::Malformed("auth"));
    }
    let ephemeral = p256::ecdh::EphemeralSecret::random(&mut OsRng);
    let as_public = ephemeral.public_key().to_encoded_point(false);
    let shared = ephemeral.diffie_hellman(&ua_public);
    let ua_public_uncompressed = ua_public.to_encoded_point(false);
    let ikm = webpush_ikm(shared.raw_secret_bytes(), &auth, ua_public_uncompressed.as_bytes(), as_public.as_bytes());
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let (cek, nonce) = cek_and_nonce(&salt, &ikm);
    // One record, the last: the plaintext, then its delimiter (0x02), no padding.
    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| PushError::Crypto)?;
    let sealed = cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: &record, aad: b"" }).map_err(|_| PushError::Crypto)?;
    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + sealed.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.as_bytes().len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// The browser's side of [`encrypt`]: open a push body with the
/// subscription's private key and auth secret. Only a single-record push,
/// which is all this service sends.
pub fn decrypt(body: &[u8], ua_secret: &SecretKey, auth: &[u8]) -> Result<Vec<u8>, PushError> {
    if body.len() < 21 {
        return Err(PushError::Malformed("push body"));
    }
    let salt = &body[..16];
    let id_len = body[20] as usize;
    let header_len = 21 + id_len;
    if body.len() < header_len + 17 {
        return Err(PushError::Malformed("push body"));
    }
    let as_public = PublicKey::from_sec1_bytes(&body[21..header_len]).map_err(|_| PushError::Malformed("keyid"))?;
    let shared = p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), as_public.as_affine());
    let ua_public = ua_secret.public_key().to_encoded_point(false);
    let ikm = webpush_ikm(shared.raw_secret_bytes(), auth, ua_public.as_bytes(), &body[21..header_len]);
    let (cek, nonce) = cek_and_nonce(salt, &ikm);
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| PushError::Crypto)?;
    let mut record = cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &body[header_len..], aad: b"" }).map_err(|_| PushError::Crypto)?;
    // Strip the padding back to the delimiter.
    while let Some(&last) = record.last() {
        record.pop();
        if last == 0x02 || last == 0x01 {
            return Ok(record);
        }
        if last != 0 {
            return Err(PushError::Malformed("record delimiter"));
        }
    }
    Err(PushError::Malformed("record delimiter"))
}

/// A browser's keys, made here: for tests that stand in for one.
pub fn test_subscription_keys() -> (SecretKey, String, Vec<u8>, String) {
    let secret = SecretKey::random(&mut OsRng);
    let p256dh = b64(secret.public_key().to_encoded_point(false).as_bytes());
    let mut auth = vec![0u8; 16];
    OsRng.fill_bytes(&mut auth);
    let auth_b64 = b64(&auth);
    (secret, p256dh, auth, auth_b64)
}

/// Verify a VAPID `Authorization` header against `public` (base64url,
/// uncompressed) for `audience`: what a push service checks. For tests.
pub fn verify_authorization(header: &str, public: &str, audience: &str, now_secs: i64) -> Result<serde_json::Value, PushError> {
    use p256::ecdsa::signature::Verifier;
    let rest = header.strip_prefix("vapid ").ok_or(PushError::Malformed("scheme"))?;
    let mut token = None;
    let mut key = None;
    for part in rest.split(',') {
        let part = part.trim();
        if let Some(t) = part.strip_prefix("t=") {
            token = Some(t);
        } else if let Some(k) = part.strip_prefix("k=") {
            key = Some(k);
        }
    }
    let (token, key) = (token.ok_or(PushError::Malformed("t"))?, key.ok_or(PushError::Malformed("k"))?);
    if key != public {
        return Err(PushError::Malformed("k is not this service's key"));
    }
    let (signing_input, signature) = token.rsplit_once('.').ok_or(PushError::Malformed("token"))?;
    let verifying = p256::ecdsa::VerifyingKey::from_sec1_bytes(&unb64(public, "k")?).map_err(|_| PushError::Malformed("k"))?;
    let signature = Signature::from_slice(&unb64(signature, "signature")?).map_err(|_| PushError::Malformed("signature"))?;
    verifying.verify(signing_input.as_bytes(), &signature).map_err(|_| PushError::Crypto)?;
    let claims = signing_input.split('.').nth(1).ok_or(PushError::Malformed("token"))?;
    let claims: serde_json::Value = serde_json::from_slice(&unb64(claims, "claims")?).map_err(|_| PushError::Malformed("claims"))?;
    if claims["aud"] != audience || claims["exp"].as_i64().unwrap_or(0) <= now_secs {
        return Err(PushError::Malformed("claims"));
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_opens_with_the_subscriptions_keys_and_no_others() {
        let (secret, p256dh, auth, auth_b64) = test_subscription_keys();
        let body = encrypt(b"{\"sealed\":\"abc\"}", &p256dh, &auth_b64).unwrap();
        assert_eq!(&body[16..20], &RECORD_SIZE.to_be_bytes());
        assert_eq!(body[20], 65);
        assert_eq!(decrypt(&body, &secret, &auth).unwrap(), b"{\"sealed\":\"abc\"}");
        let (other, _, other_auth, _) = test_subscription_keys();
        assert!(decrypt(&body, &other, &auth).is_err(), "another browser's key");
        assert!(decrypt(&body, &secret, &other_auth).is_err(), "another auth secret");
        let mut tampered = body.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decrypt(&tampered, &secret, &auth).is_err());
        assert!(matches!(encrypt(&vec![b'x'; MAX_PLAINTEXT + 1], &p256dh, &auth_b64), Err(PushError::TooLarge)));
    }

    /// RFC 8291 section 5's worked example, with its fixed salt and keys:
    /// the derivation matches the published body byte for byte.
    #[test]
    fn the_rfc_8291_example_decrypts() {
        let body = unb64("DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN", "body").unwrap();
        let secret = SecretKey::from_slice(&unb64("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94", "ua private").unwrap()).unwrap();
        let auth = unb64("BTBZMqHH6r4Tts7J_aSIgg", "auth").unwrap();
        assert_eq!(decrypt(&body, &secret, &auth).unwrap(), b"When I grow up, I want to be a watermelon");
    }

    #[test]
    fn a_vapid_header_verifies_with_the_services_public_key() {
        let key = VapidKey::generate();
        let again = VapidKey::from_private(&key.private()).unwrap();
        assert_eq!(again.public(), key.public());
        let header = key.authorization("https://fcm.googleapis.com/fcm/send/abc", "mailto:no-reply@m.pimble.app", 1_000).unwrap();
        let claims = verify_authorization(&header, &key.public(), "https://fcm.googleapis.com", 1_000).unwrap();
        assert_eq!(claims["sub"], "mailto:no-reply@m.pimble.app");
        assert!(verify_authorization(&header, &VapidKey::generate().public(), "https://fcm.googleapis.com", 1_000).is_err());
        assert!(verify_authorization(&header, &key.public(), "https://other.example", 1_000).is_err());
    }
}
