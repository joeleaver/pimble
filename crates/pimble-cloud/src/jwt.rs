//! Minting the JWT a Pimble server verifies (docs/CLOUD_CONTRACT.md
//! "Identity and grants" and section C's env table), and serving the JWKS a
//! verifier checks it against.
//!
//! Two modes, chosen once at startup by whether `JKBASE_AUTH_ISSUER_URL` is
//! set:
//!
//! - **jkbase**: mint by calling jkbase-Auth's `POST <issuer>/token`; JWKS is
//!   jkbase's own, fetched and cached.
//! - **local (development)**: this process holds an Ed25519 key (from
//!   `PIMBLE_CLOUD_DEV_SIGNING_SEED`, or a random one logged as a warning)
//!   and signs the JWT itself. Hand-rolled rather than via a JWT crate: the
//!   claim shape (a nested `claims` object, jkbase's own registered-claim
//!   set) is jkbase's, not a generic library's, and minting is one function
//!   in each mode either way — see [`JwtSigner::mint`].
//!
//! The relay (docs/RELAY_CONTRACT.md) is the one place this service also
//! **verifies** a token: a member's connection to a relayed store presents the
//! same JWT a Pimble server would be shown, and [`JwtSigner::verify`] checks
//! it against the same JWKS this service serves — its own key in local mode,
//! jkbase's in jkbase mode. It is a gate, not the authority: the owner's
//! machine verifies the token again itself and is what decides what the
//! member may do.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::error::{CloudError, CloudResult};

/// Tokens live one hour (docs/CLOUD_CONTRACT.md: "Tokens live one hour.").
pub const TOKEN_TTL_SECS: i64 = 3600;
/// Audience every Pimble-issued token carries.
pub const AUDIENCE: &str = "pimble";

/// How long a fetched jkbase JWKS is served from memory.
const JWKS_CACHE_TTL: Duration = Duration::from_secs(600);
/// The least time between two JWKS refetches caused by a token naming a `kid`
/// the cached set lacks (a key rotation, or garbage) — so a caller sending
/// made-up `kid`s cannot turn this service into a hammer on jkbase-Auth. The
/// same minute `pimble-server`'s verifier allows.
const UNKNOWN_KID_REFRESH_COOLDOWN: Duration = Duration::from_secs(60);

pub enum JwtSigner {
    Local {
        signing_key: SigningKey,
        kid: String,
        issuer: String,
    },
    Jkbase {
        issuer_url: String,
        auth_key: String,
        http: reqwest::Client,
        jwks_cache: RwLock<Option<(Instant, Value)>>,
        /// When [`JwtSigner::verify`] last refetched the JWKS over an unknown
        /// `kid` — see [`UNKNOWN_KID_REFRESH_COOLDOWN`].
        last_unknown_kid_refresh: Mutex<Option<Instant>>,
    },
}

/// A minted token plus when it expires (epoch seconds), the shape
/// `POST /token` and `POST /login`/`POST /signup` all return.
pub struct MintedToken {
    pub token: String,
    pub exp: i64,
}

impl JwtSigner {
    pub fn from_config(config: &Config) -> Self {
        match (&config.jkbase_auth_issuer_url, &config.jkbase_auth_key) {
            (Some(issuer_url), Some(auth_key)) => {
                tracing::info!(issuer_url, "minting JWTs via jkbase-Auth");
                JwtSigner::Jkbase {
                    issuer_url: issuer_url.trim_end_matches('/').to_string(),
                    auth_key: auth_key.clone(),
                    http: reqwest::Client::new(),
                    jwks_cache: RwLock::new(None),
                    last_unknown_kid_refresh: Mutex::new(None),
                }
            }
            (Some(_), None) => {
                tracing::warn!("JKBASE_AUTH_ISSUER_URL is set but JKBASE_AUTH_KEY is not; falling back to local development signing");
                Self::local(config)
            }
            (None, _) => Self::local(config),
        }
    }

