//! Signing in and what a session is good for: `GET /kdf`, `POST /login`,
//! `GET /me/keys`'s shape, `POST /token` (where each store is reached, and
//! with what), and the account's sealed personal document.

use pimble_crypto::{AccountKeyBlob, AccountPublicKeys, KdfParams, KeyEnvelope};
use pimble_host::http::Method;
use serde::{Deserialize, Serialize};

use super::{endpoint, get, post, put, send, CloudError, Http, Result};

// ── /me/personal (docs/DISCUSSIONS_CONTRACT.md section 8) ─────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct PersonalBody {
    pub version: u64,
    pub blob: String,
}

/// The account's sealed personal document, `None` when it has none.
pub async fn get_personal(http: &dyn Http, base_url: &str, session: &str) -> Result<Option<PersonalBody>> {
    match get(http, &endpoint(base_url, "/api/v1/me/personal"), Some(session)).await {
        Ok(body) => Ok(Some(body)),
        Err(e) if e.status() == Some(404) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Replace it, naming the version replaced; `Ok(None)` when another device
/// wrote first (409): fetch, merge, write again.
pub async fn put_personal(http: &dyn Http, base_url: &str, session: &str, version: u64, blob: &str) -> Result<Option<u64>> {
    let body = PersonalBody { version, blob: blob.to_string() };
    match put::<_, PersonalBody>(http, &endpoint(base_url, "/api/v1/me/personal"), session, &body).await {
        Ok(answer) => Ok(Some(answer.version)),
        Err(e) if e.status() == Some(409) => Ok(None),
        Err(e) => Err(e),
    }
}

pub async fn delete_personal(http: &dyn Http, base_url: &str, session: &str) -> Result<()> {
    let resp = send(http, Method::Delete, &endpoint(base_url, "/api/v1/me/personal"), Some(session), None)
        .await
        .map_err(|e| match e {
            CloudError::Request { source, .. } => CloudError::Request { url: base_url.to_string(), source },
            other => other,
        })?;
    if resp.is_success() || resp.status == 404 {
        Ok(())
    } else {
        Err(CloudError::Status { url: base_url.to_string(), status: resp.status, body: resp.text() })
    }
}

// ── GET /kdf ──────────────────────────────────────────────────────────────

pub async fn kdf(http: &dyn Http, base_url: &str, email: &str) -> Result<KdfParams> {
    let encoded_email: String = url::form_urlencoded::byte_serialize(email.as_bytes()).collect();
    get(http, &endpoint(base_url, &format!("/api/v1/kdf?email={encoded_email}")), None).await.map_err(super::signing_in)
}

// ── POST /login ───────────────────────────────────────────────────────────

#[derive(Serialize)]
struct LoginRequest<'a> {
    email: &'a str,
    auth_key: &'a str,
}

#[derive(Deserialize)]
pub struct UserView {
    pub id: String,
    pub email: String,
}

#[derive(Deserialize)]
pub struct LoginResponse {
    pub user: UserView,
    pub session: String,
}

/// Refused as [`super::Refusal::WrongPassword`] for a wrong password and for
/// an address with no account alike, and as [`super::Refusal::Unverified`]
/// once the password matched an address not verified yet.
pub async fn login(http: &dyn Http, base_url: &str, email: &str, auth_key: &str) -> Result<LoginResponse> {
    post(http, &endpoint(base_url, "/api/v1/login"), None, &LoginRequest { email, auth_key }).await.map_err(super::signing_in)
}

// ── GET /me/keys ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MeKeysResponse {
    pub public_keys: AccountPublicKeys,
    pub kdf: KdfParams,
    pub account_key_blob: AccountKeyBlob,
    /// The account's notice key, once one of its devices made it.
    #[serde(default)]
    pub notice_key: Option<NoticeKeyView>,
}

#[derive(Deserialize)]
pub struct NoticeKeyView {
    pub public_key: String,
    pub envelope: KeyEnvelope,
}

/// The account's public keys, the password's KDF parameters and the account
/// keys wrapped under it.
pub async fn me_keys(http: &dyn Http, base_url: &str, session: &str) -> Result<MeKeysResponse> {
    get(http, &endpoint(base_url, "/api/v1/me/keys"), Some(session)).await.map_err(super::with_session)
}

// ── POST /token ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TokenResponse {
    pub token: String,
    #[allow(dead_code)]
    pub exp: i64,
    pub rpc_url: String,
    /// The stores that are reached at an endpoint of their own rather than
    /// `rpc_url` (docs/RELAY_CONTRACT.md): every relay-tier store the account
    /// holds a grant on, its own relayed stores included. Defaulted so a
    /// service from before the relay still parses.
    #[serde(default)]
    pub stores: Vec<StoreEndpoint>,
}

