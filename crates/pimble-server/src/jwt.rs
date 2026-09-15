//! EdDSA (Ed25519) JWT verification against a JWKS endpoint
//! (docs/CLOUD_CONTRACT.md "B: pimble-server" item 1): configured by
//! `--jwks URL --issuer ISS` / `PIMBLE_JWKS_URL` / `PIMBLE_JWT_ISSUER`.
//! Audience is always `pimble`. Hand-rolled compact JWT parsing plus
//! `ed25519-dalek` for the signature, rather than pulling in `jsonwebtoken`:
//! the format this server accepts is deliberately narrow (one algorithm,
//! one audience, no nested JWTs, no encryption), so parsing it directly
//! keeps the trust boundary small and exactly as wide as what's actually
//! verified below.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use pimble_core::StoreId;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::warn;
use url::Url;

use crate::principal::{Principal, Role};

/// How often the background task refetches the JWKS regardless of demand.
const SCHEDULED_REFRESH: Duration = Duration::from_secs(10 * 60);

/// The minimum gap between two JWKS refetches triggered by an unknown `kid`
/// (a key rotation in progress, or a bad token) — never more than once a
/// minute, so a client hammering the server with garbage `kid`s can't turn
/// into a hammering of the JWKS endpoint too.
const UNKNOWN_KID_REFRESH_COOLDOWN: Duration = Duration::from_secs(60);

/// The `aud` claim this server accepts. Fixed, not configurable — every
/// Pimble deployment's tokens are minted for this one audience.
const EXPECTED_AUDIENCE: &str = "pimble";

/// Clock skew tolerance for `exp` (docs/CLOUD_CONTRACT.md "B: pimble-server"
/// item 1).
const EXPIRY_SKEW: i64 = 60;

/// Why a JWT was rejected. Never shown to the caller verbatim (the auth
/// layer's response is a generic 401), but logged so a misconfiguration is
/// diagnosable.
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("malformed JWT: {0}")]
    Malformed(&'static str),
    #[error("unsupported alg {0:?}; only EdDSA is accepted")]
    UnsupportedAlg(String),
    #[error("no kid in the JWT header")]
    MissingKid,
    #[error("no key found for kid {0:?}")]
    UnknownKid(String),
    #[error("signature verification failed")]
    BadSignature,
    #[error("wrong issuer: expected {expected:?}, got {actual:?}")]
    WrongIssuer { expected: String, actual: String },
    #[error("wrong audience: {0:?} is not accepted")]
    WrongAudience(String),
    #[error("token expired")]
    Expired,
}

/// A JWT's audience claim, either a single string or an array of strings
/// (both are valid JSON per RFC 7519 §4.1.3).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    Single(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, want: &str) -> bool {
        match self {
            Audience::Single(s) => s == want,
            Audience::Many(items) => items.iter().any(|s| s == want),
        }
    }

    fn describe(&self) -> String {
        match self {
            Audience::Single(s) => s.clone(),
            Audience::Many(items) => items.join(","),
        }
    }
}

#[derive(Debug, Deserialize)]
struct JwtHeader {
    alg: String,
    kid: Option<String>,
}

/// The custom claims nested under `claims` (docs/CLOUD_CONTRACT.md
/// "Identity and grants"): `{"claims": {"email": "...", "stores": {"<store
/// uuid>": "owner", ...}}}`.
#[derive(Debug, Default, Deserialize)]
struct CustomClaims {
    #[serde(default)]
    email: String,
    #[serde(default)]
    stores: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Audience,
    exp: i64,
    #[serde(default)]
    claims: CustomClaims,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kty: String,
    crv: Option<String>,
    x: Option<String>,
    kid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// Verifies EdDSA JWTs against a JWKS endpoint, refreshing the key set on a
/// schedule and on demand (docs/CLOUD_CONTRACT.md "B: pimble-server" item 1).
pub struct JwtVerifier {
    issuer: String,
    jwks_url: Url,
    http: reqwest::Client,
    keys: RwLock<HashMap<String, VerifyingKey>>,
    last_unknown_kid_refresh: Mutex<Option<Instant>>,
}

impl JwtVerifier {
    /// Build a verifier for `issuer`'s tokens, keyed by `jwks_url`, and
    /// spawn its background refresh task. The initial fetch is attempted
    /// but never fatal to construction: a JWKS endpoint that's briefly
    /// unreachable at startup shouldn't take the whole server down (the
    /// scheduled refresh, and any unknown-`kid` refresh a real request
    /// triggers, try again) — every token simply fails to verify (a 401,
    /// same as a wrong static token) until a fetch succeeds. Mirrors
    /// `crate::server::warm_up_embedding_model`'s "degrade, don't fail"
    /// shape for another optional startup dependency.
    pub async fn new(jwks_url: Url, issuer: String) -> anyhow::Result<std::sync::Arc<Self>> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        let verifier = std::sync::Arc::new(Self {
            issuer,
            jwks_url,
            http,
            keys: RwLock::new(HashMap::new()),
            last_unknown_kid_refresh: Mutex::new(None),
        });