    fn local(config: &Config) -> Self {
        let seed: [u8; 32] = match &config.dev_signing_seed {
            Some(hex_seed) => {
                let bytes = hex::decode(hex_seed).unwrap_or_else(|e| {
                    panic!("PIMBLE_CLOUD_DEV_SIGNING_SEED is not valid hex: {e}");
                });
                bytes.try_into().unwrap_or_else(|v: Vec<u8>| {
                    panic!("PIMBLE_CLOUD_DEV_SIGNING_SEED must decode to 32 bytes, got {}", v.len());
                })
            }
            None => {
                tracing::warn!(
                    "PIMBLE_CLOUD_DEV_SIGNING_SEED is not set; generating a random development \
                     signing key for this process only. Tokens minted now will not verify after \
                     a restart, and every replica of this process will disagree. Set the env var \
                     to a stable 32-byte hex seed before running more than one instance."
                );
                let mut seed = [0u8; 32];
                rand::RngCore::fill_bytes(&mut rand::rng(), &mut seed);
                seed
            }
        };
        let signing_key = SigningKey::from_bytes(&seed);
        let kid = kid_for(&signing_key.verifying_key());
        let issuer = format!("{}/api/v1", config.public_url.trim_end_matches('/'));
        JwtSigner::Local { signing_key, kid, issuer }
    }