/// Where one store is reached, when that is not the hosted server.
#[derive(Debug, Clone, Deserialize)]
pub struct StoreEndpoint {
    pub store_id: String,
    pub rpc_url: String,
    /// A credential for this endpoint alone, when the service issues one;
    /// the account's token otherwise.
    #[serde(default)]
    pub token: Option<String>,
}

/// Where a link connects for one store, and with what.
pub struct Reach {
    pub url: url::Url,
    pub token: String,
    /// Whether this is the store's own endpoint (relay tier) rather than
    /// the hosted server.
    pub via_relay: bool,
}

impl TokenResponse {
    /// The endpoint and credential for `store_id`: its own entry in `stores`
    /// when the answer lists one (with that entry's `token` when it carries
    /// one), else `fallback` with the account's token. `fallback` is what
    /// the caller would have connected to before stores had endpoints of
    /// their own: `rpc_url` when a replica is first linked, `sync.json`'s
    /// remote for a link that is already there.
    pub fn reach(&self, store_id: pimble_core::StoreId, fallback: Option<&url::Url>) -> std::result::Result<Reach, String> {
        let id = store_id.to_string();
        if let Some(entry) = self.stores.iter().find(|entry| entry.store_id.eq_ignore_ascii_case(&id)) {
            let url = entry.rpc_url.parse().map_err(|e| format!("cloud service returned an invalid rpc_url {:?} for store {}: {}", entry.rpc_url, id, e))?;
            return Ok(Reach { url, token: entry.token.clone().unwrap_or_else(|| self.token.clone()), via_relay: true });
        }
        let url = match fallback {
            Some(url) => url.clone(),
            None => self.rpc_url.parse().map_err(|e| format!("cloud service returned an invalid rpc_url {:?}: {}", self.rpc_url, e))?,
        };
        Ok(Reach { url, token: self.token.clone(), via_relay: false })
    }
}

/// Mint a fresh JWT for the Pimble RPC server (docs/CRYPTO_CONTRACT.md: "the
/// link mints a JWT through `POST /api/v1/token` before every connect").
pub async fn mint_token(http: &dyn Http, base_url: &str, session: &str) -> Result<TokenResponse> {
    post(http, &endpoint(base_url, "/api/v1/token"), Some(session), &serde_json::json!({})).await
}

// ── POST /stores, GET /stores ─────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn minted(json: serde_json::Value) -> TokenResponse {
        serde_json::from_value(json).expect("a /token answer")
    }

    /// `/token` across the relay tier (docs/RELAY_CONTRACT.md): an answer
    /// from before it parses and reaches the hosted server; a store with an
    /// endpoint of its own is reached there, with its own token when it has
    /// one and the account's when it has not.
    #[test]
    fn a_store_is_reached_where_the_token_answer_says() {
        let store = pimble_core::StoreId::new();
        let other = pimble_core::StoreId::new();
        let saved: url::Url = "wss://pimble.app/rpc".parse().unwrap();

        let old = minted(serde_json::json!({ "token": "general", "exp": 1, "rpc_url": "wss://pimble.app/rpc" }));
        assert!(old.stores.is_empty());
        let reach = old.reach(store, None).unwrap();
        assert_eq!((reach.url.as_str(), reach.token.as_str(), reach.via_relay), ("wss://pimble.app/rpc", "general", false));

        let relay_url = format!("wss://pimble.app/api/v1/relay/{store}");
        let answer = minted(serde_json::json!({
            "token": "general", "exp": 1, "rpc_url": "wss://pimble.app/rpc",
            "stores": [
                { "store_id": store.to_string(), "rpc_url": relay_url, "token": "for-this-store", "exp": 1 },
                { "store_id": other.to_string(), "rpc_url": format!("wss://pimble.app/api/v1/relay/{other}") },
            ],
        }));
        let reach = answer.reach(store, Some(&saved)).unwrap();
        assert_eq!((reach.url.as_str(), reach.token.as_str(), reach.via_relay), (relay_url.as_str(), "for-this-store", true));
        let reach = answer.reach(other, Some(&saved)).unwrap();
        assert_eq!((reach.token.as_str(), reach.via_relay), ("general", true), "no token of its own: the account's");

        // Not listed: what the link had, with the account's token.
        let hosted = pimble_core::StoreId::new();
        let reach = answer.reach(hosted, Some(&saved)).unwrap();
        assert_eq!((reach.url.as_str(), reach.token.as_str(), reach.via_relay), ("wss://pimble.app/rpc", "general", false));

        let broken = minted(serde_json::json!({ "token": "t", "exp": 1, "rpc_url": "x", "stores": [ { "store_id": store.to_string(), "rpc_url": "not a url" } ] }));
        assert!(broken.reach(store, None).is_err());
    }
}