        verifier.refresh().await;

        let scheduled = std::sync::Arc::clone(&verifier);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SCHEDULED_REFRESH).await;
                scheduled.refresh().await;
            }
        });

        Ok(verifier)
    }

    /// Refetch the JWKS and replace the cached key set. Logged and left in
    /// place on failure — an endpoint blip never blanks out keys that were
    /// working a moment ago.
    async fn refresh(&self) {
        match self.fetch().await {
            Ok(keys) => {
                let count = keys.len();
                *self.keys.write().await = keys;
                tracing::debug!("JWKS refreshed from {}: {} key(s)", self.jwks_url, count);
            }
            Err(e) => {
                warn!("JWKS refresh from {} failed ({}); keeping the previous key set", self.jwks_url, e);
            }
        }
    }

    async fn fetch(&self) -> anyhow::Result<HashMap<String, VerifyingKey>> {
        let body = self.http.get(self.jwks_url.clone()).send().await?.error_for_status()?.bytes().await?;
        let set: JwkSet = serde_json::from_slice(&body)?;

        let mut keys = HashMap::new();
        for jwk in set.keys {
            if jwk.kty != "OKP" || jwk.crv.as_deref() != Some("Ed25519") {
                continue; // not an Ed25519 signing key; nothing else is ever accepted
            }
            let (Some(kid), Some(x)) = (jwk.kid, jwk.x) else { continue };
            let Ok(x_bytes) = URL_SAFE_NO_PAD.decode(&x) else { continue };
            let Ok(x_arr): Result<[u8; 32], _> = x_bytes.try_into() else { continue };
            let Ok(key) = VerifyingKey::from_bytes(&x_arr) else { continue };
            keys.insert(kid, key);
        }
        Ok(keys)
    }

    /// The verifying key for `kid`, refreshing the JWKS first if it's not
    /// already cached (rate-limited to once a minute).
    async fn key_for(&self, kid: &str) -> Option<VerifyingKey> {
        if let Some(key) = self.keys.read().await.get(kid).copied() {
            return Some(key);
        }

        let should_refresh = {
            let mut last = self.last_unknown_kid_refresh.lock().unwrap();
            let now = Instant::now();
            let due = last.is_none_or(|t| now.duration_since(t) >= UNKNOWN_KID_REFRESH_COOLDOWN);
            if due {
                *last = Some(now);
            }
            due
        };
        if should_refresh {
            self.refresh().await;
        }

        self.keys.read().await.get(kid).copied()
    }

    /// Verify a compact JWT (`header.payload.signature`, all base64url no
    /// padding) and return the [`Principal::User`] it names. `EdDSA` only;
    /// `iss`/`aud`/`exp` (with skew) checked as specified.
    pub async fn verify(&self, token: &str) -> Result<Principal, JwtError> {
        let mut parts = token.split('.');
        let header_b64 = parts.next().ok_or(JwtError::Malformed("no header segment"))?;
        let payload_b64 = parts.next().ok_or(JwtError::Malformed("no payload segment"))?;
        let sig_b64 = parts.next().ok_or(JwtError::Malformed("no signature segment"))?;
        if parts.next().is_some() {
            return Err(JwtError::Malformed("more than three segments"));
        }

        let header_bytes = URL_SAFE_NO_PAD.decode(header_b64).map_err(|_| JwtError::Malformed("bad header base64"))?;
        let header: JwtHeader = serde_json::from_slice(&header_bytes).map_err(|_| JwtError::Malformed("bad header json"))?;
        if header.alg != "EdDSA" {
            return Err(JwtError::UnsupportedAlg(header.alg));
        }
        let kid = header.kid.ok_or(JwtError::MissingKid)?;

        let key = self.key_for(&kid).await.ok_or_else(|| JwtError::UnknownKid(kid.clone()))?;

        let sig_bytes = URL_SAFE_NO_PAD.decode(sig_b64).map_err(|_| JwtError::Malformed("bad signature base64"))?;
        let sig_arr: [u8; 64] = sig_bytes.as_slice().try_into().map_err(|_| JwtError::Malformed("signature is not 64 bytes"))?;
        let signature = Signature::from_bytes(&sig_arr);

        let signing_input = format!("{header_b64}.{payload_b64}");
        key.verify_strict(signing_input.as_bytes(), &signature).map_err(|_| JwtError::BadSignature)?;

        let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).map_err(|_| JwtError::Malformed("bad payload base64"))?;
        let claims: Claims = serde_json::from_slice(&payload_bytes).map_err(|_| JwtError::Malformed("bad payload json"))?;

        if claims.iss != self.issuer {
            return Err(JwtError::WrongIssuer { expected: self.issuer.clone(), actual: claims.iss });
        }
        if !claims.aud.contains(EXPECTED_AUDIENCE) {
            return Err(JwtError::WrongAudience(claims.aud.describe()));
        }
        let now = chrono::Utc::now().timestamp();
        if now > claims.exp + EXPIRY_SKEW {
            return Err(JwtError::Expired);
        }

        let grants = claims
            .claims
            .stores
            .iter()
            .filter_map(|(store_id, role)| {
                let store_id = StoreId::parse(store_id).ok()?;
                let role = Role::parse(role)?;
                Some((store_id, role))
            })
            .collect();

        Ok(Principal::User { sub: claims.sub, email: claims.claims.email, grants })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;
    use std::sync::Arc;

    fn signing_key() -> SigningKey {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        SigningKey::from_bytes(&seed)
    }

    /// Build a signed compact JWT for `signing_key`/`kid`, with `claims`
    /// spliced onto (or overriding) a default header/payload — the test's
    /// `patch` gets to mutate arbitrary fields (a wrong `iss`, a stale
    /// `exp`, ...) before signing.
    fn make_token(signing_key: &SigningKey, kid: &str, patch: impl FnOnce(&mut serde_json::Value, &mut serde_json::Value)) -> String {
        let mut header = json!({ "alg": "EdDSA", "kid": kid });
        let mut payload = json!({
            "iss": "https://issuer.example/v1",
            "sub": "user-1",
            "aud": "pimble",
            "exp": (chrono::Utc::now().timestamp() + 3600),
            "claims": { "email": "user@example.com", "stores": {} },
        });
        patch(&mut header, &mut payload);

        let header_b64 = B64.encode(serde_json::to_vec(&header).unwrap());
        let payload_b64 = B64.encode(serde_json::to_vec(&payload).unwrap());
        let signing_input = format!("{header_b64}.{payload_b64}");
        let sig = signing_key.sign(signing_input.as_bytes());
        let sig_b64 = B64.encode(sig.to_bytes());
        format!("{signing_input}.{sig_b64}")
    }

    /// A verifier with one key pre-seeded (no network): exercises
    /// `verify`'s claim checks in isolation from JWKS fetching.
    fn verifier_with_key(kid: &str, key: VerifyingKey, issuer: &str) -> JwtVerifier {
        let mut keys = HashMap::new();
        keys.insert(kid.to_string(), key);
        JwtVerifier {
            issuer: issuer.to_string(),
            jwks_url: "http://127.0.0.1:1/jwks.json".parse().unwrap(),
            http: reqwest::Client::new(),
            keys: RwLock::new(keys),
            last_unknown_kid_refresh: Mutex::new(Some(Instant::now())), // never auto-refresh in these tests
        }
    }

    #[tokio::test]
    async fn a_correctly_signed_token_verifies_and_carries_its_grants() {
        let sk = signing_key();
        let verifier = verifier_with_key("kid-1", sk.verifying_key(), "https://issuer.example/v1");
        let store_id = StoreId::new();
        let token = make_token(&sk, "kid-1", |_h, payload| {
            payload["claims"]["stores"][store_id.to_string()] = json!("editor");
        });

        let principal = verifier.verify(&token).await.expect("a well-formed, correctly signed token verifies");
        match principal {
            Principal::User { sub, email, grants } => {
                assert_eq!(sub, "user-1");
                assert_eq!(email, "user@example.com");
                assert_eq!(grants.get(&store_id), Some(&Role::Editor));
            }
            Principal::Service => panic!("expected a User principal"),
        }
    }

    #[tokio::test]
    async fn a_bad_signature_is_rejected() {
        let sk = signing_key();
        let other = signing_key();
        let verifier = verifier_with_key("kid-1", other.verifying_key(), "https://issuer.example/v1");
        let token = make_token(&sk, "kid-1", |_h, _p| {});

        assert!(matches!(verifier.verify(&token).await, Err(JwtError::BadSignature)));
    }

    #[tokio::test]
    async fn wrong_issuer_is_rejected() {
        let sk = signing_key();
        let verifier = verifier_with_key("kid-1", sk.verifying_key(), "https://issuer.example/v1");
        let token = make_token(&sk, "kid-1", |_h, payload| {
            payload["iss"] = json!("https://someone-else.example");
        });

        assert!(matches!(verifier.verify(&token).await, Err(JwtError::WrongIssuer { .. })));
    }

    #[tokio::test]
    async fn wrong_audience_is_rejected() {
        let sk = signing_key();
        let verifier = verifier_with_key("kid-1", sk.verifying_key(), "https://issuer.example/v1");
        let token = make_token(&sk, "kid-1", |_h, payload| {
            payload["aud"] = json!("someone-else");
        });

        assert!(matches!(verifier.verify(&token).await, Err(JwtError::WrongAudience(_))));
    }

    #[tokio::test]
    async fn an_expired_token_is_rejected_past_the_skew() {
        let sk = signing_key();
        let verifier = verifier_with_key("kid-1", sk.verifying_key(), "https://issuer.example/v1");
        let token = make_token(&sk, "kid-1", |_h, payload| {
            payload["exp"] = json!(chrono::Utc::now().timestamp() - 3600);
        });

        assert!(matches!(verifier.verify(&token).await, Err(JwtError::Expired)));
    }

    #[tokio::test]
    async fn a_token_within_the_skew_window_still_verifies() {
        let sk = signing_key();
        let verifier = verifier_with_key("kid-1", sk.verifying_key(), "https://issuer.example/v1");
        let token = make_token(&sk, "kid-1", |_h, payload| {
            payload["exp"] = json!(chrono::Utc::now().timestamp() - 30); // expired 30s ago, skew is 60s
        });

        assert!(verifier.verify(&token).await.is_ok());
    }

    #[tokio::test]
    async fn a_non_eddsa_alg_is_rejected() {
        let sk = signing_key();
        let verifier = verifier_with_key("kid-1", sk.verifying_key(), "https://issuer.example/v1");
        let token = make_token(&sk, "kid-1", |header, _payload| {
            header["alg"] = json!("HS256");
        });

        assert!(matches!(verifier.verify(&token).await, Err(JwtError::UnsupportedAlg(_))));
    }

    #[tokio::test]
    async fn an_unknown_kid_with_no_jwks_endpoint_is_rejected() {
        let sk = signing_key();
        let verifier = verifier_with_key("kid-1", sk.verifying_key(), "https://issuer.example/v1");
        let token = make_token(&sk, "kid-does-not-exist", |_h, _p| {});

        assert!(matches!(verifier.verify(&token).await, Err(JwtError::UnknownKid(_))));
    }

    #[tokio::test]
    async fn an_unrecognized_role_string_is_dropped_rather_than_guessed() {
        let sk = signing_key();
        let verifier = verifier_with_key("kid-1", sk.verifying_key(), "https://issuer.example/v1");
        let store_id = StoreId::new();
        let token = make_token(&sk, "kid-1", |_h, payload| {
            payload["claims"]["stores"][store_id.to_string()] = json!("superadmin");
        });

        let Principal::User { grants, .. } = verifier.verify(&token).await.unwrap() else { panic!("expected a User") };
        assert!(grants.is_empty());
    }

    /// Fetching against a real JWKS endpoint (a tiny axum stub), including
    /// the unknown-`kid` refresh path (docs/CLOUD_CONTRACT.md "B:
    /// pimble-server" item 1: refreshed on an unknown `kid`).
    #[tokio::test]
    async fn an_unknown_kid_triggers_a_refetch_that_finds_a_newly_rotated_key() {
        use axum::routing::get;
        use axum::Json;

        let sk = signing_key();
        let x = URL_SAFE_NO_PAD.encode(sk.verifying_key().to_bytes());
        let jwks = Arc::new(json!({
            "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": "rotated-kid", "x": x } ]
        }));

        let app_jwks = Arc::clone(&jwks);
        let app = axum::Router::new().route(
            "/jwks.json",
            get(move || {
                let jwks = Arc::clone(&app_jwks);
                async move { Json((*jwks).clone()) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let jwks_url: Url = format!("http://{}/jwks.json", addr).parse().unwrap();
        let verifier = JwtVerifier::new(jwks_url, "https://issuer.example/v1".to_string()).await.unwrap();

        // Signed with a kid the verifier hasn't seen in its *initial* fetch
        // (it just fetched it above, so seed with a kid that only shows up
        // after a rotation): force the cache empty to simulate "the key
        // rotated after this verifier last refreshed".
        verifier.keys.write().await.clear();
        *verifier.last_unknown_kid_refresh.lock().unwrap() = None;

        let token = make_token(&sk, "rotated-kid", |_h, _p| {});
        let principal = verifier.verify(&token).await.expect("the unknown-kid refetch should find the key");
        assert!(matches!(principal, Principal::User { .. }));
    }
}