    /// Mint a token for `sub` (a `User::user_uuid`) carrying `claims` (the
    /// `{ email, stores }` object — see docs/CLOUD_CONTRACT.md).
    pub async fn mint(&self, sub: &str, claims: Value) -> CloudResult<MintedToken> {
        match self {
            JwtSigner::Local { signing_key, kid, issuer } => {
                let now = chrono::Utc::now().timestamp();
                let exp = now + TOKEN_TTL_SECS;
                let header = json!({ "alg": "EdDSA", "typ": "JWT", "kid": kid });
                let payload = json!({
                    "iss": issuer,
                    "sub": sub,
                    "aud": AUDIENCE,
                    "iat": now,
                    "exp": exp,
                    "jti": uuid::Uuid::new_v4().to_string(),
                    "claims": claims,
                });
                let signing_input = format!("{}.{}", b64_json(&header)?, b64_json(&payload)?);
                let signature = signing_key.sign(signing_input.as_bytes());
                let token = format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
                Ok(MintedToken { token, exp })
            }
            JwtSigner::Jkbase { issuer_url, auth_key, http, .. } => {
                #[derive(Serialize)]
                struct TokenRequest<'a> {
                    sub: &'a str,
                    aud: &'a str,
                    ttl: i64,
                    claims: Value,
                }
                #[derive(serde::Deserialize)]
                struct TokenResponse {
                    token: String,
                    exp: i64,
                }
                let resp = http
                    .post(format!("{issuer_url}/token"))
                    .bearer_auth(auth_key)
                    .json(&TokenRequest { sub, aud: AUDIENCE, ttl: TOKEN_TTL_SECS, claims })
                    .send()
                    .await
                    .map_err(|e| CloudError::Internal(format!("jkbase-Auth token request: {e}")))?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(CloudError::Internal(format!("jkbase-Auth token request failed ({status}): {body}")));
                }
                let parsed: TokenResponse =
                    resp.json().await.map_err(|e| CloudError::Internal(format!("jkbase-Auth token response: {e}")))?;
                Ok(MintedToken { token: parsed.token, exp: parsed.exp })
            }
        }
    }

    /// The JWKS document served at `/.well-known/jwks.json`.
    pub async fn jwks(&self) -> CloudResult<Value> {
        match self {
            JwtSigner::Local { signing_key, kid, .. } => Ok(local_jwks(&signing_key.verifying_key(), kid)),
            JwtSigner::Jkbase { jwks_cache, .. } => {
                {
                    let cache = jwks_cache.read().await;
                    if let Some((fetched_at, value)) = cache.as_ref() {
                        if fetched_at.elapsed() < JWKS_CACHE_TTL {
                            return Ok(value.clone());
                        }
                    }
                }
                self.refetch_jwks().await
            }
        }
    }

    /// Fetch jkbase's JWKS now, whatever the cache holds, and cache it. Local
    /// mode has nothing to fetch: its one key never changes in a process.
    async fn refetch_jwks(&self) -> CloudResult<Value> {
        match self {
            JwtSigner::Local { signing_key, kid, .. } => Ok(local_jwks(&signing_key.verifying_key(), kid)),
            JwtSigner::Jkbase { issuer_url, http, jwks_cache, .. } => {
                let resp = http
                    .get(format!("{issuer_url}/.well-known/jwks.json"))
                    .send()
                    .await
                    .map_err(|e| CloudError::Internal(format!("fetching jkbase JWKS: {e}")))?;
                if !resp.status().is_success() {
                    return Err(CloudError::Internal(format!("fetching jkbase JWKS: HTTP {}", resp.status())));
                }
                let value: Value = resp.json().await.map_err(|e| CloudError::Internal(format!("parsing jkbase JWKS: {e}")))?;
                *jwks_cache.write().await = Some((Instant::now(), value.clone()));
                Ok(value)
            }
        }
    }

    /// The `iss` every token this service hands out carries: its own
    /// `<public url>/api/v1` when it signs locally, jkbase-Auth's per-project
    /// URL when jkbase mints (the value a Pimble server is given as
    /// `PIMBLE_JWT_ISSUER`, docs/DEPLOY.md).
    pub fn issuer(&self) -> &str {
        match self {
            JwtSigner::Local { issuer, .. } => issuer,
            JwtSigner::Jkbase { issuer_url, .. } => issuer_url,
        }
    }

    /// The Ed25519 key the JWKS lists under `kid`, if any. An unknown `kid`
    /// in jkbase mode refetches the key set first, at most once a minute.
    async fn verifying_key_for(&self, kid: &str) -> Result<VerifyingKey, JwtRejection> {
        let jwks = self.jwks().await.map_err(|e| JwtRejection::KeysUnavailable(e.to_string()))?;
        if let Some(key) = ed25519_key_in(&jwks, kid) {
            return Ok(key);
        }
        if let JwtSigner::Jkbase { last_unknown_kid_refresh, .. } = self {
            let due = {
                let mut last = last_unknown_kid_refresh.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                let due = last.is_none_or(|at| now.duration_since(at) >= UNKNOWN_KID_REFRESH_COOLDOWN);
                if due {
                    *last = Some(now);
                }
                due
            };
            if due {
                let jwks = self.refetch_jwks().await.map_err(|e| JwtRejection::KeysUnavailable(e.to_string()))?;
                if let Some(key) = ed25519_key_in(&jwks, kid) {
                    return Ok(key);
                }
            }
        }
        Err(JwtRejection::UnknownKid)
    }

    /// Verify a compact JWT the way a Pimble server does
    /// (`pimble-server/src/jwt.rs`, which this mirrors rather than depends
    /// on): `EdDSA` only, the signature against this service's own JWKS by
    /// `kid`, `iss` equal to [`Self::issuer`], `aud` containing
    /// [`AUDIENCE`], and `exp` still ahead — with no allowance for skew,
    /// since the only thing done with the answer is to hold a connection
    /// open until `exp`. Nothing of the token is logged or kept.
    pub async fn verify(&self, token: &str) -> Result<VerifiedToken, JwtRejection> {
        let mut parts = token.split('.');
        let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(JwtRejection::Malformed("not three segments"));
        };

        #[derive(Deserialize)]
        struct Header {
            alg: String,
            kid: Option<String>,
        }
        let header: Header = URL_SAFE_NO_PAD
            .decode(header_b64)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(JwtRejection::Malformed("unreadable header"))?;
        if header.alg != "EdDSA" {
            return Err(JwtRejection::UnsupportedAlg);
        }
        let kid = header.kid.ok_or(JwtRejection::Malformed("no kid"))?;
        let key = self.verifying_key_for(&kid).await?;

        let signature: [u8; 64] = URL_SAFE_NO_PAD
            .decode(signature_b64)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(JwtRejection::Malformed("unreadable signature"))?;
        let signing_input = format!("{header_b64}.{payload_b64}");
        key.verify_strict(signing_input.as_bytes(), &Signature::from_bytes(&signature)).map_err(|_| JwtRejection::BadSignature)?;

        // Only now is the payload anything but bytes somebody sent.
        #[derive(Deserialize)]
        struct Payload {
            iss: String,
            sub: String,
            aud: Audience,
            exp: i64,
            #[serde(default)]
            claims: CustomClaims,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Audience {
            One(String),
            Many(Vec<String>),
        }
        #[derive(Default, Deserialize)]
        struct CustomClaims {
            #[serde(default)]
            stores: Value,
        }
        let payload: Payload = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(JwtRejection::Malformed("unreadable payload"))?;

        if payload.iss != self.issuer() {
            return Err(JwtRejection::WrongIssuer);
        }
        let audience_ok = match &payload.aud {
            Audience::One(aud) => aud == AUDIENCE,
            Audience::Many(auds) => auds.iter().any(|aud| aud == AUDIENCE),
        };
        if !audience_ok {
            return Err(JwtRejection::WrongAudience);
        }
        if chrono::Utc::now().timestamp() >= payload.exp {
            return Err(JwtRejection::Expired);
        }
        Ok(VerifiedToken { sub: payload.sub, exp: payload.exp, stores: payload.claims.stores })
    }
}

