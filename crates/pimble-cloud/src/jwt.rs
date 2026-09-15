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

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::Serialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::config::Config;
use crate::error::{CloudError, CloudResult};

/// Tokens live one hour (docs/CLOUD_CONTRACT.md: "Tokens live one hour.").
pub const TOKEN_TTL_SECS: i64 = 3600;
/// Audience every Pimble-issued token carries.
pub const AUDIENCE: &str = "pimble";

pub enum JwtSigner {
    Local { signing_key: SigningKey, kid: String, issuer: String },
    Jkbase { issuer_url: String, auth_key: String, http: reqwest::Client, jwks_cache: RwLock<Option<(Instant, Value)>> },
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
            JwtSigner::Jkbase { issuer_url, http, jwks_cache, .. } => {
                const CACHE_TTL: Duration = Duration::from_secs(600);
                {
                    let cache = jwks_cache.read().await;
                    if let Some((fetched_at, value)) = cache.as_ref() {
                        if fetched_at.elapsed() < CACHE_TTL {
                            return Ok(value.clone());
                        }
                    }
                }
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