/// What [`JwtSigner::verify`] learned from a token whose signature, issuer,
/// audience and expiry all held.
#[derive(Debug)]
pub struct VerifiedToken {
    pub sub: String,
    /// Epoch seconds at which the token stops being one.
    pub exp: i64,
    /// The `claims.stores` object as minted (`crate::claims`): per store id,
    /// a role string for the whole store or `{ "roots": { node: role } }` for
    /// shares. `Null` when the token carries none.
    stores: Value,
}

impl VerifiedToken {
    /// Whether the token grants anything on `store_id` (its canonical
    /// hyphenated form, as the claim spells it), in either claim shape: a
    /// known role on the whole store, or a known role on at least one shared
    /// root. *What* it grants is for the server at the far end to enforce;
    /// this only answers "has this account any business with this store".
    pub fn names_store(&self, store_id: &str) -> bool {
        fn is_role(value: &Value) -> bool {
            matches!(value.as_str(), Some("owner" | "editor" | "reader"))
        }
        match self.stores.get(store_id) {
            Some(whole @ Value::String(_)) => is_role(whole),
            Some(Value::Object(scoped)) => scoped.get("roots").and_then(Value::as_object).is_some_and(|roots| roots.values().any(is_role)),
            _ => false,
        }
    }
}

/// Why [`JwtSigner::verify`] refused a token. Never carries any part of the
/// token, so it is safe to log and to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtRejection {
    Malformed(&'static str),
    UnsupportedAlg,
    UnknownKid,
    BadSignature,
    WrongIssuer,
    WrongAudience,
    Expired,
    /// The JWKS could not be had (jkbase-Auth unreachable): nothing can be
    /// verified right now, which is this service's failure, not the caller's.
    KeysUnavailable(String),
}

impl std::fmt::Display for JwtRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JwtRejection::Malformed(what) => write!(f, "malformed token: {what}"),
            JwtRejection::UnsupportedAlg => write!(f, "only EdDSA tokens are accepted"),
            JwtRejection::UnknownKid => write!(f, "the token names a key this service does not know"),
            JwtRejection::BadSignature => write!(f, "signature verification failed"),
            JwtRejection::WrongIssuer => write!(f, "wrong issuer"),
            JwtRejection::WrongAudience => write!(f, "wrong audience"),
            JwtRejection::Expired => write!(f, "token expired"),
            JwtRejection::KeysUnavailable(why) => write!(f, "the key set is unavailable: {why}"),
        }
    }
}

/// The Ed25519 (`OKP`/`Ed25519`) key a JWKS document lists under `kid`.
fn ed25519_key_in(jwks: &Value, kid: &str) -> Option<VerifyingKey> {
    let key = jwks.get("keys")?.as_array()?.iter().find(|key| {
        key.get("kid").and_then(Value::as_str) == Some(kid)
            && key.get("kty").and_then(Value::as_str) == Some("OKP")
            && key.get("crv").and_then(Value::as_str) == Some("Ed25519")
    })?;
    let x: [u8; 32] = URL_SAFE_NO_PAD.decode(key.get("x")?.as_str()?).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&x).ok()
}

fn b64_json<T: Serialize>(value: &T) -> CloudResult<String> {
    let bytes = serde_json::to_vec(value).map_err(|e| CloudError::Internal(format!("encoding JWT segment: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// A stable id for a key: the first 16 hex characters of SHA-256(pubkey) —
/// enough to disambiguate keys in a JWKS `keys` array without leaking any of
/// the key material itself (the JWK's `x` already carries that).
fn kid_for(verifying_key: &VerifyingKey) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(verifying_key.as_bytes());
    hex::encode(&digest[..8])
}

/// An RFC 8037 OKP JWK for an Ed25519 public key.
fn local_jwks(verifying_key: &VerifyingKey, kid: &str) -> Value {
    json!({
        "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "use": "sig",
            "alg": "EdDSA",
            "kid": kid,
            "x": URL_SAFE_NO_PAD.encode(verifying_key.as_bytes()),
        }]
    })
}
